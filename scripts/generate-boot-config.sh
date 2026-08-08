#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
LAYOUT=$ROOT/board/mboot/boot-layout.conf
OUTPUT=${1:-$ROOT/output/generated}

fail() { echo "generate-boot-config: $*" >&2; exit 1; }

test -f "$LAYOUT" || fail "missing $LAYOUT"
# This file intentionally assigns every value rather than inheriting host
# variables. Builds must not vary with USER, HOME, or the host name.
. "$LAYOUT"

is_guid() {
	printf '%s\n' "$1" | grep -Eq '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
}

for value in "$MBOOT_DISK_GUID" "$MBOOT_ROOT_PARTUUID" \
	"$MBOOT_BIOS_PARTUUID" "$MBOOT_EFI_PARTUUID" \
	"$MBOOT_ROOT_FSUUID" "$MBOOT_ROOT_PARTITION_TYPE"; do
	is_guid "$value" || fail "invalid GUID: $value"
done
[ "$MBOOT_ROOT_FSTYPE" = ext4 ] || fail "unsupported root filesystem: $MBOOT_ROOT_FSTYPE"
printf '%s\n' "$MBOOT_ROOTWAIT_SECONDS" | grep -Eq '^[1-9][0-9]*$' ||
	fail "root wait must be a positive number of seconds"
printf '%s\n' "$MBOOT_HOSTNAME" | grep -Eq '^[a-z][a-z0-9-]{0,62}$' ||
	fail "invalid hostname: $MBOOT_HOSTNAME"
printf '%s\n' "$MBOOT_KERNEL_LOGLEVEL" | grep -Eq '^[0-7]$' ||
	fail "invalid kernel log level: $MBOOT_KERNEL_LOGLEVEL"
printf '%s\n' "$MBOOT_EFI_VOLUME_ID" | grep -Eq '^[0-9a-f]{8}$' ||
	fail "invalid EFI FAT volume ID: $MBOOT_EFI_VOLUME_ID"

MBOOT_KERNEL_CMDLINE="root=PARTUUID=$MBOOT_ROOT_PARTUUID rootwait=$MBOOT_ROOTWAIT_SECONDS rootfstype=$MBOOT_ROOT_FSTYPE rw $MBOOT_KERNEL_CONSOLES loglevel=$MBOOT_KERNEL_LOGLEVEL"
export MBOOT_DISK_GUID MBOOT_ROOT_PARTUUID MBOOT_BIOS_PARTUUID
export MBOOT_EFI_PARTUUID MBOOT_ROOT_FSUUID MBOOT_ROOT_PARTITION_TYPE
export MBOOT_ROOT_FSTYPE MBOOT_ROOTWAIT_SECONDS MBOOT_HOSTNAME
export MBOOT_KERNEL_CMDLINE MBOOT_EFI_VOLUME_ID

mkdir -p "$OUTPUT"

render() {
	input=$1
	output=$2
	temporary=$output.new
	sed \
		-e "s|@MBOOT_DISK_GUID@|$MBOOT_DISK_GUID|g" \
		-e "s|@MBOOT_ROOT_PARTUUID@|$MBOOT_ROOT_PARTUUID|g" \
		-e "s|@MBOOT_BIOS_PARTUUID@|$MBOOT_BIOS_PARTUUID|g" \
		-e "s|@MBOOT_EFI_PARTUUID@|$MBOOT_EFI_PARTUUID|g" \
		-e "s|@MBOOT_ROOT_FSUUID@|$MBOOT_ROOT_FSUUID|g" \
		-e "s|@MBOOT_EFI_VOLUME_ID@|$MBOOT_EFI_VOLUME_ID|g" \
		-e "s|@MBOOT_ROOT_PARTITION_TYPE@|$MBOOT_ROOT_PARTITION_TYPE|g" \
		-e "s|@MBOOT_ROOT_FSTYPE@|$MBOOT_ROOT_FSTYPE|g" \
		-e "s|@MBOOT_HOSTNAME@|$MBOOT_HOSTNAME|g" \
		-e "s|@MBOOT_KERNEL_CMDLINE@|$MBOOT_KERNEL_CMDLINE|g" \
		"$input" > "$temporary"
	if grep -Eq '@MBOOT_[A-Z0-9_]+@' "$temporary"; then
		rm -f "$temporary"
		fail "unresolved setting in $input"
	fi
	if test -f "$output" && cmp -s "$temporary" "$output"; then
		rm -f "$temporary"
	else
		mv "$temporary" "$output"
	fi
}

render "$ROOT/configs/mboot_x86_64_defconfig.in" "$OUTPUT/mboot_x86_64_defconfig"
render "$ROOT/board/mboot/linux.config.in" "$OUTPUT/linux.config"
render "$ROOT/board/mboot/grub-bios.cfg.in" "$OUTPUT/grub-bios.cfg"
render "$ROOT/board/mboot/grub-builtin.cfg.in" "$OUTPUT/grub-builtin.cfg"
render "$ROOT/board/mboot/genimage.cfg.in" "$OUTPUT/genimage.cfg"

cat > "$OUTPUT/boot-layout.env.new" <<EOF
MBOOT_DISK_GUID=$MBOOT_DISK_GUID
MBOOT_ROOT_PARTUUID=$MBOOT_ROOT_PARTUUID
MBOOT_BIOS_PARTUUID=$MBOOT_BIOS_PARTUUID
MBOOT_EFI_PARTUUID=$MBOOT_EFI_PARTUUID
MBOOT_ROOT_FSUUID=$MBOOT_ROOT_FSUUID
MBOOT_EFI_VOLUME_ID=$MBOOT_EFI_VOLUME_ID
MBOOT_ROOT_PARTITION_TYPE=$MBOOT_ROOT_PARTITION_TYPE
MBOOT_ROOT_FSTYPE=$MBOOT_ROOT_FSTYPE
MBOOT_ROOTWAIT_SECONDS=$MBOOT_ROOTWAIT_SECONDS
MBOOT_HOSTNAME=$MBOOT_HOSTNAME
MBOOT_KERNEL_CMDLINE='$MBOOT_KERNEL_CMDLINE'
EOF
if test -f "$OUTPUT/boot-layout.env" && cmp -s "$OUTPUT/boot-layout.env.new" "$OUTPUT/boot-layout.env"; then
	rm -f "$OUTPUT/boot-layout.env.new"
else
	mv "$OUTPUT/boot-layout.env.new" "$OUTPUT/boot-layout.env"
fi
