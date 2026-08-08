#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
temporary=$(mktemp -d /tmp/mboot-boot-config-test.XXXXXX)
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

env USER=jnix LOGNAME=jnix HOME=/home/jnix HOSTNAME=mochimochi \
	"$ROOT/scripts/generate-boot-config.sh" "$temporary/first"
env USER=builder LOGNAME=builder HOME=/users/builder HOSTNAME=build-host \
	"$ROOT/scripts/generate-boot-config.sh" "$temporary/second"

diff -ru "$temporary/first" "$temporary/second"
if grep -R -E 'jnix|mochimochi|builder|build-host|/home/|/users/' \
	"$temporary/first" "$temporary/second"; then
	echo 'test-boot-config: host identity leaked into generated configuration' >&2
	exit 1
fi
grep -Fq 'rootwait=30' "$temporary/first/linux.config"
grep -Fq 'BR2_TARGET_GENERIC_HOSTNAME="mboot"' \
	"$temporary/first/mboot_x86_64_defconfig"

"$ROOT/scripts/update-config-template.sh" defconfig \
	"$temporary/first/mboot_x86_64_defconfig" "$temporary/defconfig.in"
cmp -s "$ROOT/configs/mboot_x86_64_defconfig.in" "$temporary/defconfig.in"
"$ROOT/scripts/update-config-template.sh" linux \
	"$temporary/first/linux.config" "$temporary/linux.config.in"
cmp -s "$ROOT/board/mboot/linux.config.in" "$temporary/linux.config.in"

echo 'test-boot-config: PASS'
