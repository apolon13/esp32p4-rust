/*
 * SPDX-FileCopyrightText: 2015-2022 Espressif Systems (Shanghai) CO LTD
 * SPDX-License-Identifier: Apache-2.0
 */
#include <stdio.h>
#include <string.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "esp_system.h"
#include "esp_err.h"
#include "esp_check.h"
#include "driver/gpio.h"
#include "esp_lcd_panel_io.h"
#include "esp_lcd_touch.h"
#include "esp_lcd_touch_gt911.h"

static const char *TAG = "GT911";

#define ESP_LCD_TOUCH_GT911_READ_XY_REG   (0x814E)
#define ESP_LCD_TOUCH_GT911_CONFIG_REG    (0x8047)
#define ESP_LCD_TOUCH_GT911_PRODUCT_ID_REG (0x8140)

static esp_err_t esp_lcd_touch_gt911_read_data(esp_lcd_touch_handle_t tp);
static bool      esp_lcd_touch_gt911_get_xy(esp_lcd_touch_handle_t tp,
                                             uint16_t *x, uint16_t *y, uint16_t *strength,
                                             uint8_t *point_num, uint8_t max_point_num);
static esp_err_t esp_lcd_touch_gt911_del(esp_lcd_touch_handle_t tp);
static esp_err_t touch_gt911_i2c_read(esp_lcd_touch_handle_t tp, uint16_t reg, uint8_t *data, uint8_t len);
static esp_err_t touch_gt911_i2c_write(esp_lcd_touch_handle_t tp, uint16_t reg, uint8_t data);
static esp_err_t touch_gt911_reset(esp_lcd_touch_handle_t tp);
static esp_err_t touch_gt911_read_cfg(esp_lcd_touch_handle_t tp);

esp_err_t esp_lcd_touch_new_i2c_gt911(const esp_lcd_panel_io_handle_t io,
                                      const esp_lcd_touch_config_t *config,
                                      esp_lcd_touch_handle_t *out_touch)
{
    esp_err_t ret = ESP_OK;
    esp_lcd_touch_handle_t tp = heap_caps_calloc(1, sizeof(esp_lcd_touch_t), MALLOC_CAP_DEFAULT);
    ESP_GOTO_ON_FALSE(tp, ESP_ERR_NO_MEM, err, TAG, "no mem for GT911");

    tp->io          = io;
    tp->read_data   = esp_lcd_touch_gt911_read_data;
    tp->get_xy      = esp_lcd_touch_gt911_get_xy;
    tp->del         = esp_lcd_touch_gt911_del;
    tp->data.lock.owner = portMUX_FREE_VAL;
    memcpy(&tp->config, config, sizeof(esp_lcd_touch_config_t));

    if (tp->config.rst_gpio_num != GPIO_NUM_NC) {
        const gpio_config_t gc = { .mode = GPIO_MODE_OUTPUT,
                                   .pin_bit_mask = BIT64(tp->config.rst_gpio_num) };
        ret = gpio_config(&gc);
        ESP_GOTO_ON_ERROR(ret, err, TAG, "GPIO config failed");
    }

    ret = touch_gt911_reset(tp);
    ESP_GOTO_ON_ERROR(ret, err, TAG, "reset failed");

    ret = touch_gt911_read_cfg(tp);
    ESP_GOTO_ON_ERROR(ret, err, TAG, "init failed");

err:
    if (ret != ESP_OK && tp) esp_lcd_touch_gt911_del(tp);
    *out_touch = tp;
    return ret;
}

