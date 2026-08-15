//! The inbound delivery *driver*.
//!
//! Every per-kind decision this module used to make — the chat-id/sender
//! binding, the sender/kind authorization, friend-request onboarding, receipts,
//! relay updates, profile sync, group invites, the durable message row and the
//! auto-receipt tail — now lives once in
//! [`cruisemesh_core::MessageStore::core_deliver_inbound`]. This file is the
//! driver boundary the refactor plan describes: it supplies the two inputs core
//! cannot read (this device's discovery switch, which lives in the desktop
//! config) and executes the one typed intent core cannot perform (writing the
//! LAN endpoint cache, a file outside SQLite).
//!
//! There is deliberately **no `match` on `body.kind` here**, and
//! `no_per_kind_branching` in this module's tests keeps it that way: a third
//! copy of inbound delivery policy is exactly what this module used to be.

use std::sync::Arc;

use anyhow::{Context, Result};
use cruisemesh_core::{
    CoreDeliveryVerdict, CoreDiscoveryPolicyState, CoreInboundCommit, Identity, MessageArrival,
    MessageStore,
};

use crate::lan::endpoint_cache::EndpointCache;

/// Reads this device's friends-of-friends switch: `(enabled, revision)`.
pub type DiscoveryPolicy = Arc<dyn Fn() -> (bool, u64) + Send + Sync>;

pub struct DeliveryDispatcher {
    store: Arc<MessageStore>,
    identity: Identity,
    endpoints: EndpointCache,
    discovery: DiscoveryPolicy,
}

impl DeliveryDispatcher {
    pub fn new(
        store: Arc<MessageStore>,
        identity: Identity,
        endpoints: EndpointCache,
        discovery: DiscoveryPolicy,
    ) -> Self {
        Self {
            store,
            identity,
            endpoints,
            discovery,
        }
    }

    /// Apply one opened body. An `Err` means the delivery did not land
    /// durably, so the caller must not commit: the envelope stays
    /// re-presentable and its relay copy stays unacked (DTN D4 / T4-06).
    pub fn deliver(
        &self,
        sender_user_id: Vec<u8>,
        payload: Vec<u8>,
        commit: &CoreInboundCommit,
        arrival: MessageArrival,
    ) -> Result<()> {
        let (enabled, revision) = (self.discovery)();
        let delivery = self
            .store
            .core_deliver_inbound(
                self.identity.clone(),
                sender_user_id,
                payload,
                commit.clone(),
                arrival,
                CoreDiscoveryPolicyState { enabled, revision },
            )
            .context("core inbound delivery failed")?;

        if let Some(hint) = delivery.endpoint_hint {
            self.endpoints
                .record(hint.peer_user_id, hint.endpoint, hint.observed_at_ms)?;
        }

        if delivery.verdict != CoreDeliveryVerdict::Applied {
            // A terminal policy verdict, not a durability failure: the sole
            // endpoint opened the envelope, so it is consumed and committed
            // like any other. Retrying could not make an already-authored
            // envelope more authorized.
            tracing::warn!(
                kind = delivery.kind,
                verdict = ?delivery.verdict,
                "core declined to apply an inbound body"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    /// The point of this module is that it no longer decides anything per
    /// kind. A `match` on a message kind, or a `KIND_*` constant reappearing
    /// here, means a fourth copy of delivery policy has started to grow —
    /// which is what the fold into `core_deliver_inbound` removed.
    #[test]
    fn the_delivery_driver_has_no_per_kind_branching() {
        let source = include_str!("delivery.rs");
        let code: String = source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let code = code
            .split_once("mod tests {")
            .map(|(before, _)| before.to_string())
            .unwrap_or(code);

        assert!(
            !code.contains("KIND_"),
            "the desktop delivery driver names a message kind again; per-kind policy belongs \
             in core::session::mesh_receive"
        );
        assert!(
            !code.contains("match body") && !code.contains(".kind {"),
            "the desktop delivery driver branches on a message kind again"
        );
        for decoder in [
            "decode_receipt_content",
            "decode_group_invite_content",
            "decode_profile_sync_content",
            "decode_relay_update_content",
            "decode_extended_message_body",
            "parse_friend_request_content",
        ] {
            assert!(
                !code.contains(decoder),
                "the desktop delivery driver decodes {decoder} again; core owns body semantics"
            );
        }
    }
}
