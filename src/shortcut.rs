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

/// A shortcut like "CTRL+SHIFT+h" in the portal notation, convertible to GNOME and KDE notation.
#[derive(Debug, PartialEq)]
pub struct Combo {
    mods: Vec<&'static str>, // canonical: Ctrl, Shift, Alt, Super
    key: String,
}

impl Combo {
    pub fn parse(text: &str) -> Result<Self> {
        let mut mods = Vec::new();
        let mut key = None;
        for part in text.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_uppercase().as_str() {
                "CTRL" | "CONTROL" => mods.push("Ctrl"),
                "SHIFT" => mods.push("Shift"),
                "ALT" => mods.push("Alt"),
                "LOGO" | "SUPER" | "META" => mods.push("Super"),
                _ if key.is_none() => key = Some(part.to_string()),
                _ => anyhow::bail!("shortcut '{text}' has more than one key"),
            }
        }
        let key = key.with_context(|| format!("shortcut '{text}' has no key"))?;
        if mods.is_empty() {
            anyhow::bail!("shortcut '{text}' needs a modifier (Ctrl, Shift, Alt or Super)");
        }
        Ok(Self { mods, key })
    }

    /// `<Control><Shift>h`
    pub fn gnome(&self) -> String {
        let mods: String = self.mods.iter().map(|m| match *m { "Ctrl" => "<Control>", "Shift" => "<Shift>", "Alt" => "<Alt>", _ => "<Super>" }).collect();
        format!("{mods}{}", self.key.to_lowercase())
    }

    /// `Ctrl+Shift+H`
    pub fn kde(&self) -> String {
        let mods: Vec<&str> = self.mods.iter().map(|m| if *m == "Super" { "Meta" } else { *m }).collect();
        let key = if self.key.chars().count() == 1 { self.key.to_uppercase() } else { self.key.clone() };
        format!("{}+{key}", mods.join("+"))
    }

    /// `Ctrl+Shift+H` for display
    pub fn display(&self) -> String {
        self.kde().replace("Meta", "Super")
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Desktop {
    Gnome,
    Kde,
    Other,
}

pub fn desktop() -> Desktop {
    desktop_from(&std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default())
}

fn desktop_from(xdg: &str) -> Desktop {
    let up = xdg.to_ascii_uppercase();
    if up.split(':').any(|p| p == "KDE") {
        Desktop::Kde
    } else if up.split(':').any(|p| p == "GNOME" || p == "UNITY" || p == "X-CINNAMON" || p == "BUDGIE") {
        Desktop::Gnome
    } else {
        Desktop::Other
    }
}

/// Installs a "run `dictee toggle`" keybinding in the current desktop. Returns a message for the user.
pub fn install_keybinding(shortcut: &str) -> Result<String> {
    let combo = Combo::parse(shortcut)?;
    match desktop() {
        Desktop::Gnome => {
            install_gnome_keybinding(&combo.gnome())?;
            Ok(format!("GNOME keybinding {} installed", combo.display()))
        }
        Desktop::Kde => {
            install_kde_keybinding(&combo.kde())?;
            Ok(format!("KDE shortcut {} installed (log out and in if it does not react)", combo.display()))
        }
        Desktop::Other => anyhow::bail!("unknown desktop: add a custom shortcut running `{} toggle` in your system settings", std::env::current_exe()?.display()),
    }
}

/// Content of the KDE launcher file that kglobalaccel binds the shortcut to.
fn kde_desktop_file(exe: &std::path::Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Dictee toggle dictation\nExec={} toggle\nNoDisplay=true\n\
         X-KDE-GlobalAccel-CommandShortcut=true\n",
        exe.display()
    )
}

/// Arguments for `kwriteconfig` that assign the shortcut to the launcher file.
fn kde_config_args(binding: &str) -> Vec<String> {
    ["--file", "kglobalshortcutsrc", "--group", "services", "--group", KDE_DESKTOP_ID, "--key", "_launch", binding].iter().map(|s| s.to_string()).collect()
}

const KDE_DESKTOP_ID: &str = "dictee-toggle.desktop";

/// Registers a KDE global shortcut (Plasma 5 and 6) through a launcher file and `kglobalshortcutsrc`.
/// Not tested on a real Plasma session by the author - report problems.
pub fn install_kde_keybinding(binding: &str) -> Result<()> {
    let exe = std::env::current_exe()?;
    let dir = dirs::data_dir().context("no data dir")?.join("applications");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(KDE_DESKTOP_ID), kde_desktop_file(&exe))?;
    let (writer, plasma6) = if which("kwriteconfig6") {
        ("kwriteconfig6", true)
    } else if which("kwriteconfig5") {
        ("kwriteconfig5", false)
    } else {
        anyhow::bail!("kwriteconfig6/kwriteconfig5 not found (is KDE Plasma installed?)");
    };
    let status = Command::new(writer).args(kde_config_args(binding)).status().context("cannot run kwriteconfig")?;
    if !status.success() {
        anyhow::bail!("{writer} failed");
    }
    let _ = Command::new("kbuildsycoca6").arg("--noincremental").status();
    // kglobalaccel reads kglobalshortcutsrc at start: restart it so the new shortcut is active.
    let restarted = Command::new("systemctl").args(["--user", "restart", if plasma6 { "plasma-kglobalaccel.service" } else { "plasma-kglobalaccel5.service" }]).status().map(|s| s.success()).unwrap_or(false);
    if !restarted {
        let (quit, daemon) = if plasma6 { ("kquitapp6", "kglobalaccel6") } else { ("kquitapp5", "kglobalaccel5") };
        let _ = Command::new(quit).arg(daemon).status();
        let _ = Command::new(daemon).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
    }
    Ok(())
}

fn which(bin: &str) -> bool {
    std::env::var_os("PATH").map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file())).unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_notations() {
        let c = Combo::parse("CTRL+SHIFT+h").unwrap();
        assert_eq!(c.gnome(), "<Control><Shift>h");
        assert_eq!(c.kde(), "Ctrl+Shift+H");
        let c = Combo::parse("LOGO+ALT+space").unwrap();
        assert_eq!(c.gnome(), "<Super><Alt>space");
        assert_eq!(c.kde(), "Meta+Alt+space");
        assert_eq!(c.display(), "Super+Alt+space");
    }

    #[test]
    fn rejects_bad_shortcuts() {
        assert!(Combo::parse("h").is_err());
        assert!(Combo::parse("CTRL+").is_err());
        assert!(Combo::parse("CTRL+a+b").is_err());
    }

    #[test]
    fn detects_desktops() {
        assert_eq!(desktop_from("ubuntu:GNOME"), Desktop::Gnome);
        assert_eq!(desktop_from("KDE"), Desktop::Kde);
        assert_eq!(desktop_from("Hyprland"), Desktop::Other);
    }

    #[test]
    fn kde_files_and_args() {
        let f = kde_desktop_file(std::path::Path::new("/usr/bin/dictee"));
        assert!(f.contains("Exec=/usr/bin/dictee toggle") && f.contains("X-KDE-GlobalAccel-CommandShortcut=true"));
        let a = kde_config_args("Ctrl+Shift+H");
        assert_eq!(a.last().unwrap(), "Ctrl+Shift+H");
        assert!(a.contains(&"dictee-toggle.desktop".to_string()));
    }
}
