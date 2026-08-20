BUILDROOT_DIR := $(CURDIR)/buildroot
BUILDROOT_VERSION := 2025.02.16
OUTPUT_DIR := $(CURDIR)/output
MBOOT_DEVELOPMENT ?= 0
MBOOT_DEV_AUTHORIZED_KEY ?=
BOOT_CONFIG_DIR := $(OUTPUT_DIR)/generated
BUILDROOT_DEFCONFIG := $(BOOT_CONFIG_DIR)/mboot_x86_64_defconfig
BOOT_CONFIG_STAMP := $(BOOT_CONFIG_DIR)/.buildroot-config.sha256
OUTPUT_COMPATIBILITY_VERSION := 2
OUTPUT_COMPATIBILITY_STAMP := $(OUTPUT_DIR)/.mboot-output-version
BOOT_CONFIG_SOURCES := \
	board/mboot/boot-layout.conf \
	board/mboot/genimage.cfg.in \
	board/mboot/grub-bios.cfg.in \
	board/mboot/grub-builtin.cfg.in \
	board/mboot/busybox.config \
	board/mboot/linux.config.in \
	configs/mboot_x86_64_defconfig.in \
	scripts/generate-boot-config.sh
MOCHIOS ?= $(CURDIR)/mochiOS.img
MOCHIOS_SDK_SYSROOT ?=
MOCHIOS_SDK_CRT0 ?=
MOCHIOS_SDK_RUNTIME ?=
MOCHIOS_SDK_LINKER ?=
QEMU_ACCELERATOR ?= auto
QEMU_MEMORY ?= 4096
QEMU_DISPLAY ?= gtk
QEMU_FULLSCREEN ?= yes
QEMU ?= qemu-system-x86_64
RUN_OVMF_CODE := $(CURDIR)/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_CODE_4M.fd
RUN_OVMF_VARS_TEMPLATE := $(CURDIR)/board/mboot/rootfs-overlay/usr/share/mboot/OVMF_VARS_4M.fd
RUN_DISK_IMAGE := $(OUTPUT_DIR)/run/mboot.img
QEMU_CONFIG_INPUTS := \
	$(wildcard $(CURDIR)/board/mboot/patches/qemu/*.patch) \
	$(CURDIR)/board/mboot/qemu-configure-wrapper \
	$(CURDIR)/configs/mboot_x86_64_defconfig.in \
	$(CURDIR)/external.mk \
	$(CURDIR)/package/virglrenderer/virglrenderer.mk
QEMU_CONFIG_STAMP := $(OUTPUT_DIR)/.mboot-qemu-config.sha256
LINUX_FIRMWARE_CONFIG_STAMP := $(OUTPUT_DIR)/.mboot-linux-firmware-config.sha256

JOBS ?= $(shell nproc)
HOST_CARGO := $(shell command -v cargo)
HOST_CARGO_HOME := $(if $(CARGO_HOME),$(CARGO_HOME),$(HOME)/.cargo)
HOST_RUSTC := $(shell command -v rustc)
RUSTC_SYSROOT := $(shell $(HOST_RUSTC) --print sysroot 2>/dev/null)
MBOOTD_TARGET := x86_64-unknown-linux-gnu
MBOOTD_BINARY := $(CURDIR)/target/$(MBOOTD_TARGET)/release/mbootd
MBOOT_SOURCE_DATE_EPOCH := $(shell git log -1 --format=%ct 2>/dev/null || echo 315532800)

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
	@test -n "$(RUSTC_SYSROOT)" || { echo "host rustc sysroot was not found" >&2; exit 1; }
	SOURCE_DATE_EPOCH="$(MBOOT_SOURCE_DATE_EPOCH)" CARGO_INCREMENTAL=0 \
	RUSTFLAGS='-C target-feature=+crt-static --remap-path-prefix=$(abspath $(CURDIR)/..)=/usr/src/mochios --remap-path-prefix=$(RUSTC_SYSROOT)=/usr/src/rust --remap-path-prefix=$(HOST_CARGO_HOME)=/usr/src/cargo' \
		$(HOST_CARGO) build --locked --release --target "$(MBOOTD_TARGET)" -p mbootd
	@if readelf -l "$(MBOOTD_BINARY)" | grep -Fq 'INTERP'; then \
		echo 'mbootd must not depend on the build host dynamic loader' >&2; \
		exit 1; \
	fi

.PHONY: prepare-boot-config
prepare-boot-config:
	MBOOT_DEVELOPMENT="$(MBOOT_DEVELOPMENT)" \
		scripts/generate-boot-config.sh "$(BOOT_CONFIG_DIR)"

.PHONY: check
check: prepare-boot-config
	MBOOT_BOOT_CONFIG_DIR="$(BOOT_CONFIG_DIR)" scripts/check-config.sh
	MBOOT_DEVELOPMENT="$(MBOOT_DEVELOPMENT)" scripts/test-boot-config.sh

.PHONY: check-image
check-image: build

.PHONY: check-qemu-usb
check-qemu-usb: build
	@set -eu; \
	for controller in xhci ehci; do \
		MBOOT_OUTPUT_DIR="$(OUTPUT_DIR)" \
		QEMU_ACCELERATOR="$(QEMU_ACCELERATOR)" \
		MBOOT_USB_CONTROLLER="$$controller" \
		QEMU="$(QEMU)" \
		scripts/test-qemu-usb-boot.sh; \
	done

.PHONY: check-reproducible
check-reproducible: build
	@set -eu; \
	before=$$(sha256sum "$(OUTPUT_DIR)/images/disk.img" | awk '{print $$1}'); \
	$(MAKE) build MOCHIOS="$(MOCHIOS)"; \
	after=$$(sha256sum "$(OUTPUT_DIR)/images/disk.img" | awk '{print $$1}'); \
	if [ "$$before" != "$$after" ]; then \
		echo "mBoot image is not reproducible: $$before != $$after" >&2; \
		exit 1; \
	fi; \
	echo "check-reproducible: PASS ($$after)"

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
defconfig: setup prepare-boot-config
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		BR2_EXTERNAL="$(CURDIR)" \
		BR2_DEFCONFIG="$(BUILDROOT_DEFCONFIG)" \
		defconfig
	@{ sha256sum $(BOOT_CONFIG_SOURCES); \
		printf '%s\n' 'development=$(MBOOT_DEVELOPMENT)'; \
	} | sha256sum | awk '{print $$1}' > \
		"$(BOOT_CONFIG_STAMP).new"
	@mv "$(BOOT_CONFIG_STAMP).new" "$(BOOT_CONFIG_STAMP)"
	@printf '%s\n' "$(OUTPUT_COMPATIBILITY_VERSION)" > \
		"$(OUTPUT_COMPATIBILITY_STAMP).new"
	@mv "$(OUTPUT_COMPATIBILITY_STAMP).new" "$(OUTPUT_COMPATIBILITY_STAMP)"

.PHONY: configure
configure: setup prepare-boot-config
	@set -eu; \
		digest=$$({ sha256sum $(BOOT_CONFIG_SOURCES); \
			printf '%s\n' 'development=$(MBOOT_DEVELOPMENT)'; \
		} | sha256sum | awk '{print $$1}'); \
	current=; \
	output_version=; \
	if [ -f "$(BOOT_CONFIG_STAMP)" ]; then current=$$(cat "$(BOOT_CONFIG_STAMP)"); fi; \
	if [ -f "$(OUTPUT_COMPATIBILITY_STAMP)" ]; then \
		output_version=$$(cat "$(OUTPUT_COMPATIBILITY_STAMP)"); \
	fi; \
	if [ -f "$(OUTPUT_DIR)/.config" ] && [ -d "$(OUTPUT_DIR)/target" ] && \
	   [ "$$output_version" != "$(OUTPUT_COMPATIBILITY_VERSION)" ]; then \
		echo 'mBoot output format changed; invalidating old package output'; \
		$(MAKE) -C "$(BUILDROOT_DIR)" O="$(OUTPUT_DIR)" clean; \
		current=; \
	fi; \
	if [ ! -f "$(OUTPUT_DIR)/.config" ] || [ "$$current" != "$$digest" ]; then \
		$(MAKE) -C "$(BUILDROOT_DIR)" \
			O="$(OUTPUT_DIR)" \
			BR2_EXTERNAL="$(CURDIR)" \
			BR2_DEFCONFIG="$(BUILDROOT_DEFCONFIG)" \
			defconfig; \
		printf '%s\n' "$$digest" > "$(BOOT_CONFIG_STAMP).new"; \
		mv "$(BOOT_CONFIG_STAMP).new" "$(BOOT_CONFIG_STAMP)"; \
	fi; \
	printf '%s\n' "$(OUTPUT_COMPATIBILITY_VERSION)" > \
		"$(OUTPUT_COMPATIBILITY_STAMP).new"; \
	mv "$(OUTPUT_COMPATIBILITY_STAMP).new" "$(OUTPUT_COMPATIBILITY_STAMP)"

.PHONY: menuconfig
menuconfig: configure
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		menuconfig

.PHONY: prepare-qemu
prepare-qemu:
	@set -eu; \
	digest=$$(sha256sum $(QEMU_CONFIG_INPUTS) | sha256sum | awk '{print $$1}'); \
	if [ -f "$(QEMU_CONFIG_STAMP)" ] && \
	   [ "$$(cat "$(QEMU_CONFIG_STAMP)")" = "$$digest" ]; then \
		exit 0; \
	fi; \
	qemu_built=0; \
	sdl2_built=0; \
	virglrenderer_built=0; \
	for directory in "$(OUTPUT_DIR)"/build/qemu-*; do \
		[ ! -d "$$directory" ] || qemu_built=1; \
	done; \
	for directory in "$(OUTPUT_DIR)"/build/virglrenderer-*; do \
		[ ! -d "$$directory" ] || virglrenderer_built=1; \
	done; \
	for directory in "$(OUTPUT_DIR)"/build/sdl2-*; do \
		[ ! -d "$$directory" ] || sdl2_built=1; \
	done; \
	if [ "$$qemu_built" -eq 1 ]; then \
		$(MAKE) -C "$(BUILDROOT_DIR)" O="$(OUTPUT_DIR)" qemu-dirclean; \
	fi; \
	if [ "$$virglrenderer_built" -eq 1 ]; then \
		$(MAKE) -C "$(BUILDROOT_DIR)" O="$(OUTPUT_DIR)" virglrenderer-dirclean; \
	fi; \
	if [ "$$sdl2_built" -eq 1 ]; then \
		$(MAKE) -C "$(BUILDROOT_DIR)" O="$(OUTPUT_DIR)" sdl2-dirclean; \
	fi

.PHONY: prepare-xserver
prepare-xserver: configure
	@set -eu; \
	if grep -Fqx 'BR2_PACKAGE_XSERVER_XORG_SERVER_XVFB=y' "$(OUTPUT_DIR)/.config" && \
	   [ -d "$(OUTPUT_DIR)/build" ] && \
	   find "$(OUTPUT_DIR)/build" -maxdepth 1 -type d \
		-name 'xserver_xorg-server-*' | grep -q . && \
	   [ ! -x "$(OUTPUT_DIR)/target/usr/bin/Xvfb" ]; then \
		echo 'Xvfb is enabled but missing; rebuilding xserver_xorg-server'; \
		$(MAKE) -C "$(BUILDROOT_DIR)" O="$(OUTPUT_DIR)" \
			xserver_xorg-server-dirclean; \
	fi

.PHONY: prepare-linux-firmware
prepare-linux-firmware: configure
	@set -eu; \
	digest=$$({ grep '^BR2_PACKAGE_LINUX_FIRMWARE_' "$(BUILDROOT_DEFCONFIG)" || true; \
		sha256sum "$(BUILDROOT_DIR)/package/linux-firmware/Config.in" \
			"$(BUILDROOT_DIR)/package/linux-firmware/linux-firmware.mk"; \
	} | sha256sum | awk '{print $$1}'); \
	if [ -f "$(LINUX_FIRMWARE_CONFIG_STAMP)" ] && \
	   [ "$$(cat "$(LINUX_FIRMWARE_CONFIG_STAMP)")" = "$$digest" ]; then \
		exit 0; \
	fi; \
	for directory in "$(OUTPUT_DIR)"/build/linux-firmware-*; do \
		[ ! -d "$$directory" ] || { \
			echo 'Linux firmware selection changed; rebuilding linux-firmware'; \
			$(MAKE) -C "$(BUILDROOT_DIR)" O="$(OUTPUT_DIR)" linux-firmware-dirclean; \
			break; \
		}; \
	done

.PHONY: build
build: configure check check-mochios prepare-qemu prepare-xserver prepare-linux-firmware mbootd
	MBOOT_MOCHIOS_IMAGE="$(abspath $(MOCHIOS))" \
	MBOOT_MOCHIOS_SDK_SYSROOT="$(MOCHIOS_SDK_SYSROOT)" \
	MBOOT_MOCHIOS_SDK_CRT0="$(MOCHIOS_SDK_CRT0)" \
	MBOOT_MOCHIOS_SDK_RUNTIME="$(MOCHIOS_SDK_RUNTIME)" \
	MBOOT_MOCHIOS_SDK_LINKER="$(MOCHIOS_SDK_LINKER)" \
	MBOOTD_BINARY="$(MBOOTD_BINARY)" \
	MBOOT_BOOT_CONFIG_DIR="$(BOOT_CONFIG_DIR)" \
	MBOOT_SOURCE_DATE_EPOCH="$(MBOOT_SOURCE_DATE_EPOCH)" \
	MBOOT_DEVELOPMENT="$(MBOOT_DEVELOPMENT)" \
	MBOOT_DEV_AUTHORIZED_KEY="$(MBOOT_DEV_AUTHORIZED_KEY)" \
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		-j"$(JOBS)"
	@sha256sum $(QEMU_CONFIG_INPUTS) | sha256sum | awk '{print $$1}' > \
		"$(QEMU_CONFIG_STAMP).new"
	@mv "$(QEMU_CONFIG_STAMP).new" "$(QEMU_CONFIG_STAMP)"
	@{ grep '^BR2_PACKAGE_LINUX_FIRMWARE_' "$(BUILDROOT_DEFCONFIG)" || true; \
		sha256sum "$(BUILDROOT_DIR)/package/linux-firmware/Config.in" \
			"$(BUILDROOT_DIR)/package/linux-firmware/linux-firmware.mk"; \
	} | sha256sum | awk '{print $$1}' > "$(LINUX_FIRMWARE_CONFIG_STAMP).new"
	@mv "$(LINUX_FIRMWARE_CONFIG_STAMP).new" "$(LINUX_FIRMWARE_CONFIG_STAMP)"
	MBOOT_MOCHIOS_IMAGE="$(abspath $(MOCHIOS))" \
	MBOOT_OUTPUT_DIR="$(OUTPUT_DIR)" scripts/check-image.sh

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
check-config: configure
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
		BR2_DEFCONFIG="$(BUILDROOT_DEFCONFIG)" \
		savedefconfig
	scripts/update-config-template.sh defconfig "$(BUILDROOT_DEFCONFIG)" \
		configs/mboot_x86_64_defconfig.in
	scripts/generate-boot-config.sh "$(BOOT_CONFIG_DIR)"

.PHONY: linux-menuconfig
linux-menuconfig: configure
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		BR2_EXTERNAL="$(CURDIR)" \
		linux-menuconfig

.PHONY: linux-update
linux-update: configure
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		BR2_EXTERNAL="$(CURDIR)" \
		linux-update-defconfig
	scripts/update-config-template.sh linux "$(BOOT_CONFIG_DIR)/linux.config" \
		board/mboot/linux.config.in
	scripts/generate-boot-config.sh "$(BOOT_CONFIG_DIR)"
		
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
	@echo "  make check-reproducible  Build twice and compare the complete image hash"
	@echo "  make check-qemu-usb    Boot the image through xHCI and EHCI USB storage"
	@echo "  make run               Build and launch with QEMU"
	@echo "  make run-built         Launch the existing image with QEMU"
	@echo "  make clean             Clean Buildroot outputs"
	@echo "  make distclean         Remove the entire output directory"
	@echo "  make rebuild           Clean and rebuild"
