#!/bin/sh

set -e

BOARD_DIR=$(dirname "$0")

test -d "$BINARIES_DIR/efi-part/EFI/BOOT"
install -m 0644 "$BOARD_DIR/grub-efi.cfg" "$BINARIES_DIR/efi-part/EFI/BOOT/grub.cfg"
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
