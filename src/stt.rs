//! Speech-to-text engines behind one entry point: Whisper (whisper.cpp, in process) and Qwen3-ASR (llama.cpp server).

use crate::audio::SAMPLE_RATE;
use crate::config::Config;
use crate::{models, qwen};
use anyhow::{Context, Result, bail};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

/// Shorter audio than this (seconds) is treated as an accidental press.
pub const MIN_SECONDS: f32 = 0.4;
const MAX_PROMPT_CHARS: usize = 500;
/// Recordings with an RMS level below this (about -54 dBFS) count as silence: this only catches dead or muted input, not a noisy room.
const SILENCE_RMS: f32 = 0.002;
/// Silence appended before decoding: whisper drops the last words of very tight recordings.
const TAIL_PADDING_SECONDS: f32 = 0.25;

/// Anything that turns audio into text; lets the dictation flow be tested without models.
pub trait Transcribe {
    /// Transcribes 16 kHz mono samples. Returns an empty string for silence or too short audio.
    fn transcribe(&mut self, cfg: &Config, samples: &[f32]) -> Result<String>;
    /// Called when a recording starts: lets a slow engine load while the user speaks.
    fn prewarm(&mut self, _cfg: &Config) {}
    /// Called when a recording ended (stopped or cancelled): a warmed-up engine is no longer waiting for audio.
    fn recording_ended(&mut self) {}
}

#[derive(Default)]
pub struct Stt {
    whisper: Option<(String, WhisperContext)>,
    qwen: Option<qwen::Server>,
}

impl Stt {
    fn qwen(&mut self) -> &qwen::Server {
        self.qwen.get_or_insert_with(qwen::Server::new)
    }

    fn whisper(&mut self, cfg: &Config) -> Result<&WhisperContext> {
        let key = format!("{}|{}", cfg.models_path().display(), cfg.model);
        if self.whisper.as_ref().map(|(k, _)| k.as_str()) != Some(key.as_str()) {
            let path = models::path(&cfg.models_path(), &cfg.model)?;
            if !path.is_file() {
                bail!("model '{}' is not downloaded - open Settings and download it", cfg.model);
            }
            whisper_rs::install_logging_hooks(); // silence whisper.cpp / ggml stderr output
            let ctx = WhisperContext::new_with_params(path.to_str().context("bad model path")?, WhisperContextParameters::default())
                .map_err(|e| anyhow::anyhow!("cannot load model: {e}"))?;
            self.whisper = Some((key, ctx));
        }
        Ok(&self.whisper.as_ref().expect("loaded above").1)
    }

    fn run_whisper(&mut self, cfg: &Config, samples: &[f32]) -> Result<String> {
        let prompt = build_prompt(&cfg.clean_phrases());
        let ctx = self.whisper(cfg)?;
        let mut state = ctx.create_state().map_err(|e| anyhow::anyhow!("{e}"))?;
        // Greedy decoding: beam search was several times slower for no visible gain in dictation.
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(cfg.threads);
        params.set_language(Some(cfg.language.as_str())); // "auto" = detect
        if !prompt.is_empty() {
            params.set_initial_prompt(&prompt);
        }
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params.set_suppress_nst(true);
        let mut padded = samples.to_vec();
        padded.resize(samples.len() + (SAMPLE_RATE as f32 * TAIL_PADDING_SECONDS) as usize, 0.0);
        state.full(params, &padded).map_err(|e| anyhow::anyhow!("transcription failed: {e}"))?;
        Ok(state.as_iter().map(|seg| seg.to_string()).collect())
    }
}

impl Transcribe for Stt {
    fn transcribe(&mut self, cfg: &Config, samples: &[f32]) -> Result<String> {
        if (samples.len() as f32) < SAMPLE_RATE as f32 * MIN_SECONDS || is_silent(samples) {
            return Ok(String::new()); // models invent text for silence, so do not ask them
        }
        let raw = if cfg.stt_engine == "qwen" { self.qwen().transcribe(cfg, samples)? } else { self.run_whisper(cfg, samples)? };
        Ok(clean_text(&raw))
    }

