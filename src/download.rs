//! Verified file downloads (Hugging Face and pinned release URLs).

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::Path;

/// SHA-256 of a file in a Hugging Face repo, from the LFS metadata of the repo tree.
pub fn hf_sha256(repo: &str, revision: &str, file: &str) -> Result<String> {
    let url = format!("https://huggingface.co/api/models/{repo}/tree/{revision}");
    let text = ureq::get(&url).call().with_context(|| format!("cannot reach {url}"))?.into_body().read_to_string()?;
    parse_hf_sha256(&text, file)
}

pub fn parse_hf_sha256(json: &str, file: &str) -> Result<String> {
    let v: serde_json::Value = serde_json::from_str(json)?;
    v.as_array()
        .and_then(|a| a.iter().find(|e| e["path"] == file))
        .and_then(|e| e["lfs"]["oid"].as_str())
        .map(str::to_string)
        .with_context(|| format!("no checksum published for {file}"))
}

pub fn hf_url(repo: &str, revision: &str, file: &str) -> String {
    format!("https://huggingface.co/{repo}/resolve/{revision}/{file}")
}

/// Downloads `url` to `target` (via a `.part` file), verifies SHA-256, reports `(bytes_done, bytes_total)`.
pub fn fetch(url: &str, target: &Path, expected_sha256: &str, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let resp = ureq::get(url).call().with_context(|| format!("download failed: {url}"))?;
    let total: u64 = resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(0);
    let tmp = target.with_extension("part");
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
    if got != expected_sha256 {
        let _ = std::fs::remove_file(&tmp);
        bail!("checksum mismatch for {url} (expected {expected_sha256}, got {got})");
    }
    std::fs::rename(&tmp, target)?;
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lfs_oid() {
        let json = r#"[{"path":"a.bin","lfs":{"oid":"abc"}},{"path":"b.bin","lfs":{"oid":"def"}},{"path":"c.md"}]"#;
        assert_eq!(parse_hf_sha256(json, "b.bin").unwrap(), "def");
        assert!(parse_hf_sha256(json, "c.md").is_err());
    }
}
