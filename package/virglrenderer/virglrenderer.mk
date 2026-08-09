VIRGLRENDERER_VERSION = 1.3.0
VIRGLRENDERER_SITE = https://gitlab.freedesktop.org/virgl/virglrenderer/-/archive/$(VIRGLRENDERER_VERSION)
VIRGLRENDERER_SOURCE = virglrenderer-$(VIRGLRENDERER_VERSION).tar.gz
VIRGLRENDERER_LICENSE = MIT
VIRGLRENDERER_LICENSE_FILES = COPYING
VIRGLRENDERER_INSTALL_STAGING = YES
VIRGLRENDERER_DEPENDENCIES = \
	host-pkgconf \
	host-python-pyyaml \
	libdrm \
	libegl \
	libepoxy \
	libgbm
VIRGLRENDERER_CONF_OPTS = \
	-Dplatforms=egl \
	-Dminigbm_allocation=false \
	-Dvenus=false \
	-Dvideo=false \
	-Dtests=false \
	-Dfuzzer=false \
	-Dtracing=none

define VIRGLRENDERER_REMOVE_OLD_TARGET_LIBRARIES
	rm -f $(TARGET_DIR)/usr/lib/libvirglrenderer.so*
endef
VIRGLRENDERER_PRE_INSTALL_TARGET_HOOKS += VIRGLRENDERER_REMOVE_OLD_TARGET_LIBRARIES

define VIRGLRENDERER_REMOVE_VTEST_SERVER
	rm -f $(TARGET_DIR)/usr/bin/virgl_test_server
endef
VIRGLRENDERER_POST_INSTALL_TARGET_HOOKS += VIRGLRENDERER_REMOVE_VTEST_SERVER

$(eval $(meson-package))
