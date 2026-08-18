//! **DL-3's carrier**: this person's roster, sealed pairwise to each contact.
//!
//! `specs/multi-device-v1.md` §4 DL-3: "Rosters gossip exactly like other
//! sealed 1:1 traffic — relay, LAN, BLE, and carry equally, sealed pairwise per
//! contact. There is no central roster service and the relay never sees roster
//! plaintext." §9 step 5 says when: the moment a link completes, the person's
//! contacts have to be told, so senders can start addressing the new device.
//! §10.1 says it again for the other direction, as the contact leg of a
//! revocation.
//!
//! Everything either statement needs already existed except the wire itself.
//! WP4 built the document codec (`core_encode_roster` / `core_decode_roster`),
//! WP5 built the revocation plan that produces the exact bytes and the exact
//! recipient list (`RevocationCommit::roster_document` / `contact_user_ids`),
//! and `MessageStore::apply_contact_roster` has been the single funnel every
//! roster acceptance runs through since WP1. What was missing was an envelope
//! kind that carries a roster document to a contact, so the send side of DL-3
//! was a plan and nothing put it on a wire. This module is that wire, and
//! [`crate::KIND_ROSTER_GOSSIP`] is the kind.
//!
//! ## It is one idempotent intent, not three triggers
//!
//! The spec names three moments a contact must be told: a link completing
//! (§9.5), a revocation (§10.1), and — the one it does not spell out but the
//! field demands — a person becoming a contact *after* the roster last changed,
//! who would otherwise never hear about it at all.
//!
//! Writing three send paths would mean three chances to disagree about who is
//! owed what. Instead there is one call, [`MessageStore::announce_own_roster`],
//! and a per-contact ledger of the roster head each contact was last told. Each
//! of the three moments is that call; so is app start, and so is any moment a
//! shell is unsure. A contact already holding the current head costs nothing, a
//! contact owed it gets exactly one envelope, and a trigger a platform forgets
//! to fire is repaired by the next one rather than lost. The staleness question
//! — *who is owed this document* — stays a core fact that neither shell
//! re-derives.
//!
//! The ledger records the head, the moment it was authored, and the lamport of
//! the envelope that carried it. That last field is what lets an entry be
//! *proven*: a gossiped roster is ordinary sealed 1:1 mail, so the contact's
//! cumulative DELIVERED receipt covers it like anything else, and once the
//! watermark passes that lamport the contact demonstrably holds the document
//! and is never told it again. Below the watermark there is no proof, so an
//! entry stands only as long as the envelope that carried it could still
//! arrive, and then the contact is owed it again — otherwise a contact out of
//! reach for that whole window is marked told on the strength of a copy that
//! expired unread, and the ACK-MD-2 churn never ends. See
//! [`announcement_covers`].
//!
//! ## What it does not do
//!
//! It never sends a roster that is not this person's own. DL-5 is unchanged and
//! is not the point here: the point is DL-3's narrower rule that gossip is
//! *pairwise*. A device gossips the document it holds about itself, to the
//! contacts it has, one sealed copy each — never a third party's roster, and
//! never to a directory. The receive side enforces the same rule from the other
//! end (`deliver_inbound_body` refuses a gossiped roster whose `person_id` is
//! not the identity that sealed it), so neither half rests on the other's good
//! behaviour.
//!
//! It also sends nothing at all before §9.4's activation gate opens: the pass
//! asks `guard_link_gate(Author)` before it walks a single contact, so a device
//! still being adopted stays silent here too. And it announces only under the
//! identity the roster is *about* — a linked device's throwaway per-device
//! identity gossips nothing, because a document about the person signed by a
//! stranger is refused at every far end anyway.

use rusqlite::{params, Connection, OptionalExtension};

use crate::device_roster::roster_head_hash;
use crate::outbound_retirement::authored_delivery_lifetime_ms;
use crate::store::store_err;
use crate::{AuthoredEnvelope, CoreError, Identity, MessageStore, KIND_ROSTER_GOSSIP};

