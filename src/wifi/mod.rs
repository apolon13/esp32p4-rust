use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::modem::Modem,
    nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault},
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

pub struct WifiWorker {
    cmd_tx:      SyncSender<WifiCmd>,
    event_rx:    Receiver<WifiEvent>,
    cancel_flag: Arc<AtomicBool>,
}

impl WifiWorker {
    pub fn spawn(modem: Modem, sysloop: EspSystemEventLoop, nvs: EspDefaultNvsPartition) -> Self {
        let (cmd_tx,   cmd_rx)   = std::sync::mpsc::sync_channel::<WifiCmd>(4);
        let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<WifiEvent>(8);
        let cancel_flag  = Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel_flag.clone();
        let modem: Modem<'static> = unsafe { core::mem::transmute(modem) };
        spawn_worker_thread(modem, sysloop, nvs, cmd_rx, event_tx, cancel_clone);
        Self { cmd_tx, event_rx, cancel_flag }
    }

    pub fn try_recv(&self) -> Option<WifiEvent> { self.event_rx.try_recv().ok() }
    pub fn cmd_sender(&self) -> SyncSender<WifiCmd> { self.cmd_tx.clone() }
    pub fn cancel_flag(&self) -> Arc<AtomicBool> { self.cancel_flag.clone() }
}

// ── Worker thread ─────────────────────────────────────────────────────────────

fn spawn_worker_thread(
    modem:    Modem<'static>,
    sysloop:  EspSystemEventLoop,
    nvs:      EspDefaultNvsPartition,
    cmd_rx:   Receiver<WifiCmd>,
    event_tx: SyncSender<WifiEvent>,
    cancel:   Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .stack_size(16384)
        .name("wifi_worker".to_string())
        .spawn(move || worker_loop(modem, sysloop, nvs, cmd_rx, event_tx, cancel))
        .expect("wifi worker thread spawn failed");
}

enum PollResult { Cmd(WifiCmd), Timeout, Disconnected }

fn worker_loop(
    modem:    Modem<'static>,
    sysloop:  EspSystemEventLoop,
    nvs:      EspDefaultNvsPartition,
    cmd_rx:   Receiver<WifiCmd>,
    event_tx: SyncSender<WifiEvent>,
    cancel:   Arc<AtomicBool>,
) {
    let nvs_creds = nvs.clone();
    let mut wifi = match init_wifi(modem, sysloop, nvs, &event_tx) { Some(w) => w, None => return };
    let _ = event_tx.send(WifiEvent::Ready);
    let mut connected = auto_connect(&mut wifi, &nvs_creds, &cancel, &event_tx);
    loop {
        match poll_cmd(&cmd_rx, connected) {
            PollResult::Disconnected => break,
            PollResult::Timeout      => connected = watchdog_reconnect(&mut wifi, &nvs_creds, &cancel, &event_tx),
            PollResult::Cmd(cmd)     => handle_cmd(&mut wifi, cmd, &nvs_creds, &cancel, &event_tx, &mut connected),
        }
    }
}

fn init_wifi(
    modem:    Modem<'static>,
    sysloop:  EspSystemEventLoop,
    nvs:      EspDefaultNvsPartition,
    event_tx: &SyncSender<WifiEvent>,
) -> Option<BlockingWifi<EspWifi<'static>>> {
    let esp_wifi = match EspWifi::new(modem, sysloop.clone(), Some(nvs)) {
        Ok(w)  => w,
        Err(e) => { let _ = event_tx.send(WifiEvent::ScanError(format!("WiFi init: {e}"))); return None; }
    };
    match BlockingWifi::wrap(esp_wifi, sysloop) {
        Ok(w)  => Some(w),
        Err(e) => { let _ = event_tx.send(WifiEvent::ScanError(format!("WiFi wrap: {e}"))); None }
    }
}

fn auto_connect(
    wifi:      &mut BlockingWifi<EspWifi<'static>>,
    nvs_creds: &EspDefaultNvsPartition,
    cancel:    &AtomicBool,
    event_tx:  &SyncSender<WifiEvent>,
) -> bool {
    for (ssid, password) in load_all_credentials(nvs_creds) {
        log::info!("Trying stored credentials for '{ssid}'…");
        if do_connect(wifi, &ssid, &password, cancel, event_tx) { return true; }
    }
    false
}

fn watchdog_reconnect(
    wifi:      &mut BlockingWifi<EspWifi<'static>>,
    nvs_creds: &EspDefaultNvsPartition,
    cancel:    &AtomicBool,
    event_tx:  &SyncSender<WifiEvent>,
) -> bool {
    if wifi.is_connected().unwrap_or(true) { return true; }
    log::warn!("WiFi connection lost, attempting reconnect…");
    let _ = event_tx.send(WifiEvent::Disconnected);
    auto_connect(wifi, nvs_creds, cancel, event_tx)
}

