//! Text-only rewriting through `codex app-server` (JSONL over stdio), with all tools and features disabled.
//! Uses the existing Codex login. The protocol follows what Mluva uses: initialize, thread/start, turn/start.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

const TEXT_ONLY: &str = include_str!("../resources/codex-text-only.json");
const BASE_INSTRUCTIONS: &str = "You transform dictated text. Follow the user's requested operation exactly. Return only replacement text, \
without commentary, quotes, or Markdown fences. Never use tools or infer facts absent from the supplied text.";
const ENV_ALLOWED: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "CODEX_HOME",
    "OPENAI_API_KEY",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
    "DBUS_SESSION_BUS_ADDRESS",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "all_proxy",
    "no_proxy",
];

struct Session {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: u64,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Session {
    fn send(&mut self, v: &Value) -> Result<()> {
        writeln!(self.stdin, "{v}")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Sends a request and waits for its response; other messages before it are ignored.
    fn request(&mut self, method: &str, params: Value, deadline: Instant) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({"method": method, "id": id, "params": params}))?;
        loop {
            let msg = self.recv(deadline)?;
            if msg.get("method").is_some() && msg.get("id").is_some() {
                self.refuse(&msg)?;
                bail!("Codex asked for something outside text-only rewriting ({})", msg["method"]);
            }
            if msg.get("id") == Some(&json!(id)) {
                if let Some(err) = msg.get("error") {
                    bail!("Codex rejected {method}: {err}");
                }
                return Ok(msg["result"].clone());
            }
        }
    }

    fn refuse(&mut self, request: &Value) -> Result<()> {
        self.send(&json!({"id": request["id"], "error": {"code": -32601, "message": "not supported"}}))
    }

    fn recv(&mut self, deadline: Instant) -> Result<Value> {
        let left = deadline.saturating_duration_since(Instant::now());
        match self.rx.recv_timeout(left) {
            Ok(v) => Ok(v),
            Err(RecvTimeoutError::Timeout) => bail!("Codex app-server timed out"),
            Err(RecvTimeoutError::Disconnected) => bail!("Codex app-server exited"),
        }
    }
}

/// Makes a `codex` command text-only: every tool and feature off (`-c` overrides), a restricted environment.
/// No --strict-config: older Codex versions do not know the newest feature keys and would refuse to start.
pub fn make_text_only(cmd: &mut Command) {
    let config: serde_json::Map<String, Value> = serde_json::from_str(TEXT_ONLY).expect("valid text-only config");
    for (key, value) in &config {
        cmd.arg("-c").arg(format!("{key}={value}"));
    }
    for feature in ["code_mode_only", "current_time_reminder", "send_message_to_user_async", "deferred_executor"] {
        cmd.arg("-c").arg(format!("features.{feature}=false"));
    }
    crate::util::restrict_env(cmd, ENV_ALLOWED);
}

fn spawn(cwd: &std::path::Path) -> Result<Session> {
    let mut cmd = Command::new("codex");
    cmd.arg("app-server");
    make_text_only(&mut cmd);
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().context("cannot run 'codex app-server' (is Codex installed and logged in?)")?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
            if let Ok(v) = serde_json::from_str::<Value>(&line)
                && tx.send(v).is_err()
            {
                break;
            }
        }
    });
    Ok(Session { child, stdin, rx, next_id: 1 })
}

/// Runs one text transformation. `model` may be empty (Codex default model).
pub fn run(model: &str, prompt: &str, timeout: Duration) -> Result<String> {
    let dir = tempfile::tempdir()?; // private working directory, removed on every exit path
    run_in(dir.path(), model, prompt, timeout)
}

