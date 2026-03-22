use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::modem::Modem,
    nvs::{EspDefaultNvsPartition, EspNvs},
    wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::time::Duration;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct ScannedNetwork {
    pub ssid:    String,
    pub rssi:    i32,
    pub secured: bool,
}

pub enum WifiCmd {
    Scan,
    Connect { ssid: String, password: String },
    /// Отключиться от сети.  Если `forget_ssid` задан — стирает сохранённые
    /// учётные данные для этой SSID, отключая автоподключение после перезагрузки.
    Disconnect { forget_ssid: Option<String> },
}

pub enum WifiEvent {
    Ready,
    ScanResult(Vec<ScannedNetwork>),
    ScanError(String),
    Connecting { ssid: String, attempt: u8, max: u8 },
    Connected { ssid: String, ip: String },
    ConnectError(String),
    Disconnected,
}

// ── WifiWorker ────────────────────────────────────────────────────────────────

/// Handle to the WiFi background thread.
/// Commands are sent via [`WifiWorker::send`]; results are received via
/// [`WifiWorker::try_recv`] (non-blocking — call from the render loop).
pub struct WifiWorker {
    cmd_tx:      SyncSender<WifiCmd>,
    event_rx:    Receiver<WifiEvent>,
    cancel_flag: Arc<AtomicBool>,
}

impl WifiWorker {
    /// Spawn the WiFi worker on a dedicated thread (Core 1 via FreeRTOS).
    /// All blocking WiFi operations run there; the render loop is never stalled.
    pub fn spawn(
        modem:   Modem,
        sysloop: EspSystemEventLoop,
        nvs:     EspDefaultNvsPartition,
    ) -> Self {
        let (cmd_tx,   cmd_rx)   = std::sync::mpsc::sync_channel::<WifiCmd>(4);
        let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<WifiEvent>(8);
        let cancel_flag = Arc::new(AtomicBool::new(false));
        let cancel_worker = cancel_flag.clone();

        // SAFETY: Modem is a singleton peripheral that lives for the entire
        // program.  We move ownership to the worker thread and never access it
        // from any other thread.
        let modem: Modem<'static> = unsafe { core::mem::transmute(modem) };

        std::thread::Builder::new()
            .stack_size(16384)
            .name("wifi_worker".to_string())
            .spawn(move || worker_loop(modem, sysloop, nvs, cmd_rx, event_tx, cancel_worker))
            .expect("wifi worker thread spawn failed");

        Self { cmd_tx, event_rx, cancel_flag }
    }

    /// Try to receive the next event from the worker without blocking.
    pub fn try_recv(&self) -> Option<WifiEvent> {
        self.event_rx.try_recv().ok()
    }

    /// Clone the sender so closures can dispatch commands independently.
    pub fn cmd_sender(&self) -> SyncSender<WifiCmd> {
        self.cmd_tx.clone()
    }

    /// Get a clone of the cancel flag (for use in UI callbacks).
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel_flag.clone()
    }
}

// ── Worker loop (runs on the dedicated thread) ────────────────────────────────

