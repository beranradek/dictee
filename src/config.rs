//! User configuration stored as TOML in `~/.config/dictee/config.toml`.

use crate::options;
use anyhow::{Context, Result, bail};
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
                "Claude Code",
                "Codex",
                "GitHub",
                "GitLab",
                "Kubernetes",
                "Docker",
                "Keycloak",
                "Kotlin",
                "Spring Boot",
                "PostgreSQL",
                "Ubuntu",
                "Wayland",
                "PipeWire",
                "Dictee",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        }
    }
}

impl Config {
    /// Folder for downloaded models (`~` is expanded); the default is under the user data dir.
    pub fn models_path(&self) -> PathBuf {
        let configured = self.models_dir.trim();
        if configured.is_empty() {
            return dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("dictee").join("models");
        }
        match configured.strip_prefix("~/") {
            Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
            None => PathBuf::from(configured),
        }
    }

    /// Rejects values the engines cannot work with, with a message that names the setting.
    pub fn validate(&self) -> Result<()> {
        let check = |ok: bool, name: &str, value: &str, allowed: &[options::Choice]| -> Result<()> {
            if !ok {
                let list: Vec<&str> = allowed.iter().map(|c| c.id).collect();
                bail!("invalid {name} '{value}' (allowed: {})", list.join(", "));
            }
            Ok(())
        };
        check(options::contains(options::STT_ENGINES, &self.stt_engine), "stt_engine", &self.stt_engine, options::STT_ENGINES)?;
        check(options::contains(options::PASTE_KEYS, &self.paste_keys), "paste_keys", &self.paste_keys, options::PASTE_KEYS)?;
        check(
            options::contains(options::REWRITE_ENGINES, &self.rewrite.engine),
            "rewrite.engine",
            &self.rewrite.engine,
            options::REWRITE_ENGINES,
        )?;
        check(options::contains(options::DEVICES, &self.qwen.device), "qwen.device", &self.qwen.device, options::DEVICES)?;
        crate::models::find(&self.model)?;
        crate::qwen::find(&self.qwen.model)?;
        if options::language(&self.language).is_none() {
            let list: Vec<&str> = options::LANGUAGES.iter().map(|l| l.choice.id).collect();
            bail!("invalid language '{}' (allowed: {})", self.language, list.join(", "));
        }
        Ok(())
    }

    pub fn dir() -> PathBuf {
        dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("dictee")
    }

    /// `$DICTEE_CONFIG` (tests, a second instance), else `~/.config/dictee/config.toml`.
    pub fn path() -> PathBuf {
        std::env::var_os("DICTEE_CONFIG").map(PathBuf::from).unwrap_or_else(|| Self::dir().join("config.toml"))
    }

