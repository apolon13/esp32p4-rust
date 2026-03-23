mod ffi;
mod framebuffer;

pub use framebuffer::{Framebuffer, HEIGHT, WIDTH};

pub struct Display {
    fb: Framebuffer,
}

impl Display {
    pub fn init() -> Self {
        unsafe { ffi::display_init() };
        Self { fb: Framebuffer::new() }
    }

    pub fn backlight_on(&self) {
        unsafe { ffi::display_backlight_on() };
    }

    pub fn backlight_off(&self) {
        unsafe { ffi::display_backlight_off() };
    }

    pub fn render<F>(&self, f: F)
    where
        F: FnOnce(&mut [slint::platform::software_renderer::Rgb565Pixel], usize),
    {
        self.fb.render(f);
    }

    /// Block until the next vsync.  Call once per render-loop iteration.
    pub fn sync_vsync(&self) {
        self.fb.sync_vsync();
    }

    /// Try to wait for vsync for at most `timeout_ms` ms.
    /// Returns `true` if vsync occurred, `false` on timeout.
    pub fn try_vsync_timeout(&self, timeout_ms: u32) -> bool {
        self.fb.try_vsync_timeout(timeout_ms)
    }
}
