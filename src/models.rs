//! Whisper GGML model catalog and downloader (Hugging Face, ggerganov/whisper.cpp).

use crate::download::RemoteFile;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

pub struct ModelInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub file: RemoteFile,
}

impl ModelInfo {
    pub fn size_mb(&self) -> u64 {
        self.file.size / 1_000_000
    }
}

const REPO: &str = "ggerganov/whisper.cpp";
const REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

const fn whisper(id: &'static str, label: &'static str, name: &'static str, size: u64, sha256: &'static str) -> ModelInfo {
    ModelInfo { id, label, file: RemoteFile { repo: REPO, revision: REVISION, name, size, sha256 } }
}

pub const MODELS: &[ModelInfo] = &[
    whisper("tiny", "Tiny", "ggml-tiny.bin", 77_691_713, "be07e048e1e599ad46341c8d2a135645097a538221678b7acdd1b1919c6e1b21"),
    whisper("base", "Base", "ggml-base.bin", 147_951_465, "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe"),
    whisper(
        "small-q5",
        "Small (compressed)",
        "ggml-small-q5_1.bin",
        190_085_487,
        "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
    ),
    whisper("small", "Small (default)", "ggml-small.bin", 487_601_967, "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b"),
    whisper(
        "turbo-q5",
        "Large v3 Turbo (compressed)",
        "ggml-large-v3-turbo-q5_0.bin",
        574_041_195,
        "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    ),
    whisper(
        "turbo",
        "Large v3 Turbo",
        "ggml-large-v3-turbo.bin",
        1_624_555_275,
        "1fc70f774d38eb169993ac391eea357ef47c88757ef72ee5943879b7e8e2bc69",
    ),
];

pub fn find(id: &str) -> Result<&'static ModelInfo> {
    MODELS.iter().find(|m| m.id == id).with_context(|| format!("unknown model '{id}'"))
}

pub fn path(dir: &Path, id: &str) -> Result<PathBuf> {
    Ok(dir.join(find(id)?.file.name))
}

pub fn is_downloaded(dir: &Path, id: &str) -> bool {
    find(id).map(|m| m.file.is_in(dir)).unwrap_or(false)
}

/// Downloads a model (checksum verified, resumable) unless it is already in `dir`.
pub fn download(dir: &Path, id: &str, progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
    find(id)?.file.fetch_into(dir, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_unique_and_found() {
        for m in MODELS {
            assert_eq!(find(m.id).unwrap().file.name, m.file.name);
            assert_eq!(m.file.sha256.len(), 64);
        }
        assert!(find("nope").is_err());
    }
}
