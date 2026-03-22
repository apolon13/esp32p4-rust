use esp_idf_svc::hal::{
    delay::TickType,
    gpio::{Gpio1, Gpio2, PinDriver},
    rmt::{
        config::{ReceiveConfig, RxChannelConfig},
        PinState, RxChannelDriver, Symbol,
    },
    units::FromValueType,
};
use esp_idf_svc::sys::EspError;
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::Duration;

// ── Публичные типы ───────────────────────────────────────────────────────────

/// Успешно декодированный 433 МГц OOK-код с метаданными протокола.
#[derive(Debug, Clone)]
pub struct RfCode {
    pub code:      u64,
    pub bit_count: u8,
    pub protocol:  &'static str,
}

// ── Дескриптор приёмника ─────────────────────────────────────────────────────

/// Дескриптор фонового потока SRX882.
///
/// Поток уже запущен в момент возврата [`RfReceiver::spawn`].
/// Основной поток опрашивает коды через [`RfReceiver::try_recv`] —
/// вызов никогда не блокируется.
pub struct RfReceiver {
    rx: Receiver<RfCode>,
}

impl RfReceiver {
    /// Запустить поток-приёмник.
    ///
    /// `ch_pin`   — GPIO1, линия CH (enable) приёмника SRX882.
    /// `data_pin` — GPIO2, линия DATA.
    pub fn spawn(ch_pin: Gpio1<'_>, data_pin: Gpio2<'_>) -> Self {
        let (tx, rx) = std::sync::mpsc::sync_channel::<RfCode>(16);

        // SAFETY: пины — синглтон-переферия, живут всё время работы программы.
        // Мы передаём владение рабочему потоку и больше не обращаемся к ним.
        let ch_pin:   Gpio1<'static> = unsafe { core::mem::transmute(ch_pin) };
        let data_pin: Gpio2<'static> = unsafe { core::mem::transmute(data_pin) };

        std::thread::Builder::new()
            .stack_size(8192)
            .name("rf_recv".to_string())
            .spawn(move || match recv_loop(ch_pin, data_pin, tx) {
                Ok(())  => log::warn!("RF receiver loop exited unexpectedly"),
                Err(e)  => log::error!("RF receiver fatal: {e}"),
            })
            .expect("rf_recv thread spawn failed");

        Self { rx }
    }

    /// Вернуть следующий декодированный код, если он есть.  Не блокируется.
    pub fn try_recv(&self) -> Option<RfCode> {
        self.rx.try_recv().ok()
    }
}

// ── Таблица протоколов ───────────────────────────────────────────────────────
//
// Каждая запись описывает OOK-тайминги в единицах базового периода T:
//   sync  = HIGH(sync_high·T)  + LOW(sync_low·T)
//   bit 0 = HIGH(zero_high·T)  + LOW(zero_low·T)
//   bit 1 = HIGH(one_high·T)   + LOW(one_low·T)
//
// Декодер вычисляет T из длины sync-импульса и проверяет попадание в
// [t_min, t_max] мкс.  Протоколы проверяются по порядку.

struct Protocol {
    name:       &'static str,
    sync_high:  u32,
    sync_low:   u32,
    zero_high:  u32,
    zero_low:   u32,
    one_high:   u32,
    one_low:    u32,
    bit_count:  u8,
    t_min:      u32,
    t_max:      u32,
}

static PROTOCOLS: &[Protocol] = &[
    // ── 24-битные протоколы ──────────────────────────────────────────────
    Protocol { name: "EV1527",    sync_high:  1, sync_low: 31, zero_high: 1, zero_low: 3, one_high: 3, one_low: 1, bit_count: 24, t_min:  80, t_max:  800 },
    Protocol { name: "EV1527-HR", sync_high:  1, sync_low: 71, zero_high: 4, zero_low:11, one_high: 9, one_low: 6, bit_count: 24, t_min:  50, t_max:  400 },
    Protocol { name: "HS2303",    sync_high: 36, sync_low:  1, zero_high: 1, zero_low: 2, one_high: 2, one_low: 1, bit_count: 24, t_min:  50, t_max:  500 },
    Protocol { name: "PT2262v2",  sync_high:  1, sync_low: 10, zero_high: 1, zero_low: 2, one_high: 2, one_low: 1, bit_count: 24, t_min: 200, t_max: 1200 },
    Protocol { name: "SC2262",    sync_high:  1, sync_low: 10, zero_high: 1, zero_low: 3, one_high: 3, one_low: 1, bit_count: 24, t_min: 100, t_max:  700 },
    Protocol { name: "Kerui",     sync_high:  1, sync_low: 23, zero_high: 1, zero_low: 2, one_high: 2, one_low: 1, bit_count: 24, t_min: 150, t_max:  900 },
    Protocol { name: "ShortSync", sync_high:  1, sync_low:  6, zero_high: 1, zero_low: 3, one_high: 3, one_low: 1, bit_count: 24, t_min: 100, t_max:  800 },
    // ── 28-битные протоколы ──────────────────────────────────────────────
    Protocol { name: "HT6P20B",   sync_high: 10, sync_low: 40, zero_high: 1, zero_low: 5, one_high: 3, one_low: 3, bit_count: 28, t_min: 100, t_max:  900 },
    // ── 12-битные протоколы ──────────────────────────────────────────────
    Protocol { name: "NiceFLO",   sync_high:  1, sync_low: 36, zero_high: 1, zero_low: 3, one_high: 3, one_low: 1, bit_count: 12, t_min: 200, t_max:  900 },
    Protocol { name: "HT12E",     sync_high:  1, sync_low: 36, zero_high: 1, zero_low: 2, one_high: 2, one_low: 1, bit_count: 12, t_min: 100, t_max:  600 },
    Protocol { name: "CAME",      sync_high:  1, sync_low: 18, zero_high: 1, zero_low: 3, one_high: 3, one_low: 1, bit_count: 12, t_min: 200, t_max:  700 },
];

