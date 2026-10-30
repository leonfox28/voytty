# Remote sessions and the public CLI

The M7-M8 command surface is implemented as a thin client of the current
user's daemon. The CLI never reads `identity.key` or SQLite, never owns an Iroh
Endpoint, and never resolves routes itself. Local and remote terminal commands
both enter the daemon's one `SessionService`, so a Session has the same
`SessionId`, PTY, working directory, controller lease, and terminal model
regardless of which authorized device is attached.

This repository is still a development build. The Linux real-Iroh remote
Session gate has been added but its hosted Linux execution and run URL are
pending. Windows keeps shared compile-time and typed unsupported boundaries;
it is not a supported terminal runtime.

## Setup and inspection

The production default and recommended infrastructure profile is Iroh's
official N0 profile. The exact setup syntax is:

```text
voytty setup [--name <name>]
            [--profile <official-n0|self-hosted>]
            [--relay-url <https-url>]
            [--terminal-identity <voytty|ghostty>]
            [--udp-port <0-65535>]
voytty setup --show
```

The recommended noninteractive invocation is:

```text
voytty setup --name <name>
```

First setup defaults to `official-n0`, terminal identity `voytty`, and automatic
UDP port allocation (`0`). An interactive terminal asks for a missing device name;
noninteractive initialization requires `--name`. Explicit `--profile self-hosted`
requires a Relay URL, prompted when interactive or supplied with `--relay-url`.
Initialization saves all choices before starting one daemon, so a fixed UDP port
is used from its first start. For example:

```text
voytty setup --name my-host --terminal-identity ghostty --udp-port 42424
```

On an already configured host, `setup` updates only explicitly supplied terminal
identity and UDP port values. Omitted settings, device identity, and pairings are
preserved. Updates are atomic and never start or restart a daemon, even if it is
stopped; saving preferences does not require a responsive daemon. The committed
device name and infrastructure retain their existing
conflict protection: passing a different name, profile, or Relay URL is rejected
without saving any preference changes.

With no options on a configured host, `setup` shows the saved configuration.
`setup --show` always inspects only, including before initialization, and cannot
be combined with update options. It shows the name, infrastructure profile,
terminal identity, and saved UDP port; use `status` for live daemon state.
Use `daemon restart` to explicitly start a configured, stopped daemon. If first
startup fails after configuration was saved, the same command starts it again.

The previous `config` command has been removed. Use `setup --terminal-identity`
and `setup --udp-port` to change preferences, and `setup --show` to inspect them.
Run these commands on the host being configured; they have no `--target` option.

These inspection commands do not start a daemon or create state:

```text
voytty status
voytty doctor
voytty logs [-n|--lines <n>]
voytty setup --show
```

`status` summarizes the device name, version, daemon state,
infrastructure, network state, and running Session names/count. `doctor` shows
the detailed local socket/lock and network observations, effective account,
managed state, and the lack of automatic login startup. Status and doctor
output is human-readable text; logs additionally supports `--json`. Single-object fields
are aligned, lists are name-first and full IDs appear on a separate line.
Setup distinguishes Not configured from Configured even when the daemon is stopped.
Results (including doctor reports and logs) use stdout; prompts, confirmations,
progress and errors use stderr. Public execution errors have one `Error:` prefix.
A typed missing-Session error includes a shell-quoted `session list --target=...`
hint, while cancellation, unknown outcomes and created-but-not-attached failures
retain their distinct diagnostics.

`setup --terminal-identity <voytty|ghostty>` chooses the local host's child-program
identity. `voytty` is the default and restores native identification; `ghostty`
enables compatibility for applications such as Herdr that select OSC 9
notifications by terminal name. On a configured host, changes apply to new
sessions without starting or restarting the daemon. Existing sessions keep their
identity. See [terminal identity and notifications](persistent-sessions.md) for
the environment contract and notification prerequisites.

`logs` reads a bounded tail once: 100 lines by default, at most 1,000 lines and
1 MiB. `-n` aliases `--lines`; missing logs produce an English explanation. Key
events persist by default, with optional detail enabled for 15 minutes using
`logs debug on`. Level/component/Session/time filters, archives and `--json` are
supported. `logs export --output PATH [--include-debug]` creates a safe explicit
export. See [Local logs and diagnostics](logging.md) for limits and migration.

Daemon lifecycle commands are:

```text
voytty daemon stop [-y|--yes]
voytty daemon restart [-y|--yes]
```

