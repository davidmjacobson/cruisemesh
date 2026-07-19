//! End-to-end mailbox test: real sealed envelopes from `cruisemesh-core`
//! driven through the in-process Axum app.
//!
//! Covers DESIGN.md §9: post → fetch-by-hint → open → ack → gone, msg_id
//! dedupe within a family token, cross-family isolation, expiry pruning,
//! and content-agnostic handling of text + receipt envelopes (the traffic
//! Lane A will upload over relay).

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cruisemesh_core::{
    compute_recipient_hint, decode_message_body, decode_receipt_content, default_expiry,
    encode_message_body, encode_receipt_content, generate_identity, generate_msg_id, open_message,
    seal_message, Identity, MessageBody, ReceiptContent, DEFAULT_HOP_TTL, KIND_RECEIPT, KIND_TEXT,
    RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
};
use cruisemesh_relayd::{app, AppState, RelayStore};
use tempfile::NamedTempFile;
use tower::util::ServiceExt;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis() as i64
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn test_app(tokens: &[&str]) -> (NamedTempFile, Router) {
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let auth: HashSet<String> = tokens.iter().map(|t| (*t).to_string()).collect();
    (db, app(AppState::new(store, auth)))
}

async fn body_json(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

struct AuthoredEnvelope {
    msg_id: Vec<u8>,
    hop_ttl: u8,
    recipient_hint: Vec<u8>,
    sealed: Vec<u8>,
    expiry_ms: i64,
}

/// Author a sealed text envelope the same way phones do: encode body → seal
/// to recipient → public header fields from core helpers.
fn author_text(sender: &Identity, recipient: &Identity, text: &str, lamport: u64) -> AuthoredEnvelope {
    let timestamp = now_ms();
    // Wire chat_id = frame sender's own userId (MeshService convention).
    let body = MessageBody {
        kind: KIND_TEXT,
        chat_id: sender.user_id.clone(),
        lamport,
        timestamp,
        content: text.as_bytes().to_vec(),
    };
    let payload = encode_message_body(body).unwrap();
    let sealed = seal_message(sender.clone(), recipient.agree_pk.clone(), payload).unwrap();
    AuthoredEnvelope {
        msg_id: generate_msg_id(),
        hop_ttl: DEFAULT_HOP_TTL,
        recipient_hint: compute_recipient_hint(recipient.user_id.clone(), timestamp),
        sealed,
        expiry_ms: default_expiry(timestamp),
    }
}

/// Cumulative receipt envelope sealed *to the original text sender* (who
/// needs the tick). Content-agnostic on the relay — same POST shape as text.
fn author_receipt(
    receipt_sender: &Identity,
    receipt_recipient: &Identity,
    chat_id: Vec<u8>,
    original_sender_user_id: Vec<u8>,
    through_lamport: u64,
    receipt_type: u8,
) -> AuthoredEnvelope {
    let timestamp = now_ms();
    let content = encode_receipt_content(ReceiptContent {
        chat_id: chat_id.clone(),
        sender_user_id: original_sender_user_id,
        lamport: through_lamport,
        receipt_type,
    })
    .unwrap();
    let body = MessageBody {
        kind: KIND_RECEIPT,
        chat_id,
        lamport: through_lamport,
        timestamp,
        content,
    };
    let payload = encode_message_body(body).unwrap();
    let sealed =
        seal_message(receipt_sender.clone(), receipt_recipient.agree_pk.clone(), payload).unwrap();
    AuthoredEnvelope {
        msg_id: generate_msg_id(),
        hop_ttl: DEFAULT_HOP_TTL,
        recipient_hint: compute_recipient_hint(receipt_recipient.user_id.clone(), timestamp),
        sealed,
        expiry_ms: default_expiry(timestamp),
    }
}

async fn post_envelope(app: &Router, token: &str, env: &AuthoredEnvelope) -> serde_json::Value {
    let request = Request::builder()
        .method("POST")
        .uri("/envelopes")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "msg_id": b64(&env.msg_id),
                "hop_ttl": env.hop_ttl,
                "recipient_hint": b64(&env.recipient_hint),
                "sealed": b64(&env.sealed),
                "expiry_ms": env.expiry_ms,
            })
            .to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "POST /envelopes failed");
    body_json(response).await
}

