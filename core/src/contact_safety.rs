//! What a contact's roster change means for the person looking at it
//! (`specs/multi-device-v1.md` §10 step 4).
//!
//! §10's fourth step is one sentence — "contacts get the standard
//! changed-safety-state surface treatment" — and this module is its core half:
//! the *facts*, with reason codes, durable and acknowledgeable. The copy, the
//! banner, the badge and the tap target are WP6's; nothing here renders
//! anything or decides how loud it should be.
//!
//! # Why a fact table and not a computed predicate
//!
//! A changed safety state is an *event*, not a property. By the time a person
//! opens the app the roster that buried a device is simply the current roster,
//! and asking "is anything wrong?" of it returns no. The thing worth telling
//! somebody is that it **changed**, and that survives only if it is written
//! down when it happens. This is the same shape as `identity_clone_warnings`
//! (WPT's clone guard) and it is acknowledged the same way: a person clears it,
//! and a later change raises a fresh one.
//!
//! # The three reasons, and the one that is deliberately absent
//!
//! [`ContactSafetyReason`] names three changes. A **device being added** is not
//! among them, and that omission is load-bearing rather than an oversight: §2
//! goal 1 says a person's device count is invisible to other users, which is
//! why [`crate::MessageStore::apply_contact_roster`] already strips the §14.3
//! soft-cap warning off the contact path. A surface that announced "Bob added a
//! phone" would disclose from gossip exactly what that goal protects, and it
//! would fire on every ordinary link — the family-obvious surface would become
//! noise, and noise is how a real warning gets swiped away unread.
//!
//! Revocation is different, and §10.4 says so directly. It is the change that
//! means *something was taken away from someone*, and the person on the other
//! end has a reason to know: mail they send from here on is sealed to a new
//! inbox generation, and a device they may have verified in person no longer
//! speaks for their friend.
//!
//! # A first roster raises nothing
//!
//! [`core_roster_safety_changes`] returns nothing when there was no stored
//! roster, even if the incoming document carries tombstones. A contact who has
//! just gossiped their first roster is not reporting a change — they are
//! telling this device, for the first time, what their fleet looks like, and
//! its burials happened before this device knew the person had devices at all.
//! Announcing them would put a safety warning on the ordinary act of a friend
//! upgrading their app.

use rusqlite::{params, Connection};

use crate::device_roster::{Roster, RosterUpdateDecision, RosterUpdateOutcome, RosterUpdateReason};
use crate::revocation::core_roster_newly_revoked;
use crate::store::store_err;
use crate::{CoreError, MessageStore, DEVICE_ID_LEN};

pub(crate) const CONTACT_SAFETY_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS contact_safety_facts (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    person_user_id BLOB NOT NULL,
    reason         INTEGER NOT NULL,
    -- The roster version the fact is ABOUT: the incoming document for an
    -- accepted change, the stored one being defended for a fork. Part of the
    -- uniqueness key, so idempotent gossip and a re-synced copy of the same
    -- change raise one fact rather than a stream of them.
    recovery_epoch INTEGER NOT NULL,
    seq            INTEGER NOT NULL,
    -- The revoked device ids, concatenated at their fixed width. Empty for
    -- every reason that names no device.
    device_ids     BLOB NOT NULL,
    acknowledged   INTEGER NOT NULL DEFAULT 0,
    UNIQUE(person_user_id, reason, recovery_epoch, seq)
);
CREATE INDEX IF NOT EXISTS idx_contact_safety_facts_person
    ON contact_safety_facts(person_user_id, id DESC);
";

/// The most facts one read hands back, newest first.
///
/// Every list that crosses the FFI boundary is bounded, and this one needs a
/// bound for a slow reason rather than an urgent one: facts are append-only and
/// a long-lived person linking and revoking devices over years accumulates
/// them. Newest-first means the cap can only ever drop old acknowledged
/// history, never a warning still waiting to be read.
pub const CONTACT_SAFETY_FACT_PAGE: u32 = 200;

