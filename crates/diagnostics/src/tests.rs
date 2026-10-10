use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Memory {
    records: Mutex<Vec<(bool, Vec<u8>)>>,
    control: Mutex<Control>,
    fail: AtomicBool,
}
impl Sink for Memory {
    fn append(&self, detail: bool, bytes: &[u8]) -> std::io::Result<()> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(std::io::ErrorKind::StorageFull.into());
        }
        self.records
            .lock()
            .expect("memory sink")
            .push((detail, bytes.into()));
        Ok(())
    }
    fn control(&self) -> std::io::Result<Control> {
        Ok(*self.control.lock().expect("control"))
    }
}
fn wait(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < deadline, "worker deadline");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn stream_progress_export_accepts_only_numeric_fields() {
    let progress = StreamProgress {
        stream_id: 4,
        read_bytes: 1026,
        decoded_frames: 7,
        queued_frames: 2,
        buffered_bytes: 1026,
        connection_stream_frames: 29,
    };
    let record = Record::new(
        Event::new(Kind::StreamObserved)
            .level(Level::Debug)
            .connection(11)
            .stream_progress(progress),
        "abc-123",
        1,
    );
    let encoded = record.encode().expect("stream progress schema");
    assert_eq!(
        Record::decode(&encoded)
            .expect("stream progress schema")
            .fields
            .stream_progress,
        Some(progress)
    );
    let mut injected = serde_json::to_value(&record).expect("stream progress schema");
    injected["fields"]["stream_progress"]["payload"] = serde_json::json!("PRIVATE_FRAME_TEXT");
    assert!(
        Record::decode(&serde_json::to_vec(&injected).expect("stream progress schema")).is_none()
    );
    injected["fields"]["stream_progress"]
        .as_object_mut()
        .expect("stream progress schema")
        .remove("payload");
    injected["fields"]["stream_progress"]["read_bytes"] = serde_json::json!("PRIVATE_FRAME_TEXT");
    assert!(
        Record::decode(&serde_json::to_vec(&injected).expect("stream progress schema")).is_none()
    );
}

