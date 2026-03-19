/*
 * Display initialisation for JC1060P470C_I_W_Y board
 * ESP32-P4 + JD9165  7" IPS  1024×600  MIPI-DSI 2-lane
 *
 * Double-buffering:
 *   Two PSRAM framebuffers are allocated.  The DPI controller ping-pongs
 *   between them automatically every frame (~60 Hz).  display_swap_buffers()
 *   blocks until the vsync that confirms the newly written frame started
 *   being displayed, then flips the back-buffer index so the CPU always
 *   writes to the buffer that the DMA is NOT currently reading.
 *
 * Pin map
 *   GPIO23  — backlight (LEDC PWM)
 *   GPIO5   — LCD_RST
 *   LDO ch3 — MIPI-DSI PHY power (2 500 mV)
 */

#include "display_init.h"
#include "esp_lcd_jd9165.h"

#include <stdbool.h>
#include "driver/ledc.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_panel_ops.h"
#include "esp_lcd_mipi_dsi.h"
#include "esp_ldo_regulator.h"
#include "freertos/FreeRTOS.h"
#include "freertos/semphr.h"

#define BSP_MIPI_DSI_PHY_LDO_CHAN    3
#define BSP_MIPI_DSI_PHY_LDO_MV     2500
#define BSP_LCD_RST_GPIO             5
#define BSP_LCD_BACKLIGHT_GPIO       23
#define LCD_LEDC_CHANNEL             LEDC_CHANNEL_0
#define LCD_LEDC_TIMER               LEDC_TIMER_1

static esp_ldo_channel_handle_t  phy_pwr_chan  = NULL;
static esp_lcd_dsi_bus_handle_t  mipi_dsi_bus  = NULL;
static esp_lcd_panel_io_handle_t mipi_dbi_io   = NULL;
static esp_lcd_panel_handle_t    display_panel = NULL;

/* ── double-buffering state ─────────────────────────────────────────────── */

static void             *s_fb[2]     = {NULL, NULL};
/* Index of the buffer the DPI is currently displaying (front buffer).
 * Updated only in display_swap_buffers() after esp_lcd_panel_draw_bitmap()
 * commits a new frame and the refresh-done ISR confirms the switch. */
static volatile int      s_front_idx = 0;
static SemaphoreHandle_t s_vsync_sem = NULL;

/* Called from the DPI interrupt when DMA finishes one full refresh cycle. */
static bool IRAM_ATTR on_refresh_done(esp_lcd_panel_handle_t panel,
                                      esp_lcd_dpi_panel_event_data_t *edata,
                                      void *user_ctx)
{
    BaseType_t woken = pdFALSE;
    xSemaphoreGiveFromISR(s_vsync_sem, &woken);
    return woken == pdTRUE;
}

/* ── backlight ──────────────────────────────────────────────────────────── */

static void backlight_ledc_init(void)
{
    ledc_timer_config_t tc = {
        .speed_mode      = LEDC_LOW_SPEED_MODE,
        .timer_num       = LCD_LEDC_TIMER,
        .duty_resolution = LEDC_TIMER_10_BIT,
        .freq_hz         = 5000,
        .clk_cfg         = LEDC_AUTO_CLK,
    };
    ESP_ERROR_CHECK(ledc_timer_config(&tc));

    ledc_channel_config_t cc = {
        .gpio_num   = BSP_LCD_BACKLIGHT_GPIO,
        .speed_mode = LEDC_LOW_SPEED_MODE,
        .channel    = LCD_LEDC_CHANNEL,
        .timer_sel  = LCD_LEDC_TIMER,
        .duty       = 0,
        .hpoint     = 0,
    };
    ESP_ERROR_CHECK(ledc_channel_config(&cc));
}

void display_backlight_on(void)
{
    ledc_set_duty(LEDC_LOW_SPEED_MODE, LCD_LEDC_CHANNEL, 1023);
    ledc_update_duty(LEDC_LOW_SPEED_MODE, LCD_LEDC_CHANNEL);
}

void display_backlight_off(void)
{
    ledc_set_duty(LEDC_LOW_SPEED_MODE, LCD_LEDC_CHANNEL, 0);
    ledc_update_duty(LEDC_LOW_SPEED_MODE, LCD_LEDC_CHANNEL);
}

/* ── double-buffer API ──────────────────────────────────────────────────── */