fn run_in(dir: &std::path::Path, model: &str, prompt: &str, timeout: Duration) -> Result<String> {
    let deadline = Instant::now() + timeout;
    let mut s = spawn(dir)?;
    s.request(
        "initialize",
        json!({"clientInfo": {"name": "dictee", "title": "Dictee", "version": env!("CARGO_PKG_VERSION")}, "capabilities": {"experimentalApi": true}}),
        deadline,
    )?;
    s.send(&json!({"method": "initialized", "params": {}}))?;
    let mut thread = json!({
        "cwd": dir, "approvalPolicy": "never", "sandbox": "read-only", "ephemeral": true, "serviceName": "dictee",
        "environments": [], "dynamicTools": [], "developerInstructions": "", "baseInstructions": BASE_INSTRUCTIONS,
    });
    if !model.is_empty() {
        thread["model"] = json!(model);
    }
    let started = s.request("thread/start", thread, deadline)?;
    if started["thread"]["ephemeral"] != json!(true) {
        bail!("Codex cannot confirm an ephemeral text-only thread - update Codex");
    }
    let thread_id = started["thread"]["id"].as_str().context("Codex returned no thread id")?.to_string();
    let turn = s.request("turn/start", json!({"threadId": thread_id, "input": [{"type": "text", "text": prompt}]}), deadline)?;
    let turn_id = turn["turn"]["id"].as_str().context("Codex returned no turn id")?.to_string();
    collect(&mut s, &turn_id, deadline)
}

fn collect(s: &mut Session, turn_id: &str, deadline: Instant) -> Result<String> {
    let mut out = String::new();
    loop {
        let msg = s.recv(deadline)?;
        if msg.get("method").is_some() && msg.get("id").is_some() {
            s.refuse(&msg)?;
            bail!("Codex asked for something outside text-only rewriting ({})", msg["method"]);
        }
        let (method, params) = (msg["method"].as_str().unwrap_or(""), &msg["params"]);
        if method == "item/agentMessage/delta" && params["turnId"] == turn_id {
            out.push_str(params["delta"].as_str().unwrap_or(""));
        }
        if method == "turn/completed" && params["turn"]["id"] == turn_id {
            let status = params["turn"]["status"].as_str().unwrap_or("");
            if status != "completed" {
                let detail = params["turn"]["error"]["message"].as_str().unwrap_or("no details");
                bail!("Codex turn ended with status '{status}': {detail}");
            }
            let text = out.trim().to_string();
            if text.is_empty() {
                bail!("Codex returned no text");
            }
            return Ok(text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_only_config_is_valid_json_object() {
        let m: serde_json::Map<String, Value> = serde_json::from_str(TEXT_ONLY).unwrap();
        assert!(m.len() > 20);
        assert_eq!(m["features.shell_tool"], json!(false));
    }

    #[test]
    fn collects_deltas_until_completed() {
        // Fake session fed through the channel; the child is a harmless `cat`.
        let mut child = Command::new("cat").stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let (tx, rx) = mpsc::channel();
        for m in [
            json!({"method":"item/agentMessage/delta","params":{"turnId":"t1","delta":"Hello "}}),
            json!({"method":"item/agentMessage/delta","params":{"turnId":"other","delta":"NO"}}),
            json!({"method":"item/agentMessage/delta","params":{"turnId":"t1","delta":"world"}}),
            json!({"method":"turn/completed","params":{"turn":{"id":"t1","status":"completed"}}}),
        ] {
            tx.send(m).unwrap();
        }
        let mut s = Session { child, stdin, rx, next_id: 1 };
        assert_eq!(collect(&mut s, "t1", Instant::now() + Duration::from_secs(2)).unwrap(), "Hello world");
    }

    #[test]
    fn failed_turn_is_an_error() {
        let mut child = Command::new("cat").stdin(Stdio::piped()).stdout(Stdio::null()).spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let (tx, rx) = mpsc::channel();
        tx.send(json!({"method":"turn/completed","params":{"turn":{"id":"t1","status":"failed"}}})).unwrap();
        let mut s = Session { child, stdin, rx, next_id: 1 };
        assert!(collect(&mut s, "t1", Instant::now() + Duration::from_secs(2)).is_err());
    }
}
