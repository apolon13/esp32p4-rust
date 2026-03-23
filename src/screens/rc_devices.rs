use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::SyncSender;

use slint::ComponentHandle;

use crate::control::ControlCmd;
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
    cmd_tx:      SyncSender<ControlCmd>,
}

impl RcDevicesScreenHandler {
    pub fn new(app: &AppWindow, rf_receiver: RfReceiver, store: DeviceStore, cmd_tx: SyncSender<ControlCmd>) -> Self {
        let store = Rc::new(RefCell::new(store));
        Self::sync_devices_to_ui(app, &store.borrow());
        Self::register_callbacks(app, &store);
        Self { rf_receiver, app: app.as_weak(), store, cmd_tx }
    }

    /// Дренирует входящие RF-коды и обновляет UI.  Не блокируется.
    pub fn poll(&self) {
        let Some(app) = self.app.upgrade() else { return };
        if self.poll_binding(&app) { return; }
        if app.get_rc_scanning() { self.poll_scan(&app); } else { self.poll_runtime(); }
    }

    // ── Приватные методы ──────────────────────────────────────────

    /// Обрабатывает режим привязки. Возвращает `true`, если привязка активна.
    fn poll_binding(&self, app: &AppWindow) -> bool {
        if !app.get_rc_binding_active() { return false; }
        let button_idx = app.get_rc_binding_button_idx() as usize;
        if button_idx < 4 {
            let device_id = app.get_rc_binding_device_id() as u32;
            self.try_bind_button(app, device_id, button_idx);
        }
        true
    }

    fn try_bind_button(&self, app: &AppWindow, device_id: u32, button_idx: usize) {
        while let Some(rf) = self.rf_receiver.try_recv() {
            let code = format!("{:X}", rf.code);
            if self.code_bound_this_session(device_id, &code, button_idx) { continue; }
            self.store.borrow_mut().bind_button(device_id, button_idx, code.clone());
            Self::set_binding_btn_code(app, button_idx, code.into());
            app.set_rc_binding_button_idx(button_idx as i32 + 1);
            break;
        }
    }

    /// Проверяет, использован ли код в текущей сессии привязки (кнопки 0..button_idx).
    /// Игнорирует ранее сохранённые коды для кнопок начиная с button_idx —
    /// иначе повторная привязка пульта никогда не принимала бы прежние коды.
    fn code_bound_this_session(&self, device_id: u32, code: &str, current_idx: usize) -> bool {
        self.store.borrow()
            .devices()
            .iter()
            .find(|d| d.id() == device_id)
            .map_or(false, |dev| {
                (0..current_idx).any(|i| dev.button(i).map_or(false, |c| c == code))
            })
    }

    fn poll_scan(&self, app: &AppWindow) {
        while let Some(rf) = self.rf_receiver.try_recv() {
            let candidate = ScanCandidate::from_rf_code(&rf);
            let store = self.store.borrow();
            if store.contains_code(&candidate.code_hex) || store.contains_button_code(&candidate.code_hex) {
                log::info!("RF learn: код 0x{} уже привязан — пропускаем", candidate.code_hex);
                continue;
            }
            drop(store);
            log::info!("RF learn: новый код 0x{} [{}]", candidate.code_hex, candidate.protocol);
            candidate.populate_form(app);
            break;
        }
    }

    fn poll_runtime(&self) {
        while let Some(rf) = self.rf_receiver.try_recv() {
            let code = format!("{:X}", rf.code);
            if let Some(cmd) = code_to_cmd(&self.store.borrow(), &code) {
                log::info!("RF: команда {:?} от кода {}", cmd, code);
                let _ = self.cmd_tx.try_send(cmd);
            }
        }
    }

    fn sync_devices_to_ui(app: &AppWindow, store: &DeviceStore) {
        let items: Vec<RcDeviceInfo> = store.devices().iter().map(to_slint).collect();
        app.set_rc_devices(Rc::new(slint::VecModel::from(items)).into());
    }

    fn register_callbacks(app: &AppWindow, store: &Rc<RefCell<DeviceStore>>) {
        Self::register_scan_callbacks(app);
        Self::register_save_callback(app, store);
        Self::register_delete_callback(app, store);
        Self::register_name_delete_last(app);
        Self::register_binding_load_codes(app, store);
    }

