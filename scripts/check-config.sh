#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
GENERATED=${MBOOT_BOOT_CONFIG_DIR:-$ROOT/output/generated}
cd "$ROOT"

fail() { echo "check-config: $*" >&2; exit 1; }
require_line() { grep -Fqx "$2" "$1" || fail "$1 lacks: $2"; }

scripts/generate-boot-config.sh "$GENERATED"
. "$GENERATED/boot-layout.env"

require_line Makefile 'BUILDROOT_DEFCONFIG := $(BOOT_CONFIG_DIR)/mboot_x86_64_defconfig'
require_line Makefile 'OUTPUT_COMPATIBILITY_VERSION := 2'
require_line configs/mboot_x86_64_defconfig.in 'BR2_REPRODUCIBLE=y'
require_line "$GENERATED/mboot_x86_64_defconfig" 'BR2_TARGET_GENERIC_HOSTNAME="mboot"'
require_line "$GENERATED/mboot_x86_64_defconfig" 'BR2_ROOTFS_POST_BUILD_SCRIPT="$(BR2_EXTERNAL_MBOOT_PATH)/board/mboot/post-build.sh"'
require_line "$GENERATED/mboot_x86_64_defconfig" 'BR2_ROOTFS_POST_IMAGE_SCRIPT="$(BR2_EXTERNAL_MBOOT_PATH)/board/mboot/post-image.sh"'
require_line "$GENERATED/mboot_x86_64_defconfig" '# BR2_TARGET_GENERIC_GETTY is not set'
require_line "$GENERATED/mboot_x86_64_defconfig" 'BR2_LINUX_KERNEL_CUSTOM_CONFIG_FILE="$(BR2_EXTERNAL_MBOOT_PATH)/output/generated/linux.config"'
require_line "$GENERATED/mboot_x86_64_defconfig" 'BR2_TARGET_GRUB2_BUILTIN_CONFIG_PC="$(BR2_EXTERNAL_MBOOT_PATH)/output/generated/grub-builtin.cfg"'
require_line "$GENERATED/mboot_x86_64_defconfig" '# BR2_TARGET_GRUB2_X86_64_EFI is not set'
require_line board/mboot/rootfs-overlay/etc/mboot.conf 'MBOOT_VCPUS='
require_line board/mboot/rootfs-overlay/etc/mboot.conf 'MBOOT_ACCELERATOR=auto'
require_line "$GENERATED/grub-builtin.cfg" "search --no-floppy --fs-uuid --set=root $MBOOT_ROOT_FSUUID"
require_line "$GENERATED/linux.config" 'CONFIG_CMDLINE_OVERRIDE=y'
require_line "$GENERATED/linux.config" "CONFIG_CMDLINE=\"$MBOOT_KERNEL_CMDLINE\""
grep -Fq "linux /boot/bzImage $MBOOT_KERNEL_CMDLINE" "$GENERATED/grub-bios.cfg" ||
	fail 'generated BIOS command line differs from boot-layout.conf'
grep -Fq "partition-uuid = $MBOOT_ROOT_PARTUUID" "$GENERATED/genimage.cfg" ||
	fail 'generated root PARTUUID is missing'
grep -Fq "partition-type-uuid = $MBOOT_ROOT_PARTITION_TYPE" "$GENERATED/genimage.cfg" ||
	fail 'generated x86-64 root partition type is missing'

case " $MBOOT_KERNEL_CMDLINE " in
	*' rootwait='*) : ;;
	*) fail 'kernel command line lacks a bounded rootwait timeout' ;;
esac
case " $MBOOT_KERNEL_CMDLINE " in
	*' rootwait '*) fail 'kernel command line still contains an infinite rootwait' ;;
esac
case " $MBOOT_KERNEL_CMDLINE " in
	*" root=PARTUUID=$MBOOT_ROOT_PARTUUID "*) : ;;
	*) fail 'kernel command line does not use the owned root PARTUUID' ;;
esac
case " $MBOOT_KERNEL_CMDLINE " in
	*" rootfstype=$MBOOT_ROOT_FSTYPE "*) : ;;
	*) fail 'kernel command line does not name the root filesystem type' ;;
