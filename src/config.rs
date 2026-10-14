//! User configuration stored as TOML in `~/.config/dictee/config.toml`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct RewriteCfg {
    /// "none", "claude" (CLI), "codex" (CLI exec), "codex-app" (Codex app-server) or "openai" (OpenAI/LiteLLM compatible API)
    pub engine: String,
    /// Optional model name passed to the CLI (empty = CLI default)
    pub model: String,
    pub timeout_secs: u64,
    /// Extra free-text instructions appended to the rewrite prompt
    pub extra_instructions: String,
    /// Chat API base URL including the API prefix, e.g. "http://localhost:4000/v1" (LiteLLM) or "https://api.openai.com/v1"
    pub base_url: String,
    /// Name of the environment variable that holds the API key (empty = no key)
    pub api_key_env: String,
    /// Alternative: file containing the API key (used when the variable is empty or unset)
    pub api_key_file: String,
}

impl Default for RewriteCfg {
    fn default() -> Self {
        Self {
            engine: "none".into(),
            model: String::new(),
            timeout_secs: 60,
            extra_instructions: String::new(),
            base_url: "http://localhost:4000/v1".into(),
            api_key_env: "OPENAI_API_KEY".into(),
            api_key_file: String::new(),
        }
    }
}

/// Settings for Qwen3-ASR running on a managed llama.cpp `llama-server`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct QwenCfg {
    /// Model id from `qwen::MODELS`
    pub model: String,
    /// "cpu" or "vulkan" (any Vulkan GPU, experimental)
    pub device: String,
    /// Own llama-server binary (empty = managed download of the pinned release)
    pub server_path: String,
    /// Keep the server (and model in RAM) this long after the last dictation; 0 = stop right away
    pub keep_alive_secs: u64,
    /// Threads for the server; 0 = same as the Whisper thread setting
    pub threads: i32,
}

impl Default for QwenCfg {
    fn default() -> Self {
        Self { model: "qwen3-1.7b-q4".into(), device: "cpu".into(), server_path: String::new(), keep_alive_secs: 120, threads: 0 }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Config {
    /// PipeWire node name of the microphone; empty = system default
    pub mic: String,
    /// Speech engine: "whisper" (whisper.cpp) or "qwen" (Qwen3-ASR on llama.cpp)
    pub stt_engine: String,
    /// Folder for downloaded models; empty = `~/.local/share/dictee/models`
    pub models_dir: String,
    /// Model id from `models::MODELS`
    pub model: String,
    /// "auto", "cs" or "en"
    pub language: String,
    pub threads: i32,
    /// Preferred trigger for the desktop portal, e.g. "CTRL+SHIFT+h"
    pub shortcut: String,
    /// Hold-to-talk: start on key press, stop on key release
    pub stop_on_release: bool,
    /// Press the paste keys after copying (needs ydotoold + /dev/uinput access)
    pub auto_paste: bool,
    /// "ctrl+v" or "ctrl+shift+v" (terminals)
    pub paste_keys: String,
    pub notify: bool,
    /// Show the floating bubble while dictating (it takes keyboard focus on GNOME Wayland while visible)
    pub show_bubble: bool,
    pub qwen: QwenCfg,
    pub rewrite: RewriteCfg,
    /// Known names and phrases, given to the STT model and the rewrite LLM
    pub phrases: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        let threads = std::thread::available_parallelism().map(|n| n.get() as i32).unwrap_or(4).clamp(2, 8);
        Self {
            mic: String::new(),
            stt_engine: "whisper".into(),
            models_dir: String::new(),
            model: "small".into(),
            language: "auto".into(),
            threads,
            shortcut: "CTRL+SHIFT+h".into(),
            stop_on_release: false,
            auto_paste: false,
            paste_keys: "ctrl+v".into(),
            notify: true,
            show_bubble: true,
            qwen: QwenCfg::default(),
            rewrite: RewriteCfg::default(),
            phrases: [
                "Etnetera", "Radek Beran", "Claude Code", "Codex", "GitHub", "GitLab", "Kubernetes", "Docker", "Keycloak",
                "Kotlin", "Spring Boot", "PostgreSQL", "Ubuntu", "Wayland", "PipeWire", "Dictee", "Mluva",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

impl Config {
    pub fn dir() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("dictee")
    }

    pub fn path() -> PathBuf {
        Self::dir().join("config.toml")
    }

    /// Loads the config; a missing file gives (and writes) the defaults, a broken file is an error.
    pub fn load() -> Result<Self> {
        let path = Self::path();
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("invalid config {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Self::default();
                cfg.save()?;
                Ok(cfg)
            }
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        std::fs::create_dir_all(Self::dir())?;
        let tmp = Self::path().with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, Self::path())?;
        Ok(())
    }

    /// Phrases without blanks or duplicates.
    pub fn clean_phrases(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in &self.phrases {
            let p = p.trim();
            if !p.is_empty() && !out.iter().any(|o| o == p) {
                out.push(p.to_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_gets_defaults() {
        let cfg: Config = toml::from_str("model = \"tiny\"\n[rewrite]\nengine = \"claude\"").unwrap();
        assert_eq!(cfg.model, "tiny");
        assert_eq!(cfg.rewrite.engine, "claude");
        assert_eq!(cfg.rewrite.timeout_secs, 60);
        assert!(!cfg.phrases.is_empty());
    }

    #[test]
    fn roundtrip() {
        let cfg = Config::default();
        assert_eq!(toml::from_str::<Config>(&toml::to_string_pretty(&cfg).unwrap()).unwrap(), cfg);
    }

    #[test]
    fn phrases_are_cleaned() {
        let cfg = Config { phrases: vec![" A ".into(), "".into(), "A".into(), "B".into()], ..Config::default() };
        assert_eq!(cfg.clean_phrases(), vec!["A", "B"]);
    }
}
