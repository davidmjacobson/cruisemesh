//! §9.3–§9.4 end to end: a confirmed channel, a canonical bootstrap, two-phase
//! activation, and the silence in between.
//!
//! `specs/multi-device-v1.md` §13's WP3 gate has three parts. Two of them are
//! David's — linking two dev builds on LAN and on relay-only. The third is this
//! file's: *"new device provably silent pre-activation"*, proven the only way a
//! claim like that can be, by asking a pre-activation store to do each of the
//! three forbidden things and watching it refuse.
//!
//! Everything here runs over a loopback: two `MessageStore`s, two ceremony
//! objects, and a `Vec<u8>` between them. No sockets, no phones. What the
//! phones add is the transport, which is exactly what §13 says must already be
//! sim-proven before they are picked up.

use cruisemesh_core::{
    compute_recipient_hint, core_device_namespace_id, core_link_activation_ack,
    core_link_bootstrap_chunks, core_link_bootstrap_decode, core_link_bootstrap_encode,
    core_link_bootstrap_join, core_link_device_offer, core_link_genesis_roster,
    core_link_open_activation_ack, core_link_open_device_offer, core_link_sign_new_device_roster,
    core_roster_head_hash, generate_device_keypair, generate_identity, Contact,
    CoreInboundDisposition, CoreLinkActionKind, CoreLinkActivationStage, CoreLinkApprovingDevice,
    CoreLinkGatedAction, CoreLinkNewDevice, CoreLinkOutcome, CoreRelayEnvelopeDisposition,
    DeviceKeypair, Group, Identity, LinkBootstrap, MessageStore, Roster, StoredMessage, KIND_TEXT,
    LEGACY_DEVICE_ID,
};

const NOW: i64 = 1_755_000_000_000;
/// A stand-in channel binding for the cases that never run a ceremony.
const BINDING: [u8; 32] = [0x7C; 32];

/// The person as their first phone knows them: an identity, a roster naming
/// that phone as the approving device, and a store with something in it worth
/// carrying to a second phone.
struct Approving {
    store: MessageStore,
    identity: Identity,
    device: DeviceKeypair,
    roster: Roster,
    contact: Contact,
    group: Group,
}

fn approving_device() -> Approving {
    let store = MessageStore::open(":memory:".to_string()).unwrap();
    let identity = generate_identity();
    let device = generate_device_keypair();

    let contact = Contact {
        user_id: vec![0xC1; 16],
        name: "Bob".to_string(),
        sign_pk: vec![0xC2; 32],
        agree_pk: vec![0xC3; 32],
        relay_url: Some("https://relay.example".to_string()),
        relay_token: Some("family-token".to_string()),
        nickname: Some("Dad".to_string()),
    };
    store.upsert_contact(contact.clone()).unwrap();
    // A nickname is local-only presentation and never rides a contact-facing
    // wire format. It DOES ride this export: an export crosses one channel
    // between two devices of one person (SYNC-3), and "Dad" is exactly the kind
    // of thing that should be on both of that person's phones.
    store
        .set_contact_nickname(contact.user_id.clone(), contact.nickname.clone())
        .unwrap();

    let group = Group {
        id: vec![0xD1; 16],
        name: "Cabin 8".to_string(),
        member_user_ids: vec![identity.user_id.clone(), contact.user_id.clone()],
        key: vec![0xD2; 32],
        metadata_revision: 1,
        metadata_changed_by: identity.user_id.clone(),
    };
    store.upsert_group(group.clone()).unwrap();

    for lamport in 1..=3_u64 {
        store
            .insert_incoming_message(
                StoredMessage {
                    chat_id: contact.user_id.clone(),
                    sender_user_id: contact.user_id.clone(),
                    lamport,
                    timestamp: NOW - 1_000 * lamport as i64,
                    kind: KIND_TEXT,
                    payload: format!("message {lamport}").into_bytes(),
                    sender_device_id: LEGACY_DEVICE_ID.to_vec(),
                },
                vec![lamport as u8; 16],
                None,
            )
            .unwrap();
    }

    // §3's identity upgrade: the deployed key becomes the person root and this
    // phone becomes device one.
    let roster = core_link_genesis_roster(
        identity.sign_sk.clone(),
        device.sign_pk.clone(),
        device.agree_pk.clone(),
    )
    .unwrap();
    store
        .adopt_own_roster(
            roster.clone(),
            identity.sign_pk.clone(),
            device.device_id.clone(),
        )
        .unwrap();

    Approving {
        store,
        identity,
        device,
        roster,
        contact,
        group,
    }
}

