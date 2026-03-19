use slint::platform::software_renderer::Rgb565Pixel;

use super::ffi::{self, CACHE_SYNC_FLAGS};

pub const WIDTH:  u32 = 1024;
pub const HEIGHT: u32 = 600;

const BYTE_LEN:  usize = (WIDTH * HEIGHT * 2) as usize;
const PIXEL_LEN: usize = (WIDTH * HEIGHT) as usize;

pub struct Framebuffer;

impl Framebuffer {
    pub(super) fn new() -> Self {
        // Clear both buffers so the display starts black.
        // display_swap_buffers() waits for vsync, so this takes ~2 frame periods.
        unsafe {
            for _ in 0..2 {
                let ptr = ffi::display_back_buffer();
                core::ptr::write_bytes(ptr, 0, BYTE_LEN);
                ffi::esp_cache_msync(ptr as *mut _, BYTE_LEN, CACHE_SYNC_FLAGS);
                ffi::display_swap_buffers();
            }
        }
        Self
    }

    /// Render a frame into the back buffer and flush the CPU cache.
    /// The caller must invoke `Display::sync_vsync()` once per loop iteration
    /// to pace rendering and keep the buffer index in sync with the hardware.
    pub fn render<F>(&self, f: F)
    where
        F: FnOnce(&mut [Rgb565Pixel], usize),
    {
        unsafe {
            let ptr = ffi::display_back_buffer();
            let fb  = core::slice::from_raw_parts_mut(ptr as *mut Rgb565Pixel, PIXEL_LEN);
            f(fb, WIDTH as usize);
            ffi::esp_cache_msync(ptr as *mut _, BYTE_LEN, CACHE_SYNC_FLAGS);
        }
    }

    /// Block until the next vsync.  Must be called exactly once per render-loop
    /// iteration whether or not a frame was rendered, to keep `display_back_buffer()`
    /// in sync with the hardware ping-pong.
    pub(super) fn sync_vsync(&self) {
        unsafe { ffi::display_swap_buffers(); }
    }

    /// Try to wait for vsync for at most `timeout_ms` milliseconds.
    /// Returns `true` if vsync occurred, `false` if the timeout expired.
    /// Use in a polling loop to interleave touch sampling while waiting for vsync.
    pub(super) fn try_vsync_timeout(&self, timeout_ms: u32) -> bool {
        unsafe { ffi::display_wait_vsync_timeout(timeout_ms) }
    }
}
