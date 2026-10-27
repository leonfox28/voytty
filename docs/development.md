# Development environment

## Exact project baseline

The project toolchain is fixed by `rust-toolchain.toml`; it does not follow the
floating `stable` channel. The initial Apple Silicon development machine was
verified on 2026-08-21 with:

| Component | Verified value |
| --- | --- |
| Host | macOS 26.6.2, arm64, 10 CPUs, 32 GiB RAM |
| Rustup | 1.29.0 |
| Rust / Cargo | 1.98.0 |
| Rust components | rustfmt, Clippy, rust-src |
| Rust targets | `aarch64-apple-darwin`, `aarch64-apple-ios`, `aarch64-linux-android` |
| Preserved user toolchain | `1.95.0-aarch64-apple-darwin` |
| Docker CLI | 29.7.2 |
| Docker Engine in Colima | 29.5.2 |
| Docker Compose | 5.5.0 |
| Docker Buildx | 0.36.1 (BuildKit 0.30.0 on the active Colima builder) |
| Colima | 0.10.3 |
| pkg-config / pkgconf | 3.0.5 |
| jq | 1.7.1 |
| cargo-deny | 0.20.2 |
| just | 1.42.4 in CI; compatible local release |
| actionlint | 1.7.12 |

Rust 1.95.0 is user-owned and must not be removed. A future Rust or cargo-deny
upgrade is an explicit repository change and must rerun every quality gate.

## Product version

The root `[workspace.package].version` is the single lockstep product version,
and all public voytty product crates inherit it with `version.workspace = true`;
do not assign a component-specific version to a crate. A release preparation
changes this one value, refreshes `Cargo.lock`, and advances the CLI, daemon,
protocol, platform libraries, and Relay wrapper as one product.

`tests/relay/handshake-probe` is deliberately outside the root workspace. It is
an internal acceptance tool rather than a shipped voytty component, so its
`0.0.0` package version is not part of the lockstep release contract.

## Install missing tools on macOS

Always inspect installed versions before installing. Phase Zero used Homebrew's
Docker CLI and Compose with Colima; Docker Desktop and administrator-granted
startup services are not required.

```bash
brew install docker docker-compose docker-buildx colima pkg-config jq
cargo install cargo-deny --version 0.20.2 --locked
brew install just shellcheck actionlint gh
```

Homebrew's Compose plugin directory must be visible to the Docker CLI. Preserve
any existing Docker configuration while ensuring `~/.docker/config.json`
contains the following additional plugin path:

```json
{
  "cliPluginsExtraDirs": [
    "/opt/homebrew/lib/docker/cli-plugins"
  ]
}
```

The initial Colima profile is deliberately manual and does not start at login:

```bash
colima start --cpu 4 --memory 6 --disk 40 --vm-type vz --runtime docker
docker version
docker compose version
docker run --rm hello-world
```

Use `colima stop` when the local VM is not needed. `colima start` reuses the
same profile and data on the next development session.

## Rust and protobuf

Opening the repository causes rustup to select exact Rust 1.98.0 and ensure the
declared components are installed. Platform targets can be restored without
changing the default toolchain:

```bash
rustup target add \
  aarch64-apple-darwin \
  aarch64-apple-ios \
  aarch64-linux-android \
  --toolchain 1.98.0-aarch64-apple-darwin
```

Do not install a global `protoc` for this project. `voytty-proto` uses the exact
`protoc-bin-vendored` dependency from `Cargo.lock`, then passes that executable
directly to `prost-build`.

## Workspace dependency direction

```text
                         voytty-core
                    /        |        \
             voytty-proto  voytty-platform  voytty-terminal
                    \        |        /
                         voytty-daemon
                              |
                          voytty-cli
```