fn bytes_of(action: &cruisemesh_core::CoreLinkAction) -> Vec<u8> {
    match &action.kind {
        CoreLinkActionKind::SendBytes { bytes } => bytes.clone(),
        other => panic!("expected bytes to send, got {other:?}"),
    }
}

/// Drive §9.1–§9.2 to a confirmed channel and hand back both halves plus the
/// binding both ends derived. This is slice one's ceremony, used as the
/// transport it is.
fn confirmed_channel() -> (CoreLinkNewDevice, CoreLinkApprovingDevice, Vec<u8>) {
    let newcomer = CoreLinkNewDevice::new(
        vec!["192.168.1.24:45892".to_string()],
        Vec::new(),
        NOW,
        None,
    )
    .unwrap();
    let approver = CoreLinkApprovingDevice::scan(newcomer.qr_text(), 1, None).unwrap();

    newcomer.start(NOW);
    let message = bytes_of(&approver.start(NOW));
    approver.resume_sent(NOW);
    let message = bytes_of(&newcomer.resume_peer_bytes(NOW, message));
    newcomer.resume_sent(NOW);
    let message = bytes_of(&approver.resume_peer_bytes(NOW, message));
    newcomer.resume_peer_bytes(NOW, message);
    approver.resume_sent(NOW);

    let confirm = bytes_of(&approver.confirm(NOW));
    let approver_summary = approver.resume_sent(NOW);
    newcomer.resume_peer_bytes(NOW, confirm);
    assert!(matches!(
        approver_summary.kind,
        CoreLinkActionKind::Finished { .. }
    ));
    let summary = newcomer.summary().unwrap();
    assert_eq!(summary.outcome, CoreLinkOutcome::ChannelReady);
    let binding = summary.channel_binding.clone().unwrap();
    assert_eq!(
        approver.summary().unwrap().channel_binding,
        Some(binding.clone())
    );
    (newcomer, approver, binding)
}

/// The three things §9.4 forbids, asked of a store directly. Returns what each
/// one did, so the same probe can be run before and after activation and the
/// two answers compared.
struct Probe {
    authored: bool,
    acked: Vec<i64>,
    fetch_hints: usize,
    carry_offers: usize,
    /// The digest spray: the loudest thing this device says to a peer it meets.
    spray_lanes: usize,
    /// Mail already queued for a relay, and the rows an upload would post for
    /// it. Both must be empty during the window: the queue is not the wire, but
    /// uploading is.
    pending_uploads: usize,
    upload_rows: usize,
}

fn probe(store: &MessageStore, identity: &Identity, contact: &Contact, device_id: &[u8]) -> Probe {
    // Authoring, at the top of the path — before a lamport is spent.
    let authored = store
        .author_pairwise_message(
            identity.clone(),
            contact.clone(),
            KIND_TEXT,
            b"hello from the new phone".to_vec(),
            None,
            NOW,
        )
        .is_ok();

    // Acking: a row addressed to THIS device's §7 namespace and consumed —
    // the one case ACK-MD-1 says is unambiguously this device's to delete.
    let acked = store
        .core_relay_ack_ids_with_consumed(
            vec![CoreRelayEnvelopeDisposition {
                relay_id: 41,
                msg_id: vec![0x41; 16],
                disposition: CoreInboundDisposition::Consumed,
                recipient_hint: compute_recipient_hint(
                    core_device_namespace_id(identity.user_id.clone(), device_id.to_vec()),
                    NOW,
                ),
            }],
            identity.user_id.clone(),
            NOW,
        )
        .unwrap();

    // Advertising: the hint sets that tell a relay which mailboxes are this
    // device's business, and the carry drain it offers a peer it just met.
    let fetch_hints = store
        .relay_fetch_hints(identity.user_id.clone(), NOW)
        .unwrap()
        .len();
    let carry_offers = store
        .delivery_hints_for_peer(contact.user_id.clone(), NOW)
        .unwrap()
        .len();

    // The digest spray, which is where a stranger meeting this phone learns it
    // exists at all, and the two relay upload planners.
    let spray = store
        .core_digest_spray_plan(
            identity.user_id.clone(),
            contact.user_id.clone(),
            Vec::new(),
            Vec::new(),
            NOW,
            64 * 1024,
            64 * 1024,
            64 * 1024,
            32,
            cruisemesh_core::HIDDEN_SPRAY_KINDS.to_vec(),
            Vec::new(),
            None,
        )
        .unwrap();
    let spray_lanes = spray.carried_frames.len()
        + spray.own_outbound_frames.len()
        + spray.own_receipt_frames.len();

    let pending = store
        .pending_relay_outbound_envelopes(32, NOW, Vec::new())
        .unwrap();
    let pending_uploads = pending.len();
    let upload_rows = pending
        .into_iter()
        .map(|envelope| {
            store
                .core_outbound_relay_rows(envelope, identity.user_id.clone(), None)
                .unwrap()
                .len()
        })
        .sum();

    Probe {
        authored,
        acked,
        fetch_hints,
        carry_offers,
        spray_lanes,
        pending_uploads,
        upload_rows,
    }
}

