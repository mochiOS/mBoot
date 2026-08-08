#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
MODE=${1:-}
INPUT=${2:-}
OUTPUT=${3:-}
ENV_FILE=$ROOT/output/generated/boot-layout.env

fail() { echo "update-config-template: $*" >&2; exit 1; }
test -f "$INPUT" || fail "missing input: $INPUT"
test -f "$ENV_FILE" || fail 'run make prepare-boot-config first'
. "$ENV_FILE"

temporary=$OUTPUT.new
case "$MODE" in
	defconfig)
		sed \
			-e 's|^BR2_TARGET_GENERIC_HOSTNAME=.*$|BR2_TARGET_GENERIC_HOSTNAME="@MBOOT_HOSTNAME@"|' \
			-e "s|$MBOOT_ROOT_FSUUID|@MBOOT_ROOT_FSUUID@|g" \
			"$INPUT" > "$temporary"
		grep -Fq '@MBOOT_HOSTNAME@' "$temporary" || fail 'hostname setting was not saved'
		grep -Fq '@MBOOT_ROOT_FSUUID@' "$temporary" || fail 'root filesystem UUID was not saved'
		;;
	linux)
		sed \
			-e 's|^CONFIG_CMDLINE=.*$|CONFIG_CMDLINE="@MBOOT_KERNEL_CMDLINE@"|' \
			"$INPUT" > "$temporary"
		grep -Fq '@MBOOT_KERNEL_CMDLINE@' "$temporary" || fail 'kernel command line was not saved'
		;;
	*) fail 'mode must be defconfig or linux' ;;
esac
mv "$temporary" "$OUTPUT"
