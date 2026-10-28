//! Native Windows evidence for the SID boundary, named-pipe FIN, and ConPTY.
#![cfg(windows)]
use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use voytty_platform::local::{AsyncListener, Stream};
use voytty_platform::pty::{ExplicitPtyCommand, PtyChildState, PtyHost, PtySize};
use voytty_platform::user_state::{FileLock, UserPaths, atomic_write, validate_regular_file};

#[tokio::test]
async fn named_pipe_preserves_large_writes_directional_fin_and_drop_delivery() {
    let temporary = tempfile::tempdir().expect("fixture");
    let endpoint = temporary.path().join("pipe");
    let listener = AsyncListener::bind(&endpoint).expect("private listener");
    let task = tokio::spawn(async move {
        let (mut peer, ()) = listener.accept().await.expect("accept");
        let mut bytes = Vec::new();
        peer.read_to_end(&mut bytes)
            .await
            .expect("read through client FIN");
        assert_eq!(bytes, vec![42; 180_000]);
        peer.write_all(b"host completion after FIN")
            .await
            .expect("reply");
        // Drop must preserve bytes already accepted by write_all.
    });
    let mut client = Stream::connect(&endpoint)
        .await
        .expect("authenticated connect");
    client
        .write_all(&vec![42; 180_000])
        .await
        .expect("large write");
    client.shutdown().await.expect("client FIN");
    let mut reply = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), client.read_to_end(&mut reply))
        .await
        .expect("bounded reply")
        .expect("read response");
    assert_eq!(reply, b"host completion after FIN");
    task.await.expect("server");
}

#[test]
fn private_state_acl_lock_atomic_replace_and_foreign_grants() {
    let temporary = tempfile::tempdir().expect("fixture");
    let paths = UserPaths::for_test(
        0,
        temporary.path().to_owned(),
        temporary.path().join("state"),
        temporary.path().join("run"),
    );
    std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| paths.prepare_state_directories()))
            .collect();
        for task in tasks {
            task.join()
                .expect("startup worker")
                .expect("concurrent private state creation");
        }
    });
    atomic_write(paths.config(), 0, |file| file.write_all(b"old")).expect("initial bytes");
    atomic_write(paths.config(), 0, |file| file.write_all(b"new")).expect("atomic replacement");
    assert_eq!(std::fs::read(paths.config()).expect("read"), b"new");
    // Windows flushes file handles, while directory flushing is a Unix boundary.
    use voytty_diagnostics::Sink;
    let diagnostics = voytty_platform::diagnostics::Store::new(paths.clone());
    diagnostics
        .append(false, b"private diagnostic fixture\n")
        .expect("diagnostic append");
    diagnostics.flush().expect("diagnostic durability barrier");
    let first = FileLock::try_acquire(paths.lifecycle_lock(), 0)
        .expect("lock")
        .expect("first owner");
    assert!(
        FileLock::try_acquire(paths.lifecycle_lock(), 0)
            .expect("second probe")
            .is_none()
    );
    drop(first);
    assert!(
        FileLock::try_acquire(paths.lifecycle_lock(), 0)
            .expect("released")
            .is_some()
    );
    let executable = paths.config().with_file_name("voytty.exe");
    voytty_platform::user_state::install_executable(paths.config(), &executable, 0)
        .expect("private executable install");
    assert!(
        voytty_platform::user_state::install_executable(paths.config(), &executable, 0).is_err()
    );
    let activation = voytty_platform::user_state::activate_executable(&executable, 0, |file| {
        file.write_all(b"candidate")
    })
    .expect("activate");
    assert_eq!(
        std::fs::read(&executable).expect("activated bytes"),
        b"candidate"
    );
    activation.rollback().expect("rollback");
    assert_eq!(std::fs::read(&executable).expect("restored bytes"), b"new");
    voytty_platform::user_state::activate_executable(&executable, 0, |file| {
        file.write_all(b"committed")
    })
    .expect("reactivate")
    .commit()
    .expect("commit");
    assert_eq!(
        std::fs::read(&executable).expect("committed bytes"),
        b"committed"
    );
    voytty_platform::user_state::remove_owned_executable(&executable, 0).expect("uninstall");
    assert!(!executable.exists());
    let sid = win_custody::current_user_sid_string().expect("SID");
    win_custody::SecurityDescriptor::from_sddl(&format!("D:P(A;;FA;;;{sid})(A;;FR;;;WD)"))
        .expect("public ACL")
        .apply_to_tree(paths.config(), true)
        .expect("inject public grant");
    assert!(
        validate_regular_file(paths.config(), 0).is_err(),
        "another account must not read managed state"
    );
}