Stopping an already stopped daemon succeeds. With no running Sessions, stop
and restart proceed directly. Otherwise they list the Session names and ask
for English `[y/N]` confirmation because those Sessions and PTYs will end.
Detached Sessions also need confirmation; connections without Sessions do not.
A newly admitted Session encountered by the stop request is presented in the
same invocation before it can be ended. `restart` explicitly starts a daemon.
Like `update`, both may be run inside a remote or local voytty Session: after
confirmation a one-shot process independent of that terminal completes the
stop and any restart while the Session ends. Reconnect after a restart.

Confirmation accepts `y` or `yes`, ignoring case and surrounding whitespace.
Empty input, EOF, or another answer cancels. `-y`/`--yes` skips the prompt;
noninteractive input with live work must supply it. Public `--force` has been
removed. The same input rules apply to close, revoke, reset, uninstall and
update. Reset/uninstall still confirm actual deletion even with no Sessions.

## Fixed UDP listen port

The host defaults to automatic UDP port allocation, including when an older
configuration has no `udp_port` field. Select a port during initialization with
`setup --name <name> --udp-port <port>`, or update a configured host with:

```text
voytty setup --show              # show saved settings; does not query live sockets
voytty setup --udp-port 42424     # save an example fixed port for the next start
voytty setup --udp-port 0         # restore automatic allocation for the next start
```

Choose an available integer from 1 through 65535 for a fixed port. Zero means
automatic; negative values, values above 65535, and non-integers are rejected.
Ports below 1024 may require OS privileges. On an already configured host, changing
the saved port does not start or restart a daemon or change current connections
or Sessions. It atomically saves the top-level `udp_port` integer in `config.toml`
under the managed state directory (normally `~/.voytty` on Unix). Identity,
pairings, terminal preferences, and infrastructure selection are preserved.
Repeating `setup` without `--udp-port` preserves the saved port.

When ready to apply a change, run `voytty daemon restart`; this ends existing
Sessions and uses the normal confirmation rules described above. For a daemon
managed by a service manager, restart its service during your maintenance window.
A stopped daemon reads the saved value on its next start. Saving a port does not
reserve or preflight it: availability is determined when the daemon binds.

A fixed port is requested on both `0.0.0.0` and `[::]`. IPv4 binding is required.
If its port is occupied or access is denied, local CLI readiness remains usable,
while remote networking is degraded and `voytty doctor` reports
`endpoint_bind_failed`. The network supervisor retries the same saved port with
bounded backoff; it never silently switches to an automatically allocated port.
Release the conflicting port to allow recovery, or save another value and restart.
IPv6 keeps Iroh's existing optional-bind behavior: if unavailable or occupied,
IPv4 continues and no IPv6 listener is created for that daemon run. It does not
fall back to another IPv6 port. With automatic allocation, the IPv4 and IPv6
ports can differ and can change after a restart.

For a cloud host, allow inbound **UDP** on the chosen port in both the cloud
firewall/security group and the host firewall, for the IP families you use.
42424 above is only an example, not a product default. A fixed local port does
not force the NAT's public port mapping, guarantee a Direct path, or replace
Relay access. Iroh still discovers the public mapping and performs NAT traversal.
Pairing, device identity, encryption, address discovery, and mobile clients are
unchanged; no App update or re-pairing is needed.

## Pairing and directional trust

Pairing commands are:

```text
voytty pair create
voytty pair accept [--stdin] [--alias <alias>]
```

`pair create` creates one ticket with a 10-minute validity. Interactive stdout
shows a monochrome QR and the same manual ticket. If the terminal is too narrow,
its width is unavailable, or QR encoding fails, it retains the manual ticket and
explains the fallback on stderr; no second offer is created. Noninteractive
stdout contains exactly the raw ticket plus one newline, with no ANSI or guidance.
Validity and receiving instructions always go to stderr. Actual output-write
failures still fail the command. There are no TTL/QR options or PNG export.
Keep the ticket out of command arguments, environment variables, shell history,
logs and error reports; the QR carries the same bearer authority and expiry.

By default, `pair accept` reads one bounded line from an interactive TTY while
echo is disabled. It has no ticket positional argument and no `--ticket` flag.
A non-TTY is rejected without reading unless automation explicitly selects
`--stdin`; that mode reads the bounded standard input to EOF, trims surrounding
whitespace, and rejects input over 16 KiB. `--alias` assigns the exact outbound
alias after acceptance; the old `--name` spelling is rejected. Success reports
the control direction and a `voytty connect` command using the actual stored
alias, quoted for shell use when needed.

