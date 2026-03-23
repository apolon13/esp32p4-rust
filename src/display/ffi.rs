/// DIR_C2M | UNALIGNED — flush CPU cache to PSRAM, allow unaligned size
pub const CACHE_SYNC_FLAGS: i32 = (1 << 0) | (1 << 2);

extern "C" {
    /// Initialise display hardware and allocate double framebuffers.
    pub fn display_init();
    /// Return pointer to the current back buffer (safe to render into).
    pub fn display_back_buffer() -> *mut u8;
    /// Block until vsync confirms our frame is on screen, then swap indices.
    /// Must be called after esp_cache_msync() on the back buffer.
    pub fn display_swap_buffers();
    /// Try to wait for vsync for at most `timeout_ms` ms.
    /// Returns true if vsync occurred, false if timeout expired.
    pub fn display_wait_vsync_timeout(timeout_ms: u32) -> bool;
    pub fn display_backlight_on();
    pub fn display_backlight_off();
    pub fn esp_cache_msync(addr: *mut core::ffi::c_void, size: usize, flags: i32) -> i32;
}
