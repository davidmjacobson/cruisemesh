//! Gossip / mesh-relay dedupe state (DESIGN.md §5.3).
//!
//! The one piece of gossip that has to live in the core (rather than the
//! Android shell) is the **seen-ID set**: the bounded record of which
//! envelope `msg_id`s (the 16 random bytes in every §6.4 header) this node
//! has already handled, so epidemic flooding forwards each envelope at most
//! once and a frame that loops back over the mesh's redundant links is
//! dropped instead of re-relayed. It's here, not in Kotlin, so the same
//! dedupe logic can back a future headless simulation (DESIGN.md §10) and an
//! eventual iOS shell (§10) without reimplementation.
//!
//! DESIGN.md §5.3 specifies "a seen-ID LRU (bloom-filter-backed, ~50k
//! entries)". This implementation is an **exact** bounded set (a `HashSet`
//! for membership plus a `VecDeque` recording insertion order for eviction),
//! not a bloom filter. At family/ship scale 50k exact 16-byte ids is well
//! under a megabyte, and an exact set can never false-positive-drop a genuine
//! new message the way a bloom filter can -- a worthwhile trade until the
//! entry count is ever shown to matter. Eviction is FIFO (oldest inserted id
//! first), not true access-ordered LRU: once an id is seen we drop the frame
//! immediately and never "touch" the id again, so recency-of-use and
//! recency-of-insertion coincide for this workload. Evicting a very old id
//! only risks re-relaying one truly ancient envelope once if it reappears --
//! harmless, and bounded by the envelope's own `hop_ttl`/`expiry`.
//!
//! Thread-safety: [`MeshService`]'s receive path (BLE binder threads) and the
//! outgoing send path (`MeshSender`, UI thread) share one instance, so all
//! state is behind a `Mutex`. Operations are O(1) amortized with no I/O under
//! the lock.

use std::collections::{HashSet, VecDeque};
use std::sync::Mutex;

/// DESIGN.md §5.3's "~50k entries". See module docs for why this is an exact
/// set rather than the bloom filter the design mentions.
const DEFAULT_CAPACITY: usize = 50_000;

/// Bounded set of recently-seen envelope `msg_id`s for flood dedupe
/// (DESIGN.md §5.3). Created once per process and shared by the receive and
/// send paths (see module docs).
#[derive(uniffi::Object)]
pub struct SeenIds {
    inner: Mutex<Inner>,
}

struct Inner {
    /// Membership test.
    seen: HashSet<Vec<u8>>,
    /// Insertion order, for FIFO eviction once `seen.len()` exceeds capacity.
    /// Holds exactly the same ids as `seen`.
    order: VecDeque<Vec<u8>>,
    capacity: usize,
}

impl Inner {
    fn new(capacity: usize) -> Self {
        Inner {
            seen: HashSet::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Insert `msg_id` (assumed not already present) and evict oldest ids
    /// until back within capacity.
    fn insert(&mut self, msg_id: Vec<u8>) {
        self.seen.insert(msg_id.clone());
        self.order.push_back(msg_id);
        while self.order.len() > self.capacity {
            if let Some(evicted) = self.order.pop_front() {
                self.seen.remove(&evicted);
            }
        }
    }
}

#[uniffi::export]
impl SeenIds {
    /// Create an empty seen-set sized for DESIGN.md §5.3's ~50k entries.
    #[uniffi::constructor]
    pub fn new() -> Self {
        SeenIds { inner: Mutex::new(Inner::new(DEFAULT_CAPACITY)) }
    }

    /// Record that we've now handled `msg_id`, returning `true` if it was
    /// **new** (the caller should process/relay this envelope) or `false` if
    /// it was already in the set (a duplicate to drop). This is the atomic
    /// test-and-set the flood receive path runs on every inbound envelope.
    pub fn check_and_record(&self, msg_id: Vec<u8>) -> bool {
        let mut inner = self.inner.lock().expect("seen-ids mutex poisoned");
        if inner.seen.contains(&msg_id) {
            return false;
        }
        inner.insert(msg_id);
        true
    }

    /// Record `msg_id` as seen without reporting novelty, for envelopes this
    /// node *authored*: a sealed box can't be opened by its own sender
    /// (DESIGN.md §6.3's anonymous ephemeral sealing), so without this our own
    /// message flooded back to us by a relay would look "foreign" and get
    /// re-relayed. Idempotent -- recording an already-seen id is a no-op and
    /// won't create a duplicate eviction-queue entry.
    pub fn record(&self, msg_id: Vec<u8>) {
        let mut inner = self.inner.lock().expect("seen-ids mutex poisoned");
        if !inner.seen.contains(&msg_id) {
            inner.insert(msg_id);
        }
    }

    /// Current number of retained ids (for tests/diagnostics).
    pub fn len(&self) -> u64 {
        self.inner.lock().expect("seen-ids mutex poisoned").order.len() as u64
    }
}

impl Default for SeenIds {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a set with a small capacity so eviction is easy to exercise.
    fn with_capacity(capacity: usize) -> SeenIds {
        SeenIds { inner: Mutex::new(Inner::new(capacity)) }
    }

    #[test]
    fn first_sighting_is_new_repeat_is_not() {
        let seen = SeenIds::new();
        assert!(seen.check_and_record(b"msg-1".to_vec()));
        assert!(!seen.check_and_record(b"msg-1".to_vec()));
        // A different id is still new.
        assert!(seen.check_and_record(b"msg-2".to_vec()));
    }

    #[test]
    fn record_marks_seen_so_a_later_check_reports_duplicate() {
        let seen = SeenIds::new();
        seen.record(b"mine".to_vec());
        // Our own message bounced back must be recognised as a duplicate.
        assert!(!seen.check_and_record(b"mine".to_vec()));
    }

    #[test]
    fn record_is_idempotent_and_does_not_double_count() {
        let seen = SeenIds::new();
        seen.record(b"mine".to_vec());
        seen.record(b"mine".to_vec());
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn evicts_oldest_once_over_capacity() {
        let seen = with_capacity(2);
        assert!(seen.check_and_record(b"a".to_vec()));
        assert!(seen.check_and_record(b"b".to_vec()));
        // Inserting a third evicts "a" (the oldest).
        assert!(seen.check_and_record(b"c".to_vec()));
        assert_eq!(seen.len(), 2);
        // "a" was evicted, so it reads as new again (acceptable per §5.3).
        assert!(seen.check_and_record(b"a".to_vec()));
        // "b" and "c" are still remembered.
        assert!(!seen.check_and_record(b"c".to_vec()));
    }

    #[test]
    fn len_reflects_distinct_ids() {
        let seen = SeenIds::new();
        seen.check_and_record(b"a".to_vec());
        seen.check_and_record(b"b".to_vec());
        seen.check_and_record(b"a".to_vec()); // duplicate, no growth
        assert_eq!(seen.len(), 2);
    }
}
