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
//! ## Sync digests (DESIGN.md §7.3)
//!
//! On peer connect, each side summarizes what it already has per chat so the
//! other side can send just what's missing. §7.3 describes that summary as
//! "(chat id, highest-contiguous lamport, recent msg_id bloom filter)".
//! [`MessageStore::chat_digest`] implements the contiguous-lamport half of
//! that -- one [`DigestEntry`] per sender who has posted in the chat,
//! reusing the same gap-aware [`MessageStore::highest_contiguous_lamport`]
//! logic already needed for per-sender ordering. The bloom filter (which
//! would additionally let out-of-order/non-contiguous messages be
//! discovered without waiting for the gap to fill) is **not** implemented
//! here -- it needs a design for what counts as "recent" and how false
//! positives are handled, and the contiguous-lamport digest is enough to
//! drive [`MessageStore::messages_after`] for the common case. TODO: add the
//! bloom filter once §7.3's "recent msg_id" scope is nailed down.
//! [`MessageStore::messages_after`] answers the other half: given what a
//! peer's digest says they already have, which of *our* messages from a
//! given sender are they missing.

use rusqlite::{params, Connection, OptionalExtension};
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
}

/// One entry of a per-chat sync digest (DESIGN.md §7.3): "I have `sender_user_id`'s
/// messages in this chat contiguously through `through_lamport`."
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct DigestEntry {
    pub sender_user_id: Vec<u8>,
    pub through_lamport: u64,
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

    /// All messages in a chat, oldest first.
    pub fn messages_for_chat(&self, chat_id: Vec<u8>) -> Result<Vec<StoredMessage>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let mut stmt = conn
            .prepare(
                "SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload
                 FROM messages WHERE chat_id = ?1 ORDER BY lamport ASC",
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

    /// Add or update a contact, keyed on `user_id` -- re-scanning the same
    /// FriendCard (e.g. after they update their display name) replaces the
    /// row rather than erroring or duplicating.
    pub fn upsert_contact(&self, contact: Contact) -> Result<(), CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO contacts (user_id, name, sign_pk, agree_pk, relay_url)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(user_id) DO UPDATE SET
                name = excluded.name,
                sign_pk = excluded.sign_pk,
                agree_pk = excluded.agree_pk,
                relay_url = excluded.relay_url",
            params![
                contact.user_id,
                contact.name,
                contact.sign_pk,
                contact.agree_pk,
                contact.relay_url,
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Look up a single contact by UserID, or `None` if not a contact.
    pub fn get_contact(&self, user_id: Vec<u8>) -> Result<Option<Contact>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.query_row(
            "SELECT user_id, name, sign_pk, agree_pk, relay_url FROM contacts WHERE user_id = ?1",
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
            .prepare("SELECT user_id, name, sign_pk, agree_pk, relay_url FROM contacts ORDER BY name ASC")
            .map_err(store_err)?;
        let rows = stmt.query_map([], row_to_contact).map_err(store_err)?;
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

fn row_to_contact(row: &rusqlite::Row) -> rusqlite::Result<Contact> {
    Ok(Contact {
        user_id: row.get(0)?,
        name: row.get(1)?,
        sign_pk: row.get(2)?,
        agree_pk: row.get(3)?,
        relay_url: row.get(4)?,
    })
}

fn store_err(e: rusqlite::Error) -> CoreError {
    CoreError::Store(e.to_string())
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

CREATE TABLE IF NOT EXISTS contacts (
    user_id   BLOB PRIMARY KEY,
    name      TEXT NOT NULL,
    sign_pk   BLOB NOT NULL,
    agree_pk  BLOB NOT NULL,
    relay_url TEXT
);

CREATE TABLE IF NOT EXISTS receipts (
    chat_id         BLOB NOT NULL,
    sender_user_id  BLOB NOT NULL,
    receipt_type    INTEGER NOT NULL,
    through_lamport INTEGER NOT NULL,
    PRIMARY KEY(chat_id, sender_user_id, receipt_type)
);
";

#[cfg(test)]
mod tests {
    use super::*;

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

    fn contact(user_id: &[u8], name: &str) -> Contact {
        Contact {
            user_id: user_id.to_vec(),
            name: name.to_string(),
            sign_pk: vec![1u8; 32],
            agree_pk: vec![2u8; 32],
            relay_url: None,
        }
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
}
