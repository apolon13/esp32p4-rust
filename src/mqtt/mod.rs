use esp_idf_svc::{
    mqtt::client::{EspMqttClient, EspMqttEvent, EventPayload, MqttClientConfiguration, QoS},
    nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault},
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::time::Duration;

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct MqttTopics {
    pub arm:    String,
    pub disarm: String,
    pub silent: String,
    pub alarm:  String,
}

#[derive(Clone, Debug)]
pub struct MqttConfig {
    pub host:      String,
    pub port:      u16,
    pub username:  String,
    pub password:  String,
    pub client_id: String,
    pub topics:    MqttTopics,
}

pub enum MqttCmd { Connect(MqttConfig), Disconnect }

pub use crate::control::ControlCmd;

pub enum MqttEvent {
    Connecting { attempt: u8, max: u8 },
    Connected,
    ConnectError(String),
    Disconnected,
    Command(ControlCmd),
}

// ── MqttWorker ────────────────────────────────────────────────────────────────

pub struct MqttWorker {
    cmd_tx:      SyncSender<MqttCmd>,
    event_rx:    Receiver<MqttEvent>,
    cancel_flag: Arc<AtomicBool>,
    nvs:         EspDefaultNvsPartition,
}

impl MqttWorker {
    pub fn spawn(nvs: EspDefaultNvsPartition, wifi_rx: Receiver<bool>) -> Self {
        let (cmd_tx,   cmd_rx)   = std::sync::mpsc::sync_channel::<MqttCmd>(4);
        let (event_tx, event_rx) = std::sync::mpsc::sync_channel::<MqttEvent>(8);
        let cancel_flag  = Arc::new(AtomicBool::new(false));
        let cancel_clone = cancel_flag.clone();
        let nvs_worker   = nvs.clone();
        std::thread::Builder::new()
            .stack_size(16384)
            .name("mqtt_worker".to_string())
            .spawn(move || worker_loop(nvs_worker, cmd_rx, wifi_rx, event_tx, cancel_clone))
            .expect("mqtt worker spawn failed");
        Self { cmd_tx, event_rx, cancel_flag, nvs }
    }

    pub fn try_recv(&self)                   -> Option<MqttEvent>     { self.event_rx.try_recv().ok() }
    pub fn cmd_sender(&self)                 -> SyncSender<MqttCmd>   { self.cmd_tx.clone() }
    pub fn cancel_flag(&self)                -> Arc<AtomicBool>       { self.cancel_flag.clone() }
    pub fn saved_config(&self)               -> Option<MqttConfig>    { load_config(&self.nvs) }
}

// ── Worker ────────────────────────────────────────────────────────────────────

type IncomingMsg = (String, Vec<u8>);

struct SessionData {
    client:  EspMqttClient<'static>,
    config:  MqttConfig,
    msg_rx:  Receiver<IncomingMsg>,
    disc_rx: Receiver<()>,
}

fn worker_loop(
    nvs:     EspDefaultNvsPartition,
    cmd_rx:  Receiver<MqttCmd>,
    wifi_rx: Receiver<bool>,
    evt_tx:  SyncSender<MqttEvent>,
    cancel:  Arc<AtomicBool>,
) {
    loop {
        match disconnected_loop(&nvs, &cmd_rx, &wifi_rx, &evt_tx, &cancel) {
            None     => return,
            Some(sd) => { if !connected_loop(sd, &cmd_rx, &wifi_rx, &evt_tx) { return; } }
        }
    }
}

fn disconnected_loop(
    nvs:     &EspDefaultNvsPartition,
    cmd_rx:  &Receiver<MqttCmd>,
    wifi_rx: &Receiver<bool>,
    evt_tx:  &SyncSender<MqttEvent>,
    cancel:  &AtomicBool,
) -> Option<SessionData> {
    loop {
        let mut wifi_up = false;
        while let Ok(v) = wifi_rx.try_recv() { if v { wifi_up = true; } }
        if wifi_up {
            if let Some(cfg) = load_config(nvs) {
                if let Some(sd) = do_connect(cfg, cancel, evt_tx) { return Some(sd); }
            }
        }
        match cmd_rx.recv_timeout(Duration::from_millis(200)) {
            Ok(MqttCmd::Connect(cfg)) => {
                save_config(nvs, &cfg);
                if let Some(sd) = do_connect(cfg, cancel, evt_tx) { return Some(sd); }
            }
            Ok(MqttCmd::Disconnect)             => {}
            Err(RecvTimeoutError::Disconnected) => return None,
            Err(RecvTimeoutError::Timeout)      => {}
        }
    }
}