Trust is directional. If host A creates a ticket and controller B accepts it:

- A authorizes B to control A (`inbound_status` on A).
- B records A as a device it can connect to (`outbound_known` on B).
- A does not gain permission to control B. Establish that direction with a
  separate ticket from B.

Device management preserves those two directions:

```text
voytty device list
voytty device rename <device> <alias>
voytty device revoke <device> [-y|--yes]
```

The table leads with device names and aliases and retains full device IDs,
Connection, Known host and Allowed here columns. Known host is the local outbound
record, not proof of current remote permission; Allowed here is inbound Yes/No/Revoked. An unconnected device is not necessarily unreachable.
An empty list explains how to pair a device. It does not expose route cache
entries, direct addresses, Relay URLs, tickets, terminal content, or working
directories.

`rename` changes only an outbound known-device alias and therefore rejects an
inbound-only record. `revoke` changes only the selected device's authorization
to control this host; its outbound-known record is retained. Revoke disconnects
and detaches that remote principal, but it does not close the host Session or
PTY and does not affect another principal. It uses the shared `[y/N]`
confirmation or explicit `-y`/`--yes`.

An unauthorized remote peer receives only the generic `unauthorized` category.
The wire response does not reveal whether the identity is unknown, revoked, or
using a stale authorization generation.

## Targets and selectors

Every Session command resolves its target inside the local daemon:

- `local` is the reserved target for this daemon and cannot be a device alias.
- A remote target is an exact, case-sensitive outbound alias or a canonical
  full Device ID: exactly 64 lowercase hexadecimal characters.
- Exact aliases are checked before hex-looking short text is rejected. Without
  an exact alias, short/prefix IDs are invalid. A 64-character ID candidate
  must be lowercase; one that is also another device's exact alias is rejected
  as ambiguous.
- Remote Session access requires the outbound-known direction. Inbound
  authorization alone does not make a device connectable.

A Session selector is either an exact, case-sensitive Session name or a
canonical full Session ID of 32 lowercase hexadecimal characters. No prefix
matching is performed.

`local` uses same-UID IPC only. It does not self-dial, create an Iroh
connection, perform DNS/Pkarr lookup, or use a Relay.

## Session commands

The public commands are:

```text
voytty connect [<target>] [--session <name-or-id>] [--takeover]
voytty session list [--target <target>]
voytty session create <name> [--target <target>] [--cwd <host-path>]
voytty session rename <session> <new-name> [--target <target>]
voytty session close <session> [--target <target>] [-y|--yes]
```

`connect` defaults to target `local`. Only when `--session` is omitted does the
daemon atomically create or reuse `main`. An explicit `--session main`, any other
name, or a full ID only attaches an existing Session; missing selections fail
without creation. `session create` creates the
named Session and immediately attaches the exact returned `SessionId`; if the
follow-up attach fails, the created Session remains live and its ID is reported.
`--cwd` is interpreted and validated by the selected host, not the controller.

All Session management commands default to `local`; use `--target <target>` for
another host. Positionals select the Session/name, never the target. The list
shows Target and a Name / State / Size table (columns x rows), with full IDs on
the next line. Empty results include a default-connect creation hint.

`session close` ends the exact selected Session and its PTY using shared
`[y/N]` confirmation or explicit `-y`/`--yes`. Rename preserves the Session ID.
Session lists intentionally omit the working directory and terminal content.

There is one controller per Session. An ordinary second attachment never
steals it and returns occupied. `--takeover` first synchronizes a replacement,
then atomically invalidates the previous controller lease. Local attachment has
no priority over remote attachment: remote-to-local and local-to-remote
takeover use the same rule and continue the same Session and PTY.

With no command, behavior is setup-sensitive:

- Before setup, `voytty` prints the fixed `voytty setup` guidance and does not
  create an identity or start a daemon.
- After setup, `voytty` is equivalent to
  `voytty connect local`.
- `voytty --help`, `voytty --version`, parse failures, and all inspection
  commands remain side-effect free.

Commands that need pairing, device, Session, or terminal service validate an
existing setup and then singleflight-start one daemon on demand. They never
perform setup implicitly.

## Attachment, reconnect, and terminal controls

