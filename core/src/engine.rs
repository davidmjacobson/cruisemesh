//! Shared mesh state-machine decisions.
//!
//! Native code owns radios, sockets, HTTP, and lifecycle. This module owns
//! the decisions that must remain identical on Android and iOS: inbound
//! gating, relay acknowledgement safety, HELLO identity locking, and the
//! complete digest-time mule spray plan.

use std::collections::HashSet;

use crate::{
    encode_envelope_frame, CarriedEnvelope, CoreError, MessageStore, OutboundEnvelope,
    OutgoingReceiptEnvelope, RECEIPT_TYPE_DELIVERED,
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

/// Inbound flood-dedupe + expiry gate (DESIGN.md §5.3).
///
/// `is_new_msg_id` must come from a non-mutating check
/// ([`crate::SeenIds::contains`]), never from [`crate::SeenIds::check_and_record`]
/// -- see DTN D4 / `gossip.rs` module docs. The caller is responsible for
/// calling [`crate::SeenIds::record`] itself, and only once the envelope has
/// reached a terminal handled state (consumed, carried, expired-drop, or
/// relayed-onward-only). Invariant: an envelope whose durable handling
/// failed must be re-presentable; an envelope that was handled (even by
/// deliberate drop, e.g. the `Expired` arm below) must be deduped.
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

#[uniffi::export]
pub fn core_should_ack_inbound(disposition: CoreInboundDisposition) -> bool {
    matches!(
        disposition,
        CoreInboundDisposition::Consumed | CoreInboundDisposition::Expired
    )
}

#[uniffi::export]
pub fn core_relay_ack_ids(items: Vec<CoreRelayEnvelopeDisposition>) -> Vec<i64> {
    items
        .into_iter()
        .filter(|item| core_should_ack_inbound(item.disposition))
        .map(|item| item.relay_id)
        .collect()
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
                disposition: CoreInboundDisposition::Consumed,
            },
            CoreRelayEnvelopeDisposition {
                relay_id: 2,
                disposition: CoreInboundDisposition::Carried,
            },
            CoreRelayEnvelopeDisposition {
                relay_id: 3,
                disposition: CoreInboundDisposition::Expired,
            },
            CoreRelayEnvelopeDisposition {
                relay_id: 4,
                disposition: CoreInboundDisposition::Seen,
            },
        ];
        assert_eq!(core_relay_ack_ids(items), vec![1, 3]);
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
