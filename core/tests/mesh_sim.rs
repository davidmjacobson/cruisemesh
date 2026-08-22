//! Headless multi-node mesh simulation (DESIGN.md §5.3 gossip, §10/§11 M2).
//!
//! The gossip relay and carry queue can't be exercised with the two physical
//! phones on hand -- relaying and muling only mean anything with three or more
//! nodes and intermittent contact. This simulation fills that gap: it stands
//! up many nodes with real identities and real crypto, connects and churns
//! them over discrete rounds, and checks that a sealed message reaches its
//! recipient across hops and time gaps it could never reach directly.
//!
//! It drives the **real** core primitives -- `seal_message`/`open_message`,
//! the §6.4 frame codec, [`SeenIds`], and the [`MessageStore`] carry queue:
//!   * receive: this is no longer a third copy. `SimNode::receive`
//!     and `SimNode::receive_from_relay` both call the one production inbound
//!     transaction, [`MessageStore::process_inbound_frame`], which owns dedupe,
//!     expiry, open-for-self/group-with-membership-guard, flood/carry
//!     classification, and the ack-safety evidence. The sim only supplies the
//!     frame and records the delivered payload; the disposition is core's.
//!   * meeting (HELLO/DIGEST): [`MessageStore::plan_mesh_meet`] owns the whole
//!     encounter -- peer capabilities, the digest cadence and the DIGEST
//!     frames themselves, digest-confirm, digest exclusion, the targeted
//!     drain, the per-epoch offer allowance and the budgeted spray.
//!     `Network::meet` only decides who is adjacent and moves the returned
//!     frames between them. The simulation keeps no meet or spray arithmetic
//!     of its own: it stopped being a third implementation.
//!   * relay: group-addressed uploads fan out into one row per member hint;
//!     each node polls its own hints and, through the same core transaction,
//!     only acks rows it consumed.

use std::collections::VecDeque;
use std::sync::Arc;

use cruisemesh_core::{
    compute_recipient_hint, core_device_namespace_id, core_encode_sync_history,
    core_group_fanout_rows, core_own_capabilities, core_plan_sync_backfill, core_seal_sync_record,
    core_sign_device_cert, core_sign_roster, core_sign_sync_record, core_sync_digest_gaps,
    core_sync_record_id, dedupe_hints, default_expiry, device_fanout_msg_id, encode_envelope_frame,
    generate_device_keypair, generate_identity, generate_msg_id, parse_frame, seal_group_message,
    seal_message, CarriedEnvelope, Contact, CoreCarriedOfferGate, CoreInboundDisposition,
    CoreInboundSource, CoreMeetOutcome, CoreMeetRequest, CoreMeshRouterState,
    CoreRelayEnvelopeDisposition, CoreSprayPolicy, CoreSprayTrigger, CoreTransport, DeviceCert,
    DeviceKeypair, Frame, Group, Identity, InboxKey, MessageStore, OutboundEnvelope,
    OwnDeviceFleet, Roster, RosterUpdateOutcome, RosterVersion, SeenIds, StoredMessage,
    SyncBackfillAction, SyncRecord, SyncRecordKind, DEFAULT_HOP_TTL,
    DEVICE_CERT_FLAG_ROSTER_SIGNING, KIND_TEXT,
};

const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;
const BASE_NOW: i64 = 1_700_000_000_000;
const FOREIGN_BUDGET: i64 = 5 * 1024 * 1024;

/// One mesh participant: identity + crypto, its persistent carry queue, its
/// flood-dedupe set, and an inbox of payloads it successfully opened.
struct SimNode {
    identity: Identity,
    store: MessageStore,
    /// Process-wide flood-dedupe set, shared by reference with the core inbound
    /// transaction exactly as a device shares `GossipState.seenIds`.
    seen: Arc<SeenIds>,
    /// Plaintext of every message this node was the recipient of and opened.
    inbox: Vec<Vec<u8>>,
    /// Per-device encounter state the planner records walk cursors on.
    router: CoreMeshRouterState,
    /// Per-device spray cadence / burst bucket.
    spray: CoreSprayPolicy,
    /// Per-device allowance for how many peers may walk this device's carry
    /// store in one short epoch (G3).
    offers: CoreCarriedOfferGate,
    /// Monotonic stream position for the rows [`SimNode::record_delivery`]
    /// persists, so two opened messages never collide into the conflict
    /// quarantine and get dropped from the digest.
    delivered_seq: u64,
}

impl SimNode {
    fn new() -> Self {
        let identity = generate_identity();
        let router = CoreMeshRouterState::new();
        router.set_local_user_id(identity.user_id.clone());
        SimNode {
            identity,
            store: MessageStore::open(":memory:".to_string()).expect("open in-memory store"),
            seen: Arc::new(SeenIds::new()),
            inbox: Vec::new(),
            router,
            spray: CoreSprayPolicy::new(),
            offers: CoreCarriedOfferGate::new(),
            delivered_seq: 0,
        }
    }

    /// The shells' kind dispatch, reduced to the one effect the DTN rules
    /// depend on: an opened payload becomes a durable `messages` row.
    ///
    /// This is what makes a node's next DIGEST able to name what it consumed
    /// (`core_digest_advertised_msg_ids` reads `messages`), which is the only
    /// evidence that ever retires a mule's 1:1 carry under `CARRY-01`. While
    /// the sim opened straight into an in-memory `inbox` and skipped this, no
    /// mule could ever obtain proof of receipt, and the suite could not tell a
    /// correct planner from one that deleted at dispatch.
    ///
    /// The sim seals raw plaintext rather than an encoded `ExtendedMessageBody`
    /// (see `author_and_flood`), so the row is synthesized from what a shell
    /// would have decoded. Ordering matches the production callers: persist
    /// first, commit the inbound transaction second — never the reverse.
    fn record_delivery(&mut self, payload: &[u8], sender_user_id: &[u8], msg_id: Vec<u8>) {
        self.delivered_seq += 1;
        self.store
            .insert_incoming_message(
                StoredMessage {
                    chat_id: sender_user_id.to_vec(),
                    sender_user_id: sender_user_id.to_vec(),
                    lamport: self.delivered_seq,
                    timestamp: self.delivered_seq as i64,
                    kind: KIND_TEXT,
                    payload: payload.to_vec(),
                    sender_device_id: cruisemesh_core::LEGACY_DEVICE_ID.to_vec(),
                },
                msg_id,
                None,
            )
            .expect("persist opened message");
    }

    fn user_id(&self) -> Vec<u8> {
        self.identity.user_id.clone()
    }

    fn import_group(&self, group: Group) {
        self.store.upsert_group(group).expect("import group");
    }

    /// Receive one BLE/LAN frame through the production inbound transaction
    /// [`MessageStore::process_inbound_frame`]. Delivers any opened payload into
    /// this node's inbox (the sim's stand-in for the shells' kind dispatch) and
    /// returns the hop-decremented frame to flood onward, or `None` when the
    /// frame is a duplicate, expired, delivered to us, or out of hops.
    fn receive(&mut self, frame_bytes: &[u8], now: i64) -> Option<Vec<u8>> {
        let outcome = self
            .store
            .process_inbound_frame(
                self.identity.clone(),
                self.seen.clone(),
                CoreInboundSource::Mesh,
                frame_bytes.to_vec(),
                now,
            )
            .expect("process inbound mesh frame");
        let sender = outcome.delivered_sender.clone();
        let delivered_msg_id = outcome.commit.as_ref().map(|commit| commit.msg_id.clone());
        for payload in outcome.delivered_payloads {
            if let (Some(sender), Some(msg_id)) = (sender.as_ref(), delivered_msg_id.as_ref()) {
                self.record_delivery(&payload, sender, msg_id.clone());
            }
            self.inbox.push(payload);
        }
        // DTN D4: the sim's in-memory delivery cannot fail, so a delivered
        // payload always commits — recording `seen` and any hidden evidence the
        // core deferred until after delivery. A real caller skips this on a
        // durable-delivery failure and reports Failed instead.
        if let Some(commit) = outcome.commit {
            self.store
                .core_commit_inbound_delivery(self.seen.clone(), commit);
        }
        outcome.relay_frame
    }

    /// Receive one §8 self-sync frame.
    ///
    /// There is deliberately almost nothing here. The whole disposition —
    /// dedupe, expiry, the pairwise open, the SYNC-3 roster gate, the apply,
    /// and the ACK-MD-1 consumed-hidden evidence that lets this device's
    /// fan-out row be deleted — runs inside
    /// [`MessageStore::process_inbound_frame`], because a sync record is
    /// ordinary sealed 1:1 traffic and the inbound transaction is where this
    /// codebase decides what ordinary sealed 1:1 traffic means. The sim used to
    /// own the record/message split itself, which made it a fourth
    /// implementation of a rule that has to be identical on both shells.
    ///
    /// What is left is what a shell has left: hand the frame in, deliver
    /// whatever comes back out as ordinary mail, and commit.
    ///
    /// Returns how many sync records core applied — at most one, since a frame
    /// carries one sealed body.
    fn receive_sync(&mut self, frame_bytes: &[u8], now: i64) -> usize {
        let outcome = self
            .store
            .process_inbound_frame(
                self.identity.clone(),
                self.seen.clone(),
                CoreInboundSource::Mesh,
                frame_bytes.to_vec(),
                now,
            )
            .expect("process inbound mesh frame");
        // A consumed frame that handed nothing back to deliver, and still
        // counted as delivered work, is a sync record core applied itself.
        let applied = usize::from(
            outcome.delivered_payloads.is_empty()
                && outcome.disposition == CoreInboundDisposition::Consumed
                && outcome.work.delivered == 1,
        );
        let sender = outcome.delivered_sender.clone();
        let delivered_msg_id = outcome.commit.as_ref().map(|commit| commit.msg_id.clone());
        for payload in outcome.delivered_payloads {
            if let (Some(sender), Some(msg_id)) = (sender.as_ref(), delivered_msg_id.as_ref()) {
                self.record_delivery(&payload, sender, msg_id.clone());
            }
            self.inbox.push(payload);
        }
        if let Some(commit) = outcome.commit {
            self.store
                .core_commit_inbound_delivery(self.seen.clone(), commit);
        }
        applied
    }

    /// Relay-source counterpart to [`Self::receive`], through the same core
    /// transaction: a fetched row is dispositioned by
    /// [`MessageStore::process_inbound_frame`] with [`CoreInboundSource::Relay`]
    /// — consumed if it opens for us, otherwise durably carried and left unacked
    /// in the relay mailbox.
    fn receive_from_relay(&mut self, envelope: &RelayEnvelope, now: i64) -> CoreInboundDisposition {
        let frame = encode_envelope_frame(
            envelope.msg_id.clone(),
            envelope.hop_ttl,
            envelope.expiry,
            envelope.recipient_hint.clone(),
            envelope.sealed.clone(),
        );
        let outcome = self
            .store
            .process_inbound_frame(
                self.identity.clone(),
                self.seen.clone(),
                CoreInboundSource::Relay,
                frame,
                now,
            )
            .expect("process inbound relay frame");
        let sender = outcome.delivered_sender.clone();
        let delivered_msg_id = outcome.commit.as_ref().map(|commit| commit.msg_id.clone());
        for payload in outcome.delivered_payloads {
            if let (Some(sender), Some(msg_id)) = (sender.as_ref(), delivered_msg_id.as_ref()) {
                self.record_delivery(&payload, sender, msg_id.clone());
            }
            self.inbox.push(payload);
        }
        // DTN D4: commit the deferred `seen`/hidden-evidence bookkeeping now the
        // (infallible, in-memory) delivery has landed, so the disposition
        // returned for the relay ack decision reflects a durably-consumed copy.
        if let Some(commit) = outcome.commit {
            self.store
                .core_commit_inbound_delivery(self.seen.clone(), commit);
        }
        outcome.disposition
    }

