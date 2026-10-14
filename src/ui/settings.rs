//! Settings window. Every change is saved immediately to the config file.

use crate::audio::{self, Recorder};
use crate::config::Config;
use crate::controller::Cmd;
use crate::{models, qwen};
use crate::shortcut::{self, Status};
use adw::prelude::*;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

const LANGS: [(&str, &str); 3] = [("auto", "Auto (cs / en)"), ("cs", "Czech"), ("en", "English")];
const ENGINES: [(&str, &str); 5] = [
    ("none", "Off - raw transcript"),
    ("claude", "Claude CLI"),
    ("codex", "Codex CLI (exec)"),
    ("codex-app", "Codex app-server"),
    ("openai", "OpenAI-compatible API"),
];
const STT_ENGINES: [(&str, &str); 2] = [("whisper", "Whisper (whisper.cpp)"), ("qwen", "Qwen3-ASR (llama.cpp)")];
const DEVICES: [(&str, &str); 2] = [("cpu", "CPU"), ("vulkan", "GPU - Vulkan (experimental)")];
const PASTE: [(&str, &str); 2] = [("ctrl+v", "Ctrl+V"), ("ctrl+shift+v", "Ctrl+Shift+V (terminals)")];

struct W {
    mic: adw::ComboRow,
    mics: RefCell<Vec<String>>,
    model: adw::ComboRow,
    stt_engine: adw::ComboRow,
    qwen_model: adw::ComboRow,
    qwen_device: adw::ComboRow,
    qwen_server: adw::EntryRow,
    qwen_keep: adw::SpinRow,
    shortcut: adw::EntryRow,
    rw_url: adw::EntryRow,
    rw_key_env: adw::EntryRow,
    rw_key_file: adw::EntryRow,
    models_dir: adw::EntryRow,
    lang: adw::ComboRow,
    threads: adw::SpinRow,
    hold: adw::SwitchRow,
    paste: adw::SwitchRow,
    paste_keys: adw::ComboRow,
    notify: adw::SwitchRow,
    bubble: adw::SwitchRow,
    engine: adw::ComboRow,
    rw_model: adw::EntryRow,
    rw_extra: adw::EntryRow,
    phrases: gtk::TextBuffer,
    base: RefCell<Config>,
    /// Set while widgets are rebuilt programmatically, so no half-updated state gets saved
    busy: std::cell::Cell<bool>,
}

impl W {
    fn collect(&self) -> Config {
        let mut c = self.base.borrow().clone();
        c.mic = self.mics.borrow().get(self.mic.selected() as usize).cloned().unwrap_or_default();
        c.stt_engine = STT_ENGINES[self.stt_engine.selected() as usize].0.to_string();
        c.qwen.model = qwen::MODELS[self.qwen_model.selected() as usize].id.to_string();
        c.qwen.device = DEVICES[self.qwen_device.selected() as usize].0.to_string();
        c.qwen.server_path = self.qwen_server.text().trim().to_string();
        c.qwen.keep_alive_secs = self.qwen_keep.value() as u64;
        c.shortcut = self.shortcut.text().trim().to_string();
        c.rewrite.base_url = self.rw_url.text().trim().to_string();
        c.rewrite.api_key_env = self.rw_key_env.text().trim().to_string();
        c.rewrite.api_key_file = self.rw_key_file.text().trim().to_string();
        c.models_dir = self.models_dir.text().trim().to_string();
        c.model = models::MODELS[self.model.selected() as usize].id.to_string();
        c.language = LANGS[self.lang.selected() as usize].0.to_string();
        c.threads = self.threads.value() as i32;
        c.stop_on_release = self.hold.is_active();
        c.auto_paste = self.paste.is_active();
        c.paste_keys = PASTE[self.paste_keys.selected() as usize].0.to_string();
        c.notify = self.notify.is_active();
        c.show_bubble = self.bubble.is_active();
        c.rewrite.engine = ENGINES[self.engine.selected() as usize].0.to_string();
        c.rewrite.model = self.rw_model.text().trim().to_string();
        c.rewrite.extra_instructions = self.rw_extra.text().trim().to_string();
        let (s, e) = self.phrases.bounds();
        c.phrases = self.phrases.text(&s, &e, false).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
        c
    }
}

