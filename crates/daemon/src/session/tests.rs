#[cfg(unix)]
mod color_tests;

use super::*;

#[cfg(unix)]
use voytty_platform::pty::ExplicitPtyCommand;

#[test]
fn invalid_working_directory_error_redacts_the_host_path() {
    let secret_path = "/private/host/project/secret-cwd";
    let error = map_pty_error(PtyError::InvalidPath {
        kind: PtyPathKind::WorkingDirectory,
        path: secret_path.into(),
        issue: voytty_platform::pty::PtyPathIssue::NotFound,
    });

    assert_eq!(error.kind(), DomainErrorKind::InvalidWorkingDirectory);
    assert_eq!(
        error.detail(),
        "working directory is invalid or inaccessible"
    );
    assert!(!error.to_string().contains(secret_path));
}

#[test]
fn session_summary_debug_redacts_working_directory_and_keeps_metadata() {
    let cwd_sentinel = "/private/tmp/DAEMON_CWD_SENTINEL_305e/project";
    let summary = SessionSummary {
        session_id: SessionId::from_array([0x5a; SessionId::LENGTH]),
        name: SessionName::new("debug-safe-session").expect("valid session name"),
        revision: Revision::new(59),
        has_controller: true,
        working_directory: cwd_sentinel.into(),
        viewport: TerminalSize::new(47, 163),
    };
    let fingerprint = OperationFingerprint::Create {
        base_colors: Default::default(),

        name: summary.name.clone(),
        working_directory: Some(PathBuf::from(cwd_sentinel)),
        viewport: Some(summary.viewport),
    };

    let rendered = format!("{summary:?} {fingerprint:?}");
    assert!(!rendered.contains(cwd_sentinel));
    assert!(rendered.contains("[REDACTED]"));
    assert!(rendered.contains("debug-safe-session"));
    assert!(rendered.contains("has_controller: true"));
    assert!(rendered.contains("rows: 47"));
    assert!(rendered.contains("columns: 163"));
    assert!(rendered.contains("working_directory_present: true"));
    assert_eq!(summary.working_directory, PathBuf::from(cwd_sentinel));
    assert_eq!(summary, summary.clone());
    assert_eq!(fingerprint, fingerprint.clone());
    assert_ne!(
        fingerprint,
        OperationFingerprint::Create {
            base_colors: Default::default(),

            name: summary.name.clone(),
            working_directory: Some(PathBuf::from("/private/tmp/a-different-cwd")),
            viewport: Some(summary.viewport),
        },
        "redacted Debug must not change cwd-sensitive replay equality"
    );
}

#[test]
fn viewport_and_replay_identity_are_bounded() {
    let limits = ResourceLimits::default();
    assert!(validate_viewport(limits, TerminalSize::new(80, 240)).is_ok());
    assert_eq!(
        validate_viewport(limits, TerminalSize::new(81, 240))
            .expect_err("oversized viewport")
            .kind(),
        DomainErrorKind::ResourceExhausted
    );
    let device = DeviceId::from_array([7; 32]);
    let first = ReplayKey::new(
        AttachmentPrincipal::LocalSameUid {
            own_device_id: device,
            local_view_id: AttachmentId::from_array([1; 16]),
        },
        3,
    );
    let second = ReplayKey::new(
        AttachmentPrincipal::LocalSameUid {
            own_device_id: device,
            local_view_id: AttachmentId::from_array([2; 16]),
        },
        3,
    );
    assert_eq!(first, second, "local views share one stable device epoch");
}

#[test]
fn replay_result_window_is_exact_and_recovers_after_oldest_completion() {
    let epoch = ReplayEpoch::default();
    let session_id = SessionId::from_array([0x31; SessionId::LENGTH]);
    let mut in_flight = Vec::with_capacity(OPERATION_RESULTS_PER_EPOCH);
    for sequence in 1..=OPERATION_RESULTS_PER_EPOCH {
        let sequence = u64::try_from(sequence).expect("result-window sequence fits u64");
        let ReplayRegistration::Execute(cell) = epoch
            .register(sequence, OperationFingerprint::Close { session_id })
            .expect("every production result slot is admitted")
        else {
            panic!("a fresh result slot must execute");
        };
        in_flight.push(cell);
    }

    let overflow_sequence =
        u64::try_from(OPERATION_RESULTS_PER_EPOCH + 1).expect("overflow sequence fits u64");
    let Err(error) = epoch.register(
        overflow_sequence,
        OperationFingerprint::Close { session_id },
    ) else {
        panic!("the next in-flight result must not exceed the exact window");
    };
    assert_eq!(error.kind(), DomainErrorKind::ResourceExhausted);

    in_flight[0].complete(Err(DaemonError::new(
        DomainErrorKind::Cancelled,
        "completed oldest fixture result",
    )));
    let ReplayRegistration::Execute(recovered) = epoch
        .register(
            overflow_sequence,
            OperationFingerprint::Close { session_id },
        )
        .expect("completing the oldest result recovers one exact slot")
    else {
        panic!("the recovered result slot must execute");
    };
    let state = epoch.state.lock().expect("result-window state");
    assert_eq!(state.operations.len(), OPERATION_RESULTS_PER_EPOCH);
    assert_eq!(state.low_water, 2);
    drop(state);
    let Err(evicted) = epoch.register(1, OperationFingerprint::Close { session_id }) else {
        panic!("the reclaimed oldest result cannot execute again");
    };
    assert_eq!(evicted.kind(), DomainErrorKind::OperationOutcomeUnknown);
    recovered.complete(Err(DaemonError::new(
        DomainErrorKind::Cancelled,
        "recovered fixture result",
    )));
}

#[test]
fn replay_principal_limit_is_exact_and_a_new_incarnation_recovers_capacity() {
    let principal = |byte| AttachmentPrincipal::RemoteEndpoint {
        device_id: DeviceId::from_array([byte; DeviceId::LENGTH]),
        auth_generation: 1,
    };
    let mut replay = ReplayRegistry::new();
    let first_principal = principal(1);
    let mut first_lease = None;
    for index in 0..MAX_REPLAY_PRINCIPALS {
        let byte = u8::try_from(index + 1).expect("principal fixture fits one byte");
        let lease = replay
            .issue(principal(byte))
            .expect("every production replay-principal slot is admitted");
        if index == 0 {
            first_lease = Some(lease);
        }
    }
    assert_eq!(replay.issued_through.len(), MAX_REPLAY_PRINCIPALS);

    let overflow = principal(0xfe);
    let error = replay
        .issue(overflow)
        .expect_err("the next replay principal exceeds the daemon-lifetime bound");
    assert_eq!(error.kind(), DomainErrorKind::ResourceExhausted);
    let first_lease = first_lease.expect("first daemon lease retained");
    drop(replay);

    let mut restarted = ReplayRegistry::new();
    let mut restarted_incarnation = first_lease.daemon_incarnation.to_bytes();
    restarted_incarnation[0] ^= 0xff;
    restarted.incarnation = DaemonIncarnation::from_array(restarted_incarnation);
    let restarted_lease = restarted
        .issue(first_principal)
        .expect("a new daemon incarnation starts with recovered principal capacity");
    assert_eq!(restarted.issued_through.len(), 1);
    assert_eq!(restarted_lease.ordinal, first_lease.ordinal);
    assert_ne!(
        restarted_lease.daemon_incarnation,
        first_lease.daemon_incarnation,
    );
    let fingerprint = OperationFingerprint::Close {
        session_id: SessionId::from_array([0x32; SessionId::LENGTH]),
    };
    let Err(old_daemon) = restarted.register(
        ReplayKey::new(first_principal, first_lease.ordinal),
        OperationId {
            lease: first_lease,
            sequence: 1,
        },
        fingerprint.clone(),
    ) else {
        panic!("capacity recovery must not revive an old-daemon operation");
    };
    assert_eq!(old_daemon.kind(), DomainErrorKind::OperationOutcomeUnknown);
    let ReplayRegistration::Execute(restarted_cell) = restarted
        .register(
            ReplayKey::new(first_principal, restarted_lease.ordinal),
            OperationId {
                lease: restarted_lease,
                sequence: 1,
            },
            fingerprint,
        )
        .expect("the new-incarnation lease remains executable")
    else {
        panic!("the new-incarnation operation must execute");
    };
    restarted_cell.complete(Err(DaemonError::new(
        DomainErrorKind::Cancelled,
        "new-incarnation fixture result",
    )));
}

#[test]
fn replay_epochs_retire_with_an_exact_per_principal_floor() {
    let principal = AttachmentPrincipal::LocalSameUid {
        own_device_id: DeviceId::from_array([9; 32]),
        local_view_id: AttachmentId::from_array([1; 16]),
    };
    let mut replay = ReplayRegistry::new();
    let mut first = None;
    for _ in 0..=MAX_ACTIVE_OPERATION_EPOCHS {
        let lease = replay.issue(principal).expect("lease is daemon-issued");
        first.get_or_insert(lease);
        let registration = replay
            .register(
                ReplayKey::new(principal, lease.ordinal),
                OperationId { lease, sequence: 1 },
                OperationFingerprint::Close {
                    session_id: SessionId::from_array([1; 16]),
                },
            )
            .expect("issued lease is admitted");
        let ReplayRegistration::Execute(cell) = registration else {
            panic!("new epoch must execute");
        };
        cell.complete(Err(DaemonError::new(
            DomainErrorKind::Cancelled,
            "fixture result",
        )));
    }
    assert_eq!(replay.epochs.len(), MAX_ACTIVE_OPERATION_EPOCHS);
    let first = first.expect("first lease");
    let retired = replay.register(
        ReplayKey::new(principal, first.ordinal),
        OperationId {
            lease: first,
            sequence: 1,
        },
        OperationFingerprint::Close {
            session_id: SessionId::from_array([1; 16]),
        },
    );
    let Err(error) = retired else {
        panic!("retired epoch cannot execute again");
    };
    assert_eq!(error.kind(), DomainErrorKind::OperationOutcomeUnknown);
}

#[test]
fn replay_never_retires_an_in_flight_result_to_admit_a_new_epoch() {
    let principal = AttachmentPrincipal::LocalSameUid {
        own_device_id: DeviceId::from_array([8; 32]),
        local_view_id: AttachmentId::from_array([2; 16]),
    };
    let mut replay = ReplayRegistry::new();
    let mut pending = Vec::new();
    let mut leases = Vec::new();
    for _ in 0..MAX_ACTIVE_OPERATION_EPOCHS {
        let lease = replay.issue(principal).expect("lease is daemon-issued");
        let ReplayRegistration::Execute(cell) = replay
            .register(
                ReplayKey::new(principal, lease.ordinal),
                OperationId { lease, sequence: 1 },
                OperationFingerprint::Close {
                    session_id: SessionId::from_array([2; 16]),
                },
            )
            .expect("in-flight epoch is admitted")
        else {
            panic!("new epoch must execute");
        };
        leases.push(lease);
        pending.push(cell);
    }

    let error = replay
        .issue(principal)
        .expect_err("an in-flight result cannot be made unknowable");
    assert_eq!(error.kind(), DomainErrorKind::ResourceExhausted);

    pending[0].complete(Err(DaemonError::new(
        DomainErrorKind::Cancelled,
        "completed fixture result",
    )));
    let next_lease = replay
        .issue(principal)
        .expect("a completed oldest epoch can retire");
    let ReplayRegistration::Execute(new_cell) = replay
        .register(
            ReplayKey::new(principal, next_lease.ordinal),
            OperationId {
                lease: next_lease,
                sequence: 1,
            },
            OperationFingerprint::Close {
                session_id: SessionId::from_array([2; 16]),
            },
        )
        .expect("newly issued epoch executes")
    else {
        panic!("the new epoch must execute");
    };
    new_cell.complete(Err(DaemonError::new(
        DomainErrorKind::Cancelled,
        "new fixture result",
    )));
    let old = replay.register(
        ReplayKey::new(principal, leases[0].ordinal),
        OperationId {
            lease: leases[0],
            sequence: 1,
        },
        OperationFingerprint::Close {
            session_id: SessionId::from_array([2; 16]),
        },
    );
    let Err(error) = old else {
        panic!("a retired operation must never be registered for execution");
    };
    assert_eq!(error.kind(), DomainErrorKind::OperationOutcomeUnknown);
}