    fn prewarm(&mut self, cfg: &Config) {
        if cfg.stt_engine == "qwen" {
            self.qwen().prewarm(cfg.clone());
        }
    }

    fn recording_ended(&mut self) {
        if let Some(q) = &self.qwen {
            q.cancel_warmup();
        }
    }
}

/// True when the recording has no audible signal (dead or muted microphone).
fn is_silent(samples: &[f32]) -> bool {
    let mean_square = samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32;
    mean_square.sqrt() < SILENCE_RMS
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

/// Last words of a bracketed segment that mark it as a sound annotation (`[Music]`, `(soft music)`, `[BLANK_AUDIO]`).
const NON_SPEECH: &[&str] = &[
    "blank_audio",
    "silence",
    "music",
    "applause",
    "laughter",
    "noise",
    "inaudible",
    "sound",
    "ticho",
    "hudba",
    "potlesk",
    "smích",
    "šum",
];

fn is_annotation(inner: &str) -> bool {
    let words: Vec<String> = inner.split_whitespace().map(str::to_lowercase).collect();
    // Short, and ending in an annotation word: "(soft music)" yes, "(sound card)" and "[Silence of the Lambs]" no.
    words.len() <= 3 && words.last().is_some_and(|last| NON_SPEECH.contains(&last.as_str()))
}

/// Removes whisper's non-speech annotations like `[BLANK_AUDIO]` or `(music)` and tidies blanks.
/// Other bracketed text is kept: the user may really have dictated it.
pub fn clean_text(raw: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(start) = rest.find(['[', '(']) {
        let close = if rest[start..].starts_with('[') { ']' } else { ')' };
        out.push_str(&rest[..start]);
        match rest[start + 1..].find(close) {
            Some(len) => {
                if !is_annotation(&rest[start + 1..start + 1 + len]) {
                    out.push_str(&rest[start..start + len + 2]);
                }
                rest = &rest[start + len + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_sound_annotations_only() {
        assert_eq!(clean_text(" [BLANK_AUDIO] Hello  world (music) "), "Hello world");
        assert_eq!(clean_text("[ Silence ]"), "");
        assert_eq!(clean_text("(upbeat music) text"), "text");
        assert_eq!(clean_text("See the doc (page 3) and [1] now"), "See the doc (page 3) and [1] now");
        assert_eq!(clean_text("broken (open"), "broken (open");
        assert_eq!(
            clean_text("a (sound card) b [Silence of the Lambs] (noise reduction)"),
            "a (sound card) b [Silence of the Lambs] (noise reduction)"
        );
    }

    #[test]
    fn prompt_is_limited() {
        let many: Vec<String> = (0..500).map(|i| format!("Term{i}")).collect();
        assert!(build_prompt(&many).chars().count() <= MAX_PROMPT_CHARS + 1);
        assert_eq!(build_prompt(&["A".into(), "B".into()]), "A, B.");
        assert_eq!(build_prompt(&[]), "");
    }

    #[test]
    fn silence_is_detected() {
        assert!(is_silent(&[0.0; 16000]));
        assert!(is_silent(&[0.001; 16000]));
        let mut tick = vec![0.0f32; 16000];
        tick[0] = 0.1;
        assert!(is_silent(&tick), "one tiny tick in a dead recording is still silence");
        assert!(!is_silent(&[0.05; 16000]));
        assert!(!is_silent(&[0.0, 0.2, -0.3]));
    }

    #[test]
    fn short_or_silent_audio_is_empty_without_loading_models() {
        let mut stt = Stt::default();
        let cfg = Config { model: "no-such-model".into(), ..Config::default() };
        assert_eq!(stt.transcribe(&cfg, &[0.5; 100]).unwrap(), "");
        assert_eq!(stt.transcribe(&cfg, &[0.0; 16000]).unwrap(), "");
    }
}