fn strings(items: &[(&str, &str)]) -> gtk::StringList {
    gtk::StringList::new(&items.iter().map(|i| i.1).collect::<Vec<_>>())
}

fn index_of(items: &[(&str, &str)], key: &str) -> u32 {
    items.iter().position(|i| i.0 == key).unwrap_or(0) as u32
}

fn model_labels() -> Vec<String> {
    models::MODELS.iter().map(|m| m.label.to_string()).collect()
}

fn qwen_labels() -> Vec<String> {
    qwen::MODELS.iter().map(|m| m.label.to_string()).collect()
}

/// Subtitles with size and download state of the selected models.
fn describe(w: &W) {
    let m = &models::MODELS[w.model.selected() as usize];
    let state = |ready| if ready { "ready" } else { "not downloaded" };
    w.model.set_subtitle(&format!("{} MB, {} - runs locally, no data leaves this computer", m.size_mb, state(models::is_downloaded(m.id))));
    let q = &qwen::MODELS[w.qwen_model.selected() as usize];
    w.qwen_model.set_subtitle(&format!("{} MB, about {} GB RAM, {} - local llama.cpp server", q.total_mb(), q.ram_gb, state(qwen::model_ready(q.id))));
}

fn list(items: &[String]) -> gtk::StringList {
    gtk::StringList::new(&items.iter().map(String::as_str).collect::<Vec<_>>())
}

fn apply_visibility(w: &W) {
    let qwen_on = w.stt_engine.selected() == 1;
    w.model.set_visible(!qwen_on);
    for row in [w.qwen_model.upcast_ref::<gtk::Widget>(), w.qwen_device.upcast_ref(), w.qwen_server.upcast_ref(), w.qwen_keep.upcast_ref()] {
        row.set_visible(qwen_on);
    }
    let api = ENGINES[w.engine.selected() as usize].0 == "openai";
    for row in [&w.rw_url, &w.rw_key_env, &w.rw_key_file] {
        row.set_visible(api);
    }
}

/// Rebuilds the model list (labels show the downloaded state) and keeps the selection.
fn reload_models(w: &W) {
    w.busy.set(true);
    let sel = w.model.selected();
    w.model.set_model(Some(&gtk::StringList::new(&model_labels().iter().map(String::as_str).collect::<Vec<_>>())));
    w.model.set_selected(sel);
    let qsel = w.qwen_model.selected();
    w.qwen_model.set_model(Some(&list(&qwen_labels())));
    w.qwen_model.set_selected(qsel);
    w.busy.set(false);
}

fn set_mics(w: &W, current: &str) {
    let mut names = vec![String::new()];
    let mut labels = vec!["System default".to_string()];
    match audio::list_mics() {
        Ok(list) => {
            for m in list {
                names.push(m.node_name);
                labels.push(m.description);
            }
        }
        Err(e) => eprintln!("dictee: cannot list microphones: {e:#}"),
    }
    let mut sel = names.iter().position(|n| n == current).unwrap_or(0);
    if !current.is_empty() && sel == 0 {
        names.push(current.to_string());
        labels.push(format!("{current} (not connected)"));
        sel = names.len() - 1;
    }
    w.busy.set(true);
    w.mic.set_model(Some(&gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>())));
    w.mic.set_selected(sel as u32);
    *w.mics.borrow_mut() = names;
    w.busy.set(false);
}

