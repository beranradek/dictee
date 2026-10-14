//! Delivers the final text: clipboard, optional auto-paste, desktop notification.

use anyhow::{bail, Context, Result};
use std::io::Write;
use std::process::{Command, Stdio};

/// Puts text on the clipboard. Uses wl-copy on Wayland, xclip/xsel on X11.
pub fn copy(text: &str) -> Result<()> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let candidates: &[(&str, &[&str])] = if wayland {
        &[("wl-copy", &[])]
    } else {
        &[("xclip", &["-selection", "clipboard"]), ("xsel", &["--clipboard", "--input"])]
    };
    for (bin, args) in candidates {
        match Command::new(bin).args(*args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn() {
            Ok(mut child) => {
                child.stdin.take().unwrap().write_all(text.as_bytes())?;
                // wl-copy/xclip stay alive as clipboard owners; reaping in a thread avoids zombies.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Ok(());
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e).with_context(|| format!("cannot run {bin}")),
        }
    }
    bail!("no clipboard tool found - install wl-clipboard (Wayland) or xclip (X11)")
}

/// Presses the paste shortcut through ydotool (Linux input codes: ctrl=29, shift=42, v=47).
pub fn paste(keys: &str) -> Result<()> {
    let seq: &[&str] = match keys {
        "ctrl+shift+v" => &["29:1", "42:1", "47:1", "47:0", "42:0", "29:0"],
        _ => &["29:1", "47:1", "47:0", "29:0"],
    };
    let out = Command::new("ydotool").arg("key").args(seq).output().context("cannot run ydotool")?;
    if !out.status.success() {
        bail!("ydotool failed (is ydotoold running and /dev/uinput accessible?): {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

pub fn notify(title: &str, body: &str) {
    let _ = Command::new("notify-send").args(["-a", "Dictee", "-t", "3000", title, body]).status();
}
