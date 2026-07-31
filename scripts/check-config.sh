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
require_line board/mboot/rootfs-overlay/etc/mboot.conf 'MBOOT_VCPUS=1'
require_line configs/mboot_x86_64_defconfig 'BR2_TARGET_GRUB2_BUILTIN_CONFIG_PC="$(BR2_EXTERNAL_MBOOT_PATH)/board/mboot/grub-builtin.cfg"'
require_line configs/mboot_x86_64_defconfig '# BR2_TARGET_GRUB2_X86_64_EFI is not set'
require_line board/mboot/grub-builtin.cfg 'search --no-floppy --fs-uuid --set=root 6d426f6f-7400-4b00-8a00-000000000004'
require_line board/mboot/linux.config 'CONFIG_CMDLINE_OVERRIDE=y'
grep -Fq -- '--enable-alsa' board/mboot/qemu-configure-wrapper || fail 'target QEMU ALSA override is missing'

for symbol in CONFIG_EFI CONFIG_ACPI CONFIG_PCI CONFIG_SATA_AHCI \
	CONFIG_BLK_DEV_NVME CONFIG_USB_XHCI_HCD CONFIG_USB_HID CONFIG_SERIO_I8042 \
	CONFIG_INPUT_EVDEV CONFIG_INPUT_UINPUT CONFIG_I2C_HID_ACPI CONFIG_VT CONFIG_EXT4_FS \
	CONFIG_VFAT_FS CONFIG_EFIVAR_FS CONFIG_KVM \
	CONFIG_GENERIC_CPU CONFIG_CPU_SUP_INTEL CONFIG_X86_MCE_INTEL CONFIG_MICROCODE \
	CONFIG_X86_INTEL_PSTATE CONFIG_INTEL_IDLE \
	CONFIG_IOMMU_SUPPORT CONFIG_INTEL_IOMMU CONFIG_AMD_IOMMU CONFIG_THERMAL \
	CONFIG_CPU_FREQ CONFIG_WATCHDOG CONFIG_DRM_I915 CONFIG_DRM_AMDGPU \
	CONFIG_DRM_NOUVEAU CONFIG_DRM_VIRTIO_GPU CONFIG_DRM_VMWGFX CONFIG_DRM_SIMPLEDRM; do
	require_line board/mboot/linux.config "$symbol=y"
done
require_line board/mboot/linux.config 'CONFIG_KVM_INTEL=m'
require_line board/mboot/linux.config 'CONFIG_KVM_AMD=m'
grep -Fq 'root=PARTUUID=6d426f6f-7400-4b00-8a00-000000000001' \
	board/mboot/linux.config || fail 'EFI-stub kernel root argument is missing'
grep -Fq 'cpu=qemu64' board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'KVM guest CPU is not vendor-neutral'

for script in board/mboot/post-build.sh board/mboot/post-image.sh board/mboot/qemu-configure-wrapper \
	board/mboot/rootfs-overlay/etc/init.d/S40xorg \
	board/mboot/rootfs-overlay/etc/init.d/S90mboot \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher; do
	sh -n "$script" || fail "shell syntax: $script"
done
grep -Fq 'MBOOT_MOCHIOS_IMAGE="$(abspath $(MOCHIOS))"' Makefile ||
	fail 'Makefile does not pass the mochiOS image into Buildroot'
grep -Fq 'MOCHIOS_IMAGE=/var/lib/mboot/mochiOS.img' \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'launcher does not use the embedded mochiOS image'

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
