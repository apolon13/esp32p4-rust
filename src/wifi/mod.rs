use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    hal::modem::Modem,
    nvs::EspDefaultNvsPartition,
    wifi::{AuthMethod, BlockingWifi, ClientConfiguration, Configuration, EspWifi},
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;

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
    Disconnect,
}

pub enum WifiEvent {
    Ready,
    ScanResult(Vec<ScannedNetwork>),
    ScanError(String),
    Connecting { ssid: String, attempt: u8, max: u8 },
    Connected { ssid: String },
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

    /// Send a command to the worker (non-blocking; drops the command if the
    /// channel is full, which should never happen in normal operation).
    pub fn send(&self, cmd: WifiCmd) {
        let _ = self.cmd_tx.try_send(cmd);
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

    while let Ok(cmd) = cmd_rx.recv() {
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
                        let mut nets: Vec<ScannedNetwork> = aps
                            .into_iter()
                            .map(|ap| ScannedNetwork {
                                ssid:    ap.ssid.as_str().to_owned(),
                                rssi:    ap.signal_strength as i32,
                                secured: ap.auth_method
                                    .map_or(false, |m| m != AuthMethod::None),
                            })
                            .collect();
                        nets.sort_by(|a, b| b.rssi.cmp(&a.rssi));
                        let _ = event_tx.send(WifiEvent::ScanResult(nets));
                    }
                    Err(e) => { let _ = event_tx.send(WifiEvent::ScanError(e.to_string())); }
                }
            }

            WifiCmd::Connect { ssid, password } => {
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
                        ssid: ssid.clone(), attempt, max: MAX,
                    });
                    let _ = wifi.set_configuration(&Configuration::Client(
                        ClientConfiguration {
                            ssid:     ssid.as_str().try_into().unwrap_or_default(),
                            password: password.as_str().try_into().unwrap_or_default(),
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
                        Ok(()) => { last_err = "netif did not come up".to_owned(); }
                        Err(e) => { last_err = e.to_string(); }
                    }
                    let _ = wifi.disconnect();
                }

                if cancelled {
                    let _ = wifi.disconnect();
                    let _ = event_tx.send(WifiEvent::ConnectError(
                        "Отменено пользователем".to_owned(),
                    ));
                } else if connected {
                    let _ = event_tx.send(WifiEvent::Connected { ssid });
                } else {
                    let _ = event_tx.send(WifiEvent::ConnectError(last_err));
                }
            }

            WifiCmd::Disconnect => {
                let _ = wifi.disconnect();
                let _ = event_tx.send(WifiEvent::Disconnected);
            }
        }
    }
}