fn poll_cmd(cmd_rx: &Receiver<WifiCmd>, is_connected: bool) -> PollResult {
    if is_connected {
        match cmd_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(cmd) => PollResult::Cmd(cmd),
            Err(RecvTimeoutError::Timeout)      => PollResult::Timeout,
            Err(RecvTimeoutError::Disconnected) => PollResult::Disconnected,
        }
    } else {
        match cmd_rx.recv() {
            Ok(cmd) => PollResult::Cmd(cmd),
            Err(_)  => PollResult::Disconnected,
        }
    }
}

fn handle_cmd(
    wifi:      &mut BlockingWifi<EspWifi<'static>>,
    cmd:       WifiCmd,
    nvs_creds: &EspDefaultNvsPartition,
    cancel:    &AtomicBool,
    event_tx:  &SyncSender<WifiEvent>,
    connected: &mut bool,
) {
    match cmd {
        WifiCmd::Scan                          => do_scan(wifi, event_tx),
        WifiCmd::Connect { ssid, password }    => {
            *connected = do_connect(wifi, &ssid, &password, cancel, event_tx);
            if *connected { save_credentials(nvs_creds, &ssid, &password); }
        }
        WifiCmd::Disconnect { forget_ssid } => {
            *connected = false;
            let _ = wifi.disconnect();
            if let Some(ssid) = forget_ssid { forget_credential(nvs_creds, &ssid); }
            let _ = event_tx.send(WifiEvent::Disconnected);
        }
    }
}

// ── Scan ─────────────────────────────────────────────────────────────────────

fn do_scan(wifi: &mut BlockingWifi<EspWifi<'static>>, event_tx: &SyncSender<WifiEvent>) {
    if !wifi.is_started().unwrap_or(false) {
        let _ = wifi.set_configuration(&Configuration::Client(ClientConfiguration::default()));
        let _ = wifi.start();
    }
    match wifi.scan() {
        Ok(aps) => { let _ = event_tx.send(WifiEvent::ScanResult(deduplicate_aps(aps))); }
        Err(e)  => { let _ = event_tx.send(WifiEvent::ScanError(e.to_string())); }
    }
}

fn deduplicate_aps(aps: Vec<esp_idf_svc::wifi::AccessPointInfo>) -> Vec<ScannedNetwork> {
    let mut best: HashMap<String, ScannedNetwork> = HashMap::new();
    for ap in aps {
        let ssid = ap.ssid.as_str().to_owned();
        if ssid.is_empty() { continue; }
        let net = ScannedNetwork {
            ssid:    ssid.clone(),
            rssi:    ap.signal_strength as i32,
            secured: ap.auth_method.map_or(false, |m| m != AuthMethod::None),
        };
        best.entry(ssid).and_modify(|e| { if net.rssi > e.rssi { *e = net.clone(); } }).or_insert(net);
    }
    let mut nets: Vec<ScannedNetwork> = best.into_values().collect();
    nets.sort_by(|a, b| b.rssi.cmp(&a.rssi));
    nets
}

// ── Connect with retries ─────────────────────────────────────────────────────

enum AttemptResult { Connected, Cancelled, Failed(String) }

fn do_connect(
    wifi:     &mut BlockingWifi<EspWifi<'static>>,
    ssid:     &str,
    password: &str,
    cancel:   &AtomicBool,
    event_tx: &SyncSender<WifiEvent>,
) -> bool {
    cancel.store(false, Ordering::Relaxed);
    let (connected, cancelled, last_err) = connect_loop(wifi, ssid, password, cancel, event_tx);
    report_connect_result(wifi, ssid, connected, cancelled, &last_err, event_tx)
}

fn connect_loop(
    wifi:     &mut BlockingWifi<EspWifi<'static>>,
    ssid:     &str,
    password: &str,
    cancel:   &AtomicBool,
    event_tx: &SyncSender<WifiEvent>,
) -> (bool, bool, String) {
    const MAX: u8 = 3;
    let mut last_err = String::new();
    for attempt in 1..=MAX {
        if cancel.load(Ordering::Relaxed) { return (false, true, last_err); }
        let _ = event_tx.send(WifiEvent::Connecting { ssid: ssid.to_owned(), attempt, max: MAX });
        match attempt_once(wifi, ssid, password, cancel) {
            AttemptResult::Connected   => return (true, false, last_err),
            AttemptResult::Cancelled   => return (false, true, last_err),
            AttemptResult::Failed(err) => { let _ = wifi.disconnect(); last_err = err; }
        }
    }
    (false, false, last_err)
}

