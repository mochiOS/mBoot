BUILDROOT_DIR := $(CURDIR)/buildroot
BUILDROOT_VERSION := 2025.02.16
OUTPUT_DIR := $(CURDIR)/output
BUILDROOT_DEFCONFIG := mboot_x86_64_defconfig
MOCHIOS ?= $(CURDIR)/mochiOS.img
QEMU_ACCELERATOR ?= auto
QEMU_MEMORY ?= 4096
QEMU_DISPLAY ?= gtk
RUN_OVMF_CODE := $(CURDIR)/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_CODE_4M.fd
RUN_OVMF_VARS_TEMPLATE := $(CURDIR)/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_VARS_4M.fd

JOBS ?= $(shell nproc)

# WSL PATH fix for Buildroot
override export PATH := /usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

.PHONY: all
all: build

.PHONY: check
check:
	scripts/check-config.sh

.PHONY: check-image
check-image: build
	MBOOT_MOCHIOS_IMAGE="$(abspath $(MOCHIOS))" scripts/check-image.sh

.PHONY: check-mochios
check-mochios:
	@set -eu; \
	image="$(abspath $(MOCHIOS))"; \
	if [ ! -f "$$image" ]; then \
		echo "mochiOS image not found: $$image" >&2; exit 1; \
	fi; \
	if [ ! -r "$$image" ]; then \
		echo "mochiOS image is not readable: $$image" >&2; exit 1; \
	fi; \
	size=$$(wc -c < "$$image"); \
	if [ "$$size" -lt 67108864 ]; then \
		echo "mochiOS image is too small: $$size bytes" >&2; exit 1; \
	fi; \
	signature=$$(dd if="$$image" bs=1 skip=512 count=8 2>/dev/null); \
	if [ "$$signature" != "EFI PART" ]; then \
		echo "mochiOS image is not a raw GPT disk: $$image" >&2; exit 1; \
	fi

.PHONY: setup
setup:
	@if [ -f "$(BUILDROOT_DIR)/Makefile" ]; then \
		:; \
	elif git ls-files --error-unmatch buildroot >/dev/null 2>&1; then \
		git submodule update --init --recursive; \
	else \
		git clone --depth 1 --branch "$(BUILDROOT_VERSION)" \
			https://gitlab.com/buildroot.org/buildroot.git "$(BUILDROOT_DIR)"; \
	fi

.PHONY: defconfig
defconfig: setup
	mkdir -p "$(OUTPUT_DIR)"
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		BR2_EXTERNAL="$(CURDIR)" \
		"$(BUILDROOT_DEFCONFIG)"

.PHONY: menuconfig
menuconfig: check-config
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		menuconfig

.PHONY: build
build: check-config check-mochios
	MBOOT_MOCHIOS_IMAGE="$(abspath $(MOCHIOS))" \
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		-j"$(JOBS)"

.PHONY: run
run: build
	@set -eu; \
	accel="$(QEMU_ACCELERATOR)"; \
	if [ "$$accel" = auto ]; then \
		if [ -r /dev/kvm ] && [ -w /dev/kvm ] && \
		   grep -Eq '^flags[[:space:]]*:.*[[:space:]](vmx|svm)([[:space:]]|$$)' /proc/cpuinfo; \
		then accel=kvm; else accel=tcg; fi; \
	fi; \
	case "$$accel" in kvm|tcg) :;; *) echo "invalid QEMU_ACCELERATOR: $$accel" >&2; exit 1;; esac; \
	if [ "$$accel" = kvm ]; then accel_args='-accel kvm -cpu host'; else accel_args='-accel tcg -cpu max'; fi; \
	if [ ! -f output/images/OVMF_VARS.fd ] || \
	   [ "$$(wc -c < output/images/OVMF_VARS.fd)" -ne "$$(wc -c < "$(RUN_OVMF_VARS_TEMPLATE)")" ]; then \
		cp "$(RUN_OVMF_VARS_TEMPLATE)" output/images/OVMF_VARS.fd; \
	fi; \
	qemu-system-x86_64 \
	$$accel_args \
	-machine q35 \
	-smp 4 \
	-m "$(QEMU_MEMORY)" \
	-drive if=pflash,format=raw,readonly=on,file="$(RUN_OVMF_CODE)" \
	-drive if=pflash,format=raw,file=output/images/OVMF_VARS.fd \
	-drive file=output/images/disk.img,format=raw,if=none,id=mboot \
	-device virtio-blk-pci,drive=mboot,serial=MBOOT \
	-device virtio-vga \
	-display "$(QEMU_DISPLAY)" \
	-serial mon:stdio

.PHONY: clean
clean:
	@if [ -f "$(OUTPUT_DIR)/Makefile" ]; then \
		$(MAKE) -C "$(BUILDROOT_DIR)" \
			O="$(OUTPUT_DIR)" \
			clean; \
	fi

.PHONY: distclean
distclean:
	rm -rf "$(OUTPUT_DIR)"

.PHONY: rebuild
rebuild: clean build

.PHONY: check-config
check-config:
	@if [ ! -f "$(OUTPUT_DIR)/.config" ]; then \
		echo "Buildroot is not configured."; \
		echo "Run: make defconfig"; \
		exit 1; \
	fi

.PHONY: savedefconfig
savedefconfig:
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		BR2_EXTERNAL="$(CURDIR)" \
		BR2_DEFCONFIG="$(CURDIR)/configs/mboot_x86_64_defconfig" \
		savedefconfig

.PHONY: linux-menuconfig
linux-menuconfig: check-config
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		BR2_EXTERNAL="$(CURDIR)" \
		linux-menuconfig

.PHONY: linux-update
linux-update: check-config
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		BR2_EXTERNAL="$(CURDIR)" \
		linux-update-defconfig
		
.PHONY: help
help:
	@echo "mBoot Buildroot targets:"
	@echo "  make setup             Initialize Git submodules"
	@echo "  make defconfig         Generate the default x86_64 configuration"
	@echo "  make savedefconfig     Save the current configuration to configs/mboot_x86_64_defconfig"
	@echo "  make menuconfig        Open Buildroot menuconfig"
	@echo "  make linux-menuconfig  Open Linux kernel menuconfig"
	@echo "  make linux-update      Update the Linux kernel defconfig"
	@echo "  make build             Build disk.img and USB-writable mboot.iso with embedded mochiOS"
	@echo "  make check             Run repository regression checks"
	@echo "  make check-image       Validate the completed image and target"
	@echo "  make run               Build and launch with QEMU"
	@echo "  make clean             Clean Buildroot outputs"
	@echo "  make distclean         Remove the entire output directory"
	@echo "  make rebuild           Clean and rebuild"
