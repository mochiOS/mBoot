OUTPUT_DIR ?= $(CURDIR)/output
CONFIG ?= $(CURDIR)/config/qemu.toml
MNU_DIR ?= $(abspath $(CURDIR)/../mnu)
IMAGE ?= $(OUTPUT_DIR)/mochiOS.iso
DRIVER_LINUX_KERNEL ?=
DRIVER_LINUX_INITRAMFS ?=

CARGO ?= $(shell command -v cargo 2>/dev/null)
RUSTC ?= $(shell command -v rustc 2>/dev/null)
MNU_ABI_PATCH = $(if $(wildcard $(MNU_DIR)/crates/abi/Cargo.toml),--config 'patch."https://github.com/mochiOS/mnu".mnu-abi.path="$(MNU_DIR)/crates/abi"')

.PHONY: all build clean device-io-test driver-linux-test help image image-test qemu-test test
.PHONY: hv-build hv-device-io-test hv-driver-linux-test hv-image hv-image-test hv-qemu-test hv-test

all: image

build: image

test:
	@test -n "$(CARGO)" || { echo 'cargo was not found' >&2; exit 1; }
	$(CARGO) test --package mboot --lib $(MNU_ABI_PATCH)

image:
	@test -f "$(MNU_DIR)/Cargo.toml" || { \
		echo 'mnu repository was not found; set MNU_DIR=/path/to/mnu' >&2; exit 1; \
	}
	MBOOT_HOST_CARGO="$(CARGO)" \
	MBOOT_OUTPUT_DIR="$(OUTPUT_DIR)" \
	MBOOT_DRIVER_LINUX_KERNEL="$(DRIVER_LINUX_KERNEL)" \
	MBOOT_DRIVER_LINUX_INITRAMFS="$(DRIVER_LINUX_INITRAMFS)" \
		scripts/build-image.pl \
		--config "$(CONFIG)" \
		--mnu-dir "$(MNU_DIR)" \
		--output "$(IMAGE)"

image-test: image
	HV_CONFIG="$(CONFIG)" HV_DISK_IMAGE="$(IMAGE)" scripts/test-image.sh

device-io-test:
	MBOOT_HOST_CARGO="$(CARGO)" MBOOT_HOST_RUSTC="$(RUSTC)" \
		MNU_DIR="$(MNU_DIR)" scripts/test-device-io.sh

driver-linux-test:
	@test -n "$(DRIVER_LINUX_KERNEL)" || { echo 'set DRIVER_LINUX_KERNEL=/path/to/vmlinux' >&2; exit 1; }
	@test -n "$(DRIVER_LINUX_INITRAMFS)" || { echo 'set DRIVER_LINUX_INITRAMFS=/path/to/initramfs.cpio' >&2; exit 1; }
	$(MAKE) image \
		CONFIG="$(CURDIR)/config/qemu-driver-linux.toml" \
		IMAGE="$(OUTPUT_DIR)/driver-linux.iso" \
		DRIVER_LINUX_KERNEL="$(DRIVER_LINUX_KERNEL)" \
		DRIVER_LINUX_INITRAMFS="$(DRIVER_LINUX_INITRAMFS)"
	HV_DISK_IMAGE="$(OUTPUT_DIR)/driver-linux.iso" scripts/test-driver-linux.sh

qemu-test:
	$(MAKE) image CONFIG="$(CURDIR)/config/qemu.toml" IMAGE="$(OUTPUT_DIR)/qemu.iso"
	RING_BOOTSTRAP_DOMAIN_ELF="$(MNU_DIR)/target/x86_64-unknown-none/release/ring-bootstrap" \
	MBOOT_LAUNCH_MANIFEST="$(OUTPUT_DIR)/launch.manifest" scripts/test-qemu.sh

clean:
	rm -rf "$(OUTPUT_DIR)"

# Compatibility aliases for callers that used the temporary hv-* target names.
hv-build: build
hv-test: test
hv-image: image
hv-image-test: image-test
hv-device-io-test: device-io-test
hv-driver-linux-test: driver-linux-test
hv-qemu-test: qemu-test

help:
	@echo 'mBoot targets:'
	@echo '  make image             Build the bootable Type-1 mBoot image'
	@echo '  make test              Run hypervisor unit tests'
	@echo '  make image-test        Build and boot the configured image with QEMU'
	@echo '  make qemu-test         Test scheduling and Domain recovery with QEMU'
	@echo '  make device-io-test    Test PCI, DMA, IOMMU, and interrupt routing'
	@echo '  make driver-linux-test Boot externally supplied Driver Linux artifacts'
