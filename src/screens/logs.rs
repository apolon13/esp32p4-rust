use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, Model};

use crate::logger::LogBuffer;
use crate::{AppWindow, LogEntry as SlintLogEntry};

/// Обработчик экрана логов.
///
/// Опрашивает [`LogBuffer`] раз в кадр и пушит новые строки в Slint-модель.
/// Навигация (в конец / в начало / очистить) обрабатывается через коллбэки.
pub struct LogScreenHandler {
    buffer:      Arc<Mutex<LogBuffer>>,
    app:         slint::Weak<AppWindow>,
    known_total: std::cell::Cell<usize>,
}

impl LogScreenHandler {
    pub fn new(app: &AppWindow, buffer: Arc<Mutex<LogBuffer>>) -> Self {
        Self::register_callbacks(app);

        Self {
            buffer,
            app: app.as_weak(),
            known_total: std::cell::Cell::new(0),
        }
    }

    /// Синхронизирует новые записи в Slint-модель.  Не блокируется.
    pub fn poll(&self) {
        let Some(app) = self.app.upgrade() else { return };

        let Ok(buf) = self.buffer.try_lock() else { return };

        let total = buf.total();
        if total == self.known_total.get() {
            return; // новых записей нет
        }
        self.known_total.set(total);

        // Перестраиваем модель целиком (буфер небольшой — ≤ 256 строк).
        let items: Vec<SlintLogEntry> = buf.entries()
            .map(|e| SlintLogEntry {
                level:   e.level as i32,
                message: e.message.as_str().into(),
            })
            .collect();

        let model = std::rc::Rc::new(slint::VecModel::from(items));
        let count = model.row_count() as i32;
        app.set_log_entries(model.into());

        // Автопрокрутка в конец, если пользователь не зафиксировал позицию.
        if !app.get_log_pinned() {
            app.set_log_scroll_to(count.saturating_sub(1));
        }
    }

    // ── Регистрация коллбэков ─────────────────────────────────────

    fn register_callbacks(app: &AppWindow) {
        // «В конец»
        {
            let app_weak = app.as_weak();
            app.on_log_scroll_end(move || {
                let app   = app_weak.upgrade().unwrap();
                let count = app.get_log_entries().row_count() as i32;
                app.set_log_scroll_to(count.saturating_sub(1));
                app.set_log_pinned(false);
            });
        }
        // «В начало»
        {
            let app_weak = app.as_weak();
            app.on_log_scroll_start(move || {
                let app = app_weak.upgrade().unwrap();
                app.set_log_scroll_to(0);
                app.set_log_pinned(true);
            });
        }
        // «Зафиксировать / снять фиксацию»
        {
            let app_weak = app.as_weak();
            app.on_log_toggle_pin(move || {
                let app     = app_weak.upgrade().unwrap();
                let pinned  = app.get_log_pinned();
                app.set_log_pinned(!pinned);
            });
        }
    }
}
