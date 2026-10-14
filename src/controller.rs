//! Dictation state machine: record -> transcribe -> (rewrite) -> deliver.

use crate::audio::Recorder;
use crate::config::Config;
use crate::stt::Transcribe;
use crate::{deliver, rewrite, util};
use anyhow::{Result, bail};
use async_channel::Sender;
use std::sync::atomic::AtomicU32;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The bubble holds keyboard focus on Wayland: hide it this long before the text is copied and pasted, so focus can return.
const FOCUS_RETURN_DELAY: Duration = Duration::from_millis(250);
/// Paste keys are sent this long after the copy, so the compositor already has the new selection.
const PASTE_DELAY: Duration = Duration::from_millis(150);
const NOTIFY_MAX_CHARS: usize = 300;

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
    /// Hide the bubble now (it holds keyboard focus; text must be pasted into the previous window)
    HideBubble,
    ShowSettings,
    Quit,
}

pub type SharedState = Arc<Mutex<State>>;

pub struct Outcome {
    pub raw: String,
    pub text: String,
    /// Set when the rewrite failed and the raw text was used
    pub rewrite_error: Option<String>,
}

/// Transcribes and optionally rewrites. `on_state` reports the stage changes.
pub fn process<T: Transcribe>(cfg: &Config, stt: &mut T, samples: &[f32], mut on_state: impl FnMut(State)) -> Result<Outcome> {
    on_state(State::Transcribing);
    let raw = stt.transcribe(cfg, samples)?;
    if raw.is_empty() || cfg.rewrite.engine == "none" {
        return Ok(Outcome { text: raw.clone(), raw, rewrite_error: None });
    }
    on_state(State::Rewriting);
    match rewrite::rewrite(cfg, &raw) {
        Ok(text) => Ok(Outcome { raw, text, rewrite_error: None }),
        Err(e) => Ok(Outcome { text: raw.clone(), raw, rewrite_error: Some(format!("{e:#}")) }),
    }
}

/// Side effects of delivering text; a trait so the flow is testable.
pub trait Output {
    fn copy(&self, text: &str) -> Result<()>;
    fn paste(&self, keys: &str) -> Result<()>;
    fn notify(&self, title: &str, body: &str);
}

pub struct SystemOutput;

impl Output for SystemOutput {
    fn copy(&self, text: &str) -> Result<()> {
        deliver::copy(text)
    }
    fn paste(&self, keys: &str) -> Result<()> {
        deliver::paste(keys)
    }
    fn notify(&self, title: &str, body: &str) {
        deliver::notify(title, &util::truncate(body, NOTIFY_MAX_CHARS));
    }
}

/// The whole flow after recording: transcribe, rewrite, copy, paste, notify.
/// `hide_bubble` runs just before the text is delivered. Returns the delivered text (None = nothing was said).
pub fn finish<T: Transcribe>(
    cfg: &Config,
    stt: &mut T,
    out: &impl Output,
    samples: &[f32],
    on_state: impl FnMut(State),
    mut hide_bubble: impl FnMut(),
    delays: (Duration, Duration),
) -> Result<Option<String>> {
    if samples.is_empty() {
        bail!("no audio was captured - check the microphone in Settings");
    }
    let outcome = process(cfg, stt, samples, on_state)?;
    if outcome.text.is_empty() {
        hide_bubble();
        if cfg.notify {
            out.notify("No speech detected", "");
        }
        return Ok(None);
    }
    hide_bubble();
    std::thread::sleep(delays.0);
    out.copy(&outcome.text)?;
    let mut warning = outcome.rewrite_error.map(|e| format!("Rewrite failed, raw text used: {e}"));
    if cfg.auto_paste {
        std::thread::sleep(delays.1);
        if let Err(e) = out.paste(&cfg.paste_keys) {
            warning = Some(format!("Copied, but auto-paste failed: {e:#}"));
        }
    }
    match &warning {
        Some(w) => out.notify("Copied (with warning)", &format!("{}\n{w}", outcome.text)),
        None if cfg.notify => out.notify("Copied to clipboard", &outcome.text),
        None => {}
    }
    Ok(Some(outcome.text))
}

