# End-to-end test boundary

Use explicit disposable hosts and emulator app storage. Never inject test state
into the user's `~/.voytty`. Relay checks live in `tests/relay/`; runtime evidence
and remaining real-device/network gates live in
[session continuity verification](../../docs/verification/session-continuity.md).

## Three real product devices

Current results, artifact identities, pending fixes, and evidence locations are
recorded in [multi-device verification](../../docs/verification/multi-device.md).

`multi-device.py` creates two Ubuntu 24.04 ARM64 Lima VZ guests (4 CPU, 6 GiB,
60 GiB each) and a new rootable API 36 Google APIs ARM64 emulator (2 CPU,
3 GiB). Install Lima, Python 3.11+, JDK 17, the Android SDK/NDK used by Gradle,
and `system-images;android-36;google_apis;arm64-v8a` first. It needs Internet
access to build and reach official-n0. Linux uses real per-account daemons and
CLI PTYs. Android uses `MultiDeviceTest`'s opt-in private mailbox with the real
Application repository and Activity.

The lab root defaults to `${XDG_STATE_HOME:-~/.local/state}/voytty-verification/multi-device`
and can be overridden with `VOYTTY_MULTI_DEVICE_ROOT`. It holds the environment
record, the emulator's disk, the venv and all run evidence, so it is deliberately
outside `target/` (which `cargo clean` deletes) and outside the source tree that
`build` archives. The commands below use `$LAB` for that directory.

```sh
LAB=${VOYTTY_MULTI_DEVICE_ROOT:-${XDG_STATE_HOME:-$HOME/.local/state}/voytty-verification/multi-device}
python3 -m venv "$LAB/venv"
"$LAB/venv/bin/pip" install -r tests/e2e/requirements-multi-device.txt
"$LAB/venv/bin/python" tests/e2e/multi-device.py create
"$LAB/venv/bin/python" tests/e2e/multi-device.py build
"$LAB/venv/bin/python" tests/e2e/multi-device.py preflight
"$LAB/venv/bin/python" tests/e2e/multi-device.py run --group extended --repeat 1 --soak-seconds 0 --keep-running
"$LAB/venv/bin/python" tests/e2e/multi-device.py run --group core --repeat 10 --soak-seconds 3600 --seed 20261003
```

The default environment is `$LAB/environment/`.
Use another `--environment` and even `--port` for a separate lab. Every adb
invocation uses the recorded serial. `matrix` prints the expanded rows with the
same group, scenario, host, path and controller filters as `run`;
`--scenario cold,takeover --host A --path direct --repeat 1 --soak-seconds 0`
selects a short diagnostic run. `--run-id` names a new evidence directory and
refuses to overwrite one. `soak --soak-seconds 3600` runs endurance separately,
preserving each host's original shell across seeded random transitions.
The endurance clock starts after A's original shell and background counter are
established; preparation time is reported separately and cannot shorten the hour.
Endurance failures retain their logs and keep the whole run failed. After a
read-only check confirms the original Session still exists, the runner may
detach its test clients and explicitly reopen that exact ID to continue the
hour's samples. An ended Session or `operation_outcome_unknown` stops the run;
the recorded duration then remains incomplete. It never recreates a Session
or resends an uncertain mutation to finish the clock.
Fault-injection prerequisites retain `blocked` action status separately from
product failures. Either status keeps endurance acceptance failed; continued
sampling cannot erase the original action. The audit rejects a claimed pass
containing any failed, blocked, or skipped action.
Fixture entry and explicit reopens after an already-recorded endurance failure
may make at most three connection attempts to that same Session ID, alternating
the remote Linux client, Android, then Linux and checking host state between
attempts. The next action uses the device that actually reopened the Session.
Every failed attempt has separate evidence and
keeps the whole endurance run failed. Normal scenario actions and cold-connection
tests do not use this preparation/reentry helper.

For ten repetitions of the connection/recovery/concurrency core and one of each
extended process, lifecycle, authorization and upload case, run:

```sh
"$LAB/venv/bin/python" tests/e2e/multi-device.py run --group extended --repeat 1 --soak-seconds 0 --keep-running
"$LAB/venv/bin/python" tests/e2e/multi-device.py run --group core --repeat 10 --soak-seconds 3600 --seed 20261003
```

