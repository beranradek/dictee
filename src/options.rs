//! Fixed choices shared by the config validation, the settings window and the engines (single source of truth).

#[derive(Debug, Clone, Copy)]
pub struct Choice {
    pub id: &'static str,
    pub label: &'static str,
}

const fn c(id: &'static str, label: &'static str) -> Choice {
    Choice { id, label }
}

pub const STT_ENGINES: &[Choice] = &[c("whisper", "Whisper (whisper.cpp)"), c("qwen", "Qwen3-ASR (llama.cpp)")];
pub const DEVICES: &[Choice] = &[c("cpu", "CPU"), c("vulkan", "GPU - Vulkan (experimental)")];
pub const PASTE_KEYS: &[Choice] = &[c("ctrl+v", "Ctrl+V"), c("ctrl+shift+v", "Ctrl+Shift+V (terminals)")];
pub const REWRITE_ENGINES: &[Choice] = &[
    c("none", "Off - raw transcript"),
    c("claude", "Claude CLI"),
    c("codex", "Codex CLI (exec)"),
    c("codex-app", "Codex app-server"),
    c("openai", "OpenAI-compatible API"),
];

pub struct Language {
    pub choice: Choice,
    /// Name Qwen3-ASR expects when the language is forced (None = automatic detection)
    pub qwen_name: Option<&'static str>,
}

pub const LANGUAGES: &[Language] = &[
    Language { choice: c("auto", "Auto (cs / en)"), qwen_name: None },
    Language { choice: c("cs", "Czech"), qwen_name: Some("Czech") },
    Language { choice: c("en", "English"), qwen_name: Some("English") },
];

pub fn contains(list: &[Choice], id: &str) -> bool {
    list.iter().any(|x| x.id == id)
}

pub fn index_of(list: &[Choice], id: &str) -> usize {
    list.iter().position(|x| x.id == id).unwrap_or(0)
}

pub fn language(id: &str) -> Option<&'static Language> {
    LANGUAGES.iter().find(|l| l.choice.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookups() {
        assert!(contains(STT_ENGINES, "qwen") && !contains(STT_ENGINES, "x"));
        assert_eq!(index_of(REWRITE_ENGINES, "codex-app"), 3);
        assert_eq!(index_of(REWRITE_ENGINES, "unknown"), 0);
        assert_eq!(language("cs").unwrap().qwen_name, Some("Czech"));
        assert!(language("auto").unwrap().qwen_name.is_none());
    }
}