    fn register_scan_callbacks(app: &AppWindow) {
        let app_weak = app.as_weak();
        app.on_rc_scan_start(move || { app_weak.upgrade().unwrap().set_rc_scanning(true); });
        let app_weak = app.as_weak();
        app.on_rc_scan_cancel(move || { app_weak.upgrade().unwrap().set_rc_scanning(false); });
    }

    fn register_save_callback(app: &AppWindow, store: &Rc<RefCell<DeviceStore>>) {
        let app_weak = app.as_weak();
        let store    = store.clone();
        app.on_rc_device_save(move |id, name, type_str| {
            let app   = app_weak.upgrade().unwrap();
            let dtype = DeviceType::from_str(type_str.as_str());
            let new_remote = Self::apply_save(&app, &store, id, &name, dtype);
            Self::sync_devices_to_ui(&app, &store.borrow());
            Self::start_binding_if_remote(&app, new_remote);
        });
    }

    /// Применяет сохранение: добавляет или обновляет устройство.
    /// Возвращает `Some((id, name))` если добавлен новый Remote.
    fn apply_save(
        app:   &AppWindow,
        store: &Rc<RefCell<DeviceStore>>,
        id:    i32,
        name:  &slint::SharedString,
        dtype: DeviceType,
    ) -> Option<(u32, String)> {
        let mut st = store.borrow_mut();
        if id == 0 {
            st.add(name.as_str(), dtype,
                   app.get_rc_code_hex().as_str(),
                   app.get_rc_protocol().as_str(),
                   app.get_rc_bit_count() as u8)
                .filter(|_| dtype == DeviceType::Remote)
                .map(|d| (d.id(), d.name().to_owned()))
        } else {
            if let Some(dev) = st.devices().iter().find(|d| d.id() == id as u32).cloned() {
                st.update(dev.with_name(name.as_str()).with_type(dtype));
            }
            None
        }
    }

    fn start_binding_if_remote(app: &AppWindow, new_remote: Option<(u32, String)>) {
        if let Some((device_id, device_name)) = new_remote {
            for i in 0..4 { Self::set_binding_btn_code(app, i, "".into()); }
            app.set_rc_binding_device_id(device_id as i32);
            app.set_rc_binding_device_name(device_name.into());
            app.set_rc_binding_button_idx(0);
            app.set_rc_binding_active(true);
        }
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

    fn register_binding_load_codes(app: &AppWindow, store: &Rc<RefCell<DeviceStore>>) {
        let app_weak = app.as_weak();
        let store    = store.clone();
        app.on_rc_binding_load_codes(move |device_id| {
            let app = app_weak.upgrade().unwrap();
            let st  = store.borrow();
            if let Some(dev) = st.devices().iter().find(|d| d.id() == device_id as u32) {
                Self::populate_button_codes(&app, dev);
            }
        });
    }

    fn populate_button_codes(app: &AppWindow, dev: &crate::rc_devices::RfDevice) {
        for i in 0..4 {
            Self::set_binding_btn_code(app, i, dev.button(i).unwrap_or("").into());
        }
    }

    fn set_binding_btn_code(app: &AppWindow, idx: usize, code: slint::SharedString) {
        match idx {
            0 => app.set_rc_binding_btn_code_0(code),
            1 => app.set_rc_binding_btn_code_1(code),
            2 => app.set_rc_binding_btn_code_2(code),
            3 => app.set_rc_binding_btn_code_3(code),
            _ => {}
        }
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

// ── RF code → ControlCmd ──────────────────────────────────────────────────────

fn code_to_cmd(store: &DeviceStore, code: &str) -> Option<ControlCmd> {
    for device in store.devices() {
        if device.device_type() != DeviceType::Remote { continue; }
        for idx in 0..4 {
            if device.button(idx) == Some(code) {
                return Some(match idx {
                    0 => ControlCmd::Arm,
                    1 => ControlCmd::Disarm,
                    2 => ControlCmd::Silent,
                    _ => ControlCmd::Alarm,
                });
            }
        }
    }
    None
}
