//! The §13 WP5 gate, driven across separate stores
//! (`specs/multi-device-v1.md` §10, §13).
//!
//! Each module in `core/src` pins its own half of revocation: `revocation.rs`
//! owns the roster arithmetic and the sibling handoff, `contact_safety.rs` owns
//! §10.4's facts, `session/mesh_receive.rs` owns §10.3's refusal, and
//! `relay_rotation.rs` owns §10.2's credential ceremony. What none of them can
//! show, because each holds one store, is the thing the gate actually asks for:
//! **a mixed fleet**, where the person doing the revoking, the sibling that
//! survives it, and a contact who has been offline for months are three
//! different devices with three different views, and the revocation has to be
//! correct in all of them at once.
//!
//! The gate, verbatim from §13, and where each half is proved:
//!
//! * *"a months-offline contact sealing to a stale roster still delivers to
//!   survivors"* — [`a_months_offline_contact_still_reaches_the_survivors`]
//!   here, end to end through the production inbound pair, and
//!   `MD-SEAL-STALE-ROSTER` in `multi_device_contract.rs` for the fan-out
//!   addressing and ACK-MD-1 half of the same window.
//! * *"a revoked device demonstrably loses relay fetch after rotation"* — the
//!   client ceremony is [`the_rotation_retires_the_credential_the_thief_kept`]
//!   here; the server half, which is the one that actually cuts the thief off,
//!   is `relayd`'s
//!   `rotation_cuts_the_old_credential_off_without_losing_a_single_row`. It
//!   lives there because only the server can prove a fetch and an ack are
//!   refused, and only the server can prove no sibling lost an un-fetched row.
//! * *"recovery-epoch override dethrones a stolen approving device"* —
//!   [`the_recovery_epoch_dethrones_a_stolen_approving_device_for_a_contact`],
//!   seen from the CONTACT's store, which is where dethroning either works or
//!   does not: the thief's rosters and the thief's mail both have to stop
//!   counting on somebody else's phone.
//!
//! The threat model throughout is §10's: the revoked device is hostile, holds
//! the old inbox key and the old relay credential, and replays everything it
//! ever saw. Nothing below asks it to cooperate.

use std::sync::Arc;

use cruisemesh_core::{
    compute_recipient_hint, core_link_genesis_roster, core_link_sign_new_device_roster,
    core_mint_inbox_key, core_plan_relay_rotation, core_recovery_revoke_roster,
    core_revoke_devices_roster, encode_envelope_frame, encode_message_body_extended,
    generate_device_keypair, generate_identity, generate_msg_id, relay_deposit_token_for,
    relay_token_is_deposit, seal_message, Contact, ContactDeviceState, ContactSafetyReason,
    CoreDeliveryVerdict, CoreDiscoveryPolicyState, CoreInboundSource, DeviceKeypair, Identity,
    InboxKey, MessageArrival, MessageBody, MessageStore, RevocationAdoptionOutcome, Roster,
    RosterUpdateOutcome, RosterUpdateReason, SeenIds, DEFAULT_HOP_TTL, KIND_TEXT, MS_PER_DAY,
};

const NOW: i64 = 1_755_000_000_000;

/// §10.1 as a shell really performs it (see
/// `MessageStore::commit_own_revocation`'s durable-key precondition): begin,
/// which hands the rotated key over for platform storage, and only then
/// commit. The whole point of the split is that no backlog is ever re-sealed
/// to a key that might not have survived the process, so every test here goes
/// through it rather than around it.
fn commit_revocation(
    store: &MessageStore,
    fleet: &Fleet,
    update: &cruisemesh_core::RevocationUpdate,
    own: &DeviceKeypair,
    superseded: Option<InboxKey>,
) -> cruisemesh_core::RevocationCommit {
    let key = store
        .begin_own_revocation(
            update.clone(),
            fleet.person.sign_pk.clone(),
            own.clone(),
            NOW,
        )
        .expect("begin");
    assert_eq!(key, update.inbox_key);
    store
        .commit_own_revocation(
            update.clone(),
            fleet.person.sign_pk.clone(),
            own.clone(),
            superseded,
            NOW,
        )
        .expect("commit")
}