Arrows run from a consumer to its dependency. Core owns domain and
transport-neutral terminal values; proto owns the wire codec; platform owns OS
and PTY boundaries; the host-only terminal crate owns the pinned Alacritty
parser/grid/state adapter. Daemon combines those owners and retains the live
Session registry; CLI owns command parsing and the outer raw-terminal renderer.
Core/proto never depend on the host terminal engine, and CLI has no direct
engine dependency. The CLI reaches local and remote Sessions only through the
daemon; do not move transport state into core, give the CLI an Endpoint or
identity key, or move OS/Session ownership into adapters. Hosted Linux
remote-runtime evidence remains a separate acceptance gate; see
[Remote sessions and the public CLI](remote-cli.md).

## Local command owner

```bash
just doctor
just check-fast
just check
```

`just check` is the authoritative pre-push gate for product, test, build, dependency,
and workflow changes on the current host. For changes limited to `README.md`,
`AGENTS.md`, and Markdown under `docs/` (except `docs/relay.md`, which relay tests
consume), `just ci-policy` is sufficient locally. PR dependency policy still runs.
Ordinary commits can use targeted checks; `check-fast` is an optional checkpoint,
not a mandatory full-workspace check before every commit.

The full local gate composes the hosted profiles rather than repeating their commands:
`ci-policy`, `ci-unix` with docs, `ci-dependencies`, and the Docker-free
`relay-static` part of `ci-relay`, so a change to a hosted profile reaches the
local gate too. `just check-fast` is the shorter edit loop: source policy,
`ci-policy`, and native Clippy. Neither command claims evidence from a different OS/CPU, the pinned
glibc builder, Docker/QEMU execution, a protected signing secret, final signed
installers, GitHub attestation, or immutable publication; GitHub Actions owns
those boundaries and the recipes print that distinction.

The UniFFI bridge and its parser-backport tests now belong to `voytty-app`.

The terminal-identity integration test uses the real account's login shell.
It waits for Bash/Zsh's bracketed-paste mode, or the default Bash `$`/`#` prompt
at the live cursor for older Bash, before sending its first command. Custom Bash
prompts without those suffixes need bracketed paste enabled for this fixture.
This test precondition does not restrict product shell support.

Run `just android-check` and `just mobile-core-check ios` when shared
`client/core/proto/diagnostics`, Iroh/noq, or Cargo dependencies change. These
recipes compile the public client for mobile targets without a private checkout.
They do not replace application runtime tests. For shared changes, also update
the pin in `voytty-app` and run its `just android-check`, `just ios-device-build`
and `just ios-check <simulator-uuid>` before accepting the integration. Report
unavailable platform evidence accurately. See [the split boundary](mobile-repository.md).

CI uses the same repository profiles:

| Hosted owner | Reproduction entry |
| --- | --- |
| Policy and portable checks | `just ci-policy` |
| Three Unix hosts (full CI) | `just ci-unix <docs>` on the matching host |
| Dependency policy | `just ci-dependencies` |
| Shared Android/iOS client compilation | `just mobile-core-check android`, `just mobile-core-check ios` |
| Docker/QEMU relay bundle | `just ci-relay` on Docker-capable Linux |
| Three native candidates (full CI; uploaded only by main) | `tools/release/build-native.sh <output-directory>` with explicit source identity on the native/pinned builder |

Workspace version, every format check (workspace, patched Iroh, relay probe)
and the secret scan each run once in `ci-policy`, docs run on Linux x64, and the
no-argument CLI smoke runs against each of the three native release candidates
instead of a separate debug rebuild. Source checkout policy
runs before compilation in every supported OS matrix entry. macOS Intel and
Windows CI are paused until a future task explicitly restores them.

## Cache ownership

Command caches are keyed by OS/architecture, profile and pinned tool versions,
independently of the product lockfile. Only main writes these small caches.
Version checks follow the installation owner: Go-installed actionlint reports
`v1.7.12`, while Homebrew's build reports `1.7.12`.
Cargo downloads use lockfile keys with an OS/architecture/toolchain fallback.
The same download namespace is usable by main candidates and tag publication;
no compiled PR output becomes a publication input.

