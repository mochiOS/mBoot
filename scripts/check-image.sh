#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
OUTPUT=${MBOOT_OUTPUT_DIR:-$ROOT/output}
IMAGES=$OUTPUT/images
TARGET=$OUTPUT/target
GENERATED=$OUTPUT/generated
KCONFIG=$OUTPUT/build/linux-6.12.98/.config
KERNEL_SOURCE=$OUTPUT/build/linux-6.12.98
QEMU_CONFIG=$OUTPUT/build/qemu-9.2.0/build/config-host.h
SDL2_CONFIG=$OUTPUT/staging/usr/include/SDL2/SDL_config.h

fail() { echo "check-image: $*" >&2; exit 1; }

test -f "$GENERATED/boot-layout.env" || fail 'generated boot layout is missing'
. "$GENERATED/boot-layout.env"

for tool in blkid cmp debugfs grep od readelf sfdisk sha256sum stat; do
	command -v "$tool" >/dev/null 2>&1 || fail "required host tool is missing: $tool"
done
for image in disk.img mboot.iso efi-part.vfat rootfs.ext2 bzImage boot.img grub.img; do
	test -s "$IMAGES/$image" || fail "missing image: $image"
done
cmp -s "$IMAGES/disk.img" "$IMAGES/mboot.iso" ||
	fail 'mboot.iso is not identical to the completed raw GPT disk image'
sfdisk --verify "$IMAGES/disk.img" >/dev/null 2>&1 || fail 'GPT verification failed'

partition_dump=$(sfdisk --dump "$IMAGES/disk.img")
printf '%s\n' "$partition_dump" | grep -Fqi "label-id: $MBOOT_DISK_GUID" ||
	fail 'owned GPT disk UUID is missing'
printf '%s\n' "$partition_dump" | grep -Fqi 'type=21686148-6449-6E6F-744E-656564454649' ||
	fail 'GPT BIOS Boot Partition is missing'
root_lines=$(printf '%s\n' "$partition_dump" |
	awk -v uuid="$MBOOT_ROOT_PARTUUID" 'tolower($0) ~ "uuid=" tolower(uuid) { print }')
[ "$(printf '%s\n' "$root_lines" | grep -c .)" -eq 1 ] ||
	fail 'expected root PARTUUID does not identify exactly one partition'
printf '%s\n' "$root_lines" | grep -Fqi "type=$MBOOT_ROOT_PARTITION_TYPE" ||
	fail 'root partition does not use the x86-64 Linux root type GUID'
root_start=$(printf '%s\n' "$root_lines" |
	sed -n 's/.*start=[[:space:]]*\([0-9][0-9]*\).*/\1/p')
root_sectors=$(printf '%s\n' "$root_lines" |
	sed -n 's/.*size=[[:space:]]*\([0-9][0-9]*\).*/\1/p')
[ -n "$root_start" ] && [ -n "$root_sectors" ] || fail 'cannot parse root partition extent'
root_offset=$((root_start * 512))
root_bytes=$((root_sectors * 512))
filesystem_bytes=$(stat -c %s "$IMAGES/rootfs.ext2")
[ "$root_bytes" -eq "$filesystem_bytes" ] || fail 'root partition size differs from root filesystem image'
cmp -n "$root_bytes" -i "$root_offset":0 "$IMAGES/disk.img" "$IMAGES/rootfs.ext2" ||
	fail 'completed disk root partition differs from the validated root filesystem'

[ "$(blkid -s UUID -o value "$IMAGES/rootfs.ext2")" = "$MBOOT_ROOT_FSUUID" ] ||
	fail 'root filesystem UUID differs from boot-layout.conf'
[ "$(blkid -s TYPE -o value "$IMAGES/rootfs.ext2")" = "$MBOOT_ROOT_FSTYPE" ] ||
	fail 'root filesystem type differs from boot-layout.conf'
