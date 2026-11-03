# voytty

voytty is a remote terminal for working on your computers from another computer
or the official iOS and Android apps. It supports macOS, Linux, and Windows.

- **Keep work running:** disconnect and return to the same terminal session
  while voytty's background service (daemon) is running on the host.
- **No public IP required:** automatic NAT traversal (hole punching) connects
  your devices directly when possible, with relay fallback when needed.
- **Encrypted connections:** terminal traffic is end-to-end encrypted, including
  when it passes through a relay.
- **Simple pairing:** connect with a one-time pairing ticket.

## Install

### macOS and Linux

Supports Apple Silicon with macOS 13+, and Linux x86-64/ARM64 with glibc 2.28+.
The installer does not support Intel Macs, Alpine/musl, or NixOS.

```bash
curl -fsSL https://voytty.com/install.sh | sh
```

The default location is `~/.local/bin/voytty`. Follow the installer's PATH
instructions if needed.

### Windows

Supports x86-64 and ARM64. Requires Windows 10 version 1809+ and PowerShell 5.1+.
Run in PowerShell:

```powershell
& ([scriptblock]::Create((irm https://voytty.com/install.ps1)))
```

The default location is `%LOCALAPPDATA%\voytty\bin\voytty.exe`. Follow the
installer's PATH instructions for future terminals.

Installation does not start the daemon; complete setup below.

See the [installation guide](wiki/install.md) for specific versions and recovery.
Coming from zterm? Follow the [migration guide](wiki/install.md#transition-from-zterm)
and pair your devices again.

## Update

To update an official installation, run:

```bash
voytty update
```

Updates end running sessions and ask for confirmation first when any are active.

## Use

### 1. Set up the computer you want to access

```bash
voytty setup --name workstation
voytty pair create
```

First setup starts the daemon. The second command displays a QR code and a
pairing ticket, valid for 10 minutes. Share the ticket only with the device you
want to authorize.

### 2. Pair and connect from another computer

Install voytty there, then run:

```bash
voytty setup --name laptop
voytty pair accept --alias workstation
```

Paste the ticket when prompted, then connect:

```bash
voytty connect workstation
```

This opens the host's `main` session, creating it if needed. Pairing allows the
laptop to control the workstation; access in the reverse direction requires
separate pairing. After setup, running `voytty` alone opens a local session.

### 3. Disconnect and return later

Press **Ctrl+]**, then **.** to detach without ending the session. Run
`voytty connect workstation` again to return. Typing `exit` in the session's
shell ends it; sessions also end when the host or daemon stops or restarts.

A few useful commands:

| Command | Purpose |
| --- | --- |
| `voytty session list --target workstation` | List the host's sessions |
| `voytty device list` | Show paired devices |
| `voytty daemon autostart enable` | Start the daemon at login on this computer |
| `voytty status` | Check this computer's daemon and connections |
| `voytty doctor` | Diagnose connection or setup problems |

Use `voytty --help` for all commands. See the [CLI guide](wiki/remote-cli.md)
for named sessions, connection troubleshooting, and advanced options, or the
[logging guide](wiki/logging.md) for diagnostics. All user guides are in
[wiki/](wiki/README.md).

## Uninstall

For an official installation, run:

```bash
voytty uninstall
```

After confirmation, this stops the daemon, ends its sessions, and removes the
executable, local identity, configuration, and pairing data. Reinstalling
requires pairing your devices again.

## Development

Install Rust through rustup and install `just`, then clone and build:

```bash
git clone https://github.com/leonfox28/voytty.git
cd voytty
just doctor
cargo build --locked --package voytty-cli
```

The repository pins its Rust toolchain in `rust-toolchain.toml`. Read the
[development guide](docs/development.md) for required tools and platform checks,
and run `just check-changes` before pushing changes. See
[CONTRIBUTING.md](CONTRIBUTING.md) for contribution terms and
[release operations](docs/releasing.md) for publishing.

This repository contains the CLI, daemon, and shared Rust libraries. The official
mobile apps are maintained separately; see [repository boundaries](docs/mobile-repository.md).

## License

voytty is source-available under [PolyForm Noncommercial 1.0.0](LICENSE),
not an OSI-approved open-source license. Noncommercial use and modification do
not require publishing your changes. Commercial use requires a separate
[commercial-use entitlement](COMMERCIAL-LICENSE.md). Third-party components
retain their own licenses. See the [licensing guide](wiki/licensing.md) for details.
