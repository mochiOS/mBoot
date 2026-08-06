#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
IMAGES=$ROOT/output/images
TARGET=$ROOT/output/target
KCONFIG=$ROOT/output/build/linux-6.12.98/.config
QEMU_CONFIG=$ROOT/output/build/qemu-9.2.0/build/config-host.h
ROOT_PARTUUID=6d426f6f-7400-4b00-8a00-000000000001
ROOT_FSUUID=6d426f6f-7400-4b00-8a00-000000000004

fail() { echo "check-image: $*" >&2; exit 1; }

for image in disk.img mboot.iso efi-part.vfat rootfs.ext2 bzImage boot.img grub.img; do
	test -s "$IMAGES/$image" || fail "missing image: $image"
done
cmp -s "$IMAGES/disk.img" "$IMAGES/mboot.iso" ||
	fail 'mboot.iso is not identical to the completed raw GPT disk image'
[ "$(blkid -s UUID -o value "$IMAGES/rootfs.ext2")" = "$ROOT_FSUUID" ] ||
	fail 'root filesystem UUID is not fixed for GRUB discovery'
sfdisk --dump "$IMAGES/disk.img" | grep -qi "uuid=$ROOT_PARTUUID" ||
	fail 'root GPT partition UUID is missing'
sfdisk --dump "$IMAGES/disk.img" | grep -qi 'label-id: 6D426F6F-7400-4B00-8A00-000000000000' ||
	fail 'owned GPT disk UUID is missing'
sfdisk --dump "$IMAGES/disk.img" | grep -qi 'type=21686148-6449-6E6F-744E-656564454649' ||
	fail 'GPT BIOS Boot Partition is missing'
[ "$(od -An -tx1 -j 510 -N 2 "$IMAGES/disk.img" | tr -d ' \n')" = 55aa ] ||
	fail 'BIOS MBR signature is missing'
[ "$(od -An -tu8 -j 92 -N 8 "$IMAGES/disk.img" | tr -d ' ')" = 2048 ] ||
	fail 'BIOS boot sector does not point to the core image'
[ "$(od -An -tu8 -j 1049076 -N 8 "$IMAGES/disk.img" | tr -d ' ')" = 2049 ] ||
	fail 'BIOS core-image blocklist is not embedded at the expected LBA'

bios_cfg=$(debugfs -R 'cat /boot/grub/grub.cfg' "$IMAGES/rootfs.ext2" 2>/dev/null)
printf '%s\n' "$bios_cfg" | grep -Fq "root=PARTUUID=$ROOT_PARTUUID" ||
	fail 'BIOS GRUB configuration is incomplete'
efi_kernel=/tmp/mboot-check-BOOTX64.EFI
rm -f "$efi_kernel"
$ROOT/output/host/bin/mcopy -i "$IMAGES/efi-part.vfat" ::/EFI/BOOT/BOOTX64.EFI "$efi_kernel"
cmp -s "$IMAGES/bzImage" "$efi_kernel" || fail 'EFI fallback loader is not the Linux EFI-stub kernel'
rm -f "$efi_kernel"

for path in etc/init.d/S40xorg etc/init.d/S80mbootd etc/init.d/S90mboot \
	usr/libexec/mboot-launcher \
	usr/sbin/mbootd usr/bin/qemu-system-x86_64 usr/bin/Xorg; do
	test -x "$TARGET/$path" || fail "missing target executable: /$path"
done
test ! -e "$TARGET/usr/libexec/mboot-detect-disk" ||
	fail 'obsolete external mochiOS disk detector remains in target'
