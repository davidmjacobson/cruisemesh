//! WP4's gate (`specs/multi-device-v1.md` §13): "two devices, arbitrary
//! interleaved online/offline schedules, converge to identical stores; no
//! double-authoring."
//!
//! ## What this file drives
//!
//! A person's devices, each with its own real store, device keypair and
//! position in one signed roster. Nothing is mocked below the store: a round is
//! a sealed §8 **digest record** the asking device authors, opened and applied
//! by the answering one; a gap set computed from the watermarks that digest
//! carried; a plan; a real seal opened with §6's person inbox key and admitted
//! against the roster; and a real `MessageStore::core_apply_sync_record`.
//! Authoring is `author_pairwise_message`, so the rows a record carries are the
//! rows the app would have.
//!
//! Two things about that are load-bearing. The digest is a record on the wire,
//! not a driver convenience: what makes a device send anything is a sibling's
//! sealed watermarks arriving, so if the digest carrier broke, this file would
//! stop converging rather than keep working off bookkeeping the driver happened
//! to have. And every seal here is made with the **authoring device's** key
//! (`core_device_sync_identity`), never the person root — §14.2 keeps the root
//! inside the encrypted backup, so a fleet that could only seal with the root
//! would be a fleet that never syncs.
//!
//! The schedules are generated from a seed and are deterministic: a failure
//! prints its seed and re-runs identically. Each step picks one of
//!
//! * **compose** — type into a chat's shared draft on one device;
//! * **send** — take *that device's* draft and, subject to SYNC-2's claim,
//!   author it;
//! * **retype and send** — the person picks up the other device and retypes
//!   rather than waiting for the draft, which is the case SYNC-2's claim is for;
//! * **receive** — a contact's message reaching exactly ONE device, which is
//!   §6's constrained-path shape (one person-sealed copy, whoever hears it
//!   first) and the case self-sync exists to repair;
//! * **read** — advance a read watermark on one device;
//! * **meet a new contact** — on one device only;
//! * **re-learn a contact** — a fresh card for somebody already known, on one
//!   device, which is the case a "leave the existing row alone" merge could
//!   never converge;
//! * **make or rename a group** — on one device only;
//! * **collide on a setting** — two devices writing one key at the *same*
//!   epoch, which is the tie a plain last-epoch-wins merge sits forked on
//!   forever;
//! * **anti-entropy round** — one direction, between two devices that are both
//!   online at that step;
//! * **go offline / come back** — the interleaving itself.
//!
//! A round is strictly one-directional and reads only what the sending device
//! had already stored, so nothing in the driver assumes the two devices are
//! concurrently anything (SYNC-1). Offline devices are simply never chosen as a
//! round endpoint.
//!
//! ## The honest coverage envelope
//!
//! Explored: interleavings of the actions above across 2- and 3-device fleets,
//! with per-device online/offline windows, one-directional rounds in both
//! orders, message arrival at one device only, contacts learned and re-learned
//! on one device only, groups made and renamed, same-epoch settings collisions,
//! repeated composition of text a sibling may or may not have already sent,
//! **whole-state and delta harvests**, **round budgets tight enough to truncate
//! a round**, **a roster bump mid-run and the re-seal SYNC-3 forces**, and **a
//! lossy link that drops, duplicates and reorders records inside a round**.
//!
//! **Not** explored here, and each is somebody else's work package rather than
//! a gap left by laziness: inbox key generations beyond 0 and the §10.1
//! rotation that bumps them (WP5 — `core_sync_seal_is_current`'s generation
//! half is pinned in `sync_stream.rs`'s unit tests instead); revoked-device
//! records (WP5); the own-roster record kind, which §6 keeps out of anti-entropy
//! entirely because inbox key custody is a ceremony's (WP3/WP5); contact-facing
//! roster gossip, which is distribution rather than self-sync; and group
//! *crypto*, which §11 leaves untouched in v1.
//!
//! Three further limits are properties of the design rather than of this file,
//! and each is pinned by a named test rather than described in a comment:
//!
//! * [`sync_2_names_the_window_it_cannot_close`] — a sibling can only decline to
//!   re-author something it has **heard about**. The driver therefore asserts
//!   against ground truth (the authoring device's own store, read directly, on
//!   every single send) and separately counts the duplicates that fall inside
//!   the blind window, which the final assertion requires to account for every
//!   duplicate row in the fleet.
//! * [`a_tight_budget_converges_in_more_rounds_not_fewer_records`] — a round
//!   budget bounds bytes, never correctness: a truncated round leaves the
//!   sibling's watermark where it was and the next round asks again. A record
//!   larger than a whole round is the one case that does not recover on its own,
//!   and it is pinned in `sync_stream.rs`'s planner tests rather than here.
//! * [`two_records_at_one_stream_slot_keep_the_first_and_report_the_second_held`]
//!   — a stream slot is a record's identity, so a clone sharing a device key
//!   cannot rewrite what a sibling already applied. That is a guarantee about
//!   the *slot*, not about the clone: §1 names the shared-key clone as a thing
//!   to prevent, and preventing it is §9's link ceremony's job, not this one's.
//!
//! Two of the group actions deserve naming for what they are rather than for
//! what they look like. A rename derives its new name from the group's next
//! revision, so two devices renaming concurrently produce the identical name at
//! the identical revision — because §11 breaks a metadata tie on
//! `(revision, changed_by)` and both of one person's devices sign as the same
//! person, which leaves two genuinely different concurrent renames with no
//! winner. That is a property of the v1 group design and not one WP4 gets to fix
//! from the sync side.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use cruisemesh_core::{
    apply_group_metadata_update, core_device_sync_identity, core_encode_sync_contacts,
    core_encode_sync_digest, core_encode_sync_groups, core_encode_sync_history,
    core_encode_sync_settings, core_encode_sync_watermarks, core_open_sync_record,
    core_plan_sync_backfill, core_seal_sync_record, core_sign_device_cert, core_sign_roster,
    core_sign_sync_record, core_sync_digest_gaps, core_sync_record_kind_wire, create_group,
    create_group_metadata_update, generate_device_keypair, generate_identity, Contact, DeviceCert,
    DeviceKeypair, Identity, InboxKey, MessageStore, OutboundAuthorDecision, OwnDeviceFleet,
    Roster, RosterVersion, StoredMessage, SyncBackfillAction, SyncDigest, SyncHistoryPayload,
    SyncRecord, SyncRecordKind, SyncSettingEntry, DEVICE_CERT_FLAG_ROSTER_SIGNING, KIND_TEXT,
    LEGACY_DEVICE_ID, RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ, SYNC_OUTBOUND_DEDUP_WINDOW_MS,
};

const BASE_NOW: i64 = 1_700_000_000_000;
/// One simulated minute per step. Long enough that a schedule spans hours (so
/// timestamps are distinguishable), short enough that a whole run stays inside
/// SYNC-2's dedup window and the window is therefore never what makes the
/// no-double-author assertion pass.
const STEP_MS: i64 = 60_000;
const PAGE_LIMIT: u32 = 256;
/// Deliberately generous for the ordinary schedules. Truncation is explored by
/// [`a_tight_budget_converges_in_more_rounds_not_fewer_records`] with a budget
/// small enough to cut a round in half; here a truncated round would only make
/// convergence slower to reach and could mask a real divergence behind "we ran
/// out of passes".
const ROUND_BUDGET_BYTES: u64 = 4 * 1024 * 1024;

/// The streams a device harvests. The own roster is deliberately absent: §6
/// keeps inbox key custody with the shell and `set_own_device_fleet` is a
/// ceremony's monotone whole-record write, so anti-entropy re-publishing the own
/// roster is exactly what `sync_store`'s module docs refuse to do. The digest is
/// absent because it is not a stream at all — it is minted per round in
/// [`Fleet::mint_digest`].
const SYNC_KINDS: [SyncRecordKind; 5] = [
    SyncRecordKind::History,
    SyncRecordKind::Watermarks,
    SyncRecordKind::Contacts,
    SyncRecordKind::Groups,
    SyncRecordKind::Settings,
];

// ---------------------------------------------------------------------------
// A seeded, reproducible schedule generator
// ---------------------------------------------------------------------------

/// SplitMix64. Written out rather than pulled in so a schedule is reproducible
/// across toolchains and across whatever a dependency decides to do next.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x5DEE_CE66_D5C3_91C1)
    }

    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    fn percent(&mut self, chance: u64) -> bool {
        self.next_u64() % 100 < chance
    }
}

/// How much of a stream a harvest puts on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Harvest {
    /// Every record carries the device's whole view of the stream. Simple, and
    /// blind in one specific way: a record that never arrives is repaired by
    /// the next one, so a convergence bug in the *transfer* can hide behind the
    /// redundancy.
    Snapshot,
    /// The History stream carries only entries this device has not already put
    /// on it. Each record is then the only copy of what it holds, so losing one
    /// has to be repaired by SYNC-1's watermark arithmetic and by nothing else.
    Delta,
}

/// What a link does to the records a round planned.
///
/// Drops are confined to the schedule (see [`Fleet::converge`]) — a converge
/// pass that could drop records forever is a test that flakes rather than a
/// property. Duplicates and reordering run everywhere, because neither can lose
/// anything and both are exactly what a real DTN hop does.
#[derive(Clone, Copy, Debug, Default)]
struct Lossy {
    drop_percent: u64,
    duplicate_percent: u64,
    reorder_percent: u64,
}

// ---------------------------------------------------------------------------
// The fleet
// ---------------------------------------------------------------------------

struct Node {
    store: MessageStore,
    keys: DeviceKeypair,
    online: bool,
    /// The encoded payload this device last put on each of its own streams, so
    /// a harvest only mints a record when something actually changed. Keyed by
    /// the kind's wire byte because that is what the stream is keyed by.
    last_payload: HashMap<u8, Vec<u8>>,
    /// [`Harvest::Delta`] only: the origin message ids this device has already
    /// put on its own History stream.
    published_history: BTreeSet<Vec<u8>>,
    /// This device's Digest stream position. Digests are never retained, so the
    /// store cannot answer "what comes next" for them and the driver keeps the
    /// counter — which is exactly the shape a shell would keep.
    digest_seq: u64,
}

