//! MRB1: cross-transport "mule-to-relay bridge" end-to-end.
//!
//! The product's headline claim: a phone with NO internet gets its message
//! to a shore recipient because a *different* family phone (the package
//! holder) carries the sealed envelope over BLE and uplinks it to the relay
//! mailbox -- and the read receipt comes back over the same bridge in
//! reverse. Every piece of that machinery exists (`family_carried_envelopes`
//! for the upload pass, `enqueue_relay_carried_envelope` for proxy-polling,
//! the relayd mailbox, the cumulative receipt watermark), but no test
//! covered the full chain until this file.
//!
//! Composition of the two existing harnesses:
//! * the in-process Axum `Router` + temp SQLite relay from
//!   `e2e_mailbox.rs` / `e2e_limits.rs` (real HTTP semantics via
//!   `tower::ServiceExt::oneshot`), driven here through the *actual client
//!   codecs* in `core/src/relay_wire.rs` rather than hand-rolled JSON;
//! * the multi-device pattern from `core/tests/mesh_sim.rs`: real
//!   identities, real crypto, one in-memory [`MessageStore`] per phone, and
//!   a minimal re-implementation of the BLE receive/classify step
//!   (dedupe/flood is exercised by `mesh_sim.rs`; this file only needs the
//!   open-vs-carry decision).
//!
//! Code paths pinned (line numbers as of this commit):
//! * `core/src/store.rs` `family_carried_envelopes` (is_family=1 AND
//!   from_relay=0 selection for the upload pass),
//!   `pending_relay_outbound_envelopes`,
//!   `pending_relay_outgoing_receipt_envelopes`,
//!   `enqueue_relay_carried_envelope` (from_relay=1, excluded from
//!   re-upload), `record_receipt` / `receipt_through` (monotonic watermark).
//! * `core/src/relay_wire.rs` `relay_encode_post_envelope`,
//!   `relay_build_fetch_path`, `relay_decode_fetch_page`,
//!   `relay_encode_ack_request` against the live server handlers.
//! * `relayd/src/lib.rs` `insert_envelope_with_quota` per-(family_token,
//!   msg_id) dedupe upsert and `fetch_envelopes` family+hint+cursor filter.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::Router;
use cruisemesh_core::{
    compute_recipient_hint, core_tick_status_for, decode_message_body, decode_receipt_content,
    encode_envelope_frame, generate_identity, open_message, parse_frame, relay_build_fetch_path,
    relay_decode_fetch_page, relay_decode_post_response, relay_encode_ack_request,
    relay_encode_post_envelope, relay_fetch_batch_limit, CarriedEnvelope, Contact,
    CoreRelayFetchPage, CoreTickStatus, Frame, Identity, MessageStore, OpenedMessage,
    StoredMessage, KIND_RECEIPT, KIND_TEXT, MS_PER_DAY, RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
};
use cruisemesh_relayd::{app, AppState, RelayStore};
use tempfile::NamedTempFile;
use tower::util::ServiceExt;

/// Same foreign-carry budget the mesh sim uses; irrelevant to these tests
/// (everything here is family traffic) but required by the enqueue API.
const FOREIGN_BUDGET: i64 = 5 * 1024 * 1024;

const FAMILY: &str = "family-a";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis() as i64
}

/// The `recipient_hint`s `user_id` could match for a still-live envelope:
/// the UserID hashed against each recent day-number (mirror of
/// `mesh_sim.rs::recent_hints` / `MeshService.recentHintsFor`). A phone
/// polls the relay on its own hints AND its contacts' hints (proxy-polling,
/// DESIGN.md §6.4).
fn recent_hints(user_id: &[u8], now: i64) -> Vec<Vec<u8>> {
    (0..=7)
        .map(|days_ago| compute_recipient_hint(user_id.to_vec(), now - days_ago * MS_PER_DAY))
        .collect()
}

/// One phone: real identity + crypto, its own persistent store, and the
/// contacts it recognizes for the family/foreign carry classification.
struct Phone {
    identity: Identity,
    store: MessageStore,
    name: String,
    contacts: Vec<Contact>,
}

impl Phone {
    fn new(name: &str) -> Self {
        Phone {
            identity: generate_identity(),
            store: MessageStore::open(":memory:".to_string()).expect("open in-memory store"),
            name: name.to_string(),
            contacts: Vec::new(),
        }
    }