fn worker_loop(
    modem:    Modem<'static>,
    sysloop:  EspSystemEventLoop,
    nvs:      EspDefaultNvsPartition,
    cmd_rx:   Receiver<WifiCmd>,
    event_tx: SyncSender<WifiEvent>,
    cancel:   Arc<AtomicBool>,
) {
    let nvs_creds = nvs.clone();

    let esp_wifi = match EspWifi::new(modem, sysloop.clone(), Some(nvs)) {
        Ok(w)  => w,
        Err(e) => {
            let _ = event_tx.send(WifiEvent::ScanError(format!("WiFi init: {e}")));
            return;
        }
    };
    let mut wifi = match BlockingWifi::wrap(esp_wifi, sysloop) {
        Ok(w)  => w,
        Err(e) => {
            let _ = event_tx.send(WifiEvent::ScanError(format!("WiFi wrap: {e}")));
            return;
        }
    };

    let _ = event_tx.send(WifiEvent::Ready);

    // Auto-connect with stored credentials from previous session
    let mut is_connected = false;
    for (ssid, password) in load_all_credentials(&nvs_creds) {
        log::info!("Trying stored credentials for '{ssid}'…");
        if do_connect(&mut wifi, &ssid, &password, &cancel, &event_tx) {
            is_connected = true;
            break;
        }
    }

    loop {
        // When connected — poll every 10 s to detect drops and auto-reconnect.
        // When idle — block until the UI sends a command.
        let cmd = if is_connected {
            match cmd_rx.recv_timeout(Duration::from_secs(10)) {
                Ok(cmd) => Some(cmd),
                Err(RecvTimeoutError::Timeout) => {
                    if !wifi.is_connected().unwrap_or(true) {
                        log::warn!("WiFi connection lost, attempting reconnect…");
                        is_connected = false;
                        let _ = event_tx.send(WifiEvent::Disconnected);
                        for (ssid, pwd) in load_all_credentials(&nvs_creds) {
                            if do_connect(&mut wifi, &ssid, &pwd, &cancel, &event_tx) {
                                is_connected = true;
                                break;
                            }
                        }
                    }
                    None
                }
                Err(RecvTimeoutError::Disconnected) => break,
            }
        } else {
            match cmd_rx.recv() {
                Ok(cmd) => Some(cmd),
                Err(_) => break,
            }
        };

        let Some(cmd) = cmd else { continue };

        match cmd {
            WifiCmd::Scan => {
                if !wifi.is_started().unwrap_or(false) {
                    let _ = wifi.set_configuration(&Configuration::Client(
                        ClientConfiguration::default(),
                    ));
                    let _ = wifi.start();
                }
                match wifi.scan() {
                    Ok(aps) => {
                        // Deduplicate by SSID — keep the AP with the strongest signal
                        let mut best: HashMap<String, ScannedNetwork> = HashMap::new();
                        for ap in aps {
                            let ssid = ap.ssid.as_str().to_owned();
                            if ssid.is_empty() { continue; }
                            let net = ScannedNetwork {
                                ssid:    ssid.clone(),
                                rssi:    ap.signal_strength as i32,
                                secured: ap.auth_method
                                    .map_or(false, |m| m != AuthMethod::None),
                            };
                            best.entry(ssid)
                                .and_modify(|e| { if net.rssi > e.rssi { *e = net.clone(); } })
                                .or_insert(net);
                        }
                        let mut nets: Vec<ScannedNetwork> = best.into_values().collect();
                        nets.sort_by(|a, b| b.rssi.cmp(&a.rssi));
                        let _ = event_tx.send(WifiEvent::ScanResult(nets));
                    }
                    Err(e) => { let _ = event_tx.send(WifiEvent::ScanError(e.to_string())); }
                }
            }

            WifiCmd::Connect { ssid, password } => {
                is_connected = do_connect(&mut wifi, &ssid, &password, &cancel, &event_tx);
                if is_connected {
                    save_credentials(&nvs_creds, &ssid, &password);
                }
            }

            WifiCmd::Disconnect { forget_ssid } => {
                is_connected = false;
                let _ = wifi.disconnect();
                if let Some(ssid) = forget_ssid {
                    forget_credential(&nvs_creds, &ssid);
                }
                let _ = event_tx.send(WifiEvent::Disconnected);
            }
        }
    }
}

// ── Connect with retries ─────────────────────────────────────────────────────

fn do_connect(
    wifi:     &mut BlockingWifi<EspWifi<'static>>,
    ssid:     &str,
    password: &str,
    cancel:   &AtomicBool,
    event_tx: &SyncSender<WifiEvent>,
) -> bool {
    cancel.store(false, Ordering::Relaxed);

    const MAX: u8 = 3;
    let mut connected = false;
    let mut cancelled = false;
    let mut last_err  = String::new();

    for attempt in 1..=MAX {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        let _ = event_tx.send(WifiEvent::Connecting {
            ssid: ssid.to_owned(), attempt, max: MAX,
        });
        let _ = wifi.set_configuration(&Configuration::Client(
            ClientConfiguration {
                ssid:     ssid.try_into().unwrap_or_default(),
                password: password.try_into().unwrap_or_default(),
                ..Default::default()
            },
        ));
        if !wifi.is_started().unwrap_or(false) {
            let _ = wifi.start();
        }
        match wifi.connect() {
            Ok(()) if cancel.load(Ordering::Relaxed) => {
                cancelled = true;
                let _ = wifi.disconnect();
                break;
            }
            Ok(()) if wifi.wait_netif_up().is_ok() => {
                if cancel.load(Ordering::Relaxed) {
                    cancelled = true;
                    let _ = wifi.disconnect();
                    break;
                }
                connected = true;
                break;
            }
            Ok(()) => { last_err = friendly_error("netif did not come up"); }
            Err(e) => { last_err = friendly_error(&e.to_string()); }
        }
        let _ = wifi.disconnect();
    }

    if cancelled {
        let _ = wifi.disconnect();
        let _ = event_tx.send(WifiEvent::ConnectError(
            "Отменено пользователем".to_owned(),
        ));
        false
    } else if connected {
        let ip = wifi.wifi().sta_netif()
            .get_ip_info()
            .map(|info| format!("{}", info.ip))
            .unwrap_or_default();
        let ip = if ip == "0.0.0.0" { String::new() } else { ip };
        let _ = event_tx.send(WifiEvent::Connected { ssid: ssid.to_owned(), ip });
        true
    } else {
        let _ = event_tx.send(WifiEvent::ConnectError(last_err));
        false
    }
}