    /// One encounter through the production planner. Every argument the sim
    /// supplies is transport-observable: who the peer is, which link this is,
    /// what its DIGEST advertised, what brought us here, and the clock.
    fn plan_meet(
        &self,
        peer_user_id: Vec<u8>,
        peer_address: String,
        peer_known_msg_ids: Vec<Vec<u8>>,
        trigger: CoreSprayTrigger,
        now: i64,
    ) -> CoreMeetOutcome {
        self.store
            .plan_mesh_meet(
                &self.router,
                &self.spray,
                &self.offers,
                CoreMeetRequest {
                    own_user_id: self.user_id(),
                    peer_user_id,
                    peer_address,
                    peer_known_msg_ids,
                    // The sim is a closed, trusted graph -- meetings here
                    // stand in for an identified session, not a spoofable
                    // cleartext HELLO. CARRY-02 is owned by the planner's
                    // `peer_authenticated` flag and the module tests;
                    // flipping this to false would disable digest-confirm for
                    // every sim edge.
                    peer_authenticated: true,
                    peer_capabilities: Some(core_own_capabilities()),
                    trigger,
                    now_ms: now,
                    // One simulated clock: the sim has no NTP and no
                    // monotonic/wall split to preserve.
                    spray_now_ms: now,
                },
            )
            .expect("plan encounter")
    }
}

/// The `recipient_hint`s a peer with `user_id` could match for a still-live
/// envelope: their UserID hashed against each day-number in the expiry window
/// (mirror of `MeshService.recentHintsFor`).
fn recent_hints(user_id: &[u8], now: i64) -> Vec<Vec<u8>> {
    (0..=7)
        .map(|days_ago| compute_recipient_hint(user_id.to_vec(), now - days_ago * MS_PER_DAY))
        .collect()
}

/// The hints one node fetches under: the person's, plus
/// (`specs/multi-device-v1.md` §7) THIS device's own namespace and no
/// sibling's — a device fetches the rows it is the sole true consumer of, and
/// the content of a sibling's rows converges by §8 self-sync (WP4) rather than
/// by every device downloading every other device's mail.
///
/// A node that has never linked has no fleet, so this is exactly
/// [`recent_hints`] over the person, and every single-device scenario below
/// fetches precisely what it fetched before §7 existed.
///
/// The production builder, not a mirror of it: what a phone subscribes to is
/// the thing these gates are about, so a divergence between core's fetch set
/// and the sim's would hide exactly the bug they exist to catch.
fn fetch_hints(node: &SimNode, now: i64) -> Vec<Vec<u8>> {
    dedupe_hints(
        node.store
            .relay_self_hints(node.user_id(), now)
            .expect("relay fetch hints"),
    )
}

/// `count` nodes that are devices of ONE person: one identity, therefore one
/// `user_id` and one set of person keys, with real per-device keypairs and
/// each store told which device it is — the state §9's two-phase activation
/// leaves behind.
///
/// The shared identity is not a shortcut. §6 makes the inbox key
/// person-scoped, so in v1 every device of a person genuinely can open every
/// sibling's mail; a fixture that gave each device its own key would make
/// ACK-MD-1's namespace refusal untestable by hiding it behind a decryption
/// failure that v1 does not have.
///
/// The device keypairs are real because a sender only learns this fleet by
/// verifying its roster ([`teach_roster`]), and a roster of invented ids would
/// not survive DL-1's chain check.
fn linked_devices(count: usize) -> (Vec<SimNode>, Vec<DeviceKeypair>) {
    let identity = generate_identity();
    let devices: Vec<DeviceKeypair> = (0..count).map(|_| generate_device_keypair()).collect();
    let device_ids: Vec<Vec<u8>> = devices.iter().map(|d| d.device_id.clone()).collect();
    let nodes = device_ids
        .iter()
        .map(|device_id| {
            let mut node = SimNode::new();
            node.identity = identity.clone();
            node.router.set_local_user_id(identity.user_id.clone());
            node.store
                .set_own_device_fleet(OwnDeviceFleet {
                    own_device_id: Some(device_id.clone()),
                    device_ids: device_ids.clone(),
                    // The version of the roster `teach_roster` publishes, so
                    // the projection and the document agree about where in
                    // DL-1's ordering this fleet came from.
                    projected_from: RosterVersion {
                        recovery_epoch: 0,
                        seq: 1,
                    },
                })
                .expect("activate this device into its fleet");
            node
        })
        .collect();
    (nodes, devices)
}

/// Teach `sender` who `person` is and which devices they hold, the only way
/// v1 allows: a contact row whose `sign_pk` is the person root (§3), then a
/// person-root-signed genesis roster through the shipped
/// `apply_contact_roster`. Nothing here writes the device table directly — if
/// a DL rule would reject the document, this fixture fails.
fn teach_roster(sender: &SimNode, person: &Identity, devices: &[DeviceKeypair]) {
    sender
        .store
        .upsert_contact(Contact {
            user_id: person.user_id.clone(),
            name: "Linked person".to_string(),
            sign_pk: person.sign_pk.clone(),
            agree_pk: person.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        })
        .expect("the person is a contact");
    assert_eq!(
        sender
            .store
            .apply_contact_roster(signed_roster(person, devices))
            .expect("apply roster")
            .outcome,
        RosterUpdateOutcome::Accepted,
        "the fixture roster must actually land"
    );
}

/// The person-root-signed genesis roster for `devices`.
///
/// Shared by [`teach_roster`], which hands it to a *contact*, and by §8's
/// self-sync, where the same document is the person's OWN roster — the one
/// `core_sync_record_admit` checks a record's author against. One builder, so a
/// sibling and a stranger are looking at identical bytes, which is the point of
/// a roster being a signed document rather than a per-side projection.
fn signed_roster(person: &Identity, devices: &[DeviceKeypair]) -> Roster {
    let certs = devices
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
            recovery_epoch: 0,
            seq: 0,
            devices: certs,
            tombstones: Vec::new(),
            approving_device_id: devices[0].device_id.clone(),
            inbox_key_generation: 0,
            signer_sign_pk: Vec::new(),
            signature: Vec::new(),
        },
        person.sign_sk.clone(),
    )
    .expect("roster signs")
}

/// §6's person-scoped inbox key, as v1 actually has it.
///
/// The person's identity agreement keypair *is* the inbox key in this build:
/// every linked device holds the identity, §6 gives every linked device the
/// inbox key, and WP4 has no linking ceremony of its own to mint a separate one
/// with (§9/WP3 owns that, and WP5 owns rotating it). Naming it here rather
/// than passing `identity.agree_pk` around keeps the seam visible: when the
/// ceremony mints a real generation-N key, this function is the one call site
/// the sim has to change, and `generation` is already the field that will carry
/// it.
fn person_inbox_key(person: &Identity) -> InboxKey {
    InboxKey {
        generation: 0,
        agree_pk: person.agree_pk.clone(),
        agree_sk: person.agree_sk.clone(),
    }
}

/// The roster version a fixture roster names, as a record's `roster_version`.
fn roster_version_of(roster: &Roster) -> RosterVersion {
    RosterVersion {
        recovery_epoch: roster.recovery_epoch,
        seq: roster.seq,
    }
}

/// Author one §8 History record on `device`'s own stream, covering everything
/// this node holds in `chat_id` from `sender_person_id`, and retain it sealed.
///
/// Every step is a shipped primitive: the store pages the history, the store
/// mints the stream position, `core_sign_sync_record` puts this device's
/// signature on it in §3's sync domain, `core_seal_sync_record` seals it to the
/// person's inbox key (SYNC-3 — there is no parameter here that could address
/// it anywhere else), and the store retains the sealed copy so a sibling that
/// is dark for a week is still answerable.
fn author_history_record(
    node: &SimNode,
    device: &DeviceKeypair,
    roster: &Roster,
    inbox_key: &InboxKey,
    chat_id: &[u8],
    sender_person_id: &[u8],
    now: i64,
) -> SyncRecord {
    let payload = node
        .store
        .core_sync_history_page(
            node.user_id(),
            chat_id.to_vec(),
            sender_person_id.to_vec(),
            0,
            SYNC_PAGE_LIMIT,
        )
        .expect("page history");
    assert!(
        !payload.entries.is_empty(),
        "there is nothing to sync if the authoring device holds nothing"
    );
    let stream_seq = node
        .store
        .core_sync_next_stream_seq(device.device_id.clone(), SyncRecordKind::History)
        .expect("next stream position");
    let record = core_sign_sync_record(
        SyncRecord {
            kind: SyncRecordKind::History,
            person_id: node.user_id(),
            author_device_id: Vec::new(),
            roster_version: roster_version_of(roster),
            inbox_key_generation: inbox_key.generation,
            stream_seq,
            timestamp_ms: now,
            payload: core_encode_sync_history(payload).expect("encode history"),
            signature: Vec::new(),
        },
        device.sign_sk.clone(),
    )
    .expect("record signs");
    let sealed = core_seal_sync_record(record.clone(), node.identity.clone(), inbox_key.clone())
        .expect("record seals to the person's own devices");
    assert!(
        node.store
            .core_sync_retain_record(record.clone(), sealed, now)
            .expect("retain"),
        "a freshly minted stream position must be a new slot"
    );
    record
}

/// One SYNC-1 anti-entropy round from `from` to `to`, over BLE frames.
///
/// The shape is the whole point and is worth reading as the spec sentence it
/// implements: `to` states the watermarks it can prove, `from` computes what it
/// owes with the *same* gap function used in the other direction, the planner
/// decides what fits, and each planned record goes out as an ordinary sealed
/// envelope frame. Nothing here waits for anything: `to` never answers, and
/// every input `from` used was already in its own store before the round began.
///
/// Returns how many records were applied.
fn self_sync_round(
    from: &SimNode,
    to: &mut SimNode,
    to_device_id: &[u8],
    roster: &Roster,
    inbox_key: &InboxKey,
    now: i64,
) -> usize {
    let person_id = from.user_id();
    let theirs = to
        .store
        .core_sync_digest(person_id.clone())
        .expect("sibling digest");
    let mine = from
        .store
        .core_sync_digest(person_id.clone())
        .expect("own digest");
    let owed = core_sync_digest_gaps(theirs, mine).expect("what we owe the sibling");
    let records = from
        .store
        .core_sync_backfill_records(owed.clone(), SYNC_PAGE_LIMIT)
        .expect("stored records for those gaps");
    let offers = from.store.core_sync_backfill_offers(records.clone());
    let plan = core_plan_sync_backfill(
        owed,
        offers,
        roster_version_of(roster),
        inbox_key.generation,
        SYNC_ROUND_BUDGET_BYTES,
    );

    let mut applied = 0;
    for step in &plan.steps {
        assert_eq!(
            step.action,
            SyncBackfillAction::Send,
            "the roster has not moved, so nothing needs re-sealing"
        );
        let stored = &records[step.offer_index as usize];
        // Addressed exactly as §7's per-device fan-out addresses any other 1:1
        // envelope, because that is what this is: one copy, for one sibling.
        //
        // The base id is derived rather than random — `core_sync_record_id`
        // names the stream SLOT, so a record re-sent after a dropped link, or
        // re-sealed after a roster change, dedupes against the copy already in
        // flight instead of spending a second relay row. `device_fanout_msg_id`
        // then puts each sibling's copy in its own id namespace, and the hint
        // is that sibling's own daily device hint rather than the person's
        // shared one. Both halves are what ACK-MD-1 needs: a row addressed to
        // the person at large is one every device fetches and none may delete,
        // so a self-sync row sent that way would sit in the mailbox until it
        // expired. Addressed per device, the sibling that consumes it is
        // provably its sole true endpoint consumer and may ack it away.
        let msg_id = device_fanout_msg_id(
            core_sync_record_id(
                person_id.clone(),
                stored.author_device_id.clone(),
                stored.kind,
                stored.stream_seq,
            ),
            to_device_id.to_vec(),
        );
        let frame = encode_envelope_frame(
            msg_id,
            DEFAULT_HOP_TTL,
            default_expiry(now),
            compute_recipient_hint(
                core_device_namespace_id(person_id.clone(), to_device_id.to_vec()),
                now,
            ),
            stored.sealed.clone(),
        );
        applied += to.receive_sync(&frame, now);
    }
    applied
}

