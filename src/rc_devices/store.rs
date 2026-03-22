use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};

use super::device::{DeviceType, RfDevice};

// ── NVS layout ───────────────────────────────────────────────────────────────
//
// Namespace : "rc_devs"   (≤ 15 chars)
//
// Global keys
//   "nid"        → u32   : следующий свободный id
//   "ids"        → str   : comma-separated список активных id, e.g. "1,3,5"
//
// На устройство с id N (N ≤ 9999, ключи ≤ 7 chars):
//   "nm{N}"      → str   : название (≤ 63 bytes)
//   "ty{N}"      → str   : тип устройства (≤ 15 bytes)
//   "cx{N}"      → str   : код в hex (≤ 15 bytes)
//   "pr{N}"      → str   : протокол (≤ 15 bytes)
//   "bc{N}"      → u8    : количество бит

const NS: &str = "rc_devs";

pub struct DeviceStore {
    partition: EspDefaultNvsPartition,
    devices:   Vec<RfDevice>,
    next_id:   u32,
}

impl DeviceStore {
    /// Открыть (или инициализировать) хранилище.
    /// Загружает все устройства из NVS при старте.
    pub fn open(partition: EspDefaultNvsPartition) -> Self {
        let (devices, next_id) = match EspNvs::new(partition.clone(), NS, true) {
            Ok(nvs) => Self::load_all(&nvs),
            Err(e)  => {
                log::warn!("DeviceStore: NVS open failed: {e}");
                (Vec::new(), 1)
            }
        };
        log::info!("DeviceStore: loaded {} device(s)", devices.len());
        Self { partition, devices, next_id }
    }

    // ── Запросы (read-only) ───────────────────────────────────────

    pub fn devices(&self) -> &[RfDevice] {
        &self.devices
    }

    /// Проверяет, зарегистрирован ли уже такой hex-код (защита от дубликатов).
    pub fn contains_code(&self, code_hex: &str) -> bool {
        self.devices.iter().any(|d| d.code_hex() == code_hex)
    }

    // ── Мутации ───────────────────────────────────────────────────

    /// Добавить новое устройство; назначает id из счётчика; сохраняет в NVS.
    /// Возвращает ссылку на сохранённое устройство.
    pub fn add(
        &mut self,
        name:        &str,
        device_type: DeviceType,
        code_hex:    &str,
        protocol:    &str,
        bit_count:   u8,
    ) -> &RfDevice {
        let id = self.next_id;
        self.next_id += 1;

        let device = RfDevice::from_parts(
            id,
            name.to_owned(),
            device_type,
            code_hex.to_owned(),
            protocol.to_owned(),
            bit_count,
        );

        // Push first so write_index sees the complete list.
        self.devices.push(device);
        let device = self.devices.last().unwrap();
        match EspNvs::new(self.partition.clone(), NS, true) {
            Ok(mut nvs) => {
                Self::write_device(&mut nvs, device);
                Self::write_index(&mut nvs, &self.devices, self.next_id);
            }
            Err(e) => log::warn!("DeviceStore: NVS open failed on add: {e}"),
        }
        self.devices.last().unwrap()
    }

    /// Заменить существующее устройство обновлённой копией (сохраняет в NVS).
    /// Ничего не делает, если id не найден.
    pub fn update(&mut self, updated: RfDevice) {
        if let Some(pos) = self.devices.iter().position(|d| d.id() == updated.id()) {
            self.devices[pos] = updated;
            match EspNvs::new(self.partition.clone(), NS, true) {
                Ok(mut nvs) => {
                    Self::write_device(&mut nvs, &self.devices[pos]);
                    Self::write_index(&mut nvs, &self.devices, self.next_id);
                }
                Err(e) => log::warn!("DeviceStore: NVS open failed on update: {e}"),
            }
        }
    }

    /// Удалить устройство по id; стирает его ключи из NVS.
    pub fn delete(&mut self, id: u32) {
        if let Some(pos) = self.devices.iter().position(|d| d.id() == id) {
            self.devices.remove(pos);
            self.persist_all_after_delete(id);
        }
    }

