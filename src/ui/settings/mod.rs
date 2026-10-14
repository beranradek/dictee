//! Settings window. Every change is saved (debounced) to the config file.

mod downloads;
mod form;
mod saver;

use crate::audio::Recorder;
use crate::config::Config;
use crate::shortcut::{self, Status};
use adw::prelude::*;
use downloads::Downloads;
use form::Form;
use libadwaita as adw;
use saver::Saver;
use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::time::Duration;

const MIC_TEST_TICKS: u32 = 60; // 3 s at 50 ms

pub(super) fn toast(toasts: &adw::ToastOverlay, text: &str) {
    toasts.add_toast(adw::Toast::new(text));
}

pub(super) fn button(label: &str) -> gtk::Button {
    gtk::Button::builder().label(label).valign(gtk::Align::Center).build()
}

fn dictation_group(form: &Rc<Form>, status: Status, toasts: &adw::ToastOverlay) -> (adw::PreferencesGroup, Rc<dyn Fn()>) {
    let portal_row = adw::ActionRow::builder().title("Global shortcut").subtitle(status.lock().unwrap().as_str()).build();
    let desktop = shortcut::desktop();
    let system_btn = button("System settings");
    let toasts_for_btn = toasts.clone();
    system_btn.connect_clicked(move |_| {
        let (program, args): (&str, &[&str]) = match shortcut::desktop() {
            shortcut::Desktop::Gnome => ("gnome-control-center", &["keyboard"]),
            shortcut::Desktop::Kde => ("kcmshell6", &["kcm_keys"]),
            shortcut::Desktop::Other => return,
        };
        match std::process::Command::new(program).args(args).spawn() {
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => toast(&toasts_for_btn, &format!("Cannot start {program}: {e}")),
        }
    });
    system_btn.set_sensitive(desktop != shortcut::Desktop::Other);
    portal_row.add_suffix(&system_btn);
    {
        let row = portal_row.clone();
        gtk::glib::timeout_add_local(Duration::from_secs(1), move || {
            if row.root().is_none() {
                return gtk::glib::ControlFlow::Break;
            }
            row.set_subtitle(status.lock().unwrap().as_str());
            gtk::glib::ControlFlow::Continue
        });
    }

    let desktop_name = match desktop {
        shortcut::Desktop::Gnome => "GNOME",
        shortcut::Desktop::Kde => "KDE",
        shortcut::Desktop::Other => "this desktop",
    };
    let fallback_row = adw::ActionRow::builder().build();
    let refresh_fallback: Rc<dyn Fn()> = {
        let (row, form) = (fallback_row.clone(), form.clone());
        Rc::new(move || {
            let combo = shortcut::Combo::parse(&form.shortcut.text()).map(|c| c.display()).unwrap_or_else(|_| "invalid shortcut".into());
            row.set_title(&format!("Fallback: {desktop_name} keybinding {combo}"));
            row.set_subtitle(if desktop == shortcut::Desktop::Other {
                "Not automatic here: add a custom shortcut that runs `dictee toggle`"
            } else {
                "Use when the portal shortcut does not work. Toggle only (no hold to talk)"
            });
        })
    };
    refresh_fallback();
    let install_btn = button("Install");
    install_btn.set_sensitive(desktop != shortcut::Desktop::Other);
    {
        let (form, toasts) = (form.clone(), toasts.clone());
        install_btn.connect_clicked(move |_| match shortcut::install_keybinding(&form.shortcut.text()) {
            Ok(msg) => toast(&toasts, &msg),
            Err(e) => toast(&toasts, &format!("Failed: {e:#}")),
        });
    }
    fallback_row.add_suffix(&install_btn);

    let group = adw::PreferencesGroup::builder().title("Dictation").build();
    group.add(&portal_row);
    group.add(&form.shortcut);
    group.add(&form.hold);
    group.add(&fallback_row);
    (group, refresh_fallback)
}

