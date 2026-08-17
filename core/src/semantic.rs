use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Bound::Excluded;

use data_encoding::HEXLOWER;
use rusqlite::{params, OptionalExtension};

use crate::store::store_err;
use crate::CoreError;
use crate::{
    decode_reaction_payload, ConsumedHiddenLamport, CoreMessageTarget, GroupReceiptState,
    MessageStore, StoredMessage, KIND_ATTACHMENT_MANIFEST, KIND_GROUP_INVITE, KIND_REACTION,
    KIND_TEXT, RECEIPT_TYPE_READ,
};

type ReplyReference = (Option<Vec<u8>>, Option<Vec<u8>>);

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

/// Aggregate group tick (DESIGN.md §7.2): ✓✓ iff every *eligible* current
/// member has delivered (filled iff every eligible member has read).
///
/// A member is eligible for message `lamport` at `message_timestamp` when
/// they are not the author and they were already in the group when the
/// message was sent (`added_at_ms == 0` means founding / unknown-old, so
/// they count; a later `added_at_ms` after the message does not). An empty
/// eligible set — only the author remains — is vacuously Read.
#[uniffi::export]
pub fn core_group_tick_status_for(
    lamport: u64,
    message_timestamp: i64,
    author_user_id: Vec<u8>,
    state: GroupReceiptState,
) -> CoreTickStatus {
    let mut delivered_through = u64::MAX;
    let mut read_through = u64::MAX;
    let mut eligible = 0u32;
    for member in state.members {
        if member.member_user_id == author_user_id {
            continue;
        }
        if member.added_at_ms > 0 && member.added_at_ms > message_timestamp {
            continue;
        }
        eligible += 1;
        delivered_through = delivered_through.min(member.delivered_through);
        read_through = read_through.min(member.read_through);
    }
    if eligible == 0 {
        return CoreTickStatus::Read;
    }
    core_tick_status_for(lamport, delivered_through, read_through)
}

/// **1:1 chats only.** Compares every non-self sender's lamport against a
/// single scalar `read_through` watermark, which is only correct when there
/// is exactly one other sender in the chat. In a group, each member has an
/// independent lamport stream with its own read watermark, so this
/// undercounts (or miscounts entirely) group unread -- use
/// [`MessageStore::semantic_unread_count`] instead, which reads the
/// per-sender watermarks from `outgoing_receipts` and handles both cases
/// correctly (FC8).
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

/// Return the current reactors for one emoji on one message.
///
/// Reaction messages are last-write-wins per reactor and target: changing to a
/// different emoji replaces the old reaction, while a blank emoji clears it.
/// The returned user ids are byte-sorted so both shells present a stable list
/// before applying their local contact names.
#[uniffi::export]
pub fn core_reactors_for_reaction(
    messages: Vec<StoredMessage>,
    target: CoreMessageTarget,
    emoji: String,
) -> Vec<Vec<u8>> {
    let mut reactors: HashMap<Vec<u8>, (u64, String)> = HashMap::new();
    for message in messages
        .into_iter()
        .filter(|message| message.kind == KIND_REACTION)
    {
        let Some(reaction) = decode_reaction_payload(message.payload) else {
            continue;
        };
        if reaction.target != target {
            continue;
        }
        if reactors
            .get(&message.sender_user_id)
            .is_some_and(|old| old.0 > message.lamport)
        {
            continue;
        }
        if reaction.emoji.trim().is_empty() {
            reactors.remove(&message.sender_user_id);
        } else {
            reactors.insert(message.sender_user_id, (message.lamport, reaction.emoji));
        }
    }
    let mut matching: Vec<_> = reactors
        .into_iter()
        .filter_map(|(user_id, (_, current_emoji))| (current_emoji == emoji).then_some(user_id))
        .collect();
    matching.sort();
    matching
}

