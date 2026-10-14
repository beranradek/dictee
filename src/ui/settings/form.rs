//! The settings widgets and their mapping to and from `Config`.

use crate::audio;
use crate::config::Config;
use crate::options::{self, Choice};
use crate::{models, qwen};
use adw::prelude::*;
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub fn choices(list: &[Choice]) -> gtk::StringList {
    gtk::StringList::new(&list.iter().map(|c| c.label).collect::<Vec<_>>())
}

fn combo(title: &str, list: &[Choice]) -> adw::ComboRow {
    adw::ComboRow::builder().title(title).model(&choices(list)).build()
}

fn entry(title: &str) -> adw::EntryRow {
    adw::EntryRow::builder().title(title).build()
}

fn switch(title: &str, subtitle: &str) -> adw::SwitchRow {
    adw::SwitchRow::builder().title(title).subtitle(subtitle).build()
}

fn pick<'a>(list: &'a [Choice], row: &adw::ComboRow) -> &'a str {
    list.get(row.selected() as usize).map_or(list[0].id, |c| c.id)
}

pub struct Form {
    pub shortcut: adw::EntryRow,
    pub hold: adw::SwitchRow,
    pub mic: adw::ComboRow,
    mic_ids: RefCell<Vec<String>>,
    pub stt_engine: adw::ComboRow,
    pub whisper_model: adw::ComboRow,
    pub qwen_model: adw::ComboRow,
    pub qwen_device: adw::ComboRow,
    pub qwen_server: adw::EntryRow,
    pub qwen_keep: adw::SpinRow,
    pub models_dir: adw::EntryRow,
    pub lang: adw::ComboRow,
    pub threads: adw::SpinRow,
    pub paste: adw::SwitchRow,
    pub paste_keys: adw::ComboRow,
    pub notify: adw::SwitchRow,
    pub bubble: adw::SwitchRow,
    pub rw_engine: adw::ComboRow,
    pub rw_model: adw::EntryRow,
    pub rw_url: adw::EntryRow,
    pub rw_key_env: adw::EntryRow,
    pub rw_key_file: adw::EntryRow,
    pub rw_extra: adw::EntryRow,
    pub phrases: gtk::TextBuffer,
    /// Set while widgets are filled programmatically, so no half-updated state gets saved
    pub busy: Cell<bool>,
    /// The configuration the widgets were last filled from or saved as; only differences to it are written to the file
    baseline: RefCell<Config>,
}

impl Form {
    pub fn new(cfg: &Config) -> Rc<Self> {
        let whisper_labels: Vec<Choice> = models::MODELS.iter().map(|m| Choice { id: m.id, label: m.label }).collect();
        let qwen_labels: Vec<Choice> = qwen::MODELS.iter().map(|m| Choice { id: m.id, label: m.label }).collect();
        let lang_choices: Vec<Choice> = options::LANGUAGES.iter().map(|l| l.choice).collect();
        let f = Rc::new(Self {
            shortcut: entry("Suggested shortcut, e.g. CTRL+SHIFT+h (system settings decide the active one)"),
            hold: switch("Hold to talk", "Stop dictation when the shortcut keys are released"),
            mic: adw::ComboRow::builder().title("Microphone").build(),
            mic_ids: RefCell::default(),
            stt_engine: combo("Speech engine", options::STT_ENGINES),
            whisper_model: combo("Whisper model", &whisper_labels),
            qwen_model: combo("Qwen3-ASR model", &qwen_labels),
            qwen_device: combo("Run on", options::DEVICES),
            qwen_server: entry("Own llama-server path (empty = managed download)"),
            qwen_keep: adw::SpinRow::with_range(0.0, 3600.0, 30.0),
            models_dir: entry("Models folder (empty = default)"),
            lang: combo("Language", &lang_choices),
            threads: adw::SpinRow::with_range(1.0, 64.0, 1.0),
            paste: switch("Paste automatically", "Needs ydotoold and access to /dev/uinput. Off = copy only, paste yourself"),
            paste_keys: combo("Paste keys", options::PASTE_KEYS),
            notify: switch("Show notification when text is copied", ""),
            bubble: switch(
                "Show floating bubble while dictating",
                "On GNOME Wayland the bubble takes keyboard focus while visible; it is hidden before text is pasted",
            ),
            rw_engine: combo("Improve text with", options::REWRITE_ENGINES),
            rw_model: entry("Model name (empty = default; required for the API)"),
            rw_url: entry("API base URL (LiteLLM, OpenAI), e.g. http://localhost:4000/v1"),
            rw_key_env: entry("API key: environment variable name"),
            rw_key_file: entry("API key: file (used when the variable is empty)"),
            rw_extra: entry("Extra instructions (optional)"),
            phrases: gtk::TextBuffer::new(None),
            busy: Cell::new(false),
            baseline: RefCell::new(cfg.clone()),
        });
        f.threads.set_title("CPU threads");
        f.qwen_keep.set_title("Keep model loaded (seconds, 0 = unload at once)");
        f.fill(cfg);
        f
    }