`--group all` remains the default. One complete repetition has 234 rows: 146
core and 88 extended. `--controller A|B|C` restricts the original control device.
The run manifest records the expanded matrix; a failed prerequisite cannot count
as exercising the later fault. The Android restart cases begin with a named
session and create a separate `main` session before killing the app or using
`adb reboot`. Automatic restoration or reopening the saved host must select the
original ID. This makes an accidental fallback to the default session observable.
The existing Android test identity begins with background connection enabled and
notification permission granted. Lifecycle cases explicitly vary both settings;
fresh-identity pairing retains product defaults. This baseline is recorded per
run and uses only the app's settings API and the owned emulator's permission.

VZ's `vzNAT` supports host-to-guest access but isolates guests from one another.
The guests therefore also join Lima's unprivileged `user-v2` network. Preflight
checks host/emulator/guest ping and UDP in the directions actually used. Path
assertions read the selected path from the product, never infer it from ping or
from a firewall rule. Relay cases block non-DNS UDP at all three test UIDs, then
start fresh transports; migration cases change the path of a live connection.
Linux `cold` and `first-existing` cases stop the controller daemon, then let
the real CLI launch it. `connect_seconds` includes daemon and endpoint startup;
there is no prior wait for Network Online or peer warm-up query. The manifest
marks this contract as `cli-autostart-daemon-v1`. Earlier diagnostic runs waited
for the local endpoint to be Online and cover a new peer connection, not the
complete CLI/daemon startup sequence. Android cases start a fresh application
process before the first host request, without a peer warm-up query.
Before each independent remote scenario, the host must report `Network Online`
through its local status command within 60 seconds. This prevents a previous
fault's `Degraded` state from leaking into the next row. The check precedes
controller cold startup, never contacts a peer, and is recorded separately as
`host_readiness` under `local-host-online-before-scenario-v1`. Local terminals
do not require network readiness. Fault recovery inside a scenario and actions
within the endurance run retain their original detection/recovery limits.
Linux readiness also waits for the real PTY presentation to open its input
fence: these main-screen shell fixtures show their cursor when ready. A driver
`Active` event alone can precede a deferred resize after reconnect. Continuity
commands are submitted once, within the original 60 second recovery bound.
`--relay-scope host` retains the asymmetric host-only UDP diagnostic variant.
Reboots reapply the affected guest's Relay rules before reopening its client.

The two existing `TerminalUiTest` gesture/pointer tests require an explicit
`hostName` for a paired disposable host. A generic instrumentation run skips
them when that argument is absent; it never selects a personal machine name.
After the three-device runner has paired C with the owned hosts, run them with:

```sh
"$LAB/venv/bin/python" tests/e2e/multi-device-hosts.py \
  --output "$LAB/checks/android-hosts-01.json"
adb -s emulator-5586 shell am instrument -w -r \
  -e class com.voytty.app.TerminalUiTest#gesturesCopyOverlayAndActivityRetention,com.voytty.app.TerminalUiTest#childMouseAndAlternateScrollUseTouchWithoutStealingLongPress \
  -e hostName multi-device-A \
  com.voytty.app.dev.test/androidx.test.runner.AndroidJUnitRunner
```

Use the environment's recorded serial and the exact paired test-host name when
creating a separate lab. Fixture skips are reported separately from passes.
Choose a new evidence filename for each preparation. `start` starts the VMs and
emulator; `preflight` verifies infrastructure connectivity and artifact hashes.
Neither starts a stopped product daemon after a VM reboot. Standalone real-host
instrumentation (including `NativeTerminalTest`) must run the host preparation
above first. It starts each configured A/B daemon with an explicit local read and
checks local `Network Online`, without restarting live hosts, creating Sessions,
or contacting a peer or the Android controller. Keep Android cold until the
instrumentation starts. The matrix already checks host readiness separately;
do not use this helper to pre-start a Linux cold controller inside a matrix row.

Evidence includes the manifest, matrix, per-case result, actual path snapshots,
timings, PTY bytes, Android native/app logs, screenshot, and shell continuity
proof. Session ID, PID, cwd, variable, background counter and execution nonces
must survive recovery. Failed, blocked and skipped rows remain distinct. An
indeterminate mutation is reconciled with read-only host state, never resent
with a different operation ID. Detection/recovery limits are 120/60 seconds.
Android assertions require a live instrumentation process. If it exits, the
runner fails immediately; the collected `android-snapshot.json` retains the
last mailbox value as evidence and does not establish the state after exit.
For cold-connection or synchronization diagnosis, explicitly enable a finite
15-minute detail window on each owned Linux test account before the run:

