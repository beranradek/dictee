//! Global start/stop shortcut through the xdg-desktop-portal GlobalShortcuts API (press and release events),
//! with a GNOME custom keybinding (toggle only) as fallback.

use crate::config::Config;
use crate::controller::Cmd;
use anyhow::{Context, Result};
use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
use futures_util::StreamExt;
use std::process::Command;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

const APP_ID: &str = "cz.etnetera.Dictee";
const SHORTCUT_ID: &str = "dictate";

/// Human readable shortcut status for the settings window.
pub type Status = Arc<Mutex<String>>;

pub fn spawn(tx: Sender<Cmd>, status: Status) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
        if let Err(e) = rt.block_on(run(tx, status.clone())) {
            *status.lock().unwrap() = format!("Portal shortcut unavailable: {e:#}");
            eprintln!("dictee: global shortcut portal failed: {e:#}");
        }
    });
}

async fn run(tx: Sender<Cmd>, status: Status) -> Result<()> {
    if let Ok(id) = APP_ID.parse() {
        if let Err(e) = ashpd::register_host_app(id).await {
            eprintln!("dictee: portal app registration failed: {e}");
        }
    }
    let gs = GlobalShortcuts::new().await.context("GlobalShortcuts portal missing")?;
    let session = gs.create_session(Default::default()).await.context("cannot create shortcut session")?;
    let cfg = Config::load()?;
    *status.lock().unwrap() = "Shortcut: waiting for your confirmation in the system dialog...".to_string();
    let new = NewShortcut::new(SHORTCUT_ID, "Start / stop dictation").preferred_trigger(cfg.shortcut.as_str());
    let bound = gs.bind_shortcuts(&session, &[new], None, Default::default()).await?.response()?;
    let trigger = bound.shortcuts().iter().find(|s| s.id() == SHORTCUT_ID).map(|s| s.trigger_description().to_string());
    *status.lock().unwrap() = match trigger {
        Some(t) if !t.is_empty() => format!("Shortcut: {t}"),
        _ => "No shortcut assigned - set one in system settings (Keyboard > Shortcuts)".to_string(),
    };
    eprintln!("dictee: {}", status.lock().unwrap());
    let mut activated = gs.receive_activated().await?;
    let mut deactivated = gs.receive_deactivated().await?;
    loop {
        tokio::select! {
            Some(a) = activated.next() => if a.shortcut_id() == SHORTCUT_ID {
                let hold = Config::load().map(|c| c.stop_on_release).unwrap_or(false);
                let _ = tx.send(if hold { Cmd::Start } else { Cmd::Toggle });
            },
            Some(d) = deactivated.next() => if d.shortcut_id() == SHORTCUT_ID
                && Config::load().map(|c| c.stop_on_release).unwrap_or(false) {
                let _ = tx.send(Cmd::Stop);
            },
            else => break,
        }
    }
    Ok(())
}

/// Registers a GNOME custom keybinding that runs `dictee toggle`. `binding` looks like `<Control><Shift>h`.
pub fn install_gnome_keybinding(binding: &str) -> Result<()> {
    const SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
    const PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/dictee/";
    let gs = |args: &[&str]| -> Result<String> {
        let out = Command::new("gsettings").args(args).output().context("gsettings not available")?;
        if !out.status.success() {
            anyhow::bail!("gsettings failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let exe = std::env::current_exe()?;
    let key_schema = format!("{SCHEMA}.custom-keybinding:{PATH}");
    gs(&["set", &key_schema, "name", "Dictee toggle"])?;
    gs(&["set", &key_schema, "command", &format!("{} toggle", exe.display())])?;
    gs(&["set", &key_schema, "binding", binding])?;
    let current = gs(&["get", SCHEMA, "custom-keybindings"])?;
    if !current.contains(PATH) {
        let new = if current.contains('\'') {
            current.trim_end_matches(']').to_string() + &format!(", '{PATH}']")
        } else {
            format!("['{PATH}']")
        };
        gs(&["set", SCHEMA, "custom-keybindings", &new])?;
    }
    Ok(())
}
