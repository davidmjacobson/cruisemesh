//! WebSocket push e2e tests (`GET /ws`) using `tokio-tungstenite` + real TCP.
//!
//! Covers: auth reject; replay-on-connect matches poll; live push; two-family
//! isolation; slow-consumer disconnect. Round-1 mailbox tests live in
//! `e2e_mailbox.rs` and stay unchanged.

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cruisemesh_core::{
    compute_recipient_hint, default_expiry, encode_message_body, generate_identity, generate_msg_id,
    seal_message, Identity, MessageBody, DEFAULT_HOP_TTL, KIND_TEXT,
};
use cruisemesh_relayd::{app, AppState, RelayStore, WS_MAX_INBOUND_MESSAGE_BYTES};
use futures_util::{SinkExt, StreamExt};
use tempfile::NamedTempFile;
use tokio::net::TcpListener;
use tower::util::ServiceExt;
use axum::body::Body;
use axum::http::{Request, StatusCode};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis() as i64
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

struct AuthoredEnvelope {
    msg_id: Vec<u8>,
    hop_ttl: u8,
    recipient_hint: Vec<u8>,
    sealed: Vec<u8>,
    expiry_ms: i64,
}

fn author_text(sender: &Identity, recipient: &Identity, text: &str, lamport: u64) -> AuthoredEnvelope {
    let timestamp = now_ms();
    let body = MessageBody {
        kind: KIND_TEXT,
        chat_id: sender.user_id.clone(),
        lamport,
        timestamp,
        content: text.as_bytes().to_vec(),
    };
    let payload = encode_message_body(body);
    let sealed = seal_message(sender.clone(), recipient.agree_pk.clone(), payload).unwrap();
    AuthoredEnvelope {
        msg_id: generate_msg_id(),
        hop_ttl: DEFAULT_HOP_TTL,
        recipient_hint: compute_recipient_hint(recipient.user_id.clone(), timestamp),
        sealed,
        expiry_ms: default_expiry(timestamp),
    }
}

async fn spawn_router(state: AppState) -> (Router, String) {
    let router = app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serve = router.clone();
    tokio::spawn(async move {
        axum::serve(listener, serve).await.unwrap();
    });
    (router, format!("ws://{addr}"))
}

async fn post_envelope(router: &Router, token: &str, env: &AuthoredEnvelope) {
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
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn get_envelopes_json(
    router: &Router,
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
    let response = router.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn ws_auth_reject() {
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let (_router, ws_url) = spawn_router(AppState::new(
        store,
        HashSet::from(["family-a".to_string()]),
    ))
    .await;
    let url = format!("{ws_url}/ws?hints={}&token=bad-token", b64(&[1u8; 8]));
    assert!(
        tokio_tungstenite::connect_async(&url).await.is_err(),
        "bad token must fail handshake"
    );
}

#[tokio::test]
async fn ws_drops_clients_that_send_oversized_messages() {
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let (_router, ws_url) = spawn_router(AppState::new(
        store,
        HashSet::from(["family-a".to_string()]),
    ))
    .await;
    let url = format!("{ws_url}/ws?hints={}&token=family-a", b64(&[1u8; 8]));
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    socket
        .send(tokio_tungstenite::tungstenite::Message::Text(
            "x".repeat(WS_MAX_INBOUND_MESSAGE_BYTES + 1).into(),
        ))
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("server did not close the oversized websocket");
    match result {
        None | Some(Err(_)) => {}
        Some(Ok(message)) => assert!(message.is_close()),
    }
}

#[tokio::test]
async fn ws_replay_on_connect_matches_poll() {
    let alice = generate_identity();
    let bob = generate_identity();
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let (router, ws_url) = spawn_router(AppState::new(
        store,
        HashSet::from(["family-a".to_string()]),
    ))
    .await;

    let env1 = author_text(&alice, &bob, "hello 1", 1);
    let env2 = author_text(&alice, &bob, "hello 2", 2);
    post_envelope(&router, "family-a", &env1).await;
    post_envelope(&router, "family-a", &env2).await;

    let polled = get_envelopes_json(&router, "family-a", &[env1.recipient_hint.clone()], 0).await;
    assert_eq!(polled["envelopes"].as_array().unwrap().len(), 2);

    let url = format!(
        "{ws_url}/ws?hints={}&token=family-a&after=0",
        b64(&env1.recipient_hint)
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    let msg1 = socket.next().await.unwrap().unwrap();
    assert!(msg1.is_text());
    assert!(msg1.to_text().unwrap().contains(&b64(&env1.msg_id)));

    let msg2 = socket.next().await.unwrap().unwrap();
    assert!(msg2.is_text());
    assert!(msg2.to_text().unwrap().contains(&b64(&env2.msg_id)));
}

#[tokio::test]
async fn ws_live_push_after_connect() {
    let alice = generate_identity();
    let bob = generate_identity();
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let (router, ws_url) = spawn_router(AppState::new(
        store,
        HashSet::from(["family-a".to_string()]),
    ))
    .await;

    let env1 = author_text(&alice, &bob, "live push", 1);
    let url = format!(
        "{ws_url}/ws?hints={}&token=family-a&after=0",
        b64(&env1.recipient_hint)
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();
    tokio::task::yield_now().await;

    post_envelope(&router, "family-a", &env1).await;

    let msg = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .expect("live push timeout")
        .unwrap()
        .unwrap();
    assert!(msg.is_text());
    assert!(msg.to_text().unwrap().contains(&b64(&env1.msg_id)));
}

#[tokio::test]
async fn ws_two_families_isolated() {
    let alice = generate_identity();
    let bob = generate_identity();
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let (router, ws_url) = spawn_router(AppState::new(
        store,
        HashSet::from(["family-a".to_string(), "family-b".to_string()]),
    ))
    .await;

    let env1 = author_text(&alice, &bob, "iso", 1);
    let url_b = format!(
        "{ws_url}/ws?hints={}&token=family-b&after=0",
        b64(&env1.recipient_hint)
    );
    let (mut socket_b, _) = tokio_tungstenite::connect_async(&url_b).await.unwrap();
    tokio::task::yield_now().await;

    post_envelope(&router, "family-a", &env1).await;

    let result = tokio::time::timeout(Duration::from_millis(200), socket_b.next()).await;
    assert!(result.is_err(), "family-b must not see family-a push");
}

#[tokio::test]
async fn ws_slow_consumer_disconnect_path() {
    let alice = generate_identity();
    let bob = generate_identity();
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let (router, ws_url) = spawn_router(AppState::with_hub_capacity(
        store,
        HashSet::from(["family-a".to_string()]),
        4,
    ))
    .await;

    let seed = author_text(&alice, &bob, "seed", 1);
    let url = format!(
        "{ws_url}/ws?hints={}&token=family-a&after=0",
        b64(&seed.recipient_hint)
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    for i in 0..64u64 {
        let mut env = author_text(&alice, &bob, &format!("flood {i}"), i + 1);
        env.recipient_hint = seed.recipient_hint.clone();
        post_envelope(&router, "family-a", &env).await;
    }

    let mut saw_close = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), socket.next()).await {
            Ok(None) | Ok(Some(Err(_))) => {
                saw_close = true;
                break;
            }
            Ok(Some(Ok(msg))) if msg.is_close() => {
                saw_close = true;
                break;
            }
            Ok(Some(Ok(_))) => continue,
            Err(_) => continue,
        }
    }
    assert!(saw_close, "slow consumer must be disconnected");
}