async fn get_envelopes(
    app: &Router,
    token: &str,
    hints: &[Vec<u8>],
    after: i64,
) -> serde_json::Value {
    let hints_q = hints.iter().map(|h| b64(h)).collect::<Vec<_>>().join(",");
    let request = Request::builder()
        .uri(format!("/envelopes?hints={hints_q}&after={after}&limit=100"))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET /envelopes failed");
    body_json(response).await
}

async fn ack(app: &Router, token: &str, ids: &[i64]) -> u64 {
    let request = Request::builder()
        .method("POST")
        .uri("/envelopes/ack")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "ids": ids }).to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "POST /envelopes/ack failed");
    let json = body_json(response).await;
    json["deleted"].as_u64().unwrap()
}

fn decode_sealed_field(value: &serde_json::Value) -> Vec<u8> {
    URL_SAFE_NO_PAD
        .decode(value.as_str().expect("sealed string"))
        .expect("base64url sealed")
}

#[tokio::test]
async fn real_sealed_text_post_fetch_open_ack_gone() {
    let alice = generate_identity();
    let bob = generate_identity();
    let (_db, app) = test_app(&["family-a"]);

    let env = author_text(&alice, &bob, "hello over relay", 1);
    let posted = post_envelope(&app, "family-a", &env).await;
    let id = posted["id"].as_i64().unwrap();
    assert!(id > 0);

    let fetched = get_envelopes(&app, "family-a", &[env.recipient_hint.clone()], 0).await;
    let envelopes = fetched["envelopes"].as_array().unwrap();
    assert_eq!(envelopes.len(), 1);
    assert_eq!(envelopes[0]["msg_id"], b64(&env.msg_id));
    assert_eq!(envelopes[0]["hop_ttl"].as_u64().unwrap(), DEFAULT_HOP_TTL as u64);

    let sealed = decode_sealed_field(&envelopes[0]["sealed"]);
    let opened = open_message(bob.clone(), sealed).unwrap();
    assert_eq!(opened.sender_user_id, alice.user_id);
    let body = decode_message_body(opened.payload).unwrap();
    assert_eq!(body.kind, KIND_TEXT);
    assert_eq!(body.content, b"hello over relay");
    assert_eq!(body.lamport, 1);

    // Still present until ack.
    let again = get_envelopes(&app, "family-a", &[env.recipient_hint.clone()], 0).await;
    assert_eq!(again["envelopes"].as_array().unwrap().len(), 1);

    assert_eq!(ack(&app, "family-a", &[id]).await, 1);
    let gone = get_envelopes(&app, "family-a", &[env.recipient_hint.clone()], 0).await;
    assert!(gone["envelopes"].as_array().unwrap().is_empty());
    assert_eq!(gone["next_cursor"].as_i64().unwrap(), 0);
}