/// Why a contact's safety state changed (§10.4). Reason codes only — the copy
/// that goes with each one is WP6's, and lives in `strings.xml` /
/// `Localizable.xcstrings` like every other user-facing string.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactSafetyReason {
    /// §10.1 / DL-4: this contact buried one or more of their devices, and
    /// [`ContactSafetyFact::device_ids`] names which. Mail authored from here
    /// on is sealed to a new inbox generation (§6), and events newly signed by
    /// a buried device are refused (§10.3).
    DeviceRevoked,
    /// §14.2 seen from outside: this contact's roster climbed to a higher
    /// `recovery_epoch`, and only the person root inside their
    /// passphrase-encrypted `.cmbak` can sign that. Somebody opened that
    /// backup — the owner recovering, or, if the owner did not, the single
    /// most important thing this surface can say.
    IdentityRecovered,
    /// DL-2: two verified rosters at the same `(recovery_epoch, seq)` with
    /// different content. The stored one is kept, this person's roster updates
    /// are quarantined from here on, and the spec asks for the same treatment
    /// as a changed fingerprint. Never auto-resolved — this fact is raised
    /// once per stored version and cleared only by a person.
    RosterForked,
}

impl ContactSafetyReason {
    /// The stable on-disk code. Written out rather than derived from the
    /// variant order so that reordering the enum — which a future reason
    /// naturally would — cannot silently re-label rows already in the field.
    fn wire(self) -> i64 {
        match self {
            ContactSafetyReason::DeviceRevoked => 1,
            ContactSafetyReason::IdentityRecovered => 2,
            ContactSafetyReason::RosterForked => 3,
        }
    }

    fn from_wire(code: i64) -> Result<Self, CoreError> {
        match code {
            1 => Ok(ContactSafetyReason::DeviceRevoked),
            2 => Ok(ContactSafetyReason::IdentityRecovered),
            3 => Ok(ContactSafetyReason::RosterForked),
            other => Err(CoreError::Store(format!(
                "stored contact safety fact has unknown reason code {other}"
            ))),
        }
    }
}

/// One changed-safety-state fact, before it is written down.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ContactSafetyChange {
    pub reason: ContactSafetyReason,
    /// The device ids the change names — the newly buried ones for
    /// [`ContactSafetyReason::DeviceRevoked`], empty for every other reason.
    pub device_ids: Vec<Vec<u8>>,
    /// The roster version this fact is about.
    pub recovery_epoch: u64,
    pub seq: u64,
}

/// A stored fact, as a surface reads it back.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ContactSafetyFact {
    pub person_user_id: Vec<u8>,
    pub reason: ContactSafetyReason,
    pub device_ids: Vec<Vec<u8>>,
    pub recovery_epoch: u64,
    pub seq: u64,
    /// Observation order across every contact, monotone and never reused.
    ///
    /// It is deliberately not a wall clock. Nothing on the write path has one:
    /// [`MessageStore::apply_contact_roster`] is reached from pairwise gossip
    /// (DL-3) and from a sibling's self-sync alike, and neither carries a
    /// trustworthy time for when the *change* happened — the roster document
    /// has no timestamp at all, by design (§4). An order this device can vouch
    /// for beats a time it would have to invent.
    pub observed_seq: u64,
    pub acknowledged: bool,
}

