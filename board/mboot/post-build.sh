#!/bin/sh

set -e

BOARD_DIR=$(dirname "$0")

if [ -z "${MBOOTD_BINARY:-}" ] || [ ! -x "$MBOOTD_BINARY" ]; then
	echo 'mBoot: MBOOTD_BINARY must name the built mbootd executable' >&2
	exit 1
fi
install -D -m 0755 "$MBOOTD_BINARY" "$TARGET_DIR/usr/sbin/mbootd"

install -d -m 0755 "$BINARIES_DIR/efi-part/EFI/BOOT"
rm -f "$BINARIES_DIR/efi-part/EFI/BOOT/bootx64.efi" \
	"$BINARIES_DIR/efi-part/EFI/BOOT/BOOTX64.EFI" \
	"$BINARIES_DIR/efi-part/EFI/BOOT/grub.cfg"
install -m 0644 "$BINARIES_DIR/bzImage" \
	"$BINARIES_DIR/efi-part/EFI/BOOT/BOOTX64.EFI"
install -D -m 0644 "$BOARD_DIR/grub-bios.cfg" "$TARGET_DIR/boot/grub/grub.cfg"
set -- "$BUILD_DIR"/grub2-*/build-i386-pc/grub-core/boot.img
[ "$#" -eq 1 ] && [ -f "$1" ] || {
	echo 'mBoot: cannot uniquely locate GRUB BIOS boot.img' >&2
	exit 1
}
install -m 0644 "$1" "$BINARIES_DIR/boot.img"

# Keep appliance state private even though the launcher currently runs as root
# to obtain raw block-device access.
install -d -m 0700 "$TARGET_DIR/var/lib/mboot"
rm -f "$TARGET_DIR/usr/libexec/mboot-detect-disk" \
	"$TARGET_DIR/etc/udev/rules.d/60-mboot-mochios.rules"
if [ -z "${MBOOT_MOCHIOS_IMAGE:-}" ] || [ ! -r "$MBOOT_MOCHIOS_IMAGE" ]; then
	echo 'mBoot: MBOOT_MOCHIOS_IMAGE must name a readable raw GPT image' >&2
	exit 1
fi
install -m 0600 "$MBOOT_MOCHIOS_IMAGE" "$TARGET_DIR/var/lib/mboot/mochiOS.img"
# Buildroot normally points /var/log at /tmp. Appliance diagnostics must
# survive a reboot and be inspectable after an early display failure.
if [ -L "$TARGET_DIR/var/log" ]; then rm "$TARGET_DIR/var/log"; fi
install -d -m 0755 "$TARGET_DIR/var/log"
install -d -m 0750 "$TARGET_DIR/var/log/mboot"

# Buildroot's empty-password default is inappropriate even without a getty.
sed -i 's/^root:[^:]*:/root:!:/' "$TARGET_DIR/etc/shadow"

# Xorg 21.1's modular modesetting/fbdev drivers leave these runtime providers
# implicit. With immediate module binding that fails before driver fallback can
# initialize, so record the dependencies explicitly in the target ELF files.
modesetting="$TARGET_DIR/usr/lib/xorg/modules/drivers/modesetting_drv.so"
fbdev="$TARGET_DIR/usr/lib/xorg/modules/drivers/fbdev_drv.so"
readelf -d "$modesetting" | grep -Fq 'Shared library: [libgbm.so.1]' ||
	"$HOST_DIR/bin/patchelf" --add-needed libgbm.so.1 "$modesetting"
if ! readelf -d "$fbdev" | grep -Fq 'Shared library: [libfbdevhw.so]'; then
	"$HOST_DIR/bin/patchelf" --add-needed libfbdevhw.so "$fbdev"
fi
"$HOST_DIR/bin/patchelf" --set-rpath '$ORIGIN/..' "$fbdev"