esac

for symbol in CONFIG_EFI CONFIG_PARTITION_ADVANCED CONFIG_EFI_PARTITION CONFIG_ACPI CONFIG_PCI \
	CONFIG_SCSI CONFIG_BLK_DEV_SD CONFIG_SATA_AHCI CONFIG_BLK_DEV_NVME \
	CONFIG_USB CONFIG_USB_XHCI_HCD CONFIG_USB_XHCI_PCI CONFIG_USB_EHCI_HCD \
	CONFIG_USB_EHCI_PCI CONFIG_USB_OHCI_HCD CONFIG_USB_OHCI_HCD_PCI \
	CONFIG_USB_UHCI_HCD CONFIG_USB_STORAGE CONFIG_USB_UAS CONFIG_USB_HID \
	CONFIG_SERIO_I8042 CONFIG_INPUT_EVDEV CONFIG_INPUT_UINPUT CONFIG_I2C_HID_ACPI \
	CONFIG_VT CONFIG_EXT4_FS CONFIG_VFAT_FS CONFIG_EFIVAR_FS CONFIG_KVM \
	CONFIG_VIRTIO_INPUT CONFIG_GENERIC_CPU CONFIG_CPU_SUP_INTEL CONFIG_X86_MCE_INTEL \
	CONFIG_MICROCODE CONFIG_X86_INTEL_PSTATE CONFIG_INTEL_IDLE CONFIG_IOMMU_SUPPORT \
	CONFIG_INTEL_IOMMU CONFIG_AMD_IOMMU CONFIG_THERMAL CONFIG_CPU_FREQ \
	CONFIG_WATCHDOG CONFIG_TRANSPARENT_HUGEPAGE CONFIG_TRANSPARENT_HUGEPAGE_MADVISE \
	CONFIG_DRM_VIRTIO_GPU CONFIG_DRM_VMWGFX CONFIG_DRM_SIMPLEDRM; do
	require_line "$GENERATED/linux.config" "$symbol=y"
done
for symbol in CONFIG_DRM_I915 CONFIG_DRM_AMDGPU CONFIG_DRM_NOUVEAU; do
	require_line "$GENERATED/linux.config" "$symbol=m"
done
require_line "$GENERATED/linux.config" 'CONFIG_KVM_INTEL=m'
require_line "$GENERATED/linux.config" 'CONFIG_KVM_AMD=m'

root_uuid_sources=$(grep -R -l -F "$MBOOT_ROOT_PARTUUID" \
	Makefile configs board scripts readme.md 2>/dev/null || true)
[ "$root_uuid_sources" = board/mboot/boot-layout.conf ] || {
	printf '%s\n' "$root_uuid_sources" >&2
	fail 'root PARTUUID is defined outside boot-layout.conf'
}
fs_uuid_sources=$(grep -R -l -F "$MBOOT_ROOT_FSUUID" \
	Makefile configs board scripts readme.md 2>/dev/null || true)
[ "$fs_uuid_sources" = board/mboot/boot-layout.conf ] || {
	printf '%s\n' "$fs_uuid_sources" >&2
	fail 'root filesystem UUID is defined outside boot-layout.conf'
}

grep -Fq -- '--enable-alsa' board/mboot/qemu-configure-wrapper ||
	fail 'target QEMU ALSA override is missing'
grep -Fq -- '--enable-opengl' board/mboot/qemu-configure-wrapper ||
	fail 'target QEMU OpenGL override is missing'
grep -Fq -- '--enable-virglrenderer' board/mboot/qemu-configure-wrapper ||
	fail 'target QEMU VirGL renderer override is missing'
