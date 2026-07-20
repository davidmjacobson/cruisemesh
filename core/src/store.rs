//! Message + contact store: SQLite-backed persistence (DESIGN.md §7.1,
//! §10). `insert_message` is idempotent on (chat_id, sender_user_id,
//! lamport): re-delivering the same envelope (expected under DTN) is a
//! no-op. A conflict whose (timestamp, kind, payload) *don't* match is
//! treated as the sender having forked/reset their stream rather than a
//! duplicate -- see [MessageStore::insert_message]'s doc comment for the
//! recovery this triggers. Per-chat lamport counters are maintained
//! independently by each sender (DESIGN.md §7.1), so gap detection in
//! [MessageStore::highest_contiguous_lamport] is keyed on (chat_id,
//! sender_user_id), not chat_id alone.
//!
//! Contacts (DESIGN.md §6.2) live in the same store/connection rather than a
//! separate file: they're the other half of "who can I seal a message to,"
//! which is exactly the data a message store needs alongside messages
//! themselves.
//!
//! Groups (DESIGN.md §6.5) live here too: one `groups` row for the stable
//! id/name/key tuple, plus `group_members` rows for the current membership.
//! Group chat history reuses the existing `messages` table with `chat_id =
//! group_id`; the existing `(chat_id, sender_user_id, lamport)` streams were
//! already designed for multi-sender chats.
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
//! identity `(chat_id, sender_user_id, kind, lamport, recipient_user_id)` so
//! reconnect retries reuse the same `msg_id` and ciphertext instead of
//! re-sealing a fresh envelope every time. The recipient now participates in
//! the dedupe key because `kind=4` group invites are one logical chat event
//! fanned out as several pairwise-sealed envelopes, one per member.
//! `insert_outgoing_message` writes the plaintext message row and one queued
//! envelope in one transaction so local history and sync state never diverge
//! on a crash boundary.
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
//!
//! The advertised msg-id list is also how a mule learns it can safely drop a
//! carried 1:1 envelope (DTN_TODOS.md §3.2, D2 mule-drain-confirm): the true
//! recipient doesn't carry a message it opens, it *consumes* it, so
//! [`MessageStore::recent_consumed_msg_ids`] feeds the same advertised list
//! alongside [`MessageStore::carried_msg_ids`] (see
//! `engine.rs::core_digest_advertised_msg_ids`) -- otherwise a message we
//! successfully received would never show up in what we tell a mule we
//! already have, and the mule would keep it until expiry. The other half of
//! D2 -- actually removing a carried envelope once a peer's digest proves
//! they have it -- is `engine.rs::core_confirm_carried_deliveries`.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::collections::HashSet;
use std::sync::Mutex;

use crate::groups::{canonicalize_members, validate_group};
use crate::{
    verify_introduction_ticket, CoreError, FriendDirectoryContent, Group, IntroductionTicket,
    SuggestedFriendCard, KIND_INTRODUCED_FRIEND_REQUEST,
};

const MESSAGE_ID_LEN: usize = 16;
const CARRIED_CONTENT_DIGEST_LEN: usize = 32;
const DEFAULT_TOTAL_CARRY_BUDGET_BYTES: i64 = 64 * 1024 * 1024;

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

/// Local-only diagnostics for how an incoming message reached this device.
/// `transport`: 0 = BLE direct, 1 = BLE through another device, 2 = relay,
/// 3 = same-LAN direct, 4 = same-LAN through another device.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct MessageArrival {
    pub transport: u8,
    pub hops_taken: u8,
    pub received_at: i64,
}

/// Stable envelope identity for a stored message and, for replies, the
/// encrypted id of the quoted message. Legacy rows may have no reference.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct MessageReference {
    pub msg_id: Vec<u8>,
    pub reply_to_msg_id: Option<Vec<u8>>,
}

/// Where a stored message row lives (`chat_id`) and who authored it
/// (`sender_user_id`), keyed by stable envelope `msg_id` -- see
/// [`MessageStore::message_origin_by_msg_id`]. Both fields are needed by the
/// relay ack-decision path: the local storage convention makes their
/// comparison meaningful (a 1:1 incoming row has `chat_id ==
/// sender_user_id`; a group row has `chat_id = group id`, which never equals
/// a member's user id).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct MessageOrigin {
    pub chat_id: Vec<u8>,
    pub sender_user_id: Vec<u8>,
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
    /// A local-only nickname the user set for this contact (T16). Presentation
    /// only: it is NEVER written to a `FriendCard`, digest, or any wire format,
    /// and importing a friend card never overwrites it. `None`/blank means fall
    /// back to `name`. Defaulted so existing constructors need not pass it.
    #[uniffi(default = None)]
    pub nickname: Option<String>,
}

/// The name to show for a contact: the local nickname when the user has set a
/// non-blank one (T16), otherwise the card `name`. Kept in core so both shells
/// resolve identically everywhere a contact name is displayed.
#[uniffi::export]
pub fn core_contact_display_name(contact: Contact) -> String {
    match contact.nickname.as_deref().map(str::trim) {
        Some(nickname) if !nickname.is_empty() => nickname.to_string(),
        _ => contact.name.clone(),
    }
}

/// The last authenticated friends-of-friends policy advertised by a contact.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ContactDiscoveryPolicy {
    pub user_id: Vec<u8>,
    pub protocol_version: u8,
    pub enabled: bool,
    pub revision: u64,
}

/// One candidate/source pair. Callers group rows with the same candidate
/// UserID to present all known mutual friends.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FriendSuggestion {
    pub candidate: SuggestedFriendCard,
    pub introducer_user_id: Vec<u8>,
    pub ticket: IntroductionTicket,
    /// 0 = available, 1 = requested, 2 = hidden.
    pub state: u8,
}

/// How an accepted contact first entered the local trust graph.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ContactProvenance {
    pub user_id: Vec<u8>,
    /// 0 = direct QR/link, 1 = introduced by another accepted contact.
    pub source: u8,
    pub introducer_user_id: Option<Vec<u8>>,
    pub introduced_at_ms: i64,
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
    pub(crate) conn: Mutex<Connection>,
}

#[uniffi::export]
impl MessageStore {
    /// Open (creating if needed) the message store at `path`. Pass
    /// `":memory:"` for an ephemeral in-process store.
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Self, CoreError> {
        let mut conn = Connection::open(&path).map_err(store_err)?;
        conn.execute_batch(SCHEMA).map_err(store_err)?;
        migrate_delivery_metrics_schema(&conn)?;
        ensure_contact_column(&conn, "relay_token", "TEXT")?;
        ensure_contact_column(&conn, "avatar", "BLOB")?;
        ensure_contact_column(&conn, "avatar_epoch", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_contact_column(&conn, "nickname", "TEXT")?;
        // Relay proxy-polling (see enqueue_relay_carried_envelope): marks a
        // carried envelope as one we pulled FROM the relay rather than one we
        // received over BLE, so the relay-upload query can skip re-uploading
        // it. Older on-disk stores predate the column.
        ensure_column(
            &conn,
            "carried_envelopes",
            "from_relay",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "carried_envelopes", "content_digest", "BLOB")?;
        migrate_carried_content_digests(&mut conn)?;
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_carried_content_digest
             ON carried_envelopes(content_digest)",
            [],
        )
        .map_err(store_err)?;
        ensure_column(&conn, "messages", "arrival_transport", "INTEGER")?;
        ensure_column(&conn, "receipts", "via_transport", "INTEGER")?;
        ensure_column(&conn, "messages", "hops_taken", "INTEGER")?;
        ensure_column(&conn, "messages", "received_at", "INTEGER")?;
        ensure_column(&conn, "messages", "msg_id", "BLOB")?;
        ensure_column(&conn, "messages", "reply_to_msg_id", "BLOB")?;
        ensure_column(&conn, "messages", "outbound_expiry", "INTEGER")?;
        ensure_column(
            &conn,
            "groups",
            "metadata_revision",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "groups",
            "metadata_changed_by",
            "BLOB NOT NULL DEFAULT X''",
        )?;
        // Older stores already have stable ids for locally authored rows in
        // the outbound queue. Backfill those so they can be quoted after an
        // upgrade; received legacy rows cannot be recovered retroactively.
        conn.execute(
            "UPDATE messages
             SET msg_id = (
                 SELECT outbound_envelopes.msg_id
                 FROM outbound_envelopes
                 WHERE outbound_envelopes.chat_id = messages.chat_id
                   AND outbound_envelopes.sender_user_id = messages.sender_user_id
                   AND outbound_envelopes.lamport = messages.lamport
                 ORDER BY outbound_envelopes.queued_at ASC
                 LIMIT 1
             )
             WHERE msg_id IS NULL
               AND EXISTS (
                   SELECT 1 FROM outbound_envelopes
                   WHERE outbound_envelopes.chat_id = messages.chat_id
                     AND outbound_envelopes.sender_user_id = messages.sender_user_id
                     AND outbound_envelopes.lamport = messages.lamport
               )",
            [],
        )
        .map_err(store_err)?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_chat_msg_id
             ON messages(chat_id, msg_id)",
            [],
        )
        .map_err(store_err)?;
        // Unlike the composite index above, `message_origin_by_msg_id` looks
        // a `msg_id` up with no `chat_id` in hand (a relay-fetched envelope
        // knows only its own `msg_id`), so it needs `msg_id` leading an index
        // on its own to avoid a full table scan.
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_msg_id ON messages(msg_id)",
            [],
        )
        .map_err(store_err)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Writes a transactionally consistent standalone SQLite snapshot.
    /// The destination must not already exist; callers should use a unique
    /// temporary path and remove it after reading the backup bytes.
    pub fn backup_to(&self, destination: String) -> Result<(), CoreError> {
        let destination = std::path::Path::new(destination.trim());
        if !destination.is_absolute() {
            return Err(CoreError::Store(
                "backup destination must be an absolute path".into(),
            ));
        }
        match std::fs::symlink_metadata(destination) {
            Ok(_) => return Err(CoreError::Store("backup destination already exists".into())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CoreError::Store(format!(
                    "cannot inspect backup destination: {error}"
                )))
            }
        }
        if !destination.parent().is_some_and(std::path::Path::is_dir) {
            return Err(CoreError::Store(
                "backup destination parent is not a directory".into(),
            ));
        }
        let conn = self
            .conn
            .lock()
            .map_err(|_| CoreError::Store("store lock is unavailable".into()))?;
        conn.execute("VACUUM INTO ?1", params![destination.to_string_lossy()])
            .map_err(store_err)?;
        Ok(())
    }

    /// Insert a message from a remote sender's stream, distinguishing a true
    /// duplicate from a *forked* stream instead of silently dropping both the
    /// same way.
    ///
    /// A conflict on `(chat_id, sender_user_id, lamport)` is ambiguous on its
    /// own: it could be a digest resend or relay copy of a message we already
    /// have (same sealed content, arriving twice -- expected under DTN and
    /// harmless to ignore), or it could be a sender who reset their stream
    /// (e.g. deleted the chat and re-added the contact, per DESIGN.md §7.1's
    /// "own outgoing stream has no gaps" -- deleting local history restarts
    /// their lamport counter at 1) and is now re-using a lamport number we
    /// already hold from their *old* stream for a genuinely new message. This
    /// method tells the two apart by comparing the existing row's
    /// `(timestamp, kind, payload)` against the incoming one:
    ///
    /// - **Identical** on all three -> true duplicate. No-op, returns `Ok(false)`,
    ///   same behavior as the old plain `INSERT OR IGNORE`.
    /// - **Different** -> a fork. The sender's old stream at and above this
    ///   lamport is abandoned, so we drop our stale copy of that tail and
    ///   insert the new message in its place. We also clear
    ///   `outgoing_receipts` / `outgoing_receipt_envelopes` for this
    ///   `(chat_id, sender_user_id)`: those are *our* "delivered/read through
    ///   N" watermarks about *their* stream, and they were computed against
    ///   the abandoned history -- left in place, they'd keep telling the
    ///   sender (who now has no history past their reset) that we've already
    ///   read messages they haven't sent yet, which is exactly the false ✓✓
    ///   this recovery exists to stop. `receipts` (the peer's acks of *our*
    ///   stream) is untouched -- unrelated to their stream resetting. All of
    ///   this runs in one transaction so a crash mid-recovery can't leave the
    ///   stale tail and the new message coexisting.
    pub fn insert_message(&self, message: StoredMessage) -> Result<bool, CoreError> {
        incoming_message_reference::insert(self, message, None, None)
    }

    /// Insert an opened incoming message together with the envelope id used
    /// for quoting it and an optional encrypted reply target.
    pub fn insert_incoming_message(
        &self,
        message: StoredMessage,
        msg_id: Vec<u8>,
        reply_to_msg_id: Option<Vec<u8>>,
    ) -> Result<bool, CoreError> {
        validate_msg_id("msg_id", &msg_id)?;
        if let Some(reply_to_msg_id) = reply_to_msg_id.as_deref() {
            validate_msg_id("reply_to_msg_id", reply_to_msg_id)?;
        }
        incoming_message_reference::insert(self, message, Some(msg_id), reply_to_msg_id)
    }
}

mod incoming_message_reference {
    use super::*;

    pub(super) fn insert(
        store: &MessageStore,
        message: StoredMessage,
        msg_id: Option<Vec<u8>>,
        reply_to_msg_id: Option<Vec<u8>>,
    ) -> Result<bool, CoreError> {
        validate_stored_message(&message)?;
        let mut conn = store.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;

        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO messages
                    (chat_id, sender_user_id, lamport, timestamp, kind, payload,
                     msg_id, reply_to_msg_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    message.chat_id,
                    message.sender_user_id,
                    message.lamport as i64,
                    message.timestamp,
                    message.kind as i64,
                    message.payload,
                    msg_id,
                    reply_to_msg_id,
                ],
            )
            .map_err(store_err)?
            > 0;

        if inserted {
            tx.commit().map_err(store_err)?;
            return Ok(true);
        }

