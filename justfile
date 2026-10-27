set shell := ["sh", "-eu", "-c"]

default:
    @just --list

# Generate Swift bindings and the Xcode project; Xcode owns per-SDK Rust linking.
ios-project:
    python3 tools/ios/build.py prepare

# Ad-hoc signed Apple Silicon simulator app, including Keychain entitlements.
ios-build:
    python3 tools/ios/build.py build

# Cross-build the device app without accessing a signing account.
ios-device-build:
    python3 tools/ios/build.py device-build

# Native bridge, persistence, terminal input and app launch checks on a simulator.
ios-check simulator:
    python3 tools/ios/build.py check --simulator {{ quote(simulator) }}

# Opt-in native UI/physical-key acceptance against an already-paired disposable host.
ios-ui-check simulator fixture:
    python3 tools/ios/build.py ui-check --simulator {{ quote(simulator) }} --ui-fixture {{ quote(fixture) }}

# Target the system notification/document-picker flow during service edits.
ios-ui-services-check simulator fixture:
    python3 tools/ios/build.py ui-check --simulator {{ quote(simulator) }} --ui-fixture {{ quote(fixture) }} --ui-scope services

ios-ui-files-check simulator fixture:
    python3 tools/ios/build.py ui-check --simulator {{ quote(simulator) }} --ui-fixture {{ quote(fixture) }} --ui-scope files

ios-ui-photos-check simulator fixture:
    python3 tools/ios/build.py ui-check --simulator {{ quote(simulator) }} --ui-fixture {{ quote(fixture) }} --ui-scope photos

ios-ui-recovery-check simulator fixture:
    python3 tools/ios/build.py ui-check --simulator {{ quote(simulator) }} --ui-fixture {{ quote(fixture) }} --ui-scope recovery

ios-ui-reconnect-check simulator fixture:
    python3 tools/ios/build.py ui-check --simulator {{ quote(simulator) }} --ui-fixture {{ quote(fixture) }} --ui-scope reconnect

# Native failure alert and explicit retry against the disposable host.
ios-ui-connection-check simulator fixture:
    python3 tools/ios/build.py ui-check --simulator {{ quote(simulator) }} --ui-fixture {{ quote(fixture) }} --ui-scope connection

# System Back, cancelled/completed edge swipes and remote Session survival.
ios-ui-navigation-check simulator fixture:
    python3 tools/ios/build.py ui-check --simulator {{ quote(simulator) }} --ui-fixture {{ quote(fixture) }} --ui-scope navigation

# Centered system deletion alert, explicit cancellation and selected-Session deletion.
ios-ui-deletion-check simulator fixture:
    python3 tools/ios/build.py ui-check --simulator {{ quote(simulator) }} --ui-fixture {{ quote(fixture) }} --ui-scope deletion

# Native Session sheet gestures, editor transitions, keyboard restoration and switching.
ios-ui-sessions-check simulator fixture:
    python3 tools/ios/build.py ui-check --simulator {{ quote(simulator) }} --ui-fixture {{ quote(fixture) }} --ui-scope sessions

# System settings Form and cancellation of the named host-removal alert.
ios-ui-settings-check simulator fixture:
    python3 tools/ios/build.py ui-check --simulator {{ quote(simulator) }} --ui-fixture {{ quote(fixture) }} --ui-scope settings

# Generate Kotlin/native outputs and assemble the local Android APK.
android-build:
    sh tools/android/build.sh :app:assembleDebug

# Android static/unit checks (device tests use the explicitly selected emulator).
android-check:
    sh tools/android/build.sh :app:lintDebug :app:testDebugUnitTest

# Stable local signing identity stays outside the checkout.
android-apk version_code:
    python3 tools/android/apk.py {{ quote(version_code) }}

# Install the debug build using adb's explicit serial selection.
android-install serial:
    sh tools/android/build.sh :app:assembleDebug
    adb -s {{ quote(serial) }} install -r apps/android/app/build/outputs/apk/debug/app-debug.apk

# Report required local tools and the hosted-only evidence boundary.
doctor:
    sh tools/ci/doctor.sh

# Fast edit-loop checks: source and portable policy plus native Clippy.
check-fast:
    sh tests/source-policy.sh
    just ci-policy
    cargo +1.98.0 clippy --workspace --all-targets --all-features -- -D warnings

# Authoritative pre-push gate: every hosted profile that runs on the current host.
check:
    sh tests/source-policy.sh
    just ci-policy
    just ci-unix true
    just ci-dependencies
    just relay-static
    if command -v docker >/dev/null 2>&1 && docker compose version >/dev/null 2>&1; then sh tests/relay/static.sh; else echo 'HOSTED-ONLY: relay Compose metadata requires Docker Compose'; fi
    @echo 'HOSTED-ONLY: other supported macOS/Linux hosts, glibc 2.28, Docker/QEMU relay tests, protected signing, final installers, attestation, and immutable publication run in GitHub Actions.'