/// **The §13 WP3 gate, core's half.** One link, all the way through, with the
/// new device asked to misbehave at every stage.
#[test]
fn a_new_device_is_silent_until_it_has_acknowledged_the_roster() {
    let approving = approving_device();
    let newcomer_store = MessageStore::open(":memory:".to_string()).unwrap();
    let newcomer_device = generate_device_keypair();

    // ---- §9.1–§9.2: a confirmed channel --------------------------------
    let (newcomer, approver, binding) = confirmed_channel();

    // ---- The pre-activation window opens -------------------------------
    // The new device goes quiet the moment it has a channel, not once the
    // bootstrap arrives: the export crosses the wire inside this window.
    newcomer_store
        .begin_link_activation(binding.clone(), NOW)
        .unwrap();
    assert_eq!(
        newcomer_store.link_activation().unwrap().stage,
        CoreLinkActivationStage::AwaitingBootstrap
    );
    for action in [
        CoreLinkGatedAction::Advertise,
        CoreLinkGatedAction::Author,
        CoreLinkGatedAction::Ack,
    ] {
        assert!(
            !newcomer_store.link_gate(action).unwrap().allowed,
            "{action:?} must be refused before the bootstrap has even landed"
        );
    }

    // ---- §9.3: the new device names its keys ---------------------------
    let offer_frame = core_link_device_offer(
        newcomer_device.sign_sk.clone(),
        newcomer_device.agree_pk.clone(),
        binding.clone(),
    )
    .unwrap();
    let sealed = newcomer.seal_channel_frame(offer_frame).unwrap();
    let offer = core_link_open_device_offer(
        approver.open_channel_frame(sealed).unwrap(),
        binding.clone(),
    )
    .unwrap();
    assert_eq!(offer.device_id, newcomer_device.device_id);

    // ---- §9.4a: the approving device signs seq+1 and streams the export --
    let update = core_link_sign_new_device_roster(
        approving.roster.clone(),
        approving.identity.sign_pk.clone(),
        approving.device.sign_sk.clone(),
        offer.device_sign_pk.clone(),
        offer.device_agree_pk.clone(),
    )
    .unwrap();
    assert_eq!(update.roster.seq, approving.roster.seq + 1);

    let bootstrap = approving
        .store
        .build_link_bootstrap(
            approving.identity.clone(),
            update.roster.clone(),
            approving.device.sign_sk.clone(),
            binding.clone(),
            0,
            0,
            NOW,
        )
        .unwrap();
    let payload = core_link_bootstrap_encode(bootstrap.clone()).unwrap();
    let mut received = Vec::new();
    for chunk in core_link_bootstrap_chunks(payload.clone()).unwrap() {
        let sealed = approver.seal_channel_frame(chunk).unwrap();
        received.push(newcomer.open_channel_frame(sealed).unwrap());
    }
    let arrived = core_link_bootstrap_decode(core_link_bootstrap_join(received).unwrap()).unwrap();
    assert_eq!(arrived, bootstrap, "the export survives the channel intact");

    // It is a statement of what this person knows, not a database image.
    assert_eq!(arrived.contacts.len(), 1);
    assert_eq!(arrived.contacts[0].contact, approving.contact);
    // Compared against what the store holds rather than against the literal
    // fixture: the group table normalises member order, so a byte-for-byte
    // comparison with the struct handed to `upsert_group` passes or fails on
    // where this run's random person id happens to sort. What the export owes
    // is that it carries what the store holds.
    assert_eq!(arrived.groups, approving.store.list_groups().unwrap());
    assert_eq!(arrived.groups.len(), 1);
    assert_eq!(arrived.groups[0].id, approving.group.id);
    assert_eq!(arrived.history_head.len(), 3);
    assert_eq!(arrived.person.person_id, approving.identity.user_id);
    assert_eq!(
        arrived.person.inbox_agree_sk, approving.identity.agree_sk,
        "§6: a linked device holds the person-scoped inbox key"
    );

    let import = newcomer_store
        .import_link_bootstrap(
            arrived.clone(),
            newcomer_device.sign_pk.clone(),
            Some(approving.identity.user_id.clone()),
            NOW + 1,
        )
        .unwrap();
    assert_eq!(import.own_device_id, newcomer_device.device_id);
    assert_eq!(
        import.roster_head,
        core_roster_head_hash(update.roster.clone())
    );
    assert_eq!(import.contacts_imported, 1);
    assert_eq!(import.groups_imported, 1);
    assert_eq!(import.messages_imported, 3);
    assert_eq!(
        import.catch_up.len(),
        1,
        "the WP4 seam names the chat it truncated"
    );

    // The store now holds the person's world...
    assert_eq!(
        newcomer_store.list_contacts().unwrap(),
        vec![approving.contact.clone()]
    );
    assert_eq!(
        newcomer_store.list_groups().unwrap(),
        approving.store.list_groups().unwrap()
    );
    assert_eq!(
        newcomer_store
            .messages_for_chat(approving.contact.user_id.clone())
            .unwrap()
            .len(),
        3
    );
    // ...and is still not allowed to say a word about it (§9.4b outstanding).
    assert_eq!(
        newcomer_store.link_activation().unwrap().stage,
        CoreLinkActivationStage::AwaitingRosterAck
    );
    let silent = probe(
        &newcomer_store,
        &approving.identity,
        &approving.contact,
        &newcomer_device.device_id,
    );
    assert!(
        !silent.authored,
        "a pre-activation device authored a message"
    );
    assert!(
        silent.acked.is_empty(),
        "a pre-activation device planned an ack: {:?}",
        silent.acked
    );
    assert_eq!(
        silent.fetch_hints, 0,
        "a pre-activation device published hints"
    );
    assert_eq!(
        silent.carry_offers, 0,
        "a pre-activation device offered carry"
    );
    assert_eq!(
        silent.spray_lanes, 0,
        "a pre-activation device sprayed a digest"
    );
    assert_eq!(
        silent.pending_uploads, 0,
        "a pre-activation device offered queued mail for relay upload"
    );
    assert_eq!(
        silent.upload_rows, 0,
        "a pre-activation device planned relay rows"
    );
    // And the fleet routing reads is still the unlinked default.
    assert!(newcomer_store
        .own_device_fleet()
        .unwrap()
        .own_device_id
        .is_none());

    // A near-miss does not close activation: §9.4 says the EXACT head.
    let mut wrong = import.roster_head.clone();
    wrong[0] ^= 0x01;
    assert!(newcomer_store
        .complete_link_activation(wrong, NOW + 2)
        .is_err());
    assert!(
        !newcomer_store
            .link_gate(CoreLinkGatedAction::Author)
            .unwrap()
            .allowed,
        "a refused acknowledgement leaves the device silent"
    );

    // ---- §9.4b: the exact head, acknowledged back ----------------------
    let ack_frame = core_link_activation_ack(
        newcomer_device.sign_sk.clone(),
        import.roster_head.clone(),
        binding.clone(),
    )
    .unwrap();
    let sealed = newcomer.seal_channel_frame(ack_frame).unwrap();
    let ack = core_link_open_activation_ack(
        approver.open_channel_frame(sealed).unwrap(),
        update.roster.clone(),
        offer.device_sign_pk.clone(),
        binding.clone(),
    )
    .unwrap();
    assert_eq!(ack.device_id, newcomer_device.device_id);
    assert_eq!(ack.roster_head, import.roster_head);

    // Both sides now adopt the roster they agreed on.
    approving
        .store
        .adopt_own_roster(
            update.roster.clone(),
            approving.identity.sign_pk.clone(),
            approving.device.device_id.clone(),
        )
        .unwrap();
    let activation = newcomer_store
        .complete_link_activation(import.roster_head.clone(), NOW + 3)
        .unwrap();
    assert_eq!(activation.stage, CoreLinkActivationStage::Activated);

    // ---- Visible ------------------------------------------------------
    let loud = probe(
        &newcomer_store,
        // A linked device authors in its OWN stream once WP4's per-device
        // authoring lands; today's authoring path still signs with the person
        // key, so that is what this probe hands it. What is under test here is
        // the gate: the same call that was refused a moment ago now runs.
        &approving.identity,
        &approving.contact,
        &newcomer_device.device_id,
    );
    assert!(loud.authored, "an activated device may author");
    assert_eq!(
        loud.acked,
        vec![41],
        "ACK-MD-1: its own namespace, consumed"
    );
    assert!(loud.fetch_hints > 0);
    assert!(loud.carry_offers > 0);

    // The fleets agree, and each device knows which one it is.
    let newcomer_fleet = newcomer_store.own_device_fleet().unwrap();
    let approving_fleet = approving.store.own_device_fleet().unwrap();
    assert_eq!(newcomer_fleet.device_ids, approving_fleet.device_ids);
    assert_eq!(newcomer_fleet.device_ids.len(), 2);
    assert_eq!(
        newcomer_fleet.own_device_id,
        Some(newcomer_device.device_id.clone())
    );
    assert_eq!(
        approving_fleet.own_device_id,
        Some(approving.device.device_id.clone())
    );
    assert_eq!(newcomer_fleet.projected_from, update.roster.version());
    assert_eq!(
        newcomer_store.own_roster().unwrap(),
        Some(update.roster.clone())
    );
}

