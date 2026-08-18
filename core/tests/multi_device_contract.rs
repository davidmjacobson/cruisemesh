//! Multi-device v1 WP0 contract vectors.
//!
//! Implemented vectors drive today's core. Future-surface vectors deliberately
//! remain data-only: they describe the accepted v1 rules without a test-only
//! roster, key, or linking implementation.
//!
//! WP1 moved most of this file out of the second category. Every roster vector
//! now runs the shipped `core_roster_accept` through the shipped
//! `MessageStore::apply_contact_roster`, and both stream vectors run a real
//! body through encode → seal → open → decode → device-aware insert.
//!
//! WP2 moved the rest of what it owned. The ack vectors now address rows the
//! way a real §7 fan-out does (`core_device_namespace_id`,
//! `device_fanout_msg_id`) and run the production ack planner against a fleet
//! written through the shipped `set_own_device_fleet`; the §14.3 cap vectors
//! perform a real add through `apply_contact_roster` instead of asking the cap
//! policy function what it thinks. What stays data-only stays that way because
//! the mechanism genuinely does not exist yet, not because driving it was
//! inconvenient:
//!
//! * MD-ROSTER-PAIRWISE-GOSSIP. Half of DL-3 already ships: the roster head
//!   rides the `CMFRIEND4` card and the roster-head TLV travels inside the
//!   pairwise seal, so nothing about a roster is ever exposed to a relay or a
//!   directory. The missing piece is narrower than "gossip does not exist" —
//!   there is no envelope kind that carries the roster *document* itself, so
//!   there is no end-to-end path to drive. WP4/WP5 own that carrier.
//!   **WP6 closed it**, see below.
//! * MD-SEAL-STALE-ROSTER needs §6's inbox key generations. WP2 came and
//!   went without minting them -- it generalized addressing and acks, not
//!   sealing -- so this one is WP5's alone now. **WP5 closed it**, see below.
//! * MD-ROSTER-FIRST-CONTACT-ANCHOR needs a second source of truth about a
//!   person's recovery epoch (WP5's recovery flow).
//! * MD-RECOVERY-ROOT-CUSTODY needs the person root minted and stored apart
//!   from the identity key (WP3).
//! * MD-ROSTER-GOSSIP-TO-CONTACTS is new with WP3, and it is the dormancy note
//!   that matters most right now. §9 step 5 says the approving device tells the
//!   person's CONTACTS about the new roster; DL-3's send side does not exist.
//!   WP4 owns the own-device sync records that would carry it and WP5 owns the
//!   contact notification, so nothing in WP3 can close it.
//!   **WP6 closed it**, see below.
//!
//!   The consequence is concrete and worth stating rather than implying. WP3
//!   makes fleets larger than one device real, and ACK-MD-2 forbids such a
//!   fleet from acking the single person-addressed row a contact who has not
//!   heard about the roster keeps uploading. Nobody deletes those rows, so each
//!   one churns until its 7-day expiry. Bounded, and dev-only: linking is
//!   behind Internal Tools until WP6, so the only fleets that exist are the
//!   ones being deliberately tested.
//!
//! WP4 moved `MD-SYNC-BLE-DAY-CONVERGE` out of that list — §8's records,
//! digests and backfill plan exist, so the day is driven rather than described —
//! and added three vectors of its own:
//!
//! * `MD-STREAM-AUTHOR-OWN-DEVICE` and `MD-STREAM-AUTHOR-UNLINKED`. WP1 built
//!   §5's stream key and the sealed-body field that fills it, then deliberately
//!   left every authoring call site on the legacy stream, because §9.4 says a
//!   device authors nothing under an id no roster has named. WP4 is where the
//!   id exists, so the pair pins both halves at once: an activated device
//!   authors on its own stream in the store AND inside the seal, and an install
//!   that has never linked emits byte-identical envelopes to today's.
//! * `MD-SYNC-OUTBOUND-DEDUP` for SYNC-2, driven through the shipped claim on a
//!   store holding a sibling's authored row.
//!
//! WP5's revocation slice moved exactly one vector, and moved nothing else:
//!
//! * `MD-SEAL-STALE-ROSTER` left the data-only list because §10.1's rotation
//!   exists. Its driver performs a real revocation through
//!   `core_revoke_devices_roster` and then asserts both ends of §6's accepted
//!   window: a months-offline contact sealing to the roster it still holds
//!   reaches the surviving devices, the row it addressed to the buried device is
//!   a different row that starves nobody, the revocation supersedes the stale
//!   roster the moment it arrives, and a replayed pre-revocation generation
//!   cannot pull sealing back (`InboxGenerationRegressed`).
//! * `MD-ROSTER-GOSSIP-TO-CONTACTS` did NOT move, and the reason is narrower
//!   than it was. §10.1's own-device leg is executed — the rotation announcement
//!   is a real signed record, sealed once per surviving sibling — and the
//!   contact leg now produces the exact document and the exact recipient list
//!   (`RevocationCommit::roster_document` / `contact_user_ids`). What is still
//!   missing is the same thing WP3 named: no envelope kind carries a roster
//!   document to a contact. A plan is not a delivery, so the vector stays
//!   data-only until the carrier lands.
//!
//! WP5's relay-token slice (§10 step 2) moved no vector at all, which is worth
//! saying out loud rather than leaving to inference. There has never been a
//! contract vector for the family relay credential — the ledger's subject is
//! rosters, streams and acks, and `family_token` is none of those — so the
//! rotation is pinned where its behaviour actually lives:
//! `core/src/relay_rotation.rs` for the ceremony and its accepted windows, and
//! `relayd/src/lib.rs` for the two halves of §13's WP5 gate that only the
//! server can prove (a revoked device demonstrably losing fetch *and ack*
//! after rotation; every un-fetched row carried across with its id, so no
//! sibling is starved to cut the thief off).
//!
//! One thing it did change under this file's nose, and it is a *widening* of
//! the mixed-fleet story rather than a narrowing: `MD-SEAL-STALE-ROSTER`'s
//! months-offline contact now has a second way to be stranded — their friend
//! card's relay credential dies with the rotation, not only their roster going
//! stale. Both windows close the same way, by propagation with a human repair
//! path behind it, and the relay half is driven end to end in
//! `relay_rotation.rs` through the shipped `CAP_RELAY_UPDATE` notice rather
//! than described here.
//!
//! WP5's third slice — §10 steps 3 and 4 — added two ids and moved none:
//!
//! * `MD-REVOKE-REFUSE-NEW-EVENTS` for §10.3, driven through the production
//!   inbound pair (`process_inbound_frame` → `core_deliver_inbound`) rather
//!   than by asking a policy function. The asymmetry is the whole vector:
//!   events the buried device authored while it was vouched for are still
//!   there afterwards, and events it signs after the burial land nowhere.
//!   History is never rewritten by a later revocation — a rule that deleted it
//!   would let a thief erase a conversation by getting himself revoked.
//! * `MD-REVOKE-CONTACT-NOTIFIED` for §10.4, driven through
//!   `apply_contact_roster`, which is the single funnel both DL-3 gossip and a
//!   sibling's self-sync reach. Its shape is decided as much by §2 goal 1 as by
//!   §10: a revocation raises a fact, an ordinary link raises nothing, because
//!   a person's device count is nobody else's business and a warning on every
//!   link is how a real warning gets swiped away unread.
//!
//! Neither is a *surface*: WP6 owns the copy, and the reason codes here name
//! what changed without saying a word of it. And `MD-ROSTER-GOSSIP-TO-CONTACTS`
//! still did not move — §10.4 tells a contact what a roster they RECEIVED
//! means, which is a different problem from getting the roster to them, and
//! that carrier is still missing.
//!
//! **WP6's carrier slice built it, and moved both DL-3 vectors together.**
//! `KIND_ROSTER_GOSSIP` is an ordinary sealed 1:1 kind carrying
//! `core_encode_roster`'s document, authored one copy per contact by
//! `MessageStore::announce_own_roster` and applied inbound through the same
//! `apply_contact_roster` funnel every other roster path already used. Both
//! drivers now run two stores against each other over a real envelope frame:
//!
//! * `MD-ROSTER-PAIRWISE-GOSSIP` checks DL-3's two claims where they can
//!   actually fail — the sealed copy opens for its one recipient and for no
//!   third party, and the document does not appear anywhere in the frame a
//!   relay would store.
//! * `MD-ROSTER-GOSSIP-TO-CONTACTS` drives §9 step 5 and asserts the ACK-MD-2
//!   cost its own fixture names, on both sides of the moment it closes: before
//!   the gossip the sending contact plans ONE person-addressed row that the
//!   receiving fleet refuses to ack, and after it the same message plans one
//!   row per device, each in that device's namespace, each acked by exactly the
//!   device that consumed it.
//!
//! One thing these vectors do NOT yet pin, and it should be said here rather
//! than discovered: both drivers run inbound through core's own
//! `deliver_inbound_body`, but neither shell does. Android and iOS still carry
//! mirrored `handleIncomingRosterGossip` handlers with their own copy of DL-3's
//! "the roster must be about its sender" check, because their per-kind delivery
//! has not moved onto `core_deliver_inbound`. So a vector passing here proves
//! the rule holds in core, not that a phone applies it — the two mirrored
//! handlers are what has to keep agreeing until that migration lands, at which
//! point they and their duplicated check are deleted rather than maintained.
//!
//! So the churn window WP3 opened is closed by propagation rather than by
//! expiry — which is what §9 step 5 always said, and is now what the code does.
//!
//! **What "implemented and pinned" does NOT mean here.** The ack vectors below
//! run the production planner against a fleet of two devices — and the fleet
//! record itself still has no production writer. `set_own_device_fleet`'s only
//! callers are tests; §9's activation ceremony, which is what will write it in
//! the field, is WP3's. WP4 gave that record its first production *reader* —
//! `author_pairwise_message` stamps §5's stream from it — which changes nothing
//! about the dormancy: with no writer, every device in the field still reads an
//! empty fleet, still authors on the legacy stream, and still plans exactly
//! today's acks (`a_single_device_identity_plans_exactly_todays_acks`,
//! `MD-STREAM-AUTHOR-UNLINKED`). So ACK-MD-1, ACK-MD-2 and §5's authoring
//! stamp are implemented, pinned, and dormant: correct the day WP3 turns them
//! on, and unreachable until then. That is the intended state of the work
//! package, not a gap in it.

use cruisemesh_core::{
    compute_recipient_hint, core_derive_device_id, core_device_add_outcome,
    core_device_namespace_id, core_device_stream_id, core_encode_roster, core_encode_sync_history,
    core_open_sync_record, core_own_capabilities, core_plan_sync_backfill, core_relay_ack_ids,
    core_roster_validate, core_seal_sync_record, core_should_ack_inbound, core_sign_device_cert,
    core_sign_roster, core_sign_sync_record, core_sync_digest_gaps, decode_extended_message_body,
    device_fanout_msg_id, encode_envelope_frame, encode_message_body, encode_message_body_extended,
    encode_own_roster, generate_device_keypair, generate_identity, open_message, parse_frame,
    seal_message, CarriedEnvelope, Contact, ContactDeviceState, ContactSafetyReason,
    CoreDeliveryVerdict, CoreDiscoveryPolicyState, CoreInboundDisposition, CoreLinkActivationStage,
    CoreLinkGateReason, CoreLinkGatedAction, CoreRelayEnvelopeDisposition, DeviceAddOutcome,
    DeviceCert, DeviceKeypair, DeviceTombstone, ExtendedMessageBody, Frame, Identity, InboxKey,
    IncomingMessageInsertOutcome, MessageArrival, MessageBody, MessageStore,
    OutboundAuthorDecision, OwnDeviceFleet, RevocationAdoptionOutcome, Roster, RosterRejection,
    RosterUpdateOutcome, RosterUpdateReason, StoredMessage, SyncBackfillAction, SyncRecord,
    SyncRecordKind, CAP_MULTI_DEVICE, DEVICE_CERT_FLAG_ROSTER_SIGNING, DEVICE_HARD_CAP,
    DEVICE_ID_LEN, DEVICE_SOFT_CAP, KIND_TEXT, LEGACY_DEVICE_ID as CORE_LEGACY_DEVICE_ID,
    SYNC_OUTBOUND_DEDUP_WINDOW_MS,
};
use ed25519_dalek::SigningKey;

/// Assert with the vector id in the failure message, following
/// `protocol_contract.rs`'s contract assertion style.
macro_rules! contract_assert {
    ($id:expr, $cond:expr, $($detail:tt)+) => {
        assert!(
            $cond,
            "{} violated: {}",
            $id,
            format_args!($($detail)+),
        )
    };
}

/// §5: envelopes with no sealed-body device field map to this reserved
/// all-zero stream. The sentinel is written out here rather than imported so
/// that a core constant which quietly picked a different value would have to
/// be reconciled by hand; MD-STREAM-LEGACY-ID's driver asserts the two agree.
const LEGACY_DEVICE_ID: [u8; 16] = [0u8; 16];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RosterVersion {
    recovery_epoch: u64,
    seq: u64,
}

/// DL-5 fixture for a roster document. It has fields for device keys and
/// revocation tombstones and *structurally* has nowhere to put an endpoint --
/// that is the point of the vector. WP1's real `Roster` type must keep that
/// property: endpoints must be impossible to represent, not merely absent by
/// convention, so the endpoint-privacy invariant cannot regress by an
/// innocent-looking struct field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RosterDocument {
    device_keys: &'static [&'static [u8]],
    tombstones: &'static [&'static [u8]],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    RosterUpdate {
        stored: RosterVersion,
        incoming: RosterVersion,
    },
    /// DL-1's second half: version ordering alone never accepts a roster; the
    /// signature chain must also verify back to the person root.
    RosterSignatureChain {
        stored: RosterVersion,
        incoming: RosterVersion,
        chain_verifies_to_person_root: bool,
    },
    RosterFork {
        version: RosterVersion,
        stored_content: &'static [u8],
        incoming_content: &'static [u8],
    },
    /// DL-2's follow-on: once a person is quarantined, later rosters for that
    /// person stay quarantined however well-formed they are.
    RosterUpdateAfterFork {
        quarantined_at: RosterVersion,
        incoming: RosterVersion,
        incoming_is_strictly_higher: bool,
        auto_resolves: bool,
    },
    /// DL-2's one refinement: a document signed by the **person root** at a
    /// strictly higher `recovery_epoch` is heard through the quarantine and
    /// clears it. Not an auto-resolution -- it is the person resolving the
    /// fork with the one credential a thief provably cannot hold (§14.2 keeps
    /// the root inside the encrypted backup), and refusing it would leave a
    /// fork an attacker caused permanently unrecoverable.
    RosterForkPiercedByRoot {
        quarantined_at: RosterVersion,
        incoming: RosterVersion,
        signed_by_person_root: bool,
    },
    RosterTombstonedDeviceReturns {
        version: RosterVersion,
        tombstoned_device_id: &'static [u8],
        returning_device_id: &'static [u8],
    },
    RosterRelinkFreshKey {
        version: RosterVersion,
        tombstoned_device_id: &'static [u8],
        replacement_device_id: &'static [u8],
    },
    RosterPairwiseGossipNoDirectory {
        version: RosterVersion,
        sealed_pairwise: bool,
        uses_directory: bool,
    },
    RosterKeysNeverEndpoints {
        version: RosterVersion,
        document: RosterDocument,
    },
    /// §14.2 supremacy applied to the one case with no baseline: the FIRST
    /// roster ever seen for a person. Nothing is stored, so the epoch rule has
    /// nothing to compare against and the document is adopted at whatever
    /// epoch it names.
    RosterFirstContactAnchor {
        adopted: RosterVersion,
        stored_baseline_exists: bool,
        signed_by_approving_device: bool,
    },
    /// §14.2's epoch rule: raising `recovery_epoch` above a *stored* one takes
    /// the person root's signature.
    RecoveryEpochRequiresRoot {
        version: RosterVersion,
        device_key_alone_can_mint_higher_epoch: bool,
    },
    /// §3 / §14.2 custody, which is what makes the epoch rule mean anything:
    /// the person root secret lives only inside the passphrase-encrypted
    /// `.cmbak`, never on a device.
    RecoveryRootCustody {
        root_secret_only_in_encrypted_backup: bool,
        device_keypair_can_carry_root_secret: bool,
    },
    LegacyEnvelopeWithoutDevice {
        legacy_device_id: [u8; 16],
    },
    TwoDevicesSamePersonSameLamport {
        first_device_id: &'static [u8],
        second_device_id: &'static [u8],
    },
    /// §5, WP4: the other half of the stream key, which WP1 could describe but
    /// not produce. A device that has been activated into a fleet must AUTHOR
    /// on its own stream — the stored row and the sealed body both — while an
    /// install that has never linked keeps emitting exactly today's bytes.
    AuthoringDeviceStamped {
        linked: bool,
    },
    /// §8 SYNC-2: one outbound, one author. Once a sibling's authored row has
    /// reached this device, composing the same text again must edit the draft
    /// rather than put a second copy on the stream.
    SiblingAlreadyAuthoredThisOutbound,
    OwnDeviceFanoutConsumed,
    /// Named `…Carried` until WP2. The rename is the finding: a sibling's row
    /// is not withheld because this device failed to open it -- §6's inbox key
    /// is person-scoped, so it opens -- but because it is not addressed to
    /// this device's namespace.
    ///
    /// The Carried scenario it was renamed *from* is kept as
    /// [`Scenario::SiblingDeviceFanoutCarried`] rather than replaced. The two
    /// plan no ack for two independent reasons, and a swap would have quietly
    /// traded one of them away: changing which situation a vector describes is
    /// a reason to ADD coverage, never to lose it.
    SiblingDeviceFanoutConsumed,
    /// The scenario WP0 originally wrote as `MD-ACK-SIBLING-FANOUT`: a
    /// sibling's row this device merely muled. It is withheld by the
    /// disposition rule alone -- `Carried` is never ackable -- which is a
    /// different guarantee from the namespace refusal above, and the one that
    /// still protects the row on a device whose fleet record is empty.
    SiblingDeviceFanoutCarried,
    LegacyPersonAddressedConsumed,
    PersonSealedCarriedCopy,
    StaleRosterSealing {
        revoked_device_still_listed: bool,
        sealed_with_inbox_key_generation: u64,
        current_inbox_key_generation: u64,
        surviving_devices_still_receive: bool,
    },
    /// §10.3: a device is buried, and then keeps signing. Events it authored
    /// while it was still vouched for stay exactly where they are; events it
    /// signs afterwards are refused. The two counts are what the driver sends,
    /// not what it hopes for.
    RevokedDeviceKeepsSigning {
        events_before_revocation: u8,
        events_after_revocation: u8,
        stored_history_survives: bool,
    },
    /// §10.4: what a contact is told. Only the changes that mean something was
    /// taken away raise a fact — §2 goal 1 keeps a person's device COUNT
    /// invisible, so an ordinary link raises nothing.
    ContactNotifiedOfRevocation {
        device_added_raises_a_fact: bool,
        device_revoked_raises_a_fact: bool,
    },
    /// §8, WP4: a day in which mail reaches one device of a fleet over BLE
    /// only. The devices must converge afterwards by self-sync — no relay row
    /// gets a second reader to make it happen.
    BleOnlyDayConverges {
        reached_over_ble: u8,
        fleet_size: u8,
        converges_by_self_sync: bool,
    },
    CapabilityReservation,
    /// §9 step 5: a device is added to a person's roster, and the person's
    /// contacts have to be told. `roster_reaches_contacts` was the target
    /// rather than the state of the world until WP6's `KIND_ROSTER_GOSSIP`
    /// carried a roster document to a contact; the driver now asserts it.
    RosterGossipToContacts {
        fleet_size_after_link: u8,
        roster_reaches_contacts: bool,
        /// What an un-updated contact keeps uploading, and what ACK-MD-2
        /// forbids the fleet from deleting -- asserted on the sending side
        /// before the gossip lands, since that is where the cost is paid and
        /// where its disappearance is visible.
        person_addressed_rows_churn_until_expiry: bool,
    },
    AddDevice {
        resulting_device_count: u8,
    },
    /// §10 step 5, WP5 follow-up: the device a roster BURIES meets its fleet
    /// again and is handed that roster on the link.
    ///
    /// The field session that produced this vector is worth restating, because
    /// the gap it found was a spec gap rather than a slow path: a removed phone
    /// with no contacts and no Shore Pass sat four minutes on the same Wi-Fi as
    /// its approver, still holding the pre-revocation roster and still saying
    /// "Mesh on". Every carrier v1 had was addressed to a set that phone was
    /// not in, so no amount of waiting would ever have converged it.
    RevokedDeviceMeetsTheFleet {
        /// Whether the document on the link really is this person's, signed
        /// back to their root. `false` is the forgery the frame must refuse:
        /// the notice is authenticated by the DOCUMENT, never by the link that
        /// carried it.
        notice_is_person_signed: bool,
    },
}

