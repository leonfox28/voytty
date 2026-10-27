use super::*;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Server {
    url: String,
    calls: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new(reply: impl Fn(usize, &mut TcpStream) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("fixture listener");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let url = format!("http://{}/asset", listener.local_addr().expect("address"));
        let calls = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (count, done) = (calls.clone(), stop.clone());
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !done.load(Ordering::Acquire) && Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_millis(200)))
                            .expect("read bound");
                        stream
                            .set_write_timeout(Some(Duration::from_millis(200)))
                            .expect("write bound");
                        // Wait for a complete request, not the first TCP packet.
                        // Closing with unread headers can reset a valid response.
                        let mut request = Vec::new();
                        let mut chunk = [0; 1024];
                        while request.len() < 4096 {
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(length) => request.extend_from_slice(&chunk[..length]),
                            }
                            if request.ends_with(b"\r\n\r\n") {
                                reply(count.fetch_add(1, Ordering::AcqRel), &mut stream);
                                break;
                            }
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2))
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
        });
        Self {
            url,
            calls,
            stop,
            worker: Some(worker),
        }
    }

    fn count(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.worker
            .take()
            .expect("fixture worker")
            .join()
            .expect("fixture completed");
    }
}

fn fetcher() -> HttpReleaseFetcher {
    fetcher_with_idle_timeout(Duration::from_millis(500))
}

fn fetcher_with_idle_timeout(idle_timeout: Duration) -> HttpReleaseFetcher {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let policy = DownloadPolicy {
        metadata_budget: Duration::from_millis(500),
        archive_budget: Duration::from_secs(2),
        idle_timeout,
        retry_delay: Duration::from_millis(1),
    };
    // Only the loopback fixture client accepts HTTP; production stays HTTPS-only.
    let client = reqwest::Client::builder()
        .no_proxy()
        .read_timeout(policy.idle_timeout)
        .retry(reqwest::retry::never())
        .build()
        .expect("fixture client");
    HttpReleaseFetcher::with_client(client, policy).expect("fixture fetcher")
}

#[test]
fn transient_status_and_truncated_body_retry_from_empty_buffers() {
    for first in [
        "HTTP/1.1 200 OK\r\nContent-Len",
        "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nbad",
    ] {
        let server = Server::new(move |index, stream| {
            let response = if index == 0 {
                first
            } else {
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ngood"
            };
            stream
                .write_all(response.as_bytes())
                .expect("fixture response");
        });
        assert_eq!(fetcher().fetch(&server.url, 64).expect("download"), b"good");
        assert_eq!(server.count(), 2);
    }
}

#[test]
fn permanent_status_and_oversize_bodies_fail_without_retry() {
    for response in [
        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n",
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nlarge\r\n0\r\n\r\n",
    ] {
        let server = Server::new(move |_, stream| {
            stream
                .write_all(response.as_bytes())
                .expect("fixture response");
        });
        assert_eq!(
            fetcher().fetch(&server.url, 4),
            Err(DistributionError::Fetch)
        );
        assert_eq!(server.count(), 1);
    }
}

#[test]
fn stalled_and_slow_streams_are_bounded() {
    for drip in [false, true] {
        let server = Server::new(move |_, stream| {
            if stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n")
                .is_err()
            {
                return;
            }
            for _ in 0..20 {
                thread::sleep(Duration::from_millis(10));
                if drip && stream.write_all(b"x").is_err() {
                    break;
                }
            }
        });
        let start = Instant::now();
        assert_eq!(
            fetcher_with_idle_timeout(Duration::from_millis(40)).fetch(&server.url, 100),
            Err(DistributionError::Fetch)
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(server.count() > 0 && server.count() <= 4);
    }
}

#[test]
fn retry_count_and_total_budget_both_limit_requests() {
    let server = Server::new(|_, stream| {
        let _ = stream.write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
    });
    assert_eq!(
        fetcher().fetch(&server.url, 64),
        Err(DistributionError::Fetch)
    );
    assert_eq!(server.count(), 4);

    let mut bounded = fetcher();
    bounded.policy.retry_delay = Duration::from_secs(1);
    assert_eq!(
        bounded.fetch(&server.url, 64),
        Err(DistributionError::Fetch)
    );
    assert_eq!(server.count(), 5);
}

#[test]
fn production_fetcher_rejects_plain_http_before_network() {
    let server = Server::new(|_, _| panic!("HTTP must not be requested"));
    assert_eq!(
        HttpReleaseFetcher::new()
            .expect("client")
            .fetch(&server.url, 64),
        Err(DistributionError::Fetch)
    );
    assert_eq!(server.count(), 0);
}