/// An install that never links behaves exactly as it does today — the property
/// that lets this gate ship to a fleet of single-device phones.
#[test]
fn an_install_that_never_links_is_unaffected() {
    let approving = approving_device();
    let probe = probe(
        &approving.store,
        &approving.identity,
        &approving.contact,
        &approving.device.device_id,
    );
    assert!(probe.authored);
    assert!(probe.fetch_hints > 0);
    assert!(probe.carry_offers > 0);
    // An install that never linked still plans and uploads exactly as it did
    // before the gate existed: the message it just authored is queued for its
    // contact's relay and would be posted.
    assert!(probe.pending_uploads > 0);
    assert!(probe.upload_rows > 0);

    let fresh = MessageStore::open(":memory:".to_string()).unwrap();
    assert_eq!(
        fresh.link_activation().unwrap().stage,
        CoreLinkActivationStage::NotLinking
    );
    for action in [
        CoreLinkGatedAction::Advertise,
        CoreLinkGatedAction::Author,
        CoreLinkGatedAction::Ack,
    ] {
        assert!(fresh.link_gate(action).unwrap().allowed);
    }
}

/// **The window is a window.** A ceremony that opened it and then failed hands
/// the gates back -- and does so without ever having been activated, so nothing
/// it half-learned survives.
#[test]
fn a_failed_ceremony_reopens_every_gate_it_closed() {
    let approving = approving_device();
    let newcomer_store = MessageStore::open(":memory:".to_string()).unwrap();
    let newcomer_device = generate_device_keypair();
    let (_newcomer, _approver, binding) = confirmed_channel();

    newcomer_store
        .begin_link_activation(binding.clone(), NOW)
        .unwrap();
    let silent = probe(
        &newcomer_store,
        &approving.identity,
        &approving.contact,
        &newcomer_device.device_id,
    );
    assert!(!silent.authored);
    assert_eq!(silent.fetch_hints, 0);
    assert_eq!(silent.spray_lanes, 0);

    // The other phone went away mid-ceremony. Nothing was imported, nothing was
    // acknowledged, and this phone must not be left mute forever.
    let abandoned = newcomer_store.abandon_link_activation(NOW + 1).unwrap();
    assert_eq!(abandoned.stage, CoreLinkActivationStage::NotLinking);
    assert!(abandoned.own_device_id.is_none());

    let loud = probe(
        &newcomer_store,
        &approving.identity,
        &approving.contact,
        &newcomer_device.device_id,
    );
    assert!(
        loud.authored,
        "an abandoned ceremony must give authoring back"
    );
    assert!(loud.fetch_hints > 0);
    assert!(loud.pending_uploads > 0);
    // And the phone is still an unlinked phone, not a half-linked one.
    assert!(newcomer_store
        .own_device_fleet()
        .unwrap()
        .own_device_id
        .is_none());
    assert!(newcomer_store.own_roster().unwrap().is_none());
}