#[test]
fn transport_trace_is_numeric_allowlisted_and_dynamically_gated() {
    use tracing_subscriber::prelude::*;
    struct OwnerEvents(Arc<AtomicBool>);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for OwnerEvents {
        fn on_event(&self, _: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
            self.0.store(true, Ordering::Release);
        }
    }
    let owner_observed = Arc::new(AtomicBool::new(false));
    let sink = Arc::new(Memory::default());
    let recorder = Recorder::new(sink.clone()).expect("recorder");
    let subscriber = tracing_subscriber::registry()
        .with(OwnerEvents(owner_observed.clone()).with_filter(
            tracing_subscriber::filter::filter_fn(|metadata| metadata.target() == "test_owner"),
        ))
        .with(transport_trace_layer(recorder.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);
    assert!(!tracing::enabled!(target: "unrelated_dependency", tracing::Level::TRACE));
    let emit = || {
        let enabled = tracing::enabled!(target: "iroh::zterm_transport", tracing::Level::DEBUG);
        tracing::debug!(target: "iroh::zterm_transport", phase = 4u64, relay = 17u64,
            peer = 23u64, packet = 31u64, bytes = 1200u64, age_ms = 12u64,
            elapsed_ms = 37u64, kind = 2u64,
            address = "PRIVATE_ADDRESS", payload = ?b"PRIVATE_PAYLOAD", "PRIVATE_MESSAGE");
        enabled
    };
    assert!(!emit(), "disabled detail must not enable transport hashing");
    *sink.control.lock().expect("control") = Control::enabled(now_ms());
    wait(|| recorder.detail_enabled());
    assert!(emit(), "enabling detail must revisit the same callsite");
    assert!(!tracing::enabled!(target: "unrelated_dependency", tracing::Level::TRACE));
    tracing::debug!(target: "iroh::zterm_transport", phase = 999u64, message = "PRIVATE_UNKNOWN");
    tracing::debug!(target: "iroh::zterm_transport", phase = "4", message = "PRIVATE_TEXT_PHASE");
    tracing::debug!(target: "iroh::untrusted", phase = 4u64, message = "PRIVATE_DEPENDENCY");
    // Dependency text is reduced to a closed phase only inside an anonymous
    // dial span; unrelated events and every address/error field stay private.
    tracing::trace!(target: "iroh_relay::client::tls", "Starting TLS handshake");
    let dial = tracing::debug_span!(target: "iroh::zterm_transport", "relay_dial",
        relay = 41u64, elapsed_ms = 53u64, address = "PRIVATE_SPAN_ADDRESS");
    {
        let _entered = dial.enter();
        tracing::trace!(target: "iroh_relay::client::tls", address = "PRIVATE_ADDRESS", "connecting TCP stream");
        tracing::trace!(target: "iroh_relay::client::tls", "TCP stream connected");
        tracing::trace!(target: "iroh_relay::client::tls", "Starting TLS handshake");
        tracing::trace!(target: "iroh_relay::client::tls", "tls_connector connect success");
        tracing::trace!(target: "iroh_relay::client::conn", "server_handshake: started");
        tracing::trace!(target: "iroh_relay::client::conn", "server_handshake: done");
        tracing::trace!(target: "iroh_relay::client::conn", "PRIVATE_UNKNOWN_MESSAGE");
        tracing::trace!(target: "iroh_relay::client::conn", "{}", "PRIVATE_".repeat(100));
        tracing::trace!(target: "iroh_relay::client::conn", "Starting TLS handshake");
        let other = tracing::debug_span!(target: "iroh::zterm_transport", "relay_dial",
            relay = 67u64, elapsed_ms = 71u64);
        let _other = other.enter();
        tracing::trace!(target: "iroh_relay::client::tls", "Starting TLS handshake");
    }
    assert!(recorder.flush(Duration::from_secs(2)));
    *sink.control.lock().expect("control") = Control::disabled();
    wait(|| !recorder.detail_enabled());
    assert!(!emit());
    tracing::info!(target: "test_owner", "owner event remains enabled");
    assert!(owner_observed.load(Ordering::Acquire));
    assert!(!tracing::enabled!(target: "iroh_relay::client::tls", tracing::Level::TRACE));
    {
        let _entered = dial.enter();
        tracing::trace!(target: "iroh_relay::client::tls", "Starting TLS handshake");
    }
    assert!(recorder.flush(Duration::from_secs(2)));
    let records = sink.records.lock().expect("records");
    let traces: Vec<_> = records
        .iter()
        .filter_map(|(detail, bytes)| {
            let record = Record::decode(bytes).expect("safe record");
            assert!(!String::from_utf8_lossy(bytes).contains("PRIVATE_"));
            (record.event == Kind::TransportTrace).then_some((*detail, record))
        })
        .collect();
    assert_eq!(traces.len(), 8);
    assert!(traces[0].0);
    let fields = &traces[0].1.fields;
    assert_eq!(fields.transport_phase, Some(TransportPhase::RelayEnqueued));
    assert_eq!(fields.transport_peer_tag, Some(23));
    assert_eq!(fields.transport_relay_tag, Some(17));
    assert_eq!(fields.transport_packet_tag, Some(31));
    assert_eq!(fields.transport_packet_kind, Some(2));
    assert_eq!(fields.transport_elapsed_ms, Some(37));
    assert_eq!(fields.transport_queue_ms, Some(12));
    assert_eq!(fields.bytes, Some(1200));
    let mut invalid = serde_json::to_value(&traces[0].1).expect("record");
    invalid["fields"]["transport_packet_kind"] = serde_json::json!(99);
    assert!(Record::decode(&serde_json::to_vec(&invalid).expect("json")).is_none());
    let phases: Vec<_> = traces[1..]
        .iter()
        .map(|(_, r)| r.fields.transport_phase)
        .collect();
    assert_eq!(
        phases,
        [
            Some(TransportPhase::RelayTcpConnecting),
            Some(TransportPhase::RelayTcpReady),
            Some(TransportPhase::RelayTlsStarted),
            Some(TransportPhase::RelayTlsReady),
            Some(TransportPhase::RelayUpgradeReady),
            Some(TransportPhase::RelayAuthReady),
            Some(TransportPhase::RelayTlsStarted),
        ]
    );
    assert!(traces[1..7].iter().all(|(detail, r)| *detail
        && r.fields.transport_relay_tag == Some(41)
        && r.fields.transport_elapsed_ms.is_some_and(|ms| ms >= 53)));
    assert_eq!(traces[7].1.fields.transport_relay_tag, Some(67));
}
#[test]
fn relay_failures_keep_only_known_categories() {
    use tracing_subscriber::prelude::*;
    let sink = Arc::new(Memory::default());
    *sink.control.lock().expect("control") = Control::enabled(now_ms());
    let recorder = Recorder::new(sink.clone()).expect("recorder");
    wait(|| recorder.detail_enabled());
    let subscriber = tracing_subscriber::registry().with(transport_trace_layer(recorder.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);
    for (phase, failure) in [(12u64, 11u64), (3, 6), (11, 14), (12, 999)] {
        tracing::debug!(target: "iroh::zterm_transport", phase, failure, relay = 17u64,
            error = "PRIVATE_REMOTE_MESSAGE", address = "PRIVATE_ADDRESS");
    }
    assert!(recorder.flush(Duration::from_secs(1)));
    let records = sink.records.lock().expect("records");
    let traces: Vec<_> = records
        .iter()
        .filter_map(|(_, bytes)| {
            assert!(!String::from_utf8_lossy(bytes).contains("PRIVATE_"));
            let record = Record::decode(bytes).expect("valid record");
            (record.event == Kind::TransportTrace).then_some(record)
        })
        .collect();
    assert_eq!(traces.len(), 4);
    assert_eq!(
        traces
            .iter()
            .map(|r| (r.fields.transport_phase, r.fields.transport_failure))
            .collect::<Vec<_>>(),
        [
            (
                Some(TransportPhase::RelayDisconnected),
                Some(TransportFailure::PingTimeout)
            ),
            (
                Some(TransportPhase::RelayDialFailed),
                Some(TransportFailure::Tls)
            ),
            (
                Some(TransportPhase::RelayUnconfirmedLost),
                Some(TransportFailure::ServerClosed)
            ),
            (Some(TransportPhase::RelayDisconnected), None),
        ]
    );
    let mut forged = serde_json::to_value(&traces[0]).expect("json");
    forged["fields"]["transport_failure"] = serde_json::json!("PRIVATE_REMOTE_MESSAGE");
    assert!(Record::decode(&serde_json::to_vec(&forged).expect("json")).is_none());
}

#[test]
fn finite_control_rejects_rollback_malformed_and_expired_intervals() {
    let now = 1_000_000;
    let control = Control::enabled(now);
    assert_eq!(control.remaining_ms(now), 900_000);
    assert_eq!(control.remaining_ms(now - 1), 0);
    assert_eq!(control.remaining_ms(now + 900_000), 0);
    assert_eq!(
        Control {
            deadline_ms: i64::MAX,
            ..control
        }
        .remaining_ms(now),
        0
    );
    assert_eq!(
        Control {
            schema: 99,
            ..control
        }
        .remaining_ms(now),
        0
    );
    assert_eq!(Control::disabled().remaining_ms(now), 0);
}
#[test]
fn worker_routes_detail_only_during_explicit_interval_and_preserves_safe_correlation() {
    let sink = Arc::new(Memory::default());
    let recorder = Recorder::new(sink.clone()).expect("worker");
    let operation = Operation::default();
    recorder.record(Event::new(Kind::ConnectionStarted).operation(&operation));
    recorder.record(Event::new(Kind::SyncChanged).level(Level::Debug));
    assert!(recorder.flush(Duration::from_secs(1)));
    assert!(
        sink.records
            .lock()
            .expect("records")
            .iter()
            .all(|(detail, _)| !detail)
    );
    *sink.control.lock().expect("control") = Control::enabled(now_ms());
    wait(|| recorder.detail_enabled());
    recorder.record(
        Event::new(Kind::SyncChanged)
            .level(Level::Debug)
            .operation(&operation),
    );
    recorder.record(
        Event::new(Kind::ConnectionFailed)
            .level(Level::Warn)
            .error(zterm_core::DomainErrorKind::Unauthorized)
            .operation(&operation),
    );
    assert!(recorder.flush(Duration::from_secs(1)));
    let records = sink.records.lock().expect("records");
    assert_eq!(records.iter().filter(|(detail, _)| *detail).count(), 1);
    let events: Vec<_> = records
        .iter()
        .map(|(_, bytes)| Record::decode(bytes).expect("known record"))
        .collect();
    let failed = events
        .iter()
        .find(|r| r.event == Kind::ConnectionFailed)
        .expect("outcome");
    assert_eq!(failed.fields.operation_id, Some(operation.id()));
    assert_eq!(failed.fields.category.as_deref(), Some("unauthorized"));
    assert!(failed.fields.elapsed_ms.is_some());
    drop(records);
    *sink.control.lock().expect("control") = Control::enabled(now_ms() - 900_001);
    wait(|| !recorder.detail_enabled());
    recorder.record(Event::new(Kind::SyncChanged).level(Level::Debug));
    assert!(recorder.flush(Duration::from_secs(1)));
    assert_eq!(
        sink.records
            .lock()
            .expect("records")
            .iter()
            .filter(|(detail, _)| *detail)
            .count(),
        1
    );
}
#[test]
fn snapshot_stages_keep_order_without_per_session_coalescing() {
    let sink = Arc::new(Memory::default());
    *sink.control.lock().expect("control") = Control::enabled(now_ms());
    let recorder = Recorder::new(sink.clone()).expect("worker");
    wait(|| recorder.detail_enabled());
    let session = zterm_core::SessionId::from_array([0x46; 16]);
    let stages = [
        Stage::Snapshot,
        Stage::SnapshotSent,
        Stage::SnapshotAcknowledged,
    ];
    for revision in 1..=2 {
        for stage in stages {
            assert!(
                recorder.record(
                    Event::new(Kind::SyncChanged)
                        .level(Level::Debug)
                        .session(session)
                        .stage(stage)
                        .epoch(revision),
                ),
                "each snapshot stage remains observable below the global detail bound"
            );
        }
    }
    assert!(recorder.flush(Duration::from_secs(1)));
    let observed: Vec<_> = sink
        .records
        .lock()
        .expect("records")
        .iter()
        .filter_map(|(_, bytes)| Record::decode(bytes))
        .filter(|event| event.event == Kind::SyncChanged)
        .map(|event| (event.fields.epoch, event.fields.stage))
        .collect();
    let expected: Vec<_> = (1..=2)
        .flat_map(|revision| stages.map(|stage| (Some(revision), Some(stage))))
        .collect();
    assert_eq!(observed, expected);
    // Ordinary repeated sync warnings retain their existing summary policy.
    assert!(
        recorder.record(
            Event::new(Kind::SyncChanged)
                .level(Level::Debug)
                .session(session)
        )
    );
    assert!(
        !recorder.record(
            Event::new(Kind::SyncChanged)
                .level(Level::Debug)
                .session(session)
        )
    );
}

#[test]
fn import_rejects_payload_fields_forged_messages_unknown_codes_and_oversize() {
    let record = Record::new(Event::new(Kind::UploadFailed), "123-ab", 1);
    let mut json = serde_json::to_value(&record).expect("record");
    json["fields"]["filename"] = "SECRET_SENTINEL".into();
    assert!(Record::decode(&serde_json::to_vec(&json).expect("json")).is_none());
    let mut json = serde_json::to_value(&record).expect("record");
    json["message"] = "TERMINAL_SENTINEL".into();
    assert!(Record::decode(&serde_json::to_vec(&json).expect("json")).is_none());
    let mut json = serde_json::to_value(&record).expect("record");
    json["fields"]["category"] = "CREDENTIAL_SENTINEL".into();
    assert!(Record::decode(&serde_json::to_vec(&json).expect("json")).is_none());
    assert!(Record::decode(&vec![b'x'; MAX_RECORD_BYTES + 1]).is_none());
}
#[test]
fn sink_failure_does_not_block_producers_and_loss_is_reported_after_recovery() {
    let sink = Arc::new(Memory::default());
    sink.fail.store(true, Ordering::Relaxed);
    let recorder = Recorder::new(sink.clone()).expect("worker");
    recorder.record(Event::new(Kind::AppStarted));
    assert!(!recorder.flush(Duration::from_secs(1)));
    assert!(recorder.lost() > 0);
    sink.fail.store(false, Ordering::Relaxed);
    recorder.record(Event::new(Kind::NativeInitialized));
    assert!(recorder.flush(Duration::from_secs(1)));
    assert!(
        sink.records
            .lock()
            .expect("records")
            .iter()
            .any(|(_, bytes)| Record::decode(bytes).is_some_and(
                |r| r.event == Kind::RecordsLost && r.fields.count.is_some_and(|n| n > 0)
            ))
    );
}

#[test]
fn blocked_sink_keeps_producers_bounded_and_detail_expires_without_worker_polling() {
    struct Blocked {
        memory: Memory,
        entered: AtomicBool,
        released: Mutex<bool>,
        wake: std::sync::Condvar,
    }
    impl Sink for Blocked {
        fn append(&self, detail: bool, bytes: &[u8]) -> std::io::Result<()> {
            self.entered.store(true, Ordering::Release);
            let guard = self.released.lock().expect("gate");
            let _guard = self
                .wake
                .wait_while(guard, |released| !*released)
                .expect("gate");
            self.memory.append(detail, bytes)
        }
        fn control(&self) -> std::io::Result<Control> {
            self.memory.control()
        }
    }
    let sink = Arc::new(Blocked {
        memory: Memory::default(),
        entered: AtomicBool::new(false),
        released: Mutex::new(false),
        wake: std::sync::Condvar::new(),
    });
    *sink.memory.control.lock().expect("interval") = Control::enabled(now_ms() - 898_800);
    let recorder = Recorder::new(sink.clone()).expect("recorder");
    wait(|| sink.entered.load(Ordering::Acquire));
    let start = Instant::now();
    for _ in 0..QUEUE_RECORDS {
        assert!(recorder.record(Event::new(Kind::ConnectionStarted)));
    }
    assert!(
        !recorder.record(Event::new(Kind::ConnectionStarted)),
        "entry bound"
    );
    assert!(
        recorder.record(Event::new(Kind::ViewportChanged).level(Level::Debug)),
        "independent detail capacity"
    );
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "producers never wait for sink"
    );
    assert!(
        !recorder.flush(Duration::from_millis(20)),
        "flush has a deadline"
    );
    wait(|| !recorder.detail_enabled());
    assert!(
        !recorder.record(Event::new(Kind::RouteChanged).level(Level::Debug)),
        "expiry does not depend on a writable sink"
    );
    *sink.released.lock().expect("release") = true;
    sink.wake.notify_all();
    assert!(recorder.flush(Duration::from_secs(2)));
    let records = sink.memory.records.lock().expect("records");
    assert_eq!(
        records
            .iter()
            .filter(|(_, bytes)| Record::decode(bytes)
                .is_some_and(|r| r.event == Kind::ConnectionStarted))
            .count(),
        QUEUE_RECORDS
    );
    assert!(
        records
            .iter()
            .any(|(_, bytes)| Record::decode(bytes).is_some_and(|r| r.event == Kind::RecordsLost))
    );
}

#[test]
fn merged_tail_orders_rfc3339_fractional_seconds_by_time_not_text() {
    use std::io::Write;
    let mut snapshots = Vec::new();
    for (detail, timestamps) in [
        (false, ["2026-09-27T00:00:00.9Z", "2026-09-27T00:00:00.01Z"]),
        (true, ["2026-09-27T00:00:00.11Z", "2026-09-27T00:00:00Z"]),
    ] {
        let mut file = tempfile::tempfile().expect("snapshot");
        for timestamp in timestamps {
            let mut record = Record::new(Event::new(Kind::ConnectionStarted), "abc-123", 1);
            record.timestamp = timestamp.into();
            file.write_all(&record.encode().expect("known record"))
                .expect("record");
        }
        snapshots.push(Snapshot {
            length: file.metadata().expect("length").len(),
            file,
            start: 0,
            detail,
        });
    }
    let result = tail(
        &mut snapshots,
        &Filter {
            lines: 4,
            include_debug: true,
            json: true,
            ..Default::default()
        },
    )
    .expect("merged tail");
    let timestamps: Vec<_> = result
        .lines
        .iter()
        .map(|line| Record::decode(line.as_bytes()).expect("record").timestamp)
        .collect();
    assert_eq!(
        timestamps,
        [
            "2026-09-27T00:00:00Z",
            "2026-09-27T00:00:00.01Z",
            "2026-09-27T00:00:00.11Z",
            "2026-09-27T00:00:00.9Z"
        ]
    );
}