```sh
limactl shell --workdir=/tmp voytty-md-20261003t024103z-a sudo -Hu voytty-a /usr/local/bin/voytty logs debug on
limactl shell --workdir=/tmp voytty-md-20261003t024103z-b sudo -Hu voytty-b /usr/local/bin/voytty logs debug on
```

Substitute the recorded guest and test-account names for another environment.
This local setting does not dial a peer or prewarm a cold connection. The
runner collects both the ordinary daemon log and the current/rotated detail
logs; Android instrumentation enables its detail window when it starts. Its
window also expires after 15 minutes. During a longer run, explicitly renew all
three devices before expiry (for example, at 12 minutes), using a new evidence
filename each time:

```sh
python3 tests/e2e/multi-device-diagnostics.py --output "$LAB/checks/detail-renew-01.json"
```

The helper requires the recorded VMs/emulator to already be running and the
Android app's diagnostic storage to exist. It calls the local Linux CLI and
atomically writes the same finite Android control schema under the app UID,
using the emulator's clock. It changes no connection state and records each
renewal; `--devices A B` or `--devices C` limits its scope. It never starts an
instance, restarts the application, or overwrites an existing evidence file.
Product diagnostics never renew the window themselves. Compare elapsed stages within
the same operation: clocks across devices are not calibrated. TLS handshake
data arriving is distinct from full authentication, and `snapshot_sent` means
the stream was flushed, not that the client applied or acknowledged it.

Renew explicitly before each diagnostic run as well as during long runs: a
rebuild/install does not renew Linux's window. Missing detail from an earlier
case cannot be reconstructed from an older process's rotated log.

R02 relay traces use a closed numeric vocabulary during that same finite
window. They separate relay dialing, TCP/TLS/upgrade/authentication, datagram
queueing, writes, flushes, receipts, delivery to QUIC, expiration and explicit
rate-limit reports. Relay/peer tags are anonymous and process-local; encrypted
datagram fingerprints can correlate both ends despite rebatching. They do not
identify application frames, prove peer delivery from a flush, synchronize
device clocks, or establish rate limiting from a slow connection. The adapter
does not export dependency messages, addresses, identities or payloads. See
`vendor/iroh/VOYTTY.md` for pinned dependency markers and upgrade requirements.

`relay_tcp_ready` describes the local socket. A transparent TCP proxy can
acknowledge it while its own outward connection is still `SYN_SENT`; this was
observed through the operator's TUN during R02 diagnosis. `relay_dial_failed`
includes the closed `transport_failure` category in current candidates; older
records omitted it. The pinned relay actor's dial timeout is 10 seconds,
separate from the current product's 15-second initial-request budget and
5-second ready-operation budget. A dial failure near 5 seconds must not be
labelled as that relay actor timer expiring.
Correlate the same failed process with bounded TCP header and socket-queue
observations before assigning a cause. Header-only capture cannot decrypt a
TLS/auth message or distinguish the proxy's TCP acknowledgement from server
receipt. Also separate discovery/health probe sockets from actual relay dials.
Current Android detail also has `stream_observed` samples, limited to one per
second per reader while polled. Compare ordered read bytes, decoder progress
and connection-wide QUIC counters within one process/connection/stream. These
samples contain no frame data, add no probe and retain the finite detail window;
connection-wide STREAM counts include other streams and retransmissions.

