# Multiplexer shortcut verification

Date: 2026-10-29. Baseline: `ff61d52826c4b3865e84cd9b563b38dbbafaf773`
(voytty 1.0.1). Host: macOS 27.0.1 arm64, Ghostty 1.4.0,
herdr 0.9.5, tmux 3.7c.

## Finding

The CLI decoded focus-in (`CSI I`) and focus-out (`CSI O`) as opaque input.
An opaque event between `Ctrl+]` and `.` cancelled the pending command, consumed
the focus report, and forwarded the period to the child.

The user-operated Ghostty capture from an isolated, real voytty/herdr session
contained repeated `CSI I` reports while focus reporting was enabled. Its
eventual shortcut succeeded: no focus report arrived between its prefix and
period. An earlier raw-key capture likewise contained valid enhanced keys with
about 0.59 seconds between prefix and period, inside the one-second deadline.

Injecting focus reports between those keys reproduced the failure with real
herdr and with tmux configured using `set -g focus-events on`. This establishes
a compatibility defect and explains why output/focus activity can distinguish
a busy session from an idle one. The original failing user session was not
instrumented, so attributing that specific occurrence remains an inference.

The fix gives focus reports their own host-input event. They retain their normal
forwarding and leave the local command deadline unchanged. Unknown key sequences
still cancel the command. Both seven-bit CSI and eight-bit CSI focus encodings
are covered.

## Runtime evidence

The probe used the CLI library's real terminal loop, local IPC, daemon,
SessionService and PTYs. It supplied private `UserPaths`, separate herdr
configuration/state, and a private tmux socket. Tests waited for the inner shell
and presentation to become ready. No existing user session was stopped.

| Scenario | Before | After |
| --- | --- | --- |
| herdr, enhanced prefix/release, focus reports, period | Stayed attached | Exit 0 |
| tmux with focus events, legacy prefix, focus reports, period | Stayed attached | Exit 0 |
| tmux defaults, legacy prefix and period | Exit 0 | Exit 0 |
| herdr, captured Ghostty key sequence and approximately 0.59 s timing | Exit 0 | Exit 0 |
| Double prefix delivered to a raw reader inside herdr/tmux | One literal `0x1d` | One literal `0x1d` |
| herdr `Ctrl+B q` / tmux `Ctrl+B d`, then voytty detach | See note below | Exit 0 |

All eight final runtime scenarios preserved the voytty Session and restored
terminal attributes, excluding macOS's kernel-managed `PENDIN` bit as in the
existing integration fixture. Real herdr requested keyboard flags 7; tmux used
legacy flags 0. The tmux focus case explicitly enabled focus reporting.

During exploratory runs, one herdr-to-shell transition produced
`MalformedFrame`; its diagnostic did not preserve the detailed cause. This is
an unresolved separate observation, not evidence that the focus fix resolves
all screen-transition behavior. Early fixture attempts also sent input before
startup synchronization finished. The final fixture waits for readiness and uses
short temporary paths to stay within macOS's Unix-socket path limit.

## Automated regression

`focus_reports_preserve_pending_command_and_forward_the_original_bytes` failed
on the baseline: it observed no detach and forwarded `.`. After the fix, it
covers legacy/enhanced prefixes, byte-fragmented input, both focus directions,
both CSI encodings, and exact forwarding. A second regression verifies that
focus reports do not extend the one-second deadline. Parameterized `CSI 1 I`
remains an unknown key sequence.

The existing `daemon_autospawn` enhanced-prefix integration scenario now inserts
focus-in and focus-out around the prefix release before the encoded period. It
continues to verify successful detach, terminal restoration and retained
Session/keyboard state.

The desktop status bar now shows the available commands while the prefix is
pending. Renderer coverage includes local/remote views, main/alternate screens,
narrow and wide terminals, unchanged child geometry, and restoration of upload
progress. The real terminal-loop fixture checks legacy/enhanced prefix hints,
unknown-command cancellation and timeout clearing by observing completed screen
updates rather than sleeping for a fixed interval. Mobile rendering is unchanged.

Validation commands:

```sh
cargo test -p voytty-cli --all-features
cargo clippy -p voytty-cli --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
git diff --check
```

This evidence covers local macOS runtime behavior. Remote sessions, other
operating systems, and the original user's installed-client failure after the
fix have not been accepted by these tests.