#[test]
fn replay_rejects_unissued_and_old_daemon_leases_without_mutating_state() {
    let principal = AttachmentPrincipal::LocalSameUid {
        own_device_id: DeviceId::from_array([19; 32]),
        local_view_id: AttachmentId::from_array([3; 16]),
    };
    let mut first_daemon = ReplayRegistry::new();
    let lease = first_daemon.issue(principal).expect("issued lease");
    let mut restarted = ReplayRegistry::new();
    let before = (
        restarted.epochs.len(),
        restarted.retired_through.len(),
        restarted.issued_through.len(),
    );
    let operation = OperationId { lease, sequence: 1 };
    let fingerprint = OperationFingerprint::Close {
        session_id: SessionId::from_array([3; 16]),
    };
    let old = restarted.register(
        ReplayKey::new(principal, lease.ordinal),
        operation,
        fingerprint.clone(),
    );
    let Err(old) = old else {
        panic!("restart rejects old lease");
    };
    assert_eq!(old.kind(), DomainErrorKind::OperationOutcomeUnknown);
    assert_eq!(
        before,
        (
            restarted.epochs.len(),
            restarted.retired_through.len(),
            restarted.issued_through.len(),
        )
    );

    let issued = restarted.issue(principal).expect("new daemon lease");
    let invented = OperationId {
        lease: OperationLease {
            ordinal: issued.ordinal + 10,
            ..issued
        },
        sequence: 1,
    };
    let Err(error) = restarted.register(
        ReplayKey::new(principal, invented.lease.ordinal),
        invented,
        fingerprint,
    ) else {
        panic!("invented high lease is rejected");
    };
    assert_eq!(error.kind(), DomainErrorKind::OperationOutcomeUnknown);
    assert_eq!(
        restarted.issued_through[&ReplayPrincipal::from_attachment(principal)],
        1
    );
}

#[test]
fn lost_empty_leases_retire_boundedly_and_ordinal_exhaustion_is_explicit() {
    let principal = AttachmentPrincipal::LocalSameUid {
        own_device_id: DeviceId::from_array([21; 32]),
        local_view_id: AttachmentId::from_array([5; 16]),
    };
    let mut replay = ReplayRegistry::new();
    let first = replay.issue(principal).expect("first empty lease");
    let mut latest = first;
    for _ in 0..(MAX_ACTIVE_OPERATION_EPOCHS * 3) {
        latest = replay
            .issue(principal)
            .expect("lost empty lease remains bounded");
    }
    assert_eq!(replay.epochs.len(), MAX_ACTIVE_OPERATION_EPOCHS);
    let Err(retired) = replay.register(
        ReplayKey::new(principal, first.ordinal),
        OperationId {
            lease: first,
            sequence: 1,
        },
        OperationFingerprint::Close {
            session_id: SessionId::from_array([5; 16]),
        },
    ) else {
        panic!("old empty lease was retired");
    };
    assert_eq!(retired.kind(), DomainErrorKind::OperationOutcomeUnknown);
    let ReplayRegistration::Execute(cell) = replay
        .register(
            ReplayKey::new(principal, latest.ordinal),
            OperationId {
                lease: latest,
                sequence: u64::MAX - 1,
            },
            OperationFingerprint::Close {
                session_id: SessionId::from_array([5; 16]),
            },
        )
        .expect("latest issued lease executes")
    else {
        panic!("latest operation executes");
    };
    cell.complete(Err(DaemonError::new(
        DomainErrorKind::Cancelled,
        "fixture result",
    )));

    let replay_principal = ReplayPrincipal::from_attachment(principal);
    replay.issued_through.insert(replay_principal, u64::MAX);
    let exhausted = replay.issue(principal).expect_err("ordinal never wraps");
    assert_eq!(exhausted.kind(), DomainErrorKind::ResourceExhausted);
}