/* Returns the buffer the DMA is NOT currently reading — always safe to write. */
void *display_back_buffer(void)
{
    return s_fb[s_front_idx ^ 1];
}

/*
 * Commit the current back buffer to the display and wait for the switch.
 *
 * 1. Drain any stale vsync semaphore so we wait for a fresh one.
 * 2. Tell the DPI controller to display our back buffer starting next frame.
 * 3. Wait for the refresh-done ISR that confirms the new frame is on screen.
 * 4. Flip the tracking index so display_back_buffer() returns the other buffer.
 */
void display_swap_buffers(void)
{
    void *back = s_fb[s_front_idx ^ 1];

    while (xSemaphoreTake(s_vsync_sem, 0) == pdTRUE) {}

    ESP_ERROR_CHECK(esp_lcd_panel_draw_bitmap(display_panel, 0, 0, 1024, 600, back));

    xSemaphoreTake(s_vsync_sem, portMAX_DELAY);

    s_front_idx ^= 1;
}

bool display_wait_vsync_timeout(uint32_t timeout_ms)
{
    return xSemaphoreTake(s_vsync_sem, pdMS_TO_TICKS(timeout_ms)) == pdTRUE;
}

/* ── main init ──────────────────────────────────────────────────────────── */

void display_init(void)
{
    backlight_ledc_init();

    /* 1. Power on MIPI-DSI PHY via on-chip LDO */
    esp_ldo_channel_config_t ldo_cfg = {
        .chan_id    = BSP_MIPI_DSI_PHY_LDO_CHAN,
        .voltage_mv = BSP_MIPI_DSI_PHY_LDO_MV,
    };
    ESP_ERROR_CHECK(esp_ldo_acquire_channel(&ldo_cfg, &phy_pwr_chan));

    /* 2. MIPI-DSI bus  (2 lanes, 750 Mbps) */
    esp_lcd_dsi_bus_config_t bus_cfg = JD9165_PANEL_BUS_DSI_2CH_CONFIG();
    ESP_ERROR_CHECK(esp_lcd_new_dsi_bus(&bus_cfg, &mipi_dsi_bus));

    /* 3. DBI panel IO for register commands */
    esp_lcd_dbi_io_config_t dbi_cfg = JD9165_PANEL_IO_DBI_CONFIG();
    ESP_ERROR_CHECK(esp_lcd_new_panel_io_dbi(mipi_dsi_bus, &dbi_cfg, &mipi_dbi_io));

    /* 4. DPI video timing for 1024×600 @ ~60 Hz, RGB565 */
    esp_lcd_dpi_panel_config_t dpi_cfg =
        JD9165_1024_600_PANEL_60HZ_DPI_CONFIG(LCD_COLOR_PIXEL_FORMAT_RGB565);
    dpi_cfg.num_fbs = 2;

    /* 5. JD9165 vendor config */
    jd9165_vendor_config_t vendor_cfg = {
        .init_cmds      = NULL,
        .init_cmds_size = 0,
        .mipi_config    = {
            .dsi_bus    = mipi_dsi_bus,
            .dpi_config = &dpi_cfg,
        },
    };

    esp_lcd_panel_dev_config_t panel_cfg = {
        .reset_gpio_num = BSP_LCD_RST_GPIO,
        .rgb_ele_order  = LCD_RGB_ELEMENT_ORDER_RGB,
        .bits_per_pixel = 16,
        .vendor_config  = &vendor_cfg,
    };

    ESP_ERROR_CHECK(esp_lcd_new_panel_jd9165(mipi_dbi_io, &panel_cfg, &display_panel));
    esp_lcd_panel_reset(display_panel);
    esp_lcd_panel_init(display_panel);

    /* 6. Two DMA framebuffers (ping-pong, both in PSRAM) */
    ESP_ERROR_CHECK(
        esp_lcd_dpi_panel_get_frame_buffer(display_panel, 2, &s_fb[0], &s_fb[1])
    );

    /* 7. Vsync semaphore + refresh-done callback */
    s_vsync_sem = xSemaphoreCreateBinary();
    assert(s_vsync_sem != NULL);

    esp_lcd_dpi_panel_event_callbacks_t cbs = {
        .on_refresh_done = on_refresh_done,
    };
    ESP_ERROR_CHECK(
        esp_lcd_dpi_panel_register_event_callbacks(display_panel, &cbs, NULL)
    );
}
