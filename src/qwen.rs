//! Qwen3-ASR speech recognition through a managed llama.cpp `llama-server` on loopback.
//!
//! The server starts when a recording starts (warm-up while the user speaks), stays alive for a configurable time,
//! listens only on 127.0.0.1 with a random API key, and dies together with the daemon.

use crate::config::{Config, QwenCfg};
use crate::{audio::SAMPLE_RATE, download, models};
use anyhow::{bail, Context, Result};
use base64::Engine;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const RUNTIME_VERSION: &str = "b11011";
/// Longest audio piece sent in one request (keeps the 4096 token context safe).
const MAX_CHUNK_SECONDS: usize = 30;
const LANGUAGE_NAMES: [(&str, &str); 2] = [("cs", "Czech"), ("en", "English")];

pub struct QwenModel {
    pub id: &'static str,
    pub label: &'static str,
    pub repo: &'static str,
    pub revision: &'static str,
    pub model_file: &'static str,
    pub model_size: u64,
    pub mmproj_file: &'static str,
    pub mmproj_size: u64,
    pub ram_gb: f32,
}

impl QwenModel {
    pub fn total_mb(&self) -> u64 {
        (self.model_size + self.mmproj_size) / 1_000_000
    }
}

pub const MODELS: &[QwenModel] = &[
    QwenModel {
        id: "qwen3-0.6b-q8",
        label: "0.6B Q8 - small, fast",
        repo: "ggml-org/Qwen3-ASR-0.6B-GGUF",
        revision: "main",
        model_file: "Qwen3-ASR-0.6B-Q8_0.gguf",
        model_size: 804_749_248,
        mmproj_file: "mmproj-Qwen3-ASR-0.6B-Q8_0.gguf",
        mmproj_size: 214_392_480,
        ram_gb: 2.0,
    },
    QwenModel {
        id: "qwen3-1.7b-q4",
        label: "1.7B Q4 - recommended",
        repo: "getonit/Qwen3-ASR-1.7B-Q4_0-GGUF",
        revision: "bdff9f9b31f387d6eb797a8d827c08904048c57c",
        model_file: "Qwen3-ASR-1.7B-Q4_0.gguf",
        model_size: 1_229_449_152,
        mmproj_file: "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf",
        mmproj_size: 355_709_344,
        ram_gb: 3.5,
    },
    QwenModel {
        id: "qwen3-1.7b-q8",
        label: "1.7B Q8 - best quality",
        repo: "ggml-org/Qwen3-ASR-1.7B-GGUF",
        revision: "main",
        model_file: "Qwen3-ASR-1.7B-Q8_0.gguf",
        model_size: 2_165_034_944,
        mmproj_file: "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf",
        mmproj_size: 355_709_344,
        ram_gb: 4.5,
    },
];

struct RuntimeAsset {
    url: &'static str,
    size: u64,
    sha256: &'static str,
}

fn runtime_asset(device: &str) -> Result<RuntimeAsset> {
    Ok(match device {
        "cpu" => RuntimeAsset {
            url: "https://github.com/ggml-org/llama.cpp/releases/download/b11011/llama-b11011-bin-ubuntu-x64.tar.gz",
            size: 16_855_444,
            sha256: "e060290d1950469f465092230726b94464a9c2d0f56f1434cc61c7b9d5227527",
        },
        "vulkan" => RuntimeAsset {
            url: "https://github.com/ggml-org/llama.cpp/releases/download/b11011/llama-b11011-bin-ubuntu-vulkan-x64.tar.gz",
            size: 30_217_811,
            sha256: "0ed8eb969fb52acfa06fd960d683e7365dfca962ae8ec214b504875c93de8a72",
        },
        other => bail!("unknown device '{other}' (use cpu or vulkan)"),
    })
}

pub fn find(id: &str) -> Result<&'static QwenModel> {
    MODELS.iter().find(|m| m.id == id).with_context(|| format!("unknown Qwen model '{id}'"))
}

fn root() -> PathBuf {
    models::dir().join("qwen")
}

pub fn model_dir(id: &str) -> PathBuf {
    root().join(id)
}

fn runtime_dir(device: &str) -> PathBuf {
    root().join("runtime").join(format!("{device}-{RUNTIME_VERSION}"))
}

