use std::rc::Rc;
use slint::platform::software_renderer::MinimalSoftwareWindow;

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
    let log_buffer = logger::install();
    let display    = display::Display::init();
    let mut touch  = touch::TouchController::init();
    let window     = platform::init(display::WIDTH, display::HEIGHT);

    let (wifi_worker, rf_receiver, device_store) = init_peripherals();

    let app = AppWindow::new().expect("failed to create AppWindow");
    app.show().expect("failed to show AppWindow");

    let wifi_screen = screens::wifi::WifiScreenHandler::new(&app, wifi_worker);
    let rc_screen   = screens::rc_devices::RcDevicesScreenHandler::new(&app, rf_receiver, device_store);
    let log_screen  = screens::logs::LogScreenHandler::new(&app, log_buffer);

    prime_display(&display, &window);
    display.backlight_on();

    unsafe { esp_idf_svc::sys::esp_task_wdt_add(core::ptr::null_mut()); }
    run_loop(&mut touch, &window, &display, &wifi_screen, &rc_screen, &log_screen);
}

fn init_peripherals() -> (wifi::WifiWorker, rf_receiver::RfReceiver, rc_devices::DeviceStore) {
    use esp_idf_svc::{
        eventloop::EspSystemEventLoop,
        hal::peripherals::Peripherals,
        nvs::EspDefaultNvsPartition,
    };
    let p = Peripherals::take().expect("peripherals taken");
    let s = EspSystemEventLoop::take().expect("event loop taken");
    let n = EspDefaultNvsPartition::take().expect("NVS taken");
    let rf_receiver  = rf_receiver::RfReceiver::spawn(p.pins.gpio1, p.pins.gpio2);
    let device_store = rc_devices::DeviceStore::open(n.clone());
    let wifi_worker  = wifi::WifiWorker::spawn(p.modem, s, n);
    (wifi_worker, rf_receiver, device_store)
}

fn prime_display(display: &display::Display, window: &Rc<MinimalSoftwareWindow>) {
    for _ in 0..2 {
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            display.render(|fb, stride| { renderer.render(fb, stride); });
        });
        display.sync_vsync();
    }
}

fn run_loop(
    touch:       &mut touch::TouchController,
    window:      &Rc<MinimalSoftwareWindow>,
    display:     &display::Display,
    wifi_screen: &screens::wifi::WifiScreenHandler,
    rc_screen:   &screens::rc_devices::RcDevicesScreenHandler,
    log_screen:  &screens::logs::LogScreenHandler,
) {
    loop {
        poll_all(touch, window, wifi_screen, rc_screen, log_screen);
        if !try_render(display, window) {
            while !display.try_vsync_timeout(4) {
                touch.poll(window);
                slint::platform::update_timers_and_animations();
            }
        }
        unsafe { esp_idf_svc::sys::esp_task_wdt_reset(); }
    }
}

fn poll_all(
    touch:       &mut touch::TouchController,
    window:      &Rc<MinimalSoftwareWindow>,
    wifi_screen: &screens::wifi::WifiScreenHandler,
    rc_screen:   &screens::rc_devices::RcDevicesScreenHandler,
    log_screen:  &screens::logs::LogScreenHandler,
) {
    touch.poll(window);
    slint::platform::update_timers_and_animations();
    wifi_screen.poll();
    rc_screen.poll();
    log_screen.poll();
}

fn try_render(display: &display::Display, window: &Rc<MinimalSoftwareWindow>) -> bool {
    let mut rendered = false;
    window.draw_if_needed(|renderer| {
        rendered = true;
        display.render(|fb, stride| { renderer.render(fb, stride); });
    });
    if rendered { display.sync_vsync(); }
    rendered
}
