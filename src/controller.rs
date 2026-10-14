//! Dictation state machine: record -> transcribe -> (rewrite) -> deliver.

use crate::audio::Recorder;
use crate::config::Config;
use crate::{deliver, rewrite, stt::Stt};
use anyhow::Result;
use std::sync::atomic::AtomicU32;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Recording,
    Transcribing,
    Rewriting,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Recording => "recording",
            State::Transcribing => "transcribing",
            State::Rewriting => "rewriting",
        }
    }
}

#[derive(Debug)]
pub enum Cmd {
    Start,
    Stop,
    Toggle,
    Cancel,
    ShowSettings,
    Quit,
}

/// Messages for the GUI thread.
#[derive(Debug)]
pub enum Event {
    State(State),
    /// Input level source while recording
    Level(Arc<AtomicU32>),
    Done,
    Error(String),
    ShowSettings,
    Quit,
    /// Hide the bubble now (it holds keyboard focus; text must be pasted into the previous window)
    HideBubble,
}

pub type SharedState = Arc<Mutex<State>>;

pub struct Outcome {
    pub raw: String,
    pub text: String,
    /// Set when the rewrite failed and the raw text was used
    pub rewrite_error: Option<String>,
}

/// Transcribes and optionally rewrites. `on_state` reports the stage changes.
pub fn process(cfg: &Config, stt: &mut Stt, samples: &[f32], mut on_state: impl FnMut(State)) -> Result<Outcome> {
    on_state(State::Transcribing);
    let raw = stt.transcribe(cfg, samples)?;
    if raw.is_empty() || cfg.rewrite.engine == "none" {
        return Ok(Outcome { text: raw.clone(), raw, rewrite_error: None });
    }
    on_state(State::Rewriting);
    match rewrite::rewrite(cfg, &raw) {
        Ok(text) => Ok(Outcome { raw, text, rewrite_error: None }),
        Err(e) => Ok(Outcome { text: raw.clone(), raw, rewrite_error: Some(e.to_string()) }),
    }
}

pub fn run(rx: Receiver<Cmd>, ui: Sender<Event>, shared: SharedState) {
    let mut stt = Stt::default();
    let mut recorder: Option<Recorder> = None;
    let set = |s: State| {
        *shared.lock().unwrap() = s;
        let _ = ui.send(Event::State(s));
    };
    for cmd in rx {
        let recording = recorder.is_some();
        match cmd {
            Cmd::Toggle if recording => stop(&mut recorder, &mut stt, &ui, &set),
            Cmd::Toggle | Cmd::Start if !recording => match start(&mut stt) {
                Ok(r) => {
                    let _ = ui.send(Event::Level(r.level()));
                    recorder = Some(r);
                    set(State::Recording);
                }
                Err(e) => {
                    deliver::notify("Cannot start dictation", &format!("{e:#}"));
                    let _ = ui.send(Event::Error(format!("{e:#}")));
                }
            },
            Cmd::Stop if recording => stop(&mut recorder, &mut stt, &ui, &set),
            Cmd::Cancel => {
                if let Some(r) = recorder.take() {
                    r.cancel();
                    set(State::Idle);
                }
            }
            Cmd::ShowSettings => {
                let _ = ui.send(Event::ShowSettings);
            }
            Cmd::Quit => {
                if let Some(r) = recorder.take() {
                    r.cancel();
                }
                let _ = ui.send(Event::Quit);
                break;
            }
            _ => {}
        }
    }
}

fn start(stt: &mut Stt) -> Result<Recorder> {
    let cfg = Config::load()?;
    let recorder = Recorder::start(&cfg.mic)?;
    stt.prewarm(&cfg);
    Ok(recorder)
}

fn stop(recorder: &mut Option<Recorder>, stt: &mut Stt, ui: &Sender<Event>, set: &impl Fn(State)) {
    let samples = recorder.take().map(|r| r.stop()).unwrap_or_default();
    let result = Config::load().and_then(|cfg| {
        let out = process(&cfg, stt, &samples, set)?;
        if out.text.is_empty() {
            return Ok(None);
        }
        // The bubble takes focus on Wayland. Hide it first so focus returns to the target window.
        let _ = ui.send(Event::HideBubble);
        std::thread::sleep(std::time::Duration::from_millis(250));
        deliver::copy(&out.text)?;
        let mut note = String::new();
        if cfg.auto_paste {
            // Small delay so the compositor has the new selection before the key press.
            std::thread::sleep(std::time::Duration::from_millis(150));
            if let Err(e) = deliver::paste(&cfg.paste_keys) {
                note = format!("Copied, but auto-paste failed: {e:#}");
            }
        }
        if let Some(e) = &out.rewrite_error {
            note = format!("Rewrite failed, raw text used: {e}");
        }
        if cfg.notify || !note.is_empty() {
            deliver::notify(if note.is_empty() { "Copied to clipboard" } else { "Copied (with warning)" }, &format!("{}\n{note}", out.text));
        }
        Ok(Some(out.text))
    });
    set(State::Idle);
    match result {
        Ok(Some(_)) => {
            let _ = ui.send(Event::Done);
        }
        Ok(None) => {}
        Err(e) => {
            let _ = ui.send(Event::HideBubble);
            deliver::notify("Dictation failed", &format!("{e:#}"));
            let _ = ui.send(Event::Error(format!("{e:#}")));
        }
    }
}