// ── Основной цикл приёма ─────────────────────────────────────────────────────

fn recv_loop(
    ch_pin:   Gpio1<'static>,
    data_pin: Gpio2<'static>,
    tx:       SyncSender<RfCode>,
) -> Result<(), EspError> {
    let mut ch = PinDriver::output(ch_pin)?;
    ch.set_high()?;
    log::info!("SRX882 enabled (CH = GPIO1 HIGH)");

    let mut rx = RxChannelDriver::new(
        data_pin,
        &RxChannelConfig {
            resolution: 1.MHz().into(),
            ..Default::default()
        },
    )?;

    let rx_cfg = ReceiveConfig {
        signal_range_min: Duration::from_nanos(3000),
        signal_range_max: Duration::from_millis(13),
        timeout: Some(TickType::from(Duration::from_secs(10)).0),
        ..Default::default()
    };

    let mut buf       = [Symbol::default(); 256];
    let mut prev      = LastSeen::default();

    log::info!(
        "RF: listening on GPIO2 ({} protocols)",
        PROTOCOLS.len(),
    );

    loop {
        match rx.receive(&mut buf, &rx_cfg) {
            Ok(n) if n >= 13 => {
                let symbols = &buf[..n];
                if let Some(rc) = decode(symbols) {
                    if !prev.is_repeat(&rc) {
                        log::info!(
                            "RF [{proto}] code=0x{code:0width$X} ({bits}bit)",
                            proto = rc.protocol,
                            code  = rc.code,
                            bits  = rc.bit_count,
                            width = ((rc.bit_count as usize) + 3) / 4,
                        );
                        let _ = tx.try_send(rc.clone());
                        prev.update(&rc);
                    }
                } else {
                    let _ = symbols; // unrecognised burst, discard
                }
            }
            Ok(n) if n > 0 => {}  // short burst, ignored
            Ok(_) => {}
            Err(e) if e.code() == esp_idf_svc::sys::ESP_ERR_TIMEOUT => {}
            Err(e) => {
                log::warn!("RF rx error: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

// ── Защита от повторов ───────────────────────────────────────────────────────

#[derive(Default)]
struct LastSeen {
    code:  u64,
    proto: &'static str,
    time:  Option<std::time::Instant>,
}

impl LastSeen {
    fn is_repeat(&self, rc: &RfCode) -> bool {
        self.time.map_or(false, |t| {
            rc.code == self.code
                && rc.protocol == self.proto
                && t.elapsed() < Duration::from_millis(400)
        })
    }

    fn update(&mut self, rc: &RfCode) {
        self.code  = rc.code;
        self.proto = rc.protocol;
        self.time  = Some(std::time::Instant::now());
    }
}

// ── Декодер ──────────────────────────────────────────────────────────────────

fn decode(symbols: &[Symbol]) -> Option<RfCode> {
    PROTOCOLS.iter().find_map(|p| try_protocol(symbols, p))
}

fn try_protocol(symbols: &[Symbol], proto: &'static Protocol) -> Option<RfCode> {
    let needed = 1 + proto.bit_count as usize;

    for start in 0..symbols.len().saturating_sub(needed) {
        let sync = symbols[start];
        let h    = sync.level0();
        let l    = sync.level1();

        if h.pin_state != PinState::High || l.pin_state != PinState::Low {
            continue;
        }

        let ht          = h.ticks.ticks() as u32;
        let lt          = l.ticks.ticks() as u32;
        let sync_units  = proto.sync_high + proto.sync_low;
        let t           = (ht + lt) / sync_units;

        if t < proto.t_min || t > proto.t_max { continue; }

        let sync_tol = (t * 2).max(100);
        if !near(ht, t * proto.sync_high, sync_tol)
        || !near(lt, t * proto.sync_low,  sync_tol) { continue; }

        if let Some(code) = decode_bits(symbols, start + 1, proto.bit_count as usize, t, proto) {
            return Some(RfCode { code, bit_count: proto.bit_count, protocol: proto.name });
        }
    }
    None
}

fn decode_bits(
    symbols:  &[Symbol],
    offset:   usize,
    num_bits: usize,
    t:        u32,
    proto:    &Protocol,
) -> Option<u64> {
    if offset + num_bits > symbols.len() { return None; }

    let tol      = (t * 3 / 4).max(80);
    let mut code = 0u64;

    for i in 0..num_bits {
        let sym = symbols[offset + i];
        let bh  = sym.level0().ticks.ticks() as u32;
        let bl  = sym.level1().ticks.ticks() as u32;
        code <<= 1;
        if      near(bh, t * proto.one_high,  tol) && near(bl, t * proto.one_low,  tol) { code |= 1; }
        else if near(bh, t * proto.zero_high, tol) && near(bl, t * proto.zero_low, tol) { /* 0 */ }
        else    { return None; }
    }
    Some(code)
}

#[inline]
fn near(val: u32, expected: u32, tolerance: u32) -> bool {
    val.abs_diff(expected) <= tolerance
}

