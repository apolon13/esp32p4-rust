use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use std::sync::mpsc::{Receiver, SyncSender};

use super::device::{DeviceType, RfDevice};

// ── NVS layout ───────────────────────────────────────────────────────────────
//
// Namespace : "rc_devs"   (≤ 15 chars)
//
// Global key
//   "nid"        → u32   : следующий свободный id  (1..MAX_ID)
//
// На устройство с id N (N < MAX_ID=1000, ключи ≤ 6 chars "nm999"):
//   "nm{N}"      → str   : название (≤ 63 bytes)
//   "ty{N}"      → str   : тип (стабильный ключ: "sensor","remote",…)
//   "cx{N}"      → str   : код в hex (≤ 15 bytes)
//   "pr{N}"      → str   : протокол (≤ 15 bytes)
//   "bc{N}"      → u8    : количество бит
//
// Индексный ключ "ids" (CSV) убран.  Устройства перечисляются сканом
// от 1 до next_id при старте — запись существует если cx{N} непустой.

const NS:     &str = "rc_devs";
const MAX_ID: u32  = 1000; // ключи "nm999" = 5 символов — в пределах лимита NVS (15)

// ── Фоновые NVS-операции ─────────────────────────────────────────────────────

struct NvsDeviceData {
    id:        u32,
    name:      String,
    type_key:  String,
    code_hex:  String,
    protocol:  String,
    bit_count: u8,
}

enum NvsOp {
    /// Записать/обновить устройство + сохранить next_id.
    Persist(NvsDeviceData, u32),
    /// Стереть поля устройства + сохранить next_id.
    Delete(u32, u32),
}

// ── DeviceStore ──────────────────────────────────────────────────────────────

pub struct DeviceStore {
    devices: Vec<RfDevice>,
    next_id: u32,
    nvs_tx:  SyncSender<NvsOp>,
}

