#!/bin/sh

set -e

if [ -z "${MBOOT_BOOT_CONFIG_DIR:-}" ] ||
	[ ! -f "$MBOOT_BOOT_CONFIG_DIR/grub-bios.cfg" ]; then
	echo 'mBoot: generated boot configuration is missing' >&2
	exit 1
fi
case "${MBOOT_SOURCE_DATE_EPOCH:-}" in
	''|*[!0-9]*) echo 'mBoot: reproducible source timestamp is missing' >&2; exit 1 ;;
esac

if [ -z "${MBOOTD_BINARY:-}" ] || [ ! -x "$MBOOTD_BINARY" ]; then
	echo 'mBoot: MBOOTD_BINARY must name the built mbootd executable' >&2
	exit 1
fi
install -D -m 0755 "$MBOOTD_BINARY" "$TARGET_DIR/usr/sbin/mbootd"
if readelf -l "$TARGET_DIR/usr/sbin/mbootd" | grep -Fq 'INTERP'; then
	echo 'mBoot: mbootd unexpectedly depends on a host dynamic loader' >&2
	exit 1
fi

install -d -m 0755 "$BINARIES_DIR/efi-part/EFI/BOOT"
rm -f "$BINARIES_DIR/efi-part/EFI/BOOT/bootx64.efi" \
	"$BINARIES_DIR/efi-part/EFI/BOOT/BOOTX64.EFI" \
	"$BINARIES_DIR/efi-part/EFI/BOOT/grub.cfg"
install -m 0644 "$BINARIES_DIR/bzImage" \
	"$BINARIES_DIR/efi-part/EFI/BOOT/BOOTX64.EFI"
find "$BINARIES_DIR/efi-part" -exec \
	touch -h -d "@$MBOOT_SOURCE_DATE_EPOCH" {} +
install -D -m 0644 "$MBOOT_BOOT_CONFIG_DIR/grub-bios.cfg" \
	"$TARGET_DIR/boot/grub/grub.cfg"
install -D -m 0644 "$MBOOT_BOOT_CONFIG_DIR/boot-layout.env" \
	"$TARGET_DIR/etc/mboot-boot.conf"
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
# Cross-toolchain GDB auto-load helpers contain absolute build-host paths and
# have no purpose in this appliance image.
find "$TARGET_DIR" -type f -name '*-gdb.py' -delete
rm -rf "$TARGET_DIR/usr/libexec/libinput"
rm -f "$TARGET_DIR/usr/bin/libinput"
# Buildroot normally points /var/log at /tmp. Appliance diagnostics must
# survive a reboot and be inspectable after an early display failure.
if [ -L "$TARGET_DIR/var/log" ]; then rm "$TARGET_DIR/var/log"; fi
install -d -m 0755 "$TARGET_DIR/var/log"
install -d -m 0750 "$TARGET_DIR/var/log/mboot"

# Buildroot's empty-password default is inappropriate even without a getty.
sed -i 's/^root:[^:]*:/root:!:/' "$TARGET_DIR/etc/shadow"

# Xorg 21.1's modular modesetting/fbdev drivers leave these runtime providers
# implicit. Restore each driver from its package output before applying the ELF
# edits so repeated Buildroot finalization cannot accumulate patchelf changes.
set -- "$BUILD_DIR"/xserver_xorg-server-*/hw/xfree86/drivers/modesetting/.libs/modesetting_drv.so
[ "$#" -eq 1 ] && [ -f "$1" ] || {
	echo 'mBoot: cannot uniquely locate the modesetting driver' >&2
	exit 1
}
modesetting_source=$1
set -- "$BUILD_DIR"/xdriver_xf86-video-fbdev-*/src/.libs/fbdev_drv.so
[ "$#" -eq 1 ] && [ -f "$1" ] || {
	echo 'mBoot: cannot uniquely locate the fbdev driver' >&2
	exit 1
}
fbdev_source=$1
modesetting="$TARGET_DIR/usr/lib/xorg/modules/drivers/modesetting_drv.so"
fbdev="$TARGET_DIR/usr/lib/xorg/modules/drivers/fbdev_drv.so"
install -m 0755 "$modesetting_source" "$modesetting"
install -m 0755 "$fbdev_source" "$fbdev"
target_strip="$HOST_DIR/bin/x86_64-buildroot-linux-gnu-strip"
"$target_strip" --remove-section=.comment --remove-section=.note "$modesetting" "$fbdev"
"$HOST_DIR/bin/patchelf" --add-needed libgbm.so.1 "$modesetting"
"$HOST_DIR/bin/patchelf" --set-rpath '$ORIGIN/../../..' "$modesetting"
"$HOST_DIR/bin/patchelf" --add-needed libfbdevhw.so "$fbdev"
"$HOST_DIR/bin/patchelf" --set-rpath '$ORIGIN/..' "$fbdev"
