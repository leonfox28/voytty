# Local Iroh 1.3.0 endpoint lifecycle fixes

This is the crates.io `iroh` 1.3.0 source, with its MIT / Apache-2.0 licenses.
Upstream: <https://github.com/n0-computer/iroh>, revision
`0072d7d84b233f9e7185eb676f049beaf557ac03` (`iroh/`). Published crate SHA-256:
`885787b892b5e2507c701f132ecbd45d2bad4bbb75157a19427087c16dadb833`.
The registry installation marker is omitted. The standalone workspace uses the
same patched NOQ as the product. Android's vendor source inputs cover this crate.

`Builder::persistent_stateless_reset(true)` is an explicit opt-in for endpoints
whose secret identity has one active owner. The default remains a random reset
key and NOQ's hashed CID generator. The opt-in derives a separate BLAKE3 key using
the fixed context `iroh zterm persistent stateless reset v1` and the secret
identity, and uses random 16-byte CIDs. TLS identity, ticket keys, handshake
tokens, deadlines and QUIC reset rate/size limits are unchanged.

Previously a complete restart changed both the reset key and the hashed CID
generator's validation key. Old CIDs were rejected before reset generation; even
without that rejection, the new reset token was not recognized. Changing only
one component does not restore the first cached query. Reusing the hashed
generator with a stable key would also retain its small 24-bit random CID nonce;
the new policy uses 128 fresh random bits instead.

The persistent key follows identity rotation and is not stored or logged
separately. Its context must remain stable across releases while the identity
remains usable. A migration from the old default cannot repair old tokens that
were issued with a random key; recovery applies to connections established with
the new policy before restart.

