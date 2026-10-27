//! Opt-in, content-free relay timing events for the application's finite detail window.
//! No dependency log message, address, endpoint identity or payload is forwarded.

use std::{sync::OnceLock, time::Instant};

use iroh_base::{EndpointId, RelayUrl, SecretKey};
use iroh_relay::protos::relay::Datagrams;

pub(crate) const TARGET: &str = "iroh::voytty_transport";

fn elapsed_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START
        .get_or_init(Instant::now)
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64
}

pub(crate) fn dial_span(url: &RelayUrl) -> tracing::Span {
    if !tracing::enabled!(target: TARGET, tracing::Level::DEBUG) {
        return tracing::Span::none();
    }
    tracing::debug_span!(target: TARGET, "relay_dial", relay = tag(b"relay", url.as_str().as_bytes()), elapsed_ms = elapsed_ms())
}

fn tag(domain: &[u8], value: &[u8]) -> u64 {
    static KEY: OnceLock<[u8; 32]> = OnceLock::new();
    let mut hash = blake3::Hasher::new_keyed(KEY.get_or_init(|| SecretKey::generate().to_bytes()));
    hash.update(domain);
    hash.update(value);
    u64::from_le_bytes(
        hash.finalize().as_bytes()[..8]
            .try_into()
            .expect("eight bytes"),
    )
}

pub(crate) fn relay(phase: u64, url: &RelayUrl) {
    if tracing::enabled!(target: TARGET, tracing::Level::DEBUG) {
        emit(phase, tag(b"relay", url.as_str().as_bytes()), 0, 0, 0, 0, 0);
    }
}

/// The application maps these numeric codes to a closed error vocabulary.
pub(crate) fn failure(phase: u64, url: &RelayUrl, failure: u64) {
    if tracing::enabled!(target: TARGET, tracing::Level::DEBUG) {
        let relay = tag(b"relay", url.as_str().as_bytes());
        let elapsed_ms = elapsed_ms();
        tracing::debug!(target: TARGET, phase, relay, failure, elapsed_ms);
    }
}

pub(crate) fn datagrams(
    phase: u64,
    url: &RelayUrl,
    peer: EndpointId,
    data: &Datagrams,
    age_ms: u64,
) {
    if !tracing::enabled!(target: TARGET, tracing::Level::DEBUG) {
        return;
    }
    let relay = tag(b"relay", url.as_str().as_bytes());
    let peer = tag(b"peer", peer.as_bytes());
    let stride = data
        .segment_size
        .map_or(data.contents.len(), |n| u16::from(n) as usize);
    for packet in data.contents.chunks(stride.max(1)) {
        // Hash the encrypted UDP datagram separately, so batching cannot change
        // correlation across endpoints. This is a diagnostic hint, not an ID
        // used for protocol decisions, and does not expose packet contents.
        let packet_tag = u64::from_le_bytes(
            blake3::hash(packet).as_bytes()[..8]
                .try_into()
                .expect("eight bytes"),
        );
        let kind = if packet.first().is_some_and(|b| b & 0x80 == 0) {
            1 // short header
        } else if packet.get(1..5) == Some(&[0, 0, 0, 1]) {
            2 + u64::from((packet[0] >> 4) & 3) // Initial, 0-RTT, Handshake, Retry
        } else {
            0
        };
        emit(
            phase,
            relay,
            peer,
            packet_tag,
            packet.len() as u64,
            age_ms,
            kind,
        );
    }
}

fn emit(phase: u64, relay: u64, peer: u64, packet: u64, bytes: u64, age_ms: u64, kind: u64) {
    let elapsed_ms = elapsed_ms();
    tracing::debug!(target: TARGET, phase, relay, peer, packet, bytes, age_ms, kind, elapsed_ms);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        num::NonZeroU16,
        sync::{Arc, Mutex},
    };
    use tracing::field::{Field, Visit};
    use tracing_subscriber::{Layer, layer::Context, prelude::*};

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<BTreeMap<String, u64>>>>);
    impl<S: tracing::Subscriber> Layer<S> for Capture {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
            if event.metadata().target() != TARGET {
                return;
            }
            #[derive(Default)]
            struct Numbers(BTreeMap<String, u64>);
            impl Visit for Numbers {
                fn record_u64(&mut self, field: &Field, value: u64) {
                    self.0.insert(field.name().into(), value);
                }
                fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {
                    panic!("trace must contain only numbers");
                }
            }
            let mut numbers = Numbers::default();
            event.record(&mut numbers);
            self.0.lock().unwrap().push(numbers.0);
        }
    }

    #[test]
    fn voytty_transport_trace_matches_datagrams_across_rebatching() {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        let _guard = tracing::subscriber::set_default(subscriber);
        let url: RelayUrl = "https://private.invalid/".parse().unwrap();
        let peer = SecretKey::generate().public();
        let other_peer = SecretKey::generate().public();
        let data = [0xc0, 0, 0, 0, 1, 42, 0xe0, 0, 0, 0, 1, 43];
        let batch = Datagrams {
            ecn: None,
            segment_size: NonZeroU16::new(6),
            contents: bytes::Bytes::copy_from_slice(&data),
        };
        datagrams(4, &url, peer, &batch, 0);
        for packet in data.chunks(6) {
            let one = Datagrams {
                ecn: None,
                segment_size: None,
                contents: bytes::Bytes::copy_from_slice(packet),
            };
            datagrams(6, &url, other_peer, &one, 0);
        }
        let records = capture.0.lock().unwrap();
        assert_eq!(records.len(), 4);
        for i in 0..2 {
            assert_eq!(records[i]["packet"], records[i + 2]["packet"]);
            assert_ne!(records[i]["peer"], records[i + 2]["peer"]);
            assert_eq!(records[i]["bytes"], 6);
        }
        assert_ne!(records[0]["packet"], records[1]["packet"]);
        assert_eq!(records[0]["kind"], 2);
        assert_eq!(records[1]["kind"], 4);
        assert_ne!(tag(b"peer", b"same value"), tag(b"relay", b"same value"));
    }
}
