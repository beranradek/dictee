//! Dictation state machine: record -> transcribe -> (rewrite) -> deliver.

use crate::audio::{EndReason, Recorder};
use crate::config::Config;
use crate::stt::Transcribe;
use crate::{deliver, rewrite, util};
use anyhow::{Result, bail};
use async_channel::Sender;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
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
    match rewrite::rewrite(cfg, &raw).and_then(|reply| rewrite::check_reply(&raw, &reply)) {
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

/// How `finish` talks back while it works.
pub struct Hooks<'a> {
    /// Stage changes (transcribing, rewriting)
    pub on_state: &'a mut dyn FnMut(State),
    /// Runs just before the text is delivered
    pub hide_bubble: &'a mut dyn FnMut(),
    /// Set by the user to deliver nothing
    pub cancelled: &'a AtomicBool,
    /// Pauses before the copy (focus return) and before the paste
    pub delays: (Duration, Duration),
}

/// The whole flow after recording: transcribe, rewrite, copy, paste, notify.
/// Returns the delivered text (None = nothing was said, or the job was cancelled).
pub fn finish<T: Transcribe>(cfg: &Config, stt: &mut T, out: &impl Output, samples: &[f32], hooks: &mut Hooks) -> Result<Option<String>> {
    if samples.is_empty() {
        bail!("no audio was captured - check the microphone in Settings");
    }
    let outcome = process(cfg, stt, samples, &mut *hooks.on_state)?;
    if hooks.cancelled.load(Ordering::SeqCst) {
        return Ok(None); // the user cancelled while the engines were working: deliver nothing
    }
    if outcome.text.is_empty() {
        (hooks.hide_bubble)();
        if cfg.notify {
            out.notify("No speech detected", "");
        }
        return Ok(None);
    }
    (hooks.hide_bubble)();
    std::thread::sleep(hooks.delays.0);
    out.copy(&outcome.text)?;
    let mut warning = outcome.rewrite_error.map(|e| format!("Rewrite failed, raw text used: {e}"));
    if cfg.auto_paste {
        std::thread::sleep(hooks.delays.1);
        if let Err(e) = out.paste(&cfg.paste_keys) {
            warning = Some(format!("Copied, but auto-paste failed: {e:#}"));
        }
    }
    match &warning {
        // The dictated text goes into a notification only when the user allowed notifications.
        Some(w) if cfg.notify => out.notify("Copied (with warning)", &format!("{}\n{w}", outcome.text)),
        Some(w) => out.notify("Dictation warning", w),
        None if cfg.notify => out.notify("Copied to clipboard", &outcome.text),
        None => {}
    }
    Ok(Some(outcome.text))
}

/// What the controller and its worker threads share to talk to the GUI.
#[derive(Clone)]
struct Ctx {
    ui: Sender<Event>,
    shared: SharedState,
    show_bubble: bool,
}

impl Ctx {
    fn emit(&self, ev: Event) {
        let _ = self.ui.send_blocking(ev);
    }

    fn set_state(&self, s: State) {
        *self.shared.lock().unwrap_or_else(|e| e.into_inner()) = s;
        // The bubble setting is read when a recording starts; the state itself is always tracked.
        if self.show_bubble || s == State::Idle {
            self.emit(Event::State(s));
        }
    }

    fn fail(&self, title: &str, detail: &str) {
        eprintln!("dictee: {title}: {detail}");
        self.emit(Event::HideBubble);
        SystemOutput.notify(title, detail);
    }
}

/// The transcription of one recording, running on its own thread so that cancel and quit stay responsive.
struct Job {
    cancelled: Arc<AtomicBool>,
    running: Arc<AtomicBool>,
}

impl Job {
    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

pub struct Controller<T: Transcribe + Send + 'static> {
    stt: Arc<Mutex<T>>,
    recorder: Option<Recorder>,
    /// Configuration of the recording in progress, so start and stop use the same engine and settings
    session: Option<Config>,
    job: Option<Job>,
    ctx: Ctx,
    /// Lets the recorder ask for a stop when it ends by itself
    cmd_tx: std::sync::mpsc::Sender<Cmd>,
}