/// How long a recorded announcement is taken as still standing *without proof*.
///
/// A gossiped roster leaves no `messages` row on screen at the far end, but it
/// is ordinary sealed 1:1 mail with an ordinary lamport, so the recipient's
/// cumulative DELIVERED receipt does cover it — once they have acked past that
/// lamport, "they were told" stops being an inference and becomes a fact this
/// device holds. That is the first test [`announcement_covers`] applies, and it
/// is why the ledger stores the authored lamport alongside the head.
///
/// This window is the fallback for everything below that watermark: a receipt
/// that has not come back yet, and one that never will. Recording authorship
/// and stopping there would mean a contact who stays out of reach for the whole
/// life of that queued copy is marked told, the copy expires undelivered, and
/// they are never told again: permanent divergence, and the ACK-MD-2 churn this
/// whole carrier exists to end would run for as long as the roster happened not
/// to change.
///
/// So an unproven announcement stands exactly as long as the envelope carrying
/// it could still be delivered, and no longer. The window is the envelope's own
/// lifetime rather than a number chosen here, because that is precisely the
/// moment the evidence for "they were told" runs out. The cost of being wrong
/// in this direction is one small sealed envelope per unreachable contact per
/// expiry window; the cost of the other direction is a contact who never learns.
fn announcement_stands_for_ms() -> i64 {
    authored_delivery_lifetime_ms(KIND_ROSTER_GOSSIP)
}

pub(crate) const ROSTER_GOSSIP_SCHEMA_SQL: &str = "
-- DL-3's per-contact ledger: the roster head this device has already told each
-- contact about. One row per contact who has been told anything, ever.
--
-- It holds no secret and nothing a contact does not already have: a head hash
-- is a hash of a document that was sealed to that contact. What it buys is
-- idempotence — the difference between 'tell everyone who is owed this' and
-- 're-send a roster to the whole contact list every time an app starts'.
--
-- A row is dropped when the contact is (`forget_person`), so re-adding a
-- contact tells them again from scratch rather than assuming a person whose
-- store row was deleted still remembers.
--
-- `announced_lamport` is the lamport of the envelope that carried the head.
-- It is what turns 'we authored it' into 'they hold it': a cumulative
-- DELIVERED receipt at or above it is proof, and proof outranks the
-- envelope-lifetime window below it (see `announcement_covers`).
CREATE TABLE IF NOT EXISTS roster_gossip_announcements (
    person_user_id    BLOB PRIMARY KEY,
    roster_head       BLOB NOT NULL,
    announced_at_ms   INTEGER NOT NULL,
    announced_lamport INTEGER NOT NULL DEFAULT 0
);
";

/// What one [`MessageStore::announce_own_roster`] pass did.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct RosterGossipAnnouncement {
    /// The head every contact in [`Self::envelopes`] is now recorded as having
    /// been told. Empty on an install that has never linked a device — there is
    /// no roster to gossip, so the whole pass is a no-op and every count below
    /// is zero.
    pub roster_head: Vec<u8>,
    /// One authored, sealed, durably queued envelope per contact newly told.
    ///
    /// The recipient is `envelope.message.chat_id` — a pairwise authored
    /// message is filed in its recipient's own thread — so a driver never has
    /// to pair this list back up against a contact list it walked separately.
    /// Sending them is the caller's, exactly as for every other authored
    /// envelope: they are already queued, so a send that fails costs a delay
    /// and not a delivery.
    pub envelopes: Vec<AuthoredEnvelope>,
    /// Contacts that already hold this head and were left alone.
    pub already_current: u32,
    /// Blocked contacts, which are told nothing — not even that this person's
    /// devices changed.
    pub skipped_blocked: u32,
    /// Contacts this pass could not author for — a store error, or a contact
    /// row that vanished under a concurrent delete.
    ///
    /// They are left *unrecorded* in the ledger on purpose, so the next pass
    /// asks about them again. A non-zero count is a delay, never a loss, and
    /// it is reported rather than swallowed so a shell can say so if it ever
    /// wants to.
    pub failed: u32,
}

impl RosterGossipAnnouncement {
    fn nothing_to_gossip() -> Self {
        RosterGossipAnnouncement {
            roster_head: Vec::new(),
            envelopes: Vec::new(),
            already_current: 0,
            skipped_blocked: 0,
            failed: 0,
        }
    }
}