Private command failures retain only an allowlisted public error code when
available; ticket text, raw private output, and error details remain withheld.
Concurrent public commands reserve separate log names before execution and
serialize journal appends, so simultaneous completions cannot replace each
other's evidence.
Fresh-identity pairing records the host's authorization list before cleanup,
the newly observed principals, and the post-revocation result in
`outcome-reconciliation.json`. A Session list cannot reconcile pairing.
Diagnostic collection errors cannot prevent the authorization query/revocation
or replace the original pairing error. The manifest marks this contract as
`host-authorization-before-revoke-v1`; the audit rejects missing observations
or surviving test grants for successful and indeterminate pairing operations.
If pairing succeeded but the subsequent Session operation is indeterminate,
`session-outcome-reconciliation.json` separately records a read-only host Session
query and preserves the authorization evidence. The manifest/audit mark this as
`host-session-query-after-accepted-pair-v1`; an authorization list alone cannot
establish that later mutation's result.
If a fresh acceptance does not return successfully, cleanup first stops the
disposable host and confirms that its test UID has no daemon process. This
removes the old in-memory offer and joins possible late authorization commits
before querying/revoking grants. `pair-attempt.json` and
`pair-cleanup-barrier.json` retain this sequence; the manifest/audit mark it as
`failed-accept-host-process-exit-v1`. The original operation remains failed and
its timeout is unchanged. A failed barrier is recorded as incomplete cleanup
alongside the original pairing error, stops the matrix, and leaves remaining
rows skipped; an empty authorization query cannot replace confirmed host exit.
After an indeterminate Session mutation, a matrix row keeps its original error,
read-only query and diagnostics, then stops its disposable host and confirms the
old daemon processes have exited. An empty Session list alone cannot prevent a
late creation or takeover from affecting the next independent row.
`session-cleanup-barrier.json` and `unknown-session-host-process-exit-v1` record
this cleanup; an unconfirmed exit stops the matrix and leaves remaining rows
skipped. This does not retry the mutation, change product deadlines, or apply
to the continuous original-session soak.
Revocation uses the controller's own public device ID and records it in
`revocation-principal.json`; display names and unrelated old grants are not
identity evidence. The Android coordinator reads that public ID locally from
the already initialized native runtime without making a peer request. Rebuild
the instrumentation APK when using this runner; an older APK that omits the ID
blocks a revocation case instead of guessing its principal.
Handoff detection and host release share one deadline from fault injection;
slow observations cannot return success after it expires. The manifest marks
this timing contract, and the auditor checks detection, host release and old
controller retirement measurements against their bounds.
Offline faults drop both test-UID OUTPUT and INPUT to that UID's currently
owned TCP/UDP socket ports, including IPv4-mapped IPv6 sockets. SSH, adb and
loopback remain reachable. Intent is journaled before mutation; an independent
EOF guard restores rules if the coordinator dies. Only uniquely named test
chains are removed. The environment's `fault-evidence/` retains each chain's
IPv4/IPv6 counters before removal, also copied into the affected case and
linked from its metrics. Quiet-shell short outages first arm eight seconds of
real PTY output, then request a distinct controller viewport while the fault is
active. Dropped incoming Relay packets do not themselves guarantee outgoing
acknowledgements. The resize avoids shell input during the outage and is checked
on the original Session after recovery, within the same 60-second limit.
For Android, the displayed viewport must also match, with `Active` and input
ready, before the runner sends the postfault continuity probe. The manifest/audit
mark these checks as `shell-output-and-controller-resize-v1` and
`host-and-android-surface-v1`; older evidence retains its original contract.
Both ingress and egress must show dropped packets; otherwise
the injection is blocked, never passed. Manifests identify this scope as
`owned-sockets-bidirectional`; older runs used egress-only faults and remain
supplemental evidence for that narrower condition.
`--offline-scope egress` reproduces those one-way-loss conditions for targeted
regression checks; it is recorded separately and does not replace the default
bidirectional acceptance matrix.
Android host-open failures are preserved before
any subsequent session selection, so selection cannot hide a failed cold open.
When opening a host already attaches the requested Session, the coordinator
retains its active, synchronizing or reconnecting frame, matching the session
panel's current-row behavior. It does not detach and select that same ID again.
An explicitly requested takeover may continue after a known `session_occupied`
response. Concurrent races wait for both requests to settle before checking the
stable winner; a competitor that is still opening is not a retired controller.
Normal CLI detach also records the exit code and whether cleanup needed a
signal. A forced cleanup or nonzero exit cannot pass a normal-detach assertion;
cleanup still releases the owned PTY after the failure.

The runner stops all three instances by default, retaining their disks and app
data. `--keep-running` is useful between diagnostic runs. `start`, `collect`,
`cleanup` and `stop` are explicit entry points. Faults affect only test UIDs in
both IP families; a pipe-watching child restores rules even if the coordinator
is killed. Durable journals let `cleanup` recover interrupted identity rotation
or firewall cleanup. Fresh-pair scenarios retain the original and tested private
identity directories inside their disposable device, then restore the original.
Tickets only cross private files or stdin and are excluded from evidence.