impl<T: Transcribe + Send + 'static> Controller<T> {
    pub fn new(stt: T, ui: Sender<Event>, shared: SharedState, cmd_tx: std::sync::mpsc::Sender<Cmd>) -> Self {
        Self {
            stt: Arc::new(Mutex::new(stt)),
            recorder: None,
            session: None,
            job: None,
            ctx: Ctx { ui, shared, show_bubble: true },
            cmd_tx,
        }
    }

    /// Handles commands until `Quit`. A panic while handling one command is reported and does not end the daemon.
    pub fn run(mut self, rx: Receiver<Cmd>) {
        while let Ok(cmd) = rx.recv() {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.handle(cmd))) {
                Ok(true) => {}
                Ok(false) => break,
                Err(_) => self.recover(),
            }
        }
        self.ctx.emit(Event::Quit); // also when the channel closed: a daemon without a controller must not linger
    }

    /// Returns false when the daemon should exit.
    fn handle(&mut self, cmd: Cmd) -> bool {
        let recording = self.recorder.is_some();
        let processing = self.job.as_ref().is_some_and(Job::is_running);
        match cmd {
            Cmd::Toggle | Cmd::Stop if recording => self.stop(),
            Cmd::Toggle | Cmd::Start if !recording && !processing => self.start(),
            Cmd::Cancel if recording => self.cancel_recording(),
            Cmd::Cancel if processing => self.cancel_job(),
            Cmd::ShowSettings => self.ctx.emit(Event::ShowSettings),
            Cmd::Quit => {
                self.cancel_recording();
                self.cancel_job();
                self.ctx.emit(Event::Quit);
                return false;
            }
            _ => {} // e.g. a second shortcut press while the text is being transcribed
        }
        true
    }

    fn recover(&mut self) {
        self.cancel_recording();
        self.ctx.fail("Dictation failed", "internal error - see the daemon log");
        self.ctx.set_state(State::Idle);
    }

    fn start(&mut self) {
        let result = Config::load().and_then(|cfg| {
            let tx = self.cmd_tx.clone();
            let recorder = Recorder::start(&cfg.mic, move |reason| {
                let _ = tx.send(Cmd::Stop);
                match reason {
                    EndReason::Limit => SystemOutput.notify("Recording limit reached", "Dictation was stopped after the maximum length"),
                    EndReason::Died => SystemOutput.notify("Microphone stream ended", "Dictation was stopped; check the microphone"),
                }
            })?;
            self.ctx.show_bubble = cfg.show_bubble;
            self.stt.lock().unwrap_or_else(|e| e.into_inner()).prewarm(&cfg);
            self.session = Some(cfg);
            Ok(recorder)
        });
        match result {
            Ok(r) => {
                self.ctx.emit(Event::Level(r.level()));
                self.recorder = Some(r);
                self.ctx.set_state(State::Recording);
            }
            Err(e) => self.ctx.fail("Cannot start dictation", &format!("{e:#}")),
        }
    }

    fn cancel_recording(&mut self) {
        if let Some(r) = self.recorder.take() {
            r.cancel();
            self.session = None;
            self.stt.lock().unwrap_or_else(|e| e.into_inner()).recording_ended();
            self.ctx.set_state(State::Idle);
        }
    }

    /// Asks the running transcription to deliver nothing; the worker finishes in the background.
    fn cancel_job(&mut self) {
        if let Some(job) = self.job.as_ref().filter(|j| j.is_running()) {
            job.cancelled.store(true, Ordering::SeqCst);
            self.ctx.emit(Event::HideBubble);
        }
    }

    fn stop(&mut self) {
        let samples = self.recorder.take().map(|r| r.stop()).unwrap_or_default();
        self.stt.lock().unwrap_or_else(|e| e.into_inner()).recording_ended(); // also after an accidental tap: a warmed-up engine must not stay loaded
        if std::env::var_os("DICTEE_DEBUG").is_some() {
            let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32).sqrt();
            eprintln!("dictee: recorded {:.1} s, rms {rms:.4}", samples.len() as f32 / crate::audio::SAMPLE_RATE as f32);
        }
        let cfg = match self.session.take().map_or_else(Config::load, Ok) {
            Ok(cfg) => cfg,
            Err(e) => {
                self.ctx.fail("Dictation failed", &format!("{e:#}"));
                self.ctx.set_state(State::Idle);
                return;
            }
        };
        let job = Job { cancelled: Arc::new(AtomicBool::new(false)), running: Arc::new(AtomicBool::new(true)) };
        let (stt, ctx, cancelled, running) = (self.stt.clone(), self.ctx.clone(), job.cancelled.clone(), job.running.clone());
        self.job = Some(job);
        self.ctx.set_state(State::Transcribing);
        std::thread::spawn(move || {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut stt = stt.lock().unwrap_or_else(|e| e.into_inner());
                let mut hooks = Hooks {
                    on_state: &mut |s| ctx.set_state(s),
                    hide_bubble: &mut || ctx.emit(Event::HideBubble),
                    cancelled: &cancelled,
                    delays: (FOCUS_RETURN_DELAY, PASTE_DELAY),
                };
                finish(&cfg, &mut *stt, &SystemOutput, &samples, &mut hooks)
            }));
            match outcome {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => ctx.fail("Dictation failed", &format!("{e:#}")),
                Err(_) => ctx.fail("Dictation failed", "internal error - see the daemon log"),
            }
            running.store(false, Ordering::SeqCst);
            ctx.set_state(State::Idle);
        });
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

    /// Runs `finish` with no pauses; `cancelled` and the stage log are given by the test.
    fn run_with(cfg: &Config, text: &'static str, out: &FakeOut, samples: &[f32], cancelled: &AtomicBool) -> Result<Option<String>> {
        let mut hooks = Hooks { on_state: &mut |_| {}, hide_bubble: &mut || {}, cancelled, delays: (Duration::ZERO, Duration::ZERO) };
        finish(cfg, &mut FakeStt(text), out, samples, &mut hooks)
    }

    fn run(cfg: &Config, text: &'static str, out: &FakeOut, samples: &[f32]) -> Result<Option<String>> {
        run_with(cfg, text, out, samples, &AtomicBool::new(false))
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
    fn cancelled_job_delivers_nothing() {
        let out = FakeOut::default();
        let cancelled = AtomicBool::new(true);
        let result = run_with(&Config::default(), "hello", &out, &[0.1; 10], &cancelled).unwrap();
        assert_eq!(result, None);
        assert!(out.calls.borrow().is_empty(), "no copy, no notification");
    }

    #[test]
    fn warning_without_notify_setting_does_not_leak_the_text() {
        let out = FakeOut { paste_fails: true, ..FakeOut::default() };
        let cfg = Config { auto_paste: true, notify: false, ..Config::default() };
        run(&cfg, "secret words", &out, &[0.1; 10]).unwrap();
        let calls = out.calls.borrow();
        let last = calls.last().unwrap();
        assert!(last.starts_with("notify:Dictation warning|") && !last.contains("secret words"), "{last}");
    }

    #[test]
    fn states_are_reported_in_order() {
        let mut seen = Vec::new();
        process(&Config::default(), &mut FakeStt("x"), &[0.1], |s| seen.push(s)).unwrap();
        assert_eq!(seen, vec![State::Transcribing]);
    }
}