/// A digest reduced to the claim two converged devices must agree on: which
/// streams, at which watermark. The serve flag is deliberately excluded — it is
/// a per-device fact (only an author can serve its own stream), so requiring it
/// to match would be asserting that two phones authored the same records.
fn watermarks(digest: &cruisemesh_core::SyncDigest) -> Vec<(Vec<u8>, u8, u64)> {
    let mut out: Vec<(Vec<u8>, u8, u64)> = digest
        .streams
        .iter()
        .map(|stream| {
            (
                stream.author_device_id.clone(),
                stream.kind,
                stream.through_seq,
            )
        })
        .collect();
    out.sort();
    out
}

/// Enough for any fixture here, and small enough that a page cap is still
/// exercised rather than bypassed.
const SYNC_PAGE_LIMIT: u32 = 64;

/// One round's byte budget, matching the foreign-carry spray allowance: a
/// self-sync round is one more lane competing for the same encounter, and
/// giving it an unbounded budget would be the one place in this file where a
/// lane is allowed to monopolize a link.
const SYNC_ROUND_BUDGET_BYTES: u64 = 128 * 1024;

/// Seal `payload` from `from` to the person `to` — the only sealing v1 has for
/// 1:1 mail, and what every row of a per-device fan-out carries.
fn seal_to_person(from: &SimNode, to: &SimNode, payload: &[u8]) -> Vec<u8> {
    seal_message(
        from.identity.clone(),
        to.identity.agree_pk.clone(),
        payload.to_vec(),
    )
    .expect("seal")
}

/// The queued 1:1 envelope a send leaves behind, as
/// `MessageStore::core_outbound_relay_rows` receives it. Composed here rather
/// than read out of the outbound queue because these gates are about what the
/// fan-out does with an envelope, not about how it got queued.
fn outbound_envelope(from: &SimNode, to: &SimNode, sealed: Vec<u8>, now: i64) -> OutboundEnvelope {
    OutboundEnvelope {
        msg_id: generate_msg_id(),
        recipient_user_id: to.user_id(),
        chat_id: to.user_id(),
        sender_user_id: from.user_id(),
        kind: KIND_TEXT,
        lamport: 1,
        timestamp: now,
        hop_ttl: DEFAULT_HOP_TTL,
        expiry: default_expiry(now),
        recipient_hint: compute_recipient_hint(to.user_id(), now),
        sealed,
    }
}

/// One server mailbox row. The relay is intentionally content-blind: it
/// stores the public envelope header plus sealed bytes and routes only by
/// recipient hint.
#[derive(Clone)]
struct RelayEnvelope {
    id: i64,
    msg_id: Vec<u8>,
    hop_ttl: u8,
    expiry: i64,
    recipient_hint: Vec<u8>,
    sealed: Vec<u8>,
}

/// Minimal in-memory relay actor for client-side integration coverage. It
/// models the pieces `mesh_sim` previously skipped: deterministic group
/// fan-out upload, hint-scoped fetch, disposition-driven ack, and server-side
/// dedupe by `msg_id`.
struct RelayActor {
    rows: Vec<RelayEnvelope>,
    next_id: i64,
}

impl RelayActor {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            next_id: 1,
        }
    }

    fn post_group(
        &mut self,
        original_msg_id: Vec<u8>,
        group: &Group,
        hop_ttl: u8,
        expiry: i64,
        sealed: Vec<u8>,
        authored_at: i64,
    ) {
        for row in core_group_fanout_rows(
            original_msg_id,
            group.member_user_ids.clone(),
            hop_ttl,
            expiry,
            sealed,
            authored_at,
        ) {
            self.post_row(
                row.msg_id,
                row.recipient_hint,
                row.hop_ttl,
                row.expiry,
                row.sealed,
            );
        }
    }

    /// Upload one queued 1:1 envelope exactly as a shell would
    /// (`specs/multi-device-v1.md` §7): whatever
    /// `MessageStore::core_outbound_relay_rows` plans for it, posted row for
    /// row, with the same `msg_id` dedupe [`Self::post_group`] gets.
    ///
    /// The sender decides the shape, not the sim: a sender that knows a
    /// multi-device roster plans per-device rows, and one that does not plans
    /// the single person-addressed row it always planned. Nothing here can
    /// address a row the production planner would not have.
    fn post_outbound(&mut self, sender: &SimNode, envelope: OutboundEnvelope) {
        for row in sender
            .store
            .core_outbound_relay_rows(envelope, sender.user_id(), None)
            .expect("plan the outbound relay rows")
        {
            self.post_row(
                row.msg_id,
                row.recipient_hint,
                row.hop_ttl,
                row.expiry,
                row.sealed,
            );
        }
    }

    fn post_row(
        &mut self,
        msg_id: Vec<u8>,
        recipient_hint: Vec<u8>,
        hop_ttl: u8,
        expiry: i64,
        sealed: Vec<u8>,
    ) {
        if self.rows.iter().any(|existing| existing.msg_id == msg_id) {
            return;
        }
        let id = self.next_id;
        self.next_id += 1;
        self.rows.push(RelayEnvelope {
            id,
            msg_id,
            hop_ttl,
            expiry,
            recipient_hint,
            sealed,
        });
    }

    fn holds(&self, msg_id: &[u8]) -> bool {
        self.rows.iter().any(|row| row.msg_id == msg_id)
    }

    /// The stored sealed bytes of one row, for gates that need "still there,
    /// unchanged" rather than only "still there".
    fn sealed_of(&self, msg_id: &[u8]) -> Option<Vec<u8>> {
        self.rows
            .iter()
            .find(|row| row.msg_id == msg_id)
            .map(|row| row.sealed.clone())
    }

    fn poll(&mut self, node: &mut SimNode, now: i64) -> Vec<CoreInboundDisposition> {
        let fetch_hints = fetch_hints(node, now);
        let fetched: Vec<RelayEnvelope> = self
            .rows
            .iter()
            .filter(|row| fetch_hints.contains(&row.recipient_hint))
            .cloned()
            .collect();
        let mut dispositions = Vec::with_capacity(fetched.len());
        let mut ack_inputs = Vec::with_capacity(fetched.len());
        for envelope in fetched {
            let disposition = node.receive_from_relay(&envelope, now);
            dispositions.push(disposition);
            ack_inputs.push(CoreRelayEnvelopeDisposition {
                relay_id: envelope.id,
                msg_id: envelope.msg_id,
                disposition,
                recipient_hint: envelope.recipient_hint,
            });
        }
        let ack_ids = node
            .store
            .core_relay_ack_ids_with_consumed(ack_inputs, node.user_id(), now)
            .expect("select safe relay acknowledgements");
        self.rows.retain(|row| !ack_ids.contains(&row.id));
        dispositions
    }

    fn pending_len(&self) -> usize {
        self.rows.len()
    }

    /// Give every stored row an independently ackable twin: the retried
    /// upload / duplicated fan-out case.
    fn duplicate_every_row(&mut self) {
        for index in 0..self.rows.len() {
            let mut twin = self.rows[index].clone();
            twin.id = self.next_id;
            self.next_id += 1;
            self.rows.push(twin);
        }
    }
}

/// A collection of nodes plus the current round's adjacency (which nodes are
/// in radio contact). Adjacency is symmetric and reset each round to model
/// mobility/churn.
struct Network {
    nodes: Vec<SimNode>,
    adjacency: Vec<Vec<usize>>,
    /// Every frame handed onto a link this run -- the flooding-cost metric.
    transmissions: usize,
    /// The same frames, kept whole. `transmissions` counts airtime; BLOB-01
    /// needs the bytes themselves, because the question it asks is not "how
    /// much went out" but "what was in it". Recorded for every lane the
    /// planner can put on a link -- flood, DIGEST, targeted drain and spray --
    /// so a scan of this vector is a scan of everything the sim ever framed.
    framed: Vec<Vec<u8>>,
}

impl Network {
    fn new(n: usize) -> Self {
        Network {
            nodes: (0..n).map(|_| SimNode::new()).collect(),
            adjacency: vec![Vec::new(); n],
            transmissions: 0,
            framed: Vec::new(),
        }
    }

    fn set_edges(&mut self, edges: &[(usize, usize)]) {
        for adj in &mut self.adjacency {
            adj.clear();
        }
        for &(a, b) in edges {
            self.adjacency[a].push(b);
            self.adjacency[b].push(a);
        }
    }

    /// Seal `payload` from `from` to recipient `to`, wrap it in a fresh §6.4
    /// header, mark our own msg_id seen (a sealed box can't be opened by its
    /// sender), and flood it to `from`'s current neighbors. Mirrors the send
    /// path plus the initial flood.
    fn author_and_flood(&mut self, from: usize, to: usize, payload: &[u8], hop_ttl: u8, now: i64) {
        let recipient_agree_pk = self.nodes[to].identity.agree_pk.clone();
        let recipient_user_id = self.nodes[to].user_id();
        let sealed = seal_message(
            self.nodes[from].identity.clone(),
            recipient_agree_pk,
            payload.to_vec(),
        )
        .expect("seal");
        let msg_id = generate_msg_id();
        self.nodes[from].seen.record(msg_id.clone());
        let frame = encode_envelope_frame(
            msg_id,
            hop_ttl,
            default_expiry(now),
            compute_recipient_hint(recipient_user_id, now),
            sealed,
        );
        self.flood_from(from, frame, now);
    }

    fn author_group_and_flood(
        &mut self,
        from: usize,
        group: Group,
        payload: &[u8],
        hop_ttl: u8,
        now: i64,
    ) {
        let sealed = seal_group_message(
            self.nodes[from].identity.clone(),
            group.clone(),
            payload.to_vec(),
        )
        .expect("group seal");
        let msg_id = generate_msg_id();
        self.nodes[from].seen.record(msg_id.clone());
        let frame = encode_envelope_frame(
            msg_id,
            hop_ttl,
            default_expiry(now),
            compute_recipient_hint(group.id, now),
            sealed,
        );
        self.flood_from(from, frame, now);
    }

    fn author_group_to_relay(
        &self,
        relay: &mut RelayActor,
        from: usize,
        group: &Group,
        payload: &[u8],
        hop_ttl: u8,
        now: i64,
    ) {
        let sealed = seal_group_message(
            self.nodes[from].identity.clone(),
            group.clone(),
            payload.to_vec(),
        )
        .expect("group seal");
        relay.post_group(
            generate_msg_id(),
            group,
            hop_ttl,
            default_expiry(now),
            sealed,
            now,
        );
    }

