use std::rc::Rc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;
use slint::ComponentHandle;
use crate::{AppWindow, NetworkInfo};
use crate::wifi::{WifiCmd, WifiEvent, WifiWorker};

/// Owns all WiFi screen state and registers its callbacks on `AppWindow`.
///
/// Call [`WifiScreenHandler::poll`] once per render frame to dispatch
/// incoming WiFi events to the UI — it never blocks.
pub struct WifiScreenHandler {
    worker:  WifiWorker,
    app:     slint::Weak<AppWindow>,
}

impl WifiScreenHandler {
    pub fn new(app: &AppWindow, worker: WifiWorker) -> Self {
        // ── Scan ──────────────────────────────────────────────────
        {
            let tx       = worker.cmd_sender();
            let app_weak = app.as_weak();
            app.on_wifi_scan_requested(move || {
                let app = app_weak.upgrade().unwrap();
                app.set_wifi_scanning(true);
                app.set_wifi_status("Сканирование...".into());
                send_cmd(&tx, WifiCmd::Scan);
            });
        }

        // ── Connect ───────────────────────────────────────────────
        {
            let tx       = worker.cmd_sender();
            let app_weak = app.as_weak();
            app.on_wifi_connect_requested(move |ssid, password| {
                let app = app_weak.upgrade().unwrap();
                app.set_wifi_connecting(true);
                app.set_wifi_status(format!("Подключение к {ssid}...").into());
                send_cmd(&tx, WifiCmd::Connect {
                    ssid:     ssid.into(),
                    password: password.into(),
                });
            });
        }

        // ── Cancel connect (во время попытки подключения) ─────────
        {
            let cancel = worker.cancel_flag();
            let app_weak = app.as_weak();
            app.on_wifi_cancel_connect(move || {
                cancel.store(true, Ordering::Relaxed);
                let app = app_weak.upgrade().unwrap();
                app.set_wifi_status("Отмена...".into());
            });
        }

        // ── Disconnect (ручное отключение) ────────────────────────
        {
            let tx = worker.cmd_sender();
            let app_weak = app.as_weak();
            app.on_wifi_disconnect(move || {
                let app  = app_weak.upgrade().unwrap();
                let ssid = app.get_wifi_connected_ssid().to_string();
                app.set_wifi_status("Отключение...".into());
                send_cmd(&tx, WifiCmd::Disconnect { forget_ssid: Some(ssid) });
            });
        }

        // ── Password delete-last ──────────────────────────────────
        {
            let app_weak = app.as_weak();
            app.on_wifi_password_delete_last(move || {
                let app = app_weak.upgrade().unwrap();
                let cur: String = app.get_wifi_password().into();
                let trimmed: String =
                    cur.chars().take(cur.chars().count().saturating_sub(1)).collect();
                app.set_wifi_password(trimmed.into());
            });
        }

        Self { worker, app: app.as_weak() }
    }

    /// Drain all pending WiFi events and update the UI.  Non-blocking.
    pub fn poll(&self) {
        let Some(app) = self.app.upgrade() else { return };

        while let Some(event) = self.worker.try_recv() {
            match event {
                WifiEvent::Ready => {
                    app.set_wifi_ready(true);
                    app.set_wifi_status("Нажмите Сканировать".into());
                }
                WifiEvent::ScanResult(nets) => {
                    let count = nets.len();
                    let model = Rc::new(slint::VecModel::from(
                        nets.iter()
                            .map(|n| NetworkInfo {
                                ssid:    n.ssid.as_str().into(),
                                rssi:    n.rssi,
                                secured: n.secured,
                            })
                            .collect::<Vec<_>>(),
                    ));
                    app.set_wifi_networks(model.into());
                    app.set_wifi_status(format!("Найдено сетей: {count}").into());
                    app.set_wifi_scanning(false);
                }
                WifiEvent::ScanError(e) => {
                    app.set_wifi_status(format!("ERR: {e}").into());
                    app.set_wifi_scanning(false);
                }
                WifiEvent::Connecting { ssid, attempt, max } => {
                    app.set_wifi_connecting(true);
                    app.set_wifi_status(
                        format!("Подключение к {ssid} ({attempt}/{max})...").into(),
                    );
                }
                WifiEvent::Connected { ssid, ip } => {
                    let status = if ip.is_empty() {
                        format!("Подключено: {ssid}")
                    } else {
                        format!("Подключено: {ssid} • {ip}")
                    };
                    app.set_wifi_status(status.into());
                    app.set_wifi_is_connected(true);
                    app.set_wifi_connected_ssid(ssid.as_str().into());
                    app.set_wifi_connected_ip(ip.as_str().into());
                    app.set_wifi_connecting(false);
                }
                WifiEvent::ConnectError(e) => {
                    let msg = if e.starts_with("Отменено") {
                        "Подключение отменено".into()
                    } else {
                        format!("Ошибка: {e}")
                    };
                    app.set_wifi_status(msg.into());
                    app.set_wifi_is_connected(false);
                    app.set_wifi_connected_ip("".into());
                    app.set_wifi_connecting(false);
                }
                WifiEvent::Disconnected => {
                    app.set_wifi_is_connected(false);
                    app.set_wifi_connected_ip("".into());
                    app.set_wifi_connecting(false);
                    app.set_wifi_status("Отключено".into());
                }
            }
        }
    }
}

fn send_cmd(tx: &SyncSender<WifiCmd>, cmd: WifiCmd) {
    if tx.try_send(cmd).is_err() {
        log::warn!("wifi cmd channel full — dropping command");
    }
}
