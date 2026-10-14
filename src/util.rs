//! Small helpers shared by several modules.

use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command};
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

/// The child dies with its parent (also after a crash). Note: Linux ties this to the *thread* that spawned it.
pub fn die_with_parent(cmd: &mut Command) {
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
}

/// Runs the child in its own process group (so `kill_tree` reaches launcher shims and their workers) and ties it to the parent.
pub fn isolate(cmd: &mut Command) {
    cmd.process_group(0);
    die_with_parent(cmd);
}

/// True when the child has ended but is not reaped yet (so its process group id is still reserved and safe to signal).
pub fn exited_unreaped(child: &Child) -> bool {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::waitid(libc::P_PID, child.id(), &mut info, libc::WEXITED | libc::WNOHANG | libc::WNOWAIT) };
    rc == 0 && unsafe { info.si_pid() } != 0
}

/// Kills a child started with `isolate` together with everything it started, and reaps it.
pub fn kill_tree(child: &mut Child) {
    unsafe {
        libc::killpg(child.id() as libc::pid_t, libc::SIGKILL);
    }
    let _ = child.kill(); // also when the child is not a group leader: wait() below must never block on a live process
    let _ = child.wait();
}

pub fn which(bin: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join(bin)).find(|p| p.is_file())
}

/// User-level install dirs (npm, pnpm, bun, cargo, pipx). A desktop session often lacks them in PATH: shell profiles are not read there.
fn user_bin_dirs() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return Vec::new() };
    [".npm-global/bin", ".local/bin", ".local/share/pnpm", ".bun/bin", ".cargo/bin", "bin"].iter().map(|d| home.join(d)).collect()
}

fn find_in(bin: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(bin)).find(|p| p.is_file())
}

/// Command for a user-installed CLI: looks in PATH first, then in the user-level install dirs. Falls back to the bare name.
pub fn cli_command(bin: &str) -> Command {
    let found = which(bin).or_else(|| find_in(bin, &user_bin_dirs()));
    Command::new(found.as_deref().map_or(bin.as_ref(), |p| p.as_os_str()))
}

/// Puts the directory of the program first in the child's PATH, so launcher scripts (`#!/usr/bin/env node`) find their siblings.
/// Call after `restrict_env`, which clears the environment.
pub fn prepend_program_dir(cmd: &mut Command) {
    let Some(dir) = std::path::Path::new(cmd.get_program()).parent().filter(|d| !d.as_os_str().is_empty()).map(|d| d.to_path_buf()) else {
        return;
    };
    let current =
        cmd.get_envs().find(|(k, _)| *k == "PATH").and_then(|(_, v)| v.map(|v| v.to_owned())).or_else(|| std::env::var_os("PATH"));
    let mut dirs = vec![dir];
    dirs.extend(current.iter().flat_map(std::env::split_paths));
    if let Ok(joined) = std::env::join_paths(dirs) {
        cmd.env("PATH", joined);
    }
}

/// HTTP client with a global timeout. `env_proxy = false` ignores proxy variables (use for loopback servers).
pub fn http_agent(timeout: Duration, env_proxy: bool) -> ureq::Agent {
    let builder = ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(true);
    let builder = if env_proxy { builder } else { builder.proxy(None) };
    builder.build().into()
}

/// Path of the running binary. After an upgrade replaced it, Linux reports "... (deleted)": use the real path then.
pub fn exe_path() -> std::io::Result<PathBuf> {
    Ok(strip_deleted(std::env::current_exe()?))
}

fn strip_deleted(exe: PathBuf) -> PathBuf {
    match exe.to_str().and_then(|s| s.strip_suffix(" (deleted)")) {
        Some(clean) => PathBuf::from(clean),
        None => exe,
    }
}

