//! Verified, resumable file downloads. Every file is pinned by commit and SHA-256, so a changed remote cannot slip in.

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_ATTEMPTS: u32 = 5;

/// A file of a Hugging Face repo at a fixed commit.
pub struct RemoteFile {
    pub repo: &'static str,
    pub revision: &'static str,
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

impl RemoteFile {
    pub fn url(&self) -> String {
        format!("https://huggingface.co/{}/resolve/{}/{}", self.repo, self.revision, self.name)
    }

    pub fn is_in(&self, dir: &Path) -> bool {
        dir.join(self.name).is_file()
    }

    /// Downloads into `dir` unless the file is already there.
    pub fn fetch_into(&self, dir: &Path, progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
        let target = dir.join(self.name);
        if !target.is_file() {
            fetch(&self.url(), &target, self.sha256, progress)?;
        }
        Ok(target)
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(30)))
        .timeout_recv_response(Some(Duration::from_secs(60)))
        // Per attempt: a stalled connection fails after this and the next attempt resumes from the partial file.
        .timeout_recv_body(Some(Duration::from_secs(15 * 60)))
        .build()
        .into()
}

fn part_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().expect("file name").to_os_string();
    name.push(".part");
    target.with_file_name(name)
}

/// Downloads `url` to `target` through a `.part` file (resuming after a failure), checks SHA-256, reports `(done, total)` bytes.
pub fn fetch(url: &str, target: &Path, expected_sha256: &str, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = part_path(target);
    let agent = agent();
    let mut last_error = None;
    for attempt in 1..=MAX_ATTEMPTS {
        match attempt_download(&agent, url, &part, &mut progress) {
            Ok(()) => {
                let got = sha256_of(&part)?;
                if got != expected_sha256 {
                    let _ = std::fs::remove_file(&part);
                    bail!("checksum mismatch for {url} (expected {expected_sha256}, got {got})");
                }
                std::fs::rename(&part, target)?;
                return Ok(());
            }
            Err(e) => {
                last_error = Some(e);
                std::thread::sleep(Duration::from_secs(u64::from(attempt) * 2));
            }
        }
    }
    Err(last_error.expect("at least one attempt")).with_context(|| format!("download failed: {url}"))
}

fn attempt_download(agent: &ureq::Agent, url: &str, part: &Path, progress: &mut impl FnMut(u64, u64)) -> Result<()> {
    let have = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    let mut request = agent.get(url);
    if have > 0 {
        request = request.header("Range", &format!("bytes={have}-"));
    }
    let response = match request.call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(416)) => return Ok(()), // the partial file is already complete
        Err(e) => return Err(e.into()),
    };
    let resumed = have > 0 && response.status().as_u16() == 206;
    let remaining: u64 = response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut done = if resumed { have } else { 0 };
    let total = if remaining > 0 { done + remaining } else { 0 };
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(!resumed).open(part)?;
    if resumed {
        file.seek(SeekFrom::End(0))?;
    }
    let mut body = response.into_body().into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])?;
        done += n as u64;
        progress(done, total);
    }
    file.flush()?;
    Ok(())
}

pub fn sha256_of(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let (mut hasher, mut buf) = (Sha256::new(), vec![0u8; 1024 * 1024]);
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// Minimal HTTP server for one file; understands `Range: bytes=N-`. Returns the base URL.
    fn serve(content: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut start, mut line) = (0usize, String::new());
                while reader.read_line(&mut line).is_ok() && line.trim() != "" {
                    if let Some(r) = line.to_lowercase().strip_prefix("range: bytes=") {
                        start = r.trim().trim_end_matches('-').parse().unwrap_or(0);
                    }
                    line.clear();
                }
                let (status, body) = if start > 0 { ("206 Partial Content", &content[start..]) } else { ("200 OK", content) };
                let head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body);
            }
        });
        format!("http://127.0.0.1:{port}/f.bin")
    }

    const CONTENT: &[u8] = &[7u8; 100_000];

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn downloads_and_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.bin");
        let mut last = (0, 0);
        fetch(&serve(CONTENT), &target, &sha(CONTENT), |d, t| last = (d, t)).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), CONTENT);
        assert_eq!(last, (100_000, 100_000));
        assert!(!part_path(&target).exists());
    }

    #[test]
    fn resumes_a_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.bin");
        std::fs::write(part_path(&target), &CONTENT[..40_000]).unwrap();
        fetch(&serve(CONTENT), &target, &sha(CONTENT), |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), CONTENT);
    }

    #[test]
    fn wrong_checksum_is_rejected_and_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.bin");
        let err = fetch(&serve(CONTENT), &target, "00", |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
        assert!(!target.exists() && !part_path(&target).exists());
    }
}
