use std::collections::{BTreeMap, HashMap};

use data_encoding::HEXLOWER;
use rusqlite::{params, OptionalExtension};

use crate::store::store_err;
use crate::CoreError;
use crate::{
    decode_reaction_payload, CoreMessageTarget, MessageStore, StoredMessage,
    KIND_ATTACHMENT_MANIFEST, KIND_GROUP_INVITE, KIND_REACTION, KIND_TEXT, RECEIPT_TYPE_READ,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreTickStatus {
    Sent,
    Delivered,
    Read,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreReactionSummary {
    pub emoji: String,
    pub count: u32,
    pub reacted_by_own_user: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreReactionTargetSummary {
    pub target: CoreMessageTarget,
    pub reactions: Vec<CoreReactionSummary>,
}

#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct CoreReplyMetadata {
    pub message: CoreMessageTarget,
    pub msg_id: Option<Vec<u8>>,
    pub reply_to_msg_id: Option<Vec<u8>>,
    pub target: Option<StoredMessage>,
}

#[uniffi::export]
pub fn core_is_visible_chat_kind(kind: u8) -> bool {
    matches!(
        kind,
        KIND_TEXT | KIND_ATTACHMENT_MANIFEST | KIND_GROUP_INVITE
    )
}

#[uniffi::export]
pub fn core_visible_chat_messages(messages: Vec<StoredMessage>) -> Vec<StoredMessage> {
    messages
        .into_iter()
        .filter(|message| core_is_visible_chat_kind(message.kind))
        .collect()
}

#[uniffi::export]
pub fn core_tick_status_for(
    lamport: u64,
    delivered_through: u64,
    read_through: u64,
) -> CoreTickStatus {
    if lamport <= read_through {
        CoreTickStatus::Read
    } else if lamport <= delivered_through {
        CoreTickStatus::Delivered
    } else {
        CoreTickStatus::Sent
    }
}

#[uniffi::export]
pub fn core_unread_count(
    messages: Vec<StoredMessage>,
    own_user_id: Vec<u8>,
    read_through: u64,
) -> u32 {
    messages
        .into_iter()
        .filter(|message| {
            core_is_visible_chat_kind(message.kind)
                && message.sender_user_id != own_user_id
                && message.lamport > read_through
        })
        .count() as u32
}

#[uniffi::export]
pub fn core_last_visible_message(messages: Vec<StoredMessage>) -> Option<StoredMessage> {
    messages
        .into_iter()
        .filter(|message| core_is_visible_chat_kind(message.kind))
        .max_by_key(|message| message.timestamp)
}

#[uniffi::export]
pub fn core_reaction_summaries_by_target(
    messages: Vec<StoredMessage>,
    own_user_id: Vec<u8>,
) -> Vec<CoreReactionTargetSummary> {
    #[derive(Clone)]
    struct State {
        lamport: u64,
        emoji: String,
        own: bool,
    }
    let mut targets: BTreeMap<String, (CoreMessageTarget, HashMap<Vec<u8>, State>)> =
        BTreeMap::new();
    for message in messages
        .into_iter()
        .filter(|message| message.kind == KIND_REACTION)
    {
        let Some(reaction) = decode_reaction_payload(message.payload) else {
            continue;
        };
        let key = stable_key(&reaction.target);
        let (_, reactors) = targets
            .entry(key)
            .or_insert_with(|| (reaction.target.clone(), HashMap::new()));
        if reactors
            .get(&message.sender_user_id)
            .is_some_and(|old| old.lamport > message.lamport)
        {
            continue;
        }
        if reaction.emoji.trim().is_empty() {
            reactors.remove(&message.sender_user_id);
        } else {
            reactors.insert(
                message.sender_user_id.clone(),
                State {
                    lamport: message.lamport,
                    emoji: reaction.emoji,
                    own: message.sender_user_id == own_user_id,
                },
            );
        }
    }
    targets
        .into_values()
        .map(|(target, reactors)| {
            let mut grouped: BTreeMap<String, (u32, bool)> = BTreeMap::new();
            for state in reactors.into_values() {
                let entry = grouped.entry(state.emoji).or_default();
                entry.0 += 1;
                entry.1 |= state.own;
            }
            let mut reactions: Vec<_> = grouped
                .into_iter()
                .map(|(emoji, (count, own))| CoreReactionSummary {
                    emoji,
                    count,
                    reacted_by_own_user: own,
                })
                .collect();
            reactions.sort_by(|a, b| {
                b.reacted_by_own_user
                    .cmp(&a.reacted_by_own_user)
                    .then(a.emoji.cmp(&b.emoji))
            });
            CoreReactionTargetSummary { target, reactions }
        })
        .collect()
}

#[uniffi::export]
pub fn core_visible_gap_indices(messages: Vec<StoredMessage>) -> Vec<u32> {
    let visible = core_visible_chat_messages(messages.clone());
    let visible_indices: HashMap<String, u32> = visible
        .iter()
        .enumerate()
        .map(|(index, message)| (message_key(message), index as u32))
        .collect();
    let mut last = HashMap::<Vec<u8>, u64>::new();
    let mut result = Vec::new();
    for message in messages {
        let previous = last.get(&message.sender_user_id).copied();
        if let (Some(index), Some(previous)) =
            (visible_indices.get(&message_key(&message)), previous)
        {
            if message.lamport > previous.saturating_add(1) {
                result.push(*index);
            }
        }
        last.entry(message.sender_user_id)
            .and_modify(|value| *value = (*value).max(message.lamport))
            .or_insert(message.lamport);
    }
    result.sort_unstable();
    result.dedup();
    result
}

#[uniffi::export]
impl MessageStore {
    /// Unread visible messages across every non-self sender stream in a chat,
    /// using each stream's persisted local READ watermark.
    pub fn semantic_unread_count(
        &self,
        chat_id: Vec<u8>,
        own_user_id: Vec<u8>,
    ) -> Result<u32, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages m
             WHERE m.chat_id = ?1 AND m.sender_user_id != ?2 AND m.kind IN (?3, ?4, ?5)
               AND m.lamport > COALESCE((SELECT through_lamport FROM outgoing_receipts r
                   WHERE r.chat_id = m.chat_id AND r.sender_user_id = m.sender_user_id
                     AND r.receipt_type = ?6), 0)",
                params![
                    chat_id,
                    own_user_id,
                    KIND_TEXT as i64,
                    KIND_ATTACHMENT_MANIFEST as i64,
                    KIND_GROUP_INVITE as i64,
                    RECEIPT_TYPE_READ as i64
                ],
                |row| row.get(0),
            )
            .map_err(store_err)?;
        Ok(count as u32)
    }

    /// Resolve all stable ids and reply targets for a timeline under one lock.
    pub fn reply_metadata(
        &self,
        messages: Vec<StoredMessage>,
    ) -> Result<Vec<CoreReplyMetadata>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        messages.into_iter().map(|message| {
            let reference: Option<(Option<Vec<u8>>, Option<Vec<u8>>)> = conn.query_row(
                "SELECT msg_id, reply_to_msg_id FROM messages
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3",
                params![message.chat_id, message.sender_user_id, message.lamport as i64],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional().map_err(store_err)?;
            let (msg_id, reply_to_msg_id) = reference.unwrap_or((None, None));
            let target = match &reply_to_msg_id {
                Some(id) => conn.query_row(
                    "SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload FROM messages
                     WHERE chat_id = ?1 AND msg_id = ?2 ORDER BY id ASC LIMIT 1",
                    params![message.chat_id, id], |row| Ok(StoredMessage { chat_id: row.get(0)?,
                        sender_user_id: row.get(1)?, lamport: row.get::<_, i64>(2)? as u64,
                        timestamp: row.get(3)?, kind: row.get::<_, i64>(4)? as u8, payload: row.get(5)? }),
                ).optional().map_err(store_err)?,
                None => None,
            };
            Ok(CoreReplyMetadata { message: CoreMessageTarget { sender_user_id: message.sender_user_id,
                lamport: message.lamport, kind: message.kind }, msg_id, reply_to_msg_id, target })
        }).collect()
    }
}

