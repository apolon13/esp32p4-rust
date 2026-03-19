/*
 * GT911 touch controller initialisation
 * I2C0 — SDA: GPIO7, SCL: GPIO8 — 100 kHz
 * I2C address: 0x5D
 * NOTE: I2C_NUM_1 is reserved by esp_hosted/esp_wifi_remote (old driver).
 *       Using I2C_NUM_0 with the new master driver avoids the conflict.
 */
#include "touch_init.h"
#include "esp_lcd_touch.h"
#include "esp_lcd_touch_gt911.h"
#include "driver/i2c_master.h"
#include "esp_lcd_panel_io.h"
#include "esp_err.h"

#define TOUCH_I2C_PORT    I2C_NUM_0
#define TOUCH_SDA_GPIO    7
#define TOUCH_SCL_GPIO    8
#define TOUCH_I2C_HZ      100000
#define TOUCH_X_MAX       1024
#define TOUCH_Y_MAX       600

static esp_lcd_touch_handle_t s_tp = NULL;

void touch_init(void)
{
    /* I2C master bus */
    i2c_master_bus_handle_t i2c_bus = NULL;
    i2c_master_bus_config_t bus_cfg = {
        .i2c_port             = TOUCH_I2C_PORT,
        .sda_io_num           = TOUCH_SDA_GPIO,
        .scl_io_num           = TOUCH_SCL_GPIO,
        .clk_source           = I2C_CLK_SRC_DEFAULT,
        .glitch_ignore_cnt    = 7,
        .flags.enable_internal_pullup = true,
    };
    ESP_ERROR_CHECK(i2c_new_master_bus(&bus_cfg, &i2c_bus));

    /* I2C panel IO for GT911 */
    esp_lcd_panel_io_handle_t tp_io = NULL;
    esp_lcd_panel_io_i2c_config_t tp_io_cfg = ESP_LCD_TOUCH_IO_I2C_GT911_CONFIG();
    tp_io_cfg.scl_speed_hz = TOUCH_I2C_HZ;
    ESP_ERROR_CHECK(esp_lcd_new_panel_io_i2c_v2(i2c_bus, &tp_io_cfg, &tp_io));

    /* GT911 touch driver */
    esp_lcd_touch_config_t tp_cfg = {
        .x_max        = TOUCH_X_MAX,
        .y_max        = TOUCH_Y_MAX,
        .rst_gpio_num = GPIO_NUM_NC,
        .int_gpio_num = GPIO_NUM_NC,
        .levels       = { .reset = 0, .interrupt = 0 },
        .flags        = { .swap_xy = 0, .mirror_x = 0, .mirror_y = 0 },
    };
    ESP_ERROR_CHECK(esp_lcd_touch_new_i2c_gt911(tp_io, &tp_cfg, &s_tp));
}

bool touch_read(uint16_t *x, uint16_t *y)
{
    uint16_t xs[1], ys[1], strengths[1];
    uint8_t  cnt = 0;

    esp_lcd_touch_read_data(s_tp);
    bool pressed = esp_lcd_touch_get_coordinates(s_tp, xs, ys, strengths, &cnt, 1);

    if (pressed && cnt > 0) {
        *x = xs[0];
        *y = ys[0];
        return true;
    }
    return false;
}