/// A GVariant text-format string literal: `gsettings set` parses its value argument as GVariant, so a plain string with quotes
/// or spaces inside (like a shell command) must be wrapped and escaped.
pub fn gvariant_string(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// Quotes for a POSIX shell (GNOME parses keybinding commands like that).
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Quotes a program path for the `Exec=` line of a .desktop file: inside double quotes `"`, `` ` ``, `$` and `\` get a backslash,
/// the value then needs its backslashes doubled (desktop entry string rules), and `%` is written `%%`.
pub fn desktop_exec_quote(path: &str) -> String {
    let mut quoted = String::new();
    for c in path.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                quoted.push('\\');
                quoted.push(c);
            }
            '%' => quoted.push_str("%%"),
            _ => quoted.push(c),
        }
    }
    format!("\"{}\"", quoted.replace('\\', "\\\\"))
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
        assert_eq!(desktop_exec_quote("a$b\"c"), "\"a\\\\$b\\\\\"c\"");
        assert_eq!(desktop_exec_quote("50%/x"), "\"50%%/x\"");
        assert_eq!(desktop_exec_quote(r"a\b"), r#""a\\\\b""#);
    }

    #[test]
    fn kill_tree_ends_a_plain_child_and_an_isolated_one() {
        let mut plain = Command::new("sleep").arg("30").spawn().unwrap();
        kill_tree(&mut plain);
        assert!(plain.try_wait().unwrap().is_some());
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        isolate(&mut cmd);
        let mut isolated = cmd.spawn().unwrap();
        kill_tree(&mut isolated);
        assert!(isolated.try_wait().unwrap().is_some());
    }

    #[test]
    fn exited_unreaped_sees_a_finished_child_without_reaping_it() {
        let mut child = Command::new("true").spawn().unwrap();
        for _ in 0..100 {
            if exited_unreaped(&child) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(exited_unreaped(&child));
        assert!(child.wait().unwrap().success(), "still reapable afterwards");
    }

    #[test]
    fn gvariant_strings_are_escaped() {
        assert_eq!(gvariant_string("Dictee toggle"), "'Dictee toggle'");
        assert_eq!(gvariant_string(&format!("{} toggle", shell_quote("/home/a b/dictee"))), r"'\'/home/a b/dictee\' toggle'");
        assert_eq!(gvariant_string(r"a\b"), r"'a\\b'");
    }

    #[test]
    fn deleted_suffix_is_stripped() {
        assert_eq!(strip_deleted(PathBuf::from("/usr/bin/dictee (deleted)")), PathBuf::from("/usr/bin/dictee"));
        assert_eq!(strip_deleted(PathBuf::from("/usr/bin/dictee")), PathBuf::from("/usr/bin/dictee"));
    }

    #[test]
    fn restrict_env_passes_only_listed_variables() {
        let mut cmd = Command::new("env");
        restrict_env(&mut cmd, &["PATH"]);
        let out = String::from_utf8(cmd.env("DICTEE_TEST_SECRET", "x").output().unwrap().stdout).unwrap();
        assert!(out.contains("PATH=") && out.contains("DICTEE_TEST_SECRET=x"), "env() set after restrict is kept");
        let mut cmd = Command::new("env");
        restrict_env(&mut cmd, &["PATH"]);
        let out = String::from_utf8(cmd.output().unwrap().stdout).unwrap();
        assert!(out.lines().all(|l| l.starts_with("PATH=")), "only PATH survives: {out}");
    }

    #[test]
    fn find_in_uses_extra_dirs_and_prepend_adds_program_dir() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("fake-cli");
        std::fs::write(&bin, "#!/bin/sh\necho $PATH\n").unwrap();
        assert_eq!(find_in("fake-cli", &[dir.path().to_path_buf()]), Some(bin.clone()));
        assert!(find_in("fake-cli", &[]).is_none());
        let mut cmd = Command::new(&bin);
        restrict_env(&mut cmd, &["PATH"]);
        prepend_program_dir(&mut cmd);
        let path = cmd.get_envs().find(|(k, _)| *k == "PATH").unwrap().1.unwrap().to_owned();
        assert_eq!(std::env::split_paths(&path).next().unwrap(), dir.path());
    }

    #[test]
    fn which_finds_sh() {
        assert!(which("sh").is_some());
        assert!(which("definitely-not-a-binary-xyz").is_none());
    }
}