// ---------------------------------------------------------------------------
// A person, their fleet, and the ordinary send path
// ---------------------------------------------------------------------------

/// One person with three linked devices, built through the shipped WP3
/// ceremony: the approving device, a sibling that will survive, and the phone
/// that gets stolen.
struct Fleet {
    person: Identity,
    approver: DeviceKeypair,
    sibling: DeviceKeypair,
    stolen: DeviceKeypair,
    roster: Roster,
    inbox_key: InboxKey,
}

fn fleet() -> Fleet {
    let person = generate_identity();
    let approver = generate_device_keypair();
    let sibling = generate_device_keypair();
    let stolen = generate_device_keypair();
    let genesis = core_link_genesis_roster(
        person.sign_sk.clone(),
        approver.sign_pk.clone(),
        approver.agree_pk.clone(),
    )
    .expect("genesis");
    let mut roster = genesis;
    for device in [&sibling, &stolen] {
        roster = core_link_sign_new_device_roster(
            roster,
            person.sign_pk.clone(),
            approver.sign_sk.clone(),
            device.sign_pk.clone(),
            device.agree_pk.clone(),
        )
        .expect("link")
        .roster;
    }
    let inbox_key = core_mint_inbox_key(roster.inbox_key_generation);
    Fleet {
        person,
        approver,
        sibling,
        stolen,
        roster,
        inbox_key,
    }
}

fn contact_row(identity: &Identity, name: &str) -> Contact {
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

fn arrival() -> MessageArrival {
    MessageArrival {
        transport: 3,
        hops_taken: 0,
        received_at: NOW,
    }
}

fn discovery() -> CoreDiscoveryPolicyState {
    CoreDiscoveryPolicyState {
        enabled: true,
        revision: 0,
    }
}

/// Send one ordinary 1:1 message from `sender`'s `device` to `me`'s store,
/// through the production inbound pair. Nothing here is revocation-aware: it
/// is the same path every message in the field takes.
fn deliver(
    store: &MessageStore,
    me: &Identity,
    sender: &Identity,
    device: Option<&DeviceKeypair>,
    lamport: u64,
    text: &str,
) -> CoreDeliveryVerdict {
    let payload = encode_message_body_extended(
        MessageBody {
            kind: KIND_TEXT,
            chat_id: sender.user_id.clone(),
            lamport,
            timestamp: NOW,
            content: text.as_bytes().to_vec(),
        },
        None,
        device.map(|device| device.device_id.clone()),
        None,
    )
    .expect("encode body");
    let sealed = seal_message(sender.clone(), me.agree_pk.clone(), payload).expect("seal");
    let frame = encode_envelope_frame(
        generate_msg_id(),
        DEFAULT_HOP_TTL,
        NOW + 7 * MS_PER_DAY,
        compute_recipient_hint(me.user_id.clone(), NOW),
        sealed,
    );
    let outcome = store
        .process_inbound_frame(
            me.clone(),
            Arc::new(SeenIds::new()),
            CoreInboundSource::Mesh,
            frame,
            NOW,
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
            arrival(),
            discovery(),
        )
        .expect("delivery")
        .verdict
}

/// A device of `fleet`, with its own store, holding the fleet's roster and
/// sync context — what §9's activation leaves behind.
fn device_store(fleet: &Fleet, own: &DeviceKeypair) -> MessageStore {
    let store = MessageStore::open(":memory:".to_string()).expect("open");
    store
        .adopt_own_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            own.device_id.clone(),
        )
        .expect("adopt");
    store
        .core_set_own_sync_context(fleet.roster.clone(), fleet.roster.inbox_key_generation)
        .expect("sync context");
    store
}

// ---------------------------------------------------------------------------
// §13 gate: the months-offline contact
// ---------------------------------------------------------------------------

