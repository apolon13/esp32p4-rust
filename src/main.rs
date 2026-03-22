slint::include_modules!();

mod display;
mod logger;
mod platform;
mod rc_devices;
mod rf_receiver;
mod screens;
mod touch;
mod wifi;

fn main() {
    // ── Логгер (до всего остального, чтобы поймать каждое сообщение) ─
    let log_buffer = logger::install();

    // ── Hardware ──────────────────────────────────────────────────
    let display   = display::Display::init();
    let mut touch = touch::TouchController::init();
    let window    = platform::init(display::WIDTH, display::HEIGHT);

    // ── Peripherals ───────────────────────────────────────────────
    let (wifi_worker, rf_receiver, device_store) = {
        use esp_idf_svc::{
            eventloop::EspSystemEventLoop,
            hal::peripherals::Peripherals,
            nvs::EspDefaultNvsPartition,
        };
        let p = Peripherals::take().expect("peripherals taken");
        let s = EspSystemEventLoop::take().expect("event loop taken");
        let n = EspDefaultNvsPartition::take().expect("NVS taken");

        // SRX882 433 MHz receiver: GPIO1 = CH (enable), GPIO2 = DATA.
        let rf_receiver = rf_receiver::RfReceiver::spawn(p.pins.gpio1, p.pins.gpio2);

        // RF device store — открываем NVS до передачи partition в WiFi.
        let device_store = rc_devices::DeviceStore::open(n.clone());

        // WiFi worker — spawned on Core 1, never blocks the render loop.
        let wifi_worker = wifi::WifiWorker::spawn(p.modem, s, n);
        (wifi_worker, rf_receiver, device_store)
    };

    // ── UI ────────────────────────────────────────────────────────
    let app = AppWindow::new().expect("failed to create AppWindow");
    app.show().expect("failed to show AppWindow");

    // ── Screen handlers ───────────────────────────────────────────
    let wifi_screen = screens::wifi::WifiScreenHandler::new(&app, wifi_worker);
    let rc_screen   = screens::rc_devices::RcDevicesScreenHandler::new(&app, rf_receiver, device_store);
    let log_screen  = screens::logs::LogScreenHandler::new(&app, log_buffer);

    // Pre-fill both physical framebuffers before turning on the backlight.
    for _ in 0..2 {
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            display.render(|fb, stride| { renderer.render(fb, stride); });
        });
        display.sync_vsync();
    }

    // ── Backlight on ──────────────────────────────────────────────
    display.backlight_on();

    unsafe {
        esp_idf_svc::sys::esp_task_wdt_add(core::ptr::null_mut());
    }

    // ── Render loop (Core 0) ──────────────────────────────────────
    loop {
        touch.poll(&window);
        slint::platform::update_timers_and_animations();
        wifi_screen.poll();
        rc_screen.poll();
        log_screen.poll();

        let mut rendered = false;
        window.draw_if_needed(|renderer| {
            rendered = true;
            display.render(|fb, stride| { renderer.render(fb, stride); });
        });

        if rendered {
            display.sync_vsync();
        } else {
            while !display.try_vsync_timeout(4) {
                touch.poll(&window);
                slint::platform::update_timers_and_animations();
            }
        }
        unsafe { esp_idf_svc::sys::esp_task_wdt_reset(); }
    }
}
