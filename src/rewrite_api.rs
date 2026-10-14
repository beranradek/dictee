//! Rewriting through an OpenAI-compatible chat API (OpenAI, LiteLLM, llama.cpp server, Ollama, ...).

use crate::config::RewriteCfg;
use anyhow::{Context, Result, bail};
use std::time::Duration;

/// Host part of a URL (without port, brackets of IPv6 removed).
fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or("");
    match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.split(':').next().unwrap_or(""),
    }
}

/// True for `localhost` and loopback IP addresses only (not for names that merely start with 127.).
fn is_loopback(url: &str) -> bool {
    let h = host(url);
    h == "localhost" || h.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Accepts https URLs, and http only for loopback hosts. No credentials, query or fragment in the URL.
pub fn validate_base_url(url: &str) -> Result<()> {
    let rest = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(r), _) => r,
        (None, Some(r)) => {
            if !is_loopback(url) {
                bail!("plain http is only allowed for localhost - use https:// for remote servers");
            }
            r
        }
        _ => bail!("API base URL must start with https:// (or http:// for localhost)"),
    };
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() || authority.contains('@') || url.contains('?') || url.contains('#') {
        bail!("API base URL must not contain credentials, query or fragment");
    }
    Ok(())
}

/// The API key from the named environment variable, else from the key file; `None` when neither is configured.
pub fn api_key(rw: &RewriteCfg) -> Result<Option<String>> {
    let env = rw.api_key_env.trim();
    if !env.is_empty()
        && let Ok(v) = std::env::var(env)
        && !v.trim().is_empty()
    {
        return Ok(Some(v.trim().to_string()));
    }
    let file = rw.api_key_file.trim();
    if !file.is_empty() {
        let v = std::fs::read_to_string(file).with_context(|| format!("cannot read API key file {file}"))?;
        return Ok(Some(v.trim().to_string()));
    }
    Ok(None)
}

pub fn chat(rw: &RewriteCfg, system: &str, user: &str) -> Result<String> {
    let base = rw.base_url.trim().trim_end_matches('/');
    validate_base_url(base)?;
    if rw.model.trim().is_empty() {
        bail!("set a model name for the API rewrite in Settings");
    }
    let key = api_key(rw)?;
    if key.is_none() && !is_loopback(base) {
        bail!("no API key: set the environment variable '{}' or an API key file", rw.api_key_env);
    }
    let agent = crate::util::http_agent(Duration::from_secs(rw.timeout_secs.max(5)), !is_loopback(base));
    let mut req = agent.post(&format!("{base}/chat/completions"));
    if let Some(k) = key {
        req = req.header("Authorization", &format!("Bearer {k}"));
    }
    let body = serde_json::json!({
        "model": rw.model.trim(),
        "temperature": 0,
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
    });
    let resp = req.send_json(&body).map_err(|e| anyhow::anyhow!("chat API request failed: {e}"))?;
    let v: serde_json::Value = resp.into_body().read_json()?;
    parse_reply(&v)
}

fn parse_reply(v: &serde_json::Value) -> Result<String> {
    let text = v["choices"][0]["message"]["content"].as_str().context("chat API returned no text")?.trim().to_string();
    if text.is_empty() {
        bail!("chat API returned empty text");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rules() {
        assert!(validate_base_url("https://api.openai.com/v1").is_ok());
        assert!(validate_base_url("http://localhost:4000/v1").is_ok());
        assert!(validate_base_url("http://127.0.0.1:8080/v1").is_ok());
        assert!(validate_base_url("http://[::1]:8080/v1").is_ok());
        assert!(validate_base_url("http://example.com/v1").is_err());
        assert!(validate_base_url("http://127.0.0.1.evil.com/v1").is_err());
        assert!(validate_base_url("http://localhost.evil.com/v1").is_err());
        assert!(validate_base_url("https:///v1").is_err());
        assert!(validate_base_url("https://user:pw@example.com/v1").is_err());
        assert!(validate_base_url("https://example.com/v1?key=1").is_err());
        assert!(validate_base_url("ftp://example.com").is_err());
    }

    #[test]
    fn parses_reply() {
        let v = serde_json::json!({"choices":[{"message":{"content":" Hi there \n"}}]});
        assert_eq!(parse_reply(&v).unwrap(), "Hi there");
        assert!(parse_reply(&serde_json::json!({})).is_err());
    }

    #[test]
    fn key_from_file_when_env_missing() {
        let dir = std::env::temp_dir().join(format!("dictee-key-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("key");
        std::fs::write(&f, "sk-test\n").unwrap();
        let rw =
            RewriteCfg { api_key_env: "DICTEE_TEST_NO_SUCH_VAR".into(), api_key_file: f.display().to_string(), ..RewriteCfg::default() };
        assert_eq!(api_key(&rw).unwrap().as_deref(), Some("sk-test"));
        let none = RewriteCfg { api_key_env: String::new(), api_key_file: String::new(), ..RewriteCfg::default() };
        assert_eq!(api_key(&none).unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
