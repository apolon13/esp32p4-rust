use slint::ComponentHandle;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use std::{rc::Rc, sync::{Arc, Mutex}};
use crate::{AppWindow, gsm::{GsmMonitor, SimStatus}};

const NS:     &str   = "sim";
const MAX_WL: usize  = 10;

pub struct SimScreenHandler {
    app:    slint::Weak<AppWindow>,
    status: Arc<Mutex<SimStatus>>,
}

impl SimScreenHandler {
    pub fn new(app: &AppWindow, gsm: &GsmMonitor, nvs: EspDefaultNvsPartition) -> Self {
        let saved = load_whitelist(&nvs);
        *gsm.whitelist.lock().unwrap() = saved.clone();
        set_whitelist_ui(app, &saved);

        let handler = Self {
            app:    app.as_weak(),
            status: Arc::clone(&gsm.status),
        };

        // Добавить номер
        {
            let wl  = Arc::clone(&gsm.whitelist);
            let nvs = nvs.clone();
            let aw  = app.as_weak();
            app.on_sim_whitelist_add(move |num| {
                let num = num.trim().to_string();
                if num.is_empty() { return; }
                let mut guard = wl.lock().unwrap();
                if guard.len() >= MAX_WL || guard.contains(&num) { return; }
                guard.push(num);
                save_whitelist(&nvs, &guard);
                let snap = guard.clone();
                drop(guard);
                if let Some(a) = aw.upgrade() { set_whitelist_ui(&a, &snap); }
            });
        }

        // Удалить номер
        {
            let wl  = Arc::clone(&gsm.whitelist);
            let nvs = nvs.clone();
            let aw  = app.as_weak();
            app.on_sim_whitelist_remove(move |idx| {
                let mut guard = wl.lock().unwrap();
                if (idx as usize) < guard.len() {
                    guard.remove(idx as usize);
                    save_whitelist(&nvs, &guard);
                }
                let snap = guard.clone();
                drop(guard);
                if let Some(a) = aw.upgrade() { set_whitelist_ui(&a, &snap); }
            });
        }

        // DEL для поля ввода
        {
            let aw = app.as_weak();
            app.on_sim_editing_delete_last(move || {
                if let Some(a) = aw.upgrade() {
                    let cur = a.get_sim_editing_text().to_string();
                    a.set_sim_editing_text(super::delete_last_char(&cur).into());
                }
            });
        }

        handler
    }

    pub fn poll(&self) {
        let Some(app) = self.app.upgrade() else { return };
        if let Ok(s) = self.status.lock() {
            app.set_sim_msisdn(s.msisdn.as_str().into());
            app.set_sim_signal(s.signal.as_str().into());
            app.set_sim_reg(s.reg.as_str().into());
            app.set_sim_cpin(s.cpin.as_str().into());
        }
    }
}

// ── UI helper ─────────────────────────────────────────────────────────────────

fn set_whitelist_ui(app: &AppWindow, list: &[String]) {
    let items: Vec<slint::SharedString> =
        list.iter().map(|s| slint::SharedString::from(s.as_str())).collect();
    let model = Rc::new(slint::VecModel::from(items));
    app.set_sim_whitelist(model.into());
}

// ── NVS helpers ───────────────────────────────────────────────────────────────

fn load_whitelist(nvs: &EspDefaultNvsPartition) -> Vec<String> {
    let Ok(h) = EspNvs::new(nvs.clone(), NS, true) else { return vec![] };
    let count = h.get_u8("wl_n").ok().flatten().unwrap_or(0) as usize;
    let mut result = Vec::new();
    let mut buf = [0u8; 24];
    for i in 0..count.min(MAX_WL) {
        let key = format!("wl{i}");
        if let Ok(Some(s)) = h.get_str(&key, &mut buf) {
            result.push(s.to_owned());
        }
    }
    result
}

fn save_whitelist(nvs: &EspDefaultNvsPartition, list: &[String]) {
    let Ok(h) = EspNvs::new(nvs.clone(), NS, true) else { return };
    let _ = h.set_u8("wl_n", list.len() as u8);
    for (i, num) in list.iter().enumerate() {
        let key = format!("wl{i}");
        let _ = h.set_str(&key, num);
    }
}