#[test]
fn read_only_session_queries_allocate_no_operation_lease_state() {
    let service = SessionService::with_spawner(
        DeviceId::from_array([22; 32]),
        ResourceLimits::default(),
        |_size, _cwd| panic!("read-only query must not spawn"),
    );
    assert!(service.list().expect("empty list").is_empty());
    let replay = service.replay.lock().expect("replay state");
    assert!(replay.epochs.is_empty());
    assert!(replay.issued_through.is_empty());
    assert!(replay.retired_through.is_empty());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_remote_resume_cell_moves_the_visible_checkpoint_or_falls_back_to_snapshot() {
    use std::io::Write as _;

    use nix::sys::stat::Mode;

    let temporary = tempfile::tempdir().expect("temporary resume-cell fixture");
    let gate = temporary.path().join("gate");
    nix::unistd::mkfifo(&gate, Mode::S_IRUSR | Mode::S_IWUSR).expect("create child-output gate");
    let service = unix_fixture_service(
        DeviceId::from_array([0x81; 32]),
        temporary.path().to_path_buf(),
        "printf 'READY\\r\\n'; IFS= read -r _ < gate; printf 'LATER\\r\\n'; exec /bin/cat",
    );
    let local = service.local_principal(AttachmentId::from_array([0x81; 16]));
    let lease = service
        .issue_operation_lease(local)
        .expect("fixture operation lease");
    let summary = service
        .create(
            local,
            OperationId { lease, sequence: 1 },
            SessionName::new("resume-cell").expect("fixture session name"),
            None,
            None,
        )
        .expect("fixture session creates");
    let remote = AttachmentPrincipal::RemoteEndpoint {
        device_id: DeviceId::from_array([0x82; 32]),
        auth_generation: 7,
    };
    let view_id = ResumeViewId::from_array([0x83; 16]);
    let request = |known_revision| RemoteAttachmentRequest {
        base_colors: Default::default(),

        selector: Some(SessionSelector::Id(summary.session_id)),
        create_main: false,
        takeover: false,
        initial_viewport: None,
        resume: RemoteResumeRequest {
            view_id,
            known_revision,
        },
    };

    let first = service
        .prepare_remote_attach_until(
            remote,
            request(None),
            Instant::now() + Duration::from_secs(2),
        )
        .expect("initial remote attachment");
    assert!(first.initial_delta.is_none());
    assert!(
        first
            .attachment
            .snapshot_applied(first.snapshot.revision)
            .expect("activate first remote attachment")
            .is_none()
    );
    let baseline = wait_for_attachment_text(&first, b"READY").await;
    let first_attachment_id = first.attachment.attachment_id();
    let overlap_error = match service.prepare_remote_attach_until(
        remote,
        request(Some(baseline)),
        Instant::now() + Duration::from_secs(2),
    ) {
        Ok(_) => panic!("a live view cannot be resumed before transport EOF"),
        Err(error) => error,
    };
    assert_eq!(overlap_error.kind(), DomainErrorKind::TransportUnavailable);
    let mut different_view = request(Some(baseline));
    different_view.resume.view_id = ResumeViewId::from_array([0x84; 16]);
    let occupied = service
        .prepare_remote_attach_until(
            remote,
            different_view,
            Instant::now() + Duration::from_secs(2),
        )
        .err()
        .expect("another view cannot wait to reclaim control");
    assert_eq!(occupied.kind(), DomainErrorKind::SessionOccupied);
    first
        .attachment
        .write_input(b"")
        .expect("the original controller survives an overlapping resume request");
    let mut revisions = first
        .attachment
        .revision_watch()
        .expect("revision watermark");
    let _ = revisions.borrow_and_update();
    assert!(
        first
            .attachment
            .detach_for_remote_resume_until(Instant::now() + Duration::from_secs(2))
            .expect("transport EOF saves the exact remote controller checkpoint")
    );
    assert!(
        !service
            .list()
            .expect("controller release observation")
            .into_iter()
            .find(|candidate| candidate.session_id == summary.session_id)
            .expect("fixture session remains live")
            .has_controller
    );

    let gate_write = tokio::task::spawn_blocking(move || {
        let mut gate = std::fs::OpenOptions::new()
            .write(true)
            .open(gate)
            .expect("open child-output gate");
        gate.write_all(b"continue\n").expect("release child output");
        gate.flush().expect("flush child-output gate");
    });
    tokio::time::timeout(Duration::from_secs(2), gate_write)
        .await
        .expect("child opened its output gate")
        .expect("gate writer task");
    tokio::time::timeout(Duration::from_secs(2), async {
        while *revisions.borrow_and_update() <= baseline {
            revisions
                .changed()
                .await
                .expect("driver revision remains open");
        }
    })
    .await
    .expect("post-disconnect output reached the authoritative model");

    let resumed = service
        .prepare_remote_attach_until(
            remote,
            request(Some(baseline)),
            Instant::now() + Duration::from_secs(2),
        )
        .expect("matching remote checkpoint resumes");
    assert_ne!(resumed.attachment.attachment_id(), first_attachment_id);
    let delta = resumed
        .initial_delta
        .as_ref()
        .expect("exact checkpoint produces one merged semantic delta");
    assert_eq!(delta.from_revision, baseline);
    assert!(delta.to_revision > baseline);
    assert!(terminal_delta_contains(delta, "LATER"));
    let resumed_revision = delta.to_revision;
    assert!(
        resumed
            .attachment
            .snapshot_applied(resumed_revision)
            .expect("acknowledge resume delta")
            .is_none()
    );
    assert!(
        resumed
            .attachment
            .detach_for_remote_resume_until(Instant::now() + Duration::from_secs(2))
            .expect("second transport EOF replaces the sole resume cell")
    );

    let mismatched = service
        .prepare_remote_attach_until(
            remote,
            request(Some(Revision::ZERO)),
            Instant::now() + Duration::from_secs(2),
        )
        .expect("a mismatched baseline falls back to an authoritative snapshot");
    assert!(mismatched.initial_delta.is_none());
    assert!(terminal_snapshot_contains(&mismatched.snapshot, b"LATER"));
    assert!(
        mismatched
            .attachment
            .snapshot_applied(mismatched.snapshot.revision)
            .expect("activate fallback snapshot")
            .is_none()
    );
    mismatched.attachment.detach();

    let after_explicit_detach = service
        .prepare_remote_attach_until(
            remote,
            request(Some(mismatched.snapshot.revision)),
            Instant::now() + Duration::from_secs(2),
        )
        .expect("explicit detach leaves no resumable controller checkpoint");
    assert!(after_explicit_detach.initial_delta.is_none());
    after_explicit_detach.attachment.detach();
    service.shutdown().expect("resume fixture shuts down");
}

#[cfg(unix)]
#[test]
fn resumed_attachment_effect_eligibility_waits_for_its_current_active_barrier() {
    let temporary = tempfile::tempdir().expect("temporary resume-effect fixture");
    let service = unix_fixture_service(
        DeviceId::from_array([0xa1; 32]),
        temporary.path().to_path_buf(),
        "exec /bin/cat",
    );
    let local = service.local_principal(AttachmentId::from_array([0xa1; 16]));
    let lease = service
        .issue_operation_lease(local)
        .expect("fixture operation lease");
    let summary = service
        .create(
            local,
            OperationId { lease, sequence: 1 },
            SessionName::new("resume-effect-fence").expect("fixture session name"),
            None,
            None,
        )
        .expect("fixture session creates");
    let remote = AttachmentPrincipal::RemoteEndpoint {
        device_id: DeviceId::from_array([0xa2; 32]),
        auth_generation: 11,
    };
    let view_id = ResumeViewId::from_array([0xa3; 16]);
    let request = |known_revision| RemoteAttachmentRequest {
        base_colors: Default::default(),

        selector: Some(SessionSelector::Id(summary.session_id)),
        create_main: false,
        takeover: false,
        initial_viewport: None,
        resume: RemoteResumeRequest {
            view_id,
            known_revision,
        },
    };

    let first = service
        .prepare_remote_attach_until(
            remote,
            request(None),
            Instant::now() + Duration::from_secs(2),
        )
        .expect("initial remote attachment");
    assert!(
        first
            .attachment
            .snapshot_applied(first.snapshot.revision)
            .expect("activate initial controller")
            .is_none()
    );
    let retained_revision = first.snapshot.revision;
    assert!(
        first
            .attachment
            .detach_for_remote_resume_until(Instant::now() + Duration::from_secs(2))
            .expect("save active controller checkpoint")
    );

    let resumed = service
        .prepare_remote_attach_until(
            remote,
            request(Some(retained_revision)),
            Instant::now() + Duration::from_secs(2),
        )
        .expect("matching checkpoint resumes into a new attachment lifetime");
    let before_active = TerminalHostEffect::ClipboardWrite(
        voytty_core::terminal::TerminalClipboardWrite::new("resume before active".to_owned())
            .expect("valid clipboard fixture"),
    );
    resumed
        .attachment
        .effect_broker
        .publish_for_test(before_active)
        .expect("publish through the real Session broker");
    assert!(
        resumed
            .attachment
            .take_host_effect()
            .expect("inspect resumed attachment effect")
            .is_none(),
        "a retained checkpoint must not transfer effect eligibility into a new attachment lifetime"
    );

    assert!(
        resumed
            .attachment
            .snapshot_applied(resumed.snapshot.revision)
            .expect("activate resumed attachment")
            .is_none()
    );
    let after_active = TerminalHostEffect::ClipboardWrite(
        voytty_core::terminal::TerminalClipboardWrite::new("resume after active".to_owned())
            .expect("valid clipboard fixture"),
    );
    resumed
        .attachment
        .effect_broker
        .publish_for_test(after_active)
        .expect("publish after current Active barrier");
    let TerminalHostEffect::ClipboardWrite(write) = resumed
        .attachment
        .take_host_effect()
        .expect("take resumed attachment effect")
        .expect("active resumed attachment is eligible")
    else {
        panic!("expected clipboard effect");
    };
    assert_eq!(write.as_str(), "resume after active");

    resumed.attachment.detach();
    service
        .shutdown()
        .expect("resume-effect fixture shuts down");
}

#[cfg(unix)]
#[test]
fn takeover_awaiting_replacement_effect_eligibility_waits_for_active_barrier() {
    let temporary = tempfile::tempdir().expect("temporary takeover-effect fixture");
    let service = unix_fixture_service(
        DeviceId::from_array([0xb1; 32]),
        temporary.path().to_path_buf(),
        "exec /bin/cat",
    );
    let original = service.local_principal(AttachmentId::from_array([0xb1; 16]));
    let original_lease = service
        .issue_operation_lease(original)
        .expect("original operation lease");
    let summary = service
        .create(
            original,
            OperationId {
                lease: original_lease,
                sequence: 1,
            },
            SessionName::new("takeover-effect-fence").expect("fixture session name"),
            None,
            None,
        )
        .expect("fixture session creates");
    let active = service
        .prepare_attach(
            original,
            Some(SessionSelector::Id(summary.session_id)),
            false,
            false,
            None,
        )
        .expect("original controller prepares");
    assert!(
        active
            .attachment
            .snapshot_applied(active.snapshot.revision)
            .expect("activate original controller")
            .is_none()
    );

    let replacement_principal = service.local_principal(AttachmentId::from_array([0xb2; 16]));
    let replacement_lease = service
        .issue_operation_lease(replacement_principal)
        .expect("replacement operation lease");
    let replacement = service
        .prepare_attach(
            replacement_principal,
            Some(SessionSelector::Id(summary.session_id)),
            false,
            true,
            None,
        )
        .expect("takeover attachment prepares");
    assert!(
        replacement
            .attachment
            .snapshot_applied(replacement.snapshot.revision)
            .expect("acknowledge prepared takeover snapshot")
            .is_none()
    );
    let current_snapshot = replacement
        .attachment
        .sync_latest(replacement.snapshot.revision)
        .expect("begin a replacement snapshot before takeover response");
    service
        .takeover(
            replacement_principal,
            OperationId {
                lease: replacement_lease,
                sequence: 1,
            },
            &replacement.attachment,
        )
        .expect("commit takeover while current replacement is awaiting acknowledgement");

    let before_active = TerminalHostEffect::ClipboardWrite(
        voytty_core::terminal::TerminalClipboardWrite::new("takeover before active".to_owned())
            .expect("valid clipboard fixture"),
    );
    replacement
        .attachment
        .effect_broker
        .publish_for_test(before_active)
        .expect("publish through the real Session broker");
    assert!(
        replacement
            .attachment
            .take_host_effect()
            .expect("inspect takeover attachment effect")
            .is_none(),
        "takeover commit must not bypass the current attachment lifetime's Active barrier"
    );

    assert!(
        replacement
            .attachment
            .snapshot_applied(current_snapshot.revision)
            .expect("activate replacement controller")
            .is_none()
    );
    let after_active = TerminalHostEffect::ClipboardWrite(
        voytty_core::terminal::TerminalClipboardWrite::new("takeover after active".to_owned())
            .expect("valid clipboard fixture"),
    );
    replacement
        .attachment
        .effect_broker
        .publish_for_test(after_active)
        .expect("publish after replacement Active barrier");
    let TerminalHostEffect::ClipboardWrite(write) = replacement
        .attachment
        .take_host_effect()
        .expect("take replacement attachment effect")
        .expect("active takeover attachment is eligible")
    else {
        panic!("expected clipboard effect");
    };
    assert_eq!(write.as_str(), "takeover after active");

    replacement.attachment.detach();
    service
        .shutdown()
        .expect("takeover-effect fixture shuts down");
}

#[cfg(unix)]
#[test]
fn overlapping_snapshot_replies_accept_the_same_applied_revision_once() {
    let temporary = tempfile::tempdir().expect("overlapping snapshot fixture");
    let service = unix_fixture_service(
        DeviceId::from_array([0x92; 32]),
        temporary.path().to_path_buf(),
        "exec /bin/cat",
    );
    let principal = service.local_principal(AttachmentId::from_array([0x92; 16]));
    let prepared = service
        .prepare_attach(principal, None, true, false, None)
        .expect("initial attachment");
    let attachment = &prepared.attachment;
    attachment
        .resize(TerminalSize::new(31, 97))
        .expect("viewport observation may cross the first snapshot");
    let newer = attachment
        .sync_latest(prepared.snapshot.revision)
        .expect("newer snapshot is already on the wire");
    assert_ne!(newer.revision, prepared.snapshot.revision);
    let repeated = attachment
        .snapshot_applied(prepared.snapshot.revision)
        .expect("old ACK requests a replacement")
        .expect("old ACK does not activate the newer snapshot");
    assert_eq!(repeated.revision, newer.revision);
    assert_eq!(
        attachment
            .write_input(b"blocked")
            .expect_err("initial synchronization still fences input")
            .kind(),
        DomainErrorKind::NotSynchronized
    );
    assert!(
        attachment
            .snapshot_applied(newer.revision)
            .expect("apply newer snapshot")
            .is_none()
    );
    assert!(
        attachment
            .snapshot_applied(repeated.revision)
            .expect("the replacement reply needs the same harmless ACK")
            .is_none()
    );
    attachment
        .write_input(b"")
        .expect("controller remains active");
    assert_eq!(
        attachment
            .snapshot_applied(Revision::new(newer.revision.get() + 1))
            .expect_err("an unissued ACK is still invalid")
            .kind(),
        DomainErrorKind::NotSynchronized
    );
    attachment
        .resize(TerminalSize::new(32, 98))
        .expect("another viewport");
    let newest = attachment
        .sync_latest(newer.revision)
        .expect("new synchronization fence");
    assert_ne!(newest.revision, newer.revision);
    assert!(
        attachment
            .snapshot_applied(newer.revision)
            .expect("ignore a repeated old ACK during newer synchronization")
            .is_none()
    );
    assert!(matches!(
        attachment
            .lifecycle_watch()
            .expect("attachment lifecycle")
            .borrow()
            .clone(),
        AttachmentLifecycle::AwaitingSnapshot { revision } if revision == newest.revision
    ));
    assert!(
        attachment
            .snapshot_applied(newest.revision)
            .expect("apply newest snapshot")
            .is_none()
    );

    let successor = service.local_principal(AttachmentId::from_array([0x93; 16]));
    let replacement = service
        .prepare_attach(
            successor,
            Some(SessionSelector::Id(attachment.session_id())),
            false,
            true,
            None,
        )
        .expect("successor prepares");
    replacement
        .attachment
        .snapshot_applied(replacement.snapshot.revision)
        .expect("successor applies its own snapshot");
    service
        .takeover(
            successor,
            OperationId {
                lease: service
                    .issue_operation_lease(successor)
                    .expect("successor operation lease"),
                sequence: 1,
            },
            &replacement.attachment,
        )
        .expect("explicit successor takeover");
    assert_eq!(
        attachment
            .snapshot_applied(newest.revision)
            .expect_err("a retired attachment cannot acknowledge anything")
            .kind(),
        DomainErrorKind::LeaseLost
    );
    attachment.detach();
    service.shutdown().expect("fixture shutdown");
}

#[cfg(unix)]
#[test]
fn semantic_history_and_input_obey_the_controller_sync_window() {
    let temporary = tempfile::tempdir().expect("temporary visual-sync fixture");
    let service = unix_fixture_service(
        DeviceId::from_array([0x91; 32]),
        temporary.path().to_path_buf(),
        "exec /bin/cat",
    );
    let controller = service.local_principal(AttachmentId::from_array([0x91; 16]));
    let lease = service
        .issue_operation_lease(controller)
        .expect("fixture operation lease");
    let summary = service
        .create(
            controller,
            OperationId { lease, sequence: 1 },
            SessionName::new("visual-sync-window").expect("fixture session name"),
            None,
            None,
        )
        .expect("fixture session creates");
    let prepared = service
        .prepare_attach(
            controller,
            Some(SessionSelector::Id(summary.session_id)),
            false,
            false,
            None,
        )
        .expect("controller attachment prepares");
    let metrics = prepared
        .snapshot
        .surface
        .scroll_metrics
        .expect("main-screen snapshot carries history coordinates");
    let query = TerminalHistoryWindowQuery {
        anchor: TerminalHistoryWindowAnchor {
            epoch: metrics.epoch,
            revision: metrics.revision,
            max_offset_from_bottom: metrics.max_offset_from_bottom,
            viewport: prepared.snapshot.surface.size,
        },
        target_offset_from_bottom: 0,
        older_margin_rows: 0,
        newer_margin_rows: 0,
    };
    let error = prepared
        .attachment
        .history_window_until(query, Instant::now() + Duration::from_secs(1))
        .expect_err("the first snapshot must be acknowledged before history projection");
    assert_eq!(error.kind(), DomainErrorKind::NotSynchronized);
    let error = prepared
        .attachment
        .write_input(b"must-not-reach-initial-pty")
        .expect_err("the first snapshot must be acknowledged before input");
    assert_eq!(error.kind(), DomainErrorKind::NotSynchronized);
    assert!(
        prepared
            .attachment
            .snapshot_applied(prepared.snapshot.revision)
            .expect("activate controller")
            .is_none()
    );
    let replacement = prepared
        .attachment
        .sync_latest(prepared.snapshot.revision)
        .expect("enter deterministic visual synchronization");
    assert!(matches!(
        prepared
            .attachment
            .history_window_until(query, Instant::now() + Duration::from_secs(1))
            .expect("an existing controller may read history during resynchronization"),
        TerminalSurfaceHistoryWindowResult::Frame(_)
    ));
    prepared
        .attachment
        .write_input(b"")
        .expect("existing controller input crosses its visual-sync window");
    assert!(
        prepared
            .attachment
            .snapshot_applied(replacement.revision)
            .expect("acknowledge replacement")
            .is_none()
    );
    service.shutdown().expect("fixture session shuts down");
}

#[cfg(unix)]
#[test]
fn remote_attachment_count_is_directional_and_read_only() {
    let temporary = tempfile::tempdir().expect("temporary attachment-count fixture");
    let service = unix_fixture_service(
        DeviceId::from_array([0x71; 32]),
        temporary.path().to_path_buf(),
        "exec /bin/cat",
    );
    let local = service.local_principal(AttachmentId::from_array([0x71; 16]));
    let lease = service
        .issue_operation_lease(local)
        .expect("fixture operation lease");
    let create = |sequence, name: &str| {
        service
            .create(
                local,
                OperationId { lease, sequence },
                SessionName::new(name).expect("fixture session name"),
                None,
                None,
            )
            .expect("fixture session creates")
    };
    let first = create(1, "remote-count-a");
    let second = create(2, "remote-count-b");
    let other = create(3, "remote-count-c");
    let target_id = DeviceId::from_array([0x72; 32]);
    let target = AttachmentPrincipal::RemoteEndpoint {
        device_id: target_id,
        auth_generation: 4,
    };
    let other_id = DeviceId::from_array([0x73; 32]);
    let first_attachment = service
        .prepare_attach(
            target,
            Some(SessionSelector::Id(first.session_id)),
            false,
            false,
            None,
        )
        .expect("first remote attachment");
    let second_attachment = service
        .prepare_attach(
            target,
            Some(SessionSelector::Id(second.session_id)),
            false,
            false,
            None,
        )
        .expect("second remote attachment");
    let other_attachment = service
        .prepare_attach(
            AttachmentPrincipal::RemoteEndpoint {
                device_id: other_id,
                auth_generation: 9,
            },
            Some(SessionSelector::Id(other.session_id)),
            false,
            false,
            None,
        )
        .expect("other remote attachment");

    let deadline = Instant::now() + Duration::from_secs(2);
    assert_eq!(
        service
            .remote_attachment_count_until(target_id, deadline)
            .expect("target count"),
        2
    );
    assert_eq!(
        service
            .remote_attachment_count_until(other_id, deadline)
            .expect("other count"),
        1
    );
    assert_eq!(service.list().expect("sessions remain live").len(), 3);

    for prepared in [&first_attachment, &second_attachment, &other_attachment] {
        assert!(
            prepared
                .attachment
                .snapshot_applied(prepared.snapshot.revision)
                .expect("count leaves attachment usable")
                .is_none()
        );
    }
    service.shutdown().expect("fixture sessions shut down");
}

#[test]
fn panicking_execution_completes_duplicate_waiters_with_outcome_unknown() {
    let service = SessionService::with_spawner(
        DeviceId::from_array([20; 32]),
        ResourceLimits::default(),
        |_size, _cwd| panic!("spawner must not run"),
    );
    let principal = service.local_principal(AttachmentId::from_array([4; 16]));
    let lease = service
        .issue_operation_lease(principal)
        .expect("issued replay lease");
    let operation_id = OperationId { lease, sequence: 1 };
    let fingerprint = OperationFingerprint::Close {
        session_id: SessionId::from_array([4; 16]),
    };
    let (entered, observed) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let executor_service = service.clone();
    let executor_fingerprint = fingerprint.clone();
    let executor = thread::spawn(move || {
        executor_service.execute_replayed(principal, operation_id, executor_fingerprint, || {
            entered.send(()).expect("panic observer");
            released.recv().expect("panic release");
            panic!("deterministic mutation panic");
        })
    });
    observed.recv().expect("executor entered");
    let waiter_service = service.clone();
    let waiter = thread::spawn(move || {
        waiter_service.execute_replayed(principal, operation_id, fingerprint, || {
            panic!("duplicate closure must never run")
        })
    });
    release.send(()).expect("release panic");
    for result in [
        executor.join().expect("executor thread joins"),
        waiter.join().expect("waiter thread joins"),
    ] {
        assert_eq!(
            result.expect_err("panic is reported as unknown").kind(),
            DomainErrorKind::OperationOutcomeUnknown
        );
    }
}

#[test]
fn operation_completion_recovers_poison_and_wakes_with_a_terminal_result() {
    let cell = Arc::new(OperationCell::new(OperationFingerprint::Close {
        session_id: SessionId::from_array([44; 16]),
    }));
    let poisoned = Arc::clone(&cell);
    assert!(
        thread::spawn(move || {
            let _result = poisoned.result.lock().expect("operation result lock");
            panic!("inject operation-result poison");
        })
        .join()
        .is_err()
    );

    cell.complete(Err(outcome_unknown()));
    assert_eq!(
        cell.wait()
            .expect_err("poison recovery retains terminal unknown result")
            .kind(),
        DomainErrorKind::OperationOutcomeUnknown
    );
}

#[cfg(unix)]
#[test]
fn actor_mailbox_is_exactly_bounded_and_recovers_after_the_worker_drains() {
    let temporary = tempfile::tempdir().expect("temporary actor-mailbox fixture");
    let working_directory = temporary.path().to_path_buf();
    let sleep = [Path::new("/bin/sleep"), Path::new("/usr/bin/sleep")]
        .into_iter()
        .find(|path| path.is_file())
        .expect("sleep fixture")
        .to_path_buf();
    let service = SessionService::with_spawner(
        DeviceId::from_array([0x33; DeviceId::LENGTH]),
        ResourceLimits::default(),
        move |size, requested| {
            let cwd = requested.unwrap_or(&working_directory).to_path_buf();
            let session = PtyHost::new()
                .spawn(
                    ExplicitPtyCommand::new(&sleep, &cwd).arg("30"),
                    PtySize::new(size.rows, size.columns),
                )
                .map_err(map_pty_error)?;
            Ok((session, cwd))
        },
    );
    let principal = service.local_principal(AttachmentId::from_array([0x34; AttachmentId::LENGTH]));
    let lease = service
        .issue_operation_lease(principal)
        .expect("mailbox fixture lease");
    let summary = service
        .create(
            principal,
            OperationId { lease, sequence: 1 },
            SessionName::new("mailbox-bound").expect("mailbox fixture name"),
            None,
            None,
        )
        .expect("mailbox fixture creates");
    let actor = service
        .resolve(&SessionSelector::Id(summary.session_id))
        .expect("mailbox fixture actor resolves");

    let (entered, entered_receiver) = mpsc::sync_channel(1);
    let (release, release_receiver) = mpsc::sync_channel(1);
    let blocked_actor = Arc::clone(&actor);
    let blocker = thread::spawn(move || {
        blocked_actor.block_pty_effect_for_test(
            Instant::now() + Duration::from_secs(5),
            entered,
            release_receiver,
            Arc::new(AtomicUsize::new(0)),
        )
    });
    entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("actor entered the deterministic blocking boundary");

    let (queued_entered, queued_entered_receiver) = mpsc::sync_channel(1);
    let (queued_release, queued_release_receiver) = mpsc::sync_channel(1);
    let queued_executions = Arc::new(AtomicUsize::new(0));
    let queued_deadline = Instant::now() + Duration::from_secs(5);
    let queued_waiter = actor
        .enqueue_command(queued_deadline, {
            let queued_executions = Arc::clone(&queued_executions);
            move |meta, reply| SessionCommand::BlockPtyEffect {
                meta,
                entered: queued_entered,
                release: queued_release_receiver,
                executions: queued_executions,
                reply,
            }
        })
        .expect("the first production mailbox slot admits a deterministic barrier");
    for _ in 1..SESSION_COMMAND_CAPACITY {
        assert!(
            actor.commands.try_send(SessionCommand::Wake).is_ok(),
            "every production mailbox slot is admitted while the worker is blocked",
        );
    }
    let Err(saturated) = actor.enqueue_command(Instant::now(), |meta, reply| {
        SessionCommand::CountRemoteAttachments {
            meta,
            device_id: DeviceId::from_array([0x35; DeviceId::LENGTH]),
            reply,
        }
    }) else {
        panic!("the next actor command must be backpressured at the exact bound");
    };
    assert_eq!(saturated.kind(), DomainErrorKind::DeadlineExceeded);

    release.send(()).expect("release blocked actor worker");
    blocker
        .join()
        .expect("mailbox blocker thread joins")
        .expect("mailbox blocker completes");

    queued_entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("the worker receives exactly the first queued command");
    assert_eq!(queued_executions.load(Ordering::Acquire), 1);

    let recovered_deadline = Instant::now() + Duration::from_secs(5);
    let recovered = actor
        .enqueue_command(recovered_deadline, |meta, reply| {
            SessionCommand::CountRemoteAttachments {
                meta,
                device_id: DeviceId::from_array([0x36; DeviceId::LENGTH]),
                reply,
            }
        })
        .expect("one receive recovers exactly one production mailbox slot");
    let Err(full_again) = actor.enqueue_command(Instant::now(), |meta, reply| {
        SessionCommand::CountRemoteAttachments {
            meta,
            device_id: DeviceId::from_array([0x37; DeviceId::LENGTH]),
            reply,
        }
    }) else {
        panic!("the recovered slot must restore the exact mailbox bound");
    };
    assert_eq!(full_again.kind(), DomainErrorKind::DeadlineExceeded);

    queued_release
        .send(())
        .expect("release the first queued actor command");
    queued_waiter
        .wait(queued_deadline)
        .expect("the queued barrier completes after release");
    assert_eq!(
        recovered
            .wait(recovered_deadline)
            .expect("the recovered mailbox command completes"),
        0,
    );

    service.shutdown().expect("mailbox fixture shuts down");
}

#[cfg(unix)]
#[test]
fn operational_logs_capture_committed_events_without_terminal_or_state_contents() {
    // Tracing callsite interest is process-wide. A dedicated helper keeps
    // unrelated actor tests registering no-op dispatchers out of this
    // subscriber's capture without installing a global subscriber.
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "session::tests::operational_logs_child",
            "--ignored",
            "--nocapture",
        ])
        .output()
        .expect("isolated log fixture process");
    assert!(
        output.status.success(),
        "log fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
#[ignore = "isolated operational-log capture helper"]
fn operational_logs_child() {
    use crate::network::{NetworkDiagnostic, NetworkReporter, NetworkState};
    use std::io::Read as _;
    use tracing_subscriber::prelude::*;
    let temporary = tempfile::tempdir().expect("logging/admission fixture succeeds");
    let cwd = temporary.path().join("LOG_CWD_SENTINEL");
    std::fs::create_dir(&cwd).expect("logging/admission fixture succeeds");
    let capture = tempfile::tempfile().expect("logging/admission fixture succeeds");
    let writer = capture
        .try_clone()
        .expect("logging/admission fixture succeeds");
    let paths = voytty_platform::user_state::UserPaths::for_test(
        nix::unistd::geteuid().as_raw(),
        temporary.path().into(),
        temporary.path().join("state"),
        temporary.path().join("run"),
    );
    paths.prepare_state_directories().expect("private logs");
    let recorder = voytty_diagnostics::Recorder::new(Arc::new(
        voytty_platform::diagnostics::Store::new(paths.clone()),
    ))
    .expect("safe recorder");
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_target(true)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || {
            writer
                .try_clone()
                .expect("logging/admission fixture succeeds")
        })
        .finish()
        .with(crate::diagnostics::ApplicationEvents(recorder.clone()));
    let dispatch = tracing::Dispatch::new(subscriber);
    let _guard = tracing::dispatcher::set_default(&dispatch);
    let id = DeviceId::from_array([0x91; 32]);
    let service = unix_fixture_service(id, cwd, "printf LOG_TERMINAL_SENTINEL; exec cat");
    let principal = service.local_principal(AttachmentId::from_array([0x92; 16]));
    let lease = service
        .issue_operation_lease(principal)
        .expect("logging/admission fixture succeeds");
    let operation = OperationId { lease, sequence: 1 };
    let name = SessionName::new("logged-session").expect("logging/admission fixture succeeds");
    let created = service
        .create(principal, operation, name.clone(), None, None)
        .expect("logging/admission fixture succeeds");
    let replay = service
        .create(principal, operation, name, None, None)
        .expect("logging/admission fixture succeeds");
    assert_eq!(created.session_id, replay.session_id);
    let attachment = service
        .prepare_attach(
            principal,
            Some(SessionSelector::Id(created.session_id)),
            false,
            false,
            None,
        )
        .expect("logging/admission fixture succeeds");
    attachment
        .attachment
        .snapshot_applied(attachment.snapshot.revision)
        .expect("logging/admission fixture succeeds");
    attachment
        .attachment
        .write_input(b"LOG_INPUT_SENTINEL\n")
        .expect("logging/admission fixture succeeds");
    // Cancelling a prepared replacement never releases the current
    // controller, whether cancellation is explicit or transport EOF.
    for transport_closed in [false, true] {
        let replacement = service
            .prepare_attach(
                principal,
                Some(SessionSelector::Id(created.session_id)),
                false,
                true,
                None,
            )
            .expect("prepare replacement without taking control");
        if transport_closed {
            assert!(
                !replacement
                    .attachment
                    .detach_for_remote_resume_until(default_deadline())
                    .expect("prepared replacement cannot save a controller checkpoint")
            );
        } else {
            replacement.attachment.detach();
        }
        attachment
            .attachment
            .resize(created.viewport)
            .expect("original controller retains input authority");
    }
    attachment.attachment.detach();
    service
        .close(
            principal,
            OperationId { lease, sequence: 2 },
            created.session_id,
        )
        .expect("logging/admission fixture succeeds");
    let (network, _) = NetworkReporter::initializing(id);
    network.update(|observation| {
        observation.state = NetworkState::Degraded;
        observation.diagnostic = Some(NetworkDiagnostic::HomeRelayUnavailable);
        observation.home_relay = Some("https://LOG_RELAY_SENTINEL.example".to_owned());
    });
    network.update(|observation| {
        observation.bind_attempts += 1;
    });
    network.update(|observation| {
        observation.state = NetworkState::Online;
        observation.diagnostic = None;
    });
    assert!(recorder.flush(Duration::from_secs(2)));
    let structured = std::fs::read_to_string(paths.daemon_log()).expect("safe structured logs");
    let records: Vec<_> = structured
        .lines()
        .map(|line| voytty_diagnostics::Record::decode(line.as_bytes()).expect("validated record"))
        .collect();
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event == voytty_diagnostics::Kind::SessionCreated)
            .count(),
        1
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event == voytty_diagnostics::Kind::ControllerDetached)
            .count(),
        1
    );
    assert!(records.iter().any(|record| record.event
        == voytty_diagnostics::Kind::ControllerAttached
        && record.fields.attachment_id.is_some()));
    assert_eq!(
        records
            .iter()
            .filter(|record| record.event == voytty_diagnostics::Kind::NetworkChanged)
            .count(),
        2
    );
    let degraded = records
        .iter()
        .find(|record| {
            record.event == voytty_diagnostics::Kind::NetworkChanged
                && record.level == voytty_diagnostics::Level::Warn
        })
        .expect("network degradation");
    assert_eq!(
        degraded.fields.stage,
        Some(voytty_diagnostics::Stage::Degraded)
    );
    assert_eq!(
        degraded.fields.category.as_deref(),
        Some("home_relay_unavailable")
    );
    assert!(degraded.fields.publish.is_some() && degraded.fields.lookup.is_some());
    assert!(!structured.contains("SENTINEL"));
    let mut capture = capture;
    use std::io::{Seek as _, SeekFrom};
    capture
        .seek(SeekFrom::Start(0))
        .expect("logging/admission fixture succeeds");
    let mut logs = String::new();
    capture
        .read_to_string(&mut logs)
        .expect("logging/admission fixture succeeds");
    assert_eq!(logs.matches("Session created").count(), 1);
    assert!(logs.contains(&created.session_id.to_string()) && logs.contains("logged-session"));
    assert!(
        logs.contains("Session ended") && logs.contains("explicit_close"),
        "captured events: {logs}"
    );
    assert!(logs.contains("Controller attached"));
    assert_eq!(logs.matches("Controller detached").count(), 1);
    assert_eq!(logs.matches("Network degraded").count(), 1);
    assert_eq!(logs.matches("Network state changed").count(), 1);
    assert!(logs.contains("home_relay_unavailable") && logs.contains("online"));
    for sentinel in [
        "LOG_CWD_SENTINEL",
        "LOG_TERMINAL_SENTINEL",
        "LOG_INPUT_SENTINEL",
        "LOG_RELAY_SENTINEL",
    ] {
        assert!(!logs.contains(sentinel));
    }
    assert!(
        !logs
            .lines()
            .any(|line| line.contains("WARN") && line.contains("detached"))
    );
}