pub struct Controller<T: Transcribe> {
    stt: T,
    recorder: Option<Recorder>,
    ui: Sender<Event>,
    /// Lets the recorder ask for a stop when it reaches its length limit
    cmd_tx: std::sync::mpsc::Sender<Cmd>,
    shared: SharedState,
    show_bubble: bool,
}

impl<T: Transcribe> Controller<T> {
    pub fn new(stt: T, ui: Sender<Event>, shared: SharedState, cmd_tx: std::sync::mpsc::Sender<Cmd>) -> Self {
        Self { stt, recorder: None, ui, cmd_tx, shared, show_bubble: true }
    }

    fn emit(&self, ev: Event) {
        let _ = self.ui.send_blocking(ev);
    }

    fn set_state(&self, s: State) {
        *self.shared.lock().unwrap() = s;
        // The bubble setting is read when a recording starts; the state itself is always tracked.
        if self.show_bubble || s == State::Idle {
            self.emit(Event::State(s));
        }
    }

    fn fail(&self, title: &str, e: &anyhow::Error) {
        eprintln!("dictee: {title}: {e:#}");
        self.emit(Event::HideBubble);
        SystemOutput.notify(title, &format!("{e:#}"));
    }

    pub fn run(mut self, rx: Receiver<Cmd>) {
        while let Ok(cmd) = rx.recv() {
            let was_recording = self.recorder.is_some();
            if !self.handle(cmd) {
                break;
            }
            if was_recording && self.recorder.is_none() {
                // Processing blocked this thread. Start, stop and cancel requests that piled up meanwhile (a second shortcut
                // press, a bubble click) refer to a state that no longer exists and must not start a ghost recording.
                while let Ok(late) = rx.try_recv() {
                    if matches!(late, Cmd::Quit | Cmd::ShowSettings) && !self.handle(late) {
                        return;
                    }
                }
            }
        }
    }

    /// Returns false when the daemon should exit.
    fn handle(&mut self, cmd: Cmd) -> bool {
        let recording = self.recorder.is_some();
        match cmd {
            Cmd::Toggle | Cmd::Stop if recording => self.stop(),
            Cmd::Toggle | Cmd::Start if !recording => self.start(),
            Cmd::Cancel => self.cancel(),
            Cmd::ShowSettings => self.emit(Event::ShowSettings),
            Cmd::Quit => {
                self.cancel();
                self.emit(Event::Quit);
                return false;
            }
            _ => {}
        }
        true
    }

    fn start(&mut self) {
        let result = Config::load().and_then(|cfg| {
            let tx = self.cmd_tx.clone();
            let recorder = Recorder::start(&cfg.mic, move || {
                SystemOutput.notify("Recording limit reached", "Dictation was stopped after the maximum length");
                let _ = tx.send(Cmd::Stop);
            })?;
            self.show_bubble = cfg.show_bubble;
            self.stt.prewarm(&cfg);
            Ok(recorder)
        });
        match result {
            Ok(r) => {
                self.emit(Event::Level(r.level()));
                self.recorder = Some(r);
                self.set_state(State::Recording);
            }
            Err(e) => self.fail("Cannot start dictation", &e),
        }
    }

    fn cancel(&mut self) {
        if let Some(r) = self.recorder.take() {
            r.cancel();
            self.stt.recording_ended();
            self.set_state(State::Idle);
        }
    }