[ "$(blkid -s UUID -o value "$IMAGES/efi-part.vfat" | tr '[:upper:]' '[:lower:]' | tr -d '-')" = "$MBOOT_EFI_VOLUME_ID" ] ||
	fail 'EFI filesystem volume ID differs from boot-layout.conf'
debugfs -R 'stat /' "$IMAGES/rootfs.ext2" 2>&1 | grep -Fq 'Inode: 2' ||
	fail 'root filesystem is unreadable'

[ "$(od -An -tx1 -j 510 -N 2 "$IMAGES/disk.img" | tr -d ' \n')" = 55aa ] ||
	fail 'BIOS MBR signature is missing'
[ "$(od -An -tu8 -j 92 -N 8 "$IMAGES/disk.img" | tr -d ' ')" = 2048 ] ||
	fail 'BIOS boot sector does not point to the core image'
[ "$(od -An -tu8 -j 1049076 -N 8 "$IMAGES/disk.img" | tr -d ' ')" = 2049 ] ||
	fail 'BIOS core-image blocklist is not embedded at the expected LBA'

bios_cfg=$(debugfs -R 'cat /boot/grub/grub.cfg' "$IMAGES/rootfs.ext2" 2>/dev/null)
printf '%s\n' "$bios_cfg" | grep -Fq "$MBOOT_KERNEL_CMDLINE" ||
	fail 'BIOS GRUB command line differs from boot-layout.conf'
grep -Fqx "CONFIG_CMDLINE=\"$MBOOT_KERNEL_CMDLINE\"" "$KCONFIG" ||
	fail 'EFI-stub kernel command line differs from boot-layout.conf'
case " $MBOOT_KERNEL_CMDLINE " in
	*' rootwait='*) : ;;
	*) fail 'final kernel lacks a bounded root device timeout' ;;
esac
grep -Fq 'rootwait=' "$KERNEL_SOURCE/Documentation/admin-guide/kernel-parameters.txt" ||
	fail 'selected kernel does not implement bounded rootwait syntax'

temporary=$(mktemp -d /tmp/mboot-check-image.XXXXXX)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
"$OUTPUT/host/bin/mcopy" -i "$IMAGES/efi-part.vfat" \
	::/EFI/BOOT/BOOTX64.EFI "$temporary/BOOTX64.EFI"
cmp -s "$IMAGES/bzImage" "$temporary/BOOTX64.EFI" ||
	fail 'EFI fallback loader is not the Linux EFI-stub kernel'

for path in etc/init.d/S03mboot-root etc/init.d/S10udev etc/init.d/S40xorg \
	etc/init.d/S80mbootd etc/init.d/S90mboot \
	etc/init.d/S95mboot-diagnostics \
	usr/libexec/mboot-launcher usr/sbin/mbootd usr/bin/qemu-system-x86_64 usr/bin/Xorg \
	usr/bin/xterm usr/bin/xcalc usr/bin/xclock usr/bin/x86_64-elf-gcc; do
	test -x "$TARGET/$path" || fail "missing target executable: /$path"
	debugfs -R "stat /$path" "$IMAGES/rootfs.ext2" 2>&1 | grep -Fq 'Inode:' ||
		fail "root filesystem lacks: /$path"
done
for path in usr/lib/mochios-sdk/crt0.o \
	usr/lib/mochios-sdk/libmochi_user_newlib_runtime.a \
	usr/lib/mochios-sdk/linker.ld usr/lib/mochios-sdk/x86_64-elf/lib/libc.a \
	usr/lib/mochios-sdk/x86_64-elf/include/stdio.h; do
	test -s "$TARGET/$path" || fail "missing mochiOS SDK file: /$path"
	debugfs -R "stat /$path" "$IMAGES/rootfs.ext2" 2>&1 | grep -Fq 'Inode:' ||
		fail "root filesystem lacks SDK file: /$path"
done
debugfs -R 'stat /etc/mboot-boot.conf' "$IMAGES/rootfs.ext2" 2>&1 | grep -Fq 'Inode:' ||
	fail 'root filesystem lacks generated boot diagnostics configuration'
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

