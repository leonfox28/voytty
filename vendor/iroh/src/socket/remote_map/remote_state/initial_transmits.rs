//! Bounded, short-lived Initial fanout while address discovery is in flight.

use super::{DATAGRAM_SEND_TIMEOUT, Instant, OwnedTransmit, TransportsSender, VecDeque};

const MAX_INITIAL_TRANSMITS: usize = 16;
const MAX_INITIAL_BYTES: usize = 64 * 1024;

#[derive(Default)]
pub(super) struct InitialTransmits {
    queue: VecDeque<InitialTransmit>,
    bytes: usize,
}

pub(super) struct InitialTransmit {
    pub(super) sender: Box<TransportsSender>,
    pub(super) transmit: OwnedTransmit,
    pub(super) expires_at: Instant,
}

impl InitialTransmits {
    pub(super) fn remember(&mut self, sender: &TransportsSender, transmit: &OwnedTransmit) {
        self.prune();
        let len = transmit.contents.len();
        let stride = transmit.segment_size.unwrap_or(len);
        // Mixed-address sends normally contain Initials. Do not retain empty,
        // oversized, 0-RTT-only or other packet types. Iroh uses QUIC v1.
        if len == 0
            || len > MAX_INITIAL_BYTES
            || stride == 0
            || !transmit.contents.chunks(stride).all(|packet| {
                packet.len() >= 5 && packet[0] & 0xf0 == 0xc0 && packet[1..5] == [0, 0, 0, 1]
            })
        {
            return;
        }
        while self.queue.len() >= MAX_INITIAL_TRANSMITS || self.bytes + len > MAX_INITIAL_BYTES {
            self.pop_front();
        }
        self.bytes += len;
        self.queue.push_back(InitialTransmit {
            sender: Box::new(sender.clone()),
            transmit: transmit.clone(),
            expires_at: Instant::now() + DATAGRAM_SEND_TIMEOUT,
        });
    }

    pub(super) fn expires_at(&self) -> Option<Instant> {
        self.queue.front().map(|initial| initial.expires_at)
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = &InitialTransmit> {
        self.queue.iter()
    }

    pub(super) fn prune(&mut self) {
        let now = Instant::now();
        while self.expires_at().is_some_and(|deadline| deadline <= now) {
            self.pop_front();
        }
    }

    pub(super) fn clear(&mut self) {
        self.queue.clear();
        self.bytes = 0;
    }

    fn pop_front(&mut self) {
        if let Some(initial) = self.queue.pop_front() {
            self.bytes -= initial.transmit.contents.len();
        }
    }
}

#[cfg(all(test, not(wasm_browser)))]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn packet(len: usize) -> OwnedTransmit {
        let mut contents = vec![0; len];
        contents[..5].copy_from_slice(&[0xc0, 0, 0, 0, 1]);
        OwnedTransmit {
            ecn: None,
            contents: Bytes::from(contents),
            segment_size: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn voytty_initial_fanout_expiry_keeps_original_packet_age() {
        let (sender, _receiver) = TransportsSender::with_bounded_relay(1, std::iter::empty());
        let mut cache = InitialTransmits::default();
        cache.remember(&sender, &packet(1200));
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        cache.remember(&sender, &packet(1201));
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        cache.prune();
        assert_eq!(
            cache
                .iter()
                .map(|p| p.transmit.contents.len())
                .collect::<Vec<_>>(),
            [1201]
        );
        assert_eq!(cache.bytes, 1201);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        cache.prune();
        assert_eq!(cache.expires_at(), None);
        assert_eq!(cache.bytes, 0);
    }

    #[tokio::test]
    async fn voytty_initial_fanout_bounds_count_and_bytes() {
        let (sender, _receiver) = TransportsSender::with_bounded_relay(1, std::iter::empty());
        let mut cache = InitialTransmits::default();
        for _ in 0..100 {
            cache.remember(&sender, &packet(1200));
        }
        assert_eq!(cache.iter().count(), MAX_INITIAL_TRANSMITS);
        cache.remember(&sender, &packet(MAX_INITIAL_BYTES));
        assert_eq!(cache.iter().count(), 1);
        assert_eq!(cache.bytes, MAX_INITIAL_BYTES);
        cache.remember(&sender, &packet(MAX_INITIAL_BYTES + 1));
        assert_eq!(cache.iter().count(), 1);
        assert_eq!(cache.bytes, MAX_INITIAL_BYTES);
        cache.clear();
        assert_eq!(cache.expires_at(), None);
        assert_eq!(cache.bytes, 0);
    }

    #[tokio::test]
    async fn voytty_initial_fanout_rejects_other_packets_and_checks_each_segment() {
        let (sender, _receiver) = TransportsSender::with_bounded_relay(1, std::iter::empty());
        let mut cache = InitialTransmits::default();
        for header in [0x40, 0xd0, 0xe0, 0xf0] {
            let mut non_initial = packet(1200);
            let mut bytes = non_initial.contents.to_vec();
            bytes[0] = header;
            non_initial.contents = bytes.into();
            cache.remember(&sender, &non_initial);
        }
        let mut batched = packet(1200);
        batched.segment_size = Some(600); // Second segment has no Initial header.
        cache.remember(&sender, &batched);
        batched.segment_size = Some(0);
        cache.remember(&sender, &batched);
        assert_eq!(cache.iter().count(), 0);
        let mut bytes = packet(1200).contents.to_vec();
        bytes.extend_from_slice(&packet(1200).contents);
        batched.contents = bytes.into();
        batched.segment_size = Some(1200);
        cache.remember(&sender, &batched);
        assert_eq!(cache.iter().count(), 1);
    }
}