#[test]
fn idle_shutdown_serializes_with_creation_admission_without_cancelling_work() {
    let registry = RegistryInner::default();
    assert!(
        registry
            .owned_entries()
            .expect("logging/admission fixture succeeds")
            .is_empty()
    );
    let name =
        SessionName::new("admitted-after-observation").expect("logging/admission fixture succeeds");
    let NameReservation::Owner(creation) = registry
        .reserve_name(&name, false)
        .expect("logging/admission fixture succeeds")
    else {
        panic!("new reservation")
    };
    assert_eq!(
        registry
            .begin_shutdown(false)
            .err()
            .expect("logging/admission fixture succeeds"),
        vec![name.to_string()]
    );
    assert!(!creation.is_cancelled());
    assert!(cleanup_lock(&registry.state).accepting);
    assert_eq!(
        registry
            .begin_shutdown(true)
            .expect("logging/admission fixture succeeds")
            .len(),
        1
    );
    let idle = RegistryInner::default();
    assert!(
        idle.begin_shutdown(false)
            .expect("logging/admission fixture succeeds")
            .is_empty()
    );
    assert!(idle.reserve_name(&name, false).is_err());
}

#[cfg(unix)]
#[test]
fn blocked_session_effect_isolated_from_status_deadlines_and_another_session() {
    let temporary = tempfile::tempdir().expect("temporary session-effect fixture");
    let working_directory = temporary.path().to_path_buf();
    let shell = [Path::new("/bin/sh"), Path::new("/usr/bin/sh")]
        .into_iter()
        .find(|path| path.is_file())
        .expect("POSIX shell fixture")
        .to_path_buf();
    let service = SessionService::with_spawner(
        DeviceId::from_array([61; 32]),
        ResourceLimits::default(),
        move |size, requested| {
            let cwd = requested.unwrap_or(&working_directory).to_path_buf();
            let session = PtyHost::new()
                .spawn(
                    ExplicitPtyCommand::new(&shell, &cwd).arg("-i"),
                    PtySize::new(size.rows, size.columns),
                )
                .map_err(map_pty_error)?;
            Ok((session, cwd))
        },
    );
    let principal = service.local_principal(AttachmentId::from_array([7; 16]));
    let operation_lease = service
        .issue_operation_lease(principal)
        .expect("fixture operation lease");
    let create = |sequence, name: &str| {
        service
            .create(
                principal,
                OperationId {
                    lease: operation_lease,
                    sequence,
                },
                SessionName::new(name).expect("test session name"),
                None,
                None,
            )
            .expect("test session creates")
    };
    let first = create(1, "blocked-a");
    let second = create(2, "responsive-b");
    let first_actor = service
        .resolve(&SessionSelector::Id(first.session_id))
        .expect("first actor resolves");
    let second_actor = service
        .resolve(&SessionSelector::Id(second.session_id))
        .expect("second actor resolves");

    let first_executions = Arc::new(AtomicUsize::new(0));
    let (first_entered, entered) = mpsc::sync_channel(1);
    let (release_first, first_release) = mpsc::sync_channel(1);
    let (first_result, result) = mpsc::sync_channel(1);
    let blocked_actor = Arc::clone(&first_actor);
    let blocked_executions = Arc::clone(&first_executions);
    let blocked_thread = thread::spawn(move || {
        let result = blocked_actor.block_pty_effect_for_test(
            Instant::now() + Duration::from_secs(3),
            first_entered,
            first_release,
            blocked_executions,
        );
        let _ = first_result.send(result);
    });
    entered
        .recv_timeout(Duration::from_secs(1))
        .expect("session A entered the blocking PTY-effect boundary");
    assert_eq!(first_executions.load(Ordering::Acquire), 1);

    let (listed, listed_result) = mpsc::sync_channel(1);
    let list_service = service.clone();
    let list_thread = thread::spawn(move || {
        let _ = listed.send(list_service.list());
    });
    assert_eq!(
        listed_result
            .recv_timeout(Duration::from_millis(250))
            .expect("status remains responsive")
            .expect("status succeeds")
            .len(),
        2
    );
    list_thread.join().expect("status thread joins");

    let (second_entered, second_started) = mpsc::sync_channel(1);
    let (release_second, second_release) = mpsc::sync_channel(1);
    release_second.send(()).expect("pre-release session B");
    second_actor
        .block_pty_effect_for_test(
            Instant::now() + Duration::from_millis(250),
            second_entered,
            second_release,
            Arc::new(AtomicUsize::new(0)),
        )
        .expect("session B executes while session A is blocked");
    second_started
        .recv_timeout(Duration::from_millis(250))
        .expect("session B entered its effect boundary");

    let expired_executions = Arc::new(AtomicUsize::new(0));
    let (expired_entered, expired_started) = mpsc::sync_channel(1);
    let (expired_release, expired_receiver) = mpsc::sync_channel(1);
    expired_release
        .send(())
        .expect("pre-release expired command");
    let expired = first_actor
        .block_pty_effect_for_test(
            Instant::now() + Duration::from_millis(30),
            expired_entered,
            expired_receiver,
            Arc::clone(&expired_executions),
        )
        .expect_err("queued session-A command expires independently");
    assert_eq!(expired.kind(), DomainErrorKind::DeadlineExceeded);

    release_first.send(()).expect("release session A");
    result
        .recv_timeout(Duration::from_secs(1))
        .expect("blocking session-A effect finishes")
        .expect("blocking session-A effect succeeds");
    blocked_thread.join().expect("blocked effect thread joins");

    let (fence_entered, fence_started) = mpsc::sync_channel(1);
    let (release_fence, fence_release) = mpsc::sync_channel(1);
    release_fence.send(()).expect("pre-release fence");
    first_actor
        .block_pty_effect_for_test(
            Instant::now() + Duration::from_secs(1),
            fence_entered,
            fence_release,
            Arc::new(AtomicUsize::new(0)),
        )
        .expect("post-expiration fence executes");
    fence_started
        .recv_timeout(Duration::from_millis(250))
        .expect("post-expiration fence entered");
    assert_eq!(expired_executions.load(Ordering::Acquire), 0);
    assert!(expired_started.try_recv().is_err());

    service.shutdown().expect("fixture sessions shut down");
}

