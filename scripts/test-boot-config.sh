#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
temporary=$(mktemp -d /tmp/mboot-boot-config-test.XXXXXX)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM
DEVELOPMENT=${MBOOT_DEVELOPMENT:-0}

case "$DEVELOPMENT" in
	0) expected_hostname=mboot ;;
	1) expected_hostname=mboot-dev ;;
	*) echo 'test-boot-config: MBOOT_DEVELOPMENT must be 0 or 1' >&2; exit 1 ;;
esac

env MBOOT_DEVELOPMENT="$DEVELOPMENT" USER=jnix LOGNAME=jnix HOME=/home/jnix HOSTNAME=mochimochi \
	"$ROOT/scripts/generate-boot-config.sh" "$temporary/first"
env MBOOT_DEVELOPMENT="$DEVELOPMENT" USER=builder LOGNAME=builder HOME=/users/builder HOSTNAME=build-host \
	"$ROOT/scripts/generate-boot-config.sh" "$temporary/second"

diff -ru "$temporary/first" "$temporary/second"
if grep -R -E 'jnix|mochimochi|builder|build-host|/home/|/users/' \
	"$temporary/first" "$temporary/second"; then
	echo 'test-boot-config: host identity leaked into generated configuration' >&2
	exit 1
fi
grep -Fq 'rootwait=30' "$temporary/first/linux.config"
grep -Fq "BR2_TARGET_GENERIC_HOSTNAME=\"$expected_hostname\"" \
	"$temporary/first/mboot_x86_64_defconfig"

if [ "$DEVELOPMENT" = 0 ]; then
	"$ROOT/scripts/update-config-template.sh" defconfig \
		"$temporary/first/mboot_x86_64_defconfig" "$temporary/defconfig.in"
	cmp -s "$ROOT/configs/mboot_x86_64_defconfig.in" "$temporary/defconfig.in"
fi
"$ROOT/scripts/update-config-template.sh" linux \
	"$temporary/first/linux.config" "$temporary/linux.config.in"
cmp -s "$ROOT/board/mboot/linux.config.in" "$temporary/linux.config.in"

echo 'test-boot-config: PASS'