[ "$(cat "$TARGET/etc/hostname")" = "$MBOOT_HOSTNAME" ] || fail 'target hostname is not mboot'
image_hostname=$(debugfs -R 'cat /etc/hostname' "$IMAGES/rootfs.ext2" 2>/dev/null)
[ "$image_hostname" = "$MBOOT_HOSTNAME" ] || fail 'image hostname is not mboot'
image_passwd=$(debugfs -R 'cat /etc/passwd' "$IMAGES/rootfs.ext2" 2>/dev/null)
image_group=$(debugfs -R 'cat /etc/group' "$IMAGES/rootfs.ext2" 2>/dev/null)
printf '%s\n' "$image_passwd" | grep -Fq 'root:x:0:0:' ||
	fail 'root filesystem passwd database is unreadable or invalid'
printf '%s\n' "$image_passwd" | awk -F: '$3 >= 1000 && $3 < 65534 { exit 1 }' ||
	fail 'root filesystem contains an unintended regular user'
for identity in "${USER:-}" "${LOGNAME:-}" "$(hostname 2>/dev/null || true)" jnix mochimochi; do
	case "$identity" in ''|root|mboot|localhost) continue ;; esac
	if printf '%s\n%s\n%s\n' "$image_hostname" "$image_passwd" "$image_group" |
		grep -Fqi "$identity"; then
		fail "host identity leaked into root filesystem: $identity"
	fi
	if grep -R -a -Fqi --exclude=mochiOS.img "$identity" "$TARGET" 2>/dev/null; then
		fail "host identity leaked into root filesystem configuration: $identity"
	fi
done

grep -Fqx '#define CONFIG_AUDIO_ALSA' "$QEMU_CONFIG" || fail 'target QEMU lacks ALSA support'
grep -Fqx '#define CONFIG_OPENGL' "$QEMU_CONFIG" || fail 'target QEMU lacks OpenGL support'
grep -Fqx '#define SDL_VIDEO_OPENGL_EGL 1' "$SDL2_CONFIG" ||
	fail 'target SDL2 lacks EGL context support'
grep -Fqx '#define SDL_VIDEO_OPENGL_GLX 1' "$SDL2_CONFIG" ||
	fail 'target SDL2 lacks GLX fallback support'
grep -Eq '^#define VIRGL_VERSION_MAJOR [1-9][0-9]*$' "$QEMU_CONFIG" ||
	fail 'target QEMU lacks VirGL renderer support'
test -s "$TARGET/usr/lib/libvirglrenderer.so.1" || fail 'VirGL renderer library is missing'
debugfs -R 'stat /usr/lib/libvirglrenderer.so.1' "$IMAGES/rootfs.ext2" 2>&1 |
	grep -Fq 'Inode:' || fail 'root filesystem lacks VirGL renderer library'
test ! -e "$TARGET/usr/bin/virgl_test_server" ||
	fail 'VirGL development test server remains in target'
grep -q '^root:!:' "$TARGET/etc/shadow" || fail 'root account is not locked'
! grep -q '^[^#].*getty' "$TARGET/etc/inittab" || fail 'an interactive getty is enabled'
readelf -l "$TARGET/usr/sbin/mbootd" | grep -Fq 'INTERP' &&
	fail 'mbootd depends on a development-host dynamic loader'
readelf -d "$TARGET/usr/lib/xorg/modules/drivers/modesetting_drv.so" |
	grep -Fq 'Shared library: [libgbm.so.1]' || fail 'modesetting GBM dependency is missing'
readelf -d "$TARGET/usr/lib/xorg/modules/drivers/fbdev_drv.so" |
	grep -Fq 'Shared library: [libfbdevhw.so]' || fail 'fbdevhw dependency is missing'
[ "$("$OUTPUT/host/bin/patchelf" --print-rpath "$TARGET/usr/lib/xorg/modules/drivers/fbdev_drv.so")" = '$ORIGIN/..' ] ||
	fail 'fbdevhw module search path is incorrect'

