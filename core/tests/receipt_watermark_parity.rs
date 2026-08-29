//! Cross-engine parity for the DELIVERED receipt watermark.
//!
//! Two implementations of "how far through this peer's stream have we got?"
//! exist at once right now, and they must not drift:
//!
//! * the **shipped** one, in both shells — `PeerStreamWatermark.through` in
//!   `android/app/src/main/kotlin/com/cruisemesh/app/mesh/PeerStreamWatermark.kt`
//!   and `ios/CruiseMesh/Mesh/PeerStreamWatermark.swift`. Each is literally
//!   `max(store.highestLamport(chatId, senderUserId), atLeastLamport)`;
//! * the **new** one, in `core/src/session/mesh_receive.rs`'s auto-receipt
//!   tail, which the shells will call instead once package D1 switches the
//!   inbound engine on. It is not switched on today, so nothing in the field
//!   would notice it disagreeing.
//!
//! That is exactly the window in which a divergence gets shipped: it costs
//! nothing until the flip, and then it costs everything. The watermark has
//! already been fixed twice for being a *contiguous* count instead of a MAX,
//! and the engine reintroduced the contiguous version. So this file pins the
//! two together over one input sequence rather than trusting that a reader
//! notices.
//!
//! [`shipped_peer_stream_watermark`] below restates the shells' expression, so
//! the assertion is "the engine agrees with the shipped formula", not "the
//! engine agrees with itself". Both shell files are checked to still exist,
//! following `protocol_contract.rs`'s rule that a named owner has to be a real
//! file — a reference to a renamed or deleted file is worse than no reference.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use cruisemesh_core::{
    compute_recipient_hint, encode_envelope_frame, encode_group_invite_content,
    encode_message_body, generate_identity, generate_msg_id, seal_message, Contact,
    CoreDeliveryVerdict, CoreDiscoveryPolicyState, CoreInboundSource, Group, Identity,
    MessageArrival, MessageBody, MessageStore, SeenIds, DEFAULT_HOP_TTL, KIND_GROUP_INVITE,
    KIND_TEXT, RECEIPT_TYPE_DELIVERED,
};

const NOW: i64 = 1_700_000_000_000;
const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

/// The shipped shells' watermark, restated.
///
/// `PeerStreamWatermark.through` on both platforms is one expression:
/// `max(store.highestLamport(chatId, senderUserId), atLeastLamport)`. A plain
/// MAX, never `highestContiguousLamport` — a peer's stream legitimately
/// contains lamports this device will never hold a row for (a front gap from
/// the authoring lamport ratchet after a chat wipe or restore, an interior gap
/// from a kind this build does not file, and the group invite whose row is
/// filed under the group chat), and a contiguous count stops at the first one
/// and reports the same number forever.
fn shipped_peer_stream_watermark(
    store: &MessageStore,
    chat_id: &[u8],
    sender_user_id: &[u8],
    at_least_lamport: u64,
) -> u64 {
    store
        .highest_lamport(chat_id.to_vec(), sender_user_id.to_vec())
        .expect("highest_lamport")
        .max(at_least_lamport)
}

/// What the engine actually wrote for this peer.
fn engine_watermark(store: &MessageStore, sender_user_id: &[u8]) -> u64 {
    store
        .outgoing_receipt_through(
            sender_user_id.to_vec(),
            sender_user_id.to_vec(),
            RECEIPT_TYPE_DELIVERED,
        )
        .expect("outgoing_receipt_through")
}

fn store() -> MessageStore {
    MessageStore::open(":memory:".to_string()).expect("in-memory store")
}

fn contact(identity: &Identity, name: &str) -> Contact {
    Contact {
        user_id: identity.user_id.clone(),
        name: name.into(),
        sign_pk: identity.sign_pk.clone(),
        agree_pk: identity.agree_pk.clone(),
        relay_url: None,
        relay_token: None,
        nickname: None,
    }
}

fn body(kind: u8, chat_id: Vec<u8>, lamport: u64, content: Vec<u8>) -> Vec<u8> {
    encode_message_body(MessageBody {
        kind,
        chat_id,
        lamport,
        timestamp: NOW,
        content,
    })
    .expect("encode body")
}

/// One envelope through the production inbound pair the shells will call:
/// `process_inbound_frame` (open/authorize/carry) then `core_deliver_inbound`
/// (the per-kind fold, whose tail writes the receipt watermark under test).
fn deliver(store: &MessageStore, me: &Identity, sender: &Identity, payload: Vec<u8>) {
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
    let delivered = outcome
        .delivered_payloads
        .first()
        .cloned()
        .expect("a pairwise envelope for us is delivered");
    let delivery = store
        .core_deliver_inbound(
            me.clone(),
            outcome.delivered_sender.expect("verified sender"),
            delivered,
            outcome.commit.expect("commit token"),
            MessageArrival {
                transport: 3,
                hops_taken: 0,
                received_at: NOW,
            },
            CoreDiscoveryPolicyState {
                enabled: true,
                revision: 0,
            },
        )
        .expect("delivery");
    assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
}

