MBOOT_QEMU_PATCH_DIR = $(BR2_EXTERNAL_MBOOT_PATH)/board/mboot/patches/qemu

define MBOOT_QEMU_APPLY_MBOOT_PATCHES
	$(APPLY_PATCHES) $(@D) $(MBOOT_QEMU_PATCH_DIR) \*.patch
endef
QEMU_POST_PATCH_HOOKS += MBOOT_QEMU_APPLY_MBOOT_PATCHES

# Buildroot 2025.02 deliberately disables target QEMU audio and OpenGL.
# Wrap QEMU's configure entry point so mBoot's final options enable the
# appliance backends while retaining the rest of Buildroot's package recipe.
define MBOOT_QEMU_INSTALL_CONFIGURE_WRAPPER
	if [ ! -e $(@D)/configure.mboot-real ]; then \
		mv $(@D)/configure $(@D)/configure.mboot-real; \
	fi; \
	$(INSTALL) -m 0755 $(BR2_EXTERNAL_MBOOT_PATH)/board/mboot/qemu-configure-wrapper $(@D)/configure
endef
QEMU_PRE_CONFIGURE_HOOKS += MBOOT_QEMU_INSTALL_CONFIGURE_WRAPPER
QEMU_DEPENDENCIES += alsa-lib libegl libepoxy libgbm virglrenderer

# external.mk is included after Buildroot has expanded qemu.mk's package
# rules. Add the late dependency explicitly so virglrenderer is installed
# into staging before Meson probes it.
$(QEMU_TARGET_CONFIGURE): | virglrenderer

include $(sort $(wildcard $(BR2_EXTERNAL_MBOOT_PATH)/package/*/*.mk))
