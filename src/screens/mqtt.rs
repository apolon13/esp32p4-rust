use slint::ComponentHandle;
use std::sync::atomic::Ordering;
use std::sync::mpsc::SyncSender;
use crate::AppWindow;
use crate::control::ControlCmd;
use crate::mqtt::{MqttCmd, MqttConfig, MqttEvent, MqttTopics, MqttWorker};

// ── Индексы полей формы (синхронизированы с Slint on_mqtt_field_set) ──────────
const FIELD_HOST:          i32 = 1;
const FIELD_PORT:          i32 = 2;
const FIELD_USERNAME:      i32 = 3;
const FIELD_PASSWORD:      i32 = 4;
const FIELD_CLIENT_ID:     i32 = 5;
const FIELD_TOPIC_ARM:     i32 = 6;
const FIELD_TOPIC_DISARM:  i32 = 7;
const FIELD_TOPIC_SILENT:  i32 = 8;
const FIELD_TOPIC_ALARM:   i32 = 9;

pub struct MqttScreenHandler {
    worker: MqttWorker,
    app:    slint::Weak<AppWindow>,
    cmd_tx: SyncSender<ControlCmd>,
}

impl MqttScreenHandler {
    pub fn new(app: &AppWindow, worker: MqttWorker, cmd_tx: SyncSender<ControlCmd>) -> Self {
        prefill_form(app, &worker);
        Self::register_connect(app, worker.cmd_sender());
        Self::register_cancel(app, worker.cancel_flag());
        Self::register_disconnect(app, worker.cmd_sender());
        Self::register_field_set(app);
        Self::register_editing_delete_last(app);
        Self { worker, app: app.as_weak(), cmd_tx }
    }

    pub fn poll(&self) {
        let Some(app) = self.app.upgrade() else { return };
        while let Some(event) = self.worker.try_recv() {
            handle_event(&app, event, &self.cmd_tx);
        }
    }

    fn register_connect(app: &AppWindow, tx: SyncSender<MqttCmd>) {
        let app_weak = app.as_weak();
        app.on_mqtt_connect_requested(move || {
            let app = app_weak.upgrade().unwrap();
            let cfg = read_config(&app);
            app.set_mqtt_connecting(true);
            app.set_mqtt_status(format!("Подключение к {}...", cfg.host).into());
            if tx.try_send(MqttCmd::Connect(cfg)).is_err() {
                log::warn!("mqtt cmd channel full");
            }
        });
    }

    fn register_cancel(app: &AppWindow, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        let app_weak = app.as_weak();
        app.on_mqtt_cancel_connect(move || {
            cancel.store(true, Ordering::Relaxed);
            app_weak.upgrade().unwrap().set_mqtt_status("Отмена...".into());
        });
    }

    fn register_disconnect(app: &AppWindow, tx: SyncSender<MqttCmd>) {
        app.on_mqtt_disconnect(move || {
            if tx.try_send(MqttCmd::Disconnect).is_err() {
                log::warn!("mqtt cmd channel full");
            }
        });
    }

    fn register_field_set(app: &AppWindow) {
        let app_weak = app.as_weak();
        app.on_mqtt_field_set(move |field, value| {
            if let Some(app) = app_weak.upgrade() { set_field(&app, field, value); }
        });
    }

    fn register_editing_delete_last(app: &AppWindow) {
        let app_weak = app.as_weak();
        app.on_mqtt_editing_delete_last(move || {
            let app = app_weak.upgrade().unwrap();
            let cur: String = app.get_mqtt_editing_text().into();
            app.set_mqtt_editing_text(super::delete_last_char(&cur).into());
        });
    }
}

// ── Form helpers ──────────────────────────────────────────────────────────────

fn prefill_form(app: &AppWindow, worker: &MqttWorker) {
    let cfg = worker.saved_config().unwrap_or_else(default_config);
    app.set_mqtt_host(cfg.host.into());
    app.set_mqtt_port(cfg.port.to_string().into());
    app.set_mqtt_username(cfg.username.into());
    app.set_mqtt_password(cfg.password.into());
    app.set_mqtt_client_id(cfg.client_id.into());
    app.set_mqtt_topic_arm(cfg.topics.arm.into());
    app.set_mqtt_topic_disarm(cfg.topics.disarm.into());
    app.set_mqtt_topic_silent(cfg.topics.silent.into());
    app.set_mqtt_topic_alarm(cfg.topics.alarm.into());
    app.set_mqtt_status("Не подключено".into());
}

