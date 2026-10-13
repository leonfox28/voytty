//! Nonblocking native I/O behind the PTY's blocking Rust interface.
//!
//! A blocking master write can prevent a slave's exit from completing on
//! macOS. Waiting for readiness outside the kernel write lets the independent
//! child owner cancel input before terminating and reaping that child.

use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use filedescriptor::FileDescriptor;
use mio::{Events, Interest, Poll, Token, Waker, unix::SourceFd};
use portable_pty::MasterPty;

use super::InputStop;

type Reader = Box<dyn Read + Send>;
type Writer = Box<dyn Write + Send>;

pub(super) fn interruptible(
    master: &dyn MasterPty,
    reader: Reader,
    writer: Writer,
) -> io::Result<(Reader, Writer, Arc<InputStop>)> {
    let fd = master
        .as_raw_fd()
        .ok_or_else(|| io::Error::other("Unix PTY has no native descriptor"))?;
    let mut readiness = FileDescriptor::dup(&fd).map_err(io::Error::other)?;
    readiness.set_non_blocking(true).map_err(io::Error::other)?;
    let readiness = Arc::new(readiness);
    let reader_ready = Readiness::new(Arc::clone(&readiness), Interest::READABLE)?;
    let writer_ready = Readiness::new(readiness, Interest::WRITABLE)?;
    let stopped = Arc::new(InputStop {
        stopped: AtomicBool::new(false),
        wake: Some(Waker::new(writer_ready.poll.registry(), Token(1))?),
    });
    Ok((
        Box::new(BlockingReader {
            inner: reader,
            readiness: reader_ready,
        }),
        Box::new(InterruptibleWriter {
            inner: writer,
            readiness: writer_ready,
            stopped: Arc::clone(&stopped),
        }),
        stopped,
    ))
}

struct Readiness {
    poll: Poll,
    events: Events,
    // Keep the registered descriptor alive even if the master owner is dropped.
    _fd: Arc<FileDescriptor>,
}

impl Readiness {
    fn new(fd: Arc<FileDescriptor>, interest: Interest) -> io::Result<Self> {
        let poll = Poll::new()?;
        poll.registry()
            .register(&mut SourceFd(&fd.as_raw_fd()), Token(0), interest)?;
        Ok(Self {
            poll,
            events: Events::with_capacity(2),
            _fd: fd,
        })
    }

    fn wait(&mut self) -> io::Result<()> {
        match self.poll.poll(&mut self.events, None) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => Ok(()),
            result => result,
        }
    }
}

struct BlockingReader {
    inner: Reader,
    readiness: Readiness,
}

impl Read for BlockingReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.inner.read(bytes) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.readiness.wait()?;
                }
                result => return result,
            }
        }
    }
}

struct InterruptibleWriter {
    inner: Writer,
    readiness: Readiness,
    stopped: Arc<InputStop>,
}

impl Write for InterruptibleWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        loop {
            if self.stopped.stopped.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "PTY input closed",
                ));
            }
            // Bound each native write as well as checking cancellation between
            // writes; one large input frame must not monopolize the PTY driver.
            match self.inner.write(&bytes[..bytes.len().min(1024)]) {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.readiness.wait()?;
                }
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    struct ObservedWriter {
        socket: UnixStream,
        blocked: Option<mpsc::SyncSender<()>>,
    }

    impl Write for ObservedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let result = self.socket.write(bytes);
            if result
                .as_ref()
                .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
                && let Some(blocked) = self.blocked.take()
            {
                blocked.send(()).expect("test observes backpressure");
            }
            result
        }

        fn flush(&mut self) -> io::Result<()> {
            self.socket.flush()
        }
    }

    fn backpressured_writer() -> (
        InterruptibleWriter,
        UnixStream,
        mpsc::Receiver<()>,
        Arc<InputStop>,
    ) {
        let (socket, peer) = UnixStream::pair().expect("socket pair");
        socket.set_nonblocking(true).expect("nonblocking writer");
        let fd = FileDescriptor::dup(&socket).expect("owned readiness descriptor");
        let readiness = Readiness::new(Arc::new(fd), Interest::WRITABLE).expect("readiness");
        let stopped = Arc::new(InputStop {
            stopped: AtomicBool::new(false),
            wake: Some(Waker::new(readiness.poll.registry(), Token(1)).expect("input wake")),
        });
        let (blocked, observed) = mpsc::sync_channel(1);
        (
            InterruptibleWriter {
                inner: Box::new(ObservedWriter {
                    socket,
                    blocked: Some(blocked),
                }),
                readiness,
                stopped: Arc::clone(&stopped),
            },
            peer,
            observed,
            stopped,
        )
    }

    #[test]
    fn cancellation_wakes_backpressured_input_without_peer_reading() {
        let (mut writer, _peer, blocked, stopped) = backpressured_writer();
        let (finished, result) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            finished
                .send(writer.write_all(&vec![0; 4 * 1024 * 1024]))
                .expect("test awaits writer");
        });
        blocked
            .recv_timeout(Duration::from_secs(3))
            .expect("backpressure");
        stopped.stop();
        assert_eq!(
            result
                .recv_timeout(Duration::from_secs(3))
                .expect("close wakes the writer")
                .expect_err("cancelled input cannot succeed")
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        worker.join().expect("writer joins");
    }

    #[test]
    fn backpressured_input_resumes_without_losing_or_reordering_bytes() {
        let (mut writer, mut peer, blocked, _stopped) = backpressured_writer();
        let bytes: Vec<_> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let expected = bytes.clone();
        let write_worker = thread::spawn(move || writer.write_all(&bytes));
        blocked
            .recv_timeout(Duration::from_secs(3))
            .expect("backpressure");
        let (finished, result) = mpsc::sync_channel(1);
        let read_worker = thread::spawn(move || {
            let mut received = vec![0; expected.len()];
            peer.read_exact(&mut received).expect("complete input");
            finished
                .send(received == expected)
                .expect("test awaits bytes");
        });
        assert!(
            result
                .recv_timeout(Duration::from_secs(3))
                .expect("peer drains input"),
            "partial writes must preserve every byte in order"
        );
        write_worker
            .join()
            .expect("writer joins")
            .expect("input succeeds");
        read_worker.join().expect("reader joins");
    }
}
