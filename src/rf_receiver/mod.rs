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

// ── Public types ────────────────────────────────────────────────────────────

/// Successfully decoded RF code with protocol metadata.
#[derive(Debug, Clone)]
pub struct RfCode {
    pub code: u64,
    pub bit_count: u8,
    pub protocol: &'static str,
}

// ── Public API ──────────────────────────────────────────────────────────────

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

// ── Protocol table ──────────────────────────────────────────────────────────
//
// Each entry describes OOK timing in multiples of a base period T:
//   sync  = HIGH(sync_high·T) + LOW(sync_low·T)
//   bit 0 = HIGH(zero_high·T) + LOW(zero_low·T)
//   bit 1 = HIGH(one_high·T)  + LOW(one_low·T)
//
// The decoder derives T from the observed sync pulse length and checks that
// it falls within [t_min, t_max] µs.  Protocols are tried in order — put the
// most common / most distinctive first.

struct Protocol {
    name: &'static str,
    sync_high: u32,
    sync_low: u32,
    zero_high: u32,
    zero_low: u32,
    one_high: u32,
    one_low: u32,
    bit_count: u8,
    t_min: u32,
    t_max: u32,
}

static PROTOCOLS: &[Protocol] = &[
    // ── 24-bit protocols (sensors, PIR, door/window, remotes) ───────────
    //
    // EV1527 / PT2262 — by far the most widespread 433 MHz encoding.
    // Used in: generic door/window sensors, PIR motion detectors, smoke
    // detectors, water leak sensors, cheap security remotes.
    // Sync 1:31, Bit0 1:3, Bit1 3:1, T ≈ 100–500 µs
    Protocol {
        name: "EV1527",
        sync_high: 1,
        sync_low: 31,
        zero_high: 1,
        zero_low: 3,
        one_high: 3,
        one_low: 1,
        bit_count: 24,
        t_min: 80,
        t_max: 800,
    },
    // High-resolution variant — some smoke / gas sensors.
    // Sync 1:71, Bit0 4:11, Bit1 9:6
    Protocol {
        name: "EV1527-HR",
        sync_high: 1,
        sync_low: 71,
        zero_high: 4,
        zero_low: 11,
        one_high: 9,
        one_low: 6,
        bit_count: 24,
        t_min: 50,
        t_max: 400,
    },
    // HS2303-PT — long-HIGH sync, standalone PIR sensors.
    // Sync 36:1, Bit0 1:2, Bit1 2:1
    Protocol {
        name: "HS2303",
        sync_high: 36,
        sync_low: 1,
        zero_high: 1,
        zero_low: 2,
        one_high: 2,
        one_low: 1,
        bit_count: 24,
        t_min: 50,
        t_max: 500,
    },
    // PT2262 variant — some branded door/window & motion sensors.
    // Sync 1:10, Bit0 1:2, Bit1 2:1
    Protocol {
        name: "PT2262v2",
        sync_high: 1,
        sync_low: 10,
        zero_high: 1,
        zero_low: 2,
        one_high: 2,
        one_low: 1,
        bit_count: 24,
        t_min: 200,
        t_max: 1200,
    },
    // SC2262 / SC5262 — older alarm panels, keyfobs.
    // Sync 1:10, Bit0 1:3, Bit1 3:1
    Protocol {
        name: "SC2262",
        sync_high: 1,
        sync_low: 10,
        zero_high: 1,
        zero_low: 3,
        one_high: 3,
        one_low: 1,
        bit_count: 24,
        t_min: 100,
        t_max: 700,
    },
    // Medium-sync variant — Kerui / Sonoff-compatible sensors.
    // Sync 1:23, Bit0 1:2, Bit1 2:1
    Protocol {
        name: "Kerui",
        sync_high: 1,
        sync_low: 23,
        zero_high: 1,
        zero_low: 2,
        one_high: 2,
        one_low: 1,
        bit_count: 24,
        t_min: 150,
        t_max: 900,
    },
    // Short-sync variant — some cheap Chinese security kits.
    // Sync 1:6, Bit0 1:3, Bit1 3:1
    Protocol {
        name: "ShortSync",
        sync_high: 1,
        sync_low: 6,
        zero_high: 1,
        zero_low: 3,
        one_high: 3,
        one_low: 1,
        bit_count: 24,
        t_min: 100,
        t_max: 800,
    },
    // ── 28-bit protocols ────────────────────────────────────────────────
    //
    // HT6P20B — Sonoff / eWeLink security remotes (22 addr + 2 data + 4 anti).
    // Sync 10:40, Bit0 1:5, Bit1 3:3
    Protocol {
        name: "HT6P20B",
        sync_high: 10,
        sync_low: 40,
        zero_high: 1,
        zero_low: 5,
        one_high: 3,
        one_low: 3,
        bit_count: 28,
        t_min: 100,
        t_max: 900,
    },
    // ── 12-bit protocols (gate remotes, keypads) ────────────────────────
    //
    // Nice FLO — gate / garage remotes.
    // Sync 1:36, Bit0 1:3, Bit1 3:1
    Protocol {
        name: "NiceFLO",
        sync_high: 1,
        sync_low: 36,
        zero_high: 1,
        zero_low: 3,
        one_high: 3,
        one_low: 1,
        bit_count: 12,
        t_min: 200,
        t_max: 900,
    },
    // Holtek HT12E — security keypads, simple wireless remotes.
    // Sync 1:36, Bit0 1:2, Bit1 2:1
    Protocol {
        name: "HT12E",
        sync_high: 1,
        sync_low: 36,
        zero_high: 1,
        zero_low: 2,
        one_high: 2,
        one_low: 1,
        bit_count: 12,
        t_min: 100,
        t_max: 600,
    },
    // CAME TOP — 12-bit gate / barrier remotes.
    // Sync 1:18, Bit0 1:3, Bit1 3:1
    Protocol {
        name: "CAME",
        sync_high: 1,
        sync_low: 18,
        zero_high: 1,
        zero_low: 3,
        one_high: 3,
        one_low: 1,
        bit_count: 12,
        t_min: 200,
        t_max: 700,
    },
];

