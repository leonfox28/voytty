#!/bin/sh
set -eu

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT HUP INT TERM
cat >"$fixture/Cargo.toml" <<'TOML'
[package]
name = "cache-fixture"
version = "0.0.0"
edition = "2024"
[lib]
path = "lib.rs"
TOML
printf 'pub fn fixture() {}\n' >"$fixture/lib.rs"

# Exercise real Cargo cleanup in a separate target root. A wrong manifest or
# target directory must not silently retain path-package compilation outputs.
for profile in debug release; do
    if [ "$profile" = release ]; then flags=--release; else flags=; fi
    # shellcheck disable=SC2086
    cargo +1.98.0 build --quiet --manifest-path "$fixture/Cargo.toml" \
        --target-dir "$fixture/compiled" $flags
    deps="$fixture/compiled/$profile/deps"
    test -f "$fixture/compiled/$profile/libcache_fixture.rlib"
    touch "$deps/libthird_party.rlib" "$deps/libmacro.so" "$deps/libmacro.dylib" "$deps/test-executable"
    chmod 700 "$deps/libmacro.so" "$deps/libmacro.dylib" "$deps/test-executable"
done
# Hosted caches restore selected subdirectories, not this root marker.
rm "$fixture/compiled/CACHEDIR.TAG"
VOYTTY_CACHE_MANIFEST_PATH="$fixture/Cargo.toml" \
    VOYTTY_CACHE_TARGET_DIR="$fixture/compiled" GITHUB_OUTPUT="$fixture/output" \
    sh "$repo_root/tools/ci/trim-rust-cache.sh" debug release
grep -Fxq 'cacheable=true' "$fixture/output"
for profile in debug release; do
    deps="$fixture/compiled/$profile/deps"
    test ! -e "$fixture/compiled/$profile/libcache_fixture.rlib"
    if find "$fixture/compiled/$profile" \( -name '*cache_fixture*' -o -name '*cache-fixture*' \) | grep -q .; then
        echo 'path-package outputs survived cache trimming' >&2
        exit 1
    fi
    test -f "$deps/libthird_party.rlib"
    test -f "$deps/libmacro.so"
    test -f "$deps/libmacro.dylib"
    test ! -e "$deps/test-executable"
done
echo 'cache trimming preserves third-party libraries and removes path/test outputs'
