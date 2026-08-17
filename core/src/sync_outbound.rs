//! SYNC-2: one outbound message, one author.
//!
//! `specs/multi-device-v1.md` §8 states the rule and then states the product
//! behaviour that makes it hold: "an outgoing message is authored once, by one
//! device, in that device's stream. Sync must make a sibling aware of a pending
//! outbound before it re-authors ('send from whichever device is in hand' edits
//! the draft, not the stream)."
//!
//! Two mechanisms, both of them SYNC-1 anti-entropy wearing different hats:
//!
//! * **The draft is shared state, not a local scratchpad.** A chat's composer
//!   text lives in the Settings stream under a reserved key, so it converges
//!   the way every other shared setting does: last epoch wins, neither device
//!   need be online. Picking up the tablet mid-sentence therefore continues the
//!   sentence, which is what "edits the draft" means in the field, and
//!   authoring **clears** the draft — so the clear converges too, and the
//!   sibling's composer empties instead of holding a message that has already
//!   gone out.
//! * **The authored row is the durable proof.** Once a sibling's
//!   [`crate::SyncRecordKind::History`] record lands, the message is an
//!   ordinary row on that sibling's device stream. [`outbound_claim`] reads
//!   exactly that, so the awareness SYNC-2 asks for is not a new channel: it is
//!   the history stream, arriving.
//!
//! The distinction the claim draws is deliberately narrow. A repeat from *this*
//! device is a person deciding to say something twice, and refusing it would be
//! the app second-guessing its user. A repeat of text a **sibling** already put
//! on the wire, inside a window short enough that it can only have come from a
//! draft that had not caught up, is the §8 bug — "identical re-uploads of an
//! already-posted row are safe under relayd's msg_id dedup, but two distinct
//! authored copies of the same text are a product bug".
//!
//! Nothing here is a lock. A lock would need both devices online, which SYNC-1
//! forbids assuming; what this module does instead is make the losing device
//! *notice*, which is all that is available to a fleet that may never be
//! concurrently reachable.

use rusqlite::{params, Connection, OptionalExtension};

use crate::store::store_err;
use crate::sync_record::SyncSettingEntry;
use crate::sync_store::{put_setting, setting};
use crate::{CoreError, MessageStore};

/// The reserved Settings-stream key prefix for a chat's composer draft.
///
/// One namespace, so a shell never invents a key that collides with a real
/// shared setting, and so a future build reading a database it does not fully
/// understand can still tell drafts apart from everything else in the stream.
pub(crate) const SYNC_DRAFT_KEY_PREFIX: &str = "chat.draft.";

/// How far back [`MessageStore::core_sync_outbound_claim`] looks by default for
/// a sibling's copy of the same text.
///
/// Twelve hours rather than minutes: the window has to cover the gap between a
/// send on one device and the *first encounter* with the sibling, and on a ship
/// that gap is a shore excursion, not a coffee break. It is deliberately not
/// unbounded — beyond about a day, an identical message is far likelier to be a
/// person repeating themselves than a draft that never caught up, and the app
/// must not silently refuse to send "we're at the pool" for a second time on a
/// second day.
pub const SYNC_OUTBOUND_DEDUP_WINDOW_MS: i64 = 12 * 60 * 60 * 1000;

/// The Settings-stream key a chat's shared draft lives under.
///
/// Exported because the shells write drafts and a key format re-derived on each
/// platform is exactly the duplicated arithmetic core-first exists to stop: two
/// devices that disagreed about the key would each hold a draft the other never
/// saw, which is the SYNC-2 failure this key exists to prevent.
#[uniffi::export]
pub fn core_sync_draft_key(chat_id: Vec<u8>) -> String {
    let mut key = String::with_capacity(SYNC_DRAFT_KEY_PREFIX.len() + chat_id.len() * 2);
    key.push_str(SYNC_DRAFT_KEY_PREFIX);
    for byte in &chat_id {
        key.push_str(&format!("{byte:02x}"));
    }
    key
}

/// What a device should do with a compose-and-send it is holding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum OutboundAuthorDecision {
    /// Nothing of this person's has this text on the wire. Author it.
    Author,
    /// A **sibling** device already authored it, on its own stream, recently
    /// enough that this can only be a draft that had not caught up. Authoring
    /// again would put a second distinct copy of one message in front of the
    /// recipient (§8, SYNC-2). Clear the composer instead.
    AlreadyAuthoredBySibling,
}

