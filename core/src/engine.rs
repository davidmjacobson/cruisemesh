//! Shared mesh state-machine decisions.
//!
//! Native code owns radios, sockets, HTTP, and lifecycle. This module owns
//! the decisions that must remain identical on Android and iOS: inbound
//! gating, relay acknowledgement safety, HELLO identity locking, and the
//! complete digest-time mule spray plan.

use std::collections::HashSet;

use crate::{
    encode_envelope_frame, CarriedEnvelope, CoreError, MessageOrigin, MessageStore,
    OutboundEnvelope, OutgoingReceiptEnvelope, RECEIPT_TYPE_DELIVERED,
};

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreInboundGate {
    Dispatch,
    Seen,
    Expired,
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreInboundDisposition {
    Consumed,
    Carried,
    Expired,
    Seen,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayEnvelopeDisposition {
    pub relay_id: i64,
    /// Stable envelope id. Only consulted for [`CoreInboundDisposition::Seen`]
    /// items, to look up whether THIS device durably consumed this exact
    /// envelope -- see [`MessageStore::core_relay_ack_ids_with_consumed`].
    pub msg_id: Vec<u8>,
    pub disposition: CoreInboundDisposition,
}

/// Exact frames to emit after accepting a peer's DIGEST.
///
/// The three lists remain separate for diagnostics, but are selected by one
/// core operation so neither platform can accidentally omit a traffic class.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreDigestSprayPlan {
    pub carried_frames: Vec<Vec<u8>>,
    pub own_outbound_frames: Vec<Vec<u8>>,
    pub own_receipt_frames: Vec<Vec<u8>>,
}

#[uniffi::export]
pub fn core_inbound_gate(is_new_msg_id: bool, expiry_ms: i64, now_ms: i64) -> CoreInboundGate {
    if !is_new_msg_id {
        CoreInboundGate::Seen
    } else if expiry_ms <= now_ms {
        CoreInboundGate::Expired
    } else {
        CoreInboundGate::Dispatch
    }
}

/// Whether a relay-fetched envelope with this disposition may be acked
/// (deleted from the relay mailbox) on the strength of the disposition
/// alone.
///
/// Only [`CoreInboundDisposition::Consumed`] (it was ours to open, and we
/// did) and [`CoreInboundDisposition::Expired`] (it's dead weight regardless
/// of who it was for) are safe to remove this way.
/// [`CoreInboundDisposition::Carried`] must NOT be acked: relay
/// proxy-polling means we may have fetched a contact's envelope on their
/// behalf, and the relay copy is the durable fallback until the real
/// recipient (or another proxy) fetches and consumes it -- deleting it here
/// would silently drop the message. [`CoreInboundDisposition::Seen`] also
/// returns `false` here, but it is NOT necessarily a dead end: see
/// [`consumed_seen_is_ackable`] and
/// [`MessageStore::core_relay_ack_ids_with_consumed`] for the narrow,
/// independently store-verified case where a Seen copy is still safe to
/// ack -- this function alone can't tell, since it has no store access.
///
/// Safety invariant (applies to this function and
/// [`consumed_seen_is_ackable`] alike): never ack a relay copy unless THIS
/// device was the envelope's sole true endpoint consumer; when in doubt,
/// don't ack. Re-fetch churn is recoverable; a deleted relay copy is not.
#[uniffi::export]
pub fn core_should_ack_inbound(disposition: CoreInboundDisposition) -> bool {
    matches!(
        disposition,
        CoreInboundDisposition::Consumed | CoreInboundDisposition::Expired
    )
}

/// Ack ids among `items` using [`core_should_ack_inbound`] alone -- i.e.
/// Consumed/Expired only. Deliberately does not know about the
/// consumed-SEEN rule (it has no store access to check it): a caller that
/// can look up message origins should prefer
/// [`MessageStore::core_relay_ack_ids_with_consumed`] instead, which folds
/// this same rule in and additionally acks the narrow SEEN case covered by
/// [`consumed_seen_is_ackable`].
#[uniffi::export]
pub fn core_relay_ack_ids(items: Vec<CoreRelayEnvelopeDisposition>) -> Vec<i64> {
    items
        .into_iter()
        .filter(|item| core_should_ack_inbound(item.disposition))
        .map(|item| item.relay_id)
        .collect()
}

/// Narrow follow-up check for a relay-fetched envelope that deduped as
/// [`CoreInboundDisposition::Seen`]: [`core_should_ack_inbound`] can't vouch
/// for it (dedupe happens before a disposition is re-derived, so Seen alone
/// doesn't say what the *original* handling was), but the message store can
/// independently answer "did THIS device actually consume a copy of this
/// exact `msg_id`, as a 1:1 message addressed to us and to us alone?"
///
/// `origin` is [`MessageStore::message_origin_by_msg_id`]'s result for the
/// envelope's `msg_id`, or `None` if no such row exists. A row only exists
/// for kinds that persist a durable `msg_id`: 1:1/group text, attachment
/// manifests, and reactions (inserted via `insert_incoming_message`), plus
/// our own authored outbound messages (`insert_outgoing_message`/
/// `insert_outgoing_reply`). Of those, exactly one shape is ackable -- a 1:1
/// incoming row, recognizable by the local storage convention that a 1:1
/// chat is keyed by the other party, so `chat_id == sender_user_id`:
///
/// - `origin` is `None` -> NOT ackable. Either we never consumed it (merely
///   muled/flooded a copy -- the relay copy is the real recipient's durable
///   fallback), or it was a hidden kind -- receipts, profile sync, friend
///   requests/directory, group invites, LAN endpoint hints -- which are
///   stored (if at all) via the plain `insert_message` path that never
///   records a `msg_id`. Hidden kinds leave no durable trace tying a
///   specific `msg_id` to "we consumed it," so there is nothing to safely
///   vouch for them with -- correctness over bandwidth, per the invariant
///   on [`core_should_ack_inbound`].
/// - `sender_user_id == own_user_id` -> NOT ackable. Our own outbound
///   message echoing back (own outbound `msg_id`s are seeded into the
///   gossip-dedupe set at startup, so a relay copy of our own envelope
///   always dedupes as Seen): that relay copy exists *for the recipient*,
///   and deleting it would silently drop their only remaining way to fetch
///   the message.
/// - `chat_id != sender_user_id` -> a GROUP row -> NOT ackable. The family
///   shares one relay mailbox per family token, and a group envelope on it
///   is a single copy that every member fetches. We consumed *our* read of
///   it, but other members -- in particular one with internet-only
///   connectivity and no BLE path to the rest of the family -- still need
///   the relay copy; acking here would delete their message permanently.
///   For group envelopes this device is only ONE of several endpoint
///   consumers, never "the" consumer, so the invariant forbids the ack.
///   (The pre-existing CONSUMED-path ack in
///   [`MessageStore::core_relay_ack_ids_with_consumed`] already acks a
///   group envelope on first relay fetch -- DTN_TODOS.md N1 -- which has
///   the same shared-mailbox problem; that is a separate, deliberately
///   out-of-scope issue this check does not widen.)
/// - Otherwise (row exists, sender is someone else, `chat_id ==
///   sender_user_id`) -> a 1:1 message sealed to us that we stored -> this
///   device was the envelope's sole true endpoint consumer -> ackable.
///
/// Safety invariant, stated verbatim (DTN_TODOS.md §3.1): "never ack a
/// relay copy unless THIS device was the envelope's sole true endpoint
/// consumer; when in doubt, don't ack."
#[uniffi::export]
pub fn core_consumed_seen_is_ackable(origin: Option<MessageOrigin>, own_user_id: Vec<u8>) -> bool {
    match origin {
        Some(origin) => {
            origin.sender_user_id != own_user_id && origin.chat_id == origin.sender_user_id
        }
        None => false,
    }
}

/// A link may establish its identity once. Repeating the same HELLO is
/// harmless; changing identity on an authenticated link is rejected.
#[uniffi::export]
pub fn core_hello_identity_matches(
    current_user_id: Option<Vec<u8>>,
    hello_user_id: Vec<u8>,
) -> bool {
    current_user_id.is_none_or(|current| current == hello_user_id)
}

fn frame_carried(envelope: CarriedEnvelope) -> Vec<u8> {
    encode_envelope_frame(
        envelope.msg_id,
        envelope.hop_ttl,
        envelope.expiry,
        envelope.recipient_hint,
        envelope.sealed,
    )
}

fn frame_outbound(envelope: OutboundEnvelope) -> Vec<u8> {
    encode_envelope_frame(
        envelope.msg_id,
        envelope.hop_ttl,
        envelope.expiry,
        envelope.recipient_hint,
        envelope.sealed,
    )
}

fn frame_receipt(envelope: OutgoingReceiptEnvelope) -> Vec<u8> {
    encode_envelope_frame(
        envelope.msg_id,
        envelope.hop_ttl,
        envelope.expiry,
        envelope.recipient_hint,
        envelope.sealed,
    )
}

fn select_own_outbound(
    pending_by_recipient: Vec<Vec<OutboundEnvelope>>,
    peer_known_msg_ids: &HashSet<Vec<u8>>,
    now_ms: i64,
    budget_bytes: u64,
) -> Vec<OutboundEnvelope> {
    let mut selected = Vec::new();
    let mut used = 0_u64;
    'recipients: for pending in pending_by_recipient {
        for envelope in pending {
            if envelope.expiry <= now_ms || peer_known_msg_ids.contains(&envelope.msg_id) {
                continue;
            }
            let size = envelope.sealed.len() as u64;
            if used.saturating_add(size) > budget_bytes {
                break 'recipients;
            }
            used += size;
            selected.push(envelope);
        }
    }
    selected
}