# Canonical portable CI owner: version, every format check, workflow/release policy, secret scan, shell, and Python syntax.
ci-policy:
    just licensing-check
    sh tests/workspace-version.sh
    sh tests/terminal-dependency-policy.sh
    cargo +1.98.0 fmt --all -- --check
    cargo +1.98.0 fmt --manifest-path vendor/iroh/Cargo.toml -- --check
    cargo +1.98.0 fmt --manifest-path tests/relay/handshake-probe/Cargo.toml -- --check
    sh tests/secret-scan.sh
    sh tests/secret-scan-fixture.sh
    PYTHONDONTWRITEBYTECODE=1 python3 tests/ci/workflow_policy_test.py
    PYTHONDONTWRITEBYTECODE=1 python3 tests/e2e/multi_device_audit_test.py
    sh tests/ci/trim-cache-fixture.sh
    actionlint
    sh tests/release/static.sh
    sh tests/release/operator-fixture.sh
    sh tests/release/candidate-fixture.sh
    PYTHONDONTWRITEBYTECODE=1 python3 tests/release/android_test.py
    PYTHONDONTWRITEBYTECODE=1 python3 tests/release/recover_draft_test.py
    shellcheck -s sh install/install.sh tests/release/*.sh tests/ci/*.sh tests/secret-scan*.sh tests/terminal-dependency-policy.sh $(find tools/ci tools/release -type f -name '*.sh' -print)
    sh tools/ci/check-python-syntax.sh tests/e2e/*.py tests/ci/*.py tools/ci/*.py tests/release/https_fixture.py tests/release/https_fixture_bind_test.py tools/android/*.py tools/ios/*.py tools/release/recover-draft.py tests/release/android_test.py tests/release/recover_draft_test.py

# Full Unix runtime evidence; CI assigns docs to its canonical host. The
# no-argument CLI smoke runs against each native release candidate instead.
ci-unix docs='false':
    case {{ quote(docs) }} in true|false) ;; *) echo 'docs must be true or false' >&2; exit 64;; esac
    cargo +1.98.0 clippy --workspace --all-targets --all-features -- -D warnings
    cargo +1.98.0 test --workspace --all-features
    just ci-noq
    just ci-iroh
    just ci-askama
    if [ {{ quote(docs) }} = true ]; then cargo +1.98.0 doc --workspace --no-deps; fi

# Refresh the shared Android/iOS copies of the canonical project license texts.
licensing-sync:
    python3 tools/licensing.py sync

# Check project metadata, the original PolyForm text, and bundled license copies.
licensing-check:
    python3 tools/licensing.py check

# Keep the patched transport's upstream suite and offline-path regression in CI.
ci-noq:
    cargo +1.98.0 test --locked --manifest-path vendor/noq-proto/Cargo.toml --target-dir target/noq-tests --lib

# Endpoint lifecycle and socket-free cold-relay queue expiry regressions.
ci-iroh:
    cargo +1.98.0 test --locked --manifest-path vendor/iroh/Cargo.toml --target-dir target/iroh-tests --no-default-features --features tls-ring,unstable-custom-transports --lib voytty_

# Upstream template/config regressions for the UniFFI parser backport.
ci-askama:
    cargo +1.98.0 test --locked --manifest-path vendor/askama_derive/Cargo.toml --target-dir target/askama-tests --lib

# Workspace and isolated relay-probe dependency policy.
ci-dependencies:
    cargo +1.98.0 deny check
    cargo +1.98.0 deny --manifest-path tests/relay/handshake-probe/Cargo.toml --config tests/relay/handshake-probe/deny.toml check

# Docker-free relay contracts: probe Clippy, shell syntax, and pinned upstream artifacts.
relay-static:
    cargo +1.98.0 clippy --locked --manifest-path tests/relay/handshake-probe/Cargo.toml -- -D warnings
    sh -n deploy/relay/*.sh tests/relay/*.sh
    sh tests/relay/verify-upstream.sh

# Docker-capable Ubuntu owner for the complete optional relay bundle.
ci-relay:
    just relay-static
    sh tests/relay/static.sh
    sh tests/relay/build-platforms.sh
    sh tests/relay/smoke.sh

# Prepare the version in the current feature PR, or open a standalone release PR from main.
release-prepare version:
    sh tools/release/operator.sh prepare {{ quote(version) }}

# Tag exact protected green main and watch the formal release workflow.
release-publish version:
    sh tools/release/operator.sh publish {{ quote(version) }}

# Finish a reviewed release PR: wait, merge exact head, wait main, tag, and publish.
release version pr:
    sh tools/release/operator.sh finish {{ quote(version) }} {{ quote(pr) }}