fn attempt_once(
    wifi:     &mut BlockingWifi<EspWifi<'static>>,
    ssid:     &str,
    password: &str,
    cancel:   &AtomicBool,
) -> AttemptResult {
    let _ = wifi.set_configuration(&Configuration::Client(ClientConfiguration {
        ssid:     ssid.try_into().unwrap_or_default(),
        password: password.try_into().unwrap_or_default(),
        ..Default::default()
    }));
    if !wifi.is_started().unwrap_or(false) { let _ = wifi.start(); }
    match wifi.connect() {
        Ok(()) if cancel.load(Ordering::Relaxed) => { let _ = wifi.disconnect(); AttemptResult::Cancelled }
        Ok(()) if wifi.wait_netif_up().is_ok() => {
            if cancel.load(Ordering::Relaxed) { let _ = wifi.disconnect(); return AttemptResult::Cancelled; }
            AttemptResult::Connected
        }
        Ok(()) => AttemptResult::Failed(friendly_error("netif did not come up")),
        Err(e) => AttemptResult::Failed(friendly_error(&e.to_string())),
    }
}

fn report_connect_result(
    wifi:      &mut BlockingWifi<EspWifi<'static>>,
    ssid:      &str,
    connected: bool,
    cancelled: bool,
    last_err:  &str,
    event_tx:  &SyncSender<WifiEvent>,
) -> bool {
    if cancelled {
        let _ = wifi.disconnect();
        let _ = event_tx.send(WifiEvent::ConnectError("Отменено пользователем".to_owned()));
        false
    } else if connected {
        let _ = event_tx.send(WifiEvent::Connected { ssid: ssid.to_owned(), ip: get_ip(wifi) });
        true
    } else {
        let _ = event_tx.send(WifiEvent::ConnectError(last_err.to_owned()));
        false
    }
}

fn get_ip(wifi: &BlockingWifi<EspWifi<'static>>) -> String {
    let ip = wifi.wifi().sta_netif().get_ip_info()
        .map(|info| format!("{}", info.ip))
        .unwrap_or_default();
    if ip == "0.0.0.0" { String::new() } else { ip }
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
    let h = match EspNvs::new(nvs.clone(), "wifi_cred", true) {
        Ok(h)  => h,
        Err(_) => return Vec::new(),
    };
    load_from_new_format(&h).unwrap_or_else(|| load_from_old_format(&h))
}

fn load_from_new_format(h: &EspNvs<NvsDefault>) -> Option<Vec<(String, String)>> {
    let count = h.get_u8("count").ok().flatten()?;
    let n = (count as usize).min(MAX_STORED);
    let mut creds = Vec::with_capacity(n);
    for i in 0..n {
        let mut sbuf = [0u8; 64];
        let mut pbuf = [0u8; 128];
        if let (Some(s), Some(p)) = (
            h.get_str(&format!("s{i}"), &mut sbuf).ok().flatten(),
            h.get_str(&format!("p{i}"), &mut pbuf).ok().flatten(),
        ) {
            creds.push((s.to_string(), p.to_string()));
        }
    }
    Some(creds)
}

fn load_from_old_format(h: &EspNvs<NvsDefault>) -> Vec<(String, String)> {
    let mut sbuf = [0u8; 64];
    let mut pbuf = [0u8; 128];
    if let (Some(s), Some(p)) = (
        h.get_str("ssid", &mut sbuf).ok().flatten(),
        h.get_str("pwd",  &mut pbuf).ok().flatten(),
    ) {
        if !s.is_empty() { return vec![(s.to_string(), p.to_string())]; }
    }
    Vec::new()
}

fn write_credentials(nvs: &EspDefaultNvsPartition, creds: &[(String, String)]) {
    match EspNvs::new(nvs.clone(), "wifi_cred", true) {
        Ok(h) => {
            let _ = h.set_u8("count", creds.len() as u8);
            for (i, (s, p)) in creds.iter().enumerate() {
                let _ = h.set_str(&format!("s{i}"), s);
                let _ = h.set_str(&format!("p{i}"), p);
            }
        }
        Err(e) => log::warn!("Failed to open NVS for credential storage: {e}"),
    }
}

fn save_credentials(nvs: &EspDefaultNvsPartition, ssid: &str, password: &str) {
    let mut existing = load_all_credentials(nvs);
    existing.retain(|(s, _)| s != ssid);
    existing.insert(0, (ssid.to_owned(), password.to_owned()));
    existing.truncate(MAX_STORED);
    write_credentials(nvs, &existing);
    log::info!("WiFi credentials saved for '{ssid}' ({} total)", existing.len());
}

fn forget_credential(nvs: &EspDefaultNvsPartition, ssid: &str) {
    let mut existing = load_all_credentials(nvs);
    let before = existing.len();
    existing.retain(|(s, _)| s != ssid);
    if existing.len() == before { return; }
    write_credentials(nvs, &existing);
    log::info!("WiFi: забыта сеть '{ssid}' (осталось {})", existing.len());
}