/// The vocabulary this file's targets and driver results are written in.
///
/// Variants no longer named by any vector are kept, not deleted. An outcome
/// leaves this list only when the rule it describes is *abandoned*, and none of
/// these were — a work package changed what the core does, which is a different
/// thing. Keeping them means a rollback, or a regression that lands back on the
/// old behaviour, stays expressible in the ledger instead of needing this enum
/// re-invented to describe it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum Outcome {
    Accepted,
    Ignored,
    ForkQuarantined,
    /// Retired by WP1. Before the message stream key gained its device
    /// dimension, a sibling device authoring at a lamport the person already
    /// held collided on `(chat_id, sender_user_id, lamport)` and was
    /// quarantined as a fork -- which is exactly what
    /// `MD-STREAM-SIBLING-LAMPORT`'s driver used to report.
    Quarantined,
    TombstonePermanent,
    FreshKeyAccepted,
    PairwiseGossipNoDirectory,
    KeysOnlyNoEndpoints,
    /// §3 / §14.2 custody: the root secret is only ever in the encrypted
    /// backup. Data-only -- `MD-RECOVERY-ROOT-CUSTODY` owns it and WP3 owns
    /// the mechanism.
    RecoveryRequiresEncryptedBackup,
    /// §14.2's enforced half: only a root signature raises the epoch above a
    /// stored one.
    EpochRequiresRootSignature,
    /// §14.2 supremacy at first contact, which nothing yet checks: an adoption
    /// with no baseline should still be anchored to the person root's
    /// authority over the epoch. `MD-ROSTER-FIRST-CONTACT-ANCHOR`'s target.
    FirstAdoptionAnchoredToRoot,
    /// Retired by WP1. Before `core_own_capabilities()` advertised
    /// `CAP_MULTI_DEVICE`, WPT had reserved the bit without announcing it, and
    /// `MD-CAPABILITY-RESERVED`'s driver reported this.
    ReservedNotAdvertised,
    LegacyDeviceStream,
    SeparateStreams,
    /// §5, WP4: an activated device's own outbound lands on its own stream, in
    /// the store and inside the seal.
    AuthoredOnOwnDeviceStream,
    /// §5, §12: an install that has never linked authors byte-identically to
    /// every build in the field. The permanent state of a legacy peer, and the
    /// state of every device today.
    AuthoredOnLegacyStream,
    /// §8 SYNC-2: the second device declines to re-author and the person's
    /// message stays one message.
    DeferredToSiblingAuthor,
    Acknowledged,
    NotAcknowledgedBecauseCarried,
    NotAcknowledgedByNamespaceRefusal,
    /// §8's target for a BLE-only day: the fleet converges through self-sync,
    /// not through a second device reading a row that has one true consumer.
    SiblingsConvergeBySelfSync,
    /// §9 step 5's target: a person's contacts learn the new roster, so they
    /// stop addressing that person as one device and the person-addressed rows
    /// ACK-MD-2 forbids the fleet from acking stop being uploaded at all.
    ContactsLearnTheRoster,
    PersonDigestProofOnly,
    DigestProofOnly,
    SurvivingDevicesDeliver,
    /// §10.3, WP5: an event newly signed by a buried device is refused, and
    /// the stream that device wrote seals at its last pre-revocation point —
    /// nothing already stored is rewritten.
    RevokedSignatureRefused,
    /// §10.4, WP5: the contact holds a changed-safety-state fact naming the
    /// revocation, in core reason codes. What a family member reads is WP6's.
    ContactSafetyStateRaised,
    Advertised,
    /// §10 step 5: the buried device stops. It stores the roster that removed
    /// it, clears the fleet projection routing and acks read, and shuts the
    /// §9.4 gate on advertising, authoring and acking alike.
    RevokedDeviceEjectsItself,
    DeviceAdded,
    DeviceAddedWithWarning,
    DeviceAddRefused,
}

struct Vector {
    id: &'static str,
    scenario: Scenario,
    target_outcome: Outcome,
    implemented: bool,
}

const VECTORS: &[Vector] = &[
    // DL-1: (2, 0) strictly supersedes (1, 1000), even though its seq resets.
    Vector {
        id: "MD-ROSTER-GREATER",
        scenario: Scenario::RosterUpdate {
            stored: RosterVersion {
                recovery_epoch: 1,
                seq: 1000,
            },
            incoming: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
        },
        target_outcome: Outcome::Accepted,
        implemented: true,
    },
    // DL-1: (1, 1001) must not roll back the already stored (2, 0).
    Vector {
        id: "MD-ROSTER-LOWER",
        scenario: Scenario::RosterUpdate {
            stored: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            incoming: RosterVersion {
                recovery_epoch: 1,
                seq: 1001,
            },
        },
        target_outcome: Outcome::Ignored,
        implemented: true,
    },
    // DL-1: equal (recovery_epoch, seq) is idempotent gossip, not an update.
    Vector {
        id: "MD-ROSTER-EQUAL",
        scenario: Scenario::RosterUpdate {
            stored: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            incoming: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
        },
        target_outcome: Outcome::Ignored,
        implemented: true,
    },
    // DL-1 discriminator: the ordinary case. Within one recovery epoch, a
    // strictly higher seq is the normal add/revoke and is accepted. Without
    // this vector an implementation that only ever compared `recovery_epoch`
    // would pass every other DL-1 vector.
    Vector {
        id: "MD-ROSTER-SAME-EPOCH-ADVANCE",
        scenario: Scenario::RosterUpdate {
            stored: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            incoming: RosterVersion {
                recovery_epoch: 2,
                seq: 1,
            },
        },
        target_outcome: Outcome::Accepted,
        implemented: true,
    },
    // DL-1 discriminator: within one recovery epoch a lower seq is a replayed
    // or stale roster and is ignored -- the same-epoch twin of MD-ROSTER-LOWER.
    Vector {
        id: "MD-ROSTER-SAME-EPOCH-ROLLBACK",
        scenario: Scenario::RosterUpdate {
            stored: RosterVersion {
                recovery_epoch: 2,
                seq: 5,
            },
            incoming: RosterVersion {
                recovery_epoch: 2,
                seq: 3,
            },
        },
        target_outcome: Outcome::Ignored,
        implemented: true,
    },
    // DL-1 discriminator: version ordering is necessary, never sufficient. A
    // strictly higher (3, 0) whose signature chain does not verify back to the
    // person root is ignored -- otherwise anyone could mint a roster.
    Vector {
        id: "MD-ROSTER-CHAIN-BROKEN",
        scenario: Scenario::RosterSignatureChain {
            stored: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            incoming: RosterVersion {
                recovery_epoch: 3,
                seq: 0,
            },
            chain_verifies_to_person_root: false,
        },
        target_outcome: Outcome::Ignored,
        implemented: true,
    },
    // DL-2: equal (recovery_epoch, seq) plus different content is a fork.
    Vector {
        id: "MD-ROSTER-FORK",
        scenario: Scenario::RosterFork {
            version: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            stored_content: b"approved-device-a",
            incoming_content: b"approved-device-b",
        },
        target_outcome: Outcome::ForkQuarantined,
        implemented: true,
    },
    // DL-2 follow-on: quarantine is sticky. A legitimately higher (2, 1)
    // roster arriving after the fork does NOT lift the quarantine -- "never
    // auto-resolve a fork" means a later good version is not a resolution.
    Vector {
        id: "MD-ROSTER-FORK-QUARANTINE-PERSISTS",
        scenario: Scenario::RosterUpdateAfterFork {
            quarantined_at: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            incoming: RosterVersion {
                recovery_epoch: 2,
                seq: 1,
            },
            incoming_is_strictly_higher: true,
            auto_resolves: false,
        },
        target_outcome: Outcome::ForkQuarantined,
        implemented: true,
    },
    // DL-2's refinement: the person root, at a strictly higher recovery epoch,
    // is the one document a quarantine yields to. `-QUARANTINE-PERSISTS` above
    // keeps its full meaning for every non-root document; this is the narrow
    // hole deliberately left in it, and it is narrow because the root is the
    // one signature §14.2 keeps off every device a thief could steal.
    Vector {
        id: "MD-ROSTER-FORK-ROOT-PIERCE",
        scenario: Scenario::RosterForkPiercedByRoot {
            quarantined_at: RosterVersion {
                recovery_epoch: 2,
                seq: 0,
            },
            incoming: RosterVersion {
                recovery_epoch: 3,
                seq: 0,
            },
            signed_by_person_root: true,
        },
        target_outcome: Outcome::Accepted,
        implemented: true,
    },
    // DL-4: the tombstoned device_id itself can never return.
    Vector {
        id: "MD-ROSTER-TOMBSTONE",
        scenario: Scenario::RosterTombstonedDeviceReturns {
            version: RosterVersion {
                recovery_epoch: 2,
                seq: 1,
            },
            tombstoned_device_id: b"revoked-phone-key",
            returning_device_id: b"revoked-phone-key",
        },
        target_outcome: Outcome::TombstonePermanent,
        implemented: true,
    },
    // DL-4: re-linking that hardware mints a fresh key, which may be accepted.
    Vector {
        id: "MD-ROSTER-RELINK-FRESH-KEY",
        scenario: Scenario::RosterRelinkFreshKey {
            version: RosterVersion {
                recovery_epoch: 2,
                seq: 2,
            },
            tombstoned_device_id: b"revoked-phone-key",
            replacement_device_id: b"relinked-phone-fresh-key",
        },
        target_outcome: Outcome::FreshKeyAccepted,
        implemented: true,
    },
    // DL-3: a roster is sealed pairwise gossip; no directory sees plaintext.
    // Driven since WP6's carrier: the document rides a real
    // `KIND_ROSTER_GOSSIP` envelope, so "pairwise" is checked by opening it
    // (and failing to, as a third party) and "no plaintext" by searching the
    // frame a relay would store for the document and finding none of it.
    Vector {
        id: "MD-ROSTER-PAIRWISE-GOSSIP",
        scenario: Scenario::RosterPairwiseGossipNoDirectory {
            version: RosterVersion {
                recovery_epoch: 2,
                seq: 3,
            },
            sealed_pairwise: true,
            uses_directory: false,
        },
        target_outcome: Outcome::PairwiseGossipNoDirectory,
        implemented: true,
    },
    // DL-5: roster documents carry device keys and tombstones, never
    // third-party endpoints. Be exact about what enforces that, because the
    // fixture below is not it: every byte field of the real `Roster` is a
    // `Vec<u8>`, so the *type* forbids nothing, and the single gate is
    // `core_roster_validate`'s fixed-width check. What that gate actually buys
    // is that no field can hold a free-form address -- not that a 16- or
    // 32-byte value cannot be chosen adversarially, which it can, and which the
    // driver below now includes.
    Vector {
        id: "MD-ROSTER-KEYS-NOT-ENDPOINTS",
        scenario: Scenario::RosterKeysNeverEndpoints {
            version: RosterVersion {
                recovery_epoch: 2,
                seq: 4,
            },
            document: RosterDocument {
                device_keys: &[b"alice-phone-device-key", b"alice-tablet-device-key"],
                tombstones: &[b"revoked-phone-key"],
            },
        },
        target_outcome: Outcome::KeysOnlyNoEndpoints,
        implemented: true,
    },
    // §14.2 supremacy where there is no baseline: the first roster ever seen
    // for a person is adopted at whatever epoch it names, because nothing is
    // stored to compare it against. The target is that a first adoption is
    // still anchored to the person root's authority over the epoch -- which
    // today it is not, and cannot be without a second source of truth about
    // where that person's epoch had got to.
    Vector {
        id: "MD-ROSTER-FIRST-CONTACT-ANCHOR",
        scenario: Scenario::RosterFirstContactAnchor {
            adopted: RosterVersion {
                recovery_epoch: 9,
                seq: 4,
            },
            stored_baseline_exists: false,
            signed_by_approving_device: true,
        },
        target_outcome: Outcome::FirstAdoptionAnchoredToRoot,
        implemented: false,
    },
    // §14.2, the enforced half: an approving device cannot raise the epoch
    // above a stored one; only a root signature can.
    Vector {
        id: "MD-RECOVERY-EPOCH-REQUIRES-ROOT",
        scenario: Scenario::RecoveryEpochRequiresRoot {
            version: RosterVersion {
                recovery_epoch: 3,
                seq: 0,
            },
            device_key_alone_can_mint_higher_epoch: false,
        },
        target_outcome: Outcome::EpochRequiresRootSignature,
        implemented: true,
    },
    // §3 / §14.2, the custody half: the epoch rule is only worth anything
    // because the root secret is not on any device. Data-only -- WP3 owns
    // where that secret lives and how the encrypted backup carries it.
    Vector {
        id: "MD-RECOVERY-ROOT-CUSTODY",
        scenario: Scenario::RecoveryRootCustody {
            root_secret_only_in_encrypted_backup: true,
            device_keypair_can_carry_root_secret: false,
        },
        target_outcome: Outcome::RecoveryRequiresEncryptedBackup,
        implemented: false,
    },
    // §5: absent sealed-body device fields become the all-zero legacy stream.
    Vector {
        id: "MD-STREAM-LEGACY-ID",
        scenario: Scenario::LegacyEnvelopeWithoutDevice {
            legacy_device_id: LEGACY_DEVICE_ID,
        },
        target_outcome: Outcome::LegacyDeviceStream,
        implemented: true,
    },
    // §5: sibling author streams must remain independent even at one lamport.
    Vector {
        id: "MD-STREAM-SIBLING-LAMPORT",
        scenario: Scenario::TwoDevicesSamePersonSameLamport {
            first_device_id: b"alice-phone-device-key",
            second_device_id: b"alice-tablet-device-key",
        },
        target_outcome: Outcome::SeparateStreams,
        implemented: true,
    },
    // §5, WP4: authoring puts this device's real id on the row and in the seal.
    // WP1 built the stream key and the sealed-body field and deliberately left
    // every authoring call site on the legacy stream, because a device may not
    // claim an id before a roster names it (§9.4). WP4 is where the id exists,
    // so this is where the authoring paths start using it.
    Vector {
        id: "MD-STREAM-AUTHOR-OWN-DEVICE",
        scenario: Scenario::AuthoringDeviceStamped { linked: true },
        target_outcome: Outcome::AuthoredOnOwnDeviceStream,
        implemented: true,
    },
    // §5, §12: and the same call on an install that has never linked emits what
    // it emits today, to the byte. This is the vector that would catch WP4
    // accidentally making every device in the field start advertising a device
    // id it does not have.
    Vector {
        id: "MD-STREAM-AUTHOR-UNLINKED",
        scenario: Scenario::AuthoringDeviceStamped { linked: false },
        target_outcome: Outcome::AuthoredOnLegacyStream,
        implemented: true,
    },
    // ACK-MD-1: a consumed row in this device's fan-out namespace acks.
    Vector {
        id: "MD-ACK-OWN-FANOUT",
        scenario: Scenario::OwnDeviceFanoutConsumed,
        target_outcome: Outcome::Acknowledged,
        implemented: true,
    },
    // ACK-MD-1: a sibling's row opens here (§6 seals to the person) and is
    // refused anyway, by namespace. WP2 made the refusal real; before it, the
    // row happened not to ack because the fixture called it Carried.
    Vector {
        id: "MD-ACK-SIBLING-FANOUT",
        scenario: Scenario::SiblingDeviceFanoutConsumed,
        target_outcome: Outcome::NotAcknowledgedByNamespaceRefusal,
        implemented: true,
    },
    // ACK-MD-1, the OTHER reason a sibling's row survives: this device merely
    // muled it. Kept beside the Consumed vector above rather than replaced by
    // it -- disposition and namespace are two independent guarantees, and the
    // disposition one is what still protects the row on a device whose fleet
    // record is empty.
    Vector {
        id: "MD-ACK-SIBLING-FANOUT-CARRIED",
        scenario: Scenario::SiblingDeviceFanoutCarried,
        target_outcome: Outcome::NotAcknowledgedBecauseCarried,
        implemented: true,
    },
    // ACK-MD-2: a legacy person-addressed row opens here and is consumed, and
    // a multi-device fleet must leave it for its sibling devices anyway.
    Vector {
        id: "MD-ACK-LEGACY-PERSON-ROW",
        scenario: Scenario::LegacyPersonAddressedConsumed,
        target_outcome: Outcome::NotAcknowledgedByNamespaceRefusal,
        implemented: true,
    },
    // ACK-MD-3: this is a person-sealed carried copy. A single device cannot
    // confirm for the person; receipt proof must be person-safe in v1.
    Vector {
        id: "MD-ACK-CARRIED-DIGEST-PROOF",
        scenario: Scenario::PersonSealedCarriedCopy,
        target_outcome: Outcome::PersonDigestProofOnly,
        implemented: true,
    },
    // §8, WP4: a BLE-only day reaches one device of a fleet, and the fleet
    // converges afterwards by self-sync. Driven for real since WP4's SYNC-1
    // anti-entropy landed -- the driver below runs the shipped digest, gap and
    // backfill surface, and the sim's
    // `a_ble_only_day_converges_through_sibling_self_sync` runs the same
    // convergence over real BLE frames. The two moved together, as WP2 wrote
    // them to have to.
    Vector {
        id: "MD-SYNC-BLE-DAY-CONVERGE",
        scenario: Scenario::BleOnlyDayConverges {
            reached_over_ble: 1,
            fleet_size: 2,
            converges_by_self_sync: true,
        },
        target_outcome: Outcome::SiblingsConvergeBySelfSync,
        implemented: true,
    },
    // §8 SYNC-2: two distinct authored copies of one text are a product bug.
    // Driven through the shipped claim on a store that has taken the sibling's
    // row; `core/tests/sync_convergence.rs` runs the same rule against
    // generated schedules and pins the window it cannot close.
    Vector {
        id: "MD-SYNC-OUTBOUND-DEDUP",
        scenario: Scenario::SiblingAlreadyAuthoredThisOutbound,
        target_outcome: Outcome::DeferredToSiblingAuthor,
        implemented: true,
    },
    // §9 step 5: the person's contacts learn the roster. WP3 built the ceremony
    // that makes a fleet larger than one device and WP5 produced the document
    // and the recipient list; WP6 built the envelope kind that carries one to a
    // contact, which is what turned this from a plan into a delivery. The
    // driver runs both ends -- the shipped announcement authors, a second store
    // applies through the production inbound pair -- and asserts the churn cost
    // the third field names both before it closes and after.
    Vector {
        id: "MD-ROSTER-GOSSIP-TO-CONTACTS",
        scenario: Scenario::RosterGossipToContacts {
            fleet_size_after_link: 2,
            roster_reaches_contacts: true,
            person_addressed_rows_churn_until_expiry: true,
        },
        target_outcome: Outcome::ContactsLearnTheRoster,
        implemented: true,
    },
    // §6: stale inbox-key sealing has bounded exposure, not a delivery brick.
    // A months-offline contact seals to the roster it knows -- which still
    // lists the revoked device and still uses the pre-revocation inbox key
    // generation -- and the surviving devices must still receive that mail.
    //
    // Driven since WP5: the rotation the vector's two generations describe is
    // performed by the shipped `core_revoke_devices_roster`, and the driver
    // asserts both ends of the window -- the stale roster still delivers to the
    // survivors, and the revocation supersedes it the moment it arrives.
    Vector {
        id: "MD-SEAL-STALE-ROSTER",
        scenario: Scenario::StaleRosterSealing {
            revoked_device_still_listed: true,
            sealed_with_inbox_key_generation: 4,
            current_inbox_key_generation: 5,
            surviving_devices_still_receive: true,
        },
        target_outcome: Outcome::SurvivingDevicesDeliver,
        implemented: true,
    },
    // §10.3: a buried device keeps its key and keeps signing. Everything it
    // wrote while it was trusted stays; nothing it signs afterwards lands. The
    // asymmetry is the rule — deleting stored history on a later revocation
    // would let a thief erase a conversation by getting himself revoked.
    Vector {
        id: "MD-REVOKE-REFUSE-NEW-EVENTS",
        scenario: Scenario::RevokedDeviceKeepsSigning {
            events_before_revocation: 2,
            events_after_revocation: 1,
            stored_history_survives: true,
        },
        target_outcome: Outcome::RevokedSignatureRefused,
        implemented: true,
    },
    // §10.4: the contact's changed-safety-state treatment, as core facts with
    // reason codes. Its shape is decided by §2 goal 1 as much as by §10: a
    // revocation is news, an ordinary link is nobody else's business.
    Vector {
        id: "MD-REVOKE-CONTACT-NOTIFIED",
        scenario: Scenario::ContactNotifiedOfRevocation {
            device_added_raises_a_fact: false,
            device_revoked_raises_a_fact: true,
        },
        target_outcome: Outcome::ContactSafetyStateRaised,
        implemented: true,
    },
    // §12: WP1 advertises the reserved capability through HELLO2.
    Vector {
        id: "MD-CAPABILITY-RESERVED",
        scenario: Scenario::CapabilityReservation,
        target_outcome: Outcome::Advertised,
        implemented: true,
    },
    // §14.3: soft 8 / hard 16, with the boundary pinned by the 2026-08-16
    // decision: counts are the roster size AFTER this add; up to 8 is silent,
    // the 9th warns, 16 is the last allowed, the 17th is refused.
    Vector {
        id: "MD-DEVICE-CAP-7",
        scenario: Scenario::AddDevice {
            resulting_device_count: 7,
        },
        target_outcome: Outcome::DeviceAdded,
        implemented: true,
    },
    Vector {
        id: "MD-DEVICE-CAP-8",
        scenario: Scenario::AddDevice {
            resulting_device_count: 8,
        },
        target_outcome: Outcome::DeviceAdded,
        implemented: true,
    },
    Vector {
        id: "MD-DEVICE-CAP-9",
        scenario: Scenario::AddDevice {
            resulting_device_count: 9,
        },
        target_outcome: Outcome::DeviceAddedWithWarning,
        implemented: true,
    },
    Vector {
        id: "MD-DEVICE-CAP-16",
        scenario: Scenario::AddDevice {
            resulting_device_count: 16,
        },
        target_outcome: Outcome::DeviceAddedWithWarning,
        implemented: true,
    },
    Vector {
        id: "MD-DEVICE-CAP-17",
        scenario: Scenario::AddDevice {
            resulting_device_count: 17,
        },
        target_outcome: Outcome::DeviceAddRefused,
        implemented: true,
    },
    // §10 step 5. Appended at the end, per this file's rules: the pinned tables
    // are append-only and nothing above moved.
    Vector {
        id: "MD-REVOKE-SELF-EJECTS",
        scenario: Scenario::RevokedDeviceMeetsTheFleet {
            notice_is_person_signed: true,
        },
        target_outcome: Outcome::RevokedDeviceEjectsItself,
        implemented: true,
    },
    // The discriminator without which the vector above would pass for an
    // implementation that ejected on any roster-shaped bytes at all.
    Vector {
        id: "MD-REVOKE-NOTICE-UNSIGNED-IGNORED",
        scenario: Scenario::RevokedDeviceMeetsTheFleet {
            notice_is_person_signed: false,
        },
        target_outcome: Outcome::Ignored,
        implemented: true,
    },
];