struct Fleet {
    person: Identity,
    roster: Roster,
    inbox_key: InboxKey,
    nodes: Vec<Node>,
    contacts: Vec<Contact>,
    /// The identity behind each contact, kept so a test can actually seal mail
    /// *as* one of them — a contact whose keys the driver threw away can only
    /// ever be asserted about, never exercised.
    contact_identities: Vec<Identity>,
    /// The contact's own lamport counter per chat: a contact is one sender, so
    /// their stream numbers are shared however many of this person's devices
    /// happen to hear a given message.
    contact_lamport: Vec<u64>,
    /// Group ids the schedule has created, in creation order.
    groups: Vec<Vec<u8>>,
    /// text -> (device index that authored it first, step at which it did).
    authored: HashMap<String, (usize, u64)>,
    /// text -> every device stream it ended up on. SYNC-2 is about one text on
    /// one *stream*; a person saying the same thing twice from one device is a
    /// decision, so only a second device counts as a duplicate.
    authored_devices: HashMap<String, BTreeSet<usize>>,
    /// The last few things the person composed in each chat, on any device.
    /// This is the *person's* memory, not a device's: it is what lets the
    /// schedule model somebody picking up the other phone and retyping what
    /// they were about to say, which is the case SYNC-2's claim exists for and
    /// which a shared draft alone does not cover (a shell may not adopt drafts,
    /// and a person may retype rather than wait for one to arrive).
    last_composed: HashMap<usize, Vec<String>>,
    /// Duplicates the driver has *proved* are inside §8's blind window: a
    /// second device authored the same text while its own store held no
    /// sibling's copy of it.
    blind_window_duplicates: usize,
    step: u64,
    harvest_mode: Harvest,
    lossy: Lossy,
    round_budget: u64,
    /// When set, [`Fleet::run`] never flips a device's online state. The
    /// degenerate "nobody met anybody all day" schedule has to be *fixed*, not
    /// merely likely.
    freeze_online: bool,
    /// True while [`Fleet::converge`] is running, so a round's work lands in the
    /// converge counters instead of the schedule's. Without the split, "the
    /// schedule interleaved rounds with everything else" and "the fleet
    /// eventually finished talking" are one number, and a generator that had
    /// stopped scheduling rounds entirely would still look busy.
    in_converge: bool,
    /// A private stream for the lossy link, seeded per fleet so a lossy run is
    /// as reproducible as a lossless one.
    link_rng: Rng,
    coverage: ScheduleCoverage,
}

/// Counts of the things a schedule has to actually do for the properties above
/// to mean anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ScheduleCoverage {
    authored: usize,
    /// Sends SYNC-2 turned into a draft edit instead of a second stream row.
    sibling_refusals: usize,
    received: usize,
    /// Rounds that moved a record **during the schedule** — the interleaving
    /// itself.
    schedule_rounds_with_work: usize,
    /// Rounds that moved a record during the final drain.
    converge_rounds_with_work: usize,
    records_applied: usize,
    /// Sealed digest records opened and read. Every round is one, so this is
    /// also the round count.
    digests_read: usize,
    records_dropped: usize,
    records_duplicated: usize,
    /// Rounds the byte budget cut short. A budget that never bit would make
    /// [`a_tight_budget_converges_in_more_rounds_not_fewer_records`] a test of
    /// nothing.
    rounds_truncated: usize,
    contacts_relearned: usize,
    group_changes: usize,
    setting_ties: usize,
}

impl ScheduleCoverage {
    fn add(&mut self, other: ScheduleCoverage) {
        self.authored += other.authored;
        self.sibling_refusals += other.sibling_refusals;
        self.received += other.received;
        self.schedule_rounds_with_work += other.schedule_rounds_with_work;
        self.converge_rounds_with_work += other.converge_rounds_with_work;
        self.records_applied += other.records_applied;
        self.digests_read += other.digests_read;
        self.records_dropped += other.records_dropped;
        self.records_duplicated += other.records_duplicated;
        self.rounds_truncated += other.rounds_truncated;
        self.contacts_relearned += other.contacts_relearned;
        self.group_changes += other.group_changes;
        self.setting_ties += other.setting_ties;
    }
}

impl Fleet {
    fn new(device_count: usize) -> Self {
        Fleet::with_seed(device_count, 0)
    }

    fn with_seed(device_count: usize, link_seed: u64) -> Self {
        let person = generate_identity();
        let keys: Vec<DeviceKeypair> = (0..device_count)
            .map(|_| generate_device_keypair())
            .collect();
        let device_ids: Vec<Vec<u8>> = keys.iter().map(|k| k.device_id.clone()).collect();
        let roster = signed_roster(&person, &keys, 0);
        let nodes = keys
            .iter()
            .map(|k| {
                let store = MessageStore::open(":memory:".to_string()).expect("open");
                store
                    .set_own_device_fleet(OwnDeviceFleet {
                        own_device_id: Some(k.device_id.clone()),
                        device_ids: device_ids.clone(),
                        projected_from: RosterVersion {
                            recovery_epoch: roster.recovery_epoch,
                            seq: roster.seq + 1,
                        },
                    })
                    .expect("activate this device into its fleet");
                Node {
                    store,
                    keys: k.clone(),
                    online: true,
                    last_payload: HashMap::new(),
                    published_history: BTreeSet::new(),
                    digest_seq: 0,
                }
            })
            .collect();
        let mut fleet = Fleet {
            // §6 as v1 actually has it: every linked device holds the person
            // identity, so the person's agreement keypair IS the inbox key
            // until WP3's ceremony mints a generation of its own. The same
            // seam `mesh_sim.rs` names.
            inbox_key: InboxKey {
                generation: 0,
                agree_pk: person.agree_pk.clone(),
                agree_sk: person.agree_sk.clone(),
            },
            person,
            roster,
            nodes,
            contacts: Vec::new(),
            contact_identities: Vec::new(),
            contact_lamport: Vec::new(),
            groups: Vec::new(),
            authored: HashMap::new(),
            authored_devices: HashMap::new(),
            last_composed: HashMap::new(),
            blind_window_duplicates: 0,
            step: 0,
            harvest_mode: Harvest::Snapshot,
            lossy: Lossy::default(),
            round_budget: ROUND_BUDGET_BYTES,
            freeze_online: false,
            in_converge: false,
            link_rng: Rng::new(link_seed ^ 0xA5A5_5A5A_A5A5_5A5A),
            coverage: ScheduleCoverage::default(),
        };
        // One contact every device already knows, so the schedule has a chat to
        // work in from step 0 without waiting on a contacts round.
        let (shared, identity) = fleet.new_contact_record("Ash");
        for node in &fleet.nodes {
            node.store.upsert_contact(shared.clone()).expect("contact");
        }
        fleet.contacts.push(shared);
        fleet.contact_identities.push(identity);
        fleet.contact_lamport.push(0);
        fleet
    }

    fn now(&self) -> i64 {
        BASE_NOW + (self.step as i64) * STEP_MS
    }

    fn roster_version(&self) -> RosterVersion {
        RosterVersion {
            recovery_epoch: self.roster.recovery_epoch,
            seq: self.roster.seq,
        }
    }

    fn new_contact_record(&self, name: &str) -> (Contact, Identity) {
        let identity = generate_identity();
        let contact = Contact {
            user_id: identity.user_id.clone(),
            name: format!("{name} {}", self.contacts.len()),
            sign_pk: identity.sign_pk.clone(),
            agree_pk: identity.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        };
        (contact, identity)
    }

    // -- actions ------------------------------------------------------------

    /// Type into a chat's shared draft (SYNC-2: the draft is fleet state).
    fn compose(&mut self, device: usize, chat: usize) {
        let Some(contact) = self.known_contact(device, chat) else {
            return;
        };
        let text = format!("note {}", self.step);
        // A short memory rather than only the newest line: a person retypes
        // what they meant to send, not necessarily the last thing they typed,
        // and a one-slot memory would make the collision SYNC-2 exists for
        // vanishingly rare in a generated schedule.
        let remembered = self.last_composed.entry(chat).or_default();
        remembered.push(text.clone());
        if remembered.len() > 4 {
            remembered.remove(0);
        }
        self.nodes[device]
            .store
            .core_sync_set_chat_draft(contact.user_id, text, self.now() as u64)
            .expect("draft");
    }

    /// Send whatever this device is holding in that chat's composer.
    ///
    /// This is the whole of SYNC-2 in one place: consult the claim, and either
    /// author on this device's stream or leave the stream alone. A shell that
    /// skipped the claim would still "work" — it would just post a second copy
    /// of one message, which is the product bug §8 names.
    fn send(&mut self, device: usize, chat: usize) {
        let Some(contact) = self.known_contact(device, chat) else {
            return;
        };
        let Some(text) = self.nodes[device]
            .store
            .core_sync_chat_draft(contact.user_id.clone())
            .expect("draft")
        else {
            return;
        };
        self.send_text(device, contact, text);
    }

    /// The person picks up the other device and retypes what they were about to
    /// say, instead of waiting for the draft to arrive.
    ///
    /// This is the case SYNC-2's claim is *for*. The shared draft covers the
    /// version of this where the second device waits — its clear and the
    /// authored row ride the same round, so the composer is simply empty by the
    /// time it gets there — and covers nothing at all for a shell that never
    /// adopted drafts or a person who does not wait.
    fn send_remembered(&mut self, device: usize, chat: usize, pick: usize) {
        let Some(contact) = self.known_contact(device, chat) else {
            return;
        };
        let Some(remembered) = self.last_composed.get(&chat) else {
            return;
        };
        if remembered.is_empty() {
            return;
        }
        let text = remembered[pick % remembered.len()].clone();
        self.send_text(device, contact, text);
    }

