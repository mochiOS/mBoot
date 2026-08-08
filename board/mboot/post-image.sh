#!/bin/sh
set -eu

if [ -z "${MBOOT_BOOT_CONFIG_DIR:-}" ] ||
	[ ! -f "$MBOOT_BOOT_CONFIG_DIR/genimage.cfg" ]; then
	echo 'mBoot: generated image configuration is missing' >&2
	exit 1
fi
support/scripts/genimage.sh -c "$MBOOT_BOOT_CONFIG_DIR/genimage.cfg"

# The first GPT partition is a fixed, contiguous 1 MiB BIOS Boot Partition at
# LBA 2048. grub-bios-setup insists on a host block device, so patch the two
# documented GRUB PC-BIOS block pointers and embed without loop devices/root.
# boot.img offset 0x5c is its 64-bit core sector; the blocklist at the end of
# grub.img's first sector starts with a 64-bit sector for the remaining core.
core_size=$(wc -c < "$BINARIES_DIR/grub.img")
[ "$core_size" -le 1048576 ] || {
	echo "mBoot: GRUB core image does not fit the BIOS Boot Partition" >&2
	exit 1
}
printf '\000\010\000\000\000\000\000\000' |
	dd of="$BINARIES_DIR/boot.img" bs=1 seek=92 conv=notrunc 2>/dev/null
printf '\001\010\000\000\000\000\000\000' |
	dd of="$BINARIES_DIR/grub.img" bs=1 seek=500 conv=notrunc 2>/dev/null
dd if="$BINARIES_DIR/boot.img" of="$BINARIES_DIR/disk.img" \
	bs=1 count=446 conv=notrunc 2>/dev/null
dd if="$BINARIES_DIR/grub.img" of="$BINARIES_DIR/disk.img" \
	bs=512 seek=2048 conv=notrunc 2>/dev/null

# USB imaging tools commonly expose .iso files more prominently than .img
# files. This remains a writable BIOS/UEFI raw GPT image (not ISO9660), so an
# installed mBoot system can persist both mochiOS and its OVMF state.
usb_image="$BINARIES_DIR/mboot.iso"
usb_image_new=${usb_image}.new
rm -f "$usb_image_new"
cp --sparse=always "$BINARIES_DIR/disk.img" "$usb_image_new"
mv "$usb_image_new" "$usb_image"