/// The one pinned table of every vector's target outcome. Editing any
/// vector's `target_outcome` -- including flipping a data-only rule -- fails
/// here, so a future work package cannot quietly re-decide an accepted rule
/// while its own tests stay green.
const PINNED_TARGETS: &[(&str, Outcome)] = &[
    ("MD-ROSTER-GREATER", Outcome::Accepted),
    ("MD-ROSTER-LOWER", Outcome::Ignored),
    ("MD-ROSTER-EQUAL", Outcome::Ignored),
    ("MD-ROSTER-SAME-EPOCH-ADVANCE", Outcome::Accepted),
    ("MD-ROSTER-SAME-EPOCH-ROLLBACK", Outcome::Ignored),
    ("MD-ROSTER-CHAIN-BROKEN", Outcome::Ignored),
    ("MD-ROSTER-FORK", Outcome::ForkQuarantined),
    (
        "MD-ROSTER-FORK-QUARANTINE-PERSISTS",
        Outcome::ForkQuarantined,
    ),
    ("MD-ROSTER-FORK-ROOT-PIERCE", Outcome::Accepted),
    ("MD-ROSTER-TOMBSTONE", Outcome::TombstonePermanent),
    ("MD-ROSTER-RELINK-FRESH-KEY", Outcome::FreshKeyAccepted),
    (
        "MD-ROSTER-PAIRWISE-GOSSIP",
        Outcome::PairwiseGossipNoDirectory,
    ),
    ("MD-ROSTER-KEYS-NOT-ENDPOINTS", Outcome::KeysOnlyNoEndpoints),
    (
        "MD-ROSTER-FIRST-CONTACT-ANCHOR",
        Outcome::FirstAdoptionAnchoredToRoot,
    ),
    (
        "MD-RECOVERY-EPOCH-REQUIRES-ROOT",
        Outcome::EpochRequiresRootSignature,
    ),
    (
        "MD-RECOVERY-ROOT-CUSTODY",
        Outcome::RecoveryRequiresEncryptedBackup,
    ),
    ("MD-STREAM-LEGACY-ID", Outcome::LegacyDeviceStream),
    ("MD-STREAM-SIBLING-LAMPORT", Outcome::SeparateStreams),
    (
        "MD-STREAM-AUTHOR-OWN-DEVICE",
        Outcome::AuthoredOnOwnDeviceStream,
    ),
    ("MD-STREAM-AUTHOR-UNLINKED", Outcome::AuthoredOnLegacyStream),
    ("MD-ACK-OWN-FANOUT", Outcome::Acknowledged),
    (
        "MD-ACK-SIBLING-FANOUT",
        Outcome::NotAcknowledgedByNamespaceRefusal,
    ),
    (
        "MD-ACK-SIBLING-FANOUT-CARRIED",
        Outcome::NotAcknowledgedBecauseCarried,
    ),
    (
        "MD-ACK-LEGACY-PERSON-ROW",
        Outcome::NotAcknowledgedByNamespaceRefusal,
    ),
    (
        "MD-ACK-CARRIED-DIGEST-PROOF",
        Outcome::PersonDigestProofOnly,
    ),
    (
        "MD-SYNC-BLE-DAY-CONVERGE",
        Outcome::SiblingsConvergeBySelfSync,
    ),
    ("MD-SYNC-OUTBOUND-DEDUP", Outcome::DeferredToSiblingAuthor),
    (
        "MD-ROSTER-GOSSIP-TO-CONTACTS",
        Outcome::ContactsLearnTheRoster,
    ),
    ("MD-SEAL-STALE-ROSTER", Outcome::SurvivingDevicesDeliver),
    (
        "MD-REVOKE-REFUSE-NEW-EVENTS",
        Outcome::RevokedSignatureRefused,
    ),
    (
        "MD-REVOKE-CONTACT-NOTIFIED",
        Outcome::ContactSafetyStateRaised,
    ),
    ("MD-CAPABILITY-RESERVED", Outcome::Advertised),
    ("MD-DEVICE-CAP-7", Outcome::DeviceAdded),
    ("MD-DEVICE-CAP-8", Outcome::DeviceAdded),
    ("MD-DEVICE-CAP-9", Outcome::DeviceAddedWithWarning),
    ("MD-DEVICE-CAP-16", Outcome::DeviceAddedWithWarning),
    ("MD-DEVICE-CAP-17", Outcome::DeviceAddRefused),
    ("MD-REVOKE-SELF-EJECTS", Outcome::RevokedDeviceEjectsItself),
    ("MD-REVOKE-NOTICE-UNSIGNED-IGNORED", Outcome::Ignored),
];

/// The pinned map of executed vector id -> what today's core actually does.
/// This is the WP0 photograph of current behaviour: every change to a driver
/// result, in either direction, must be a deliberate edit here.
///
/// WP1's edits, and what made each of them true: every roster id joined the
/// table because `core_roster_accept` and the contact-roster tables now exist
/// and the drivers run them; `MD-STREAM-LEGACY-ID` and
/// `MD-STREAM-SIBLING-LAMPORT` because the message stream key gained its
/// device dimension and the sealed body gained the field that fills it (the
/// sibling result moved from `Quarantined`, which was the shared-stream
/// collision, to `SeparateStreams`); `MD-CAPABILITY-RESERVED` because
/// `core_own_capabilities()` now advertises the bit.
///
/// WP2 moved two of the four ack ids, and each is a real behaviour change in
/// `core_relay_ack_ids_with_consumed` rather than a fixture edit:
/// `MD-ACK-SIBLING-FANOUT` from `NotAcknowledgedBecauseCarried` because the
/// planner now refuses a *consumed* sibling row by namespace, and
/// `MD-ACK-LEGACY-PERSON-ROW` from `Acknowledged` because a fleet holding more
/// than one device no longer deletes the person's one shared row (ACK-MD-2).
/// `MD-ACK-OWN-FANOUT` is untouched, which is the point of it.
/// `MD-ACK-CARRIED-DIGEST-PROOF` is untouched too: WP2 changed nothing about
/// carried copies, so its divergence stands. `MD-ACK-SIBLING-FANOUT-CARRIED`
/// is new and reports what `MD-ACK-SIBLING-FANOUT` used to: the scenario did
/// not stop being true when the other vector was re-pointed at a consumed row,
/// so it kept its coverage under its own id.
///
/// WP4 moved `MD-SYNC-BLE-DAY-CONVERGE` into this table — it had no driver at
/// all before §8's records existed — and added
/// `MD-STREAM-AUTHOR-OWN-DEVICE`, `MD-STREAM-AUTHOR-UNLINKED` and
/// `MD-SYNC-OUTBOUND-DEDUP` as new ids rather than re-pointing any existing
/// one. Nothing already in the table moved, and that is the claim worth
/// reading: WP4 changed what an *activated* device authors, and a device in the
/// field is not activated, so every previously pinned result is still the
/// result. `MD-STREAM-AUTHOR-UNLINKED` exists precisely to make that stay true
/// by accident-proof rather than by inspection.
///
/// WP5 added `MD-SEAL-STALE-ROSTER` (§6's window, once rotation existed) and
/// then `MD-REVOKE-REFUSE-NEW-EVENTS` / `MD-REVOKE-CONTACT-NOTIFIED` (§10
/// steps 3 and 4). Again nothing already here moved, and again that is the
/// claim rather than the coincidence: §10.3 refuses what a roster BURIED, and
/// no roster in the field has buried anything, so every peer on today's build
/// — which stamps no device id at all — delivers exactly as it did before.
/// `MD-STREAM-AUTHOR-UNLINKED` and `MD-STREAM-LEGACY-ID` are what keep that
/// honest rather than hopeful.
///
/// WP5's follow-up appended `MD-REVOKE-SELF-EJECTS` and
/// `MD-REVOKE-NOTICE-UNSIGNED-IGNORED` for §10 step 5, and again nothing
/// already here moved. The claim that goes with them: step 5 only ever fires on
/// a device a signed roster has BURIED, and no roster in the field has buried
/// anything, so every peer on today's build behaves exactly as it did — a
/// device that never linked has no own roster at all and refuses the notice
/// before any rule is consulted.
const PINNED_DRIVER_RESULTS: &[(&str, Outcome)] = &[
    ("MD-ROSTER-GREATER", Outcome::Accepted),
    ("MD-ROSTER-LOWER", Outcome::Ignored),
    ("MD-ROSTER-EQUAL", Outcome::Ignored),
    ("MD-ROSTER-SAME-EPOCH-ADVANCE", Outcome::Accepted),
    ("MD-ROSTER-SAME-EPOCH-ROLLBACK", Outcome::Ignored),
    ("MD-ROSTER-CHAIN-BROKEN", Outcome::Ignored),
    ("MD-ROSTER-FORK", Outcome::ForkQuarantined),
    (
        "MD-ROSTER-FORK-QUARANTINE-PERSISTS",
        Outcome::ForkQuarantined,
    ),
    ("MD-ROSTER-FORK-ROOT-PIERCE", Outcome::Accepted),
    ("MD-ROSTER-TOMBSTONE", Outcome::TombstonePermanent),
    ("MD-ROSTER-RELINK-FRESH-KEY", Outcome::FreshKeyAccepted),
    (
        "MD-ROSTER-PAIRWISE-GOSSIP",
        Outcome::PairwiseGossipNoDirectory,
    ),
    ("MD-ROSTER-KEYS-NOT-ENDPOINTS", Outcome::KeysOnlyNoEndpoints),
    (
        "MD-RECOVERY-EPOCH-REQUIRES-ROOT",
        Outcome::EpochRequiresRootSignature,
    ),
    ("MD-STREAM-LEGACY-ID", Outcome::LegacyDeviceStream),
    ("MD-STREAM-SIBLING-LAMPORT", Outcome::SeparateStreams),
    (
        "MD-STREAM-AUTHOR-OWN-DEVICE",
        Outcome::AuthoredOnOwnDeviceStream,
    ),
    ("MD-STREAM-AUTHOR-UNLINKED", Outcome::AuthoredOnLegacyStream),
    ("MD-ACK-OWN-FANOUT", Outcome::Acknowledged),
    (
        "MD-ACK-SIBLING-FANOUT",
        Outcome::NotAcknowledgedByNamespaceRefusal,
    ),
    (
        "MD-ACK-SIBLING-FANOUT-CARRIED",
        Outcome::NotAcknowledgedBecauseCarried,
    ),
    (
        "MD-ACK-LEGACY-PERSON-ROW",
        Outcome::NotAcknowledgedByNamespaceRefusal,
    ),
    ("MD-ACK-CARRIED-DIGEST-PROOF", Outcome::DigestProofOnly),
    (
        "MD-SYNC-BLE-DAY-CONVERGE",
        Outcome::SiblingsConvergeBySelfSync,
    ),
    ("MD-SYNC-OUTBOUND-DEDUP", Outcome::DeferredToSiblingAuthor),
    (
        "MD-ROSTER-GOSSIP-TO-CONTACTS",
        Outcome::ContactsLearnTheRoster,
    ),
    ("MD-SEAL-STALE-ROSTER", Outcome::SurvivingDevicesDeliver),
    (
        "MD-REVOKE-REFUSE-NEW-EVENTS",
        Outcome::RevokedSignatureRefused,
    ),
    (
        "MD-REVOKE-CONTACT-NOTIFIED",
        Outcome::ContactSafetyStateRaised,
    ),
    ("MD-CAPABILITY-RESERVED", Outcome::Advertised),
    ("MD-DEVICE-CAP-7", Outcome::DeviceAdded),
    ("MD-DEVICE-CAP-8", Outcome::DeviceAdded),
    ("MD-DEVICE-CAP-9", Outcome::DeviceAddedWithWarning),
    ("MD-DEVICE-CAP-16", Outcome::DeviceAddedWithWarning),
    ("MD-DEVICE-CAP-17", Outcome::DeviceAddRefused),
    ("MD-REVOKE-SELF-EJECTS", Outcome::RevokedDeviceEjectsItself),
    ("MD-REVOKE-NOTICE-UNSIGNED-IGNORED", Outcome::Ignored),
];

/// This person's wire identity (`person_id` in new code, `user_id` today).
const PERSON_ID: [u8; 16] = [0x5A; 16];
/// Device *labels*, resolved to real keypairs and real 16-byte device ids by
/// [`device`], exactly as the roster vectors resolve theirs.
const OWN_DEVICE_ID: &[u8] = b"alice-phone-device-key";
const SIBLING_DEVICE_ID: &[u8] = b"alice-tablet-device-key";
const NOW_MS: i64 = 1_700_000_000_000;

/// §7's per-device hint namespace.
///
/// WP2 replaced the test-only stand-in that stood here with the shipped
/// derivation: these vectors now address a row exactly the way a real
/// per-device fan-out does — `core_device_namespace_id` for the hint,
/// `device_fanout_msg_id` for the row id — so a change to either derivation is
/// a change to these vectors, and the ack rules below are being asked about
/// the bytes that will really be on the wire.
fn device_hint(device_label: &[u8]) -> Vec<u8> {
    compute_recipient_hint(
        core_device_namespace_id(PERSON_ID.to_vec(), device(device_label).device_id),
        NOW_MS,
    )
}

/// One relay row of a per-device fan-out, addressed to `device_label`.
fn fanout_item(
    relay_id: i64,
    disposition: CoreInboundDisposition,
    device_label: &[u8],
) -> CoreRelayEnvelopeDisposition {
    CoreRelayEnvelopeDisposition {
        relay_id,
        msg_id: device_fanout_msg_id(vec![relay_id as u8; 16], device(device_label).device_id),
        disposition,
        recipient_hint: device_hint(device_label),
    }
}

fn person_hint() -> Vec<u8> {
    compute_recipient_hint(PERSON_ID.to_vec(), NOW_MS)
}

// ---------------------------------------------------------------------------
// Roster fixtures (§3, §4)
// ---------------------------------------------------------------------------

/// The person root secret. Fixed, never generated: "the signature chain
/// verifies back to the person root" only means something if the root is one
/// specific key on every run, in the same spirit as `identity.rs`'s golden
/// vectors.
const ROOT_SK: [u8; 32] = [0x11; 32];
/// A key this person never vouched for, used to break a chain on purpose.
const STRANGER_SK: [u8; 32] = [0x99; 32];
/// The contact's X25519 key. Rosters never touch it; it is here because a
/// contact row has one.
const PERSON_AGREE_PK: [u8; 32] = [0x12; 32];

/// One of the vectors' symbolic device names, resolved to a real keypair.
struct FixtureDevice {
    sign_sk: [u8; 32],
    sign_pk: Vec<u8>,
    agree_pk: Vec<u8>,
    device_id: Vec<u8>,
}

fn sign_pk_of(sign_sk: &[u8; 32]) -> Vec<u8> {
    SigningKey::from_bytes(sign_sk)
        .verifying_key()
        .as_bytes()
        .to_vec()
}

/// Resolve a vector's device label to a fixed keypair. The mapping is a
/// literal table rather than a hash of the label so that the bytes behind
/// `b"revoked-phone-key"` are pinned, and so an unknown label is a loud
/// failure instead of a silently different device.
fn device(label: &[u8]) -> FixtureDevice {
    let seed = match label {
        b"alice-phone-device-key" => 0x21,
        b"alice-tablet-device-key" => 0x22,
        b"revoked-phone-key" => 0x23,
        b"relinked-phone-fresh-key" => 0x24,
        b"approved-device-a" => 0x25,
        b"approved-device-b" => 0x26,
        _ => unreachable!("unknown fixture device label"),
    };
    device_from_seed(seed)
}

/// The unnamed devices §14.3's cap vectors need: a person may hold seventeen
/// of them, and naming seventeen would say nothing that the count does not.
/// Seeds start above every labelled device's so a cap roster can never
/// accidentally contain one of the named fixtures.
fn device_from_seed(seed: u8) -> FixtureDevice {
    let sign_sk = [seed; 32];
    let sign_pk = sign_pk_of(&sign_sk);
    FixtureDevice {
        device_id: core_derive_device_id(sign_pk.clone()).expect("16-byte device id"),
        sign_sk,
        sign_pk,
        // Never used to seal anything here; distinct per device so two
        // certificates can never accidentally be the same document.
        agree_pk: vec![seed ^ 0x80; 32],
    }
}

fn person_root_sign_pk() -> Vec<u8> {
    sign_pk_of(&ROOT_SK)
}

/// §3: the person id *is* the deployed identity's `user_id`. Nothing new is
/// derived for it.
fn person_id() -> Vec<u8> {
    core_derive_device_id(person_root_sign_pk()).expect("16-byte person id")
}

/// The contact row a roster is verified against: `sign_pk` is the person root,
/// which is §3's whole no-re-friending claim.
fn roster_contact() -> Contact {
    Contact {
        user_id: person_id(),
        name: "Roster fixture".to_string(),
        sign_pk: person_root_sign_pk(),
        agree_pk: PERSON_AGREE_PK.to_vec(),
        relay_url: None,
        relay_token: None,
        nickname: None,
    }
}

/// A device certificate, signed by `signer_sk`. Every fixture certificate is
/// root-signed, which is the chain every roster here descends from; the
/// chain-broken vector is the one that passes a stranger.
fn cert(device: &FixtureDevice, flags: u32, signer_sk: &[u8; 32]) -> DeviceCert {
    core_sign_device_cert(
        DeviceCert {
            person_id: person_id(),
            device_sign_pk: device.sign_pk.clone(),
            device_agree_pk: device.agree_pk.clone(),
            added_epoch: 0,
            flags,
            signer_sign_pk: Vec::new(),
            signature: Vec::new(),
        },
        signer_sk.to_vec(),
    )
    .expect("device certificate signs")
}

