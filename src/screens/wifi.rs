use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;
use slint::ComponentHandle;
use crate::{AppWindow, NetworkInfo};
use crate::wifi::{ScannedNetwork, WifiCmd, WifiEvent, WifiWorker};

pub struct WifiScreenHandler {
    worker:       WifiWorker,
    app:          slint::Weak<AppWindow>,
    wifi_notify:  Option<SyncSender<bool>>,
}

impl WifiScreenHandler {
    pub fn new(app: &AppWindow, worker: WifiWorker, wifi_notify: Option<SyncSender<bool>>) -> Self {
        Self::register_scan(app, worker.cmd_sender());
        Self::register_connect(app, worker.cmd_sender());
        Self::register_cancel(app, worker.cancel_flag());
        Self::register_disconnect(app, worker.cmd_sender());
        Self::register_password_delete_last(app);
        Self { worker, app: app.as_weak(), wifi_notify }
    }

    pub fn poll(&self) {
        let Some(app) = self.app.upgrade() else { return };
        while let Some(event) = self.worker.try_recv() {
            let notify = match &event {
                WifiEvent::Connected { .. } => Some(true),
                WifiEvent::Disconnected     => Some(false),
                WifiEvent::ConnectError(_)  => Some(false),
                _                          => None,
            };
            handle_event(&app, event);
            if let (Some(state), Some(tx)) = (notify, &self.wifi_notify) {
                let _ = tx.try_send(state);
            }
        }
    }

    fn register_scan(app: &AppWindow, tx: SyncSender<WifiCmd>) {
        let app_weak = app.as_weak();
        app.on_wifi_scan_requested(move || {
            let app = app_weak.upgrade().unwrap();
            app.set_wifi_scanning(true);
            app.set_wifi_status("Сканирование...".into());
            send_cmd(&tx, WifiCmd::Scan);
        });
    }

    fn register_connect(app: &AppWindow, tx: SyncSender<WifiCmd>) {
        let app_weak = app.as_weak();
        app.on_wifi_connect_requested(move |ssid, password| {
            let app = app_weak.upgrade().unwrap();
            app.set_wifi_connecting(true);
            app.set_wifi_status(format!("Подключение к {ssid}...").into());
            send_cmd(&tx, WifiCmd::Connect { ssid: ssid.into(), password: password.into() });
        });
    }

    fn register_cancel(app: &AppWindow, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let app_weak = app.as_weak();
        app.on_wifi_cancel_connect(move || {
            cancel.store(true, Ordering::Relaxed);
            app_weak.upgrade().unwrap().set_wifi_status("Отмена...".into());
        });
    }

    fn register_disconnect(app: &AppWindow, tx: SyncSender<WifiCmd>) {
        let app_weak = app.as_weak();
        app.on_wifi_disconnect(move || {
            let app  = app_weak.upgrade().unwrap();
            let ssid = app.get_wifi_connected_ssid().to_string();
            app.set_wifi_status("Отключение...".into());
            send_cmd(&tx, WifiCmd::Disconnect { forget_ssid: Some(ssid) });
        });
    }

    fn register_password_delete_last(app: &AppWindow) {
        let app_weak = app.as_weak();
        app.on_wifi_password_delete_last(move || {
            let app = app_weak.upgrade().unwrap();
            let cur: String = app.get_wifi_password().into();
            app.set_wifi_password(super::delete_last_char(&cur).into());
        });
    }
}

// ── Event handlers ────────────────────────────────────────────────────────────

fn handle_event(app: &AppWindow, event: WifiEvent) {
    match event {
        WifiEvent::Ready                              => handle_ready(app),
        WifiEvent::ScanResult(nets)                  => handle_scan_result(app, nets),
        WifiEvent::ScanError(e)                      => handle_scan_error(app, &e),
        WifiEvent::Connecting { ssid, attempt, max } => handle_connecting(app, &ssid, attempt, max),
        WifiEvent::Connected { ssid, ip }            => handle_connected(app, &ssid, &ip),
        WifiEvent::ConnectError(e)                   => handle_connect_error(app, &e),
        WifiEvent::Disconnected                      => handle_disconnected(app),
    }
}

fn handle_ready(app: &AppWindow) {
    app.set_wifi_ready(true);
    app.set_wifi_status("Нажмите Сканировать".into());
}

fn handle_scan_result(app: &AppWindow, nets: Vec<ScannedNetwork>) {
    let count = nets.len();
    let model = Rc::new(slint::VecModel::from(
        nets.iter()
            .map(|n| NetworkInfo { ssid: n.ssid.as_str().into(), rssi: n.rssi, secured: n.secured })
            .collect::<Vec<_>>(),
    ));
    app.set_wifi_networks(model.into());
    app.set_wifi_status(format!("Найдено сетей: {count}").into());
    app.set_wifi_scanning(false);
}

fn handle_scan_error(app: &AppWindow, e: &str) {
    app.set_wifi_status(format!("ERR: {e}").into());
    app.set_wifi_scanning(false);
}

fn handle_connecting(app: &AppWindow, ssid: &str, attempt: u8, max: u8) {
    app.set_wifi_connecting(true);
    app.set_wifi_status(format!("Подключение к {ssid} ({attempt}/{max})...").into());
}

fn handle_connected(app: &AppWindow, ssid: &str, ip: &str) {
    let status = if ip.is_empty() { format!("Подключено: {ssid}") }
                 else             { format!("Подключено: {ssid} • {ip}") };
    app.set_wifi_status(status.into());
    app.set_wifi_is_connected(true);
    app.set_wifi_connected_ssid(ssid.into());
    app.set_wifi_connected_ip(ip.into());
    app.set_wifi_connecting(false);
}

fn handle_connect_error(app: &AppWindow, e: &str) {
    let msg = if e.starts_with("Отменено") { "Подключение отменено".into() }
              else                          { format!("Ошибка: {e}") };
    app.set_wifi_status(msg.into());
    app.set_wifi_is_connected(false);
    app.set_wifi_connected_ip("".into());
    app.set_wifi_connecting(false);
}

fn handle_disconnected(app: &AppWindow) {
    app.set_wifi_is_connected(false);
    app.set_wifi_connected_ip("".into());
    app.set_wifi_connecting(false);
    app.set_wifi_status("Отключено".into());
}

fn send_cmd(tx: &SyncSender<WifiCmd>, cmd: WifiCmd) {
    if tx.try_send(cmd).is_err() {
        log::warn!("wifi cmd channel full — dropping command");
    }
}