    /// Shows `cfg` in the widgets.
    pub fn fill(&self, cfg: &Config) {
        *self.baseline.borrow_mut() = cfg.clone();
        self.busy.set(true);
        self.shortcut.set_text(&cfg.shortcut);
        self.hold.set_active(cfg.stop_on_release);
        self.set_mics(&cfg.mic);
        self.stt_engine.set_selected(options::index_of(options::STT_ENGINES, &cfg.stt_engine) as u32);
        self.whisper_model.set_selected(models::MODELS.iter().position(|m| m.id == cfg.model).unwrap_or(0) as u32);
        self.qwen_model.set_selected(qwen::MODELS.iter().position(|m| m.id == cfg.qwen.model).unwrap_or(0) as u32);
        self.qwen_device.set_selected(options::index_of(options::DEVICES, &cfg.qwen.device) as u32);
        self.qwen_server.set_text(&cfg.qwen.server_path);
        self.qwen_keep.set_value(cfg.qwen.keep_alive_secs as f64);
        self.models_dir.set_text(&cfg.models_dir);
        self.lang.set_selected(options::LANGUAGES.iter().position(|l| l.choice.id == cfg.language).unwrap_or(0) as u32);
        self.threads.set_value(cfg.threads as f64);
        self.paste.set_active(cfg.auto_paste);
        self.paste_keys.set_selected(options::index_of(options::PASTE_KEYS, &cfg.paste_keys) as u32);
        self.notify.set_active(cfg.notify);
        self.bubble.set_active(cfg.show_bubble);
        self.rw_engine.set_selected(options::index_of(options::REWRITE_ENGINES, &cfg.rewrite.engine) as u32);
        self.rw_model.set_text(&cfg.rewrite.model);
        self.rw_url.set_text(&cfg.rewrite.base_url);
        self.rw_key_env.set_text(&cfg.rewrite.api_key_env);
        self.rw_key_file.set_text(&cfg.rewrite.api_key_file);
        self.rw_extra.set_text(&cfg.rewrite.extra_instructions);
        self.phrases.set_text(&cfg.phrases.join("\n"));
        self.busy.set(false);
        // The widgets normalise some values (ranges, trimmed or cleaned text). The baseline is what they show, so that only a
        // change the user makes counts as a change and untouched file values are never rewritten.
        let shown = self.snapshot();
        *self.baseline.borrow_mut() = shown;
        self.refresh(&cfg.models_path());
    }

    /// Writes the widget values into `c` (settings without a widget stay as they are).
    pub fn apply(&self, c: &mut Config) {
        let text = |row: &adw::EntryRow| row.text().trim().to_string();
        c.shortcut = text(&self.shortcut);
        c.stop_on_release = self.hold.is_active();
        c.mic = self.mic_ids.borrow().get(self.mic.selected() as usize).cloned().unwrap_or_default();
        c.stt_engine = pick(options::STT_ENGINES, &self.stt_engine).to_string();
        c.model = models::MODELS[self.whisper_model.selected() as usize].id.to_string();
        c.qwen.model = qwen::MODELS[self.qwen_model.selected() as usize].id.to_string();
        c.qwen.device = pick(options::DEVICES, &self.qwen_device).to_string();
        c.qwen.server_path = text(&self.qwen_server);
        c.qwen.keep_alive_secs = self.qwen_keep.value() as u64;
        c.models_dir = text(&self.models_dir);
        c.language = options::LANGUAGES[self.lang.selected() as usize].choice.id.to_string();
        c.threads = self.threads.value() as i32;
        c.auto_paste = self.paste.is_active();
        c.paste_keys = pick(options::PASTE_KEYS, &self.paste_keys).to_string();
        c.notify = self.notify.is_active();
        c.show_bubble = self.bubble.is_active();
        c.rewrite.engine = pick(options::REWRITE_ENGINES, &self.rw_engine).to_string();
        c.rewrite.model = text(&self.rw_model);
        c.rewrite.base_url = text(&self.rw_url);
        c.rewrite.api_key_env = text(&self.rw_key_env);
        c.rewrite.api_key_file = text(&self.rw_key_file);
        c.rewrite.extra_instructions = text(&self.rw_extra);
        let (s, e) = self.phrases.bounds();
        c.phrases = self.phrases.text(&s, &e, false).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
    }