/// Classify what an applied roster decision changed about a contact's safety
/// state (§10.4). Pure: no store, no clock, no transport.
///
/// `previous` is what was stored before the decision ran, and `decision` is
/// what [`crate::core_roster_accept`] said about `incoming`. The returned facts
/// are in a fixed order — fork, recovery, revocation — so a caller that writes
/// them down gets the same observation order on every device.
///
/// Two changes genuinely can arrive together: §14.2's recovery-revocation
/// signs a new epoch *and* buries the stolen approving device in one document,
/// and both halves are worth saying. They come back as two facts rather than
/// one blended reason, because a person acknowledging "yes, I recovered my
/// account" has not thereby acknowledged which device was cut off.
#[uniffi::export]
pub fn core_roster_safety_changes(
    previous: Option<Roster>,
    incoming: Roster,
    decision: RosterUpdateDecision,
) -> Vec<ContactSafetyChange> {
    let Some(previous) = previous else {
        // See the module docs: a first roster is an introduction, not a change.
        return Vec::new();
    };
    let mut changes = Vec::new();
    match decision.outcome {
        RosterUpdateOutcome::ForkQuarantined => {
            // Raised on the transition only. A person whose rosters are
            // already quarantined keeps gossiping perfectly good documents
            // that this device keeps refusing (`PersonQuarantined`), and each
            // one is the same unresolved fork, not a new one.
            if decision.reason == RosterUpdateReason::ForkedContent {
                changes.push(ContactSafetyChange {
                    reason: ContactSafetyReason::RosterForked,
                    device_ids: Vec::new(),
                    // The version being DEFENDED, which is what a fork is a
                    // fork of, and which does not move while the quarantine
                    // stands — so repeat gossip dedupes onto one fact.
                    recovery_epoch: previous.recovery_epoch,
                    seq: previous.seq,
                });
            }
        }
        RosterUpdateOutcome::Accepted => {
            if incoming.recovery_epoch > previous.recovery_epoch {
                changes.push(ContactSafetyChange {
                    reason: ContactSafetyReason::IdentityRecovered,
                    device_ids: Vec::new(),
                    recovery_epoch: incoming.recovery_epoch,
                    seq: incoming.seq,
                });
            }
            let revoked = core_roster_newly_revoked(Some(previous), incoming.clone());
            if !revoked.is_empty() {
                changes.push(ContactSafetyChange {
                    reason: ContactSafetyReason::DeviceRevoked,
                    device_ids: revoked,
                    recovery_epoch: incoming.recovery_epoch,
                    seq: incoming.seq,
                });
            }
        }
        // A document that changed nothing stored changed nothing to surface.
        RosterUpdateOutcome::Ignored => {}
    }
    changes
}

#[uniffi::export]
impl MessageStore {
    /// The §10.4 facts this device holds, newest first, at most
    /// [`CONTACT_SAFETY_FACT_PAGE`] of them.
    ///
    /// `include_acknowledged` is what separates the badge from the history: a
    /// surface asking "is there anything to tell this person" passes `false`,
    /// and a safety screen listing what has happened passes `true`.
    pub fn contact_safety_facts(
        &self,
        include_acknowledged: bool,
    ) -> Result<Vec<ContactSafetyFact>, CoreError> {
        let conn = self.locked_conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, person_user_id, reason, recovery_epoch, seq, device_ids, acknowledged
                 FROM contact_safety_facts
                 WHERE (?1 != 0 OR acknowledged = 0)
                 ORDER BY id DESC
                 LIMIT ?2",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(
                params![i64::from(include_acknowledged), CONTACT_SAFETY_FACT_PAGE],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, Vec<u8>>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .map_err(store_err)?;
        let mut facts = Vec::new();
        for row in rows {
            let (id, person_user_id, reason, recovery_epoch, seq, device_ids, acknowledged) =
                row.map_err(store_err)?;
            facts.push(ContactSafetyFact {
                person_user_id,
                reason: ContactSafetyReason::from_wire(reason)?,
                device_ids: decode_device_ids(&device_ids)?,
                recovery_epoch: recovery_epoch as u64,
                seq: seq as u64,
                observed_seq: id as u64,
                acknowledged: acknowledged != 0,
            });
        }
        Ok(facts)
    }

    /// Mark one contact's facts acknowledged through
    /// [`ContactSafetyFact::observed_seq`], returning how many moved.
    ///
    /// "Through a watermark" rather than "by id" so that acknowledging what a
    /// person actually saw cannot silently clear a fact that arrived while the
    /// screen was open: a fact observed after the watermark the surface was
    /// showing stays unacknowledged, and comes back.
    ///
    /// Acknowledgement is not resolution. A quarantined fork stays quarantined
    /// (DL-2: only a person resolves a fork, and clearing this row is not that
    /// act), a tombstone stays forever (DL-4), and §10.3's refusal of a buried
    /// device's new events is unaffected. This clears a *notification*, and
    /// nothing else.
    pub fn acknowledge_contact_safety_facts(
        &self,
        person_user_id: Vec<u8>,
        through_observed_seq: u64,
    ) -> Result<u32, CoreError> {
        let conn = self.locked_conn();
        let moved = conn
            .execute(
                "UPDATE contact_safety_facts SET acknowledged = 1
                 WHERE person_user_id = ?1 AND id <= ?2 AND acknowledged = 0",
                params![person_user_id, through_observed_seq as i64],
            )
            .map_err(store_err)?;
        Ok(moved as u32)
    }
}