        // Conflict: a row already exists at this (chat_id, sender_user_id,
        // lamport). Figure out whether it's the same message or a fork.
        let existing: Option<(i64, i64, Vec<u8>, Option<Vec<u8>>)> = tx
            .query_row(
                "SELECT timestamp, kind, payload, reply_to_msg_id FROM messages
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3",
                params![
                    message.chat_id,
                    message.sender_user_id,
                    message.lamport as i64
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(store_err)?;

        let is_true_duplicate = match &existing {
            Some((timestamp, kind, payload, existing_reply_to_msg_id)) => {
                *timestamp == message.timestamp
                    && *kind == message.kind as i64
                    && *payload == message.payload
                    && *existing_reply_to_msg_id == reply_to_msg_id
            }
            // Shouldn't happen (we just failed to insert on a conflict), but
            // if the row vanished under us, treat it as nothing to recover.
            None => {
                tx.commit().map_err(store_err)?;
                return Ok(false);
            }
        };

        if is_true_duplicate {
            if msg_id.is_some() {
                tx.execute(
                    "UPDATE messages SET msg_id = COALESCE(msg_id, ?4)
                     WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3",
                    params![
                        message.chat_id,
                        message.sender_user_id,
                        message.lamport as i64,
                        msg_id,
                    ],
                )
                .map_err(store_err)?;
            }
            tx.commit().map_err(store_err)?;
            return Ok(false);
        }

        // Fork: the sender re-numbered from below what we already hold.
        // Drop our stale copy of the abandoned tail (this and every later
        // lamport we have from their old stream -- they'll resend anything
        // beyond this message under the new numbering too)...
        tx.execute(
            "DELETE FROM messages WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport >= ?3",
            params![
                message.chat_id,
                message.sender_user_id,
                message.lamport as i64
            ],
        )
        .map_err(store_err)?;
        // ...and the watermarks we'd computed against that abandoned tail,
        // so we stop reporting stale "delivered/read through N" back to the
        // sender (root cause of the false ✓✓ this recovery fixes). These
        // regenerate correctly from the normal receive/view paths.
        tx.execute(
            "DELETE FROM outgoing_receipts WHERE chat_id = ?1 AND sender_user_id = ?2",
            params![message.chat_id, message.sender_user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outgoing_receipt_envelopes WHERE chat_id = ?1 AND sender_user_id = ?2",
            params![message.chat_id, message.sender_user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "INSERT INTO messages
                (chat_id, sender_user_id, lamport, timestamp, kind, payload,
                 msg_id, reply_to_msg_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                message.chat_id,
                message.sender_user_id,
                message.lamport as i64,
                message.timestamp,
                message.kind as i64,
                message.payload,
                msg_id,
                reply_to_msg_id,
            ],
        )
        .map_err(store_err)?;

        tx.commit().map_err(store_err)?;
        Ok(true)
    }
}

#[uniffi::export]
impl MessageStore {
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
        outgoing_message_reference::insert(self, message, envelope, None, queued_at_ms)
    }

    /// Atomically persist a locally authored reply and its stable sealed
    /// envelope. The target id remains local metadata as well as encrypted
    /// body metadata, so rendering never needs to reopen ciphertext.
    pub fn insert_outgoing_reply(
        &self,
        message: StoredMessage,
        envelope: OutboundEnvelope,
        reply_to_msg_id: Vec<u8>,
        queued_at_ms: i64,
    ) -> Result<bool, CoreError> {
        validate_msg_id("msg_id", &envelope.msg_id)?;
        validate_msg_id("reply_to_msg_id", &reply_to_msg_id)?;
        outgoing_message_reference::insert(
            self,
            message,
            envelope,
            Some(reply_to_msg_id),
            queued_at_ms,
        )
    }
}

mod outgoing_message_reference {
    use super::*;

    pub(super) fn insert(
        store: &MessageStore,
        message: StoredMessage,
        envelope: OutboundEnvelope,
        reply_to_msg_id: Option<Vec<u8>>,
        queued_at_ms: i64,
    ) -> Result<bool, CoreError> {
        validate_stored_message(&message)?;
        validate_sqlite_u64("envelope lamport", envelope.lamport)?;
        let mut conn = store.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        tx.execute(
            "INSERT OR IGNORE INTO messages
                (chat_id, sender_user_id, lamport, timestamp, kind, payload,
                 msg_id, reply_to_msg_id, outbound_expiry)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                message.chat_id,
                message.sender_user_id,
                message.lamport as i64,
                message.timestamp,
                message.kind as i64,
                message.payload,
                envelope.msg_id,
                reply_to_msg_id,
                envelope.expiry,
            ],
        )
        .map_err(store_err)?;
        tx.execute(
            "UPDATE messages
             SET msg_id = COALESCE(msg_id, ?4),
                 reply_to_msg_id = COALESCE(reply_to_msg_id, ?5),
                 outbound_expiry = COALESCE(outbound_expiry, ?6)
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3",
            params![
                message.chat_id,
                message.sender_user_id,
                message.lamport as i64,
                envelope.msg_id,
                reply_to_msg_id,
                envelope.expiry,
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
                        &envelope.recipient_user_id,
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
}

#[uniffi::export]
impl MessageStore {
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
        let rows = stmt
            .query_map(params![chat_id], row_to_message)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Stable id and optional reply target for one stored message. Returns
    /// `None` for legacy rows whose inbound envelope id was never recorded.
    pub fn message_reference(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        lamport: u64,
    ) -> Result<Option<MessageReference>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT msg_id, reply_to_msg_id
             FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3
               AND msg_id IS NOT NULL",
            params![chat_id, sender_user_id, lamport as i64],
            |row| {
                Ok(MessageReference {
                    msg_id: row.get(0)?,
                    reply_to_msg_id: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(store_err)
    }

    /// Expiry of a locally-authored message's durable outbound envelope.
    /// This remains available after the retry queue prunes expired ciphertext.
    pub fn outbound_message_expiry(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        lamport: u64,
    ) -> Result<Option<i64>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT outbound_expiry FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3",
            params![chat_id, sender_user_id, lamport as i64],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .map_err(store_err)
        .map(|value| value.flatten())
    }

    /// Resolve a quoted message by stable envelope id within one chat.
    /// Missing history is expected and returns `None`.
    pub fn message_by_msg_id(
        &self,
        chat_id: Vec<u8>,
        msg_id: Vec<u8>,
    ) -> Result<Option<StoredMessage>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload
             FROM messages
             WHERE chat_id = ?1 AND msg_id = ?2
             ORDER BY id ASC
             LIMIT 1",
            params![chat_id, msg_id],
            row_to_message,
        )
        .optional()
        .map_err(store_err)
    }

    /// Chat and sender of a stored message keyed by its stable envelope
    /// `msg_id` alone, searched across every chat -- unlike
    /// [`Self::message_by_msg_id`], which needs `chat_id` up front and is
    /// useless here because a relay-fetched envelope only carries its own
    /// `msg_id`.
    ///
    /// This backs the consumed-SEEN relay ack rule in `engine.rs`
    /// (`MessageStore::core_relay_ack_ids_with_consumed`): a relay-fetched
    /// copy that dedupes as `Seen` (already handled via some other path) is
    /// only safe to ack if THIS device actually consumed it as a real
    /// message, not merely muled it. A row only exists here for kinds that
    /// persist a durable `msg_id` -- 1:1/group text, attachment manifests,
    /// reactions (inserted via `insert_incoming_message`) and our own
    /// authored messages (via `insert_outgoing_message`/
    /// `insert_outgoing_reply`). Hidden kinds -- receipts, profile sync,
    /// friend requests/directory, group invites, LAN endpoint hints -- are
    /// stored, if at all, via the plain `insert_message` with `msg_id =
    /// NULL`, so they never match and the caller correctly treats "no
    /// match" as "cannot vouch for this copy, don't ack."
    ///
    /// Returns `None` for an unknown `msg_id` (never stored, or hidden-kind
    /// with no durable id). The store deliberately returns the raw
    /// [`MessageOrigin`] instead of a verdict: it is the caller's job to
    /// exclude own-authored rows (`sender_user_id == own user id` -- the
    /// relay copy is there for the recipient) and group rows (`chat_id !=
    /// sender_user_id` -- other members of the shared family mailbox still
    /// need the relay copy).
    pub fn message_origin_by_msg_id(
        &self,
        msg_id: Vec<u8>,
    ) -> Result<Option<MessageOrigin>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT chat_id, sender_user_id FROM messages
             WHERE msg_id = ?1 ORDER BY id ASC LIMIT 1",
            params![msg_id],
            |row| {
                Ok(MessageOrigin {
                    chat_id: row.get(0)?,
                    sender_user_id: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(store_err)
    }

    /// Attach first-arrival diagnostics to an already inserted incoming
    /// message. A redundant mesh/relay copy never overwrites the original
    /// route, hop count, or receive time.
    pub fn record_message_arrival(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        lamport: u64,
        arrival: MessageArrival,
    ) -> Result<bool, CoreError> {
        // 0/1 = BLE direct/muled, 2 = relay, 3/4 = LAN direct/muled.
        if arrival.transport > 4 {
            return Err(CoreError::Malformed(
                "invalid message arrival transport".to_string(),
            ));
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn
            .execute(
                "UPDATE messages
                 SET arrival_transport = ?4, hops_taken = ?5, received_at = ?6
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3
                   AND arrival_transport IS NULL",
                params![
                    chat_id,
                    sender_user_id,
                    lamport as i64,
                    arrival.transport as i64,
                    arrival.hops_taken as i64,
                    arrival.received_at,
                ],
            )
            .map_err(store_err)?;
        if changed > 0 {
            // V2 field metric: log the inbound arrival alongside the diagnostic
            // update, on the message's first arrival only. Best-effort and
            // metadata-only; a metrics failure must not fail delivery.
            let _ = conn.execute(
                "INSERT OR IGNORE INTO delivery_metrics
                    (chat_hash, lamport, direction, sender_hash, at_ms, arrival_transport, hop_count)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6)",
                params![
                    metric_chat_hash(&chat_id),
                    lamport as i64,
                    metric_sender_hash(&sender_user_id),
                    arrival.received_at,
                    arrival.transport as i64,
                    arrival.hops_taken as i64,
                ],
            );
        }
        Ok(changed > 0)
    }

    /// V2 field metric: record that this device authored an outbound message
    /// at `lamport` in `chat_id` at `sent_at_ms`, so the cruise-test export can
    /// later measure delivery latency and the route a receipt returned on.
    /// Idempotent per (chat, lamport); metadata only -- the chat is stored as
    /// an 8-byte hash and no content is kept. See [`delivery_metrics`].
    pub fn record_sent_metric(
        &self,
        chat_id: Vec<u8>,
        lamport: u64,
        sent_at_ms: i64,
    ) -> Result<(), CoreError> {
        validate_sqlite_u64("metric lamport", lamport)?;
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT OR IGNORE INTO delivery_metrics
                (chat_hash, lamport, direction, sender_hash, at_ms)
             VALUES (?1, ?2, 0, ?3, ?4)",
            params![
                metric_chat_hash(&chat_id),
                lamport as i64,
                metric_sender_self(),
                sent_at_ms,
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// V2 field metric: stamp the delivery time and return route (T6
    /// `via_transport`) onto every outbound metric row in `chat_id` at or below
    /// the confirmed `through_lamport` that isn't already marked delivered.
    /// Cumulative receipts confirm a run of messages at once, so this covers
    /// them all; the first confirmation wins (a later, higher watermark still
    /// stamps the messages it newly covers). Metadata only.
    pub fn record_delivered_metric(
        &self,
        chat_id: Vec<u8>,
        through_lamport: u64,
        delivered_at_ms: i64,
        via_transport: Option<u8>,
    ) -> Result<(), CoreError> {
        validate_sqlite_u64("metric lamport", through_lamport)?;
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "UPDATE delivery_metrics
             SET delivered_at_ms = ?3, via_transport = ?4
             WHERE chat_hash = ?1 AND direction = 0
               AND lamport <= ?2 AND delivered_at_ms IS NULL",
            params![
                metric_chat_hash(&chat_id),
                through_lamport as i64,
                delivered_at_ms,
                via_transport.map(|t| t as i64),
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// V2 field metrics as CSV for the cruise-test export (metadata only). One
    /// row per sent/received message; `latency_ms` is the send->delivered gap
    /// for confirmed outbound messages. Transports use the
    /// [`MessageArrival::transport`] encoding (0/1 BLE direct/muled, 2 relay,
    /// 3/4 LAN direct/muled). Empty cells are unknown/not-applicable.
    pub fn export_delivery_metrics_csv(&self) -> Result<String, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT chat_hash, lamport, direction, sender_hash, at_ms, delivered_at_ms,
                        via_transport, arrival_transport, hop_count
                 FROM delivery_metrics
                 ORDER BY direction, chat_hash, lamport, sender_hash",
            )
            .map_err(store_err)?;
        let mut out = String::from(
            "direction,chat,lamport,sender,at_ms,delivered_at_ms,latency_ms,via_transport,arrival_transport,hop_count\n",
        );
        let rows = stmt
            .query_map([], |row| {
                let chat_hash: Vec<u8> = row.get(0)?;
                let lamport: i64 = row.get(1)?;
                let direction: i64 = row.get(2)?;
                let sender_hash: Vec<u8> = row.get(3)?;
                let at_ms: i64 = row.get(4)?;
                let delivered_at_ms: Option<i64> = row.get(5)?;
                let via_transport: Option<i64> = row.get(6)?;
                let arrival_transport: Option<i64> = row.get(7)?;
                let hop_count: Option<i64> = row.get(8)?;
                let latency_ms = match (direction, delivered_at_ms) {
                    (0, Some(d)) => Some(d - at_ms),
                    _ => None,
                };
                let dir = if direction == 0 { "sent" } else { "received" };
                // "sent" rows always carry the fixed self sentinel (this
                // device is the sole author of its own stream) -- leave the
                // sender cell blank rather than print a meaningless constant.
                let sender_cell = if direction == 1 {
                    hex_lower(&sender_hash)
                } else {
                    String::new()
                };
                let cell = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_default();
                Ok(format!(
                    "{dir},{},{lamport},{sender_cell},{at_ms},{},{},{},{},{}\n",
                    hex_lower(&chat_hash),
                    cell(delivered_at_ms),
                    cell(latency_ms),
                    cell(via_transport),
                    cell(arrival_transport),
                    cell(hop_count),
                ))
            })
            .map_err(store_err)?;
        for row in rows {
            out.push_str(&row.map_err(store_err)?);
        }
        Ok(out)
    }

    /// First-arrival diagnostics for one message, or `None` for locally
    /// authored/legacy rows that predate diagnostics.
    pub fn message_arrival(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        lamport: u64,
    ) -> Result<Option<MessageArrival>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT arrival_transport, hops_taken, received_at
             FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3
               AND arrival_transport IS NOT NULL",
            params![chat_id, sender_user_id, lamport as i64],
            |row| {
                Ok(MessageArrival {
                    transport: row.get::<_, i64>(0)? as u8,
                    hops_taken: row.get::<_, i64>(1)? as u8,
                    received_at: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(store_err)
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

    /// The highest lamport value actually held from this sender in this
    /// chat -- a plain MAX, with no contiguity requirement. Returns 0 when
    /// nothing has been received yet.
    ///
    /// This is deliberately a *different* primitive from
    /// [`MessageStore::highest_contiguous_lamport`], and the split matters:
    /// the transactional authoring Lamport ratchet
    /// lets a sender's stream legitimately start above 1 after a chat
    /// history wipe, because lamports below the new base never existed for
    /// anyone -- there is nothing to be "contiguous from 1" with. A
    /// receiver holding only e.g. {3, 4} from that sender has a perfectly
    /// complete view of everything the sender ever sent, but
    /// `highest_contiguous_lamport` reports 0 for it (it stops at the first
    /// missing lamport, and 1 and 2 are permanently missing). Basing
    /// delivered/read receipts and the local read/unread badge on that 0
    /// makes them stall forever: `handle_chat_viewed`-style callers would
    /// record read-through 0 and the unread count would never clear.
    ///
    /// `highest_lamport` fixes that by answering "what's the highest
    /// message I actually hold from this sender" instead -- which is the
    /// right basis for a receipt/badge watermark. It is *not* a safe
    /// substitute for [`MessageStore::highest_contiguous_lamport`] in
    /// digest sync ([`MessageStore::chat_digest`]): that path genuinely
    /// needs the gap-aware contiguous count so it can detect a hole and
    /// re-request the missing early messages (DESIGN.md §7.3). Reporting a
    /// bare MAX there would let a real front-gap (message 1 lost in
    /// transit, not wiped) go undetected forever, since the peer would
    /// believe we already have everything up to the max we've seen.
    ///
    /// Moving receipts/badges to MAX is safe from a message-loss
    /// standpoint: `record_receipt` (a peer acking delivery/read of *our*
    /// stream) never prunes `outbound_envelopes` -- only expiry and
    /// chat-delete do -- so an overstated watermark here cannot cause a
    /// sender to drop an undelivered message of their own.
    pub fn highest_lamport(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
    ) -> Result<u64, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT COALESCE(MAX(lamport), 0) FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2",
            params![chat_id, sender_user_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(store_err)
        .map(|n| n as u64)
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
            let through_lamport =
                highest_contiguous_lamport_locked(&conn, &chat_id, &sender_user_id)?;
            entries.push(DigestEntry {
                sender_user_id,
                through_lamport,
            });
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
            .query_map(
                params![chat_id, sender_user_id, after_lamport as i64],
                row_to_message,
            )
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
            .query_map(
                params![chat_id, sender_user_id, after_lamport as i64],
                row_to_outbound,
            )
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
            .execute(
                "DELETE FROM outbound_envelopes WHERE expiry <= ?1",
                params![now_ms],
            )
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
        validate_receipt_watermark(envelope.receipt_type, envelope.through_lamport)?;
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
    ///
    /// `via_transport` (T6) is the transport the receipt itself returned on
    /// (the [`MessageArrival::transport`] encoding), recorded so a message's
    /// Info pane can prove *how* delivery was confirmed. It is overwritten
    /// when the watermark actually advances and the new receipt carries a
    /// known transport, and (FC4) also filled in when the watermark merely
    /// *matches* the stored one but the stored route is still unknown --
    /// otherwise a first confirmation whose return route we couldn't
    /// determine would permanently hide a later, more informative receipt at
    /// the same watermark. A stale/replayed receipt (lower watermark) never
    /// touches it, and a receipt with an unknown route (`via_transport =
    /// None`) never clears an already-known one. Pass `None` when the return
    /// route isn't known.
    pub fn record_receipt(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
        through_lamport: u64,
        via_transport: Option<u8>,
    ) -> Result<(), CoreError> {
        validate_receipt_watermark(receipt_type, through_lamport)?;
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO receipts (chat_id, sender_user_id, receipt_type, through_lamport, via_transport)
                VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                via_transport = CASE
                    WHEN excluded.via_transport IS NOT NULL
                         AND (
                             excluded.through_lamport > through_lamport
                             OR (excluded.through_lamport = through_lamport
                                 AND via_transport IS NULL)
                         )
                    THEN excluded.via_transport
                    ELSE via_transport END,
                through_lamport = MAX(through_lamport, excluded.through_lamport)",
            params![
                chat_id,
                sender_user_id,
                receipt_type as i64,
                through_lamport as i64,
                via_transport.map(|t| t as i64)
            ],
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

    /// The transport a peer's `receipt_type` receipt returned on for the
    /// highest watermark recorded so far (T6) -- the [`MessageArrival::transport`]
    /// encoding. `None` if no such receipt exists yet or its return route was
    /// unknown. Any message whose lamport is at or below
    /// [`MessageStore::receipt_through`] for the same key was confirmed by this
    /// route, so the Info pane can show it against every acknowledged message,
    /// not just the one at the exact watermark.
    pub fn receipt_via_transport(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
    ) -> Result<Option<u8>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let via: Option<Option<i64>> = conn
            .query_row(
                "SELECT via_transport FROM receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![chat_id, sender_user_id, receipt_type as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(via.flatten().map(|t| t as u8))
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
        validate_receipt_watermark(receipt_type, through_lamport)?;
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO outgoing_receipts (chat_id, sender_user_id, receipt_type, through_lamport)
                VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                through_lamport = MAX(through_lamport, excluded.through_lamport)",
            params![
                chat_id,
                sender_user_id,
                receipt_type as i64,
                through_lamport as i64
            ],
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

    /// Import a friend card without allowing an older/blank card to erase a
    /// complete relay configuration already known for that contact.
    pub fn upsert_imported_contact(&self, mut contact: Contact) -> Result<Contact, CoreError> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let incoming_has_relay = contact
            .relay_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && contact
                .relay_token
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
        if !incoming_has_relay {
            let existing: Option<(Option<String>, Option<String>)> = tx
                .query_row(
                    "SELECT relay_url, relay_token FROM contacts WHERE user_id = ?1",
                    params![contact.user_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(store_err)?;
            if let Some((url, token)) = existing {
                let existing_has_relay =
                    url.as_deref().is_some_and(|value| !value.trim().is_empty())
                        && token
                            .as_deref()
                            .is_some_and(|value| !value.trim().is_empty());
                if existing_has_relay {
                    contact.relay_url = url;
                    contact.relay_token = token;
                }
            }
        }
        tx.execute(
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
        tx.commit().map_err(store_err)?;
        Ok(contact)
    }

    /// Apply a contact avatar update only if `epoch` is newer than the stored
    /// avatar epoch. `None` or an empty blob clears the avatar but still
    /// records the newer epoch.
    pub fn set_contact_avatar(
        &self,
        user_id: Vec<u8>,
        avatar: Option<Vec<u8>>,
        epoch: i64,
    ) -> Result<bool, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let avatar = avatar.filter(|bytes| !bytes.is_empty());
        let changed = conn
            .execute(
                "UPDATE contacts
                 SET avatar = ?2, avatar_epoch = ?3
                 WHERE user_id = ?1 AND ?3 > avatar_epoch",
                params![user_id, avatar, epoch],
            )
            .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// Set (or clear) the local nickname for a contact (T16). A `None` or
    /// blank/whitespace value clears it, falling display back to the card
    /// `name`. Returns whether a row was updated (false = unknown contact).
    /// This never touches any wire-visible field; the nickname stays local.
    pub fn set_contact_nickname(
        &self,
        user_id: Vec<u8>,
        nickname: Option<String>,
    ) -> Result<bool, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let nickname = nickname
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let changed = conn
            .execute(
                "UPDATE contacts SET nickname = ?2 WHERE user_id = ?1",
                params![user_id, nickname],
            )
            .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// The canonical JPEG avatar bytes for a contact, if one has been synced.
    pub fn contact_avatar(&self, user_id: Vec<u8>) -> Result<Option<Vec<u8>>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT avatar FROM contacts WHERE user_id = ?1",
            params![user_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_err)
        .map(|row| row.flatten())
    }

    /// The newest avatar/display-name profile-sync epoch applied for a contact.
    pub fn contact_avatar_epoch(&self, user_id: Vec<u8>) -> Result<i64, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let epoch: Option<i64> = conn
            .query_row(
                "SELECT avatar_epoch FROM contacts WHERE user_id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(epoch.unwrap_or(0))
    }

    /// Delete a contact and, with it, the entire 1:1 chat: the contact row,
    /// every message whose `chat_id` is their UserID (DESIGN.md §7.1: a 1:1
    /// chat's id *is* the peer's UserID), that chat's incoming/outgoing
    /// receipt rows, and every queued retry artifact keyed to this chat
    /// (`outgoing_receipt_envelopes`, `outbound_envelopes`).
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
    /// The two queued-envelope tables matter as much as `messages` here: a
    /// deleted chat that left `outbound_envelopes` behind re-arms the
    /// reset-stream trap fixed in fc6b9f9 (recover-from-forked-stream) --
    /// stale queued envelopes can resend frames from the deleted history to
    /// a peer whose lamport stream has since moved on, which is exactly the
    /// shape of bug that recovery exists to catch, not reintroduce via a
    /// leftover queue. And a leftover `receipts` row is exactly the
    /// overstated ratchet that painted false read-ticks before that fix: a
    /// delete must yield a genuinely blank slate, not a chat that looks
    /// empty locally but still remembers watermarks against history the
    /// user asked to erase. If the contact is re-added, the peer's replayed
    /// receipts plus fork recovery re-establish consistency from scratch --
    /// nothing here needs to survive a delete to make that work.
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
        tx.execute(
            "DELETE FROM outgoing_receipts WHERE chat_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outgoing_receipt_envelopes WHERE chat_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outbound_envelopes WHERE chat_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM contact_discovery_policy WHERE user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM contact_provenance WHERE user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM friend_suggestions
             WHERE candidate_user_id = ?1 OR introducer_user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM friend_suggestion_state WHERE candidate_user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM friend_directory_state WHERE introducer_user_id = ?1",
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
            "SELECT user_id, name, sign_pk, agree_pk, relay_url, relay_token, nickname FROM contacts WHERE user_id = ?1",
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
            .prepare("SELECT user_id, name, sign_pk, agree_pk, relay_url, relay_token, nickname FROM contacts ORDER BY name ASC")
            .map_err(store_err)?;
        let rows = stmt.query_map([], row_to_contact).map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Apply an authenticated contact's discovery policy if it is newer.
    pub fn upsert_contact_discovery_policy(
        &self,
        policy: ContactDiscoveryPolicy,
    ) -> Result<bool, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let changed = conn
            .execute(
                "INSERT INTO contact_discovery_policy
                    (user_id, protocol_version, enabled, revision)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(user_id) DO UPDATE SET
                    protocol_version = excluded.protocol_version,
                    enabled = excluded.enabled,
                    revision = excluded.revision
                 WHERE excluded.revision > contact_discovery_policy.revision",
                params![
                    policy.user_id,
                    policy.protocol_version as i64,
                    i64::from(policy.enabled),
                    policy.revision as i64,
                ],
            )
            .map_err(store_err)?
            > 0;
        Ok(changed)
    }

    pub fn get_contact_discovery_policy(
        &self,
        user_id: Vec<u8>,
    ) -> Result<Option<ContactDiscoveryPolicy>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT user_id, protocol_version, enabled, revision
             FROM contact_discovery_policy WHERE user_id = ?1",
            params![user_id],
            |row| {
                Ok(ContactDiscoveryPolicy {
                    user_id: row.get(0)?,
                    protocol_version: row.get::<_, i64>(1)? as u8,
                    enabled: row.get::<_, i64>(2)? != 0,
                    revision: row.get::<_, i64>(3)? as u64,
                })
            },
        )
        .optional()
        .map_err(store_err)
    }

    /// Atomically replace all suggestions supplied by one introducer. The
    /// directory's tickets are checked here so both mobile shells share the
    /// same fail-closed behavior.
    pub fn apply_friend_directory(
        &self,
        introducer_user_id: Vec<u8>,
        recipient_user_id: Vec<u8>,
        content: FriendDirectoryContent,
        now_ms: i64,
    ) -> Result<bool, CoreError> {
        if content.version != 1 || content.entries.len() > 64 {
            return Err(CoreError::Malformed("invalid friend directory".to_string()));
        }
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let introducer_sign_pk: Option<Vec<u8>> = tx
            .query_row(
                "SELECT sign_pk FROM contacts WHERE user_id = ?1",
                params![introducer_user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let Some(introducer_sign_pk) = introducer_sign_pk else {
            return Err(CoreError::Malformed(
                "friend directory sender is not a contact".to_string(),
            ));
        };
        let applied: Option<i64> = tx
            .query_row(
                "SELECT applied_revision FROM friend_directory_state
                 WHERE introducer_user_id = ?1",
                params![introducer_user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        if applied.is_some_and(|revision| revision as u64 >= content.revision) {
            tx.commit().map_err(store_err)?;
            return Ok(false);
        }

        for entry in &content.entries {
            if entry.ticket.introducer_user_id != introducer_user_id
                || !verify_introduction_ticket(
                    entry.ticket.clone(),
                    introducer_sign_pk.clone(),
                    entry.candidate.user_id.clone(),
                    recipient_user_id.clone(),
                    entry.candidate_policy_revision,
                    now_ms,
                )?
            {
                return Err(CoreError::Malformed(
                    "friend directory contains an invalid introduction ticket".to_string(),
                ));
            }

            // A requested suggestion becomes retryable once there is no
            // longer an unexpired introduced-request envelope queued for it.
            // Hidden suggestions remain suppressed across snapshots.
            let request_still_pending: bool = tx
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM outbound_envelopes
                         WHERE recipient_user_id = ?1 AND kind = ?2 AND expiry > ?3
                     )",
                    params![
                        &entry.candidate.user_id,
                        KIND_INTRODUCED_FRIEND_REQUEST as i64,
                        now_ms,
                    ],
                    |row| row.get(0),
                )
                .map_err(store_err)?;
            if !request_still_pending {
                tx.execute(
                    "DELETE FROM friend_suggestion_state
                     WHERE candidate_user_id = ?1 AND state = 1",
                    params![&entry.candidate.user_id],
                )
                .map_err(store_err)?;
            }
        }

        tx.execute(
            "INSERT INTO friend_directory_state (introducer_user_id, applied_revision)
             VALUES (?1, ?2)
             ON CONFLICT(introducer_user_id) DO UPDATE SET
                applied_revision = excluded.applied_revision",
            params![introducer_user_id, content.revision as i64],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM friend_suggestions WHERE introducer_user_id = ?1",
            params![introducer_user_id],
        )
        .map_err(store_err)?;
        for entry in content.entries {
            let ticket =
                serde_json::to_vec(&entry.ticket).map_err(|e| CoreError::Store(e.to_string()))?;
            tx.execute(
                "INSERT INTO friend_suggestions
                    (candidate_user_id, introducer_user_id, name, sign_pk, agree_pk,
                     candidate_policy_revision, ticket, expires_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    entry.candidate.user_id,
                    introducer_user_id,
                    entry.candidate.name,
                    entry.candidate.sign_pk,
                    entry.candidate.agree_pk,
                    entry.candidate_policy_revision as i64,
                    ticket,
                    entry.ticket.expires_at_ms,
                ],
            )
            .map_err(store_err)?;
        }
        tx.commit().map_err(store_err)?;
        Ok(true)
    }

    pub fn list_friend_suggestions(&self, now_ms: i64) -> Result<Vec<FriendSuggestion>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT s.candidate_user_id, s.name, s.sign_pk, s.agree_pk,
                        s.introducer_user_id, s.ticket,
                        COALESCE(x.state, 0)
                 FROM friend_suggestions s
                 LEFT JOIN friend_suggestion_state x
                    ON x.candidate_user_id = s.candidate_user_id
                 LEFT JOIN contacts c ON c.user_id = s.candidate_user_id
                 WHERE c.user_id IS NULL AND s.expires_at_ms >= ?1
                       AND COALESCE(x.state, 0) != 2
                 ORDER BY lower(s.name), s.introducer_user_id",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![now_ms], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, i64>(6)? as u8,
                ))
            })
            .map_err(store_err)?;
        let mut suggestions = Vec::new();
        for row in rows {
            let (user_id, name, sign_pk, agree_pk, introducer_user_id, ticket_json, state) =
                row.map_err(store_err)?;
            let ticket: IntroductionTicket = serde_json::from_slice(&ticket_json)
                .map_err(|e| CoreError::Store(e.to_string()))?;
            suggestions.push(FriendSuggestion {
                candidate: SuggestedFriendCard {
                    name,
                    user_id,
                    sign_pk,
                    agree_pk,
                },
                introducer_user_id,
                ticket,
                state,
            });
        }
        Ok(suggestions)
    }

    /// State values: 0 available, 1 requested, 2 hidden.
    pub fn set_friend_suggestion_state(
        &self,
        candidate_user_id: Vec<u8>,
        state: u8,
    ) -> Result<(), CoreError> {
        if state > 2 {
            return Err(CoreError::Malformed("invalid suggestion state".to_string()));
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO friend_suggestion_state (candidate_user_id, state)
             VALUES (?1, ?2)
             ON CONFLICT(candidate_user_id) DO UPDATE SET state = excluded.state",
            params![candidate_user_id, state as i64],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn remove_friend_suggestion(&self, candidate_user_id: Vec<u8>) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "DELETE FROM friend_suggestions WHERE candidate_user_id = ?1",
            params![candidate_user_id],
        )
        .map_err(store_err)?;
        conn.execute(
            "DELETE FROM friend_suggestion_state WHERE candidate_user_id = ?1",
            params![candidate_user_id],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn clear_friend_suggestions(&self) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute_batch(
            "DELETE FROM friend_suggestions;
             DELETE FROM friend_directory_state;",
        )
        .map_err(store_err)
    }

    pub fn upsert_contact_provenance(
        &self,
        provenance: ContactProvenance,
    ) -> Result<(), CoreError> {
        if provenance.source > 1 {
            return Err(CoreError::Malformed(
                "invalid contact provenance".to_string(),
            ));
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO contact_provenance
                (user_id, source, introducer_user_id, introduced_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(user_id) DO UPDATE SET
                source = CASE WHEN contact_provenance.source = 0 THEN 0 ELSE excluded.source END,
                introducer_user_id = CASE WHEN contact_provenance.source = 0
                    THEN contact_provenance.introducer_user_id ELSE excluded.introducer_user_id END,
                introduced_at_ms = CASE WHEN contact_provenance.source = 0
                    THEN contact_provenance.introduced_at_ms ELSE excluded.introduced_at_ms END",
            params![
                provenance.user_id,
                provenance.source as i64,
                provenance.introducer_user_id,
                provenance.introduced_at_ms,
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn get_contact_provenance(
        &self,
        user_id: Vec<u8>,
    ) -> Result<Option<ContactProvenance>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT user_id, source, introducer_user_id, introduced_at_ms
             FROM contact_provenance WHERE user_id = ?1",
            params![user_id],
            |row| {
                Ok(ContactProvenance {
                    user_id: row.get(0)?,
                    source: row.get::<_, i64>(1)? as u8,
                    introducer_user_id: row.get(2)?,
                    introduced_at_ms: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(store_err)
    }

    /// Add or replace a group definition and its full membership. Updating an
    /// existing group id replaces the stored key/member list atomically,
    /// which is the v1 rotation path for membership changes.
    pub fn upsert_group(&self, group: Group) -> Result<(), CoreError> {
        validate_group(&group)?;
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        upsert_group_tx(&tx, &group)?;
        tx.commit().map_err(store_err)?;
        Ok(())
    }

    /// Look up one imported group by id, including its current member list.
    pub fn get_group(&self, group_id: Vec<u8>) -> Result<Option<Group>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let row: Option<GroupRow> = conn
            .query_row(
                "SELECT group_id, name, group_key, metadata_revision, metadata_changed_by
                 FROM groups WHERE group_id = ?1",
                params![&group_id],
                row_to_group_row,
            )
            .optional()
            .map_err(store_err)?;
        row.map(|row| hydrate_group(&conn, row)).transpose()
    }

    /// All imported groups, alphabetical by name then id for stable ordering.
    pub fn list_groups(&self) -> Result<Vec<Group>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let raw = {
            let mut stmt = conn
                .prepare(
                    "SELECT group_id, name, group_key, metadata_revision, metadata_changed_by
                     FROM groups ORDER BY name ASC, group_id ASC",
                )
                .map_err(store_err)?;
            let rows = stmt.query_map([], row_to_group_row).map_err(store_err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(store_err)?
        };
        raw.into_iter()
            .map(|row| hydrate_group(&conn, row))
            .collect()
    }

    /// Delete a group definition, its membership rows, and every row of
    /// local chat history / retry state keyed by that group id: `messages`,
    /// `receipts`, `outgoing_receipts`, `outgoing_receipt_envelopes`, and
    /// `outbound_envelopes` -- the same "genuinely blank slate" purge as
    /// [`MessageStore::delete_contact`] (see that method's doc comment for
    /// why leftover queued envelopes or receipt watermarks are a bug, not
    /// just clutter: they re-arm the reset-stream trap fixed in fc6b9f9 and
    /// paint false read-ticks). Atomic and idempotent.
    pub fn delete_group(&self, group_id: Vec<u8>) -> Result<bool, CoreError> {
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        tx.execute(
            "DELETE FROM group_members WHERE group_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        let removed = tx
            .execute("DELETE FROM groups WHERE group_id = ?1", params![&group_id])
            .map_err(store_err)?;
        tx.execute(
            "DELETE FROM messages WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM receipts WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outgoing_receipts WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outgoing_receipt_envelopes WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outbound_envelopes WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(removed > 0)
    }

    // --- carry queue (DESIGN.md §5.3) --------------------------------------

    /// Store a foreign envelope for later store-and-forward delivery
    /// (DESIGN.md §5.3 carry queue). Keyed on `msg_id`, so re-enqueuing an
    /// envelope we're already carrying is a no-op (returns `false`); a fresh
    /// insert returns `true`. A digest over `recipient_hint || sealed` also
    /// collapses a ciphertext rewrapped under a new attacker-selected public
    /// `msg_id`, while preserving group fan-out copies with different hints.
    ///
    /// `is_family` marks whether this envelope is addressed to someone this
    /// node knows (its `recipient_hint` matched a contact -- the caller
    /// decides, since it holds the contacts and the hint derivation). Family
    /// envelopes win eviction fights: foreign rows are evicted first. Foreign
    /// rows additionally share `foreign_budget_bytes`, and the entire queue
    /// has a hard 64 MiB sealed-byte ceiling so a forged family hint cannot
    /// grow it indefinitely. Resource eviction is never reported as delivery
    /// and never produces a receipt or relay ack. All of this happens in one
    /// transaction.
    pub fn enqueue_carried_envelope(
        &self,
        envelope: CarriedEnvelope,
        is_family: bool,
        received_at_ms: i64,
        foreign_budget_bytes: i64,
    ) -> Result<bool, CoreError> {
        validate_carried_envelope(&envelope, received_at_ms)?;
        let content_digest = carried_content_digest(&envelope.recipient_hint, &envelope.sealed);
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let size = envelope.sealed.len() as i64;
        let changed = tx
            .execute(
                "INSERT OR IGNORE INTO carried_envelopes
                    (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family,
                     received_at, size_bytes, content_digest)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    envelope.msg_id,
                    envelope.hop_ttl as i64,
                    envelope.expiry,
                    envelope.recipient_hint,
                    envelope.sealed,
                    is_family as i64,
                    received_at_ms,
                    size,
                    content_digest,
                ],
            )
            .map_err(store_err)?;

        if changed == 0 {
            // Already carrying this msg_id: nothing inserted, so the budget
            // can't have grown -- skip eviction.
            tx.commit().map_err(store_err)?;
            return Ok(false);
        }

        tx.execute(
            "DELETE FROM carried_envelopes WHERE expiry <= ?1",
            params![received_at_ms],
        )
        .map_err(store_err)?;
        enforce_carried_budgets(
            &tx,
            foreign_budget_bytes.max(0),
            DEFAULT_TOTAL_CARRY_BUDGET_BYTES,
        )?;
        tx.commit().map_err(store_err)?;
        Ok(true)
    }

    /// Store an envelope pulled FROM the relay that we're proxying for its
    /// real recipient (relay proxy-polling: an internet phone fetches a
    /// contact's `recipient_hint`s alongside its own so a 1:1 message can
    /// bridge across BLE clusters -- see `MeshService.pollRelayMailbox` /
    /// `relayProxyHints` on the Kotlin side). This is the relay-sourced
    /// twin of [`MessageStore::enqueue_carried_envelope`]: always
    /// `is_family = 1` (the relay hint match already proved it's addressed
    /// to someone we know, so it gets family-first eviction priority) and
    /// `from_relay = 1`, which excludes it from
    /// [`MessageStore::family_carried_envelopes`] -- the relay-upload query
    /// -- because it is *already on the relay*; re-uploading it would just
    /// churn traffic and could resurrect a copy the real recipient already
    /// acked. It still shows up in [`MessageStore::carried_envelopes_for_hints`]
    /// / [`MessageStore::carried_envelopes_for_peer_sync`] so we can hand it
    /// to the real recipient over BLE. `INSERT OR IGNORE` keyed on `msg_id`,
    /// so re-fetching the same still-unacked proxy envelope on a later poll
    /// pass is a no-op. Returns whether a new row was inserted.
    pub fn enqueue_relay_carried_envelope(
        &self,
        envelope: CarriedEnvelope,
        now_ms: i64,
    ) -> Result<bool, CoreError> {
        validate_carried_envelope(&envelope, now_ms)?;
        let content_digest = carried_content_digest(&envelope.recipient_hint, &envelope.sealed);
        let mut conn = self.conn.lock().expect("store mutex poisoned");
        let tx = conn.transaction().map_err(store_err)?;
        let size = envelope.sealed.len() as i64;
        let changed = tx
            .execute(
                "INSERT OR IGNORE INTO carried_envelopes
                    (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family,
                     received_at, size_bytes, from_relay, content_digest)
                 VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, 1, ?8)",
                params![
                    envelope.msg_id,
                    envelope.hop_ttl as i64,
                    envelope.expiry,
                    envelope.recipient_hint,
                    envelope.sealed,
                    now_ms,
                    size,
                    content_digest,
                ],
            )
            .map_err(store_err)?;
        if changed > 0 {
            tx.execute(
                "DELETE FROM carried_envelopes WHERE expiry <= ?1",
                params![now_ms],
            )
            .map_err(store_err)?;
            enforce_carried_budgets(&tx, i64::MAX, DEFAULT_TOTAL_CARRY_BUDGET_BYTES)?;
        }
        tx.commit().map_err(store_err)?;
        Ok(changed > 0)
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
        let placeholders = std::iter::repeat("?")
            .take(hints.len())
            .collect::<Vec<_>>()
            .join(",");
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

    /// Up to `limit` recent message-stream `msg_id`s this device holds,
    /// newest first: every `messages` row with a recorded envelope id, which
    /// covers both *consumed* incoming messages (opened-and-stored via
    /// `insert_incoming_message`) and our own *authored* ones
    /// (`insert_outgoing_message` writes the envelope's `msg_id` into the
    /// message row too, and `open` backfills it for older rows). This is the
    /// counterpart to [`MessageStore::carried_msg_ids`] for the D2
    /// mule-drain-confirm fix (DTN_TODOS.md §3.2): a recipient does not carry
    /// a message it has decrypted and stored -- it consumes it, so
    /// `carried_msg_ids` alone never advertises "I got it" for our own
    /// incoming mail. Merging this list into what we advertise in our own
    /// outgoing DIGEST (`recent_msg_id`s, see `protocol.rs`'s DIGEST frame
    /// docs, and `engine.rs::core_digest_advertised_msg_ids`) is what lets a
    /// mule that's still holding our envelope in its carry queue notice, on
    /// its next digest exchange with us, that we already have it and drop
    /// its copy -- without any wire-format change, since the DIGEST frame
    /// already carries an arbitrary `recent_msg_id` list.
    ///
    /// Including our own authored ids alongside the consumed ones is harmless
    /// and actively useful: a mule's Hook-B spray can hand us back an
    /// envelope we ourselves authored, and advertising its `msg_id` here
    /// suppresses that resend at the source -- the same rationale as the
    /// Kotlin side's `seedSeenIdsFromOwnHistory`.
    ///
    /// Newest first (unlike `carried_msg_ids`'s oldest-first) because the
    /// caller bounds the merged advertised list to a fixed count
    /// (`engine.rs::DIGEST_ADVERTISED_MSG_IDS_LIMIT`): the most recently
    /// landed messages are the ones most likely to still be sitting in some
    /// mule's carry queue, so they're the ones worth prioritizing when the
    /// list must be truncated.
    ///
    /// Only rows with a non-`NULL` `msg_id` participate -- legacy rows that
    /// predate envelope-id recording don't have one and are silently skipped
    /// by the `WHERE` clause.
    pub fn recent_consumed_msg_ids(&self, limit: u64) -> Result<Vec<Vec<u8>>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT msg_id FROM messages
                 WHERE msg_id IS NOT NULL
                 ORDER BY id DESC
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
            .execute(
                "DELETE FROM carried_envelopes WHERE msg_id = ?1",
                params![msg_id],
            )
            .map_err(store_err)?;
        Ok(removed > 0)
    }

    /// Delete every carried envelope whose `expiry` is at or before `now_ms`
    /// (DESIGN.md §5.3: "carriers drop the envelope past this time"). Returns
    /// how many were pruned.
    pub fn prune_expired_carried(&self, now_ms: i64) -> Result<u64, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let pruned = conn
            .execute(
                "DELETE FROM carried_envelopes WHERE expiry <= ?1",
                params![now_ms],
            )
            .map_err(store_err)?;
        Ok(pruned as u64)
    }

    /// Number of envelopes currently in the carry queue (diagnostics/tests).
    pub fn carried_len(&self) -> Result<u64, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM carried_envelopes", [], |row| {
                row.get(0)
            })
            .map_err(store_err)?;
        Ok(count as u64)
    }

    /// Unexpired carried envelopes that were classified as family traffic
    /// when received, oldest first. Used by relay upload so one phone with
    /// internet can uplink ciphertext it is muling for known contacts.
    /// Excludes `from_relay = 1` rows: those were pulled FROM the relay by
    /// proxy-polling in the first place (see
    /// [`MessageStore::enqueue_relay_carried_envelope`]), so re-uploading them
    /// here would be pointless churn (and could resurrect an envelope the
    /// real recipient already acked).
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
                 WHERE is_family = 1 AND from_relay = 0 AND expiry > ?1
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

fn validate_carried_envelope(envelope: &CarriedEnvelope, now_ms: i64) -> Result<(), CoreError> {
    if envelope.hop_ttl > crate::DEFAULT_HOP_TTL {
        return Err(CoreError::Malformed(format!(
            "carried envelope hop_ttl exceeds {}",
            crate::DEFAULT_HOP_TTL
        )));
    }
    if envelope.expiry <= now_ms
        || envelope.expiry > now_ms.saturating_add(crate::MAX_CARRY_FUTURE_MS)
    {
        return Err(CoreError::Malformed(
            "carried envelope expiry is outside the accepted window".to_string(),
        ));
    }
    Ok(())
}

/// Length of the metadata-only chat/sender hashes used by
/// [`delivery_metrics`]. Eight bytes is enough to keep distinct chats (or
/// senders) apart in an export without storing (or being reversible to) the
/// raw contact/group/user id.
const METRIC_CHAT_HASH_LEN: usize = 8;

/// Lowercase hex, for rendering the metric chat/sender hash in the CSV
/// export.
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Shared hash used for both the chat and sender tags in [`delivery_metrics`]
/// rows: a short, non-reversible tag, never a raw user/group id.
fn metric_hash8(id: &[u8]) -> Vec<u8> {
    let mut hasher =
        Blake2bVar::new(METRIC_CHAT_HASH_LEN).expect("valid BLAKE2b digest length");
    hasher.update(id);
    let mut digest = vec![0; METRIC_CHAT_HASH_LEN];
    hasher
        .finalize_variable(&mut digest)
        .expect("digest output has configured length");
    digest
}

/// A short, non-reversible tag for a chat id, used only to group field-metric
/// rows in the export. Never a raw user/group id -- see [`delivery_metrics`].
fn metric_chat_hash(chat_id: &[u8]) -> Vec<u8> {
    metric_hash8(chat_id)
}

/// FC1: a short, non-reversible tag for a sender's user id, added to the
/// [`delivery_metrics`] primary key so that two group members who happen to
/// share a lamport value (each member has an independent lamport stream) no
/// longer collide and silently drop one arrival.
fn metric_sender_hash(sender_user_id: &[u8]) -> Vec<u8> {
    metric_hash8(sender_user_id)
}

/// Sentinel sender hash for locally authored ("sent") [`delivery_metrics`]
/// rows. `record_sent_metric` has no sender argument -- this device is
/// always the sole author of its own outbound stream, so there is no
/// collision to resolve and a fixed placeholder is enough to satisfy the
/// primary key.
fn metric_sender_self() -> Vec<u8> {
    vec![0u8; METRIC_CHAT_HASH_LEN]
}

/// FC1 migration: pre-existing on-disk stores have `delivery_metrics` keyed
/// on `(chat_hash, lamport, direction)` only, which silently drops a group
/// arrival whenever two senders share a lamport value at the same watermark
/// (routine -- every group member has an independent lamport stream). This
/// table is local, best-effort, pre-cruise diagnostics data (see the schema
/// doc comment) with no cross-device meaning, so a row-preserving migration
/// isn't worth it: drop and let it be recreated with the current schema. New
/// arrivals repopulate it going forward.
fn migrate_delivery_metrics_schema(conn: &Connection) -> Result<(), CoreError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(delivery_metrics)")
        .map_err(store_err)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(store_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_err)?;
    drop(stmt);
    if names.is_empty() || names.iter().any(|c| c == "sender_hash") {
        // Fresh store (SCHEMA already created the current shape) or already
        // migrated on a previous open.
        return Ok(());
    }
    conn.execute("DROP TABLE delivery_metrics", [])
        .map_err(store_err)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS delivery_metrics (
            chat_hash         BLOB NOT NULL,
            lamport           INTEGER NOT NULL,
            direction         INTEGER NOT NULL,
            sender_hash       BLOB NOT NULL,
            at_ms             INTEGER NOT NULL,
            delivered_at_ms   INTEGER,
            via_transport     INTEGER,
            arrival_transport INTEGER,
            hop_count         INTEGER,
            PRIMARY KEY(chat_hash, lamport, direction, sender_hash)
        );",
    )
    .map_err(store_err)?;
    Ok(())
}

fn carried_content_digest(recipient_hint: &[u8], sealed: &[u8]) -> Vec<u8> {
    let mut hasher =
        Blake2bVar::new(CARRIED_CONTENT_DIGEST_LEN).expect("valid BLAKE2b digest length");
    hasher.update(recipient_hint);
    hasher.update(sealed);
    let mut digest = vec![0; CARRIED_CONTENT_DIGEST_LEN];
    hasher
        .finalize_variable(&mut digest)
        .expect("digest output has configured length");
    digest
}

/// Backfill the content-level dedupe key for existing stores and collapse
/// pre-migration duplicates deterministically, keeping the oldest row. No
/// deletion here is a delivery signal; this is local queue compaction only.
fn migrate_carried_content_digests(conn: &mut Connection) -> Result<(), CoreError> {
    let tx = conn.transaction().map_err(store_err)?;
    let mut seen: HashSet<Vec<u8>> = {
        let mut stmt = tx
            .prepare(
                "SELECT content_digest FROM carried_envelopes
                 WHERE content_digest IS NOT NULL",
            )
            .map_err(store_err)?;
        let collected = stmt
            .query_map([], |row| row.get(0))
            .map_err(store_err)?
            .collect::<Result<HashSet<_>, _>>()
            .map_err(store_err)?;
        collected
    };
    loop {
        let row: Option<(i64, Vec<u8>, Vec<u8>)> = tx
            .query_row(
                "SELECT rowid, recipient_hint, sealed
                 FROM carried_envelopes
                 WHERE content_digest IS NULL
                 ORDER BY received_at ASC, msg_id ASC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(store_err)?;
        let Some((rowid, recipient_hint, sealed)) = row else {
            break;
        };
        let digest = carried_content_digest(&recipient_hint, &sealed);
        if seen.insert(digest.clone()) {
            tx.execute(
                "UPDATE carried_envelopes SET content_digest = ?1 WHERE rowid = ?2",
                params![digest, rowid],
            )
            .map_err(store_err)?;
        } else {
            tx.execute(
                "DELETE FROM carried_envelopes WHERE rowid = ?1",
                params![rowid],
            )
            .map_err(store_err)?;
        }
    }
    enforce_carried_budgets(&tx, i64::MAX, DEFAULT_TOTAL_CARRY_BUDGET_BYTES)?;
    tx.commit().map_err(store_err)
}

fn enforce_carried_budgets(
    tx: &Transaction<'_>,
    foreign_budget_bytes: i64,
    total_budget_bytes: i64,
) -> Result<(), CoreError> {
    let mut foreign_total: i64 = tx
        .query_row(
            "SELECT COALESCE(SUM(size_bytes), 0)
             FROM carried_envelopes WHERE is_family = 0",
            [],
            |row| row.get(0),
        )
        .map_err(store_err)?;
    while foreign_total > foreign_budget_bytes {
        let oldest: Option<(Vec<u8>, i64)> = tx
            .query_row(
                "SELECT msg_id, size_bytes FROM carried_envelopes
                 WHERE is_family = 0
                 ORDER BY received_at ASC, msg_id ASC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(store_err)?;
        let Some((msg_id, size)) = oldest else {
            break;
        };
        tx.execute(
            "DELETE FROM carried_envelopes WHERE msg_id = ?1",
            params![msg_id],
        )
        .map_err(store_err)?;
        foreign_total = foreign_total.saturating_sub(size);
    }

    let mut total: i64 = tx
        .query_row(
            "SELECT COALESCE(SUM(size_bytes), 0) FROM carried_envelopes",
            [],
            |row| row.get(0),
        )
        .map_err(store_err)?;
    while total > total_budget_bytes.max(0) {
        let oldest: Option<(Vec<u8>, i64)> = tx
            .query_row(
                "SELECT msg_id, size_bytes FROM carried_envelopes
                 ORDER BY is_family ASC, received_at ASC, msg_id ASC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(store_err)?;
        let Some((msg_id, size)) = oldest else {
            break;
        };
        tx.execute(
            "DELETE FROM carried_envelopes WHERE msg_id = ?1",
            params![msg_id],
        )
        .map_err(store_err)?;
        total = total.saturating_sub(size);
    }
    Ok(())
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

pub(crate) fn row_to_outbound(row: &rusqlite::Row) -> rusqlite::Result<OutboundEnvelope> {
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

pub(crate) fn row_to_outgoing_receipt(
    row: &rusqlite::Row,
) -> rusqlite::Result<OutgoingReceiptEnvelope> {
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
        nickname: row.get(6)?,
    })
}

struct GroupRow {
    id: Vec<u8>,
    name: String,
    key: Vec<u8>,
    metadata_revision: u64,
    metadata_changed_by: Vec<u8>,
}

fn row_to_group_row(row: &rusqlite::Row) -> rusqlite::Result<GroupRow> {
    Ok(GroupRow {
        id: row.get(0)?,
        name: row.get(1)?,
        key: row.get(2)?,
        metadata_revision: row.get(3)?,
        metadata_changed_by: row.get(4)?,
    })
}

fn hydrate_group(conn: &Connection, row: GroupRow) -> Result<Group, CoreError> {
    Ok(Group {
        member_user_ids: load_group_members(conn, &row.id)?,
        id: row.id,
        name: row.name,
        key: row.key,
        metadata_revision: row.metadata_revision,
        metadata_changed_by: row.metadata_changed_by,
    })
}

/// Persist a group and its canonical member set inside an existing transaction.
/// A stale invite (revision zero) must not roll back newer metadata.
pub(crate) fn upsert_group_tx(tx: &Transaction<'_>, group: &Group) -> Result<(), CoreError> {
    validate_group(group)?;
    let current: Option<(u64, Vec<u8>)> = tx
        .query_row(
            "SELECT metadata_revision, metadata_changed_by FROM groups WHERE group_id = ?1",
            params![&group.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(store_err)?;
    if current.as_ref().is_some_and(|(revision, changed_by)| {
        (*revision, changed_by.as_slice())
            > (
                group.metadata_revision,
                group.metadata_changed_by.as_slice(),
            )
    }) {
        return Ok(());
    }

    tx.execute(
        "INSERT INTO groups
            (group_id, name, group_key, metadata_revision, metadata_changed_by)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(group_id) DO UPDATE SET
            name = excluded.name,
            group_key = excluded.group_key,
            metadata_revision = excluded.metadata_revision,
            metadata_changed_by = excluded.metadata_changed_by",
        params![
            &group.id,
            &group.name,
            &group.key,
            group.metadata_revision,
            &group.metadata_changed_by,
        ],
    )
    .map_err(store_err)?;
    tx.execute(
        "DELETE FROM group_members WHERE group_id = ?1",
        params![&group.id],
    )
    .map_err(store_err)?;
    for member_user_id in canonicalize_members(group.member_user_ids.clone()) {
        tx.execute(
            "INSERT INTO group_members (group_id, user_id) VALUES (?1, ?2)",
            params![&group.id, member_user_id],
        )
        .map_err(store_err)?;
    }
    Ok(())
}

fn load_group_members(conn: &Connection, group_id: &[u8]) -> Result<Vec<Vec<u8>>, CoreError> {
    let mut stmt = conn
        .prepare("SELECT user_id FROM group_members WHERE group_id = ?1 ORDER BY user_id ASC")
        .map_err(store_err)?;
    let rows = stmt
        .query_map(params![group_id], |row| row.get::<_, Vec<u8>>(0))
        .map_err(store_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
}

pub(crate) fn store_err(e: rusqlite::Error) -> CoreError {
    CoreError::Store(e.to_string())
}

fn validate_msg_id(field: &str, msg_id: &[u8]) -> Result<(), CoreError> {
    if msg_id.len() != MESSAGE_ID_LEN {
        return Err(CoreError::Malformed(format!(
            "{field} must be exactly {MESSAGE_ID_LEN} bytes"
        )));
    }
    Ok(())
}

fn validate_stored_message(message: &StoredMessage) -> Result<(), CoreError> {
    validate_sqlite_u64("message lamport", message.lamport)
}

fn validate_receipt_watermark(receipt_type: u8, through_lamport: u64) -> Result<(), CoreError> {
    if receipt_type != crate::RECEIPT_TYPE_DELIVERED && receipt_type != crate::RECEIPT_TYPE_READ {
        return Err(CoreError::Malformed("invalid receipt type".into()));
    }
    validate_sqlite_u64("receipt watermark", through_lamport)
}

fn validate_sqlite_u64(field: &str, value: u64) -> Result<(), CoreError> {
    if value > i64::MAX as u64 {
        return Err(CoreError::Malformed(format!(
            "{field} exceeds the supported range"
        )));
    }
    Ok(())
}

pub(crate) fn outbound_message_dedupe_key(
    chat_id: &[u8],
    sender_user_id: &[u8],
    kind: u8,
    lamport: u64,
    recipient_user_id: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        1 + 2 + chat_id.len() + 2 + sender_user_id.len() + 1 + 8 + 2 + recipient_user_id.len(),
    );
    out.push(1);
    write_bytes16_local(&mut out, chat_id);
    write_bytes16_local(&mut out, sender_user_id);
    out.push(kind);
    out.extend_from_slice(&lamport.to_be_bytes());
    write_bytes16_local(&mut out, recipient_user_id);
    out
}

fn write_bytes16_local(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn ensure_contact_column(conn: &Connection, name: &str, column_def: &str) -> Result<(), CoreError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(contacts)")
        .map_err(store_err)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(store_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_err)?;
    if names.iter().any(|existing| existing == name) {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE contacts ADD COLUMN {name} {column_def}"),
        [],
    )
    .map_err(store_err)?;
    Ok(())
}

/// Generic version of [`ensure_contact_column`] for any table: adds `name`
/// (with `column_def`) to `table` if an older on-disk schema doesn't already
/// have it. Idempotent -- a no-op once the column exists.
fn ensure_column(
    conn: &Connection,
    table: &str,
    name: &str,
    column_def: &str,
) -> Result<(), CoreError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(store_err)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(store_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_err)?;
    if names.iter().any(|existing| existing == name) {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {name} {column_def}"),
        [],
    )
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
    arrival_transport INTEGER,
    hops_taken     INTEGER,
    received_at    INTEGER,
    msg_id         BLOB,
    reply_to_msg_id BLOB,
    outbound_expiry INTEGER,
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
    relay_token TEXT,
    avatar BLOB,
    avatar_epoch INTEGER NOT NULL DEFAULT 0,
    nickname TEXT
);

CREATE TABLE IF NOT EXISTS contact_discovery_policy (
    user_id BLOB PRIMARY KEY,
    protocol_version INTEGER NOT NULL,
    enabled INTEGER NOT NULL,
    revision INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS friend_directory_state (
    introducer_user_id BLOB PRIMARY KEY,
    applied_revision INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS friend_suggestions (
    candidate_user_id BLOB NOT NULL,
    introducer_user_id BLOB NOT NULL,
    name TEXT NOT NULL,
    sign_pk BLOB NOT NULL,
    agree_pk BLOB NOT NULL,
    candidate_policy_revision INTEGER NOT NULL,
    ticket BLOB NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    PRIMARY KEY(candidate_user_id, introducer_user_id)
);
CREATE INDEX IF NOT EXISTS idx_friend_suggestions_introducer
    ON friend_suggestions(introducer_user_id);

CREATE TABLE IF NOT EXISTS friend_suggestion_state (
    candidate_user_id BLOB PRIMARY KEY,
    state INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS contact_provenance (
    user_id BLOB PRIMARY KEY,
    source INTEGER NOT NULL,
    introducer_user_id BLOB,
    introduced_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS groups (
    group_id             BLOB PRIMARY KEY,
    name                 TEXT NOT NULL,
    group_key            BLOB NOT NULL,
    metadata_revision    INTEGER NOT NULL DEFAULT 0,
    metadata_changed_by  BLOB NOT NULL DEFAULT X''
);

CREATE TABLE IF NOT EXISTS group_members (
    group_id BLOB NOT NULL,
    user_id  BLOB NOT NULL,
    PRIMARY KEY(group_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_group_members_user_id ON group_members(user_id);

CREATE TABLE IF NOT EXISTS receipts (
    chat_id         BLOB NOT NULL,
    sender_user_id  BLOB NOT NULL,
    receipt_type    INTEGER NOT NULL,
    through_lamport INTEGER NOT NULL,
    via_transport   INTEGER,
    PRIMARY KEY(chat_id, sender_user_id, receipt_type)
);

CREATE TABLE IF NOT EXISTS outgoing_receipts (
    chat_id         BLOB NOT NULL,
    sender_user_id  BLOB NOT NULL,
    receipt_type    INTEGER NOT NULL,
    through_lamport INTEGER NOT NULL,
    PRIMARY KEY(chat_id, sender_user_id, receipt_type)
);

-- V2 field metrics: a local, metadata-only ledger for the cruise test.
-- One row per message we sent or received, keyed by an 8-byte hash of the
-- chat id (never the raw id), an 8-byte hash of the sender (FC1: in a group,
-- every member has an independent lamport stream, so two senders routinely
-- share a lamport value in the same chat -- the sender hash keeps their
-- arrivals from colliding on the primary key), and our/their lamport. No
-- message content is ever stored here. `direction` 0 = we sent it, 1 = we
-- received it.
CREATE TABLE IF NOT EXISTS delivery_metrics (
    chat_hash         BLOB NOT NULL,
    lamport           INTEGER NOT NULL,
    direction         INTEGER NOT NULL,
    sender_hash       BLOB NOT NULL,
    at_ms             INTEGER NOT NULL,
    delivered_at_ms   INTEGER,
    via_transport     INTEGER,
    arrival_transport INTEGER,
    hop_count         INTEGER,
    PRIMARY KEY(chat_hash, lamport, direction, sender_hash)
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
    size_bytes     INTEGER NOT NULL,
    from_relay     INTEGER NOT NULL DEFAULT 0,
    content_digest BLOB
);
CREATE INDEX IF NOT EXISTS idx_carried_hint ON carried_envelopes(recipient_hint);
CREATE INDEX IF NOT EXISTS idx_carried_expiry ON carried_envelopes(expiry);
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        create_introduction_ticket, generate_identity, FriendDirectoryEntry, Group,
        KIND_GROUP_INVITE, RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
    };
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

    fn outbound_for(
        message: &StoredMessage,
        recipient_user_id: &[u8],
        msg_id: &[u8],
    ) -> OutboundEnvelope {
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

    fn group(id_byte: u8, name: &str, key_byte: u8, members: &[&[u8]]) -> Group {
        Group {
            id: vec![id_byte; 16],
            name: name.to_string(),
            member_user_ids: members.iter().map(|member| test_user_id(member)).collect(),
            key: vec![key_byte; 32],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        }
    }

    fn test_user_id(label: &[u8]) -> Vec<u8> {
        let mut user_id = vec![0; 16];
        let count = label.len().min(user_id.len());
        user_id[..count].copy_from_slice(&label[..count]);
        user_id
    }

    #[test]
    fn insert_then_fetch_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "there"))
            .unwrap();

        let messages = store.messages_for_chat(b"chat-a".to_vec()).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].payload, b"hi");
        assert_eq!(messages[1].payload, b"there");
    }

    #[test]
    fn incoming_message_reference_round_trips_and_resolves_quote_target() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let original = msg(b"chat-a", b"alice", 1, "first");
        let original_id = vec![1; MESSAGE_ID_LEN];
        assert!(store
            .insert_incoming_message(original.clone(), original_id.clone(), None)
            .unwrap());

        let reply = msg(b"chat-a", b"alice", 2, "second");
        let reply_id = vec![2; MESSAGE_ID_LEN];
        assert!(store
            .insert_incoming_message(reply.clone(), reply_id.clone(), Some(original_id.clone()),)
            .unwrap());
        assert_eq!(
            store
                .message_reference(
                    reply.chat_id.clone(),
                    reply.sender_user_id.clone(),
                    reply.lamport,
                )
                .unwrap(),
            Some(MessageReference {
                msg_id: reply_id,
                reply_to_msg_id: Some(original_id.clone()),
            }),
        );
        assert_eq!(
            store
                .message_by_msg_id(reply.chat_id.clone(), original_id)
                .unwrap(),
            Some(original),
        );

        // A redundant copy cannot replace the first stable envelope id.
        assert!(!store
            .insert_incoming_message(
                reply.clone(),
                vec![3; MESSAGE_ID_LEN],
                Some(vec![1; MESSAGE_ID_LEN]),
            )
            .unwrap());
        assert_eq!(
            store
                .message_reference(reply.chat_id, reply.sender_user_id, reply.lamport)
                .unwrap()
                .unwrap()
                .msg_id,
            vec![2; MESSAGE_ID_LEN],
        );
    }

    #[test]
    fn message_origin_by_msg_id_finds_a_one_to_one_row_by_msg_id_alone() {
        // Unlike `message_by_msg_id`, no `chat_id` is supplied -- this is the
        // relay ack-decision path, which only ever has the envelope's
        // `msg_id` in hand. A 1:1 incoming row follows the local convention
        // "chat keyed by the other party": chat_id == sender_user_id, which
        // is exactly what the ack-decision helper keys off.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let incoming = msg(b"alice", b"alice", 1, "hi");
        let incoming_id = vec![7; MESSAGE_ID_LEN];
        store
            .insert_incoming_message(incoming.clone(), incoming_id.clone(), None)
            .unwrap();

        assert_eq!(
            store.message_origin_by_msg_id(incoming_id).unwrap(),
            Some(MessageOrigin {
                chat_id: b"alice".to_vec(),
                sender_user_id: b"alice".to_vec(),
            }),
        );
    }

    #[test]
    fn message_origin_by_msg_id_reports_a_group_row_with_its_group_chat_id() {
        // A consumed group message is stored under chat_id = group id with
        // sender_user_id = the authoring member, so chat_id !=
        // sender_user_id. The origin must surface both fields untouched: the
        // ack-decision helper relies on that inequality to refuse to ack a
        // group envelope off the shared family relay mailbox (other members
        // still need the relay copy).
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let group_message = msg(b"group-1", b"alice", 1, "hi group");
        let group_msg_id = vec![10; MESSAGE_ID_LEN];
        store
            .insert_incoming_message(group_message, group_msg_id.clone(), None)
            .unwrap();

        assert_eq!(
            store.message_origin_by_msg_id(group_msg_id).unwrap(),
            Some(MessageOrigin {
                chat_id: b"group-1".to_vec(),
                sender_user_id: b"alice".to_vec(),
            }),
        );
    }

    #[test]
    fn message_origin_by_msg_id_returns_none_for_an_unknown_msg_id() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(
            store
                .message_origin_by_msg_id(vec![0xEE; MESSAGE_ID_LEN])
                .unwrap(),
            None,
        );
    }

    #[test]
    fn message_origin_by_msg_id_returns_our_own_id_for_authored_outbound_messages() {
        // Our own outbound envelope also has a `messages` row (via
        // `insert_outgoing_message`), with `sender_user_id == us`. The store
        // deliberately does NOT filter this out -- it's the caller's job
        // (`engine::consumed_seen_is_ackable`) to compare the returned
        // sender against its own identity and refuse to ack an own-authored
        // envelope, since that relay copy exists for the recipient, not us.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"self", 1, "hello");
        let envelope = outbound_for(&message, b"recipient", &[8; MESSAGE_ID_LEN]);
        store
            .insert_outgoing_message(message, envelope, 1_700_000_000_000)
            .unwrap();

        assert_eq!(
            store
                .message_origin_by_msg_id(vec![8; MESSAGE_ID_LEN])
                .unwrap(),
            Some(MessageOrigin {
                chat_id: b"chat-a".to_vec(),
                sender_user_id: b"self".to_vec(),
            }),
        );
    }

    #[test]
    fn message_origin_by_msg_id_does_not_match_hidden_kind_rows_with_no_msg_id() {
        // Hidden kinds (receipts, profile sync, friend requests/directory,
        // group invites, LAN endpoint hints) are stored via plain
        // `insert_message`, which never records a `msg_id` -- so they must
        // never spuriously match a real envelope's `msg_id` here.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "hidden-kind-payload"))
            .unwrap();

        assert_eq!(
            store
                .message_origin_by_msg_id(vec![9; MESSAGE_ID_LEN])
                .unwrap(),
            None,
        );
    }

    #[test]
    fn outgoing_reply_persists_reference_atomically() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"self", 1, "reply");
        let envelope = outbound_for(&message, b"recipient", &[4; MESSAGE_ID_LEN]);
        let reply_to = vec![5; MESSAGE_ID_LEN];
        assert!(store
            .insert_outgoing_reply(message.clone(), envelope, reply_to.clone(), 1_000)
            .unwrap());
        assert_eq!(
            store
                .message_reference(message.chat_id, message.sender_user_id, message.lamport)
                .unwrap(),
            Some(MessageReference {
                msg_id: vec![4; MESSAGE_ID_LEN],
                reply_to_msg_id: Some(reply_to),
            }),
        );
    }

    #[test]
    fn open_backfills_authored_message_ids_from_the_outbound_queue() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cruisemesh-store-message-reference-backfill-{unique}.sqlite"
        ));
        let path_str = path.to_string_lossy().to_string();
        let message = msg(b"chat-a", b"self", 1, "sent before upgrade");
        let msg_id = vec![6; MESSAGE_ID_LEN];

        let store = MessageStore::open(path_str.clone()).unwrap();
        store
            .insert_outgoing_message(
                message.clone(),
                outbound_for(&message, b"recipient", &msg_id),
                1_000,
            )
            .unwrap();
        drop(store);

        // Model the old schema's message row after the columns are added but
        // before open() performs its outbound-queue backfill.
        let conn = Connection::open(&path_str).unwrap();
        conn.execute("UPDATE messages SET msg_id = NULL", [])
            .unwrap();
        drop(conn);

        let reopened = MessageStore::open(path_str).unwrap();
        assert_eq!(
            reopened
                .message_reference(message.chat_id, message.sender_user_id, message.lamport)
                .unwrap(),
            Some(MessageReference {
                msg_id,
                reply_to_msg_id: None,
            }),
        );

        drop(reopened);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn incoming_references_require_fixed_width_ids() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(matches!(
            store.insert_incoming_message(
                msg(b"chat-a", b"alice", 1, "hi"),
                vec![1; MESSAGE_ID_LEN - 1],
                None,
            ),
            Err(CoreError::Malformed(_))
        ));
    }

    #[test]
    fn message_arrival_records_first_route_without_changing_message_shape() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        store.insert_message(message.clone()).unwrap();
        let first = MessageArrival {
            transport: 1,
            hops_taken: 2,
            received_at: 1_700_000_000_500,
        };
        assert!(store
            .record_message_arrival(
                message.chat_id.clone(),
                message.sender_user_id.clone(),
                message.lamport,
                first.clone(),
            )
            .unwrap());
        assert_eq!(
            store
                .message_arrival(
                    message.chat_id.clone(),
                    message.sender_user_id.clone(),
                    message.lamport,
                )
                .unwrap(),
            Some(first),
        );

        assert!(!store
            .record_message_arrival(
                message.chat_id.clone(),
                message.sender_user_id.clone(),
                message.lamport,
                MessageArrival {
                    transport: 2,
                    hops_taken: 7,
                    received_at: 1_700_000_999_999,
                },
            )
            .unwrap());
        assert_eq!(
            store.messages_for_chat(message.chat_id.clone()).unwrap(),
            vec![message],
        );
    }

    #[test]
    fn message_arrival_accepts_lan_routes_and_rejects_unknown_routes() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        store.insert_message(message.clone()).unwrap();

        assert!(store
            .record_message_arrival(
                message.chat_id.clone(),
                message.sender_user_id.clone(),
                message.lamport,
                MessageArrival {
                    transport: 4,
                    hops_taken: 1,
                    received_at: 1_700_000_000_500,
                },
            )
            .unwrap());

        assert!(matches!(
            store.record_message_arrival(
                message.chat_id,
                message.sender_user_id,
                message.lamport,
                MessageArrival {
                    transport: 5,
                    hops_taken: 0,
                    received_at: 1_700_000_000_600,
                },
            ),
            Err(CoreError::Malformed(_))
        ));
    }

    #[test]
    fn message_arrival_is_absent_for_legacy_or_outgoing_rows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        store.insert_message(message.clone()).unwrap();
        assert_eq!(
            store
                .message_arrival(
                    message.chat_id.clone(),
                    message.sender_user_id.clone(),
                    message.lamport,
                )
                .unwrap(),
            None,
        );
        assert_eq!(
            store
                .message_reference(message.chat_id, message.sender_user_id, message.lamport)
                .unwrap(),
            None,
        );
    }

    #[test]
    fn open_migrates_old_messages_table_to_add_arrival_and_reference_columns() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cruisemesh-store-migration-message-arrival-{unique}.sqlite"
        ));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE messages (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                chat_id        BLOB NOT NULL,
                sender_user_id BLOB NOT NULL,
                lamport        INTEGER NOT NULL,
                timestamp      INTEGER NOT NULL,
                kind           INTEGER NOT NULL,
                payload        BLOB NOT NULL,
                UNIQUE(chat_id, sender_user_id, lamport)
            );
            INSERT INTO messages
                (chat_id, sender_user_id, lamport, timestamp, kind, payload)
            VALUES
                (x'636861742D61', x'616C696365', 1, 1700000000000, 0, x'6869');
            ",
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str).unwrap();
        let arrival = MessageArrival {
            transport: 2,
            hops_taken: 3,
            received_at: 1_700_000_000_500,
        };
        assert!(store
            .record_message_arrival(b"chat-a".to_vec(), b"alice".to_vec(), 1, arrival.clone(),)
            .unwrap());
        assert_eq!(
            store
                .message_arrival(b"chat-a".to_vec(), b"alice".to_vec(), 1)
                .unwrap(),
            Some(arrival),
        );

        let legacy = StoredMessage {
            chat_id: b"chat-a".to_vec(),
            sender_user_id: b"alice".to_vec(),
            lamport: 1,
            timestamp: 1_700_000_000_000,
            kind: 0,
            payload: b"hi".to_vec(),
        };
        let legacy_id = vec![9; MESSAGE_ID_LEN];
        assert!(!store
            .insert_incoming_message(legacy, legacy_id.clone(), None)
            .unwrap());
        assert_eq!(
            store
                .message_reference(b"chat-a".to_vec(), b"alice".to_vec(), 1)
                .unwrap(),
            Some(MessageReference {
                msg_id: legacy_id,
                reply_to_msg_id: None,
            }),
        );

        drop(store);
        fs::remove_file(path).unwrap();
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
        assert!(store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap());
        // Re-delivery of the same envelope (expected under DTN): no-op, not an error.
        assert!(!store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap());
        assert_eq!(
            store.messages_for_chat(b"chat-a".to_vec()).unwrap().len(),
            1
        );
    }

