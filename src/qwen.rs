//! Qwen3-ASR speech recognition through a managed llama.cpp `llama-server` on loopback.
//!
//! The server starts when a recording starts (warm-up while the user speaks), stays alive for a configurable time,
//! listens only on 127.0.0.1 with a random API key, and dies together with the daemon.

use crate::config::{Config, QwenCfg};
use crate::download::{self, RemoteFile};
use crate::{audio, options, util};
use anyhow::{Context, Result, bail};
use base64::Engine;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const RUNTIME_VERSION: &str = "b11011";
/// Longest audio piece sent in one request (keeps the 4096 token context safe).
const MAX_CHUNK_SECONDS: usize = 30;
/// A warm server is kept at least this long while the user records (the recording cap plus a margin).
const WARM_MAX: Duration = Duration::from_secs(audio::MAX_SECONDS as u64 + 5 * 60);
const SERVER_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LANG",
    "LC_ALL",
    "XDG_RUNTIME_DIR",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_CONFIG_HOME",
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "VK_ICD_FILENAMES",
    "VK_DRIVER_FILES",
    "LD_LIBRARY_PATH",
];

pub struct QwenModel {
    pub id: &'static str,
    pub label: &'static str,
    pub model: RemoteFile,
    pub mmproj: RemoteFile,
    pub ram_gb: f32,
}

impl QwenModel {
    pub fn total_mb(&self) -> u64 {
        (self.model.size + self.mmproj.size) / 1_000_000
    }
}

const fn file(repo: &'static str, revision: &'static str, name: &'static str, size: u64, sha256: &'static str) -> RemoteFile {
    RemoteFile { repo, revision, name, size, sha256 }
}

const REV_0_6B: &str = "928ab958557df9aa2ef1c93e0e83c7ad0933fae2";
const REV_1_7B_Q8: &str = "36a678687ba7d07a74ca70ccb0e36902e005fb80";
const REV_1_7B_Q4: &str = "bdff9f9b31f387d6eb797a8d827c08904048c57c";
const REPO_1_7B_Q4: &str = "getonit/Qwen3-ASR-1.7B-Q4_0-GGUF";

