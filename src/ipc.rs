//! Tiny line protocol on a unix socket so `dictee toggle` (desktop keybinding, scripts) can drive the daemon.

use crate::controller::{Cmd, SharedState};
use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::Duration;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LINE: u64 = 64;

/// `$DICTEE_SOCKET` (tests, a second instance), else `$XDG_RUNTIME_DIR/dictee.sock`; without it a private per-user folder
/// in the temp dir (never a shared path).
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("DICTEE_SOCKET") {
        return PathBuf::from(path);
    }
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir).join("dictee.sock");
    }
    let dir = std::env::temp_dir().join(format!("dictee-{}", unsafe { libc::getuid() }));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    dir.join("dictee.sock")
}

/// Sends one command to the running daemon and returns its reply.
pub fn send(cmd: &str) -> Result<String> {
    send_to(&socket_path(), cmd)
}

fn send_to(path: &Path, cmd: &str) -> Result<String> {
    let mut s = UnixStream::connect(path).context("dictee daemon is not running (start it with `dictee`)")?;
    s.set_read_timeout(Some(IO_TIMEOUT))?;
    s.set_write_timeout(Some(IO_TIMEOUT))?;
    s.write_all(format!("{cmd}\n").as_bytes())?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply).context("no answer from the dictee daemon")?;
    Ok(reply.trim().to_string())
}

pub fn daemon_running() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

/// Starts the server thread. Fails when another daemon already owns the socket.
pub fn serve(tx: Sender<Cmd>, state: SharedState) -> Result<()> {
    serve_at(&socket_path(), tx, state)
}

fn serve_at(path: &Path, tx: Sender<Cmd>, state: SharedState) -> Result<()> {
    if UnixStream::connect(path).is_ok() {
        bail!("dictee is already running");
    }
    let _ = std::fs::remove_file(path); // stale socket of a dead daemon
    let listener = UnixListener::bind(path).with_context(|| format!("cannot bind {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let _ = handle(&conn, &tx, &state);
        }
    });
    Ok(())
}

fn handle(conn: &UnixStream, tx: &Sender<Cmd>, state: &SharedState) -> Result<()> {
    // A silent or endless client must not block the single accept loop.
    conn.set_read_timeout(Some(IO_TIMEOUT))?;
    conn.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut line = String::new();
    BufReader::new(conn.take(MAX_LINE)).read_line(&mut line)?;
    let reply = match parse(line.trim()) {
        Some(Some(cmd)) => {
            let _ = tx.send(cmd);
            "ok".to_string()
        }
        Some(None) => state.lock().unwrap().as_str().to_string(),
        None => format!("error: unknown command '{}'", line.trim()),
    };
    (&*conn).write_all(format!("{reply}\n").as_bytes())?;
    Ok(())
}

/// `Some(Some(cmd))` = command, `Some(None)` = status query, `None` = unknown.
fn parse(word: &str) -> Option<Option<Cmd>> {
    Some(Some(match word {
        "start" => Cmd::Start,
        "stop" => Cmd::Stop,
        "toggle" => Cmd::Toggle,
        "cancel" => Cmd::Cancel,
        "settings" => Cmd::ShowSettings,
        "quit" => Cmd::Quit,
        "status" => return Some(None),
        _ => return None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::State;
    use std::sync::{Arc, Mutex, mpsc};

    #[test]
    fn parses_words() {
        assert!(matches!(parse("toggle"), Some(Some(Cmd::Toggle))));
        assert!(matches!(parse("status"), Some(None)));
        assert!(parse("bogus").is_none());
    }

    #[test]
    fn roundtrip_over_a_socket_with_private_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.sock");
        let (tx, rx) = mpsc::channel();
        serve_at(&path, tx.clone(), Arc::new(Mutex::new(State::Recording))).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(send_to(&path, "status").unwrap(), "recording");
        assert_eq!(send_to(&path, "toggle").unwrap(), "ok");
        assert!(matches!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), Cmd::Toggle));
        assert!(send_to(&path, "bogus").unwrap().starts_with("error"));
        assert!(serve_at(&path, tx, Arc::new(Mutex::new(State::Idle))).is_err(), "second daemon must be refused");
    }
}