Interactive attach requires both stdin and stdout to be TTYs. Before any
attachment request, the CLI saves the exact terminal attributes, enters raw
mode and a voytty-owned alternate screen, and installs SIGWINCH, SIGINT,
SIGTERM, and SIGHUP handling. Normal detach, typed failure, captured signal,
cancellation, and panic unwind restore terminal modes and display state before
printing a content-free diagnostic. SIGKILL and process abort cannot run this
cleanup and are not claimed as recoverable.

The view moves through preparing, synchronizing, active, and reconnecting
states. A temporary remote transport loss displays:

```text
[voytty: reconnecting]
```

The host Session and PTY continue. After the first successful attach, the
bridge freezes the exact Session ID; reconnect never silently creates another
Session with the same name after host daemon restart or Session termination.
Each reconnect uses a fresh remote attachment identity while the local view
identity remains stable.

While preparing, synchronizing, or reconnecting, the CLI drains and discards
ordinary input instead of queueing it for replay. It retains only the latest
viewport. A full snapshot is written and flushed before its exact revision is
acknowledged; input and resize become effective only in the active state. A
revision gap requests an authoritative full synchronization rather than
guessing missing output. SIGWINCH updates are coalesced.

The fixed local command prefix is `Ctrl+]`:

- `Ctrl+] .` detaches only this view. It does not close or signal the Session.
- `Ctrl+] v` uploads one copied file or clipboard image in a remote view.
- `Ctrl+] c` cancels that upload. `Ctrl+V` remains ordinary paste.
- `Ctrl+] Ctrl+]` sends one literal Ctrl+] key using the existing child encoding.
- The desktop CLI status bar shows `Ctrl+] command: . detach` while the prefix
  is pending, with upload/cancel hints on remote views. The hint temporarily
  replaces upload progress and disappears after a command, cancellation, or timeout.
- An unknown command or a one-second timeout cancels locally. The attempted
  command is not replayed to the Session.
- Legacy and already-reported enhanced keys use the same command dispatcher.
  Prefix handling does not change the terminal's keyboard-reporting mode.
- Terminal focus reports keep their normal forwarding and do not cancel a
  pending prefix command.
- Input-method text is not remapped to a physical key: a Chinese `。` does not
  trigger the period command.

The former `--escape` customization/disable option has been removed.

For file upload, copy exactly one regular file in the system file manager, or
copy an image, then press `Ctrl+]` followed by plain `v`. Text containing a path
or URL is not an upload source. Files of any type are accepted up to **50 MB
(50,000,000 bytes)**. Original file bytes are preserved; a raw clipboard image
is saved as PNG. The remote host and the local daemon must both support uploads.

Progress, size and speed appear on the right of the bottom status row. Input to
the child is paused and discarded during upload; terminal output and local
navigation continue. After publication the absolute path is pasted once at the
cursor, using bracketed paste when enabled, without pressing Enter. Detaching,
disconnecting or losing control cancels the operation and prevents delayed input
from reaching another attachment. A single-row terminal must be enlarged first.

Uploads are private to the host's effective user under
`/tmp/voytty-<uid>/<session>/<transfer>/file[.ext]`. Successful files are retained
without a voytty age or cumulative-size cleanup policy; normal host `/tmp` cleanup
may still remove them. An uncertain result can mean a file remains on the host;
retrying is an explicit new upload. The AI tool decides how to interpret the
inserted path and supported file format.

Because the terminal is raw, ordinary keyboard `Ctrl-C`, `Ctrl-Z`, and similar
control bytes go to the active host PTY. A separately delivered process signal
terminates the local view through the restoration path.

Session state is daemon-lifetime, not disk-persistent. Detach, CLI exit, stream
loss, route change, and device revoke do not end the PTY. Root-shell exit,
explicit `session close`, confirmed daemon stop/restart, identity reset, daemon
crash, and host reboot do. A new daemon starts with no live Session registry.

## Ambiguous operations

Read-only remote requests may retry one byte-identical service request after a
transport ambiguity. Create, rename, close, and takeover reuse the same
daemon-issued operation lease, operation ID, target, and encoded bytes for at
most one remote retry, so a committed response loss replays the exact result.
Operation-lease allocation itself is not retried after a write ambiguity.

