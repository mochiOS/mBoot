#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$ROOT"

fail() { echo "check-config: $*" >&2; exit 1; }
require_line() { grep -Fqx "$2" "$1" || fail "$1 lacks: $2"; }

require_line Makefile 'BUILDROOT_DEFCONFIG := mboot_x86_64_defconfig'
require_line configs/mboot_x86_64_defconfig 'BR2_ROOTFS_POST_BUILD_SCRIPT="$(BR2_EXTERNAL_MBOOT_PATH)/board/mboot/post-build.sh"'
require_line configs/mboot_x86_64_defconfig 'BR2_ROOTFS_POST_IMAGE_SCRIPT="$(BR2_EXTERNAL_MBOOT_PATH)/board/mboot/post-image.sh"'
require_line configs/mboot_x86_64_defconfig '# BR2_TARGET_GENERIC_GETTY is not set'
grep -Fq -- '--enable-alsa' board/mboot/qemu-configure-wrapper || fail 'target QEMU ALSA override is missing'

for symbol in CONFIG_EFI CONFIG_ACPI CONFIG_PCI CONFIG_SATA_AHCI \
	CONFIG_BLK_DEV_NVME CONFIG_USB_XHCI_HCD CONFIG_USB_HID CONFIG_SERIO_I8042 \
	CONFIG_INPUT_EVDEV CONFIG_INPUT_UINPUT CONFIG_I2C_HID_ACPI CONFIG_VT CONFIG_EXT4_FS \
	CONFIG_VFAT_FS CONFIG_EFIVAR_FS CONFIG_KVM CONFIG_KVM_INTEL CONFIG_KVM_AMD \
	CONFIG_IOMMU_SUPPORT CONFIG_INTEL_IOMMU CONFIG_AMD_IOMMU CONFIG_THERMAL \
	CONFIG_CPU_FREQ CONFIG_WATCHDOG CONFIG_DRM_I915 CONFIG_DRM_AMDGPU \
	CONFIG_DRM_NOUVEAU CONFIG_DRM_VIRTIO_GPU CONFIG_DRM_VMWGFX CONFIG_DRM_SIMPLEDRM; do
	require_line board/mboot/linux.config "$symbol=y"
done

for script in board/mboot/post-build.sh board/mboot/post-image.sh board/mboot/qemu-configure-wrapper \
	board/mboot/rootfs-overlay/etc/init.d/S40xorg \
	board/mboot/rootfs-overlay/etc/init.d/S90mboot \
	board/mboot/rootfs-overlay/usr/libexec/mboot-detect-disk \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher; do
	sh -n "$script" || fail "shell syntax: $script"
done

if grep -R -nE '/dev/vdb|vt0?7|killall[[:space:]]+qemu-system' \
	Makefile configs board; then
	fail 'fixed disk/VT or broad QEMU termination pattern remains'
fi
if grep -Fq 'board/pc/' configs/mboot_x86_64_defconfig; then
	fail 'mBoot defconfig still depends on Buildroot board/pc'
fi
if grep -R -n 'UUID_TMP' board/mboot; then
	fail 'an unresolved image UUID placeholder remains'
fi

test -s board/mboot/rootfs-overlay/usr/share/mboot/OVMF_CODE_4M.fd || fail 'OVMF code missing'
test -s board/mboot/rootfs-overlay/usr/share/mboot/OVMF_VARS_4M.fd || fail 'OVMF template missing'
echo 'check-config: PASS'