/// [`OutboundAuthorDecision`] plus who, so a shell can say something true.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct OutboundAuthorClaim {
    pub decision: OutboundAuthorDecision,
    /// The sibling that holds it, for
    /// [`OutboundAuthorDecision::AlreadyAuthoredBySibling`] only.
    pub author_device_id: Option<Vec<u8>>,
    /// That row's lamport, so a shell can scroll to the message it is about to
    /// tell somebody they already sent.
    pub lamport: u64,
}

impl OutboundAuthorClaim {
    fn author() -> Self {
        OutboundAuthorClaim {
            decision: OutboundAuthorDecision::Author,
            author_device_id: None,
            lamport: 0,
        }
    }
}

/// SYNC-2's read: has a sibling already authored exactly this, recently?
///
/// Matching is on the stored `(chat, person, kind, payload)` — the message as
/// the store holds it, not a hash of the composer's text — so a sibling's copy
/// counts whether it arrived through §8's History stream or through any other
/// route that files a row on that sibling's stream. `own_device_id` is excluded
/// rather than filtered afterwards: a second send from the same device is a
/// person repeating themselves and is none of this function's business.
///
/// [`LEGACY_DEVICE_ID`] is excluded for the same reason and a different one.
/// §5 files every pre-migration row and every row from a v1 peer on that one
/// synthetic stream, so it is not a *device* at all — it is the bucket this
/// person's own history sat in before there were devices. A row there is
/// therefore never evidence that "a sibling has this on the wire": it is
/// evidence that this person, on this install, said it once, possibly years
/// ago. Counting it would make the app refuse to re-send a message a person
/// deliberately repeated, blaming a sibling that does not exist. And the
/// exclusion cannot be left to `own_device_id` covering it, because on a linked
/// device `own_device_id` is a real id and the legacy rows are still there.
pub(crate) fn outbound_claim(
    conn: &Connection,
    own_person_id: &[u8],
    own_device_id: &[u8],
    chat_id: &[u8],
    kind: u8,
    payload: &[u8],
    not_before_ms: i64,
) -> Result<OutboundAuthorClaim, CoreError> {
    let sibling: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT sender_device_id, lamport FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND kind = ?3 AND payload = ?4
               AND sender_device_id <> ?5 AND sender_device_id <> ?7 AND timestamp >= ?6
             ORDER BY lamport DESC
             LIMIT 1",
            params![
                chat_id,
                own_person_id,
                i64::from(kind),
                payload,
                own_device_id,
                not_before_ms,
                crate::LEGACY_DEVICE_ID.to_vec(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(store_err)?;
    Ok(match sibling {
        None => OutboundAuthorClaim::author(),
        Some((author_device_id, lamport)) => OutboundAuthorClaim {
            decision: OutboundAuthorDecision::AlreadyAuthoredBySibling,
            author_device_id: Some(author_device_id),
            lamport: lamport as u64,
        },
    })
}

/// Clear a chat's shared draft, inside the transaction that just authored.
///
/// Two decisions worth reading:
///
/// * **Only when there is something to clear.** Writing an empty draft for
///   every chat a person ever sends in would put a Settings entry on the wire
///   per conversation to say nothing. A device with no draft row has nothing a
///   sibling could be holding *through this device*, and the sibling's own
///   unsynced draft is caught by [`outbound_claim`] instead.
/// * **The epoch always wins.** `put_setting` is strictly-greater, so clearing
///   at exactly the epoch the draft was written at — the same millisecond, a
///   real case when a shell stamps both from one `now_ms` — would silently do
///   nothing and leave the sibling composing a sent message. One past the
///   stored epoch is the floor.
pub(crate) fn clear_chat_draft(
    conn: &Connection,
    chat_id: &[u8],
    at_ms: i64,
) -> Result<bool, CoreError> {
    let key = core_sync_draft_key(chat_id.to_vec());
    let Some(existing) = setting(conn, &key)? else {
        return Ok(false);
    };
    if existing.value.is_empty() {
        return Ok(false);
    }
    let epoch = u64::try_from(at_ms)
        .unwrap_or(0)
        .max(existing.epoch.saturating_add(1));
    put_setting(
        conn,
        &SyncSettingEntry {
            key,
            value: Vec::new(),
            epoch,
            author_device_id: crate::store::own_authoring_device_id(conn)?,
        },
    )
}

/// SYNC-2's face for the shells.
#[uniffi::export]
impl MessageStore {
    /// Ask, before authoring, whether a sibling already put this exact message
    /// on the wire (SYNC-2).
    ///
    /// `not_before_ms` is the oldest sibling copy that still counts as "the
    /// same send" — pass `now_ms - `[`SYNC_OUTBOUND_DEDUP_WINDOW_MS`] unless
    /// there is a reason to differ. A shell that skips this call does not
    /// break: it re-authors, the recipient sees the message twice, and the
    /// fleet converges on two rows. That is precisely the product bug §8 names,
    /// which is why the convergence property tests drive authoring through here
    /// and assert the fleet never holds two copies of one text.
    pub fn core_sync_outbound_claim(
        &self,
        own_person_id: Vec<u8>,
        chat_id: Vec<u8>,
        kind: u8,
        payload: Vec<u8>,
        not_before_ms: i64,
    ) -> Result<OutboundAuthorClaim, CoreError> {
        let conn = self.locked_conn();
        let own_device_id = crate::store::own_authoring_device_id(&conn)?;
        outbound_claim(
            &conn,
            &own_person_id,
            &own_device_id,
            &chat_id,
            kind,
            &payload,
            not_before_ms,
        )
    }

    /// Write this chat's composer draft into the shared Settings stream, so the
    /// device in the person's hand is the one that finishes the sentence.
    ///
    /// `epoch` is the last-writer-wins ordering key and is milliseconds: pass
    /// the same `now_ms` the shell stamps everything else with. A stale write
    /// returns `false` and changes nothing, which is what lets a sibling that
    /// has been offline flush its typing without stamping on newer text.
    pub fn core_sync_set_chat_draft(
        &self,
        chat_id: Vec<u8>,
        draft: String,
        epoch: u64,
    ) -> Result<bool, CoreError> {
        let conn = self.locked_conn();
        let author_device_id = crate::store::own_authoring_device_id(&conn)?;
        put_setting(
            &conn,
            &SyncSettingEntry {
                key: core_sync_draft_key(chat_id),
                value: draft.into_bytes(),
                epoch,
                author_device_id,
            },
        )
    }

    /// This chat's shared draft, or `None` when there is none — including the
    /// cleared-by-authoring case, which stores an empty value so the *clear*
    /// itself can converge.
    pub fn core_sync_chat_draft(&self, chat_id: Vec<u8>) -> Result<Option<String>, CoreError> {
        let conn = self.locked_conn();
        let Some(entry) = setting(&conn, &core_sync_draft_key(chat_id))? else {
            return Ok(None);
        };
        if entry.value.is_empty() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&entry.value).into_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        generate_identity, Contact, Identity, OwnDeviceFleet, RosterVersion, StoredMessage,
        KIND_TEXT,
    };

    const NOW: i64 = 1_700_000_000_000;

    fn linked_store(own: &[u8], siblings: &[&[u8]]) -> MessageStore {
        let store = MessageStore::open(":memory:".to_string()).expect("open");
        let mut device_ids: Vec<Vec<u8>> = vec![own.to_vec()];
        device_ids.extend(siblings.iter().map(|id| id.to_vec()));
        store
            .set_own_device_fleet(OwnDeviceFleet {
                own_device_id: Some(own.to_vec()),
                device_ids,
                projected_from: RosterVersion {
                    recovery_epoch: 0,
                    seq: 1,
                },
            })
            .expect("activate");
        store
    }

    fn contact_of(identity: &Identity) -> Contact {
        Contact {
            user_id: identity.user_id.clone(),
            name: "Ash".to_string(),
            sign_pk: identity.sign_pk.clone(),
            agree_pk: identity.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    /// A row exactly as a sibling's History record lands it: the person's own
    /// outbound, on the sibling's device stream.
    fn sibling_row(
        store: &MessageStore,
        person: &Identity,
        chat: &[u8],
        device: &[u8],
        text: &str,
    ) {
        store
            .insert_incoming_message_from_device(
                StoredMessage {
                    chat_id: chat.to_vec(),
                    sender_user_id: person.user_id.clone(),
                    lamport: 1,
                    timestamp: NOW,
                    kind: KIND_TEXT,
                    payload: text.as_bytes().to_vec(),
                    sender_device_id: device.to_vec(),
                },
                Some(device.to_vec()),
                vec![0x11; 16],
                None,
                None,
            )
            .expect("sibling row");
    }

    #[test]
    fn a_siblings_recent_copy_of_the_same_text_stops_a_second_author() {
        let own = vec![0xA1; 16];
        let sibling = vec![0xB2; 16];
        let store = linked_store(&own, &[&sibling]);
        let person = generate_identity();
        let chat = generate_identity().user_id;
        sibling_row(&store, &person, &chat, &sibling, "we docked early");

        let claim = store
            .core_sync_outbound_claim(
                person.user_id.clone(),
                chat.clone(),
                KIND_TEXT,
                b"we docked early".to_vec(),
                NOW - SYNC_OUTBOUND_DEDUP_WINDOW_MS,
            )
            .expect("claim");
        assert_eq!(
            claim.decision,
            OutboundAuthorDecision::AlreadyAuthoredBySibling,
            "SYNC-2: the phone already sent this; the tablet must edit the \
             draft, not the stream"
        );
        assert_eq!(claim.author_device_id, Some(sibling));
        assert_eq!(claim.lamport, 1);
    }

    #[test]
    fn this_devices_own_repeat_is_the_persons_business_and_not_refused() {
        let own = vec![0xA1; 16];
        let store = linked_store(&own, &[&[0xB2; 16][..]]);
        let person = generate_identity();
        let chat = generate_identity().user_id;
        sibling_row(&store, &person, &chat, &own, "ok");

        assert_eq!(
            store
                .core_sync_outbound_claim(
                    person.user_id,
                    chat,
                    KIND_TEXT,
                    b"ok".to_vec(),
                    NOW - SYNC_OUTBOUND_DEDUP_WINDOW_MS,
                )
                .expect("claim")
                .decision,
            OutboundAuthorDecision::Author,
            "saying the same thing twice from one device is a decision, not a \
             fleet bug"
        );
    }

    /// §5 files every pre-migration row and every row from a v1 peer on the one
    /// synthetic legacy stream, so a row there is this person's own old history
    /// rather than a sibling's copy of what is being typed now.
    ///
    /// Left in, the app would refuse to send a message somebody deliberately
    /// repeated — "on my way" for the second time this week — and would blame a
    /// sibling that does not exist. The exclusion cannot be folded into
    /// `own_device_id`, because on a linked device that is a real id and the
    /// legacy rows are still sitting there underneath it.
    #[test]
    fn a_pre_migration_row_on_the_legacy_stream_is_not_a_siblings_copy() {
        let own = vec![0xA1; 16];
        let store = linked_store(&own, &[&[0xB2; 16][..]]);
        let person = generate_identity();
        let chat = generate_identity().user_id;
        sibling_row(
            &store,
            &person,
            &chat,
            &crate::LEGACY_DEVICE_ID,
            "on my way",
        );

        assert_eq!(
            store
                .core_sync_outbound_claim(
                    person.user_id,
                    chat,
                    KIND_TEXT,
                    b"on my way".to_vec(),
                    NOW - SYNC_OUTBOUND_DEDUP_WINDOW_MS,
                )
                .expect("claim")
                .decision,
            OutboundAuthorDecision::Author,
            "the legacy stream is where this person's own history lived before \
             there were devices; it is never evidence of a sibling"
        );
    }

    #[test]
    fn an_old_sibling_copy_falls_outside_the_window() {
        let own = vec![0xA1; 16];
        let sibling = vec![0xB2; 16];
        let store = linked_store(&own, &[&sibling]);
        let person = generate_identity();
        let chat = generate_identity().user_id;
        sibling_row(&store, &person, &chat, &sibling, "at the pool");

        assert_eq!(
            store
                .core_sync_outbound_claim(
                    person.user_id,
                    chat,
                    KIND_TEXT,
                    b"at the pool".to_vec(),
                    NOW + 1,
                )
                .expect("claim")
                .decision,
            OutboundAuthorDecision::Author,
            "yesterday's identical message is history, not a stale draft"
        );
    }

    #[test]
    fn authoring_clears_the_shared_draft_so_the_sibling_composer_empties() {
        let store = linked_store(&[0xA1; 16][..], &[&[0xB2; 16][..]]);
        let identity = generate_identity();
        let contact = contact_of(&generate_identity());
        store.upsert_contact(contact.clone()).expect("contact");

        assert!(store
            .core_sync_set_chat_draft(
                contact.user_id.clone(),
                "we docked ea".to_string(),
                NOW as u64
            )
            .expect("draft"));
        assert_eq!(
            store
                .core_sync_chat_draft(contact.user_id.clone())
                .expect("read"),
            Some("we docked ea".to_string())
        );

        store
            .author_pairwise_message(
                identity,
                contact.clone(),
                KIND_TEXT,
                b"we docked early".to_vec(),
                None,
                // Deliberately the SAME millisecond the draft was stamped
                // with: a shell that takes one clock reading per user action
                // would otherwise leave the draft standing forever.
                NOW,
            )
            .expect("author");

        assert_eq!(
            store
                .core_sync_chat_draft(contact.user_id.clone())
                .expect("read"),
            None,
            "the composer's text went out; the draft must not survive to be \
             sent again from the tablet"
        );
        let cleared = store
            .core_sync_get_setting(core_sync_draft_key(contact.user_id))
            .expect("setting")
            .expect("the clear is a stream entry, not a deletion");
        assert!(
            cleared.value.is_empty() && cleared.epoch > NOW as u64,
            "the clear itself has to converge, so it is a newer empty value \
             rather than a missing row"
        );
    }

    #[test]
    fn a_chat_with_no_draft_writes_nothing_on_send() {
        let store = linked_store(&[0xA1; 16][..], &[&[0xB2; 16][..]]);
        let identity = generate_identity();
        let contact = contact_of(&generate_identity());
        store.upsert_contact(contact.clone()).expect("contact");
        store
            .author_pairwise_message(identity, contact, KIND_TEXT, b"hi".to_vec(), None, NOW)
            .expect("author");
        assert!(
            store
                .core_sync_settings_page(64)
                .expect("settings")
                .entries
                .is_empty(),
            "an empty draft cleared on every send would put one Settings entry \
             per conversation on the wire to say nothing"
        );
    }

    /// A `.cmbak` restored without conversations must not hand a sibling back
    /// the half-typed message the user chose to leave behind — a draft is
    /// unsent message content, and the Settings stream would otherwise carry it
    /// straight to every other device on the next round.
    #[test]
    fn a_restore_that_excludes_history_keeps_the_draft_slot_and_drops_the_text() {
        let dir = std::env::temp_dir().join(format!("cm-draft-restore-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("restored.sqlite");
        let path_str = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);

        let chat = vec![0x77; 16];
        {
            let store = MessageStore::open(path_str.clone()).expect("open");
            store
                .core_sync_set_chat_draft(chat.clone(), "meet me at the".to_string(), 5)
                .expect("draft");
            store
                .core_sync_put_setting(SyncSettingEntry {
                    key: "theme".to_string(),
                    value: b"dark".to_vec(),
                    epoch: 5,
                    author_device_id: crate::LEGACY_DEVICE_ID.to_vec(),
                })
                .expect("setting");
        }

        crate::sanitize_restored_message_store_with_options(
            path_str.clone(),
            crate::BackupContentOptions {
                include_message_history: false,
                include_pending_deliveries_for_others: false,
            },
            NOW,
        )
        .expect("sanitize");

        let store = MessageStore::open(path_str).expect("reopen");
        assert_eq!(
            store.core_sync_chat_draft(chat.clone()).expect("draft"),
            None,
            "the excluded text must not survive the restore"
        );
        let cleared = store
            .core_sync_get_setting(core_sync_draft_key(chat))
            .expect("setting")
            .expect("the slot survives so the clear can converge");
        assert!(
            cleared.value.is_empty() && cleared.epoch > 5,
            "a sibling still holding the old draft must lose to this, not win \
             against a missing row"
        );
        assert_eq!(
            store
                .core_sync_get_setting("theme".to_string())
                .expect("setting")
                .expect("preferences are not message content")
                .value,
            b"dark".to_vec(),
            "excluding conversations must not wipe the person's settings"
        );
        let _ = std::fs::remove_file(dir.join("restored.sqlite"));
    }

    #[test]
    fn a_draft_key_is_one_string_for_one_chat() {
        let chat = vec![0x0a, 0xff, 0x00];
        assert_eq!(core_sync_draft_key(chat.clone()), "chat.draft.0aff00");
        assert_ne!(core_sync_draft_key(chat), core_sync_draft_key(vec![0x0a]));
    }
}
