//! Message store: SQLite-backed persistence for chat messages (DESIGN.md
//! §7.1, §10). `insert_message` is idempotent on (chat_id, sender_user_id,
//! lamport), so re-delivering the same envelope (expected under DTN) is
//! safe. Per-chat lamport counters are maintained independently by each
//! sender (DESIGN.md §7.1), so gap detection in [MessageStore::highest_contiguous_lamport]
//! is keyed on (chat_id, sender_user_id), not chat_id alone.

use rusqlite::{params, Connection};
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
}