    fn send_text(&mut self, device: usize, contact: Contact, text: String) {
        let now = self.now();
        let claim = self.nodes[device]
            .store
            .core_sync_outbound_claim(
                self.person.user_id.clone(),
                contact.user_id.clone(),
                KIND_TEXT,
                text.as_bytes().to_vec(),
                now - SYNC_OUTBOUND_DEDUP_WINDOW_MS,
            )
            .expect("claim");

        // Ground truth, read straight out of the store this device is about to
        // author into, by a query that is not the claim's: does a SIBLING's row
        // for this exact text already sit here? Everything below turns on that
        // fact rather than on anything the driver remembered, because a driver
        // that scored itself would pass a SYNC-2 that had stopped working.
        let sibling_row = self.sibling_row_here(device, &contact.user_id, &text);

        if claim.decision == OutboundAuthorDecision::AlreadyAuthoredBySibling {
            self.coverage.sibling_refusals += 1;
            assert!(
                sibling_row.is_some(),
                "SYNC-2 refused a send this device's own store holds no sibling \
                 row for; a refusal has to be evidence, not a guess"
            );
            assert!(
                self.authored_devices[&text]
                    .iter()
                    .any(|author| *author != device),
                "SYNC-2 must only ever refuse on the strength of a SIBLING's \
                 row; a device refused on its own stream would be the app \
                 second-guessing the person"
            );
            // "Edits the draft, not the stream": the composer is emptied and
            // nothing is authored.
            if self.nodes[device]
                .store
                .core_sync_chat_draft(contact.user_id.clone())
                .expect("draft")
                .is_some()
            {
                self.nodes[device]
                    .store
                    .core_sync_set_chat_draft(contact.user_id, String::new(), now as u64 + 1)
                    .expect("clear draft");
            }
            return;
        }

        // About to author. If the sibling's copy is already sitting in this
        // device's own store, this is not a blind window and never was: the
        // evidence was right there and SYNC-2 should have read it. A statistic
        // here would be a way of not noticing.
        assert!(
            sibling_row.is_none(),
            "SYNC-2 violated: device {device} is about to author {text:?} while \
             its own store already holds a sibling's row for it on stream \
             {:?} — the duplicate is a bug, not a window",
            sibling_row
        );

        let first_on_this_device = self
            .authored_devices
            .get(&text)
            .is_none_or(|devices| !devices.contains(&device));
        if let Some(&(first, _)) = self.authored.get(&text) {
            if first != device && first_on_this_device {
                // §8's irreducible window: the sibling's row is genuinely not
                // here, so there was nothing to read. Counted, and the final
                // assertion requires the fleet's duplicate rows to be exactly
                // these.
                self.blind_window_duplicates += 1;
            }
        }

        self.nodes[device]
            .store
            .author_pairwise_message(
                self.person.clone(),
                contact,
                KIND_TEXT,
                text.as_bytes().to_vec(),
                None,
                now,
            )
            .expect("author");
        self.coverage.authored += 1;
        self.authored
            .entry(text.clone())
            .or_insert((device, self.step));
        self.authored_devices
            .entry(text)
            .or_default()
            .insert(device);
    }

    /// Does this device's own store already hold a **sibling's** row for this
    /// exact outbound text? Returns the stream it is on.
    ///
    /// Read through `messages_for_chat` rather than through
    /// `core_sync_outbound_claim`'s own query, deliberately: the point is to
    /// check the claim against the store, and a check that ran the claim's SQL
    /// would only be checking it against itself. The legacy stream is excluded
    /// for the same reason the claim excludes it — §5 files pre-migration rows
    /// there, and those are this person's own history, not a sibling's.
    fn sibling_row_here(&self, device: usize, chat: &[u8], text: &str) -> Option<Vec<u8>> {
        let own = self.nodes[device].keys.device_id.clone();
        self.nodes[device]
            .store
            .messages_for_chat(chat.to_vec())
            .expect("messages")
            .into_iter()
            .find(|message| {
                message.sender_user_id == self.person.user_id
                    && message.kind == KIND_TEXT
                    && message.payload == text.as_bytes()
                    && message.sender_device_id != own
                    && message.sender_device_id != LEGACY_DEVICE_ID.to_vec()
            })
            .map(|message| message.sender_device_id)
    }

    /// A contact's message arriving at exactly one device (§6).
    fn receive(&mut self, device: usize, chat: usize) {
        let Some(contact) = self.known_contact(device, chat) else {
            return;
        };
        self.contact_lamport[chat] += 1;
        let lamport = self.contact_lamport[chat];
        let mut msg_id = vec![0u8; 16];
        msg_id[0] = 0xC0 | (chat as u8 & 0x0F);
        msg_id[1..9].copy_from_slice(&lamport.to_be_bytes());
        self.nodes[device]
            .store
            .insert_incoming_message_from_device(
                StoredMessage {
                    chat_id: contact.user_id.clone(),
                    sender_user_id: contact.user_id.clone(),
                    lamport,
                    timestamp: self.now(),
                    kind: KIND_TEXT,
                    payload: format!("from ash {lamport}").into_bytes(),
                    // A contact is a v1 peer here: no sealed-body device field,
                    // so §5 files them on the reserved legacy stream.
                    sender_device_id: LEGACY_DEVICE_ID.to_vec(),
                },
                None,
                msg_id,
                None,
                None,
            )
            .expect("receive");
        self.coverage.received += 1;
    }

    fn read(&mut self, device: usize, chat: usize) {
        let Some(contact) = self.known_contact(device, chat) else {
            return;
        };
        let highest = self.nodes[device]
            .store
            .messages_for_chat(contact.user_id.clone())
            .expect("messages")
            .iter()
            .filter(|m| m.sender_user_id == contact.user_id)
            .map(|m| m.lamport)
            .max()
            .unwrap_or(0);
        if highest == 0 {
            return;
        }
        for receipt_type in [RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ] {
            self.nodes[device]
                .store
                .record_outgoing_receipt(
                    contact.user_id.clone(),
                    contact.user_id.clone(),
                    receipt_type,
                    highest,
                )
                .expect("receipt");
        }
    }

    /// A new friendship made on ONE device. Every other device must learn it
    /// through the Contacts stream or the fleet has not converged.
    fn meet_contact(&mut self, device: usize) {
        let (contact, identity) = self.new_contact_record("Bo");
        self.nodes[device]
            .store
            .upsert_contact(contact.clone())
            .expect("contact");
        self.contacts.push(contact);
        self.contact_identities.push(identity);
        self.contact_lamport.push(0);
    }

    /// Somebody this person already knows, re-added from a **fresh card** on one
    /// device: a corrected name and a relay endpoint they did not have before.
    ///
    /// This is the case a "leave the local row alone" merge cannot converge —
    /// each device would sit on its own card, refusing the other's, with no
    /// surface anywhere showing that the two disagree. It is also not rare: it
    /// is what happens every time somebody re-scans a friend card because their
    /// relay moved.
    fn relearn_contact(&mut self, device: usize, chat: usize) {
        let Some(contact) = self.known_contact(device, chat) else {
            return;
        };
        let version = self.step;
        self.nodes[device]
            .store
            .upsert_contact(Contact {
                name: format!("{} v{version}", contact.name),
                relay_url: Some(format!("https://relay{version}.example")),
                relay_token: Some(format!("tok{version}")),
                ..contact
            })
            .expect("re-learned contact");
        self.coverage.contacts_relearned += 1;
    }

    /// Make a group on one device, or rename one that exists.
    ///
    /// The new name is derived from the group and its *next* revision rather
    /// than from the step, so two devices that rename concurrently produce the
    /// identical name at the identical revision. That is not a dodge: §11's
    /// metadata rule breaks a tie on `(revision, changed_by)`, and both of this
    /// person's devices sign as the same person, so two genuinely different
    /// concurrent renames at one revision have no winner — a property of the v1
    /// group design that WP4 does not get to fix from the sync side.
    fn touch_group(&mut self, device: usize, pick: usize) {
        if self.groups.is_empty() || pick.is_multiple_of(3) {
            let members = vec![
                self.person.user_id.clone(),
                self.contacts[0].user_id.clone(),
            ];
            let group = create_group(format!("deck {}", self.groups.len()), members)
                .expect("group validates");
            self.nodes[device]
                .store
                .upsert_group(group.clone())
                .expect("group");
            self.groups.push(group.id);
            self.coverage.group_changes += 1;
            return;
        }
        let group_id = self.groups[pick % self.groups.len()].clone();
        let Some(group) = self.nodes[device]
            .store
            .get_group(group_id)
            .expect("group lookup")
        else {
            return;
        };
        let next_revision = group.metadata_revision + 1;
        let update = create_group_metadata_update(
            group.clone(),
            self.person.user_id.clone(),
            format!("deck v{next_revision}"),
            group.member_user_ids.clone(),
        )
        .expect("metadata update");
        if let Some(renamed) =
            apply_group_metadata_update(group, update, self.person.user_id.clone())
                .expect("apply metadata update")
        {
            self.nodes[device]
                .store
                .upsert_group(renamed)
                .expect("renamed group");
            self.coverage.group_changes += 1;
        }
    }

    /// Two devices write one shared setting at the **same epoch**.
    ///
    /// The tie a wall-clock epoch produces in the field: a person toggles a
    /// preference on the phone and on the tablet inside the same minute, both
    /// offline. Under epoch-alone last-writer-wins each device keeps its own
    /// value forever, because neither incoming record is strictly newer, and
    /// nothing on either surface shows that the fleet is split.
    fn collide_setting(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        let epoch = self.now() as u64;
        let key = format!("prefs.tone{}", self.step % 3);
        for (device, value) in [(a, "warm"), (b, "cool")] {
            self.nodes[device]
                .store
                .core_sync_put_setting(SyncSettingEntry {
                    key: key.clone(),
                    value: value.as_bytes().to_vec(),
                    epoch,
                    // Overwritten by the store with this device's own id: no
                    // writer may choose its own place in the tiebreak.
                    author_device_id: LEGACY_DEVICE_ID.to_vec(),
                })
                .expect("setting");
        }
        self.coverage.setting_ties += 1;
    }

    fn known_contact(&self, device: usize, chat: usize) -> Option<Contact> {
        let contact = self.contacts.get(chat)?;
        self.nodes[device]
            .store
            .get_contact(contact.user_id.clone())
            .expect("contact lookup")
    }

    // -- anti-entropy -------------------------------------------------------

    /// Put anything that changed on this device's own streams, sealed and
    /// retained. Returns how many new records were minted.
    ///
    /// Change detection is on the encoded payload, so a device that has learned
    /// nothing since its last record mints nothing — which is what makes the
    /// convergence loop below terminate rather than trading records forever.
    fn harvest(&mut self, device: usize) -> usize {
        let now = self.now();
        let mut minted = 0;
        for kind in SYNC_KINDS {
            let (payload, published) = self.page(device, kind);
            let wire = core_sync_record_kind_wire(kind);
            if self.nodes[device].last_payload.get(&wire) == Some(&payload) {
                continue;
            }
            self.author_record(device, kind, payload.clone(), now);
            self.nodes[device].last_payload.insert(wire, payload);
            self.nodes[device].published_history.extend(published);
            minted += 1;
        }
        minted
    }

    /// Sign, seal and retain one record on this device's own stream.
    fn author_record(&mut self, device: usize, kind: SyncRecordKind, payload: Vec<u8>, now: i64) {
        let record = self.sign_record(device, kind, payload, now, None);
        let sealed = core_seal_sync_record(
            record.clone(),
            core_device_sync_identity(self.nodes[device].keys.clone()),
            self.inbox_key.clone(),
        )
        .expect("SYNC-3: sealed to this person's own devices, by the device that authored it");
        assert!(
            self.nodes[device]
                .store
                .core_sync_retain_record(record, sealed, now)
                .expect("retain"),
            "a freshly minted stream position must be a new slot"
        );
    }

