//! Message + contact store: SQLite-backed persistence (DESIGN.md §7.1,
//! §10). `insert_message` is idempotent on (chat_id, sender_user_id,
//! lamport), so re-delivering the same envelope (expected under DTN) is
//! safe. Per-chat lamport counters are maintained independently by each
//! sender (DESIGN.md §7.1), so gap detection in [MessageStore::highest_contiguous_lamport]
//! is keyed on (chat_id, sender_user_id), not chat_id alone.
//!
//! Contacts (DESIGN.md §6.2) live in the same store/connection rather than a
//! separate file: they're the other half of "who can I seal a message to,"
//! which is exactly the data a message store needs alongside messages
//! themselves.
//!
//! ## Receipts (DESIGN.md §7.2)
//!
//! The `receipts` table records what *this device's peer* has acknowledged
//! about messages *this device sent*: "the peer has delivered/read messages
//! authored by `sender_user_id` (this device's own UserID, from the peer's
//! point of view) in `chat_id`, through `through_lamport`." Per §7.2 a
//! receipt is cumulative, and receipts "are tiny, idempotent, and re-sent
//! opportunistically on every peer sync, so a lost receipt heals itself" --
//! which means the same or a stale receipt can arrive more than once, and an
//! older cumulative value can arrive *after* a newer one (DTN reordering).
//! [`MessageStore::record_receipt`] is therefore a monotonic upsert: it only
//! ever raises `through_lamport`, never lowers it.
//!
//! ## Outbound envelope queue (DESIGN.md §4, §9)
//!
//! Locally authored traffic should be sealed once, persisted, and then handed
//! to whatever transports are up. The `outbound_envelopes` table is that
//! transport-agnostic queue for authored chat messages: it stores the exact
//! §6.4 public header plus sealed bytes, keyed by the logical message
//! identity `(chat_id, sender_user_id, kind, lamport)` so reconnect retries
//! reuse the same `msg_id` and ciphertext instead of re-sealing a fresh
//! envelope every time. `insert_outgoing_message` writes the plaintext
//! message row and its queued envelope in one transaction so local history
//! and sync state never diverge on a crash boundary.
//!
//! ## Outgoing receipts (DESIGN.md §7.2, §7.3)
//!
//! The `outgoing_receipts` table is the mirror image of `receipts`: it tracks
//! what *this device* has locally observed and should tell its peer about
//! messages the peer authored: "I have delivered/read your messages in
//! `chat_id` through `through_lamport`." Keeping that cumulative watermark in
//! the store lets a lost standalone receipt heal itself on the next digest
//! sync -- §7.3's "receipts first" rule. Like incoming receipts, outgoing
//! ones are cumulative and monotonic, so stale retries must never lower the
//! stored watermark.
//!
//! ## Outgoing receipt envelope queue (DESIGN.md §7.2, §7.3, §9)
//!
//! Relay upload needs the same "seal once, retry many times" property as
//! authored text, but receipts are not chat-stream messages: they never live
//! in `messages`, carry no lamport sequence of their own, and must never be
//! acked in return. The `outgoing_receipt_envelopes` table is therefore a
//! separate queue keyed by the logical receipt watermark
//! `(chat_id, sender_user_id, receipt_type)`. Each row stores the exact
//! sealed envelope for the *latest* cumulative watermark owed on that key.
//! Re-queueing the same watermark is a no-op that preserves the existing
//! `msg_id`; advancing the watermark replaces the row with a newly sealed
//! envelope and clears its relay-posted marker so the higher cumulative
//! receipt uploads on the next relay sync.
//!
//! ## Sync digests (DESIGN.md §7.3)
//!
//! On peer connect, each side summarizes what it already has per chat so the
//! other side can send just what's missing. §7.3 describes that summary as
//! "(chat id, highest-contiguous lamport, recent msg_id bloom filter)".
//! [`MessageStore::chat_digest`] implements the contiguous-lamport half of
//! that -- one [`DigestEntry`] per sender who has posted in the chat,
//! reusing the same gap-aware [`MessageStore::highest_contiguous_lamport`]
//! logic already needed for per-sender ordering. The msg-id half currently
//! ships as an **exact** list of carried-envelope `msg_id`s
//! ([`MessageStore::carried_msg_ids`]) rather than a bloom filter: family
//! scale keeps that list small enough, exactness avoids false positives, and
//! it is sufficient to unlock spray-on-connect for mule chains. A true bloom
//! filter remains a possible future compression step, especially once
//! out-of-order/non-contiguous delivered message ids also need to participate.
//! [`MessageStore::messages_after`] answers the other half: given what a
//! peer's digest says they already have, which of *our* messages from a
//! given sender are they missing.

use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::sync::Mutex;

use crate::CoreError;

/// One stored message body (DESIGN.md §7.1). `timestamp` is milliseconds
/// since the Unix epoch; `kind` matches the DESIGN.md §7.1 `kind` byte
/// (text=1, receipt=2, friend-request=3, group-invite=4, ...).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct StoredMessage {
    pub chat_id: Vec<u8>,
    pub sender_user_id: Vec<u8>,
    pub lamport: u64,
    pub timestamp: i64,
    pub kind: u8,
    pub payload: Vec<u8>,
}

/// An accepted friend (DESIGN.md §6.2): the public half of someone else's
/// identity, imported from a scanned/pasted `FriendCard`. `user_id` is
/// derived the same way as one's own (`friend_card_user_id`), so it's a
/// stable primary key even though a display name can be edited later.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct Contact {
    pub user_id: Vec<u8>,
    pub name: String,
    pub sign_pk: Vec<u8>,
    pub agree_pk: Vec<u8>,
    pub relay_url: Option<String>,
    pub relay_token: Option<String>,
}

/// One entry of a per-chat sync digest (DESIGN.md §7.3): "I have `sender_user_id`'s
/// messages in this chat contiguously through `through_lamport`."
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct DigestEntry {
    pub sender_user_id: Vec<u8>,
    pub through_lamport: u64,
}

/// A sealed envelope this node is muling for someone else (DESIGN.md §5.3
/// carry queue): a foreign envelope we couldn't open (so it isn't for us) but
/// hold on to, to hand to its recipient when we next meet them. These are the
/// §6.4 public-header fields plus the opaque sealed blob -- everything needed
/// to reconstruct the exact `0x02` frame for onward delivery (see
/// [`crate::encode_envelope_frame`]). The internal eviction bookkeeping
/// (is-family, received-at, size) is deliberately *not* on this record: it's
/// an implementation detail of the store's budget enforcement, not something
/// the transport layer needs when it pulls an envelope back out to send.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CarriedEnvelope {
    pub msg_id: Vec<u8>,
    pub hop_ttl: u8,
    pub expiry: i64,
    pub recipient_hint: Vec<u8>,
    pub sealed: Vec<u8>,
}

/// One locally authored sealed envelope persisted for resend over BLE and
/// relay. This is the exact §6.4 public header plus sealed bytes, alongside
/// the local message metadata needed to query the queue by chat/sender/lamport
/// and to stage relay uploads later.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct OutboundEnvelope {
    pub msg_id: Vec<u8>,
    pub recipient_user_id: Vec<u8>,
    pub chat_id: Vec<u8>,
    pub sender_user_id: Vec<u8>,
    pub kind: u8,
    pub lamport: u64,
    pub timestamp: i64,
    pub hop_ttl: u8,
    pub expiry: i64,
    pub recipient_hint: Vec<u8>,
    pub sealed: Vec<u8>,
}

/// One persisted outgoing receipt envelope for relay upload and retry.
/// Unlike [`OutboundEnvelope`], this queue is keyed by the cumulative receipt
/// watermark rather than the chat lamport stream, so `through_lamport` is the
/// semantic identity that advances while `msg_id` stays stable for a given
/// watermark.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct OutgoingReceiptEnvelope {
    pub msg_id: Vec<u8>,
    pub recipient_user_id: Vec<u8>,
    pub chat_id: Vec<u8>,
    pub sender_user_id: Vec<u8>,
    pub receipt_type: u8,
    pub through_lamport: u64,
    pub timestamp: i64,
    pub hop_ttl: u8,
    pub expiry: i64,
    pub recipient_hint: Vec<u8>,
    pub sealed: Vec<u8>,
}

#[derive(uniffi::Object)]
pub struct MessageStore {
    conn: Mutex<Connection>,
}

