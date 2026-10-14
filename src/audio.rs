//! Microphone capture through `pw-record` (PipeWire) and WAV loading.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

pub const SAMPLE_RATE: u32 = 16_000;
/// Safety cap so a forgotten recording cannot eat all memory.
pub const MAX_SECONDS: usize = 15 * 60;

#[derive(Clone, Debug)]
pub struct Mic {
    pub node_name: String,
    pub description: String,
}

/// Lists PipeWire audio sources (microphones), without monitor sources.
pub fn list_mics() -> Result<Vec<Mic>> {
    let out = Command::new("pw-dump").output().context("cannot run pw-dump (is PipeWire installed?)")?;
    if !out.status.success() {
        bail!("pw-dump failed");
    }
    parse_pw_dump(&String::from_utf8_lossy(&out.stdout))
}

fn parse_pw_dump(json: &str) -> Result<Vec<Mic>> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    let mut mics = Vec::new();
    for obj in v.as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
        let props = &obj["info"]["props"];
        if props["media.class"] == "Audio/Source"
            && let Some(name) = props["node.name"].as_str()
        {
            let desc = props["node.description"].as_str().unwrap_or(name);
            mics.push(Mic { node_name: name.into(), description: desc.into() });
        }
    }
    Ok(mics)
}

pub struct Recorder {
    child: Child,
    reader: JoinHandle<()>,
    samples: Arc<Mutex<Vec<i16>>>,
    level: Arc<AtomicU32>,
}

impl Recorder {
    /// Starts capturing 16 kHz mono from `mic` (empty = default source). Fails when `pw-record` dies right away.
    /// `on_limit` runs once when the recording reaches `MAX_SECONDS` (audio after that point is not kept).
    pub fn start(mic: &str, on_limit: impl FnOnce() + Send + 'static) -> Result<Self> {
        // pw-record waits forever for a target that does not exist, so check that the microphone is connected.
        if !mic.is_empty() && list_mics().is_ok_and(|mics| !mics.iter().any(|m| m.node_name == mic)) {
            bail!("microphone '{mic}' is not connected - choose another one in Settings");
        }
        let mut cmd = Command::new("pw-record");
        cmd.args(["--rate", "16000", "--channels", "1", "--format", "s16"]);
        if !mic.is_empty() {
            cmd.args(["--target", mic]);
        }
        cmd.arg("-").stdout(Stdio::piped()).stderr(Stdio::piped()).stdin(Stdio::null());
        let mut child = cmd.spawn().context("cannot run pw-record (is pipewire-bin installed?)")?;
        let mut stdout = child.stdout.take().context("no pw-record stdout")?;
        let mut stderr = child.stderr.take().context("no pw-record stderr")?;
        let samples = Arc::new(Mutex::new(Vec::new()));
        let level = Arc::new(AtomicU32::new(0));
        let (s2, l2) = (samples.clone(), level.clone());
        let reader = std::thread::spawn(move || {
            let mut on_limit = Some(on_limit);
            let mut buf = [0u8; 4096];
            let mut odd: Option<u8> = None;
            while let Ok(n) = stdout.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut chunk: Vec<i16> = Vec::with_capacity(n / 2 + 1);
                let mut bytes = buf[..n].iter().copied();
                while let Some(lo) = odd.take().or_else(|| bytes.next()) {
                    match bytes.next() {
                        Some(hi) => chunk.push(i16::from_le_bytes([lo, hi])),
                        None => odd = Some(lo),
                    }
                }
                if chunk.is_empty() {
                    continue;
                }
                let rms = (chunk.iter().map(|&s| (s as f64).powi(2)).sum::<f64>() / chunk.len() as f64).sqrt();
                l2.store(rms as u32, Ordering::Relaxed);
                let mut all = s2.lock().unwrap();
                if all.len() < MAX_SECONDS * SAMPLE_RATE as usize {
                    all.extend_from_slice(&chunk);
                } else if let Some(f) = on_limit.take() {
                    drop(all);
                    f();
                }
            }
        });
        // A bad target or a stopped PipeWire makes pw-record exit at once: report that now, not after the user has spoken.
        for _ in 0..8 {
            std::thread::sleep(std::time::Duration::from_millis(20));
            if let Ok(Some(status)) = child.try_wait() {
                let mut why = String::new();
                let _ = stderr.read_to_string(&mut why);
                let _ = reader.join();
                let target = if mic.is_empty() { "the default microphone".to_string() } else { format!("microphone '{mic}'") };
                bail!("pw-record exited ({status}) - is {target} available? {}", why.trim());
            }
        }
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = stderr.read_to_end(&mut sink); // drain so pw-record can never block on a full pipe
        });
        Ok(Self { child, reader, samples, level })
    }

    /// Current input level (RMS of the last chunk, 0..32767).
    pub fn level(&self) -> Arc<AtomicU32> {
        self.level.clone()
    }

    /// Stops capture and returns the audio as f32 in -1..1 (empty when nothing was captured).
    pub fn stop(mut self) -> Vec<f32> {
        // SIGTERM lets pw-record finish cleanly (SIGINT is ignored by processes started as background jobs);
        // after a grace period it is killed. The audio was read from its pipe all along, so nothing is lost.
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while matches!(self.child.try_wait(), Ok(None)) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = self.reader.join();
        let s = self.samples.lock().unwrap();
        s.iter().map(|&x| x as f32 / 32768.0).collect()
    }

    pub fn cancel(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = self.reader.join();
    }
}

/// Reads a WAV file as mono 16 kHz f32 (linear resampling when needed).
pub fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut r = hound::WavReader::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let spec = r.spec();
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<std::result::Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.map(|v| v as f32 / scale)).collect::<std::result::Result<_, _>>()?
        }
    };
    let ch = spec.channels.max(1) as usize;
    let mono: Vec<f32> = raw.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32).collect();
    Ok(resample(&mono, spec.sample_rate, SAMPLE_RATE))
}

fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from as f64 / to as f64;
    let n = (input.len() as f64 / ratio) as usize;
    (0..n)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = input[idx];
            let b = *input.get(idx + 1).unwrap_or(&a);
            a + (b - a) * frac
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sources_only() {
        let json = r#"[
            {"info":{"props":{"media.class":"Audio/Source","node.name":"alsa_input.x","node.description":"Built-in Mic"}}},
            {"info":{"props":{"media.class":"Audio/Sink","node.name":"alsa_output.y"}}},
            {"type":"x"}]"#;
        let mics = parse_pw_dump(json).unwrap();
        assert_eq!(mics.len(), 1);
        assert_eq!(mics[0].node_name, "alsa_input.x");
        assert_eq!(mics[0].description, "Built-in Mic");
    }

    #[test]
    fn resample_halves_length() {
        let v = resample(&vec![0.5; 32000], 32000, 16000);
        assert_eq!(v.len(), 16000);
    }
}
