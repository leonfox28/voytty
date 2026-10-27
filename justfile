set shell := ["sh", "-eu", "-c"]

default:
    @just --list

# Compile the public shared client for mobile; app/runtime checks live in voytty-app.
mobile-core-check platform:
    sh tools/ci/check-mobile-core.sh {{ quote(platform) }}

android-check:
    just mobile-core-check android

# Report required local tools and the hosted-only evidence boundary.
doctor:
    sh tools/ci/doctor.sh

# Check the complete branch diff and local edits before an ordinary push.
check-changes base='origin/main':
    python3 tools/ci/workflow-policy.py local {{ quote(base) }}

# Explicit full hosted validation and unsigned candidates; never publishes.
full-check:
    python3 tools/ci/full-check.py

# Fast edit-loop checks: source and portable policy plus native Clippy.
check-fast:
    sh tests/source-policy.sh
    just ci-policy
    cargo +1.98.0 clippy --workspace --all-targets --all-features -- -D warnings

# Optional full current-host checkpoint; ordinary pushes use check-changes.
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
    PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s tests/ci -p '*_test.py'
    sh tests/ci/trim-cache-fixture.sh
    actionlint
    sh tests/release/static.sh
    just installer-check
    sh tests/release/operator-fixture.sh
    sh tests/release/candidate-fixture.sh
    PYTHONDONTWRITEBYTECODE=1 python3 tests/release/recover_draft_test.py
    shellcheck -s sh install/install.sh tests/release/*.sh tests/ci/*.sh tests/secret-scan*.sh tests/terminal-dependency-policy.sh $(find tools/ci tools/release -type f -name '*.sh' -print)
    sh tools/ci/check-python-syntax.sh tests/ci/*.py tools/ci/*.py tests/release/https_fixture.py tests/release/https_fixture_bind_test.py tests/release/installer_test.py tests/release/installer_download_test.py tools/release/recover-draft.py tests/release/recover_draft_test.py

# Bootstrap, platform selection, download bounds, and install-path regressions.
installer-check:
    PYTHONDONTWRITEBYTECODE=1 python3 tests/release/installer_test.py
    PYTHONDONTWRITEBYTECODE=1 python3 tests/release/installer_download_test.py

# Unix workspace evidence; patched transport suites run when their inputs change.
ci-unix docs='false' transport='true':
    case {{ quote(docs) }} in true|false) ;; *) echo 'docs must be true or false' >&2; exit 64;; esac
    cargo +1.98.0 clippy --workspace --all-targets --all-features -- -D warnings
    cargo +1.98.0 test --workspace --all-features
    "${CARGO_TARGET_DIR:-target}/debug/voytty"
    case {{ quote(transport) }} in true) just ci-noq; just ci-iroh ;; false) ;; *) exit 64 ;; esac
    if [ {{ quote(docs) }} = true ]; then cargo +1.98.0 doc --workspace --no-deps; fi

# Validate the canonical project license metadata. Mobile bundles live in voytty-app.
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
