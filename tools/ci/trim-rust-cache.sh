#!/bin/sh
set -eu

# Keep third-party compilation outputs; test binaries, workspace outputs and
# incremental state cost more to transfer than to rebuild on this repository.
# Path packages (workspace members and patched vendor crates) are fingerprinted
# by source mtime, so a fresh checkout rebuilds them whether or not they are
# cached.
#
# Arguments name cached layouts under target/: a profile directory such as
# debug or release, or TRIPLE/release for a cross-compiled release.
[ "$#" -gt 0 ] || set -- debug
packages=$(cargo +1.98.0 metadata --locked --format-version 1 \
    | jq -r '.packages[] | select(.source == null) | "\(.name)@\(.version)"')
maximum_kib=2621440
size=0
for layout in "$@"; do
    case "$layout" in
        debug) clean_flags= ;;
        release) clean_flags=--release ;;
        */release) clean_flags="--target ${layout%/release} --release" ;;
        *) echo "unsupported cache layout: $layout" >&2; exit 64 ;;
    esac
    for package in $packages; do
        # Word splitting yields the fixed flags chosen above.
        # shellcheck disable=SC2086
        cargo +1.98.0 clean --package "$package" $clean_flags
    done
    if [ -d "target/$layout/deps" ]; then
        find "target/$layout/deps" -type f -perm -u+x \
            ! -name '*.so' ! -name '*.dylib' -delete
    fi
    for directory in "target/$layout/deps" "target/$layout/build" "target/$layout/.fingerprint"; do
        if [ -d "$directory" ]; then
            size=$((size + $(du -sk "$directory" | awk '{print $1}')))
        fi
    done
done
echo "Compiled dependency cache: $size KiB (maximum $maximum_kib KiB)"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
    if [ "$size" -le "$maximum_kib" ]; then
        echo 'cacheable=true' >>"$GITHUB_OUTPUT"
    else
        # Restores keep using the last saved entry; bump the key version to reset it.
        echo "::warning::Compiled dependency cache is $size KiB, above $maximum_kib KiB; not saved"
        echo 'cacheable=false' >>"$GITHUB_OUTPUT"
    fi
fi
