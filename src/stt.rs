//! Local speech-to-text with whisper.cpp.

use crate::config::Config;
use crate::models;
use anyhow::{bail, Context, Result};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Shorter audio than this (seconds) is treated as an accidental press.
pub const MIN_SECONDS: f32 = 0.4;
const MAX_PROMPT_CHARS: usize = 500;

#[derive(Default)]
pub struct Stt {
    loaded: Option<(String, WhisperContext)>,
}

impl Stt {
    fn context(&mut self, model: &str) -> Result<&WhisperContext> {
        if self.loaded.as_ref().map(|(id, _)| id.as_str()) != Some(model) {
            let path = models::path(model)?;
            if !path.is_file() {
                bail!("model '{model}' is not downloaded - open Settings and download it");
            }
            whisper_rs::install_logging_hooks(); // silence whisper.cpp / ggml stderr output
            let mut cp = WhisperContextParameters::default();
            cp.flash_attn(false);
            let ctx = WhisperContext::new_with_params(path.to_str().context("bad model path")?, cp)
                .map_err(|e| anyhow::anyhow!("cannot load model: {e}"))?;
            self.loaded = Some((model.to_string(), ctx));
        }
        Ok(&self.loaded.as_ref().unwrap().1)
    }

    /// Transcribes 16 kHz mono samples. Returns an empty string for silence.
    pub fn transcribe(&mut self, cfg: &Config, samples: &[f32]) -> Result<String> {
        if (samples.len() as f32) < crate::audio::SAMPLE_RATE as f32 * MIN_SECONDS {
            return Ok(String::new());
        }
        let prompt = build_prompt(&cfg.clean_phrases());
        let ctx = self.context(&cfg.model)?;
        let mut state = ctx.create_state().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(cfg.threads);
        match cfg.language.as_str() {
            "auto" => params.set_language(Some("auto")),
            lang => params.set_language(Some(lang)),
        }
        if !prompt.is_empty() {
            params.set_initial_prompt(&prompt);
        }
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_nst(true);
        // Pad with a little silence: whisper drops the last words of very tight recordings.
        let mut padded = samples.to_vec();
        padded.extend(std::iter::repeat(0.0).take(crate::audio::SAMPLE_RATE as usize / 4));
        state.full(params, &padded).map_err(|e| anyhow::anyhow!("transcription failed: {e}"))?;
        let mut text = String::new();
        for seg in state.as_iter() {
            text.push_str(&seg.to_string());
        }
        Ok(clean_text(&text))
    }
}

/// Whisper treats the initial prompt as earlier text, so a plain term list biases spelling.
pub fn build_prompt(phrases: &[String]) -> String {
    let mut out = String::new();
    for p in phrases {
        if out.chars().count() + p.chars().count() + 2 > MAX_PROMPT_CHARS {
            break;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(p);
    }
    if out.is_empty() { out } else { format!("{out}.") }
}

/// Removes whisper's non-speech markers like `[BLANK_AUDIO]` or `(music)` and tidies blanks.
pub fn clean_text(raw: &str) -> String {
    let mut out = String::new();
    let mut depth_close: Option<char> = None;
    for c in raw.chars() {
        match (depth_close, c) {
            (None, '[') => depth_close = Some(']'),
            (None, '(') => depth_close = Some(')'),
            (Some(close), c) if c == close => depth_close = None,
            (Some(_), _) => {}
            (None, c) => out.push(c),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_markers() {
        assert_eq!(clean_text(" [BLANK_AUDIO] Hello  world (music) "), "Hello world");
        assert_eq!(clean_text("[ Silence ]"), "");
    }

    #[test]
    fn prompt_is_limited() {
        let many: Vec<String> = (0..500).map(|i| format!("Term{i}")).collect();
        assert!(build_prompt(&many).chars().count() <= MAX_PROMPT_CHARS + 1);
        assert_eq!(build_prompt(&["A".into(), "B".into()]), "A, B.");
        assert_eq!(build_prompt(&[]), "");
    }
}
