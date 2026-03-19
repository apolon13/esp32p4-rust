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

# ── Component cache invalidation ────────────────────────────────────────────
# When any local C component source changes, only that component's cmake
# artifacts are deleted so ninja recompiles it without a full reconfigure
# (which would require network access to download remote components).
#
# Add new component headers/sources here as they are created.
C_COMPONENT_SRCS = \
	components/display_init/display_init.c \
	components/display_init/include/display_init.h

COMP_STAMP = target/.idf_component_stamp

$(COMP_STAMP): $(C_COMPONENT_SRCS)
	@mkdir -p target
	@echo "[make] C components changed — invalidating cmake component cache"
	@for profile in debug release; do \
		for d in target/$(TARGET)/$$profile/build/esp-idf-sys-*/out/build/esp-idf/display_init; do \
			rm -rf $$d 2>/dev/null || true; \
		done; \
		for d in target/$(TARGET)/$$profile/.fingerprint/esp-idf-sys-*; do \
			rm -rf $$d 2>/dev/null || true; \
		done; \
	done
	@touch build.rs
	@touch $@

# ── Targets ─────────────────────────────────────────────────────────────────

.PHONY: build image flash monitor flash-monitor flash-release clean

build: $(COMP_STAMP)
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

flash-release: $(COMP_STAMP)
	cargo build --release
	$(MAKE) flash PROFILE=release

clean:
	cargo clean
	rm -f target/.idf_component_stamp
