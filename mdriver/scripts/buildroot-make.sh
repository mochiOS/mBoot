#!/usr/bin/env bash
set -euo pipefail

mdriver_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
clean_path=
IFS=: read -r -a path_entries <<<"$PATH"
for path_entry in "${path_entries[@]}"; do
    [[ -n "$path_entry" ]] || continue
    [[ "$path_entry" != *[$' \t\n']* ]] || continue
    if [[ -n "$clean_path" ]]; then
        clean_path+=:
    fi
    clean_path+="$path_entry"
done
export PATH="$clean_path"

exec make -C "$mdriver_dir/buildroot" \
    O="$mdriver_dir/output" \
    BR2_EXTERNAL="$mdriver_dir" \
    BR2_DL_DIR="$mdriver_dir/dl" \
    "$@"
