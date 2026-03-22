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
use std::time::Duration;

/// Spawn a background thread that captures 433 MHz RC codes from the SRX882
/// receiver (CH = GPIO1, DATA = GPIO2) and logs every recognised code.
pub fn spawn(ch_pin: Gpio1<'_>, data_pin: Gpio2<'_>) {
    // SAFETY: GPIO pins are singleton peripherals that live for the entire
    // program. We move ownership to the worker thread and never access them
    // from any other thread.
    let ch_pin: Gpio1<'static> = unsafe { core::mem::transmute(ch_pin) };
    let data_pin: Gpio2<'static> = unsafe { core::mem::transmute(data_pin) };

    std::thread::Builder::new()
        .stack_size(8192)
        .name("rf_recv".to_string())
        .spawn(move || match receiver_loop(ch_pin, data_pin) {
            Ok(()) => log::warn!("RF receiver loop exited unexpectedly"),
            Err(e) => log::error!("RF receiver fatal: {e}"),
        })
        .expect("rf_recv thread spawn failed");
}

// ── Main receive loop ────────────────────────────────────────────────────────

fn receiver_loop(ch_pin: Gpio1<'static>, data_pin: Gpio2<'static>) -> Result<(), EspError> {
    let mut ch = PinDriver::output(ch_pin)?;
    ch.set_high()?;
    log::info!("SRX882 enabled (CH = GPIO1 HIGH)");

    // RMT RX: 1 MHz resolution → 1 tick = 1 µs
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

    let mut buf = [Symbol::default(); 256];
    let mut prev_code: u32 = 0;
    let mut prev_time = std::time::Instant::now();

    log::info!("RF: listening for 433 MHz RC codes on GPIO2 …");

    loop {
        match rx.receive(&mut buf, &rx_cfg) {
            Ok(n) if n >= 25 => {
                let symbols = &buf[..n];
                if let Some((code, bits, proto)) = decode_rc(symbols) {
                    let now = std::time::Instant::now();
                    if code != prev_code
                        || now.duration_since(prev_time) > Duration::from_millis(400)
                    {
                        log::info!(
                            "RC code: 0x{code:06X} (dec: {code}, {bits} bits, proto: {proto})"
                        );
                        prev_code = code;
                        prev_time = now;
                    }
                } else {
                    dump_symbols(symbols);
                }
            }
            Ok(n) if n > 0 => {
                log::debug!("RF: short burst ({n} symbols), ignored");
            }
            Ok(_) => {}
            Err(e) if e.code() == esp_idf_svc::sys::ESP_ERR_TIMEOUT => {}
            Err(e) => {
                log::warn!("RF rx error: {e}");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

// ── Protocol decoding ────────────────────────────────────────────────────────

fn decode_rc(symbols: &[Symbol]) -> Option<(u32, u8, &'static str)> {
    decode_ev1527(symbols).map(|(c, b)| (c, b, "EV1527"))
}

/// EV1527 / PT2262 24-bit protocol.
///
/// Frame structure (each element is one RMT symbol):
///   sync : HIGH 1·T  +  LOW 31·T
///   bit 0: HIGH 1·T  +  LOW 3·T
///   bit 1: HIGH 3·T  +  LOW 1·T
///
/// T is typically 100–500 µs depending on the transmitter.
fn decode_ev1527(symbols: &[Symbol]) -> Option<(u32, u8)> {
    let needed = 25; // 1 sync + 24 data bits

    for start in 0..symbols.len().saturating_sub(needed) {
        let sync = symbols[start];
        let h = sync.level0();
        let l = sync.level1();

        if h.pin_state != PinState::High || l.pin_state != PinState::Low {
            continue;
        }

        let ht = h.ticks.ticks() as u32;
        let lt = l.ticks.ticks() as u32;

        // Plausible base period: 80–800 µs
        if ht < 80 || ht > 800 {
            continue;
        }
        // Sync ratio HIGH:LOW ≈ 1:31 (accept 1:10 … 1:50)
        let ratio = lt / ht.max(1);
        if !(10..=50).contains(&ratio) {
            continue;
        }

        let t = ht;

        if let Some(code) = try_decode_bits(symbols, start + 1, 24, t) {
            return Some((code, 24));
        }
    }
    None
}

fn try_decode_bits(symbols: &[Symbol], offset: usize, num_bits: usize, t: u32) -> Option<u32> {
    if offset + num_bits > symbols.len() {
        return None;
    }
    let mut code: u32 = 0;
    for i in 0..num_bits {
        let sym = symbols[offset + i];
        let bh = sym.level0().ticks.ticks() as u32;
        let bl = sym.level1().ticks.ticks() as u32;

        code <<= 1;
        if near(bh, t * 3, t) && near(bl, t, t) {
            code |= 1;
        } else if near(bh, t, t) && near(bl, t * 3, t) {
            // bit 0 — already shifted in as 0
        } else {
            return None;
        }
    }
    Some(code)
}

fn near(val: u32, expected: u32, base_t: u32) -> bool {
    let tol = (base_t * 3 / 4).max(80);
    val.abs_diff(expected) <= tol
}

// ── Debug helpers ────────────────────────────────────────────────────────────

fn dump_symbols(symbols: &[Symbol]) {
    if !log::log_enabled!(log::Level::Debug) {
        return;
    }
    let mut s = String::with_capacity(symbols.len() * 24);
    for (i, sym) in symbols.iter().enumerate() {
        let h = sym.level0();
        let l = sym.level1();
        use std::fmt::Write;
        let _ = write!(
            s,
            "\n  [{i:3}] H:{:5}us L:{:5}us",
            h.ticks.ticks(),
            l.ticks.ticks()
        );
    }
    log::debug!("RF raw symbols ({} total):{s}", symbols.len());
}