A live host acknowledges a request at the QUIC layer within about one round
trip, even while its service is still working. When a retry is allowed and the
connection acknowledges nothing for four path round trips (at least one second,
at most half the remaining time), the first attempt counts as a transport
ambiguity. That connection is retired and the retry uses a fresh connection
within the same deadline. The count covers the whole connection, so this
happens even while a terminal attachment uses it: nothing on the connection
reaches the host, and the attachment reconnects at once instead of after the
QUIC idle timeout. A request that fails after its write on a connection the
host still acknowledges retires that connection only if nothing else uses it.
This covers a host that restarted on another Relay or address while the client
kept its old connection. Lease allocation and the retry itself still wait for
the full deadline.

If nothing is acknowledged halfway through that window, the client also starts
connecting to the host in the background, without the protocol greeting, so
a live host keeps using the old connection. If the old connection is then
retired, the retry uses whichever is connected first: the background
connection or a fresh one started after the window. Otherwise the background
connection is closed unused. The request is still sent at most twice.

If the daemon cannot prove whether a mutation committed, it reports
`operation_outcome_unknown`. The client does not allocate a fresh lease and
silently rerun that logical operation. Inspect current state before deciding on
a new, independent command.

## Identity reset

```text
voytty reset [-y|--yes]
```

Identity reset is destructive but does not uninstall the binary. It reports
the current public identity and running Session names, then asks once to end
the Sessions and delete local identity, configuration and pairing data. Use `-y`/`--yes` to confirm directly.
Existing identity data requires confirmation even with no Sessions; an already
absent state root is a successful no-op.
It then performs a bounded daemon stop, obtains the lifecycle lock, rechecks
the exact identity and daemon state, and removes only the validated managed
state root. No `RevokeSelf` is sent and setup is not run automatically.

After reset, run `voytty setup` to create a different identity and pair again.
The signed installer, explicit updater, and uninstaller reuse this local
lifecycle boundary; see [Install, update, and uninstall](install.md).

## Networks with a local proxy or TUN

Every remote connection first reaches the peer through its public Relay over
TCP/TLS on port 443, then moves to a direct UDP path when NAT traversal
succeeds. Each endpoint discovers its public address by UDP to the Relays on
port 7842. Terminal content stays end-to-end encrypted either way; see
[the Relay trust boundary](relay.md) for the metadata a Relay can observe.
Because connection setup always uses the Relay, a slow or unreliable route to
the Relays also delays connections that end up direct.

With a TUN proxy such as a Clash/mihomo-based client:

- Exclude `+.iroh.link` from fake-IP so Iroh's DNS answers stay real.
- Keep UDP to `iroh.link` direct. Proxying it makes the discovered public
  address the proxy's, and direct connections then fail.
- If the direct route to the Relays is unreliable, send only TCP to
  `iroh.link` through the proxy. In mihomo syntax, ahead of any direct rule:

  ```text
  AND,((DOMAIN-SUFFIX,iroh.link),(NETWORK,TCP)),<proxy group>
  DOMAIN-SUFFIX,iroh.link,DIRECT
  ```

Apply this on every device that runs such a proxy, hosts and controllers
alike. Typical symptoms of a poor Relay route are first connections that take
many seconds or end in `deadline_exceeded`, and Relay-only sessions with
multi-second latency. On one mainland-China network, 0–10% of new direct TLS
connections to the public Relays failed and the slowest took 2–8 s. With only
TCP proxied, none failed in a short probe, first connections took 1.2–4.2 s,
and Relay round trips fell to about 0.2–0.8 s while direct connections still
formed. These are measurements of one network, not a guarantee; see the
[lab record](verification/multi-device-lab.md).

## Transport acceptance boundary

The production default remains `OfficialN0`. The Linux-only
`two_daemon_transport` test composes two task-private production transport
owners, but deliberately builds its test Endpoints with `RelayMode::Disabled`,
IPv4 loopback, and a task-only direct route. It proves pair/normal ALPN
isolation, Endpoint/primary reuse, directional authorization, and direct-route
non-persistence. It is not evidence for the remote Session workflow, the
official-N0 Relay, public Internet traversal, an optional self-hosted Relay,
DNS/Pkarr discovery, two physical networks, NAT migration, or the public CLI as
multiple OS processes.

Developer macOS runs may compile this target but must never execute its real
Iroh case. Hosted Linux owns its actual runtime result. A broader remote
Session or pairing workflow target should be added only together with its
hosted job and run evidence, using the same fixture instead of another
daemon-like harness. Windows CI is currently paused; restoring shared
compile/Clippy evidence requires an explicit task and a hosted Windows run.
A macOS cross-compile cannot substitute for it.
