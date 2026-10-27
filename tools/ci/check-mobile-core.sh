#!/bin/sh
set -eu

# Shared transport stays buildable without checking out the private apps.
case "${1:-}" in
    android)
        if [ -z "${ANDROID_HOME:-}" ] && [ "$(uname -s)" = Darwin ]; then
            ANDROID_HOME="$HOME/Library/Android/sdk"
            export ANDROID_HOME
        fi
        : "${ANDROID_HOME:?set ANDROID_HOME to the Android SDK}"
        ANDROID_NDK_HOME="$ANDROID_HOME/ndk/28.2.13676358"
        export ANDROID_NDK_HOME
        cargo +1.98.0 ndk --target arm64-v8a --platform 26 check --locked -p voytty-client
        ;;
    ios)
        cargo +1.98.0 check --locked -p voytty-client --target aarch64-apple-ios
        cargo +1.98.0 check --locked -p voytty-client --target aarch64-apple-ios-sim
        ;;
    *) echo 'usage: check-mobile-core.sh android|ios' >&2; exit 64 ;;
esac