    fn sign_record(
        &self,
        device: usize,
        kind: SyncRecordKind,
        payload: Vec<u8>,
        now: i64,
        stream_seq: Option<u64>,
    ) -> SyncRecord {
        let node = &self.nodes[device];
        let stream_seq = stream_seq.unwrap_or_else(|| {
            node.store
                .core_sync_next_stream_seq(node.keys.device_id.clone(), kind)
                .expect("next stream position")
        });
        core_sign_sync_record(
            SyncRecord {
                kind,
                person_id: self.person.user_id.clone(),
                author_device_id: Vec::new(),
                roster_version: self.roster_version(),
                inbox_key_generation: self.inbox_key.generation,
                stream_seq,
                timestamp_ms: now,
                payload,
                signature: Vec::new(),
            },
            node.keys.sign_sk.clone(),
        )
        .expect("record signs")
    }

    /// The encoded payload for one of this device's streams, right now, plus
    /// the history ids it consumed (empty outside [`Harvest::Delta`]).
    fn page(&self, device: usize, kind: SyncRecordKind) -> (Vec<u8>, Vec<Vec<u8>>) {
        let store = &self.nodes[device].store;
        match kind {
            SyncRecordKind::History => {
                let mut entries = Vec::new();
                for contact in store.list_contacts().expect("contacts") {
                    for sender in [self.person.user_id.clone(), contact.user_id.clone()] {
                        entries.extend(
                            store
                                .core_sync_history_page(
                                    self.person.user_id.clone(),
                                    contact.user_id.clone(),
                                    sender,
                                    0,
                                    PAGE_LIMIT,
                                )
                                .expect("history page")
                                .entries,
                        );
                    }
                }
                let published = if self.harvest_mode == Harvest::Delta {
                    // Only what this device has not already put on its stream.
                    // Filtered by origin id rather than by a lamport cursor
                    // because §5's stream key is (chat, person, DEVICE,
                    // lamport): two of a person's devices legitimately hold
                    // rows at one lamport on different streams, so a cursor
                    // over the merged view would have a tie to get wrong and
                    // would silently skip a row.
                    let already = &self.nodes[device].published_history;
                    entries.retain(|entry| !already.contains(&entry.origin_msg_id));
                    entries
                        .iter()
                        .map(|entry| entry.origin_msg_id.clone())
                        .collect()
                } else {
                    Vec::new()
                };
                (
                    core_encode_sync_history(SyncHistoryPayload { entries })
                        .expect("encode history"),
                    published,
                )
            }
            SyncRecordKind::Watermarks => (
                core_encode_sync_watermarks(
                    store.core_sync_watermark_page(PAGE_LIMIT).expect("page"),
                )
                .expect("encode watermarks"),
                Vec::new(),
            ),
            SyncRecordKind::Contacts => (
                core_encode_sync_contacts(store.core_sync_contacts_page(PAGE_LIMIT).expect("page"))
                    .expect("encode contacts"),
                Vec::new(),
            ),
            SyncRecordKind::Groups => (
                core_encode_sync_groups(store.core_sync_groups_page(PAGE_LIMIT).expect("page"))
                    .expect("encode groups"),
                Vec::new(),
            ),
            SyncRecordKind::Settings => (
                core_encode_sync_settings(store.core_sync_settings_page(PAGE_LIMIT).expect("page"))
                    .expect("encode settings"),
                Vec::new(),
            ),
            SyncRecordKind::OwnRoster | SyncRecordKind::Digest => {
                unreachable!("SYNC_KINDS harvests neither the own roster nor a digest")
            }
        }
    }

    /// Mint the sealed §8 digest record `device` would send to say what it
    /// holds. Never retained: see [`SyncRecordKind::Digest`].
    fn mint_digest(&mut self, device: usize) -> Vec<u8> {
        let now = self.now();
        let digest = self.nodes[device]
            .store
            .core_sync_digest(self.person.user_id.clone())
            .expect("own digest");
        self.nodes[device].digest_seq += 1;
        let seq = self.nodes[device].digest_seq;
        let record = self.sign_record(
            device,
            SyncRecordKind::Digest,
            core_encode_sync_digest(digest).expect("encode digest"),
            now,
            Some(seq),
        );
        core_seal_sync_record(
            record,
            core_device_sync_identity(self.nodes[device].keys.clone()),
            self.inbox_key.clone(),
        )
        .expect("a digest is sealed to the person's own devices like any other record")
        .sealed
    }

    /// One SYNC-1 round, `from` answering what `to` can prove it lacks.
    ///
    /// The exchange is the exchange: `to` seals its watermarks into a digest
    /// record, `from` opens and applies that record and gets the watermarks
    /// back from the store, and only then does `from` know what it owes.
    /// Nothing here reads `to`'s store directly, which is what makes the round
    /// a thing that could happen over a mule a week apart rather than a thing
    /// that needs both devices in one process.
    fn round(&mut self, from: usize, to: usize) -> usize {
        let sealed_digest = self.mint_digest(to);
        let opened =
            core_open_sync_record(sealed_digest, self.inbox_key.clone(), self.roster.clone())
                .expect("SYNC-3 admits a sibling's digest");
        let read = self.nodes[from]
            .store
            .core_apply_sync_record(opened, self.now())
            .expect("apply the digest");
        let theirs = read
            .peer_digest
            .expect("a digest record hands its watermarks back rather than filing them");
        self.coverage.digests_read += 1;

        let mine = self.nodes[from]
            .store
            .core_sync_digest(self.person.user_id.clone())
            .expect("own digest");
        let owed = core_sync_digest_gaps(theirs, mine).expect("what we owe");
        if owed.is_empty() {
            return 0;
        }
        let records = self.nodes[from]
            .store
            .core_sync_backfill_records(owed.clone(), PAGE_LIMIT)
            .expect("stored records");
        let offers = self.nodes[from]
            .store
            .core_sync_backfill_offers(records.clone());
        let plan = core_plan_sync_backfill(
            owed,
            offers,
            self.roster_version(),
            self.inbox_key.generation,
            self.round_budget,
        );
        if !plan.deferred.is_empty() {
            self.coverage.rounds_truncated += 1;
        }
        let now = self.now();
        let mut wire: Vec<Vec<u8>> = Vec::new();
        for step in &plan.steps {
            assert_eq!(
                step.action,
                SyncBackfillAction::Send,
                "a record whose seal is stale must be re-sealed before it goes \
                 out, never sent as it stands"
            );
            wire.push(records[step.offer_index as usize].sealed.clone());
        }
        let wire = self.mangle(wire);

        let mut applied = 0;
        for sealed in wire {
            let record = core_open_sync_record(sealed, self.inbox_key.clone(), self.roster.clone())
                .expect("SYNC-3 admits a sibling's record");
            self.nodes[to]
                .store
                .core_apply_sync_record(record, now)
                .expect("apply");
            applied += 1;
        }
        if applied > 0 {
            if self.in_converge {
                self.coverage.converge_rounds_with_work += 1;
            } else {
                self.coverage.schedule_rounds_with_work += 1;
            }
            self.coverage.records_applied += applied;
        }
        applied
    }

    /// What the link does to a round's records: drop, duplicate, reorder.
    ///
    /// A duplicate has to be free (SYNC-1 re-offers records whenever a round is
    /// cut short, so re-arrival is the normal case) and a reorder has to be
    /// harmless (every merge in `sync_store` is order-independent, and the
    /// contiguous cursor advances separately). Both are asserted by the
    /// convergence assertions rather than by anything here — this function only
    /// makes the link stop being perfect.
    fn mangle(&mut self, records: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        if self.lossy.drop_percent == 0
            && self.lossy.duplicate_percent == 0
            && self.lossy.reorder_percent == 0
        {
            return records;
        }
        let mut out: Vec<Vec<u8>> = Vec::with_capacity(records.len());
        for record in records {
            // Drops are confined to the schedule: a converge pass that could
            // lose records forever is a flaky test, not a property.
            if !self.in_converge && self.link_rng.percent(self.lossy.drop_percent) {
                self.coverage.records_dropped += 1;
                continue;
            }
            let duplicate = self.link_rng.percent(self.lossy.duplicate_percent);
            out.push(record.clone());
            if duplicate {
                self.coverage.records_duplicated += 1;
                out.push(record);
            }
        }
        for index in 1..out.len() {
            if self.link_rng.percent(self.lossy.reorder_percent) {
                out.swap(index - 1, index);
            }
        }
        out
    }

    /// Run the schedule generated by `seed` for `steps` steps.
    fn run(&mut self, seed: u64, steps: u64) {
        let mut rng = Rng::new(seed);
        let devices = self.nodes.len();
        for _ in 0..steps {
            self.step += 1;
            // The interleaving itself: a device drops off and comes back on its
            // own schedule, and nothing waits for it.
            if !self.freeze_online && rng.percent(12) {
                let d = rng.below(devices);
                self.nodes[d].online = !self.nodes[d].online;
            }
            let chat = rng.below(self.contacts.len());
            let device = rng.below(devices);
            match rng.below(100) {
                0..=19 => self.compose(device, chat),
                20..=34 => self.send(device, chat),
                35..=47 => {
                    let pick = rng.below(8);
                    self.send_remembered(device, chat, pick)
                }
                48..=59 => self.receive(device, chat),
                60..=68 => self.read(device, chat),
                69..=72 => self.meet_contact(device),
                73..=75 => self.relearn_contact(device, chat),
                76..=79 => {
                    let pick = rng.below(8);
                    self.touch_group(device, pick)
                }
                80..=83 => {
                    let other = rng.below(devices);
                    self.collide_setting(device, other)
                }
                _ => {
                    let from = device;
                    let to = rng.below(devices);
                    if from != to && self.nodes[from].online && self.nodes[to].online {
                        self.harvest(from);
                        self.round(from, to);
                    }
                }
            }
        }
    }

    /// Everybody comes back and the fleet is allowed to finish talking.
    fn converge(&mut self) {
        let devices = self.nodes.len();
        for node in &mut self.nodes {
            node.online = true;
        }
        self.in_converge = true;
        let mut passes = 0;
        loop {
            self.step += 1;
            let mut work = 0;
            for device in 0..devices {
                work += self.harvest(device);
            }
            for from in 0..devices {
                for to in 0..devices {
                    if from != to {
                        work += self.round(from, to);
                    }
                }
            }
            if work == 0 {
                break;
            }
            passes += 1;
            assert!(
                passes < 64,
                "anti-entropy did not settle: a round that keeps finding work \
                 after everything has been exchanged is a convergence bug, not \
                 a slow fleet"
            );
        }
        self.in_converge = false;
    }

