//! Small helpers shared by several modules.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Clears the environment of `cmd`, then passes only the listed variables (no API keys or proxies by accident).
pub fn restrict_env(cmd: &mut Command, allowed: &[&str]) {
    cmd.env_clear();
    for var in allowed {
        if let Some(v) = std::env::var_os(var) {
            cmd.env(var, v);
        }
    }
}

pub fn which(bin: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(bin)).find(|p| p.is_file())
}

/// HTTP client with a global timeout. `env_proxy = false` ignores proxy variables (use for loopback servers).
pub fn http_agent(timeout: Duration, env_proxy: bool) -> ureq::Agent {
    let builder = ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(true);
    let builder = if env_proxy { builder } else { builder.proxy(None) };
    builder.build().into()
}

/// Path of the running binary. After an upgrade replaced it, Linux reports "... (deleted)": use the real path then.
pub fn exe_path() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(clean) => PathBuf::from(clean),
        None => exe,
    })
}

/// Quotes for a POSIX shell (GNOME parses keybinding commands like that).
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Quotes a program path for the `Exec=` line of a .desktop file.
pub fn desktop_exec_quote(path: &str) -> String {
    let escaped: String = path.chars().flat_map(|c| if matches!(c, '"' | '`' | '$' | '\\') { vec!['\\', c] } else { vec![c] }).collect();
    format!("\"{escaped}\"")
}

/// Cuts text to `max` characters, adding an ellipsis.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.to_string() } else { format!("{}...", text.chars().take(max).collect::<String>()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncates_on_char_boundaries() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("příliš žluťoučký", 5), "příli...");
    }

    #[test]
    fn quoting() {
        assert_eq!(shell_quote("/a b/dictee"), "'/a b/dictee'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(desktop_exec_quote("/a b/dictee"), "\"/a b/dictee\"");
        assert_eq!(desktop_exec_quote("a$b\"c"), "\"a\\$b\\\"c\"");
    }

    #[test]
    fn which_finds_sh() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-binary-xyz").is_none());
    }
}