#[tokio::test]
async fn receipt_envelope_round_trip_is_content_agnostic() {
    let alice = generate_identity();
    let bob = generate_identity();
    let (_db, app) = test_app(&["family-a"]);

    // Alice → Bob text (Bob will later ack with a receipt sealed to Alice).
    let text = author_text(&alice, &bob, "ping", 3);
    post_envelope(&app, "family-a", &text).await;

    // Bob's delivered receipt for Alice's stream, sealed to Alice.
    let receipt = author_receipt(
        &bob,
        &alice,
        bob.user_id.clone(), // wire chat_id = receipt frame sender
        alice.user_id.clone(),
        3,
        RECEIPT_TYPE_DELIVERED,
    );
    let posted = post_envelope(&app, "family-a", &receipt).await;
    let receipt_id = posted["id"].as_i64().unwrap();

    // Alice polls with her own day-hint and gets the receipt (not Bob's text).
    let for_alice = get_envelopes(&app, "family-a", &[receipt.recipient_hint.clone()], 0).await;
    let envs = for_alice["envelopes"].as_array().unwrap();
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0]["msg_id"], b64(&receipt.msg_id));

    let opened = open_message(alice.clone(), decode_sealed_field(&envs[0]["sealed"])).unwrap();
    assert_eq!(opened.sender_user_id, bob.user_id);
    let body = decode_message_body(opened.payload).unwrap();
    assert_eq!(body.kind, KIND_RECEIPT);
    let content = decode_receipt_content(body.content).unwrap();
    assert_eq!(content.lamport, 3);
    assert_eq!(content.receipt_type, RECEIPT_TYPE_DELIVERED);
    assert_eq!(content.sender_user_id, alice.user_id);

    // Bob still has his text waiting (different hint) — receipts don't
    // interfere with text rows on the same family token.
    let for_bob = get_envelopes(&app, "family-a", &[text.recipient_hint.clone()], 0).await;
    assert_eq!(for_bob["envelopes"].as_array().unwrap().len(), 1);

    assert_eq!(ack(&app, "family-a", &[receipt_id]).await, 1);
    let gone = get_envelopes(&app, "family-a", &[receipt.recipient_hint.clone()], 0).await;
    assert!(gone["envelopes"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn mixed_text_and_read_receipt_share_mailbox_without_kind_filter() {
    let alice = generate_identity();
    let bob = generate_identity();
    let (_db, app) = test_app(&["family-a"]);

    // Two envelopes destined for Alice: a prior-mule text (Bob→Alice) and a
    // read receipt from Charlie→Alice would be the multi-party case; here we
    // reuse Bob for both kinds so one hint fetch returns both.
    let text = author_text(&bob, &alice, "mixed path", 1);
    let receipt = author_receipt(
        &bob,
        &alice,
        bob.user_id.clone(),
        alice.user_id.clone(),
        1,
        RECEIPT_TYPE_READ,
    );
    // Force both onto Alice's current-day hint (author_* already does that).
    post_envelope(&app, "family-a", &text).await;
    post_envelope(&app, "family-a", &receipt).await;

    let page = get_envelopes(&app, "family-a", &[text.recipient_hint.clone()], 0).await;
    let envs = page["envelopes"].as_array().unwrap();
    assert_eq!(envs.len(), 2, "server must return both kinds for the same hint");

    let mut kinds = Vec::new();
    for env in envs {
        let opened = open_message(alice.clone(), decode_sealed_field(&env["sealed"])).unwrap();
        let body = decode_message_body(opened.payload).unwrap();
        kinds.push(body.kind);
    }
    kinds.sort();
    assert_eq!(kinds, vec![KIND_TEXT, KIND_RECEIPT]);
}

#[tokio::test]
async fn msg_id_dedupe_within_family_and_cross_family_isolation() {
    let alice = generate_identity();
    let bob = generate_identity();
    let (_db, app) = test_app(&["family-a", "family-b"]);

    let env = author_text(&alice, &bob, "shared msg_id shape", 1);

    let first = post_envelope(&app, "family-a", &env).await;
    let second = post_envelope(&app, "family-a", &env).await;
    assert_eq!(
        first["id"].as_i64().unwrap(),
        second["id"].as_i64().unwrap(),
        "same family + msg_id must dedupe to one row"
    );

    // Same msg_id is allowed in another family (isolation boundary).
    let other_family = post_envelope(&app, "family-b", &env).await;
    assert_ne!(
        first["id"].as_i64().unwrap(),
        other_family["id"].as_i64().unwrap()
    );

    let for_a = get_envelopes(&app, "family-a", &[env.recipient_hint.clone()], 0).await;
    assert_eq!(for_a["envelopes"].as_array().unwrap().len(), 1);

    let for_b = get_envelopes(&app, "family-b", &[env.recipient_hint.clone()], 0).await;
    assert_eq!(for_b["envelopes"].as_array().unwrap().len(), 1);

    // family-a ack must not remove family-b's copy.
    let a_id = for_a["envelopes"][0]["id"].as_i64().unwrap();
    assert_eq!(ack(&app, "family-a", &[a_id]).await, 1);
    let for_b_after = get_envelopes(&app, "family-b", &[env.recipient_hint.clone()], 0).await;
    assert_eq!(for_b_after["envelopes"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn wrong_hint_sees_nothing_and_healthz_is_open() {
    let alice = generate_identity();
    let bob = generate_identity();
    let (_db, app) = test_app(&["family-a"]);

    let env = author_text(&alice, &bob, "hint filter", 1);
    post_envelope(&app, "family-a", &env).await;

    // Alice's own day-hint is not Bob's — she must not pull Bob's inbound mail.
    let alice_hint = compute_recipient_hint(alice.user_id.clone(), now_ms());
    let miss = get_envelopes(&app, "family-a", &[alice_hint], 0).await;
    assert!(miss["envelopes"].as_array().unwrap().is_empty());

    let health = app
        .oneshot(
            Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    let json = body_json(health).await;
    assert_eq!(json["status"], "ok");
}

#[tokio::test]
async fn expiry_pruning_drops_stale_sealed_envelopes() {
    let alice = generate_identity();
    let bob = generate_identity();
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let app = app(AppState::new(
        store.clone(),
        HashSet::from(["family-a".to_string()]),
    ));

    let live = author_text(&alice, &bob, "still good", 1);
    let mut expired = author_text(&alice, &bob, "already dead", 2);
    // Force an already-past expiry while keeping real sealed bytes.
    expired.expiry_ms = now_ms() - 1;

    store
        .insert_envelope(
            "family-a",
            expired.msg_id.clone(),
            expired.hop_ttl,
            expired.recipient_hint.clone(),
            expired.sealed.clone(),
            expired.expiry_ms,
            now_ms() - 10_000,
        )
        .unwrap();
    post_envelope(&app, "family-a", &live).await;

    let page = get_envelopes(&app, "family-a", &[live.recipient_hint.clone()], 0).await;
    let envs = page["envelopes"].as_array().unwrap();
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0]["msg_id"], b64(&live.msg_id));

    let opened = open_message(bob, decode_sealed_field(&envs[0]["sealed"])).unwrap();
    let body = decode_message_body(opened.payload).unwrap();
    assert_eq!(body.content, b"still good");
}


async fn spawn_app_with_router(tokens: &[&str]) -> (NamedTempFile, Router, String) {
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let auth: HashSet<String> = tokens.iter().map(|t| (*t).to_string()).collect();
    let app = app(AppState::new(store, auth));
    
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app_clone = app.clone();
    tokio::spawn(async move {
        axum::serve(listener, app_clone).await.unwrap();
    });
    
    (db, app, format!("ws://{}", addr))
}

use futures_util::StreamExt;

#[tokio::test]
async fn ws_live_push_after_connect() {
    let alice = generate_identity();
    let bob = generate_identity();
    let (_db, app, ws_url) = spawn_app_with_router(&["family-a"]).await;
    
    let env1 = author_text(&alice, &bob, "hello 1", 1);
    let url = format!("{ws_url}/ws?hints={}&token=family-a&after=0", b64(&env1.recipient_hint));
    println!("TEST: connecting to {}", url);
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    println!("TEST: connected");

    // Post while connected
    println!("TEST: posting envelope");
    post_envelope(&app, "family-a", &env1).await;
    println!("TEST: envelope posted");

    println!("TEST: waiting for socket.next");
    let msg1_opt = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next()).await;
    println!("TEST: received {:?}", msg1_opt);
    let msg1 = msg1_opt.unwrap().unwrap().unwrap();
    assert!(msg1.is_text());
    let txt = msg1.to_text().unwrap();
    assert!(txt.contains(&b64(&env1.msg_id)));
}
