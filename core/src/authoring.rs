use rusqlite::{params, OptionalExtension, Transaction};

use crate::causal_order::causal_display_timestamp;
use crate::device_link::activation::CoreLinkGatedAction;
use crate::outbound_retirement::{authored_expiry, backfill_rejoins_the_queue, retire_superseded};
use crate::store::{
    outbound_message_dedupe_key, own_authoring_device_id, row_to_outbound, row_to_outgoing_receipt,
    store_err, upsert_group_tx,
};
use crate::sync_outbound::clear_chat_draft;
use crate::{
    apply_group_metadata_update, compute_recipient_hint, create_group_metadata_update,
    default_expiry, encode_envelope_frame, encode_group_invite_content,
    encode_group_metadata_update, encode_message_body, encode_message_body_extended,
    encode_message_body_with_reply, encode_receipt_content, generate_msg_id, seal_group_message,
    seal_message, Contact, CoreError, Group, GroupMetadataUpdate, Identity, MessageBody,
    MessageStore, OutboundEnvelope, OutgoingReceiptEnvelope, ReceiptContent, StoredMessage,
    DEFAULT_HOP_TTL, KIND_ATTACHMENT_MANIFEST, KIND_FRIEND_DIRECTORY, KIND_FRIEND_REQUEST,
    KIND_GROUP_INVITE, KIND_GROUP_METADATA_UPDATE, KIND_INTRODUCED_FRIEND_REQUEST,
    KIND_LAN_ENDPOINT_HINT, KIND_PROFILE_SYNC, KIND_REACTION, KIND_RECEIPT, KIND_RELAY_UPDATE,
    KIND_ROSTER_GOSSIP, KIND_TEXT, LEGACY_DEVICE_ID, RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
};

