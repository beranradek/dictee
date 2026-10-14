//! Tiny line protocol on a unix socket so `dictee toggle` (GNOME keybinding, scripts) can drive the daemon.

use crate::controller::{Cmd, SharedState};
use anyhow::{bail, Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::Sender;

pub fn socket_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join("dictee.sock")
}

/// Sends one command to the running daemon and returns its reply.
pub fn send(cmd: &str) -> Result<String> {
    let mut s = UnixStream::connect(socket_path()).context("dictee daemon is not running (start it with `dictee`)")?;
    s.write_all(format!("{cmd}\n").as_bytes())?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply)?;
    Ok(reply.trim().to_string())
}

pub fn daemon_running() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

/// Starts the server thread. Fails when another daemon already owns the socket.
pub fn serve(tx: Sender<Cmd>, state: SharedState) -> Result<()> {
    let path = socket_path();
    if daemon_running() {
        bail!("dictee is already running");
    }
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("cannot bind {}", path.display()))?;
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let mut line = String::new();
            if BufReader::new(&conn).read_line(&mut line).is_err() {
                continue;
            }
            let reply = match parse(line.trim()) {
                Some(Some(cmd)) => {
                    let _ = tx.send(cmd);
                    "ok".to_string()
                }
                Some(None) => state.lock().unwrap().as_str().to_string(),
                None => format!("error: unknown command '{}'", line.trim()),
            };
            let _ = (&conn).write_all(format!("{reply}\n").as_bytes());
        }
    });
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

    #[test]
    fn parses_words() {
        assert!(matches!(parse("toggle"), Some(Some(Cmd::Toggle))));
        assert!(matches!(parse("status"), Some(None)));
        assert!(parse("bogus").is_none());
    }
}