require_line configs/mboot_x86_64_defconfig.in 'BR2_PACKAGE_MESA3D_OPENGL_EGL=y'
require_line configs/mboot_x86_64_defconfig.in 'BR2_PACKAGE_VIRGLRENDERER=y'
require_line configs/mboot_x86_64_defconfig.in 'BR2_PACKAGE_SDL2_OPENGL=y'
require_line board/mboot/rootfs-overlay/etc/mboot.conf 'MBOOT_GUEST_GPU=virtio-vga-gl'
require_line board/mboot/rootfs-overlay/etc/mboot.conf 'MBOOT_GUEST_WIDTH=auto'
require_line board/mboot/rootfs-overlay/etc/mboot.conf 'MBOOT_GUEST_HEIGHT=auto'
grep -Fq 'virtio-vga-gl|virtio-gpu-gl-pci) display_gl=on' \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'inner QEMU VirGL display selection is missing'
grep -Fq -- '-display "sdl,gl=$display_gl,show-cursor=off,window-close=off"' \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'inner QEMU SDL OpenGL selection is missing'
grep -Fq 'active_display_geometry()' board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'active display geometry detection is missing'
grep -Fq 'gpu_device=$gpu,xres=$guest_width,yres=$guest_height' \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'inner QEMU native display resolution is missing'
grep -Fq 'cpu=qemu64,+x2apic' board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'guest CPU is not vendor-neutral with x2APIC enabled'
grep -Fq 'host_cpus=$(online_cpu_count)' board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'online host CPU discovery is missing'
if grep -Eq -- 'virtio-(keyboard|mouse)-pci' board/mboot/rootfs-overlay/usr/libexec/mboot-launcher; then
	fail 'inner QEMU must use the mochiOS-supported q35 i8042 input path'
fi
grep -Fq -- '-device qemu-xhci,id=xhci' Makefile || fail 'outer QEMU USB controller is missing'
grep -Fq -- '-device usb-kbd,bus=xhci.0' Makefile || fail 'outer QEMU keyboard is missing'
grep -Fq -- '-device usb-mouse,bus=xhci.0' Makefile || fail 'outer QEMU mouse is missing'
grep -Fq 'RUN_DISK_IMAGE := $(OUTPUT_DIR)/run/mboot.img' Makefile ||
	fail 'outer QEMU runtime image is not isolated from the build artifact'
grep -Fq 'QEMU_FULLSCREEN ?= yes' Makefile || fail 'outer QEMU is not fullscreen by default'
grep -Fq 'auto) cache=writethrough' board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'embedded image auto cache mode does not use buffered reads'
grep -Fq 'warming embedded mochiOS image into host page cache' \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'embedded image cache warmup is missing'
grep -Fq '|| gui_fullscreen)' board/mboot/patches/qemu/0001-sdl-keep-fullscreen-window-size.patch ||
	fail 'inner QEMU fullscreen resize patch is missing'
grep -Fq 'SDL_SetWindowInputFocus(scon->real_window)' board/mboot/patches/qemu/0001-sdl-keep-fullscreen-window-size.patch ||
	fail 'inner QEMU fullscreen input focus patch is missing'
grep -Fq 'if (gui_grab || gui_fullscreen ||' board/mboot/patches/qemu/0001-sdl-keep-fullscreen-window-size.patch ||
	fail 'inner QEMU fullscreen input forwarding patch is missing'
grep -Fq 'SDL_HINT_VIDEO_X11_FORCE_EGL' board/mboot/patches/qemu/0002-sdl-prefer-egl-for-virgl.patch ||
	fail 'inner QEMU SDL EGL preference patch is missing'
grep -Fq 'qemu_egl_display = eglGetCurrentDisplay()' board/mboot/patches/qemu/0002-sdl-prefer-egl-for-virgl.patch ||
	fail 'inner QEMU SDL EGL display handoff is missing'
grep -Fq 'SDL_GL_MakeCurrent(scon->real_window, ctx) != 0' board/mboot/patches/qemu/0002-sdl-prefer-egl-for-virgl.patch ||
	fail 'inner QEMU VirGL context validation is missing'
grep -Fq 'QEMU_POST_PATCH_HOOKS += MBOOT_QEMU_APPLY_MBOOT_PATCHES' external.mk ||
	fail 'inner QEMU patch hook is missing'
grep -Fq 'QEMU_PRE_CONFIGURE_HOOKS += MBOOT_QEMU_INSTALL_CONFIGURE_WRAPPER' external.mk ||
	fail 'inner QEMU configure wrapper hook is missing'
