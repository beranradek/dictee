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

    /// True when the complete file is there (checked by size; the checksum was verified when it was downloaded).
    pub fn is_in(&self, dir: &Path) -> bool {
        std::fs::metadata(dir.join(self.name)).is_ok_and(|m| m.is_file() && m.len() == self.size)
    }

    /// Downloads into `dir` unless the file is already there.
    pub fn fetch_into(&self, dir: &Path, progress: impl FnMut(u64, u64)) -> Result<PathBuf> {
        let target = dir.join(self.name);
        if !self.is_in(dir) {
            fetch(&self.url(), &target, self.sha256, self.size, progress)?;
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

/// How an attempt failed: transient errors are retried (resuming), permanent ones end the download at once.
enum Failure {
    Retry(anyhow::Error),
    Permanent(anyhow::Error),
}

impl<E: Into<anyhow::Error>> From<E> for Failure {
    fn from(e: E) -> Self {
        Failure::Retry(e.into())
    }
}

/// Downloads `url` to `target` through a `.part` file (resuming after a failure), checks SHA-256, reports `(done, total)` bytes.
/// `max_size` is the expected size: a longer body is rejected instead of filling the disk.
pub fn fetch(url: &str, target: &Path, expected_sha256: &str, max_size: u64, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = part_path(target);
    let agent = agent();
    let mut last_error = None;
    for attempt in 1..=MAX_ATTEMPTS {
        match attempt_download(&agent, url, &part, max_size, &mut progress) {
            Ok(()) => {
                let got = sha256_of(&part)?;
                if got != expected_sha256 {
                    let _ = std::fs::remove_file(&part);
                    bail!("checksum mismatch for {url} (expected {expected_sha256}, got {got})");
                }
                std::fs::rename(&part, target)?;
                return Ok(());
            }
            Err(Failure::Permanent(e)) => return Err(e).with_context(|| format!("download failed: {url}")),
            Err(Failure::Retry(e)) => {
                last_error = Some(e);
                std::thread::sleep(Duration::from_secs(u64::from(attempt) * 2));
            }
        }
    }
    Err(last_error.expect("at least one attempt")).with_context(|| format!("download failed: {url}"))
}

fn attempt_download(
    agent: &ureq::Agent,
    url: &str,
    part: &Path,
    max_size: u64,
    progress: &mut impl FnMut(u64, u64),
) -> Result<(), Failure> {
    use std::os::fd::AsRawFd;
    let permanent = |msg: &str| Failure::Permanent(anyhow::anyhow!("{msg}"));
    let mut file = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(part)?;
    // One downloader per partial file: the CLI and the settings window must not write to it at the same time.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(permanent("another dictee process is already downloading this file"));
    }
    let mut have = file.metadata()?.len();
    if have > max_size {
        file.set_len(0)?;
        have = 0;
    }
    let mut request = agent.get(url);
    if have > 0 {
        request = request.header("Range", &format!("bytes={have}-"));
    }
    let response = match request.call() {
        Ok(r) => r,
        Err(ureq::Error::StatusCode(416)) => return Ok(()), // the partial file is already complete
        // Client errors will not go away by waiting (404, 403 ...), except timeouts and rate limits.
        Err(ureq::Error::StatusCode(code)) if (400..500).contains(&code) && code != 408 && code != 429 => {
            return Err(Failure::Permanent(anyhow::anyhow!("server answered HTTP {code}")));
        }
        Err(e) => return Err(e.into()),
    };
    let status = response.status().as_u16();
    let range_ok =
        response.headers().get("content-range").and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with(&format!("bytes {have}-")));
    let resumed = have > 0 && status == 206 && range_ok;
    if have > 0 && !resumed {
        file.set_len(0)?; // the server ignored or changed the range: start over
        if status == 206 {
            return Err(Failure::Retry(anyhow::anyhow!("server sent a different byte range")));
        }
    }
    let remaining: u64 = response.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut done = if resumed { have } else { 0 };
    let total = if remaining > 0 { done + remaining } else { 0 };
    file.seek(SeekFrom::Start(done))?;
    let mut body = response.into_body().into_reader();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = body.read(&mut buf)?;
        if n == 0 {
            break;
        }
        done += n as u64;
        if done > max_size {
            let _ = file.set_len(0);
            return Err(permanent("the server sent more data than expected"));
        }
        file.write_all(&buf[..n])?;
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
                let range = if start > 0 {
                    format!("Content-Range: bytes {start}-{}/{}\r\n", content.len() - 1, content.len())
                } else {
                    String::new()
                };
                let head = format!("HTTP/1.1 {status}\r\n{range}Content-Length: {}\r\nConnection: close\r\n\r\n", body.len());
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
        fetch(&serve(CONTENT), &target, &sha(CONTENT), 100_000, |d, t| last = (d, t)).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), CONTENT);
        assert_eq!(last, (100_000, 100_000));
        assert!(!part_path(&target).exists());
    }

    #[test]
    fn resumes_a_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.bin");
        std::fs::write(part_path(&target), &CONTENT[..40_000]).unwrap();
        let mut first = None;
        let started = std::time::Instant::now();
        fetch(&serve(CONTENT), &target, &sha(CONTENT), 100_000, |d, _| _ = first.get_or_insert(d)).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), CONTENT);
        assert!(first.unwrap() > 40_000, "must continue after the existing bytes, not restart");
        assert!(started.elapsed() < Duration::from_secs(2), "no retry was needed");
    }

    #[test]
    fn oversized_body_is_rejected_without_retries() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.bin");
        let started = std::time::Instant::now();
        let err = fetch(&serve(CONTENT), &target, &sha(CONTENT), 50_000, |_, _| {}).unwrap_err();
        assert!(format!("{err:#}").contains("more data than expected"));
        assert!(started.elapsed() < Duration::from_secs(2), "permanent errors must not be retried");
        assert!(!target.exists());
    }

    #[test]
    fn second_downloader_is_refused_while_the_first_holds_the_lock() {
        use std::os::fd::AsRawFd;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.bin");
        let held = std::fs::OpenOptions::new().write(true).create(true).truncate(false).open(part_path(&target)).unwrap();
        assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
        let err = fetch(&serve(CONTENT), &target, &sha(CONTENT), 100_000, |_, _| {}).unwrap_err();
        assert!(format!("{err:#}").contains("already downloading"));
    }

    #[test]
    fn is_in_checks_the_size() {
        let dir = tempfile::tempdir().unwrap();
        let file = RemoteFile { repo: "r", revision: "v", name: "x.bin", size: 4, sha256: "" };
        assert!(!file.is_in(dir.path()));
        std::fs::write(dir.path().join("x.bin"), b"abc").unwrap();
        assert!(!file.is_in(dir.path()), "truncated file is not ready");
        std::fs::write(dir.path().join("x.bin"), b"abcd").unwrap();
        assert!(file.is_in(dir.path()));
    }

    #[test]
    fn wrong_checksum_is_rejected_and_cleaned_up() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.bin");
        let err = fetch(&serve(CONTENT), &target, "00", 100_000, |_, _| {}).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
        assert!(!target.exists() && !part_path(&target).exists());
    }
}