pub const MODELS: &[QwenModel] = &[
    QwenModel {
        id: "qwen3-0.6b-q8",
        label: "0.6B Q8 - small, fast",
        model: file(
            "ggml-org/Qwen3-ASR-0.6B-GGUF",
            REV_0_6B,
            "Qwen3-ASR-0.6B-Q8_0.gguf",
            804_749_248,
            "bca259818b50ca7c4c05e9bdb35a5dc04fa039653a6d6f3f0f331f96f6aa1971",
        ),
        mmproj: file(
            "ggml-org/Qwen3-ASR-0.6B-GGUF",
            REV_0_6B,
            "mmproj-Qwen3-ASR-0.6B-Q8_0.gguf",
            214_392_480,
            "41a342b5e4c514e968cb756de6cd1b7be39eff43c44c57a2ef5fc6522e36603d",
        ),
        ram_gb: 2.0,
    },
    QwenModel {
        id: "qwen3-1.7b-q4",
        label: "1.7B Q4 - recommended",
        model: file(
            REPO_1_7B_Q4,
            REV_1_7B_Q4,
            "Qwen3-ASR-1.7B-Q4_0.gguf",
            1_229_449_152,
            "adf2c487fcce3993bb30c9406748f5ef111a3a28b0a6c26f34223185011427c7",
        ),
        mmproj: file(
            REPO_1_7B_Q4,
            REV_1_7B_Q4,
            "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf",
            355_709_344,
            "46c1d533af3f354ceb37ce855dbceff7da7fa7cf1e6a523df3b13440bd164c0d",
        ),
        ram_gb: 3.5,
    },
    QwenModel {
        id: "qwen3-1.7b-q8",
        label: "1.7B Q8 - best quality",
        model: file(
            "ggml-org/Qwen3-ASR-1.7B-GGUF",
            REV_1_7B_Q8,
            "Qwen3-ASR-1.7B-Q8_0.gguf",
            2_165_034_944,
            "58e22d0532d4eacaf034cfac17a6fed159f37c41390c710186783be439d1fc57",
        ),
        mmproj: file(
            "ggml-org/Qwen3-ASR-1.7B-GGUF",
            REV_1_7B_Q8,
            "mmproj-Qwen3-ASR-1.7B-Q8_0.gguf",
            355_709_344,
            "46c1d533af3f354ceb37ce855dbceff7da7fa7cf1e6a523df3b13440bd164c0d",
        ),
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

// Paths. `root` is the configured models folder (`Config::models_path`).

pub fn model_dir(root: &Path, id: &str) -> PathBuf {
    root.join("qwen").join(id)
}

fn runtime_dir(root: &Path, device: &str) -> PathBuf {
    root.join("qwen").join("runtime").join(format!("{device}-{RUNTIME_VERSION}"))
}

/// The managed `llama-server` binary (the tarball has one top folder, `llama-<version>`).
fn managed_binary(root: &Path, device: &str) -> PathBuf {
    runtime_dir(root, device).join(format!("llama-{RUNTIME_VERSION}")).join("llama-server")
}

/// The server to run: the user's own binary, or the managed download.
pub fn server_binary(root: &Path, cfg: &QwenCfg) -> Result<PathBuf> {
    let own = cfg.server_path.trim();
    let path = if own.is_empty() { managed_binary(root, &cfg.device) } else { PathBuf::from(own) };
    if !path.is_file() {
        bail!("llama-server not found at {} - download the runtime in Settings", path.display());
    }
    Ok(path)
}

pub fn model_ready(root: &Path, id: &str) -> bool {
    find(id).map(|m| m.model.is_in(&model_dir(root, id)) && m.mmproj.is_in(&model_dir(root, id))).unwrap_or(false)
}

pub fn ready(root: &Path, cfg: &QwenCfg) -> bool {
    model_ready(root, &cfg.model) && server_binary(root, cfg).is_ok()
}

/// Downloads the runtime (unless the user set an own server path) and the model files.
/// `progress` gets `(bytes_done, bytes_total)` over everything.
pub fn download_all(root: &Path, cfg: &QwenCfg, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let model = find(&cfg.model)?;
    let asset = runtime_asset(&cfg.device)?;
    let need_runtime = cfg.server_path.trim().is_empty() && !managed_binary(root, &cfg.device).is_file();
    let dir = model_dir(root, model.id);
    let files: Vec<&RemoteFile> = [&model.model, &model.mmproj].into_iter().filter(|f| !f.is_in(&dir)).collect();
    let total = if need_runtime { asset.size } else { 0 } + files.iter().map(|f| f.size).sum::<u64>();
    let mut base = 0u64;

    if need_runtime {
        let runtime = runtime_dir(root, &cfg.device);
        let archive = runtime.with_extension("tar.gz");
        download::fetch(asset.url, &archive, asset.sha256, asset.size, |d, _| progress(d, total))?;
        base += asset.size;
        let unpacked = unpack(&archive, &runtime);
        let _ = std::fs::remove_file(&archive);
        unpacked?;
    }
    for f in files {
        f.fetch_into(&dir, |d, _| progress(base + d, total))?;
        base += f.size;
    }
    progress(total, total);
    Ok(())
}

/// Unpacks into a staging folder and renames it, so a failed unpack never leaves a half-installed runtime.
fn unpack(archive: &Path, dest: &Path) -> Result<()> {
    let stage = dest.with_extension("stage");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)?;
    let result = (|| {
        let file = std::fs::File::open(archive)?;
        // `tar` refuses entries that escape the destination (absolute paths, `..`).
        tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(&stage).context("cannot unpack llama.cpp runtime")?;
        if !stage.join(format!("llama-{RUNTIME_VERSION}")).join("llama-server").is_file() {
            bail!("llama-server missing in the runtime archive");
        }
        let _ = std::fs::remove_dir_all(dest);
        std::fs::rename(&stage, dest)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&stage);
    }
    result
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
    /// Number of leases (requests, or warm-ups in progress)
    busy: usize,
    /// Set by a warm-up: the server is meant for the recording that is in progress
    warm_since: Option<Instant>,
}

type SpawnRequest = (Command, Sender<std::io::Result<Child>>);

/// Owner of the (lazily started) llama-server process.
#[derive(Clone)]
pub struct Server {
    inner: Arc<Mutex<Inner>>,
    /// Serializes server starts, so `inner` is never held while the (slow) start runs.
    start_lock: Arc<Mutex<()>>,
    /// Processes are spawned by the long-lived reaper thread: PR_SET_PDEATHSIG fires when the *thread* that spawned the child exits.
    spawner: Sender<SpawnRequest>,
}

/// A running server that cannot be stopped while the lease exists.
struct Lease {
    inner: Arc<Mutex<Inner>>,
    port: u16,
    key: String,
    stop_when_unused: bool,
}

impl Drop for Lease {
    fn drop(&mut self) {
        let mut g = self.inner.lock().unwrap();
        g.busy -= 1;
        g.last_used = Some(Instant::now());
        if self.stop_when_unused && g.busy == 0 {
            g.running = None;
        }
    }
}

impl Server {
    pub fn new() -> Self {
        let inner: Arc<Mutex<Inner>> = Arc::default();
        let weak = Arc::downgrade(&inner);
        let (spawner, requests) = mpsc::channel::<SpawnRequest>();
        std::thread::spawn(move || {
            loop {
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
                let warm = g.warm_since.is_some_and(|t| t.elapsed() < WARM_MAX);
                let idle = g.last_used.is_some_and(|t| t.elapsed() > g.keep_alive);
                if g.running.is_some() && g.busy == 0 && idle && !warm {
                    g.running = None;
                }
            }
        });
        Self { inner, start_lock: Arc::default(), spawner }
    }

    /// Starts (or reuses) the server and returns a lease. Blocks until the server answers `/health`.
    /// A warm-up that was cancelled while it waited for its turn returns `None` and starts nothing.
    fn acquire(&self, cfg: &Config, warm: bool) -> Result<Option<Lease>> {
        let sig = signature(cfg);
        let _one_start_at_a_time = self.start_lock.lock().unwrap();
        {
            let mut g = self.inner.lock().unwrap();
            if warm && g.warm_since.is_none() {
                return Ok(None);
            }
            g.keep_alive = Duration::from_secs(cfg.qwen.keep_alive_secs);
            g.last_used = Some(Instant::now());
            if !warm {
                g.warm_since = None; // a real request ends the warm-up phase
            }
            if g.running.as_mut().is_some_and(|r| r.signature == sig && matches!(r.child.try_wait(), Ok(None))) {
                return Ok(Some(self.lease(&mut g, warm, cfg)));
            }
            g.running = None; // stops a server that is dead or configured differently
        }
        let running = start(cfg, sig, &self.spawner)?; // slow: `inner` is not held, so cancel and stop stay responsive
        let mut g = self.inner.lock().unwrap();
        g.running = Some(running);
        Ok(Some(self.lease(&mut g, warm, cfg)))
    }

    fn lease(&self, g: &mut Inner, warm: bool, cfg: &Config) -> Lease {
        let r = g.running.as_ref().expect("server is running");
        g.busy += 1;
        Lease { inner: self.inner.clone(), port: r.port, key: r.key.clone(), stop_when_unused: !warm && cfg.qwen.keep_alive_secs == 0 }
    }

    /// Fire and forget start, used at the beginning of a recording. The warm flag is set here, on the caller's thread, so a
    /// later `cancel_warmup` always wins over a warm-up that has not started yet.
    pub fn prewarm(&self, cfg: Config) {
        self.inner.lock().unwrap().warm_since = Some(Instant::now());
        let me = self.clone();
        std::thread::spawn(move || {
            if let Err(e) = me.acquire(&cfg, true) {
                eprintln!("dictee: qwen warm-up failed: {e:#}");
            }
        });
    }

    /// The recording ended or was cancelled: the warm server may now follow the normal keep-alive rule.
    pub fn cancel_warmup(&self) {
        self.inner.lock().unwrap().warm_since = None;
    }

    pub fn transcribe(&self, cfg: &Config, samples: &[f32]) -> Result<String> {
        let lease = self.acquire(cfg, false)?.expect("a real request always gets a lease");
        let context = crate::stt::build_prompt(&cfg.clean_phrases());
        let mut parts = Vec::new();
        for chunk in split_chunks(samples, MAX_CHUNK_SECONDS) {
            let wav = wav_bytes(chunk);
            let mut text = recognize(&lease, &wav, &cfg.language, &context)?;
            if !context.is_empty() && echoes_context(&text, &context) {
                // The model repeated the phrase list instead of transcribing: ask again without context.
                text = recognize(&lease, &wav, &cfg.language, "")?;
            }
            if !text.is_empty() {
                parts.push(text);
            }
        }
        Ok(parts.join(" "))
    }
}

fn signature(cfg: &Config) -> String {
    let q = &cfg.qwen;
    format!("{}|{}|{}|{}|{}", q.model, q.device, q.server_path, threads(cfg), cfg.models_path().display())
}

fn threads(cfg: &Config) -> i32 {
    if cfg.qwen.threads > 0 { cfg.qwen.threads } else { cfg.threads }
}

fn start(cfg: &Config, signature: String, spawner: &Sender<SpawnRequest>) -> Result<Running> {
    let q = &cfg.qwen;
    let root = cfg.models_path();
    let model = find(&q.model)?;
    if !model_ready(&root, &q.model) {
        bail!("Qwen model '{}' is not downloaded - open Settings and download it", q.model);
    }
    let binary = server_binary(&root, q)?;
    let dir = model_dir(&root, model.id);
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    let key = random_key()?;
    // The key file lives in a private folder and is removed as soon as the server has read it.
    let key_base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let key_dir = tempfile::Builder::new().prefix("dictee-").tempdir_in(key_base)?;
    let key_file = key_dir.path().join("key");
    {
        use std::io::Write;
        std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&key_file)?.write_all(key.as_bytes())?;
    }
    let mut cmd = Command::new(&binary);
    cmd.arg("-m").arg(dir.join(model.model.name)).arg("--mmproj").arg(dir.join(model.mmproj.name));
    cmd.args(["--host", "127.0.0.1", "--port", &port.to_string(), "--api-key-file"]).arg(&key_file);
    let t = threads(cfg).to_string();
    cmd.args(["--no-webui", "--log-disable", "-c", "4096", "-np", "1", "-t", &t, "-tb", &t]);
    if q.server_path.trim().is_empty() {
        cmd.args(["--fit", "off"]); // flag of the pinned release; an own binary may not know it
    }
    if q.device == "cpu" {
        cmd.args(["--device", "none", "-ngl", "0", "--no-mmproj-offload", "--no-op-offload"]);
    } else {
        cmd.args(["-ngl", "99"]);
    }
    util::restrict_env(&mut cmd, SERVER_ENV);
    let debug = std::env::var_os("DICTEE_DEBUG").is_some();
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(if debug { Stdio::inherit() } else { Stdio::null() });
    util::die_with_parent(&mut cmd); // dies with the daemon, even when it crashes
    let (reply_tx, reply_rx) = mpsc::channel();
    spawner.send((cmd, reply_tx)).map_err(|_| anyhow::anyhow!("spawner thread is gone"))?;
    let child = reply_rx.recv()?.with_context(|| format!("cannot start {}", binary.display()))?;
    let running = Running { child, port, key, signature }; // dropping it kills the child on every error below
    wait_healthy(running, Duration::from_secs(180))
}