/// §9.3 must not fold one person's world into another's, and must not be
/// satisfiable by a bootstrap that belongs to some other ceremony.
#[test]
fn an_import_refuses_the_wrong_person_and_the_wrong_ceremony() {
    let approving = approving_device();
    let newcomer_device = generate_device_keypair();
    let (_newcomer, _approver, binding) = confirmed_channel();

    let update = core_link_sign_new_device_roster(
        approving.roster.clone(),
        approving.identity.sign_pk.clone(),
        approving.device.sign_sk.clone(),
        newcomer_device.sign_pk.clone(),
        newcomer_device.agree_pk.clone(),
    )
    .unwrap();
    let bootstrap = approving
        .store
        .build_link_bootstrap(
            approving.identity.clone(),
            update.roster.clone(),
            approving.device.sign_sk.clone(),
            binding.clone(),
            0,
            0,
            NOW,
        )
        .unwrap();

    // The shell said this phone was being set up as somebody else.
    let fresh = MessageStore::open(":memory:".to_string()).unwrap();
    fresh.begin_link_activation(binding.clone(), NOW).unwrap();
    assert!(fresh
        .import_link_bootstrap(
            bootstrap.clone(),
            newcomer_device.sign_pk.clone(),
            Some(vec![0xAB; 16]),
            NOW,
        )
        .is_err());
    assert!(fresh.list_contacts().unwrap().is_empty());

    // A phone that already holds someone's world, and nobody said whose.
    let occupied = MessageStore::open(":memory:".to_string()).unwrap();
    occupied
        .upsert_contact(Contact {
            user_id: vec![0xE1; 16],
            name: "Someone else".to_string(),
            sign_pk: vec![0xE2; 32],
            agree_pk: vec![0xE3; 32],
            relay_url: None,
            relay_token: None,
            nickname: None,
        })
        .unwrap();
    occupied
        .begin_link_activation(binding.clone(), NOW)
        .unwrap();
    assert!(occupied
        .import_link_bootstrap(
            bootstrap.clone(),
            newcomer_device.sign_pk.clone(),
            None,
            NOW
        )
        .is_err());
    assert_eq!(occupied.list_contacts().unwrap().len(), 1);

    // And an export made for a different ceremony, on a phone whose window is
    // otherwise perfectly ready for one.
    let elsewhere = MessageStore::open(":memory:".to_string()).unwrap();
    elsewhere
        .begin_link_activation(vec![0x5D; 32], NOW)
        .unwrap();
    assert!(elsewhere
        .import_link_bootstrap(
            bootstrap.clone(),
            newcomer_device.sign_pk.clone(),
            Some(approving.identity.user_id.clone()),
            NOW,
        )
        .is_err());
    assert!(elsewhere.list_contacts().unwrap().is_empty());

    // Expiry is the third leg, and it is the same refusal.
    let late = MessageStore::open(":memory:".to_string()).unwrap();
    late.begin_link_activation(binding, NOW).unwrap();
    assert!(late
        .import_link_bootstrap(
            bootstrap,
            newcomer_device.sign_pk,
            Some(approving.identity.user_id.clone()),
            NOW + 24 * 60 * 60 * 1000,
        )
        .is_err());
    assert!(late.list_contacts().unwrap().is_empty());
}

