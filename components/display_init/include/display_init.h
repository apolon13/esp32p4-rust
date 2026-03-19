#pragma once

#include <stdbool.h>
#include <stdint.h>

/*
 * Initialise the JD9165 7" 1024×600 MIPI-DSI display.
 *
 * Allocates two PSRAM framebuffers for tear-free double-buffering.
 * Call display_back_buffer() to get the buffer to render into, then
 * esp_cache_msync(), then display_swap_buffers() to present the frame.
 *
 * The backlight is left OFF after init; call display_backlight_on() once
 * the first frame has been rendered.
 */
void display_init(void);

/* Returns a pointer to the current back buffer (safe to write). */
void *display_back_buffer(void);

/*
 * Wait for the vsync that confirms the DPI hardware has started reading the
 * buffer written since the last call, then flip the back-buffer index.
 * Must be called after esp_cache_msync() on the back buffer.
 */
void display_swap_buffers(void);

/*
 * Try to wait for the next vsync for at most `timeout_ms` milliseconds.
 * Returns true if vsync occurred, false if the timeout expired first.
 * Use this instead of display_swap_buffers() when you want to interleave
 * work (e.g. touch polling) while waiting for vsync.
 */
bool display_wait_vsync_timeout(uint32_t timeout_ms);

void display_backlight_on(void);
void display_backlight_off(void);