/// The guard: one sequence of arrivals, shaped so every way a peer's stream
/// legitimately gains a hole is represented, and after **every** step the
/// engine's watermark equals the shipped shells' formula over the same store.
///
/// The step comments record what each arrival would have produced under the
/// contiguous implementation this replaces, since that is the drift being
/// guarded against and it is invisible from the assertions alone.
#[test]
fn the_engine_and_the_shipped_shells_agree_on_the_receipt_watermark() {
    let store = store();
    let me = generate_identity();
    let sender = generate_identity();
    store.upsert_contact(contact(&sender, "Sender")).unwrap();

    // A front gap: the sender wiped their chat history (or restored a backup),
    // so their lamport ratchet restarted above 1 and lamports 1 and 2 never
    // existed for anyone. Contiguous: 0, forever. MAX: 3.
    deliver(
        &store,
        &me,
        &sender,
        body(
            KIND_TEXT,
            sender.user_id.clone(),
            3,
            b"after the wipe".to_vec(),
        ),
    );
    assert_eq!(
        engine_watermark(&store, &sender.user_id),
        shipped_peer_stream_watermark(&store, &sender.user_id, &sender.user_id, 0),
    );
    assert_eq!(engine_watermark(&store, &sender.user_id), 3);

    // An interior gap: lamport 4 is not held here — a sideband kind an older
    // build drops without writing a `messages` row, or simply a message still
    // in flight. Contiguous: still 0. MAX: 5.
    deliver(
        &store,
        &me,
        &sender,
        body(KIND_TEXT, sender.user_id.clone(), 5, b"deck 9".to_vec()),
    );
    assert_eq!(
        engine_watermark(&store, &sender.user_id),
        shipped_peer_stream_watermark(&store, &sender.user_id, &sender.user_id, 0),
    );
    assert_eq!(engine_watermark(&store, &sender.user_id), 5);

    // A late arrival that fills a hole must not move the watermark backwards
    // — the receipt is cumulative and monotonic on both sides.
    deliver(
        &store,
        &me,
        &sender,
        body(
            KIND_TEXT,
            sender.user_id.clone(),
            4,
            b"the late one".to_vec(),
        ),
    );
    assert_eq!(
        engine_watermark(&store, &sender.user_id),
        shipped_peer_stream_watermark(&store, &sender.user_id, &sender.user_id, 0),
    );
    assert_eq!(engine_watermark(&store, &sender.user_id), 5);

    // A group invite rides this 1:1 lamport stream but its row is filed under
    // the group's chat id, so a plain MAX over the 1:1 chat sits below it.
    // Both sides raise the floor to the invite's own lamport; the shells pass
    // it as `atLeastLamport`, the engine as its `receipt_floor`.
    let group = Group {
        id: vec![0x55; 16],
        name: "Muster".into(),
        member_user_ids: vec![me.user_id.clone(), sender.user_id.clone()],
        key: vec![0x66; 32],
        metadata_revision: 0,
        metadata_changed_by: Vec::new(),
    };
    let invite = encode_group_invite_content(group.clone()).unwrap();
    deliver(
        &store,
        &me,
        &sender,
        body(KIND_GROUP_INVITE, sender.user_id.clone(), 9, invite),
    );
    assert!(
        store
            .messages_for_chat(sender.user_id.clone())
            .unwrap()
            .iter()
            .all(|row| row.lamport != 9),
        "the invite row belongs to the group chat, so the 1:1 MAX cannot see it"
    );
    assert_eq!(
        engine_watermark(&store, &sender.user_id),
        shipped_peer_stream_watermark(&store, &sender.user_id, &sender.user_id, 9),
    );
    assert_eq!(engine_watermark(&store, &sender.user_id), 9);

    // The repair lane is untouched by all of the above: the digest still
    // reports the gap-aware contiguous watermark for this sender, so a
    // genuinely lost message is still detected and re-requested. The widening
    // is the receipt watermark's alone.
    assert_eq!(
        store
            .highest_contiguous_lamport(sender.user_id.clone(), sender.user_id.clone())
            .unwrap(),
        0,
        "hole detection stays with the digest's contiguous watermark"
    );
}

/// A reference to a shell file is only worth having if the file is still
/// there. Mirrors `protocol_contract.rs`'s "a named owner has to be a real
/// file" check, scoped to the two files this parity claim rests on.
#[test]
fn the_named_shipped_implementations_exist() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core/ has a parent")
        .to_path_buf();
    for relative in [
        "android/app/src/main/kotlin/com/cruisemesh/app/mesh/PeerStreamWatermark.kt",
        "ios/CruiseMesh/Mesh/PeerStreamWatermark.swift",
    ] {
        let path: PathBuf = root.join(relative);
        assert!(
            path.is_file(),
            "{relative} is named as the shipped watermark implementation but does not exist; \
             update this file's reference rather than leaving it pointing at nothing"
        );
    }
}
