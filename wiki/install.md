# Install, update, and uninstall

voytty's official native channel is one signed GitHub Release per lockstep Cargo
version. It supports Apple Silicon macOS 13 or newer, plus arm64 and
x86_64 GNU/Linux with glibc 2.28 or newer, and native Windows x86-64/ARM64.
Alpine/musl and NixOS are not supported by this installer. Intel macOS is no longer published; its
historical Releases remain available. Package-manager channels and background
updates are intentionally absent.

The macOS/Linux installer requires `curl` and either `sha256sum` or `shasum`. Linux needs a
64-bit userspace, not only a 64-bit kernel. On Apple Silicon, a terminal running
through Rosetta still selects the native arm64 binary.

## Install

The normal path installs the latest stable native Release through one fixed
bootstrap URL:

```bash
curl -fsSL https://voytty.com/install.sh | sh
```

To inspect the bootstrap before running it, use the optional review-first path:

```bash
curl -fsSL \
  --output /tmp/voytty-install.sh \
  https://voytty.com/install.sh
less /tmp/voytty-install.sh
sh /tmp/voytty-install.sh
```

Select one exact stable or prerelease tag, or another user-owned destination:

```bash
curl -fsSL https://voytty.com/install.sh \
  | sh -s -- --version vX.Y.Z
curl -fsSL https://voytty.com/install.sh \
  | sh -s -- --install-dir "$HOME/bin"
```

The downloaded review-first script accepts the same options:

```bash
sh /tmp/voytty-install.sh --version vX.Y.Z
sh /tmp/voytty-install.sh --install-dir "$HOME/bin"
```

`VOYTTY_INSTALL_DIR` also selects the destination; `--install-dir` takes
precedence. With a pipeline, put the variable on the `sh` process:

```bash
curl -fsSL https://voytty.com/install.sh \
  | VOYTTY_INSTALL_DIR="$HOME/bin" sh
```

Exact version tags may include a prerelease suffix but cannot include SemVer
build metadata (`+build`), matching the release publisher. The bootstrap defers
its installation body until its function has been received. This reduces partial
execution when streamed into `sh`, but does not guarantee that every truncated
pipeline returns a failure. For an explicit download-success check, use:

```bash
curl -fsSL --output /tmp/voytty-install.sh \
  https://voytty.com/install.sh \
  && sh /tmp/voytty-install.sh
```

The default destination is `~/.local/bin/voytty`. If that directory is not on
`PATH`, the installer prints the required guidance; it does not edit shell
startup files. An existing writable `~/.local/bin`, including the ordinary
`0775` user-private-group directory produced by `umask 0002`, is accepted
without requiring `chmod`. The installer refuses an existing destination, a
relative, symlinked, non-directory, or unwritable install path, an unsupported
target, NixOS, musl, an old OS or glibc, and any incomplete or mismatched
Release asset.

The installer prints download/verification stages and a setup command, and warns
when another `voytty` shadows the installed binary on `PATH` or `zterm` is still
installed. PATH suggestions are shell-quoted and never edit configuration files.
Both installation and update execute candidates in a private directory beside
the destination executable, so a system temporary directory mounted `noexec`
does not prevent verification. The install filesystem itself must allow execution.

Downloads retain HTTPS-only redirects and byte limits. Each small asset has a
60-second budget; assets over 1 MiB have a 600-second budget. These budgets cover
at most four attempts, including backoff. Stalled or persistently sub-1-KiB/s
transfers are aborted after a 30-second observation window. Transient network
errors and HTTP 408/429/500/502/503/504 may retry; permanent responses, unsafe
redirects, oversize payloads, and integrity failures stop installation.

Installation does not run `setup`, create `~/.voytty`, generate an identity,
start a daemon, register a service, invoke `sudo`, or change shell files. Run
this separately when ready:

```bash
voytty setup
```

## Windows

The Windows bootstrap selects the operating system's native architecture, including
ARM64 when invoked from an emulated x64 shell. It requires PowerShell 5.1 or newer,
Windows `tar.exe`, and ConPTY (Windows 10 version 1809 or newer). Native CI runs on
Windows Server 2025 x64 and Windows 11 ARM64; it does not establish runtime evidence
for every older Windows build. See [Windows support](../docs/windows.md).

Run the installer in PowerShell:

```powershell
& ([scriptblock]::Create((irm https://voytty.com/install.ps1)))
# Optional: -Version vX.Y.Z -InstallDir 'C:\Users\me\bin' -NoPath
```

The default destination is `%LOCALAPPDATA%\voytty\bin\voytty.exe`. Installation
adds this directory to the current PowerShell process's PATH unless `-NoPath` is
supplied and prints guidance for future terminals; it does not change the registry
PATH. No administrator privileges, setup or daemon startup are required. Existing
executables are never overwritten. Manifest/signature and archive hashes, exact
lengths, the single-file archive inventory and native binary identity are verified.
Downloads allow only HTTPS redirects (at most five), 30 seconds without data and
bounded total time/bytes. Unlike the POSIX downloader, this installer does not
retry failed transfers automatically.

`voytty setup`, `connect`, Session commands, pairing, `update`, `uninstall` and
explicit `daemon autostart` work through the same command surface. Windows hosts
start Windows PowerShell with the account profile as their default shell. Private
state uses the account SID and protected ACLs; see [Windows support](../docs/windows.md)
for paths, console behavior and executable replacement.

