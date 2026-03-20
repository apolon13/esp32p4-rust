slint::include_modules!();

mod display;
mod platform;
mod screens;
mod touch;
mod wifi;

fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    // ── Hardware ──────────────────────────────────────────────────
    let display   = display::Display::init();
    let mut touch = touch::TouchController::init();
    let window    = platform::init(display::WIDTH, display::HEIGHT);

    // ── WiFi worker — spawned on Core 1, never blocks the render loop ─
    let wifi_worker = spawn_wifi_worker();

    // ── UI ────────────────────────────────────────────────────────
    let app = AppWindow::new().expect("failed to create AppWindow");
    app.show().expect("failed to show AppWindow");

    // ── Screen handlers ───────────────────────────────────────────
    let wifi_screen = screens::wifi::WifiScreenHandler::new(&app, wifi_worker);

    // Pre-fill both physical framebuffers before turning on the backlight.
    // SwappedBuffers needs two renders so neither buffer contains raw PSRAM.
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
    //
    // RepaintBufferType::SwappedBuffers: Slint internally accumulates dirty
    // regions across two consecutive draw calls, so both physical framebuffers
    // are always brought up to date after any UI change.
    //
    // After rendering, display.sync_vsync() commits the back buffer to the
    // DPI controller via esp_lcd_panel_draw_bitmap and waits for the refresh
    // cycle that confirms the switch.  When idle (no render), we pace the
    // loop to vsync with try_vsync_timeout, polling touch every 4 ms.
    loop {
        touch.poll(&window);
        slint::platform::update_timers_and_animations();
        wifi_screen.poll();

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

fn spawn_wifi_worker() -> wifi::WifiWorker {
    use esp_idf_svc::{
        eventloop::EspSystemEventLoop,
        hal::peripherals::Peripherals,
        nvs::EspDefaultNvsPartition,
    };
    let p = Peripherals::take().expect("peripherals taken");
    let s = EspSystemEventLoop::take().expect("event loop taken");
    let n = EspDefaultNvsPartition::take().expect("NVS taken");
    wifi::WifiWorker::spawn(p.modem, s, n)
}