/// Write the classified changes for one person, ignoring any this device has
/// already recorded.
///
/// Called inside `apply_contact_roster`'s transaction, so a fact and the roster
/// that produced it land together or not at all — a surface can never claim a
/// revocation the store did not accept.
pub(crate) fn record_changes(
    conn: &Connection,
    person_user_id: &[u8],
    changes: &[ContactSafetyChange],
) -> Result<(), CoreError> {
    for change in changes {
        conn.execute(
            "INSERT OR IGNORE INTO contact_safety_facts
                (person_user_id, reason, recovery_epoch, seq, device_ids)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                person_user_id,
                change.reason.wire(),
                change.recovery_epoch as i64,
                change.seq as i64,
                encode_device_ids(&change.device_ids),
            ],
        )
        .map_err(store_err)?;
    }
    Ok(())
}

/// A person's facts go when the person does, exactly as their roster does:
/// a warning about somebody this user removed has nobody to be about.
pub(crate) fn delete_person(conn: &Connection, person_user_id: &[u8]) -> Result<(), CoreError> {
    conn.execute(
        "DELETE FROM contact_safety_facts WHERE person_user_id = ?1",
        params![person_user_id],
    )
    .map_err(store_err)?;
    Ok(())
}

/// The open-time sweep, mirroring `roster_store::sweep_orphaned_persons` for
/// the paths that do not run `delete_contact` (a restored `.cmbak` from a build
/// whose delete path differed, an interrupted write).
pub(crate) fn sweep_orphaned_persons(conn: &Connection) -> Result<(), CoreError> {
    conn.execute(
        "DELETE FROM contact_safety_facts
         WHERE person_user_id NOT IN (SELECT user_id FROM contacts)",
        [],
    )
    .map_err(store_err)?;
    Ok(())
}

fn encode_device_ids(device_ids: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(device_ids.len() * DEVICE_ID_LEN);
    for device_id in device_ids {
        out.extend_from_slice(device_id);
    }
    out
}