#[cfg(unix)]
#[test]
fn blocked_session_does_not_prevent_remote_detach_on_another_session() {
    let temporary = tempfile::tempdir().expect("temporary remote-detach fixture");
    let working_directory = temporary.path().to_path_buf();
    let shell = [Path::new("/bin/sh"), Path::new("/usr/bin/sh")]
        .into_iter()
        .find(|path| path.is_file())
        .expect("POSIX shell fixture")
        .to_path_buf();
    let service = SessionService::with_spawner(
        DeviceId::from_array([63; 32]),
        ResourceLimits::default(),
        move |size, requested| {
            let cwd = requested.unwrap_or(&working_directory).to_path_buf();
            let session = PtyHost::new()
                .spawn(
                    ExplicitPtyCommand::new(&shell, &cwd).arg("-i"),
                    PtySize::new(size.rows, size.columns),
                )
                .map_err(map_pty_error)?;
            Ok((session, cwd))
        },
    );
    let principal = service.local_principal(AttachmentId::from_array([9; 16]));
    let operation_lease = service
        .issue_operation_lease(principal)
        .expect("fixture operation lease");
    let create = |sequence, name: &str| {
        service
            .create(
                principal,
                OperationId {
                    lease: operation_lease,
                    sequence,
                },
                SessionName::new(name).expect("test session name"),
                None,
                None,
            )
            .expect("test session creates")
    };
    let blocked = create(1, "blocked-a");
    let responsive = create(2, "responsive-b");
    let blocked_actor = service
        .resolve(&SessionSelector::Id(blocked.session_id))
        .expect("blocked actor resolves");

    // One remote endpoint becomes the controller of both sessions. The
    // prepared handles stay alive so their actor attachments are not reaped
    // as detached before the detach under test.
    let remote = AttachmentPrincipal::RemoteEndpoint {
        device_id: DeviceId::from_array([0x63; 32]),
        auth_generation: 1,
    };
    let _blocked_attach = service
        .prepare_attach(
            remote,
            Some(SessionSelector::Id(blocked.session_id)),
            false,
            false,
            None,
        )
        .expect("blocked session remote attach succeeds");
    let _responsive_attach = service
        .prepare_attach(
            remote,
            Some(SessionSelector::Id(responsive.session_id)),
            false,
            false,
            None,
        )
        .expect("responsive session remote attach succeeds");

    // Deterministically block session A inside its PTY-effect boundary.
    let (entered, entered_rx) = mpsc::sync_channel(1);
    let (release, release_rx) = mpsc::sync_channel(1);
    let blocked_actor_thread = Arc::clone(&blocked_actor);
    let block_thread = thread::spawn(move || {
        blocked_actor_thread.block_pty_effect_for_test(
            Instant::now() + Duration::from_secs(3),
            entered,
            release_rx,
            Arc::new(AtomicUsize::new(0)),
        )
    });
    entered_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("session A entered its blocking boundary");

    // Detach the remote endpoint in a background thread. Phase one admits a
    // command to every actor before phase two waits, so the responsive
    // session must be released even while A is still blocked. A test-only
    // observer fires inside the responsive actor after it has processed its
    // detach command, giving a deterministic barrier instead of a poll.
    let responsive_id = responsive.session_id;
    let (processed_tx, processed_rx) = mpsc::sync_channel(1);
    let detach_service = service.clone();
    let (impact_tx, impact_rx) = mpsc::sync_channel(1);
    let detach_thread = thread::spawn(move || {
        let impact = detach_service.detach_remote_principal_until_observed(
            DeviceId::from_array([0x63; 32]),
            Instant::now() + Duration::from_secs(5),
            move |session_id| (session_id == responsive_id).then(|| processed_tx.clone()),
        );
        let _ = impact_tx.send(impact);
    });

    processed_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("responsive actor processed its detach while session A stayed blocked");

    // The observer fires after the actor refreshes its cached summary, so a
    // single synchronous read (no polling) is deterministic evidence.
    let controller_of = |session_id: SessionId| {
        service
            .list()
            .expect("list succeeds")
            .into_iter()
            .any(|summary| summary.session_id == session_id && summary.has_controller)
    };
    assert!(
        !controller_of(responsive_id),
        "responsive session remote controller was not released while session A stayed blocked"
    );
    assert!(
        controller_of(blocked.session_id),
        "blocked session controller must remain attached until its actor is released"
    );

    release.send(()).expect("release session A");
    let impact = impact_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("remote detach completes after session A releases")
        .expect("remote detach succeeds");
    assert_eq!(impact.sessions_affected, 2);
    assert_eq!(impact.attachments_removed, 2);
    assert_eq!(impact.controllers_released, 2);
    block_thread
        .join()
        .expect("blocked effect thread joins")
        .expect("blocked effect succeeds");
    detach_thread.join().expect("detach thread joins");

    service.shutdown().expect("fixture sessions shut down");
}