static esp_err_t esp_lcd_touch_gt911_read_data(esp_lcd_touch_handle_t tp)
{
    uint8_t buf[41], clear = 0;
    uint8_t touch_cnt = 0;

    esp_err_t err = touch_gt911_i2c_read(tp, ESP_LCD_TOUCH_GT911_READ_XY_REG, buf, 1);
    ESP_RETURN_ON_ERROR(err, TAG, "I2C read error");

    if ((buf[0] & 0x80) == 0) {
        touch_gt911_i2c_write(tp, ESP_LCD_TOUCH_GT911_READ_XY_REG, clear);
        return ESP_OK;
    }

    touch_cnt = buf[0] & 0x0f;
    if (touch_cnt == 0 || touch_cnt > 5) {
        touch_gt911_i2c_write(tp, ESP_LCD_TOUCH_GT911_READ_XY_REG, clear);
        return ESP_OK;
    }

    err = touch_gt911_i2c_read(tp, ESP_LCD_TOUCH_GT911_READ_XY_REG + 1, &buf[1], touch_cnt * 8);
    ESP_RETURN_ON_ERROR(err, TAG, "I2C read error");

    touch_gt911_i2c_write(tp, ESP_LCD_TOUCH_GT911_READ_XY_REG, clear);

    portENTER_CRITICAL(&tp->data.lock);
    touch_cnt = (touch_cnt > CONFIG_ESP_LCD_TOUCH_MAX_POINTS ? CONFIG_ESP_LCD_TOUCH_MAX_POINTS : touch_cnt);
    tp->data.points = touch_cnt;
    for (size_t i = 0; i < touch_cnt; i++) {
        tp->data.coords[i].x        = ((uint16_t)buf[(i * 8) + 3] << 8) + buf[(i * 8) + 2];
        tp->data.coords[i].y        = ((uint16_t)buf[(i * 8) + 5] << 8) + buf[(i * 8) + 4];
        tp->data.coords[i].strength = ((uint16_t)buf[(i * 8) + 7] << 8) + buf[(i * 8) + 6];
    }
    portEXIT_CRITICAL(&tp->data.lock);
    return ESP_OK;
}

static bool esp_lcd_touch_gt911_get_xy(esp_lcd_touch_handle_t tp,
                                        uint16_t *x, uint16_t *y, uint16_t *strength,
                                        uint8_t *point_num, uint8_t max_point_num)
{
    portENTER_CRITICAL(&tp->data.lock);
    *point_num = (tp->data.points > max_point_num ? max_point_num : tp->data.points);
    for (size_t i = 0; i < *point_num; i++) {
        x[i] = tp->data.coords[i].x;
        y[i] = tp->data.coords[i].y;
        if (strength) strength[i] = tp->data.coords[i].strength;
    }
    tp->data.points = 0;
    portEXIT_CRITICAL(&tp->data.lock);
    return (*point_num > 0);
}

static esp_err_t esp_lcd_touch_gt911_del(esp_lcd_touch_handle_t tp)
{
    if (tp->config.rst_gpio_num != GPIO_NUM_NC) gpio_reset_pin(tp->config.rst_gpio_num);
    if (tp->config.int_gpio_num != GPIO_NUM_NC) gpio_reset_pin(tp->config.int_gpio_num);
    free(tp);
    return ESP_OK;
}

static esp_err_t touch_gt911_reset(esp_lcd_touch_handle_t tp)
{
    if (tp->config.rst_gpio_num != GPIO_NUM_NC) {
        ESP_RETURN_ON_ERROR(gpio_set_level(tp->config.rst_gpio_num,  tp->config.levels.reset), TAG, "GPIO error");
        vTaskDelay(pdMS_TO_TICKS(10));
        ESP_RETURN_ON_ERROR(gpio_set_level(tp->config.rst_gpio_num, !tp->config.levels.reset), TAG, "GPIO error");
        vTaskDelay(pdMS_TO_TICKS(10));
    }
    return ESP_OK;
}

static esp_err_t touch_gt911_read_cfg(esp_lcd_touch_handle_t tp)
{
    uint8_t buf[4];
    ESP_RETURN_ON_ERROR(touch_gt911_i2c_read(tp, ESP_LCD_TOUCH_GT911_PRODUCT_ID_REG, buf, 3), TAG, "read error");
    ESP_RETURN_ON_ERROR(touch_gt911_i2c_read(tp, ESP_LCD_TOUCH_GT911_CONFIG_REG,     buf+3, 1), TAG, "read error");
    return ESP_OK;
}

static esp_err_t touch_gt911_i2c_read(esp_lcd_touch_handle_t tp, uint16_t reg, uint8_t *data, uint8_t len)
{
    return esp_lcd_panel_io_rx_param(tp->io, reg, data, len);
}

static esp_err_t touch_gt911_i2c_write(esp_lcd_touch_handle_t tp, uint16_t reg, uint8_t data)
{
    return esp_lcd_panel_io_tx_param(tp->io, reg, (uint8_t[]){data}, 1);
}