/// A bootstrap that does not carry a certificate for the device importing it is
/// not this device's bootstrap, and leaves it silent.
#[test]
fn a_bootstrap_for_another_device_does_not_activate_this_one() {
    let approving = approving_device();
    let intended = generate_device_keypair();
    let interloper = generate_device_keypair();

    let update = core_link_sign_new_device_roster(
        approving.roster.clone(),
        approving.identity.sign_pk.clone(),
        approving.device.sign_sk.clone(),
        intended.sign_pk.clone(),
        intended.agree_pk.clone(),
    )
    .unwrap();
    let bootstrap = approving
        .store
        .build_link_bootstrap(
            approving.identity.clone(),
            update.roster,
            approving.device.sign_sk.clone(),
            BINDING.to_vec(),
            0,
            0,
            NOW,
        )
        .unwrap();

    let store = MessageStore::open(":memory:".to_string()).unwrap();
    store.begin_link_activation(BINDING.to_vec(), NOW).unwrap();
    assert!(store
        .import_link_bootstrap(bootstrap.clone(), interloper.sign_pk, None, NOW)
        .is_err());
    assert!(
        !store
            .link_gate(CoreLinkGatedAction::Author)
            .unwrap()
            .allowed
    );

    // And a bootstrap whose roster does not chain to the person it names is
    // refused before a single contact is written.
    let mut forged: LinkBootstrap = bootstrap.clone();
    forged.person.person_sign_pk = generate_identity().sign_pk;
    assert!(store
        .import_link_bootstrap(forged, intended.sign_pk.clone(), None, NOW)
        .is_err());
    assert!(store.list_contacts().unwrap().is_empty());
}
