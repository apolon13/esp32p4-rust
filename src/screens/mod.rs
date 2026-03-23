pub mod logs;
pub mod mqtt;
pub mod rc_devices;
pub mod security;
pub mod settings;
pub mod wifi;

/// Убрать последний символ Unicode из строки.
/// Используется в DEL-кнопках виртуальной клавиатуры по всем экранам.
pub fn delete_last_char(s: &str) -> String {
    let mut chars = s.chars();
    chars.next_back();
    chars.as_str().to_owned()
}