// ── Human-readable WiFi errors ───────────────────────────────────────────────

fn friendly_error(raw: &str) -> String {
    if raw.contains("WIFI_REASON_AUTH_FAIL") || raw.contains("AUTH") {
        "Неверный пароль".to_owned()
    } else if raw.contains("WIFI_REASON_NO_AP_FOUND") || raw.contains("NO_AP") {
        "Сеть не найдена".to_owned()
    } else if raw.contains("WIFI_REASON_ASSOC_TOOMANY") {
        "Слишком много подключений к точке доступа".to_owned()
    } else if raw.contains("TIMEOUT") || raw.contains("timeout") {
        "Таймаут подключения".to_owned()
    } else if raw == "netif did not come up" {
        "Не удалось получить IP-адрес".to_owned()
    } else if raw.contains("WIFI_REASON_BEACON_TIMEOUT") {
        "Сеть недоступна".to_owned()
    } else {
        raw.to_owned()
    }
}

// ── NVS credential storage ───────────────────────────────────────────────────

const MAX_STORED: usize = 5;

fn load_all_credentials(nvs: &EspDefaultNvsPartition) -> Vec<(String, String)> {
    let nvs_handle = match EspNvs::new(nvs.clone(), "wifi_cred", true) {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };

    if let Some(count) = nvs_handle.get_u8("count").ok().flatten() {
        let n = (count as usize).min(MAX_STORED);
        let mut creds = Vec::with_capacity(n);
        for i in 0..n {
            let sk = format!("s{i}");
            let pk = format!("p{i}");
            let mut sbuf = [0u8; 64];
            let mut pbuf = [0u8; 128];
            if let (Some(s), Some(p)) = (
                nvs_handle.get_str(&sk, &mut sbuf).ok().flatten(),
                nvs_handle.get_str(&pk, &mut pbuf).ok().flatten(),
            ) {
                creds.push((s.to_string(), p.to_string()));
            }
        }
        return creds;
    }

    // Backward compatibility: read old single-credential keys
    let mut sbuf = [0u8; 64];
    let mut pbuf = [0u8; 128];
    if let (Some(s), Some(p)) = (
        nvs_handle.get_str("ssid", &mut sbuf).ok().flatten(),
        nvs_handle.get_str("pwd", &mut pbuf).ok().flatten(),
    ) {
        if !s.is_empty() {
            return vec![(s.to_string(), p.to_string())];
        }
    }
    Vec::new()
}

fn save_credentials(nvs: &EspDefaultNvsPartition, ssid: &str, password: &str) {
    let mut existing = load_all_credentials(nvs);
    existing.retain(|(s, _)| s != ssid);
    existing.insert(0, (ssid.to_owned(), password.to_owned()));
    existing.truncate(MAX_STORED);

    match EspNvs::new(nvs.clone(), "wifi_cred", true) {
        Ok(h) => {
            let _ = h.set_u8("count", existing.len() as u8);
            for (i, (s, p)) in existing.iter().enumerate() {
                let _ = h.set_str(&format!("s{i}"), s);
                let _ = h.set_str(&format!("p{i}"), p);
            }
            log::info!("WiFi credentials saved for '{ssid}' ({} total)", existing.len());
        }
        Err(e) => log::warn!("Failed to open NVS for credential storage: {e}"),
    }
}

fn forget_credential(nvs: &EspDefaultNvsPartition, ssid: &str) {
    let mut existing = load_all_credentials(nvs);
    let before = existing.len();
    existing.retain(|(s, _)| s != ssid);
    if existing.len() == before {
        return; // SSID не был сохранён
    }
    match EspNvs::new(nvs.clone(), "wifi_cred", true) {
        Ok(h) => {
            let _ = h.set_u8("count", existing.len() as u8);
            for (i, (s, p)) in existing.iter().enumerate() {
                let _ = h.set_str(&format!("s{i}"), s);
                let _ = h.set_str(&format!("p{i}"), p);
            }
            log::info!("WiFi: забыта сеть '{ssid}' (осталось {})", existing.len());
        }
        Err(e) => log::warn!("Failed to open NVS to forget credential: {e}"),
    }
}