#[uniffi::export]
impl MessageStore {
    /// **§9 step 5 and §10.1's contact leg: tell the contacts who are owed it.**
    ///
    /// Seals this person's current roster document pairwise to every contact
    /// that has not already been told this exact head, queues one envelope
    /// each, and records what was told. Idempotent: called twice in a row, the
    /// second call authors nothing.
    ///
    /// Call it when a link completes, when a revocation commits, when a contact
    /// is added, and at app start. None of those is a special case here — each
    /// is the same question ("who is owed the roster I hold?") asked at a moment
    /// when the answer may have changed.
    ///
    /// Returns [`RosterGossipAnnouncement::nothing_to_gossip`]'s empty shape on
    /// an install that has never linked a device. That is the overwhelming
    /// majority of the fleet and it is deliberately free: no roster, nothing to
    /// say about one, no envelope, no row.
    ///
    /// "Already told" is not taken as permanent. Nothing ever comes back to say
    /// a contact holds a roster, so a recorded announcement stands only while
    /// the envelope carrying it could still be delivered
    /// ([`announcement_stands_for_ms`]); after that the contact is owed it
    /// again. That is what stops a contact who was out of reach for the whole
    /// window from being marked told forever on the strength of a copy that
    /// expired unread.
    ///
    /// A per-contact failure is not fatal to the pass. If one contact's envelope
    /// cannot be authored — a store error, a contact row that vanished under a
    /// concurrent delete — that contact is counted in
    /// [`RosterGossipAnnouncement::failed`], left unrecorded, and the rest of
    /// the list is still told. Leaving it unrecorded is what makes the next call
    /// retry it; recording it would lose the contact silently.
    ///
    /// It gossips only under the identity the roster is *about*. A linked device
    /// signs its outbound mail with a throwaway per-device identity while the
    /// roster it holds names the person; announcing under that identity would
    /// seal a document about the person to contacts as if a stranger were
    /// vouching for them, and every recipient would refuse it (the receive side
    /// checks exactly this). So a mismatch returns the empty shape: the
    /// approving device does the routine announcing, and the sibling stays
    /// quiet rather than authoring envelopes nobody can accept.
    pub fn announce_own_roster(
        &self,
        identity: Identity,
        now_ms: i64,
    ) -> Result<RosterGossipAnnouncement, CoreError> {
        let Some(roster) = self.own_roster()? else {
            return Ok(RosterGossipAnnouncement::nothing_to_gossip());
        };
        if identity.user_id != roster.person_id {
            return Ok(RosterGossipAnnouncement::nothing_to_gossip());
        }
        // §9.4's activation gate, asked once up front rather than only inside
        // the per-contact author call, so a device still being adopted still
        // gets the plain error it always got instead of a pass that quietly
        // reports every contact as failed.
        self.guard_link_gate(crate::device_link::activation::CoreLinkGatedAction::Author)?;
        // The document, encoded once for the whole fan-out. Every contact gets
        // identical plaintext — the roster is the same public fact for
        // everybody — and differs only in the pairwise seal around it, which is
        // DL-3's whole shape.
        let document = crate::core_encode_roster(roster.clone())?;
        let head = roster_head_hash(&roster);

        let mut announcement = RosterGossipAnnouncement {
            roster_head: head.clone(),
            envelopes: Vec::new(),
            already_current: 0,
            skipped_blocked: 0,
            failed: 0,
        };
        for contact in self.list_contacts()? {
            // A blocked contact hears nothing from us, endpoint changes and
            // device changes alike (the same rule `RelayUpdateSender` follows,
            // stated here because this path never goes through it).
            if self.is_user_blocked(contact.user_id.clone())? {
                announcement.skipped_blocked += 1;
                continue;
            }
            let told = {
                let conn = self.locked_conn();
                let told = announced_head(&conn, &contact.user_id)?;
                let delivered = delivered_through(&conn, &contact.user_id, &identity.user_id)?;
                (told, delivered)
            };
            let (told, delivered) = told;
            if announcement_covers(told.as_ref(), &head, now_ms, delivered) {
                announcement.already_current += 1;
                continue;
            }
            // Per-contact, not per-pass. One contact whose row vanished under a
            // concurrent delete, or whose seal failed, must not cost the rest of
            // the list their document. Nothing is recorded for a contact that
            // failed, so the next pass simply asks again.
            let authored = match self.author_pairwise_message(
                identity.clone(),
                contact.clone(),
                KIND_ROSTER_GOSSIP,
                document.clone(),
                None,
                now_ms,
            ) {
                Ok(authored) => authored,
                Err(_) => {
                    announcement.failed += 1;
                    continue;
                }
            };
            let recorded = {
                let conn = self.locked_conn();
                record_announced(
                    &conn,
                    &contact.user_id,
                    &head,
                    now_ms,
                    authored.message.lamport as i64,
                )
            };
            if recorded.is_err() {
                // The envelope is queued and will be sent, but we cannot prove
                // to ourselves that we told them. Counting it failed keeps the
                // next pass honest; the worst case is one duplicate document.
                announcement.failed += 1;
                continue;
            }
            announcement.envelopes.push(authored);
        }
        Ok(announcement)
    }