    /// Deliver `frame` to every neighbor of `origin`, cascading relays through
    /// the round's connected component until quiescent. Per-node dedupe
    /// guarantees termination.
    fn flood_from(&mut self, origin: usize, frame: Vec<u8>, now: i64) {
        let mut queue: VecDeque<(usize, Vec<u8>, usize)> = VecDeque::new();
        for &nb in &self.adjacency[origin] {
            queue.push_back((nb, frame.clone(), origin));
        }
        while let Some((target, frame, from)) = queue.pop_front() {
            self.transmissions += 1;
            self.framed.push(frame.clone());
            let relay = self.nodes[target].receive(&frame, now);
            if let Some(relayed) = relay {
                let neighbors = self.adjacency[target].clone();
                for nb in neighbors {
                    if nb != from {
                        queue.push_back((nb, relayed.clone(), target));
                    }
                }
            }
        }
    }

    /// Transport half of a HELLO encounter: each in-contact pair identifies,
    /// the sender reads the peer's digest off the wire-equivalent store API,
    /// core plans the encounter, and this method only moves the returned
    /// frames. No disposition, digest cadence, spray, exclusion, or
    /// carried-removal policy lives here.
    fn meet(&mut self, now: i64) {
        self.meet_with(now, CoreSprayTrigger::FirstContact);
    }

    /// [`Self::meet`] with an explicit trigger, for the long-lived-link
    /// scenarios where the encounter is a maintenance re-digest rather than a
    /// fresh HELLO.
    fn meet_with(&mut self, now: i64, trigger: CoreSprayTrigger) {
        let n = self.nodes.len();
        for a in 0..n {
            let neighbors = self.adjacency[a].clone();
            for b in neighbors {
                let address = format!("sim:{a}->{b}");
                let peer_user_id = self.nodes[b].user_id();
                // What the peer would put on a DIGEST frame. Fetching it is
                // transport; acting on it is core's.
                let peer_known_msg_ids = self.nodes[b]
                    .store
                    .core_digest_advertised_msg_ids()
                    .expect("peer digest");
                let outcome = {
                    let node = &self.nodes[a];
                    if node.router.user_id_for(address.clone()).is_none() {
                        node.router
                            .on_connected(address.clone(), CoreTransport::Lan);
                        assert!(node.router.on_hello(address.clone(), peer_user_id.clone()));
                    }
                    node.plan_meet(peer_user_id, address, peer_known_msg_ids, trigger, now)
                };
                // The planner's own DIGEST frames go on the radio like any
                // other frame. This sim reads a peer's advertised set straight
                // off its store above (its stand-in for receiving the frame),
                // so here they are only counted and checked for shape -- but
                // they are counted, because they are airtime the encounter
                // planner is now responsible for.
                for frame in &outcome.digest_frames {
                    assert!(
                        matches!(parse_frame(frame.clone()), Ok(Frame::Digest { .. })),
                        "the planner emitted a frame that is not a DIGEST"
                    );
                    self.transmissions += 1;
                    self.framed.push(frame.clone());
                }
                for frame in outcome.targeted_frames.iter().chain(&outcome.spray_frames) {
                    self.transmissions += 1;
                    self.framed.push(frame.clone());
                    // Meetings do not cascade floods; the inbound path may
                    // still return a re-flood frame, which we drop.
                    let _ = self.nodes[b].receive(frame, now);
                }
            }
        }
    }

    fn inbox_len(&self, node: usize) -> usize {
        self.nodes[node].inbox.len()
    }

    fn openers_of(&self, payload: &[u8]) -> Vec<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.inbox.iter().any(|m| m == payload))
            .map(|(i, _)| i)
            .collect()
    }
}

#[test]
fn flood_cascades_across_a_multihop_chain_and_only_the_recipient_opens() {
    // 0 - 1 - 2 - 3, all connected this round. 0 sends to 3, two relays away.
    let mut net = Network::new(4);
    net.set_edges(&[(0, 1), (1, 2), (2, 3)]);
    let msg = b"three hops to dinner";

    net.author_and_flood(0, 3, msg, DEFAULT_HOP_TTL, BASE_NOW);

    assert_eq!(
        net.openers_of(msg),
        vec![3],
        "only the intended recipient opens it"
    );
    assert_eq!(net.inbox_len(3), 1);
}

#[test]
fn hop_ttl_bounds_the_flood() {
    let msg = b"barely made it";

    // Recipient two hops away (0 - 1 - 2). hop_ttl = 1 means "neighbors only":
    // node 1 receives it but has no hops left to forward, so 2 never sees it.
    let mut short = Network::new(3);
    short.set_edges(&[(0, 1), (1, 2)]);
    short.author_and_flood(0, 2, msg, 1, BASE_NOW);
    assert!(
        short.openers_of(msg).is_empty(),
        "hop_ttl=1 can't reach a 2-hop recipient"
    );

    // hop_ttl = 2 gives node 1 exactly one forward, reaching node 2.
    let mut ok = Network::new(3);
    ok.set_edges(&[(0, 1), (1, 2)]);
    ok.author_and_flood(0, 2, msg, 2, BASE_NOW);
    assert_eq!(
        ok.openers_of(msg),
        vec![2],
        "hop_ttl=2 reaches the 2-hop recipient"
    );
}

// This test used to assert the mule dropped its copy in the same `meet()`
// that handed the envelope over — which is delete-on-dispatch, the opposite
// of CARRY-01. It now walks the real sequence: offer, then proof on a later
// encounter. Production Android never had the shortcut
// (`InboundEnvelopeProcessor.drainCarriedEnvelopesTo` is offer-only:
// "Never remove carried on send — digest proof only"); only this simulation
// did, which meant the canonical DTN scenario was verifying the wrong rule.
#[test]
fn a_single_mule_carries_a_message_across_a_time_gap() {
    // The canonical DTN win: sender and recipient are never in contact, but a
    // mule meets each in turn. 0 = sender, 1 = mule, 2 = recipient.
    let mut net = Network::new(3);
    let msg = b"see you at the far end of the ship";

    // Round 1: only 0 and 1 are in range. 0 sends to 2 (not present). 1 can't
    // open it -> relays (no other neighbors) and carries it.
    net.set_edges(&[(0, 1)]);
    net.author_and_flood(0, 2, msg, DEFAULT_HOP_TTL, BASE_NOW);
    assert!(
        net.openers_of(msg).is_empty(),
        "recipient wasn't in range yet"
    );
    assert_eq!(
        net.nodes[1].store.carried_len().unwrap(),
        1,
        "mule is carrying it"
    );

    // Round 2: the mule has moved and now meets the recipient; the sender is
    // gone. The carry-drain hands it over on HELLO.
    let later = BASE_NOW + 30_000;
    net.set_edges(&[(1, 2)]);
    net.meet(later);

    assert_eq!(
        net.openers_of(msg),
        vec![2],
        "the mule delivered it to the recipient"
    );
    // CARRY-01: handing the frame over is not proof the peer stored it. The
    // digest the mule read in THIS encounter was built before the delivery
    // landed, so the durable copy must survive.
    assert_eq!(
        net.nodes[1].store.carried_len().unwrap(),
        1,
        "dispatch is not digest-proof; the mule keeps its copy"
    );

    // Round 3: they meet again. The recipient's digest now names the msg_id it
    // consumed, and that proof — not the earlier send — is what retires the
    // carry.
    net.meet(later + 30_000);
    assert_eq!(
        net.nodes[1].store.carried_len().unwrap(),
        0,
        "mule dropped it once the recipient's digest proved receipt"
    );
}

#[test]
fn an_expired_envelope_is_never_delivered() {
    // 0 - 1 - 2; 0 sends to 2, but the envelope's expiry is already in the past.
    let mut net = Network::new(3);
    net.set_edges(&[(0, 1), (1, 2)]);
    let recipient_agree_pk = net.nodes[2].identity.agree_pk.clone();
    let recipient_user_id = net.nodes[2].user_id();
    let sealed = seal_message(
        net.nodes[0].identity.clone(),
        recipient_agree_pk,
        b"too late".to_vec(),
    )
    .expect("seal");
    let msg_id = generate_msg_id();
    let already_expired = BASE_NOW - 1;
    let frame = encode_envelope_frame(
        msg_id,
        DEFAULT_HOP_TTL,
        already_expired,
        compute_recipient_hint(recipient_user_id, BASE_NOW),
        sealed,
    );

    net.flood_from(0, frame, BASE_NOW);

    assert!(
        net.openers_of(b"too late").is_empty(),
        "expired envelope is dropped, not delivered"
    );
    assert_eq!(
        net.nodes[1].store.carried_len().unwrap(),
        0,
        "and not carried"
    );
}

#[test]
fn fifty_node_dense_flood_delivers_once_and_dedupe_bounds_the_cost() {
    // 50 nodes, all mutually in range this round -- the worst case for flood
    // amplification. The message must reach its recipient, exactly one node
    // (the recipient) may open it, and dedupe must keep total transmissions
    // bounded rather than exploding combinatorially.
    let n = 50;
    let mut net = Network::new(n);
    let edges: Vec<(usize, usize)> = (0..n)
        .flat_map(|a| (a + 1..n).map(move |b| (a, b)))
        .collect();
    net.set_edges(&edges);
    let msg = b"all hands: lifeboat drill at 1400";

    net.author_and_flood(0, 37, msg, DEFAULT_HOP_TTL, BASE_NOW);

    assert_eq!(
        net.openers_of(msg),
        vec![37],
        "exactly the recipient opens it, nobody else"
    );
    // Each node relays a given msg_id at most once (dedupe), to at most n-1
    // neighbors, so transmissions can't exceed n*(n-1). A blow-up would mean
    // the seen-set isn't cutting the flood.
    assert!(
        net.transmissions <= n * (n - 1),
        "flood cost {} exceeded the dedupe bound {}",
        net.transmissions,
        n * (n - 1),
    );
}

#[test]
fn multi_mule_carry_chain_delivers_once_spray_on_connect_exists() {
    // 0 sends to 3. A chain of mules meets pairwise over time -- 0&1, then
    // 1&2, then 2&3 -- but 0 and 3 never share a mule that also meets the
    // other. With spray-on-connect paired to the digest's exact carried
    // `msg_id` set, the envelope should now cross 1->2 and then reach 3.
    let mut net = Network::new(4);
    let msg = b"lost in the relay chain";

    net.set_edges(&[(0, 1)]);
    net.author_and_flood(0, 3, msg, DEFAULT_HOP_TTL, BASE_NOW);
    assert_eq!(
        net.nodes[1].store.carried_len().unwrap(),
        1,
        "mule 1 picked it up"
    );

    net.set_edges(&[(1, 2)]);
    net.meet(BASE_NOW + 10_000);
    assert_eq!(
        net.nodes[2].store.carried_len().unwrap(),
        1,
        "mule 1 sprays it onward to mule 2"
    );

    net.set_edges(&[(2, 3)]);
    net.meet(BASE_NOW + 20_000);

    assert_eq!(
        net.openers_of(msg),
        vec![3],
        "multi-mule carry chain reaches the recipient"
    );
}

#[test]
fn repeated_mule_meetings_do_not_resend_known_carried_envelopes() {
    let mut net = Network::new(4);
    let msg = b"don't keep re-spraying me";

    net.set_edges(&[(0, 1)]);
    net.author_and_flood(0, 3, msg, DEFAULT_HOP_TTL, BASE_NOW);

    net.set_edges(&[(1, 2)]);
    net.meet(BASE_NOW + 10_000);
    let after_first_meet = net.transmissions;

    // Same two mules meet again before the recipient leg. Mule 2 already
    // carries this msg_id, so the exact digest set should suppress a resend.
    net.meet(BASE_NOW + 20_000);
    assert_eq!(
        net.transmissions, after_first_meet,
        "second meeting was fully suppressed"
    );
}