fn microphone_group(form: &Rc<Form>, toasts: &adw::ToastOverlay) -> adw::PreferencesGroup {
    let refresh = gtk::Button::builder().icon_name("view-refresh-symbolic").valign(gtk::Align::Center).tooltip_text("Refresh list").build();
    {
        let form = form.clone();
        refresh.connect_clicked(move |_| form.set_mics(&form.selected_mic()));
    }
    form.mic.add_suffix(&refresh);

    let level = gtk::LevelBar::builder().min_value(0.0).max_value(1.0).width_request(140).valign(gtk::Align::Center).build();
    let test_btn = button("Test 3 s");
    let test_row = adw::ActionRow::builder().title("Test microphone").subtitle("Speak while the bar moves").build();
    test_row.add_suffix(&level);
    test_row.add_suffix(&test_btn);
    {
        let (form, toasts, level) = (form.clone(), toasts.clone(), level.clone());
        test_btn.connect_clicked(move |btn| match Recorder::start(&form.selected_mic(), |_| {}) {
            Ok(recorder) => run_mic_test(recorder, btn, &level),
            Err(e) => toast(&toasts, &format!("{e:#}")),
        });
    }
    let group = adw::PreferencesGroup::builder().title("Microphone").build();
    group.add(&form.mic);
    group.add(&test_row);
    group
}

/// Shows the input level for three seconds, then stops the test recording.
fn run_mic_test(recorder: Recorder, btn: &gtk::Button, level: &gtk::LevelBar) {
    btn.set_sensitive(false);
    let source = recorder.level();
    let mut recorder = Some(recorder);
    let (btn, level) = (btn.clone(), level.clone());
    let mut ticks = 0;
    gtk::glib::timeout_add_local(Duration::from_millis(50), move || {
        ticks += 1;
        level.set_value((source.load(Ordering::Relaxed) as f64 / 6000.0).min(1.0));
        if ticks < MIC_TEST_TICKS {
            return gtk::glib::ControlFlow::Continue;
        }
        if let Some(r) = recorder.take() {
            r.cancel();
        }
        level.set_value(0.0);
        btn.set_sensitive(true);
        gtk::glib::ControlFlow::Break
    });
}

fn speech_group(form: &Form, downloads: &Downloads) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Speech recognition").build();
    group.add(&form.stt_engine);
    group.add(&form.whisper_model);
    group.add(&form.qwen_model);
    group.add(&form.qwen_device);
    group.add(&form.qwen_server);
    group.add(&form.qwen_keep);
    group.add(&form.models_dir);
    group.add(&downloads.row);
    group.add(&form.lang);
    group.add(&form.threads);
    group
}

fn output_group(form: &Form) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Output").description("The text is always copied to the clipboard").build();
    group.add(&form.paste);
    group.add(&form.paste_keys);
    group.add(&form.notify);
    group.add(&form.bubble);
    group
}

fn rewrite_group(form: &Form) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Text improvement")
        .description("Optional. Sends the transcript and your phrases to the chosen CLI or API (CLIs use your own login)")
        .build();
    group.add(&form.rw_engine);
    for row in [&form.rw_model, &form.rw_url, &form.rw_key_env, &form.rw_key_file, &form.rw_extra] {
        group.add(row);
    }
    group
}

fn phrases_group(form: &Form) -> adw::PreferencesGroup {
    let view = gtk::TextView::builder()
        .buffer(&form.phrases)
        .wrap_mode(gtk::WrapMode::WordChar)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(10)
        .right_margin(10)
        .build();
    let scroll = gtk::ScrolledWindow::builder().child(&view).min_content_height(170).css_classes(["card"]).build();
    let group = adw::PreferencesGroup::builder()
        .title("Known phrases")
        .description("One per line: project names, repositories, programs, people. Used to correct recognition errors")
        .build();
    group.add(&scroll);
    group
}