fn connected_loop(
    sd:      SessionData,
    cmd_rx:  &Receiver<MqttCmd>,
    wifi_rx: &Receiver<bool>,
    evt_tx:  &SyncSender<MqttEvent>,
) -> bool {
    let SessionData { client: _client, config, msg_rx, disc_rx } = sd;
    loop {
        if disc_rx.try_recv().is_ok()  { return send_disc(evt_tx); }
        if check_wifi_down(wifi_rx)    { return send_disc(evt_tx); }
        while let Ok(msg) = msg_rx.try_recv() { handle_message(&msg, &config, evt_tx); }
        match cmd_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(MqttCmd::Disconnect)             => return send_disc(evt_tx),
            Ok(MqttCmd::Connect(_))             => {}
            Err(RecvTimeoutError::Disconnected) => return false,
            Err(RecvTimeoutError::Timeout)      => {}
        }
    }
}

fn check_wifi_down(wifi_rx: &Receiver<bool>) -> bool {
    let mut down = false;
    while let Ok(v) = wifi_rx.try_recv() { if !v { down = true; } }
    down
}

fn send_disc(evt_tx: &SyncSender<MqttEvent>) -> bool {
    let _ = evt_tx.send(MqttEvent::Disconnected);
    true
}

// ── Connection with retries ───────────────────────────────────────────────────

enum AttemptResult {
    Connected { client: EspMqttClient<'static>, msg_rx: Receiver<IncomingMsg>, disc_rx: Receiver<()> },
    Cancelled,
    Failed(String),
}

fn do_connect(
    config: MqttConfig,
    cancel: &AtomicBool,
    evt_tx: &SyncSender<MqttEvent>,
) -> Option<SessionData> {
    cancel.store(false, Ordering::Relaxed);
    const MAX: u8 = 3;
    let url = format!("mqtt://{}:{}", config.host, config.port);
    let mut last_err = String::new();
    for attempt in 1..=MAX {
        if cancel.load(Ordering::Relaxed) { break; }
        let _ = evt_tx.send(MqttEvent::Connecting { attempt, max: MAX });
        match attempt_once(&url, &config, cancel) {
            AttemptResult::Connected { client, msg_rx, disc_rx } => {
                let _ = evt_tx.send(MqttEvent::Connected);
                return Some(SessionData { client, config, msg_rx, disc_rx });
            }
            AttemptResult::Cancelled => break,
            AttemptResult::Failed(e) => { last_err = e; }
        }
    }
    let msg = if cancel.load(Ordering::Relaxed) { "Отменено".to_owned() } else { last_err };
    let _ = evt_tx.send(MqttEvent::ConnectError(msg));
    None
}

fn attempt_once(url: &str, config: &MqttConfig, cancel: &AtomicBool) -> AttemptResult {
    let (msg_tx,  msg_rx)  = std::sync::mpsc::sync_channel::<IncomingMsg>(8);
    let (conn_tx, conn_rx) = std::sync::mpsc::sync_channel::<bool>(1);
    let (disc_tx, disc_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let mut client: EspMqttClient<'static> = match create_client(url, config, msg_tx, conn_tx, disc_tx) {
        Err(e) => return AttemptResult::Failed(e),
        Ok(c)  => c,
    };
    if !wait_for_conn(&conn_rx, cancel) {
        return if cancel.load(Ordering::Relaxed) {
            AttemptResult::Cancelled
        } else {
            AttemptResult::Failed("Таймаут подключения".to_owned())
        };
    }
    if subscribe_topics(&mut client, &config.topics) {
        AttemptResult::Connected { client, msg_rx, disc_rx }
    } else {
        AttemptResult::Failed("Ошибка подписки на топики".to_owned())
    }
}

fn create_client(
    url:     &str,
    config:  &MqttConfig,
    msg_tx:  SyncSender<IncomingMsg>,
    conn_tx: SyncSender<bool>,
    disc_tx: SyncSender<()>,
) -> Result<EspMqttClient<'static>, String> {
    let user = if config.username.is_empty()  { None } else { Some(config.username.as_str()) };
    let pass = if config.password.is_empty()  { None } else { Some(config.password.as_str()) };
    let cid  = if config.client_id.is_empty() { None } else { Some(config.client_id.as_str()) };
    let cfg = MqttClientConfiguration {
        username: user, password: pass, client_id: cid, ..Default::default()
    };
    EspMqttClient::new_cb(url, &cfg, move |event: EspMqttEvent<'_>| {
        match event.payload() {
            EventPayload::Connected(_) => { let _ = conn_tx.send(true); }
            EventPayload::Disconnected => { let _ = disc_tx.send(()); }
            EventPayload::Received { topic: Some(t), data, .. } => {
                let _ = msg_tx.try_send((t.to_owned(), data.to_vec()));
            }
            _ => {}
        }
    }).map_err(|e| e.to_string())
}

