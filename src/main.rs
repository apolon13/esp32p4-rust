fn main() {
    esp_idf_svc::sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("Hello from ESP32-P4!");

    let mut counter: u32 = 0;
    loop {
        log::info!("[tick {}]", counter);
        counter = counter.wrapping_add(1);
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}