#[uniffi::export]
impl MessageStore {
    /// Open (creating if needed) the message store at `path`. Pass
    /// `":memory:"` for an ephemeral in-process store.
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Self, CoreError> {
        let conn = Connection::open(&path).map_err(store_err)?;
        conn.execute_batch(SCHEMA).map_err(store_err)?;
        ensure_contact_column(&conn, "relay_token", "TEXT")?;
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Insert a message. Returns `true` if a new row was inserted, `false`
    /// if (chat_id, sender_user_id, lamport) was already present.
    pub fn insert_message(&self, message: StoredMessage) -> Result<bool, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO messages
                    (chat_id, sender_user_id, lamport, timestamp, kind, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    message.chat_id,
                    message.sender_user_id,
                    message.lamport as i64,
                    message.timestamp,
                    message.kind as i64,
                    message.payload,
                ],
            )
        .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// Atomically persist one locally authored message and the exact sealed
    /// envelope that should be retried for it over BLE and relay. The message
    /// row stays idempotent on `(chat_id, sender_user_id, lamport)`; the
    /// outbound queue uses the same logical identity as its dedupe key, so
    /// re-queuing the same authored message is a no-op instead of creating a
    /// second `msg_id`.
    pub fn insert_outgoing_message(
        &self,
        message: StoredMessage,
        envelope: OutboundEnvelope,
        queued_at_ms: i64,
    ) -> Result<bool, CoreError> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        tx.execute(
            "INSERT OR IGNORE INTO messages
                (chat_id, sender_user_id, lamport, timestamp, kind, payload)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                message.chat_id,
                message.sender_user_id,
                message.lamport as i64,
                message.timestamp,
                message.kind as i64,
                message.payload,
            ],
        )
        .map_err(store_err)?;
        let changed = tx
            .execute(
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
                    queued_at_ms,
                ],
            )
            .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(changed > 0)
    }

    /// All messages in a chat, oldest first by author timestamp.
    ///
    /// `lamport` is only comparable within one sender's stream
    /// (`chat_id`,`sender_user_id`), not across both participants in a 1:1
    /// chat. So conversation display order must not be `ORDER BY lamport`
    /// across the whole chat or a later message from Alice can render before
    /// an earlier-timestamped message from Bob simply because Alice's local
    /// counter is smaller. `id` is only a stable tie-breaker for equal
    /// timestamps.
    pub fn messages_for_chat(&self, chat_id: Vec<u8>) -> Result<Vec<StoredMessage>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload
                 FROM messages WHERE chat_id = ?1 ORDER BY timestamp ASC, id ASC",
            )
            .map_err(store_err)?;
        let rows = stmt.query_map(params![chat_id], row_to_message).map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// The highest lamport value N such that every message `1..=N` from this
    /// sender in this chat is present -- the point up to which there's no
    /// gap (DESIGN.md §7.3: "message 12 arrived, 11 hasn't -- keep
    /// waiting"). Returns 0 if message 1 itself hasn't arrived yet.
    pub fn highest_contiguous_lamport(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
    ) -> Result<u64, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        highest_contiguous_lamport_locked(&conn, &chat_id, &sender_user_id)
    }

    /// A sync digest for `chat_id` (DESIGN.md §7.3): one [`DigestEntry`] per
    /// distinct sender who has ever posted in this chat, each with their
    /// [`MessageStore::highest_contiguous_lamport`]. Ordered by
    /// `sender_user_id` for a deterministic wire encoding. See the module
    /// docs for why this covers only the contiguous-lamport half of §7.3's
    /// digest (the recent-msg_id bloom filter is deferred).
    pub fn chat_digest(&self, chat_id: Vec<u8>) -> Result<Vec<DigestEntry>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT sender_user_id FROM messages
                 WHERE chat_id = ?1 ORDER BY sender_user_id ASC",
            )
            .map_err(store_err)?;
        let senders = stmt
            .query_map(params![chat_id], |row| row.get::<_, Vec<u8>>(0))
            .map_err(store_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(store_err)?;

        let mut entries = Vec::with_capacity(senders.len());
        for sender_user_id in senders {
            let through_lamport = highest_contiguous_lamport_locked(&conn, &chat_id, &sender_user_id)?;
            entries.push(DigestEntry { sender_user_id, through_lamport });
        }
        Ok(entries)
    }

    /// Messages from `sender_user_id` in `chat_id` with `lamport >
    /// after_lamport`, oldest first -- what a peer whose digest reported
    /// `after_lamport` for this sender is missing (DESIGN.md §7.3).
    pub fn messages_after(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        after_lamport: u64,
    ) -> Result<Vec<StoredMessage>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload
                 FROM messages
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport > ?3
                 ORDER BY lamport ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![chat_id, sender_user_id, after_lamport as i64], row_to_message)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Exact sealed envelopes for this device's authored messages in
    /// `chat_id` whose lamport is above `after_lamport`, oldest first. This
    /// is the transport-level counterpart to [`MessageStore::messages_after`]:
    /// same logical retry set, but with the stable persisted `msg_id` and
    /// ciphertext needed for dedupe-safe resend across any authored
    /// message-kind that participates in the chat lamport stream.
    pub fn outbound_envelopes_after(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        after_lamport: u64,
    ) -> Result<Vec<OutboundEnvelope>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, kind, lamport,
                        timestamp, hop_ttl, expiry, recipient_hint, sealed
                 FROM outbound_envelopes
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport > ?3
                 ORDER BY lamport ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![chat_id, sender_user_id, after_lamport as i64], row_to_outbound)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Relay-upload candidates: locally authored envelopes not yet marked as
    /// posted to a relay, unexpired as of `now_ms`, oldest first.
    pub fn pending_relay_outbound_envelopes(
        &self,
        limit: u64,
        now_ms: i64,
    ) -> Result<Vec<OutboundEnvelope>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, kind, lamport,
                        timestamp, hop_ttl, expiry, recipient_hint, sealed
                 FROM outbound_envelopes
                 WHERE relay_posted_at IS NULL AND expiry > ?1
                 ORDER BY queued_at ASC, msg_id ASC
                 LIMIT ?2",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![now_ms, limit as i64], row_to_outbound)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Mark one outbound envelope as successfully posted to a relay. Returns
    /// `true` if a queued row was updated.
    pub fn mark_outbound_envelope_relay_posted(
        &self,
        msg_id: Vec<u8>,
        posted_at_ms: i64,
    ) -> Result<bool, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn
            .execute(
                "UPDATE outbound_envelopes SET relay_posted_at = ?2 WHERE msg_id = ?1",
                params![msg_id, posted_at_ms],
            )
            .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// Delete expired outbound envelopes as of `now_ms`. The plaintext
    /// message history stays intact; this only prunes retry state whose public
    /// expiry window has elapsed.
    pub fn prune_expired_outbound_envelopes(&self, now_ms: i64) -> Result<u64, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let pruned = conn
            .execute("DELETE FROM outbound_envelopes WHERE expiry <= ?1", params![now_ms])
            .map_err(store_err)?;
        Ok(pruned as u64)
    }

    /// The latest relay-uploadable receipt envelope persisted for this
    /// cumulative outgoing receipt watermark, if any.
    pub fn outgoing_receipt_envelope(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
    ) -> Result<Option<OutgoingReceiptEnvelope>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, receipt_type,
                    through_lamport, timestamp, hop_ttl, expiry, recipient_hint, sealed
             FROM outgoing_receipt_envelopes
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
            params![chat_id, sender_user_id, receipt_type as i64],
            row_to_outgoing_receipt,
        )
        .optional()
        .map_err(store_err)
    }

    /// Persist or advance the exact sealed receipt envelope to relay-upload
    /// for one logical outgoing receipt watermark. Same watermark -> no-op,
    /// preserving the existing `msg_id`; higher watermark -> replace the row
    /// and clear `relay_posted_at`; lower watermark -> ignored as stale.
    pub fn upsert_outgoing_receipt_envelope(
        &self,
        envelope: OutgoingReceiptEnvelope,
        queued_at_ms: i64,
    ) -> Result<bool, CoreError> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT through_lamport FROM outgoing_receipt_envelopes
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![
                    &envelope.chat_id,
                    &envelope.sender_user_id,
                    envelope.receipt_type as i64,
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let changed = match existing {
            Some(current) if current >= envelope.through_lamport as i64 => false,
            Some(_) => {
                tx.execute(
                    "UPDATE outgoing_receipt_envelopes
                     SET msg_id = ?4,
                         recipient_user_id = ?5,
                         through_lamport = ?6,
                         timestamp = ?7,
                         hop_ttl = ?8,
                         expiry = ?9,
                         recipient_hint = ?10,
                         sealed = ?11,
                         queued_at = ?12,
                         relay_posted_at = NULL
                     WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                    params![
                        &envelope.chat_id,
                        &envelope.sender_user_id,
                        envelope.receipt_type as i64,
                        &envelope.msg_id,
                        &envelope.recipient_user_id,
                        envelope.through_lamport as i64,
                        envelope.timestamp,
                        envelope.hop_ttl as i64,
                        envelope.expiry,
                        &envelope.recipient_hint,
                        &envelope.sealed,
                        queued_at_ms,
                    ],
                )
                .map_err(store_err)?;
                true
            }
            None => {
                tx.execute(
                    "INSERT INTO outgoing_receipt_envelopes
                        (chat_id, sender_user_id, receipt_type, through_lamport, msg_id,
                         recipient_user_id, timestamp, hop_ttl, expiry, recipient_hint,
                         sealed, queued_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        &envelope.chat_id,
                        &envelope.sender_user_id,
                        envelope.receipt_type as i64,
                        envelope.through_lamport as i64,
                        &envelope.msg_id,
                        &envelope.recipient_user_id,
                        envelope.timestamp,
                        envelope.hop_ttl as i64,
                        envelope.expiry,
                        &envelope.recipient_hint,
                        &envelope.sealed,
                        queued_at_ms,
                    ],
                )
                .map_err(store_err)?;
                true
            }
        };
        tx.commit().map_err(store_err)?;
        Ok(changed)
    }

    /// Relay-upload candidates: persisted receipt envelopes not yet marked as
    /// posted to a relay, unexpired as of `now_ms`, oldest first.
    pub fn pending_relay_outgoing_receipt_envelopes(
        &self,
        limit: u64,
        now_ms: i64,
    ) -> Result<Vec<OutgoingReceiptEnvelope>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, receipt_type,
                        through_lamport, timestamp, hop_ttl, expiry, recipient_hint, sealed
                 FROM outgoing_receipt_envelopes
                 WHERE relay_posted_at IS NULL AND expiry > ?1
                 ORDER BY queued_at ASC, msg_id ASC
                 LIMIT ?2",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![now_ms, limit as i64], row_to_outgoing_receipt)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Mark one outgoing receipt envelope as successfully posted to a relay.
    /// Returns `true` if a queued row was updated.
    pub fn mark_outgoing_receipt_envelope_relay_posted(
        &self,
        msg_id: Vec<u8>,
        posted_at_ms: i64,
    ) -> Result<bool, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn
            .execute(
                "UPDATE outgoing_receipt_envelopes SET relay_posted_at = ?2 WHERE msg_id = ?1",
                params![msg_id, posted_at_ms],
            )
            .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// Delete expired outgoing receipt envelopes as of `now_ms`. The
    /// underlying outgoing receipt watermark remains in `outgoing_receipts`;
    /// this only prunes the persisted sealed retry artifact.
    pub fn prune_expired_outgoing_receipt_envelopes(&self, now_ms: i64) -> Result<u64, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let pruned = conn
            .execute(
                "DELETE FROM outgoing_receipt_envelopes WHERE expiry <= ?1",
                params![now_ms],
            )
            .map_err(store_err)?;
        Ok(pruned as u64)
    }

    /// Record that a peer has delivered/read messages authored by
    /// `sender_user_id` in `chat_id` through `through_lamport` (DESIGN.md
    /// §7.2). Monotonic: if a receipt for the same (chat_id,
    /// sender_user_id, receipt_type) is already recorded with a
    /// `through_lamport` at or above this one, it's left unchanged --
    /// receipts can arrive out of order or be replayed under DTN, and a
    /// stale/duplicate receipt must never regress what's already known.
    pub fn record_receipt(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
        through_lamport: u64,
    ) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO receipts (chat_id, sender_user_id, receipt_type, through_lamport)
                VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                through_lamport = MAX(through_lamport, excluded.through_lamport)",
            params![chat_id, sender_user_id, receipt_type as i64, through_lamport as i64],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// The cumulative lamport a receipt of `receipt_type` covers for
    /// `sender_user_id`'s messages in `chat_id` (DESIGN.md §7.2). Returns 0
    /// if no such receipt has been recorded.
    pub fn receipt_through(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
    ) -> Result<u64, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let through: Option<i64> = conn
            .query_row(
                "SELECT through_lamport FROM receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![chat_id, sender_user_id, receipt_type as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(through.unwrap_or(0) as u64)
    }

    /// Record that *this device* has delivered/read messages authored by
    /// `sender_user_id` in `chat_id` through `through_lamport` -- the
    /// cumulative receipt watermark to send back on the next peer sync
    /// (DESIGN.md §7.2, §7.3). Monotonic for the same reason as
    /// [`MessageStore::record_receipt`]: once a receipt watermark advances,
    /// stale retries must never regress it.
    pub fn record_outgoing_receipt(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
        through_lamport: u64,
    ) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO outgoing_receipts (chat_id, sender_user_id, receipt_type, through_lamport)
                VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                through_lamport = MAX(through_lamport, excluded.through_lamport)",
            params![chat_id, sender_user_id, receipt_type as i64, through_lamport as i64],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// The cumulative lamport this device should report back in an outgoing
    /// receipt of `receipt_type` for `sender_user_id`'s messages in `chat_id`
    /// (DESIGN.md §7.2, §7.3). Returns 0 if no such local receipt state has
    /// been recorded yet.
    pub fn outgoing_receipt_through(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
    ) -> Result<u64, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let through: Option<i64> = conn
            .query_row(
                "SELECT through_lamport FROM outgoing_receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![chat_id, sender_user_id, receipt_type as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(through.unwrap_or(0) as u64)
    }

    /// Add or update a contact, keyed on `user_id` -- re-scanning the same
    /// FriendCard (e.g. after they update their display name) replaces the
    /// row rather than erroring or duplicating.
    pub fn upsert_contact(&self, contact: Contact) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO contacts (user_id, name, sign_pk, agree_pk, relay_url, relay_token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(user_id) DO UPDATE SET
                name = excluded.name,
                sign_pk = excluded.sign_pk,
                agree_pk = excluded.agree_pk,
                relay_url = excluded.relay_url,
                relay_token = excluded.relay_token",
            params![
                contact.user_id,
                contact.name,
                contact.sign_pk,
                contact.agree_pk,
                contact.relay_url,
                contact.relay_token,
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Delete a contact and, with it, the entire 1:1 chat: the contact row,
    /// every message whose `chat_id` is their UserID (DESIGN.md §7.1: a 1:1
    /// chat's id *is* the peer's UserID), and that chat's incoming/outgoing
    /// receipt rows.
    ///
    /// Messages are deleted rather than retained, deliberately: the driving
    /// use case is pruning a dead contact whose identity changed (e.g. a
    /// reinstall regenerated their keys), where the old chat can never
    /// receive again -- and this app's privacy posture (DESIGN.md §6.4
    /// hides even receipt metadata from the wire) argues against quietly
    /// hoarding plaintext history for a peer the user chose to remove.
    /// Group messages from this sender are untouched (they live under the
    /// group's chat_id and belong to the group, not the contact).
    ///
    /// Atomic (single transaction) and idempotent: deleting an unknown
    /// contact is a no-op. Returns `true` if a contact row was removed.
    pub fn delete_contact(&self, user_id: Vec<u8>) -> Result<bool, CoreError> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let removed = tx
            .execute("DELETE FROM contacts WHERE user_id = ?1", params![user_id])
            .map_err(store_err)?;
        tx.execute("DELETE FROM messages WHERE chat_id = ?1", params![user_id])
            .map_err(store_err)?;
        tx.execute("DELETE FROM receipts WHERE chat_id = ?1", params![user_id])
            .map_err(store_err)?;
        tx.execute("DELETE FROM outgoing_receipts WHERE chat_id = ?1", params![user_id])
            .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outgoing_receipt_envelopes WHERE chat_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(removed > 0)
    }

    /// Look up a single contact by UserID, or `None` if not a contact.
    pub fn get_contact(&self, user_id: Vec<u8>) -> Result<Option<Contact>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT user_id, name, sign_pk, agree_pk, relay_url, relay_token FROM contacts WHERE user_id = ?1",
            params![user_id],
            row_to_contact,
        )
        .optional()
        .map_err(store_err)
    }

    /// All contacts, alphabetical by name.
    pub fn list_contacts(&self) -> Result<Vec<Contact>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare("SELECT user_id, name, sign_pk, agree_pk, relay_url, relay_token FROM contacts ORDER BY name ASC")
            .map_err(store_err)?;
        let rows = stmt.query_map([], row_to_contact).map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    // --- carry queue (DESIGN.md §5.3) --------------------------------------

    /// Store a foreign envelope for later store-and-forward delivery
    /// (DESIGN.md §5.3 carry queue). Keyed on `msg_id`, so re-enqueuing an
    /// envelope we're already carrying is a no-op (returns `false`); a fresh
    /// insert returns `true`.
    ///
    /// `is_family` marks whether this envelope is addressed to someone this
    /// node knows (its `recipient_hint` matched a contact -- the caller
    /// decides, since it holds the contacts and the hint derivation). Family
    /// envelopes are kept until they expire and **never** evicted for space;
    /// only foreign envelopes count against `foreign_budget_bytes` (DESIGN.md
    /// §5.3: "Family messages always win eviction fights"). When inserting a
    /// new foreign envelope pushes the foreign total over budget, the oldest
    /// foreign envelopes (by `received_at_ms`) are evicted until it fits --
    /// possibly including this one, if a single envelope exceeds the whole
    /// budget. All of this happens in one transaction.
    pub fn enqueue_carried_envelope(
        &self,
        envelope: CarriedEnvelope,
        is_family: bool,
        received_at_ms: i64,
        foreign_budget_bytes: i64,
    ) -> Result<bool, CoreError> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let size = envelope.sealed.len() as i64;
        let changed = tx
            .execute(
                "INSERT OR IGNORE INTO carried_envelopes
                    (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family, received_at, size_bytes)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    envelope.msg_id,
                    envelope.hop_ttl as i64,
                    envelope.expiry,
                    envelope.recipient_hint,
                    envelope.sealed,
                    is_family as i64,
                    received_at_ms,
                    size,
                ],
            )
            .map_err(store_err)?;

        if changed == 0 {
            // Already carrying this msg_id: nothing inserted, so the budget
            // can't have grown -- skip eviction.
            tx.commit().map_err(store_err)?;
            return Ok(false);
        }

        // Family envelopes are evict-proof; enforce the budget over foreign
        // ones only, dropping oldest-first until back within it.
        let mut foreign_total: i64 = tx
            .query_row(
                "SELECT COALESCE(SUM(size_bytes), 0) FROM carried_envelopes WHERE is_family = 0",
                [],
                |row| row.get(0),
            )
            .map_err(store_err)?;
        while foreign_total > foreign_budget_bytes {
            let oldest: Option<(Vec<u8>, i64)> = tx
                .query_row(
                    "SELECT msg_id, size_bytes FROM carried_envelopes
                     WHERE is_family = 0 ORDER BY received_at ASC, msg_id ASC LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(store_err)?;
            match oldest {
                Some((msg_id, sz)) => {
                    tx.execute("DELETE FROM carried_envelopes WHERE msg_id = ?1", params![msg_id])
                        .map_err(store_err)?;
                    foreign_total -= sz;
                }
                None => break,
            }
        }
        tx.commit().map_err(store_err)?;
        Ok(true)
    }

    /// Carried envelopes whose `recipient_hint` matches any of `hints` and
    /// that haven't expired as of `now_ms`, oldest first (DESIGN.md §5.3).
    /// The caller passes the set of hints a just-met peer could match --
    /// `recipient_hint` rotates daily (§6.4), so that's the peer's UserID
    /// hashed against each recent day. A match means "this envelope is for
    /// that peer," so the caller can hand it over and then
    /// [`MessageStore::remove_carried_envelope`] it.
    pub fn carried_envelopes_for_hints(
        &self,
        hints: Vec<Vec<u8>>,
        now_ms: i64,
    ) -> Result<Vec<CarriedEnvelope>, CoreError> {
        if hints.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        let placeholders = std::iter::repeat("?").take(hints.len()).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT msg_id, hop_ttl, expiry, recipient_hint, sealed
             FROM carried_envelopes
             WHERE expiry > ?1 AND recipient_hint IN ({placeholders})
             ORDER BY received_at ASC, msg_id ASC"
        );
        let mut stmt = conn.prepare(&sql).map_err(store_err)?;
        let mut bindings: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(hints.len() + 1);
        bindings.push(&now_ms);
        for hint in &hints {
            bindings.push(hint);
        }
        let rows = stmt
            .query_map(bindings.as_slice(), row_to_carried)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Up to `limit` carried-envelope `msg_id`s, oldest first. This is the
    /// exact-set stand-in for §7.3's "recent msg_id bloom filter": enough for
    /// a peer to say "I already carry these" so another mule doesn't blindly
    /// resend them on every reconnect.
    pub fn carried_msg_ids(&self, limit: u64) -> Result<Vec<Vec<u8>>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT msg_id FROM carried_envelopes
                 ORDER BY received_at ASC, msg_id ASC
                 LIMIT ?1",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![limit as i64], |row| row.get::<_, Vec<u8>>(0))
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Carried envelopes suitable to spray to a non-recipient mule on peer
    /// sync: unexpired as of `now_ms`, not already known to the peer
    /// (`peer_known_msg_ids` from its digest), and not actually addressed to
    /// that peer (`peer_hints`, which the targeted-delivery path handles
    /// separately). Ordered oldest first.
    pub fn carried_envelopes_for_peer_sync(
        &self,
        peer_hints: Vec<Vec<u8>>,
        peer_known_msg_ids: Vec<Vec<u8>>,
        now_ms: i64,
    ) -> Result<Vec<CarriedEnvelope>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT msg_id, hop_ttl, expiry, recipient_hint, sealed
                 FROM carried_envelopes
                 WHERE expiry > ?1
                 ORDER BY received_at ASC, msg_id ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![now_ms], row_to_carried)
            .map_err(store_err)?;
        let known_msg_ids: HashSet<Vec<u8>> = peer_known_msg_ids.into_iter().collect();
        let peer_hints: HashSet<Vec<u8>> = peer_hints.into_iter().collect();
        let all = rows.collect::<Result<Vec<_>, _>>().map_err(store_err)?;
        Ok(all
            .into_iter()
            .filter(|env| !known_msg_ids.contains(&env.msg_id))
            .filter(|env| !peer_hints.contains(&env.recipient_hint))
            .collect())
    }

    /// Drop a carried envelope by `msg_id` -- called once it's been handed to
    /// its recipient (DESIGN.md §5.3: a mule's job is done on delivery).
    /// Returns `true` if a row was removed.
    pub fn remove_carried_envelope(&self, msg_id: Vec<u8>) -> Result<bool, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let removed = conn
            .execute("DELETE FROM carried_envelopes WHERE msg_id = ?1", params![msg_id])
            .map_err(store_err)?;
        Ok(removed > 0)
    }

    /// Delete every carried envelope whose `expiry` is at or before `now_ms`
    /// (DESIGN.md §5.3: "carriers drop the envelope past this time"). Returns
    /// how many were pruned.
    pub fn prune_expired_carried(&self, now_ms: i64) -> Result<u64, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let pruned = conn
            .execute("DELETE FROM carried_envelopes WHERE expiry <= ?1", params![now_ms])
            .map_err(store_err)?;
        Ok(pruned as u64)
    }

    /// Number of envelopes currently in the carry queue (diagnostics/tests).
    pub fn carried_len(&self) -> Result<u64, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM carried_envelopes", [], |row| row.get(0))
            .map_err(store_err)?;
        Ok(count as u64)
    }

    /// Unexpired carried envelopes that were classified as family traffic
    /// when received, oldest first. Used by relay upload so one phone with
    /// internet can uplink ciphertext it is muling for known contacts.
    pub fn family_carried_envelopes(
        &self,
        limit: u64,
        now_ms: i64,
    ) -> Result<Vec<CarriedEnvelope>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT msg_id, hop_ttl, expiry, recipient_hint, sealed
                 FROM carried_envelopes
                 WHERE is_family = 1 AND expiry > ?1
                 ORDER BY received_at ASC, msg_id ASC
                 LIMIT ?2",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![now_ms, limit as i64], row_to_carried)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }
}

