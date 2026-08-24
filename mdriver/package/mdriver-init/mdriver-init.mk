################################################################################
# mdriver-init
################################################################################

MDRIVER_INIT_VERSION = 1
MDRIVER_INIT_SITE = $(BR2_EXTERNAL_MDRIVER_PATH)/init
MDRIVER_INIT_SITE_METHOD = local
MDRIVER_INIT_LICENSE = Apache-2.0
MDRIVER_INIT_LICENSE_FILES = LICENSE

define MDRIVER_INIT_BUILD_CMDS
	$(TARGET_CC) $(TARGET_CFLAGS) -static -nostdlib -fno-stack-protector \
		-Wl,-e,_start -Wl,--build-id=none $(@D)/init.c -o $(@D)/init
endef

define MDRIVER_INIT_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 $(@D)/init $(TARGET_DIR)/init
endef

$(eval $(generic-package))