impl DeviceStore {
    /// Открыть хранилище: загрузить данные из NVS, запустить фоновый поток записи.
    pub fn open(partition: EspDefaultNvsPartition) -> Self {
        let (devices, next_id) = match EspNvs::new(partition.clone(), NS, true) {
            Ok(nvs) => Self::load_all(&nvs),
            Err(e)  => {
                log::warn!("DeviceStore: NVS open failed on startup: {e}");
                (Vec::new(), 1)
            }
        };
        log::info!(
            "DeviceStore: loaded {} device(s), next_id={}",
            devices.len(), next_id,
        );

        let (nvs_tx, nvs_rx) = std::sync::mpsc::sync_channel::<NvsOp>(64);

        std::thread::Builder::new()
            .stack_size(6144)
            .name("nvs_store".to_string())
            .spawn(move || nvs_writer_loop(partition, nvs_rx))
            .expect("nvs_store thread spawn failed");

        Self { devices, next_id, nvs_tx }
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

    /// Добавить новое устройство.
    /// Возвращает `None` если достигнут лимит [`MAX_ID`].
    pub fn add(
        &mut self,
        name:        &str,
        device_type: DeviceType,
        code_hex:    &str,
        protocol:    &str,
        bit_count:   u8,
    ) -> Option<&RfDevice> {
        if self.next_id >= MAX_ID {
            log::warn!("DeviceStore: device limit ({MAX_ID}) reached — ignoring add");
            return None;
        }
        let id = self.next_id;
        self.next_id += 1;

        self.devices.push(RfDevice::from_parts(
            id,
            name.to_owned(),
            device_type,
            code_hex.to_owned(),
            protocol.to_owned(),
            bit_count,
        ));

        let pos = self.devices.len() - 1;
        let data = to_nvs_data(&self.devices[pos]);
        self.enqueue(NvsOp::Persist(data, self.next_id));
        Some(&self.devices[pos])
    }

    /// Заменить существующее устройство обновлённой копией.
    pub fn update(&mut self, updated: RfDevice) {
        if let Some(pos) = self.devices.iter().position(|d| d.id() == updated.id()) {
            self.devices[pos] = updated;
            let data = to_nvs_data(&self.devices[pos]);
            self.enqueue(NvsOp::Persist(data, self.next_id));
        }
    }

    /// Удалить устройство по id.
    pub fn delete(&mut self, id: u32) {
        if self.devices.iter().any(|d| d.id() == id) {
            self.devices.retain(|d| d.id() != id);
            self.enqueue(NvsOp::Delete(id, self.next_id));
        }
    }

    // ── Приватные методы ──────────────────────────────────────────

    fn enqueue(&self, op: NvsOp) {
        if self.nvs_tx.try_send(op).is_err() {
            log::warn!("DeviceStore: NVS write queue full — write may be lost");
        }
    }

    // ── NVS-чтение (при старте, на вызывающем потоке) ─────────────

    fn load_all(nvs: &EspNvs<NvsDefault>) -> (Vec<RfDevice>, u32) {
        let next_id = nvs.get_u32("nid").ok().flatten().unwrap_or(1).min(MAX_ID);
        let mut devices = Vec::new();
        for id in 1..next_id {
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

        let name      = nvs.get_str(&key("nm", id), &mut nm_buf).ok()?.unwrap_or("").to_owned();
        let type_str  = nvs.get_str(&key("ty", id), &mut ty_buf).ok()?.unwrap_or("").to_owned();
        let code_hex  = nvs.get_str(&key("cx", id), &mut cx_buf).ok()?.unwrap_or("").to_owned();
        let protocol  = nvs.get_str(&key("pr", id), &mut pr_buf).ok()?.unwrap_or("").to_owned();
        let bit_count = nvs.get_u8(&key("bc", id)).ok()??.into();

        if code_hex.is_empty() {
            return None; // пустая/удалённая запись
        }

        Some(RfDevice::from_parts(
            id,
            name,
            DeviceType::from_key(&type_str), // обратно совместим со старыми UI-строками
            code_hex,
            protocol,
            bit_count,
        ))
    }
}

// ── Вспомогательные функции ──────────────────────────────────────────────────

fn to_nvs_data(d: &RfDevice) -> NvsDeviceData {
    NvsDeviceData {
        id:        d.id(),
        name:      d.name().to_owned(),
        type_key:  d.device_type().as_key().to_owned(),
        code_hex:  d.code_hex().to_owned(),
        protocol:  d.protocol().to_owned(),
        bit_count: d.bit_count(),
    }
}

fn key(prefix: &str, id: u32) -> String {
    format!("{prefix}{id}")
}

// ── Фоновый поток NVS-записи ─────────────────────────────────────────────────
//
// Блокируется на приёме команд. При получении команды дренирует очередь
// и фиксирует все накопленные изменения за один цикл open→write→drop(commit).
// Drop EspNvs вызывает nvs_commit() → данные записываются во flash.

fn nvs_writer_loop(partition: EspDefaultNvsPartition, rx: Receiver<NvsOp>) {
    loop {
        let first = match rx.recv() {
            Ok(op)  => op,
            Err(_)  => break, // sender уничтожен (store освобождён)
        };

        // Собираем все ожидающие операции для батч-записи
        let mut ops = vec![first];
        while let Ok(op) = rx.try_recv() {
            ops.push(op);
        }

        match EspNvs::new(partition.clone(), NS, true) {
            Ok(mut nvs) => {
                for op in ops {
                    apply_op(&mut nvs, op);
                }
                // nvs уничтожается здесь → nvs_commit() → запись во flash
            }
            Err(e) => log::warn!("NVS writer: open failed: {e}"),
        }
    }
    log::info!("NVS writer thread exiting");
}

fn apply_op(nvs: &mut EspNvs<NvsDefault>, op: NvsOp) {
    match op {
        NvsOp::Persist(d, next_id) => {
            let id = d.id;
            let _ = nvs.set_str(&key("nm", id), &d.name);
            let _ = nvs.set_str(&key("ty", id), &d.type_key);
            let _ = nvs.set_str(&key("cx", id), &d.code_hex);
            let _ = nvs.set_str(&key("pr", id), &d.protocol);
            let _ = nvs.set_u8(&key("bc", id), d.bit_count);
            let _ = nvs.set_u32("nid", next_id);
        }
        NvsOp::Delete(id, next_id) => {
            // esp-idf-svc не предоставляет erase_key напрямую;
            // перезаписываем пустыми значениями — cx пустое служит маркером удаления.
            let _ = nvs.set_str(&key("nm", id), "");
            let _ = nvs.set_str(&key("ty", id), "");
            let _ = nvs.set_str(&key("cx", id), "");
            let _ = nvs.set_str(&key("pr", id), "");
            let _ = nvs.set_u8(&key("bc", id), 0);
            let _ = nvs.set_u32("nid", next_id);
        }
    }
}