## Explicit updates

Updates occur only when requested:

```bash
voytty update
voytty update --version vX.Y.Z
```

The first command selects the latest stable Release; the second may select an
exact stable or prerelease tag. Same-version installs and managed downgrades
are rejected. Manifest signature, target, version monotonicity, archive
length/hash, and candidate self-check all finish before voytty contacts or
stops the daemon.

The updater requires only its own target's package. Other platforms may be
absent or newly introduced without blocking this machine's update. The signed
manifest's common metadata is authenticated first; the selected entry must be
unique and pass URL, support-floor, length, digest, and build-identity checks.
No package for this platform produces an explicit unsupported-platform error.
The full publication matrix is checked by maintainer tooling, not the updater.

Update and uninstall are available only from an official managed Release
binary. A source-tree, ordinary-CI, or `UNCONFIGURED` build is rejected before
an update request reaches the network and before uninstall observes or deletes
state. Official binaries may still be installed in any user-selected absolute
writable directory; the proof is about build identity, not a hard-coded path.

With no live Sessions, update proceeds directly. Otherwise it lists their
names, including detached Sessions, and asks for English `[y/N]` confirmation.
Enter `y` or `yes` to continue in the same invocation; `-y`/`--yes` confirms
directly. Empty input, EOF, or another answer cancels. Noninteractive use with
live Sessions requires `-y`/`--yes`; public `--force` has been removed.

After approval, voytty stops the daemon, atomically activates the candidate,
and rechecks it. Activation/post-check failure restores the preceding binary.
A successful update starts the new daemon on configured installations, even
if it was previously stopped. The CLI shows actual update phases and reports
success after local readiness, without waiting for Internet connectivity.
PTYs ended for the update are not restored.

You can run the same command inside a remote voytty Session or a local voytty
self-attachment. A one-shot updater runs independently of that terminal. After
you confirm and the updater accepts the operation, closing the Session or
losing the connection does not cancel installation. Reconnect manually after
the new daemon starts; the old Session's shell and programs have ended.

An ordinary external terminal waits for the actual update result. On a
configured server, update stages and the final outcome are also recorded in
the existing daemon log. After reconnecting, check `voytty --version`,
`voytty status`, and `voytty logs`. If the daemon cannot start, use SSH or
another independent terminal to inspect the same information and follow the
restart guidance. Acceptance is not a claim that installation has succeeded.

The first upgrade from an older updater that terminates with its own Session
must still be run from an independent terminal, such as SSH. Downloading the
fixed candidate cannot change the old updater process already executing.
Subsequent upgrades can use the independent updater from within voytty.

Before setup, update installs the binary and prints `Run voytty setup to
configure and start the daemon.` It never creates an identity implicitly. If
activation committed but startup fails, the new binary stays installed and
the command returns an explicit partial-completion error with restart guidance.
An already installed older updater still runs its old behavior for the first
upgrade; invoking the new binary enables these command changes.

Binary rollback does not imply persistent-state rollback. A future release
that changes the state schema must provide its own migration and recovery
contract before publication.

## Uninstall and recovery

Review impact interactively, or confirm noninteractively:

```bash
voytty uninstall
voytty uninstall -y
```

Uninstall asks once to confirm the executable/identity deletion and any running
Sessions it will end, or accepts `-y`/`--yes` directly. Deletion still needs
confirmation when there are no Sessions. It reuses the identity-reset boundary:
stop the daemon, remove only the validated managed state inventory, and unlink
the exact running executable last. This destroys the local device
identity and authorization state; reinstalling and running setup creates a new
identity that must be paired again. It does not send `RevokeSelf` to other
devices, so copied old private keys remain a per-host revoke concern.

Ordinary update failures leave either the current binary untouched or the
automatic rollback binary restored. If a published binary cannot run its own
updater, quarantine the exact executable, review/download a known-good
versioned installer, and install into the now-empty destination. Do this only
when that release supports the current state schema; otherwise preserve
`~/.voytty` and request a release-specific recovery procedure. Never replace an
unknown file or bypass signature/hash verification merely to force a rollback.

## Transition from zterm

voytty is a fresh installation, not an in-place update of zterm. The CLI is
`voytty`, native state lives in `~/.voytty`, and Android/iOS application IDs
start with `com.voytty.app`. Pairing tickets use `voytty-pair-v1:`;
both ends must run voytty and be paired again. Existing zterm identities,
configuration, Sessions and mobile data are not imported or deleted.

Before switching, finish any old Sessions and use the old executable to run
`zterm daemon autostart disable` and `zterm daemon stop`. Install voytty, run
`voytty setup`, and pair each client again. Enable `voytty daemon autostart`
separately if desired. The two applications have separate service identities.

Previously published zterm Releases retain their original asset names and
signatures after the GitHub repository rename. The new installer and updater
require a voytty Release; they cannot install or update from those historical
assets. Historical version examples describe the old release process, not
renamed copies of those immutable assets.

Early voytty development builds used `io.github.leonfox28.voytty` (and `.dev`)
on Android and iOS. The domain-based IDs are now `com.voytty.app` and
`com.voytty.app.dev`. These install as separate apps and do not import the old
mobile identity, saved hosts or settings; pair the new app again. Existing
development installations can remain installed until no longer needed.

For signing and verification details, see the
[installer trust model](../docs/install.md#what-is-authenticated).
