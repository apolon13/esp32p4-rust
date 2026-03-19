# security — Rust on ESP32-P4

Rust (std) проект для платы JC1060P470C с ESP32-P4 (7" LCD, 16MB Flash, 32MB PSRAM).

## Требования

- Rust nightly + `rust-src`
- ESP-IDF v5.4.3 (устанавливается автоматически через `esp-idf-sys`)
- Python 3 с пакетом `esptool` (входит в ESP-IDF)
- `ldproxy`: `cargo install ldproxy`

## Сборка и прошивка

```bash
make flash          # собрать + прошить
make flash-monitor  # собрать + прошить + мониторинг
make monitor        # только мониторинг (Ctrl-C для выхода)
```

Перед прошивкой переведите плату в boot mode: зажать BOOT, нажать/отпустить RESET, отпустить BOOT.

## Известная проблема: espflash и chip revision v1.x

**Симптом:** устройство бесконечно перезагружается после прошивки через `espflash flash` / `cargo run`.

**Причина:** `espflash` (проверено на v3.x) при конвертации ELF -> binary записывает в заголовок образа `max_chip_rev = v0.99`, игнорируя значение из sdkconfig ESP-IDF. Bootloader ESP-IDF при загрузке проверяет ревизию чипа и отклоняет образ:

```
E (86) boot_comm: Image requires chip rev <= v0.99, but chip is v1.3
E (92) boot: Factory app partition is not bootable
```

Чип ESP32-P4 revision v1.3 не попадает в диапазон v0.0–v0.99, и bootloader отказывается запускать приложение — устройство уходит в reboot loop без какого-либо полезного вывода в консоль.

**Решение:** использовать `esptool.py elf2image` вместо `espflash save-image` для конвертации ELF в binary. `esptool.py` корректно читает метаданные ревизии из ELF. Прошивать все три файла (bootloader, partition table, app) через `esptool.py write_flash`. Всё это автоматизировано в `Makefile` — используйте `make flash`.

**Не работает:**
```bash
cargo run                    # использует espflash — образ с неверной ревизией
espflash flash --monitor ... # то же самое
```

**Работает:**
```bash
make flash          # esptool.py elf2image + esptool.py write_flash
```
