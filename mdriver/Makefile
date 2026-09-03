MDRIVER_DIR := $(CURDIR)
BUILDROOT_DIR := $(MDRIVER_DIR)/buildroot
OUTPUT_DIR := $(MDRIVER_DIR)/output
DL_DIR := $(MDRIVER_DIR)/dl
BR2_EXTERNAL := $(MDRIVER_DIR)
BR2_MAKE := $(MDRIVER_DIR)/scripts/buildroot-make.sh
SETUP_STAMP := $(MDRIVER_DIR)/.cache/setup-complete
ARTIFACTS_STAMP := $(OUTPUT_DIR)/.artifacts-ready
LINUX_INPUT_STATE := $(MDRIVER_DIR)/.cache/linux-inputs.sha256
MDRIVER_INPUTS := $(shell find \
	$(MDRIVER_DIR)/board \
	$(MDRIVER_DIR)/configs \
	$(MDRIVER_DIR)/package \
	$(MDRIVER_DIR)/scripts \
	-type f -print 2>/dev/null)
JOBS ?= $(shell getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)

.PHONY: all build build-offline clean defconfig distclean legal-info prepare-linux setup source

all: build

setup:
	@$(MDRIVER_DIR)/scripts/setup-buildroot.sh

defconfig: setup
	@$(BR2_MAKE) mdriver_x86_64_defconfig
	@$(BR2_MAKE) check-dependencies

source: defconfig
	@$(BR2_MAKE) source

$(SETUP_STAMP): $(MDRIVER_DIR)/../setup.sh \
		$(MDRIVER_DIR)/configs/mdriver_x86_64_defconfig \
		$(MDRIVER_DIR)/board/mdriver/linux.config
	@MNU_DIR="$(abspath $(MDRIVER_DIR)/../../core)" $(MDRIVER_DIR)/../setup.sh

build: prepare-linux
	@$(MAKE) $(ARTIFACTS_STAMP)

prepare-linux: $(SETUP_STAMP)
	@$(MDRIVER_DIR)/scripts/prepare-linux-inputs.sh

$(ARTIFACTS_STAMP): $(SETUP_STAMP) $(LINUX_INPUT_STATE) $(MDRIVER_INPUTS) $(MDRIVER_DIR)/Makefile
	@$(BR2_MAKE) -j$(JOBS)
	@$(MDRIVER_DIR)/scripts/publish-artifacts.sh
	@touch "$@"

build-offline: defconfig
	@$(BR2_MAKE) BR2_PRIMARY_SITE_ONLY=y -j$(JOBS)
	@$(MDRIVER_DIR)/scripts/publish-artifacts.sh
	@touch "$(ARTIFACTS_STAMP)"

legal-info: build
	@$(BR2_MAKE) legal-info

clean:
	@$(BR2_MAKE) clean
	@rm -f "$(SETUP_STAMP)" "$(ARTIFACTS_STAMP)"

distclean:
	@rm -rf "$(BUILDROOT_DIR)" "$(OUTPUT_DIR)" "$(DL_DIR)" "$(MDRIVER_DIR)/.cache"
