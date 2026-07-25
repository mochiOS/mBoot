BUILDROOT_DIR := $(CURDIR)/buildroot
OUTPUT_DIR := $(CURDIR)/output
BUILDROOT_DEFCONFIG := qemu_x86_64_defconfig

JOBS ?= $(shell nproc)

.PHONY: all
all: build

.PHONY: setup
setup:
	git submodule update --init --recursive

.PHONY: defconfig
defconfig: setup
	mkdir -p "$(OUTPUT_DIR)"
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		"$(BUILDROOT_DEFCONFIG)"

.PHONY: menuconfig
menuconfig: check-config
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		menuconfig

.PHONY: build
build: check-config
	$(MAKE) -C "$(BUILDROOT_DIR)" \
		O="$(OUTPUT_DIR)" \
		-j"$(JOBS)"

.PHONY: run
run: build
	"$(OUTPUT_DIR)/images/start-qemu.sh"

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

.PHONY: help
help:
	@echo "mBoot Buildroot targets:"
	@echo "  make setup       Initialize Git submodules"
	@echo "  make defconfig   Generate the default x86_64 configuration"
	@echo "  make menuconfig  Open Buildroot menuconfig"
	@echo "  make build       Build mBoot"
	@echo "  make run         Build and launch with QEMU"
	@echo "  make clean       Clean Buildroot outputs"
	@echo "  make distclean   Remove the entire output directory"
	@echo "  make rebuild     Clean and rebuild"