    /// The configuration as the widgets show it now. No file access: it is the baseline with the widget values on top.
    pub fn snapshot(&self) -> Config {
        let mut c = self.baseline.borrow().clone();
        self.apply(&mut c);
        c
    }

    /// `(baseline, widgets)`: what to save is the difference between the two.
    pub fn pending(&self) -> (Config, Config) {
        (self.baseline.borrow().clone(), self.snapshot())
    }

    /// Remembers what was saved, so the next save only writes later changes.
    pub fn mark_saved(&self, saved: Config) {
        *self.baseline.borrow_mut() = saved;
    }

    /// Updates which rows are shown and the size / download hints, for the models folder the widgets name.
    pub fn refresh_view(&self) {
        self.refresh(&self.snapshot().models_path());
    }

    /// Shows only the rows of the selected engines and updates the size / download hints.
    pub fn refresh(&self, models_root: &std::path::Path) {
        let qwen_on = pick(options::STT_ENGINES, &self.stt_engine) == "qwen";
        self.whisper_model.set_visible(!qwen_on);
        for row in [
            self.qwen_model.upcast_ref::<gtk::Widget>(),
            self.qwen_device.upcast_ref(),
            self.qwen_server.upcast_ref(),
            self.qwen_keep.upcast_ref(),
        ] {
            row.set_visible(qwen_on);
        }
        let api = pick(options::REWRITE_ENGINES, &self.rw_engine) == "openai";
        for row in [&self.rw_url, &self.rw_key_env, &self.rw_key_file] {
            row.set_visible(api);
        }
        let state = |ready| if ready { "ready" } else { "not downloaded" };
        let m = &models::MODELS[self.whisper_model.selected() as usize];
        self.whisper_model.set_subtitle(&format!(
            "{} MB, {} - runs locally, no data leaves this computer",
            m.size_mb(),
            state(models::is_downloaded(models_root, m.id))
        ));
        let q = &qwen::MODELS[self.qwen_model.selected() as usize];
        self.qwen_model.set_subtitle(&format!(
            "{} MB, about {} GB RAM, {} - local llama.cpp server",
            q.total_mb(),
            q.ram_gb,
            state(qwen::model_ready(models_root, q.id))
        ));
    }

    /// Fills the microphone list from PipeWire; keeps `current` selectable even when it is not connected.
    pub fn set_mics(&self, current: &str) {
        let mut ids = vec![String::new()];
        let mut labels = vec!["System default".to_string()];
        match audio::list_mics() {
            Ok(list) => {
                for m in list {
                    ids.push(m.node_name);
                    labels.push(m.description);
                }
            }
            Err(e) => eprintln!("dictee: cannot list microphones: {e:#}"),
        }
        let mut selected = ids.iter().position(|n| n == current).unwrap_or(0);
        if !current.is_empty() && selected == 0 {
            ids.push(current.to_string());
            labels.push(format!("{current} (not connected)"));
            selected = ids.len() - 1;
        }
        let was_busy = self.busy.replace(true);
        self.mic.set_model(Some(&gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>())));
        *self.mic_ids.borrow_mut() = ids;
        self.mic.set_selected(selected as u32);
        self.busy.set(was_busy);
    }

    pub fn selected_mic(&self) -> String {
        self.mic_ids.borrow().get(self.mic.selected() as usize).cloned().unwrap_or_default()
    }
}
