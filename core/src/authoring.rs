use rusqlite::{params, OptionalExtension, Transaction};

use crate::store::{
    outbound_message_dedupe_key, row_to_outbound, row_to_outgoing_receipt, store_err,
    upsert_group_tx,
};
use crate::{
    apply_group_metadata_update, compute_recipient_hint, create_group_metadata_update,
    default_expiry, encode_envelope_frame, encode_group_invite_content,
    encode_group_metadata_update, encode_message_body, encode_message_body_with_reply,
    encode_receipt_content, generate_msg_id, seal_group_message, seal_message, Contact, CoreError,
    Group, GroupMetadataUpdate, Identity, MessageBody, MessageStore, OutboundEnvelope,
    OutgoingReceiptEnvelope, ReceiptContent, StoredMessage, DEFAULT_HOP_TTL,
    KIND_ATTACHMENT_MANIFEST, KIND_FRIEND_DIRECTORY, KIND_FRIEND_REQUEST, KIND_GROUP_INVITE,
    KIND_GROUP_METADATA_UPDATE, KIND_INTRODUCED_FRIEND_REQUEST, KIND_LAN_ENDPOINT_HINT,
    KIND_PROFILE_SYNC, KIND_REACTION, KIND_RECEIPT, KIND_TEXT, RECEIPT_TYPE_DELIVERED,
    RECEIPT_TYPE_READ,
};

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
        if !is_pairwise_kind(kind) {
            return Err(CoreError::Malformed(format!(
                "unsupported pairwise authored kind {kind}"
            )));
        }
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let (lamport, acknowledged_delivered) =
            next_authored_lamport(&tx, &contact.user_id, &identity.user_id)?;
        let message = StoredMessage {
            chat_id: contact.user_id.clone(),
            sender_user_id: identity.user_id.clone(),
            lamport,
            timestamp: timestamp_ms,
            kind,
            payload,
        };
        let envelope =
            build_pairwise_envelope(identity, &contact, &message, reply_to_msg_id.as_deref())?;
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

    /// Seal and persist an envelope for a legacy authored message that was
    /// stored before the outbound queue existed. Repeated calls return the
    /// already-persisted envelope instead of generating a new msg id.
    pub fn backfill_pairwise_envelope(
        &self,
        identity: Identity,
        contact: Contact,
        message: StoredMessage,
        reply_to_msg_id: Option<Vec<u8>>,
    ) -> Result<AuthoredEnvelope, CoreError> {
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
        let envelope =
            build_pairwise_envelope(identity, &contact, &message, reply_to_msg_id.as_deref())?;
        insert_authored_rows(
            &tx,
            &message,
            &envelope,
            reply_to_msg_id.as_deref(),
            message.timestamp,
        )?;
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
        if kind != KIND_TEXT && kind != KIND_ATTACHMENT_MANIFEST && kind != KIND_REACTION {
            return Err(CoreError::Malformed(format!(
                "unsupported group authored kind {kind}"
            )));
        }
        if !group
            .member_user_ids
            .iter()
            .any(|member| *member == identity.user_id)
        {
            return Err(CoreError::Malformed(
                "group author is not a member".to_string(),
            ));
        }
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let (lamport, acknowledged_delivered) =
            next_authored_lamport(&tx, &group.id, &identity.user_id)?;
        let message = StoredMessage {
            chat_id: group.id.clone(),
            sender_user_id: identity.user_id.clone(),
            lamport,
            timestamp: timestamp_ms,
            kind,
            payload,
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
            timestamp: timestamp_ms,
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
        let message = StoredMessage {
            chat_id: group.id.clone(),
            sender_user_id: identity.user_id.clone(),
            lamport,
            timestamp: timestamp_ms,
            kind: KIND_GROUP_METADATA_UPDATE,
            payload,
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
            timestamp: timestamp_ms,
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
        let invite = encode_group_invite_content(group.clone())?;
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let (lamport, acknowledged_delivered) =
            next_authored_lamport(&tx, &group.id, &identity.user_id)?;
        let message = StoredMessage {
            chat_id: group.id,
            sender_user_id: identity.user_id.clone(),
            lamport,
            timestamp: timestamp_ms,
            kind: KIND_GROUP_INVITE,
            payload: invite,
        };
        let mut authored_invites = Vec::new();
        for member in members {
            if member.user_id == identity.user_id {
                continue;
            }
            let envelope = build_pairwise_envelope(identity.clone(), &member, &message, None)?;
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
    let next = own.max(delivered).max(read).saturating_add(1) as u64;
    Ok((next, delivered as u64))
}

fn build_pairwise_envelope(
    identity: Identity,
    contact: &Contact,
    message: &StoredMessage,
    reply_to_msg_id: Option<&[u8]>,
) -> Result<OutboundEnvelope, CoreError> {
    let body = encoded_body(message, identity.user_id.clone(), reply_to_msg_id)?;
    Ok(OutboundEnvelope {
        msg_id: generate_msg_id(),
        recipient_user_id: contact.user_id.clone(),
        chat_id: message.chat_id.clone(),
        sender_user_id: message.sender_user_id.clone(),
        kind: message.kind,
        lamport: message.lamport,
        timestamp: message.timestamp,
        hop_ttl: DEFAULT_HOP_TTL,
        expiry: default_expiry(message.timestamp),
        recipient_hint: compute_recipient_hint(contact.user_id.clone(), message.timestamp),
        sealed: seal_message(identity, contact.agree_pk.clone(), body)?,
    })
}

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
    match reply_to_msg_id {
        Some(id) => encode_message_body_with_reply(body, id.to_vec()),
        None => encode_message_body(body),
    }
}

fn insert_authored_rows(
    tx: &Transaction<'_>,
    message: &StoredMessage,
    envelope: &OutboundEnvelope,
    reply_to_msg_id: Option<&[u8]>,
    queued_at_ms: i64,
) -> Result<(), CoreError> {
    tx.execute(
        "INSERT OR IGNORE INTO messages
            (chat_id, sender_user_id, lamport, timestamp, kind, payload, msg_id, reply_to_msg_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            message.chat_id,
            message.sender_user_id,
            message.lamport as i64,
            message.timestamp,
            message.kind as i64,
            message.payload,
            envelope.msg_id,
            reply_to_msg_id
        ],
    )
    .map_err(store_err)?;
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
            | KIND_ATTACHMENT_MANIFEST
            | KIND_REACTION
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        create_group, decode_extended_message_body, decode_group_metadata_update,
        decode_message_body, encode_attachment_payload, generate_identity, open_group_message,
        open_message, AttachmentMediaType, CoreAttachmentPayload,
    };

    fn contact(identity: &Identity, name: &str) -> Contact {
        Contact {
            user_id: identity.user_id.clone(),
            name: name.into(),
            sign_pk: identity.sign_pk.clone(),
            agree_pk: identity.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
        }
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
}