/// Shared by [`MessageStore::highest_contiguous_lamport`] and
/// [`MessageStore::chat_digest`] (which needs it once per sender, under a
/// single lock acquisition -- `Connection`'s `Mutex` isn't reentrant, so
/// `chat_digest` can't just call the `&self` method above for each sender).
fn highest_contiguous_lamport_locked(
    conn: &Connection,
    chat_id: &[u8],
    sender_user_id: &[u8],
) -> Result<u64, CoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT lamport FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2
             ORDER BY lamport ASC",
        )
        .map_err(store_err)?;
    let lamports = stmt
        .query_map(params![chat_id, sender_user_id], |row| row.get::<_, i64>(0))
        .map_err(store_err)?;

    let mut expected: u64 = 1;
    for lamport in lamports {
        let lamport = lamport.map_err(store_err)? as u64;
        if lamport != expected {
            break;
        }
        expected += 1;
    }
    Ok(expected - 1)
}

fn row_to_message(row: &rusqlite::Row) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        chat_id: row.get(0)?,
        sender_user_id: row.get(1)?,
        lamport: row.get::<_, i64>(2)? as u64,
        timestamp: row.get(3)?,
        kind: row.get::<_, i64>(4)? as u8,
        payload: row.get(5)?,
    })
}

fn row_to_carried(row: &rusqlite::Row) -> rusqlite::Result<CarriedEnvelope> {
    Ok(CarriedEnvelope {
        msg_id: row.get(0)?,
        hop_ttl: row.get::<_, i64>(1)? as u8,
        expiry: row.get(2)?,
        recipient_hint: row.get(3)?,
        sealed: row.get(4)?,
    })
}

