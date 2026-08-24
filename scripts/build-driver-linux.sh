#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
OUTPUT_DIR=${OUTPUT_DIR:-$ROOT/output}
ARTIFACT_DIR=$OUTPUT_DIR/driver-linux
KERNEL_DIR=

for candidate in "$OUTPUT_DIR"/build/linux-*; do
    if [ -f "$candidate/Makefile" ]; then
        KERNEL_DIR=$candidate
        break
    fi
done

if [ -z "$KERNEL_DIR" ]; then
    echo "Driver Linux kernel source was not prepared; run make configure first" >&2
    exit 1
fi

mkdir -p "$ARTIFACT_DIR/root"
${CC:-gcc} -static -nostdlib -fno-stack-protector -fno-pie -no-pie \
    -Wl,--build-id=none -Os \
    "$ROOT/driver-linux/init.c" -o "$ARTIFACT_DIR/root/init"

rm -f "$ARTIFACT_DIR/initramfs.cpio"
(
    cd "$ARTIFACT_DIR/root"
    printf '%s\n' init | cpio --quiet -o -H newc > "$ARTIFACT_DIR/initramfs.cpio"
)

cp "$KERNEL_DIR/vmlinux" "$ARTIFACT_DIR/vmlinux.new"
mv "$ARTIFACT_DIR/vmlinux.new" "$ARTIFACT_DIR/vmlinux"
echo "[done] Driver Linux artifacts: $ARTIFACT_DIR"