/// A valid roster for the fixture person at `version`.
///
/// `devices[0]` is the approving device and the only certificate carrying the
/// roster-signing flag (§3's authority split). The signer follows the rule,
/// never the convenience: `seq == 0` is root-signed, because genesis and the
/// first roster of any recovery epoch must be (§3, §14.2); every other version
/// is signed by the approving device, which is what an ordinary add or revoke
/// looks like.
fn roster_at(
    version: RosterVersion,
    devices: &[&[u8]],
    tombstones: &[&[u8]],
    inbox_key_generation: u64,
) -> Roster {
    roster_signed_by(version, devices, tombstones, inbox_key_generation, None)
}

/// The same document with the signer forced, for the vectors about who is
/// allowed to sign what.
fn roster_signed_by(
    version: RosterVersion,
    devices: &[&[u8]],
    tombstones: &[&[u8]],
    inbox_key_generation: u64,
    signer_sk: Option<[u8; 32]>,
) -> Roster {
    roster_of(
        version,
        devices.iter().map(|label| device(label)).collect(),
        tombstones,
        inbox_key_generation,
        signer_sk,
    )
}

/// [`roster_signed_by`] over devices that are already resolved, for the §14.3
/// vectors, whose devices are counted rather than named.
fn roster_of(
    version: RosterVersion,
    resolved: Vec<FixtureDevice>,
    tombstones: &[&[u8]],
    inbox_key_generation: u64,
    signer_sk: Option<[u8; 32]>,
) -> Roster {
    let approver = resolved
        .first()
        .expect("a roster names an approving device");
    let certs = resolved
        .iter()
        .enumerate()
        .map(|(index, dev)| {
            let flags = if index == 0 {
                DEVICE_CERT_FLAG_ROSTER_SIGNING
            } else {
                0
            };
            cert(dev, flags, &ROOT_SK)
        })
        .collect();
    let signer = signer_sk.unwrap_or(if version.seq == 0 {
        ROOT_SK
    } else {
        approver.sign_sk
    });
    core_sign_roster(
        Roster {
            person_id: person_id(),
            recovery_epoch: version.recovery_epoch,
            seq: version.seq,
            devices: certs,
            tombstones: tombstones
                .iter()
                .map(|label| DeviceTombstone {
                    device_id: device(label).device_id,
                    revoked_at_seq: version.seq,
                })
                .collect(),
            approving_device_id: approver.device_id.clone(),
            inbox_key_generation,
            signer_sign_pk: Vec::new(),
            signature: Vec::new(),
        },
        signer.to_vec(),
    )
    .expect("roster signs")
}

/// A valid roster holding exactly `device_count` devices. §14.3 counts the
/// devices the document names, so a "resulting device count" of N is simply a
/// roster of size N.
fn roster_of_size(version: RosterVersion, device_count: usize) -> Roster {
    roster_of(
        version,
        (0..device_count)
            .map(|index| device_from_seed(0x30 + index as u8))
            .collect(),
        &[],
        0,
        None,
    )
}

/// A store that already knows the fixture person as a contact — the only
/// precondition `apply_contact_roster` has, since a roster about a stranger is
/// not this device's business (DL-3).
fn roster_store() -> MessageStore {
    let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
    store
        .upsert_contact(roster_contact())
        .expect("the fixture person is a contact");
    store
}

/// The envelope id the BLE-day vector's one message rides under. Fixed rather
/// than generated: the vector is about one message reaching one device, and a
/// fresh id per run would make "the sibling already has it" indistinguishable
/// from "the sibling got a different message".
const BLE_DAY_MSG_ID: [u8; 16] = [0xB1; 16];

/// A person's OWN genesis roster over a freshly generated fleet, signed by the
/// person root (§3) — the document `core_open_sync_record` admits a sync
/// record's author against.
///
/// Distinct from [`roster_of`] and friends, which build a *contact's* roster
/// from this file's pinned device labels. §8's vector needs real X25519
/// material to seal with, which the label fixtures deliberately do not carry
/// (a roster never seals anything), so it generates its own.
fn own_fleet_roster(person: &Identity, fleet: &[DeviceKeypair]) -> Roster {
    own_fleet_roster_at(
        person,
        fleet,
        RosterVersion {
            recovery_epoch: 0,
            seq: 0,
        },
    )
}

/// [`own_fleet_roster`] at a version the caller names, for the vectors whose
/// fixture pins one. Signed the way `roster_of` signs a contact's document: the
/// person root at genesis, the approving device once a roster has moved.
fn own_fleet_roster_at(
    person: &Identity,
    fleet: &[DeviceKeypair],
    version: RosterVersion,
) -> Roster {
    let certs = fleet
        .iter()
        .enumerate()
        .map(|(index, device)| {
            core_sign_device_cert(
                DeviceCert {
                    person_id: person.user_id.clone(),
                    device_sign_pk: device.sign_pk.clone(),
                    device_agree_pk: device.agree_pk.clone(),
                    added_epoch: 0,
                    flags: if index == 0 {
                        DEVICE_CERT_FLAG_ROSTER_SIGNING
                    } else {
                        0
                    },
                    signer_sign_pk: Vec::new(),
                    signature: Vec::new(),
                },
                person.sign_sk.clone(),
            )
            .expect("device certificate signs")
        })
        .collect();
    core_sign_roster(
        Roster {
            person_id: person.user_id.clone(),
            recovery_epoch: version.recovery_epoch,
            seq: version.seq,
            devices: certs,
            tombstones: Vec::new(),
            approving_device_id: fleet[0].device_id.clone(),
            inbox_key_generation: 0,
            signer_sign_pk: Vec::new(),
            signature: Vec::new(),
        },
        if version.seq == 0 {
            person.sign_sk.clone()
        } else {
            fleet[0].sign_sk.clone()
        },
    )
    .expect("own roster signs")
}

