#!/bin/sh

set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
OUTPUT=${MBOOT_OUTPUT_DIR:-$ROOT/output}
IMAGES=$OUTPUT/images
FIRMWARE=$OUTPUT/target/usr/share/mboot
QEMU=${QEMU:-qemu-system-x86_64}
ACCELERATOR=${QEMU_ACCELERATOR:-auto}
CONTROLLER=${MBOOT_USB_CONTROLLER:-xhci}
TIMEOUT_SECONDS=${MBOOT_USB_BOOT_TIMEOUT_SECONDS:-}

fail() { echo "test-qemu-usb-boot: $*" >&2; exit 1; }

test -s "$IMAGES/disk.img" || fail "missing image: $IMAGES/disk.img"
test -s "$FIRMWARE/OVMF_CODE_4M.fd" || fail "missing OVMF code: $FIRMWARE/OVMF_CODE_4M.fd"
test -s "$FIRMWARE/OVMF_VARS_4M.fd" || fail "missing OVMF variables: $FIRMWARE/OVMF_VARS_4M.fd"
test -f "$OUTPUT/generated/boot-layout.env" || fail 'generated boot layout is missing'
. "$OUTPUT/generated/boot-layout.env"

case "$ACCELERATOR" in
	auto)
		if [ -r /dev/kvm ] && [ -w /dev/kvm ] &&
			grep -Eq '^flags[[:space:]]*:.*[[:space:]](vmx|svm)([[:space:]]|$)' /proc/cpuinfo; then
			ACCELERATOR=kvm
		else
			ACCELERATOR=tcg
		fi
		;;
	kvm|tcg) : ;;
	*) fail "invalid QEMU accelerator: $ACCELERATOR" ;;
esac

if [ -z "$TIMEOUT_SECONDS" ]; then
	case "$ACCELERATOR:$CONTROLLER" in
		kvm:ehci) TIMEOUT_SECONDS=90 ;;
		kvm:*) TIMEOUT_SECONDS=60 ;;
		tcg:ehci) TIMEOUT_SECONDS=240 ;;
		*) TIMEOUT_SECONDS=180 ;;
	esac
fi
case "$TIMEOUT_SECONDS" in
	''|*[!0-9]*) fail 'timeout must be a positive integer' ;;
esac
[ "$TIMEOUT_SECONDS" -gt 0 ] || fail 'timeout must be a positive integer'

temporary=$(mktemp -d /tmp/mboot-qemu-usb.XXXXXX)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
disk=$temporary/mboot.img
vars=$temporary/OVMF_VARS.fd
serial=$temporary/serial.log
rootfs=$temporary/rootfs.ext4

cp --reflink=auto --sparse=always "$IMAGES/disk.img" "$disk"
cp "$FIRMWARE/OVMF_VARS_4M.fd" "$vars"

if [ "$ACCELERATOR" = kvm ]; then
	set -- -accel kvm -cpu host,-vmx,-svm
else
	set -- -accel tcg -cpu qemu64,-vmx,-svm
fi

case "$CONTROLLER" in
	xhci)
		set -- "$@" -device qemu-xhci,id=usb-controller
		controller_log='using xhci_hcd'
		;;
	ehci)
		set -- "$@" -device ich9-usb-ehci1,id=usb-controller
		controller_log='using ehci-pci'
		;;
	*) fail "invalid USB controller: $CONTROLLER" ;;
esac

set +e
timeout "$TIMEOUT_SECONDS" "$QEMU" "$@" \
	-machine q35,i8042=off -smp 4 -m 4096 \
	-drive "if=pflash,format=raw,readonly=on,file=$FIRMWARE/OVMF_CODE_4M.fd" \
	-drive "if=pflash,format=raw,file=$vars" \
	-drive "file=$disk,format=raw,if=none,id=mboot" \
	-device usb-storage,drive=mboot,bus=usb-controller.0,serial=MBOOT \
	-device virtio-vga -display none -monitor none -serial "file:$serial"
qemu_status=$?
set -e
[ "$qemu_status" -eq 124 ] || fail "QEMU exited unexpectedly with status $qemu_status"

grep -Fq 'UEFI QEMU QEMU USB HARDDRIVE MBOOT' "$serial" ||
	fail 'firmware did not start the USB mass-storage image'

partition_dump=$(sfdisk --dump "$disk")
root_line=$(printf '%s\n' "$partition_dump" |
	awk -v uuid="$MBOOT_ROOT_PARTUUID" 'tolower($0) ~ "uuid=" tolower(uuid) { print }')
[ "$(printf '%s\n' "$root_line" | grep -c .)" -eq 1 ] ||
	fail 'expected root PARTUUID does not identify exactly one partition'
root_start=$(printf '%s\n' "$root_line" |
	sed -n 's/.*start=[[:space:]]*\([0-9][0-9]*\).*/\1/p')
root_sectors=$(printf '%s\n' "$root_line" |
	sed -n 's/.*size=[[:space:]]*\([0-9][0-9]*\).*/\1/p')
[ -n "$root_start" ] && [ -n "$root_sectors" ] || fail 'cannot parse root partition extent'
dd if="$disk" of="$rootfs" bs=512 skip="$root_start" count="$root_sectors" \
	conv=sparse status=none

boot_log=$(debugfs -R 'cat /var/log/mboot/boot.log' "$rootfs" 2>/dev/null)
kernel_log=$(debugfs -R 'cat /var/log/mboot/kernel.log' "$rootfs" 2>/dev/null)
guest_log=$(debugfs -R 'cat /var/log/mboot/mochios.log' "$rootfs" 2>/dev/null)

printf '%s\n' "$boot_log" | grep -Fq "expected root: PARTUUID=$MBOOT_ROOT_PARTUUID type=$MBOOT_ROOT_FSTYPE" ||
	fail 'root diagnostics do not contain the expected boot identity'
printf '%s\n' "$boot_log" | grep -Fq '/dev/root / ext4 rw' ||
	fail 'USB root filesystem was not mounted as ext4'
printf '%s\n' "$boot_log" | grep -Eq '[[:space:]]sda3$' ||
	fail 'USB root partition was not detected as sda3'
printf '%s\n' "$kernel_log" | grep -Fq "$controller_log" ||
	fail "kernel log lacks $CONTROLLER USB enumeration"
printf '%s\n' "$kernel_log" | grep -Fq 'Direct-Access     QEMU     QEMU HARDDISK' ||
	fail 'kernel log lacks SCSI USB mass-storage enumeration'
printf '%s\n' "$kernel_log" | grep -Fq 'VFS: Mounted root (ext4 filesystem) on device 8:3.' ||
	fail 'kernel did not mount the USB ext4 partition as root'
printf '%s\n' "$guest_log" | grep -Fq "exec: loaded 'core.service'" ||
	fail 'embedded mochiOS did not reach userspace'

echo "test-qemu-usb-boot: PASS ($ACCELERATOR, $CONTROLLER)"