#[cfg(unix)]
#[test]
fn shutdown_is_concurrent_bounded_and_truthful_until_owned_children_are_reaped() {
    let temporary = tempfile::tempdir().expect("temporary shutdown fixture");
    let working_directory = temporary.path().to_path_buf();
    let shell = [Path::new("/bin/sh"), Path::new("/usr/bin/sh")]
        .into_iter()
        .find(|path| path.is_file())
        .expect("POSIX shell fixture")
        .to_path_buf();
    let spawn_index = Arc::new(AtomicUsize::new(0));
    let process_ids = Arc::new(Mutex::new(Vec::new()));
    let service =
        SessionService::with_spawner(DeviceId::from_array([62; 32]), ResourceLimits::default(), {
            let spawn_index = Arc::clone(&spawn_index);
            let process_ids = Arc::clone(&process_ids);
            move |size, requested| {
                let cwd = requested.unwrap_or(&working_directory).to_path_buf();
                let command = if spawn_index.fetch_add(1, Ordering::AcqRel) == 0 {
                    ExplicitPtyCommand::new(&shell, &cwd)
                        .arg("-c")
                        .arg("trap '' HUP; printf 'STUCK-READY\\r\\n'; while :; do :; done")
                } else {
                    ExplicitPtyCommand::new(&shell, &cwd)
                        .arg("-c")
                        .arg("exec /bin/sleep 30")
                };
                let session = PtyHost::new()
                    .spawn(command, PtySize::new(size.rows, size.columns))
                    .map_err(map_pty_error)?;
                process_ids
                    .lock()
                    .map_err(|_| synchronization_error("shutdown fixture process IDs"))?
                    .push(session.process_id().expect("fixture process id"));
                Ok((session, cwd))
            }
        });
    let principal = service.local_principal(AttachmentId::from_array([8; 16]));
    let operation_lease = service
        .issue_operation_lease(principal)
        .expect("fixture operation lease");
    let stuck = service
        .create(
            principal,
            OperationId {
                lease: operation_lease,
                sequence: 1,
            },
            SessionName::new("stuck-a").expect("valid stuck name"),
            None,
            None,
        )
        .expect("stuck fixture creates");
    let fast = service
        .create(
            principal,
            OperationId {
                lease: operation_lease,
                sequence: 2,
            },
            SessionName::new("fast-b").expect("valid fast name"),
            None,
            None,
        )
        .expect("fast fixture creates");
    let stuck_actor = service
        .inner
        .resolve(&SessionSelector::Id(stuck.session_id))
        .expect("stuck actor remains addressable");
    let fast_actor = service
        .inner
        .resolve(&SessionSelector::Id(fast.session_id))
        .expect("fast actor remains addressable");
    let prepared = service
        .prepare_attach(
            principal,
            Some(SessionSelector::Id(stuck.session_id)),
            false,
            false,
            None,
        )
        .expect("stuck fixture attaches");
    let ready_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = prepared
            .attachment
            .sync_latest(Revision::ZERO)
            .expect("stuck fixture snapshot");
        if terminal_snapshot_contains(&snapshot, b"STUCK-READY") {
            break;
        }
        assert!(
            Instant::now() < ready_deadline,
            "HUP-resistant child never installed its handler"
        );
        thread::sleep(Duration::from_millis(5));
    }

    let process_ids = process_ids
        .lock()
        .expect("shutdown fixture process IDs")
        .clone();
    let stuck_pid = i32::try_from(process_ids[0]).expect("stuck pid fits pid_t");
    let fast_pid = i32::try_from(process_ids[1]).expect("fast pid fits pid_t");
    let started = Instant::now();
    let error = service
        .shutdown_until(Instant::now() + Duration::from_millis(20))
        .expect_err("shutdown cannot claim success while the resistant child is owned");
    assert_eq!(error.kind(), DomainErrorKind::DeadlineExceeded);
    assert!(started.elapsed() < Duration::from_millis(150));
    assert!(error.detail().contains("stuck-a"));
    assert!(process_exists(stuck_pid));
    assert_eq!(
        stuck_actor.requested_end_reason(),
        Some(SessionEndReason::DaemonStop)
    );
    assert_eq!(
        fast_actor.requested_end_reason(),
        Some(SessionEndReason::DaemonStop),
        "shutdown must request every owner before waiting for cleanup"
    );
    assert!(
        service
            .list()
            .expect("status remains available after failed shutdown")
            .iter()
            .any(|summary| summary.session_id == stuck.session_id)
    );

    let completion_deadline = Instant::now() + Duration::from_secs(3);
    while !service.list().expect("poll shutdown completion").is_empty()
        && Instant::now() < completion_deadline
    {
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        service.list().expect("final shutdown status").is_empty(),
        "the original concurrent shutdown did not finish after escalation"
    );
    assert!(!process_exists(stuck_pid));
    assert!(!process_exists(fast_pid));
    assert!(
        service
            .shutdown_until(Instant::now() + Duration::from_secs(1))
            .expect("a retry observes fully released ownership")
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn publication_loss_after_spawn_closes_the_child_and_releases_the_reservation() {
    let temporary = tempfile::tempdir().expect("temporary publication-loss fixture");
    let working_directory = temporary.path().to_path_buf();
    let shell = [Path::new("/bin/sh"), Path::new("/usr/bin/sh")]
        .into_iter()
        .find(|path| path.is_file())
        .expect("POSIX shell fixture")
        .to_path_buf();
    let (spawned, spawned_child) = mpsc::sync_channel(1);
    let (release_spawn, release) = mpsc::sync_channel(1);
    let release = Arc::new(Mutex::new(release));
    let service =
        SessionService::with_spawner(DeviceId::from_array([63; 32]), ResourceLimits::default(), {
            let release = Arc::clone(&release);
            move |size, requested| {
                let cwd = requested.unwrap_or(&working_directory).to_path_buf();
                let session = PtyHost::new()
                    .spawn(
                        ExplicitPtyCommand::new(&shell, &cwd)
                            .arg("-c")
                            .arg("exec /bin/sleep 30"),
                        PtySize::new(size.rows, size.columns),
                    )
                    .map_err(map_pty_error)?;
                spawned
                    .send(session.process_id().expect("fixture process id"))
                    .map_err(|_| synchronization_error("spawn observer"))?;
                release
                    .lock()
                    .map_err(|_| synchronization_error("spawn release"))?
                    .recv()
                    .map_err(|_| synchronization_error("spawn release"))?;
                Ok((session, cwd))
            }
        });
    let principal = service.local_principal(AttachmentId::from_array([9; 16]));
    let operation_lease = service
        .issue_operation_lease(principal)
        .expect("fixture operation lease");
    let create_service = service.clone();
    let (created, create_result) = mpsc::sync_channel(1);
    let create_thread = thread::spawn(move || {
        let result = create_service.create(
            principal,
            OperationId {
                lease: operation_lease,
                sequence: 1,
            },
            SessionName::new("publication-loss").expect("valid fixture name"),
            None,
            None,
        );
        let _ = created.send(result);
    });
    let process_id = i32::try_from(
        spawned_child
            .recv_timeout(Duration::from_secs(1))
            .expect("child spawned behind the provisional reservation"),
    )
    .expect("fixture pid fits pid_t");

    let stop = service
        .shutdown_until(Instant::now() + Duration::from_millis(20))
        .expect_err("provisional child ownership prevents a truthful stop");
    assert_eq!(stop.kind(), DomainErrorKind::DeadlineExceeded);
    assert!(process_exists(process_id));
    assert!(
        service
            .list()
            .expect("diagnostic list remains available")
            .is_empty()
    );

    release_spawn.send(()).expect("release spawned session");
    let error = create_result
        .recv_timeout(Duration::from_secs(2))
        .expect("cancelled create finishes cleanup")
        .expect_err("cancelled publication cannot become live");
    assert_eq!(error.kind(), DomainErrorKind::Cancelled);
    create_thread.join().expect("create thread joins");
    assert!(!process_exists(process_id));
    assert!(
        service
            .list()
            .expect("no half-published session")
            .is_empty()
    );
    assert!(
        service
            .shutdown_until(Instant::now() + Duration::from_secs(1))
            .expect("cleanup releases the provisional resource")
            .is_empty()
    );
}

#[cfg(unix)]
#[test]
fn creation_actor_and_close_panics_leave_retriable_owned_state() {
    let temporary = tempfile::tempdir().expect("temporary unwind fixture");
    let working_directory = temporary.path().to_path_buf();
    let shell = [Path::new("/bin/sh"), Path::new("/usr/bin/sh")]
        .into_iter()
        .find(|path| path.is_file())
        .expect("POSIX shell fixture")
        .to_path_buf();
    let process_ids = Arc::new(Mutex::new(Vec::new()));
    let service =
        SessionService::with_spawner(DeviceId::from_array([64; 32]), ResourceLimits::default(), {
            let process_ids = Arc::clone(&process_ids);
            move |size, requested| {
                let cwd = requested.unwrap_or(&working_directory).to_path_buf();
                let session = PtyHost::new()
                    .spawn(
                        ExplicitPtyCommand::new(&shell, &cwd)
                            .arg("-c")
                            .arg("exec /bin/sleep 30"),
                        PtySize::new(size.rows, size.columns),
                    )
                    .map_err(map_pty_error)?;
                process_ids
                    .lock()
                    .map_err(|_| synchronization_error("unwind fixture process IDs"))?
                    .push(session.process_id().expect("fixture process id"));
                Ok((session, cwd))
            }
        });
    let principal = service.local_principal(AttachmentId::from_array([10; 16]));
    let lease = service
        .issue_operation_lease(principal)
        .expect("unwind fixture lease");

    service.panic_next_creation_after_spawn_for_test();
    let creation = service
        .create(
            principal,
            OperationId { lease, sequence: 1 },
            SessionName::new("creation-panic").expect("fixture name"),
            None,
            None,
        )
        .expect_err("creation panic is not hidden as success");
    assert_eq!(creation.kind(), DomainErrorKind::OperationOutcomeUnknown);
    assert!(
        service
            .list()
            .expect("registry after creation panic")
            .is_empty()
    );
    assert_eq!(service.inner.reservation_count().expect("reservations"), 0);

    let actor_summary = service
        .create(
            principal,
            OperationId { lease, sequence: 2 },
            SessionName::new("actor-panic").expect("fixture name"),
            None,
            None,
        )
        .expect("actor panic fixture creates");
    let actor = service
        .resolve(&SessionSelector::Id(actor_summary.session_id))
        .expect("actor resolves");
    actor
        .panic_worker_for_test(Instant::now() + Duration::from_secs(2))
        .expect("actor worker panic finalizes");
    actor.join_finished().expect("panicked actor thread joins");
    assert!(
        service
            .list()
            .expect("registry after actor panic")
            .is_empty()
    );
    assert_eq!(service.inner.reservation_count().expect("reservations"), 0);

    let close_summary = service
        .create(
            principal,
            OperationId { lease, sequence: 3 },
            SessionName::new("close-panic").expect("fixture name"),
            None,
            None,
        )
        .expect("close panic fixture creates");
    let close_actor = service
        .resolve(&SessionSelector::Id(close_summary.session_id))
        .expect("close actor resolves");
    close_actor.panic_next_close_for_test();
    close_actor.begin_end(SessionEndReason::ExplicitClose);
    let first_close = close_actor
        .wait_finished_until(Instant::now() + Duration::from_secs(1))
        .expect_err("close panic is surfaced");
    assert_eq!(first_close.kind(), DomainErrorKind::OperationOutcomeUnknown);
    close_actor.begin_end(SessionEndReason::ExplicitClose);
    close_actor
        .wait_finished_until(Instant::now() + Duration::from_secs(2))
        .expect("close retry releases child");
    close_actor.join_finished().expect("close actor joins");
    assert!(
        service
            .list()
            .expect("registry after close retry")
            .is_empty()
    );
    assert_eq!(service.inner.reservation_count().expect("reservations"), 0);

    let poisoned = service
        .create(
            principal,
            OperationId { lease, sequence: 4 },
            SessionName::new("summary-panic").expect("fixture name"),
            None,
            None,
        )
        .expect("summary panic fixture creates");
    service
        .create(
            principal,
            OperationId { lease, sequence: 5 },
            SessionName::new("summary-peer").expect("fixture name"),
            None,
            None,
        )
        .expect("summary peer creates");
    let poison_actor = service
        .resolve(&SessionSelector::Id(poisoned.session_id))
        .expect("poison actor resolves");
    let poison = thread::spawn(move || {
        let _cached = poison_actor.cached.lock().expect("summary cache locks");
        panic!("injected summary-cache poison");
    });
    assert!(poison.join().is_err());
    let shutdown = service
        .shutdown_until(Instant::now() + Duration::from_secs(2))
        .expect_err("typed summary failure is surfaced after closing every owner");
    assert_eq!(shutdown.kind(), DomainErrorKind::StoreUnavailable);
    assert!(
        service
            .inner
            .owned_entries()
            .expect("owned entries")
            .is_empty()
    );
    assert_eq!(service.inner.reservation_count().expect("reservations"), 0);

    let retry = service
        .create(
            principal,
            OperationId { lease, sequence: 6 },
            SessionName::new("after-summary-error").expect("fixture name"),
            None,
            None,
        )
        .expect("failed shutdown resumes mutation admission");
    service
        .close(
            principal,
            OperationId { lease, sequence: 7 },
            retry.session_id,
        )
        .expect("cleanup retry fixture closes");

    for process_id in process_ids.lock().expect("process IDs").iter().copied() {
        let process_id = i32::try_from(process_id).expect("pid fits pid_t");
        assert!(
            !process_exists(process_id),
            "child {process_id} survived unwind cleanup"
        );
    }
}

#[cfg(unix)]
#[test]
fn session_id_collision_never_overwrites_an_existing_owner_reservation() {
    let temporary = tempfile::tempdir().expect("temporary ID-collision fixture");
    let service = unix_fixture_service(
        DeviceId::from_array([70; 32]),
        temporary.path().to_path_buf(),
        "exec /bin/sleep 30",
    );
    let principal = service.local_principal(AttachmentId::from_array([70; 16]));
    let lease = service
        .issue_operation_lease(principal)
        .expect("collision fixture lease");
    let first = service
        .create(
            principal,
            OperationId { lease, sequence: 1 },
            SessionName::new("collision-first").expect("first name"),
            None,
            None,
        )
        .expect("first owner creates");
    let replacement = SessionId::from_array([0x5a; 16]);
    assert_ne!(first.session_id, replacement);
    service
        .inner
        .inject_session_id_candidates([first.session_id, replacement]);
    let second = service
        .create(
            principal,
            OperationId { lease, sequence: 2 },
            SessionName::new("collision-second").expect("second name"),
            None,
            None,
        )
        .expect("collision candidate is skipped atomically");
    assert_eq!(second.session_id, replacement);
    assert_eq!(service.inner.reservation_count().expect("reservations"), 2);

    service
        .close(
            principal,
            OperationId { lease, sequence: 3 },
            first.session_id,
        )
        .expect("first owner closes");
    assert_eq!(service.inner.reservation_count().expect("reservations"), 1);
    assert!(
        service
            .resolve(&SessionSelector::Id(second.session_id))
            .is_ok()
    );
    service
        .close(
            principal,
            OperationId { lease, sequence: 4 },
            second.session_id,
        )
        .expect("second owner closes");
    assert_eq!(service.inner.reservation_count().expect("reservations"), 0);
}

#[cfg(unix)]
#[test]
fn shutdown_joins_each_cleanup_owner_even_when_ids_conflict() {
    let temporary = tempfile::tempdir().expect("temporary cleanup-owner fixture");
    let working_directory = temporary.path().to_path_buf();
    let service = unix_fixture_service(
        DeviceId::from_array([74; 32]),
        working_directory.clone(),
        "exec /bin/sleep 30",
    );
    let principal = service.local_principal(AttachmentId::from_array([74; 16]));
    let lease = service
        .issue_operation_lease(principal)
        .expect("cleanup-owner lease");
    let first = service
        .create(
            principal,
            OperationId { lease, sequence: 1 },
            SessionName::new("collision-live").expect("live name"),
            None,
            None,
        )
        .expect("live owner creates");
    let first_actor = service
        .resolve(&SessionSelector::Id(first.session_id))
        .expect("live actor resolves");

    // Model the defensive cleanup-only state used after an impossible
    // poisoned/corrupt registration collision: it has its own actor/token
    // but the externally supplied ID conflicts with a live owner.
    let size = TerminalSize::new(24, 80);
    let model = TerminalModel::new(size, ResourceLimits::default().recent_history_rows)
        .expect("cleanup-owner model");
    let shell = [Path::new("/bin/sh"), Path::new("/usr/bin/sh")]
        .into_iter()
        .find(|path| path.is_file())
        .expect("POSIX shell fixture");
    let session = PtyHost::new()
        .spawn(
            ExplicitPtyCommand::new(shell, &working_directory)
                .arg("-c")
                .arg("exec /bin/sleep 30"),
            PtySize::new(size.rows, size.columns),
        )
        .expect("cleanup-only PTY spawns");
    let driver = TerminalDriver::start(session, model, TerminalDriverConfig::default())
        .expect("cleanup-only driver starts");
    let cleanup_actor = SessionActor::start(
        first.session_id,
        size,
        driver,
        Arc::downgrade(&service.inner),
        ResourceLimits::default(),
    )
    .expect("cleanup-only actor starts");
    cleanup_actor.mark_registry_owned();
    cleanup_lock(&service.inner.state)
        .cleanup_only
        .push(SessionEntry {
            actor: Arc::clone(&cleanup_actor),
            name: SessionName::new("collision-cleanup").expect("cleanup name"),
            working_directory,
            ownership: OwnershipToken::new(),
        });

    service
        .shutdown_until(Instant::now() + Duration::from_secs(2))
        .expect("both conflicting owners shut down");
    assert!(
        cleanup_lock(&first_actor.worker).is_none(),
        "live actor handle was joined"
    );
    assert!(
        cleanup_lock(&cleanup_actor.worker).is_none(),
        "cleanup-only actor handle was joined independently"
    );
}

#[cfg(unix)]
#[test]
fn failed_publication_retains_name_until_timeout_cleanup_reaps_the_actor() {
    let temporary = tempfile::tempdir().expect("temporary cleanup-timeout fixture");
    let working_directory = temporary.path().to_path_buf();
    let shell = [Path::new("/bin/sh"), Path::new("/usr/bin/sh")]
        .into_iter()
        .find(|path| path.is_file())
        .expect("POSIX shell fixture")
        .to_path_buf();
    let service = SessionService::with_spawner(
        DeviceId::from_array([71; 32]),
        ResourceLimits::default(),
        move |size, requested| {
            let cwd = requested.unwrap_or(&working_directory).to_path_buf();
            let session = PtyHost::new()
                .spawn(
                    ExplicitPtyCommand::new(&shell, &cwd)
                        .arg("-c")
                        .arg("trap '' HUP; : > .cleanup-timeout-ready; while :; do :; done"),
                    PtySize::new(size.rows, size.columns),
                )
                .map_err(map_pty_error)?;
            let ready = cwd.join(".cleanup-timeout-ready");
            let ready_deadline = Instant::now() + Duration::from_secs(2);
            while !ready.is_file() {
                if Instant::now() >= ready_deadline {
                    return Err(synchronization_error("cleanup-timeout child readiness"));
                }
                thread::sleep(Duration::from_millis(2));
            }
            Ok((session, cwd))
        },
    );
    let principal = service.local_principal(AttachmentId::from_array([71; 16]));
    let lease = service
        .issue_operation_lease(principal)
        .expect("cleanup-timeout lease");
    let name = SessionName::new("cleanup-timeout").expect("fixture name");
    service.inner.fail_next_provisional_registration();
    let error = service
        .create_until(
            principal,
            OperationId { lease, sequence: 1 },
            name.clone(),
            None,
            None,
            Instant::now() + Duration::from_millis(20),
        )
        .expect_err("bounded creation cleanup times out");
    assert_eq!(error.kind(), DomainErrorKind::DeadlineExceeded);
    assert_eq!(service.inner.owned_entries().expect("owned actor").len(), 1);
    assert_eq!(service.inner.reservation_count().expect("reservation"), 1);
    let duplicate = service
        .create(
            principal,
            OperationId { lease, sequence: 2 },
            name.clone(),
            None,
            None,
        )
        .expect_err("starting name remains unavailable during cleanup");
    assert_eq!(duplicate.kind(), DomainErrorKind::SessionAlreadyExists);

    let deadline = Instant::now() + Duration::from_secs(3);
    while service.inner.reservation_count().expect("poll reservation") != 0 {
        assert!(
            Instant::now() < deadline,
            "cleanup never released ownership"
        );
        thread::sleep(Duration::from_millis(5));
    }
    let replacement = service
        .create(
            principal,
            OperationId { lease, sequence: 3 },
            name,
            None,
            None,
        )
        .expect("same name is admitted after actual cleanup");
    service
        .close(
            principal,
            OperationId { lease, sequence: 4 },
            replacement.session_id,
        )
        .expect("replacement closes");
}

#[cfg(unix)]
#[test]
fn failed_publication_cleanup_error_keeps_a_retriable_provisional_owner() {
    let temporary = tempfile::tempdir().expect("temporary cleanup-error fixture");
    let service = unix_fixture_service(
        DeviceId::from_array([72; 32]),
        temporary.path().to_path_buf(),
        "exec /bin/sleep 30",
    );
    let principal = service.local_principal(AttachmentId::from_array([72; 16]));
    let lease = service
        .issue_operation_lease(principal)
        .expect("cleanup-error lease");
    let name = SessionName::new("cleanup-error").expect("fixture name");
    service.inner.fail_next_provisional_registration();
    service.panic_next_creation_cleanup_for_test();
    let error = service
        .create(
            principal,
            OperationId { lease, sequence: 1 },
            name.clone(),
            None,
            None,
        )
        .expect_err("cleanup panic is surfaced");
    assert_eq!(error.kind(), DomainErrorKind::OperationOutcomeUnknown);
    let owned = service.inner.owned_entries().expect("retriable owner");
    assert_eq!(owned.len(), 1);
    assert_eq!(service.inner.reservation_count().expect("reservation"), 1);
    let duplicate = service
        .create(
            principal,
            OperationId { lease, sequence: 2 },
            name.clone(),
            None,
            None,
        )
        .expect_err("failed cleanup retains its name");
    assert_eq!(duplicate.kind(), DomainErrorKind::SessionAlreadyExists);

    owned[0].actor.begin_end(SessionEndReason::DriverFailure);
    owned[0]
        .actor
        .wait_finished_until(Instant::now() + Duration::from_secs(2))
        .expect("explicit cleanup retry reaps actor");
    owned[0].actor.join_finished().expect("cleanup actor joins");
    assert_eq!(service.inner.reservation_count().expect("reservation"), 0);
    let replacement = service
        .create(
            principal,
            OperationId { lease, sequence: 3 },
            name,
            None,
            None,
        )
        .expect("same name is admitted only after retry cleanup");
    service
        .close(
            principal,
            OperationId { lease, sequence: 4 },
            replacement.session_id,
        )
        .expect("replacement closes");
}

#[cfg(unix)]
#[test]
fn poisoned_registry_name_and_resource_locks_reclaim_only_matching_owner() {
    let temporary = tempfile::tempdir().expect("temporary poison fixture");
    let service = unix_fixture_service(
        DeviceId::from_array([73; 32]),
        temporary.path().to_path_buf(),
        "exec /bin/sleep 30",
    );
    let principal = service.local_principal(AttachmentId::from_array([73; 16]));
    let lease = service
        .issue_operation_lease(principal)
        .expect("poison fixture lease");
    let first = service
        .create(
            principal,
            OperationId { lease, sequence: 1 },
            SessionName::new("poison-first").expect("first name"),
            None,
            None,
        )
        .expect("first creates");
    let second = service
        .create(
            principal,
            OperationId { lease, sequence: 2 },
            SessionName::new("poison-second").expect("second name"),
            None,
            None,
        )
        .expect("second creates");
    let first_actor = service
        .resolve(&SessionSelector::Id(first.session_id))
        .expect("first actor");

    let registry = Arc::clone(&service.inner);
    assert!(
        thread::spawn(move || {
            let _state = registry.state.lock().expect("registry/name lock");
            panic!("inject registry/name ownership poison");
        })
        .join()
        .is_err()
    );
    assert_eq!(
        service
            .list()
            .expect_err("ordinary API surfaces registry poison")
            .kind(),
        DomainErrorKind::StoreUnavailable
    );
    let registry = Arc::clone(&service.inner);
    assert!(
        thread::spawn(move || {
            let _reservations = registry.reservations.lock().expect("reservation lock");
            panic!("inject reservation ownership poison");
        })
        .join()
        .is_err()
    );

    first_actor.begin_end(SessionEndReason::ExplicitClose);
    first_actor
        .wait_finished_until(Instant::now() + Duration::from_secs(2))
        .expect("poison-aware finalizer closes first");
    first_actor.join_finished().expect("first actor joins");
    assert_eq!(service.inner.reservation_count().expect("reservations"), 1);
    assert!(
        service
            .resolve(&SessionSelector::Id(first.session_id))
            .is_err()
    );
    assert!(
        service
            .resolve(&SessionSelector::Id(second.session_id))
            .is_ok()
    );
    assert_eq!(
        service.list().expect("unrelated owner remains")[0].name,
        SessionName::new("poison-second").expect("second name")
    );
    service
        .close(
            principal,
            OperationId { lease, sequence: 3 },
            second.session_id,
        )
        .expect("unrelated owner closes normally");
}

#[cfg(unix)]
#[test]
fn reserve_complete_and_shutdown_share_one_non_deadlocking_lock_order() {
    let temporary = tempfile::tempdir().expect("temporary lock-order fixture");
    let cwd = temporary.path().to_path_buf();
    let shell = [Path::new("/bin/sh"), Path::new("/usr/bin/sh")]
        .into_iter()
        .find(|path| path.is_file())
        .expect("POSIX shell fixture")
        .to_path_buf();
    let spawn_count = Arc::new(AtomicUsize::new(0));
    let (reserved, observed_reserved) = mpsc::sync_channel(1);
    let (release, released) = mpsc::sync_channel(1);
    let released = Arc::new(Mutex::new(released));
    let service =
        SessionService::with_spawner(DeviceId::from_array([74; 32]), ResourceLimits::default(), {
            let spawn_count = Arc::clone(&spawn_count);
            let released = Arc::clone(&released);
            move |size, requested| {
                let actual = requested.unwrap_or(&cwd).to_path_buf();
                if spawn_count.fetch_add(1, Ordering::AcqRel) == 1 {
                    reserved
                        .send(())
                        .map_err(|_| synchronization_error("reserve observer"))?;
                    released
                        .lock()
                        .map_err(|_| synchronization_error("reserve release"))?
                        .recv()
                        .map_err(|_| synchronization_error("reserve release"))?;
                }
                let session = PtyHost::new()
                    .spawn(
                        ExplicitPtyCommand::new(&shell, &actual)
                            .arg("-c")
                            .arg("exec /bin/sleep 30"),
                        PtySize::new(size.rows, size.columns),
                    )
                    .map_err(map_pty_error)?;
                Ok((session, actual))
            }
        });
    let principal = service.local_principal(AttachmentId::from_array([74; 16]));
    let lease = service
        .issue_operation_lease(principal)
        .expect("lock-order lease");
    let first = service
        .create(
            principal,
            OperationId { lease, sequence: 1 },
            SessionName::new("lock-live").expect("live name"),
            None,
            None,
        )
        .expect("live fixture creates");

    let create_service = service.clone();
    let (create_done, create_result) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = create_service.create(
            principal,
            OperationId { lease, sequence: 2 },
            SessionName::new("lock-starting").expect("starting name"),
            None,
            None,
        );
        let _ = create_done.send(result);
    });
    observed_reserved
        .recv_timeout(Duration::from_secs(1))
        .expect("second creation owns its reservation");

    let close_service = service.clone();
    let (close_done, close_result) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = close_service.close(
            principal,
            OperationId { lease, sequence: 3 },
            first.session_id,
        );
        let _ = close_done.send(result);
    });
    let shutdown_service = service.clone();
    let (shutdown_done, shutdown_result) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let result = shutdown_service.shutdown_until(Instant::now() + Duration::from_secs(3));
        let _ = shutdown_done.send(result);
    });
    release.send(()).expect("release reserved creation");

    let create = create_result
        .recv_timeout(Duration::from_secs(3))
        .expect("reserve path never deadlocks");
    assert!(
        create.is_ok()
            || create
                .as_ref()
                .is_err_and(|error| error.kind() == DomainErrorKind::Cancelled)
    );
    let close = close_result
        .recv_timeout(Duration::from_secs(3))
        .expect("completion path never deadlocks");
    assert!(
        close.is_ok()
            || close
                .as_ref()
                .is_err_and(|error| error.kind() == DomainErrorKind::SessionNotFound)
    );
    shutdown_result
        .recv_timeout(Duration::from_secs(3))
        .expect("shutdown path never deadlocks")
        .expect("shutdown releases all owners");
    assert_eq!(service.inner.reservation_count().expect("reservations"), 0);
}