fn wait_healthy(mut running: Running, limit: Duration) -> Result<Running> {
    let agent = util::http_agent(Duration::from_millis(800), false);
    let deadline = Instant::now() + limit;
    loop {
        if let Ok(Some(status)) = running.child.try_wait() {
            bail!("llama-server exited at start ({status}); try the CPU device or another model");
        }
        if Instant::now() > deadline {
            bail!("llama-server did not become ready in {} s", limit.as_secs());
        }
        if agent.get(&format!("http://127.0.0.1:{}/health", running.port)).call().is_ok() {
            return Ok(running);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn random_key() -> Result<String> {
    use std::io::Read;
    let mut b = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut b)?;
    Ok(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn recognize(lease: &Lease, wav: &[u8], language: &str, context: &str) -> Result<String> {
    let body = request_body(wav, language, context);
    let resp = util::http_agent(Duration::from_secs(180), false)
        .post(&format!("http://127.0.0.1:{}/v1/chat/completions", lease.port))
        .header("Authorization", &format!("Bearer {}", lease.key))
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
    if let Some(name) = forced_language(language) {
        // Forcing the language: continue a prepared assistant answer.
        messages.push(serde_json::json!({"role":"assistant","content":format!("language {name}<asr_text>")}));
        body["continue_final_message"] = true.into();
        body["add_generation_prompt"] = false.into();
    }
    body["messages"] = messages.into();
    body
}

fn forced_language(id: &str) -> Option<&'static str> {
    options::language(id).and_then(|l| l.qwen_name)
}

fn parse_response(v: &serde_json::Value, language: &str) -> Result<String> {
    let choice = &v["choices"][0];
    if choice["finish_reason"] == "length" {
        bail!("transcript too long for one request");
    }
    let text = choice["message"]["content"].as_str().context("llama-server returned no text")?;
    let text = match text.split_once("<asr_text>") {
        Some((_, rest)) => rest,
        None if forced_language(language).is_some() => text,
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
    out.extend_from_slice(&audio::SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(audio::SAMPLE_RATE * 2).to_le_bytes());
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
    let max = max_seconds * audio::SAMPLE_RATE as usize;
    let window = audio::SAMPLE_RATE as usize / 20; // 50 ms
    let mut out = Vec::new();
    let mut start = 0;
    while samples.len() - start > max {
        let search_from = start + max - 5 * audio::SAMPLE_RATE as usize;
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
            assert_eq!(find(m.id).unwrap().model.name, m.model.name);
            assert_eq!(m.model.sha256.len(), 64);
            assert_eq!(m.mmproj.sha256.len(), 64);
            assert!(m.total_mb() > 100);
        }
        assert!(runtime_asset("cpu").is_ok() && runtime_asset("vulkan").is_ok() && runtime_asset("x").is_err());
        assert!(!ready(Path::new("/nonexistent"), &QwenCfg::default()));
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
