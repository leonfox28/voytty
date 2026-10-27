# voytty

Formerly zterm. This rename uses new application IDs, data directories and
pairing protocols; install voytty and pair your devices again. See the
[rename transition](docs/install.md#transition-from-zterm) before switching.

Source-available under [PolyForm Noncommercial 1.0.0](LICENSE). Noncommercial
use and modification do not require publishing your changes. Commercial use
requires a separate entitlement; the [commercial-use grant](COMMERCIAL-LICENSE.md)
covers an entitled user's official client and companion CLI/daemon on personal
or authorized work computers. See [licensing / 许可说明](docs/licensing.md)
and [contribution terms](CONTRIBUTING.md). Third-party components retain their
original licenses. This is not an OSI-approved open-source license.

voytty is a cross-platform remote-terminal project built around daemon-lifetime
terminal Sessions and end-to-end encrypted Iroh connections. The current tree
contains the shared terminal/domain model, versioned protobuf contract, secure
per-user state, one same-UID daemon, pairing and directional authorization, a
daemon-owned connection broker, local and remote Session adapters, and the
public raw-terminal CLI. The Android app adds native touch scrolling, cross-screen
selection, system IME, QR pairing and Session management over the same shared
client. See [Android build, usage and APK installation](docs/android.md) and the
[session usability roadmap](docs/roadmap.md).

The [Swift iOS app](docs/ios.md) follows the Android interface and Session workflow
over the same client: QR/manual pairing, saved hosts, native terminal selection
and keyboard input, image/file uploads, notifications and diagnostic export.
See the [parity and acceptance record](docs/ios-parity.md) for platform differences.
Build it locally with Xcode; signed-phone acceptance and iOS distribution remain pending.

This is still a development build. The Linux real-Iroh remote Session target
exists but its hosted runtime result and public multi-process CLI evidence are
pending; macOS development runs compile that target without executing it.
Windows is not a terminal runtime in this milestone.

## Install

Install the latest stable native Release with the fixed bootstrap URL:

```bash
curl -fsSL https://raw.githubusercontent.com/leonfox28/voytty/main/install/install.sh | sh
```

The installer does not run `setup` or start the daemon. See
[Install, update, and uninstall](docs/install.md) for exact-version,
review-first, update, uninstall, and recovery paths.

Current public commands are:

```text
voytty setup [--name <name>] [--profile <official-n0|self-hosted>]
            [--relay-url <https-url>]
voytty status
voytty doctor
voytty pair create
voytty pair accept [--stdin] [--alias <alias>]
voytty device list
voytty device rename <device> <alias>
voytty device revoke <device> [-y|--yes]
voytty connect [<target>] [--session <name-or-id>] [--takeover]
voytty session list [--target <target>]
voytty session create <name> [--target <target>] [--cwd <host-path>]
voytty session rename <session> <new-name> [--target <target>]
voytty session close <session> [--target <target>] [-y|--yes]
voytty daemon autostart <enable|disable|status>
voytty daemon stop [-y|--yes]
voytty daemon restart [-y|--yes]
voytty logs [-n|--lines <n>] [--level <level>] [--since 30m] [--include-debug] [--json]
voytty logs debug on|off|status
voytty logs export --output <path> [--include-debug]
voytty reset [-y|--yes]
voytty update [--version <vSEMVER>] [-y|--yes]
voytty uninstall [-y|--yes]
voytty --license
```

`setup`, `daemon restart`, and a successful update of a configured installation
start the daemon. Pair, device, connect, and Session commands start it on demand
only after validating an
existing setup. Inspection, logs, stop, help/version, and parse failures never
start it. With setup complete, bare `voytty` is equivalent to
`voytty connect local`; before setup it only prints setup
guidance. Only omitted `--session` enables atomic creation of `main`; an explicit
`--session main` (like any explicit name/ID) requires an existing Session.

First setup defaults to `official-n0`; `voytty setup --name <name>` is enough.
Commands print human-readable text. Update, daemon stop and daemon restart list
running Sessions and ask for English `[y/N]` confirmation only when they would
end live work; use `-y` or `--yes` to confirm directly. `logs -n 100` reads recent
records once. Key events are saved by default; optional detail expires after 15
minutes. See [Local logs and diagnostics](docs/logging.md) for filters, Android
controls, export and retention limits.

See [Remote sessions and the public CLI](docs/remote-cli.md) for the exact
command, target, pairing, reconnect, takeover, raw-terminal, ambiguity, and
identity-reset contracts. [Persistent session engine](docs/persistent-sessions.md)
documents the daemon-lifetime PTY and resource boundaries, while
[Core and local daemon](docs/core-local-daemon.md) documents local state and
lifecycle ownership. [Install, update, and uninstall](docs/install.md) covers
the signed native Release path and its trust/recovery boundaries.

## Version policy

voytty uses one lockstep SemVer for the product rather than independent
component versions. The root `[workspace.package].version` is the only source
for all product crates; future CLI, daemon, desktop/mobile apps, protocol
artifacts advance together. Relay image publication is currently paused.

A GitHub Release tag must equal `v` plus Cargo's resolved workspace version.
Default releases include macOS arm64 and Linux arm64/x64. macOS Intel,
Windows CI/distribution, and relay image publication require an explicit future
task before being restored. Internal validation tools such as the isolated Relay
handshake probe are not product deliverables and keep their own non-product
version.

## Repository boundaries

- `crates/` — six Rust crates owning core, protocol, platform, terminal-engine, daemon, and CLI boundaries.
- `proto/voytty/v2/` — the semantic product wire schema compiled with vendored `protoc` binaries.
- `deploy/relay/` — official relay artifact verification and Compose deployment.
- `tests/relay/` — architecture, checksum, minimal configuration, and health checks.
- `install/` and `tools/release/` — the reviewed bootstrap/template and private
  signed native-asset assembler.
- `.github/workflows/release.yml` — protected exact-tag native Release workflow
  for the three supported Unix targets and Android arm64, reusing exact main CI candidates.
- `docs/development.md` — exact local toolchain baseline and repeatable commands.
- `docs/releasing.md` — one-PR version preparation, exact main candidates, signing,
  publication, and resumable operator commands.
- `docs/relay.md` — relay trust boundary, publication, and deployment.
- `docs/phase-zero-verification.md` — evidence from the completed local gate.
- `docs/core-local-daemon.md` — current M2–M3 behavior, state, CLI, and exclusions.
- `docs/persistent-sessions.md` — M4 daemon-lifetime session and local attachment contracts.
- `docs/remote-cli.md` — M7–M8 public commands, directional trust, remote attachment, and evidence boundaries.
- `docs/install.md` — signed installer, explicit update/uninstall, recovery,
  and release-operator checkpoints.

## Development and release entry points

```bash
just check
just release-prepare <version>
just release-publish <version>
```

Run `just doctor` when setting up a machine. See [Development and CI](docs/development.md)
for PR/hosted evidence and [Release operations](docs/releasing.md) for the
two-phase release flow.

Local readiness and status do not initiate or wait on Iroh, DNS, Relay, or
Internet work; a running daemon owns its Endpoint separately. Never place SSH
private keys, identity keys, or real deployment credentials in this repository.
