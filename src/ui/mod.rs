//! GTK front end: floating bubble, settings window, event pump from the controller thread.

mod bubble;
mod settings;

use crate::controller::{Cmd, Event, SharedState, State};
use crate::shortcut::Status;
use adw::prelude::*;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

pub const APP_ID: &str = "cz.etnetera.Dictee";

pub fn run(cmd_tx: Sender<Cmd>, events: Receiver<Event>, _state: SharedState, status: Status, show_settings: bool) {
    let app = adw::Application::builder().application_id(APP_ID).flags(gtk::gio::ApplicationFlags::NON_UNIQUE).build();
    let slot: Rc<RefCell<Option<Rc<Ui>>>> = Rc::default();
    let events = Rc::new(RefCell::new(Some(events)));
    app.connect_activate(move |app| {
        if slot.borrow().is_some() {
            return;
        }
        let ui = Rc::new(Ui {
            bubble: bubble::Bubble::new(app, cmd_tx.clone()),
            settings: RefCell::new(None),
            app: app.clone(),
            cmd_tx: cmd_tx.clone(),
            status: status.clone(),
        });
        std::mem::forget(app.hold());
        let rx = events.borrow_mut().take().expect("events");
        let u = ui.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(40), move || {
            while let Ok(ev) = rx.try_recv() {
                u.handle(ev);
            }
            gtk::glib::ControlFlow::Continue
        });
        if show_settings {
            ui.show_settings();
        }
        *slot.borrow_mut() = Some(ui);
    });
    app.run_with_args::<&str>(&[]);
}

struct Ui {
    bubble: Rc<bubble::Bubble>,
    settings: RefCell<Option<adw::ApplicationWindow>>,
    app: adw::Application,
    cmd_tx: Sender<Cmd>,
    status: Status,
}

impl Ui {
    fn handle(self: &Rc<Self>, ev: Event) {
        match ev {
            Event::State(State::Idle) => self.bubble.set_state(State::Idle),
            Event::State(s) if crate::config::Config::load().map(|c| c.show_bubble).unwrap_or(true) => self.bubble.set_state(s),
            Event::State(_) => {}
            Event::Level(l) => self.bubble.set_level(l),
            Event::Done => {}
            Event::HideBubble => self.bubble.set_state(State::Idle),
            Event::Error(e) => eprintln!("dictee: {e}"),
            Event::ShowSettings => self.show_settings(),
            Event::Quit => self.app.quit(),
        }
    }

    fn show_settings(&self) {
        let mut slot = self.settings.borrow_mut();
        if let Some(w) = slot.as_ref() {
            w.present();
            return;
        }
        let w = settings::build(&self.app, self.status.clone(), self.cmd_tx.clone());
        // Keep the window alive (hidden) so it can be shown again.
        w.connect_close_request(|w| {
            w.set_visible(false);
            gtk::glib::Propagation::Stop
        });
        if std::env::var_os("DICTEE_DEBUG").is_some() {
            w.connect_is_active_notify(|w| eprintln!("dictee: settings is-active = {}", w.is_active()));
        }
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