fn default_config() -> MqttConfig {
    MqttConfig {
        host:      String::new(),
        port:      1883,
        username:  String::new(),
        password:  String::new(),
        client_id: String::new(),
        topics: MqttTopics {
            arm:    "security/arm".to_owned(),
            disarm: "security/disarm".to_owned(),
            silent: "security/silent".to_owned(),
            alarm:  "security/alarm".to_owned(),
        },
    }
}

fn read_config(app: &AppWindow) -> MqttConfig {
    let port_str: String = app.get_mqtt_port().into();
    MqttConfig {
        host:      app.get_mqtt_host().into(),
        port:      port_str.parse::<u16>().unwrap_or(1883),
        username:  app.get_mqtt_username().into(),
        password:  app.get_mqtt_password().into(),
        client_id: app.get_mqtt_client_id().into(),
        topics: MqttTopics {
            arm:    app.get_mqtt_topic_arm().into(),
            disarm: app.get_mqtt_topic_disarm().into(),
            silent: app.get_mqtt_topic_silent().into(),
            alarm:  app.get_mqtt_topic_alarm().into(),
        },
    }
}

fn set_field(app: &AppWindow, field: i32, val: slint::SharedString) {
    match field {
        FIELD_HOST         => app.set_mqtt_host(val),
        FIELD_PORT         => app.set_mqtt_port(val),
        FIELD_USERNAME     => app.set_mqtt_username(val),
        FIELD_PASSWORD     => app.set_mqtt_password(val),
        FIELD_CLIENT_ID    => app.set_mqtt_client_id(val),
        FIELD_TOPIC_ARM    => app.set_mqtt_topic_arm(val),
        FIELD_TOPIC_DISARM => app.set_mqtt_topic_disarm(val),
        FIELD_TOPIC_SILENT => app.set_mqtt_topic_silent(val),
        FIELD_TOPIC_ALARM  => app.set_mqtt_topic_alarm(val),
        _                  => {}
    }
}

// ── Event handlers ────────────────────────────────────────────────────────────

fn handle_event(app: &AppWindow, event: MqttEvent, cmd_tx: &SyncSender<ControlCmd>) {
    match event {
        MqttEvent::Connecting { attempt, max } => handle_connecting(app, attempt, max),
        MqttEvent::Connected                  => handle_connected(app),
        MqttEvent::ConnectError(e)            => handle_error(app, &e),
        MqttEvent::Disconnected               => handle_disconnected(app),
        MqttEvent::Command(cmd)               => handle_command(app, cmd, cmd_tx),
    }
}

fn handle_connecting(app: &AppWindow, attempt: u8, max: u8) {
    app.set_mqtt_connecting(true);
    app.set_mqtt_status(format!("Подключение ({attempt}/{max})...").into());
}

fn handle_connected(app: &AppWindow) {
    let host: String = app.get_mqtt_host().into();
    let port: String = app.get_mqtt_port().into();
    app.set_mqtt_is_connected(true);
    app.set_mqtt_connecting(false);
    app.set_mqtt_status(format!("Подключено: {host}:{port}").into());
}

fn handle_error(app: &AppWindow, e: &str) {
    app.set_mqtt_connecting(false);
    app.set_mqtt_is_connected(false);
    let msg = if e.contains("Отменено") { "Подключение отменено" } else { e };
    app.set_mqtt_status(msg.into());
}

fn handle_disconnected(app: &AppWindow) {
    app.set_mqtt_connecting(false);
    app.set_mqtt_is_connected(false);
    app.set_mqtt_status("Отключено".into());
}

fn handle_command(app: &AppWindow, cmd: ControlCmd, cmd_tx: &SyncSender<ControlCmd>) {
    let name = match cmd {
        ControlCmd::Arm    => "Охрана",
        ControlCmd::Disarm => "Снять с охраны",
        ControlCmd::Silent => "Без звука",
        ControlCmd::Alarm  => "Тревога",
    };
    log::info!("MQTT: команда «{name}»");
    app.set_mqtt_status(format!("Команда: {name}").into());
    let _ = cmd_tx.try_send(cmd);
}