    /// Loads the config; a missing file gives (and writes) the defaults, a broken file is an error.
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &std::path::Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let mut cfg: Self = toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))?;
                cfg.threads = cfg.threads.clamp(1, 64);
                cfg.qwen.threads = cfg.qwen.threads.clamp(0, 64);
                cfg.rewrite.timeout_secs = cfg.rewrite.timeout_secs.clamp(5, 3600); // a huge value would overflow the deadline
                cfg.qwen.keep_alive_secs = cfg.qwen.keep_alive_secs.min(24 * 3600);
                cfg.validate().with_context(|| format!("invalid config {}", path.display()))?;
                Ok(cfg)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let cfg = Self::default();
                cfg.save_to(path)?;
                Ok(cfg)
            }
            Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
        }
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Applies only what differs between `before` and `after` onto `target`.
    /// The settings window uses it so that a value it never touched (e.g. edited in the file meanwhile) is not overwritten.
    pub fn merge_changes(before: &Self, after: &Self, target: &Self) -> Result<Self> {
        use serde_json::{Value, to_value};
        fn merge(before: &Value, after: &Value, target: &mut Value) {
            if let (Value::Object(b), Value::Object(a), Value::Object(t)) = (before, after, &mut *target) {
                for (key, new) in a {
                    match (b.get(key), t.get_mut(key)) {
                        (Some(old), _) if old == new => {}
                        (Some(old), Some(slot)) if old.is_object() && new.is_object() => merge(old, new, slot),
                        _ => {
                            t.insert(key.clone(), new.clone());
                        }
                    }
                }
            }
        }
        let mut merged = to_value(target)?;
        merge(&to_value(before)?, &to_value(after)?, &mut merged);
        Ok(serde_json::from_value(merged)?)
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
    fn validation_names_the_bad_setting() {
        let mut cfg = Config::default();
        assert!(cfg.validate().is_ok());
        cfg.language = "de".into();
        assert!(cfg.validate().unwrap_err().to_string().contains("language"));
        cfg.language = "cs".into();
        cfg.rewrite.engine = "gpt".into();
        assert!(cfg.validate().unwrap_err().to_string().contains("rewrite.engine"));
    }

    #[test]
    fn models_path_expands_home_and_default() {
        let mut cfg = Config::default();
        assert!(cfg.models_path().ends_with("dictee/models"));
        cfg.models_dir = "/mnt/data/models".into();
        assert_eq!(cfg.models_path(), PathBuf::from("/mnt/data/models"));
        cfg.models_dir = "~/m".into();
        assert!(!cfg.models_path().starts_with("~") && cfg.models_path().ends_with("m"));
    }

    #[test]
    fn load_creates_defaults_and_rejects_broken_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("config.toml");
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!(cfg, Config::default());
        assert!(path.is_file(), "defaults are written on first load");
        std::fs::write(&path, "threads = 1000\n[qwen]\nthreads = -5\n").unwrap();
        let clamped = Config::load_from(&path).unwrap();
        assert_eq!((clamped.threads, clamped.qwen.threads), (64, 0));
        std::fs::write(&path, "language = \"xx\"\n").unwrap();
        assert!(Config::load_from(&path).unwrap_err().to_string().contains("invalid config"));
        std::fs::write(&path, "not toml [[[").unwrap();
        assert!(Config::load_from(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "not toml [[[", "a broken file is never overwritten by load");
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        let mut cfg = Config::default();
        cfg.rewrite.engine = "openai".into();
        cfg.phrases = vec!["Zeta".into()];
        cfg.save_to(&path).unwrap();
        assert_eq!(Config::load_from(&path).unwrap(), cfg);
    }

    #[test]
    fn merge_applies_only_changed_fields() {
        let before = Config::default();
        let mut after = before.clone(); // what the settings window shows after the user changed two things
        after.notify = !before.notify;
        after.rewrite.engine = "claude".into();
        let mut target = before.clone(); // the file, edited by hand meanwhile
        target.model = "medium".into();
        target.rewrite.timeout_secs = 99;
        let merged = Config::merge_changes(&before, &after, &target).unwrap();
        assert_eq!(merged.model, "medium", "untouched field keeps the hand edit");
        assert_eq!(merged.rewrite.timeout_secs, 99, "nested untouched field keeps the hand edit");
        assert_eq!(merged.notify, after.notify);
        assert_eq!(merged.rewrite.engine, "claude");
    }

    #[test]
    fn defaults_are_valid_and_point_to_existing_models() {
        let cfg = Config::default();
        cfg.validate().unwrap();
        assert!(crate::models::find(&cfg.model).is_ok() && crate::qwen::find(&cfg.qwen.model).is_ok());
        let mut bad = cfg.clone();
        bad.model = "no-such-model".into();
        assert!(bad.validate().is_err(), "unknown model ids are rejected, not silently replaced in the UI");
    }

    #[test]
    fn extreme_timeouts_are_clamped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "[rewrite]\ntimeout_secs = 9223372036854775807\n[qwen]\nkeep_alive_secs = 99999999\n").unwrap();
        let cfg = Config::load_from(&path).unwrap();
        assert_eq!((cfg.rewrite.timeout_secs, cfg.qwen.keep_alive_secs), (3600, 86400));
    }

    #[test]
    fn phrases_are_cleaned() {
        let cfg = Config { phrases: vec![" A ".into(), "".into(), "A".into(), "B".into()], ..Config::default() };
        assert_eq!(cfg.clean_phrases(), vec!["A", "B"]);
    }
}