    /// SYNC-3's re-seal, driven: the roster moves, and every record this fleet
    /// is still holding for a sibling has to be re-authored against the roster
    /// it actually has before it may go out again.
    ///
    /// Re-*seal* alone would not be enough and the code says so: a record
    /// commits to its `roster_version` inside the device signature, so the
    /// record is re-signed at the same stream slot and re-sealed around that.
    /// The slot — and therefore `core_sync_record_id`, and therefore the relay
    /// row — is unchanged, which is the whole reason that id names the slot and
    /// not the bytes.
    fn bump_roster(&mut self) {
        let keys: Vec<DeviceKeypair> = self.nodes.iter().map(|node| node.keys.clone()).collect();
        self.roster = signed_roster(&self.person, &keys, self.roster.seq + 1);
    }

    /// How many of this device's retained records are stale under the current
    /// roster — i.e. how many a round would have to re-seal rather than send.
    fn stale_seal_count(&self, device: usize) -> usize {
        let owed = self.everything_owed(device);
        let records = self.nodes[device]
            .store
            .core_sync_backfill_records(owed.clone(), PAGE_LIMIT)
            .expect("stored records");
        let offers = self.nodes[device]
            .store
            .core_sync_backfill_offers(records.clone());
        let plan = core_plan_sync_backfill(
            owed,
            offers,
            self.roster_version(),
            self.inbox_key.generation,
            self.round_budget,
        );
        plan.steps
            .iter()
            .filter(|step| step.action == SyncBackfillAction::Reseal)
            .count()
    }

    /// Re-author and re-seal every retained record of `device` at its existing
    /// slot, against the roster this fleet holds now.
    fn reseal_all(&mut self, device: usize) -> usize {
        let owed = self.everything_owed(device);
        let records = self.nodes[device]
            .store
            .core_sync_backfill_records(owed, PAGE_LIMIT)
            .expect("stored records");
        let now = self.now();
        let mut resealed = 0;
        for stored in records {
            let old = core_open_sync_record(
                stored.sealed.clone(),
                self.inbox_key.clone(),
                self.roster.clone(),
            )
            .expect("this device can always open what it sealed");
            let record = self.sign_record(device, old.kind, old.payload, now, Some(old.stream_seq));
            let sealed = core_seal_sync_record(
                record.clone(),
                core_device_sync_identity(self.nodes[device].keys.clone()),
                self.inbox_key.clone(),
            )
            .expect("re-seals");
            assert!(
                self.nodes[device]
                    .store
                    .core_sync_reseal_record(record, sealed)
                    .expect("reseal"),
                "a re-seal replaces a row it must already have"
            );
            resealed += 1;
        }
        resealed
    }

    /// The stream slots this device is holding sealed bytes for. A re-seal must
    /// leave this list untouched: the slot is what `core_sync_record_id` names,
    /// and therefore what a relay row costs.
    fn record_ids(&self, device: usize) -> Vec<Vec<u8>> {
        let owed = self.everything_owed(device);
        let mut ids: Vec<Vec<u8>> = self.nodes[device]
            .store
            .core_sync_backfill_records(owed, PAGE_LIMIT)
            .expect("stored records")
            .into_iter()
            .map(|stored| {
                cruisemesh_core::core_sync_record_id(
                    self.person.user_id.clone(),
                    stored.author_device_id,
                    stored.kind,
                    stored.stream_seq,
                )
            })
            .collect();
        ids.sort();
        ids
    }

    /// A gap set covering every stream this device can serve, from zero — "hand
    /// me all of your own records", which is what a freshly linked sibling asks
    /// for and what the re-seal walk needs.
    fn everything_owed(&self, device: usize) -> Vec<cruisemesh_core::SyncGap> {
        let mine = self.nodes[device]
            .store
            .core_sync_digest(self.person.user_id.clone())
            .expect("own digest");
        core_sync_digest_gaps(
            SyncDigest {
                person_id: self.person.user_id.clone(),
                streams: Vec::new(),
            },
            mine,
        )
        .expect("gaps")
    }

    // -- assertions ---------------------------------------------------------

    /// Every device's stores are the same store.
    fn assert_converged(&self, seed: u64) {
        let devices = self.nodes.len();
        let chats: Vec<Vec<u8>> = self.contacts.iter().map(|c| c.user_id.clone()).collect();

        for chat in &chats {
            let reference = message_set(&self.nodes[0].store, chat);
            for device in 1..devices {
                assert_eq!(
                    message_set(&self.nodes[device].store, chat),
                    reference,
                    "seed {seed}: device {device} holds a different history \
                     than device 0"
                );
            }
        }

        for chat in &chats {
            for sender in chats.iter().chain(std::iter::once(&self.person.user_id)) {
                for receipt_type in [RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ] {
                    let reference = self.nodes[0]
                        .store
                        .outgoing_receipt_through(chat.clone(), sender.clone(), receipt_type)
                        .expect("watermark");
                    for device in 1..devices {
                        assert_eq!(
                            self.nodes[device]
                                .store
                                .outgoing_receipt_through(
                                    chat.clone(),
                                    sender.clone(),
                                    receipt_type
                                )
                                .expect("watermark"),
                            reference,
                            "seed {seed}: read state diverged on device {device}"
                        );
                    }
                }
            }
        }

        let reference = contact_set(&self.nodes[0].store);
        for device in 1..devices {
            assert_eq!(
                contact_set(&self.nodes[device].store),
                reference,
                "seed {seed}: device {device} knows a different set of people"
            );
        }

        let reference = group_set(&self.nodes[0].store);
        for device in 1..devices {
            assert_eq!(
                group_set(&self.nodes[device].store),
                reference,
                "seed {seed}: device {device} holds a different set of groups"
            );
        }

        let reference = settings_map(&self.nodes[0].store);
        for device in 1..devices {
            assert_eq!(
                settings_map(&self.nodes[device].store),
                reference,
                "seed {seed}: shared settings diverged on device {device}"
            );
        }

        // The strongest form of "the same store", and the one that catches a
        // transfer bug a whole-state harvest would paper over: after
        // convergence every device advertises the same watermark for every
        // stream in the fleet, so a further round would find nothing to move on
        // any pairing, in any direction.
        let reference = watermarks(&self.nodes[0].store, &self.person.user_id);
        for device in 1..devices {
            assert_eq!(
                watermarks(&self.nodes[device].store, &self.person.user_id),
                reference,
                "seed {seed}: device {device} advertises different SYNC-1 \
                 watermarks, so the fleet agrees on content by luck rather than \
                 by anti-entropy"
            );
        }
    }

    /// No text is on the wire twice, except where §8 could not have known.
    fn assert_no_double_author(&self, seed: u64) {
        let mut duplicates = 0;
        for contact in &self.contacts {
            // Distinct STREAMS per text, not rows per text: §8's rule is "an
            // outgoing message is authored once, by one device, in that
            // device's stream". Two rows of one text on one stream are a person
            // saying the same thing twice, which is theirs to do and not
            // SYNC-2's business.
            let mut seen: HashMap<Vec<u8>, BTreeSet<Vec<u8>>> = HashMap::new();
            for message in self.nodes[0]
                .store
                .messages_for_chat(contact.user_id.clone())
                .expect("messages")
            {
                if message.sender_user_id != self.person.user_id {
                    continue;
                }
                assert_ne!(
                    message.sender_device_id,
                    LEGACY_DEVICE_ID.to_vec(),
                    "seed {seed}: a linked device authored onto the legacy \
                     stream; §5's device dimension is what keeps two of one \
                     person's devices from forking each other"
                );
                seen.entry(message.payload)
                    .or_default()
                    .insert(message.sender_device_id);
            }
            duplicates += seen
                .values()
                .map(|streams| streams.len() - 1)
                .sum::<usize>();
        }
        assert_eq!(
            duplicates, self.blind_window_duplicates,
            "seed {seed}: the fleet holds duplicate outbound rows that SYNC-2 \
             should have prevented — every duplicate must be one the driver \
             proved was authored while the authoring device's own store held no \
             sibling copy"
        );
    }
}

/// One stored message reduced to everything two stores must agree on:
/// `(sender person, sender device, lamport, timestamp, kind, payload)`. The
/// autoincrement id and the arrival metadata are deliberately absent — they are
/// per-device facts, and requiring them to match would be asserting that two
/// phones took the same route.
type MessageFingerprint = (Vec<u8>, Vec<u8>, u64, i64, u8, Vec<u8>);

/// A contact as the fleet must agree on it: `(person id, name, signing key,
/// agreement key, relay url, relay token)`.
///
/// The relay fields belong here and their absence was a hole. A contact whose
/// endpoint converged on one device and not another is a fleet where one phone
/// can reach somebody and the other cannot — the exact failure §8 lists
/// "endpoints" among the things self-sync carries in order to prevent. The
/// nickname stays out: it is deliberately local and never rides any wire
/// format.
type ContactFingerprint = (
    Vec<u8>,
    String,
    Vec<u8>,
    Vec<u8>,
    Option<String>,
    Option<String>,
);

/// A group as the fleet must agree on it: `(id, name, sorted members, key,
/// metadata revision)`.
///
/// The revision is in here rather than excluded as a per-device detail, because
/// it is the field the store's own group upsert breaks a name conflict on: two
/// devices that agreed on a name while disagreeing about the revision would
/// disagree about the *next* rename, silently, and only one of them would take
/// it.
type GroupFingerprint = (Vec<u8>, String, Vec<Vec<u8>>, Vec<u8>, u64);

/// A chat's whole content as an order-independent set, so two stores are
/// compared on what they hold rather than on the autoincrement id order they
/// happen to have written it in.
fn message_set(store: &MessageStore, chat: &[u8]) -> BTreeSet<MessageFingerprint> {
    store
        .messages_for_chat(chat.to_vec())
        .expect("messages")
        .into_iter()
        .map(|m| {
            (
                m.sender_user_id,
                m.sender_device_id,
                m.lamport,
                m.timestamp,
                m.kind,
                m.payload,
            )
        })
        .collect()
}

