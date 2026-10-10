# Local NOQ 1.3.0 corrections

This is the crates.io `noq-proto` 1.3.0 source, licensed under the included
MIT / Apache-2.0 licenses. Upstream repository: <https://github.com/n0-computer/noq>.
Upstream revision: `c1f411562e6078852749b8bcf1190523096a107f` (`noq-proto/`).
Published crate SHA-256:
`7c1e5b6fe668491eca022f745a0a9402585626c73a7b839b3424ace15d6a9c8f`.

The root Cargo patch selects this source for every target, including Android.
The standalone workspace preserves the upstream test dependencies and lockfile.
The registry installation marker and upstream release automation file are omitted.
`just ci-noq` runs the entire suite and is included in the canonical Unix CI profile.

The two idle-timeout corrections are confined to `src/connection/mod.rs` and
`src/connection/packet_builder.rs`. Previously, the first probe on each new
path could reset the entire connection's idle timer, even without receiving
another packet. Opening an unanswered path every 20 seconds kept an offline
connection alive beyond 130 seconds, while its peer had already closed.

Track the connection's send-side reset permission separately from each path's
permission. An authenticated receive permits one subsequent connection-wide
send-side reset, as described by [RFC 9000 section 10.1](https://www.rfc-editor.org/rfc/rfc9000.html#section-10.1).
Per-path idle behavior, negotiated deadlines, keep-alives and retry limits are
unchanged. The regression in `src/tests/multipath.rs` verifies that no datagram
was received during the outage and that unanswered new paths cannot indefinitely
extend the connection.

Run the focused regression and complete upstream unit suite with:

```sh
cargo test --manifest-path vendor/noq-proto/Cargo.toml --locked --lib zterm_new_path_without_peer_packets
cargo test --manifest-path vendor/noq-proto/Cargo.toml --locked --lib
```

Before the first correction the focused regression fails; afterwards all 428 unit
tests pass. Raw diagnostic evidence is retained under
`target/verification/multi-device/checks/noq-idle-*`. This reproduces a transport
defect relevant to the observed delayed disconnects; product-level acceptance
still requires the A/B/C matrix and endurance run.

A second regression establishes a 20-second RTT using delayed QUIC packets
in the library's deterministic simulated network,
opens a 2-millisecond path and abandons the slow path. The abandoned path is
retained for draining and previously kept connection idle at 127.6 seconds,
despite a negotiated 30-second timeout. Connection idle now uses the maximum
PTO of paths that have not been abandoned. Abandonment recomputes that deadline
from the last eligible receive/send, so local path management never restarts
the idle clock. Key-discard, path-draining and connection-close timers continue
to account for all retained paths. Negotiated timeouts and the three-PTO minimum
for usable paths are unchanged.

`zterm_abandoned_slow_path_does_not_extend_connection_idle` checks immediate
abandonment, abandonment 20 seconds into an outage, and a slow path that remains
open. The first two close at 30 seconds; the last still closes at 127.6 seconds.
The isolated candidate passes all 429 unit tests. Evidence is in
`target/verification/multi-device/checks/noq-abandoned-idle-repro/`. This proves
the library defect; it does not establish the path RTTs in a previous device run.

A third correction handles stateless resets on active multipath paths. A new
path's first CID was not registered with the endpoint; recognized resets were
routed as path zero and checked only against the initial transport-parameter
token. An abandoned initial path or a rotated CID could therefore cause a valid
reset to be ignored. Conversely, a reset queued before CID retirement could
still close a connection afterwards.

`src/cid_queue.rs`, `src/endpoint.rs`, and `src/connection/mod.rs` now register
the active CID when a path opens or receives its first CID, retain its path in
the endpoint index, and check current tokens against the sending address.
Rotation, migration, migration rollback and path abandonment retain the same
ownership rules. Tokens for unused CIDs and abandoned paths cannot close the
connection, including when a reset prefix happens to contain a known CID.
Token comparison uses the existing constant-time `ResetToken` equality.
Registration does not emit endpoint events after the connection has drained.

The seven tests in `src/tests/stateless_reset.rs` use real library-generated
packets and a recreated peer endpoint with the same reset key. Before delivering
the reset they verify the inciting packet's path against the original endpoint
and the returned token against the exact destination CID. They cover both
connection roles, discarded path zero, CID rotation, peer address migration,
wrong sources/tokens, queued retired tokens, known-CID prefixes and duplicate
reset events. All seven fail on the previous installed source and pass after
the correction; the complete library suite passes 436 tests. The initial full
suite exposed an event-after-drain regression, fixed before that final pass;
the failing log and proptest failure input are retained.

Evidence: `target/verification/multi-device/checks/ordered-fixes/reset-token-probe/`,
especially `before-04.log`, `registration-only-01.log`, `final-baseline-01.log`,
`permanent-after-04.log` and `noq-full-03.log`. The earlier two inconclusive
probe attempts remain intact. This establishes a library defect, not the cause
of every historical A/B/C timeout. Related upstream report:
[noq #437](https://github.com/n0-computer/noq/issues/437). Protocol requirements:
[RFC 9000 §10.3](https://www.rfc-editor.org/rfc/rfc9000.html#section-10.3) and
[multipath draft §3.4](https://www.ietf.org/archive/id/draft-ietf-quic-multipath-21.html#section-3.4).

Remove these corrections once the selected upstream release includes equivalent
connection-wide idle reset, abandoned-path idle, and active-path stateless reset
behavior, after rerunning the regressions and multi-device recovery cases.
Do not replace them with longer test deadlines.