/// The managed `llama-server` binary for a device (the tarball has one top folder, `llama-<version>`).
fn managed_binary(device: &str) -> PathBuf {
    runtime_dir(device).join(format!("llama-{RUNTIME_VERSION}")).join("llama-server")
}

/// The server to run: the user's own binary, or the managed download.
pub fn server_binary(cfg: &QwenCfg) -> Result<PathBuf> {
    let path = if cfg.server_path.trim().is_empty() { managed_binary(&cfg.device) } else { PathBuf::from(cfg.server_path.trim()) };
    if !path.is_file() {
        bail!("llama-server not found at {} - download the runtime in Settings", path.display());
    }
    Ok(path)
}

pub fn model_ready(id: &str) -> bool {
    find(id).map(|m| model_dir(id).join(m.model_file).is_file() && model_dir(id).join(m.mmproj_file).is_file()).unwrap_or(false)
}

pub fn runtime_ready(cfg: &QwenCfg) -> bool {
    server_binary(cfg).is_ok()
}

pub fn ready(cfg: &QwenCfg) -> bool {
    model_ready(&cfg.model) && runtime_ready(cfg)
}

/// Downloads the runtime (unless the user set an own server path) and the model files.
/// `progress` gets `(bytes_done, bytes_total)` over everything.
pub fn download_all(cfg: &QwenCfg, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let model = find(&cfg.model)?;
    let own_server = !cfg.server_path.trim().is_empty();
    let asset = runtime_asset(&cfg.device)?;
    let need_runtime = !own_server && !managed_binary(&cfg.device).is_file();
    let dir = model_dir(model.id);
    let files = [(model.model_file, model.model_size), (model.mmproj_file, model.mmproj_size)];
    let mut total: u64 = if need_runtime { asset.size } else { 0 };
    total += files.iter().filter(|(f, _)| !dir.join(f).is_file()).map(|(_, s)| s).sum::<u64>();
    let mut base = 0u64;

    if need_runtime {
        let archive = runtime_dir(&cfg.device).join("runtime.tar.gz");
        download::fetch(asset.url, &archive, asset.sha256, |d, _| progress(base + d, total))?;
        base += asset.size;
        unpack(&archive, &runtime_dir(&cfg.device))?;
        let _ = std::fs::remove_file(&archive);
    }
    for (file, size) in files {
        let target = dir.join(file);
        if target.is_file() {
            continue;
        }
        let expected = download::hf_sha256(model.repo, model.revision, file)?;
        download::fetch(&download::hf_url(model.repo, model.revision, file), &target, &expected, |d, _| progress(base + d, total))?;
        base += size;
    }
    progress(total, total);
    Ok(())
}

fn unpack(archive: &Path, dest: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    // `tar` refuses entries that escape the destination (absolute paths, `..`).
    tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(dest).context("cannot unpack llama.cpp runtime")?;
    if !managed_binary_in(dest).is_file() {
        bail!("llama-server missing in the runtime archive");
    }
    Ok(())
}

fn managed_binary_in(runtime: &Path) -> PathBuf {
    runtime.join(format!("llama-{RUNTIME_VERSION}")).join("llama-server")
}

// ---------------------------------------------------------------------------------------------------------------------

struct Running {
    child: Child,
    port: u16,
    key: String,
    signature: String,
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Default)]
struct Inner {
    running: Option<Running>,
    last_used: Option<Instant>,
    keep_alive: Duration,
    busy: usize,
}

/// Owner of the (lazily started) llama-server process.
#[derive(Clone)]
pub struct Server {
    inner: Arc<Mutex<Inner>>,
    /// Processes are spawned by the long-lived reaper thread: PR_SET_PDEATHSIG fires when the *thread* that spawned the child exits.
    spawner: Sender<(Command, Sender<std::io::Result<Child>>)>,
}