fn contact_set(store: &MessageStore) -> BTreeSet<ContactFingerprint> {
    store
        .list_contacts()
        .expect("contacts")
        .into_iter()
        .map(|c| {
            (
                c.user_id,
                c.name,
                c.sign_pk,
                c.agree_pk,
                c.relay_url,
                c.relay_token,
            )
        })
        .collect()
}

fn group_set(store: &MessageStore) -> BTreeSet<GroupFingerprint> {
    store
        .list_groups()
        .expect("groups")
        .into_iter()
        .map(|g| {
            let mut members = g.member_user_ids;
            members.sort();
            (g.id, g.name, members, g.key, g.metadata_revision)
        })
        .collect()
}

fn settings_map(store: &MessageStore) -> BTreeMap<String, (Vec<u8>, u64)> {
    store
        .core_sync_settings_page(PAGE_LIMIT)
        .expect("settings")
        .entries
        .into_iter()
        .map(|entry| (entry.key, (entry.value, entry.epoch)))
        .collect()
}

/// One device's SYNC-1 view, reduced to the claim every device must agree on:
/// which streams exist and how far each is held. The serve flag is excluded
/// because it is a per-device fact — only a stream's author holds bytes it
/// could re-seal — so requiring it to match would be requiring two phones to
/// have authored the same records.
fn watermarks(store: &MessageStore, person_id: &[u8]) -> Vec<(Vec<u8>, u8, u64)> {
    let mut out: Vec<(Vec<u8>, u8, u64)> = store
        .core_sync_digest(person_id.to_vec())
        .expect("digest")
        .streams
        .into_iter()
        .map(|stream| (stream.author_device_id, stream.kind, stream.through_seq))
        .collect();
    out.sort();
    out
}