/// **A contact who has been offline for months is never bricked by somebody
/// else's revocation, and the survivors keep receiving their mail.**
///
/// This is the availability half of §6's accepted window, and it is the half
/// that is easy to lose: every mechanism in §10 exists to stop a thief, and a
/// rule written one notch too wide stops a grandmother's phone instead. So the
/// stale contact here does nothing right — they hold a roster from before the
/// revocation, they know nothing about the rotation, and they never update —
/// and their mail still lands.
#[test]
fn a_months_offline_contact_still_reaches_the_survivors() {
    let fleet = fleet();
    let bob = generate_identity();
    let sibling_store = device_store(&fleet, &fleet.sibling);
    sibling_store
        .upsert_contact(contact_row(&bob, "Bob"))
        .expect("contact");

    // Bob has been dark since before any of this. He holds the roster he was
    // last told about — which still vouches for the phone that is about to be
    // stolen — he has no idea a revocation is coming, and his own build stamps
    // no device id at all, which is what every phone in the field does today.
    assert_eq!(
        deliver(&sibling_store, &fleet.person, &bob, None, 1, "still here"),
        CoreDeliveryVerdict::Applied,
        "a months-offline contact delivers before the revocation"
    );

    // The person revokes the stolen phone on their approving device and
    // commits it: rotate the inbox key, re-seal the backlog, seal the
    // announcement per surviving sibling (§10.1).
    let approver_store = device_store(&fleet, &fleet.approver);
    let update = core_revoke_devices_roster(
        fleet.roster.clone(),
        fleet.person.sign_pk.clone(),
        fleet.approver.sign_sk.clone(),
        vec![fleet.stolen.device_id.clone()],
        fleet.inbox_key.clone(),
    )
    .expect("revocation");
    let commit = commit_revocation(
        &approver_store,
        &fleet,
        &update,
        &fleet.approver,
        Some(fleet.inbox_key.clone()),
    );

    // The sibling adopts the announcement — the only leg §10.1 executes rather
    // than plans — and lands on the new roster and the rotated key.
    let handoff = commit
        .handoffs
        .iter()
        .find(|handoff| handoff.device_id == fleet.sibling.device_id)
        .expect("the surviving sibling is addressed");
    let adoption = sibling_store
        .adopt_revocation_handoff(
            handoff.sealed.sealed.clone(),
            fleet.person.sign_pk.clone(),
            fleet.sibling.clone(),
            Some(fleet.inbox_key.clone()),
            NOW,
        )
        .expect("adopt");
    assert_eq!(adoption.outcome, RevocationAdoptionOutcome::Adopted);
    assert_eq!(
        adoption.revoked_device_ids,
        vec![fleet.stolen.device_id.clone()]
    );
    assert_eq!(adoption.inbox_key, Some(update.inbox_key.clone()));

    // Now the load-bearing assertion of the whole gate: Bob, who still knows
    // nothing, keeps delivering. His mail is not addressed by roster and is not
    // judged by one — §10.3 refuses devices a roster BURIED, and Bob's own
    // devices were not part of anybody's revocation.
    assert_eq!(
        deliver(
            &sibling_store,
            &fleet.person,
            &bob,
            None,
            2,
            "still nothing"
        ),
        CoreDeliveryVerdict::Applied,
        "a stale contact must never be bricked by a revocation in somebody else's fleet"
    );

    // And the thief, who kept the phone and its key, is finished on the
    // sibling — an own device the person buried cannot author into their own
    // history (§10.3).
    assert_eq!(
        deliver(
            &sibling_store,
            &fleet.person,
            &fleet.person,
            Some(&fleet.stolen),
            3,
            "it's me, let me back in",
        ),
        CoreDeliveryVerdict::DroppedRevokedDevice,
    );
    // The sibling's own history from before the burial is untouched: two of
    // Bob's messages, exactly as received.
    assert_eq!(
        sibling_store
            .messages_for_chat(bob.user_id.clone())
            .expect("chat")
            .len(),
        2
    );
}

