//! GTK front end: floating bubble, settings window, event pump from the controller thread.

mod bubble;
mod settings;

use crate::controller::{Cmd, Event, State};
use crate::shortcut::Status;
use adw::prelude::*;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::Duration;

pub const APP_ID: &str = "cz.etnetera.Dictee";

pub fn run(cmd_tx: Sender<Cmd>, events: async_channel::Receiver<Event>, status: Status, show_settings: bool) {
    let app = adw::Application::builder().application_id(APP_ID).flags(gtk::gio::ApplicationFlags::NON_UNIQUE).build();
    let events = Rc::new(RefCell::new(Some(events)));
    app.connect_activate(move |app| {
        // Activation can repeat; the UI is built once.
        let Some(events) = events.borrow_mut().take() else { return };
        let ui = Rc::new(Ui {
            bubble: bubble::Bubble::new(app, cmd_tx.clone()),
            settings: Rc::default(),
            app: app.clone(),
            status: status.clone(),
            _hold: app.hold(), // the daemon keeps running without any window
        });
        let u = ui.clone();
        gtk::glib::spawn_future_local(async move {
            while let Ok(ev) = events.recv().await {
                u.handle(ev);
            }
        });
        if show_settings {
            ui.show_settings();
        }
    });
    app.run_with_args::<&str>(&[]);
}

struct Ui {
    bubble: Rc<bubble::Bubble>,
    /// The open settings window. A closed window is destroyed (changes are saved on close), the next one is built from the file.
    settings: Rc<RefCell<Option<adw::ApplicationWindow>>>,
    app: adw::Application,
    status: Status,
    _hold: gtk::gio::ApplicationHoldGuard,
}

impl Ui {
    fn handle(&self, ev: Event) {
        match ev {
            Event::State(s) => self.bubble.set_state(s),
            Event::Level(l) => self.bubble.set_level(l),
            Event::HideBubble => self.bubble.set_state(State::Idle),
            Event::ShowSettings => self.show_settings(),
            Event::Quit => {
                // Closing the window flushes a change made less than the save delay ago.
                let window = RefCell::borrow(&self.settings).clone();
                if let Some(w) = window {
                    w.close();
                }
                self.app.quit()
            }
        }
    }

    fn show_settings(&self) {
        let mut slot = RefCell::borrow_mut(&self.settings);
        if let Some(w) = slot.as_ref() {
            w.present();
            return;
        }
        let w = settings::build(&self.app, self.status.clone());
        if std::env::var_os("DICTEE_DEBUG").is_some() {
            w.connect_is_active_notify(|w| eprintln!("dictee: settings is-active = {}", w.is_active()));
        }
        let registry = self.settings.clone();
        w.connect_close_request(move |_| {
            RefCell::borrow_mut(&registry).take(); // forget it; settings::build already flushes pending changes on close
            gtk::glib::Propagation::Proceed
        });
        w.present();
        if let Ok(path) = std::env::var("DICTEE_SNAPSHOT_SETTINGS") {
            let w2 = w.clone();
            gtk::glib::timeout_add_local_once(Duration::from_millis(1500), move || snapshot_png(&w2, &path));
        }
        *slot = Some(w);
    }
}

/// Dev helper: renders a widget to a PNG without needing compositor screenshots.
pub fn snapshot_png(widget: &impl IsA<gtk::Widget>, path: &str) {
    let widget = widget.as_ref();
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let (w, h) = (widget.width(), widget.height());
    if w == 0 || h == 0 {
        return;
    }
    let snap = gtk::Snapshot::new();
    paintable.snapshot(&snap, w as f64, h as f64);
    let Some(node) = snap.to_node() else { return };
    let Some(native) = widget.native() else { return };
    let Some(renderer) = native.renderer() else { return };
    let texture = renderer.render_texture(&node, None);
    if let Err(e) = texture.save_to_png(path) {
        eprintln!("dictee: snapshot failed: {e}");
    }
}
