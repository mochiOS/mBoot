# Buildroot 2025.02 deliberately disables every target QEMU audio backend.
# Wrap QEMU's configure entry point so mBoot's final options enable ALSA while
# retaining the rest of Buildroot's maintained package recipe.
define MBOOT_QEMU_ENABLE_ALSA
	if [ ! -e $(@D)/configure.mboot-real ]; then \
		mv $(@D)/configure $(@D)/configure.mboot-real; \
		$(INSTALL) -m 0755 $(BR2_EXTERNAL_MBOOT_PATH)/board/mboot/qemu-configure-wrapper $(@D)/configure; \
	fi
endef
QEMU_PRE_CONFIGURE_HOOKS += MBOOT_QEMU_ENABLE_ALSA
QEMU_DEPENDENCIES += alsa-lib