/// The same window from the CONTACT's side: Bob eventually hears about it, and
/// the moment he does, three things change at once — his stored roster, his
/// §10.4 surface, and what he will accept from the stolen phone.
#[test]
fn when_the_revocation_reaches_the_contact_it_changes_everything_at_once() {
    let fleet = fleet();
    let bob = generate_identity();
    let bob_store = MessageStore::open(":memory:".to_string()).expect("open");
    bob_store
        .upsert_contact(contact_row(&fleet.person, "Alice"))
        .expect("contact");
    bob_store
        .apply_contact_roster(fleet.roster.clone())
        .expect("first roster");

    // Before: the stolen phone is one of Alice's, and Bob takes its mail.
    assert_eq!(
        bob_store
            .contact_device_state(fleet.person.user_id.clone(), fleet.stolen.device_id.clone())
            .expect("device state"),
        ContactDeviceState::Active
    );
    assert_eq!(
        deliver(
            &bob_store,
            &bob,
            &fleet.person,
            Some(&fleet.stolen),
            1,
            "before the theft",
        ),
        CoreDeliveryVerdict::Applied
    );
    // Learning about a fleet raises no safety warning: §2 goal 1 keeps a
    // person's device count out of everyone else's business.
    assert!(bob_store
        .contact_safety_facts(true)
        .expect("facts")
        .is_empty());

    // Alice revokes, and the roster reaches Bob by DL-3 gossip.
    let update = core_revoke_devices_roster(
        fleet.roster.clone(),
        fleet.person.sign_pk.clone(),
        fleet.approver.sign_sk.clone(),
        vec![fleet.stolen.device_id.clone()],
        fleet.inbox_key.clone(),
    )
    .expect("revocation");
    assert_eq!(
        bob_store
            .apply_contact_roster(update.roster.clone())
            .expect("gossip")
            .outcome,
        RosterUpdateOutcome::Accepted
    );

    // (1) The roster: buried, forever (DL-4).
    assert_eq!(
        bob_store
            .contact_device_state(fleet.person.user_id.clone(), fleet.stolen.device_id.clone())
            .expect("device state"),
        ContactDeviceState::Revoked
    );
    // (2) §10.4's surface: one fact, naming exactly the device that was cut off.
    let facts = bob_store.contact_safety_facts(false).expect("facts");
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].reason, ContactSafetyReason::DeviceRevoked);
    assert_eq!(facts[0].person_user_id, fleet.person.user_id);
    assert_eq!(facts[0].device_ids, vec![fleet.stolen.device_id.clone()]);
    // (3) §10.3: the thief's mail stops counting, and what he already sent
    // stays. History is not rewritten by a later revocation — that would let a
    // thief erase a conversation by getting himself revoked.
    assert_eq!(
        deliver(
            &bob_store,
            &bob,
            &fleet.person,
            Some(&fleet.stolen),
            2,
            "it's still me, honestly",
        ),
        CoreDeliveryVerdict::DroppedRevokedDevice
    );
    assert_eq!(
        bob_store
            .messages_for_chat(fleet.person.user_id.clone())
            .expect("chat")
            .len(),
        1,
        "the stream seals at its last pre-revocation point"
    );
    // Alice's surviving devices are unaffected. A revocation cuts off a
    // device, never a person.
    assert_eq!(
        deliver(
            &bob_store,
            &bob,
            &fleet.person,
            Some(&fleet.approver),
            2,
            "sorry, lost my phone",
        ),
        CoreDeliveryVerdict::Applied
    );
}

// ---------------------------------------------------------------------------
// §13 gate: the relay credential
// ---------------------------------------------------------------------------