#[cfg(unix)]
async fn wait_for_attachment_text(prepared: &PreparedAttachment, expected: &[u8]) -> Revision {
    if terminal_snapshot_contains(&prepared.snapshot, expected) {
        return prepared.snapshot.revision;
    }
    let mut revisions = prepared
        .attachment
        .revision_watch()
        .expect("fixture revision watermark");
    let mut baseline = prepared.snapshot.revision;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            while *revisions.borrow_and_update() <= baseline {
                revisions
                    .changed()
                    .await
                    .expect("driver revision stays open");
            }
            let snapshot = prepared
                .attachment
                .sync_latest(baseline)
                .expect("force one deterministic fixture snapshot");
            baseline = snapshot.revision;
            let found = terminal_snapshot_contains(&snapshot, expected);
            assert!(
                prepared
                    .attachment
                    .snapshot_applied(snapshot.revision)
                    .expect("acknowledge deterministic fixture snapshot")
                    .is_none()
            );
            if found {
                return baseline;
            }
        }
    })
    .await
    .expect("fixture output synchronization marker reached the authoritative terminal")
}

#[cfg(unix)]
fn terminal_snapshot_contains(snapshot: &TerminalSurfaceSnapshot, expected: &[u8]) -> bool {
    let Ok(expected) = std::str::from_utf8(expected) else {
        return false;
    };
    snapshot.surface.rows.iter().any(|row| {
        row.cells
            .iter()
            .map(|cell| cell.contents.as_str())
            .collect::<String>()
            .contains(expected)
    })
}