/// Device ids are fixed width (`core_roster_validate` enforces it on every
/// document these ever come from), so the blob is a plain concatenation and a
/// length that is not a multiple of it is local damage rather than a short id.
fn decode_device_ids(blob: &[u8]) -> Result<Vec<Vec<u8>>, CoreError> {
    if !blob.len().is_multiple_of(DEVICE_ID_LEN) {
        return Err(CoreError::Store(
            "stored contact safety fact has a malformed device id list".to_string(),
        ));
    }
    Ok(blob.chunks(DEVICE_ID_LEN).map(<[u8]>::to_vec).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_link::activation::{
        core_link_genesis_roster, core_link_sign_new_device_roster,
    };
    use crate::device_roster::{core_roster_accept, core_sign_roster, generate_device_keypair};
    use crate::identity::generate_identity;
    use crate::revocation::{core_recovery_revoke_roster, core_revoke_devices_roster};
    use crate::sync_record::core_mint_inbox_key;
    use crate::{Contact, Identity};

    /// A contact with two devices, built through the shipped link ceremony, and
    /// this device's view of them.
    struct Friend {
        person: Identity,
        approver: crate::DeviceKeypair,
        sibling: crate::DeviceKeypair,
        genesis: Roster,
        roster: Roster,
    }

    fn friend() -> Friend {
        let person = generate_identity();
        let approver = generate_device_keypair();
        let sibling = generate_device_keypair();
        let genesis = core_link_genesis_roster(
            person.sign_sk.clone(),
            approver.sign_pk.clone(),
            approver.agree_pk.clone(),
        )
        .expect("genesis");
        let roster = core_link_sign_new_device_roster(
            genesis.clone(),
            person.sign_pk.clone(),
            approver.sign_sk.clone(),
            sibling.sign_pk.clone(),
            sibling.agree_pk.clone(),
        )
        .expect("link")
        .roster;
        Friend {
            person,
            approver,
            sibling,
            genesis,
            roster,
        }
    }

    impl Friend {
        fn contact(&self) -> Contact {
            Contact {
                user_id: self.person.user_id.clone(),
                name: "Bob".to_string(),
                sign_pk: self.person.sign_pk.clone(),
                agree_pk: self.person.agree_pk.clone(),
                relay_url: None,
                relay_token: None,
                nickname: None,
            }
        }

        fn revoke_sibling(&self) -> Roster {
            core_revoke_devices_roster(
                self.roster.clone(),
                self.person.sign_pk.clone(),
                self.approver.sign_sk.clone(),
                vec![self.sibling.device_id.clone()],
                core_mint_inbox_key(self.roster.inbox_key_generation),
            )
            .expect("revocation")
            .roster
        }
    }

    fn store_with(friend: &Friend) -> MessageStore {
        let store = MessageStore::open(":memory:".to_string()).expect("open");
        store.upsert_contact(friend.contact()).expect("contact");
        store
    }

    fn changes_for(
        previous: Option<Roster>,
        incoming: Roster,
        root: Vec<u8>,
    ) -> Vec<ContactSafetyChange> {
        let decision = core_roster_accept(previous.clone(), false, incoming.clone(), root);
        core_roster_safety_changes(previous, incoming, decision)
    }

    #[test]
    fn a_first_roster_is_an_introduction_not_a_change() {
        let friend = friend();
        // Even one carrying burials: this device is learning the fleet, and the
        // burials predate its knowing the person had devices at all.
        let revoked = friend.revoke_sibling();
        assert!(!revoked.tombstones.is_empty());
        assert!(changes_for(None, revoked, friend.person.sign_pk).is_empty());
    }

    #[test]
    fn a_revocation_raises_exactly_the_ids_it_buried() {
        let friend = friend();
        let changes = changes_for(
            Some(friend.roster.clone()),
            friend.revoke_sibling(),
            friend.person.sign_pk.clone(),
        );
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].reason, ContactSafetyReason::DeviceRevoked);
        assert_eq!(
            changes[0].device_ids,
            vec![friend.sibling.device_id.clone()]
        );
        assert_eq!(changes[0].seq, friend.roster.seq + 1);
    }

    #[test]
    fn adding_a_device_is_never_a_safety_change() {
        // §2 goal 1: a person's device count is invisible to other users. The
        // ordinary act of a friend linking a tablet must raise nothing.
        let friend = friend();
        assert!(changes_for(
            Some(friend.genesis.clone()),
            friend.roster.clone(),
            friend.person.sign_pk.clone(),
        )
        .is_empty());
        // And idempotent gossip of what is already stored raises nothing.
        assert!(changes_for(
            Some(friend.roster.clone()),
            friend.roster.clone(),
            friend.person.sign_pk,
        )
        .is_empty());
    }

    #[test]
    fn a_recovery_that_buries_a_device_raises_both_facts() {
        let friend = friend();
        let rescue = generate_device_keypair();
        let recovery = core_recovery_revoke_roster(
            friend.roster.clone(),
            friend.person.sign_sk.clone(),
            rescue.sign_pk,
            rescue.agree_pk,
            vec![friend.approver.device_id.clone()],
            None,
        )
        .expect("recovery")
        .roster;
        let changes = changes_for(
            Some(friend.roster.clone()),
            recovery,
            friend.person.sign_pk.clone(),
        );
        // Two facts, in the pinned order, because acknowledging "I recovered my
        // account" is not acknowledging which device was cut off.
        assert_eq!(
            changes
                .iter()
                .map(|change| change.reason)
                .collect::<Vec<_>>(),
            vec![
                ContactSafetyReason::IdentityRecovered,
                ContactSafetyReason::DeviceRevoked,
            ]
        );
        assert_eq!(
            changes[1].device_ids,
            vec![friend.approver.device_id.clone()]
        );
    }

    #[test]
    fn a_fork_raises_one_fact_against_the_version_it_defends() {
        let friend = friend();
        // DL-2: the same (recovery_epoch, seq), different content. Re-signing
        // the stored roster with a different device set at the same version is
        // exactly the fork the rule describes.
        let forked = core_sign_roster(
            Roster {
                devices: vec![friend.roster.devices[0].clone()],
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
                ..friend.roster.clone()
            },
            friend.approver.sign_sk.clone(),
        )
        .expect("signs");
        let decision = core_roster_accept(
            Some(friend.roster.clone()),
            false,
            forked.clone(),
            friend.person.sign_pk.clone(),
        );
        assert_eq!(decision.outcome, RosterUpdateOutcome::ForkQuarantined);
        let changes =
            core_roster_safety_changes(Some(friend.roster.clone()), forked.clone(), decision);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].reason, ContactSafetyReason::RosterForked);
        assert_eq!(changes[0].seq, friend.roster.seq);

        // Repeat gossip while quarantined is the same unresolved fork, not a
        // new one: the reason is `PersonQuarantined` and nothing is raised.
        let again = core_roster_accept(
            Some(friend.roster.clone()),
            true,
            forked.clone(),
            friend.person.sign_pk,
        );
        assert_eq!(again.reason, RosterUpdateReason::PersonQuarantined);
        assert!(core_roster_safety_changes(Some(friend.roster), forked, again).is_empty());
    }

    #[test]
    fn facts_are_stored_deduped_and_acknowledged_through_a_watermark() {
        let friend = friend();
        let store = store_with(&friend);
        store
            .apply_contact_roster(friend.roster.clone())
            .expect("first roster");
        // The introduction raised nothing.
        assert!(store.contact_safety_facts(true).expect("facts").is_empty());

        let revoked = friend.revoke_sibling();
        store.apply_contact_roster(revoked.clone()).expect("revoke");
        let facts = store.contact_safety_facts(false).expect("facts");
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].reason, ContactSafetyReason::DeviceRevoked);
        assert_eq!(facts[0].person_user_id, friend.person.user_id);
        assert_eq!(facts[0].device_ids, vec![friend.sibling.device_id.clone()]);
        assert!(!facts[0].acknowledged);

        // The same document arriving again — by gossip, then by a sibling's
        // self-sync — is idempotent all the way to the surface.
        store.apply_contact_roster(revoked).expect("again");
        assert_eq!(store.contact_safety_facts(false).expect("facts").len(), 1);

        let moved = store
            .acknowledge_contact_safety_facts(friend.person.user_id.clone(), facts[0].observed_seq)
            .expect("acknowledge");
        assert_eq!(moved, 1);
        assert!(store.contact_safety_facts(false).expect("facts").is_empty());
        // Acknowledged is not deleted: the safety screen still shows it.
        assert_eq!(store.contact_safety_facts(true).expect("facts").len(), 1);
    }

    #[test]
    fn a_fact_raised_after_the_watermark_survives_the_acknowledgement() {
        let friend = friend();
        let store = store_with(&friend);
        store
            .apply_contact_roster(friend.roster.clone())
            .expect("first roster");
        let first = friend.revoke_sibling();
        store.apply_contact_roster(first.clone()).expect("revoke");
        let seen = store.contact_safety_facts(false).expect("facts")[0].observed_seq;

        // A second change lands while the screen showing `seen` is open.
        let rescue = generate_device_keypair();
        let recovery = core_recovery_revoke_roster(
            first,
            friend.person.sign_sk.clone(),
            rescue.sign_pk,
            rescue.agree_pk,
            vec![friend.approver.device_id.clone()],
            None,
        )
        .expect("recovery")
        .roster;
        store.apply_contact_roster(recovery).expect("recovery");
        assert_eq!(store.contact_safety_facts(false).expect("facts").len(), 3);

        assert_eq!(
            store
                .acknowledge_contact_safety_facts(friend.person.user_id.clone(), seen)
                .expect("acknowledge"),
            1
        );
        // The two the person never saw are still waiting, newest first.
        let waiting = store.contact_safety_facts(false).expect("facts");
        assert_eq!(waiting.len(), 2);
        assert!(waiting[0].observed_seq > waiting[1].observed_seq);
    }

    #[test]
    fn deleting_a_contact_takes_their_facts_with_them() {
        let friend = friend();
        let store = store_with(&friend);
        store
            .apply_contact_roster(friend.roster.clone())
            .expect("first roster");
        store
            .apply_contact_roster(friend.revoke_sibling())
            .expect("revoke");
        assert_eq!(store.contact_safety_facts(true).expect("facts").len(), 1);
        assert!(store
            .delete_contact(friend.person.user_id.clone(), 1_755_000_000_000)
            .expect("delete"));
        assert!(store.contact_safety_facts(true).expect("facts").is_empty());
    }
}
