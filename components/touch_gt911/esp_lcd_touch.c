#include <stdlib.h>
#include "esp_lcd_touch.h"

esp_err_t esp_lcd_touch_read_data(esp_lcd_touch_handle_t tp)
{
    return tp->read_data(tp);
}

bool esp_lcd_touch_get_coordinates(esp_lcd_touch_handle_t tp,
                                   uint16_t *x, uint16_t *y, uint16_t *strength,
                                   uint8_t *point_num, uint8_t max_point_num)
{
    bool ret = tp->get_xy(tp, x, y, strength, point_num, max_point_num);
    if (ret && *point_num > 0) {
        for (int i = 0; i < *point_num; i++) {
            if (tp->config.flags.swap_xy)  { uint16_t t = x[i]; x[i] = y[i]; y[i] = t; }
            if (tp->config.flags.mirror_x) { x[i] = tp->config.x_max - x[i]; }
            if (tp->config.flags.mirror_y) { y[i] = tp->config.y_max - y[i]; }
        }
        if (tp->config.process_coordinates)
            tp->config.process_coordinates(tp, x, y, strength, point_num, max_point_num);
    }
    return ret;
}

esp_err_t esp_lcd_touch_del(esp_lcd_touch_handle_t tp)
{
    if (tp->del) return tp->del(tp);
    free(tp);
    return ESP_OK;
}

esp_err_t esp_lcd_touch_register_interrupt_callback(esp_lcd_touch_handle_t tp,
                                                    esp_lcd_touch_interrupt_callback_t cb)
{
    tp->config.interrupt_callback = cb;
    return ESP_OK;
}

esp_err_t esp_lcd_touch_set_swap_xy(esp_lcd_touch_handle_t tp, bool v)
{ tp->config.flags.swap_xy = v;   return ESP_OK; }
esp_err_t esp_lcd_touch_get_swap_xy(esp_lcd_touch_handle_t tp, bool *v)
{ *v = tp->config.flags.swap_xy;  return ESP_OK; }
esp_err_t esp_lcd_touch_set_mirror_x(esp_lcd_touch_handle_t tp, bool v)
{ tp->config.flags.mirror_x = v;  return ESP_OK; }
esp_err_t esp_lcd_touch_get_mirror_x(esp_lcd_touch_handle_t tp, bool *v)
{ *v = tp->config.flags.mirror_x; return ESP_OK; }
esp_err_t esp_lcd_touch_set_mirror_y(esp_lcd_touch_handle_t tp, bool v)
{ tp->config.flags.mirror_y = v;  return ESP_OK; }
esp_err_t esp_lcd_touch_get_mirror_y(esp_lcd_touch_handle_t tp, bool *v)
{ *v = tp->config.flags.mirror_y; return ESP_OK; }