fn row_to_outbound(row: &rusqlite::Row) -> rusqlite::Result<OutboundEnvelope> {
    Ok(OutboundEnvelope {
        msg_id: row.get(0)?,
        recipient_user_id: row.get(1)?,
        chat_id: row.get(2)?,
        sender_user_id: row.get(3)?,
        kind: row.get::<_, i64>(4)? as u8,
        lamport: row.get::<_, i64>(5)? as u64,
        timestamp: row.get(6)?,
        hop_ttl: row.get::<_, i64>(7)? as u8,
        expiry: row.get(8)?,
        recipient_hint: row.get(9)?,
        sealed: row.get(10)?,
    })
}

fn row_to_outgoing_receipt(row: &rusqlite::Row) -> rusqlite::Result<OutgoingReceiptEnvelope> {
    Ok(OutgoingReceiptEnvelope {
        msg_id: row.get(0)?,
        recipient_user_id: row.get(1)?,
        chat_id: row.get(2)?,
        sender_user_id: row.get(3)?,
        receipt_type: row.get::<_, i64>(4)? as u8,
        through_lamport: row.get::<_, i64>(5)? as u64,
        timestamp: row.get(6)?,
        hop_ttl: row.get::<_, i64>(7)? as u8,
        expiry: row.get(8)?,
        recipient_hint: row.get(9)?,
        sealed: row.get(10)?,
    })
}

fn row_to_contact(row: &rusqlite::Row) -> rusqlite::Result<Contact> {
    Ok(Contact {
        user_id: row.get(0)?,
        name: row.get(1)?,
        sign_pk: row.get(2)?,
        agree_pk: row.get(3)?,
        relay_url: row.get(4)?,
        relay_token: row.get(5)?,
    })
}

fn store_err(e: rusqlite::Error) -> CoreError {
    CoreError::Store(e.to_string())
}