fn wait_for_conn(conn_rx: &Receiver<bool>, cancel: &AtomicBool) -> bool {
    for _ in 0..100 {
        if cancel.load(Ordering::Relaxed) { return false; }
        match conn_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(_)                               => return true,
            Err(RecvTimeoutError::Timeout)      => {}
            Err(RecvTimeoutError::Disconnected) => return false,
        }
    }
    false
}

fn subscribe_topics(client: &mut EspMqttClient<'static>, topics: &MqttTopics) -> bool {
    let all = [&topics.arm, &topics.disarm, &topics.silent, &topics.alarm];
    for t in &all {
        if !t.is_empty() && client.subscribe(t.as_str(), QoS::AtMostOnce).is_err() {
            return false;
        }
    }
    true
}

fn handle_message(msg: &IncomingMsg, config: &MqttConfig, evt_tx: &SyncSender<MqttEvent>) {
    if let Some(cmd) = cmd_from_topic(&msg.0, &config.topics) {
        log::info!("MQTT: команда {:?}", cmd);
        let _ = evt_tx.send(MqttEvent::Command(cmd));
    }
}

fn cmd_from_topic(topic: &str, topics: &MqttTopics) -> Option<ControlCmd> {
    if !topics.arm.is_empty()    && topic == topics.arm    { return Some(ControlCmd::Arm); }
    if !topics.disarm.is_empty() && topic == topics.disarm { return Some(ControlCmd::Disarm); }
    if !topics.silent.is_empty() && topic == topics.silent { return Some(ControlCmd::Silent); }
    if !topics.alarm.is_empty()  && topic == topics.alarm  { return Some(ControlCmd::Alarm); }
    None
}

// ── NVS ──────────────────────────────────────────────────────────────────────

fn save_config(nvs: &EspDefaultNvsPartition, cfg: &MqttConfig) {
    let Ok(h) = EspNvs::new(nvs.clone(), "mqtt", true) else { return };
    let _ = h.set_str("host",  &cfg.host);
    let _ = h.set_u32("port",  cfg.port as u32);
    let _ = h.set_str("user",  &cfg.username);
    let _ = h.set_str("pass",  &cfg.password);
    let _ = h.set_str("cid",   &cfg.client_id);
    let _ = h.set_str("t_arm", &cfg.topics.arm);
    let _ = h.set_str("t_dis", &cfg.topics.disarm);
    let _ = h.set_str("t_sil", &cfg.topics.silent);
    let _ = h.set_str("t_alm", &cfg.topics.alarm);
}

fn load_config(nvs: &EspDefaultNvsPartition) -> Option<MqttConfig> {
    let h = EspNvs::new(nvs.clone(), "mqtt", true).ok()?;
    let mut buf = [0u8; 256];
    let host = h.get_str("host", &mut buf).ok().flatten()?.to_string();
    if host.is_empty() { return None; }
    let port     = h.get_u32("port").ok().flatten().unwrap_or(1883) as u16;
    let username  = get_str(&h, "user", &mut buf, "");
    let password  = get_str(&h, "pass", &mut buf, "");
    let client_id = get_str(&h, "cid",  &mut buf, "");
    let topics    = load_topics(&h);
    Some(MqttConfig { host, port, username, password, client_id, topics })
}

fn load_topics(h: &EspNvs<NvsDefault>) -> MqttTopics {
    let mut buf = [0u8; 128];
    let arm    = get_str(h, "t_arm", &mut buf, "security/arm");
    let disarm = get_str(h, "t_dis", &mut buf, "security/disarm");
    let silent = get_str(h, "t_sil", &mut buf, "security/silent");
    let alarm  = get_str(h, "t_alm", &mut buf, "security/alarm");
    MqttTopics { arm, disarm, silent, alarm }
}

fn get_str(h: &EspNvs<NvsDefault>, key: &str, buf: &mut [u8], default: &str) -> String {
    h.get_str(key, buf).ok().flatten()
        .map(str::to_owned)
        .unwrap_or_else(|| default.to_owned())
}
