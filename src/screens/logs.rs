use std::sync::{Arc, Mutex};
use std::rc::Rc;

use slint::{ComponentHandle, Model};

use crate::logger::LogBuffer;
use crate::{AppWindow, LogEntry as SlintLogEntry};

pub struct LogScreenHandler {
    buffer:      Arc<Mutex<LogBuffer>>,
    app:         slint::Weak<AppWindow>,
    known_total: std::cell::Cell<usize>,
}

impl LogScreenHandler {
    pub fn new(app: &AppWindow, buffer: Arc<Mutex<LogBuffer>>) -> Self {
        Self::register_scroll_end(app);
        Self::register_scroll_start(app);
        Self::register_toggle_pin(app);
        Self { buffer, app: app.as_weak(), known_total: std::cell::Cell::new(0) }
    }

    pub fn poll(&self) {
        let Some(app) = self.app.upgrade() else { return };
        let Ok(buf)   = self.buffer.try_lock() else { return };
        let total     = buf.total();
        if total == self.known_total.get() { return; }
        self.known_total.set(total);
        push_new_entries(&app, &buf);
    }

    fn register_scroll_end(app: &AppWindow) {
        let app_weak = app.as_weak();
        app.on_log_scroll_end(move || {
            let app   = app_weak.upgrade().unwrap();
            let count = app.get_log_entries().row_count() as i32;
            app.set_log_scroll_to(count.saturating_sub(1));
            app.set_log_pinned(false);
        });
    }

    fn register_scroll_start(app: &AppWindow) {
        let app_weak = app.as_weak();
        app.on_log_scroll_start(move || {
            let app = app_weak.upgrade().unwrap();
            app.set_log_scroll_to(0);
            app.set_log_pinned(true);
        });
    }

    fn register_toggle_pin(app: &AppWindow) {
        let app_weak = app.as_weak();
        app.on_log_toggle_pin(move || {
            let app    = app_weak.upgrade().unwrap();
            let pinned = app.get_log_pinned();
            app.set_log_pinned(!pinned);
        });
    }
}

fn push_new_entries(app: &AppWindow, buf: &LogBuffer) {
    let items: Vec<SlintLogEntry> = buf.entries()
        .map(|e| SlintLogEntry { level: e.level as i32, message: e.message.as_str().into() })
        .collect();
    let model = Rc::new(slint::VecModel::from(items));
    let count = model.row_count() as i32;
    app.set_log_entries(model.into());
    if !app.get_log_pinned() {
        app.set_log_scroll_to(count.saturating_sub(1));
    }
}
