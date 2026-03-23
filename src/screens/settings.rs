use slint::ComponentHandle;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use crate::AppWindow;

const NS: &str = "settings";

pub struct SettingsScreenHandler {
    _app: slint::Weak<AppWindow>,
}

impl SettingsScreenHandler {
    pub fn new(app: &AppWindow, nvs: &EspDefaultNvsPartition) -> Self {
        app.set_settings_pin(load_pin(nvs).into());
        app.set_settings_arm_timeout(load_timeout(nvs) as i32);
        Self::register_save_pin(app, nvs.clone());
        Self::register_save_timeout(app, nvs.clone());
        Self::register_delete_last(app);
        Self { _app: app.as_weak() }
    }

    fn register_save_timeout(app: &AppWindow, nvs: EspDefaultNvsPartition) {
        let app_weak = app.as_weak();
        app.on_settings_save_timeout(move |mins| {
            let app = app_weak.upgrade().unwrap();
            app.set_settings_arm_timeout(mins);
            save_timeout(&nvs, mins as u32);
            log::info!("Settings: arm timeout = {} min", mins);
        });
    }

    fn register_save_pin(app: &AppWindow, nvs: EspDefaultNvsPartition) {
        let app_weak = app.as_weak();
        app.on_settings_save_pin(move |pin| {
            let clean = sanitize_pin(pin.as_str());
            let app   = app_weak.upgrade().unwrap();
            app.set_settings_pin(clean.as_str().into());
            save_pin(&nvs, &clean);
            log::info!("Settings: PIN {}", if clean.is_empty() { "cleared" } else { "saved" });
        });
    }

    fn register_delete_last(app: &AppWindow) {
        let app_weak = app.as_weak();
        app.on_settings_editing_delete_last(move || {
            let app  = app_weak.upgrade().unwrap();
            let cur  = app.get_settings_editing_text().to_string();
            let next = cur.chars().take(cur.chars().count().saturating_sub(1)).collect::<String>();
            app.set_settings_editing_text(next.into());
        });
    }
}

// ── NVS helpers ───────────────────────────────────────────────────────────────

fn load_pin(nvs: &EspDefaultNvsPartition) -> String {
    let Ok(h) = EspNvs::new(nvs.clone(), NS, true) else { return String::new() };
    let mut buf = [0u8; 16];
    h.get_str("pin", &mut buf).ok().flatten()
        .map(str::to_owned)
        .unwrap_or_default()
}

fn save_pin(nvs: &EspDefaultNvsPartition, pin: &str) {
    let Ok(h) = EspNvs::new(nvs.clone(), NS, true) else { return };
    let _ = h.set_str("pin", pin);
}

fn load_timeout(nvs: &EspDefaultNvsPartition) -> u32 {
    let Ok(h) = EspNvs::new(nvs.clone(), NS, true) else { return 3 };
    h.get_u32("arm_timeout").ok().flatten().unwrap_or(3).clamp(1, 60)
}

fn save_timeout(nvs: &EspDefaultNvsPartition, mins: u32) {
    let Ok(h) = EspNvs::new(nvs.clone(), NS, true) else { return };
    let _ = h.set_u32("arm_timeout", mins);
}

fn sanitize_pin(pin: &str) -> String {
    pin.chars().filter(|c| c.is_ascii_digit()).take(8).collect()
}