    // ── Приватные NVS-операции ────────────────────────────────────

    fn persist_all_after_delete(&self, erased_id: u32) {
        match EspNvs::new(self.partition.clone(), NS, true) {
            Ok(mut nvs) => {
                Self::erase_device_keys(&mut nvs, erased_id);
                Self::write_index(&mut nvs, &self.devices, self.next_id);
            }
            Err(e) => log::warn!("DeviceStore: NVS open failed on delete: {e}"),
        }
    }

    fn load_all(nvs: &EspNvs<NvsDefault>) -> (Vec<RfDevice>, u32) {
        let next_id = nvs.get_u32("nid").ok().flatten().unwrap_or(1);

        let mut ids_buf = [0u8; 256];
        let ids_str = nvs.get_str("ids", &mut ids_buf)
            .ok()
            .flatten()
            .unwrap_or("");

        let mut devices = Vec::new();
        for tok in ids_str.split(',').filter(|s| !s.is_empty()) {
            let Ok(id) = tok.parse::<u32>() else { continue };
            if let Some(dev) = Self::read_device(nvs, id) {
                devices.push(dev);
            }
        }
        (devices, next_id)
    }

    fn read_device(nvs: &EspNvs<NvsDefault>, id: u32) -> Option<RfDevice> {
        let mut nm_buf = [0u8; 65];
        let mut ty_buf = [0u8; 17];
        let mut cx_buf = [0u8; 17];
        let mut pr_buf = [0u8; 17];

        let name     = nvs.get_str(&key("nm", id), &mut nm_buf).ok()?.unwrap_or("").to_owned();
        let type_str = nvs.get_str(&key("ty", id), &mut ty_buf).ok()?.unwrap_or("").to_owned();
        let code_hex = nvs.get_str(&key("cx", id), &mut cx_buf).ok()?.unwrap_or("").to_owned();
        let protocol = nvs.get_str(&key("pr", id), &mut pr_buf).ok()?.unwrap_or("").to_owned();
        let bit_count = nvs.get_u8(&key("bc", id)).ok()??.into();

        if code_hex.is_empty() {
            return None; // повреждённая запись
        }

        Some(RfDevice::from_parts(
            id,
            name,
            DeviceType::from_str(&type_str),
            code_hex,
            protocol,
            bit_count,
        ))
    }

    fn write_device(nvs: &mut EspNvs<NvsDefault>, d: &RfDevice) {
        let id = d.id();
        let _ = nvs.set_str(&key("nm", id), d.name());
        let _ = nvs.set_str(&key("ty", id), d.device_type().as_str());
        let _ = nvs.set_str(&key("cx", id), d.code_hex());
        let _ = nvs.set_str(&key("pr", id), d.protocol());
        let _ = nvs.set_u8(&key("bc", id), d.bit_count());
    }

    fn erase_device_keys(nvs: &mut EspNvs<NvsDefault>, id: u32) {
        // esp-idf-svc не предоставляет erase_key; перезаписываем пустыми значениями.
        let _ = nvs.set_str(&key("nm", id), "");
        let _ = nvs.set_str(&key("ty", id), "");
        let _ = nvs.set_str(&key("cx", id), "");
        let _ = nvs.set_str(&key("pr", id), "");
        let _ = nvs.set_u8(&key("bc", id), 0);
    }

    fn write_index(nvs: &mut EspNvs<NvsDefault>, devices: &[RfDevice], next_id: u32) {
        let ids: String = devices.iter()
            .map(|d| d.id().to_string())
            .collect::<Vec<_>>()
            .join(",");
        let _ = nvs.set_str("ids", &ids);
        let _ = nvs.set_u32("nid", next_id);
    }
}

// ── Вспомогательная функция: построить NVS-ключ вида "nm42" ─────────────────

fn key(prefix: &str, id: u32) -> String {
    format!("{prefix}{id}")
}
