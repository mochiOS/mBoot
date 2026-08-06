BUILDROOT_DIR := $(CURDIR)/buildroot
BUILDROOT_VERSION := 2025.02.16
OUTPUT_DIR := $(CURDIR)/output
BUILDROOT_DEFCONFIG := mboot_x86_64_defconfig
MOCHIOS ?= $(CURDIR)/mochiOS.img
QEMU_ACCELERATOR ?= auto
QEMU_MEMORY ?= 4096
QEMU_DISPLAY ?= gtk
QEMU_FULLSCREEN ?= yes
QEMU ?= qemu-system-x86_64
RUN_OVMF_CODE := $(CURDIR)/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_CODE_4M.fd
RUN_OVMF_VARS_TEMPLATE := $(CURDIR)/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_VARS_4M.fd
RUN_DISK_IMAGE := $(OUTPUT_DIR)/run/mboot.img
QEMU_FULLSCREEN_PATCH := $(CURDIR)/board/mboot/patches/qemu/0001-sdl-keep-fullscreen-window-size.patch
QEMU_FULLSCREEN_PATCH_STAMP := $(OUTPUT_DIR)/.mboot-qemu-fullscreen-patch.sha256

JOBS ?= $(shell nproc)
HOST_CARGO := $(shell command -v cargo)
MBOOTD_BINARY := $(CURDIR)/target/release/mbootd

# WSL PATH fix for Buildroot
override export PATH := /usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

.PHONY: all protocol-test mbootd
all: build

protocol-test:
	@test -n "$(HOST_CARGO)" || { echo "host cargo was not found" >&2; exit 1; }
	$(HOST_CARGO) test --workspace
	$(HOST_CARGO) check -p mboot-protocol --no-default-features

mbootd:
	@test -n "$(HOST_CARGO)" || { echo "host cargo was not found" >&2; exit 1; }
	$(HOST_CARGO) build --release -p mbootd

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

.PHONY: prepare-qemu
prepare-qemu:
	@set -eu; \
	digest=$$(sha256sum "$(QEMU_FULLSCREEN_PATCH)" | awk '{print $$1}'); \
	if [ -f "$(QEMU_FULLSCREEN_PATCH_STAMP)" ] && \
	   [ "$$(cat "$(QEMU_FULLSCREEN_PATCH_STAMP)")" = "$$digest" ]; then \
		exit 0; \
	fi; \
	qemu_built=0; \
	for directory in "$(OUTPUT_DIR)"/build/qemu-*; do \
		[ ! -d "$$directory" ] || qemu_built=1; \
	done; \
	if [ "$$qemu_built" -eq 1 ]; then \
		$(MAKE) -C "$(BUILDROOT_DIR)" O="$(OUTPUT_DIR)" qemu-dirclean; \
	fi

.PHONY: build
build: check-config check-mochios prepare-qemu mbootd
	MBOOT_MOCHIOS_IMAGE="$(abspath $(MOCHIOS))" \
	MBOOTD_BINARY="$(MBOOTD_BINARY)" \
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		-j"$(JOBS)"
	@sha256sum "$(QEMU_FULLSCREEN_PATCH)" | awk '{print $$1}' > \
		"$(QEMU_FULLSCREEN_PATCH_STAMP).new"
	@mv "$(QEMU_FULLSCREEN_PATCH_STAMP).new" "$(QEMU_FULLSCREEN_PATCH_STAMP)"

.PHONY: run run-built
run: build
	$(MAKE) run-built

run-built:
	@set -eu; \
	test -s "$(OUTPUT_DIR)/images/disk.img" || { echo "mBoot image not found: $(OUTPUT_DIR)/images/disk.img" >&2; exit 1; }; \
	mkdir -p "$(OUTPUT_DIR)/run"; \
	rm -f "$(RUN_DISK_IMAGE).new"; \
	cp --reflink=auto --sparse=always "$(OUTPUT_DIR)/images/disk.img" "$(RUN_DISK_IMAGE).new"; \
	mv "$(RUN_DISK_IMAGE).new" "$(RUN_DISK_IMAGE)"; \
	accel="$(QEMU_ACCELERATOR)"; \
	if [ "$$accel" = auto ]; then \
		if [ -r /dev/kvm ] && [ -w /dev/kvm ] && \
		   grep -Eq '^flags[[:space:]]*:.*[[:space:]](vmx|svm)([[:space:]]|$$)' /proc/cpuinfo; \
		then accel=kvm; else accel=tcg; fi; \
	fi; \
	case "$$accel" in kvm|tcg) :;; *) echo "invalid QEMU_ACCELERATOR: $$accel" >&2; exit 1;; esac; \
	if [ "$$accel" = kvm ]; then accel_args='-accel kvm -cpu host,-vmx,-svm'; else accel_args='-accel tcg -cpu qemu64,-vmx,-svm'; fi; \
	case "$(QEMU_FULLSCREEN)" in yes) fullscreen_args='-full-screen';; no) fullscreen_args='';; *) echo "invalid QEMU_FULLSCREEN: $(QEMU_FULLSCREEN)" >&2; exit 1;; esac; \
	if [ "$(QEMU_DISPLAY)" = none ]; then fullscreen_args=''; fi; \
	cp "$(RUN_OVMF_VARS_TEMPLATE)" "$(OUTPUT_DIR)/images/OVMF_VARS.fd"; \
	$(QEMU) \
	$$accel_args \
	-machine q35,i8042=off \
	-smp 4 \
	-m "$(QEMU_MEMORY)" \
	-drive if=pflash,format=raw,readonly=on,file="$(RUN_OVMF_CODE)" \
	-drive if=pflash,format=raw,file="$(OUTPUT_DIR)/images/OVMF_VARS.fd" \
	-drive file="$(RUN_DISK_IMAGE)",format=raw,if=none,id=mboot \
	-device virtio-blk-pci,drive=mboot,serial=MBOOT \
	-device virtio-vga \
	-device qemu-xhci,id=xhci \
	-device usb-kbd,bus=xhci.0,id=mboot-keyboard \
	-device usb-mouse,bus=xhci.0,id=mboot-mouse \
	-display "$(QEMU_DISPLAY)" \
	$$fullscreen_args \
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
	@echo "  make protocol-test     Test mBoot Control Protocol and Unix socket tools"
	@echo "  make check-image       Validate the completed image and target"
	@echo "  make run               Build and launch with QEMU"
	@echo "  make run-built         Launch the existing image with QEMU"
	@echo "  make clean             Clean Buildroot outputs"
	@echo "  make distclean         Remove the entire output directory"
	@echo "  make rebuild           Clean and rebuild"