/// **The client half of "a revoked device demonstrably loses relay fetch".**
///
/// §10.2 exists because relayd scopes fetch, ack and delete by `family_token`
/// alone: a phone that was cut from the roster still holds a credential that
/// works, and can delete its siblings' mail with it. The ceremony here is what
/// the client must get right — mint a replacement, write it down BEFORE
/// performing it, and only then adopt it — and the server half, which is where
/// the thief actually loses access, is `relayd`'s own gate test.
///
/// What this proves that the unit tests do not: the trigger is structural. The
/// planner takes §10.1's [`cruisemesh_core::RevocationCommit`], so the only way
/// to reach a rotation is to have committed the revocation that caused it.
#[test]
fn the_rotation_retires_the_credential_the_thief_kept() {
    let fleet = fleet();
    let bob = generate_identity();
    let store = device_store(&fleet, &fleet.approver);
    store
        .upsert_contact(contact_row(&bob, "Bob"))
        .expect("contact");

    let stolen_credential = "cmfam1-the-token-the-thief-still-has";
    let update = core_revoke_devices_roster(
        fleet.roster.clone(),
        fleet.person.sign_pk.clone(),
        fleet.approver.sign_sk.clone(),
        vec![fleet.stolen.device_id.clone()],
        fleet.inbox_key.clone(),
    )
    .expect("revocation");
    let commit = commit_revocation(
        &store,
        &fleet,
        &update,
        &fleet.approver,
        Some(fleet.inbox_key.clone()),
    );

    let plan = core_plan_relay_rotation(
        commit.clone(),
        "https://relay.example".to_string(),
        stolen_credential.to_string(),
        0,
        NOW,
    )
    .expect("plan")
    .expect("a family with a Shore Pass has something to rotate");
    assert_eq!(plan.superseded_token, stolen_credential);
    assert_ne!(plan.new_token, stolen_credential);
    assert!(!relay_token_is_deposit(plan.new_token.clone()));
    assert_eq!(
        plan.new_deposit_token,
        relay_deposit_token_for(plan.new_token.clone()),
        "both legs must derive the deposit half the same way"
    );
    assert_eq!(
        plan.revoked_device_ids,
        vec![fleet.stolen.device_id.clone()]
    );
    assert_eq!(plan.inbox_key_generation, commit.inbox_key_generation);

    // Written down before it is performed: a device that loses the response to
    // `POST /family/rotate` must still know which of the two tokens to try.
    store
        .begin_relay_rotation(plan.clone(), NOW)
        .expect("begin");
    assert_eq!(
        store.pending_relay_rotation().expect("pending"),
        Some(plan.clone())
    );

    // The server answered; adopt it.
    let rotation = store
        .commit_relay_rotation(plan.clone(), NOW)
        .expect("commit");
    assert_eq!(rotation.endpoint.token, plan.new_token);
    assert_eq!(rotation.superseded_token, stolen_credential);
    assert_eq!(rotation.deposit_token, plan.new_deposit_token);
    assert_eq!(rotation.contact_user_ids, vec![bob.user_id.clone()]);
    assert!(
        rotation.relay_epoch > 0,
        "the contact leg needs a T23 epoch above whatever was announced before"
    );
    // This device's own credential moved, which is what stops it re-presenting
    // the token the thief also holds.
    assert_eq!(
        store
            .relay_credential_setting()
            .expect("setting")
            .map(|endpoint| endpoint.token),
        Some(plan.new_token.clone())
    );
    assert!(
        store.pending_relay_rotation().expect("pending").is_none(),
        "a committed rotation leaves no journal entry to recover"
    );
}

// ---------------------------------------------------------------------------
// §13 gate: the recovery-epoch override
// ---------------------------------------------------------------------------

