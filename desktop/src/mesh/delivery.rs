use std::sync::Arc;

use anyhow::{bail, Context, Result};
use cruisemesh_core::{
    core_pairwise_sender_authorized, decode_extended_message_body, decode_group_invite_content,
    decode_lan_endpoint_content, decode_profile_sync_content, decode_receipt_content,
    decode_relay_update_content, friend_card_user_id, parse_friend_request_content, Contact,
    ContactDiscoveryPolicy, CoreInboundCommit, ExtendedMessageBody, Identity,
    IncomingMessageInsertOutcome, MessageArrival, MessageStore, StoredMessage, KIND_FRIEND_REQUEST,
    KIND_GROUP_INVITE, KIND_LAN_ENDPOINT_HINT, KIND_PROFILE_SYNC, KIND_RECEIPT, KIND_RELAY_UPDATE,
    RECEIPT_TYPE_DELIVERED,
};

use crate::lan::endpoint_cache::EndpointCache;

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

    pub fn deliver(
        &self,
        sender_user_id: Vec<u8>,
        payload: Vec<u8>,
        commit: &CoreInboundCommit,
        arrival: MessageArrival,
    ) -> Result<()> {
        let body =
            decode_extended_message_body(payload).context("invalid delivered message body")?;
        let received_at = arrival.received_at;
        // Core sets hidden_kind for every decoded pairwise delivery and never
        // for a group delivery. Do not infer this from an untrusted chat_id:
        // a pairwise sender can put an arbitrary id in its signed body.
        let pairwise = commit.hidden_kind.is_some();
        if pairwise {
            if body.chat_id != sender_user_id {
                tracing::warn!(
                    kind = body.kind,
                    "dropping pairwise envelope whose chat id does not match its verified sender"
                );
                return Ok(());
            }
            let sender_is_contact = self.store.get_contact(sender_user_id.clone())?.is_some();
            if !core_pairwise_sender_authorized(
                body.kind,
                sender_is_contact,
                sender_user_id == self.identity.user_id,
            ) {
                // This is a terminal policy rejection, not a durability
                // failure. The sole endpoint opened the envelope, so consume
                // it exactly as Android does; retrying cannot make this
                // already-authored envelope more authorized and used to crash
                // the Windows relay loop when kind 6 arrived before kind 3.
                tracing::warn!(
                    kind = body.kind,
                    "dropping pairwise envelope from an unauthorized sender"
                );
                return Ok(());
            }
        }

        match body.kind {
            KIND_FRIEND_REQUEST if pairwise => {
                self.deliver_friend_request(&sender_user_id, &body.content)?;
                self.persist(&sender_user_id, &body, commit, arrival)?;
            }
            KIND_RECEIPT if pairwise => {
                let receipt = decode_receipt_content(body.content.clone())?;
                if receipt.sender_user_id != self.identity.user_id {
                    bail!("receipt does not acknowledge this helper's messages");
                }
                if let Some(group_id) = receipt.group_id {
                    self.store.record_group_receipt(
                        group_id,
                        self.identity.user_id.clone(),
                        sender_user_id.clone(),
                        receipt.receipt_type,
                        receipt.lamport,
                        Some(arrival.transport),
                    )?;
                } else {
                    self.store.record_receipt(
                        sender_user_id.clone(),
                        self.identity.user_id.clone(),
                        receipt.receipt_type,
                        receipt.lamport,
                        Some(arrival.transport),
                        Some(arrival.received_at),
                    )?;
                }
            }
            KIND_LAN_ENDPOINT_HINT if pairwise => {
                let content = decode_lan_endpoint_content(body.content.clone())?;
                self.persist(&sender_user_id, &body, commit, arrival.clone())?;
                self.endpoints
                    .record(sender_user_id.clone(), content, arrival.received_at)?;
            }
            KIND_RELAY_UPDATE if pairwise => {
                let content = decode_relay_update_content(body.content.clone())?;
                self.persist(&sender_user_id, &body, commit, arrival)?;
                // The hidden row remains durable even if core rejects a
                // mis-scoped or over-privileged credential update.
                let _ = self
                    .store
                    .apply_contact_relay_update(sender_user_id.clone(), content);
            }
            KIND_PROFILE_SYNC if pairwise => {
                let content = decode_profile_sync_content(body.content.clone())?;
                self.persist(&sender_user_id, &body, commit, arrival)?;
                if let Some(mut contact) = self.store.get_contact(sender_user_id.clone())? {
                    self.store
                        .upsert_contact_discovery_policy(ContactDiscoveryPolicy {
                            user_id: sender_user_id.clone(),
                            protocol_version: content.friends_of_friends_version,
                            enabled: content.friends_of_friends_enabled,
                            revision: content.friends_of_friends_revision,
                        })?;
                    self.store.set_contact_avatar(
                        sender_user_id.clone(),
                        (!content.avatar.is_empty()).then_some(content.avatar),
                        content.avatar_epoch,
                    )?;
                    if contact.name != content.name {
                        contact.name = content.name;
                        self.store.upsert_contact(contact)?;
                    }
                }
            }
            KIND_GROUP_INVITE if pairwise => {
                let group = decode_group_invite_content(body.content.clone())?;
                if !group.member_user_ids.contains(&sender_user_id)
                    || !group.member_user_ids.contains(&self.identity.user_id)
                {
                    bail!("group invite membership does not include sender and helper");
                }
                self.store.upsert_group(group.clone())?;
                let mut stored_body = body.clone();
                stored_body.chat_id = group.id;
                self.persist(&sender_user_id, &stored_body, commit, arrival)?;
            }
            _ => {
                // Stage 1 has no presentation surface. Validated messages still
                // land durably so Stage 2 can render them and D4 may commit.
                self.persist(&sender_user_id, &body, commit, arrival)?;
            }
        }
        if pairwise && body.kind != KIND_RECEIPT {
            if let Some(contact) = self.store.get_contact(sender_user_id.clone())? {
                let through = self
                    .store
                    .highest_contiguous_lamport(sender_user_id.clone(), sender_user_id.clone())?;
                let _ = self.store.author_receipt(
                    self.identity.clone(),
                    contact,
                    sender_user_id,
                    RECEIPT_TYPE_DELIVERED,
                    through,
                    received_at,
                )?;
            }
        }
        Ok(())
    }

    fn deliver_friend_request(&self, sender_user_id: &[u8], content: &[u8]) -> Result<()> {
        let json = std::str::from_utf8(content).context("friend request is not UTF-8")?;
        let request = parse_friend_request_content(json.to_string())?;
        if friend_card_user_id(request.card.clone()) != sender_user_id {
            bail!("friend request card does not match its verified sender");
        }
        if let Some(shared) = request.shared {
            self.hold_shared_friend_request(sender_user_id, request.card, shared)?;
            return Ok(());
        }
        self.store.upsert_imported_contact(Contact {
            user_id: sender_user_id.to_vec(),
            name: request.card.name,
            sign_pk: request.card.sign_pk,
            agree_pk: request.card.agree_pk,
            relay_url: request.card.relay_url,
            relay_token: request.card.relay_token,
            nickname: None,
        })?;
        Ok(())
    }

    fn hold_shared_friend_request(
        &self,
        sender_user_id: &[u8],
        card: cruisemesh_core::FriendCard,
        shared: cruisemesh_core::SharedFriendCard,
    ) -> Result<()> {
        let (enabled, revision) = (self.discovery)();
        let Some(sharer) = self.store.get_contact(shared.sharer_user_id.clone())? else {
            return Ok(());
        };
        if self.store.is_user_blocked(shared.sharer_user_id.clone())? {
            return Ok(());
        }
        if cruisemesh_core::friend_card_user_id(shared.card.clone()) != self.identity.user_id {
            return Ok(());
        }
        if !enabled {
            return Ok(());
        }
        if !cruisemesh_core::verify_shared_friend_card(
            shared.clone(),
            sharer.sign_pk,
            self.identity.user_id.clone(),
            revision,
            now_ms(),
        )? {
            return Ok(());
        }
        if self
            .store
            .get_shared_request_dismissal(sender_user_id.to_vec())?
            .map(|row| row.suppressed)
            .unwrap_or(false)
        {
            return Ok(());
        }
        let now = now_ms();
        self.store
            .upsert_pending_shared_request(cruisemesh_core::PendingSharedRequest {
                requester_user_id: sender_user_id.to_vec(),
                name: card.name,
                sign_pk: card.sign_pk,
                agree_pk: card.agree_pk,
                relay_url: card.relay_url,
                relay_token: card.relay_token,
                sharer_user_id: shared.sharer_user_id,
                expires_at_ms: shared.expires_at_ms,
                first_seen_ms: now,
                last_prompted_ms: 0,
            })?;
        let _ = self
            .store
            .note_shared_request_prompt(sender_user_id.to_vec(), now);
        Ok(())
    }

    fn persist(
        &self,
        sender_user_id: &[u8],
        body: &ExtendedMessageBody,
        commit: &CoreInboundCommit,
        arrival: MessageArrival,
    ) -> Result<()> {
        let outcome = self.store.insert_incoming_message_with_arrival(
            StoredMessage {
                chat_id: body.chat_id.clone(),
                sender_user_id: sender_user_id.to_vec(),
                lamport: body.lamport,
                timestamp: body.timestamp,
                kind: body.kind,
                payload: body.content.clone(),
            },
            commit.msg_id.clone(),
            body.reply_to_msg_id.clone(),
            arrival,
        )?;
        match outcome {
            IncomingMessageInsertOutcome::Inserted | IncomingMessageInsertOutcome::Duplicate => {
                Ok(())
            }
            IncomingMessageInsertOutcome::QuarantinedConflict => {
                bail!("message stream conflict was quarantined")
            }
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(i64::MAX as u128) as i64
}
