//! Debounced autosave of the settings form.

use super::form::Form;
use super::toast;
use crate::config::Config;
use crate::shortcut::Combo;
use libadwaita as adw;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

const SAVE_DELAY: Duration = Duration::from_millis(300);

/// Saves the form shortly after the last change. Only what the user changed is written (see `Config::merge_changes`),
/// so edits made to the file by hand meanwhile are kept, also for settings that have a widget.
pub struct Saver {
    form: Rc<Form>,
    toasts: adw::ToastOverlay,
    generation: Cell<u32>,
    dirty: Cell<bool>,
}

impl Saver {
    pub fn new(form: &Rc<Form>, toasts: &adw::ToastOverlay) -> Rc<Self> {
        Rc::new(Self { form: form.clone(), toasts: toasts.clone(), generation: Cell::new(0), dirty: Cell::new(false) })
    }

    pub fn schedule(self: &Rc<Self>) {
        if self.form.busy.get() {
            return;
        }
        self.dirty.set(true);
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        let me = self.clone();
        gtk::glib::timeout_add_local_once(SAVE_DELAY, move || {
            if me.generation.get() == generation {
                me.flush();
            }
        });
    }

    /// Writes pending changes now (also called when the window closes).
    pub fn flush(&self) {
        if !self.dirty.replace(false) {
            return;
        }
        let (before, mut after) = self.form.pending();
        if Combo::parse(&after.shortcut).is_err() {
            after.shortcut = before.shortcut.clone(); // a half-typed shortcut is not saved
        }
        let saved = Config::load().and_then(|file| Config::merge_changes(&before, &after, &file)).and_then(|merged| {
            merged.validate()?;
            merged.save()
        });
        match saved {
            Ok(()) => self.form.mark_saved(after),
            Err(e) => toast(&self.toasts, &format!("Settings not saved: {e:#}")),
        }
    }
}