// ── Main receive loop ───────────────────────────────────────────────────────

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
    let mut prev_code: u64 = 0;
    let mut prev_proto: &str = "";
    let mut prev_time = std::time::Instant::now();

    log::info!(
        "RF: listening for 433 MHz codes on GPIO2 ({} protocols loaded)",
        PROTOCOLS.len(),
    );

    loop {
        match rx.receive(&mut buf, &rx_cfg) {
            Ok(n) if n >= 13 => {
                let symbols = &buf[..n];
                if let Some(rc) = decode_rc(symbols) {
                    let now = std::time::Instant::now();
                    let is_repeat = rc.code == prev_code
                        && rc.protocol == prev_proto
                        && now.duration_since(prev_time) < Duration::from_millis(400);

                    if !is_repeat {
                        log::info!(
                            "RF [{proto}] code=0x{code:0width$X} (dec={code}, {bits}bit)",
                            proto = rc.protocol,
                            code = rc.code,
                            bits = rc.bit_count,
                            width = ((rc.bit_count as usize) + 3) / 4,
                        );
                        prev_code = rc.code;
                        prev_proto = rc.protocol;
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

// ── Generic table-driven decoder ────────────────────────────────────────────

fn decode_rc(symbols: &[Symbol]) -> Option<RfCode> {
    for proto in PROTOCOLS {
        if let Some(rc) = try_protocol(symbols, proto) {
            return Some(rc);
        }
    }
    None
}

/// Try to decode `symbols` using a single protocol definition.
fn try_protocol(symbols: &[Symbol], proto: &'static Protocol) -> Option<RfCode> {
    let needed = 1 + proto.bit_count as usize;

    for start in 0..symbols.len().saturating_sub(needed) {
        let sync = symbols[start];
        let h = sync.level0();
        let l = sync.level1();

        if h.pin_state != PinState::High || l.pin_state != PinState::Low {
            continue;
        }

        let ht = h.ticks.ticks() as u32;
        let lt = l.ticks.ticks() as u32;

        // Derive T from the total sync duration.
        let sync_units = proto.sync_high + proto.sync_low;
        let t = (ht + lt) / sync_units;

        if t < proto.t_min || t > proto.t_max {
            continue;
        }

        // Verify sync HIGH and LOW match the expected multiples of T.
        let sync_tol = (t * 2).max(100);
        if !near(ht, t * proto.sync_high, sync_tol)
            || !near(lt, t * proto.sync_low, sync_tol)
        {
            continue;
        }

        if let Some(code) =
            decode_bits(symbols, start + 1, proto.bit_count as usize, t, proto)
        {
            return Some(RfCode {
                code,
                bit_count: proto.bit_count,
                protocol: proto.name,
            });
        }
    }
    None
}

/// Attempt to decode `num_bits` data symbols starting at `offset`.
fn decode_bits(
    symbols: &[Symbol],
    offset: usize,
    num_bits: usize,
    t: u32,
    proto: &Protocol,
) -> Option<u64> {
    if offset + num_bits > symbols.len() {
        return None;
    }

    let tol = (t * 3 / 4).max(80);
    let mut code: u64 = 0;

    for i in 0..num_bits {
        let sym = symbols[offset + i];
        let bh = sym.level0().ticks.ticks() as u32;
        let bl = sym.level1().ticks.ticks() as u32;

        code <<= 1;
        if near(bh, t * proto.one_high, tol) && near(bl, t * proto.one_low, tol) {
            code |= 1;
        } else if near(bh, t * proto.zero_high, tol)
            && near(bl, t * proto.zero_low, tol)
        {
            // bit 0 — already shifted in
        } else {
            return None;
        }
    }
    Some(code)
}

#[inline]
fn near(val: u32, expected: u32, tolerance: u32) -> bool {
    val.abs_diff(expected) <= tolerance
}

// ── Debug helpers ───────────────────────────────────────────────────────────

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
