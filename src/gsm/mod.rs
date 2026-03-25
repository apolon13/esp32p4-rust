use esp_idf_svc::hal::{
    delay::TickType,
    gpio::{AnyIOPin, Gpio3, Gpio4},
    uart::{config::Config, UartDriver, UART1},
    units::Hertz,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use crate::control::ControlCmd;

// ── Публичные типы ────────────────────────────────────────────────────────────

#[derive(Default, Clone)]
pub struct SimStatus {
    pub msisdn: String,
    pub signal: String,
    pub reg:    String,
    pub cpin:   String,
}

pub struct GsmMonitor {
    pub status:    Arc<Mutex<SimStatus>>,
    pub whitelist: Arc<Mutex<Vec<String>>>,
    cmd_tx:        Arc<Mutex<Option<std::sync::mpsc::SyncSender<ControlCmd>>>>,
}

impl GsmMonitor {
    pub fn spawn(uart: UART1<'_>, rx: Gpio3<'_>, tx: Gpio4<'_>) -> Self {
        let uart: UART1<'static> = unsafe { core::mem::transmute(uart) };
        let rx:   Gpio3<'static> = unsafe { core::mem::transmute(rx) };
        let tx:   Gpio4<'static> = unsafe { core::mem::transmute(tx) };
        let status    = Arc::new(Mutex::new(SimStatus::default()));
        let whitelist = Arc::new(Mutex::new(Vec::<String>::new()));
        let cmd_tx    = Arc::new(Mutex::new(None::<std::sync::mpsc::SyncSender<ControlCmd>>));
        let s = Arc::clone(&status);
        let w = Arc::clone(&whitelist);
        let c = Arc::clone(&cmd_tx);
        std::thread::Builder::new()
            .stack_size(8192)
            .name("gsm_monitor".to_string())
            .spawn(move || run(uart, rx, tx, s, w, c))
            .expect("gsm_monitor spawn failed");
        Self { status, whitelist, cmd_tx }
    }

    pub fn connect_cmd(&self, tx: std::sync::mpsc::SyncSender<ControlCmd>) {
        if let Ok(mut g) = self.cmd_tx.lock() { *g = Some(tx); }
    }
}

// ── Поток мониторинга ─────────────────────────────────────────────────────────

fn run(
    uart:      UART1<'static>,
    rx:        Gpio3<'static>,
    tx:        Gpio4<'static>,
    status:    Arc<Mutex<SimStatus>>,
    whitelist: Arc<Mutex<Vec<String>>>,
    cmd_tx:    Arc<Mutex<Option<std::sync::mpsc::SyncSender<ControlCmd>>>>,
) {
    let config = Config::new().baudrate(Hertz(115_200));
    let driver = match UartDriver::new(
        uart, tx, rx,
        Option::<AnyIOPin>::None,
        Option::<AnyIOPin>::None,
        &config,
    ) {
        Ok(d)  => d,
        Err(e) => { log::error!("GSM: UART init failed: {e}"); return; }
    };
    log::info!("GSM: monitor started");

    // Режим текстовых SMS
    at_cmd(&driver, "AT+CMGF=1");

    let msisdn = query_msisdn(&driver);
    log::info!("GSM: номер SIM = {}", msisdn.as_deref().unwrap_or("не определён"));
    if let Ok(mut s) = status.lock() {
        s.msisdn = msisdn.unwrap_or_default();
    }

    let mut last_status: Option<(String, String, String)> = None;
    loop {
        last_status = poll_status(&driver, &status, last_status);
        poll_sms(&driver, &whitelist, &cmd_tx);
        std::thread::sleep(Duration::from_secs(1));
    }
}

// ── Опрос статуса ─────────────────────────────────────────────────────────────

fn poll_status(
    driver: &UartDriver<'_>,
    status: &Arc<Mutex<SimStatus>>,
    last:   Option<(String, String, String)>,
) -> Option<(String, String, String)> {
    let csq  = at_cmd(driver, "AT+CSQ").and_then(parse_csq);
    let creg = at_cmd(driver, "AT+CREG?").and_then(parse_creg);
    let cpin = at_cmd(driver, "AT+CPIN?").and_then(parse_cpin);
    match (csq, creg, cpin) {
        (None, None, None) => {
            if last.is_some() { log::warn!("GSM: нет ответа от модуля"); }
            None
        }
        (csq, creg, cpin) => {
            let state = (
                csq.unwrap_or_default(),
                creg.unwrap_or_default(),
                cpin.unwrap_or_default(),
            );
            if last.as_ref() != Some(&state) {
                log::info!(
                    "GSM: rssi={} reg={} sim={}",
                    if state.0.is_empty() { "?" } else { &state.0 },
                    if state.1.is_empty() { "?" } else { &state.1 },
                    if state.2.is_empty() { "?" } else { &state.2 },
                );
                if let Ok(mut s) = status.lock() {
                    s.signal = state.0.clone();
                    s.reg    = state.1.clone();
                    s.cpin   = state.2.clone();
                }
            }
            Some(state)
        }
    }
}

// ── Опрос входящих SMS ────────────────────────────────────────────────────────

fn poll_sms(
    driver:    &UartDriver<'_>,
    whitelist: &Arc<Mutex<Vec<String>>>,
    cmd_tx:    &Arc<Mutex<Option<std::sync::mpsc::SyncSender<ControlCmd>>>>,
) {
    let raw = match at_cmd_wait(driver, "AT+CMGL=\"REC UNREAD\"", Duration::from_millis(1000)) {
        Some(r) if r.contains("+CMGL:") => r,
        _ => return,
    };
    for (idx, sender, text) in parse_sms_list(&raw) {
        let allowed = whitelist.lock()
            .map(|wl| wl.iter().any(|n| numbers_match(n, &sender)))
            .unwrap_or(false);
        if !allowed {
            log::warn!("GSM SMS: от неизвестного номера {sender}");
            delete_sms(driver, idx);
            continue;
        }
        match parse_sms_cmd(&text) {
            Some(cmd) => {
                log::info!("GSM SMS: команда {:?} от {sender}", cmd);
                if let Ok(g) = cmd_tx.lock() {
                    if let Some(tx) = g.as_ref() { let _ = tx.try_send(cmd); }
                }
                send_sms(driver, &sender, cmd_reply(cmd));
            }
            None => {
                log::warn!("GSM SMS: неизвестная команда от {sender}: {text:?}");
                send_sms(driver, &sender, "Неизвестная команда. Доступны: arm, disarm, silent, alarm");
            }
        }
        delete_sms(driver, idx);
    }
}

// ── SMS helpers ───────────────────────────────────────────────────────────────

fn parse_sms_list(raw: &str) -> Vec<(u32, String, String)> {
    let mut result = Vec::new();
    for part in raw.split("+CMGL:").skip(1) {
        let mut lines = part.lines();
        let header = lines.next().unwrap_or("").trim();
        // header: 1,"REC UNREAD","+79001234567","","24/01/01,12:00:00+12"
        let idx = header.split(',').next()
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let sender = header.split('"').nth(3).unwrap_or("").to_string();
        let text = lines.find(|l| !l.trim().is_empty())
            .unwrap_or("").trim().to_string();
        if !sender.is_empty() {
            result.push((idx, sender, text));
        }
    }
    result
}

fn parse_sms_cmd(text: &str) -> Option<ControlCmd> {
    match text.trim().to_lowercase().as_str() {
        "arm"    | "охрана"  => Some(ControlCmd::Arm),
        "disarm" | "снять"   => Some(ControlCmd::Disarm),
        "silent" | "тихий"   => Some(ControlCmd::Silent),
        "alarm"  | "тревога" => Some(ControlCmd::Alarm),
        _ => None,
    }
}

fn cmd_reply(cmd: ControlCmd) -> &'static str {
    match cmd {
        ControlCmd::Arm    => "OK: поставлен на охрану",
        ControlCmd::Disarm => "OK: снят с охраны",
        ControlCmd::Silent => "OK: тревога отключена",
        ControlCmd::Alarm  => "OK: тревога включена",
    }
}

