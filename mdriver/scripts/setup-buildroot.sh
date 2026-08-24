#!/usr/bin/env bash
set -euo pipefail

mdriver_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
version=2025.02.16
archive="buildroot-$version.tar.xz"
expected_sha256=15305e3d366eeaf4a5ecaf2ed42f685fd6af7fe5dbf1f62e1de5f46ee83225e2
url="https://buildroot.org/downloads/$archive"
cache_dir="$mdriver_dir/.cache"
cached_archive="$cache_dir/$archive"
buildroot_dir="$mdriver_dir/buildroot"

mkdir -p "$cache_dir"

verify_archive() {
    printf '%s  %s\n' "$expected_sha256" "$1" | sha256sum --check --status
}

if [[ ! -f "$cached_archive" ]] || ! verify_archive "$cached_archive"; then
    temporary_archive="$cached_archive.new"
    rm -f "$temporary_archive"
    curl --fail --location --retry 4 --retry-all-errors \
        --output "$temporary_archive" "$url"
    if ! verify_archive "$temporary_archive"; then
        rm -f "$temporary_archive"
        printf 'mDriver setup: Buildroot checksum mismatch\n' >&2
        exit 1
    fi
    mv "$temporary_archive" "$cached_archive"
fi

marker="$buildroot_dir/.mochios-buildroot-version"
if [[ ! -f "$marker" ]] || [[ $(<"$marker") != "$version $expected_sha256" ]]; then
    temporary_dir="$mdriver_dir/.buildroot.new"
    rm -rf "$temporary_dir"
    mkdir -p "$temporary_dir"
    tar -xJf "$cached_archive" --strip-components=1 -C "$temporary_dir"
    printf '%s %s\n' "$version" "$expected_sha256" >"$temporary_dir/.mochios-buildroot-version"
    rm -rf "$buildroot_dir"
    mv "$temporary_dir" "$buildroot_dir"
fi
