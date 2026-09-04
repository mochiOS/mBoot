################################################################################
# mdriver-gpu
################################################################################

MDRIVER_GPU_VERSION = 1
MDRIVER_GPU_SITE = $(BR2_EXTERNAL_MDRIVER_PATH)/gpu
MDRIVER_GPU_SITE_METHOD = local
MDRIVER_GPU_DEPENDENCIES = libdrm mesa3d
MDRIVER_GPU_LICENSE = Apache-2.0
MDRIVER_GPU_LICENSE_FILES = LICENSE

define MDRIVER_GPU_BUILD_CMDS
	$(TARGET_CC) $(TARGET_CFLAGS) \
		`$(PKG_CONFIG_HOST_BINARY) --cflags libdrm gbm egl glesv2` \
		$(@D)/mdriver-gpu.c -o $(@D)/mdriver-gpu \
		$(TARGET_LDFLAGS) \
		`$(PKG_CONFIG_HOST_BINARY) --libs libdrm gbm egl glesv2`
endef

define MDRIVER_GPU_INSTALL_TARGET_CMDS
	$(INSTALL) -D -m 0755 $(@D)/mdriver-gpu $(TARGET_DIR)/usr/bin/mdriver-gpu
endef

$(eval $(generic-package))