fn send_sms(driver: &UartDriver<'_>, to: &str, text: &str) {
    let cmd = format!("AT+CMGS=\"{}\"\r\n", to);
    if driver.write(cmd.as_bytes()).is_err() { return; }
    // Ждём приглашения '>' от модема перед отправкой текста
    match read_until(driver, ">", Duration::from_secs(5)) {
        Some(_) => {}
        None => {
            log::warn!("GSM SMS: нет приглашения '>' от модема, отмена отправки");
            let _ = driver.write(b"\x1B"); // ESC — отмена
            return;
        }
    }
    let msg = format!("{}\x1A", text);
    let _ = driver.write(msg.as_bytes());
    // Ждём подтверждения +CMGS или ERROR
    match read_until(driver, "+CMGS:", Duration::from_secs(10)) {
        Some(_) => log::info!("GSM SMS: подтверждение отправлено на {to}"),
        None    => log::warn!("GSM SMS: нет подтверждения отправки на {to}"),
    }
}

fn delete_sms(driver: &UartDriver<'_>, idx: u32) {
    let cmd = format!("AT+CMGD={}", idx);
    at_cmd(driver, &cmd);
}

fn numbers_match(a: &str, b: &str) -> bool {
    normalize_number(a) == normalize_number(b)
}

fn normalize_number(n: &str) -> String {
    let digits: String = n.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.starts_with('8') && digits.len() == 11 {
        format!("7{}", &digits[1..])
    } else {
        digits
    }
}

// ── Запрос номера через AT+CNUM / USSD ───────────────────────────────────────