/// True if `needle` appears anywhere in `haystack`.
///
/// The same sliding-window scan `protocol_contract.rs` and `lan_session.rs`
/// use to assert a secret is not legible in bytes that went somewhere. A
/// 32-byte window is long enough that a coincidental hit is not a thing that
/// happens.
fn contains_window(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Slack for whatever `seal_message` adds to a body -- ephemeral key, nonce,
/// tag, framing. Deliberately generous: the bound it widens is there to tell
/// a manifest from a blob, a difference of three orders of magnitude, not to
/// pin the sealer's exact overhead.
const SEAL_OVERHEAD_BYTES: usize = 1024;

/// `BLOB-01`, adversarially: a device sends media over and over, the
/// manifests flood and mule and relay and arrive, and no blob rides with them.
///
/// This is the spec's acceptance criterion ("zero blob bytes observable in
/// any envelope, spray plan, carry queue, or BLE frame under an adversarial
/// test that sends media continuously") asked of the real machinery rather
/// than of the design. Everything below is production code: real identities,
/// real sealing, the real §6.4 codec, real `MessageStore` carry queues, the
/// real spray policy and a real `seal_blob` over a megabyte of plaintext.
///
/// It asks three different questions, because no one of them is sufficient:
///
/// 1. **Shape.** Every envelope this mesh ever framed is manifest-shaped: its
///    sealed body fits the manifest cap. This is the load-bearing one. An
///    envelope body is *sealed*, so looking inside it cannot tell a manifest
///    from a blob — but its length can, and length is what a "just inline the
///    bytes" patch cannot hide.
/// 2. **Refusal.** A blob offered to the message plane directly is refused by
///    the codec and reaches no carry queue. That is what makes (1) a rule
///    rather than an observation about a well-behaved fixture.
/// 3. **Legibility.** No 32-byte window of blob ciphertext, blob plaintext or
///    the blob key appears in anything the mesh framed, carried or uploaded.
///    Deliberately the weakest of the three, and kept for what it does cover:
///    the *unsealed* surfaces — frame headers, recipient hints, msg_ids,
///    DIGEST bodies, relay routing metadata — which is where a
///    derived-from-content identifier would surface.
///
/// What is *not* production is the message kind: `SimNode::record_delivery`
/// synthesizes a `KIND_TEXT` row because the sim seals raw plaintext rather
/// than an encoded `ExtendedMessageBody` (see `author_and_flood`). That is
/// immaterial here — the kind byte lives inside the sealed body, so every
/// frame, carry row and relay row below has exactly the shape a
/// `KIND_ATTACHMENT_MANIFEST` send produces.
#[test]
fn blob_01_media_manifests_mule_while_their_blob_bytes_never_enter_the_mesh() {
    use cruisemesh_core::media::blob::{generate_blob_key, seal_blob};
    use cruisemesh_core::media::manifest::{encode_media_manifest, MediaKind, MediaManifest};
    use cruisemesh_core::media::MEDIA_MANIFEST_MAX_BYTES;

    // An alternating marker no header, digest or ciphertext would produce by
    // accident, over a plaintext far larger than any envelope.
    const MARKER: [u8; 2] = [0xB1, 0x0B];
    let plaintext: Vec<u8> = MARKER.iter().copied().cycle().take(1024 * 1024).collect();

    let mut net = Network::new(4);
    let mut relay = RelayActor::new();
    // Relay rows are scanned as they are posted, not only at the end: a row
    // this node consumes and acks would otherwise leave the mailbox before
    // anything looked at it.
    let mut relay_sealed: Vec<Vec<u8>> = Vec::new();

    // Four sends, not one. "Sends media continuously" is the criterion, and a
    // single pass that happened to leak nothing is not evidence. Each round
    // mints a fresh blob key, so each round carries its own needles.
    for round in 0..4_i64 {
        let now = BASE_NOW + round * 60_000;
        let key = generate_blob_key();
        let blob = seal_blob(&key, &plaintext).expect("seal the blob");
        let body = encode_media_manifest(&MediaManifest {
            blob_id: blob.id,
            blob_key: key.clone(),
            plaintext_bytes: plaintext.len() as u64,
            kind: MediaKind::Photo,
            mime_type: "image/jpeg".to_string(),
            width: 4032,
            height: 3024,
            duration_ms: 0,
            filename: String::new(),
            thumbnail: vec![0x7A; 2048],
            caption: "the whole ship at sunset".to_string(),
        })
        .expect("encode the manifest body");

        // The message plane, over every lane at once: flooded from 0, muled
        // 1 -> 2 -> 3 across time gaps, and posted to the relay in parallel
        // the way a phone with internet does.
        net.set_edges(&[(0, 1)]);
        net.author_and_flood(0, 3, &body, DEFAULT_HOP_TTL, now);
        let sealed = seal_to_person(&net.nodes[0], &net.nodes[3], &body);
        let envelope = outbound_envelope(&net.nodes[0], &net.nodes[3], sealed, now);
        relay.post_outbound(&net.nodes[0], envelope);
        relay_sealed.extend(relay.rows.iter().map(|row| row.sealed.clone()));

        net.set_edges(&[(1, 2)]);
        net.meet(now + 10_000);
        net.set_edges(&[(2, 3)]);
        net.meet(now + 20_000);
        relay.poll(&mut net.nodes[3], now + 30_000);

        assert!(
            net.nodes[3].inbox.iter().any(|payload| payload == &body),
            "round {round}: the manifest must actually arrive -- a plane that \
             carried nothing at all would pass every assertion below"
        );

        // (2) The refusal. The adversary's move is not subtlety, it is
        // sending the blob as mail. Seal the whole ciphertext to the
        // recipient, frame it exactly as `author_and_flood` frames a message,
        // and hand it to a mule.
        let blob_as_mail = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            default_expiry(now),
            compute_recipient_hint(net.nodes[3].user_id(), now),
            seal_to_person(&net.nodes[0], &net.nodes[3], &blob.ciphertext),
        );
        assert!(
            parse_frame(blob_as_mail.clone()).is_err(),
            "round {round}: the codec must refuse a blob-sized sealed body"
        );
        let carried_before = net.nodes[1].store.carried_len().unwrap();
        let refused = net.nodes[1].store.process_inbound_frame(
            net.nodes[1].identity.clone(),
            net.nodes[1].seen.clone(),
            CoreInboundSource::Mesh,
            blob_as_mail,
            now,
        );
        // Rejected, not carried and not re-flooded: the inbound transaction
        // treats an over-cap body as a header that failed local validation,
        // which is the same door a corrupt frame comes to.
        match &refused {
            Err(_) => {}
            Ok(outcome) => {
                assert_eq!(
                    outcome.disposition,
                    CoreInboundDisposition::Rejected,
                    "round {round}: a mule must refuse a blob rather than carry it"
                );
                assert!(
                    outcome.relay_frame.is_none(),
                    "round {round}: nor reflood it"
                );
            }
        }
        assert_eq!(
            net.nodes[1].store.carried_len().unwrap(),
            carried_before,
            "round {round}: a refused blob must leave the carry queue untouched"
        );

        // (1) The shape. Every envelope on the air fits the manifest cap;
        // nothing blob-shaped was ever framed, at any hop, by any lane.
        for (index, frame) in net.framed.iter().enumerate() {
            if let Ok(Frame::Envelope { sealed, .. }) = parse_frame(frame.clone()) {
                assert!(
                    sealed.len() <= MEDIA_MANIFEST_MAX_BYTES + SEAL_OVERHEAD_BYTES,
                    "round {round}: framed envelope {index} carries {} sealed bytes, \
                     over what the largest manifest could ever seal to",
                    sealed.len()
                );
            }
        }

        // (3) Four needles, one per way a blob could leak: its ciphertext at two
        // offsets, its plaintext, and the key that would make any of the
        // above readable.
        let needles: [(&str, Vec<u8>); 4] = [
            ("blob ciphertext (head)", blob.ciphertext[..32].to_vec()),
            (
                "blob ciphertext (mid-blob)",
                blob.ciphertext[500_000..500_032].to_vec(),
            ),
            (
                "blob plaintext",
                MARKER.iter().copied().cycle().take(32).collect(),
            ),
            ("blob key", key.as_bytes().to_vec()),
        ];

        // Everything the mesh has ever held or said, this round and every
        // round before it.
        let mut surfaces: Vec<(String, Vec<u8>)> = net
            .framed
            .iter()
            .enumerate()
            .map(|(index, frame)| (format!("framed frame {index}"), frame.clone()))
            .collect();
        for (index, node) in net.nodes.iter().enumerate() {
            // No hint filter and no exclusions: the whole carry queue, not
            // the slice some peer would have been offered.
            let page = node
                .store
                .carried_envelopes_for_peer_sync(
                    Vec::new(),
                    Vec::new(),
                    now,
                    u64::MAX,
                    u32::MAX,
                    None,
                )
                .expect("walk the whole carry queue");
            for row in page.rows {
                surfaces.push((format!("node {index} carry queue"), row.sealed));
            }
        }
        for (index, sealed) in relay_sealed.iter().enumerate() {
            surfaces.push((format!("relay row {index}"), sealed.clone()));
        }

        for (place, bytes) in &surfaces {
            for (what, needle) in &needles {
                assert!(
                    !contains_window(bytes, needle),
                    "round {round}: {what} is legible in {place}"
                );
            }
        }
    }

    // The quantitative form of the same claim: four one-megabyte sends, and
    // the mesh moved less than one of them in total airtime -- thumbnails and
    // manifests, which is exactly what the message plane is supposed to cost.
    let framed_bytes: usize = net.framed.iter().map(|frame| frame.len()).sum();
    assert!(
        framed_bytes < plaintext.len(),
        "four 1 MiB media sends put {framed_bytes} bytes on the air; \
         one blob alone is {}",
        plaintext.len()
    );
}

#[test]
fn group_message_floods_and_mules_to_every_other_member_with_dedupe() {
    let mut net = Network::new(4);
    let group = Group {
        id: vec![0x44; 16],
        name: "Deck Crew".to_string(),
        member_user_ids: net.nodes.iter().map(|node| node.user_id()).collect(),
        key: vec![0x55; 32],
        metadata_revision: 0,
        metadata_changed_by: Vec::new(),
    };
    for node in &net.nodes {
        node.import_group(group.clone());
    }
    let msg = b"all hands to station bravo";

    net.set_edges(&[(0, 1)]);
    net.author_group_and_flood(0, group, msg, DEFAULT_HOP_TTL, BASE_NOW);
    assert_eq!(
        net.openers_of(msg),
        vec![1],
        "first in-range member opens it"
    );
    assert_eq!(
        net.nodes[1].store.carried_len().unwrap(),
        1,
        "group member keeps carrying after opening"
    );

    net.set_edges(&[(1, 2)]);
    net.meet(BASE_NOW + 10_000);
    assert_eq!(
        net.openers_of(msg),
        vec![1, 2],
        "second member gets it via mule delivery"
    );
    let after_first_meet = net.transmissions;

    net.meet(BASE_NOW + 20_000);
    assert_eq!(
        net.transmissions, after_first_meet,
        "repeat meeting was dedupe-suppressed"
    );

    net.set_edges(&[(2, 3)]);
    net.meet(BASE_NOW + 30_000);
    assert_eq!(
        net.openers_of(msg),
        vec![1, 2, 3],
        "all other group members opened it once"
    );
}