test -n "${MBOOT_MOCHIOS_IMAGE:-}" || fail 'source mochiOS image was not specified'
test -s "$MBOOT_MOCHIOS_IMAGE" || fail 'source mochiOS image is missing'
test -s "$TARGET/var/lib/mboot/mochiOS.img" || fail 'embedded mochiOS image is missing from target'
source_hash=$(sha256sum "$MBOOT_MOCHIOS_IMAGE" | awk '{print $1}')
target_hash=$(sha256sum "$TARGET/var/lib/mboot/mochiOS.img" | awk '{print $1}')
[ "$source_hash" = "$target_hash" ] || fail 'embedded mochiOS image differs from its source'
rootfs_hash=$(debugfs -R 'cat /var/lib/mboot/mochiOS.img' "$IMAGES/rootfs.ext2" 2>/dev/null |
	sha256sum | awk '{print $1}')
[ "$source_hash" = "$rootfs_hash" ] || fail 'root filesystem does not contain the mochiOS image'
for path in usr/share/mboot/OVMF_CODE_4M.fd usr/share/mboot/OVMF_VARS_4M.fd; do
	test -s "$TARGET/$path" || fail "missing firmware: /$path"
done
grep -Fqx '#define CONFIG_AUDIO_ALSA' "$QEMU_CONFIG" ||
	fail 'target QEMU was not built with ALSA support'
grep -q '^root:!:' "$TARGET/etc/shadow" || fail 'root account is not locked'
! grep -q '^[^#].*getty' "$TARGET/etc/inittab" || fail 'an interactive getty is enabled'
readelf -d "$TARGET/usr/lib/xorg/modules/drivers/modesetting_drv.so" |
	grep -Fq 'Shared library: [libgbm.so.1]' || fail 'modesetting GBM dependency is missing'
readelf -d "$TARGET/usr/lib/xorg/modules/drivers/fbdev_drv.so" |
	grep -Fq 'Shared library: [libfbdevhw.so]' || fail 'fbdevhw dependency is missing'
[ "$("$ROOT/output/host/bin/patchelf" --print-rpath \
	"$TARGET/usr/lib/xorg/modules/drivers/fbdev_drv.so")" = '$ORIGIN/..' ] ||
	fail 'fbdevhw module search path is incorrect'

for symbol in CONFIG_DRM_I915=y CONFIG_DRM_AMDGPU=y CONFIG_DRM_NOUVEAU=y \
	CONFIG_DRM_VIRTIO_GPU=y CONFIG_DRM_VMWGFX=y CONFIG_DRM_SIMPLEDRM=y \
	CONFIG_KVM_INTEL=m CONFIG_KVM_AMD=m CONFIG_SATA_AHCI=y CONFIG_BLK_DEV_NVME=y \
	CONFIG_GENERIC_CPU=y CONFIG_CPU_SUP_INTEL=y CONFIG_X86_MCE_INTEL=y \
	CONFIG_MICROCODE=y CONFIG_X86_INTEL_PSTATE=y CONFIG_INTEL_IDLE=y \
	CONFIG_EFI_STUB=y CONFIG_EFIVAR_FS=y CONFIG_INTEL_IOMMU=y CONFIG_AMD_IOMMU=y \
	CONFIG_INPUT_EVDEV=y CONFIG_INPUT_KEYBOARD=y CONFIG_KEYBOARD_ATKBD=y \
	CONFIG_MOUSE_PS2=y CONFIG_HID_MULTITOUCH=m CONFIG_I2C_HID_ACPI=y \
	CONFIG_USB_XHCI_HCD=y CONFIG_USB_EHCI_HCD=y CONFIG_USB_OHCI_HCD=y \
	CONFIG_USB_STORAGE=y CONFIG_SND_HDA_INTEL=y CONFIG_SND_USB_AUDIO=m \
	CONFIG_VFAT_FS=y CONFIG_IGC=m CONFIG_IXGBE=m; do
	grep -Fqx "$symbol" "$KCONFIG" || fail "final kernel config lacks $symbol"
done
grep -Fqx 'CONFIG_CMDLINE_OVERRIDE=y' "$KCONFIG" ||
	fail 'final kernel does not force the EFI-stub command line'
grep -Fq "root=PARTUUID=$ROOT_PARTUUID" "$KCONFIG" ||
	fail 'final kernel lacks the embedded root argument'

echo 'check-image: PASS'
