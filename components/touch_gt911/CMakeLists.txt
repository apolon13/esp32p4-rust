idf_component_register(
    SRCS "esp_lcd_touch.c" "esp_lcd_touch_gt911.c" "touch_init.c"
    INCLUDE_DIRS "include"
    REQUIRES esp_lcd driver freertos
)
