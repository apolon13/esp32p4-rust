use std::cell::RefCell;
use std::rc::Rc;

use slint::ComponentHandle;

use crate::rc_devices::{DeviceStore, DeviceType, RfDevice};
use crate::rf_receiver::{RfCode, RfReceiver};
use crate::{AppWindow, RcDeviceInfo};

// ── Результат режима обучения ────────────────────────────────────────────────

/// Value-object: принятый RF-код с автоопределёнными атрибутами.
/// Создаётся только из [`RfCode`]; инвариант — `code_hex` не пустой.
struct ScanCandidate {
    code_hex:    String,
    protocol:    &'static str,
    bit_count:   u8,
    device_type: DeviceType,
}

impl ScanCandidate {
    fn from_rf_code(rf: &RfCode) -> Self {
        Self {
            code_hex:    format!("{:X}", rf.code),
            protocol:    rf.protocol,
            bit_count:   rf.bit_count,
            device_type: DeviceType::inferred_from(rf.protocol, rf.bit_count),
        }
    }

    /// Заполнить поля формы и переключить экран на редактирование.
    fn populate_form(&self, app: &AppWindow) {
        app.set_rc_code_hex(self.code_hex.as_str().into());
        app.set_rc_protocol(self.protocol.into());
        app.set_rc_bit_count(self.bit_count as i32);
        app.set_rc_edit_id(0);
        app.set_rc_device_name("".into());
        app.set_rc_device_type(self.device_type.as_str().into());
        app.set_rc_scanning(false);
        app.set_rc_show_form(true);
    }
}

// ── Конвертация домен → Slint ────────────────────────────────────────────────

fn to_slint(d: &RfDevice) -> RcDeviceInfo {
    RcDeviceInfo {
        id:          d.id() as i32,
        name:        d.name().into(),
        device_type: d.device_type().as_str().into(),
        code_hex:    d.code_hex().into(),
        protocol:    d.protocol().into(),
        bit_count:   d.bit_count() as i32,
    }
}

// ── Обработчик экрана RF-устройств ──────────────────────────────────────────

/// Связывает [`DeviceStore`] и [`RfReceiver`] с UI-экраном RF-устройств.
///
/// Вызывайте [`RcDevicesScreenHandler::poll`] один раз за кадр из render-loop.
pub struct RcDevicesScreenHandler {
    rf_receiver: RfReceiver,
    app:         slint::Weak<AppWindow>,
    store:       Rc<RefCell<DeviceStore>>,
}

impl RcDevicesScreenHandler {
    pub fn new(app: &AppWindow, rf_receiver: RfReceiver, store: DeviceStore) -> Self {
        let store = Rc::new(RefCell::new(store));

        Self::sync_devices_to_ui(app, &store.borrow());
        Self::register_callbacks(app, &store);

        Self { rf_receiver, app: app.as_weak(), store }
    }

    /// Дренирует входящие RF-коды и обновляет UI.  Не блокируется.
    pub fn poll(&self) {
        let Some(app) = self.app.upgrade() else { return };

        while let Some(rf) = self.rf_receiver.try_recv() {
            if !app.get_rc_scanning() {
                continue; // не в режиме обучения
            }

            let candidate = ScanCandidate::from_rf_code(&rf);

            if self.store.borrow().contains_code(&candidate.code_hex) {
                log::info!("RF learn: код 0x{} уже есть — пропускаем", candidate.code_hex);
                continue;
            }

            log::info!("RF learn: новый код 0x{} [{}]", candidate.code_hex, candidate.protocol);
            candidate.populate_form(&app);
            break; // обрабатываем один код за кадр
        }
    }

    // ── Приватные методы ──────────────────────────────────────────

    fn sync_devices_to_ui(app: &AppWindow, store: &DeviceStore) {
        let items: Vec<RcDeviceInfo> = store.devices().iter().map(to_slint).collect();
        app.set_rc_devices(Rc::new(slint::VecModel::from(items)).into());
    }

    fn register_callbacks(app: &AppWindow, store: &Rc<RefCell<DeviceStore>>) {
        Self::register_scan_callbacks(app);
        Self::register_save_callback(app, store);
        Self::register_delete_callback(app, store);
        Self::register_name_delete_last(app);
    }

    fn register_scan_callbacks(app: &AppWindow) {
        {
            let app_weak = app.as_weak();
            app.on_rc_scan_start(move || {
                app_weak.upgrade().unwrap().set_rc_scanning(true);
            });
        }
        {
            let app_weak = app.as_weak();
            app.on_rc_scan_cancel(move || {
                app_weak.upgrade().unwrap().set_rc_scanning(false);
            });
        }
    }

    /// `id == 0` → добавить новое устройство (код/протокол/биты берём из UI-свойств).
    /// `id  > 0` → обновить имя и тип существующего устройства.
    fn register_save_callback(app: &AppWindow, store: &Rc<RefCell<DeviceStore>>) {
        let app_weak = app.as_weak();
        let store    = store.clone();
        app.on_rc_device_save(move |id, name, type_str| {
            let app   = app_weak.upgrade().unwrap();
            let dtype = DeviceType::from_str(type_str.as_str());
            let mut st = store.borrow_mut();

            if id == 0 {
                let _ = st.add(
                    name.as_str(),
                    dtype,
                    app.get_rc_code_hex().as_str(),
                    app.get_rc_protocol().as_str(),
                    app.get_rc_bit_count() as u8,
                );
            } else if let Some(device) = st.devices()
                .iter()
                .find(|d| d.id() == id as u32)
                .cloned()
            {
                st.update(device.with_name(name.as_str()).with_type(dtype));
            }

            Self::sync_devices_to_ui(&app, &st);
        });
    }

    fn register_delete_callback(app: &AppWindow, store: &Rc<RefCell<DeviceStore>>) {
        let app_weak = app.as_weak();
        let store    = store.clone();
        app.on_rc_device_delete(move |id| {
            let app = app_weak.upgrade().unwrap();
            let mut st = store.borrow_mut();
            st.delete(id as u32);
            Self::sync_devices_to_ui(&app, &st);
        });
    }

    /// DEL-кнопка клавиатуры не умеет нарезать UTF-8 — делаем в Rust.
    fn register_name_delete_last(app: &AppWindow) {
        let app_weak = app.as_weak();
        app.on_rc_device_name_delete_last(move || {
            let app  = app_weak.upgrade().unwrap();
            let cur  = app.get_rc_device_name().to_string();
            let next = cur.chars().take(cur.chars().count().saturating_sub(1)).collect::<String>();
            app.set_rc_device_name(next.into());
        });
    }
}
