PORT       ?= /dev/cu.usbmodem101
BAUD       ?= 460800
CHIP       ?= esp32p4
TARGET     ?= riscv32imafc-esp-espidf
PROFILE    ?= debug
PYTHON     ?= /Users/antonpankov/.espressif/python_env/idf5.4_py3.13_env/bin/python

ELF        = target/$(TARGET)/$(PROFILE)/security
APP_BIN    = target/$(TARGET)/$(PROFILE)/security.bin
IDF_BUILD  = target/$(TARGET)/$(PROFILE)/build/esp-idf-sys-*/out/build
BOOTLOADER = $(IDF_BUILD)/bootloader/bootloader.bin
PARTITION  = $(IDF_BUILD)/partition_table/partition-table.bin

export IDF_PATH := /Users/antonpankov/.espressif/esp-idf/v5.4.3

# ── Targets ─────────────────────────────────────────────────────────────────

.PHONY: build image flash monitor flash-monitor flash-release clean

build:
	cargo build

image: build
	$(PYTHON) -m esptool --chip $(CHIP) elf2image \
		--flash_mode dio --flash_size 16MB --flash_freq 80m \
		-o $(APP_BIN) $(ELF)

flash: image
	$(PYTHON) -m esptool --chip $(CHIP) -p $(PORT) -b $(BAUD) \
		--before default_reset --after hard_reset \
		write_flash --flash_mode dio --flash_size 16MB --flash_freq 80m \
		0x2000 $(BOOTLOADER) \
		0x8000 $(PARTITION) \
		0x10000 $(APP_BIN)

monitor:
	$(PYTHON) -c "\
	import serial, time, sys; \
	s = serial.Serial('$(PORT)', 115200, timeout=0.1); \
	print('--- Monitor $(PORT) (Ctrl-C to exit) ---'); \
	[sys.stdout.write(s.read(4096).decode('utf-8', errors='replace')) or time.sleep(0.01) for _ in iter(int, 1)]" \
	|| true

flash-monitor: flash monitor

flash-release:
	cargo build --release
	$(MAKE) flash PROFILE=release

clean:
	cargo clean