for symbol in CONFIG_EFI_PARTITION=y CONFIG_SCSI=y CONFIG_BLK_DEV_SD=y \
	CONFIG_USB=y CONFIG_USB_XHCI_HCD=y CONFIG_USB_XHCI_PCI=y \
	CONFIG_USB_EHCI_HCD=y CONFIG_USB_EHCI_PCI=y CONFIG_USB_OHCI_HCD=y \
	CONFIG_USB_OHCI_HCD_PCI=y CONFIG_USB_UHCI_HCD=y CONFIG_USB_STORAGE=y \
	CONFIG_USB_UAS=y CONFIG_EXT4_FS=y CONFIG_DRM_I915=m CONFIG_DRM_AMDGPU=m \
	CONFIG_DRM_NOUVEAU=m CONFIG_DRM_VIRTIO_GPU=y CONFIG_DRM_VMWGFX=y \
	CONFIG_DRM_SIMPLEDRM=y CONFIG_KVM_INTEL=m CONFIG_KVM_AMD=m \
	CONFIG_SATA_AHCI=y CONFIG_BLK_DEV_NVME=y CONFIG_GENERIC_CPU=y \
	CONFIG_CPU_SUP_INTEL=y CONFIG_X86_MCE_INTEL=y CONFIG_MICROCODE=y \
	CONFIG_X86_INTEL_PSTATE=y CONFIG_INTEL_IDLE=y CONFIG_EFI_STUB=y \
	CONFIG_TRANSPARENT_HUGEPAGE=y CONFIG_TRANSPARENT_HUGEPAGE_MADVISE=y \
	CONFIG_EFIVAR_FS=y CONFIG_INTEL_IOMMU=y CONFIG_AMD_IOMMU=y \
	CONFIG_INPUT_EVDEV=y CONFIG_INPUT_KEYBOARD=y CONFIG_KEYBOARD_ATKBD=y \
	CONFIG_MOUSE_PS2=y CONFIG_HID_MULTITOUCH=m CONFIG_I2C_HID_ACPI=y \
	CONFIG_SND_HDA_INTEL=y CONFIG_SND_USB_AUDIO=m CONFIG_VFAT_FS=y \
	CONFIG_IGC=m CONFIG_IXGBE=m; do
	grep -Fqx "$symbol" "$KCONFIG" || fail "final kernel config lacks $symbol"
done
test -s "$TARGET/lib/firmware/i915/tgl_dmc_ver2_12.bin" ||
	fail 'required Intel Tiger Lake DMC firmware is missing'
debugfs -R 'stat /lib/firmware/i915/tgl_dmc_ver2_12.bin' "$IMAGES/rootfs.ext2" 2>&1 |
	grep -Fq 'Inode:' || fail 'root filesystem lacks Intel Tiger Lake DMC firmware'
modules_roots=$(find "$TARGET/lib/modules" -mindepth 1 -maxdepth 1 -type d -print)
[ "$(printf '%s\n' "$modules_roots" | grep -c .)" -eq 1 ] ||
	fail 'cannot uniquely locate the kernel module directory'
modules_root=$modules_roots
for module in i915 amdgpu nouveau; do
	module_paths=$(find "$TARGET/lib/modules" -type f -name "$module.ko" -print)
	[ "$(printf '%s\n' "$module_paths" | grep -c .)" -eq 1 ] ||
		fail "cannot uniquely locate GPU module: $module.ko"
	module_path=${module_paths#"$TARGET"}
	debugfs -R "stat $module_path" "$IMAGES/rootfs.ext2" 2>&1 | grep -Fq 'Inode:' ||
		fail "root filesystem lacks GPU module: $module.ko"
	module_relative=${module_paths#"$modules_root"/}
	grep -Fq "$module_relative:" "$modules_root/modules.dep" ||
		fail "module dependency index lacks: $module.ko"
done

echo 'check-image: PASS'