As required by [RFC 9000 §10.3.2](https://www.rfc-editor.org/rfc/rfc9000.html#section-10.3.2)
and [§21.11](https://www.rfc-editor.org/rfc/rfc9000.html#section-21.11), do not share
this identity between independently routed live endpoints. The daemon's lifetime
lock and single supervisor retain ownership until endpoint shutdown; Android's
Application retains one NativeRuntime whose OnceCell serializes initialization.
The production daemon and mobile profiles enable the policy. Cloned identities
and concurrent replicas are not supported by this opt-in API.

`just ci-iroh` checks opt-in defaults, stable identity-scoped/domain-separated
keys, independent ephemeral keys, and 8192 CID samples from two generators. Its
real QUIC/TLS regression opens a nonzero path, retires path zero at both ends,
suppresses close notifications, fully rebuilds the peer Endpoint, and requires a
recognized reset on the remaining path. The test gate uses only in-memory
transports and is compiled only with test utilities/custom transports.

The three `endpoint_restart` product tests exercise the shared controller and
both daemon infrastructure profiles through real TLS, Hello/Welcome and Session
queries. They require the very first query on an apparently open stale cache to
recover within the unchanged five-second deadline, with one fresh connection
and one delivered query. No mutation is replayed or recovery Session replaced.
Evidence is under `target/verification/multi-device/checks/ordered-fixes/endpoint-restart-fix/`.
These controlled tests do not establish the cause of every public-network timeout.

Remove the local API and patch when upstream exposes equivalent reset-key/CID
configuration, after porting these tests and running the multi-device matrix.

## Close cancellation and ownership

The upstream close path treated "closing started" as "shutdown finished". A
cancelled close made subsequent calls return immediately, concurrent calls could
report completion early, and Drop skipped aborting the remaining runtime tasks.
The actor handle now lives behind an async mutex held through shutdown. Cancelled
calls release that mutex while retaining the handle; the next caller resumes
the existing close. The last owner aborts unfinished shutdown even if closing
already started. The actor is joined before normal completion, including after
its existing 100 ms grace period expires.

QUIC close notifications, close codes, the protocol's close timer, and the
daemon's five-second deadline are unchanged. In a controlled real TLS link with
about two seconds of RTT, a responsive peer closes in about two seconds. With
close responses suppressed, the five-second caller deadline still fails while
`wait_all_draining` waits; a baseline observation reached the protocol close
timer at 11.565 seconds. The ownership fix does not turn that timeout into
successful graceful shutdown or prove the cause of the historical relay failure.

`just ci-iroh` also runs the in-memory `zterm_close_` regressions: cancelled and
concurrent callers, last-owner task cancellation, actor-handle retention, close
code delivery on a slow link, and a truthful five-second deadline failure when
the slow peer cannot respond. The original three ownership cases fail before
the repair. Evidence is under
`target/verification/multi-device/checks/ordered-fixes/endpoint-close-fix/`.
Retain these regressions when updating or replacing the vendored Iroh source.

## Relay startup queue expiry

The relay actor previously cleared its entire pending datagram channel every
three seconds while dialing. This could discard a just-queued QUIC Initial
retransmission. If the relay connected immediately afterwards, no Initial was
left to send until the next exponentially backed-off probe timeout.

Each datagram now retains its original enqueue time through routing and retry.
Cleanup removes only expired packets. Delivery checks age again after backoff,
preserves FIFO ordering and batching, and uses the existing bounded channel plus
one lookahead item. There are no per-packet tasks or unbounded reconnect buffers.
The three-second packet lifetime, relay connect timeout, production QUIC timers,
and five-second product request deadline are unchanged.

Three socket-free `zterm_cold_relay_` regressions cover a fresh packet crossing
the cleanup tick, mixed-age FIFO delivery, and a lookahead packet expiring during
backoff. The full Linux library also runs
`cold_relay_tls_first_request_survives_slow_relay_connect`: a real relay and TLS
connection behind a 3.3-second TCP forwarding delay. Its fixture uses a 300 ms
initial RTT to place a retransmit before cleanup; production RTT configuration
is unchanged. Before the fix the first request exceeded five seconds; afterwards
it completed in about 3.41 seconds. These controlled results establish the queue
defect, not the unique cause of every historical public-relay timeout.

Evidence: `target/verification/multi-device/checks/ordered-fixes/cold-start-fix/`.

## Discovery during an initial connection

An Initial already sent to a stale cached route used to wait for QUIC's next
probe timeout before reaching a newly discovered address. That also deferred
starting a new relay's TLS connection. The remote actor now extends the existing
Initial fanout when an exact-peer lookup adds a new, nonlocal address. Duplicate
addresses do not cause another send to existing paths. It retains only QUIC v1
Initial transmits while lookup is pending, at most 16 transmits / 64 KiB for
three seconds. Lookup completion, an added connection and expiry clear the cache; outgoing tasks retain the existing bound of 16 and their original
deadline. Normal QUIC timers, connection identity and application requests are
unchanged. This forwards the same encrypted packets with the same packet numbers;
it does not retry an application mutation or open another connection.

The socket-free `zterm_fresh_discovery_` regression supplies a usable route at
100 ms: the first real QUIC request took 1.018 s before and about 123 ms after.
Six `zterm_initial_fanout_` tests exercise identity, duplicate/local address,
expiry, selection/completion, capacity and packet-type boundaries. The Linux-only
`fresh_discovery_slow_tls_relay_first_request_keeps_original_deadline` uses a
real TLS relay and 4.1 s of controlled TCP startup delay. With default QUIC timers
and the same five-second request deadline, the original dial started at 1.007 s
and the request timed out; the repaired dial starts at about 109 ms and the first
request completes at 4.371 s. Keep these regressions when adopting an upstream
equivalent. They do not explain all historical public-network I/O delays.

Evidence: `target/verification/multi-device/checks/ordered-fixes/fresh-route-initial/`.

## New connections beside another connection's selected path

The remote actor keeps one selected path per peer. Upstream sent every
mixed-address datagram, which carries only a new outgoing connection's
handshake, to that path alone. It also skipped address lookup and the Initial
fanout above while a path was selected. If the peer restarted on another relay
or port without a close reaching this endpoint, the old connection kept its dead
path selected until its idle timeout: 30 seconds on a relay path, longer than
the product's 15-second startup budget. Every new dial to that peer stalled,
even when the caller supplied the correct new relay.

Handshake datagrams now go to the selected path first and then to every other
known nonlocal path, which is what upstream already did with no selected path.
A new connection starts an address lookup unless one is already running, and
its Initials are replayed to paths that lookup adds. Established connections,
path selection, holepunching, QUIC timers and product deadlines are unchanged.
Upstream `main` still pins these datagrams, so there is nothing to adopt yet;
reported as <https://github.com/n0-computer/iroh/issues/4600>. Remove this
patch once upstream ships an equivalent fix, after porting these tests.

Three `zterm_new_handshake_` / `zterm_new_connection_` /
`zterm_initial_fanout_survives_` tests fail before the repair and pass after it.
The product's `moved_host_` tests kill a relay-only host without a QUIC close,
restart its identity on another relay and dial with the same 15-second budget.
With the old connection still open, both the correct-hint and the
stale-lookup-first variants previously failed at 15.0 seconds and now succeed
in about 0.31 to 0.33 seconds. The closed-connection control succeeded in about
0.33 seconds both before and after. These tests establish the pinning defect,
not which historical R02 timeouts it caused.

## Fair scheduling of connected relay traffic

The connected actor used a biased select with outbound datagrams ahead of
received relay messages. A continuously ready send queue and writable TCP sink
could keep already-ready QUIC datagrams and pongs unread across every batch.
The send sub-loop only reads while a write is pending, so it did not provide
fairness when writes completed immediately.

Ready send/receive directions now alternate below the existing shutdown,
priority/control inboxes and liveness checks. An idle direction does not block
the other. The send sub-loop retains its behavior under backpressure. This adds
no task or queue and does not change batching, FIFO order, queue expiry, QUIC or
relay timers, terminal synchronization, or application request deadlines.

The socket-free `zterm_relay_backlog_` tests drive the actual connected actor
with typed relay messages, a ten-batch send backlog and immediately ready I/O.
Previously no incoming datagram or matching health-check pong was consumed
before the backlog completed; both checks fail before the repair and pass
afterwards. The fixtures also reject an always-receive-first replacement, and
`zterm_relay_cancel_preempts_ready_traffic` preserves shutdown priority.

These establish a relay scheduling defect, not the unique cause of the two
historical R03 public-network stalls. Those windows did not record whether both
I/O directions were continuously ready. Keep these regressions when adopting
an upstream equivalent. Evidence is under
`target/verification/multi-device/checks/ordered-fixes/r03-relay-backpressure/`.

## Finite relay timing diagnostics

`transport_trace` emits numeric metadata under `iroh::zterm_transport` only when
the application's finite detail window enables that target. Relay and peer tags
use a fresh process-local keyed hash. Encrypted datagram fingerprints correlate
queueing, writes and receipts across endpoints, independently of batching; they
are diagnostic hints, not protocol identifiers. No address, key, payload or
arbitrary dependency message is forwarded. A successful flush is not proof of
peer delivery. Monotonic timings are local to a process, not synchronized clocks.

An anonymous `relay_dial` span also lets the application's diagnostics adapter
map six exact literal markers from pinned iroh-relay 1.3.0 to TCP, TLS, upgrade
and authentication phases. It ignores all accompanying fields and unknown
messages, and bounds matching storage. Upgrades must verify those source markers
or replace them with a typed upstream observer. Tests cover dynamic enable/disable,
allowlisting, unrelated subscriber isolation and per-dial correlation. Remove
this instrumentation when an equivalent content-free upstream observer exists.

These markers describe the local connector's observations. In particular,
`relay_tcp_ready` may acknowledge a transparent proxy's local TCP leg before
the proxy has connected its outer socket. The HTTP-upgrade marker precedes
the relay protocol's authentication confirmation; measure those waits separately.
The pinned actor's `CONNECT_TIMEOUT` is 10 seconds, so elapsed time near five
seconds cannot establish that this timer fired. A later R02 capture and
outbound-only TUN/physical-interface probes
document these boundaries in
`target/verification/multi-device/checks/ordered-fixes/relay-tls-io/`.

For R03, the actor reports a closed numeric failure category at its existing
retry boundary. Dial failures distinguish the actor deadline, connector timeout,
TCP/DNS/proxy, TLS, upgrade and authentication. Run failures distinguish send and
ping timeouts, local address changes, server close, stream I/O and protocol errors.
Separate phases identify failures before and after the first successful relay
ping. No error text or source address is formatted or forwarded. Tests cover
the phase/category mapping and the application's rejection of unknown codes.
These observations identify the local failure, not its upstream network cause.

This is diagnostic support for R02/R03, not a handshake behavior fix. It changes
no connection, queue, retry or request deadline. Evidence is under
`target/verification/multi-device/checks/ordered-fixes/relay-handshake-fix/`
and `target/verification/multi-device/checks/ordered-fixes/r03-relay-cause/`.
