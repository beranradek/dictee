//! Whisper GGML model catalog and downloader (Hugging Face, ggerganov/whisper.cpp).

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::PathBuf;

pub struct ModelInfo {
    pub id: &'static str,
    pub file: &'static str,
    pub label: &'static str,
    pub size_mb: u32,
}

pub const MODELS: &[ModelInfo] = &[
    ModelInfo { id: "tiny", file: "ggml-tiny.bin", label: "Tiny - rough Czech", size_mb: 78 },
    ModelInfo { id: "base", file: "ggml-base.bin", label: "Base - basic", size_mb: 148 },
    ModelInfo { id: "small-q5", file: "ggml-small-q5_1.bin", label: "Small compressed - balanced", size_mb: 190 },
    ModelInfo { id: "small", file: "ggml-small.bin", label: "Small - good (default)", size_mb: 488 },
    ModelInfo { id: "turbo-q5", file: "ggml-large-v3-turbo-q5_0.bin", label: "Turbo compressed - best Czech", size_mb: 574 },
    ModelInfo { id: "turbo", file: "ggml-large-v3-turbo.bin", label: "Turbo - best, slow on CPU", size_mb: 1624 },
];

const REPO: &str = "ggerganov/whisper.cpp";

pub fn find(id: &str) -> Result<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id).with_context(|| format!("unknown model '{id}'"))
}

pub fn default_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("dictee").join("models")
}

/// Configured models folder (`~` is expanded), or the default one.
pub fn dir() -> PathBuf {
    let configured = crate::config::Config::load().map(|c| c.models_dir).unwrap_or_default();
    resolve_dir(configured.trim())
}

fn resolve_dir(configured: &str) -> PathBuf {
    if configured.is_empty() {
        return default_dir();
    }
    match configured.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(configured),
    }
}

pub fn path(id: &str) -> Result<PathBuf> {
    Ok(dir().join(find(id)?.file))
}

pub fn is_downloaded(id: &str) -> bool {
    path(id).map(|p| p.is_file()).unwrap_or(false)
}

/// Downloads a model, verifies its SHA-256 against the Hugging Face LFS metadata, and reports progress.
pub fn download(id: &str, mut progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    let info = find(id)?;
    let target = path(id)?;
    if target.is_file() {
        return Ok(target);
    }
    std::fs::create_dir_all(dir())?;
    let expected = expected_sha256(info.file)?;
    let url = format!("https://huggingface.co/{REPO}/resolve/main/{}", info.file);
    let resp = ureq::get(&url).call().with_context(|| format!("download failed: {url}"))?;
    let total: u64 = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(0);
    let tmp = target.with_extension("bin.part");
    let mut out = std::fs::File::create(&tmp)?;
    let mut body = resp.into_body().into_reader();
    let (mut hasher, mut done, mut buf) = (Sha256::new(), 0u64, vec![0u8; 256 * 1024]);
    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        hasher.update(&buf[..n]);
        done += n as u64;
        progress(done, total);
    }
    out.flush()?;
    let got = hex(&hasher.finalize());
    if got != expected {
        let _ = std::fs::remove_file(&tmp);
        bail!("checksum mismatch for {} (expected {expected}, got {got})", info.file);
    }
    std::fs::rename(&tmp, &target)?;
    Ok(target)
}

fn expected_sha256(file: &str) -> Result<String> {
    let url = format!("https://huggingface.co/api/models/{REPO}/tree/main");
    let text = ureq::get(&url).call()?.into_body().read_to_string()?;
    parse_sha256(&text, file)
}

fn parse_sha256(json: &str, file: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    v.as_array()
        .and_then(|a| a.iter().find(|e| e["path"] == file))
        .and_then(|e| e["lfs"]["oid"].as_str())
        .map(str::to_string)
        .with_context(|| format!("no checksum published for {file}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_unique_and_found() {
        for m in MODELS {
            assert_eq!(find(m.id).unwrap().file, m.file);
        }
        assert!(find("nope").is_err());
    }

    #[test]
    fn resolves_models_dir() {
        assert_eq!(resolve_dir(""), default_dir());
        assert_eq!(resolve_dir("/mnt/data/models"), PathBuf::from("/mnt/data/models"));
        assert!(resolve_dir("~/m").ends_with("m") && !resolve_dir("~/m").starts_with("~"));
    }

    #[test]
    fn parses_lfs_oid() {
        let json = r#"[{"path":"a.bin","lfs":{"oid":"abc"}},{"path":"b.bin","lfs":{"oid":"def"}},{"path":"c.md"}]"#;
        assert_eq!(parse_sha256(json, "b.bin").unwrap(), "def");
        assert!(parse_sha256(json, "c.md").is_err());
    }
}