/// The device id to put in a sealed body, given the stream a row lives on.
///
/// [`LEGACY_DEVICE_ID`] becomes `None`, so an unlinked device emits *no*
/// extension TLV at all and its envelopes stay byte-identical to what every
/// build in the field emits today (§5, §12). A linked device emits the real id
/// and a legacy receiver skips the unknown-to-it-shaped-but-well-formed TLV, as
/// WPT's tolerance tests pin.
fn wire_device_id(sender_device_id: &[u8]) -> Option<Vec<u8>> {
    (sender_device_id != LEGACY_DEVICE_ID).then(|| sender_device_id.to_vec())
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct AuthoredEnvelope {
    pub message: StoredMessage,
    pub envelope: OutboundEnvelope,
    pub frame: Vec<u8>,
    pub acknowledged_delivered: u64,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct AuthoredReceipt {
    pub envelope: OutgoingReceiptEnvelope,
    pub frame: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct AuthoredGroupMetadataUpdate {
    pub group: Group,
    pub update: GroupMetadataUpdate,
    pub authored: AuthoredEnvelope,
}

#[uniffi::export]
impl MessageStore {
    /// Assign, seal, and durably queue a pairwise chat-stream message in one
    /// store transaction. The counter ratchets past both receipt watermarks.
    pub fn author_pairwise_message(
        &self,
        identity: Identity,
        contact: Contact,
        kind: u8,
        payload: Vec<u8>,
        reply_to_msg_id: Option<Vec<u8>>,
        timestamp_ms: i64,
    ) -> Result<AuthoredEnvelope, CoreError> {
        // §9.4: a device still being adopted may not author ANYTHING. First,
        // before a lamport is spent or a transaction is opened.
        self.guard_link_gate(CoreLinkGatedAction::Author)?;
        if !is_pairwise_kind(kind) {
            return Err(CoreError::Malformed(format!(
                "unsupported pairwise authored kind {kind}"
            )));
        }
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let (lamport, acknowledged_delivered) =
            next_authored_lamport(&tx, &contact.user_id, &identity.user_id)?;
        // A message we author is causally after everything already in this
        // chat, whatever the two phones' clocks think.
        let display_ts = causal_timestamp_for_chat(&tx, &contact.user_id, timestamp_ms)?;
        let message = StoredMessage {
            chat_id: contact.user_id.clone(),
            sender_user_id: identity.user_id.clone(),
            lamport,
            timestamp: display_ts,
            kind,
            payload,
            // §5: this device's own stream, or the legacy one on an install
            // that has never linked. Read inside the transaction that is about
            // to write the row, so a link landing concurrently cannot leave the
            // stored row and the sealed body naming different streams.
            sender_device_id: own_authoring_device_id(&tx)?,
        };
        let envelope = build_pairwise_envelope(
            identity,
            &contact,
            &message,
            reply_to_msg_id.as_deref(),
            timestamp_ms,
            generate_msg_id(),
            // Per-kind, not flat: a payload that states its own fifteen-minute
            // validity has no business being carried for a week. See
            // `crate::outbound_retirement::authored_expiry` for the decision on
            // every kind. This is an *authoring* policy — how long a freshly
            // minted envelope deserves to chase its recipient — and the repair
            // re-seal in `backfill_pairwise_envelope` deliberately does not
            // share it.
            authored_expiry(kind, timestamp_ms),
        )?;
        insert_authored_rows(
            &tx,
            &message,
            &envelope,
            reply_to_msg_id.as_deref(),
            timestamp_ms,
        )?;
        tx.commit().map_err(store_err)?;
        Ok(authored(message, envelope, acknowledged_delivered))
    }

    pub fn author_friend_request(
        &self,
        identity: Identity,
        contact: Contact,
        friend_card_json: String,
        timestamp_ms: i64,
    ) -> Result<AuthoredEnvelope, CoreError> {
        self.author_pairwise_message(
            identity,
            contact,
            KIND_FRIEND_REQUEST,
            friend_card_json.into_bytes(),
            None,
            timestamp_ms,
        )
    }

    /// Re-seal one locally authored message for a peer whose gap-aware digest
    /// says it is missing that lamport, when the outbound queue no longer holds
    /// the sealed copy. Both shells' digest responders reach this through
    /// `queuedByLamport[lamport] ?: backfill(...)`.
    ///
    /// A queue row can be absent for three quite different reasons, and #283
    /// made the difference matter. Before it, absence meant only "authored
    /// before the outbound queue table existed", so re-sealing *and re-queuing*
    /// was right. Now it can also mean "retired on proof of delivery" or
    /// "retired because a newer generation of this snapshot kind superseded
    /// it", and re-queuing those would undo the retirement on the next digest —
    /// with the queue regrown, the relay uploader re-posting acknowledged mail,
    /// and (before the identity fix below) a fresh random `msg_id` defeating
    /// every dedupe set on both sides.
    ///
    /// So this function separates the two obligations that used to be one:
    ///
    /// * **Re-sealing is unconditional.** The peer asked for the lamport, and
    ///   its digest watermark is the gap-aware contiguous one, so it genuinely
    ///   may be missing a message our own MAX-based delivered watermark
    ///   covers. Refusing to answer would strand the peer's contiguity
    ///   permanently. The returned envelope is for immediate transmission on
    ///   the link that asked.
    /// * **Re-queuing is conditional**, on
    ///   [`crate::outbound_retirement::backfill_rejoins_the_queue`]: only a row
    ///   whose absence is unexplained — genuinely legacy, still undelivered,
    ///   still the kind of thing the queue is for — goes back in. A repair copy
    ///   never re-enters the relay-upload set or the standing spray set.
    ///
    /// The envelope's identity is the message's own persisted `msg_id`, not a
    /// new random one, so a message re-sealed on ten successive digests carries
    /// one id forever: the peer's seen-set dedupes it, and the once-per-session
    /// hidden-kind offer bound in both shells (which is keyed on `msg_id`)
    /// still bounds it. A pre-`msg_id` legacy row is given one and it is
    /// written back, so the identity is durable from then on.
    ///
    /// A stable identity does mean a peer that already recorded this `msg_id`
    /// as seen drops the retransmission. That is the point nearly everywhere —
    /// a peer that holds the message needs no second copy, and the receive path
    /// deliberately records an id only once handling reached a terminal state,
    /// so an envelope whose store failed stays re-presentable. The one case it
    /// costs anything is a peer that received the envelope and dropped it as
    /// expired: it drops the re-seal too, until its bounded FIFO seen-set
    /// evicts the id. That is the trade-off any stable identity makes, and the
    /// alternative — a fresh random id on every digest — is precisely the
    /// resend chatter the HELLO2 capability flags were introduced to stop.
    ///
    /// Its delivery expiry is [`crate::default_expiry`] from the authoring
    /// timestamp — the flat default, deliberately not the per-kind authored
    /// lifetime. The short ephemeral lifetimes are an authoring policy; applied
    /// here they would hand back an envelope that expired the moment it was
    /// built (a thirty-minute lifetime measured from a two-day-old timestamp),
    /// which the shells would frame anyway and the recipient's inbound gate
    /// would drop as expired — dead bytes on the most constrained link, and a
    /// hole in the peer's stream that could never close. A stale endpoint
    /// inside such an envelope is still refused: the payload's own validity
    /// stamp owns that check (#278) and is untouched by this.
    ///
    /// Repeated calls return the already-persisted envelope when one exists.
    pub fn backfill_pairwise_envelope(
        &self,
        identity: Identity,
        contact: Contact,
        message: StoredMessage,
        reply_to_msg_id: Option<Vec<u8>>,
    ) -> Result<AuthoredEnvelope, CoreError> {
        // §9.4: re-sealing a message is authoring one, as far as the mesh can
        // tell.
        self.guard_link_gate(CoreLinkGatedAction::Author)?;
        if !is_pairwise_kind(message.kind)
            || message.chat_id != contact.user_id
            || message.sender_user_id != identity.user_id
        {
            return Err(CoreError::Malformed(
                "legacy message does not match pairwise author or contact".to_string(),
            ));
        }
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let existing: Option<OutboundEnvelope> = tx
            .query_row(
                "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, kind, lamport,
                        timestamp, hop_ttl, expiry, recipient_hint, sealed
                 FROM outbound_envelopes
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND kind = ?3
                   AND lamport = ?4 AND recipient_user_id = ?5",
                params![
                    &message.chat_id,
                    &message.sender_user_id,
                    message.kind as i64,
                    message.lamport as i64,
                    &contact.user_id,
                ],
                row_to_outbound,
            )
            .optional()
            .map_err(store_err)?;
        if let Some(envelope) = existing {
            return Ok(authored(message, envelope, 0));
        }
        let msg_id = stable_backfill_msg_id(&tx, &message)?;
        let envelope = build_pairwise_envelope(
            identity,
            &contact,
            &message,
            reply_to_msg_id.as_deref(),
            message.timestamp,
            msg_id,
            default_expiry(message.timestamp),
        )?;
        record_authored_watermark(&tx, &message)?;
        if backfill_rejoins_the_queue(&tx, &envelope)? {
            queue_outbound_row(&tx, &envelope, message.timestamp)?;
        }
        tx.commit().map_err(store_err)?;
        Ok(authored(message, envelope, 0))
    }

    /// Assign, group-seal, and durably queue one shared group envelope.
    pub fn author_group_message(
        &self,
        identity: Identity,
        group: Group,
        kind: u8,
        payload: Vec<u8>,
        reply_to_msg_id: Option<Vec<u8>>,
        timestamp_ms: i64,
    ) -> Result<AuthoredEnvelope, CoreError> {
        // §9.4.
        self.guard_link_gate(CoreLinkGatedAction::Author)?;
        if kind != KIND_TEXT && kind != KIND_ATTACHMENT_MANIFEST && kind != KIND_REACTION {
            return Err(CoreError::Malformed(format!(
                "unsupported group authored kind {kind}"
            )));
        }
        if !group.member_user_ids.contains(&identity.user_id) {
            return Err(CoreError::Malformed(
                "group author is not a member".to_string(),
            ));
        }
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let (lamport, acknowledged_delivered) =
            next_authored_lamport(&tx, &group.id, &identity.user_id)?;
        let display_ts = causal_timestamp_for_chat(&tx, &group.id, timestamp_ms)?;
        let message = StoredMessage {
            chat_id: group.id.clone(),
            sender_user_id: identity.user_id.clone(),
            lamport,
            timestamp: display_ts,
            kind,
            payload,
            sender_device_id: own_authoring_device_id(&tx)?,
        };
        let body = encoded_body(&message, group.id.clone(), reply_to_msg_id.as_deref())?;
        let msg_id = generate_msg_id();
        let sealed = seal_group_message(identity, group.clone(), body)?;
        let envelope = OutboundEnvelope {
            msg_id,
            recipient_user_id: group.id.clone(),
            chat_id: group.id.clone(),
            sender_user_id: message.sender_user_id.clone(),
            kind,
            lamport,
            timestamp: display_ts,
            hop_ttl: DEFAULT_HOP_TTL,
            expiry: default_expiry(timestamp_ms),
            recipient_hint: compute_recipient_hint(group.id, timestamp_ms),
            sealed,
        };
        insert_authored_rows(
            &tx,
            &message,
            &envelope,
            reply_to_msg_id.as_deref(),
            timestamp_ms,
        )?;
        tx.commit().map_err(store_err)?;
        Ok(authored(message, envelope, acknowledged_delivered))
    }

    /// Atomically apply a local add-only group metadata change and queue its
    /// hidden group-stream update. The returned frame uses the existing group
    /// key and normal DTN fan-out path.
    pub fn author_group_metadata_update(
        &self,
        identity: Identity,
        group: Group,
        name: String,
        member_user_ids: Vec<Vec<u8>>,
        timestamp_ms: i64,
    ) -> Result<AuthoredGroupMetadataUpdate, CoreError> {
        // §9.4.
        self.guard_link_gate(CoreLinkGatedAction::Author)?;
        let update = create_group_metadata_update(
            group.clone(),
            identity.user_id.clone(),
            name,
            member_user_ids,
        )?;
        let updated_group =
            apply_group_metadata_update(group.clone(), update.clone(), identity.user_id.clone())?
                .ok_or_else(|| {
                CoreError::Malformed("group metadata update had no effect".to_string())
            })?;
        let payload = encode_group_metadata_update(update.clone())?;

        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let (lamport, acknowledged_delivered) =
            next_authored_lamport(&tx, &group.id, &identity.user_id)?;
        let display_ts = causal_timestamp_for_chat(&tx, &group.id, timestamp_ms)?;
        let message = StoredMessage {
            chat_id: group.id.clone(),
            sender_user_id: identity.user_id.clone(),
            lamport,
            timestamp: display_ts,
            kind: KIND_GROUP_METADATA_UPDATE,
            payload,
            sender_device_id: own_authoring_device_id(&tx)?,
        };
        let body = encoded_body(&message, group.id.clone(), None)?;
        let msg_id = generate_msg_id();
        let sealed = seal_group_message(identity, group.clone(), body)?;
        let envelope = OutboundEnvelope {
            msg_id,
            recipient_user_id: group.id.clone(),
            chat_id: group.id,
            sender_user_id: message.sender_user_id.clone(),
            kind: KIND_GROUP_METADATA_UPDATE,
            lamport,
            timestamp: display_ts,
            hop_ttl: DEFAULT_HOP_TTL,
            expiry: default_expiry(timestamp_ms),
            recipient_hint: compute_recipient_hint(message.chat_id.clone(), timestamp_ms),
            sealed,
        };
        upsert_group_tx(&tx, &updated_group)?;
        insert_authored_rows(&tx, &message, &envelope, None, timestamp_ms)?;
        tx.commit().map_err(store_err)?;
        Ok(AuthoredGroupMetadataUpdate {
            group: updated_group,
            update,
            authored: authored(message, envelope, acknowledged_delivered),
        })
    }

    /// Queue one pairwise-sealed group invite for every non-self member while
    /// storing the logical invite message exactly once.
    pub fn queue_group_invites(
        &self,
        identity: Identity,
        group: Group,
        members: Vec<Contact>,
        timestamp_ms: i64,
    ) -> Result<Vec<AuthoredEnvelope>, CoreError> {
        // §9.4.
        self.guard_link_gate(CoreLinkGatedAction::Author)?;
        let invite = encode_group_invite_content(group.clone())?;
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let (lamport, acknowledged_delivered) =
            next_authored_lamport(&tx, &group.id, &identity.user_id)?;
        let display_ts = causal_timestamp_for_chat(&tx, &group.id, timestamp_ms)?;
        let message = StoredMessage {
            chat_id: group.id,
            sender_user_id: identity.user_id.clone(),
            lamport,
            timestamp: display_ts,
            kind: KIND_GROUP_INVITE,
            payload: invite,
            sender_device_id: own_authoring_device_id(&tx)?,
        };
        let mut authored_invites = Vec::new();
        for member in members {
            if member.user_id == identity.user_id {
                continue;
            }
            let envelope = build_pairwise_envelope(
                identity.clone(),
                &member,
                &message,
                None,
                timestamp_ms,
                generate_msg_id(),
                authored_expiry(KIND_GROUP_INVITE, timestamp_ms),
            )?;
            insert_authored_rows(&tx, &message, &envelope, None, timestamp_ms)?;
            authored_invites.push(authored(message.clone(), envelope, acknowledged_delivered));
        }
        tx.commit().map_err(store_err)?;
        Ok(authored_invites)
    }

    /// Advance a cumulative outgoing receipt and its sealed retry envelope in
    /// one transaction. A stale/equal watermark returns `None` unchanged.
    pub fn author_receipt(
        &self,
        identity: Identity,
        contact: Contact,
        acked_sender_user_id: Vec<u8>,
        receipt_type: u8,
        through_lamport: u64,
        timestamp_ms: i64,
    ) -> Result<Option<AuthoredReceipt>, CoreError> {
        // §9.4. A receipt is authored mail and is also half of an ack; a device
        // that has not finished being adopted owes neither.
        self.guard_link_gate(CoreLinkGatedAction::Author)?;
        if receipt_type != RECEIPT_TYPE_DELIVERED && receipt_type != RECEIPT_TYPE_READ {
            return Err(CoreError::Malformed("invalid receipt type".to_string()));
        }
        validate_sqlite_lamport("receipt watermark", through_lamport)?;
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT through_lamport FROM outgoing_receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![contact.user_id, acked_sender_user_id, receipt_type as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        if current.is_some_and(|value| value >= through_lamport as i64) {
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO outgoing_receipts (chat_id, sender_user_id, receipt_type, through_lamport)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                through_lamport = excluded.through_lamport",
            params![
                contact.user_id,
                acked_sender_user_id,
                receipt_type as i64,
                through_lamport as i64
            ],
        )
        .map_err(store_err)?;

        let content = encode_receipt_content(ReceiptContent {
            chat_id: identity.user_id.clone(),
            sender_user_id: acked_sender_user_id.clone(),
            lamport: through_lamport,
            receipt_type,
            group_id: None,
        })?;
        let body = encode_message_body(MessageBody {
            kind: KIND_RECEIPT,
            chat_id: identity.user_id.clone(),
            lamport: 0,
            timestamp: timestamp_ms,
            content,
        })?;
        let msg_id = generate_msg_id();
        let envelope = OutgoingReceiptEnvelope {
            msg_id: msg_id.clone(),
            recipient_user_id: contact.user_id.clone(),
            chat_id: contact.user_id.clone(),
            sender_user_id: acked_sender_user_id,
            receipt_type,
            through_lamport,
            timestamp: timestamp_ms,
            hop_ttl: DEFAULT_HOP_TTL,
            expiry: default_expiry(timestamp_ms),
            recipient_hint: compute_recipient_hint(contact.user_id, timestamp_ms),
            sealed: seal_message(identity, contact.agree_pk, body)?,
        };
        tx.execute(
            "INSERT INTO outgoing_receipt_envelopes
                (chat_id, sender_user_id, receipt_type, through_lamport, msg_id,
                 recipient_user_id, timestamp, hop_ttl, expiry, recipient_hint, sealed, queued_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                through_lamport = excluded.through_lamport, msg_id = excluded.msg_id,
                recipient_user_id = excluded.recipient_user_id, timestamp = excluded.timestamp,
                hop_ttl = excluded.hop_ttl, expiry = excluded.expiry,
                recipient_hint = excluded.recipient_hint, sealed = excluded.sealed,
                queued_at = excluded.queued_at, relay_posted_at = NULL",
            params![
                envelope.chat_id,
                envelope.sender_user_id,
                receipt_type as i64,
                through_lamport as i64,
                envelope.msg_id,
                envelope.recipient_user_id,
                timestamp_ms,
                envelope.hop_ttl as i64,
                envelope.expiry,
                envelope.recipient_hint,
                envelope.sealed,
                timestamp_ms
            ],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        let frame = encode_envelope_frame(
            msg_id,
            envelope.hop_ttl,
            envelope.expiry,
            envelope.recipient_hint.clone(),
            envelope.sealed.clone(),
        );
        Ok(Some(AuthoredReceipt { envelope, frame }))
    }

    /// Return a durably queued sealed receipt for at least the requested
    /// watermark. Existing equal/newer envelopes are reused byte-for-byte;
    /// a missing or stale envelope is advanced atomically with the local
    /// outgoing receipt watermark.
    pub fn ensure_authored_receipt(
        &self,
        identity: Identity,
        contact: Contact,
        acked_sender_user_id: Vec<u8>,
        receipt_type: u8,
        through_lamport: u64,
        timestamp_ms: i64,
    ) -> Result<AuthoredReceipt, CoreError> {
        // §9.4.
        self.guard_link_gate(CoreLinkGatedAction::Author)?;
        if receipt_type != RECEIPT_TYPE_DELIVERED && receipt_type != RECEIPT_TYPE_READ {
            return Err(CoreError::Malformed("invalid receipt type".to_string()));
        }
        if through_lamport == 0 {
            return Err(CoreError::Malformed(
                "receipt watermark must be positive".to_string(),
            ));
        }
        validate_sqlite_lamport("receipt watermark", through_lamport)?;

        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT through_lamport FROM outgoing_receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![&contact.user_id, &acked_sender_user_id, receipt_type as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let desired = through_lamport.max(current.unwrap_or(0) as u64);
        let existing: Option<OutgoingReceiptEnvelope> = tx
            .query_row(
                "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, receipt_type,
                        through_lamport, timestamp, hop_ttl, expiry, recipient_hint, sealed
                 FROM outgoing_receipt_envelopes
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![&contact.user_id, &acked_sender_user_id, receipt_type as i64],
                row_to_outgoing_receipt,
            )
            .optional()
            .map_err(store_err)?;
        if let Some(envelope) = existing.filter(|item| item.through_lamport >= desired) {
            return Ok(authored_receipt(envelope));
        }

        tx.execute(
            "INSERT INTO outgoing_receipts (chat_id, sender_user_id, receipt_type, through_lamport)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                through_lamport = MAX(through_lamport, excluded.through_lamport)",
            params![
                &contact.user_id,
                &acked_sender_user_id,
                receipt_type as i64,
                desired as i64
            ],
        )
        .map_err(store_err)?;

        let content = encode_receipt_content(ReceiptContent {
            chat_id: identity.user_id.clone(),
            sender_user_id: acked_sender_user_id.clone(),
            lamport: desired,
            receipt_type,
            group_id: None,
        })?;
        let body = encode_message_body(MessageBody {
            kind: KIND_RECEIPT,
            chat_id: identity.user_id.clone(),
            lamport: 0,
            timestamp: timestamp_ms,
            content,
        })?;
        let envelope = OutgoingReceiptEnvelope {
            msg_id: generate_msg_id(),
            recipient_user_id: contact.user_id.clone(),
            chat_id: contact.user_id.clone(),
            sender_user_id: acked_sender_user_id,
            receipt_type,
            through_lamport: desired,
            timestamp: timestamp_ms,
            hop_ttl: DEFAULT_HOP_TTL,
            expiry: default_expiry(timestamp_ms),
            recipient_hint: compute_recipient_hint(contact.user_id.clone(), timestamp_ms),
            sealed: seal_message(identity, contact.agree_pk, body)?,
        };
        tx.execute(
            "INSERT INTO outgoing_receipt_envelopes
                (chat_id, sender_user_id, receipt_type, through_lamport, msg_id,
                 recipient_user_id, timestamp, hop_ttl, expiry, recipient_hint, sealed, queued_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                through_lamport = excluded.through_lamport, msg_id = excluded.msg_id,
                recipient_user_id = excluded.recipient_user_id, timestamp = excluded.timestamp,
                hop_ttl = excluded.hop_ttl, expiry = excluded.expiry,
                recipient_hint = excluded.recipient_hint, sealed = excluded.sealed,
                queued_at = excluded.queued_at, relay_posted_at = NULL",
            params![
                &envelope.chat_id,
                &envelope.sender_user_id,
                receipt_type as i64,
                desired as i64,
                &envelope.msg_id,
                &envelope.recipient_user_id,
                timestamp_ms,
                envelope.hop_ttl as i64,
                envelope.expiry,
                &envelope.recipient_hint,
                &envelope.sealed,
                timestamp_ms
            ],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(authored_receipt(envelope))
    }

    /// Seal a pairwise group receipt to `author`: "I have delivered/read
    /// your messages in `group_id` through `through_lamport`." The envelope
    /// is stored under `chat_id = group_id` so it does not collide with the
    /// 1:1 receipt we may also owe this contact. The sealed body carries
    /// `group_id` in the optional D9 tail; 1:1 receipt bytes are untouched.
    pub fn ensure_authored_group_receipt(
        &self,
        identity: Identity,
        author: Contact,
        group_id: Vec<u8>,
        receipt_type: u8,
        through_lamport: u64,
        timestamp_ms: i64,
    ) -> Result<AuthoredReceipt, CoreError> {
        if receipt_type != RECEIPT_TYPE_DELIVERED && receipt_type != RECEIPT_TYPE_READ {
            return Err(CoreError::Malformed("invalid receipt type".to_string()));
        }
        if through_lamport == 0 {
            return Err(CoreError::Malformed(
                "receipt watermark must be positive".to_string(),
            ));
        }
        // §9.4.
        self.guard_link_gate(CoreLinkGatedAction::Author)?;
        if group_id.len() != crate::GROUP_ID_LEN {
            return Err(CoreError::Malformed(format!(
                "group receipt id must be exactly {} bytes",
                crate::GROUP_ID_LEN
            )));
        }
        validate_sqlite_lamport("receipt watermark", through_lamport)?;

        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let current: Option<i64> = tx
            .query_row(
                "SELECT through_lamport FROM outgoing_receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![&group_id, &author.user_id, receipt_type as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let desired = through_lamport.max(current.unwrap_or(0) as u64);
        let existing: Option<OutgoingReceiptEnvelope> = tx
            .query_row(
                "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, receipt_type,
                        through_lamport, timestamp, hop_ttl, expiry, recipient_hint, sealed
                 FROM outgoing_receipt_envelopes
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![&group_id, &author.user_id, receipt_type as i64],
                row_to_outgoing_receipt,
            )
            .optional()
            .map_err(store_err)?;
        if let Some(envelope) = existing.filter(|item| item.through_lamport >= desired) {
            return Ok(authored_receipt(envelope));
        }

        tx.execute(
            "INSERT INTO outgoing_receipts (chat_id, sender_user_id, receipt_type, through_lamport)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                through_lamport = MAX(through_lamport, excluded.through_lamport)",
            params![
                &group_id,
                &author.user_id,
                receipt_type as i64,
                desired as i64
            ],
        )
        .map_err(store_err)?;

        let content = encode_receipt_content(ReceiptContent {
            chat_id: identity.user_id.clone(),
            sender_user_id: author.user_id.clone(),
            lamport: desired,
            receipt_type,
            group_id: Some(group_id.clone()),
        })?;
        let body = encode_message_body(MessageBody {
            kind: KIND_RECEIPT,
            chat_id: identity.user_id.clone(),
            lamport: 0,
            timestamp: timestamp_ms,
            content,
        })?;
        let envelope = OutgoingReceiptEnvelope {
            msg_id: generate_msg_id(),
            recipient_user_id: author.user_id.clone(),
            chat_id: group_id.clone(),
            sender_user_id: author.user_id.clone(),
            receipt_type,
            through_lamport: desired,
            timestamp: timestamp_ms,
            hop_ttl: DEFAULT_HOP_TTL,
            expiry: default_expiry(timestamp_ms),
            recipient_hint: compute_recipient_hint(author.user_id.clone(), timestamp_ms),
            sealed: seal_message(identity, author.agree_pk, body)?,
        };
        tx.execute(
            "INSERT INTO outgoing_receipt_envelopes
                (chat_id, sender_user_id, receipt_type, through_lamport, msg_id,
                 recipient_user_id, timestamp, hop_ttl, expiry, recipient_hint, sealed, queued_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                through_lamport = excluded.through_lamport, msg_id = excluded.msg_id,
                recipient_user_id = excluded.recipient_user_id, timestamp = excluded.timestamp,
                hop_ttl = excluded.hop_ttl, expiry = excluded.expiry,
                recipient_hint = excluded.recipient_hint, sealed = excluded.sealed,
                queued_at = excluded.queued_at, relay_posted_at = NULL",
            params![
                &envelope.chat_id,
                &envelope.sender_user_id,
                receipt_type as i64,
                desired as i64,
                &envelope.msg_id,
                &envelope.recipient_user_id,
                timestamp_ms,
                envelope.hop_ttl as i64,
                envelope.expiry,
                &envelope.recipient_hint,
                &envelope.sealed,
                timestamp_ms
            ],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(authored_receipt(envelope))
    }
}

fn authored_receipt(envelope: OutgoingReceiptEnvelope) -> AuthoredReceipt {
    let frame = encode_envelope_frame(
        envelope.msg_id.clone(),
        envelope.hop_ttl,
        envelope.expiry,
        envelope.recipient_hint.clone(),
        envelope.sealed.clone(),
    );
    AuthoredReceipt { envelope, frame }
}

fn next_authored_lamport(
    tx: &Transaction<'_>,
    chat_id: &[u8],
    sender_user_id: &[u8],
) -> Result<(u64, u64), CoreError> {
    let own: i64 = tx.query_row(
        "SELECT COALESCE(MAX(lamport), 0) FROM messages WHERE chat_id = ?1 AND sender_user_id = ?2",
        params![chat_id, sender_user_id], |row| row.get(0),
    ).map_err(store_err)?;
    let receipt = |receipt_type: u8| -> Result<i64, CoreError> {
        tx.query_row(
            "SELECT COALESCE(MAX(through_lamport), 0) FROM receipts
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
            params![chat_id, sender_user_id, receipt_type as i64],
            |row| row.get(0),
        )
        .map_err(store_err)
    };
    let delivered = receipt(RECEIPT_TYPE_DELIVERED)?;
    let read = receipt(RECEIPT_TYPE_READ)?;
    // The persisted high-water mark is the only one of these four that
    // survives `delete_contact`. Without it the counter restarts at 1 against
    // a peer who still holds our old stream: they read the reused lamports as
    // us having forked, and their fork recovery deletes their copy of the
    // conversation to resynchronise. A one-sided delete would silently become
    // a two-sided one.
    let authored_high: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(high_lamport), 0) FROM authored_lamport_watermarks
             WHERE chat_id = ?1 AND sender_user_id = ?2",
            params![chat_id, sender_user_id],
            |row| row.get(0),
        )
        .map_err(store_err)?;
    let next = own
        .max(delivered)
        .max(read)
        .max(authored_high)
        .saturating_add(1) as u64;
    Ok((next, delivered as u64))
}

/// `routing_timestamp_ms` is the TRUE clock, kept separate from
/// `message.timestamp` (which may have been floored for display order, see
/// [`crate::causal_order`]). Expiry and the recipient hint are keyed to real
/// elapsed time -- the hint's match window only looks backwards, so pushing
/// it into tomorrow's day bucket to fix a display artifact would trade a
/// cosmetic bug for an undeliverable message.
/// The display timestamp a message authored into `chat_id` right now should
/// carry: `now_ms`, floored so it cannot sort above anything already in the
/// chat. See [`crate::causal_order`] for why.
fn causal_timestamp_for_chat(
    tx: &Transaction<'_>,
    chat_id: &[u8],
    now_ms: i64,
) -> Result<i64, CoreError> {
    let newest: Option<i64> = tx
        .query_row(
            "SELECT MAX(timestamp) FROM messages WHERE chat_id = ?1",
            params![chat_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_err)?
        .flatten();
    Ok(causal_display_timestamp(newest, now_ms))
}

/// Seal one pairwise envelope. `msg_id` and `expiry` are supplied rather than
/// derived here because the two callers legitimately differ on both: authoring
/// mints a fresh id and applies the per-kind [`authored_expiry`], while the
/// repair re-seal in `backfill_pairwise_envelope` reuses the message's own
/// persisted id and the flat [`default_expiry`]. Making them parameters keeps
/// that difference visible at the call site instead of hidden behind a flag.
fn build_pairwise_envelope(
    identity: Identity,
    contact: &Contact,
    message: &StoredMessage,
    reply_to_msg_id: Option<&[u8]>,
    routing_timestamp_ms: i64,
    msg_id: Vec<u8>,
    expiry: i64,
) -> Result<OutboundEnvelope, CoreError> {
    let body = encoded_body(message, identity.user_id.clone(), reply_to_msg_id)?;
    Ok(OutboundEnvelope {
        msg_id,
        recipient_user_id: contact.user_id.clone(),
        chat_id: message.chat_id.clone(),
        sender_user_id: message.sender_user_id.clone(),
        kind: message.kind,
        lamport: message.lamport,
        timestamp: message.timestamp,
        hop_ttl: DEFAULT_HOP_TTL,
        expiry,
        recipient_hint: compute_recipient_hint(contact.user_id.clone(), routing_timestamp_ms),
        sealed: seal_message(identity, contact.agree_pk.clone(), body)?,
    })
}

/// The durable wire identity of a stored message, for a re-seal.
///
/// `messages.msg_id` is written by [`insert_authored_rows`] at authoring time
/// and is the id the recipient saw the first time, so reusing it makes a
/// re-seal a *retransmission* rather than a new piece of traffic: the peer's
/// seen-set and both shells' `msg_id`-keyed once-per-session hidden-offer
/// bound recognise it. Minting a fresh random id on every digest — what this
/// path did before #283 made re-seals routine — defeats all three.
///
/// A row authored before the column existed has no id. It gets one, written
/// back in the same transaction so the identity is stable from then on.
///
/// §5: both statements are scoped to the row's own authoring stream, named
/// explicitly the way `outgoing_message_reference::insert` names it. Leaving
/// the scope off would let a sibling's row at the same lamport answer the
/// SELECT, and would let the UPDATE stamp this device's fresh `msg_id` onto a
/// message this device never authored — a real hazard now that a sibling's
/// authored rows arrive here through §8's History stream and sit in the same
/// table under their own device id.
///
/// The stream comes from the *message*, not from this device: a repair re-seal
/// is asked to re-send a stored row, and that row may predate linking (the
/// legacy stream) even on a device that has since linked.
fn stable_backfill_msg_id(
    tx: &Transaction<'_>,
    message: &StoredMessage,
) -> Result<Vec<u8>, CoreError> {
    let sender_device_id = crate::core_device_stream_id(Some(message.sender_device_id.clone()));
    let stored: Option<Vec<u8>> = tx
        .query_row(
            "SELECT msg_id FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3
               AND sender_device_id = ?4",
            params![
                message.chat_id,
                message.sender_user_id,
                message.lamport as i64,
                sender_device_id
            ],
            |row| row.get::<_, Option<Vec<u8>>>(0),
        )
        .optional()
        .map_err(store_err)?
        .flatten()
        .filter(|id| !id.is_empty());
    if let Some(msg_id) = stored {
        return Ok(msg_id);
    }
    let msg_id = generate_msg_id();
    tx.execute(
        "UPDATE messages SET msg_id = ?4
         WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3
           AND sender_device_id = ?5 AND msg_id IS NULL",
        params![
            message.chat_id,
            message.sender_user_id,
            message.lamport as i64,
            msg_id,
            sender_device_id
        ],
    )
    .map_err(store_err)?;
    Ok(msg_id)
}

/// The sealed body for one authored row.
///
/// §5: the authoring device rides the body's `0x20` extension whenever this
/// install has one, so a message authored after linking carries the stream it
/// actually belongs to instead of being filed on the person's legacy stream by
/// every receiver — including this person's own siblings, which is what SYNC-2's
/// dedup reads. An unlinked device emits neither the extension nor any other
/// difference (see [`wire_device_id`]).
fn encoded_body(
    message: &StoredMessage,
    wire_chat_id: Vec<u8>,
    reply_to_msg_id: Option<&[u8]>,
) -> Result<Vec<u8>, CoreError> {
    let body = MessageBody {
        kind: message.kind,
        chat_id: wire_chat_id,
        lamport: message.lamport,
        timestamp: message.timestamp,
        content: message.payload.clone(),
    };
    match (
        reply_to_msg_id,
        wire_device_id(&message.sender_device_id).as_ref(),
    ) {
        // The legacy shapes, byte-for-byte, so an unlinked device's envelopes
        // are not merely equivalent to today's but identical to them.
        (Some(id), None) => encode_message_body_with_reply(body, id.to_vec()),
        (None, None) => encode_message_body(body),
        (reply, device) => encode_message_body_extended(
            body,
            reply.map(<[u8]>::to_vec),
            device.cloned(),
            // §12's roster head is a WP5 concern: nothing yet fetches a roster
            // on the strength of it, and emitting a reference a receiver cannot
            // act on would grow every envelope for nothing.
            None,
        ),
    }
}

/// Record how far our counter has got, in the caller's transaction, so it
/// holds even for kinds that are never stored as chat rows. `MAX` rather than
/// plain assignment: the watermark only ever climbs, so an out-of-order or
/// replayed author cannot walk it backwards.
fn record_authored_watermark(
    tx: &Transaction<'_>,
    message: &StoredMessage,
) -> Result<(), CoreError> {
    tx.execute(
        "INSERT INTO authored_lamport_watermarks (chat_id, sender_user_id, high_lamport)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(chat_id, sender_user_id) DO UPDATE SET
             high_lamport = MAX(high_lamport, excluded.high_lamport)",
        params![
            message.chat_id,
            message.sender_user_id,
            message.lamport as i64
        ],
    )
    .map_err(store_err)?;
    Ok(())
}

/// Put one sealed envelope in the outbound queue. Split out from
/// [`insert_authored_rows`] because the repair re-seal path queues
/// conditionally — see `MessageStore::backfill_pairwise_envelope`.
fn queue_outbound_row(
    tx: &Transaction<'_>,
    envelope: &OutboundEnvelope,
    queued_at_ms: i64,
) -> Result<(), CoreError> {
    tx.execute(
        "INSERT OR IGNORE INTO outbound_envelopes
            (dedupe_key, msg_id, recipient_user_id, chat_id, sender_user_id, kind,
             lamport, timestamp, hop_ttl, expiry, recipient_hint, sealed, queued_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            outbound_message_dedupe_key(
                &envelope.chat_id,
                &envelope.sender_user_id,
                envelope.kind,
                envelope.lamport,
                &envelope.recipient_user_id
            ),
            envelope.msg_id,
            envelope.recipient_user_id,
            envelope.chat_id,
            envelope.sender_user_id,
            envelope.kind as i64,
            envelope.lamport as i64,
            envelope.timestamp,
            envelope.hop_ttl as i64,
            envelope.expiry,
            envelope.recipient_hint,
            envelope.sealed,
            queued_at_ms
        ],
    )
    .map_err(store_err)?;
    Ok(())
}

fn insert_authored_rows(
    tx: &Transaction<'_>,
    message: &StoredMessage,
    envelope: &OutboundEnvelope,
    reply_to_msg_id: Option<&[u8]>,
    queued_at_ms: i64,
) -> Result<(), CoreError> {
    record_authored_watermark(tx, message)?;
    // §5: the device column is named rather than left to its legacy default.
    // On an unlinked install the value IS the default, so nothing about the row
    // changes; on a linked one this is what puts the person's own outbound on
    // the stream a sibling can tell apart from its own.
    tx.execute(
        "INSERT OR IGNORE INTO messages
            (chat_id, sender_user_id, sender_device_id, lamport, timestamp, kind, payload,
             msg_id, reply_to_msg_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            message.chat_id,
            message.sender_user_id,
            message.sender_device_id,
            message.lamport as i64,
            message.timestamp,
            message.kind as i64,
            message.payload,
            envelope.msg_id,
            reply_to_msg_id
        ],
    )
    .map_err(store_err)?;
    queue_outbound_row(tx, envelope, queued_at_ms)?;
    // SYNC-2: what the person was composing has now been said. Clearing the
    // shared draft in the *authoring* transaction is what makes "send from
    // whichever device is in hand edits the draft, not the stream" true on the
    // other device: the clear converges through the Settings stream and the
    // sibling's composer empties instead of holding a message already on the
    // wire. Only the kinds a person actually composes — a reaction or a group
    // invite was never a draft.
    if matches!(message.kind, KIND_TEXT | KIND_ATTACHMENT_MANIFEST) {
        clear_chat_draft(tx, &message.chat_id, queued_at_ms)?;
    }
    // Supersession (#283, contract QUEUE-01), in the same transaction that
    // queued the replacement so the queue never briefly holds both. For a
    // snapshot kind only the newest generation can inform the recipient of
    // anything: the field store had queued 120 generations of the friend
    // directory to a single contact, each one a full copy of a snapshot the
    // recipient's own revision guard would discard on arrival. See
    // `crate::outbound_retirement::supersedes_queued_generations` for the
    // per-kind justification and for why request-shaped hidden kinds are not
    // in the set.
    let superseded = retire_superseded(
        tx,
        &envelope.recipient_user_id,
        &envelope.chat_id,
        &envelope.sender_user_id,
        envelope.kind,
        envelope.lamport,
    )?;
    if superseded > 0 {
        crate::protocol_event::note_for(tx, "peer", &envelope.recipient_user_id, |peer| {
            vec![crate::protocol_event::ProtocolEventDraft::new(
                crate::protocol_event::ProtocolEventCode::OutboundRowSuperseded,
                queued_at_ms,
                "a_newer_generation_replaced_them",
            )
            .actor(peer)
            .invariants(&["QUEUE-01"])
            .count(
                "rows_superseded",
                i64::try_from(superseded).unwrap_or(i64::MAX),
            )
            .count("kind", envelope.kind as i64)]
        });
    }
    Ok(())
}

fn validate_sqlite_lamport(field: &str, value: u64) -> Result<(), CoreError> {
    if value > i64::MAX as u64 {
        return Err(CoreError::Malformed(format!(
            "{field} exceeds the supported range"
        )));
    }
    Ok(())
}

fn authored(
    message: StoredMessage,
    envelope: OutboundEnvelope,
    acknowledged_delivered: u64,
) -> AuthoredEnvelope {
    let frame = encode_envelope_frame(
        envelope.msg_id.clone(),
        envelope.hop_ttl,
        envelope.expiry,
        envelope.recipient_hint.clone(),
        envelope.sealed.clone(),
    );
    AuthoredEnvelope {
        message,
        envelope,
        frame,
        acknowledged_delivered,
    }
}

fn is_pairwise_kind(kind: u8) -> bool {
    matches!(
        kind,
        KIND_TEXT
            | KIND_FRIEND_REQUEST
            | KIND_GROUP_INVITE
            | KIND_PROFILE_SYNC
            | KIND_FRIEND_DIRECTORY
            | KIND_INTRODUCED_FRIEND_REQUEST
            | KIND_LAN_ENDPOINT_HINT
            | KIND_RELAY_UPDATE
            | KIND_ROSTER_GOSSIP
            | KIND_ATTACHMENT_MANIFEST
            | KIND_REACTION
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        create_group, decode_extended_message_body, decode_group_metadata_update,
        decode_message_body, decode_receipt_content, encode_attachment_payload, generate_identity,
        open_group_message, open_message, AttachmentMediaType, CoreAttachmentPayload,
    };

    fn contact(identity: &Identity, name: &str) -> Contact {
        Contact {
            user_id: identity.user_id.clone(),
            name: name.into(),
            sign_pk: identity.sign_pk.clone(),
            agree_pk: identity.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    /// The reported field symptom, end to end: their clock runs ahead, we
    /// reply seconds later, and the reply must still render below the message
    /// it answers.
    #[test]
    fn a_reply_to_a_fast_clocked_peer_still_renders_below_the_question() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        let alice = generate_identity();
        let bob = generate_identity();
        let bob_contact = contact(&bob, "Bob");

        // Bob's phone is five minutes fast; his question lands stamped in
        // our future.
        let our_now = 1_700_000_000_000i64;
        let his_clock = our_now + 5 * 60 * 1_000;
        store
            .insert_message(StoredMessage {
                chat_id: bob_contact.user_id.clone(),
                sender_user_id: bob.user_id.clone(),
                lamport: 1,
                timestamp: his_clock,
                kind: KIND_TEXT,
                payload: b"are you there?".to_vec(),
                sender_device_id: LEGACY_DEVICE_ID.to_vec(),
            })
            .unwrap();

        let authored = store
            .author_pairwise_message(
                alice.clone(),
                bob_contact.clone(),
                KIND_TEXT,
                b"yes!".to_vec(),
                None,
                our_now + 10_000,
            )
            .unwrap();

        assert!(
            authored.message.timestamp > his_clock,
            "reply stamped {} must sort after the question at {his_clock}",
            authored.message.timestamp
        );
        let rendered = store
            .messages_for_chat(bob_contact.user_id.clone())
            .unwrap();
        assert_eq!(rendered.len(), 2);
        assert_eq!(rendered[0].payload, b"are you there?".to_vec());
        assert_eq!(rendered[1].payload, b"yes!".to_vec());

        // The peer sees the same order, because the floored timestamp is what
        // rides the wire -- fixing only our own view would leave the question
        // and answer inverted on their phone instead.
        assert_eq!(authored.envelope.timestamp, authored.message.timestamp);

        // Routing time stays on the true clock: the recipient hint is day
        // bucketed and only matched backwards, so it must not be dragged
        // forward by a peer's fast clock.
        assert_eq!(
            authored.envelope.recipient_hint,
            compute_recipient_hint(bob_contact.user_id.clone(), our_now + 10_000)
        );
        assert_eq!(authored.envelope.expiry, default_expiry(our_now + 10_000));
    }

    #[test]
    fn agreeing_clocks_leave_the_authored_timestamp_untouched() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        let alice = generate_identity();
        let bob = generate_identity();
        let bob_contact = contact(&bob, "Bob");
        let now = 1_700_000_000_000i64;
        store
            .insert_message(StoredMessage {
                chat_id: bob_contact.user_id.clone(),
                sender_user_id: bob.user_id.clone(),
                lamport: 1,
                timestamp: now - 60_000,
                kind: KIND_TEXT,
                payload: b"hi".to_vec(),
                sender_device_id: LEGACY_DEVICE_ID.to_vec(),
            })
            .unwrap();
        let authored = store
            .author_pairwise_message(alice, bob_contact, KIND_TEXT, b"hello".to_vec(), None, now)
            .unwrap();
        assert_eq!(authored.message.timestamp, now);
    }

    #[test]
    fn pairwise_authoring_ratchets_past_receipts_and_persists_atomically() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        let alice = generate_identity();
        let bob = generate_identity();
        store
            .record_receipt(
                bob.user_id.clone(),
                alice.user_id.clone(),
                RECEIPT_TYPE_READ,
                9,
                None,
                None,
            )
            .unwrap();
        let result = store
            .author_pairwise_message(
                alice.clone(),
                contact(&bob, "Bob"),
                KIND_TEXT,
                b"hello".to_vec(),
                None,
                1_000,
            )
            .unwrap();
        assert_eq!(result.message.lamport, 10);
        assert_eq!(
            store.messages_for_chat(bob.user_id.clone()).unwrap(),
            vec![result.message.clone()]
        );
        assert_eq!(
            store
                .outbound_envelopes_after(bob.user_id.clone(), alice.user_id.clone(), 0)
                .unwrap(),
            vec![result.envelope.clone()]
        );
        let opened = open_message(bob, result.envelope.sealed).unwrap();
        assert_eq!(opened.sender_user_id, alice.user_id);
        assert_eq!(
            decode_message_body(opened.payload).unwrap().content,
            b"hello"
        );
    }

    #[test]
    fn repeated_authors_receive_unique_lamports() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        let alice = generate_identity();
        let bob = generate_identity();
        let first = store
            .author_pairwise_message(
                alice.clone(),
                contact(&bob, "Bob"),
                KIND_TEXT,
                vec![1],
                None,
                1,
            )
            .unwrap();
        let second = store
            .author_pairwise_message(alice, contact(&bob, "Bob"), KIND_TEXT, vec![2], None, 2)
            .unwrap();
        assert_eq!((first.message.lamport, second.message.lamport), (1, 2));
    }

    #[test]
    fn group_attachment_authoring_is_durable_and_openable() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        let alice = generate_identity();
        let bob = generate_identity();
        let group = create_group(
            "Family".to_string(),
            vec![alice.user_id.clone(), bob.user_id.clone()],
        )
        .unwrap();
        store.upsert_group(group.clone()).unwrap();

        let attachment = encode_attachment_payload(CoreAttachmentPayload {
            media_type: AttachmentMediaType::Image,
            mime_type: "image/jpeg".into(),
            duration_ms: 0,
            blob: vec![1, 2, 3],
            caption: String::new(),
        })
        .unwrap();
        let result = store
            .author_group_message(
                alice.clone(),
                group.clone(),
                KIND_ATTACHMENT_MANIFEST,
                attachment.clone(),
                None,
                77,
            )
            .unwrap();
        assert_eq!(result.message.kind, KIND_ATTACHMENT_MANIFEST);
        assert_eq!(
            store.messages_for_chat(group.id.clone()).unwrap(),
            vec![result.message.clone()]
        );
        let opened = open_group_message(group, result.envelope.sealed).unwrap();
        let body = decode_extended_message_body(opened.payload).unwrap();
        assert_eq!(body.kind, KIND_ATTACHMENT_MANIFEST);
        assert_eq!(body.content, attachment);
    }

    #[test]
    fn group_metadata_authoring_updates_state_and_queue_atomically() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        let alice = generate_identity();
        let bob = generate_identity();
        let carol = generate_identity();
        let group = create_group(
            "Family".to_string(),
            vec![alice.user_id.clone(), bob.user_id.clone()],
        )
        .unwrap();
        store.upsert_group(group.clone()).unwrap();

        let result = store
            .author_group_metadata_update(
                alice.clone(),
                group.clone(),
                "Cabin Crew".to_string(),
                vec![
                    alice.user_id.clone(),
                    bob.user_id.clone(),
                    carol.user_id.clone(),
                ],
                88,
            )
            .unwrap();
        assert_eq!(result.group.name, "Cabin Crew");
        assert!(result.group.member_user_ids.contains(&carol.user_id));
        assert_eq!(
            store.get_group(group.id.clone()).unwrap(),
            Some(result.group.clone())
        );
        assert_eq!(result.authored.message.kind, KIND_GROUP_METADATA_UPDATE);

        let opened = open_group_message(group, result.authored.envelope.sealed).unwrap();
        let body = decode_extended_message_body(opened.payload).unwrap();
        assert_eq!(body.kind, KIND_GROUP_METADATA_UPDATE);
        assert_eq!(
            decode_group_metadata_update(body.content).unwrap(),
            result.update
        );
    }

    #[test]
    fn ensured_receipt_is_atomic_monotonic_and_reuses_stable_frame() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        let alice = generate_identity();
        let bob = generate_identity();
        let first = store
            .ensure_authored_receipt(
                alice.clone(),
                contact(&bob, "Bob"),
                bob.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                4,
                10,
            )
            .unwrap();
        let replay = store
            .ensure_authored_receipt(
                alice.clone(),
                contact(&bob, "Bob"),
                bob.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                3,
                11,
            )
            .unwrap();
        assert_eq!(replay, first);

        let advanced = store
            .ensure_authored_receipt(
                alice,
                contact(&bob, "Bob"),
                bob.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                7,
                12,
            )
            .unwrap();
        assert_eq!(advanced.envelope.through_lamport, 7);
        assert_ne!(advanced.envelope.msg_id, first.envelope.msg_id);
        assert_eq!(
            store
                .outgoing_receipt_through(bob.user_id.clone(), bob.user_id, RECEIPT_TYPE_DELIVERED,)
                .unwrap(),
            7
        );
    }

    #[test]
    fn group_receipt_seals_pairwise_and_does_not_change_one_to_one_bytes() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        let alice = generate_identity();
        let bob = generate_identity();
        let family = create_group(
            "Family".to_string(),
            vec![alice.user_id.clone(), bob.user_id.clone()],
        )
        .unwrap();
        store.upsert_group(family.clone()).unwrap();

        let one_to_one = store
            .ensure_authored_receipt(
                alice.clone(),
                contact(&bob, "Bob"),
                bob.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                2,
                20,
            )
            .unwrap();
        let pairwise = decode_receipt_content(
            decode_message_body(
                open_message(bob.clone(), one_to_one.envelope.sealed.clone())
                    .unwrap()
                    .payload,
            )
            .unwrap()
            .content,
        )
        .unwrap();
        assert_eq!(pairwise.group_id, None);

        let group = store
            .ensure_authored_group_receipt(
                alice,
                contact(&bob, "Bob"),
                family.id.clone(),
                RECEIPT_TYPE_DELIVERED,
                2,
                21,
            )
            .unwrap();
        assert_eq!(group.envelope.chat_id, family.id);
        assert_eq!(group.envelope.recipient_user_id, bob.user_id);
        let opened = open_message(bob.clone(), group.envelope.sealed).unwrap();
        let body = decode_message_body(opened.payload).unwrap();
        let receipt = decode_receipt_content(body.content).unwrap();
        assert_eq!(receipt.group_id.as_deref(), Some(family.id.as_slice()));
        assert_eq!(receipt.sender_user_id, bob.user_id);
        assert_eq!(receipt.lamport, 2);
        assert_ne!(group.envelope.msg_id, one_to_one.envelope.msg_id);
    }
}