/// **A stolen APPROVING device is dethroned, and a contact is where it counts.**
///
/// The hard case in §3: the thief holds the one device that may sign rosters.
/// Everything it signs is valid, so contacts accept it — including a roster
/// that buries the owner's other phones. The only authority above it is §14.2's
/// person root, which lives inside the passphrase-encrypted `.cmbak` and on no
/// device at all.
///
/// `revocation.rs` proves the arithmetic. This proves the consequence on
/// somebody else's phone, which is the only place dethroning means anything.
#[test]
fn the_recovery_epoch_dethrones_a_stolen_approving_device_for_a_contact() {
    let fleet = fleet();
    let bob = generate_identity();
    let bob_store = MessageStore::open(":memory:".to_string()).expect("open");
    bob_store
        .upsert_contact(contact_row(&fleet.person, "Alice"))
        .expect("contact");
    bob_store
        .apply_contact_roster(fleet.roster.clone())
        .expect("first roster");

    // The thief uses the approving device exactly as the owner would, and
    // buries the owner's sibling. Bob accepts it, because it IS validly signed
    // — that is the whole problem this override exists for.
    let stolen_update = core_revoke_devices_roster(
        fleet.roster.clone(),
        fleet.person.sign_pk.clone(),
        fleet.approver.sign_sk.clone(),
        vec![fleet.sibling.device_id.clone()],
        fleet.inbox_key.clone(),
    )
    .expect("the thief revokes");
    assert_eq!(
        bob_store
            .apply_contact_roster(stolen_update.roster.clone())
            .expect("gossip")
            .outcome,
        RosterUpdateOutcome::Accepted
    );
    assert_eq!(
        deliver(
            &bob_store,
            &bob,
            &fleet.person,
            Some(&fleet.approver),
            1,
            "hi, it's Alice, send me money",
        ),
        CoreDeliveryVerdict::Applied,
        "before the override the thief is indistinguishable from Alice"
    );

    // Alice opens her backup on a new phone. §14.2: the root secret inside it
    // signs a roster at the next recovery epoch, built from the NEWEST roster
    // in hand — the thief's — so his burial travels forward with it (DL-4).
    let rescue = generate_device_keypair();
    let recovery = core_recovery_revoke_roster(
        stolen_update.roster.clone(),
        fleet.person.sign_sk.clone(),
        rescue.sign_pk.clone(),
        rescue.agree_pk.clone(),
        vec![fleet.approver.device_id.clone()],
        None,
    )
    .expect("recovery");
    assert_eq!(
        bob_store
            .apply_contact_roster(recovery.roster.clone())
            .expect("gossip")
            .outcome,
        RosterUpdateOutcome::Accepted
    );

    // Dethroned, in the three ways that matter to Bob:
    // (1) the thief's device is buried and his mail stops counting;
    assert_eq!(
        bob_store
            .contact_device_state(
                fleet.person.user_id.clone(),
                fleet.approver.device_id.clone()
            )
            .expect("device state"),
        ContactDeviceState::Revoked
    );
    assert_eq!(
        deliver(
            &bob_store,
            &bob,
            &fleet.person,
            Some(&fleet.approver),
            2,
            "why did you stop replying",
        ),
        CoreDeliveryVerdict::DroppedRevokedDevice
    );
    // (2) every later roster the thief signs is a rollback, not an update —
    //     a device key alone can never climb a recovery epoch;
    let thief_carries_on = core_revoke_devices_roster(
        stolen_update.roster.clone(),
        fleet.person.sign_pk.clone(),
        fleet.approver.sign_sk.clone(),
        vec![fleet.stolen.device_id.clone()],
        core_mint_inbox_key(stolen_update.roster.inbox_key_generation),
    )
    .expect("the thief keeps signing");
    let decision = bob_store
        .apply_contact_roster(thief_carries_on.roster)
        .expect("gossip");
    assert_eq!(decision.outcome, RosterUpdateOutcome::Ignored);
    assert_eq!(decision.reason, RosterUpdateReason::Rollback);
    // (3) and Bob's §10.4 surface says both halves of what happened, without
    //     blending them: somebody opened Alice's backup, AND a device was cut
    //     off. Acknowledging one is not acknowledging the other.
    let reasons: Vec<ContactSafetyReason> = bob_store
        .contact_safety_facts(false)
        .expect("facts")
        .into_iter()
        .map(|fact| fact.reason)
        .collect();
    assert!(reasons.contains(&ContactSafetyReason::IdentityRecovered));
    assert_eq!(
        reasons
            .iter()
            .filter(|reason| **reason == ContactSafetyReason::DeviceRevoked)
            .count(),
        2,
        "the thief's burial and the recovery's burial are two separate facts"
    );

    // The rescue device — the one holding the backup — now speaks for Alice.
    assert_eq!(recovery.roster.approving_device_id, rescue.device_id);
    assert_eq!(
        deliver(
            &bob_store,
            &bob,
            &fleet.person,
            Some(&rescue),
            3,
            "my phone was stolen, this is my new one",
        ),
        CoreDeliveryVerdict::Applied
    );
}
