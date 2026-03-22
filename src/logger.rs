//! Кастомный логгер: перехватывает все вызовы `log::*` макросов,
//! форматирует строки и помещает их в кольцевой буфер.
//! Сообщения одновременно выводятся в UART через `eprintln!`.

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::sync::{Arc, Mutex};

// ── Константы ────────────────────────────────────────────────────────────────

/// Максимальное количество хранимых строк лога.
const RING_CAPACITY: usize = 256;

// ── Запись лога ──────────────────────────────────────────────────────────────

/// `level`: 0=Error 1=Warn 2=Info 3=Debug 4=Trace  (совпадает с log_screen.slint)
#[derive(Clone)]
pub struct LogEntry {
    pub level:   i32,
    pub message: String,
}

fn level_to_int(l: Level) -> i32 {
    match l {
        Level::Error => 0,
        Level::Warn  => 1,
        Level::Info  => 2,
        Level::Debug => 3,
        Level::Trace => 4,
    }
}

fn level_tag(l: Level) -> char {
    match l {
        Level::Error => 'E',
        Level::Warn  => 'W',
        Level::Info  => 'I',
        Level::Debug => 'D',
        Level::Trace => 'T',
    }
}

// ── Кольцевой буфер ──────────────────────────────────────────────────────────

pub struct LogBuffer {
    entries: Vec<LogEntry>,
    /// Монотонно возрастающее число добавленных записей (включая вытесненные).
    total:   usize,
}

impl LogBuffer {
    fn new() -> Self {
        Self { entries: Vec::with_capacity(RING_CAPACITY), total: 0 }
    }

    fn push(&mut self, entry: LogEntry) {
        if self.entries.len() == RING_CAPACITY {
            self.entries.remove(0);
        }
        self.entries.push(entry);
        self.total += 1;
    }

    pub fn entries(&self) -> &[LogEntry] {
        &self.entries
    }

    pub fn total(&self) -> usize {
        self.total
    }
}

// ── Логгер ───────────────────────────────────────────────────────────────────

pub struct AppLogger {
    buffer: Arc<Mutex<LogBuffer>>,
}

impl AppLogger {
    fn new(buffer: Arc<Mutex<LogBuffer>>) -> Self {
        Self { buffer }
    }
}

impl Log for AppLogger {
    fn enabled(&self, meta: &Metadata) -> bool {
        meta.level() <= Level::Debug
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) { return; }

        // UART-вывод (eprint! идёт в stderr → UART0 на ESP-IDF).
        eprintln!("{} [{}] {}", level_tag(record.level()), record.target(), record.args());

        // Запись в кольцевой буфер.
        let msg   = format!("[{}] {}", record.target(), record.args());
        let entry = LogEntry { level: level_to_int(record.level()), message: msg };
        if let Ok(mut buf) = self.buffer.lock() {
            buf.push(entry);
        }
    }

    fn flush(&self) {}
}

// ── Публичный handle ─────────────────────────────────────────────────────────

/// Устанавливает `AppLogger` глобальным логгером.
/// Возвращает `Arc` на буфер — передайте его в `LogScreenHandler`.
pub fn install() -> Arc<Mutex<LogBuffer>> {
    let buffer = Arc::new(Mutex::new(LogBuffer::new()));
    let logger = Box::new(AppLogger::new(buffer.clone()));

    log::set_boxed_logger(logger).expect("logger already set");
    log::set_max_level(LevelFilter::Debug);

    buffer
}