    fn user_id(&self) -> Vec<u8> {
        self.identity.user_id.clone()
    }

    fn contact(&self) -> Contact {
        Contact {
            user_id: self.identity.user_id.clone(),
            name: self.name.clone(),
            sign_pk: self.identity.sign_pk.clone(),
            agree_pk: self.identity.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    /// Mirror of the BLE receive path's open-vs-carry decision (the
    /// envelope-handling core of `mesh_sim.rs::SimNode::receive`, minus
    /// dedupe/flood which that sim already covers): if the sealed box opens,
    /// this phone is the recipient -- deliver, never carry. Otherwise it is
    /// third-party traffic; classify family iff the `recipient_hint` matches
    /// a known contact's recent day-hints, then enqueue it for later
    /// store-and-forward (and, when family, relay upload).
    fn receive_over_mesh(&self, frame_bytes: &[u8], now: i64) -> Option<OpenedMessage> {
        let Ok(Frame::Envelope {
            msg_id,
            hop_ttl,
            expiry,
            recipient_hint,
            sealed,
        }) = parse_frame(frame_bytes.to_vec())
        else {
            panic!("expected an envelope frame");
        };
        if expiry <= now {
            return None;
        }
        if let Ok(opened) = open_message(self.identity.clone(), sealed.clone()) {
            return Some(opened);
        }
        let is_family = self.contacts.iter().any(|contact| {
            recent_hints(&contact.user_id, now)
                .iter()
                .any(|hint| hint == &recipient_hint)
        });
        self.store
            .enqueue_carried_envelope(
                CarriedEnvelope {
                    msg_id,
                    hop_ttl,
                    expiry,
                    recipient_hint,
                    sealed,
                },
                is_family,
                now,
                FOREIGN_BUDGET,
            )
            .expect("enqueue carried envelope");
        None
    }
}

fn befriend(a: &mut Phone, b: &mut Phone) {
    let ca = a.contact();
    let cb = b.contact();
    a.contacts.push(cb);
    b.contacts.push(ca);
}

/// Three mutually-friended family members: Alice aboard with no internet,
/// Bob aboard with internet (the package holder), Carol ashore.
fn family() -> (Phone, Phone, Phone) {
    let mut alice = Phone::new("Alice");
    let mut bob = Phone::new("Bob");
    let mut carol = Phone::new("Carol");
    befriend(&mut alice, &mut bob);
    befriend(&mut alice, &mut carol);
    befriend(&mut bob, &mut carol);
    (alice, bob, carol)
}

fn test_app_with_quota(quota_bytes: u64) -> (NamedTempFile, Router, RelayStore) {
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let auth: HashSet<String> = HashSet::from([FAMILY.to_string()]);
    let router = app(AppState::with_family_quota_bytes(
        store.clone(),
        auth,
        quota_bytes,
    ));
    (db, router, store)
}

fn test_app() -> (NamedTempFile, Router, RelayStore) {
    test_app_with_quota(u64::MAX)
}

/// POST /envelopes with a body built by the real client codec
/// (`relay_encode_post_envelope`), decoded by the real response codec.
async fn http_post_envelope(
    router: &Router,
    msg_id: &[u8],
    hop_ttl: u8,
    recipient_hint: &[u8],
    sealed: &[u8],
    expiry_ms: i64,
) -> i64 {
    let body = relay_encode_post_envelope(
        msg_id.to_vec(),
        hop_ttl,
        recipient_hint.to_vec(),
        sealed.to_vec(),
        expiry_ms,
    )
    .expect("encode post body");
    let request = Request::builder()
        .method("POST")
        .uri("/envelopes")
        .header("authorization", format!("Bearer {FAMILY}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "POST /envelopes failed");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    relay_decode_post_response(bytes.to_vec()).expect("decode post response")
}

/// GET /envelopes through the real fetch-path builder and page decoder --
/// exactly what a phone runs when polling its own and its contacts' hints.
async fn http_fetch(router: &Router, hints: &[Vec<u8>], after: i64) -> CoreRelayFetchPage {
    let path = relay_build_fetch_path(hints.to_vec(), after, relay_fetch_batch_limit())
        .expect("build fetch path");
    let request = Request::builder()
        .uri(path)
        .header("authorization", format!("Bearer {FAMILY}"))
        .body(Body::empty())
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET /envelopes failed");
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    relay_decode_fetch_page(bytes.to_vec()).expect("decode fetch page")
}

async fn http_ack(router: &Router, ids: Vec<i64>) -> u64 {
    let body = relay_encode_ack_request(ids).expect("encode ack request");
    let request = Request::builder()
        .method("POST")
        .uri("/envelopes/ack")
        .header("authorization", format!("Bearer {FAMILY}"))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "POST /envelopes/ack failed"
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    json["deleted"].as_u64().unwrap()
}

/// The full bridge, both directions:
/// Alice (aboard, no internet) -> BLE -> Bob (aboard, internet) -> relay ->
/// Carol (shore), then Carol's read receipt -> relay -> Bob proxy-poll ->
/// BLE -> Alice, whose tick status flips to Read.
#[tokio::test]
async fn no_internet_message_bridges_to_shore_and_receipt_returns_the_same_way() {
    let t0 = now_ms();
    let (alice, bob, carol) = family();
    let (_db, router, relay) = test_app();

    // 1. Alice authors a text to Carol. With no internet, her self-upload
    //    queue only accumulates -- nothing reaches the relay from her.
    let authored = alice
        .store
        .author_pairwise_message(
            alice.identity.clone(),
            carol.contact(),
            KIND_TEXT,
            b"made it aboard, see you in Miami".to_vec(),
            None,
            t0,
        )
        .expect("author text");
    assert_eq!(authored.message.lamport, 1);
    let alice_pending = alice
        .store
        .pending_relay_outbound_envelopes(16, t0)
        .unwrap();
    assert_eq!(alice_pending.len(), 1);
    assert_eq!(alice_pending[0].msg_id, authored.envelope.msg_id);
    assert_eq!(relay.count_for_family(FAMILY).unwrap(), 0);

    // 2. Alice and Bob meet over BLE. Bob cannot open the envelope (it is
    //    sealed to Carol), recognizes Carol's day-hint, and mules it as
    //    family traffic: it must land in his relay-upload selection
    //    (is_family=1, from_relay=0).
    assert!(bob.receive_over_mesh(&authored.frame, t0).is_none());
    let upload_pass = bob.store.family_carried_envelopes(16, t0).unwrap();
    assert_eq!(
        upload_pass.len(),
        1,
        "muled family envelope must enter Bob's family_carried_envelopes upload selection"
    );
    assert_eq!(upload_pass[0].msg_id, authored.envelope.msg_id);
    assert_eq!(upload_pass[0].sealed, authored.envelope.sealed);

    // 3. Bob's upload pass: post each selected envelope to the relay.
    let uplinked = &upload_pass[0];
    let relay_id = http_post_envelope(
        &router,
        &uplinked.msg_id,
        uplinked.hop_ttl,
        &uplinked.recipient_hint,
        &uplinked.sealed,
        uplinked.expiry,
    )
    .await;
    assert!(relay_id > 0);
    assert_eq!(relay.count_for_family(FAMILY).unwrap(), 1);

    // 4. Carol polls her own hints from shore, trial-decrypts, and reads
    //    Alice's words. She is the envelope's sole true endpoint consumer,
    //    so she (and only she) acks the relay copy.
    let page = http_fetch(&router, &recent_hints(&carol.user_id(), t0), 0).await;
    assert_eq!(page.envelopes.len(), 1);
    let fetched = &page.envelopes[0];
    assert_eq!(fetched.id, relay_id);
    assert_eq!(fetched.msg_id, authored.envelope.msg_id);
    let opened = open_message(carol.identity.clone(), fetched.sealed.clone())
        .expect("carol opens the bridged envelope");
    assert_eq!(opened.sender_user_id, alice.user_id());
    let body = decode_message_body(opened.payload).unwrap();
    assert_eq!(body.kind, KIND_TEXT);
    assert_eq!(body.content, b"made it aboard, see you in Miami");
    assert_eq!(body.lamport, 1);
    carol
        .store
        .insert_incoming_message(
            StoredMessage {
                // Local pairwise chat_id = the peer's user id.
                chat_id: opened.sender_user_id.clone(),
                sender_user_id: opened.sender_user_id.clone(),
                lamport: body.lamport,
                timestamp: body.timestamp,
                kind: body.kind,
                payload: body.content,
            },
            fetched.msg_id.clone(),
            None,
        )
        .expect("carol stores the message");
    assert_eq!(http_ack(&router, vec![fetched.id]).await, 1);
    assert!(
        http_fetch(&router, &recent_hints(&carol.user_id(), t0), 0)
            .await
            .envelopes
            .is_empty(),
        "acked envelope must be gone from Carol's mailbox"
    );

    // 5a. Reverse path: Carol authors a cumulative read receipt sealed and
    //     hint-addressed to Alice, and posts it to the relay directly.
    let receipt = carol
        .store
        .author_receipt(
            carol.identity.clone(),
            alice.contact(),
            alice.user_id(),
            RECEIPT_TYPE_READ,
            body.lamport,
            t0,
        )
        .expect("author receipt")
        .expect("read watermark advanced");
    let carol_pending = carol
        .store
        .pending_relay_outgoing_receipt_envelopes(16, t0)
        .unwrap();
    assert_eq!(carol_pending.len(), 1);
    assert_eq!(carol_pending[0].msg_id, receipt.envelope.msg_id);
    let receipt_relay_id = http_post_envelope(
        &router,
        &receipt.envelope.msg_id,
        receipt.envelope.hop_ttl,
        &receipt.envelope.recipient_hint,
        &receipt.envelope.sealed,
        receipt.envelope.expiry,
    )
    .await;
    assert!(receipt_relay_id > 0);
    carol
        .store
        .mark_outgoing_receipt_envelope_relay_posted(receipt.envelope.msg_id.clone(), t0)
        .unwrap();
    assert!(carol
        .store
        .pending_relay_outgoing_receipt_envelopes(16, t0)
        .unwrap()
        .is_empty());

    // 5b. Bob proxy-polls with ALICE's hints (a phone fetches on its own
    //     hints and its contacts'), cannot open the receipt, and enqueues it
    //     through the relay-sourced carried path. DTN ack safety: Bob is not
    //     the endpoint consumer, so he never acks the relay copy.
    let proxy_page = http_fetch(&router, &recent_hints(&alice.user_id(), t0), 0).await;
    assert_eq!(proxy_page.envelopes.len(), 1);
    let proxied = &proxy_page.envelopes[0];
    assert_eq!(proxied.msg_id, receipt.envelope.msg_id);
    assert!(
        open_message(bob.identity.clone(), proxied.sealed.clone()).is_err(),
        "the receipt is sealed to Alice, not to the package holder"
    );
    assert!(bob
        .store
        .enqueue_relay_carried_envelope(
            CarriedEnvelope {
                msg_id: proxied.msg_id.clone(),
                hop_ttl: proxied.hop_ttl,
                expiry: proxied.expiry_ms,
                recipient_hint: proxied.recipient_hint.clone(),
                sealed: proxied.sealed.clone(),
            },
            t0,
        )
        .unwrap());
    // No churn: the relay-fetched copy is deliverable over BLE but never
    // enters Bob's upload selection. (Alice's original text legitimately
    // stays in it -- a carried 1:1 envelope is dropped only on digest-proof
    // of receipt, and re-posting it is dedupe-safe.)
    assert!(!bob
        .store
        .family_carried_envelopes(16, t0)
        .unwrap()
        .iter()
        .any(|envelope| envelope.msg_id == receipt.envelope.msg_id));
    // Bob never acked, so the receipt stays on the relay for Alice's own
    // (hypothetical, she has no internet) direct poll too.
    assert_eq!(relay.count_for_family(FAMILY).unwrap(), 1);

    // 5c. Bob and Alice meet over BLE: the carried-envelope drain hands the
    //     receipt to its true recipient, who records the watermark.
    let drained = bob
        .store
        .carried_envelopes_for_hints(recent_hints(&alice.user_id(), t0), t0)
        .unwrap();
    assert_eq!(drained.len(), 1);
    for envelope in drained {
        let frame = encode_envelope_frame(
            envelope.msg_id.clone(),
            envelope.hop_ttl,
            envelope.expiry,
            envelope.recipient_hint.clone(),
            envelope.sealed.clone(),
        );
        let opened = alice
            .receive_over_mesh(&frame, t0)
            .expect("alice opens her receipt");
        assert_eq!(opened.sender_user_id, carol.user_id());
        let receipt_body = decode_message_body(opened.payload).unwrap();
        assert_eq!(receipt_body.kind, KIND_RECEIPT);
        let content = decode_receipt_content(receipt_body.content).unwrap();
        assert_eq!(content.sender_user_id, alice.user_id());
        assert_eq!(content.receipt_type, RECEIPT_TYPE_READ);
        assert_eq!(content.lamport, 1);
        alice
            .store
            .record_receipt(
                // Local pairwise chat_id = the acking peer's user id.
                opened.sender_user_id,
                content.sender_user_id,
                content.receipt_type,
                content.lamport,
                None,
            )
            .unwrap();
        bob.store.remove_carried_envelope(envelope.msg_id).unwrap();
    }

    // 6. Alice's cumulative receipt watermark advanced: her message -- which
    //    never touched the internet from her device -- now shows Read.
    let read_through = alice
        .store
        .receipt_through(carol.user_id(), alice.user_id(), RECEIPT_TYPE_READ)
        .unwrap();
    assert_eq!(read_through, 1);
    let delivered_through = alice
        .store
        .receipt_through(carol.user_id(), alice.user_id(), RECEIPT_TYPE_DELIVERED)
        .unwrap();
    assert_eq!(
        core_tick_status_for(authored.message.lamport, delivered_through, read_through),
        CoreTickStatus::Read
    );
}

/// Dedupe: Bob uplinked Alice's envelope while muling it; when Alice later
/// gets internet herself and uploads her own pending copy of the SAME
/// msg_id, the relay must keep exactly one row -- and the repost must not
/// count against quota (mirror of e2e_limits.rs's
/// `dedupe_repost_of_existing_msg_id_is_never_quota_checked`, reached here
/// through the real mule-then-sender sequence).
#[tokio::test]
async fn senders_late_self_upload_after_mule_upload_dedupes_and_skips_quota() {
    let t0 = now_ms();
    let (alice, bob, carol) = family();

    let authored = alice
        .store
        .author_pairwise_message(
            alice.identity.clone(),
            carol.contact(),
            KIND_TEXT,
            b"uploaded twice, stored once".to_vec(),
            None,
            t0,
        )
        .expect("author text");
    // Quota sized to fit exactly one copy of this envelope's sealed bytes.
    let quota = authored.envelope.sealed.len() as u64;
    let (_db, router, relay) = test_app_with_quota(quota);

    // Bob mules it and uploads first, filling the family mailbox exactly.
    assert!(bob.receive_over_mesh(&authored.frame, t0).is_none());
    let carried = bob.store.family_carried_envelopes(16, t0).unwrap();
    assert_eq!(carried.len(), 1);
    let bob_relay_id = http_post_envelope(
        &router,
        &carried[0].msg_id,
        carried[0].hop_ttl,
        &carried[0].recipient_hint,
        &carried[0].sealed,
        carried[0].expiry,
    )
    .await;
    assert_eq!(relay.family_sealed_bytes(FAMILY).unwrap(), quota);

    // Alice "gets internet" and runs her own upload pass over
    // pending_relay_outbound_envelopes. Same msg_id: pure dedupe, never
    // quota-checked even though the mailbox is already full.
    let pending = alice
        .store
        .pending_relay_outbound_envelopes(16, t0)
        .unwrap();
    assert_eq!(pending.len(), 1);
    let alice_relay_id = http_post_envelope(
        &router,
        &pending[0].msg_id,
        pending[0].hop_ttl,
        &pending[0].recipient_hint,
        &pending[0].sealed,
        pending[0].expiry,
    )
    .await;
    assert_eq!(
        alice_relay_id, bob_relay_id,
        "same (family_token, msg_id) must dedupe to the one existing row"
    );
    assert_eq!(relay.count_for_family(FAMILY).unwrap(), 1);
    assert_eq!(relay.family_sealed_bytes(FAMILY).unwrap(), quota);

    // Her retry queue drains once the post is marked done.
    assert!(alice
        .store
        .mark_outbound_envelope_relay_posted(pending[0].msg_id.clone(), t0)
        .unwrap());
    assert!(alice
        .store
        .pending_relay_outbound_envelopes(16, t0)
        .unwrap()
        .is_empty());
}

/// No churn: envelopes Bob pulled FROM the relay while proxy-polling for
/// Alice (from_relay=1) stay deliverable over BLE but never reappear in his
/// family_carried_envelopes upload selection -- re-uploading them would be
/// pointless traffic and could resurrect an already-acked copy. Re-fetching
/// the same still-unacked proxy envelope on a later poll is a no-op.
#[tokio::test]
async fn relay_fetched_copies_never_reenter_the_upload_selection() {
    let t0 = now_ms();
    let (alice, bob, carol) = family();
    let (_db, router, _relay) = test_app();

    // Carol posts a receipt for Alice straight to the relay.
    let receipt = carol
        .store
        .author_receipt(
            carol.identity.clone(),
            alice.contact(),
            alice.user_id(),
            RECEIPT_TYPE_DELIVERED,
            3,
            t0,
        )
        .expect("author receipt")
        .expect("watermark advanced");
    http_post_envelope(
        &router,
        &receipt.envelope.msg_id,
        receipt.envelope.hop_ttl,
        &receipt.envelope.recipient_hint,
        &receipt.envelope.sealed,
        receipt.envelope.expiry,
    )
    .await;

    // Bob proxy-polls Alice's hints twice; both times the enqueue is the
    // relay-sourced from_relay=1 path.
    for pass in 0..2 {
        let page = http_fetch(&router, &recent_hints(&alice.user_id(), t0), 0).await;
        assert_eq!(page.envelopes.len(), 1, "unacked proxy row stays fetchable");
        let proxied = &page.envelopes[0];
        let inserted = bob
            .store
            .enqueue_relay_carried_envelope(
                CarriedEnvelope {
                    msg_id: proxied.msg_id.clone(),
                    hop_ttl: proxied.hop_ttl,
                    expiry: proxied.expiry_ms,
                    recipient_hint: proxied.recipient_hint.clone(),
                    sealed: proxied.sealed.clone(),
                },
                t0,
            )
            .unwrap();
        assert_eq!(inserted, pass == 0, "re-fetch on a later poll is a no-op");
    }

    assert_eq!(bob.store.carried_len().unwrap(), 1);
    assert_eq!(
        bob.store
            .carried_envelopes_for_hints(recent_hints(&alice.user_id(), t0), t0)
            .unwrap()
            .len(),
        1,
        "the proxy copy stays deliverable to Alice over BLE"
    );
    assert!(
        bob.store
            .family_carried_envelopes(16, t0)
            .unwrap()
            .is_empty(),
        "from_relay=1 copies must never enter the relay-upload selection"
    );
}

/// Privacy: the muled envelope is fetchable only via its recipient's
/// rotating day-hint. Neither the package holder's own hints nor an
/// unrelated stranger's return anything -- the relay never leaks a family
/// member's inbound mail to a fetch for someone else's hints.
#[tokio::test]
async fn fetching_with_unrelated_hints_sees_nothing() {
    let t0 = now_ms();
    let (alice, bob, carol) = family();
    let (_db, router, _relay) = test_app();

    let authored = alice
        .store
        .author_pairwise_message(
            alice.identity.clone(),
            carol.contact(),
            KIND_TEXT,
            b"for carol's hints only".to_vec(),
            None,
            t0,
        )
        .expect("author text");
    assert!(bob.receive_over_mesh(&authored.frame, t0).is_none());
    let carried = bob.store.family_carried_envelopes(16, t0).unwrap();
    http_post_envelope(
        &router,
        &carried[0].msg_id,
        carried[0].hop_ttl,
        &carried[0].recipient_hint,
        &carried[0].sealed,
        carried[0].expiry,
    )
    .await;

    // Bob's own hints (the uploader!) see nothing...
    assert!(http_fetch(&router, &recent_hints(&bob.user_id(), t0), 0)
        .await
        .envelopes
        .is_empty());
    // ...nor do a stranger's.
    let stranger = generate_identity();
    assert!(http_fetch(&router, &recent_hints(&stranger.user_id, t0), 0)
        .await
        .envelopes
        .is_empty());
    // Carol's do.
    assert_eq!(
        http_fetch(&router, &recent_hints(&carol.user_id(), t0), 0)
            .await
            .envelopes
            .len(),
        1
    );
}