    #[test]
    fn writes_reject_values_that_sqlite_cannot_represent() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(store
            .insert_message(msg(b"chat-a", b"alice", i64::MAX as u64 + 1, "bad"))
            .is_err());
        assert!(store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                i64::MAX as u64 + 1,
                None,
            )
            .is_err());
        assert!(store
            .record_receipt(b"chat-a".to_vec(), b"alice".to_vec(), 0xff, 1, None)
            .is_err());
        assert!(store
            .upsert_outgoing_receipt_envelope(
                outgoing_receipt_for(
                    b"chat-a",
                    b"alice",
                    b"bob",
                    RECEIPT_TYPE_DELIVERED,
                    i64::MAX as u64 + 1,
                    &[9; 16],
                ),
                1,
            )
            .is_err());
        assert!(store
            .messages_for_chat(b"chat-a".to_vec())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn insert_message_true_duplicate_is_ignored() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(store
            .insert_message(msg(b"chat-a", b"alice", 3, "hi"))
            .unwrap());
        // Same (chat_id, sender_user_id, lamport) *and* same (timestamp,
        // kind, payload) -- a true duplicate (digest resend / relay replay
        // of the identical sealed message), not a fork. No-op, row count
        // unchanged.
        assert!(!store
            .insert_message(msg(b"chat-a", b"alice", 3, "hi"))
            .unwrap());
        assert_eq!(
            store.messages_for_chat(b"chat-a".to_vec()).unwrap().len(),
            1
        );
    }

    #[test]
    fn insert_message_fork_recovers_abandoned_tail_and_resets_outgoing_receipts() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // Alice's original stream: messages 1..5, and we'd told her (via
        // outgoing_receipts) that we'd read through 5.
        for lamport in 1..=5u64 {
            store
                .insert_message(msg(b"chat-a", b"alice", lamport, "old"))
                .unwrap();
        }
        store
            .record_outgoing_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
                5,
            )
            .unwrap();
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_READ,
                )
                .unwrap(),
            5
        );

        // Alice deleted the chat and re-added us: her lamport counter
        // restarted, so she resends a genuinely new message 3 with different
        // content/timestamp -- a fork, not a duplicate of the old message 3.
        let mut forked = msg(b"chat-a", b"alice", 3, "new-after-reset");
        forked.timestamp = 1_700_000_500_000;
        assert!(store.insert_message(forked).unwrap());

        // Old messages 3, 4, 5 are gone; the new message 3 replaces them.
        let remaining = store.messages_for_chat(b"chat-a".to_vec()).unwrap();
        assert_eq!(remaining.len(), 3); // 1, 2, and the new 3
        let three = remaining.iter().find(|m| m.lamport == 3).unwrap();
        assert_eq!(three.payload, b"new-after-reset");
        assert_eq!(three.timestamp, 1_700_000_500_000);
        assert!(remaining.iter().all(|m| m.lamport != 4 && m.lamport != 5));

        // Our contiguous view of Alice's stream is now capped at the fork point.
        assert_eq!(
            store
                .highest_contiguous_lamport(b"chat-a".to_vec(), b"alice".to_vec())
                .unwrap(),
            3
        );

        // The stale "read through 5" watermark about Alice's *old* stream is
        // cleared -- it was overstated relative to her reset stream and would
        // otherwise keep painting false checkmarks on messages she hasn't
        // sent yet under the new numbering.
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_READ,
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn insert_message_fork_recovery_does_not_touch_receipts_table() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        for lamport in 1..=5u64 {
            store
                .insert_message(msg(b"chat-a", b"alice", lamport, "old"))
                .unwrap();
        }
        // `receipts` in this chat is the peer's ack of *our own* outgoing
        // stream ("self") -- unrelated to alice's stream resetting -- and
        // must survive her fork recovery untouched.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"self".to_vec(),
                crate::RECEIPT_TYPE_READ,
                2,
                None,
            )
            .unwrap();

        let mut forked = msg(b"chat-a", b"alice", 3, "new-after-reset");
        forked.timestamp = 1_700_000_500_000;
        assert!(store.insert_message(forked).unwrap());

        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"self".to_vec(),
                    crate::RECEIPT_TYPE_READ
                )
                .unwrap(),
            2
        );
    }

    #[test]
    fn messages_are_isolated_per_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"chat-b", b"alice", 1, "yo"))
            .unwrap();

        assert_eq!(
            store.messages_for_chat(b"chat-a".to_vec()).unwrap().len(),
            1
        );
        assert_eq!(
            store.messages_for_chat(b"chat-b".to_vec()).unwrap().len(),
            1
        );
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
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "one"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "two"))
            .unwrap();
        // lamport 3 is missing -- message 4 arrived out of order (DTN reality).
        store
            .insert_message(msg(b"chat-a", b"alice", 4, "four"))
            .unwrap();

        let n = store
            .highest_contiguous_lamport(b"chat-a".to_vec(), b"alice".to_vec())
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn highest_contiguous_lamport_is_per_sender_not_per_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "there"))
            .unwrap();
        // Bob's own counter in the same chat starts independently at 1.
        store
            .insert_message(msg(b"chat-a", b"bob", 1, "hey"))
            .unwrap();

        assert_eq!(
            store
                .highest_contiguous_lamport(b"chat-a".to_vec(), b"alice".to_vec())
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .highest_contiguous_lamport(b"chat-a".to_vec(), b"bob".to_vec())
                .unwrap(),
            1
        );
    }

    #[test]
    fn highest_lamport_is_zero_with_no_messages() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let n = store
            .highest_lamport(b"chat-a".to_vec(), b"alice".to_vec())
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn highest_lamport_is_max_across_a_front_gap() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // Lamports 1 and 2 never existed for alice -- e.g. her stream base
        // was ratcheted forward after a chat history wipe -- so her stream
        // legitimately starts at 3. `highest_contiguous_lamport` would
        // report 0 here (nothing contiguous from 1); `highest_lamport`
        // reports what she's actually sent.
        store
            .insert_message(msg(b"chat-a", b"alice", 3, "three"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 4, "four"))
            .unwrap();

        let n = store
            .highest_lamport(b"chat-a".to_vec(), b"alice".to_vec())
            .unwrap();
        assert_eq!(n, 4);
    }

    #[test]
    fn highest_lamport_is_max_across_an_internal_gap() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "one"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "two"))
            .unwrap();
        // lamport 3 is missing -- message 4 arrived out of order (DTN
        // reality) -- but we still hold the max the sender has reached.
        store
            .insert_message(msg(b"chat-a", b"alice", 4, "four"))
            .unwrap();

        let n = store
            .highest_lamport(b"chat-a".to_vec(), b"alice".to_vec())
            .unwrap();
        assert_eq!(n, 4);
    }

    #[test]
    fn highest_lamport_is_per_sender_not_per_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "there"))
            .unwrap();
        // Bob's own counter in the same chat starts independently at 1.
        store
            .insert_message(msg(b"chat-a", b"bob", 1, "hey"))
            .unwrap();

        assert_eq!(
            store
                .highest_lamport(b"chat-a".to_vec(), b"alice".to_vec())
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .highest_lamport(b"chat-a".to_vec(), b"bob".to_vec())
                .unwrap(),
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

        assert_eq!(
            store.messages_for_chat(b"chat-a".to_vec()).unwrap(),
            vec![message]
        );
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
        assert_eq!(
            store.messages_for_chat(b"chat-a".to_vec()).unwrap(),
            vec![message]
        );
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
    fn group_invites_can_queue_one_pairwise_envelope_per_recipient() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut invite = msg(&vec![0x11; 16], b"alice", 7, "group invite");
        invite.kind = KIND_GROUP_INVITE;
        let first = outbound_for(&invite, b"bob", b"msg-000000000011");
        let second = outbound_for(&invite, b"carol", b"msg-000000000012");

        assert!(store
            .insert_outgoing_message(invite.clone(), first.clone(), 1_700_000_000_100)
            .unwrap());
        assert!(store
            .insert_outgoing_message(invite.clone(), second.clone(), 1_700_000_000_200)
            .unwrap());

        assert_eq!(
            store.messages_for_chat(invite.chat_id.clone()).unwrap(),
            vec![invite]
        );
        let mut recipients: Vec<Vec<u8>> = store
            .outbound_envelopes_after(vec![0x11; 16], b"alice".to_vec(), 0)
            .unwrap()
            .into_iter()
            .map(|envelope| envelope.recipient_user_id)
            .collect();
        recipients.sort();
        assert_eq!(recipients, vec![b"bob".to_vec(), b"carol".to_vec()]);
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

        store
            .insert_outgoing_message(live, live_env.clone(), 1_000)
            .unwrap();
        store
            .insert_outgoing_message(stale, stale_env, 1_100)
            .unwrap();
        store
            .insert_outgoing_message(posted, posted_env.clone(), 1_200)
            .unwrap();
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
            store
                .pending_relay_outgoing_receipt_envelopes(10, 3_000)
                .unwrap(),
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
            store
                .pending_relay_outgoing_receipt_envelopes(10, 2_000)
                .unwrap(),
            vec![live],
        );
    }

    #[test]
    fn family_carried_envelopes_return_only_unexpired_family_rows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(carried(b"fam", b"h1", 9_000, 10), true, 1_000, BIG_BUDGET)
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"foreign", b"h2", 9_000, 10),
                false,
                2_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"expired", b"h3", 1_500, 10),
                true,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();

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
            nickname: None,
        }
    }

    #[test]
    fn contact_discovery_policy_only_moves_forward() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let id = vec![1; 16];
        assert!(store
            .upsert_contact_discovery_policy(ContactDiscoveryPolicy {
                user_id: id.clone(),
                protocol_version: 1,
                enabled: true,
                revision: 5,
            })
            .unwrap());
        assert!(!store
            .upsert_contact_discovery_policy(ContactDiscoveryPolicy {
                user_id: id.clone(),
                protocol_version: 1,
                enabled: false,
                revision: 4,
            })
            .unwrap());
        let policy = store.get_contact_discovery_policy(id).unwrap().unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.revision, 5);
    }

    #[test]
    fn friend_directory_replaces_one_source_and_honors_suppression() {
        let alice = generate_identity();
        let bob = generate_identity();
        let carol = generate_identity();
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .upsert_contact(Contact {
                user_id: alice.user_id.clone(),
                name: "Alice".to_string(),
                sign_pk: alice.sign_pk.clone(),
                agree_pk: alice.agree_pk.clone(),
                relay_url: None,
                relay_token: None,
                nickname: None,
            })
            .unwrap();
        let ticket = create_introduction_ticket(
            alice.clone(),
            carol.user_id.clone(),
            bob.user_id.clone(),
            3,
            1_000,
            100_000,
            vec![9; 16],
        )
        .unwrap();
        let directory = FriendDirectoryContent {
            version: 1,
            revision: 1,
            entries: vec![FriendDirectoryEntry {
                candidate: SuggestedFriendCard {
                    name: "Carol".to_string(),
                    user_id: carol.user_id.clone(),
                    sign_pk: carol.sign_pk.clone(),
                    agree_pk: carol.agree_pk.clone(),
                },
                candidate_policy_revision: 3,
                ticket,
            }],
        };
        assert!(store
            .apply_friend_directory(
                alice.user_id.clone(),
                bob.user_id.clone(),
                directory.clone(),
                2_000,
            )
            .unwrap());
        assert!(!store
            .apply_friend_directory(alice.user_id.clone(), vec![2; 16], directory, 2_000,)
            .unwrap());
        let suggestions = store.list_friend_suggestions(2_000).unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].candidate.name, "Carol");

        store
            .set_friend_suggestion_state(suggestions[0].candidate.user_id.clone(), 1)
            .unwrap();
        let newer_ticket = create_introduction_ticket(
            alice.clone(),
            carol.user_id.clone(),
            bob.user_id.clone(),
            3,
            2_000,
            101_000,
            vec![10; 16],
        )
        .unwrap();
        assert!(store
            .apply_friend_directory(
                alice.user_id.clone(),
                bob.user_id,
                FriendDirectoryContent {
                    version: 1,
                    revision: 2,
                    entries: vec![FriendDirectoryEntry {
                        candidate: SuggestedFriendCard {
                            name: "Carol".to_string(),
                            user_id: carol.user_id,
                            sign_pk: carol.sign_pk,
                            agree_pk: carol.agree_pk,
                        },
                        candidate_policy_revision: 3,
                        ticket: newer_ticket,
                    }],
                },
                2_000,
            )
            .unwrap());
        let suggestions = store.list_friend_suggestions(2_000).unwrap();
        assert_eq!(suggestions[0].state, 0);

        store
            .set_friend_suggestion_state(suggestions[0].candidate.user_id.clone(), 2)
            .unwrap();
        assert!(store.list_friend_suggestions(2_000).unwrap().is_empty());

        assert!(store
            .apply_friend_directory(
                alice.user_id,
                vec![2; 16],
                FriendDirectoryContent {
                    version: 1,
                    revision: 3,
                    entries: Vec::new(),
                },
                2_000,
            )
            .unwrap());
    }

    #[test]
    fn direct_provenance_cannot_be_downgraded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let user_id = vec![3; 16];
        store
            .upsert_contact_provenance(ContactProvenance {
                user_id: user_id.clone(),
                source: 0,
                introducer_user_id: None,
                introduced_at_ms: 10,
            })
            .unwrap();
        store
            .upsert_contact_provenance(ContactProvenance {
                user_id: user_id.clone(),
                source: 1,
                introducer_user_id: Some(vec![4; 16]),
                introduced_at_ms: 20,
            })
            .unwrap();
        let provenance = store.get_contact_provenance(user_id).unwrap().unwrap();
        assert_eq!(provenance.source, 0);
        assert!(provenance.introducer_user_id.is_none());
        assert_eq!(provenance.introduced_at_ms, 10);
    }

    #[test]
    fn open_migrates_an_old_contacts_table_to_add_relay_token() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
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
            params![
                b"alice-id".to_vec(),
                "Alice",
                vec![1u8; 32],
                vec![2u8; 32],
                "https://relay.example"
            ],
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str.clone()).unwrap();
        let migrated = store.get_contact(b"alice-id".to_vec()).unwrap().unwrap();
        assert_eq!(
            migrated.relay_url,
            Some("https://relay.example".to_string())
        );
        assert_eq!(migrated.relay_token, None);

        let mut updated = migrated.clone();
        updated.relay_token = Some("family-token".to_string());
        store.upsert_contact(updated.clone()).unwrap();
        assert_eq!(
            store.get_contact(b"alice-id".to_vec()).unwrap(),
            Some(updated)
        );

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn set_contact_nickname_round_trips_and_blank_clears() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        assert!(store
            .set_contact_nickname(b"alice-id".to_vec(), Some("  Mom  ".to_string()))
            .unwrap());
        // Whitespace is trimmed on the way in.
        assert_eq!(
            store
                .get_contact(b"alice-id".to_vec())
                .unwrap()
                .unwrap()
                .nickname,
            Some("Mom".to_string())
        );

        // A blank value clears the nickname.
        assert!(store
            .set_contact_nickname(b"alice-id".to_vec(), Some("   ".to_string()))
            .unwrap());
        assert_eq!(
            store
                .get_contact(b"alice-id".to_vec())
                .unwrap()
                .unwrap()
                .nickname,
            None
        );

        // Unknown contact reports no change.
        assert!(!store
            .set_contact_nickname(b"nobody".to_vec(), Some("X".to_string()))
            .unwrap());
    }

    #[test]
    fn reimporting_a_friend_card_preserves_the_local_nickname() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        store
            .set_contact_nickname(b"alice-id".to_vec(), Some("Mom".to_string()))
            .unwrap();

        // Re-importing the card (e.g. a re-scan) carries no nickname and must
        // not erase the local one.
        let mut card = contact(b"alice-id", "Alice iPhone");
        card.relay_url = Some("https://relay.example".to_string());
        card.relay_token = Some("family".to_string());
        store.upsert_imported_contact(card).unwrap();

        let after = store.get_contact(b"alice-id".to_vec()).unwrap().unwrap();
        assert_eq!(after.nickname, Some("Mom".to_string()));
        // The card name still updated; only the nickname is sticky.
        assert_eq!(after.name, "Alice iPhone");
    }

    #[test]
    fn contact_display_name_prefers_a_nonblank_nickname() {
        let mut c = contact(b"alice-id", "Alice");
        assert_eq!(core_contact_display_name(c.clone()), "Alice");

        c.nickname = Some("Mom".to_string());
        assert_eq!(core_contact_display_name(c.clone()), "Mom");

        // A blank nickname falls back to the card name.
        c.nickname = Some("   ".to_string());
        assert_eq!(core_contact_display_name(c), "Alice");
    }

    #[test]
    fn open_migrates_contacts_table_to_add_avatar_columns() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("cruisemesh-store-avatar-migration-{unique}.sqlite"));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE contacts (
                user_id   BLOB PRIMARY KEY,
                name      TEXT NOT NULL,
                sign_pk   BLOB NOT NULL,
                agree_pk  BLOB NOT NULL,
                relay_url TEXT,
                relay_token TEXT
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO contacts (user_id, name, sign_pk, agree_pk, relay_url, relay_token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                b"alice-id".to_vec(),
                "Alice",
                vec![1u8; 32],
                vec![2u8; 32],
                "https://relay.example",
                "token"
            ],
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str.clone()).unwrap();
        assert_eq!(store.contact_avatar(b"alice-id".to_vec()).unwrap(), None);
        assert_eq!(store.contact_avatar_epoch(b"alice-id".to_vec()).unwrap(), 0);
        assert!(store
            .set_contact_avatar(b"alice-id".to_vec(), Some(vec![1, 2, 3]), 10)
            .unwrap());
        assert_eq!(
            store.contact_avatar(b"alice-id".to_vec()).unwrap(),
            Some(vec![1, 2, 3])
        );

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn upsert_then_get_contact_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        let found = store
            .get_contact(b"alice-id".to_vec())
            .unwrap()
            .expect("contact exists");
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
        store
            .upsert_contact(contact(b"alice-id", "Alice R."))
            .unwrap();

        let contacts = store.list_contacts().unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].name, "Alice R.");
    }

    #[test]
    fn upsert_contact_preserves_avatar_and_epoch() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        assert!(store
            .set_contact_avatar(b"alice-id".to_vec(), Some(vec![9, 8, 7]), 123)
            .unwrap());

        let mut updated = contact(b"alice-id", "Alice R.");
        updated.relay_url = Some("https://relay.example".to_string());
        store.upsert_contact(updated).unwrap();

        assert_eq!(
            store.contact_avatar(b"alice-id".to_vec()).unwrap(),
            Some(vec![9, 8, 7])
        );
        assert_eq!(
            store.contact_avatar_epoch(b"alice-id".to_vec()).unwrap(),
            123
        );
    }

    #[test]
    fn imported_blank_card_preserves_existing_relay_details() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut original = contact(b"alice-id", "Alice");
        original.relay_url = Some("https://relay.example".to_string());
        original.relay_token = Some("family-token".to_string());
        store.upsert_contact(original).unwrap();

        let imported = store
            .upsert_imported_contact(contact(b"alice-id", "Alice Renamed"))
            .unwrap();
        assert_eq!(imported.relay_url.as_deref(), Some("https://relay.example"));
        assert_eq!(imported.relay_token.as_deref(), Some("family-token"));
        assert_eq!(
            store.get_contact(b"alice-id".to_vec()).unwrap(),
            Some(imported)
        );
    }

    #[test]
    fn imported_complete_relay_replaces_existing_details() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut original = contact(b"alice-id", "Alice");
        original.relay_url = Some("https://old.example".to_string());
        original.relay_token = Some("old".to_string());
        store.upsert_contact(original).unwrap();

        let mut incoming = contact(b"alice-id", "Alice");
        incoming.relay_url = Some("https://new.example".to_string());
        incoming.relay_token = Some("new".to_string());
        let imported = store.upsert_imported_contact(incoming).unwrap();
        assert_eq!(imported.relay_url.as_deref(), Some("https://new.example"));
        assert_eq!(imported.relay_token.as_deref(), Some("new"));
    }

    #[test]
    fn set_contact_avatar_applies_only_newer_epochs() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        assert!(store
            .set_contact_avatar(b"alice-id".to_vec(), Some(vec![1]), 100)
            .unwrap());
        assert!(!store
            .set_contact_avatar(b"alice-id".to_vec(), Some(vec![2]), 99)
            .unwrap());
        assert!(!store
            .set_contact_avatar(b"alice-id".to_vec(), Some(vec![3]), 100)
            .unwrap());
        assert_eq!(
            store.contact_avatar(b"alice-id".to_vec()).unwrap(),
            Some(vec![1])
        );
        assert_eq!(
            store.contact_avatar_epoch(b"alice-id".to_vec()).unwrap(),
            100
        );

        assert!(store
            .set_contact_avatar(b"alice-id".to_vec(), Some(Vec::new()), 101)
            .unwrap());
        assert_eq!(store.contact_avatar(b"alice-id".to_vec()).unwrap(), None);
        assert_eq!(
            store.contact_avatar_epoch(b"alice-id".to_vec()).unwrap(),
            101
        );
    }

    #[test]
    fn set_contact_avatar_unknown_contact_is_noop() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(!store
            .set_contact_avatar(b"nobody".to_vec(), Some(vec![1]), 1)
            .unwrap());
        assert_eq!(store.contact_avatar_epoch(b"nobody".to_vec()).unwrap(), 0);
        assert_eq!(store.contact_avatar(b"nobody".to_vec()).unwrap(), None);
    }

    #[test]
    fn list_contacts_is_alphabetical() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"bob-id", "Bob")).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        let names: Vec<String> = store
            .list_contacts()
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
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
        store
            .insert_message(msg(b"alice-id", b"alice-id", 1, "from alice"))
            .unwrap();
        store
            .insert_message(msg(b"alice-id", b"me", 1, "from me"))
            .unwrap();
        store
            .record_receipt(
                b"alice-id".to_vec(),
                b"me".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                1,
                None,
            )
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

        assert!(store
            .messages_for_chat(b"alice-id".to_vec())
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .receipt_through(
                    b"alice-id".to_vec(),
                    b"me".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
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
        store
            .insert_message(msg(b"alice-id", b"alice-id", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"bob-id", b"bob-id", 1, "yo"))
            .unwrap();
        // A group chat where alice posted: her group messages belong to the
        // group's chat_id, not to her contact, and must survive.
        store
            .insert_message(msg(b"group-1", b"alice-id", 1, "group msg"))
            .unwrap();

        assert!(store.delete_contact(b"alice-id".to_vec()).unwrap());

        assert_eq!(store.list_contacts().unwrap().len(), 1);
        assert_eq!(
            store.messages_for_chat(b"bob-id".to_vec()).unwrap().len(),
            1
        );
        assert_eq!(
            store.messages_for_chat(b"group-1".to_vec()).unwrap().len(),
            1
        );
        assert!(store
            .messages_for_chat(b"alice-id".to_vec())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn delete_contact_purges_all_per_chat_state_and_leaves_other_chat_alone() {
        // Regression test for the silent-blackhole-adjacent bug: a delete
        // that only cleared `messages`/`receipts`/`outgoing_receipts` left
        // `outgoing_receipt_envelopes` and `outbound_envelopes` behind,
        // re-arming the reset-stream trap fixed in fc6b9f9 and leaving a
        // "deleted" chat that could still resend frames from its erased
        // history. Seed every one of the five per-chat tables for two
        // contacts, delete one, and verify a genuinely blank slate for it
        // while the other contact's chat is untouched.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        store.upsert_contact(contact(b"bob-id", "Bob")).unwrap();

        // Alice's chat: one message from her, a receipt she gave us, an
        // outgoing receipt watermark, its queued envelope, and one queued
        // outbound message envelope.
        store
            .insert_message(msg(b"alice-id", b"alice-id", 1, "hi from alice"))
            .unwrap();
        store
            .record_receipt(
                b"alice-id".to_vec(),
                b"me".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(
                b"alice-id".to_vec(),
                b"alice-id".to_vec(),
                RECEIPT_TYPE_READ,
                1,
            )
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(
                outgoing_receipt_for(
                    b"alice-id",
                    b"alice-id",
                    b"alice-id",
                    RECEIPT_TYPE_READ,
                    1,
                    b"rcpt-alice-1",
                ),
                1_700_000_000_100,
            )
            .unwrap();
        let alice_outgoing = msg(b"alice-id", b"me", 1, "reply to alice");
        store
            .insert_outgoing_message(
                alice_outgoing.clone(),
                outbound_for(&alice_outgoing, b"alice-id", b"msg-alice-out-1"),
                1_700_000_000_200,
            )
            .unwrap();

        // Bob's chat: the identical shape of state, to prove it survives.
        store
            .insert_message(msg(b"bob-id", b"bob-id", 1, "hi from bob"))
            .unwrap();
        store
            .record_receipt(
                b"bob-id".to_vec(),
                b"me".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(b"bob-id".to_vec(), b"bob-id".to_vec(), RECEIPT_TYPE_READ, 1)
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(
                outgoing_receipt_for(
                    b"bob-id",
                    b"bob-id",
                    b"bob-id",
                    RECEIPT_TYPE_READ,
                    1,
                    b"rcpt-bob-1",
                ),
                1_700_000_000_100,
            )
            .unwrap();
        let bob_outgoing = msg(b"bob-id", b"me", 1, "reply to bob");
        let bob_outbound = outbound_for(&bob_outgoing, b"bob-id", b"msg-bob-out-1");
        store
            .insert_outgoing_message(
                bob_outgoing.clone(),
                bob_outbound.clone(),
                1_700_000_000_200,
            )
            .unwrap();

        assert!(store.delete_contact(b"alice-id".to_vec()).unwrap());

        // All five per-chat tables are empty for alice's chat_id.
        assert!(store
            .messages_for_chat(b"alice-id".to_vec())
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .receipt_through(b"alice-id".to_vec(), b"me".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"alice-id".to_vec(),
                    b"alice-id".to_vec(),
                    RECEIPT_TYPE_READ
                )
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_envelope(
                    b"alice-id".to_vec(),
                    b"alice-id".to_vec(),
                    RECEIPT_TYPE_READ
                )
                .unwrap(),
            None
        );
        assert!(store
            .outbound_envelopes_after(b"alice-id".to_vec(), b"me".to_vec(), 0)
            .unwrap()
            .is_empty());

        // Bob's chat is untouched in every one of the same five tables
        // (2 messages: bob's incoming one plus our outgoing reply).
        assert_eq!(
            store.messages_for_chat(b"bob-id".to_vec()).unwrap().len(),
            2
        );
        assert_eq!(
            store
                .receipt_through(b"bob-id".to_vec(), b"me".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .outgoing_receipt_through(b"bob-id".to_vec(), b"bob-id".to_vec(), RECEIPT_TYPE_READ)
                .unwrap(),
            1
        );
        assert!(store
            .outgoing_receipt_envelope(b"bob-id".to_vec(), b"bob-id".to_vec(), RECEIPT_TYPE_READ)
            .unwrap()
            .is_some());
        assert_eq!(
            store
                .outbound_envelopes_after(b"bob-id".to_vec(), b"me".to_vec(), 0)
                .unwrap(),
            vec![bob_outbound],
        );
    }

    // --- groups (DESIGN.md §6.5) ------------------------------------------

    #[test]
    fn upsert_then_get_group_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let group = group(0x11, "Family", 0x22, &[b"carol", b"alice", b"alice"]);
        store.upsert_group(group.clone()).unwrap();

        assert_eq!(
            store.get_group(group.id.clone()).unwrap(),
            Some(Group {
                id: group.id,
                name: "Family".to_string(),
                member_user_ids: vec![test_user_id(b"alice"), test_user_id(b"carol")],
                key: vec![0x22; 32],
                metadata_revision: 0,
                metadata_changed_by: Vec::new(),
            })
        );
    }

    #[test]
    fn upsert_group_replaces_key_and_members_for_rotation() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut rotated = group(0x11, "Bridge", 0x22, &[b"alice", b"bob"]);
        store.upsert_group(rotated.clone()).unwrap();
        rotated.key = vec![0x33; 32];
        rotated.member_user_ids = vec![test_user_id(b"alice"), test_user_id(b"dave")];

        store.upsert_group(rotated.clone()).unwrap();

        assert_eq!(store.get_group(rotated.id.clone()).unwrap(), Some(rotated));
    }

    #[test]
    fn stale_group_invite_cannot_roll_back_newer_metadata() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let stale = group(0x11, "Old name", 0x22, &[b"alice", b"bob"]);
        let mut current = stale.clone();
        current.name = "New name".to_string();
        current.member_user_ids.push(test_user_id(b"carol"));
        current.metadata_revision = 4;
        current.metadata_changed_by = test_user_id(b"alice");
        store.upsert_group(current.clone()).unwrap();

        store.upsert_group(stale).unwrap();

        assert_eq!(store.get_group(current.id.clone()).unwrap(), Some(current));
    }

    #[test]
    fn list_groups_orders_by_name() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .upsert_group(group(0x22, "Zulu", 0x33, &[b"alice"]))
            .unwrap();
        store
            .upsert_group(group(0x11, "Alpha", 0x22, &[b"bob"]))
            .unwrap();

        let names: Vec<String> = store
            .list_groups()
            .unwrap()
            .into_iter()
            .map(|group| group.name)
            .collect();
        assert_eq!(names, vec!["Alpha".to_string(), "Zulu".to_string()]);
    }

    #[test]
    fn delete_group_removes_group_and_group_chat_state() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let group = group(0x11, "Family", 0x22, &[b"alice", b"bob"]);
        store.upsert_group(group.clone()).unwrap();
        store
            .insert_message(StoredMessage {
                chat_id: group.id.clone(),
                sender_user_id: b"alice".to_vec(),
                lamport: 1,
                timestamp: 1_700_000_000_000,
                kind: 1,
                payload: b"group hi".to_vec(),
            })
            .unwrap();
        store
            .record_receipt(
                group.id.clone(),
                b"alice".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(group.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_READ, 1)
            .unwrap();

        assert!(store.delete_group(group.id.clone()).unwrap());
        assert_eq!(store.get_group(group.id.clone()).unwrap(), None);
        assert!(store
            .messages_for_chat(group.id.clone())
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .receipt_through(group.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_through(group.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_READ)
                .unwrap(),
            0
        );
    }

    #[test]
    fn delete_group_purges_all_per_chat_state_and_leaves_other_group_alone() {
        // Mirrors delete_contact_purges_all_per_chat_state_and_leaves_other_chat_alone:
        // seed all five per-chat tables for two groups, delete one, verify a
        // blank slate for it and the other group's state untouched.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let group_a = group(0x11, "Family", 0x22, &[b"alice", b"bob"]);
        let group_b = group(0x33, "Crew", 0x44, &[b"carol", b"dave"]);
        store.upsert_group(group_a.clone()).unwrap();
        store.upsert_group(group_b.clone()).unwrap();

        store
            .insert_message(msg(&group_a.id, b"alice", 1, "hi group a"))
            .unwrap();
        store
            .record_receipt(
                group_a.id.clone(),
                b"alice".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(group_a.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_READ, 1)
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(
                outgoing_receipt_for(
                    &group_a.id,
                    b"alice",
                    b"alice",
                    RECEIPT_TYPE_READ,
                    1,
                    b"rcpt-a-1",
                ),
                1_700_000_000_100,
            )
            .unwrap();
        let a_outgoing = msg(&group_a.id, b"me", 1, "reply in group a");
        store
            .insert_outgoing_message(
                a_outgoing.clone(),
                outbound_for(&a_outgoing, b"alice", b"msg-a-out-1"),
                1_700_000_000_200,
            )
            .unwrap();

        store
            .insert_message(msg(&group_b.id, b"carol", 1, "hi group b"))
            .unwrap();
        store
            .record_receipt(
                group_b.id.clone(),
                b"carol".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(group_b.id.clone(), b"carol".to_vec(), RECEIPT_TYPE_READ, 1)
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(
                outgoing_receipt_for(
                    &group_b.id,
                    b"carol",
                    b"carol",
                    RECEIPT_TYPE_READ,
                    1,
                    b"rcpt-b-1",
                ),
                1_700_000_000_100,
            )
            .unwrap();
        let b_outgoing = msg(&group_b.id, b"me", 1, "reply in group b");
        let b_outbound = outbound_for(&b_outgoing, b"carol", b"msg-b-out-1");
        store
            .insert_outgoing_message(b_outgoing.clone(), b_outbound.clone(), 1_700_000_000_200)
            .unwrap();

        assert!(store.delete_group(group_a.id.clone()).unwrap());

        // All five per-chat tables are empty for group_a's chat_id.
        assert!(store
            .messages_for_chat(group_a.id.clone())
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .receipt_through(
                    group_a.id.clone(),
                    b"alice".to_vec(),
                    RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_through(group_a.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_READ)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_envelope(group_a.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_READ)
                .unwrap(),
            None
        );
        assert!(store
            .outbound_envelopes_after(group_a.id.clone(), b"me".to_vec(), 0)
            .unwrap()
            .is_empty());

        // group_b's state is untouched in every one of the same five tables
        // (2 messages: carol's incoming one plus our outgoing reply).
        assert_eq!(
            store.messages_for_chat(group_b.id.clone()).unwrap().len(),
            2
        );
        assert_eq!(
            store
                .receipt_through(
                    group_b.id.clone(),
                    b"carol".to_vec(),
                    RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .outgoing_receipt_through(group_b.id.clone(), b"carol".to_vec(), RECEIPT_TYPE_READ)
                .unwrap(),
            1
        );
        assert!(store
            .outgoing_receipt_envelope(group_b.id.clone(), b"carol".to_vec(), RECEIPT_TYPE_READ)
            .unwrap()
            .is_some());
        assert_eq!(
            store
                .outbound_envelopes_after(group_b.id.clone(), b"me".to_vec(), 0)
                .unwrap(),
            vec![b_outbound],
        );
    }

    // --- receipts (DESIGN.md §7.2) -----------------------------------------

    #[test]
    fn receipt_through_is_zero_when_none_recorded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let through = store
            .receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
            )
            .unwrap();
        assert_eq!(through, 0);
    }

    #[test]
    fn record_receipt_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                5,
                None,
            )
            .unwrap();

        let through = store
            .receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
            )
            .unwrap();
        assert_eq!(through, 5);
    }

    #[test]
    fn record_receipt_is_monotonic_upward() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                5,
                None,
            )
            .unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
            )
            .unwrap();

        let through = store
            .receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
            )
            .unwrap();
        assert_eq!(through, 9);
    }

    #[test]
    fn record_receipt_never_regresses_on_a_lower_or_replayed_value() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
            )
            .unwrap();
        // A stale/replayed receipt (lower, or the same, value) must not undo progress.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                3,
                None,
            )
            .unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
            )
            .unwrap();

        let through = store
            .receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
            )
            .unwrap();
        assert_eq!(through, 9);
    }

    #[test]
    fn record_receipt_records_and_advances_via_transport() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // First confirmation returned over relay (transport 2).
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                5,
                Some(2),
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            Some(2)
        );

        // A later confirmation that advances the watermark over BLE direct
        // (transport 0) updates the recorded route.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                Some(0),
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            Some(0)
        );
    }

    #[test]
    fn via_transport_is_kept_when_the_watermark_does_not_advance_or_route_is_unknown() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                Some(3), // local Wi-Fi confirmed the watermark first
            )
            .unwrap();
        // A re-sent receipt for the same watermark on a different link must not
        // overwrite the transport that first confirmed it.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                Some(2),
            )
            .unwrap();
        // A watermark-advancing receipt whose return route is unknown keeps the
        // last known route rather than clearing it.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                12,
                None,
            )
            .unwrap();

        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            12
        );
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            Some(3)
        );
    }

    #[test]
    fn via_transport_backfills_from_null_at_the_same_watermark_but_never_clears_afterward() {
        // FC4: the first receipt at a watermark can arrive with an unknown
        // route (via_transport = None). A later receipt confirming the *same*
        // watermark with a known route must fill the gap instead of being
        // permanently ignored -- but once a route is known, a later
        // unknown-route receipt (even a resend of the same watermark) must
        // never clear it back to unknown.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            None
        );

        // Same watermark, now with a known route (BLE direct, transport 0):
        // fills the previously-unknown route.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                Some(0),
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            9
        );
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            Some(0)
        );

        // A later receipt at the same watermark with an unknown route must
        // never clear the route that's now known.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            Some(0)
        );
    }

    #[test]
    fn receipt_via_transport_is_none_when_unrecorded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            None
        );
    }

    #[test]
    fn open_migrates_an_old_receipts_table_to_add_via_transport() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("cruisemesh-receipts-migration-{unique}.sqlite"));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE receipts (
                chat_id         BLOB NOT NULL,
                sender_user_id  BLOB NOT NULL,
                receipt_type    INTEGER NOT NULL,
                through_lamport INTEGER NOT NULL,
                PRIMARY KEY(chat_id, sender_user_id, receipt_type)
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO receipts (chat_id, sender_user_id, receipt_type, through_lamport)
             VALUES (?1, ?2, ?3, ?4)",
            params![b"alice-id".to_vec(), b"me".to_vec(), 1i64, 3i64],
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str.clone()).unwrap();
        // The pre-existing watermark survives the migration; its route is
        // unknown (the column was just added, defaulting to NULL).
        assert_eq!(
            store
                .receipt_through(b"alice-id".to_vec(), b"me".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            3
        );
        assert_eq!(
            store
                .receipt_via_transport(b"alice-id".to_vec(), b"me".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            None
        );
        // A newer confirmation that advances the watermark now records its route.
        store
            .record_receipt(
                b"alice-id".to_vec(),
                b"me".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                5,
                Some(0),
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_via_transport(b"alice-id".to_vec(), b"me".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            Some(0)
        );

        drop(store);
        let _ = std::fs::remove_file(&path_str);
    }

    #[test]
    fn receipt_types_are_independent() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
            )
            .unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
                4,
                None,
            )
            .unwrap();

        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            9
        );
        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_READ
                )
                .unwrap(),
            4
        );
    }

    #[test]
    fn receipts_are_independent_per_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
            )
            .unwrap();
        store
            .record_receipt(
                b"chat-b".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                2,
                None,
            )
            .unwrap();

        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            9
        );
        assert_eq!(
            store
                .receipt_through(
                    b"chat-b".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            2
        );
    }

    // --- delivery metrics / field export (V2) -----------------------------

    #[test]
    fn sent_then_delivered_metric_stamps_latency_and_route_for_the_covered_run() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        for (lamport, at) in [(1u64, 1_000i64), (2, 1_100), (3, 1_200)] {
            store
                .record_sent_metric(b"alice".to_vec(), lamport, at)
                .unwrap();
        }
        // A cumulative delivered receipt through lamport 2, returned over BLE
        // direct (transport 0), stamps messages 1 and 2 -- not 3.
        store
            .record_delivered_metric(b"alice".to_vec(), 2, 1_500, Some(0))
            .unwrap();

        let csv = store.export_delivery_metrics_csv().unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "direction,chat,lamport,sender,at_ms,delivered_at_ms,latency_ms,via_transport,arrival_transport,hop_count"
        );
        // Rows are ordered by direction then chat then lamport, so 1..=3
        // follow. "sent" rows carry an empty sender cell (see
        // `metric_sender_self`).
        assert!(lines[1].starts_with("sent,"));
        assert!(lines[1].ends_with(",1,,1000,1500,500,0,,"));
        assert!(lines[2].ends_with(",2,,1100,1500,400,0,,"));
        // Message 3 is beyond the watermark: no delivery yet.
        assert!(lines[3].ends_with(",3,,1200,,,,,"));
    }

    #[test]
    fn delivered_metric_keeps_the_first_confirmation() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.record_sent_metric(b"alice".to_vec(), 1, 1_000).unwrap();
        store
            .record_delivered_metric(b"alice".to_vec(), 1, 1_500, Some(3))
            .unwrap();
        // A later receipt for the same message must not overwrite the first
        // confirmation's time or route.
        store
            .record_delivered_metric(b"alice".to_vec(), 1, 1_900, Some(0))
            .unwrap();

        let csv = store.export_delivery_metrics_csv().unwrap();
        let row = csv.lines().nth(1).unwrap();
        assert!(row.ends_with(",1,,1000,1500,500,3,,"), "row was: {row}");
    }

    #[test]
    fn record_message_arrival_logs_an_inbound_metric_once() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.insert_message(msg(b"bob", b"bob", 5, "hi")).unwrap();
        let first = MessageArrival {
            transport: 2,
            hops_taken: 3,
            received_at: 1_700_000_000_777,
        };
        assert!(store
            .record_message_arrival(b"bob".to_vec(), b"bob".to_vec(), 5, first)
            .unwrap());
        // A redundant later copy neither overwrites the arrival nor adds a row.
        let redundant = MessageArrival {
            transport: 0,
            hops_taken: 1,
            received_at: 1_700_000_009_999,
        };
        assert!(!store
            .record_message_arrival(b"bob".to_vec(), b"bob".to_vec(), 5, redundant)
            .unwrap());

        let csv = store.export_delivery_metrics_csv().unwrap();
        let received: Vec<&str> = csv.lines().filter(|l| l.starts_with("received,")).collect();
        assert_eq!(received.len(), 1);
        let bob_sender = hex_lower(&metric_sender_hash(b"bob"));
        assert!(
            received[0].ends_with(&format!(",5,{bob_sender},1700000000777,,,,2,3")),
            "row: {}",
            received[0]
        );
    }

    #[test]
    fn record_message_arrival_keys_on_sender_so_group_lamport_collisions_dont_drop_rows() {
        // FC1: in a group, every member has an independent lamport stream,
        // so two different senders routinely share `lamport = 1` in the same
        // chat. Before FC1 the primary key omitted the sender and the
        // second arrival silently vanished under INSERT OR IGNORE.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"group-1", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"group-1", b"carol", 1, "hey"))
            .unwrap();

        let alice_arrival = MessageArrival {
            transport: 0,
            hops_taken: 1,
            received_at: 1_700_000_001_000,
        };
        let carol_arrival = MessageArrival {
            transport: 3,
            hops_taken: 2,
            received_at: 1_700_000_002_000,
        };
        assert!(store
            .record_message_arrival(b"group-1".to_vec(), b"alice".to_vec(), 1, alice_arrival)
            .unwrap());
        assert!(store
            .record_message_arrival(b"group-1".to_vec(), b"carol".to_vec(), 1, carol_arrival)
            .unwrap());

        let csv = store.export_delivery_metrics_csv().unwrap();
        let received: Vec<&str> = csv.lines().filter(|l| l.starts_with("received,")).collect();
        // Both arrivals landed as distinct rows -- neither dropped the other.
        assert_eq!(received.len(), 2, "csv was:\n{csv}");
        let alice_sender = hex_lower(&metric_sender_hash(b"alice"));
        let carol_sender = hex_lower(&metric_sender_hash(b"carol"));
        assert_ne!(alice_sender, carol_sender);
        assert!(received.iter().any(|r| r.contains(&format!(",1,{alice_sender},"))));
        assert!(received.iter().any(|r| r.contains(&format!(",1,{carol_sender},"))));
    }

    #[test]
    fn metrics_export_hashes_the_chat_and_never_leaks_the_raw_id() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let chat = b"super-secret-contact-id".to_vec();
        store.record_sent_metric(chat.clone(), 1, 1_000).unwrap();
        let csv = store.export_delivery_metrics_csv().unwrap();
        assert!(!csv.contains("super-secret-contact-id"));
        // Same chat hashes stably to the same tag across calls.
        store.record_sent_metric(chat.clone(), 2, 1_100).unwrap();
        let tag1 = csv.lines().nth(1).unwrap().split(',').nth(1).unwrap().to_string();
        let csv2 = store.export_delivery_metrics_csv().unwrap();
        let tag2 = csv2.lines().nth(1).unwrap().split(',').nth(1).unwrap();
        assert_eq!(tag1, tag2);
    }

    #[test]
    fn empty_metrics_export_is_just_the_header() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let csv = store.export_delivery_metrics_csv().unwrap();
        assert_eq!(csv.lines().count(), 1);
        assert!(csv.starts_with("direction,chat,lamport,"));
    }

    #[test]
    fn open_migrates_an_old_delivery_metrics_table_to_add_the_sender_key() {
        // FC1: pre-existing stores have `delivery_metrics` keyed on
        // (chat_hash, lamport, direction) only. Opening such a store must
        // not fail or panic; the disposable diagnostics table is dropped and
        // recreated with the sender-aware primary key (see
        // `migrate_delivery_metrics_schema`), and new arrivals -- including
        // the group lamport-collision case the old key couldn't hold -- work
        // afterward.
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir()
            .join(format!("cruisemesh-delivery-metrics-migration-{unique}.sqlite"));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE delivery_metrics (
                chat_hash         BLOB NOT NULL,
                lamport           INTEGER NOT NULL,
                direction         INTEGER NOT NULL,
                at_ms             INTEGER NOT NULL,
                delivered_at_ms   INTEGER,
                via_transport     INTEGER,
                arrival_transport INTEGER,
                hop_count         INTEGER,
                PRIMARY KEY(chat_hash, lamport, direction)
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO delivery_metrics (chat_hash, lamport, direction, at_ms)
             VALUES (?1, ?2, 0, ?3)",
            params![vec![1u8; 8], 1i64, 1_000i64],
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str.clone()).unwrap();
        // Old-shape data is local, best-effort diagnostics: it's fine for the
        // pre-migration row to be gone, as long as the store opens cleanly
        // and the export still works.
        let csv = store.export_delivery_metrics_csv().unwrap();
        assert_eq!(csv.lines().count(), 1, "csv was:\n{csv}");

        // The migrated schema now tolerates a group lamport collision.
        store
            .insert_message(msg(b"group-1", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"group-1", b"carol", 1, "hey"))
            .unwrap();
        assert!(store
            .record_message_arrival(
                b"group-1".to_vec(),
                b"alice".to_vec(),
                1,
                MessageArrival {
                    transport: 0,
                    hops_taken: 1,
                    received_at: 1_700_000_001_000,
                },
            )
            .unwrap());
        assert!(store
            .record_message_arrival(
                b"group-1".to_vec(),
                b"carol".to_vec(),
                1,
                MessageArrival {
                    transport: 3,
                    hops_taken: 2,
                    received_at: 1_700_000_002_000,
                },
            )
            .unwrap());
        let csv = store.export_delivery_metrics_csv().unwrap();
        let received = csv.lines().filter(|l| l.starts_with("received,")).count();
        assert_eq!(received, 2, "csv was:\n{csv}");

        let _ = std::fs::remove_file(&path);
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
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "one"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "two"))
            .unwrap();
        // A gap: lamport 3 missing for alice.
        store
            .insert_message(msg(b"chat-a", b"alice", 4, "four"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"bob", 1, "hey"))
            .unwrap();

        let digest = store.chat_digest(b"chat-a".to_vec()).unwrap();
        assert_eq!(
            digest,
            vec![
                DigestEntry {
                    sender_user_id: b"alice".to_vec(),
                    through_lamport: 2
                },
                DigestEntry {
                    sender_user_id: b"bob".to_vec(),
                    through_lamport: 1
                },
            ]
        );
    }

    #[test]
    fn messages_after_returns_only_newer_messages_ascending() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "one"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "two"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 3, "three"))
            .unwrap();

        let missing = store
            .messages_after(b"chat-a".to_vec(), b"alice".to_vec(), 1)
            .unwrap();
        let payloads: Vec<Vec<u8>> = missing.into_iter().map(|m| m.payload).collect();
        assert_eq!(payloads, vec![b"two".to_vec(), b"three".to_vec()]);
    }

    #[test]
    fn messages_after_zero_returns_everything() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "one"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "two"))
            .unwrap();

        let missing = store
            .messages_after(b"chat-a".to_vec(), b"alice".to_vec(), 0)
            .unwrap();
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
        store_b
            .insert_message(msg(b"chat-a", b"alice", 1, "msg-1"))
            .unwrap();
        store_b
            .insert_message(msg(b"chat-a", b"alice", 2, "msg-2"))
            .unwrap();

        let b_digest = store_b.chat_digest(b"chat-a".to_vec()).unwrap();
        assert_eq!(
            b_digest,
            vec![DigestEntry {
                sender_user_id: b"alice".to_vec(),
                through_lamport: 2
            }]
        );

        let mut all_missing = Vec::new();
        for entry in &b_digest {
            let missing = store_a
                .messages_after(
                    b"chat-a".to_vec(),
                    entry.sender_user_id.clone(),
                    entry.through_lamport,
                )
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
        let fill = msg_id
            .iter()
            .fold(0xAB_u8, |acc, byte| acc.wrapping_add(*byte));
        CarriedEnvelope {
            msg_id: msg_id.to_vec(),
            hop_ttl: 7,
            expiry,
            recipient_hint: hint.to_vec(),
            sealed: vec![fill; sealed_len],
        }
    }

    #[test]
    fn enqueue_then_fetch_by_hint_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let env = carried(b"m1", b"hint-a", 2_000, 100);
        assert!(store
            .enqueue_carried_envelope(env.clone(), false, 1_000, BIG_BUDGET)
            .unwrap());

        let found = store
            .carried_envelopes_for_hints(vec![b"hint-a".to_vec()], 1_500)
            .unwrap();
        assert_eq!(found, vec![env]);
    }

    #[test]
    fn enqueue_is_idempotent_on_msg_id() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(store
            .enqueue_carried_envelope(carried(b"m1", b"h", 2_000, 100), false, 1_000, BIG_BUDGET)
            .unwrap());
        // Same msg_id, re-received under DTN: no-op, not a duplicate row.
        assert!(!store
            .enqueue_carried_envelope(carried(b"m1", b"h", 2_000, 100), false, 1_050, BIG_BUDGET)
            .unwrap());
        assert_eq!(store.carried_len().unwrap(), 1);
    }

    #[test]
    fn enqueue_dedupes_rewrapped_ciphertext_but_preserves_distinct_hints() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let original = carried(b"original", b"hint-a", 9_000, 100);
        assert!(store
            .enqueue_carried_envelope(original.clone(), true, 1_000, BIG_BUDGET)
            .unwrap());

        let mut rewrapped = original.clone();
        rewrapped.msg_id = b"attacker-new-id".to_vec();
        rewrapped.expiry = 10_000;
        assert!(!store
            .enqueue_carried_envelope(rewrapped, true, 2_000, BIG_BUDGET)
            .unwrap());

        let mut group_fanout = original;
        group_fanout.msg_id = b"member-copy".to_vec();
        group_fanout.recipient_hint = b"hint-b".to_vec();
        assert!(store
            .enqueue_carried_envelope(group_fanout, true, 3_000, BIG_BUDGET)
            .unwrap());
        assert_eq!(store.carried_len().unwrap(), 2);
    }

    #[test]
    fn carry_ingest_rejects_amplified_hop_and_expiry_fields() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut bad_hop = carried(b"hop", b"hint", 9_000, 10);
        bad_hop.hop_ttl = crate::DEFAULT_HOP_TTL + 1;
        assert!(store
            .enqueue_carried_envelope(bad_hop, true, 1_000, BIG_BUDGET)
            .is_err());

        let too_far = carried(
            b"expiry",
            b"hint",
            1_000 + crate::MAX_CARRY_FUTURE_MS + 1,
            10,
        );
        assert!(store
            .enqueue_relay_carried_envelope(too_far, 1_000)
            .is_err());
        assert_eq!(store.carried_len().unwrap(), 0);
    }

    #[test]
    fn fetch_by_hint_ignores_nonmatching_and_expired() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m1", b"hint-a", 2_000, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m2", b"hint-b", 2_000, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m3", b"hint-a", 1_200, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();

        // now_ms = 1_500: m3 (expiry 1_200) is expired; m2 has the wrong hint.
        let found = store
            .carried_envelopes_for_hints(vec![b"hint-a".to_vec()], 1_500)
            .unwrap();
        let ids: Vec<Vec<u8>> = found.into_iter().map(|e| e.msg_id).collect();
        assert_eq!(ids, vec![b"m1".to_vec()]);
    }

    #[test]
    fn fetch_matches_any_of_several_hints() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m1", b"day-a", 9_000, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m2", b"day-b", 9_000, 10),
                false,
                1_100,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m3", b"day-c", 9_000, 10),
                false,
                1_200,
                BIG_BUDGET,
            )
            .unwrap();

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
        store
            .enqueue_carried_envelope(carried(b"m1", b"h", 9_000, 10), false, 1_000, BIG_BUDGET)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"m2", b"h", 9_000, 10), false, 2_000, BIG_BUDGET)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"m3", b"h", 9_000, 10), false, 3_000, BIG_BUDGET)
            .unwrap();

        let ids = store.carried_msg_ids(2).unwrap();
        assert_eq!(ids, vec![b"m1".to_vec(), b"m2".to_vec()]);
    }

    #[test]
    fn recent_consumed_msg_ids_are_returned_newest_first_and_limited() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_incoming_message(
                msg(b"chat-a", b"alice", 1, "one"),
                vec![1; MESSAGE_ID_LEN],
                None,
            )
            .unwrap();
        store
            .insert_incoming_message(
                msg(b"chat-a", b"alice", 2, "two"),
                vec![2; MESSAGE_ID_LEN],
                None,
            )
            .unwrap();
        store
            .insert_incoming_message(
                msg(b"chat-a", b"alice", 3, "three"),
                vec![3; MESSAGE_ID_LEN],
                None,
            )
            .unwrap();

        let ids = store.recent_consumed_msg_ids(2).unwrap();
        assert_eq!(ids, vec![vec![3; MESSAGE_ID_LEN], vec![2; MESSAGE_ID_LEN]]);
    }

    #[test]
    fn recent_consumed_msg_ids_skips_rows_without_a_msg_id() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // Legacy rows (inserted before envelope-id recording existed) carry
        // no msg_id; `insert_message` reproduces that shape.
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "legacy"))
            .unwrap();
        store
            .insert_incoming_message(
                msg(b"chat-a", b"alice", 2, "two"),
                vec![2; MESSAGE_ID_LEN],
                None,
            )
            .unwrap();

        let ids = store.recent_consumed_msg_ids(10).unwrap();
        assert_eq!(ids, vec![vec![2; MESSAGE_ID_LEN]]);
    }

    /// specs/group-relay-durability.md §4.3 / §6 scenario (2): the same
    /// logical group message can arrive under two envelope identities -- the
    /// ORIGINAL msg_id over BLE and a per-member fan-out msg_id from the
    /// relay. The `UNIQUE(chat_id, sender_user_id, lamport)` dedup renders
    /// it once regardless; the second insert is a silent no-op.
    #[test]
    fn same_message_under_two_envelope_ids_renders_once() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let ble_first = store
            .insert_incoming_message(
                msg(b"group-g", b"alice", 5, "meet at the buffet"),
                vec![0x11; MESSAGE_ID_LEN], // original envelope id (BLE flood)
                None,
            )
            .unwrap();
        let relay_second = store
            .insert_incoming_message(
                msg(b"group-g", b"alice", 5, "meet at the buffet"),
                vec![0x22; MESSAGE_ID_LEN], // fan-out id (relay fetch)
                None,
            )
            .unwrap();
        assert!(ble_first);
        assert!(!relay_second, "duplicate must be a silent no-op");
        assert_eq!(
            store.messages_for_chat(b"group-g".to_vec()).unwrap().len(),
            1,
            "one rendered row despite two envelope identities"
        );
    }

    #[test]
    fn recent_consumed_msg_ids_includes_own_authored_messages() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_incoming_message(
                msg(b"chat-a", b"alice", 1, "theirs"),
                vec![1; MESSAGE_ID_LEN],
                None,
            )
            .unwrap();
        // Our own authored message: `insert_outgoing_message` records the
        // envelope's msg_id on the message row too, so it must be advertised
        // alongside consumed incoming ids -- that's what suppresses a mule's
        // Hook-B spray from handing us back an envelope we authored.
        let authored = msg(b"chat-a", b"self", 1, "mine");
        let envelope = outbound_for(&authored, b"alice", &[2; MESSAGE_ID_LEN]);
        store
            .insert_outgoing_message(authored, envelope, 1_000)
            .unwrap();

        let ids = store.recent_consumed_msg_ids(10).unwrap();
        assert_eq!(ids, vec![vec![2; MESSAGE_ID_LEN], vec![1; MESSAGE_ID_LEN]]);
    }

    #[test]
    fn peer_sync_candidates_exclude_the_peers_known_ids_and_targeted_delivery() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"known", b"day-a", 9_000, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"for-peer", b"day-b", 9_000, 10),
                false,
                2_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"spray", b"day-c", 9_000, 10),
                false,
                3_000,
                BIG_BUDGET,
            )
            .unwrap();

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
        store
            .enqueue_carried_envelope(carried(b"m1", b"h", 2_000, 10), false, 1_000, BIG_BUDGET)
            .unwrap();
        assert!(store.remove_carried_envelope(b"m1".to_vec()).unwrap());
        assert!(!store.remove_carried_envelope(b"m1".to_vec()).unwrap()); // gone, idempotent
        assert_eq!(store.carried_len().unwrap(), 0);
    }

    #[test]
    fn prune_expired_carried_drops_only_the_expired() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(carried(b"live", b"h", 5_000, 10), false, 1_000, BIG_BUDGET)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"dead", b"h", 1_500, 10), false, 1_000, BIG_BUDGET)
            .unwrap();

        assert_eq!(store.prune_expired_carried(2_000).unwrap(), 1);
        assert_eq!(store.carried_len().unwrap(), 1);
        let found = store
            .carried_envelopes_for_hints(vec![b"h".to_vec()], 2_000)
            .unwrap();
        assert_eq!(found[0].msg_id, b"live");
    }

    #[test]
    fn foreign_budget_evicts_oldest_foreign_first() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // Budget of 250 bytes; three 100-byte foreign envelopes can't all fit.
        store
            .enqueue_carried_envelope(carried(b"f1", b"h", 9_000, 100), false, 1_000, 250)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"f2", b"h", 9_000, 100), false, 2_000, 250)
            .unwrap();
        // Third insert pushes total to 300 > 250, evicting the oldest (f1).
        store
            .enqueue_carried_envelope(carried(b"f3", b"h", 9_000, 100), false, 3_000, 250)
            .unwrap();

        let ids: Vec<Vec<u8>> = store
            .carried_envelopes_for_hints(vec![b"h".to_vec()], 5_000)
            .unwrap()
            .into_iter()
            .map(|e| e.msg_id)
            .collect();
        assert_eq!(ids, vec![b"f2".to_vec(), b"f3".to_vec()]);
    }

    #[test]
    fn family_envelopes_win_foreign_budget_eviction() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // A family envelope (is_family = true) far exceeding the budget stays.
        store
            .enqueue_carried_envelope(carried(b"fam", b"h", 9_000, 400), true, 1_000, 250)
            .unwrap();
        // Foreign envelopes still get budget-capped independently...
        store
            .enqueue_carried_envelope(carried(b"f1", b"h", 9_000, 100), false, 2_000, 250)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"f2", b"h", 9_000, 100), false, 3_000, 250)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"f3", b"h", 9_000, 100), false, 4_000, 250)
            .unwrap();

        let ids: Vec<Vec<u8>> = store
            .carried_envelopes_for_hints(vec![b"h".to_vec()], 5_000)
            .unwrap()
            .into_iter()
            .map(|e| e.msg_id)
            .collect();
        // fam survives despite being 400 bytes (> budget); foreign kept to f2,f3.
        assert_eq!(ids, vec![b"fam".to_vec(), b"f2".to_vec(), b"f3".to_vec()]);
    }

    #[test]
    fn total_budget_evicts_foreign_before_oldest_family() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"family-old", b"h1", 9_000, 100),
                true,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"foreign", b"h2", 9_000, 100),
                false,
                2_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"family-new", b"h3", 9_000, 100),
                true,
                3_000,
                BIG_BUDGET,
            )
            .unwrap();

        let mut conn = store.conn.lock().unwrap();
        let tx = conn.transaction().unwrap();
        enforce_carried_budgets(&tx, BIG_BUDGET, 200).unwrap();
        tx.commit().unwrap();
        drop(conn);

        let ids = store.carried_msg_ids(10).unwrap();
        assert_eq!(ids, vec![b"family-old".to_vec(), b"family-new".to_vec()]);

        let mut conn = store.conn.lock().unwrap();
        let tx = conn.transaction().unwrap();
        enforce_carried_budgets(&tx, BIG_BUDGET, 100).unwrap();
        tx.commit().unwrap();
        drop(conn);
        assert_eq!(
            store.carried_msg_ids(10).unwrap(),
            vec![b"family-new".to_vec()]
        );
    }

    // --- relay proxy-polling (from_relay) -----------------------------------

    #[test]
    fn relay_carried_envelope_is_deliverable_over_ble_but_never_reuploaded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let env = carried(b"proxy", b"hint-a", 9_000, 10);
        assert!(store
            .enqueue_relay_carried_envelope(env.clone(), 1_000)
            .unwrap());

        // Deliverable over BLE to the real recipient...
        let found = store
            .carried_envelopes_for_hints(vec![b"hint-a".to_vec()], 2_000)
            .unwrap();
        assert_eq!(found, vec![env]);

        // ...but never re-uploaded to the relay it came from.
        let uploadable = store.family_carried_envelopes(10, 2_000).unwrap();
        assert!(uploadable.is_empty());
    }

    #[test]
    fn normal_family_carried_envelope_is_still_reuploaded() {
        // Unchanged behavior: a family envelope received over BLE (not from
        // the relay) still surfaces in the relay-upload query.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let env = carried(b"ble-family", b"hint-a", 9_000, 10);
        assert!(store
            .enqueue_carried_envelope(env.clone(), true, 1_000, BIG_BUDGET)
            .unwrap());

        let uploadable = store.family_carried_envelopes(10, 2_000).unwrap();
        assert_eq!(uploadable, vec![env]);
    }

    #[test]
    fn open_migrates_an_old_carried_envelopes_table_to_add_from_relay() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cruisemesh-store-migration-carried-{unique}.sqlite"
        ));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE carried_envelopes (
                msg_id         BLOB PRIMARY KEY,
                hop_ttl        INTEGER NOT NULL,
                expiry         INTEGER NOT NULL,
                recipient_hint BLOB NOT NULL,
                sealed         BLOB NOT NULL,
                is_family      INTEGER NOT NULL,
                received_at    INTEGER NOT NULL,
                size_bytes     INTEGER NOT NULL
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO carried_envelopes
                (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family, received_at, size_bytes)
             VALUES (?1, 7, 9000, ?2, ?3, 1, 1000, 4)",
            params![
                b"legacy-one".as_slice(),
                b"same-hint".as_slice(),
                b"same".as_slice()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO carried_envelopes
                (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family, received_at, size_bytes)
             VALUES (?1, 7, 9000, ?2, ?3, 1, 2000, 4)",
            params![
                b"legacy-two".as_slice(),
                b"same-hint".as_slice(),
                b"same".as_slice()
            ],
        )
        .unwrap();
        drop(conn);

        // Opening an old store (pre-dating from_relay) must migrate the
        // column, not error, and the new relay-sourced path must work
        // against the migrated schema.
        let store = MessageStore::open(path_str.clone()).unwrap();
        assert_eq!(store.carried_len().unwrap(), 1, "migration dedupes content");
        let env = carried(b"proxy", b"hint-a", 9_000, 10);
        assert!(store
            .enqueue_relay_carried_envelope(env.clone(), 1_000)
            .unwrap());
        let found = store
            .carried_envelopes_for_hints(vec![b"hint-a".to_vec()], 2_000)
            .unwrap();
        assert_eq!(found, vec![env]);

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn backup_to_writes_a_consistent_reopenable_snapshot() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        store
            .insert_message(msg(b"chat", b"sender", 1, "backed up"))
            .unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cruisemesh-backup-{unique}.sqlite"));

        store.backup_to(path.to_string_lossy().to_string()).unwrap();
        let restored = MessageStore::open(path.to_string_lossy().to_string()).unwrap();
        assert_eq!(
            restored.messages_for_chat(b"chat".to_vec()).unwrap().len(),
            1
        );

        drop(restored);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn backup_to_rejects_relative_and_existing_destinations() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        assert!(store.backup_to("relative.sqlite".into()).is_err());

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cruisemesh-existing-{unique}.sqlite"));
        fs::write(&path, b"leave intact").unwrap();
        assert!(store.backup_to(path.to_string_lossy().to_string()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"leave intact");
        fs::remove_file(path).unwrap();
    }
}
