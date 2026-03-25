use std::rc::Rc;
use slint::platform::software_renderer::MinimalSoftwareWindow;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use std::sync::{Arc, Mutex};

slint::include_modules!();

mod control;
mod display;
mod gsm;
mod logger;
mod mqtt;
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

    let app = AppWindow::new().expect("failed to create AppWindow");
    app.show().expect("failed to show AppWindow");

    let (wifi_screen, rc_screen, log_screen, mqtt_screen, security, sim_screen) =
        init_screens(&app, log_buffer);

    prime_display(&display, &window);
    display.backlight_on();

    unsafe { esp_idf_svc::sys::esp_task_wdt_add(core::ptr::null_mut()); }
    run_loop(&mut touch, &window, &display,
             &wifi_screen, &rc_screen, &log_screen, &mqtt_screen, &security, &sim_screen);
}

fn init_screens(
    app:        &AppWindow,
    log_buffer: Arc<Mutex<logger::LogBuffer>>,
) -> (
    screens::wifi::WifiScreenHandler,
    screens::rc_devices::RcDevicesScreenHandler,
    screens::logs::LogScreenHandler,
    screens::mqtt::MqttScreenHandler,
    screens::security::SecurityHandler,
    screens::sim::SimScreenHandler,
) {
    let (wifi_worker, rf_receiver, device_store, nvs, gsm) = init_peripherals();
    let (wifi_notify_tx, wifi_notify_rx) = std::sync::mpsc::sync_channel::<bool>(4);
    let (cmd_tx, cmd_rx) = std::sync::mpsc::sync_channel::<control::ControlCmd>(16);

    gsm.connect_cmd(cmd_tx.clone());

    let mqtt_worker = mqtt::MqttWorker::spawn(nvs.clone(), wifi_notify_rx);
    let event_pub   = mqtt_worker.event_publisher();
    let wifi_screen = screens::wifi::WifiScreenHandler::new(app, wifi_worker, Some(wifi_notify_tx));
    let rc_screen   = screens::rc_devices::RcDevicesScreenHandler::new(app, rf_receiver, device_store, cmd_tx.clone());
    let log_screen  = screens::logs::LogScreenHandler::new(app, log_buffer);
    let mqtt_screen = screens::mqtt::MqttScreenHandler::new(app, mqtt_worker, cmd_tx);
    let _settings   = screens::settings::SettingsScreenHandler::new(app, &nvs);
    let security    = screens::security::SecurityHandler::new(app, cmd_rx, event_pub);
    let sim_screen  = screens::sim::SimScreenHandler::new(app, &gsm, nvs);
    (wifi_screen, rc_screen, log_screen, mqtt_screen, security, sim_screen)
}

fn init_peripherals() -> (
    wifi::WifiWorker,
    rf_receiver::RfReceiver,
    rc_devices::DeviceStore,
    EspDefaultNvsPartition,
    gsm::GsmMonitor,
) {
    use esp_idf_svc::{
        eventloop::EspSystemEventLoop,
        hal::peripherals::Peripherals,
    };
    let p = Peripherals::take().expect("peripherals taken");
    let s = EspSystemEventLoop::take().expect("event loop taken");
    let n = EspDefaultNvsPartition::take().expect("NVS taken");
    let rf_receiver  = rf_receiver::RfReceiver::spawn(p.pins.gpio1, p.pins.gpio2);
    let gsm          = gsm::GsmMonitor::spawn(p.uart1, p.pins.gpio3, p.pins.gpio4);
    let device_store = rc_devices::DeviceStore::open(n.clone());
    let wifi_worker  = wifi::WifiWorker::spawn(p.modem, s, n.clone());
    (wifi_worker, rf_receiver, device_store, n, gsm)
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
    mqtt_screen: &screens::mqtt::MqttScreenHandler,
    security:    &screens::security::SecurityHandler,
    sim_screen:  &screens::sim::SimScreenHandler,
) {
    loop {
        poll_all(touch, window, display, wifi_screen, rc_screen, log_screen, mqtt_screen, security, sim_screen);
        if !try_render(display, window) {
            while !display.try_vsync_timeout(4) {
                let t = touch.poll(window);
                if t { security.poll(display, true); }
                slint::platform::update_timers_and_animations();
            }
        }
        unsafe { esp_idf_svc::sys::esp_task_wdt_reset(); }
    }
}

fn poll_all(
    touch:       &mut touch::TouchController,
    window:      &Rc<MinimalSoftwareWindow>,
    display:     &display::Display,
    wifi_screen: &screens::wifi::WifiScreenHandler,
    rc_screen:   &screens::rc_devices::RcDevicesScreenHandler,
    log_screen:  &screens::logs::LogScreenHandler,
    mqtt_screen: &screens::mqtt::MqttScreenHandler,
    security:    &screens::security::SecurityHandler,
    sim_screen:  &screens::sim::SimScreenHandler,
) -> bool {
    let touched = touch.poll(window);
    slint::platform::update_timers_and_animations();
    wifi_screen.poll();
    rc_screen.poll();
    log_screen.poll();
    mqtt_screen.poll();
    sim_screen.poll();
    security.poll(display, touched);
    touched
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