#[uniffi::export]
pub fn core_visible_gap_indices(
    messages: Vec<StoredMessage>,
    consumed_hidden_lamports: Vec<ConsumedHiddenLamport>,
) -> Vec<u32> {
    let visible = core_visible_chat_messages(messages.clone());
    let visible_indices: HashMap<String, u32> = visible
        .iter()
        .enumerate()
        .map(|(index, message)| (message_key(message), index as u32))
        .collect();
    // A broad high-water mark is not evidence: knowing that control lamport 4
    // arrived cannot prove user-visible lamport 3 did. Keep the exact known
    // positions so only a completely covered interval closes a visible gap.
    let mut known = HashMap::<Vec<u8>, BTreeSet<u64>>::new();
    for message in &messages {
        known
            .entry(message.sender_user_id.clone())
            .or_default()
            .insert(message.lamport);
    }
    for consumed in consumed_hidden_lamports {
        known
            .entry(consumed.sender_user_id)
            .or_default()
            .insert(consumed.lamport);
    }

    let mut last_visible = HashMap::<Vec<u8>, u64>::new();
    let mut result = Vec::new();
    for message in messages {
        let Some(index) = visible_indices.get(&message_key(&message)) else {
            continue;
        };
        if let Some(previous) = last_visible.get(&message.sender_user_id).copied() {
            if message.lamport > previous.saturating_add(1) {
                let expected = message.lamport - previous - 1;
                let covered = known
                    .get(&message.sender_user_id)
                    .map(|values| {
                        values
                            .range((Excluded(previous), Excluded(message.lamport)))
                            .count() as u64
                    })
                    .unwrap_or(0);
                if covered < expected {
                    result.push(*index);
                }
            }
        }
        last_visible
            .entry(message.sender_user_id)
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
        messages
            .into_iter()
            .map(|message| {
                let reference: Option<ReplyReference> = conn
                    .query_row(
                        "SELECT msg_id, reply_to_msg_id FROM messages
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3",
                        params![
                            message.chat_id,
                            message.sender_user_id,
                            message.lamport as i64
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(store_err)?;
                let (msg_id, reply_to_msg_id) = reference.unwrap_or((None, None));
                let target = match &reply_to_msg_id {
                    Some(id) => conn
                        .query_row(
                            "SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload,
                            sender_device_id FROM messages
                     WHERE chat_id = ?1 AND msg_id = ?2 ORDER BY id ASC LIMIT 1",
                            params![message.chat_id, id],
                            |row| {
                                Ok(StoredMessage {
                                    chat_id: row.get(0)?,
                                    sender_user_id: row.get(1)?,
                                    lamport: row.get::<_, i64>(2)? as u64,
                                    timestamp: row.get(3)?,
                                    kind: row.get::<_, i64>(4)? as u8,
                                    payload: row.get(5)?,
                                    sender_device_id: row.get(6)?,
                                })
                            },
                        )
                        .optional()
                        .map_err(store_err)?,
                    None => None,
                };
                Ok(CoreReplyMetadata {
                    message: CoreMessageTarget {
                        sender_user_id: message.sender_user_id,
                        lamport: message.lamport,
                        kind: message.kind,
                    },
                    msg_id,
                    reply_to_msg_id,
                    target,
                })
            })
            .collect()
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
            sender_device_id: crate::LEGACY_DEVICE_ID.to_vec(),
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
    fn reaction_reactors_are_current_filtered_and_stably_sorted() {
        let target = CoreMessageTarget {
            sender_user_id: vec![2],
            lamport: 1,
            kind: KIND_TEXT,
        };
        let other_target = CoreMessageTarget {
            sender_user_id: vec![2],
            lamport: 2,
            kind: KIND_TEXT,
        };
        let reaction = |sender, lamport, target: &CoreMessageTarget, emoji: &str| {
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
        let messages = vec![
            reaction(3, 1, &target, "❤️"),
            reaction(1, 1, &target, "❤️"),
            reaction(3, 2, &target, "👍"),
            reaction(4, 1, &target, "❤️"),
            reaction(4, 2, &target, ""),
            reaction(5, 1, &other_target, "❤️"),
        ];

        assert_eq!(
            core_reactors_for_reaction(messages, target, "❤️".into()),
            vec![vec![1]]
        );
    }

    #[test]
    fn hidden_messages_do_not_create_a_visible_gap() {
        let messages = vec![
            msg(1, 1, KIND_TEXT, vec![]),
            msg(1, 2, KIND_REACTION, vec![]),
            msg(1, 3, KIND_TEXT, vec![]),
        ];
        assert!(core_visible_gap_indices(messages, vec![]).is_empty());
    }

    #[test]
    fn consumed_but_discarded_control_lamports_close_a_visible_gap() {
        let messages = vec![msg(1, 1, KIND_TEXT, vec![]), msg(1, 3, KIND_TEXT, vec![])];
        let consumed = vec![ConsumedHiddenLamport {
            sender_user_id: vec![1],
            lamport: 2,
        }];
        assert!(core_visible_gap_indices(messages, consumed).is_empty());
    }

    #[test]
    fn sparse_control_lamports_do_not_hide_a_real_missing_message() {
        let messages = vec![
            msg(1, 1, KIND_TEXT, vec![]),
            msg(1, 3, KIND_REACTION, vec![]),
            msg(1, 4, KIND_TEXT, vec![]),
        ];
        assert_eq!(core_visible_gap_indices(messages, vec![]), vec![1]);
    }

    #[test]
    fn another_senders_control_lamport_cannot_close_the_gap() {
        let messages = vec![msg(1, 1, KIND_TEXT, vec![]), msg(1, 3, KIND_TEXT, vec![])];
        let consumed = vec![ConsumedHiddenLamport {
            sender_user_id: vec![2],
            lamport: 2,
        }];
        assert_eq!(core_visible_gap_indices(messages, consumed), vec![1]);
    }

    #[test]
    fn tick_status_prefers_read_and_visibility_is_canonical() {
        assert_eq!(core_tick_status_for(3, 1, 3), CoreTickStatus::Read);
        assert!(core_is_visible_chat_kind(KIND_GROUP_INVITE));
        assert!(!core_is_visible_chat_kind(KIND_REACTION));
    }

    #[test]
    fn group_tick_is_delivered_only_when_every_eligible_member_has_it() {
        let author = vec![1];
        let alice = vec![2];
        let bob = vec![3];
        let state = crate::GroupReceiptState {
            members: vec![
                crate::GroupMemberReceipt {
                    member_user_id: author.clone(),
                    delivered_through: 0,
                    read_through: 0,
                    delivered_via_transport: None,
                    added_at_ms: 0,
                },
                crate::GroupMemberReceipt {
                    member_user_id: alice,
                    delivered_through: 5,
                    read_through: 5,
                    delivered_via_transport: Some(0),
                    added_at_ms: 0,
                },
                crate::GroupMemberReceipt {
                    member_user_id: bob,
                    delivered_through: 4,
                    read_through: 0,
                    delivered_via_transport: Some(0),
                    added_at_ms: 0,
                },
            ],
        };
        assert_eq!(
            core_group_tick_status_for(5, 1_000, author.clone(), state.clone()),
            CoreTickStatus::Sent
        );
        assert_eq!(
            core_group_tick_status_for(4, 1_000, author, state),
            CoreTickStatus::Delivered
        );
    }

    #[test]
    fn group_tick_ignores_members_who_joined_after_the_message() {
        let author = vec![1];
        let late = vec![2];
        let state = crate::GroupReceiptState {
            members: vec![crate::GroupMemberReceipt {
                member_user_id: late,
                delivered_through: 0,
                read_through: 0,
                delivered_via_transport: None,
                added_at_ms: 2_000,
            }],
        };
        assert_eq!(
            core_group_tick_status_for(1, 1_000, author, state),
            CoreTickStatus::Read
        );
    }
}
