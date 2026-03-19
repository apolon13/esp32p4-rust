use slint::platform::{
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
    Platform, WindowAdapter,
};
use std::rc::Rc;

struct EspPlatform {
    window: Rc<MinimalSoftwareWindow>,
}

impl Platform for EspPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> core::time::Duration {
        core::time::Duration::from_micros(unsafe {
            esp_idf_svc::sys::esp_timer_get_time() as u64
        })
    }
}

/// Initialise the Slint platform and return the window handle.
pub fn init(width: u32, height: u32) -> Rc<MinimalSoftwareWindow> {
    // SwappedBuffers: Slint accumulates dirty regions across two consecutive
    // renders so both physical framebuffers stay consistent with the current
    // UI state.  The UI uses `visible` (not `if`) for page transitions so
    // Slint's dirty-region tracking is never confused by element creation /
    // destruction, which was the original cause of stale-frame flashes.
    let window = MinimalSoftwareWindow::new(RepaintBufferType::SwappedBuffers);
    window.set_size(slint::PhysicalSize::new(width, height));
    slint::platform::set_platform(Box::new(EspPlatform { window: window.clone() }))
        .expect("Slint platform already set");
    window
}