grep -Fq 'QEMU_CONFIG_INPUTS :=' Makefile ||
	fail 'inner QEMU cache invalidation inputs are missing'
grep -Fq 'board/mboot/qemu-configure-wrapper' Makefile ||
	fail 'inner QEMU configure wrapper is not a cache invalidation input'
grep -Fq '$(CURDIR)/configs/mboot_x86_64_defconfig.in' Makefile ||
	fail 'inner QEMU display configuration is not a cache invalidation input'
grep -Fq '$(CURDIR)/external.mk' Makefile ||
	fail 'inner QEMU package configuration is not a cache invalidation input'
grep -Fq '$(CURDIR)/package/virglrenderer/virglrenderer.mk' Makefile ||
	fail 'VirGL renderer configuration is not a cache invalidation input'
grep -Fq -- '-Dplatforms=egl' package/virglrenderer/virglrenderer.mk ||
	fail 'VirGL renderer EGL platform is missing'

for script in board/mboot/post-build.sh board/mboot/post-image.sh \
	board/mboot/qemu-configure-wrapper board/mboot/rootfs-overlay/etc/init.d/S40xorg \
	board/mboot/rootfs-overlay/etc/init.d/S03mboot-root \
	board/mboot/rootfs-overlay/etc/init.d/S80mbootd \
	board/mboot/rootfs-overlay/etc/init.d/S90mboot \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher \
	scripts/generate-boot-config.sh scripts/update-config-template.sh \
	scripts/check-image.sh; do
	sh -n "$script" || fail "shell syntax: $script"
done
grep -Fq 'MBOOT_MOCHIOS_IMAGE="$(abspath $(MOCHIOS))"' Makefile ||
	fail 'Makefile does not pass the mochiOS image into Buildroot'
grep -Fq 'MBOOTD_BINARY="$(MBOOTD_BINARY)"' Makefile ||
	fail 'Makefile does not pass mbootd into Buildroot'
grep -Fq 'MBOOT_BOOT_CONFIG_DIR="$(BOOT_CONFIG_DIR)"' Makefile ||
	fail 'Makefile does not pass generated boot configuration into Buildroot'
grep -Fq 'MBOOT_SOURCE_DATE_EPOCH="$(MBOOT_SOURCE_DATE_EPOCH)"' Makefile ||
	fail 'Makefile does not pass a reproducible timestamp into Buildroot hooks'
grep -Fq 'target-feature=+crt-static' Makefile ||
	fail 'mbootd is not built independently of the host dynamic loader'
grep -Fq -- '-chardev socket,id=mbootctl,path=/run/mboot/mochios-control.sock,server=off,reconnect-ms=1000' \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'mBoot control chardev is missing or is not reconnectable'
grep -Fq -- '-device virtio-serial-pci,id=mboot-serial,disable-legacy=on,max_ports=2' \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'dedicated modern virtio-serial controller is missing'
grep -Fq -- 'name=org.mochios.mboot.control' \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'mBoot control port name is missing'
grep -Fq 'MOCHIOS_IMAGE=/var/lib/mboot/mochiOS.img' \
	board/mboot/rootfs-overlay/usr/libexec/mboot-launcher ||
	fail 'launcher does not use the embedded mochiOS image'

if grep -R -nE '/dev/vdb|vt0?7|killall[[:space:]]+qemu-system' Makefile configs board; then
	fail 'fixed disk/VT or broad QEMU termination pattern remains'
fi
if grep -Fq 'board/pc/' configs/mboot_x86_64_defconfig.in; then
	fail 'mBoot defconfig still depends on Buildroot board/pc'
fi
if grep -R -n 'UUID_TMP' board/mboot; then
	fail 'an unresolved image UUID placeholder remains'
fi

test -s board/mboot/rootfs-overlay/usr/share/mboot/OVMF_CODE_4M.fd || fail 'OVMF code missing'
test -s board/mboot/rootfs-overlay/usr/share/mboot/OVMF_VARS_4M.fd || fail 'OVMF template missing'
echo 'check-config: PASS'