fn select_own_receipts(
    pending: Vec<OutgoingReceiptEnvelope>,
    peer_user_id: &[u8],
    peer_known_msg_ids: &HashSet<Vec<u8>>,
    now_ms: i64,
    budget_bytes: u64,
) -> Vec<OutgoingReceiptEnvelope> {
    let mut selected = Vec::new();
    let mut used = 0_u64;
    for envelope in pending {
        if envelope.recipient_user_id == peer_user_id
            || envelope.expiry <= now_ms
            || peer_known_msg_ids.contains(&envelope.msg_id)
        {
            continue;
        }
        let size = envelope.sealed.len() as u64;
        if used.saturating_add(size) > budget_bytes {
            break;
        }
        used += size;
        selected.push(envelope);
    }
    selected
}

#[uniffi::export]
impl MessageStore {
    /// Build the complete digest-time mule spray in one place.
    ///
    /// This deliberately includes all three canonical classes: foreign
    /// carried traffic, this device's pending pairwise traffic to other
    /// contacts, and pending receipts owed to other contacts.
    pub fn core_digest_spray_plan(
        &self,
        own_user_id: Vec<u8>,
        peer_user_id: Vec<u8>,
        peer_hints: Vec<Vec<u8>>,
        peer_known_msg_ids: Vec<Vec<u8>>,
        now_ms: i64,
        own_outbound_budget_bytes: u64,
        own_receipt_budget_bytes: u64,
        receipt_query_limit: u64,
    ) -> Result<CoreDigestSprayPlan, CoreError> {
        self.prune_expired_carried(now_ms)?;
        let carried =
            self.carried_envelopes_for_peer_sync(peer_hints, peer_known_msg_ids.clone(), now_ms)?;
        let known: HashSet<Vec<u8>> = peer_known_msg_ids.into_iter().collect();

        let mut pending_by_recipient = Vec::new();
        for contact in self.list_contacts()? {
            if contact.user_id == peer_user_id {
                continue;
            }
            let delivered_through = self.receipt_through(
                contact.user_id.clone(),
                own_user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
            )?;
            pending_by_recipient.push(self.outbound_envelopes_after(
                contact.user_id,
                own_user_id.clone(),
                delivered_through,
            )?);
        }

        let own_outbound = select_own_outbound(
            pending_by_recipient,
            &known,
            now_ms,
            own_outbound_budget_bytes,
        );
        let own_receipts = select_own_receipts(
            self.pending_relay_outgoing_receipt_envelopes(receipt_query_limit, now_ms)?,
            &peer_user_id,
            &known,
            now_ms,
            own_receipt_budget_bytes,
        );

        Ok(CoreDigestSprayPlan {
            carried_frames: carried.into_iter().map(frame_carried).collect(),
            own_outbound_frames: own_outbound.into_iter().map(frame_outbound).collect(),
            own_receipt_frames: own_receipts.into_iter().map(frame_receipt).collect(),
        })
    }