pub fn build(app: &adw::Application, status: Status, _tx: Sender<Cmd>) -> adw::ApplicationWindow {
    let cfg = Config::load().unwrap_or_default();
    let toasts = adw::ToastOverlay::new();

    let w = Rc::new(W {
        mic: adw::ComboRow::builder().title("Microphone").build(),
        mics: RefCell::default(),
        model: adw::ComboRow::builder().title("Whisper model").build(),
        stt_engine: adw::ComboRow::builder().title("Speech engine").model(&strings(&STT_ENGINES)).build(),
        qwen_model: adw::ComboRow::builder().title("Qwen3-ASR model").build(),
        qwen_device: adw::ComboRow::builder().title("Run on").model(&strings(&DEVICES)).build(),
        qwen_server: adw::EntryRow::builder().title("Own llama-server path (empty = managed download)").build(),
        qwen_keep: adw::SpinRow::with_range(0.0, 3600.0, 30.0),
        shortcut: adw::EntryRow::builder().title("Shortcut, e.g. CTRL+SHIFT+h").build(),
        rw_url: adw::EntryRow::builder().title("API base URL (LiteLLM, OpenAI), e.g. http://localhost:4000/v1").build(),
        rw_key_env: adw::EntryRow::builder().title("API key: environment variable name").build(),
        rw_key_file: adw::EntryRow::builder().title("API key: file (used when the variable is empty)").build(),
        models_dir: adw::EntryRow::builder().title("Models folder (empty = default)").build(),
        lang: adw::ComboRow::builder().title("Language").model(&strings(&LANGS)).build(),
        threads: adw::SpinRow::with_range(1.0, 32.0, 1.0),
        hold: adw::SwitchRow::builder().title("Hold to talk").subtitle("Stop dictation when the shortcut keys are released").build(),
        paste: adw::SwitchRow::builder()
            .title("Paste automatically")
            .subtitle("Needs ydotoold and access to /dev/uinput. Off = copy only, paste yourself")
            .build(),
        paste_keys: adw::ComboRow::builder().title("Paste keys").model(&strings(&PASTE)).build(),
        notify: adw::SwitchRow::builder().title("Show notification when text is copied").build(),
        bubble: adw::SwitchRow::builder()
            .title("Show floating bubble while dictating")
            .subtitle("On GNOME Wayland the bubble takes keyboard focus while visible; it is hidden before text is pasted")
            .build(),
        engine: adw::ComboRow::builder().title("Improve text with").model(&strings(&ENGINES)).build(),
        rw_model: adw::EntryRow::builder().title("Model name (empty = default; required for the API)").build(),
        rw_extra: adw::EntryRow::builder().title("Extra instructions (optional)").build(),
        phrases: gtk::TextBuffer::new(None),
        base: RefCell::new(cfg.clone()),
        busy: std::cell::Cell::new(false),
    });
    w.models_dir.set_text(&cfg.models_dir);
    w.qwen_keep.set_title("Keep model loaded (seconds, 0 = unload at once)");
    w.qwen_model.set_model(Some(&list(&qwen_labels())));
    w.qwen_model.set_selected(qwen::MODELS.iter().position(|m| m.id == cfg.qwen.model).unwrap_or(0) as u32);
    w.stt_engine.set_selected(index_of(&STT_ENGINES, &cfg.stt_engine));
    w.qwen_device.set_selected(index_of(&DEVICES, &cfg.qwen.device));
    w.qwen_server.set_text(&cfg.qwen.server_path);
    w.qwen_keep.set_value(cfg.qwen.keep_alive_secs as f64);
    w.shortcut.set_text(&cfg.shortcut);
    w.rw_url.set_text(&cfg.rewrite.base_url);
    w.rw_key_env.set_text(&cfg.rewrite.api_key_env);
    w.rw_key_file.set_text(&cfg.rewrite.api_key_file);
    w.threads.set_title("CPU threads");
    w.model.set_model(Some(&gtk::StringList::new(&model_labels().iter().map(String::as_str).collect::<Vec<_>>())));
    w.model.set_selected(models::MODELS.iter().position(|m| m.id == cfg.model).unwrap_or(0) as u32);
    w.lang.set_selected(index_of(&LANGS, &cfg.language));
    w.threads.set_value(cfg.threads as f64);
    w.hold.set_active(cfg.stop_on_release);
    w.paste.set_active(cfg.auto_paste);
    w.paste_keys.set_selected(index_of(&PASTE, &cfg.paste_keys));
    w.notify.set_active(cfg.notify);
    w.bubble.set_active(cfg.show_bubble);
    w.engine.set_selected(index_of(&ENGINES, &cfg.rewrite.engine));
    w.rw_model.set_text(&cfg.rewrite.model);
    w.rw_extra.set_text(&cfg.rewrite.extra_instructions);
    w.phrases.set_text(&cfg.phrases.join("\n"));
    set_mics(&w, &cfg.mic);
    apply_visibility(&w);

    // --- Dictation group
    let shortcut_row = adw::ActionRow::builder().title("Global shortcut").subtitle(status.lock().unwrap().as_str()).build();
    let sys_btn = gtk::Button::builder().label("System settings").valign(gtk::Align::Center).build();
    sys_btn.connect_clicked(|_| {
        let _ = std::process::Command::new("gnome-control-center").args(["keyboard"]).spawn();
    });
    shortcut_row.add_suffix(&sys_btn);
    {
        let row = shortcut_row.clone();
        gtk::glib::timeout_add_local(Duration::from_secs(1), move || {
            if row.root().is_none() {
                return gtk::glib::ControlFlow::Break;
            }
            row.set_subtitle(status.lock().unwrap().as_str());
            gtk::glib::ControlFlow::Continue
        });
    }
    let desktop_name = match shortcut::desktop() {
        shortcut::Desktop::Gnome => "GNOME",
        shortcut::Desktop::Kde => "KDE",
        shortcut::Desktop::Other => "this desktop",
    };
    let fallback_row = adw::ActionRow::builder().build();
    let refresh_fallback = {
        let (row, w) = (fallback_row.clone(), w.clone());
        Rc::new(move || {
            let combo = shortcut::Combo::parse(&w.shortcut.text()).map(|c| c.display()).unwrap_or_else(|_| "invalid shortcut".into());
            row.set_title(&format!("Fallback: {desktop_name} keybinding {combo}"));
            row.set_subtitle(if shortcut::desktop() == shortcut::Desktop::Other {
                "Not automatic here: add a custom shortcut that runs `dictee toggle`"
            } else {
                "Use when the portal shortcut does not work. Toggle only (no hold to talk)"
            });
        })
    };
    refresh_fallback();
    let fb_btn = gtk::Button::builder().label("Install").valign(gtk::Align::Center).sensitive(shortcut::desktop() != shortcut::Desktop::Other).build();
    {
        let (toasts, w) = (toasts.clone(), w.clone());
        fb_btn.connect_clicked(move |_| {
            let msg = match shortcut::install_keybinding(&w.shortcut.text()) {
                Ok(m) => m,
                Err(e) => format!("Failed: {e:#}"),
            };
            toasts.add_toast(adw::Toast::new(&msg));
        });
    }
    fallback_row.add_suffix(&fb_btn);
    let g_dict = adw::PreferencesGroup::builder().title("Dictation").build();
    let lower_only = std::env::var("DICTEE_SNAPSHOT_PART").as_deref() == Ok("lower");
    g_dict.add(&shortcut_row);
    g_dict.add(&w.shortcut);
    g_dict.add(&w.hold);
    g_dict.add(&fallback_row);

    // --- Microphone group
    let refresh = gtk::Button::builder().icon_name("view-refresh-symbolic").valign(gtk::Align::Center).tooltip_text("Refresh list").build();
    {
        let w = w.clone();
        refresh.connect_clicked(move |_| {
            let cur = w.mics.borrow().get(w.mic.selected() as usize).cloned().unwrap_or_default();
            set_mics(&w, &cur);
        });
    }
    w.mic.add_suffix(&refresh);
    let level = gtk::LevelBar::builder().min_value(0.0).max_value(1.0).width_request(140).valign(gtk::Align::Center).build();
    let test_btn = gtk::Button::builder().label("Test 3 s").valign(gtk::Align::Center).build();
    let test_row = adw::ActionRow::builder().title("Test microphone").subtitle("Speak while the bar moves").build();
    test_row.add_suffix(&level);
    test_row.add_suffix(&test_btn);
    {
        let w = w.clone();
        let (level, toasts) = (level.clone(), toasts.clone());
        test_btn.connect_clicked(move |btn| {
            let mic = w.collect().mic;
            match Recorder::start(&mic) {
                Ok(rec) => {
                    btn.set_sensitive(false);
                    let lvl = rec.level();
                    let rec = Rc::new(RefCell::new(Some(rec)));
                    let (btn, level) = (btn.clone(), level.clone());
                    let mut ticks = 0;
                    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
                        ticks += 1;
                        level.set_value((lvl.load(Ordering::Relaxed) as f64 / 6000.0).min(1.0));
                        if ticks >= 60 {
                            if let Some(r) = rec.borrow_mut().take() {
                                r.cancel();
                            }
                            level.set_value(0.0);
                            btn.set_sensitive(true);
                            return gtk::glib::ControlFlow::Break;
                        }
                        gtk::glib::ControlFlow::Continue
                    });
                }
                Err(e) => toasts.add_toast(adw::Toast::new(&format!("{e:#}"))),
            }
        });
    }
    let g_mic = adw::PreferencesGroup::builder().title("Microphone").build();
    g_mic.add(&w.mic);
    g_mic.add(&test_row);

    // --- Model group
    let dl_btn = gtk::Button::builder().label("Download").valign(gtk::Align::Center).build();
    let progress = gtk::ProgressBar::builder().valign(gtk::Align::Center).width_request(140).visible(false).build();
    let dl_row = adw::ActionRow::builder().title("Selected model").subtitle("Download is needed once").build();
    dl_row.add_suffix(&progress);
    dl_row.add_suffix(&dl_btn);
    let update_dl = {
        let (w, dl_btn, dl_row) = (w.clone(), dl_btn.clone(), dl_row.clone());
        Rc::new(move || {
            let c = w.collect();
            let (have, what) = if c.stt_engine == "qwen" {
                (qwen::ready(&c.qwen), "Qwen runtime and model")
            } else {
                (models::is_downloaded(&c.model), "Selected model")
            };
            describe(&w);
            dl_btn.set_sensitive(!have);
            dl_row.set_title(what);
            dl_row.set_subtitle(if have { "Ready" } else { "Not downloaded yet" });
        })
    };
    update_dl();
    {
        let (w, progress, update_dl, toasts, dl_btn2) = (w.clone(), progress.clone(), update_dl.clone(), toasts.clone(), dl_btn.clone());
        dl_btn.connect_clicked(move |btn| {
            let c = w.collect();
            btn.set_sensitive(false);
            progress.set_visible(true);
            progress.set_fraction(0.0);
            let (tx, rx) = mpsc::channel::<Result<(u64, u64), String>>();
            std::thread::spawn(move || {
                let report = |d, t| {
                    let _ = tx.send(Ok((d, t)));
                };
                let r = if c.stt_engine == "qwen" { qwen::download_all(&c.qwen, report) } else { models::download(&c.model, report).map(|_| ()) };
                let _ = tx.send(Err(r.err().map(|e| format!("{e:#}")).unwrap_or_default()));
            });
            let (progress, update_dl, toasts, w, btn) = (progress.clone(), update_dl.clone(), toasts.clone(), w.clone(), dl_btn2.clone());
            gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
                let mut finished = None;
                while let Ok(m) = rx.try_recv() {
                    match m {
                        Ok((d, t)) if t > 0 => progress.set_fraction(d as f64 / t as f64),
                        Ok(_) => progress.pulse(),
                        Err(e) => finished = Some(e),
                    }
                }
                match finished {
                    None => gtk::glib::ControlFlow::Continue,
                    Some(err) => {
                        progress.set_visible(false);
                        reload_models(&w);
                        btn.set_sensitive(true);
                        update_dl();
                        toasts.add_toast(adw::Toast::new(&if err.is_empty() { "Download finished".to_string() } else { format!("Download failed: {err}") }));
                        gtk::glib::ControlFlow::Break
                    }
                }
            });
        });
    }
    {
        let update_dl = update_dl.clone();
        w.model.connect_selected_notify(move |_| update_dl());
    }
    let g_model = adw::PreferencesGroup::builder().title("Speech recognition").build();
    g_model.add(&w.stt_engine);
    g_model.add(&w.model);
    g_model.add(&w.qwen_model);
    g_model.add(&w.qwen_device);
    g_model.add(&w.qwen_server);
    g_model.add(&w.qwen_keep);
    g_model.add(&w.models_dir);
    g_model.add(&dl_row);
    g_model.add(&w.lang);
    g_model.add(&w.threads);

    // --- Output group
    let g_out = adw::PreferencesGroup::builder().title("Output").description("The text is always copied to the clipboard").build();
    g_out.add(&w.paste);
    g_out.add(&w.paste_keys);
    g_out.add(&w.notify);
    g_out.add(&w.bubble);

    // --- Rewrite group
    let g_rw = adw::PreferencesGroup::builder()
        .title("Text improvement")
        .description("Optional. Sends the transcript and your phrases to the chosen CLI or API (CLIs use your own login)")
        .build();
    g_rw.add(&w.engine);
    g_rw.add(&w.rw_model);
    g_rw.add(&w.rw_url);
    g_rw.add(&w.rw_key_env);
    g_rw.add(&w.rw_key_file);
    g_rw.add(&w.rw_extra);

    // --- Phrases group
    let view = gtk::TextView::builder()
        .buffer(&w.phrases)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(10)
        .right_margin(10)
        .build();
    let scroll = gtk::ScrolledWindow::builder().child(&view).min_content_height(170).css_classes(["card"]).build();
    let g_phr = adw::PreferencesGroup::builder()
        .title("Known phrases")
        .description("One per line: project names, repositories, programs, people. Used to correct recognition errors")
        .build();
    g_phr.add(&scroll);

    // --- Autosave
    let save: Rc<dyn Fn()> = {
        let (w, toasts) = (w.clone(), toasts.clone());
        Rc::new(move || {
            if w.busy.get() {
                return;
            }
            let c = w.collect();
            if let Err(e) = c.save() {
                toasts.add_toast(adw::Toast::new(&format!("Cannot save settings: {e:#}")));
            }
            *w.base.borrow_mut() = c;
        })
    };
    for row in [&w.mic, &w.model, &w.lang, &w.paste_keys] {
        let s = save.clone();
        row.connect_selected_notify(move |_| s());
    }
    for row in [&w.stt_engine, &w.qwen_model, &w.qwen_device, &w.engine] {
        let (s, w2, update_dl) = (save.clone(), w.clone(), update_dl.clone());
        row.connect_selected_notify(move |_| {
            s();
            apply_visibility(&w2);
            update_dl();
        });
    }
    {
        let (s, update_dl) = (save.clone(), update_dl.clone());
        w.qwen_server.connect_changed(move |_| {
            s();
            update_dl();
        });
        let (s, refresh) = (save.clone(), refresh_fallback.clone());
        w.shortcut.connect_changed(move |_| {
            s();
            refresh();
        });
        let s = save.clone();
        w.qwen_keep.connect_value_notify(move |_| s());
    }
    for row in [&w.hold, &w.paste, &w.notify, &w.bubble] {
        let s = save.clone();
        row.connect_active_notify(move |_| s());
    }
    {
        // Save first, then re-read the folder: `models::dir()` reads the saved config.
        let (s, w2, update_dl) = (save.clone(), w.clone(), update_dl.clone());
        w.models_dir.connect_changed(move |_| {
            s();
            reload_models(&w2);
            update_dl();
        });
    }
    for row in [&w.rw_model, &w.rw_extra, &w.rw_url, &w.rw_key_env, &w.rw_key_file] {
        let s = save.clone();
        row.connect_changed(move |_| s());
    }
    {
        let s = save.clone();
        w.threads.connect_value_notify(move |_| s());
        let s = save.clone();
        w.phrases.connect_changed(move |_| s());
    }

    let page = adw::PreferencesPage::new();
    for g in [&g_dict, &g_mic, &g_model, &g_out, &g_rw, &g_phr] {
        page.add(g);
    }
    if lower_only {
        for g in [&g_dict, &g_mic, &g_model] {
            g.set_visible(false);
        }
    }
    toasts.set_child(Some(&page));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&toasts));
    let win = adw::ApplicationWindow::builder().application(app).title("Dictee settings").default_width(660).default_height(if std::env::var_os("DICTEE_SNAPSHOT_SETTINGS").is_some() { 1900 } else { 820 }).content(&toolbar).build();
    win
}
