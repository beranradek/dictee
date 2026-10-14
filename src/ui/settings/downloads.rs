//! The "download model" row of the settings window.

use super::form::Form;
use super::{button, toast};
use crate::{models, qwen};
use adw::prelude::*;
use libadwaita as adw;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::mpsc::{self, TryRecvError};
use std::time::Duration;

enum Msg {
    Progress(u64, u64),
    Done(Result<(), String>),
}

/// Download row for the model files of the selected speech engine.
pub struct Downloads {
    form: Rc<Form>,
    toasts: adw::ToastOverlay,
    pub row: adw::ActionRow,
    button: gtk::Button,
    progress: gtk::ProgressBar,
    active: Cell<bool>,
}

impl Downloads {
    pub fn new(form: &Rc<Form>, toasts: &adw::ToastOverlay) -> Rc<Self> {
        let d = Rc::new(Self {
            form: form.clone(),
            toasts: toasts.clone(),
            row: adw::ActionRow::builder().build(),
            button: button("Download"),
            progress: gtk::ProgressBar::builder().valign(gtk::Align::Center).width_request(140).visible(false).build(),
            active: Cell::new(false),
        });
        d.row.add_suffix(&d.progress);
        d.row.add_suffix(&d.button);
        let me = d.clone();
        d.button.connect_clicked(move |_| me.start());
        d.update();
        d
    }

    /// Shows whether the selected engine's files are there; also refreshes the size hints of the model rows.
    pub fn update(&self) {
        let cfg = self.form.snapshot();
        let root = cfg.models_path();
        let own_server = cfg.stt_engine == "qwen" && !cfg.qwen.server_path.trim().is_empty();
        let missing_server = own_server && !std::path::Path::new(cfg.qwen.server_path.trim()).is_file();
        let (ready, what) = if cfg.stt_engine == "qwen" {
            (qwen::ready(&root, &cfg.qwen), "Qwen runtime and model")
        } else {
            (models::is_downloaded(&root, &cfg.model), "Selected model")
        };
        self.form.refresh(&root);
        // A wrong own server path cannot be fixed by downloading: say so instead of offering a download that does nothing.
        self.button.set_sensitive(!ready && !missing_server && !self.active.get());
        self.row.set_title(what);
        self.row.set_subtitle(&if missing_server {
            format!("llama-server not found at {} - fix the path above", cfg.qwen.server_path.trim())
        } else if ready {
            "Ready".to_string()
        } else {
            "Not downloaded yet".to_string()
        });
    }

    fn start(self: &Rc<Self>) {
        let cfg = self.form.snapshot();
        self.active.set(true);
        self.progress.set_visible(true);
        self.progress.set_fraction(0.0);
        self.update();
        let (tx, rx) = mpsc::channel::<Msg>();
        std::thread::spawn(move || {
            let progress_tx = tx.clone();
            let report = move |done, total| {
                let _ = progress_tx.send(Msg::Progress(done, total));
            };
            let root = cfg.models_path();
            let result = if cfg.stt_engine == "qwen" {
                qwen::download_all(&root, &cfg.qwen, report)
            } else {
                models::download(&root, &cfg.model, report).map(|_| ())
            };
            let _ = tx.send(Msg::Done(result.map_err(|e| format!("{e:#}"))));
        });
        let me = self.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
            let mut finished = None;
            loop {
                match rx.try_recv() {
                    Ok(Msg::Progress(done, total)) if total > 0 => me.progress.set_fraction(done as f64 / total as f64),
                    Ok(Msg::Progress(..)) => me.progress.pulse(),
                    Ok(Msg::Done(result)) => finished = Some(result),
                    Err(TryRecvError::Empty) => break,
                    // The worker is gone without a result (it panicked): never leave the button disabled.
                    Err(TryRecvError::Disconnected) => {
                        finished.get_or_insert(Err("the download thread stopped unexpectedly".to_string()));
                        break;
                    }
                }
            }
            let Some(result) = finished else { return gtk::glib::ControlFlow::Continue };
            me.active.set(false);
            me.progress.set_visible(false);
            me.update();
            toast(
                &me.toasts,
                &match result {
                    Ok(()) => "Download finished".to_string(),
                    Err(e) => format!("Download failed: {e}"),
                },
            );
            gtk::glib::ControlFlow::Break
        });
    }
}