    /// Which contacts are owed the roster this device holds — the read half of
    /// [`Self::announce_own_roster`], for a surface or a test that wants to
    /// know whether anything is outstanding without authoring it.
    ///
    /// Blocked contacts are absent: they are not owed a document they will
    /// never be sent. A contact whose announcement is neither receipt-proven
    /// nor still inside the envelope's lifetime is present again — see
    /// [`announcement_covers`].
    ///
    /// `identity` is taken for the same reason [`Self::announce_own_roster`]
    /// takes it, and answers the same way: empty when this identity is not the
    /// person the roster is about.
    pub fn roster_gossip_pending(
        &self,
        identity: Identity,
        now_ms: i64,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let Some(roster) = self.own_roster()? else {
            return Ok(Vec::new());
        };
        if identity.user_id != roster.person_id {
            return Ok(Vec::new());
        }
        let head = roster_head_hash(&roster);
        let mut pending = Vec::new();
        for contact in self.list_contacts()? {
            if self.is_user_blocked(contact.user_id.clone())? {
                continue;
            }
            let (told, delivered) = {
                let conn = self.locked_conn();
                let told = announced_head(&conn, &contact.user_id)?;
                let delivered = delivered_through(&conn, &contact.user_id, &identity.user_id)?;
                (told, delivered)
            };
            if !announcement_covers(told.as_ref(), &head, now_ms, delivered) {
                pending.push(contact.user_id);
            }
        }
        Ok(pending)
    }
}

/// What this contact was last told: the roster head, when we authored it, and
/// the lamport of the envelope that carried it. `None` for a contact that has
/// never been told anything.
#[derive(Debug, PartialEq)]
pub(crate) struct AnnouncedEntry {
    pub head: Vec<u8>,
    pub announced_at_ms: i64,
    pub announced_lamport: i64,
}

