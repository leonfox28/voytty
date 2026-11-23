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
| Required Rust targets today | `aarch64-apple-darwin`, `aarch64-apple-ios`, `aarch64-apple-ios-sim`, `aarch64-linux-android` |
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
  aarch64-apple-ios-sim \
  aarch64-linux-android \
  --toolchain 1.98.0-aarch64-apple-darwin
```

Do not install a global `protoc` for this project. `voytty-proto` uses the exact
`protoc-bin-vendored` dependency from `Cargo.lock`, then passes that executable
directly to `prost-build`.

## Workspace dependency direction

The main runtime dependencies are listed below. The release tool and tests have
their own build-time dependencies.

| Consumer | Local dependencies |
| --- | --- |
| `voytty-cli` | `voytty-diagnostics`, `voytty-core`, `voytty-client`, `voytty-daemon` |
| `voytty-daemon` | `voytty-diagnostics`, `voytty-core`, `voytty-client`, `voytty-platform`, `voytty-proto`, `voytty-terminal` |
| `voytty-client` | `voytty-diagnostics`, `voytty-proto`, `voytty-core` |
| `voytty-platform` | `voytty-diagnostics`, `voytty-core` |
| `voytty-terminal` | `voytty-core` |
| `voytty-proto` | `voytty-core` |
| `voytty-diagnostics` | `voytty-core` |

Core owns transport-neutral values. The daemon owns local host sessions and
combines the OS, protocol and Alacritty adapters. The CLI uses the shared client
and the daemon for attachments and renders them in the outer terminal. Hosted Linux
remote-runtime evidence remains a separate acceptance gate; see
[Remote-session implementation contracts](remote-cli.md).

## Local commands and asynchronous main CI

```bash
just doctor
just check-changes                 # complete local diff plus uncommitted edits
just check-changes <base-commit>   # explicit comparison base
just check                         # optional full current-host checkpoint
```

Start substantive work from current `origin/main` (either directly on `main` or
on a short-lived local branch fast-forwarded into `main`), run targeted checks
while editing, then use `just check-changes` before pushing. This command uses
the same classifier and recipes as CI. It always runs policy/dependency checks
and selects native workspace or Relay checks from the complete diff, including
uncommitted/untracked files. It does not require another full `just check` or
mobile build after those selected checks pass. Hosted CI owns other OS/CPU,
Android/iOS cross targets, Docker and the glibc/macOS packaging floors.

| Changed inputs | Hosted CI checks in addition to policy/dependencies |
| --- | --- |
| README, AGENTS, Markdown under docs or wiki (except executable `docs/relay.md`) | None |
| CLI, daemon, platform, terminal Rust | Three supported Unix Rust hosts and both Windows architectures |
| Shared client/core/proto/diagnostics or protocol schemas | Unix/Windows Rust plus Android/iOS shared-client compilation |
| Relay deployment/probe or `docs/relay.md` | Relay contracts and Docker runtime |
| Release tools, installers, native build inputs | Rust and five native packaging/floor checks |
| Cargo/toolchain, vendor, CI, unknown inputs or unavailable diff history | All owners |

Patched Iroh/noq suites run for broad dependency/vendor changes and explicit full
validation. They are not repeated for unrelated workspace edits. All assertions
and existing suites remain enabled whenever their owner is selected. Native jobs
also smoke-test the debug CLI with the state-free `--help` and `--version` flags;
the release-mode CLI smoke remains part of native packaging checks.

Direct fast-forward pushes to `main` (and optional PRs) run affected owners
asynchronously without blocking subsequent commits or uploading release
candidates. Each `push` run keeps its own concurrency group so a follow-up docs
commit never cancels an in-flight cross-platform check; optional PR runs cancel
superseded revisions. The stable `CI gate` verifies the exact selected job
inventory and rejects failures, missing results and unexpected skips. Selection
compares the entire pushed or PR range to its base (`github.event.before` or PR
base), including deletions and both rename ends; missing, initial, or unreadable
history falls back to all owners. Workflow-level path filters are not used, so
the gate always reports a complete verdict.

Formal releases do not rely on ordinary push or PR runs: `just release-publish`
always requires an explicit, exact-SHA `workflow_dispatch` (`just full-check`)
on `main` with cold native builds and verified unsigned candidate assembly.

## Explicit full validation and release candidates

```bash
just full-check
```

From clean, exact current main, this dispatches `ci.yml` with an expected source
SHA, watches that exact new run, and runs every owner, including cold native
builds and unsigned candidate assembly. It does not tag, sign or publish. A main
movement during dispatch fails instead of substituting another source. Existing
failed runs are never automatically retried. When publication is requested,
`just release-publish` (or `just release` for an optional PR) rejoins an existing
exact full run or starts one before tagging; see [release operations](releasing.md).

Full validation covers three Unix hosts, native Windows x86-64/ARM64, Android/iOS shared compilation,
patched transport suites, dependency policy, Relay runtime, native packaging
floors, CLI smoke and unsigned installer/manifest verification. macOS Intel remains paused. Cross-platform runtime evidence cannot be inferred from
a local macOS pass.

Shared changes need app integration too: update the pin in `voytty-app` and run
its selected Android/iOS checks. The public compile checks do not establish app
runtime compatibility. See [the split boundary](mobile-repository.md).

## Cache ownership

Ordinary `main` pushes and optional PRs may restore third-party compiled
dependencies. Only `main` writes them, after removing all path-package and test
outputs. Explicit `workflow_dispatch` native candidate builds always start cold
(`lookup-only: true`); successful full runs save trimmed dependencies for later
non-candidate checks. Cargo downloads and command tools have separate versioned
caches. The 2.5 GiB compiled-cache limit, exact tool versions, no incremental CI
builds and exclusion of product binaries remain unchanged. Do not weaken tests
or introduce automatic retries to obtain a green run.

Local/hosted owners remain shared `justfile` recipes: `ci-policy`,
`ci-dependencies`, `ci-unix <docs> <transport>`, `ci-windows <transport>`, `ci-relay`, and
`mobile-core-check <android|ios>`. `just check` composes the complete current-host
profiles; it is useful before broad infrastructure work, not mandatory after each
ordinary change. The installer, signing, final native installer proofs and
immutable publication checks retain their separate release responsibilities.

The terminal-identity integration test uses the account's login shell and waits
for bracketed-paste mode or the default Bash prompt. Custom Bash prompts without
that suffix need bracketed-paste support for this fixture, not for product use.
Never use real `~/.voytty` for mutation tests; use explicitly disposable fixtures.

## Branch protection and ongoing checks

Repository administrators own the following `main` protection settings for the
solo-maintainer direct-push workflow. The release operator checks them and does
not mutate them:

1. disable force pushes (`allow_force_pushes: false`);
2. disable branch deletion (`allow_deletions: false`);
3. enforce those non-destructive history rules for administrators (`enforce_admins: true`);
4. do not require pull-request reviews or blocking status checks on ordinary
   fast-forward pushes (publication remains gated by exact-SHA `full-check` and
   the protected `release` Environment).

Apply protection only after `CI gate` exists on the default branch. Review critical path, cache hit rate, flaky failures and unique platform catches
after enough scoped PR/full runs to measure steady-state behavior. Weekly `dependencies.yml` also runs both
dependency-policy owners to detect new advisories while the project is idle.
This is early detection; PR/main dependency failures remain blocking.

For a failed job, use the matching recipe printed in its step summary. Runner
queue delay is separate from job execution time; job timeouts prevent hangs
but do not eliminate GitHub-hosted queueing.

`cargo run --quiet --package voytty-cli -- --help` and `--version` are
side-effect free. Never use real `~/.voytty` for mutation tests; inject
task-private `UserPaths` through the library harnesses. Local readiness must
remain independent of Iroh, DNS, Relay, and Internet access.

## Windows development

Use the native MSVC Rust 1.98.0 toolchain, Visual Studio C++ build tools and Windows
SDK, Git, PowerShell 5.1 or later, and `just` 1.42.4. ARM64 builds also require
Clang (ring's ARM64 Windows C backend). The native runners are `windows-2025`
(x86-64) and `windows-11-arm` (ARM64). Do not substitute an x64 emulation build
for ARM64 evidence. Windows targets statically link the CRT.

`just ci-windows` owns script syntax, installer native-architecture preflight and
runtime type resolution in Windows PowerShell 5.1, workspace Clippy/tests, CLI
smoke, and the selected patched transport tests. `just windows-build` owns native PE architecture
and embedded source identity checks; it requires `RELEASE_TARGET`,
`VOYTTY_SOURCE_COMMIT`, and `SOURCE_DATE_EPOCH`. Both CI matrices use these recipes.
Existing Unix PTY/signal fixtures stay on Unix; Windows adds private ACL/pipe,
ConPTY and detached daemon fixtures. Shared attachment/recovery tests run on both.
The native CI jobs also run `just windows-installer-fixture` on their disposable
hosted VMs. This HTTPS-only fixture temporarily trusts its generated certificate
in that VM's machine store and verifies cleanup; it refuses local/self-hosted
execution. Ordinary `just ci-windows` and product installation require no elevation.
See [Windows architecture and evidence](windows.md).