fn query_msisdn(driver: &UartDriver<'_>) -> Option<String> {
    if let Some(num) = query_msisdn_cnum(driver) {
        return Some(num);
    }
    log::info!("GSM: AT+CNUM пусто, пробуем USSD *103#");
    driver.write(b"AT+CUSD=1,\"*103#\",15\r\n").ok()?;
    let raw = read_until(driver, "+CUSD:", Duration::from_secs(8));
    log::info!("GSM: CUSD raw={:?}", raw.as_deref().unwrap_or("<timeout>"));
    parse_cusd(&raw?)
}

fn query_msisdn_cnum(driver: &UartDriver<'_>) -> Option<String> {
    driver.write(b"AT+CNUM\r\n").ok()?;
    let raw = read_until(driver, "+CNUM:", Duration::from_secs(3))?;
    log::info!("GSM: CNUM raw={:?}", raw);
    let line = raw.lines().find(|l| l.starts_with("+CNUM:"))?;
    let num = line.split('"').nth(3)?;
    if num.is_empty() { return None; }
    Some(num.to_string())
}

// ── AT-команды ────────────────────────────────────────────────────────────────

fn at_cmd(driver: &UartDriver<'_>, cmd: &str) -> Option<String> {
    let msg = format!("{}\r\n", cmd);
    driver.write(msg.as_bytes()).ok()?;
    let mut buf = [0u8; 128];
    let timeout = TickType::from(Duration::from_millis(500)).0;
    let n = driver.read(&mut buf, timeout).unwrap_or(0);
    if n == 0 { return None; }
    Some(String::from_utf8_lossy(&buf[..n]).into_owned())
}

/// Отправляет AT команду и ждёт "OK" (для длинных ответов, например CMGL).
fn at_cmd_wait(driver: &UartDriver<'_>, cmd: &str, timeout: Duration) -> Option<String> {
    let msg = format!("{}\r\n", cmd);
    driver.write(msg.as_bytes()).ok()?;
    read_until(driver, "OK", timeout)
}

/// Читает UART чанками до появления `marker` или истечения `timeout`.
fn read_until(driver: &UartDriver<'_>, marker: &str, timeout: Duration) -> Option<String> {
    let mut buf = [0u8; 256];
    let mut acc = String::new();
    let deadline = Instant::now() + timeout;
    let chunk_ticks = TickType::from(Duration::from_millis(300)).0;
    while Instant::now() < deadline {
        let n = driver.read(&mut buf, chunk_ticks).unwrap_or(0);
        if n > 0 { acc.push_str(&String::from_utf8_lossy(&buf[..n])); }
        if acc.contains(marker) { return Some(acc); }
    }
    None
}

// ── Парсеры ответов ───────────────────────────────────────────────────────────

fn parse_csq(resp: String) -> Option<String> {
    let line = resp.lines().find(|l| l.starts_with("+CSQ:"))?;
    let val  = line.trim_start_matches("+CSQ:").trim();
    let rssi = val.split(',').next()?.trim().parse::<i32>().ok()?;
    if rssi == 99 { return Some("нет сигнала".to_string()); }
    Some(format!("{rssi} ({}dBm)", -113 + 2 * rssi))
}

fn parse_creg(resp: String) -> Option<String> {
    let line = resp.lines().find(|l| l.starts_with("+CREG:"))?;
    let val  = line.trim_start_matches("+CREG:").trim();
    let raw_stat = val.split(',').nth(1).unwrap_or(val).trim();
    let stat = match raw_stat.parse::<u8>() {
        Ok(v)  => v,
        Err(_) => { log::warn!("GSM: CREG raw={:?} stat={:?}", val, raw_stat); return None; }
    };
    Some(match stat {
        0 => "не зарегистрирован",
        1 => "домашняя сеть",
        2 => "поиск сети",
        3 => "отказ",
        4 => "неизвестно",
        5 => "роуминг",
        6 => "домашняя сеть (SMS)",
        7 => "роуминг (SMS)",
        _ => { log::warn!("GSM: CREG неизвестный stat={stat}"); "неизвестно" }
    }.to_string())
}

fn parse_cpin(resp: String) -> Option<String> {
    resp.lines()
        .find(|l| l.starts_with("+CPIN:"))
        .map(|l| l.trim_start_matches("+CPIN:").trim().to_string())
}

fn parse_cusd(resp: &str) -> Option<String> {
    let line = resp.lines().find(|l| l.starts_with("+CUSD:"))?;
    let msg  = line.split('"').nth(1)?;
    msg.split_whitespace()
        .find(|w| {
            let d: String = w.chars().filter(|c| c.is_ascii_digit()).collect();
            (w.starts_with('+') || w.starts_with('7') || w.starts_with('8')) && d.len() >= 10
        })
        .map(|s| s.trim_end_matches([',', '.', ';']).to_string())
        .or_else(|| Some(msg.to_string()))
}