PRs only restore compiled dependency caches. Main strips path-package
(workspace and patched vendor) and test outputs before saving
`target/debug/{deps,build,.fingerprint}` and skips entries larger than
2.5 GiB. Path packages are fingerprinted by source mtime, so a fresh checkout
rebuilds them regardless. The Rust matrix builds without dev debuginfo
(`CARGO_PROFILE_DEV_DEBUG=0`): with debuginfo the trimmed sets were 3.1–5.2 GiB
and were never saved; without it they measured 1.5–2.0 GiB on 2026-10-10, Linux
x64 being largest because it also builds docs (and then still built the smoke binary). An
oversized set raises a workflow warning and is not saved; restores keep using
the last saved entry until the key version is bumped. Incremental compilation is disabled in CI. The whole `target` tree,
large test fixtures and product release outputs are not cached. Measure restore/save
and test durations together before changing this policy.

Android app/Gradle and iOS application jobs, caches and test recipes moved to
`voytty-app`; their historical measurements remain in ci-release-optimization.md.

The independent `target/noq-tests` and `target/iroh-tests` roots also cache only
third-party `debug/{deps,build,.fingerprint}` outputs, keyed by their own manifest
and lockfile, toolchain, OS and architecture. Main trims using each vendor
manifest, so the patched path packages and their tests always rebuild.

Native candidate jobs cache third-party `target/release` dependencies for PRs.
Main uses `lookup-only`, always builds cold, then strips every path package before
saving a missing key. Native keys separate target and builder floor and include
the lockfile and build scripts. Restored PR candidates still run the complete
floor checks, release self-check and no-argument CLI check. No source-identified
product binary is included in these caches.

## Branch and pull-request flow

Normal substantive work uses a branch and pull request:

```bash
git switch -c <topic>
just check
git push --set-upstream origin <topic>
gh pr create
```

CI runs for the pull request, not a duplicate branch-push workflow. Updating
the branch cancels the older PR-head run. Main runs are never cancelled: each
merge commit keeps its own exact-SHA run and candidate even when another merge
follows quickly. Merge only after the stable `CI gate`
is green. A lightweight job compares the complete tested PR merge tree with
its exact base using a NUL-delimited Git diff, including deletions and both rename
paths. Only the documentation allowlist above can skip Rust, shared mobile compilation, relay and
native jobs. Missing history, unknown paths or classification errors select full
checks. Policy and dependency jobs always run. `CI gate` always runs and verifies
all job results against the explicit classification; failed, cancelled, missing
or accidentally skipped required jobs cannot pass.

All other PRs build and check the three native candidates without uploading
them; the `CI gate` requires their success. The resulting `main`
push runs the full integration graph, uploads all three exact-SHA native
candidates, and runs one unsigned desktop assembly. Tag publication
reuses that candidate instead of compiling the product again.

The deterministic two-file release-version commit is the narrow exception to
the local `just check` step: `just release-prepare VERSION` runs focused
version/lock validation, in the current feature PR or a standalone release PR;
its PR CI owns the complete gate. See
[Release operations](releasing.md) for that path.

Repository administrators own the following `main` protection settings.
The release operator checks them and does not mutate them:

1. require changes through a pull request, with zero mandatory outside
   approvals for the solo-maintainer repository;
2. require the exact status check `CI gate` before merge;
3. apply the rules to administrators/no bypass so direct pushes are not the
   normal path;
4. disable force pushes and branch deletion.

Apply protection only after `CI gate` exists on the default branch. After
20–30 PR/main runs, review critical path, cache hit rate, flaky failures and
unique platform catches before considering nextest, path classification, or
compile-only secondary targets. Weekly `dependencies.yml` also runs both
dependency-policy owners to detect new advisories while the project is idle.
This is early detection; PR/main dependency failures remain blocking.

For a failed job, use the matching recipe printed in its step summary. Runner
queue delay is separate from job execution time; job timeouts prevent hangs
but do not eliminate GitHub-hosted queueing.

`cargo run --quiet --package voytty-cli -- --help` and `--version` are
side-effect free. Never use real `~/.voytty` for mutation tests; inject
task-private `UserPaths` through the library harnesses. Local readiness must
remain independent of Iroh, DNS, Relay, and Internet access.