#[test]
fn conpty_input_resize_exit_and_blocked_input_cleanup() {
    let account = voytty_platform::account::EffectiveAccount::current().expect("account");
    let command = ExplicitPtyCommand::new(account.shell(), account.home())
        .arg("-NoLogo").arg("-NoProfile").arg("-NonInteractive").arg("-Command")
        .arg("[Console]::WriteLine('WINDOWS-READY'); $line = [Console]::ReadLine(); [Console]::WriteLine('WINDOWS-REPLY:' + $line); exit 7");
    let mut session = PtyHost
        .spawn(command, PtySize::new(24, 80))
        .expect("ConPTY");
    let mut reader = session.take_reader().expect("reader");
    let (sender, receiver) = mpsc::channel();
    let (drained, drain_complete) = mpsc::channel();
    let reader_thread = std::thread::spawn(move || {
        let mut bytes = [0; 8192];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) => break,
                Ok(length) => {
                    if sender.send(bytes[..length].to_vec()).is_err() {
                        break;
                    }
                }
                Err(error) => panic!("ConPTY output failed: {error}"),
            }
        }
        let _ = drained.send(());
    });
    let mut output = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut wait_for = |marker: &[u8]| {
        while !output.windows(marker.len()).any(|bytes| bytes == marker) {
            output.extend(
                receiver
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .expect("bounded ConPTY output"),
            );
            assert!(
                !output.windows(4).any(|bytes| bytes == b"\x1b[6n"),
                "a fresh hosted terminal must not wait for the frontend's cursor"
            );
        }
    };
    wait_for(b"WINDOWS-READY");
    session.resize(PtySize::new(32, 100)).expect("resize");
    session.write_input(b"hello\r").expect("input");
    wait_for(b"WINDOWS-REPLY:hello");
    let status = loop {
        match session.try_wait().expect("poll child") {
            PtyChildState::Exited(status) => break status,
            PtyChildState::Running => {
                assert!(Instant::now() < deadline, "child did not exit");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    };
    assert_eq!(status.exit_code(), 7);
    drop(session);
    drain_complete
        .recv_timeout(Duration::from_secs(5))
        .expect("output must reach EOF after exit");
    reader_thread.join().expect("output drain");

    let command = ExplicitPtyCommand::new(account.shell(), account.home())
        .arg("-NoLogo")
        .arg("-NoProfile")
        .arg("-NonInteractive")
        .arg("-Command")
        .arg("Start-Sleep -Seconds 60");
    let parts = PtyHost
        .spawn(command, PtySize::default())
        .expect("backpressure ConPTY")
        .into_driver_parts()
        .expect("parts");
    let mut reader = parts.reader;
    let (drained, drain_complete) = mpsc::channel();
    let drain = std::thread::spawn(move || {
        let result = std::io::copy(&mut reader, &mut std::io::sink());
        let _ = drained.send(());
        result
    });
    let mut io = parts.io;
    let (done, complete) = mpsc::channel();
    let (started, writing) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = started.send(());
        let result = io.write_input(&vec![b'x'; 4 * 1024 * 1024]);
        drop(io);
        let _ = done.send(result);
    });
    writing
        .recv_timeout(Duration::from_secs(5))
        .expect("input worker started");
    assert!(
        matches!(
            complete.recv_timeout(Duration::from_millis(100)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "large input must remain backpressured before close"
    );
    let mut child = parts.child;
    child
        .close_explicitly()
        .expect("close backpressured session");
    let _ = complete
        .recv_timeout(Duration::from_secs(5))
        .expect("blocked input must unblock on close");
    drain_complete
        .recv_timeout(Duration::from_secs(5))
        .expect("backpressure output must reach EOF");
    drain.join().expect("drain worker").expect("drain EOF");
}
