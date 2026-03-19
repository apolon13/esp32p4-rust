idf_component_register(
    SRCS "display_init.c"
    INCLUDE_DIRS "include"
    REQUIRES esp_lcd esp_hw_support driver esp_lcd_jd9165 freertos
)