fn stable_key(target: &CoreMessageTarget) -> String {
    format!(
        "{}:{}:{}",
        HEXLOWER.encode(&target.sender_user_id),
        target.lamport,
        target.kind
    )
}
fn message_key(message: &StoredMessage) -> String {
    stable_key(&CoreMessageTarget {
        sender_user_id: message.sender_user_id.clone(),
        lamport: message.lamport,
        kind: message.kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{encode_reaction_payload, CoreReactionPayload};

    fn msg(sender: u8, lamport: u64, kind: u8, payload: Vec<u8>) -> StoredMessage {
        StoredMessage {
            chat_id: vec![9],
            sender_user_id: vec![sender],
            lamport,
            timestamp: lamport as i64,
            kind,
            payload,
        }
    }

    #[test]
    fn latest_reaction_per_user_wins_and_blank_clears() {
        let target = CoreMessageTarget {
            sender_user_id: vec![2],
            lamport: 1,
            kind: KIND_TEXT,
        };
        let reaction = |sender, lamport, emoji: &str| {
            msg(
                sender,
                lamport,
                KIND_REACTION,
                encode_reaction_payload(CoreReactionPayload {
                    target: target.clone(),
                    emoji: emoji.into(),
                })
                .unwrap(),
            )
        };
        let summaries = core_reaction_summaries_by_target(
            vec![
                reaction(1, 1, "👍"),
                reaction(1, 2, ""),
                reaction(3, 1, "❤️"),
            ],
            vec![1],
        );
        assert_eq!(
            summaries[0].reactions,
            vec![CoreReactionSummary {
                emoji: "❤️".into(),
                count: 1,
                reacted_by_own_user: false
            }]
        );
    }

    #[test]
    fn hidden_messages_do_not_create_a_visible_gap() {
        let messages = vec![
            msg(1, 1, KIND_TEXT, vec![]),
            msg(1, 2, KIND_REACTION, vec![]),
            msg(1, 3, KIND_TEXT, vec![]),
        ];
        assert!(core_visible_gap_indices(messages).is_empty());
    }

    #[test]
    fn tick_status_prefers_read_and_visibility_is_canonical() {
        assert_eq!(core_tick_status_for(3, 1, 3), CoreTickStatus::Read);
        assert!(core_is_visible_chat_kind(KIND_GROUP_INVITE));
        assert!(!core_is_visible_chat_kind(KIND_REACTION));
    }
}