/// A contact row for a generated person, with their real keys — the only trust
/// anchor `apply_contact_roster` has (§3: the deployed identity key *is* the
/// person root, so no new anchor and no re-friending).
fn person_contact(identity: &Identity, name: &str) -> Contact {
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

/// The message this device would store for a body it just opened.
///
/// Every field is read off the decoded body rather than restated from the
/// constants that produced it: these vectors are about what survives the wire,
/// and a row assembled from what the test already believes would be asserting
/// against its own expectations. `chat_id` is the deliberate exception —
/// DELIVER-01 pins a pairwise row to its *verified sender's* thread, never to
/// the `chat_id` the sender wrote — so it comes from the verified sender.
fn stored_message(sender_user_id: &[u8], decoded: &ExtendedMessageBody) -> StoredMessage {
    StoredMessage {
        chat_id: sender_user_id.to_vec(),
        sender_user_id: sender_user_id.to_vec(),
        lamport: decoded.lamport,
        timestamp: decoded.timestamp,
        kind: decoded.kind,
        payload: decoded.content.clone(),
        sender_device_id: core_device_stream_id(decoded.sender_device_id.clone()),
    }
}

/// Author one body, seal it to `recipient`, open it, and hand back what the
/// open path decoded. This is the real §5 wire path — the same
/// `encode → seal_message → open_message → decode_extended_message_body` a
/// delivery runs — so a device id that survives it survived everything the
/// envelope does to it.
fn round_trip_body(
    sender: &Identity,
    recipient: &Identity,
    lamport: u64,
    sender_device_id: Option<Vec<u8>>,
) -> ExtendedMessageBody {
    let body = MessageBody {
        kind: KIND_TEXT,
        chat_id: sender.user_id.clone(),
        lamport,
        timestamp: NOW_MS,
        content: b"one person, one or more devices".to_vec(),
    };
    let payload = match sender_device_id {
        // The legacy encoder, byte for byte: no extension bytes at all.
        None => encode_message_body(body).expect("legacy body encodes"),
        Some(device_id) => encode_message_body_extended(body, None, Some(device_id), None)
            .expect("device-stamped body encodes"),
    };
    let sealed = seal_message(sender.clone(), recipient.agree_pk.clone(), payload).expect("seals");
    let opened = open_message(recipient.clone(), sealed).expect("opens");
    decode_extended_message_body(opened.payload).expect("decodes")
}

/// §10's cast, built through the shipped ceremonies: a person, the device that
/// survives, the device that gets buried, and the rosters either side of the
/// revocation.
///
/// A freshly generated person rather than this file's pinned label fixtures,
/// because §10.3's driver has to actually SEAL an envelope, and that needs
/// X25519 secrets the label fixtures deliberately do not carry.
fn revoking_person() -> (Identity, DeviceKeypair, DeviceKeypair, Roster, Roster) {
    let person = generate_identity();
    let kept = generate_device_keypair();
    let buried = generate_device_keypair();
    // `kept` holds the roster-signing role, because §10.1's ordinary path is
    // the approving device burying somebody else — an approving device may not
    // bury itself, and dethroning one takes §14.2's recovery material.
    let genesis = cruisemesh_core::core_link_genesis_roster(
        person.sign_sk.clone(),
        kept.sign_pk.clone(),
        kept.agree_pk.clone(),
    )
    .expect("genesis");
    let before = cruisemesh_core::core_link_sign_new_device_roster(
        genesis,
        person.sign_pk.clone(),
        kept.sign_sk.clone(),
        buried.sign_pk.clone(),
        buried.agree_pk.clone(),
    )
    .expect("link")
    .roster;
    let after = cruisemesh_core::core_revoke_devices_roster(
        before.clone(),
        person.sign_pk.clone(),
        kept.sign_sk.clone(),
        vec![buried.device_id.clone()],
        cruisemesh_core::core_mint_inbox_key(before.inbox_key_generation),
    )
    .expect("the approving device revokes")
    .roster;
    (person, kept, buried, before, after)
}

/// Deliver one ordinary 1:1 message from `device` through the production
/// inbound pair — `process_inbound_frame` then `core_deliver_inbound`, which is
/// the pair every shell runs — and hand back the verdict.
fn deliver_from_device(
    store: &MessageStore,
    me: &Identity,
    sender: &Identity,
    device: &DeviceKeypair,
    lamport: u64,
) -> CoreDeliveryVerdict {
    let payload = encode_message_body_extended(
        MessageBody {
            kind: KIND_TEXT,
            chat_id: sender.user_id.clone(),
            lamport,
            timestamp: NOW_MS,
            content: b"an ordinary, correctly signed message".to_vec(),
        },
        None,
        Some(device.device_id.clone()),
        None,
    )
    .expect("body encodes");
    let sealed = seal_message(sender.clone(), me.agree_pk.clone(), payload).expect("seals");
    let frame = encode_envelope_frame(
        cruisemesh_core::generate_msg_id(),
        cruisemesh_core::DEFAULT_HOP_TTL,
        NOW_MS + 7 * cruisemesh_core::MS_PER_DAY,
        compute_recipient_hint(me.user_id.clone(), NOW_MS),
        sealed,
    );
    let outcome = store
        .process_inbound_frame(
            me.clone(),
            std::sync::Arc::new(cruisemesh_core::SeenIds::new()),
            cruisemesh_core::CoreInboundSource::Mesh,
            frame,
            NOW_MS,
        )
        .expect("inbound");
    let payload = outcome
        .delivered_payloads
        .first()
        .cloned()
        .expect("an envelope addressed to us opens");
    store
        .core_deliver_inbound(
            me.clone(),
            outcome.delivered_sender.expect("verified sender"),
            payload,
            outcome.commit.expect("commit token"),
            MessageArrival {
                transport: 3,
                hops_taken: 0,
                received_at: NOW_MS,
            },
            CoreDiscoveryPolicyState {
                enabled: true,
                revision: 0,
            },
        )
        .expect("delivery")
        .verdict
}

/// Run one already-built envelope frame through the production inbound pair,
/// including the post-delivery commit the shells run — so the ACK-01 /
/// ACK-MD-1 consumed evidence is written exactly where the field writes it.
///
/// Distinct from [`deliver_from_device`], which builds the envelope it
/// delivers. This one takes a frame some *other* store authored, which is what
/// makes a two-store vector a wire test rather than a round trip.
fn deliver_frame(store: &MessageStore, me: &Identity, frame: Vec<u8>) -> CoreDeliveryVerdict {
    let seen = std::sync::Arc::new(cruisemesh_core::SeenIds::new());
    let outcome = store
        .process_inbound_frame(
            me.clone(),
            seen.clone(),
            cruisemesh_core::CoreInboundSource::Relay,
            frame,
            NOW_MS,
        )
        .expect("inbound");
    let commit = outcome.commit.expect("commit token");
    let verdict = store
        .core_deliver_inbound(
            me.clone(),
            outcome.delivered_sender.expect("verified sender"),
            outcome
                .delivered_payloads
                .first()
                .cloned()
                .expect("an envelope addressed to us opens"),
            commit.clone(),
            MessageArrival {
                transport: 3,
                hops_taken: 0,
                received_at: NOW_MS,
            },
            CoreDiscoveryPolicyState {
                enabled: true,
                revision: 0,
            },
        )
        .expect("delivery")
        .verdict;
    if verdict == CoreDeliveryVerdict::Applied {
        store.core_commit_inbound_delivery(seen, commit);
    }
    verdict
}

fn relay_item(
    relay_id: i64,
    disposition: CoreInboundDisposition,
    recipient_hint: Vec<u8>,
) -> CoreRelayEnvelopeDisposition {
    CoreRelayEnvelopeDisposition {
        relay_id,
        msg_id: vec![relay_id as u8; 16],
        disposition,
        recipient_hint,
    }
}

/// Run the PRODUCTION ack planner -- the same
/// `MessageStore::core_relay_ack_ids_with_consumed` every shell calls, where
/// the legacy shared-hint withholding lives and where WP2 added ACK-MD-1/2 --
/// over one relay row, on a store owned by `PERSON_ID`.
///
/// The store is a *two-device* fleet, because that is the situation every ack
/// vector below is about: `OWN_DEVICE_ID` is this device and
/// `SIBLING_DEVICE_ID` is the one it must not delete mail out from under. This
/// is the state §9's two-phase activation leaves behind, written through the
/// shipped `set_own_device_fleet`. A single-device identity's planner output is
/// unchanged by all of this, which `engine.rs`'s
/// `a_single_device_identity_plans_exactly_todays_acks` pins.
///
/// **The precondition is dormant.** A stored fleet of more than one device is
/// what activates every rule the vectors below assert, and nothing in
/// production writes one yet — §9's linking ceremony is WP3's, and this fixture
/// is the only kind of caller `set_own_device_fleet` has. The rules are
/// implemented and pinned here so that they are already right on the day WP3
/// supplies the writer; until then no device in the field ever reaches them.
fn plan_acks(item: CoreRelayEnvelopeDisposition) -> Vec<i64> {
    let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
    store
        .set_own_device_fleet(OwnDeviceFleet {
            own_device_id: Some(device(OWN_DEVICE_ID).device_id),
            device_ids: vec![
                device(OWN_DEVICE_ID).device_id,
                device(SIBLING_DEVICE_ID).device_id,
            ],
            // Core's own `RosterVersion`, not this file's same-named fixture:
            // the projection carries the DL-1 ordering key of the roster §9's
            // activation took it from.
            projected_from: cruisemesh_core::RosterVersion {
                recovery_epoch: 0,
                seq: 1,
            },
        })
        .expect("this device is activated into its own fleet");
    store
        .core_relay_ack_ids_with_consumed(vec![item], PERSON_ID.to_vec(), NOW_MS)
        .expect("production relay ack planner")
}

/// Execute every present-day vector through its real core surface. Future
/// vectors return `None`, preserving their data-only status until their work
/// package exists. The returned value is the driver result used by the
/// divergence ledger below -- never a copied `current_outcome` fixture.
fn drive(vector: &Vector) -> Option<Outcome> {
    if !vector.implemented {
        return None;
    }

    let outcome = match vector.scenario {
        // DL-1 ordering, driven end to end: every document below is really
        // signed, and `apply_contact_roster` is the production entry point --
        // `core_roster_accept` plus the `contact_rosters` / `contact_devices`
        // tables that must agree with its verdict.
        Scenario::RosterUpdate { stored, incoming } => {
            let store = roster_store();
            let devices: &[&[u8]] = &[b"alice-phone-device-key"];
            let stored_doc = roster_at(stored, devices, &[], 0);
            let first = store
                .apply_contact_roster(stored_doc.clone())
                .expect("the stored roster applies");
            contract_assert!(
                vector.id,
                first.outcome == RosterUpdateOutcome::Accepted
                    && first.reason == RosterUpdateReason::FirstRoster,
                "the stored roster must be the one this contact holds, got {first:?}"
            );

            let incoming_doc = roster_at(incoming, devices, &[], 0);
            let decision = store
                .apply_contact_roster(incoming_doc.clone())
                .expect("the incoming roster applies");
            // The rule to expect is read off the fixture's own versions, never
            // off `target_outcome` -- otherwise the driver would be agreeing
            // with the answer key instead of with the core.
            let expected_reason = match (incoming.recovery_epoch, incoming.seq)
                .cmp(&(stored.recovery_epoch, stored.seq))
            {
                std::cmp::Ordering::Greater => RosterUpdateReason::Superseded,
                std::cmp::Ordering::Equal => RosterUpdateReason::IdempotentRepeat,
                std::cmp::Ordering::Less => RosterUpdateReason::Rollback,
            };
            contract_assert!(
                vector.id,
                decision.reason == expected_reason,
                "DL-1 named the wrong rule: {decision:?}"
            );
            let held = store
                .contact_roster_state(person_id())
                .expect("stored roster state")
                .roster;
            let expected_held = if decision.outcome == RosterUpdateOutcome::Accepted {
                &incoming_doc
            } else {
                &stored_doc
            };
            contract_assert!(
                vector.id,
                held.as_ref() == Some(expected_held),
                "the persisted roster must be the one the decision named"
            );
            roster_outcome(decision.outcome)
        }
        // DL-1's second half: a strictly higher version whose device
        // certificate was signed by a key this person never vouched for.
        Scenario::RosterSignatureChain {
            stored,
            incoming,
            chain_verifies_to_person_root,
        } => {
            contract_assert!(
                vector.id,
                !chain_verifies_to_person_root,
                "the chain vector is the one whose chain does not verify"
            );
            let store = roster_store();
            let devices: &[&[u8]] = &[b"alice-phone-device-key"];
            let stored_doc = roster_at(stored, devices, &[], 0);
            store
                .apply_contact_roster(stored_doc.clone())
                .expect("the stored roster applies");

            let approver = device(b"alice-phone-device-key");
            let mut forged = roster_at(incoming, devices, &[], 0);
            forged.devices = vec![cert(
                &approver,
                DEVICE_CERT_FLAG_ROSTER_SIGNING,
                &STRANGER_SK,
            )];
            // Re-signed by the person root, so nothing but the certificate's
            // own authorizer is wrong: version ordering and the document
            // signature both pass, and the chain rule is what refuses it.
            let forged = core_sign_roster(forged, ROOT_SK.to_vec()).expect("forged roster signs");
            let decision = store
                .apply_contact_roster(forged)
                .expect("the forged roster applies");
            contract_assert!(
                vector.id,
                decision.rejection == Some(RosterRejection::ChainBroken),
                "an unvouched certificate signer must break the chain, got {decision:?}"
            );
            contract_assert!(
                vector.id,
                store
                    .contact_roster_state(person_id())
                    .expect("stored roster state")
                    .roster
                    .as_ref()
                    == Some(&stored_doc),
                "a higher version that does not verify must not replace what is stored"
            );
            roster_outcome(decision.outcome)
        }
        // DL-2: one version, two documents.
        Scenario::RosterFork {
            version,
            stored_content,
            incoming_content,
        } => {
            let store = roster_store();
            let stored_doc = roster_at(version, &[stored_content], &[], 0);
            let incoming_doc = roster_at(version, &[incoming_content], &[], 0);
            contract_assert!(
                vector.id,
                stored_doc != incoming_doc && stored_doc.version() == incoming_doc.version(),
                "a fork is two different documents at one version"
            );
            store
                .apply_contact_roster(stored_doc.clone())
                .expect("the stored roster applies");
            let decision = store
                .apply_contact_roster(incoming_doc)
                .expect("the forked roster applies");
            contract_assert!(
                vector.id,
                decision.reason == RosterUpdateReason::ForkedContent && decision.quarantined,
                "equal version plus different content is a fork, got {decision:?}"
            );
            let state = store
                .contact_roster_state(person_id())
                .expect("stored roster state");
            contract_assert!(
                vector.id,
                state.quarantined && state.roster.as_ref() == Some(&stored_doc),
                "DL-2 keeps the stored roster and records the quarantine"
            );
            roster_outcome(decision.outcome)
        }
        // DL-2's follow-on: quarantine is sticky, and a later good roster is
        // not a resolution.
        Scenario::RosterUpdateAfterFork {
            quarantined_at,
            incoming,
            incoming_is_strictly_higher,
            auto_resolves,
        } => {
            contract_assert!(
                vector.id,
                incoming_is_strictly_higher && !auto_resolves,
                "the fixture is a strictly higher roster that must not auto-resolve"
            );
            let store = roster_store();
            let stored_doc = roster_at(quarantined_at, &[b"approved-device-a"], &[], 0);
            store
                .apply_contact_roster(stored_doc.clone())
                .expect("the stored roster applies");
            let fork = store
                .apply_contact_roster(roster_at(quarantined_at, &[b"approved-device-b"], &[], 0))
                .expect("the forked roster applies");
            contract_assert!(
                vector.id,
                fork.outcome == RosterUpdateOutcome::ForkQuarantined,
                "the fork must quarantine before the follow-on is meaningful"
            );

            let later = roster_at(incoming, &[b"approved-device-a"], &[], 0);
            contract_assert!(
                vector.id,
                core_roster_validate(later.clone(), person_root_sign_pk()).is_none(),
                "the follow-on roster must be valid on its own terms"
            );
            let decision = store
                .apply_contact_roster(later)
                .expect("the later roster applies");
            contract_assert!(
                vector.id,
                decision.reason == RosterUpdateReason::PersonQuarantined && decision.quarantined,
                "a quarantined person's later rosters stay quarantined, got {decision:?}"
            );
            contract_assert!(
                vector.id,
                store
                    .contact_roster_state(person_id())
                    .expect("stored roster state")
                    .roster
                    .as_ref()
                    == Some(&stored_doc),
                "the pre-fork roster is what this device keeps holding"
            );
            roster_outcome(decision.outcome)
        }
        // DL-2's refinement: the person root at a higher epoch is heard.
        Scenario::RosterForkPiercedByRoot {
            quarantined_at,
            incoming,
            signed_by_person_root,
        } => {
            contract_assert!(
                vector.id,
                signed_by_person_root,
                "the whole point of this vector is who signed it"
            );
            let store = roster_store();
            store
                .apply_contact_roster(roster_at(quarantined_at, &[b"approved-device-a"], &[], 0))
                .expect("the stored roster applies");
            let fork = store
                .apply_contact_roster(roster_at(quarantined_at, &[b"approved-device-b"], &[], 0))
                .expect("the forked roster applies");
            contract_assert!(
                vector.id,
                fork.outcome == RosterUpdateOutcome::ForkQuarantined,
                "the fork must quarantine before the pierce is meaningful"
            );

            // A device key at the same higher epoch is refused first, so what
            // this vector proves is the SIGNER and not the epoch.
            let by_a_device = roster_signed_by(
                incoming,
                &[b"approved-device-a"],
                &[],
                0,
                Some(device(b"approved-device-a").sign_sk),
            );
            contract_assert!(
                vector.id,
                store
                    .apply_contact_roster(by_a_device)
                    .expect("applies")
                    .outcome
                    == RosterUpdateOutcome::ForkQuarantined,
                "a device key cannot pierce a quarantine at any epoch"
            );

            let rescue = roster_signed_by(incoming, &[b"approved-device-a"], &[], 0, Some(ROOT_SK));
            contract_assert!(
                vector.id,
                core_roster_validate(rescue.clone(), person_root_sign_pk()).is_none()
                    && rescue.signer_sign_pk == person_root_sign_pk(),
                "the rescue document must be valid AND root-signed on its own terms"
            );
            let decision = store
                .apply_contact_roster(rescue.clone())
                .expect("the rescue roster applies");
            let state = store
                .contact_roster_state(person_id())
                .expect("stored roster state");
            contract_assert!(
                vector.id,
                decision.outcome == RosterUpdateOutcome::Accepted
                    && !decision.quarantined
                    && !state.quarantined
                    && state.roster.as_ref() == Some(&rescue),
                "the root's recovery is adopted and the fork is resolved, got {decision:?}"
            );
            roster_outcome(decision.outcome)
        }
        // DL-4: a revoked device id never returns, in either of the two shapes
        // a returning device could take.
        Scenario::RosterTombstonedDeviceReturns {
            version,
            tombstoned_device_id,
            returning_device_id,
        } => {
            contract_assert!(
                vector.id,
                tombstoned_device_id == returning_device_id,
                "this vector is the same key trying to come back"
            );
            let epoch = version.recovery_epoch;
            let store = roster_store();
            store
                .apply_contact_roster(roster_at(
                    RosterVersion {
                        recovery_epoch: epoch,
                        seq: 0,
                    },
                    &[b"alice-phone-device-key", tombstoned_device_id],
                    &[],
                    0,
                ))
                .expect("genesis applies");
            // §10: the revocation buries the device and bumps the inbox key
            // generation.
            let revocation = store
                .apply_contact_roster(roster_at(
                    version,
                    &[b"alice-phone-device-key"],
                    &[tombstoned_device_id],
                    1,
                ))
                .expect("the revocation applies");
            contract_assert!(
                vector.id,
                revocation.outcome == RosterUpdateOutcome::Accepted,
                "the revocation itself must be accepted, got {revocation:?}"
            );

            let next = RosterVersion {
                recovery_epoch: epoch,
                seq: version.seq + 1,
            };
            // Shape one: listed as active while still tombstoned.
            let listed_again = store
                .apply_contact_roster(roster_at(
                    next,
                    &[b"alice-phone-device-key", returning_device_id],
                    &[returning_device_id],
                    1,
                ))
                .expect("the resurrection applies");
            contract_assert!(
                vector.id,
                listed_again.rejection == Some(RosterRejection::TombstonedDeviceActive),
                "a tombstoned device may not also be active, got {listed_again:?}"
            );
            // Shape two: the tombstone quietly dropped, so the device looks
            // new again. Only the stored roster remembers.
            let decision = store
                .apply_contact_roster(roster_at(
                    next,
                    &[b"alice-phone-device-key", returning_device_id],
                    &[],
                    1,
                ))
                .expect("the forgetful roster applies");
            contract_assert!(
                vector.id,
                decision.reason == RosterUpdateReason::TombstoneResurrected,
                "forgetting a burial is not a later version, got {decision:?}"
            );
            contract_assert!(
                vector.id,
                listed_again.outcome == RosterUpdateOutcome::Ignored
                    && decision.outcome == RosterUpdateOutcome::Ignored,
                "neither shape of a returning device may be accepted"
            );
            contract_assert!(
                vector.id,
                store
                    .contact_device_state(person_id(), device(returning_device_id).device_id)
                    .expect("device state")
                    == ContactDeviceState::Revoked,
                "the buried device stays buried"
            );
            Outcome::TombstonePermanent
        }
        // DL-4's other half: the hardware may come back, the key may not.
        Scenario::RosterRelinkFreshKey {
            version,
            tombstoned_device_id,
            replacement_device_id,
        } => {
            let tombstoned = device(tombstoned_device_id);
            let replacement = device(replacement_device_id);
            contract_assert!(
                vector.id,
                tombstoned.device_id != replacement.device_id,
                "re-linking must mint a fresh key, not reuse the buried one"
            );
            let epoch = version.recovery_epoch;
            let store = roster_store();
            store
                .apply_contact_roster(roster_at(
                    RosterVersion {
                        recovery_epoch: epoch,
                        seq: 0,
                    },
                    &[b"alice-phone-device-key", tombstoned_device_id],
                    &[],
                    0,
                ))
                .expect("genesis applies");
            store
                .apply_contact_roster(roster_at(
                    RosterVersion {
                        recovery_epoch: epoch,
                        seq: version.seq - 1,
                    },
                    &[b"alice-phone-device-key"],
                    &[tombstoned_device_id],
                    1,
                ))
                .expect("the revocation applies");

            let decision = store
                .apply_contact_roster(roster_at(
                    version,
                    &[b"alice-phone-device-key", replacement_device_id],
                    &[tombstoned_device_id],
                    1,
                ))
                .expect("the re-link applies");
            contract_assert!(
                vector.id,
                decision.outcome == RosterUpdateOutcome::Accepted
                    && decision.reason == RosterUpdateReason::Superseded,
                "a fresh key beside the kept tombstone is an ordinary later roster, got {decision:?}"
            );
            contract_assert!(
                vector.id,
                store
                    .contact_active_device_ids(person_id())
                    .expect("active device ids")
                    .contains(&replacement.device_id),
                "the re-linked device must be active"
            );
            contract_assert!(
                vector.id,
                store
                    .contact_device_state(person_id(), tombstoned.device_id)
                    .expect("device state")
                    == ContactDeviceState::Revoked,
                "the replaced key stays revoked"
            );
            Outcome::FreshKeyAccepted
        }
        // DL-5: a roster carries keys, ids, and counters. There is nowhere an
        // endpoint could live, and the shape check is what makes that
        // structural rather than conventional.
        Scenario::RosterKeysNeverEndpoints { version, document } => {
            let valid = roster_at(version, document.device_keys, document.tombstones, 0);
            contract_assert!(
                vector.id,
                core_roster_validate(valid.clone(), person_root_sign_pk()).is_none(),
                "a keys-and-tombstones document must be valid"
            );
            contract_assert!(
                vector.id,
                valid
                    .devices
                    .iter()
                    .all(|cert| cert.device_sign_pk.len() == 32 && cert.device_agree_pk.len() == 32)
                    && valid
                        .tombstones
                        .iter()
                        .all(|tombstone| tombstone.device_id.len() == DEVICE_ID_LEN),
                "every roster byte field is a fixed-width key or id"
            );

            // Try to smuggle an endpoint through each byte field that is not a
            // counter. There is no field where it fits.
            let endpoint = b"relay.example.com:8443".to_vec();
            for smuggled in [
                {
                    let mut roster = valid.clone();
                    roster.approving_device_id = endpoint.clone();
                    roster
                },
                {
                    let mut roster = valid.clone();
                    roster.person_id = endpoint.clone();
                    roster
                },
                {
                    let mut roster = valid.clone();
                    roster.devices[0].device_agree_pk = endpoint.clone();
                    roster
                },
                {
                    let mut roster = valid.clone();
                    roster.tombstones[0].device_id = endpoint.clone();
                    roster
                },
            ] {
                let rejection = core_roster_validate(smuggled, person_root_sign_pk());
                contract_assert!(
                    vector.id,
                    rejection == Some(RosterRejection::MalformedField),
                    "an endpoint-shaped value must be refused by the shape check, got {rejection:?}"
                );
            }

            // The honest limit of that gate, pinned so nobody reads the loop
            // above as more than it is: an address padded to exactly the key
            // width passes the shape check, because a fixed-width blob is
            // indistinguishable from a key. What DL-5 rests on is that the
            // width leaves no room for a *usable* free-form address and that
            // nothing downstream ever reads these bytes as text -- so the
            // smuggled value survives only as a key nobody can seal to, and
            // the document it rides in still has to chain to the person root.
            let mut padded = b"relay.example.com:8443".to_vec();
            padded.resize(32, 0);
            let mut correct_width = valid.clone();
            correct_width.devices[0].device_agree_pk = padded.clone();
            contract_assert!(
                vector.id,
                core_roster_validate(correct_width, person_root_sign_pk())
                    == Some(RosterRejection::CertSignatureInvalid),
                "a correct-width smuggle still has to survive the certificate signature"
            );
            // And re-signed, so the signature is not what stops it: the value
            // is accepted, as a key. That is the residual DL-5 does not close,
            // and it is bounded by there being nothing that reads it as a host.
            let smuggler = device(document.device_keys[0]);
            let mut resigned = valid.clone();
            resigned.devices[0] = core_sign_device_cert(
                DeviceCert {
                    person_id: person_id(),
                    device_sign_pk: smuggler.sign_pk.clone(),
                    device_agree_pk: padded,
                    added_epoch: 0,
                    flags: DEVICE_CERT_FLAG_ROSTER_SIGNING,
                    signer_sign_pk: Vec::new(),
                    signature: Vec::new(),
                },
                ROOT_SK.to_vec(),
            )
            .expect("re-signed certificate");
            let resigned = core_sign_roster(resigned, ROOT_SK.to_vec()).expect("roster re-signs");
            contract_assert!(
                vector.id,
                core_roster_validate(resigned, person_root_sign_pk()).is_none(),
                "the width check is a shape gate, not a content gate -- say so rather than \
                 claiming endpoints are impossible"
            );

            let store = roster_store();
            let decision = store
                .apply_contact_roster(valid.clone())
                .expect("the roster applies");
            contract_assert!(
                vector.id,
                decision.outcome == RosterUpdateOutcome::Accepted
                    && store
                        .contact_roster_state(person_id())
                        .expect("stored roster state")
                        .roster
                        .as_ref()
                        == Some(&valid),
                "what is stored is the same keys-only document"
            );
            Outcome::KeysOnlyNoEndpoints
        }
        // §14.2: the epoch belongs to the recovery material — the person root
        // secret that lives only inside the encrypted `.cmbak`. What is driven
        // here is the rule the code enforces: an approving device may not
        // raise the epoch above a stored one. Where the secret *lives* is
        // MD-RECOVERY-ROOT-CUSTODY's, and stays data-only.
        Scenario::RecoveryEpochRequiresRoot {
            version,
            device_key_alone_can_mint_higher_epoch,
        } => {
            contract_assert!(
                vector.id,
                !device_key_alone_can_mint_higher_epoch,
                "§14.2 epoch-rule fixture changed"
            );
            let store = roster_store();
            let devices: &[&[u8]] = &[b"alice-phone-device-key"];
            let previous = version.recovery_epoch - 1;
            store
                .apply_contact_roster(roster_at(
                    RosterVersion {
                        recovery_epoch: previous,
                        seq: 0,
                    },
                    devices,
                    &[],
                    0,
                ))
                .expect("genesis applies");
            let stored_doc = roster_at(
                RosterVersion {
                    recovery_epoch: previous,
                    seq: 1,
                },
                devices,
                &[],
                0,
            );
            store
                .apply_contact_roster(stored_doc.clone())
                .expect("the approving device's ordinary roster applies");

            // The approving device tries to mint the new epoch itself, in both
            // shapes it could take.
            let approver_sk = device(b"alice-phone-device-key").sign_sk;
            let refused_genesis = store
                .apply_contact_roster(roster_signed_by(
                    version,
                    devices,
                    &[],
                    0,
                    Some(approver_sk),
                ))
                .expect("the device-signed epoch genesis applies");
            contract_assert!(
                vector.id,
                refused_genesis.rejection == Some(RosterRejection::GenesisNotRootSigned),
                "a new epoch's first roster must be root-signed, got {refused_genesis:?}"
            );
            let refused_later = store
                .apply_contact_roster(roster_signed_by(
                    RosterVersion {
                        recovery_epoch: version.recovery_epoch,
                        seq: 1,
                    },
                    devices,
                    &[],
                    0,
                    Some(approver_sk),
                ))
                .expect("the device-signed epoch bump applies");
            contract_assert!(
                vector.id,
                refused_later.reason == RosterUpdateReason::RecoveryEpochRequiresRoot,
                "an approving device cannot raise the epoch, got {refused_later:?}"
            );
            contract_assert!(
                vector.id,
                refused_genesis.outcome == RosterUpdateOutcome::Ignored
                    && refused_later.outcome == RosterUpdateOutcome::Ignored
                    && store
                        .contact_roster_state(person_id())
                        .expect("stored roster state")
                        .roster
                        .as_ref()
                        == Some(&stored_doc),
                "a stolen approving device may not dethrone the backup"
            );

            let decision = store
                .apply_contact_roster(roster_at(version, devices, &[], 0))
                .expect("the recovery roster applies");
            contract_assert!(
                vector.id,
                decision.outcome == RosterUpdateOutcome::Accepted
                    && decision.reason == RosterUpdateReason::Superseded,
                "only the root secret raises the epoch, got {decision:?}"
            );
            Outcome::EpochRequiresRootSignature
        }
        // §5: an envelope with no sealed-body device field -- which is every
        // legacy sender, permanently -- lands on the reserved all-zero stream,
        // through the real encode/seal/open/decode/insert path.
        Scenario::LegacyEnvelopeWithoutDevice { legacy_device_id } => {
            contract_assert!(
                vector.id,
                legacy_device_id == CORE_LEGACY_DEVICE_ID,
                "core's reserved stream id must be the one this vector pins"
            );
            let sender = generate_identity();
            let recipient = generate_identity();
            let decoded = round_trip_body(&sender, &recipient, 7, None);
            contract_assert!(
                vector.id,
                decoded.sender_device_id.is_none(),
                "a legacy body carries no device field at all"
            );
            contract_assert!(
                vector.id,
                core_device_stream_id(decoded.sender_device_id.clone())
                    == legacy_device_id.to_vec(),
                "an absent device field maps onto the legacy stream"
            );

            let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
            let outcome = store
                .insert_incoming_message_from_device(
                    stored_message(&sender.user_id, &decoded),
                    decoded.sender_device_id,
                    vec![1; 16],
                    None,
                    None,
                )
                .expect("device-aware incoming insert");
            contract_assert!(
                vector.id,
                outcome == IncomingMessageInsertOutcome::Inserted,
                "the legacy row must insert, got {outcome:?}"
            );
            contract_assert!(
                vector.id,
                store
                    .message_stream_device_ids(sender.user_id.clone(), sender.user_id.clone())
                    .expect("stream device ids")
                    == vec![legacy_device_id.to_vec()],
                "the row must land on the legacy stream and nowhere else"
            );
            Outcome::LegacyDeviceStream
        }
        // §5: two of one person's devices at one lamport are two streams, not
        // a fork. Both bodies take the real wire path, because the device id
        // has to survive the seal to be worth anything.
        Scenario::TwoDevicesSamePersonSameLamport {
            first_device_id,
            second_device_id,
        } => {
            let first = device(first_device_id);
            let second = device(second_device_id);
            contract_assert!(
                vector.id,
                first.device_id != second.device_id,
                "the scenario must name two distinct device ids"
            );
            let sender = generate_identity();
            let recipient = generate_identity();
            let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
            for (index, authoring) in [&first, &second].iter().enumerate() {
                let decoded =
                    round_trip_body(&sender, &recipient, 7, Some(authoring.device_id.clone()));
                contract_assert!(
                    vector.id,
                    decoded.sender_device_id.as_deref() == Some(authoring.device_id.as_slice()),
                    "the authoring device must survive the seal"
                );
                let outcome = store
                    .insert_incoming_message_from_device(
                        stored_message(&sender.user_id, &decoded),
                        decoded.sender_device_id,
                        vec![index as u8 + 1; 16],
                        None,
                        None,
                    )
                    .expect("device-aware incoming insert");
                contract_assert!(
                    vector.id,
                    outcome == IncomingMessageInsertOutcome::Inserted,
                    "both sibling rows must insert, got {outcome:?} for device {index}"
                );
            }
            contract_assert!(
                vector.id,
                !store.has_message_conflicts().expect("conflict diagnostic"),
                "siblings at one lamport must not read as a fork"
            );
            let mut expected = vec![first.device_id, second.device_id];
            expected.sort();
            contract_assert!(
                vector.id,
                store
                    .message_stream_device_ids(sender.user_id.clone(), sender.user_id.clone())
                    .expect("stream device ids")
                    == expected,
                "the person must hold one stream per device"
            );
            Outcome::SeparateStreams
        }
        // §5, WP4: the authoring path itself. Nothing here hands the store a
        // device id — the whole claim is that `author_pairwise_message` reads
        // the fleet this device was activated into and stamps both the stored
        // row and the sealed body from it, which is what makes a sibling's copy
        // of the person's own outbound distinguishable from this device's.
        Scenario::AuthoringDeviceStamped { linked } => {
            let person = generate_identity();
            let recipient = generate_identity();
            let contact = Contact {
                user_id: recipient.user_id.clone(),
                name: "Ash".to_string(),
                sign_pk: recipient.sign_pk.clone(),
                agree_pk: recipient.agree_pk.clone(),
                relay_url: None,
                relay_token: None,
                nickname: None,
            };
            let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
            store.upsert_contact(contact.clone()).expect("contact");
            let own = device(OWN_DEVICE_ID);
            if linked {
                store
                    .set_own_device_fleet(OwnDeviceFleet {
                        own_device_id: Some(own.device_id.clone()),
                        device_ids: vec![
                            own.device_id.clone(),
                            device(SIBLING_DEVICE_ID).device_id,
                        ],
                        projected_from: cruisemesh_core::RosterVersion {
                            recovery_epoch: 0,
                            seq: 1,
                        },
                    })
                    .expect("this device is activated into its own fleet");
            }

            let authored = store
                .author_pairwise_message(
                    person.clone(),
                    contact,
                    KIND_TEXT,
                    b"we docked early".to_vec(),
                    None,
                    NOW_MS,
                )
                .expect("author");
            let expected_stream = if linked {
                own.device_id.clone()
            } else {
                CORE_LEGACY_DEVICE_ID.to_vec()
            };
            contract_assert!(
                vector.id,
                authored.message.sender_device_id == expected_stream,
                "the stored row must live on this device's stream, got {:?}",
                authored.message.sender_device_id
            );
            contract_assert!(
                vector.id,
                store
                    .message_stream_device_ids(recipient.user_id.clone(), person.user_id.clone())
                    .expect("stream device ids")
                    == vec![expected_stream],
                "and on that stream only"
            );

            // The seal is where it has to survive: a recipient reads the
            // authoring device out of the sealed body, never out of the public
            // header, which is unchanged by any of this (§5, §12).
            let opened =
                open_message(recipient, authored.envelope.sealed.clone()).expect("recipient opens");
            let decoded = decode_extended_message_body(opened.payload).expect("decodes");
            if linked {
                contract_assert!(
                    vector.id,
                    decoded.sender_device_id.as_deref() == Some(own.device_id.as_slice()),
                    "an activated device must name itself inside the seal"
                );
                contract_assert!(
                    vector.id,
                    core_device_stream_id(decoded.sender_device_id) == own.device_id,
                    "and the recipient must file it on that device's stream"
                );
                Outcome::AuthoredOnOwnDeviceStream
            } else {
                contract_assert!(
                    vector.id,
                    decoded.sender_device_id.is_none(),
                    "an install that has never linked must emit no device field \
                     at all: §12's compatibility rests on its envelopes being \
                     identical to today's, not merely equivalent to them"
                );
                let body = MessageBody {
                    kind: KIND_TEXT,
                    chat_id: person.user_id.clone(),
                    lamport: authored.message.lamport,
                    timestamp: authored.message.timestamp,
                    content: b"we docked early".to_vec(),
                };
                contract_assert!(
                    vector.id,
                    encode_message_body(body.clone()).expect("legacy body encodes")
                        == encode_message_body_extended(body, None, None, None)
                            .expect("extended encoder with nothing to add"),
                    "the unlinked authoring path must not have grown a byte"
                );
                Outcome::AuthoredOnLegacyStream
            }
        }
        // §8 SYNC-2: this device has taken the sibling's authored row through
        // the History stream. Composing the same text again must edit the
        // draft, not put a second copy on a second stream.
        Scenario::SiblingAlreadyAuthoredThisOutbound => {
            let person = generate_identity();
            let recipient = generate_identity();
            let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
            let own = device(OWN_DEVICE_ID);
            let sibling = device(SIBLING_DEVICE_ID);
            store
                .set_own_device_fleet(OwnDeviceFleet {
                    own_device_id: Some(own.device_id.clone()),
                    device_ids: vec![own.device_id.clone(), sibling.device_id.clone()],
                    projected_from: cruisemesh_core::RosterVersion {
                        recovery_epoch: 0,
                        seq: 1,
                    },
                })
                .expect("this device is activated into its own fleet");

            // Exactly what `core_apply_sync_record` leaves behind for a
            // sibling's authored row: the person's own message, on the
            // sibling's device stream.
            store
                .insert_incoming_message_from_device(
                    StoredMessage {
                        chat_id: recipient.user_id.clone(),
                        sender_user_id: person.user_id.clone(),
                        lamport: 4,
                        timestamp: NOW_MS,
                        kind: KIND_TEXT,
                        payload: b"we docked early".to_vec(),
                        sender_device_id: sibling.device_id.clone(),
                    },
                    Some(sibling.device_id.clone()),
                    vec![9; 16],
                    None,
                    None,
                )
                .expect("the sibling's authored row lands");

            let claim = store
                .core_sync_outbound_claim(
                    person.user_id.clone(),
                    recipient.user_id.clone(),
                    KIND_TEXT,
                    b"we docked early".to_vec(),
                    NOW_MS - SYNC_OUTBOUND_DEDUP_WINDOW_MS,
                )
                .expect("SYNC-2 claim");
            contract_assert!(
                vector.id,
                claim.decision == OutboundAuthorDecision::AlreadyAuthoredBySibling
                    && claim.author_device_id.as_deref() == Some(sibling.device_id.as_slice()),
                "SYNC-2 must name the sibling that already sent it, got {claim:?}"
            );
            // The discriminator: a different message in the same chat is not
            // the same send, and refusing it would be the app losing mail.
            contract_assert!(
                vector.id,
                store
                    .core_sync_outbound_claim(
                        person.user_id,
                        recipient.user_id,
                        KIND_TEXT,
                        b"we docked late".to_vec(),
                        NOW_MS - SYNC_OUTBOUND_DEDUP_WINDOW_MS,
                    )
                    .expect("SYNC-2 claim")
                    .decision
                    == OutboundAuthorDecision::Author,
                "only the identical outbound is deduped"
            );
            Outcome::DeferredToSiblingAuthor
        }
        Scenario::OwnDeviceFanoutConsumed => {
            // ACK-MD-1: this device successfully opened a row addressed to its
            // OWN device hint namespace. This is the row that keeps acking
            // after WP2 -- it is the one namespace this device may delete from,
            // and the row it names has exactly one true consumer.
            let item = fanout_item(41, CoreInboundDisposition::Consumed, OWN_DEVICE_ID);
            let planned = plan_acks(item.clone());
            contract_assert!(
                vector.id,
                planned == vec![41],
                "the production planner must include the consumed own-device row, got {planned:?}"
            );
            // Secondary: the disposition-only rule underneath the planner.
            contract_assert!(
                vector.id,
                core_should_ack_inbound(item.disposition),
                "a consumed own-device row must be ackable"
            );
            contract_assert!(
                vector.id,
                core_relay_ack_ids(vec![item]) == vec![41],
                "the disposition-only planner agrees on the own-device row"
            );
            Outcome::Acknowledged
        }
        Scenario::SiblingDeviceFanoutConsumed => {
            // ACK-MD-1, as a real namespace refusal. WP0 wrote this vector
            // against a Carried fixture and said so: the row was merely an
            // envelope this device could not open, so it was withheld by the
            // disposition rule and the sibling hint was invisible to the
            // planner. That reading was never the rule -- and it was never
            // even the crypto, because §6 seals to a person-scoped inbox key,
            // so a sibling's row genuinely opens on this device.
            //
            // So the fixture is CONSUMED now, which is what a sibling's row
            // really reports here, and the two assertions below are the
            // discriminator: the disposition-only planner acks it, and the
            // production planner does not. The namespace is the only thing
            // standing between a sibling's mail and deletion.
            let item = fanout_item(42, CoreInboundDisposition::Consumed, SIBLING_DEVICE_ID);
            let planned = plan_acks(item.clone());
            contract_assert!(
                vector.id,
                planned.is_empty(),
                "the production planner must refuse a sibling's row by namespace, got {planned:?}"
            );
            // The fixture's `Consumed` is not self-evidence that a sibling's
            // row opens here -- this file cannot open anything, it plans acks.
            // The evidence that it does is elsewhere and is real: every row of
            // a §7 fan-out carries IDENTICAL sealed bytes
            // (`core_device_fanout_rows`, pinned by
            // `a_multi_device_contact_gets_one_row_per_device`), because §6
            // seals to a person-scoped inbox key; and `mesh_sim`'s
            // `a_two_device_recipient_over_relay_does_not_starve_the_sibling`
            // drives a real seal/open through the production inbound path on a
            // fleet of two. What this assertion pins is narrower and is the
            // discriminator: the disposition-only planner WOULD delete this
            // row, so the namespace is the only thing standing between a
            // sibling's mail and a deletion.
            contract_assert!(
                vector.id,
                core_should_ack_inbound(item.disposition),
                "the fixture must be a genuinely consumed row, or the refusal proves nothing"
            );
            contract_assert!(
                vector.id,
                core_relay_ack_ids(vec![item]) == vec![42],
                "the disposition-only planner, which cannot see namespaces, would delete it"
            );
            Outcome::NotAcknowledgedByNamespaceRefusal
        }
        Scenario::SiblingDeviceFanoutCarried => {
            // The guarantee the vector above no longer covers: a sibling's row
            // this device only muled is withheld by the disposition rule, with
            // no namespace reasoning involved at all. Both planners agree here,
            // which is exactly how it differs from the consumed case.
            let item = fanout_item(45, CoreInboundDisposition::Carried, SIBLING_DEVICE_ID);
            let planned = plan_acks(item.clone());
            contract_assert!(
                vector.id,
                planned.is_empty(),
                "a carried sibling row must plan no acknowledgement, got {planned:?}"
            );
            contract_assert!(
                vector.id,
                !core_should_ack_inbound(item.disposition),
                "Carried must not be ackable on its disposition"
            );
            contract_assert!(
                vector.id,
                core_relay_ack_ids(vec![item]).is_empty(),
                "the disposition-only planner refuses it too -- this row needs \
                 no namespace rule to survive"
            );
            Outcome::NotAcknowledgedBecauseCarried
        }
        Scenario::LegacyPersonAddressedConsumed => {
            // ACK-MD-2: a legacy sender uploads ONE person-addressed row, so
            // this fixture carries the bare person hint -- not a device
            // namespace hint. The person key opens it here, so it is genuinely
            // Consumed; deleting it would take away the only copy the siblings
            // could still fetch, which is §1's starvation failure told from the
            // legacy sender's side.
            //
            // This row and MD-ACK-OWN-FANOUT's differ in exactly one input:
            // `recipient_hint`. That is now the input the planner reads, and it
            // is why this vector plans nothing while MD-ACK-OWN-FANOUT still
            // plans `[41]`.
            let item = relay_item(43, CoreInboundDisposition::Consumed, person_hint());
            let planned = plan_acks(item.clone());
            contract_assert!(
                vector.id,
                planned.is_empty(),
                "a multi-device fleet must leave the legacy person row, got {planned:?}"
            );
            contract_assert!(
                vector.id,
                core_should_ack_inbound(item.disposition),
                "a successfully opened legacy person row really is consumed"
            );
            contract_assert!(
                vector.id,
                core_relay_ack_ids(vec![item]) == vec![43],
                "the disposition-only planner, which knows nothing of fleets, would delete it"
            );
            Outcome::NotAcknowledgedByNamespaceRefusal
        }
        Scenario::PersonSealedCarriedCopy => {
            let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
            let now_ms = NOW_MS;
            let person_id = PERSON_ID.to_vec();
            let msg_id = vec![8; 16];
            contract_assert!(
                vector.id,
                !core_should_ack_inbound(CoreInboundDisposition::Carried),
                "ACK-MD-3 carried copies must not enter the relay ack plan"
            );
            contract_assert!(
                vector.id,
                plan_acks(relay_item(
                    44,
                    CoreInboundDisposition::Carried,
                    person_hint()
                ))
                .is_empty(),
                "a carried disposition must plan no relay acknowledgement"
            );
            contract_assert!(
                vector.id,
                store
                    .enqueue_carried_envelope(
                        CarriedEnvelope {
                            msg_id: msg_id.clone(),
                            hop_ttl: 6,
                            expiry: now_ms + 60_000,
                            recipient_hint: compute_recipient_hint(person_id.clone(), now_ms),
                            sealed: vec![9; 64],
                        },
                        true,
                        now_ms,
                        1024 * 1024,
                    )
                    .expect("enqueue person-sealed carried copy"),
                "the carried fixture must be retained until receipt proof"
            );
            contract_assert!(
                vector.id,
                store
                    .core_confirm_carried_deliveries(
                        person_id.clone(),
                        vec![msg_id.clone()],
                        false,
                        now_ms,
                    )
                    .expect("unauthenticated digest")
                    == 0,
                "an unauthenticated digest must never remove the carried copy"
            );
            contract_assert!(
                vector.id,
                store
                    .carried_len()
                    .expect("carried count after unauthenticated digest")
                    == 1,
                "the unauthenticated digest must leave the carried copy present"
            );
            contract_assert!(
                vector.id,
                store
                    .core_confirm_carried_deliveries(person_id, vec![msg_id], true, now_ms)
                    .expect("authenticated digest")
                    == 1,
                "today an authenticated single-device digest retires the person-sealed copy"
            );
            Outcome::DigestProofOnly
        }
        Scenario::CapabilityReservation => {
            contract_assert!(
                vector.id,
                CAP_MULTI_DEVICE == 1 << 2,
                "CAP_MULTI_DEVICE must retain its assigned bit"
            );
            // §12: WPT reserved the bit, WP1 advertises it, and the claim is
            // true -- MD-STREAM-LEGACY-ID and MD-STREAM-SIBLING-LAMPORT above
            // drive the per-device streams this bit announces.
            contract_assert!(
                vector.id,
                core_own_capabilities() & CAP_MULTI_DEVICE != 0,
                "WP1 advertises CAP_MULTI_DEVICE through HELLO2"
            );
            Outcome::Advertised
        }
        // §14.3, through the shipped ADD PATH rather than through the policy
        // function alone.
        //
        // WP0 wrote these vectors against `core_device_add_outcome` directly,
        // which was the only cap code that existed: a free-standing decision
        // no roster ever consulted. The HARD cap has been enforced since WP1 —
        // `core_roster_validate` refuses a document over `DEVICE_HARD_CAP`, so
        // MD-DEVICE-CAP-17's refusal predates WP2. What WP2 added is the SOFT
        // cap verdict travelling back with the decision, and the folding of
        // both into one implementation, so the refusal and the verdict cannot
        // drift apart. The driver now performs a real add and reads what the
        // store did; the vectors' ids, scenarios and targets are unchanged.
        //
        // The verdict the CONTACT path returns is deliberately non-surfacing
        // (§2 goal 1 — a person's device count is invisible to other users), so
        // the driver reads the vector's outcome off the cap POLICY, which is
        // what WP3's own-roster add path will surface, and separately asserts
        // that the contact path declines to say it.
        Scenario::AddDevice {
            resulting_device_count,
        } => {
            // Discriminating, where asserting the two cap constants was not:
            // §14.3's boundary is only real if documents ONE device apart get
            // different verdicts, at both the soft and the hard edge.
            contract_assert!(
                vector.id,
                core_device_add_outcome(DEVICE_SOFT_CAP) == DeviceAddOutcome::Added
                    && core_device_add_outcome(DEVICE_SOFT_CAP + 1)
                        == DeviceAddOutcome::AddedWithWarning
                    && core_device_add_outcome(DEVICE_HARD_CAP)
                        == DeviceAddOutcome::AddedWithWarning
                    && core_device_add_outcome(DEVICE_HARD_CAP + 1) == DeviceAddOutcome::Refused,
                "§14.3's boundaries must separate documents one device apart"
            );
            let store = roster_store();
            let count = usize::from(resulting_device_count);
            // The roster this person held before the add, then the document
            // that performs it. Two applies, because "resulting device count"
            // is a claim about an add and not about a document appearing from
            // nowhere.
            let before = store.apply_contact_roster(roster_of_size(
                RosterVersion {
                    recovery_epoch: 0,
                    seq: 0,
                },
                count - 1,
            ));
            contract_assert!(
                vector.id,
                matches!(before.map(|d| d.outcome), Ok(RosterUpdateOutcome::Accepted)),
                "the roster held before this add must itself be legal"
            );
            let decision = store
                .apply_contact_roster(roster_of_size(
                    RosterVersion {
                        recovery_epoch: 0,
                        seq: 1,
                    },
                    count,
                ))
                .expect("the shipped roster path");
            let policy = core_device_add_outcome(u32::from(resulting_device_count));
            // One implementation of §14.3, with one deliberate difference: the
            // add path must agree with the policy about REFUSING, or a shell
            // could warn about a device the store refused (or stay silent about
            // one it kept) -- but it must NOT pass the soft-cap warning on,
            // because this document is about someone else.
            contract_assert!(
                vector.id,
                decision.device_count_outcome
                    == if policy == DeviceAddOutcome::AddedWithWarning {
                        DeviceAddOutcome::Added
                    } else {
                        policy
                    },
                "the contact path must report the cap verdict without surfacing \
                 the soft cap, got {decision:?} for {resulting_device_count} devices"
            );
            contract_assert!(
                vector.id,
                decision.device_count_outcome != DeviceAddOutcome::AddedWithWarning,
                "§2 goal 1: a contact's device count is never surfaced from gossip"
            );
            let held = store
                .contact_active_device_ids(person_id())
                .expect("stored devices")
                .len();
            match policy {
                DeviceAddOutcome::Refused => {
                    // Refusal is the roster being ignored whole (DL-1 leaves
                    // the stored one alone), not a truncated add.
                    contract_assert!(
                        vector.id,
                        decision.outcome == RosterUpdateOutcome::Ignored
                            && decision.rejection == Some(RosterRejection::DeviceCapExceeded),
                        "the {resulting_device_count}th device must be refused by the store, got {decision:?}"
                    );
                    contract_assert!(
                        vector.id,
                        held == count - 1,
                        "a refused add leaves the person on the devices they had, got {held}"
                    );
                    Outcome::DeviceAddRefused
                }
                added => {
                    contract_assert!(
                        vector.id,
                        decision.outcome == RosterUpdateOutcome::Accepted && held == count,
                        "the {resulting_device_count}th device must land, got {decision:?} / {held} held"
                    );
                    match added {
                        DeviceAddOutcome::AddedWithWarning => Outcome::DeviceAddedWithWarning,
                        _ => Outcome::DeviceAdded,
                    }
                }
            }
        }
        // §8's BLE-only day, driven end to end through the shipped self-sync
        // surface: no relay leg, no second encounter with the sender, and no
        // moment at which the two devices are assumed to be up together.
        Scenario::BleOnlyDayConverges {
            reached_over_ble,
            fleet_size,
            converges_by_self_sync,
        } => {
            let person = generate_identity();
            let contact = generate_identity();
            // §6's person-scoped inbox key as v1 has it: the identity
            // agreement keypair every linked device already holds. WP3's
            // ceremony mints a separate one; `generation` is the field that
            // will carry it when it does.
            let inbox_key = InboxKey {
                generation: 0,
                agree_pk: person.agree_pk.clone(),
                agree_sk: person.agree_sk.clone(),
            };
            let fleet: Vec<DeviceKeypair> =
                (0..fleet_size).map(|_| generate_device_keypair()).collect();
            let roster = own_fleet_roster(&person, &fleet);
            let roster_version = cruisemesh_core::RosterVersion {
                recovery_epoch: roster.recovery_epoch,
                seq: roster.seq,
            };
            let stores: Vec<MessageStore> = (0..fleet_size)
                .map(|_| MessageStore::open(":memory:".to_string()).expect("in-memory store"))
                .collect();

            // The BLE leg. One sealed envelope, opened and filed by exactly
            // `reached_over_ble` devices -- the radio reaches who it reaches,
            // and nothing in this vector pretends otherwise.
            let sealed = seal_message(
                contact.clone(),
                person.agree_pk.clone(),
                encode_message_body_extended(
                    MessageBody {
                        kind: KIND_TEXT,
                        chat_id: person.user_id.clone(),
                        lamport: 1,
                        timestamp: NOW_MS,
                        content: b"we docked early".to_vec(),
                    },
                    None,
                    None,
                    None,
                )
                .expect("body encodes"),
            )
            .expect("the contact seals to the person");
            for store in stores.iter().take(reached_over_ble as usize) {
                let opened = open_message(person.clone(), sealed.clone()).expect("opens");
                let decoded = decode_extended_message_body(opened.payload).expect("body decodes");
                store
                    .insert_incoming_message(
                        stored_message(&opened.sender_user_id, &decoded),
                        BLE_DAY_MSG_ID.to_vec(),
                        None,
                    )
                    .expect("the device that met the sender files the row");
            }

            // That device writes what it holds into its own §8 author stream.
            // Every call is local, and none of them asks whether a sibling is
            // reachable -- SYNC-1's standing constraint, as an absence.
            let payload = stores[0]
                .core_sync_history_page(
                    person.user_id.clone(),
                    contact.user_id.clone(),
                    contact.user_id.clone(),
                    0,
                    64,
                )
                .expect("history pages");
            let record = core_sign_sync_record(
                SyncRecord {
                    kind: SyncRecordKind::History,
                    person_id: person.user_id.clone(),
                    author_device_id: Vec::new(),
                    roster_version,
                    inbox_key_generation: inbox_key.generation,
                    stream_seq: stores[0]
                        .core_sync_next_stream_seq(
                            fleet[0].device_id.clone(),
                            SyncRecordKind::History,
                        )
                        .expect("stream position"),
                    timestamp_ms: NOW_MS,
                    payload: core_encode_sync_history(payload).expect("history encodes"),
                    signature: Vec::new(),
                },
                fleet[0].sign_sk.clone(),
            )
            .expect("the authoring device signs it");
            let sealed_record =
                core_seal_sync_record(record.clone(), person.clone(), inbox_key.clone())
                    .expect("SYNC-3: sealed to the person's own device set");
            stores[0]
                .core_sync_retain_record(record, sealed_record, NOW_MS)
                .expect("retained for whenever a sibling next surfaces");

            // SYNC-1, once per sibling: the sibling states the watermark it can
            // prove, the author computes what it owes with the same gap
            // function, the planner decides what fits, and each planned record
            // is opened against the own roster and applied.
            for store in stores.iter().skip(1) {
                let owed = core_sync_digest_gaps(
                    store
                        .core_sync_digest(person.user_id.clone())
                        .expect("the sibling's digest"),
                    stores[0]
                        .core_sync_digest(person.user_id.clone())
                        .expect("the author's digest"),
                )
                .expect("gaps");
                let records = stores[0]
                    .core_sync_backfill_records(owed.clone(), 64)
                    .expect("stored records");
                let offers = stores[0].core_sync_backfill_offers(records.clone());
                let plan = core_plan_sync_backfill(
                    owed,
                    offers,
                    roster_version,
                    inbox_key.generation,
                    128 * 1024,
                );
                for step in &plan.steps {
                    contract_assert!(
                        vector.id,
                        step.action == SyncBackfillAction::Send,
                        "the roster has not moved, so nothing needs re-sealing"
                    );
                    let opened = core_open_sync_record(
                        records[step.offer_index as usize].sealed.clone(),
                        inbox_key.clone(),
                        roster.clone(),
                    )
                    .expect("the sibling opens and admits it");
                    store
                        .core_apply_sync_record(opened, NOW_MS)
                        .expect("and applies it");
                }
            }

            let converged = stores.iter().all(|store| {
                store
                    .messages_for_chat(contact.user_id.clone())
                    .expect("the chat")
                    .len()
                    == 1
            });
            contract_assert!(
                vector.id,
                converged == converges_by_self_sync,
                "a fleet of {fleet_size} reached {reached_over_ble} time(s) over BLE must \
                 converge by self-sync, got converged={converged}"
            );
            Outcome::SiblingsConvergeBySelfSync
        }
        Scenario::StaleRosterSealing {
            sealed_with_inbox_key_generation,
            current_inbox_key_generation,
            ..
        } => {
            // §6's accepted residual, driven end to end now that §10.1's
            // revocation exists. The window is bounded by propagation and must
            // never become a delivery brick, so this arm asserts BOTH halves:
            // the stale roster keeps working, and the fresh one supersedes it.
            let stale_version = RosterVersion {
                recovery_epoch: 2,
                seq: 6,
            };
            let stale = roster_at(
                stale_version,
                &[OWN_DEVICE_ID, SIBLING_DEVICE_ID, b"revoked-phone-key"],
                &[],
                sealed_with_inbox_key_generation,
            );
            // The person revokes a device through the shipped §10.1 path. The
            // generation the fixture names is not restated here -- it is what
            // the rotation produces from the stale one, checked below.
            let update = cruisemesh_core::core_revoke_devices_roster(
                stale.clone(),
                person_root_sign_pk(),
                device(OWN_DEVICE_ID).sign_sk.to_vec(),
                vec![device(b"revoked-phone-key").device_id],
                cruisemesh_core::core_mint_inbox_key(sealed_with_inbox_key_generation),
            )
            .expect("the approving device revokes");
            contract_assert!(
                vector.id,
                update.roster.inbox_key_generation == current_inbox_key_generation,
                "a revocation must land on the generation this vector pins, got {}",
                update.roster.inbox_key_generation
            );

            // The months-offline contact still holds the pre-revocation roster,
            // which still vouches for the device that has since been buried.
            let store = roster_store();
            contract_assert!(
                vector.id,
                store
                    .apply_contact_roster(stale)
                    .expect("stale roster")
                    .outcome
                    == RosterUpdateOutcome::Accepted,
                "the contact's stored roster is the stale one"
            );
            contract_assert!(
                vector.id,
                store
                    .contact_device_state(person_id(), device(b"revoked-phone-key").device_id)
                    .expect("device state")
                    == ContactDeviceState::Active,
                "a stale roster still lists the revoked device -- that is the exposure"
            );

            // Sealing to that stale roster still reaches the survivors: each row
            // of the fan-out is addressed to its own device namespace, and the
            // surviving device acks its own and nobody else's (ACK-MD-1).
            let surviving_row = fanout_item(71, CoreInboundDisposition::Consumed, OWN_DEVICE_ID);
            contract_assert!(
                vector.id,
                plan_acks(surviving_row.clone()) == vec![71],
                "a surviving device must still receive and consume its own row"
            );
            // The row the stale contact addressed to the buried device is a
            // different row in a different namespace, so it is not the row a
            // survivor consumes and cannot starve one. It has no consumer left,
            // and it expires on the relay's existing 7-day clock -- the bounded
            // cost §6 accepts.
            let buried_row =
                fanout_item(72, CoreInboundDisposition::Consumed, b"revoked-phone-key");
            contract_assert!(
                vector.id,
                buried_row.recipient_hint != surviving_row.recipient_hint
                    && buried_row.msg_id != surviving_row.msg_id,
                "a per-device fan-out gives the buried device its own row, not the survivor's"
            );

            // And the window closes the moment the revocation propagates.
            contract_assert!(
                vector.id,
                store
                    .apply_contact_roster(update.roster.clone())
                    .expect("revocation gossip")
                    .outcome
                    == RosterUpdateOutcome::Accepted,
                "the revocation supersedes the stale roster when it arrives"
            );
            contract_assert!(
                vector.id,
                store
                    .contact_device_state(person_id(), device(b"revoked-phone-key").device_id)
                    .expect("device state")
                    == ContactDeviceState::Revoked,
                "once told, the contact stops vouching for the buried device"
            );
            // No wider than §6's window: a replay of the stale generation cannot
            // pull sealing back to the key the buried device still holds.
            contract_assert!(
                vector.id,
                store
                    .apply_contact_roster(roster_at(
                        RosterVersion {
                            recovery_epoch: stale_version.recovery_epoch,
                            seq: update.roster.seq + 1,
                        },
                        &[OWN_DEVICE_ID, SIBLING_DEVICE_ID],
                        &[b"revoked-phone-key"],
                        sealed_with_inbox_key_generation,
                    ))
                    .expect("regression attempt")
                    .reason
                    == RosterUpdateReason::InboxGenerationRegressed,
                "an inbox generation never goes backwards inside one recovery epoch"
            );
            Outcome::SurvivingDevicesDeliver
        }
        // §10.3, driven through the production inbound pair. Nothing here is
        // revocation-aware on the send side: every envelope is an ordinary
        // sealed 1:1 message, correctly signed, and the only thing that changes
        // between the accepted ones and the refused one is whether the roster
        // in the receiver's store has buried its author.
        Scenario::RevokedDeviceKeepsSigning {
            events_before_revocation,
            events_after_revocation,
            ..
        } => {
            let (person, kept, buried, before, after) = revoking_person();
            let me = generate_identity();
            let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
            store
                .upsert_contact(Contact {
                    user_id: person.user_id.clone(),
                    name: "Alice".to_string(),
                    sign_pk: person.sign_pk.clone(),
                    agree_pk: person.agree_pk.clone(),
                    relay_url: None,
                    relay_token: None,
                    nickname: None,
                })
                .expect("the revoking person is a contact");
            store
                .apply_contact_roster(before)
                .expect("the pre-revocation roster applies");

            for lamport in 1..=u64::from(events_before_revocation) {
                contract_assert!(
                    vector.id,
                    deliver_from_device(&store, &me, &person, &buried, lamport)
                        == CoreDeliveryVerdict::Applied,
                    "a device still vouched for must deliver"
                );
            }
            let sealed_at = store
                .messages_for_chat(person.user_id.clone())
                .expect("chat")
                .len();
            contract_assert!(
                vector.id,
                sealed_at == usize::from(events_before_revocation),
                "the pre-revocation history must be what was sent, got {sealed_at}"
            );

            store
                .apply_contact_roster(after)
                .expect("the revocation gossips");
            for lamport in 1..=u64::from(events_after_revocation) {
                contract_assert!(
                    vector.id,
                    deliver_from_device(
                        &store,
                        &me,
                        &person,
                        &buried,
                        lamport + u64::from(events_before_revocation),
                    ) == CoreDeliveryVerdict::DroppedRevokedDevice,
                    "an event newly signed by a buried device must be refused"
                );
            }
            contract_assert!(
                vector.id,
                store
                    .messages_for_chat(person.user_id.clone())
                    .expect("chat")
                    .len()
                    == sealed_at,
                "the stream seals at its last pre-revocation point; history is never rewritten"
            );
            // The person is not cut off — only the device is. A surviving
            // device of the same contact delivers as it always did.
            contract_assert!(
                vector.id,
                deliver_from_device(&store, &me, &person, &kept, 90)
                    == CoreDeliveryVerdict::Applied,
                "a revocation cuts off a device, never a person"
            );
            Outcome::RevokedSignatureRefused
        }
        // §10.4, driven through the same `apply_contact_roster` every gossip
        // and self-sync path funnels into. The facts are read back from the
        // store rather than from the classifier, because a fact a surface
        // cannot read is not a surface treatment.
        Scenario::ContactNotifiedOfRevocation {
            device_added_raises_a_fact,
            device_revoked_raises_a_fact,
        } => {
            let (person, kept, buried, before, after) = revoking_person();
            let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
            store
                .upsert_contact(Contact {
                    user_id: person.user_id.clone(),
                    name: "Alice".to_string(),
                    sign_pk: person.sign_pk.clone(),
                    agree_pk: person.agree_pk.clone(),
                    relay_url: None,
                    relay_token: None,
                    nickname: None,
                })
                .expect("the revoking person is a contact");

            // Learning a contact's fleet, and then watching it grow, is not a
            // safety change: §2 goal 1 keeps a person's device count invisible
            // to everyone else, and a warning on every ordinary link is how a
            // real warning gets swiped away unread.
            let genesis = cruisemesh_core::core_link_genesis_roster(
                person.sign_sk.clone(),
                kept.sign_pk.clone(),
                kept.agree_pk.clone(),
            )
            .expect("genesis");
            store.apply_contact_roster(genesis).expect("first roster");
            store
                .apply_contact_roster(before)
                .expect("a second device is linked");
            let after_add = store.contact_safety_facts(true).expect("facts");
            contract_assert!(
                vector.id,
                after_add.len() == usize::from(device_added_raises_a_fact),
                "an added device must raise {} fact(s), got {}",
                usize::from(device_added_raises_a_fact),
                after_add.len()
            );

            store.apply_contact_roster(after).expect("the revocation");
            let facts = store.contact_safety_facts(false).expect("facts");
            contract_assert!(
                vector.id,
                (facts.len() == 1) == device_revoked_raises_a_fact,
                "a revocation must raise exactly one unacknowledged fact, got {}",
                facts.len()
            );
            contract_assert!(
                vector.id,
                facts[0].reason == ContactSafetyReason::DeviceRevoked
                    && facts[0].person_user_id == person.user_id
                    && facts[0].device_ids == vec![buried.device_id.clone()],
                "the fact must name the revocation and the device it buried, got {:?}",
                facts[0]
            );
            // Acknowledgement clears the notification and nothing else: DL-4's
            // tombstone stands, and §10.3's refusal is unaffected.
            contract_assert!(
                vector.id,
                store
                    .acknowledge_contact_safety_facts(person.user_id.clone(), facts[0].observed_seq)
                    .expect("acknowledge")
                    == 1
                    && store.contact_safety_facts(false).expect("facts").is_empty()
                    && store
                        .contact_device_state(person.user_id.clone(), buried.device_id.clone())
                        .expect("device state")
                        == ContactDeviceState::Revoked,
                "acknowledging a warning must not un-bury a device"
            );
            Outcome::ContactSafetyStateRaised
        }
        // DL-3, both halves, driven now that a roster has a carrier: the
        // document is sealed pairwise to one contact, and nothing outside that
        // seal — no relay, no directory, no third party — can read it.
        //
        // The plaintext-absence check below is the one that matters. A roster
        // travelling as an ordinary envelope means the relay stores bytes it
        // cannot interpret, and the honest way to assert that is to look for
        // the document *in* the frame the relay would hold and find none of it.
        Scenario::RosterPairwiseGossipNoDirectory {
            version,
            sealed_pairwise,
            uses_directory,
        } => {
            let alice = generate_identity();
            let bob = generate_identity();
            let carol = generate_identity();
            let fleet = [generate_device_keypair(), generate_device_keypair()];
            let roster = own_fleet_roster_at(&alice, &fleet, version);

            let alice_store = MessageStore::open(":memory:".to_string()).expect("alice");
            alice_store
                .upsert_contact(person_contact(&bob, "Bob"))
                .expect("bob is a contact");
            alice_store
                .adopt_own_roster(
                    roster.clone(),
                    alice.sign_pk.clone(),
                    fleet[0].device_id.clone(),
                )
                .expect("alice adopts her own roster");
            let announcement = alice_store
                .announce_own_roster(alice.clone(), NOW_MS)
                .expect("gossip");
            let envelope = announcement
                .envelopes
                .first()
                .cloned()
                .expect("one sealed copy per contact");

            let document = cruisemesh_core::core_encode_roster(roster.clone()).expect("document");
            contract_assert!(
                vector.id,
                !envelope
                    .frame
                    .windows(document.len())
                    .any(|window| window == document.as_slice()),
                "the document a relay would store must not contain the roster in the clear"
            );
            contract_assert!(
                vector.id,
                sealed_pairwise
                    && open_message(bob.clone(), envelope.envelope.sealed.clone()).is_ok()
                    && open_message(carol, envelope.envelope.sealed.clone()).is_err(),
                "a gossiped roster opens for its one recipient and for nobody else"
            );

            // No directory: the receiving store's only inputs are the frame and
            // the contact row it already had. Nothing is looked up anywhere.
            let bob_store = MessageStore::open(":memory:".to_string()).expect("bob");
            bob_store
                .upsert_contact(person_contact(&alice, "Alice"))
                .expect("alice is a contact");
            contract_assert!(
                vector.id,
                !uses_directory
                    && deliver_frame(&bob_store, &bob, envelope.frame.clone())
                        == CoreDeliveryVerdict::Applied
                    && bob_store
                        .contact_roster_state(alice.user_id.clone())
                        .expect("state")
                        .roster
                        == Some(roster),
                "a contact learns a roster from the envelope alone"
            );
            Outcome::PairwiseGossipNoDirectory
        }
        // §9 step 5, driven end to end. Nothing below is a stand-in: the
        // document is authored by the shipped announcement, travels as a real
        // sealed envelope frame, and is applied by the production inbound pair
        // on a second store that has never been told anything about the
        // sender's devices.
        //
        // The vector's third field is the cost this closes, so it is asserted
        // where it is paid — BEFORE the gossip lands, on the sending contact's
        // own relay plan — rather than described.
        Scenario::RosterGossipToContacts {
            fleet_size_after_link,
            roster_reaches_contacts,
            person_addressed_rows_churn_until_expiry,
        } => {
            let alice = generate_identity();
            let bob = generate_identity();
            let alice_store = MessageStore::open(":memory:".to_string()).expect("alice");
            let bob_store = MessageStore::open(":memory:".to_string()).expect("bob");
            alice_store
                .upsert_contact(person_contact(&bob, "Bob"))
                .expect("bob is alice's contact");
            bob_store
                .upsert_contact(person_contact(&alice, "Alice"))
                .expect("alice is bob's contact");

            // §9: Alice links a second device, so her fleet is larger than one.
            let phone = generate_device_keypair();
            let tablet = generate_device_keypair();
            let genesis = cruisemesh_core::core_link_genesis_roster(
                alice.sign_sk.clone(),
                phone.sign_pk.clone(),
                phone.agree_pk.clone(),
            )
            .expect("genesis");
            let linked = cruisemesh_core::core_link_sign_new_device_roster(
                genesis,
                alice.sign_pk.clone(),
                phone.sign_sk.clone(),
                tablet.sign_pk.clone(),
                tablet.agree_pk.clone(),
            )
            .expect("the approving device signs the new roster")
            .roster;
            contract_assert!(
                vector.id,
                linked.devices.len() == usize::from(fleet_size_after_link),
                "the linked fleet must be {fleet_size_after_link} devices, got {}",
                linked.devices.len()
            );
            alice_store
                .adopt_own_roster(linked, alice.sign_pk.clone(), phone.device_id.clone())
                .expect("alice adopts her own roster");

            // Before the roster reaches Bob, his mail to Alice is ONE
            // person-addressed row — and ACK-MD-2 forbids her fleet from
            // deleting it, so it churns until the relay's 7-day expiry.
            let mail = bob_store
                .author_pairwise_message(
                    bob.clone(),
                    person_contact(&alice, "Alice"),
                    KIND_TEXT,
                    b"we're at the pool".to_vec(),
                    None,
                    NOW_MS,
                )
                .expect("bob writes to alice");
            let uninformed = bob_store
                .core_outbound_relay_rows(mail.envelope.clone(), bob.user_id.clone(), None)
                .expect("relay plan");
            contract_assert!(
                vector.id,
                person_addressed_rows_churn_until_expiry
                    && uninformed.len() == 1
                    && uninformed[0].recipient_hint == mail.envelope.recipient_hint,
                "a contact who has not heard the roster uploads one person-addressed row, \
                 got {} row(s)",
                uninformed.len()
            );
            contract_assert!(
                vector.id,
                alice_store
                    .core_relay_ack_ids_with_consumed(
                        vec![CoreRelayEnvelopeDisposition {
                            relay_id: 91,
                            msg_id: mail.envelope.msg_id.clone(),
                            disposition: CoreInboundDisposition::Consumed,
                            recipient_hint: uninformed[0].recipient_hint.clone(),
                        }],
                        alice.user_id.clone(),
                        NOW_MS,
                    )
                    .expect("ack planner")
                    .is_empty(),
                "ACK-MD-2: a multi-device fleet never deletes the person's one shared row"
            );

            // The carrier. One sealed envelope per contact owed the document.
            let announcement = alice_store
                .announce_own_roster(alice.clone(), NOW_MS)
                .expect("alice tells her contacts");
            contract_assert!(
                vector.id,
                announcement.envelopes.len() == 1
                    && announcement.envelopes[0].message.chat_id == bob.user_id,
                "exactly the contacts owed the roster are told, got {} envelope(s)",
                announcement.envelopes.len()
            );

            // Bob receives it the way he receives everything else.
            contract_assert!(
                vector.id,
                deliver_frame(&bob_store, &bob, announcement.envelopes[0].frame.clone())
                    == CoreDeliveryVerdict::Applied,
                "a contact's roster must be applied"
            );
            contract_assert!(
                vector.id,
                roster_reaches_contacts
                    && bob_store
                        .contact_active_device_ids(alice.user_id.clone())
                        .expect("alice's devices")
                        == vec![phone.device_id.clone(), tablet.device_id.clone()],
                "the contact now holds the person's actual fleet"
            );
            // ACK-MD-1's evidence: a roster leaves no `messages` row of its
            // own, so without this its relay copy is refetched for seven days.
            contract_assert!(
                vector.id,
                bob_store
                    .consumed_hidden_msg_id_recorded(
                        announcement.envelopes[0].envelope.msg_id.clone(),
                        NOW_MS
                    )
                    .expect("evidence"),
                "an applied roster must vouch for the relay row it arrived on"
            );

            // And the churn window closes: the same message now plans one row
            // per device, each in that device's own namespace, each with
            // exactly one true consumer.
            let informed = bob_store
                .core_outbound_relay_rows(mail.envelope.clone(), bob.user_id.clone(), None)
                .expect("relay plan");
            contract_assert!(
                vector.id,
                informed.len() == usize::from(fleet_size_after_link),
                "an informed contact fans out per device, got {} row(s)",
                informed.len()
            );
            for (row, device) in informed.iter().zip([&phone, &tablet]) {
                contract_assert!(
                    vector.id,
                    row.msg_id
                        == device_fanout_msg_id(
                            mail.envelope.msg_id.clone(),
                            device.device_id.clone()
                        )
                        && row.recipient_hint
                            == compute_recipient_hint(
                                core_device_namespace_id(
                                    alice.user_id.clone(),
                                    device.device_id.clone()
                                ),
                                mail.envelope.timestamp,
                            )
                        && row.recipient_hint != mail.envelope.recipient_hint,
                    "each row is addressed to one device's own namespace"
                );
            }
            contract_assert!(
                vector.id,
                alice_store
                    .core_relay_ack_ids_with_consumed(
                        vec![CoreRelayEnvelopeDisposition {
                            relay_id: 92,
                            msg_id: informed[0].msg_id.clone(),
                            disposition: CoreInboundDisposition::Consumed,
                            recipient_hint: informed[0].recipient_hint.clone(),
                        }],
                        alice.user_id.clone(),
                        NOW_MS,
                    )
                    .expect("ack planner")
                    == vec![92],
                "ACK-MD-1: once told, each row has one consumer and is deleted by it"
            );

            // Idempotent gossip, the send-side twin of DL-1: a contact who
            // already holds this head is told nothing on the next pass.
            contract_assert!(
                vector.id,
                alice_store
                    .announce_own_roster(alice, NOW_MS + 60_000)
                    .expect("second pass")
                    .envelopes
                    .is_empty(),
                "a contact already holding the roster is not told again"
            );
            Outcome::ContactsLearnTheRoster
        }
        // §10 step 5, driven on the real store through the real frame: the
        // approver's roster is encoded with the DL-3 wire codec, framed,
        // parsed back, and handed to the buried device's own store. Nothing
        // here is revocation-aware except the document.
        Scenario::RevokedDeviceMeetsTheFleet {
            notice_is_person_signed,
        } => {
            let (person, _kept, buried, before, after) = revoking_person();
            let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
            store
                .adopt_own_roster(
                    before.clone(),
                    person.sign_pk.clone(),
                    buried.device_id.clone(),
                )
                .expect("the buried device holds the pre-revocation roster");
            contract_assert!(
                vector.id,
                store
                    .link_gate(CoreLinkGatedAction::Advertise)
                    .expect("gate")
                    .allowed,
                "before the news reaches it, a removed device behaves exactly as it always did"
            );
            contract_assert!(
                vector.id,
                store.own_device_fleet().expect("fleet").device_ids.len() == 2,
                "and it still projects the fleet it was in"
            );

            let announced = if notice_is_person_signed {
                after.clone()
            } else {
                // The best a forger can do: this person's real document, over a
                // signature that is somebody else's.
                let stranger = generate_identity();
                core_sign_roster(after.clone(), stranger.sign_sk).expect("forged roster signs")
            };
            let frame = encode_own_roster(core_encode_roster(announced).expect("roster encodes"))
                .expect("notice frames");
            let document = match parse_frame(frame).expect("notice parses") {
                Frame::OwnRoster { document } => document,
                other => unreachable!("own roster notice parsed as {other:?}"),
            };
            // DL-5 on the bytes that really cross the link, not on the type.
            for needle in [
                b"http".as_slice(),
                b"192.168".as_slice(),
                b"relay".as_slice(),
            ] {
                contract_assert!(
                    vector.id,
                    !document
                        .windows(needle.len())
                        .any(|window| window == needle),
                    "an own-roster notice carries keys, ids and counters -- never an address"
                );
            }
            let adoption = store
                .apply_own_roster_notice(
                    document,
                    person.sign_pk.clone(),
                    buried.device_id.clone(),
                    NOW_MS,
                )
                .expect("the notice is judged");

            if !notice_is_person_signed {
                contract_assert!(
                    vector.id,
                    adoption.outcome == RevocationAdoptionOutcome::Refused,
                    "a roster the person root did not sign buries nobody"
                );
                contract_assert!(
                    vector.id,
                    store.own_roster().expect("roster").expect("held") == before
                        && store.own_device_fleet().expect("fleet").device_ids.len() == 2
                        && store
                            .link_gate(CoreLinkGatedAction::Advertise)
                            .expect("gate")
                            .allowed,
                    "and it changes nothing at all"
                );
                Outcome::Ignored
            } else {
                contract_assert!(
                    vector.id,
                    adoption.outcome == RevocationAdoptionOutcome::RevokedSelf
                        && adoption.revoked_device_ids == vec![buried.device_id.clone()],
                    "the buried device recognises itself in the document"
                );
                contract_assert!(
                    vector.id,
                    store.own_roster().expect("roster").expect("held") == after,
                    "it stores the roster that removed it, so a replay is idempotent"
                );
                contract_assert!(
                    vector.id,
                    store.own_device_fleet().expect("fleet")
                        == OwnDeviceFleet {
                            own_device_id: None,
                            device_ids: Vec::new(),
                            projected_from: after.version(),
                        },
                    "the projection routing and acks read is cleared"
                );
                contract_assert!(
                    vector.id,
                    store.link_activation().expect("activation").stage
                        == CoreLinkActivationStage::Revoked,
                    "and the stage a shell surfaces says so"
                );
                for action in [
                    CoreLinkGatedAction::Advertise,
                    CoreLinkGatedAction::Author,
                    CoreLinkGatedAction::Ack,
                ] {
                    let verdict = store.link_gate(action).expect("gate");
                    contract_assert!(
                        vector.id,
                        !verdict.allowed && verdict.reason == CoreLinkGateReason::DeviceRevoked,
                        "a removed device may not {action:?}"
                    );
                }
                Outcome::RevokedDeviceEjectsItself
            }
        }
        // Still data-only, and each for a mechanism reason rather than a shrug:
        // first-contact anchoring has no second source of truth to check an
        // adopted epoch against, and root-secret custody needs the person root
        // minted and stored separately from the identity key. There is nothing
        // real to execute.
        Scenario::RosterFirstContactAnchor { .. } | Scenario::RecoveryRootCustody { .. } => {
            unreachable!("unimplemented vector ran")
        }
    };
    Some(outcome)
}

/// Map the shipped roster verdict onto this file's vocabulary. A driver arm
/// returns what core decided, never what the vector hoped for.
fn roster_outcome(outcome: RosterUpdateOutcome) -> Outcome {
    match outcome {
        RosterUpdateOutcome::Accepted => Outcome::Accepted,
        RosterUpdateOutcome::Ignored => Outcome::Ignored,
        RosterUpdateOutcome::ForkQuarantined => Outcome::ForkQuarantined,
    }
}

/// Every vector's target outcome is pinned exactly once, in order. Flipping
/// any `target_outcome` -- data-only vectors included -- fails here.
#[test]
fn every_vector_target_outcome_is_pinned() {
    let actual: Vec<(&str, Outcome)> = VECTORS
        .iter()
        .map(|vector| (vector.id, vector.target_outcome))
        .collect();
    let expected: Vec<(&str, Outcome)> = PINNED_TARGETS.to_vec();
    assert_eq!(
        actual, expected,
        "the accepted v1 rules are a pinned table; changing a target outcome is a spec decision"
    );
}

#[test]
fn roster_and_cap_data_encode_the_accepted_rules() {
    for vector in VECTORS {
        match vector.scenario {
            Scenario::RosterUpdate { stored, incoming } if vector.id == "MD-ROSTER-GREATER" => {
                contract_assert!(
                    vector.id,
                    (stored.recovery_epoch, stored.seq) == (1, 1000)
                        && (incoming.recovery_epoch, incoming.seq) == (2, 0),
                    "DL-1 recovery-epoch supersession fixture changed"
                );
            }
            Scenario::RosterUpdate { stored, incoming } if vector.id == "MD-ROSTER-LOWER" => {
                contract_assert!(
                    vector.id,
                    (stored.recovery_epoch, stored.seq) == (2, 0)
                        && (incoming.recovery_epoch, incoming.seq) == (1, 1001),
                    "DL-1 rollback fixture changed"
                );
            }
            Scenario::RosterUpdate { stored, incoming } if vector.id == "MD-ROSTER-EQUAL" => {
                contract_assert!(
                    vector.id,
                    stored == incoming && (stored.recovery_epoch, stored.seq) == (2, 0),
                    "DL-1 equal-version fixture changed"
                );
            }
            Scenario::RosterUpdate { stored, incoming }
                if vector.id == "MD-ROSTER-SAME-EPOCH-ADVANCE" =>
            {
                contract_assert!(
                    vector.id,
                    stored.recovery_epoch == incoming.recovery_epoch && incoming.seq > stored.seq,
                    "DL-1 same-epoch advance must keep the epoch and raise the seq"
                );
            }
            Scenario::RosterUpdate { stored, incoming }
                if vector.id == "MD-ROSTER-SAME-EPOCH-ROLLBACK" =>
            {
                contract_assert!(
                    vector.id,
                    stored.recovery_epoch == incoming.recovery_epoch
                        && incoming.seq < stored.seq
                        && (stored.seq, incoming.seq) == (5, 3),
                    "DL-1 same-epoch rollback fixture changed"
                );
            }
            Scenario::RosterSignatureChain {
                stored,
                incoming,
                chain_verifies_to_person_root,
            } => contract_assert!(
                vector.id,
                (incoming.recovery_epoch, incoming.seq) > (stored.recovery_epoch, stored.seq)
                    && !chain_verifies_to_person_root,
                "DL-1 chain vector must be strictly higher AND fail to verify"
            ),
            Scenario::RosterFork {
                version,
                stored_content,
                incoming_content,
            } => contract_assert!(
                vector.id,
                (version.recovery_epoch, version.seq) == (2, 0)
                    && stored_content != incoming_content,
                "DL-2 requires equal version and different content"
            ),
            Scenario::RosterUpdateAfterFork {
                quarantined_at,
                incoming,
                incoming_is_strictly_higher,
                auto_resolves,
            } => contract_assert!(
                vector.id,
                (quarantined_at.recovery_epoch, quarantined_at.seq) == (2, 0)
                    && (incoming.recovery_epoch, incoming.seq) == (2, 1)
                    && incoming_is_strictly_higher
                    && !auto_resolves,
                "DL-2 quarantine must survive a legitimately higher roster"
            ),
            Scenario::RosterForkPiercedByRoot {
                quarantined_at,
                incoming,
                signed_by_person_root,
            } => contract_assert!(
                vector.id,
                (quarantined_at.recovery_epoch, quarantined_at.seq) == (2, 0)
                    && incoming.recovery_epoch > quarantined_at.recovery_epoch
                    && signed_by_person_root,
                "the pierce vector is a ROOT-signed document at a strictly higher epoch"
            ),
            Scenario::RosterTombstonedDeviceReturns {
                version,
                tombstoned_device_id,
                returning_device_id,
            } => contract_assert!(
                vector.id,
                (version.recovery_epoch, version.seq) == (2, 1)
                    && tombstoned_device_id == returning_device_id,
                "DL-4 tombstoned device_id fixture changed"
            ),
            Scenario::RosterRelinkFreshKey {
                version,
                tombstoned_device_id,
                replacement_device_id,
            } => contract_assert!(
                vector.id,
                (version.recovery_epoch, version.seq) == (2, 2)
                    && tombstoned_device_id != replacement_device_id,
                "DL-4 re-link must mint a fresh device key"
            ),
            Scenario::RosterPairwiseGossipNoDirectory {
                version,
                sealed_pairwise,
                uses_directory,
            } => contract_assert!(
                vector.id,
                (version.recovery_epoch, version.seq) == (2, 3)
                    && sealed_pairwise
                    && !uses_directory,
                "DL-3 roster gossip is pairwise-sealed and directory-free"
            ),
            Scenario::RosterKeysNeverEndpoints { version, document } => {
                // `RosterDocument` has device keys and tombstones and no field
                // an endpoint could live in -- but it is a fixture, and the
                // real `Roster` is all `Vec<u8>`. The enforced invariant is
                // the one the driver exercises: `core_roster_validate`'s
                // fixed-width check is the single gate, and it is what keeps a
                // free-form address out. The data here pins the fixture's
                // shape; the driver pins the gate, including its limit.
                contract_assert!(
                    vector.id,
                    (version.recovery_epoch, version.seq) == (2, 4)
                        && !document.device_keys.is_empty()
                        && !document.tombstones.is_empty()
                        && document
                            .device_keys
                            .iter()
                            .all(|key| !document.tombstones.contains(key)),
                    "DL-5 roster fixture must list device keys and tombstones, disjointly"
                );
            }
            Scenario::RosterFirstContactAnchor {
                adopted,
                stored_baseline_exists,
                signed_by_approving_device,
            } => contract_assert!(
                vector.id,
                !stored_baseline_exists && signed_by_approving_device && adopted.recovery_epoch > 0,
                "the first-contact vector is an adoption with no baseline, at a raised epoch"
            ),
            Scenario::RecoveryEpochRequiresRoot {
                version,
                device_key_alone_can_mint_higher_epoch,
            } => contract_assert!(
                vector.id,
                (version.recovery_epoch, version.seq) == (3, 0)
                    && !device_key_alone_can_mint_higher_epoch,
                "§14.2 epoch-rule fixture changed"
            ),
            Scenario::RecoveryRootCustody {
                root_secret_only_in_encrypted_backup,
                device_keypair_can_carry_root_secret,
            } => contract_assert!(
                vector.id,
                root_secret_only_in_encrypted_backup && !device_keypair_can_carry_root_secret,
                "§3 / §14.2 root-secret custody fixture changed"
            ),
            Scenario::LegacyEnvelopeWithoutDevice { legacy_device_id } => contract_assert!(
                vector.id,
                legacy_device_id == [0u8; 16],
                "§5 reserves the all-zero LEGACY_DEVICE_ID for device-less envelopes"
            ),
            Scenario::StaleRosterSealing {
                revoked_device_still_listed,
                sealed_with_inbox_key_generation,
                current_inbox_key_generation,
                surviving_devices_still_receive,
            } => contract_assert!(
                vector.id,
                revoked_device_still_listed
                    && sealed_with_inbox_key_generation < current_inbox_key_generation
                    && surviving_devices_still_receive,
                "§6 stale-roster sealing is a bounded exposure, never a delivery brick"
            ),
            Scenario::RevokedDeviceKeepsSigning {
                events_before_revocation,
                events_after_revocation,
                stored_history_survives,
            } => contract_assert!(
                vector.id,
                events_before_revocation > 0
                    && events_after_revocation > 0
                    && stored_history_survives,
                "§10.3's vector must send on BOTH sides of the burial and keep what came first"
            ),
            Scenario::ContactNotifiedOfRevocation {
                device_added_raises_a_fact,
                device_revoked_raises_a_fact,
            } => contract_assert!(
                vector.id,
                !device_added_raises_a_fact && device_revoked_raises_a_fact,
                "§10.4 tells a contact what was taken away, never how many devices somebody has"
            ),
            Scenario::RosterGossipToContacts {
                fleet_size_after_link,
                roster_reaches_contacts,
                person_addressed_rows_churn_until_expiry,
            } => contract_assert!(
                vector.id,
                fleet_size_after_link > 1
                    && roster_reaches_contacts
                    && person_addressed_rows_churn_until_expiry,
                "§9 step 5's vector is a fleet of more than one whose contacts must be told,                  and whose person-addressed rows churn until they are"
            ),
            Scenario::BleOnlyDayConverges {
                reached_over_ble,
                fleet_size,
                converges_by_self_sync,
            } => contract_assert!(
                vector.id,
                reached_over_ble < fleet_size && converges_by_self_sync,
                "§8's BLE-day vector is a fleet only partly reached, converging by self-sync"
            ),
            Scenario::AddDevice {
                resulting_device_count,
            } => {
                // §14.3, boundary as pinned by the 2026-08-16 decision: the
                // 8th device does not warn, the 9th does, 16 is allowed, the
                // 17th is refused.
                let expected = match resulting_device_count {
                    7 | 8 => Outcome::DeviceAdded,
                    9 | 16 => Outcome::DeviceAddedWithWarning,
                    17 => Outcome::DeviceAddRefused,
                    _ => unreachable!("unexpected §14.3 cap fixture"),
                };
                contract_assert!(
                    vector.id,
                    vector.target_outcome == expected,
                    "§14.3 soft-8/hard-16 add-device outcome changed"
                );
            }
            _ => {}
        }
    }
}

#[test]
fn vectors_match_the_pinned_current_core_behaviour() {
    let actual: Vec<(&str, Outcome)> = VECTORS
        .iter()
        .filter_map(|vector| drive(vector).map(|current| (vector.id, current)))
        .collect();
    let expected: Vec<(&str, Outcome)> = PINNED_DRIVER_RESULTS.to_vec();
    assert_eq!(
        actual, expected,
        "today's driven core behaviour is pinned; a changed driver result needs a deliberate edit"
    );
}

#[test]
fn divergence_ledger_is_derived_from_driver_results() {
    let actual: Vec<_> = VECTORS
        .iter()
        .filter_map(|vector| {
            drive(vector)
                .and_then(|current| (current != vector.target_outcome).then_some(vector.id))
        })
        .collect();
    // WP1 closed two of these; WP2 closed two more, and both because acks are
    // now planned per device namespace rather than per person:
    // `MD-ACK-SIBLING-FANOUT` (a consumed sibling row is refused by namespace,
    // not by disposition) and `MD-ACK-LEGACY-PERSON-ROW` (ACK-MD-2).
    //
    // One is left, and it is not WP2's to close. `MD-ACK-CARRIED-DIGEST-PROOF`
    // diverges on the *person* half of ACK-MD-3: a carried copy is already
    // removed only on digest proof, never on dispatch, which is
    // `DigestProofOnly` -- but v1 wants proof that reaches the PERSON, and one
    // device's digest cannot speak for its siblings. Closing it needs a
    // person-scoped receipt, which needs self-sync (§8, WP4). WP2 deliberately
    // changed nothing about carried copies, so nothing here moved.
    let expected = ["MD-ACK-CARRIED-DIGEST-PROOF"];
    assert_eq!(
        actual, expected,
        "implemented current-core divergences must be discovered from drivers"
    );
}

#[test]
fn unimplemented_vector_ledger_is_deliberate() {
    let actual: Vec<_> = VECTORS
        .iter()
        .filter(|vector| !vector.implemented)
        .map(|vector| vector.id)
        .collect();
    // Two now. `MD-SYNC-BLE-DAY-CONVERGE` left this list when WP4's SYNC-1
    // anti-entropy gave it something real to drive; §6's stale-roster sealing
    // left with WP5's rotation; and WP6's carrier took the two DL-3 vectors
    // together, which is the removal that needs stating rather than assuming.
    //
    // Both left for the one reason that had kept them here: no envelope kind
    // carried a roster document to a CONTACT. WP4's self-sync carried rosters
    // between one person's own devices and WP5 produced the document and the
    // recipient list, but neither put a roster on a wire pointed at somebody
    // else -- so `MD-ROSTER-PAIRWISE-GOSSIP` had no sealed envelope whose
    // sealing it could check, and `MD-ROSTER-GOSSIP-TO-CONTACTS` had no
    // delivery to drive. WP6 built `KIND_ROSTER_GOSSIP` and the announcement
    // that authors one per contact, so both drivers now run the real path
    // across two stores: one authors, the other applies through the production
    // inbound pair, and the ACK-MD-2 churn the second vector's fixture names is
    // asserted on the sending contact's own relay plan before and after.
    //
    // The two that remain each wait on a mechanism rather than on effort:
    // first-contact anchoring needs a second source of truth about a person's
    // epoch, which WP5's recovery flow does not supply -- the override proves
    // who may climb an epoch, not where a stranger's epoch had already got to;
    // root-secret custody needs the person root minted and stored separately
    // from the identity key. Driving either today would mean writing a
    // test-only stand-in and calling it core, which is the one thing this
    // ledger exists to prevent.
    let expected = ["MD-ROSTER-FIRST-CONTACT-ANCHOR", "MD-RECOVERY-ROOT-CUSTODY"];
    assert_eq!(
        actual, expected,
        "the data-only WP0 vectors are a pinned ledger"
    );
}
