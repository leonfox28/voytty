# Remote-session implementation contracts

The M7-M8 command surface is implemented as a thin client of the current
user's daemon. The CLI never reads `identity.key` or SQLite, never owns an Iroh
Endpoint, and never resolves routes itself. Local and remote terminal commands
both enter the daemon's one `SessionService`, so a Session has the same
`SessionId`, PTY, working directory, controller lease, and terminal model
regardless of which authorized device is attached.

User commands, pairing, session controls, and network troubleshooting live in
the [CLI guide](../wiki/remote-cli.md).

## Frontend lifecycle and synchronization

Commands that need pairing, device, Session, or terminal service validate an
existing setup and then singleflight-start one daemon on demand. They never
perform setup implicitly.

Interactive attach requires both stdin and stdout to be TTYs. Before any
attachment request, the CLI saves the exact terminal attributes, enters raw
mode and a voytty-owned alternate screen, and installs SIGWINCH, SIGINT,
SIGTERM, and SIGHUP handling. Normal detach, typed failure, captured signal,
cancellation, and panic unwind restore terminal modes and display state before
printing a content-free diagnostic. SIGKILL and process abort cannot run this
cleanup and are not claimed as recoverable.

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

## Historical acceptance notes

The following milestone notes record the original acceptance scope. For the
current platform matrix and CI owners, see [development](development.md) and
[Windows support](windows.md).

At the M7–M8 milestone, the tree was a development build. The Linux real-Iroh
remote Session gate had been added, but its hosted execution and run URL were
pending. Windows then provided only shared compile-time and typed unsupported
boundaries, without a supported terminal runtime.

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
daemon-like harness. Windows CI was paused at that milestone; current native
Windows checks are documented in [development](development.md#windows-development).
A macOS cross-compile cannot substitute for native Windows runtime evidence.