#[test]
fn group_relay_fanout_opens_on_every_member_and_each_copy_is_acked() {
    let mut net = Network::new(3);
    let group = Group {
        id: vec![0x66; 16],
        name: "Family".to_string(),
        member_user_ids: net.nodes.iter().map(SimNode::user_id).collect(),
        key: vec![0x77; 32],
        metadata_revision: 0,
        metadata_changed_by: Vec::new(),
    };
    for node in &net.nodes {
        node.import_group(group.clone());
    }
    let mut relay = RelayActor::new();
    let msg = b"meet at the theater after dinner";

    net.author_group_to_relay(&mut relay, 0, &group, msg, DEFAULT_HOP_TTL, BASE_NOW);
    assert_eq!(
        relay.pending_len(),
        group.member_user_ids.len(),
        "relay stores one independently ackable row per member"
    );

    for node in &net.nodes {
        let own_hint = compute_recipient_hint(node.user_id(), BASE_NOW);
        assert!(
            node.store
                .groups_matching_hint(own_hint, BASE_NOW)
                .expect("legacy hint-only candidates")
                .is_empty(),
            "fan-out is addressed to the member hint, never the group hint"
        );
    }

    for node in &mut net.nodes {
        assert_eq!(
            relay.poll(node, BASE_NOW),
            vec![CoreInboundDisposition::Consumed],
            "the member opens its fan-out copy instead of carrying it"
        );
    }

    assert_eq!(
        net.openers_of(msg),
        vec![0, 1, 2],
        "every group member receives the relay-only message"
    );
    assert_eq!(
        relay.pending_len(),
        0,
        "each per-member row is removed only after its member consumes it"
    );
}

// ---------------------------------------------------------------------------
// Multi-device relay gates (`specs/multi-device-v1.md` §13, WP2).
//
// All three run the same production ack planner every shell calls,
// `MessageStore::core_relay_ack_ids_with_consumed`, through `RelayActor::poll`.
// Nothing here decides an ack; the sim only says who fetched what.
// ---------------------------------------------------------------------------

/// WP2 gate 1: "two-device recipient over relay — first fetcher must not
/// starve the sibling."
///
/// End to end through both production planners: the sender learns the
/// recipient's roster, `core_outbound_relay_rows` decides the fan-out, and
/// `core_relay_ack_ids_with_consumed` decides every deletion.
///
/// Each device subscribes to its OWN namespace only, so each fetches, opens,
/// delivers and deletes exactly one row — and the counts below are exact for
/// that reason: a device that delivered the same logical message twice would be
/// a device paying twice for it, which is precisely the duplicate-delivery
/// failure the own-namespace-only rule exists to prevent.
///
/// The sibling's row is therefore never touched by the first device — not
/// fetched, not opened, not acked — and is still there, byte for byte, on the
/// day the sibling comes online and finds it under its own namespace.
#[test]
fn a_two_device_recipient_over_relay_does_not_starve_the_sibling() {
    let sender = SimNode::new();
    let (mut devices, fleet) = linked_devices(2);
    teach_roster(&sender, &devices[0].identity.clone(), &fleet);
    let mut relay = RelayActor::new();
    let msg = b"the tender leaves from deck 3 at nine";

    let envelope = outbound_envelope(
        &sender,
        &devices[0],
        seal_to_person(&sender, &devices[0], msg),
        BASE_NOW,
    );
    let original_msg_id = envelope.msg_id.clone();
    relay.post_outbound(&sender, envelope);
    assert_eq!(
        relay.pending_len(),
        2,
        "one independently ackable row per recipient device"
    );

    let first_row = device_fanout_msg_id(original_msg_id.clone(), fleet[0].device_id.clone());
    let sibling_row = device_fanout_msg_id(original_msg_id.clone(), fleet[1].device_id.clone());
    assert!(
        !relay.holds(&original_msg_id),
        "ACK-MD-2: no bare person row is uploaded beside the per-device rows"
    );

    let sibling_bytes = relay.sealed_of(&sibling_row).expect("the sibling's row");

    assert_eq!(
        relay.poll(&mut devices[0], BASE_NOW),
        vec![CoreInboundDisposition::Consumed],
        "the first device fetches exactly the one row addressed to its own \
         namespace -- a sibling's row is the sibling's to fetch"
    );
    assert_eq!(
        devices[0]
            .inbox
            .iter()
            .filter(|payload| *payload == msg)
            .count(),
        1,
        "the first device delivers the message exactly once"
    );
    assert!(!relay.holds(&first_row), "it acks its own namespace's row");
    assert!(
        relay.holds(&sibling_row),
        "ACK-MD-1: the sibling's row is not this device's to delete"
    );
    assert_eq!(
        relay.sealed_of(&sibling_row).as_deref(),
        Some(sibling_bytes.as_slice()),
        "and it survives untouched, not merely undeleted"
    );

    // A day later, on a different network, the sibling finds its mail under
    // its OWN namespace -- the id nobody else subscribes to.
    assert_eq!(
        relay.poll(&mut devices[1], BASE_NOW + MS_PER_DAY),
        vec![CoreInboundDisposition::Consumed],
        "the sibling fetches its own row, under its own namespace"
    );
    assert_eq!(
        devices[1]
            .inbox
            .iter()
            .filter(|payload| *payload == msg)
            .count(),
        1,
        "the second device is not starved by the first, and delivers once"
    );
    assert_eq!(
        relay.pending_len(),
        0,
        "each row is deleted by exactly the device it was addressed to"
    );
}

/// WP2 gate 2: "legacy person-addressed row never acked by a multi-device
/// fleet" (ACK-MD-2). A legacy sender uploads one row for the whole person; no
/// device may delete it, and the control at the end shows the withholding is
/// about the fleet and not about the planner having stopped acking.
///
/// The legacy sender is modelled as a sender that never learned the
/// recipient's roster, which is what `core_outbound_relay_rows` turns into the
/// single person-addressed row — the same bytes a build with no §7 in it at
/// all would upload, since that row is copied from the envelope rather than
/// recomputed. It is also what every sender in the field is today.
#[test]
fn a_legacy_person_addressed_row_is_never_acked_by_a_multi_device_fleet() {
    let sender = SimNode::new();
    let (mut devices, _fleet) = linked_devices(2);
    let mut relay = RelayActor::new();
    let msg = b"granddad is on the pier already";

    let envelope = outbound_envelope(
        &sender,
        &devices[0],
        seal_to_person(&sender, &devices[0], msg),
        BASE_NOW,
    );
    let person_row = envelope.msg_id.clone();
    relay.post_outbound(&sender, envelope);
    assert!(
        relay.holds(&person_row),
        "an unrostered recipient is uploaded as the one row it always was"
    );

    for (index, now) in [(0_usize, BASE_NOW), (1, BASE_NOW + MS_PER_DAY)] {
        relay.poll(&mut devices[index], now);
        assert_eq!(
            devices[index]
                .inbox
                .iter()
                .filter(|payload| *payload == msg)
                .count(),
            1,
            "every device of the person receives the legacy row, exactly once"
        );
        assert_eq!(
            relay.pending_len(),
            1,
            "ACK-MD-2: the person's one shared row survives device {index}'s \
             consumption; it ages out on the relay's own clock instead"
        );
    }

    // Control: the identical row addressed to a person with ONE device is
    // still deleted by that device. Legacy fleets see exactly today's
    // behaviour.
    let mut solo = SimNode::new();
    let solo_envelope = outbound_envelope(
        &sender,
        &solo,
        seal_to_person(&sender, &solo, msg),
        BASE_NOW,
    );
    relay.post_outbound(&sender, solo_envelope);
    relay.poll(&mut solo, BASE_NOW);
    assert_eq!(
        relay.pending_len(),
        1,
        "a single-device recipient is the sole true consumer and still acks"
    );
}

