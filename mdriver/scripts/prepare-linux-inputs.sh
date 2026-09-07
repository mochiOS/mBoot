#!/usr/bin/env bash
set -euo pipefail

mdriver_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
state="$mdriver_dir/.cache/linux-inputs.sha256"
mkdir -p "$mdriver_dir/.cache"
candidate=$(mktemp "$mdriver_dir/.cache/linux-inputs.XXXXXX")
trap 'rm -f -- "$candidate"' EXIT

while IFS= read -r -d '' input; do
    sha256sum "$input"
done < <(
    find \
        "$mdriver_dir/board/mdriver/patches/linux" \
        "$mdriver_dir/board/mdriver/linux.config" \
        -type f -print0 | sort -z
) >"$candidate"

if cmp -s "$candidate" "$state"; then
    exit 0
fi

if [[ -f "$state" && -d "$mdriver_dir/output/build" ]]; then
    # A single appended patch can use Kbuild's dependency tracking. Changes
    # to old patches still require re-extraction. Buildroot's Kconfig rules
    # regenerate .config when the custom configuration changes.
    declare -A previous=()
    while read -r digest input; do
        previous["$input"]=$digest
    done <"$state"
    appended=
    incremental=true
    config_changed=false
    last_patch=
    for input in "${!previous[@]}"; do
        if [[ "$input" == *.patch && "$input" > "$last_patch" ]]; then
            last_patch=$input
        fi
    done
    while read -r digest input; do
        if [[ -v previous["$input"] ]]; then
            if [[ "${previous[$input]}" != "$digest" ]]; then
                if [[ "$input" == "$mdriver_dir/board/mdriver/linux.config" ]]; then
                    config_changed=true
                else
                    incremental=false
                fi
            fi
            unset 'previous[$input]'
        elif [[ -z "$appended" && "$input" == *.patch && "$input" > "$last_patch" ]]; then
            appended=$input
        else
            incremental=false
        fi
    done <"$candidate"
    linux_dirs=("$mdriver_dir"/output/build/linux-[0-9]*/.stamp_patched)
    if $incremental && [[ ${#previous[@]} == 0 &&
                          ${#linux_dirs[@]} == 1 && -f "${linux_dirs[0]}" ]]; then
        linux_dir=${linux_dirs[0]%/.stamp_patched}
        if [[ -n "$appended" ]]; then
            patch --batch --forward --fuzz=0 --dry-run -d "$linux_dir" -p1 -i "$appended"
            patch --batch --forward --fuzz=0 -d "$linux_dir" -p1 -i "$appended"
            printf 'mDriver: incrementally applied %s\n' "${appended##*/}"
        fi
        if $config_changed; then
            rm -f -- "$linux_dir/.stamp_dotconfig" "$linux_dir/.stamp_kconfig_fixup_done" \
                "$linux_dir/.stamp_configured"
            printf 'mDriver: regenerating Linux configuration with cached objects\n'
        fi
        rm -f -- "$linux_dir/.stamp_built" "$linux_dir/.stamp_target_installed" \
            "$linux_dir/.stamp_staging_installed" "$linux_dir/.stamp_images_installed"
    else
        "$mdriver_dir/scripts/buildroot-make.sh" linux-dirclean
    fi
fi

mv -f -- "$candidate" "$state"
