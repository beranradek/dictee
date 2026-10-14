//! Small floating bubble that shows dictation progress.

use crate::controller::{Cmd, State};
use adw::prelude::*;
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

const CSS: &str = "
window.dictee-bubble { background: rgba(28,28,32,0.94); color: #fff; border-radius: 26px; }
window.dictee-bubble label { font-weight: 600; }
";

pub struct Bubble {
    window: gtk::Window,
    label: gtk::Label,
    dot: gtk::DrawingArea,
    state: Cell<State>,
    started: Cell<Option<Instant>>,
    level: RefCell<Option<Arc<AtomicU32>>>,
    timer_running: Cell<bool>,
}

impl Bubble {
    pub fn new(app: &adw::Application, tx: Sender<Cmd>) -> Rc<Self> {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &gtk::gdk::Display::default().expect("display"),
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let window = gtk::Window::builder()
            .application(app)
            .decorated(false)
            .resizable(false)
            .focusable(false)
            .title("Dictee")
            .css_classes(["dictee-bubble"])
            .build();
        let row = gtk::Box::builder().spacing(10).margin_start(16).margin_end(18).margin_top(10).margin_bottom(10).build();
        let dot = gtk::DrawingArea::builder().content_width(18).content_height(18).valign(gtk::Align::Center).build();
        let label = gtk::Label::new(Some("Listening"));
        row.append(&dot);
        row.append(&label);
        window.set_child(Some(&row));
        // Click the bubble to stop (or cancel with right click).
        let click = gtk::GestureClick::builder().button(0).build();
        click.connect_pressed(move |g, _, _, _| {
            let _ = tx.send(if g.current_button() == 3 { Cmd::Cancel } else { Cmd::Toggle });
        });
        window.add_controller(click);
        if std::env::var_os("DICTEE_DEBUG").is_some() {
            window.connect_is_active_notify(|w| eprintln!("dictee: bubble is-active = {}", w.is_active()));
        }

        let bubble = Rc::new(Self {
            window,
            label,
            dot,
            state: Cell::new(State::Idle),
            started: Cell::new(None),
            level: RefCell::new(None),
            timer_running: Cell::new(false),
        });
        let b = bubble.clone();
        bubble.dot.set_draw_func(move |_, cr, w, h| b.draw_dot(cr, w as f64, h as f64));
        bubble
    }

    pub fn set_level(&self, level: Arc<AtomicU32>) {
        *self.level.borrow_mut() = Some(level);
    }

    pub fn set_state(self: &Rc<Self>, state: State) {
        self.state.set(state);
        match state {
            State::Idle => {
                self.started.set(None);
                self.window.set_visible(false);
                return;
            }
            State::Recording => self.started.set(Some(Instant::now())),
            _ => {}
        }
        self.tick();
        self.window.present();
        self.start_timer();
        self.snapshot_if_requested();
    }

    /// Redraws the timer and level while the bubble is visible; the timer stops itself when the bubble is idle.
    fn start_timer(self: &Rc<Self>) {
        if self.timer_running.replace(true) {
            return;
        }
        let b = self.clone();
        gtk::glib::timeout_add_local(Duration::from_millis(60), move || {
            if b.state.get() == State::Idle {
                b.timer_running.set(false);
                return gtk::glib::ControlFlow::Break;
            }
            b.tick();
            gtk::glib::ControlFlow::Continue
        });
    }

    fn snapshot_if_requested(&self) {
        if let Ok(dir) = std::env::var("DICTEE_SNAPSHOT_BUBBLE") {
            let w = self.window.clone();
            let name = format!("{dir}/bubble-{}.png", self.state.get().as_str());
            gtk::glib::timeout_add_local_once(Duration::from_millis(700), move || super::snapshot_png(&w, &name));
        }
    }

    fn tick(&self) {
        let text = match self.state.get() {
            State::Recording => {
                let s = self.started.get().map(|t| t.elapsed().as_secs()).unwrap_or(0);
                format!("Listening  {}:{:02}", s / 60, s % 60)
            }
            State::Transcribing => "Transcribing...".to_string(),
            State::Rewriting => "Improving...".to_string(),
            State::Idle => return, // hidden: nothing to update
        };
        self.label.set_text(&text);
        self.dot.queue_draw();
    }

    fn draw_dot(&self, cr: &gtk::cairo::Context, w: f64, h: f64) {
        let (r, g, b, radius) = match self.state.get() {
            State::Recording => {
                let lvl = self.level.borrow().as_ref().map(|l| l.load(Ordering::Relaxed)).unwrap_or(0) as f64;
                (1.0, 0.25, 0.25, 0.45 + (lvl / 4000.0).min(1.0) * 0.55)
            }
            State::Idle => (1.0, 1.0, 1.0, 0.7),
            _ => (1.0, 0.75, 0.2, 0.8),
        };
        cr.set_source_rgb(r, g, b);
        cr.arc(w / 2.0, h / 2.0, (w.min(h) / 2.0) * radius, 0.0, std::f64::consts::TAU);
        let _ = cr.fill();
    }
}