/// WP2 gate 3, now real: a BLE-only day converges through §8 self-sync.
///
/// The gate §13 asks for is "a BLE-only day converges via §8 once WP4 lands".
/// It did not, so this test used to stop at "the sibling has nothing" and said
/// so. WP4's SYNC-1 anti-entropy is what closes it, and the shape of the close
/// is the claim worth reading:
///
/// * one message reaches exactly one device over BLE, as before — nothing here
///   makes the radio reach further;
/// * that device's consumption still does not let it delete the relay copy its
///   sibling would otherwise need (ACK-MD-1, the half WP2 owns, unchanged);
/// * and then, with **no relay leg and no second sender encounter**, one
///   digest exchange between the two devices carries the message across. The
///   sibling was never online with anybody but its own sibling, which is
///   exactly the day the gate names.
///
/// WP0 vector `MD-SYNC-BLE-DAY-CONVERGE`
/// (`core/tests/multi_device_contract.rs`) carries the target outcome in the
/// pinned ledger and moves to `implemented: true` with this test.
#[test]
fn a_ble_only_day_converges_through_sibling_self_sync() {
    let sender = SimNode::new();
    let (mut devices, fleet) = linked_devices(2);
    teach_roster(&sender, &devices[0].identity.clone(), &fleet);
    let person_id = devices[0].user_id();
    let msg = b"we docked early, walk down when you can";

    // ONE authored message, fanned out once, from which BOTH legs are built.
    // The BLE frame carries the same `msg_id` and the same sealed bytes as the
    // row the relay is holding for this device — a mule that met the sender
    // hands over the copy addressed to whoever it meets — so the relay copy
    // genuinely duplicates the BLE copy. Two separately authored envelopes
    // would carry two different ids and dedupe against nothing, which is a
    // weaker claim wearing this one's clothes.
    let mut relay = RelayActor::new();
    let envelope = outbound_envelope(
        &sender,
        &devices[0],
        seal_to_person(&sender, &devices[0], msg),
        BASE_NOW,
    );
    let original_msg_id = envelope.msg_id.clone();
    let hop_ttl = envelope.hop_ttl;
    let expiry = envelope.expiry;
    relay.post_outbound(&sender, envelope);

    let own_row = device_fanout_msg_id(original_msg_id.clone(), fleet[0].device_id.clone());
    let sibling_row = device_fanout_msg_id(original_msg_id.clone(), fleet[1].device_id.clone());
    let own_bytes = relay.sealed_of(&own_row).expect("this device's row");
    let sibling_bytes = relay.sealed_of(&sibling_row).expect("the sibling's row");

    // The BLE leg: that copy, delivered to whichever device happened to be in
    // radio range (§6 — constrained paths carry one copy).
    let frame = encode_envelope_frame(
        own_row.clone(),
        hop_ttl,
        expiry,
        compute_recipient_hint(person_id.clone(), BASE_NOW),
        own_bytes,
    );
    devices[0].receive(&frame, BASE_NOW);
    assert_eq!(
        devices[0]
            .inbox
            .iter()
            .filter(|payload| *payload == msg)
            .count(),
        1,
        "the device in radio range delivers it once"
    );
    assert!(
        devices[1].inbox.is_empty(),
        "the radio reached one device, and self-sync has not run yet"
    );

    // Explicitly Seen, not merely "not Consumed": the row this device fetches
    // carries the id it already handled over BLE, so it dedupes — and it is
    // against exactly that disposition, with store evidence saying "we have
    // this one", that ACK-MD-1 has to hold the line for the sibling.
    assert_eq!(
        relay.poll(&mut devices[0], BASE_NOW),
        vec![CoreInboundDisposition::Seen],
        "the relay copy is the same envelope the BLE frame already delivered"
    );
    assert_eq!(
        devices[0]
            .inbox
            .iter()
            .filter(|payload| *payload == msg)
            .count(),
        1,
        "and it is not delivered a second time"
    );
    assert!(
        relay.holds(&sibling_row),
        "the sibling's relay copy is still there for the day it comes online"
    );
    assert_eq!(
        relay.sealed_of(&sibling_row).as_deref(),
        Some(sibling_bytes.as_slice()),
        "untouched, not merely undeleted"
    );

    // --- §8: the two devices meet each other, and only each other -----------
    let roster = signed_roster(&devices[0].identity.clone(), &fleet);
    let inbox_key = person_inbox_key(&devices[0].identity.clone());
    // Both devices are told, once, which roster and which inbox key generation
    // their own sync traffic is admitted against (§4, §6). This is the
    // ceremony's write — WP3's link and WP5's revocation own it in production —
    // and without it the inbound transaction has nothing to check a record
    // against and leaves sync dispatch inert, which is exactly what a v1
    // single-device install wants.
    for device in devices.iter() {
        device
            .store
            .core_set_own_sync_context(roster.clone(), inbox_key.generation)
            .expect("own sync context");
    }
    let sender_id = sender.user_id();

    // The device that received the message writes it into its own author
    // stream. Authoring is local and needs nobody: SYNC-1 forbids assuming the
    // sibling is ever concurrently online, and nothing above this line knows
    // whether it is.
    author_history_record(
        &devices[0],
        &fleet[0],
        &roster,
        &inbox_key,
        &sender_id,
        &sender_id,
        BASE_NOW,
    );

    let (first, rest) = devices.split_at_mut(1);
    let applied = self_sync_round(
        &first[0],
        &mut rest[0],
        &fleet[1].device_id,
        &roster,
        &inbox_key,
        BASE_NOW,
    );
    assert_eq!(applied, 1, "one record answered the sibling's whole gap");

    // ACK-MD-1's evidence, from the side that owes it. A sync record leaves no
    // `messages` row of its own — it carries rows — so the consumed-hidden set
    // is the only thing that can later prove this device took its own fan-out
    // copy and may delete it. Without this the self-sync row would sit in the
    // relay mailbox until it expired, which is the growth half of the
    // multi-device relay problem.
    assert!(
        devices[1]
            .store
            .consumed_hidden_msg_id_count()
            .expect("evidence")
            > 0,
        "a consumed sync record has to leave the one licence that lets its          per-device relay row be acked away"
    );

    let converged = devices[1]
        .store
        .messages_for_chat(sender_id.clone())
        .expect("the sibling's chat with the sender");
    assert_eq!(
        converged.len(),
        1,
        "the sibling converged on a message it never had a radio path to"
    );
    assert_eq!(
        converged[0].payload, msg,
        "and it is the message itself, not a placeholder for one"
    );

    // Convergence is a property of the digests, not only of one lucky row:
    // after the round the two devices advertise the same watermark for the
    // stream, so a second round would move nothing.
    let mine = devices[0]
        .store
        .core_sync_digest(person_id.clone())
        .expect("digest");
    let theirs = devices[1]
        .store
        .core_sync_digest(person_id.clone())
        .expect("digest");
    // The watermarks agree; the *serve* flags deliberately do not. Only the
    // author holds bytes it could re-seal (SYNC-3), so the sibling advertises
    // the same position with `can_serve = false` — which is what stops every
    // other device asking it for records it can never hand over.
    assert_eq!(
        watermarks(&mine),
        watermarks(&theirs),
        "the two views agree on what is held"
    );
    assert!(mine.streams.iter().all(|stream| stream.can_serve));
    assert!(theirs.streams.iter().all(|stream| !stream.can_serve));
    assert!(
        core_sync_digest_gaps(theirs, mine)
            .expect("gaps")
            .is_empty(),
        "nothing is owed either way once the round has landed"
    );

    // The relay copy the sibling no longer needs is still sitting there
    // unacked: SYNC-1 converging the *content* is not a licence to ack a row
    // on the relay's behalf, and nothing in this round touched it.
    assert!(
        relay.holds(&sibling_row),
        "self-sync never acks a sibling's relay row for it"
    );
}

// ---------------------------------------------------------------------------
// Encounter stress: every scenario below drives the production planner
// (`MessageStore::plan_mesh_meet`) rather than re-composing store primitives.
// The simulation has no meet/spray policy of its own left to drift from it.
// ---------------------------------------------------------------------------

/// One courier standing next to one peer, on one named link.
struct Courier {
    courier: SimNode,
    peer: SimNode,
    address: String,
}

impl Courier {
    fn new(address: &str) -> Self {
        let courier = SimNode::new();
        let peer = SimNode::new();
        courier
            .router
            .on_connected(address.to_string(), CoreTransport::Central);
        assert!(courier.router.on_hello(address.to_string(), peer.user_id()));
        Courier {
            courier,
            peer,
            address: address.to_string(),
        }
    }

    /// Fill the courier's carry queue with `count` envelopes for `hint`.
    /// Distinct ciphertext per row: the queue dedupes on the (hint, sealed)
    /// content digest, so identical filler would collapse the backlog instead
    /// of building one.
    fn load(&self, hint: &[u8], count: usize, sealed_len: usize, base: i64) {
        for index in 0..count {
            let mut sealed = vec![0xAB; sealed_len];
            sealed[..2].copy_from_slice(&(index as u16).to_be_bytes());
            self.courier
                .store
                .enqueue_carried_envelope(
                    CarriedEnvelope {
                        msg_id: generate_msg_id(),
                        hop_ttl: DEFAULT_HOP_TTL,
                        expiry: base + 7 * MS_PER_DAY,
                        recipient_hint: hint.to_vec(),
                        sealed,
                    },
                    false,
                    base + index as i64,
                    FOREIGN_BUDGET,
                )
                .expect("enqueue backlog");
        }
    }

    /// One planned encounter, with the peer's real carry set standing in for
    /// its DIGEST. Returns how many envelope frames went over the link.
    fn round(&mut self, trigger: CoreSprayTrigger, now: i64) -> usize {
        // The peer advertises everything it holds. A real DIGEST is capped, so
        // a re-walk toward a peer holding more than the cap legitimately
        // re-offers the un-named remainder; these tests isolate the walk
        // arithmetic from that separate, deliberate redundancy.
        let known = self
            .peer
            .store
            .carried_msg_ids(u64::MAX)
            .expect("peer carried msg ids");
        self.round_with_known(known, trigger, now)
    }

    /// As [`Self::round`], but with an explicitly supplied advertised set --
    /// for modelling a peer whose proof of receipt never arrives.
    fn round_with_known(
        &mut self,
        known: Vec<Vec<u8>>,
        trigger: CoreSprayTrigger,
        now: i64,
    ) -> usize {
        let outcome = self.courier.plan_meet(
            self.peer.user_id(),
            self.address.clone(),
            known,
            trigger,
            now,
        );
        let mut offered = 0;
        for frame in outcome.targeted_frames.iter().chain(&outcome.spray_frames) {
            offered += 1;
            let _ = self.peer.receive(frame, now);
        }
        offered
    }

    /// The link died and the peer came back on another radio under a new
    /// address. The logical-peer carry state is deliberately retained.
    fn relink(&mut self, address: &str, transport: CoreTransport) {
        self.courier.router.on_disconnected(self.address.clone());
        self.courier
            .router
            .on_connected(address.to_string(), transport);
        assert!(self
            .courier
            .router
            .on_hello(address.to_string(), self.peer.user_id()));
        self.address = address.to_string();
    }

    fn carried(&self) -> usize {
        self.courier.store.carried_len().expect("carried len") as usize
    }
}

/// D8 + the per-logical-peer carried cursor: a courier parked next to one peer
/// for hours must hand over its whole backlog and then fall silent.
///
/// The re-digest fires every 3-5 minutes on a long-lived link, and each round
/// offers at most a byte budget's worth of foreign carry. A backlog many times
/// that budget therefore takes several rounds -- but it must take *several*,
/// not forever: before the cursor, every round re-read the queue from its
/// oldest row, so the only thing stopping a round from re-offering the same
/// head was the peer's digest happening to name it. This walks the whole
/// backlog once, delivers every envelope exactly once, and then goes quiet for
/// the rest of the day, across several re-walk cooldowns.
#[test]
fn a_courier_walks_a_backlog_many_times_the_budget_once_and_then_stays_quiet() {
    const SEALED_LEN: usize = 4 * 1024;
    const BACKLOG: usize = 300;
    // The D8 maintenance cadence, which is also the interval `may_spray`
    // enforces for a `Maintenance` trigger.
    const ROUND_MS: i64 = 5 * 60_000;
    // Long enough to cover the receipt-quiet backoff: a courier holding mail
    // for someone who is not here produces no proof of progress, so the spray
    // cadence deliberately stretches. That is the designed cost of muling for
    // an absent recipient -- what must not happen is the walk failing to
    // finish, or a row crossing twice.
    const ROUNDS: usize = 160;

    let mut link = Courier::new("ble:courier-peer");
    let stranger = generate_identity();
    let hint = compute_recipient_hint(stranger.user_id, BASE_NOW);
    link.load(&hint, BACKLOG, SEALED_LEN, BASE_NOW);

    let mut offers_per_round = Vec::new();
    for round in 0..ROUNDS {
        let now = BASE_NOW + (round as i64 + 1) * ROUND_MS;
        offers_per_round.push(link.round(CoreSprayTrigger::Maintenance, now));
    }

    let total_offered: usize = offers_per_round.iter().sum();
    assert_eq!(
        total_offered, BACKLOG,
        "every envelope is offered exactly once across the whole run: no row \
         re-offered, none skipped ({offers_per_round:?})"
    );
    assert_eq!(
        link.peer
            .store
            .carried_msg_ids(u64::MAX)
            .expect("peer carried msg ids")
            .len(),
        BACKLOG,
        "the peer ends up carrying the courier's whole backlog"
    );

    let walk_rounds = offers_per_round
        .iter()
        .rposition(|offers| *offers > 0)
        .expect("the walk must offer something")
        + 1;
    assert!(
        walk_rounds <= 40,
        "1.2 MiB at 256 KiB an allowed round must converge well inside the run          even with the quiet-peer backoff stretching the cadence, took {walk_rounds}"
    );
    assert!(
        offers_per_round[walk_rounds..].iter().all(|n| *n == 0),
        "once converged the lane stays quiet -- including across the re-walk \
         cooldowns this run spans ({offers_per_round:?})"
    );

    // DTN ack safety: offering is not delivery. The courier still holds every
    // envelope; a carried copy is dropped only on digest-proof of receipt.
    assert_eq!(
        link.carried(),
        BACKLOG,
        "a completed walk offers; it never acks or deletes"
    );
}