pub fn build(app: &adw::Application, status: Status) -> adw::ApplicationWindow {
    let toasts = adw::ToastOverlay::new();
    let cfg = Config::load().unwrap_or_else(|e| {
        let message = format!("config.toml is invalid, showing defaults and not saving: {e:#}");
        eprintln!("dictee: {message}");
        toast(&toasts, &message);
        Config::default()
    });
    let form = Form::new(&cfg);
    let saver = Saver::new(&form, &toasts);
    let downloads = Downloads::new(&form, &toasts);

    let (dictation, refresh_fallback) = dictation_group(&form, status, &toasts);
    let groups = [
        dictation,
        microphone_group(&form, &toasts),
        speech_group(&form, &downloads),
        output_group(&form),
        rewrite_group(&form),
        phrases_group(&form),
    ];
    wire_autosave(&form, &saver, &downloads, refresh_fallback);

    let page = adw::PreferencesPage::new();
    for g in &groups {
        page.add(g);
    }
    if std::env::var("DICTEE_SNAPSHOT_PART").as_deref() == Ok("lower") {
        groups[..3].iter().for_each(|g| g.set_visible(false)); // dev helper for window snapshots
    }
    toasts.set_child(Some(&page));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&toasts));
    let height = if std::env::var_os("DICTEE_SNAPSHOT_SETTINGS").is_some() { 1900 } else { 820 };
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Dictee settings")
        .default_width(660)
        .default_height(height)
        .content(&toolbar)
        .build();
    window.connect_close_request(move |_| {
        saver.flush(); // do not lose a change made less than SAVE_DELAY ago
        gtk::glib::Propagation::Proceed
    });
    window
}

/// Every change schedules a save; rows that change what other rows show or need also refresh them.
fn wire_autosave(form: &Rc<Form>, saver: &Rc<Saver>, downloads: &Rc<Downloads>, refresh_fallback: Rc<dyn Fn()>) {
    let changed = |extra: Rc<dyn Fn()>| -> Rc<dyn Fn()> {
        let saver = saver.clone();
        Rc::new(move || {
            extra();
            saver.schedule();
        })
    };
    let plain = changed(Rc::new(|| {}));
    let refresh_downloads = {
        let (downloads, form) = (downloads.clone(), form.clone());
        changed(Rc::new(move || {
            if !form.busy.get() {
                downloads.update();
            }
        }))
    };
    let refresh_view = {
        let form = form.clone();
        changed(Rc::new(move || {
            if !form.busy.get() {
                form.refresh_view();
            }
        }))
    };
    let on_shortcut = changed(Rc::new(move || refresh_fallback()));

    for row in [&form.mic, &form.lang, &form.paste_keys] {
        let f = plain.clone();
        row.connect_selected_notify(move |_| f());
    }
    for row in [&form.stt_engine, &form.whisper_model, &form.qwen_model, &form.qwen_device] {
        let f = refresh_downloads.clone();
        row.connect_selected_notify(move |_| f());
    }
    {
        // The rewrite engine only changes which rows are shown.
        let f = refresh_view.clone();
        form.rw_engine.connect_selected_notify(move |_| f());
    }
    for row in [&form.hold, &form.paste, &form.notify, &form.bubble] {
        let f = plain.clone();
        row.connect_active_notify(move |_| f());
    }
    for row in [&form.qwen_server, &form.models_dir] {
        let f = refresh_downloads.clone();
        row.connect_changed(move |_| f());
    }
    for row in [&form.rw_model, &form.rw_extra, &form.rw_url, &form.rw_key_env, &form.rw_key_file] {
        let f = plain.clone();
        row.connect_changed(move |_| f());
    }
    {
        let f = on_shortcut;
        form.shortcut.connect_changed(move |_| f());
        for row in [&form.threads, &form.qwen_keep] {
            let f = plain.clone();
            row.connect_value_notify(move |_| f());
        }
        let f = plain.clone();
        form.phrases.connect_changed(move |_| f());
    }
}
