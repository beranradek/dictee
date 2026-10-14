//! Whisper GGML model catalog and downloader (Hugging Face, ggerganov/whisper.cpp).

use crate::download;
use anyhow::{Context, Result};
use std::path::PathBuf;

pub struct ModelInfo {
    pub id: &'static str,
    pub file: &'static str,
    pub label: &'static str,
    pub size_mb: u32,
}

pub const MODELS: &[ModelInfo] = &[
    ModelInfo { id: "tiny", file: "ggml-tiny.bin", label: "Tiny", size_mb: 78 },
    ModelInfo { id: "base", file: "ggml-base.bin", label: "Base", size_mb: 148 },
    ModelInfo { id: "small-q5", file: "ggml-small-q5_1.bin", label: "Small (compressed)", size_mb: 190 },
    ModelInfo { id: "small", file: "ggml-small.bin", label: "Small (default)", size_mb: 488 },
    ModelInfo { id: "turbo-q5", file: "ggml-large-v3-turbo-q5_0.bin", label: "Large v3 Turbo (compressed)", size_mb: 574 },
    ModelInfo { id: "turbo", file: "ggml-large-v3-turbo.bin", label: "Large v3 Turbo", size_mb: 1624 },
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
pub fn download(id: &str, progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    let info = find(id)?;
    let target = path(id)?;
    if target.is_file() {
        return Ok(target);
    }
    let expected = download::hf_sha256(REPO, "main", info.file)?;
    download::fetch(&download::hf_url(REPO, "main", info.file), &target, &expected, progress)?;
    Ok(target)
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
}
