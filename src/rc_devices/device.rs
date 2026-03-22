/// Тип устройства — enum вместо свободной строки.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DeviceType {
    Sensor,    // Датчик
    Remote,    // Пульт
    Motion,    // Движение
    Garage,    // Гараж
    SmokeFire, // Дым/Газ
    Other,     // Другое
}

impl DeviceType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sensor    => "Датчик",
            Self::Remote    => "Пульт",
            Self::Motion    => "Движение",
            Self::Garage    => "Гараж",
            Self::SmokeFire => "Дым/Газ",
            Self::Other     => "Другое",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "Пульт"    => Self::Remote,
            "Движение" => Self::Motion,
            "Гараж"    => Self::Garage,
            "Дым/Газ"  => Self::SmokeFire,
            "Датчик"   => Self::Sensor,
            _          => Self::Other,
        }
    }

    /// Автоопределение типа по протоколу и ширине кода.
    pub fn inferred_from(protocol: &str, bits: u8) -> Self {
        match protocol {
            "NiceFLO" | "CAME" => Self::Garage,
            "HT6P20B"          => Self::Remote,
            "HS2303"           => Self::Motion,
            _ if bits == 12    => Self::Garage,
            _                  => Self::Sensor,
        }
    }
}

// ── Доменный объект ──────────────────────────────────────────────────────────
//
// Инварианты:
//   • id > 0 (назначается только через DeviceStore)
//   • code_hex не пустой (задаётся при создании из RfCode)
//
// Публичных сеттеров нет. Изменение атрибутов — через методы `with_*`,
// которые возвращают новый экземпляр (value-object pattern).

#[derive(Clone, Debug)]
pub struct RfDevice {
    id:          u32,
    name:        String,
    device_type: DeviceType,
    code_hex:    String,
    protocol:    String,
    bit_count:   u8,
}

impl RfDevice {
    /// Конструктор доступен только внутри модуля rc_devices —
    /// создавать устройства с id можно только через [`super::store::DeviceStore`].
    pub(super) fn from_parts(
        id:          u32,
        name:        String,
        device_type: DeviceType,
        code_hex:    String,
        protocol:    String,
        bit_count:   u8,
    ) -> Self {
        debug_assert!(id > 0,          "device id must be positive");
        debug_assert!(!code_hex.is_empty(), "code_hex must not be empty");
        Self { id, name, device_type, code_hex, protocol, bit_count }
    }

    // ── Getters ──────────────────────────────────────────────────
    pub fn id(&self)          -> u32         { self.id }
    pub fn name(&self)        -> &str        { &self.name }
    pub fn device_type(&self) -> DeviceType  { self.device_type }
    pub fn code_hex(&self)    -> &str        { &self.code_hex }
    pub fn protocol(&self)    -> &str        { &self.protocol }
    pub fn bit_count(&self)   -> u8          { self.bit_count }

    // ── "Builder" методы (возвращают новый объект) ────────────────
    pub fn with_name(self, name: impl Into<String>) -> Self {
        Self { name: name.into(), ..self }
    }

    pub fn with_type(self, device_type: DeviceType) -> Self {
        Self { device_type, ..self }
    }
}