/// The person-root-signed roster for `devices` at `seq`, as
/// `core_sync_record_admit` checks a record's author against.
///
/// The root signs here because this is the fixture standing in for §14.2's
/// encrypted backup, which is the one place a real deployment keeps it. Nothing
/// the *devices* do below uses it.
fn signed_roster(person: &Identity, devices: &[DeviceKeypair], seq: u64) -> Roster {
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
            seq,
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

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// §13's WP4 gate, stated exactly: two devices, arbitrary interleaved
/// online/offline schedules, identical stores, no double-authoring.
#[test]
fn two_devices_converge_under_every_explored_schedule() {
    let mut coverage = ScheduleCoverage::default();
    for seed in 0..32u64 {
        let mut fleet = Fleet::new(2);
        fleet.run(seed, 120);
        fleet.converge();
        fleet.assert_converged(seed);
        fleet.assert_no_double_author(seed);
        coverage.add(fleet.coverage);
    }
    assert_schedules_did_real_work(coverage, 5);
}

/// The generator has to have actually exercised each lane, or every assertion
/// above is a tautology about an empty store. The thresholds are deliberately
/// far below what the current seeds produce — this is a tripwire against a
/// generator that stops generating, not a second pinned fixture.
///
/// `min_refusals` is a parameter rather than a constant because SYNC-2's
/// refusal needs a sibling's row to have *arrived*, and a link that drops a
/// quarter of everything produces fewer of them. Lowering it for the lossy run
/// is honest; removing it would let the lane go quiet unnoticed.
fn assert_schedules_did_real_work(coverage: ScheduleCoverage, min_refusals: usize) {
    assert!(coverage.authored > 100, "{coverage:?}");
    assert!(coverage.received > 100, "{coverage:?}");
    assert!(coverage.records_applied > 400, "{coverage:?}");
    assert!(coverage.digests_read > 100, "{coverage:?}");
    assert!(coverage.contacts_relearned > 10, "{coverage:?}");
    assert!(coverage.group_changes > 10, "{coverage:?}");
    assert!(coverage.setting_ties > 10, "{coverage:?}");
    // The two round counters are separate on purpose: a generator that had
    // stopped interleaving rounds with the other actions would still converge
    // in the final drain and would still look busy on a single total.
    assert!(
        coverage.schedule_rounds_with_work > 50,
        "the schedule itself never interleaved a productive round with the \
         other actions, so the online/offline windows were never really \
         exercised: {coverage:?}"
    );
    assert!(coverage.converge_rounds_with_work > 20, "{coverage:?}");
    assert!(
        coverage.sibling_refusals >= min_refusals,
        "no schedule ever reached SYNC-2's refusal, so the property tests never \
         exercised the mechanism they exist to cover: {coverage:?}"
    );
}

/// The same generator with three devices. Not what the gate asks for, and run
/// anyway: two devices can hide an ordering bug that only shows when a record
/// reaches a device through a sibling that is not its author — which is the
/// case `core_sync_backfill_records`'s own-authored-only rule decides, and
/// therefore the one most worth exercising against a real schedule.
#[test]
fn three_devices_converge_under_every_explored_schedule() {
    let mut coverage = ScheduleCoverage::default();
    for seed in 100..116u64 {
        let mut fleet = Fleet::new(3);
        fleet.run(seed, 120);
        fleet.converge();
        fleet.assert_converged(seed);
        fleet.assert_no_double_author(seed);
        coverage.add(fleet.coverage);
    }
    assert_schedules_did_real_work(coverage, 5);
}

/// The same schedules with **delta** history records: each record carries only
/// what its device has not already published, so no record is a re-statement of
/// another and losing one has to be repaired by SYNC-1's arithmetic alone.
///
/// This is the blindness a whole-state harvest leaves. Under a snapshot every
/// record contains the entire history, so a fleet whose *transfer* was broken
/// would still converge on the first record that happened to land, and the
/// convergence assertions would pass while proving nothing about gap-fill.
#[test]
fn delta_records_converge_under_every_explored_schedule() {
    let mut coverage = ScheduleCoverage::default();
    for seed in 200..224u64 {
        let mut fleet = Fleet::new(3);
        fleet.harvest_mode = Harvest::Delta;
        fleet.run(seed, 120);
        fleet.converge();
        fleet.assert_converged(seed);
        fleet.assert_no_double_author(seed);
        coverage.add(fleet.coverage);
    }
    assert_schedules_did_real_work(coverage, 5);
}

/// A link that loses, repeats and reorders inside a round.
///
/// A round is not a transaction and never was: a BLE link drops mid-encounter,
/// a mule hands over what it happens to be carrying, and a relay returns rows
/// in whatever order its cursor produced. All three have to be survivable, and
/// the two that cannot lose anything — duplicates and reordering — have to be
/// survivable *silently*, because SYNC-1 re-offers records as its ordinary way
/// of recovering.
#[test]
fn a_lossy_link_still_converges_and_a_repeat_costs_nothing() {
    let mut coverage = ScheduleCoverage::default();
    for seed in 300..316u64 {
        let mut fleet = Fleet::with_seed(3, seed);
        fleet.harvest_mode = Harvest::Delta;
        fleet.lossy = Lossy {
            drop_percent: 25,
            duplicate_percent: 20,
            reorder_percent: 30,
        };
        fleet.run(seed, 120);
        fleet.converge();
        fleet.assert_converged(seed);
        fleet.assert_no_double_author(seed);
        coverage.add(fleet.coverage);
    }
    assert!(
        coverage.records_dropped > 20 && coverage.records_duplicated > 20,
        "the link was supposed to be unreliable: {coverage:?}"
    );
    assert_schedules_did_real_work(coverage, 1);
}

/// A round budget small enough to cut most rounds in half.
///
/// The budget bounds bytes and never correctness: a truncated round leaves the
/// sibling's watermark exactly where it was, so the next round asks for the
/// same run again and the fleet converges in more rounds rather than in fewer
/// records. `core_plan_sync_backfill`'s own tests own the arithmetic; what this
/// asserts is that the arithmetic composes into a fleet that still finishes.
#[test]
fn a_tight_budget_converges_in_more_rounds_not_fewer_records() {
    for seed in 400..408u64 {
        let mut generous = Fleet::with_seed(2, seed);
        generous.harvest_mode = Harvest::Delta;
        generous.run(seed, 100);
        generous.converge();
        generous.assert_converged(seed);

        let mut tight = Fleet::with_seed(2, seed);
        tight.harvest_mode = Harvest::Delta;
        // Enough for any single record and not for a round's worth of them.
        // Deliberately not smaller: a budget under one record's size stalls
        // that stream permanently, which is correct behaviour
        // (`core_plan_sync_backfill`'s own tests pin it) and is a different
        // property from "a truncated round still converges".
        tight.round_budget = TIGHT_ROUND_BUDGET_BYTES;
        tight.run(seed, 100);
        tight.converge();
        tight.assert_converged(seed);
        tight.assert_no_double_author(seed);
        assert!(
            tight.coverage.rounds_truncated > 0,
            "the budget was supposed to cut a round: {:?}",
            tight.coverage
        );
        assert!(
            generous.coverage.rounds_truncated == 0,
            "the generous budget was supposed to never bite, so the difference              between the two runs is the budget and nothing else"
        );
    }
}

/// Small enough that a round carrying several records is cut, large enough that
/// no single record is stranded. See
/// [`a_tight_budget_converges_in_more_rounds_not_fewer_records`].
const TIGHT_ROUND_BUDGET_BYTES: u64 = 4 * 1024;

/// SYNC-3's re-seal, driven end to end: the roster moves under a fleet that is
/// still holding records for a sibling, every retained record goes stale, and
/// nothing may go out until it has been re-authored against the roster the
/// fleet actually has.
///
/// The claim this makes testable is the one that was previously only a comment.
/// A record commits to its `roster_version` inside the device signature, so a
/// re-seal is a re-*sign* at the same stream slot — and the slot, and therefore
/// `core_sync_record_id`, and therefore the relay row, is unchanged.
#[test]
fn a_roster_bump_mid_run_forces_a_reseal_at_the_same_slots() {
    let mut fleet = Fleet::new(2);
    fleet.harvest_mode = Harvest::Delta;
    fleet.run(11, 60);

    // Everybody harvests, so both devices are holding sealed records.
    fleet.step += 1;
    for device in 0..2 {
        fleet.harvest(device);
    }
    let before: Vec<Vec<Vec<u8>>> = (0..2).map(|device| fleet.record_ids(device)).collect();
    assert!(
        before.iter().all(|ids| !ids.is_empty()),
        "the fixture has to actually be holding records for the bump to matter"
    );
    for device in 0..2 {
        assert_eq!(
            fleet.stale_seal_count(device),
            0,
            "nothing is stale before the roster moves"
        );
    }

    fleet.bump_roster();

    let stale: usize = (0..2).map(|device| fleet.stale_seal_count(device)).sum();
    assert!(
        stale > 0,
        "SYNC-3: every record sealed for the old roster is stale the moment the \
         roster moves, and the planner has to say so rather than send it"
    );

    let mut resealed = 0;
    for device in 0..2 {
        resealed += fleet.reseal_all(device);
        assert_eq!(
            fleet.stale_seal_count(device),
            0,
            "a re-sealed record is current again"
        );
    }
    assert!(resealed > 0);

    let after: Vec<Vec<Vec<u8>>> = (0..2).map(|device| fleet.record_ids(device)).collect();
    assert_eq!(
        before, after,
        "a re-seal keeps the stream slot, so the relay row it would spend is \
         the row it already spent"
    );

    fleet.converge();
    fleet.assert_converged(11);
    fleet.assert_no_double_author(11);
}

/// A fleet that never syncs during the schedule still converges once it does.
///
/// The degenerate interleaving — everybody offline from everybody, for the
/// whole day — is the one a scheduler is least likely to produce and the one
/// SYNC-1's standing constraint is actually about. It is *fixed* here rather
/// than hoped for: with the online state frozen, device 1 is offline for every
/// one of the schedule's steps, and the assertion below proves no round did any
/// work before the drain.
#[test]
fn a_fleet_that_never_met_all_day_converges_on_the_first_encounter() {
    let mut fleet = Fleet::new(2);
    fleet.freeze_online = true;
    fleet.nodes[1].online = false;
    fleet.run(7, 120);
    assert_eq!(
        fleet.coverage.schedule_rounds_with_work, 0,
        "the whole point of this schedule is that the two devices never met"
    );
    assert_eq!(fleet.coverage.digests_read, 0);
    assert!(
        fleet.coverage.authored > 0 && fleet.coverage.received > 0,
        "and that they were both busy while not meeting: {:?}",
        fleet.coverage
    );

    fleet.converge();
    assert!(fleet.coverage.converge_rounds_with_work > 0);
    fleet.assert_converged(7);
    fleet.assert_no_double_author(7);
}

/// SYNC-2, driven straight through: the phone sends, the tablet hears about it,
/// and the tablet does not send it again.
#[test]
fn a_sibling_that_has_heard_the_send_does_not_author_it_again() {
    let mut fleet = Fleet::new(2);
    fleet.step = 1;
    fleet.compose(0, 0);
    fleet.step += 1;
    // The draft reaches the tablet, which is what "send from whichever device
    // is in hand" needs.
    fleet.harvest(0);
    fleet.round(0, 1);
    let contact = fleet.contacts[0].user_id.clone();
    assert_eq!(
        fleet.nodes[1]
            .store
            .core_sync_chat_draft(contact.clone())
            .expect("draft"),
        fleet.nodes[0]
            .store
            .core_sync_chat_draft(contact.clone())
            .expect("draft"),
        "the composer is fleet state, not device state"
    );

    fleet.step += 1;
    fleet.send(0, 0);
    fleet.step += 1;
    fleet.harvest(0);
    fleet.round(0, 1);

    fleet.step += 1;
    fleet.send(1, 0);
    assert_eq!(
        fleet.blind_window_duplicates, 0,
        "the tablet had heard; there is no window to blame"
    );
    fleet.converge();
    fleet.assert_no_double_author(0);
    let authored: Vec<StoredMessage> = fleet.nodes[1]
        .store
        .messages_for_chat(contact)
        .expect("messages")
        .into_iter()
        .filter(|m| m.sender_user_id == fleet.person.user_id)
        .collect();
    assert_eq!(authored.len(), 1, "one message, one author, one stream");
    assert_eq!(
        authored[0].sender_device_id, fleet.nodes[0].keys.device_id,
        "and it is on the stream of the device that actually sent it"
    );
}

/// The window SYNC-2 cannot close, pinned so nobody later mistakes it for a bug
/// or for a guarantee.
///
/// A sibling declines to re-author only what it has **heard about**. If the
/// tablet holds a draft and the phone sends without the tablet ever taking a
/// round, the tablet has no way to know — no lock is available to a fleet that
/// may never be concurrently online (SYNC-1). Both copies then converge to both
/// devices, which is the honest outcome: one recipient sees the message twice
/// rather than either device losing a message it believes it sent.
#[test]
fn sync_2_names_the_window_it_cannot_close() {
    let mut fleet = Fleet::new(2);
    fleet.step = 1;
    fleet.compose(0, 0);
    fleet.step += 1;
    fleet.harvest(0);
    fleet.round(0, 1);

    fleet.step += 1;
    fleet.send(0, 0);
    // No round. The tablet still holds the draft and has heard nothing.
    fleet.step += 1;
    fleet.send(1, 0);
    assert_eq!(
        fleet.blind_window_duplicates, 1,
        "this is the blind window, and the driver has to see it as one"
    );

    fleet.converge();
    fleet.assert_converged(0);
    // And the fleet still converges — the duplicate is a product wart, never a
    // fork and never a lost message.
    fleet.assert_no_double_author(0);
}

/// Two devices write one setting at the same epoch, and both orders of meeting
/// produce the same answer.
///
/// Both orders is the point. A merge that only converged when the "right"
/// device spoke first would pass a one-directional test and leave a real fleet
/// forked on whichever phone happened to be in a pocket.
#[test]
fn a_same_epoch_settings_collision_converges_whichever_way_the_devices_meet() {
    let mut outcomes = Vec::new();
    for (first, second) in [(0usize, 1usize), (1, 0)] {
        let mut fleet = Fleet::new(2);
        fleet.step = 1;
        let epoch = fleet.now() as u64;
        for (device, value) in [(0usize, "warm"), (1usize, "cool")] {
            fleet.nodes[device]
                .store
                .core_sync_put_setting(SyncSettingEntry {
                    key: "prefs.tone".to_string(),
                    value: value.as_bytes().to_vec(),
                    epoch,
                    author_device_id: LEGACY_DEVICE_ID.to_vec(),
                })
                .expect("setting");
        }
        assert_ne!(
            settings_map(&fleet.nodes[0].store),
            settings_map(&fleet.nodes[1].store),
            "the fixture has to start genuinely split, or there is no tie"
        );

        fleet.step += 1;
        fleet.harvest(first);
        fleet.round(first, second);
        fleet.harvest(second);
        fleet.round(second, first);
        fleet.converge();
        fleet.assert_converged(0);

        let settled = settings_map(&fleet.nodes[0].store);
        let value = settled
            .get("prefs.tone")
            .expect("the setting survived")
            .0
            .clone();
        assert!(value == b"warm" || value == b"cool");
        // Which device's own value won is a function of the device ids, which
        // are freshly generated per fleet, so the assertion is that BOTH
        // devices agree — not which one.
        outcomes.push(value);
    }
    assert_eq!(outcomes.len(), 2);
}

/// A contact re-added from a fresh card converges rather than forking, and the
/// relay endpoint travels with it.
///
/// The failure this replaces is quiet: with "leave the local row alone", the
/// phone keeps the old card and the tablet keeps the new one, both believe they
/// are right, and the first symptom is a message that will not send from one of
/// them.
#[test]
fn a_re_learned_contact_converges_endpoint_and_all() {
    let mut fleet = Fleet::new(2);
    fleet.step = 1;
    let contact = fleet.contacts[0].clone();
    fleet.nodes[1]
        .store
        .upsert_contact(Contact {
            name: "Ash (new phone)".to_string(),
            relay_url: Some("https://relay-new.example".to_string()),
            relay_token: Some("tok-new".to_string()),
            ..contact.clone()
        })
        .expect("re-learned");
    assert_ne!(
        contact_set(&fleet.nodes[0].store),
        contact_set(&fleet.nodes[1].store)
    );

    fleet.converge();
    fleet.assert_converged(0);
    let settled = fleet.nodes[0]
        .store
        .get_contact(contact.user_id.clone())
        .expect("contact")
        .expect("still there");
    assert_eq!(
        settled.relay_url,
        fleet.nodes[1]
            .store
            .get_contact(contact.user_id)
            .expect("contact")
            .expect("still there")
            .relay_url,
        "an endpoint that converged on one device and not the other is a fleet \
         where one phone can reach somebody and the other cannot"
    );
}

/// Blocking is a person-level decision: it converges to the sibling through the
/// Settings stream, and the sibling actually refuses the blocked person's mail
/// afterwards.
///
/// The second half is what makes this worth a test. A converged *row* proves
/// nothing — the property is a converged **refusal** — so this runs a real
/// sealed envelope from the blocked contact through the sibling's own inbound
/// transaction and asserts it is consumed without ever being delivered.
#[test]
fn a_block_converges_and_the_siblings_inbound_gate_refuses_the_mail() {
    use cruisemesh_core::{
        compute_recipient_hint, default_expiry, encode_envelope_frame,
        encode_message_body_extended, generate_msg_id, seal_message, CoreInboundSource,
        MessageBody, SeenIds, DEFAULT_HOP_TTL,
    };
    use std::sync::Arc;

    let mut fleet = Fleet::new(2);
    fleet.step = 1;
    let blocked = fleet.contacts[0].clone();
    let blocked_identity = fleet.contact_identities[0].clone();

    // The phone blocks, locally and then for the fleet.
    fleet.nodes[0]
        .store
        .block_user(blocked.user_id.clone(), fleet.now())
        .expect("block");
    assert!(fleet.nodes[0]
        .store
        .core_sync_publish_block_list(fleet.now() as u64)
        .expect("publish"));
    assert!(
        !fleet.nodes[1]
            .store
            .is_user_blocked(blocked.user_id.clone())
            .expect("blocked"),
        "the tablet has heard nothing yet"
    );

    // A blocked person is not re-offered by the contacts page either, or the
    // fleet would keep re-seeding somebody it had just dropped.
    assert!(fleet.nodes[0]
        .store
        .core_sync_contacts_page(PAGE_LIMIT)
        .expect("page")
        .entries
        .iter()
        .all(|entry| entry.person_id != blocked.user_id));

    fleet.converge();
    assert!(
        fleet.nodes[1]
            .store
            .is_user_blocked(blocked.user_id.clone())
            .expect("blocked"),
        "blocking on the phone has to be blocking on the tablet"
    );

    // The refusal is real: this contact seals an ordinary message to the person
    // and the tablet's inbound transaction consumes it without delivering it.
    let body = encode_message_body_extended(
        MessageBody {
            kind: KIND_TEXT,
            chat_id: blocked.user_id.clone(),
            lamport: 99,
            timestamp: fleet.now(),
            content: b"still here".to_vec(),
        },
        None,
        None,
        None,
    )
    .expect("body encodes");
    let sealed = seal_message(blocked_identity, fleet.person.agree_pk.clone(), body)
        .expect("a blocked contact can still seal; the refusal is on this side");
    let frame = encode_envelope_frame(
        generate_msg_id(),
        DEFAULT_HOP_TTL,
        default_expiry(fleet.now()),
        compute_recipient_hint(fleet.person.user_id.clone(), fleet.now()),
        sealed,
    );
    let outcome = fleet.nodes[1]
        .store
        .process_inbound_frame(
            fleet.person.clone(),
            Arc::new(SeenIds::new()),
            CoreInboundSource::Mesh,
            frame,
            fleet.now(),
        )
        .expect("inbound");
    assert!(
        outcome.dropped_blocked,
        "the tablet learned the block through self-sync and has to act on it"
    );
    assert!(outcome.delivered_payloads.is_empty());
}

/// A group made on one device, and renamed on it afterwards, arrives whole on
/// the sibling — key, membership, name **and** the metadata revision that
/// decides the next rename.
///
/// The revision is the part worth a test of its own. §11 leaves group crypto
/// alone in v1 and the invite format has no revision field, so it is tempting
/// to carry only the invite bytes; a fleet that did would converge on the first
/// rename and on nothing after it, because every device that had renamed the
/// group would correctly refuse a record claiming revision 0. The failure is
/// invisible from either surface: two phones, two names, both certain.
#[test]
fn a_group_and_its_renames_converge_whole() {
    let mut fleet = Fleet::new(2);
    fleet.step = 1;
    fleet.touch_group(0, 0);
    fleet.converge();
    fleet.assert_converged(0);

    for round in 0..3 {
        fleet.step += 1;
        // Renamed on alternating devices, so the second rename has to beat a
        // revision the *other* device wrote.
        fleet.touch_group(round % 2, 1);
        fleet.converge();
        fleet.assert_converged(0);
    }

    let renamed = fleet.nodes[0]
        .store
        .list_groups()
        .expect("groups")
        .remove(0);
    assert_eq!(
        renamed.metadata_revision, 3,
        "three renames landed, one after another, rather than the fleet          sticking at the first"
    );
    assert_eq!(renamed.name, "deck v3");
}

/// Two devices that share a device key — the restored-`.cmbak`-onto-a-live-phone
/// clone §1 names as the #1 prerequisite — write different content into the
/// same stream slot. The slot is the record's identity, so the second one is
/// held rather than merged.
///
/// The alternative would be worse in both directions. Merging would let a clone
/// rewrite history its sibling had already applied; treating it as a conflict
/// would quarantine a stream on a re-arrival SYNC-1 produces routinely, since a
/// round cut short re-offers exactly the slots the far side already has. Held is
/// the answer that is safe under both, and it is pinned here because it is the
/// behaviour a "smarter" merge would quietly break.
#[test]
fn two_records_at_one_stream_slot_keep_the_first_and_report_the_second_held() {
    use cruisemesh_core::{SyncApplyOutcome, SyncSettingsPayload};

    let mut fleet = Fleet::new(2);
    fleet.step = 1;

    let settings = |value: &str, epoch: u64, author: Vec<u8>| {
        core_encode_sync_settings(SyncSettingsPayload {
            entries: vec![SyncSettingEntry {
                key: "prefs.tone".to_string(),
                value: value.as_bytes().to_vec(),
                epoch,
                author_device_id: author,
            }],
        })
        .expect("encode")
    };
    let author = fleet.nodes[0].keys.device_id.clone();
    let first = fleet.sign_record(
        0,
        SyncRecordKind::Settings,
        settings("warm", 9, author.clone()),
        fleet.now(),
        Some(1),
    );
    // The clone: the same key, the same slot, different bytes.
    let second = fleet.sign_record(
        0,
        SyncRecordKind::Settings,
        settings("cool", 99, author),
        fleet.now(),
        Some(1),
    );
    assert_ne!(first.payload, second.payload);

    let applied = fleet.nodes[1]
        .store
        .core_apply_sync_record(first, fleet.now())
        .expect("apply");
    assert_eq!(applied.outcome, SyncApplyOutcome::Applied);

    let clone = fleet.nodes[1]
        .store
        .core_apply_sync_record(second, fleet.now())
        .expect("apply");
    assert_eq!(
        clone.outcome,
        SyncApplyOutcome::AlreadyHeld,
        "a slot is a record's identity; a second document claiming it is not a \
         merge and not a conflict"
    );
    assert_eq!(clone.applied_entries, 0);
    assert_eq!(
        settings_map(&fleet.nodes[1].store)
            .get("prefs.tone")
            .expect("stored")
            .0,
        b"warm".to_vec(),
        "the clone's payload never touched the store, epoch 99 and all"
    );
}

/// The inbound transaction owns the whole §8 dispatch, including the refusals.
///
/// Three things are asserted together because they are one decision: a record
/// whose outer signature is not this person's own device set is **consumed**
/// (this device was the envelope's endpoint and the record is finished with, so
/// its relay copy must ack away rather than refetch and re-fail forever), never
/// **applied**, and never **vouched for** — that evidence row is a standing
/// licence to delete a relay copy, and nothing was taken.
#[test]
fn the_inbound_transaction_refuses_a_sync_record_from_outside_the_roster() {
    use cruisemesh_core::{
        compute_recipient_hint, core_encode_sync_record, core_sign_sync_record, default_expiry,
        encode_envelope_frame, generate_msg_id, seal_message, CoreInboundDisposition,
        CoreInboundSource, SeenIds, SyncSettingsPayload, DEFAULT_HOP_TTL,
    };
    use std::sync::Arc;

    let mut fleet = Fleet::new(2);
    fleet.step = 1;
    fleet.nodes[1]
        .store
        .core_set_own_sync_context(fleet.roster.clone(), fleet.inbox_key.generation)
        .expect("own sync context");

    // A device of nobody: real keys, a real signature, and no cert in this
    // person's roster.
    let stranger = generate_device_keypair();
    let record = core_sign_sync_record(
        SyncRecord {
            kind: SyncRecordKind::Settings,
            person_id: fleet.person.user_id.clone(),
            author_device_id: Vec::new(),
            roster_version: fleet.roster_version(),
            inbox_key_generation: fleet.inbox_key.generation,
            stream_seq: 1,
            timestamp_ms: fleet.now(),
            payload: core_encode_sync_settings(SyncSettingsPayload {
                entries: vec![SyncSettingEntry {
                    key: "prefs.tone".to_string(),
                    value: b"injected".to_vec(),
                    epoch: 9_999,
                    author_device_id: stranger.device_id.clone(),
                }],
            })
            .expect("encode"),
            signature: Vec::new(),
        },
        stranger.sign_sk.clone(),
    )
    .expect("a stranger can always sign");
    let sealed = seal_message(
        core_device_sync_identity(stranger),
        fleet.inbox_key.agree_pk.clone(),
        core_encode_sync_record(record).expect("encodes"),
    )
    .expect("and can always seal to a public key it has learned");
    let frame = encode_envelope_frame(
        generate_msg_id(),
        DEFAULT_HOP_TTL,
        default_expiry(fleet.now()),
        compute_recipient_hint(fleet.person.user_id.clone(), fleet.now()),
        sealed,
    );

    let outcome = fleet.nodes[1]
        .store
        .process_inbound_frame(
            fleet.person.clone(),
            Arc::new(SeenIds::new()),
            CoreInboundSource::Mesh,
            frame,
            fleet.now(),
        )
        .expect("inbound");
    assert_eq!(outcome.disposition, CoreInboundDisposition::Consumed);
    assert!(outcome.delivered_payloads.is_empty());
    assert_eq!(outcome.work.dropped, 1);
    assert!(outcome.commit.is_none());
    assert_eq!(
        fleet.nodes[1]
            .store
            .consumed_hidden_msg_id_count()
            .expect("evidence"),
        0,
        "a refused record is never vouched for; the licence to delete a relay \
         copy is only ever written for what this device actually took"
    );
    assert!(
        settings_map(&fleet.nodes[1].store).is_empty(),
        "and nothing it carried reached the store"
    );
}

/// The digest is the one sync outcome a shell acts on, so the inbound
/// transaction has to hand it back rather than swallow it. Everything else a
/// sync record does is finished inside core before the call returns; a digest
/// is a question, and only the driver can send the answer.
#[test]
fn the_inbound_transaction_hands_a_siblings_digest_back_to_be_answered() {
    use cruisemesh_core::{
        compute_recipient_hint, default_expiry, encode_envelope_frame, generate_msg_id,
        CoreInboundSource, SeenIds, DEFAULT_HOP_TTL,
    };
    use std::sync::Arc;

    let mut fleet = Fleet::new(2);
    fleet.step = 1;
    fleet.nodes[0]
        .store
        .core_set_own_sync_context(fleet.roster.clone(), fleet.inbox_key.generation)
        .expect("own sync context");

    // Give the sibling something to advertise.
    fleet.compose(1, 0);
    fleet.step += 1;
    fleet.harvest(1);

    let sealed = fleet.mint_digest(1);
    let frame = encode_envelope_frame(
        generate_msg_id(),
        DEFAULT_HOP_TTL,
        default_expiry(fleet.now()),
        compute_recipient_hint(fleet.person.user_id.clone(), fleet.now()),
        sealed,
    );
    let outcome = fleet.nodes[0]
        .store
        .process_inbound_frame(
            fleet.person.clone(),
            Arc::new(SeenIds::new()),
            CoreInboundSource::Mesh,
            frame,
            fleet.now(),
        )
        .expect("inbound");
    assert!(outcome.delivered_payloads.is_empty());
    let digest = outcome
        .sync_peer_digest
        .expect("a digest is a question, and the answer is the driver's to send");
    assert_eq!(digest.person_id, fleet.person.user_id);
    assert!(
        digest.streams.iter().any(|stream| stream.author_device_id
            == fleet.nodes[1].keys.device_id
            && stream.can_serve),
        "the sibling advertised the streams it can actually answer for"
    );
    // And it is directly usable: the driver computes what it owes from what
    // core handed back, with no second read of the sibling's store.
    let owed = core_sync_digest_gaps(
        digest,
        fleet.nodes[0]
            .store
            .core_sync_digest(fleet.person.user_id.clone())
            .expect("own digest"),
    )
    .expect("gaps");
    assert!(
        owed.is_empty(),
        "this device has authored nothing, so it owes nothing — the point is \
         that the arithmetic runs at all"
    );
}