    fn stop(&mut self) {
        let samples = self.recorder.take().map(|r| r.stop()).unwrap_or_default();
        if std::env::var_os("DICTEE_DEBUG").is_some() {
            let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
            eprintln!("dictee: recorded {:.1} s, rms {rms:.4}", samples.len() as f32 / crate::audio::SAMPLE_RATE as f32);
        }
        self.stt.recording_ended(); // also after an accidental tap: a warmed-up engine must not stay loaded for nothing
        let result = Config::load().and_then(|cfg| {
            let (shared, ui, show) = (self.shared.clone(), self.ui.clone(), self.show_bubble);
            let on_state = |s: State| {
                *shared.lock().unwrap() = s;
                if show {
                    let _ = ui.send_blocking(Event::State(s));
                }
            };
            let ui2 = self.ui.clone();
            let hide = || {
                let _ = ui2.send_blocking(Event::HideBubble);
            };
            finish(&cfg, &mut self.stt, &SystemOutput, &samples, on_state, hide, (FOCUS_RETURN_DELAY, PASTE_DELAY))
        });
        self.set_state(State::Idle);
        if let Err(e) = result {
            self.fail("Dictation failed", &e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakeStt(&'static str);
    impl Transcribe for FakeStt {
        fn transcribe(&mut self, _: &Config, _: &[f32]) -> Result<String> {
            Ok(self.0.to_string())
        }
    }

    #[derive(Default)]
    struct FakeOut {
        calls: RefCell<Vec<String>>,
        paste_fails: bool,
    }
    impl Output for FakeOut {
        fn copy(&self, t: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("copy:{t}"));
            Ok(())
        }
        fn paste(&self, k: &str) -> Result<()> {
            self.calls.borrow_mut().push(format!("paste:{k}"));
            if self.paste_fails { bail!("no uinput") } else { Ok(()) }
        }
        fn notify(&self, title: &str, body: &str) {
            self.calls.borrow_mut().push(format!("notify:{title}|{body}"));
        }
    }

    const NO_DELAY: (Duration, Duration) = (Duration::ZERO, Duration::ZERO);

    fn run(cfg: &Config, text: &'static str, out: &FakeOut, samples: &[f32]) -> Result<Option<String>> {
        finish(cfg, &mut FakeStt(text), out, samples, |_| {}, || {}, NO_DELAY)
    }

    #[test]
    fn copies_and_notifies() {
        let out = FakeOut::default();
        assert_eq!(run(&Config::default(), "hello", &out, &[0.1; 10]).unwrap().as_deref(), Some("hello"));
        assert_eq!(*out.calls.borrow(), vec!["copy:hello", "notify:Copied to clipboard|hello"]);
    }

    #[test]
    fn empty_audio_is_an_error() {
        let err = run(&Config::default(), "x", &FakeOut::default(), &[]).unwrap_err();
        assert!(err.to_string().contains("microphone"));
    }

    #[test]
    fn silence_delivers_nothing() {
        let out = FakeOut::default();
        assert_eq!(run(&Config::default(), "", &out, &[0.1; 10]).unwrap(), None);
        assert_eq!(*out.calls.borrow(), vec!["notify:No speech detected|"]);
    }

    #[test]
    fn auto_paste_failure_is_a_warning_not_an_error() {
        let out = FakeOut { paste_fails: true, ..FakeOut::default() };
        let cfg = Config { auto_paste: true, ..Config::default() };
        assert_eq!(run(&cfg, "hi", &out, &[0.1; 10]).unwrap().as_deref(), Some("hi"));
        let calls = out.calls.borrow();
        assert_eq!(calls[0], "copy:hi");
        assert_eq!(calls[1], "paste:ctrl+v");
        assert!(calls[2].starts_with("notify:Copied (with warning)|hi\nCopied, but auto-paste failed"));
    }

    #[test]
    fn failed_rewrite_falls_back_to_raw_text() {
        let mut cfg = Config::default();
        cfg.rewrite.engine = "openai".into();
        cfg.rewrite.model = "m".into();
        cfg.rewrite.base_url = "http://127.0.0.1:1/v1".into(); // nothing listens there
        let out = FakeOut::default();
        assert_eq!(run(&cfg, "raw text", &out, &[0.1; 10]).unwrap().as_deref(), Some("raw text"));
        let calls = out.calls.borrow();
        assert_eq!(calls[0], "copy:raw text");
        assert!(calls[1].contains("Rewrite failed, raw text used"));
    }

    #[test]
    fn states_are_reported_in_order() {
        let mut seen = Vec::new();
        process(&Config::default(), &mut FakeStt("x"), &[0.1], |s| seen.push(s)).unwrap();
        assert_eq!(seen, vec![State::Transcribing]);
    }
}