fn outbound_message_dedupe_key(
    chat_id: &[u8],
    sender_user_id: &[u8],
    kind: u8,
    lamport: u64,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 2 + chat_id.len() + 2 + sender_user_id.len() + 1 + 8);
    out.push(1);
    write_bytes16_local(&mut out, chat_id);
    write_bytes16_local(&mut out, sender_user_id);
    out.push(kind);
    out.extend_from_slice(&lamport.to_be_bytes());
    out
}

fn write_bytes16_local(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn ensure_contact_column(conn: &Connection, name: &str, column_def: &str) -> Result<(), CoreError> {
    let mut stmt = conn.prepare("PRAGMA table_info(contacts)").map_err(store_err)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(store_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_err)?;
    if names.iter().any(|existing| existing == name) {
        return Ok(());
    }
    conn.execute(&format!("ALTER TABLE contacts ADD COLUMN {name} {column_def}"), [])
        .map_err(store_err)?;
    Ok(())
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS messages (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id        BLOB NOT NULL,
    sender_user_id BLOB NOT NULL,
    lamport        INTEGER NOT NULL,
    timestamp      INTEGER NOT NULL,
    kind           INTEGER NOT NULL,
    payload        BLOB NOT NULL,
    UNIQUE(chat_id, sender_user_id, lamport)
);
CREATE INDEX IF NOT EXISTS idx_messages_chat_lamport ON messages(chat_id, lamport);
CREATE INDEX IF NOT EXISTS idx_messages_chat_timestamp_id ON messages(chat_id, timestamp, id);

CREATE TABLE IF NOT EXISTS contacts (
    user_id   BLOB PRIMARY KEY,
    name      TEXT NOT NULL,
    sign_pk   BLOB NOT NULL,
    agree_pk  BLOB NOT NULL,
    relay_url TEXT,
    relay_token TEXT
);

CREATE TABLE IF NOT EXISTS receipts (
    chat_id         BLOB NOT NULL,
    sender_user_id  BLOB NOT NULL,
    receipt_type    INTEGER NOT NULL,
    through_lamport INTEGER NOT NULL,
    PRIMARY KEY(chat_id, sender_user_id, receipt_type)
);

CREATE TABLE IF NOT EXISTS outgoing_receipts (
    chat_id         BLOB NOT NULL,
    sender_user_id  BLOB NOT NULL,
    receipt_type    INTEGER NOT NULL,
    through_lamport INTEGER NOT NULL,
    PRIMARY KEY(chat_id, sender_user_id, receipt_type)
);

CREATE TABLE IF NOT EXISTS outgoing_receipt_envelopes (
    chat_id           BLOB NOT NULL,
    sender_user_id    BLOB NOT NULL,
    receipt_type      INTEGER NOT NULL,
    through_lamport   INTEGER NOT NULL,
    msg_id            BLOB NOT NULL UNIQUE,
    recipient_user_id BLOB NOT NULL,
    timestamp         INTEGER NOT NULL,
    hop_ttl           INTEGER NOT NULL,
    expiry            INTEGER NOT NULL,
    recipient_hint    BLOB NOT NULL,
    sealed            BLOB NOT NULL,
    queued_at         INTEGER NOT NULL,
    relay_posted_at   INTEGER,
    PRIMARY KEY(chat_id, sender_user_id, receipt_type)
);
CREATE INDEX IF NOT EXISTS idx_outgoing_receipt_envelopes_relay_posted_queued
    ON outgoing_receipt_envelopes(relay_posted_at, queued_at);
CREATE INDEX IF NOT EXISTS idx_outgoing_receipt_envelopes_expiry
    ON outgoing_receipt_envelopes(expiry);

CREATE TABLE IF NOT EXISTS outbound_envelopes (
    dedupe_key        BLOB NOT NULL UNIQUE,
    msg_id            BLOB PRIMARY KEY,
    recipient_user_id BLOB NOT NULL,
    chat_id           BLOB NOT NULL,
    sender_user_id    BLOB NOT NULL,
    kind              INTEGER NOT NULL,
    lamport           INTEGER NOT NULL,
    timestamp         INTEGER NOT NULL,
    hop_ttl           INTEGER NOT NULL,
    expiry            INTEGER NOT NULL,
    recipient_hint    BLOB NOT NULL,
    sealed            BLOB NOT NULL,
    queued_at         INTEGER NOT NULL,
    relay_posted_at   INTEGER
);
CREATE INDEX IF NOT EXISTS idx_outbound_chat_sender_lamport
    ON outbound_envelopes(chat_id, sender_user_id, lamport);
CREATE INDEX IF NOT EXISTS idx_outbound_relay_posted_queued
    ON outbound_envelopes(relay_posted_at, queued_at);
CREATE INDEX IF NOT EXISTS idx_outbound_expiry ON outbound_envelopes(expiry);

CREATE TABLE IF NOT EXISTS carried_envelopes (
    msg_id         BLOB PRIMARY KEY,
    hop_ttl        INTEGER NOT NULL,
    expiry         INTEGER NOT NULL,
    recipient_hint BLOB NOT NULL,
    sealed         BLOB NOT NULL,
    is_family      INTEGER NOT NULL,
    received_at    INTEGER NOT NULL,
    size_bytes     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_carried_hint ON carried_envelopes(recipient_hint);
CREATE INDEX IF NOT EXISTS idx_carried_expiry ON carried_envelopes(expiry);
";

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    const DEFAULT_HOP_TTL: u8 = 7;

    fn msg(chat: &[u8], sender: &[u8], lamport: u64, text: &str) -> StoredMessage {
        StoredMessage {
            chat_id: chat.to_vec(),
            sender_user_id: sender.to_vec(),
            lamport,
            timestamp: 1_700_000_000_000,
            kind: 1,
            payload: text.as_bytes().to_vec(),
        }
    }

    fn outbound_for(message: &StoredMessage, recipient_user_id: &[u8], msg_id: &[u8]) -> OutboundEnvelope {
        OutboundEnvelope {
            msg_id: msg_id.to_vec(),
            recipient_user_id: recipient_user_id.to_vec(),
            chat_id: message.chat_id.clone(),
            sender_user_id: message.sender_user_id.clone(),
            kind: message.kind,
            lamport: message.lamport,
            timestamp: message.timestamp,
            hop_ttl: DEFAULT_HOP_TTL,
            expiry: message.timestamp + 60_000,
            recipient_hint: b"hint-123".to_vec(),
            sealed: format!("sealed-{}", message.lamport).into_bytes(),
        }
    }

    fn outgoing_receipt_for(
        chat_id: &[u8],
        sender_user_id: &[u8],
        recipient_user_id: &[u8],
        receipt_type: u8,
        through_lamport: u64,
        msg_id: &[u8],
    ) -> OutgoingReceiptEnvelope {
        OutgoingReceiptEnvelope {
            msg_id: msg_id.to_vec(),
            recipient_user_id: recipient_user_id.to_vec(),
            chat_id: chat_id.to_vec(),
            sender_user_id: sender_user_id.to_vec(),
            receipt_type,
            through_lamport,
            timestamp: 1_700_000_000_000,
            hop_ttl: DEFAULT_HOP_TTL,
            expiry: 1_700_000_060_000,
            recipient_hint: b"hint-456".to_vec(),
            sealed: format!("receipt-{receipt_type}-{through_lamport}").into_bytes(),
        }
    }

    #[test]
    fn insert_then_fetch_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 1, "hi")).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 2, "there")).unwrap();

        let messages = store.messages_for_chat(b"chat-a".to_vec()).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].payload, b"hi");
        assert_eq!(messages[1].payload, b"there");
    }

    #[test]
    fn messages_for_chat_orders_mixed_senders_by_timestamp_not_lamport() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();

        let mut later_lamport_earlier_time = msg(b"chat-a", b"bob", 5, "seems to work");
        later_lamport_earlier_time.timestamp = 100;
        store.insert_message(later_lamport_earlier_time).unwrap();

        let mut later_lamport_later_time = msg(b"chat-a", b"bob", 6, "what about this?");
        later_lamport_later_time.timestamp = 200;
        store.insert_message(later_lamport_later_time).unwrap();

        let mut lower_lamport_latest_time = msg(b"chat-a", b"alice", 4, "rr-test-1");
        lower_lamport_latest_time.timestamp = 300;
        store.insert_message(lower_lamport_latest_time).unwrap();

        let payloads: Vec<Vec<u8>> = store
            .messages_for_chat(b"chat-a".to_vec())
            .unwrap()
            .into_iter()
            .map(|m| m.payload)
            .collect();

        assert_eq!(
            payloads,
            vec![
                b"seems to work".to_vec(),
                b"what about this?".to_vec(),
                b"rr-test-1".to_vec(),
            ]
        );
    }

    #[test]
    fn insert_is_idempotent_on_dedupe_key() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(store.insert_message(msg(b"chat-a", b"alice", 1, "hi")).unwrap());
        // Re-delivery of the same envelope (expected under DTN): no-op, not an error.
        assert!(!store.insert_message(msg(b"chat-a", b"alice", 1, "hi")).unwrap());
        assert_eq!(store.messages_for_chat(b"chat-a".to_vec()).unwrap().len(), 1);
    }

    #[test]
    fn messages_are_isolated_per_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 1, "hi")).unwrap();
        store.insert_message(msg(b"chat-b", b"alice", 1, "yo")).unwrap();

        assert_eq!(store.messages_for_chat(b"chat-a".to_vec()).unwrap().len(), 1);
        assert_eq!(store.messages_for_chat(b"chat-b".to_vec()).unwrap().len(), 1);
    }

    #[test]
    fn highest_contiguous_lamport_is_zero_with_no_messages() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let n = store
            .highest_contiguous_lamport(b"chat-a".to_vec(), b"alice".to_vec())
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn highest_contiguous_lamport_stops_at_a_gap() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 1, "one")).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 2, "two")).unwrap();
        // lamport 3 is missing -- message 4 arrived out of order (DTN reality).
        store.insert_message(msg(b"chat-a", b"alice", 4, "four")).unwrap();

        let n = store
            .highest_contiguous_lamport(b"chat-a".to_vec(), b"alice".to_vec())
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn highest_contiguous_lamport_is_per_sender_not_per_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 1, "hi")).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 2, "there")).unwrap();
        // Bob's own counter in the same chat starts independently at 1.
        store.insert_message(msg(b"chat-a", b"bob", 1, "hey")).unwrap();

        assert_eq!(
            store.highest_contiguous_lamport(b"chat-a".to_vec(), b"alice".to_vec()).unwrap(),
            2
        );
        assert_eq!(
            store.highest_contiguous_lamport(b"chat-a".to_vec(), b"bob".to_vec()).unwrap(),
            1
        );
    }

    #[test]
    fn insert_outgoing_message_persists_message_and_queue_entry() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        let outbound = outbound_for(&message, b"bob", b"msg-000000000001");

        assert!(store
            .insert_outgoing_message(message.clone(), outbound.clone(), 1_700_000_000_100)
            .unwrap());

        assert_eq!(store.messages_for_chat(b"chat-a".to_vec()).unwrap(), vec![message]);
        assert_eq!(
            store
                .outbound_envelopes_after(b"chat-a".to_vec(), b"alice".to_vec(), 0)
                .unwrap(),
            vec![outbound],
        );
    }

    #[test]
    fn insert_outgoing_message_is_idempotent_on_logical_message_identity() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        let first = outbound_for(&message, b"bob", b"msg-000000000001");
        let second = outbound_for(&message, b"bob", b"msg-000000000002");

        assert!(store
            .insert_outgoing_message(message.clone(), first.clone(), 1_700_000_000_100)
            .unwrap());
        assert!(!store
            .insert_outgoing_message(message, second, 1_700_000_000_200)
            .unwrap());

        assert_eq!(
            store
                .outbound_envelopes_after(b"chat-a".to_vec(), b"alice".to_vec(), 0)
                .unwrap(),
            vec![first],
        );
    }

    #[test]
    fn insert_outgoing_message_can_backfill_queue_for_existing_message() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        store.insert_message(message.clone()).unwrap();

        assert!(store
            .insert_outgoing_message(
                message.clone(),
                outbound_for(&message, b"bob", b"msg-000000000001"),
                1_700_000_000_100,
            )
            .unwrap());
        assert_eq!(store.messages_for_chat(b"chat-a".to_vec()).unwrap(), vec![message]);
        assert_eq!(
            store
                .outbound_envelopes_after(b"chat-a".to_vec(), b"alice".to_vec(), 0)
                .unwrap()
                .len(),
            1,
        );
    }

    #[test]
    fn outbound_envelopes_after_includes_non_text_authored_kinds() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut friend_request = msg(b"chat-a", b"alice", 1, "{\"name\":\"Alice\"}");
        friend_request.kind = 3;
        let outbound = outbound_for(&friend_request, b"bob", b"msg-000000000003");

        assert!(store
            .insert_outgoing_message(friend_request.clone(), outbound.clone(), 1_700_000_000_100)
            .unwrap());

        assert_eq!(
            store
                .outbound_envelopes_after(b"chat-a".to_vec(), b"alice".to_vec(), 0)
                .unwrap(),
            vec![outbound],
        );
    }

    #[test]
    fn pending_relay_outbound_envelopes_skip_posted_and_expired_rows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let live = msg(b"chat-a", b"alice", 1, "live");
        let stale = msg(b"chat-a", b"alice", 2, "stale");
        let posted = msg(b"chat-a", b"alice", 3, "posted");

        let mut live_env = outbound_for(&live, b"bob", b"msg-000000000001");
        live_env.expiry = 10_000;
        let mut stale_env = outbound_for(&stale, b"bob", b"msg-000000000002");
        stale_env.expiry = 1_999;
        let mut posted_env = outbound_for(&posted, b"bob", b"msg-000000000003");
        posted_env.expiry = 10_000;

        store.insert_outgoing_message(live, live_env.clone(), 1_000).unwrap();
        store.insert_outgoing_message(stale, stale_env, 1_100).unwrap();
        store.insert_outgoing_message(posted, posted_env.clone(), 1_200).unwrap();
        assert!(store
            .mark_outbound_envelope_relay_posted(posted_env.msg_id.clone(), 1_500)
            .unwrap());

        assert_eq!(
            store.pending_relay_outbound_envelopes(10, 2_000).unwrap(),
            vec![live_env],
        );
    }

    #[test]
    fn outgoing_receipt_envelope_round_trips_and_is_queryable_by_watermark_key() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let envelope = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            5,
            b"receipt-00000001",
        );

        assert!(store
            .upsert_outgoing_receipt_envelope(envelope.clone(), 1_000)
            .unwrap());
        assert_eq!(
            store
                .outgoing_receipt_envelope(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED,
                )
                .unwrap(),
            Some(envelope),
        );
    }

    #[test]
    fn outgoing_receipt_envelope_same_watermark_preserves_stable_msg_id() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let first = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            5,
            b"receipt-00000001",
        );
        let second = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            5,
            b"receipt-00000002",
        );

        assert!(store
            .upsert_outgoing_receipt_envelope(first.clone(), 1_000)
            .unwrap());
        assert!(!store
            .upsert_outgoing_receipt_envelope(second, 2_000)
            .unwrap());
        assert_eq!(
            store
                .outgoing_receipt_envelope(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED,
                )
                .unwrap(),
            Some(first),
        );
    }

    #[test]
    fn outgoing_receipt_envelope_higher_watermark_replaces_row_and_requeues_for_relay() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let first = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            5,
            b"receipt-00000001",
        );
        let second = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            7,
            b"receipt-00000002",
        );

        assert!(store
            .upsert_outgoing_receipt_envelope(first.clone(), 1_000)
            .unwrap());
        assert!(store
            .mark_outgoing_receipt_envelope_relay_posted(first.msg_id.clone(), 1_500)
            .unwrap());
        assert!(store
            .upsert_outgoing_receipt_envelope(second.clone(), 2_000)
            .unwrap());

        assert_eq!(
            store.pending_relay_outgoing_receipt_envelopes(10, 3_000).unwrap(),
            vec![second.clone()],
        );
        assert_eq!(
            store
                .outgoing_receipt_envelope(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED,
                )
                .unwrap(),
            Some(second),
        );
    }

    #[test]
    fn pending_relay_outgoing_receipt_envelopes_skip_posted_and_expired_rows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let live = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            5,
            b"receipt-00000001",
        );
        let mut expired = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_READ,
            4,
            b"receipt-00000002",
        );
        expired.expiry = 1_999;
        let posted = outgoing_receipt_for(
            b"chat-b",
            b"bob",
            b"bob",
            crate::RECEIPT_TYPE_DELIVERED,
            9,
            b"receipt-00000003",
        );

        store
            .upsert_outgoing_receipt_envelope(live.clone(), 1_000)
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(expired, 1_100)
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(posted.clone(), 1_200)
            .unwrap();
        assert!(store
            .mark_outgoing_receipt_envelope_relay_posted(posted.msg_id.clone(), 1_500)
            .unwrap());

        assert_eq!(
            store.pending_relay_outgoing_receipt_envelopes(10, 2_000).unwrap(),
            vec![live],
        );
    }

    #[test]
    fn family_carried_envelopes_return_only_unexpired_family_rows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.enqueue_carried_envelope(carried(b"fam", b"h1", 9_000, 10), true, 1_000, BIG_BUDGET).unwrap();
        store.enqueue_carried_envelope(carried(b"foreign", b"h2", 9_000, 10), false, 2_000, BIG_BUDGET).unwrap();
        store.enqueue_carried_envelope(carried(b"expired", b"h3", 1_500, 10), true, 3_000, BIG_BUDGET).unwrap();

        let rows = store.family_carried_envelopes(10, 2_000).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].msg_id, b"fam".to_vec());
    }

    fn contact(user_id: &[u8], name: &str) -> Contact {
        Contact {
            user_id: user_id.to_vec(),
            name: name.to_string(),
            sign_pk: vec![1u8; 32],
            agree_pk: vec![2u8; 32],
            relay_url: None,
            relay_token: None,
        }
    }

    #[test]
    fn open_migrates_an_old_contacts_table_to_add_relay_token() {
        let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("cruisemesh-store-migration-{unique}.sqlite"));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE contacts (
                user_id   BLOB PRIMARY KEY,
                name      TEXT NOT NULL,
                sign_pk   BLOB NOT NULL,
                agree_pk  BLOB NOT NULL,
                relay_url TEXT
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO contacts (user_id, name, sign_pk, agree_pk, relay_url)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![b"alice-id".to_vec(), "Alice", vec![1u8; 32], vec![2u8; 32], "https://relay.example"],
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str.clone()).unwrap();
        let migrated = store.get_contact(b"alice-id".to_vec()).unwrap().unwrap();
        assert_eq!(migrated.relay_url, Some("https://relay.example".to_string()));
        assert_eq!(migrated.relay_token, None);

        let mut updated = migrated.clone();
        updated.relay_token = Some("family-token".to_string());
        store.upsert_contact(updated.clone()).unwrap();
        assert_eq!(store.get_contact(b"alice-id".to_vec()).unwrap(), Some(updated));

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn upsert_then_get_contact_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        let found = store.get_contact(b"alice-id".to_vec()).unwrap().expect("contact exists");
        assert_eq!(found.name, "Alice");
        assert_eq!(found.sign_pk, vec![1u8; 32]);
    }

    #[test]
    fn get_contact_returns_none_when_absent() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(store.get_contact(b"nobody".to_vec()).unwrap(), None);
    }

    #[test]
    fn upsert_replaces_rather_than_duplicates() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice R.")).unwrap();

        let contacts = store.list_contacts().unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].name, "Alice R.");
    }

    #[test]
    fn list_contacts_is_alphabetical() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"bob-id", "Bob")).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        let names: Vec<String> = store.list_contacts().unwrap().into_iter().map(|c| c.name).collect();
        assert_eq!(names, vec!["Alice".to_string(), "Bob".to_string()]);
    }

    #[test]
    fn delete_contact_removes_the_contact() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        assert!(store.delete_contact(b"alice-id".to_vec()).unwrap());
        assert_eq!(store.get_contact(b"alice-id".to_vec()).unwrap(), None);
        assert!(store.list_contacts().unwrap().is_empty());
    }

    #[test]
    fn delete_contact_is_a_noop_for_unknown_contact() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(!store.delete_contact(b"nobody".to_vec()).unwrap());
        // Deleting twice is idempotent, not an error.
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        assert!(store.delete_contact(b"alice-id".to_vec()).unwrap());
        assert!(!store.delete_contact(b"alice-id".to_vec()).unwrap());
    }

    #[test]
    fn delete_contact_deletes_the_1to1_chat_messages_and_receipts() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        // 1:1 chat_id = the peer's UserID (DESIGN.md §7.1): both directions live under it.
        store.insert_message(msg(b"alice-id", b"alice-id", 1, "from alice")).unwrap();
        store.insert_message(msg(b"alice-id", b"me", 1, "from me")).unwrap();
        store
            .record_receipt(b"alice-id".to_vec(), b"me".to_vec(), crate::RECEIPT_TYPE_DELIVERED, 1)
            .unwrap();
        store
            .record_outgoing_receipt(
                b"alice-id".to_vec(),
                b"alice-id".to_vec(),
                crate::RECEIPT_TYPE_READ,
                1,
            )
            .unwrap();

        assert!(store.delete_contact(b"alice-id".to_vec()).unwrap());

        assert!(store.messages_for_chat(b"alice-id".to_vec()).unwrap().is_empty());
        assert_eq!(
            store
                .receipt_through(b"alice-id".to_vec(), b"me".to_vec(), crate::RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"alice-id".to_vec(),
                    b"alice-id".to_vec(),
                    crate::RECEIPT_TYPE_READ,
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn delete_contact_leaves_other_contacts_and_chats_alone() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        store.upsert_contact(contact(b"bob-id", "Bob")).unwrap();
        store.insert_message(msg(b"alice-id", b"alice-id", 1, "hi")).unwrap();
        store.insert_message(msg(b"bob-id", b"bob-id", 1, "yo")).unwrap();
        // A group chat where alice posted: her group messages belong to the
        // group's chat_id, not to her contact, and must survive.
        store.insert_message(msg(b"group-1", b"alice-id", 1, "group msg")).unwrap();

        assert!(store.delete_contact(b"alice-id".to_vec()).unwrap());

        assert_eq!(store.list_contacts().unwrap().len(), 1);
        assert_eq!(store.messages_for_chat(b"bob-id".to_vec()).unwrap().len(), 1);
        assert_eq!(store.messages_for_chat(b"group-1".to_vec()).unwrap().len(), 1);
        assert!(store.messages_for_chat(b"alice-id".to_vec()).unwrap().is_empty());
    }

    // --- receipts (DESIGN.md §7.2) -----------------------------------------

    #[test]
    fn receipt_through_is_zero_when_none_recorded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let through = store
            .receipt_through(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED)
            .unwrap();
        assert_eq!(through, 0);
    }

    #[test]
    fn record_receipt_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED, 5)
            .unwrap();

        let through = store
            .receipt_through(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED)
            .unwrap();
        assert_eq!(through, 5);
    }

    #[test]
    fn record_receipt_is_monotonic_upward() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED, 5)
            .unwrap();
        store
            .record_receipt(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED, 9)
            .unwrap();

        let through = store
            .receipt_through(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED)
            .unwrap();
        assert_eq!(through, 9);
    }

    #[test]
    fn record_receipt_never_regresses_on_a_lower_or_replayed_value() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED, 9)
            .unwrap();
        // A stale/replayed receipt (lower, or the same, value) must not undo progress.
        store
            .record_receipt(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED, 3)
            .unwrap();
        store
            .record_receipt(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED, 9)
            .unwrap();

        let through = store
            .receipt_through(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED)
            .unwrap();
        assert_eq!(through, 9);
    }

    #[test]
    fn receipt_types_are_independent() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED, 9)
            .unwrap();
        store
            .record_receipt(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_READ, 4)
            .unwrap();

        assert_eq!(
            store
                .receipt_through(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            9
        );
        assert_eq!(
            store
                .receipt_through(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_READ)
                .unwrap(),
            4
        );
    }

    #[test]
    fn receipts_are_independent_per_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED, 9)
            .unwrap();
        store
            .record_receipt(b"chat-b".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED, 2)
            .unwrap();

        assert_eq!(
            store
                .receipt_through(b"chat-a".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            9
        );
        assert_eq!(
            store
                .receipt_through(b"chat-b".to_vec(), b"alice".to_vec(), crate::RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            2
        );
    }

    // --- outgoing receipts (DESIGN.md §7.2, §7.3) -------------------------

    #[test]
    fn outgoing_receipt_through_is_zero_when_none_recorded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let through = store
            .outgoing_receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
            )
            .unwrap();
        assert_eq!(through, 0);
    }

    #[test]
    fn record_outgoing_receipt_round_trips_and_never_regresses() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_outgoing_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
                5,
            )
            .unwrap();
        store
            .record_outgoing_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
                3,
            )
            .unwrap();

        let through = store
            .outgoing_receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
            )
            .unwrap();
        assert_eq!(through, 5);
    }

    #[test]
    fn outgoing_receipt_types_are_independent() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_outgoing_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
            )
            .unwrap();
        store
            .record_outgoing_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
                4,
            )
            .unwrap();

        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED,
                )
                .unwrap(),
            9
        );
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_READ,
                )
                .unwrap(),
            4
        );
    }

    // --- sync digests (DESIGN.md §7.3) -------------------------------------

    #[test]
    fn chat_digest_is_empty_for_unknown_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(store.chat_digest(b"chat-a".to_vec()).unwrap(), Vec::new());
    }

    #[test]
    fn chat_digest_has_one_entry_per_sender_with_their_contiguous_lamport() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 1, "one")).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 2, "two")).unwrap();
        // A gap: lamport 3 missing for alice.
        store.insert_message(msg(b"chat-a", b"alice", 4, "four")).unwrap();
        store.insert_message(msg(b"chat-a", b"bob", 1, "hey")).unwrap();

        let digest = store.chat_digest(b"chat-a".to_vec()).unwrap();
        assert_eq!(
            digest,
            vec![
                DigestEntry { sender_user_id: b"alice".to_vec(), through_lamport: 2 },
                DigestEntry { sender_user_id: b"bob".to_vec(), through_lamport: 1 },
            ]
        );
    }

    #[test]
    fn messages_after_returns_only_newer_messages_ascending() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 1, "one")).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 2, "two")).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 3, "three")).unwrap();

        let missing = store.messages_after(b"chat-a".to_vec(), b"alice".to_vec(), 1).unwrap();
        let payloads: Vec<Vec<u8>> = missing.into_iter().map(|m| m.payload).collect();
        assert_eq!(payloads, vec![b"two".to_vec(), b"three".to_vec()]);
    }

    #[test]
    fn messages_after_zero_returns_everything() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 1, "one")).unwrap();
        store.insert_message(msg(b"chat-a", b"alice", 2, "two")).unwrap();

        let missing = store.messages_after(b"chat-a".to_vec(), b"alice".to_vec(), 0).unwrap();
        assert_eq!(missing.len(), 2);
    }

    /// The composition §7.3 sync relies on: A has messages B lacks; feeding
    /// B's digest into A's `messages_after` per sender yields exactly what B
    /// is missing, no more and no less.
    #[test]
    fn chat_digest_and_messages_after_compose_to_find_exactly_the_gap() {
        let store_a = MessageStore::open(":memory:".to_string()).unwrap();
        let store_b = MessageStore::open(":memory:".to_string()).unwrap();

        for lamport in 1..=5u64 {
            let m = msg(b"chat-a", b"alice", lamport, &format!("msg-{lamport}"));
            store_a.insert_message(m).unwrap();
        }
        // B only has the first two.
        store_b.insert_message(msg(b"chat-a", b"alice", 1, "msg-1")).unwrap();
        store_b.insert_message(msg(b"chat-a", b"alice", 2, "msg-2")).unwrap();

        let b_digest = store_b.chat_digest(b"chat-a".to_vec()).unwrap();
        assert_eq!(b_digest, vec![DigestEntry { sender_user_id: b"alice".to_vec(), through_lamport: 2 }]);

        let mut all_missing = Vec::new();
        for entry in &b_digest {
            let missing = store_a
                .messages_after(b"chat-a".to_vec(), entry.sender_user_id.clone(), entry.through_lamport)
                .unwrap();
            all_missing.extend(missing);
        }

        let payloads: Vec<Vec<u8>> = all_missing.into_iter().map(|m| m.payload).collect();
        assert_eq!(
            payloads,
            vec![b"msg-3".to_vec(), b"msg-4".to_vec(), b"msg-5".to_vec()]
        );
    }

    // --- carry queue (DESIGN.md §5.3) --------------------------------------

    const BIG_BUDGET: i64 = 5 * 1024 * 1024;

    fn carried(msg_id: &[u8], hint: &[u8], expiry: i64, sealed_len: usize) -> CarriedEnvelope {
        CarriedEnvelope {
            msg_id: msg_id.to_vec(),
            hop_ttl: 7,
            expiry,
            recipient_hint: hint.to_vec(),
            sealed: vec![0xAB; sealed_len],
        }
    }

    #[test]
    fn enqueue_then_fetch_by_hint_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let env = carried(b"m1", b"hint-a", 2_000, 100);
        assert!(store.enqueue_carried_envelope(env.clone(), false, 1_000, BIG_BUDGET).unwrap());

        let found = store.carried_envelopes_for_hints(vec![b"hint-a".to_vec()], 1_500).unwrap();
        assert_eq!(found, vec![env]);
    }

    #[test]
    fn enqueue_is_idempotent_on_msg_id() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(store.enqueue_carried_envelope(carried(b"m1", b"h", 2_000, 100), false, 1_000, BIG_BUDGET).unwrap());
        // Same msg_id, re-received under DTN: no-op, not a duplicate row.
        assert!(!store.enqueue_carried_envelope(carried(b"m1", b"h", 2_000, 100), false, 1_050, BIG_BUDGET).unwrap());
        assert_eq!(store.carried_len().unwrap(), 1);
    }

    #[test]
    fn fetch_by_hint_ignores_nonmatching_and_expired() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.enqueue_carried_envelope(carried(b"m1", b"hint-a", 2_000, 10), false, 1_000, BIG_BUDGET).unwrap();
        store.enqueue_carried_envelope(carried(b"m2", b"hint-b", 2_000, 10), false, 1_000, BIG_BUDGET).unwrap();
        store.enqueue_carried_envelope(carried(b"m3", b"hint-a", 1_200, 10), false, 1_000, BIG_BUDGET).unwrap();

        // now_ms = 1_500: m3 (expiry 1_200) is expired; m2 has the wrong hint.
        let found = store.carried_envelopes_for_hints(vec![b"hint-a".to_vec()], 1_500).unwrap();
        let ids: Vec<Vec<u8>> = found.into_iter().map(|e| e.msg_id).collect();
        assert_eq!(ids, vec![b"m1".to_vec()]);
    }

    #[test]
    fn fetch_matches_any_of_several_hints() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.enqueue_carried_envelope(carried(b"m1", b"day-a", 9_000, 10), false, 1_000, BIG_BUDGET).unwrap();
        store.enqueue_carried_envelope(carried(b"m2", b"day-b", 9_000, 10), false, 1_100, BIG_BUDGET).unwrap();
        store.enqueue_carried_envelope(carried(b"m3", b"day-c", 9_000, 10), false, 1_200, BIG_BUDGET).unwrap();

        // A peer's recent-day hints cover day-a and day-c but not day-b.
        let found = store
            .carried_envelopes_for_hints(vec![b"day-a".to_vec(), b"day-c".to_vec()], 5_000)
            .unwrap();
        let ids: Vec<Vec<u8>> = found.into_iter().map(|e| e.msg_id).collect();
        assert_eq!(ids, vec![b"m1".to_vec(), b"m3".to_vec()]); // oldest received_at first
    }

    #[test]
    fn carried_msg_ids_are_returned_oldest_first_and_limited() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.enqueue_carried_envelope(carried(b"m1", b"h", 9_000, 10), false, 1_000, BIG_BUDGET).unwrap();
        store.enqueue_carried_envelope(carried(b"m2", b"h", 9_000, 10), false, 2_000, BIG_BUDGET).unwrap();
        store.enqueue_carried_envelope(carried(b"m3", b"h", 9_000, 10), false, 3_000, BIG_BUDGET).unwrap();

        let ids = store.carried_msg_ids(2).unwrap();
        assert_eq!(ids, vec![b"m1".to_vec(), b"m2".to_vec()]);
    }

    #[test]
    fn peer_sync_candidates_exclude_the_peers_known_ids_and_targeted_delivery() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.enqueue_carried_envelope(carried(b"known", b"day-a", 9_000, 10), false, 1_000, BIG_BUDGET).unwrap();
        store.enqueue_carried_envelope(carried(b"for-peer", b"day-b", 9_000, 10), false, 2_000, BIG_BUDGET).unwrap();
        store.enqueue_carried_envelope(carried(b"spray", b"day-c", 9_000, 10), false, 3_000, BIG_BUDGET).unwrap();

        let found = store
            .carried_envelopes_for_peer_sync(
                vec![b"day-b".to_vec()],
                vec![b"known".to_vec()],
                5_000,
            )
            .unwrap();
        let ids: Vec<Vec<u8>> = found.into_iter().map(|e| e.msg_id).collect();
        assert_eq!(ids, vec![b"spray".to_vec()]);
    }

    #[test]
    fn remove_carried_deletes_it() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.enqueue_carried_envelope(carried(b"m1", b"h", 2_000, 10), false, 1_000, BIG_BUDGET).unwrap();
        assert!(store.remove_carried_envelope(b"m1".to_vec()).unwrap());
        assert!(!store.remove_carried_envelope(b"m1".to_vec()).unwrap()); // gone, idempotent
        assert_eq!(store.carried_len().unwrap(), 0);
    }

    #[test]
    fn prune_expired_carried_drops_only_the_expired() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.enqueue_carried_envelope(carried(b"live", b"h", 5_000, 10), false, 1_000, BIG_BUDGET).unwrap();
        store.enqueue_carried_envelope(carried(b"dead", b"h", 1_500, 10), false, 1_000, BIG_BUDGET).unwrap();

        assert_eq!(store.prune_expired_carried(2_000).unwrap(), 1);
        assert_eq!(store.carried_len().unwrap(), 1);
        let found = store.carried_envelopes_for_hints(vec![b"h".to_vec()], 2_000).unwrap();
        assert_eq!(found[0].msg_id, b"live");
    }

    #[test]
    fn foreign_budget_evicts_oldest_foreign_first() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // Budget of 250 bytes; three 100-byte foreign envelopes can't all fit.
        store.enqueue_carried_envelope(carried(b"f1", b"h", 9_000, 100), false, 1_000, 250).unwrap();
        store.enqueue_carried_envelope(carried(b"f2", b"h", 9_000, 100), false, 2_000, 250).unwrap();
        // Third insert pushes total to 300 > 250, evicting the oldest (f1).
        store.enqueue_carried_envelope(carried(b"f3", b"h", 9_000, 100), false, 3_000, 250).unwrap();

        let ids: Vec<Vec<u8>> = store
            .carried_envelopes_for_hints(vec![b"h".to_vec()], 5_000)
            .unwrap()
            .into_iter()
            .map(|e| e.msg_id)
            .collect();
        assert_eq!(ids, vec![b"f2".to_vec(), b"f3".to_vec()]);
    }

    #[test]
    fn family_envelopes_are_never_evicted_for_budget() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // A family envelope (is_family = true) far exceeding the budget stays.
        store.enqueue_carried_envelope(carried(b"fam", b"h", 9_000, 400), true, 1_000, 250).unwrap();
        // Foreign envelopes still get budget-capped independently...
        store.enqueue_carried_envelope(carried(b"f1", b"h", 9_000, 100), false, 2_000, 250).unwrap();
        store.enqueue_carried_envelope(carried(b"f2", b"h", 9_000, 100), false, 3_000, 250).unwrap();
        store.enqueue_carried_envelope(carried(b"f3", b"h", 9_000, 100), false, 4_000, 250).unwrap();

        let ids: Vec<Vec<u8>> = store
            .carried_envelopes_for_hints(vec![b"h".to_vec()], 5_000)
            .unwrap()
            .into_iter()
            .map(|e| e.msg_id)
            .collect();
        // fam survives despite being 400 bytes (> budget); foreign kept to f2,f3.
        assert_eq!(ids, vec![b"fam".to_vec(), b"f2".to_vec(), b"f3".to_vec()]);
    }
}