Inside a disposable Linux guest, `sh tests/e2e/handshake-delay.sh 10` exercises
reverse authorization confirmation with 60 ms loopback delay. This is a separate
transport regression, not part of the official-n0 three-device matrix. Run
`"$LAB/venv/bin/python" tests/e2e/multi_device_test.py`
for coordinator-only regressions. After a matrix finishes, run
`python3 tests/e2e/multi-device-audit.py RUN_ID` to audit coverage, path evidence,
timing bounds, original-shell continuity, unique command execution and unknown
operation reconciliation. It writes `acceptance-audit.json` and returns nonzero
for inconsistent evidence; product failures remain distinct from audit errors. See
[the acceptance record](../../docs/verification/multi-device.md) for measured
results and remaining gates.

The audit requires one hour of stability by default. When a different duration
was agreed before the run, pass it explicitly, for example
`python3 tests/e2e/multi-device-audit.py --soak-seconds 600 RUN_ID` for a ten-minute
run. The requirement must match the manifest's `soak_seconds`; original-session,
both-host, unique-execution and failure checks still apply. The report records
the required duration separately from `completed_60_minutes`, so a shorter run
cannot become one-hour evidence. Offline audit regressions run in `just ci-policy`.

The separate privileged `iroh_network_gate` normally runs in its Docker image
with `eth0` as public egress. When running that test directly inside an owned
Lima guest, set `VOYTTY_GATE_EGRESS_INTERFACE` to the actual default-route device
(`lima0` in this lab), and retain the existing public Relay IPv4 overrides from
`tests/foundation/network-gate.sh`. Restore the guest's IPv4 forwarding setting
afterwards. This namespace-level test remains separate from three-device product
acceptance.

## Android background connection and process recovery

Build `voytty-cli` example `presentation_fixture` and run `init <new-root>` then
`start <root>` in a separate process. All host state and Sessions belong to that
root. Wait for `cli <root> status` to report network Online before `pair <root>`.
The pair action writes a private ticket file; don't print or commit its contents.

Use a disposable emulator or a read-only emulator instance, install the debug
APK and instrumentation APK, and copy that ticket via `adb shell run-as` into
`files/background-ticket.txt` of `com.voytty.app.dev`. Regenerate a fresh
ticket before each test that pairs, removing only the prior fixture ticket file.
Select an explicit emulator serial for every adb command.

Run the instrumentation class
`com.voytty.app.BackgroundConnectionTest` with `-e backgroundFixture 1`:

- `#connectionSurvivesActivityAndServiceLifecycle` tests a real Session, foreground
  start eligibility, notification grant/denial, lock/unlock, Activity recreation,
  Chinese IME API commits, notification detach, lost control and Session end.
  Configure POST_NOTIFICATIONS with `pm grant` / `pm revoke` **before** starting
  instrumentation and pass `-e notificationVisible 1` / `0` respectively. Android
  kills a running process on revocation, so changing permission inside its test
  runner is not a service-start failure. After initial pairing, repeated runs can
  use `-e reuseBackgroundHost 1` with the saved `presentation-fixture` host.
- `#processRecoveryNeverCreatesOrSubstitutesSession` is coordinated across separate
  process invocations. First use `-e restoreMode prepare` with a fresh ticket.
  Force-stop the application; confirm that no service restarts by itself.
  Run `resume`, then force-stop and run `occupied`. Both retain the original ID.
  On the disposable host explicitly close that Session (`cli <root> session close
  main -y` on a fresh fixture), force-stop the app, and run `ended`. The latter
  must show Session-not-found on both restoration and Retry, with no new Session.

`NativeTerminalTest -e hostName presentation-fixture` can then verify Unicode,
resize, history and detach against the same disposable host. Run normal UI/input
and store tests separately; tests that require additional fixtures report skips.

For transport interruption, use an emulator that supports `adb root`, then run:

```sh
python3 tests/e2e/android-network-interruption.py \
  --serial emulator-5580 --output /tmp/voytty-network-interruption.log
```

Choose the actual disposable emulator serial; `--adb` can select the SDK's adb.
The coordinator waits for established output before dropping only the debug
app UID's IPv4/IPv6 traffic, and removes its uniquely tagged rules in `finally`.
The test waits for `reconnecting`, confirms input is rejected, then confirms
the original Session and shell variable survive restoration without replaying
the rejected input. Read-only reachability retries prepare this fixture; it is
not a first-connection latency test. No mutation is retried with a new ID.

Stop the disposable daemon with `cli <root> daemon stop -y` after acceptance.
This proves emulator/temporary-host behavior, not actual login, a phone's power
policy, cellular handover or signed release upgrades.
