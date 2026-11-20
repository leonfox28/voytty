# Diagnostic recording internals

User controls, export, and retention limits are documented in the
[logging guide](../wiki/logging.md). This document records rate limits and
transport sampling behavior for debugging the implementation.

Each process admits at most 100 detail records per one-second window. Packet
traces may use at most 80 of those slots, leaving room for synchronization and
connection stages during bursts. Queue and storage limits still apply.
Relay failures use fixed categories for dial/TLS/authentication, ping/send
timeouts, stream closure and protocol errors. They do not include error text or
network addresses, and describe the local observation rather than its cause.
The mobile Iroh terminal reader also samples ordered stream progress at most
once per second while polled: bytes consumed, complete/queued frames, incomplete
frame bytes, and connection-wide QUIC STREAM-frame counts. Connection ordinals
and stream numbers correlate these counters within one process. Connection-wide
counts include other streams and retransmissions; they do not establish snapshot
delivery. Sampling uses the existing reader wakeup, adds no network probe, and
stops with the same finite detail window. An absent sample can mean the reader
was not polled or a record was suppressed; check loss reports before concluding
that the transport stopped.

Desktop writers hold `writer.lock` while appending or rotating. Its inode stays
in place. A nonzero length marks a pending retention cleanup for every process.
Writers set the marker before a legacy-compatible append and when publishing
or repairing daemon registration. The first eligible write by a storage adapter
checks all four log files, as does a write that finds the marker or an oversized
current file. A successful cleanup clears the marker under the same lock.
Ordinary writes reuse the validated append handle. Rotation replaces the
archive with a rename, so previously captured read handles keep their data.
