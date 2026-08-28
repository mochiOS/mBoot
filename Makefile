OUTPUT_DIR ?= $(CURDIR)/output
CONFIG ?= $(CURDIR)/config/qemu.toml
MNU_DIR ?= $(abspath $(CURDIR)/../mnu)
IMAGE ?= $(OUTPUT_DIR)/mochiOS.img
PXE_DIR ?= $(OUTPUT_DIR)/pxe
MDRIVER_KERNEL ?=
MDRIVER_INITRAMFS ?=

CARGO ?= $(shell command -v cargo 2>/dev/null)
RUSTC ?= $(shell command -v rustc 2>/dev/null)
MNU_ABI_PATCH = $(if $(wildcard $(MNU_DIR)/crates/abi/Cargo.toml),--config 'patch."https://github.com/mochiOS/mnu".mnu-abi.path="$(MNU_DIR)/crates/abi"')

.PHONY: all build clean device-io-test mdriver-test help image image-test qemu-test setup test
.PHONY: hv-build hv-device-io-test hv-mdriver-test hv-image hv-image-test hv-qemu-test hv-test

all: image

SETUP_STAMP := $(CURDIR)/mdriver/.cache/setup-complete

$(SETUP_STAMP): setup.sh mdriver/Makefile mdriver/configs/mdriver_x86_64_defconfig \
		mdriver/board/mdriver/linux.config
	@MNU_DIR="$(MNU_DIR)" MBOOT_CONFIG="$(CONFIG)" ./setup.sh

setup:
	@MNU_DIR="$(MNU_DIR)" MBOOT_CONFIG="$(CONFIG)" ./setup.sh

build: image

test:
	@test -n "$(CARGO)" || { echo 'cargo was not found' >&2; exit 1; }
	$(CARGO) test --package mboot --lib $(MNU_ABI_PATCH)

image: $(SETUP_STAMP)
	@test -f "$(MNU_DIR)/Cargo.toml" || { \
		echo 'mnu repository was not found; set MNU_DIR=/path/to/mnu' >&2; exit 1; \
	}
	MBOOT_HOST_CARGO="$(CARGO)" \
	MBOOT_OUTPUT_DIR="$(OUTPUT_DIR)" \
	MBOOT_MDRIVER_KERNEL="$(MDRIVER_KERNEL)" \
	MBOOT_MDRIVER_INITRAMFS="$(MDRIVER_INITRAMFS)" \
		scripts/build-image.pl \
		--config "$(CONFIG)" \
		--mnu-dir "$(MNU_DIR)" \
		--output "$(IMAGE)" \
		--pxe-output "$(PXE_DIR)"

image-test: image
	HV_CONFIG="$(CONFIG)" HV_DISK_IMAGE="$(IMAGE)" scripts/test-image.sh

device-io-test:
	MBOOT_HOST_CARGO="$(CARGO)" MBOOT_HOST_RUSTC="$(RUSTC)" \
		MNU_DIR="$(MNU_DIR)" scripts/test-device-io.sh

mdriver-test:
	@test -n "$(MDRIVER_KERNEL)" || { echo 'set MDRIVER_KERNEL=/path/to/vmlinux' >&2; exit 1; }
	@test -n "$(MDRIVER_INITRAMFS)" || { echo 'set MDRIVER_INITRAMFS=/path/to/initramfs.cpio' >&2; exit 1; }
	$(MAKE) image \
		CONFIG="$(CURDIR)/config/qemu-mdriver.toml" \
		IMAGE="$(OUTPUT_DIR)/mdriver.img" \
		MDRIVER_KERNEL="$(MDRIVER_KERNEL)" \
		MDRIVER_INITRAMFS="$(MDRIVER_INITRAMFS)"
	HV_DISK_IMAGE="$(OUTPUT_DIR)/mdriver.img" scripts/test-mdriver.sh
	MDRIVER_STORAGE_CORRUPT=primary \
		HV_DISK_IMAGE="$(OUTPUT_DIR)/mdriver.img" scripts/test-mdriver.sh
	$(MAKE) image \
		CONFIG="$(CURDIR)/config/qemu-mdriver-inspection.toml" \
		IMAGE="$(OUTPUT_DIR)/mdriver-inspection.img" \
		MDRIVER_KERNEL="$(MDRIVER_KERNEL)" \
		MDRIVER_INITRAMFS="$(MDRIVER_INITRAMFS)"
	MDRIVER_STORAGE_INSPECT=1 \
		HV_DISK_IMAGE="$(OUTPUT_DIR)/mdriver-inspection.img" scripts/test-mdriver.sh

qemu-test:
	$(MAKE) image CONFIG="$(CURDIR)/config/qemu.toml" IMAGE="$(OUTPUT_DIR)/qemu.img"
	RING_BOOTSTRAP_DOMAIN_ELF="$(MNU_DIR)/target/x86_64-unknown-none/release/ring-bootstrap" \
	MBOOT_LAUNCH_MANIFEST="$(OUTPUT_DIR)/launch.manifest" scripts/test-qemu.sh

clean:
	rm -rf "$(OUTPUT_DIR)"
	rm -f "$(SETUP_STAMP)"

# Compatibility aliases for callers that used the temporary hv-* target names.
hv-build: build
hv-test: test
hv-image: image
hv-image-test: image-test
hv-device-io-test: device-io-test
hv-mdriver-test: mdriver-test
hv-qemu-test: qemu-test

help:
	@echo 'mBoot targets:'
	@echo '  make image             Build the bootable Type-1 mBoot image'
	@echo '  make test              Run hypervisor unit tests'
	@echo '  make image-test        Build and boot the configured image with QEMU'
	@echo '  make qemu-test         Test scheduling and Domain recovery with QEMU'
	@echo '  make device-io-test    Test PCI, DMA, IOMMU, and interrupt routing'
	@echo '  make mdriver-test Boot externally supplied mDriver artifacts'
