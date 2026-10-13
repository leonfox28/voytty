use std::collections::{HashMap, VecDeque};

use crate::{Duration, SystemTime, shared::ConnectionId};

// Covers delayed duplicate paths without keeping connections, timers, or
// cryptographic state alive. This is a bounded duplicate filter, not replay
// authentication: expiry or capacity eviction restores normal admission.
const CAPACITY: usize = 1024;
const RETENTION: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct InitialKey {
    // Initial CIDs identify the handshake across transport paths, just as in
    // the live initial-CID index. Direct and Relay expose different addresses.
    pub(super) version: u32,
    pub(super) dst_cid: ConnectionId,
    pub(super) src_cid: ConnectionId,
}

#[derive(Default)]
pub(super) struct RetiredInitials {
    entries: HashMap<InitialKey, SystemTime>,
    order: VecDeque<InitialKey>,
}

impl RetiredInitials {
    pub(super) fn insert(&mut self, key: InitialKey, now: SystemTime) {
        // Retirement is infrequent compared with packet routing. Remove all
        // expired entries here so backwards wall-clock adjustments cannot
        // strand expired keys behind a newer FIFO entry.
        self.order.retain(|key| {
            if self.entries[key] > now {
                true
            } else {
                self.entries.remove(key);
                false
            }
        });
        if self.entries.contains_key(&key) {
            return;
        }
        if self.order.len() == CAPACITY {
            self.entries.remove(&self.order.pop_front().unwrap());
        }
        self.entries.insert(key, now + RETENTION);
        self.order.push_back(key);
    }

    pub(super) fn contains(&self, key: &InitialKey, now: SystemTime) -> bool {
        // Replays neither extend retention nor affect FIFO eviction.
        self.entries.get(key).is_some_and(|expires| *expires > now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(sequence: u64) -> InitialKey {
        InitialKey {
            version: 1,
            dst_cid: ConnectionId::new(&sequence.to_be_bytes()),
            src_cid: ConnectionId::new(&[3; 8]),
        }
    }

    #[test]
    fn zterm_retired_initials_are_bounded_and_duplicates_do_not_refresh_them() {
        let mut retired = RetiredInitials::default();
        let now = SystemTime::UNIX_EPOCH;
        for sequence in 0..CAPACITY as u64 {
            retired.insert(key(sequence), now);
        }
        assert!(retired.contains(&key(0), now + RETENTION / 2));
        retired.insert(key(0), now + RETENTION / 2);
        retired.insert(key(CAPACITY as u64), now + RETENTION / 2);
        assert_eq!(retired.entries.len(), CAPACITY);
        assert_eq!(retired.order.len(), CAPACITY);
        assert!(!retired.contains(&key(0), now));
        assert!(retired.contains(&key(1), now));
        assert!(!retired.contains(&key(1), now + RETENTION));
        assert!(retired.contains(&key(CAPACITY as u64), now + RETENTION));
        retired.insert(key(1), now + RETENTION);
        assert_eq!(retired.entries.len(), 2);
        assert!(retired.contains(&key(1), now + RETENTION));
    }

    #[test]
    fn zterm_retired_initial_identity_includes_both_cids_and_version() {
        let mut retired = RetiredInitials::default();
        let now = SystemTime::UNIX_EPOCH;
        let original = key(0);
        retired.insert(original, now);
        let mut other = original;
        other.src_cid = ConnectionId::new(&[4; 8]);
        assert!(!retired.contains(&other, now));
        other = original;
        other.dst_cid = ConnectionId::new(&[5; 8]);
        assert!(!retired.contains(&other, now));
        other = original;
        other.version = 2;
        assert!(!retired.contains(&other, now));
        assert!(retired.contains(&original, now));
    }
}