pub(crate) fn announced_head(
    conn: &Connection,
    person_user_id: &[u8],
) -> Result<Option<AnnouncedEntry>, CoreError> {
    conn.query_row(
        "SELECT roster_head, announced_at_ms, announced_lamport
         FROM roster_gossip_announcements
         WHERE person_user_id = ?1",
        params![person_user_id],
        |row| {
            Ok(AnnouncedEntry {
                head: row.get(0)?,
                announced_at_ms: row.get(1)?,
                announced_lamport: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(store_err)
}

/// This contact's cumulative DELIVERED watermark over the mail we authored to
/// them — the same `receipts` row every other delivery read uses. Zero when
/// they have never acked anything.
fn delivered_through(
    conn: &Connection,
    person_user_id: &[u8],
    own_user_id: &[u8],
) -> Result<i64, CoreError> {
    conn.query_row(
        "SELECT through_lamport FROM receipts
         WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
        params![
            person_user_id,
            own_user_id,
            crate::RECEIPT_TYPE_DELIVERED as i64
        ],
        |row| row.get(0),
    )
    .optional()
    .map_err(store_err)
    .map(|through| through.unwrap_or(0))
}

/// Whether a recorded announcement still relieves us of telling this contact
/// about `head` — the same question [`MessageStore::announce_own_roster`] and
/// [`MessageStore::roster_gossip_pending`] ask, in one place so they cannot
/// drift into disagreeing about who is owed what.
///
/// Two ways it can stand, strongest first:
///
/// * **Proof.** A gossiped roster is ordinary sealed 1:1 mail with an ordinary
///   lamport, and a cumulative DELIVERED receipt at or above that lamport says
///   the contact has it. That is a fact, not a guess, and it stands for as long
///   as the ledger row does — the whole point of DL-3's idempotence is that a
///   contact who demonstrably holds the current head is never told again.
/// * **The envelope's lifetime.** Below the watermark there is no proof either
///   way, so the announcement stands only while the copy that carried it could
///   still be delivered ([`announcement_stands_for_ms`]), and the contact is
///   owed it again afterwards.
///
/// A row written before this device recorded lamports (`announced_lamport` 0)
/// simply has no proof available and falls through to the window, which is the
/// behaviour it was written under.
fn announcement_covers(
    told: Option<&AnnouncedEntry>,
    head: &[u8],
    now_ms: i64,
    delivered_through: i64,
) -> bool {
    let Some(entry) = told else { return false };
    if entry.head.as_slice() != head {
        return false;
    }
    if entry.announced_lamport > 0 && delivered_through >= entry.announced_lamport {
        return true;
    }
    now_ms
        < entry
            .announced_at_ms
            .saturating_add(announcement_stands_for_ms())
}

pub(crate) fn record_announced(
    conn: &Connection,
    person_user_id: &[u8],
    roster_head: &[u8],
    now_ms: i64,
    announced_lamport: i64,
) -> Result<(), CoreError> {
    conn.execute(
        "INSERT INTO roster_gossip_announcements
             (person_user_id, roster_head, announced_at_ms, announced_lamport)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(person_user_id) DO UPDATE SET
             roster_head = excluded.roster_head,
             announced_at_ms = excluded.announced_at_ms,
             announced_lamport = excluded.announced_lamport",
        params![person_user_id, roster_head, now_ms, announced_lamport],
    )
    .map_err(store_err)?;
    Ok(())
}

/// Forget what a contact was told, because they are no longer a contact.
pub(crate) fn forget_person(conn: &Connection, person_user_id: &[u8]) -> Result<(), CoreError> {
    conn.execute(
        "DELETE FROM roster_gossip_announcements WHERE person_user_id = ?1",
        params![person_user_id],
    )
    .map_err(store_err)?;
    Ok(())
}

/// Drop ledger rows for people who are not contacts — the same open-time sweep
/// `roster_store` and `contact_safety` run, for the same reason: a store
/// written by an older build, or one whose delete raced a crash, must not keep
/// a stranger's row alive.
pub(crate) fn sweep_orphaned_persons(conn: &Connection) -> Result<(), CoreError> {
    conn.execute(
        "DELETE FROM roster_gossip_announcements
         WHERE person_user_id NOT IN (SELECT user_id FROM contacts)",
        [],
    )
    .map_err(store_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_link::activation::{
        core_link_genesis_roster, core_link_sign_new_device_roster,
    };
    use crate::{
        core_decode_roster, decode_extended_message_body, generate_device_keypair,
        generate_identity, open_message, Contact, DeviceKeypair,
    };

    const NOW: i64 = 1_700_000_000_000;

    fn store() -> MessageStore {
        MessageStore::open(":memory:".to_string()).expect("in-memory store")
    }

    fn contact_of(identity: &Identity, name: &str) -> Contact {
        Contact {
            user_id: identity.user_id.clone(),
            name: name.to_string(),
            sign_pk: identity.sign_pk.clone(),
            agree_pk: identity.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    /// Give `store` an own roster, the way a genesis link does.
    fn linked(store: &MessageStore, me: &Identity) -> DeviceKeypair {
        let device = generate_device_keypair();
        let roster = core_link_genesis_roster(
            me.sign_sk.clone(),
            device.sign_pk.clone(),
            device.agree_pk.clone(),
        )
        .expect("genesis roster");
        store
            .adopt_own_roster(roster, me.sign_pk.clone(), device.device_id.clone())
            .expect("adopt");
        device
    }

    #[test]
    fn an_install_that_never_linked_gossips_nothing() {
        let store = store();
        let me = generate_identity();
        let friend = generate_identity();
        store
            .upsert_contact(contact_of(&friend, "Bob"))
            .expect("contact");

        let announcement = store
            .announce_own_roster(me.clone(), NOW)
            .expect("announce is a no-op, never an error");
        assert_eq!(announcement, RosterGossipAnnouncement::nothing_to_gossip());
        assert!(store
            .roster_gossip_pending(me.clone(), NOW)
            .expect("pending")
            .is_empty());
    }

    #[test]
    fn every_contact_is_told_once_and_the_document_is_the_roster() {
        let store = store();
        let me = generate_identity();
        let bob = generate_identity();
        let carol = generate_identity();
        store.upsert_contact(contact_of(&bob, "Bob")).expect("bob");
        store
            .upsert_contact(contact_of(&carol, "Carol"))
            .expect("carol");
        linked(&store, &me);

        assert_eq!(
            store
                .roster_gossip_pending(me.clone(), NOW)
                .expect("pending")
                .len(),
            2
        );
        let first = store
            .announce_own_roster(me.clone(), NOW)
            .expect("announce");
        assert_eq!(first.envelopes.len(), 2);
        assert_eq!(first.already_current, 0);

        // The sealed body carries the roster document itself, and it opens for
        // the contact it was addressed to.
        let for_bob = first
            .envelopes
            .iter()
            .find(|authored| authored.message.chat_id == bob.user_id)
            .expect("bob is told");
        let opened = open_message(bob.clone(), for_bob.envelope.sealed.clone()).expect("opens");
        let body = decode_extended_message_body(opened.payload).expect("body");
        assert_eq!(body.kind, KIND_ROSTER_GOSSIP);
        let gossiped = core_decode_roster(body.content).expect("a roster document");
        assert_eq!(gossiped, store.own_roster().expect("roster").expect("some"));

        // Idempotent: nothing is owed, so nothing is authored.
        let second = store
            .announce_own_roster(me.clone(), NOW + 1_000)
            .expect("announce");
        assert!(second.envelopes.is_empty());
        assert_eq!(second.already_current, 2);
        assert!(store
            .roster_gossip_pending(me, NOW + 1_000)
            .expect("pending")
            .is_empty());
    }

    /// A contact who was unreachable for the whole life of the envelope we
    /// queued is owed the roster again.
    ///
    /// Nothing ever comes back to say a contact holds a gossiped roster, so
    /// "told" can only ever mean "authored". Letting that stand forever would
    /// mark the one contact who most needs the document — the one out of reach
    /// longest — as told on the strength of a copy that expired unread, and
    /// ACK-MD-2's churn for that contact would then never end.
    #[test]
    fn an_announcement_that_outlived_its_envelope_is_owed_again() {
        let store = store();
        let me = generate_identity();
        let bob = generate_identity();
        store.upsert_contact(contact_of(&bob, "Bob")).expect("bob");
        linked(&store, &me);
        store
            .announce_own_roster(me.clone(), NOW)
            .expect("first pass");

        // Still within the window the queued copy could be delivered in: the
        // contact is not re-told, because the first envelope may yet arrive.
        let inside = NOW + announcement_stands_for_ms() - 1;
        assert!(store
            .roster_gossip_pending(me.clone(), inside)
            .expect("pending")
            .is_empty());
        assert!(store
            .announce_own_roster(me.clone(), inside)
            .expect("announce")
            .envelopes
            .is_empty());

        // Past it, that copy can no longer be delivered, so the only honest
        // answer is that Bob may never have heard — and he is told again.
        let outside = NOW + announcement_stands_for_ms();
        assert_eq!(
            store
                .roster_gossip_pending(me.clone(), outside)
                .expect("pending"),
            vec![bob.user_id.clone()]
        );
        let repair = store.announce_own_roster(me, outside).expect("announce");
        assert_eq!(repair.envelopes.len(), 1);
        assert_eq!(repair.envelopes[0].message.chat_id, bob.user_id);
        assert_eq!(repair.already_current, 0);
    }

    #[test]
    fn a_contact_added_after_the_roster_is_told_on_the_next_pass() {
        let store = store();
        let me = generate_identity();
        let bob = generate_identity();
        store.upsert_contact(contact_of(&bob, "Bob")).expect("bob");
        linked(&store, &me);
        store
            .announce_own_roster(me.clone(), NOW)
            .expect("first pass");

        // §9 step 5 never fires again — the roster has not changed — so the
        // only thing that can reach a friend made afterwards is the ledger
        // noticing they hold nothing.
        let dana = generate_identity();
        store
            .upsert_contact(contact_of(&dana, "Dana"))
            .expect("dana");
        assert_eq!(
            store
                .roster_gossip_pending(me.clone(), NOW + 60_000)
                .expect("pending"),
            vec![dana.user_id.clone()]
        );
        let pass = store
            .announce_own_roster(me, NOW + 60_000)
            .expect("announce");
        assert_eq!(pass.envelopes.len(), 1);
        assert_eq!(pass.envelopes[0].message.chat_id, dana.user_id);
        assert_eq!(pass.already_current, 1);
    }

    #[test]
    fn a_changed_roster_re_tells_everybody() {
        let store = store();
        let me = generate_identity();
        let bob = generate_identity();
        store.upsert_contact(contact_of(&bob, "Bob")).expect("bob");
        let first_device = linked(&store, &me);
        store
            .announce_own_roster(me.clone(), NOW)
            .expect("first pass");

        // A second device joins the fleet: same person, new head.
        let second = generate_device_keypair();
        let grown = core_link_sign_new_device_roster(
            store.own_roster().expect("roster").expect("some"),
            me.sign_pk.clone(),
            first_device.sign_sk.clone(),
            second.sign_pk.clone(),
            second.agree_pk.clone(),
        )
        .expect("the approving device signs the new roster");
        store
            .adopt_own_roster(
                grown.roster,
                me.sign_pk.clone(),
                first_device.device_id.clone(),
            )
            .expect("adopt");

        assert_eq!(
            store
                .roster_gossip_pending(me.clone(), NOW + 120_000)
                .expect("pending"),
            vec![bob.user_id.clone()]
        );
        let pass = store
            .announce_own_roster(me, NOW + 120_000)
            .expect("announce");
        assert_eq!(pass.envelopes.len(), 1);
        assert_eq!(pass.roster_head, {
            let roster = store.own_roster().expect("roster").expect("some");
            roster_head_hash(&roster)
        });
    }

    #[test]
    fn a_blocked_contact_is_told_nothing_and_is_told_on_unblocking() {
        let store = store();
        let me = generate_identity();
        let bob = generate_identity();
        store.upsert_contact(contact_of(&bob, "Bob")).expect("bob");
        store.block_user(bob.user_id.clone(), NOW).expect("block");
        linked(&store, &me);

        let blocked_pass = store
            .announce_own_roster(me.clone(), NOW)
            .expect("announce");
        assert!(blocked_pass.envelopes.is_empty());
        assert_eq!(blocked_pass.skipped_blocked, 1);
        assert!(store
            .roster_gossip_pending(me.clone(), NOW)
            .expect("pending")
            .is_empty());

        store.unblock_user(bob.user_id.clone()).expect("unblock");
        let pass = store
            .announce_own_roster(me, NOW + 1_000)
            .expect("announce");
        assert_eq!(pass.envelopes.len(), 1);
    }

    #[test]
    fn removing_a_contact_forgets_what_they_were_told() {
        let store = store();
        let me = generate_identity();
        let bob = generate_identity();
        store.upsert_contact(contact_of(&bob, "Bob")).expect("bob");
        linked(&store, &me);
        store
            .announce_own_roster(me.clone(), NOW)
            .expect("first pass");

        assert!(store
            .delete_contact(bob.user_id.clone(), NOW + 5_000)
            .expect("remove"));
        {
            let conn = store.locked_conn();
            assert_eq!(announced_head(&conn, &bob.user_id).expect("ledger"), None);
        }
        // Re-friending tells them again rather than assuming a person whose row
        // was deleted still holds the document.
        store
            .upsert_contact(contact_of(&bob, "Bob"))
            .expect("re-friend");
        assert_eq!(
            store
                .roster_gossip_pending(me.clone(), NOW + 5_000)
                .expect("pending"),
            vec![bob.user_id]
        );
    }

    /// **A linked device must never gossip under its throwaway identity.**
    ///
    /// A device adopted into someone's fleet holds that person's roster but
    /// seals its own mail with a per-device identity. Announcing under it would
    /// send a document *about the person* signed by a stranger; every recipient
    /// refuses exactly that (`deliver_inbound_body`'s roster arm), so the only
    /// thing it could produce is a fan-out of envelopes nobody can accept.
    #[test]
    fn a_device_whose_identity_is_not_the_roster_subject_gossips_nothing() {
        let store = store();
        let me = generate_identity();
        let bob = generate_identity();
        store.upsert_contact(contact_of(&bob, "Bob")).expect("bob");
        linked(&store, &me);

        // The sibling's own identity is not the person the roster describes.
        let sibling = generate_identity();
        assert_ne!(sibling.user_id, me.user_id);
        let announcement = store
            .announce_own_roster(sibling.clone(), NOW)
            .expect("a mismatch is a quiet no-op, not an error");
        assert_eq!(announcement, RosterGossipAnnouncement::nothing_to_gossip());
        assert!(store
            .roster_gossip_pending(sibling, NOW)
            .expect("pending")
            .is_empty());

        // The approving device, whose identity *is* the subject, still gossips.
        assert_eq!(
            store
                .announce_own_roster(me.clone(), NOW)
                .expect("announce")
                .envelopes
                .len(),
            1
        );
    }

    /// A contact who has acked past the envelope that carried the roster holds
    /// it, and holds it for good — the window never re-tells them.
    #[test]
    fn a_receipt_covering_the_announcement_stands_past_the_envelope_window() {
        let store = store();
        let me = generate_identity();
        let bob = generate_identity();
        store.upsert_contact(contact_of(&bob, "Bob")).expect("bob");
        linked(&store, &me);

        let first = store
            .announce_own_roster(me.clone(), NOW)
            .expect("first pass");
        let lamport = first.envelopes[0].message.lamport;

        // Bob's cumulative DELIVERED receipt reaches the roster's lamport.
        store
            .record_receipt(
                bob.user_id.clone(),
                me.user_id.clone(),
                crate::RECEIPT_TYPE_DELIVERED,
                lamport,
                None,
                None,
            )
            .expect("bob acks");

        // Long past the point an unproven announcement would have lapsed.
        let long_after = NOW + announcement_stands_for_ms() * 3;
        assert!(store
            .roster_gossip_pending(me.clone(), long_after)
            .expect("pending")
            .is_empty());
        assert!(store
            .announce_own_roster(me, long_after)
            .expect("announce")
            .envelopes
            .is_empty());
    }

    /// Without that receipt the window still governs: the same moment re-tells.
    #[test]
    fn an_unproven_announcement_still_lapses_with_the_envelope() {
        let store = store();
        let me = generate_identity();
        let bob = generate_identity();
        store.upsert_contact(contact_of(&bob, "Bob")).expect("bob");
        linked(&store, &me);
        store
            .announce_own_roster(me.clone(), NOW)
            .expect("first pass");

        let long_after = NOW + announcement_stands_for_ms() * 3;
        assert_eq!(
            store
                .roster_gossip_pending(me, long_after)
                .expect("pending"),
            vec![bob.user_id]
        );
    }

    /// One contact that cannot be sealed to costs only that contact.
    ///
    /// The pass counts them in `failed`, records nothing for them, and tells
    /// everybody else. Recording them would lose them silently; failing the
    /// whole call would let one bad row keep the entire contact list uninformed.
    #[test]
    fn a_contact_that_cannot_be_authored_for_is_counted_and_retried() {
        let store = store();
        let me = generate_identity();
        let bob = generate_identity();
        let broken = generate_identity();
        store.upsert_contact(contact_of(&bob, "Bob")).expect("bob");
        let mut bad = contact_of(&broken, "Broken");
        bad.agree_pk = vec![7u8; 3];
        store.upsert_contact(bad).expect("a contact row exists");
        linked(&store, &me);

        let pass = store
            .announce_own_roster(me.clone(), NOW)
            .expect("one bad contact is not a failed pass");
        assert_eq!(pass.failed, 1);
        assert_eq!(pass.envelopes.len(), 1);
        assert_eq!(pass.envelopes[0].message.chat_id, bob.user_id);

        // Nothing was recorded for them, so the next pass asks again.
        assert_eq!(
            store
                .roster_gossip_pending(me.clone(), NOW + 1_000)
                .expect("pending"),
            vec![broken.user_id]
        );
        assert_eq!(
            store
                .announce_own_roster(me, NOW + 1_000)
                .expect("second pass")
                .failed,
            1
        );
    }
}