impl Server {
    pub fn new() -> Self {
        let inner: Arc<Mutex<Inner>> = Arc::default();
        let weak = Arc::downgrade(&inner);
        let (spawner, requests) = mpsc::channel::<(Command, Sender<std::io::Result<Child>>)>();
        std::thread::spawn(move || loop {
            // Spawns children on request, and stops an idle server so the model leaves RAM.
            match requests.recv_timeout(Duration::from_secs(2)) {
                Ok((mut cmd, reply)) => {
                    let _ = reply.send(cmd.spawn());
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            let Some(inner) = weak.upgrade() else { break };
            let mut g = inner.lock().unwrap();
            let idle = g.last_used.map(|t| t.elapsed() > g.keep_alive).unwrap_or(false);
            if g.running.is_some() && g.busy == 0 && idle {
                g.running = None;
            }
        });
        Self { inner, spawner }
    }

    /// Starts (or reuses) the server. Blocks until it answers `/health`.
    pub fn ensure(&self, cfg: &Config) -> Result<(u16, String)> {
        let q = &cfg.qwen;
        let sig = signature(cfg);
        let mut g = self.inner.lock().unwrap();
        g.keep_alive = Duration::from_secs(q.keep_alive_secs);
        g.last_used = Some(Instant::now());
        if let Some(r) = g.running.as_mut() {
            if r.signature == sig && matches!(r.child.try_wait(), Ok(None)) {
                return Ok((r.port, r.key.clone()));
            }
        }
        g.running = None;
        let running = start(cfg, sig, &self.spawner)?;
        let out = (running.port, running.key.clone());
        g.running = Some(running);
        Ok(out)
    }

    /// Fire and forget start, used at the beginning of a recording.
    pub fn prewarm(&self, cfg: Config) {
        let me = self.clone();
        std::thread::spawn(move || {
            if let Err(e) = me.ensure(&cfg) {
                eprintln!("dictee: qwen warm-up failed: {e:#}");
            }
        });
    }

    pub fn transcribe(&self, cfg: &Config, samples: &[f32]) -> Result<String> {
        let (port, key) = self.ensure(cfg)?;
        self.inner.lock().unwrap().busy += 1;
        let result = (|| {
            let mut parts = Vec::new();
            let context = crate::stt::build_prompt(&cfg.clean_phrases());
            for chunk in split_chunks(samples, MAX_CHUNK_SECONDS) {
                let wav = wav_bytes(chunk);
                let mut text = recognize(port, &key, &wav, &cfg.language, &context)?;
                if !context.is_empty() && echoes_context(&text, &context) {
                    // The model repeated the phrase list instead of transcribing: ask again without context.
                    text = recognize(port, &key, &wav, &cfg.language, "")?;
                }
                if !text.is_empty() {
                    parts.push(text);
                }
            }
            Ok(parts.join(" "))
        })();
        let mut g = self.inner.lock().unwrap();
        g.busy -= 1;
        g.last_used = Some(Instant::now());
        if cfg.qwen.keep_alive_secs == 0 && g.busy == 0 {
            g.running = None;
        }
        result
    }
}

fn signature(cfg: &Config) -> String {
    let q = &cfg.qwen;
    let threads = if q.threads > 0 { q.threads } else { cfg.threads };
    format!("{}|{}|{}|{}|{}", q.model, q.device, q.server_path, threads, models::dir().display())
}

fn start(cfg: &Config, signature: String, spawner: &Sender<(Command, Sender<std::io::Result<Child>>)>) -> Result<Running> {
    let q = &cfg.qwen;
    let model = find(&q.model)?;
    if !model_ready(&q.model) {
        bail!("Qwen model '{}' is not downloaded - open Settings and download it", q.model);
    }
    let binary = server_binary(q)?;
    let own = !q.server_path.trim().is_empty();
    let dir = model_dir(model.id);
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let key = random_key()?;
    let key_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join(format!("dictee-{}", std::process::id()));
    std::fs::create_dir_all(&key_dir)?;
    std::fs::set_permissions(&key_dir, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let key_file = key_dir.join("key");
    {
        use std::io::Write;
        std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&key_file)?.write_all(key.as_bytes())?;
    }
    let threads = (if q.threads > 0 { q.threads } else { cfg.threads }).to_string();
    let gpu = q.device != "cpu";
    let mut cmd = Command::new(&binary);
    cmd.arg("-m").arg(dir.join(model.model_file)).arg("--mmproj").arg(dir.join(model.mmproj_file));
    cmd.args(["--host", "127.0.0.1", "--port", &port.to_string(), "--api-key-file"]).arg(&key_file);
    cmd.args(["--no-webui", "--log-disable", "-c", "4096", "-np", "1", "-t", &threads, "-tb", &threads]);
    if !own {
        cmd.args(["--fit", "off"]); // flag of the pinned release; an own binary may not know it
    }
    if gpu {
        cmd.args(["-ngl", "99"]);
    } else {
        cmd.args(["--device", "none", "-ngl", "0", "--no-mmproj-offload", "--no-op-offload"]);
    }
    cmd.env_clear();
    for var in [
        "PATH", "HOME", "USER", "LANG", "LC_ALL", "XDG_RUNTIME_DIR", "XDG_DATA_HOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "DISPLAY",
        "WAYLAND_DISPLAY", "VK_ICD_FILENAMES", "VK_DRIVER_FILES", "LD_LIBRARY_PATH",
    ] {
        if let Some(v) = std::env::var_os(var) {
            cmd.env(var, v);
        }
    }
    let debug = std::env::var_os("DICTEE_DEBUG").is_some();
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(if debug { Stdio::inherit() } else { Stdio::null() });
    // Die with the daemon, even when it crashes.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
    let (reply_tx, reply_rx) = mpsc::channel();
    spawner.send((cmd, reply_tx)).map_err(|_| anyhow::anyhow!("spawner thread is gone"))?;
    let child = reply_rx.recv()?.with_context(|| format!("cannot start {}", binary.display()))?;
    let mut running = Running { child, port, key, signature };
    let agent = agent(Duration::from_millis(800));
    let deadline = Instant::now() + Duration::from_secs(180);
    let result = loop {
        if let Ok(Some(status)) = running.child.try_wait() {
            break Err(anyhow::anyhow!("llama-server exited at start ({status}); try the CPU device or another model"));
        }
        if Instant::now() > deadline {
            break Err(anyhow::anyhow!("llama-server did not become ready in 180 s"));
        }
        if agent.get(&format!("http://127.0.0.1:{port}/health")).call().is_ok() {
            break Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let _ = std::fs::remove_dir_all(&key_dir); // the server has read the key; do not leave it on disk
    result?;
    Ok(running)
}

fn random_key() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(timeout)).proxy(None).http_status_as_error(true).build().into()
}

fn recognize(port: u16, key: &str, wav: &[u8], language: &str, context: &str) -> Result<String> {
    let body = request_body(wav, language, context);
    let resp = agent(Duration::from_secs(180))
        .post(&format!("http://127.0.0.1:{port}/v1/chat/completions"))
        .header("Authorization", &format!("Bearer {key}"))
        .send_json(&body)
        .context("llama-server request failed")?;
    let v: serde_json::Value = resp.into_body().read_json()?;
    parse_response(&v, language)
}

fn request_body(wav: &[u8], language: &str, context: &str) -> serde_json::Value {
    let audio = base64::engine::general_purpose::STANDARD.encode(wav);
    let mut messages = Vec::new();
    if !context.is_empty() {
        // Qwen3-ASR reads the system prompt as context (names, terms) to bias recognition.
        messages.push(serde_json::json!({"role":"system","content":context}));
    }
    messages.push(serde_json::json!({"role":"user","content":[{"type":"input_audio","input_audio":{"data":audio,"format":"wav"}}]}));
    let mut body = serde_json::json!({"temperature":0,"max_tokens":1024,"stream":false,"cache_prompt":false});
    if let Some((_, name)) = LANGUAGE_NAMES.iter().find(|(code, _)| *code == language) {
        // Forcing the language: continue a prepared assistant answer.
        messages.push(serde_json::json!({"role":"assistant","content":format!("language {name}<asr_text>")}));
        body["continue_final_message"] = true.into();
        body["add_generation_prompt"] = false.into();
    }
    body["messages"] = messages.into();
    body
}

fn parse_response(v: &serde_json::Value, language: &str) -> Result<String> {
    let choice = &v["choices"][0];
    if choice["finish_reason"] == "length" {
        bail!("transcript too long for one request");
    }
    let text = choice["message"]["content"].as_str().context("llama-server returned no text")?;
    let forced = LANGUAGE_NAMES.iter().any(|(code, _)| *code == language);
    let text = match text.split_once("<asr_text>") {
        Some((_, rest)) => rest,
        None if forced => text,
        None => "", // auto mode always starts with "language X<asr_text>"; anything else is not a transcript
    };
    Ok(text.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// True when most words of `text` come from the context list (the model parroted its hint).
fn echoes_context(text: &str, context: &str) -> bool {
    let norm = |w: &str| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase();
    let ctx: std::collections::HashSet<String> = context.split_whitespace().map(norm).collect();
    let words: Vec<String> = text.split_whitespace().map(norm).filter(|w| !w.is_empty()).collect();
    words.len() >= 3 && words.iter().filter(|w| ctx.contains(*w)).count() * 10 >= words.len() * 8
}

/// 16 kHz mono 16-bit WAV bytes.
fn wav_bytes(samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + samples.len() * 2);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    out
}

/// Splits audio into pieces of at most `max_seconds`, cutting at the quietest spot of the last 5 seconds.
fn split_chunks(samples: &[f32], max_seconds: usize) -> Vec<&[f32]> {
    let max = max_seconds * SAMPLE_RATE as usize;
    let window = SAMPLE_RATE as usize / 20; // 50 ms
    let mut out = Vec::new();
    let mut start = 0;
    while samples.len() - start > max {
        let search_from = start + max - 5 * SAMPLE_RATE as usize;
        let (mut best, mut best_energy) = (start + max, f32::MAX);
        let mut pos = search_from;
        while pos + window <= start + max {
            let energy: f32 = samples[pos..pos + window].iter().map(|x| x * x).sum();
            if energy < best_energy {
                best_energy = energy;
                best = pos + window / 2;
            }
            pos += window;
        }
        out.push(&samples[start..best]);
        start = best;
    }
    out.push(&samples[start..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_consistent() {
        for m in MODELS {
            assert_eq!(find(m.id).unwrap().model_file, m.model_file);
            assert!(m.total_mb() > 100);
        }
        assert!(runtime_asset("cpu").is_ok() && runtime_asset("vulkan").is_ok() && runtime_asset("x").is_err());
    }

    #[test]
    fn wav_header_is_valid() {
        let w = wav_bytes(&[0.0, 0.5, -0.5]);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(w.len(), 44 + 6);
        let mut r = hound::WavReader::new(std::io::Cursor::new(w)).unwrap();
        assert_eq!(r.spec().sample_rate, 16000);
        assert_eq!(r.samples::<i16>().count(), 3);
    }

    #[test]
    fn chunks_are_bounded_and_complete() {
        let samples: Vec<f32> = (0..16000 * 100).map(|i| ((i / 8000) % 2) as f32 * 0.5).collect();
        let chunks = split_chunks(&samples, 30);
        assert!(chunks.len() >= 4);
        assert!(chunks.iter().all(|c| c.len() <= 30 * 16000));
        assert_eq!(chunks.iter().map(|c| c.len()).sum::<usize>(), samples.len());
        assert_eq!(split_chunks(&samples[..16000], 30).len(), 1);
    }

    #[test]
    fn parses_language_prefix() {
        let v = serde_json::json!({"choices":[{"message":{"content":"language Czech<asr_text> Dobrý  den."}}]});
        assert_eq!(parse_response(&v, "auto").unwrap(), "Dobrý den.");
        let forced = serde_json::json!({"choices":[{"message":{"content":" Hello there"}}]});
        assert_eq!(parse_response(&forced, "en").unwrap(), "Hello there");
        assert_eq!(parse_response(&forced, "auto").unwrap(), "");
    }

    #[test]
    fn detects_context_echo() {
        let ctx = "Radek Beran, Claude Code, Codex, GitHub.";
        assert!(echoes_context("Radek Beran, Claude Code, Codex, GitHub.", ctx));
        assert!(!echoes_context("Radek Beran opened the GitHub page today", ctx));
        assert!(!echoes_context("Codex", ctx));
    }

    #[test]
    fn forced_language_uses_prefix_message() {
        let b = request_body(b"x", "cs", "");
        assert_eq!(b["messages"].as_array().unwrap().len(), 2);
        assert_eq!(b["messages"][1]["content"], "language Czech<asr_text>");
        assert_eq!(request_body(b"x", "auto", "")["messages"].as_array().unwrap().len(), 1);
        let with_ctx = request_body(b"x", "auto", "Keycloak.");
        assert_eq!(with_ctx["messages"][0]["role"], "system");
        assert_eq!(with_ctx["messages"].as_array().unwrap().len(), 2);
    }
}
