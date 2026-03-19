#pragma once
#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

void touch_init(void);
bool touch_read(uint16_t *x, uint16_t *y);

#ifdef __cplusplus
}
#endif