/// The restore case: a phone comes back from an encrypted backup with a deep
/// carry queue and immediately meets a peer. Every round must stay inside the
/// per-encounter budget -- one 900-row queue may not become one 900-frame
/// burst into a single BLE FIFO -- and the walk must still converge.
#[test]
fn a_restore_with_a_deep_backlog_stays_bounded_per_round_and_still_converges() {
    const SEALED_LEN: usize = 1024;
    const BACKLOG: usize = 900;
    const ROUND_MS: i64 = 5 * 60_000;
    const ROUNDS: usize = 240;
    // CARRIED_SPRAY_BUDGET_BYTES / SEALED_LEN: the most whole envelopes one
    // round's budget can pay for.
    const MAX_FRAMES_PER_ROUND: usize = 256 * 1024 / SEALED_LEN;

    let mut link = Courier::new("ble:restored");
    let stranger = generate_identity();
    let hint = compute_recipient_hint(stranger.user_id, BASE_NOW);
    link.load(&hint, BACKLOG, SEALED_LEN, BASE_NOW);
    assert_eq!(link.carried(), BACKLOG);

    let mut offers_per_round = Vec::new();
    for round in 0..ROUNDS {
        let now = BASE_NOW + (round as i64 + 1) * ROUND_MS;
        let offered = link.round(CoreSprayTrigger::Maintenance, now);
        assert!(
            offered <= MAX_FRAMES_PER_ROUND,
            "round {round} offered {offered} frames, past the encounter budget"
        );
        offers_per_round.push(offered);
    }

    assert_eq!(
        offers_per_round.iter().sum::<usize>(),
        BACKLOG,
        "every restored row crosses exactly once"
    );
    assert_eq!(
        link.carried(),
        BACKLOG,
        "a restore hands its backlog on; it never deletes on dispatch"
    );
}

/// A mega-carrier encounter: one meeting against a queue far larger than any
/// budget must still be a bounded, terminating amount of work.
#[test]
fn a_single_mega_carrier_encounter_is_bounded_and_removes_nothing() {
    const SEALED_LEN: usize = 2 * 1024;
    const BACKLOG: usize = 1_200;

    let mut link = Courier::new("ble:mega-carrier");
    let stranger = generate_identity();
    let hint = compute_recipient_hint(stranger.user_id, BASE_NOW);
    link.load(&hint, BACKLOG, SEALED_LEN, BASE_NOW);

    let offered = link.round(CoreSprayTrigger::FirstContact, BASE_NOW + 1_000);
    assert!(
        offered > 0 && offered <= 256 * 1024 / SEALED_LEN,
        "one encounter offers a budget's worth and no more, got {offered}"
    );
    assert_eq!(
        link.carried(),
        BACKLOG,
        "the carrier is still the carrier afterwards"
    );
}

/// G3: a phone walking into a busy room brings up every link at once. At most
/// two peers may walk this device's carry store per epoch; the rest are
/// deferred, and a deferral costs the queue nothing.
#[test]
fn a_busy_room_bounds_how_many_peers_walk_the_carry_store_per_epoch() {
    let net = Network::new(6);
    let stranger = generate_identity();
    let hint = compute_recipient_hint(stranger.user_id, BASE_NOW);
    for index in 0..8_u16 {
        let mut sealed = vec![0xAB; 512];
        sealed[..2].copy_from_slice(&index.to_be_bytes());
        net.nodes[0]
            .store
            .enqueue_carried_envelope(
                CarriedEnvelope {
                    msg_id: generate_msg_id(),
                    hop_ttl: DEFAULT_HOP_TTL,
                    expiry: BASE_NOW + 7 * MS_PER_DAY,
                    recipient_hint: hint.clone(),
                    sealed,
                },
                false,
                BASE_NOW + i64::from(index),
                FOREIGN_BUDGET,
            )
            .expect("enqueue");
    }

    // Five links come up inside one 5s offer epoch.
    let mut offered_to = 0;
    let mut deferred = 0;
    for peer in 1..6 {
        let address = format!("sim:0->{peer}");
        let peer_user_id = net.nodes[peer].user_id();
        net.nodes[0]
            .router
            .on_connected(address.clone(), CoreTransport::Central);
        assert!(net.nodes[0]
            .router
            .on_hello(address.clone(), peer_user_id.clone()));
        let outcome = net.nodes[0].plan_meet(
            peer_user_id,
            address,
            Vec::new(),
            CoreSprayTrigger::FirstContact,
            BASE_NOW + 100,
        );
        if outcome.work.offer_deferred {
            deferred += 1;
        } else if outcome.work.sprayed > 0 {
            offered_to += 1;
        }
    }

    assert_eq!(offered_to, 2, "the epoch's two offer slots are spent");
    assert_eq!(deferred, 3, "the rest wait for the next epoch");
    assert_eq!(
        net.nodes[0].store.carried_len().unwrap(),
        8,
        "deferring an offer never touches the queue"
    );
}

/// LAN dies mid-walk and the same phone comes back over BLE under a different
/// address. The walk must *continue*, not restart: the cursors belong to the
/// authenticated logical peer precisely so a rotated address cannot multiply
/// one backlog offer.
#[test]
fn a_lan_link_dying_continues_the_carry_walk_over_ble() {
    const SEALED_LEN: usize = 4 * 1024;
    const BACKLOG: usize = 200;
    const ROUND_MS: i64 = 5 * 60_000;

    let mut link = Courier::new("lan:192.0.2.7");
    let stranger = generate_identity();
    let hint = compute_recipient_hint(stranger.user_id, BASE_NOW);
    link.load(&hint, BACKLOG, SEALED_LEN, BASE_NOW);

    let over_lan = link.round(CoreSprayTrigger::FirstContact, BASE_NOW + ROUND_MS);
    assert!(
        over_lan > 0 && over_lan < BACKLOG,
        "the LAN round should walk part of the backlog, got {over_lan}"
    );

    // Wi-Fi drops. The peer is still there, over BLE, at a new address.
    link.relink("ble:5F:2A:1C", CoreTransport::Central);

    let mut over_ble = 0;
    for round in 2..120 {
        over_ble += link.round(CoreSprayTrigger::Maintenance, BASE_NOW + round * ROUND_MS);
    }

    assert_eq!(
        over_lan + over_ble,
        BACKLOG,
        "the BLE half continued the walk: every row crossed exactly once"
    );
    assert_eq!(
        link.peer
            .store
            .carried_msg_ids(u64::MAX)
            .expect("peer carried")
            .len(),
        BACKLOG
    );
    assert_eq!(link.carried(), BACKLOG, "a failover never deletes");
}

/// A partition heals. The message must cross the seam once it exists, and the
/// healed pair must not then spend the rest of the day re-offering what
/// already landed.
#[test]
fn a_partition_heals_and_delivers_without_re_offering_what_landed() {
    let mut net = Network::new(5);
    let msg = b"we were on opposite ends of the ship";

    // Partition A = {0,1,2}, partition B = {3,4}. 0 authors for 4.
    net.set_edges(&[(0, 1), (1, 2), (3, 4)]);
    net.author_and_flood(0, 4, msg, DEFAULT_HOP_TTL, BASE_NOW);
    assert!(
        net.openers_of(msg).is_empty(),
        "the recipient is on the far side of the partition"
    );
    assert_eq!(net.nodes[2].store.carried_len().unwrap(), 1);

    // The seam closes: 2 meets 3.
    net.set_edges(&[(2, 3)]);
    net.meet(BASE_NOW + 10_000);
    assert_eq!(
        net.nodes[3].store.carried_len().unwrap(),
        1,
        "the envelope crossed the healed seam"
    );

    // 3 meets the recipient.
    net.set_edges(&[(3, 4)]);
    net.meet(BASE_NOW + 20_000);
    assert_eq!(net.openers_of(msg), vec![4], "delivered after the heal");

    // The healed pair keeps meeting. Digest exclusion plus the cursor must
    // make that free.
    let after_heal = net.transmissions;
    net.meet(BASE_NOW + 30_000);
    net.meet(BASE_NOW + 40_000);
    assert_eq!(
        net.transmissions, after_heal,
        "a converged pair goes quiet instead of re-offering"
    );
}

/// Receipt loss: the peer really did receive the mail, but its proof never
/// comes back (a lost DIGEST, a stuck watermark). The courier must keep the
/// copy -- removal needs proof -- while the re-offer stays bounded, and it
/// must not fall permanently silent either, because a frame lost in a link's
/// FIFO is only found again by a later pass.
#[test]
fn a_lost_receipt_never_wedges_the_carry_or_floods_the_link() {
    const ROUND_MS: i64 = 5 * 60_000;
    const ROUNDS: i64 = 12;

    let mut link = Courier::new("ble:silent-peer");
    let peer_hint = compute_recipient_hint(link.peer.user_id(), BASE_NOW);
    link.load(&peer_hint, 1, 512, BASE_NOW);

    let mut offers = Vec::new();
    for round in 1..=ROUNDS {
        // The peer never advertises anything: its digest is lost every time.
        offers.push(link.round_with_known(
            Vec::new(),
            CoreSprayTrigger::Maintenance,
            BASE_NOW + round * ROUND_MS,
        ));
    }

    let total: usize = offers.iter().sum();
    assert_eq!(
        link.carried(),
        1,
        "CARRY-01: no proof, no removal -- not even after a dozen offers"
    );
    assert!(
        total <= 3,
        "an unconfirmed carry is re-offered on the re-walk cooldown, not every \
         round ({offers:?})"
    );
    assert!(
        total >= 2,
        "and it does eventually try again: a link-FIFO loss must be \
         recoverable ({offers:?})"
    );
    assert_eq!(
        link.peer.store.carried_len().unwrap(),
        1,
        "the peer's content dedupe makes the repeats free at the receiving end"
    );
}

/// The relay handed us the same envelope twice (a retried upload, a fan-out
/// row duplicated across two hint days). It must open once, and both rows must
/// be safe to ack -- this device really was the sole endpoint consumer of that
/// msg_id.
#[test]
fn a_duplicate_relay_row_opens_once_and_both_copies_are_acked() {
    let mut net = Network::new(3);
    let group = Group {
        id: vec![0x88; 16],
        name: "Muster".to_string(),
        member_user_ids: net.nodes.iter().map(SimNode::user_id).collect(),
        key: vec![0x99; 32],
        metadata_revision: 0,
        metadata_changed_by: Vec::new(),
    };
    for node in &net.nodes {
        node.import_group(group.clone());
    }
    let mut relay = RelayActor::new();
    let msg = b"muster drill, deck seven";
    net.author_group_to_relay(&mut relay, 0, &group, msg, DEFAULT_HOP_TTL, BASE_NOW);
    relay.duplicate_every_row();
    assert_eq!(
        relay.pending_len(),
        2 * group.member_user_ids.len(),
        "every fan-out row now has a twin"
    );

    for node in &mut net.nodes {
        let dispositions = relay.poll(node, BASE_NOW);
        assert_eq!(dispositions.len(), 2, "both copies were fetched");
        assert!(
            dispositions.contains(&CoreInboundDisposition::Consumed),
            "the first copy opens"
        );
    }

    assert_eq!(
        net.openers_of(msg),
        vec![0, 1, 2],
        "every member got the message"
    );
    for node in &net.nodes {
        assert_eq!(node.inbox.len(), 1, "and got it exactly once");
    }
    let residue: Vec<usize> = net
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            let hints = recent_hints(&node.user_id(), BASE_NOW);
            relay
                .rows
                .iter()
                .any(|row| hints.contains(&row.recipient_hint))
        })
        .map(|(index, _)| index)
        .collect();
    // Both twins are acked for every member that *received* the message. The
    // author's own fan-out copy is the one exception, and deliberately so: it
    // has a durable row for that msg_id either way, so "I consumed this
    // envelope" and "I wrote this message" are not distinguishable evidence,
    // and ACK-01 says an ambiguous consumer does not ack. The row ages out on
    // expiry instead. Churn is recoverable; deleting someone else's only copy
    // is not.
    assert_eq!(
        residue,
        vec![0],
        "only the author's own copy is left unacked for want of unambiguous          proof of consumption"
    );
    assert_eq!(
        relay.pending_len(),
        1,
        "every copy a member genuinely consumed is acked, twin included"
    );
}