    /// Relay ack ids for one poll pass, folding the consumed-SEEN rule
    /// (DTN_TODOS.md §3.1) in on top of [`core_should_ack_inbound`]'s
    /// Consumed/Expired rule -- so a device that has already consumed an
    /// envelope over BLE/LAN stops re-downloading its relay copy on every
    /// 60s poll pass instead of waiting out the full expiry window.
    ///
    /// Per item:
    /// - [`CoreInboundDisposition::Consumed`] or
    ///   [`CoreInboundDisposition::Expired`]: ack (same as
    ///   [`core_relay_ack_ids`]).
    /// - [`CoreInboundDisposition::Carried`]: never ack -- the relay copy is
    ///   the durable fallback until the real recipient (or another proxy)
    ///   fetches it.
    /// - [`CoreInboundDisposition::Seen`]: look up
    ///   [`Self::message_origin_by_msg_id`] for the item's `msg_id` and ack
    ///   only if [`core_consumed_seen_is_ackable`] says so -- i.e. this
    ///   device durably stored the envelope as a 1:1 message from someone
    ///   else, not merely muled it, echoed its own message back, or read
    ///   one copy of a shared-mailbox group envelope.
    ///
    /// Safety invariant, stated verbatim (DTN_TODOS.md §3.1): "never ack a
    /// relay copy unless THIS device was the envelope's sole true endpoint
    /// consumer; when in doubt, don't ack."
    ///
    /// Note: this only extends the SEEN path. The pre-existing CONSUMED-path
    /// group ack (DTN_TODOS.md N1 -- the first member to fetch+consume a
    /// group envelope over the relay acks it away from every other member,
    /// since the family shares one relay mailbox per family token) is out
    /// of scope here and deliberately unchanged.
    pub fn core_relay_ack_ids_with_consumed(
        &self,
        items: Vec<CoreRelayEnvelopeDisposition>,
        own_user_id: Vec<u8>,
    ) -> Result<Vec<i64>, CoreError> {
        let mut acked = Vec::with_capacity(items.len());
        for item in items {
            if core_should_ack_inbound(item.disposition) {
                acked.push(item.relay_id);
                continue;
            }
            if item.disposition == CoreInboundDisposition::Seen {
                let origin = self.message_origin_by_msg_id(item.msg_id)?;
                if core_consumed_seen_is_ackable(origin, own_user_id.clone()) {
                    acked.push(item.relay_id);
                }
            }
        }
        Ok(acked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outbound(id: u8, recipient: u8, expiry: i64, size: usize) -> OutboundEnvelope {
        OutboundEnvelope {
            msg_id: vec![id; 16],
            recipient_user_id: vec![recipient; 16],
            chat_id: vec![recipient; 16],
            sender_user_id: vec![1; 16],
            kind: 1,
            lamport: id as u64,
            timestamp: 1,
            hop_ttl: 7,
            expiry,
            recipient_hint: vec![recipient; 8],
            sealed: vec![0; size],
        }
    }

    fn receipt(id: u8, recipient: u8, expiry: i64, size: usize) -> OutgoingReceiptEnvelope {
        OutgoingReceiptEnvelope {
            msg_id: vec![id; 16],
            recipient_user_id: vec![recipient; 16],
            chat_id: vec![recipient; 16],
            sender_user_id: vec![1; 16],
            receipt_type: 1,
            through_lamport: id as u64,
            timestamp: 1,
            hop_ttl: 7,
            expiry,
            recipient_hint: vec![recipient; 8],
            sealed: vec![0; size],
        }
    }

    #[test]
    fn relay_acknowledgement_never_deletes_carried_or_seen_mail() {
        let items = vec![
            CoreRelayEnvelopeDisposition {
                relay_id: 1,
                msg_id: vec![1; 16],
                disposition: CoreInboundDisposition::Consumed,
            },
            CoreRelayEnvelopeDisposition {
                relay_id: 2,
                msg_id: vec![2; 16],
                disposition: CoreInboundDisposition::Carried,
            },
            CoreRelayEnvelopeDisposition {
                relay_id: 3,
                msg_id: vec![3; 16],
                disposition: CoreInboundDisposition::Expired,
            },
            CoreRelayEnvelopeDisposition {
                relay_id: 4,
                msg_id: vec![4; 16],
                disposition: CoreInboundDisposition::Seen,
            },
        ];
        assert_eq!(core_relay_ack_ids(items), vec![1, 3]);
    }

    // -- consumed-SEEN relay ack rule (DTN_TODOS.md §3.1 / D1) --------------

    #[test]
    fn consumed_seen_is_ackable_for_a_one_to_one_row_from_someone_else() {
        // Local storage convention: a 1:1 incoming row is keyed by the other
        // party, so chat_id == sender_user_id.
        let their_user_id = vec![1_u8; 16];
        let our_user_id = vec![9_u8; 16];
        let one_to_one_row = MessageOrigin {
            chat_id: their_user_id.clone(),
            sender_user_id: their_user_id,
        };
        assert!(core_consumed_seen_is_ackable(
            Some(one_to_one_row),
            our_user_id
        ));
    }

    #[test]
    fn consumed_seen_is_not_ackable_with_no_matching_store_row() {
        // No row means either "never consumed here" (just muled) or a
        // hidden kind (receipt, profile sync, ...) that leaves no durable
        // msg_id trace -- both must be left un-acked per the safety
        // invariant.
        let our_user_id = vec![9_u8; 16];
        assert!(!core_consumed_seen_is_ackable(None, our_user_id));
    }

    #[test]
    fn consumed_seen_is_never_ackable_for_a_group_row() {
        // A group row has chat_id = group.id, sender_user_id = another
        // member. The family shares one relay mailbox per family token, so
        // this relay copy is also every OTHER member's copy -- including a
        // member with internet-only connectivity and no BLE path. Acking it
        // would delete their message permanently: for group envelopes this
        // device is only one of several endpoint consumers, never "the"
        // consumer.
        let group_id = vec![7_u8; 16];
        let member_user_id = vec![1_u8; 16];
        let our_user_id = vec![9_u8; 16];
        let group_row = MessageOrigin {
            chat_id: group_id,
            sender_user_id: member_user_id,
        };
        assert!(!core_consumed_seen_is_ackable(Some(group_row), our_user_id));
    }

    #[test]
    fn consumed_seen_is_never_ackable_for_our_own_authored_row() {
        // Our own outbound message echoing back through relay proxy-polling
        // has a messages row too (sender_user_id == us), but that relay
        // copy exists for the recipient -- acking it would delete their
        // only remaining copy. In a 1:1 chat our authored row has chat_id =
        // the peer (so chat_id != sender already fails), but the
        // own-sender guard must hold on its own and not depend on that.
        let our_user_id = vec![9_u8; 16];
        let peer_user_id = vec![1_u8; 16];
        let own_outgoing_row = MessageOrigin {
            chat_id: peer_user_id,
            sender_user_id: our_user_id.clone(),
        };
        assert!(!core_consumed_seen_is_ackable(
            Some(own_outgoing_row),
            our_user_id.clone()
        ));
        // Degenerate shape with chat_id == sender_user_id == us: still
        // excluded purely by the own-sender guard.
        let own_keyed_row = MessageOrigin {
            chat_id: our_user_id.clone(),
            sender_user_id: our_user_id.clone(),
        };
        assert!(!core_consumed_seen_is_ackable(
            Some(own_keyed_row),
            our_user_id
        ));
    }

    #[test]
    fn consumed_seen_is_ackable_uses_content_equality() {
        // chat_id and sender_user_id are equal by content but distinct
        // Vec<u8> allocations, exactly as two separate SQLite column reads
        // produce.
        let their_user_id = vec![1_u8, 2, 3];
        let our_user_id = vec![9_u8, 9, 9];
        let row = MessageOrigin {
            chat_id: vec![1_u8, 2, 3],
            sender_user_id: their_user_id,
        };
        assert!(core_consumed_seen_is_ackable(Some(row), our_user_id));
    }

    #[test]
    fn relay_ack_ids_with_consumed_acks_a_ble_consumed_one_to_one_seen_copy_but_not_a_group_seen_copy(
    ) {
        // Integration test against the real store: a 1:1 envelope this
        // device already consumed over BLE (so it has a `messages` row) but
        // that the relay poll re-fetched (so its disposition is SEEN,
        // deduped before re-processing) should now be acked. A group
        // envelope with the same shape of "already consumed, now SEEN on
        // relay" must NOT be acked -- other family members still need the
        // shared-mailbox copy.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let own_user_id = vec![9_u8; 16];
        let their_user_id = vec![1_u8; 16];
        let group_id = vec![7_u8; 16];

        let one_to_one_msg_id = vec![21_u8; 16];
        store
            .insert_incoming_message(
                crate::StoredMessage {
                    chat_id: their_user_id.clone(),
                    sender_user_id: their_user_id,
                    lamport: 1,
                    timestamp: 1,
                    kind: 1,
                    payload: b"hi".to_vec(),
                },
                one_to_one_msg_id.clone(),
                None,
            )
            .unwrap();

        let group_msg_id = vec![22_u8; 16];
        store
            .insert_incoming_message(
                crate::StoredMessage {
                    chat_id: group_id,
                    sender_user_id: vec![2_u8; 16],
                    lamport: 1,
                    timestamp: 1,
                    kind: 1,
                    payload: b"hi group".to_vec(),
                },
                group_msg_id.clone(),
                None,
            )
            .unwrap();

        let unknown_msg_id = vec![23_u8; 16];

        let items = vec![
            CoreRelayEnvelopeDisposition {
                relay_id: 100,
                msg_id: one_to_one_msg_id,
                disposition: CoreInboundDisposition::Seen,
            },
            CoreRelayEnvelopeDisposition {
                relay_id: 200,
                msg_id: group_msg_id,
                disposition: CoreInboundDisposition::Seen,
            },
            CoreRelayEnvelopeDisposition {
                relay_id: 300,
                msg_id: unknown_msg_id,
                disposition: CoreInboundDisposition::Seen,
            },
            CoreRelayEnvelopeDisposition {
                relay_id: 400,
                msg_id: vec![24_u8; 16],
                disposition: CoreInboundDisposition::Carried,
            },
            CoreRelayEnvelopeDisposition {
                relay_id: 500,
                msg_id: vec![25_u8; 16],
                disposition: CoreInboundDisposition::Consumed,
            },
        ];

        let acked = store
            .core_relay_ack_ids_with_consumed(items, own_user_id)
            .unwrap();
        assert_eq!(acked, vec![100, 500]);
    }

    #[test]
    fn inbound_gate_prioritizes_seen_then_expiry() {
        assert_eq!(core_inbound_gate(false, 0, 10), CoreInboundGate::Seen);
        assert_eq!(core_inbound_gate(true, 10, 10), CoreInboundGate::Expired);
        assert_eq!(core_inbound_gate(true, 11, 10), CoreInboundGate::Dispatch);
    }

    #[test]
    fn outbound_spray_filters_known_and_stops_at_shared_budget() {
        let known = HashSet::from([vec![2; 16]]);
        let selected = select_own_outbound(
            vec![
                vec![outbound(1, 7, 100, 4), outbound(2, 7, 100, 4)],
                vec![outbound(3, 8, 100, 5), outbound(4, 8, 100, 1)],
            ],
            &known,
            10,
            8,
        );
        assert_eq!(
            selected
                .iter()
                .map(|item| item.msg_id[0])
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn receipt_spray_excludes_the_digest_peer_and_known_ids() {
        let known = HashSet::from([vec![3; 16]]);
        let selected = select_own_receipts(
            vec![
                receipt(1, 9, 100, 2),
                receipt(2, 8, 100, 2),
                receipt(3, 7, 100, 2),
                receipt(4, 7, 5, 2),
            ],
            &[9; 16],
            &known,
            10,
            10,
        );
        assert_eq!(
            selected
                .iter()
                .map(|item| item.msg_id[0])
                .collect::<Vec<_>>(),
            vec![2]
        );
    }

    #[test]
    fn hello_identity_is_locked_after_first_value() {
        assert!(core_hello_identity_matches(None, vec![1]));
        assert!(core_hello_identity_matches(Some(vec![1]), vec![1]));
        assert!(!core_hello_identity_matches(Some(vec![1]), vec![2]));
    }
}