#[cfg(unix)]
fn terminal_delta_contains(delta: &TerminalSurfaceDelta, expected: &str) -> bool {
    delta.row_patches.iter().any(|patch| {
        patch
            .replacement
            .cells
            .iter()
            .map(|cell| cell.contents.as_str())
            .collect::<String>()
            .contains(expected)
    })
}

#[cfg(unix)]
fn unix_fixture_service(
    device_id: DeviceId,
    working_directory: PathBuf,
    script: &'static str,
) -> SessionService {
    let shell = [Path::new("/bin/sh"), Path::new("/usr/bin/sh")]
        .into_iter()
        .find(|path| path.is_file())
        .expect("POSIX shell fixture")
        .to_path_buf();
    SessionService::with_spawner(
        device_id,
        ResourceLimits::default(),
        move |size, requested| {
            let cwd = requested.unwrap_or(&working_directory).to_path_buf();
            let session = PtyHost::new()
                .spawn(
                    ExplicitPtyCommand::new(&shell, &cwd).arg("-c").arg(script),
                    PtySize::new(size.rows, size.columns),
                )
                .map_err(map_pty_error)?;
            Ok((session, cwd))
        },
    )
}

#[cfg(unix)]
fn process_exists(process_id: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(process_id), None).is_ok()
}
