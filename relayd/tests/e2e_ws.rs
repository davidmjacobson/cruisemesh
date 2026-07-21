//! WebSocket push e2e tests (`GET /ws`) using `tokio-tungstenite` + real TCP.
//!
//! Covers: auth reject; replay-on-connect matches poll; live push; two-family
//! isolation; slow-consumer disconnect. Round-1 mailbox tests live in
//! `e2e_mailbox.rs` and stay unchanged.

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cruisemesh_core::{
    compute_recipient_hint, default_expiry, encode_message_body, generate_identity,
    generate_msg_id, seal_message, Identity, MessageBody, DEFAULT_HOP_TTL, KIND_TEXT,
};
use cruisemesh_relayd::{app, AppState, RelayStore, WsLimitsConfig, WS_MAX_INBOUND_MESSAGE_BYTES};
use futures_util::{SinkExt, StreamExt};
use tempfile::NamedTempFile;
use tokio::net::TcpListener;
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

struct AuthoredEnvelope {
    msg_id: Vec<u8>,
    hop_ttl: u8,
    recipient_hint: Vec<u8>,
    sealed: Vec<u8>,
    expiry_ms: i64,
}

fn author_text(
    sender: &Identity,
    recipient: &Identity,
    text: &str,
    lamport: u64,
) -> AuthoredEnvelope {
    let timestamp = now_ms();
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
        .uri(format!(
            "/envelopes?hints={hints_q}&after={after}&limit=100"
        ))
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

// FR5: this timeout guards against a genuine hang (the WS logic itself is
// provably race-free — subscribe precedes replay, insert precedes broadcast,
// so a missed live push is always covered by replay). The old 3 s bound
// fired under CPU starvation from a parallel build/test load, not from an
// actual bug; 60 s plus a multi-thread runtime gives the scheduler enough
// slack that only a real hang trips it.
#[tokio::test(flavor = "multi_thread")]
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

    let msg = tokio::time::timeout(Duration::from_secs(60), socket.next())
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

// FR5: raise the deadline (guards a hang, not speed) but keep the default
// current_thread runtime — this test's lag/drop condition is deliberately
// induced by single-thread scheduling contention between the flood-posting
// test task and the WS-handler task; running it multi-threaded lets the
// handler drain the broadcast channel in parallel and the slow-consumer
// condition never triggers.
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
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

// --- FR6: connection caps + keepalive ---

#[tokio::test]
async fn ws_over_per_token_cap_upgrade_is_refused() {
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let ws_limits = WsLimitsConfig {
        per_token_max_connections: 1,
        global_max_connections: 10,
        ..WsLimitsConfig::default()
    };
    let (_router, ws_url) = spawn_router(AppState::with_ws_limits(
        store,
        HashSet::from(["family-a".to_string()]),
        ws_limits,
    ))
    .await;
    let url = format!("{ws_url}/ws?hints={}&token=family-a", b64(&[1u8; 8]));

    // First upgrade consumes the family's only permit; hold it open.
    let (first_socket, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("first upgrade must succeed");

    // Second upgrade for the same token must be refused with 429, not hang
    // or silently queue.
    match tokio_tungstenite::connect_async(&url).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        }
        other => panic!("expected an HTTP 429 handshake rejection, got {other:?}"),
    }

    drop(first_socket);
}

#[tokio::test]
async fn ws_over_global_cap_upgrade_is_refused_even_for_different_tokens() {
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let ws_limits = WsLimitsConfig {
        per_token_max_connections: 10,
        global_max_connections: 1,
        ..WsLimitsConfig::default()
    };
    let (_router, ws_url) = spawn_router(AppState::with_ws_limits(
        store,
        HashSet::from(["family-a".to_string(), "family-b".to_string()]),
        ws_limits,
    ))
    .await;
    let hint = b64(&[1u8; 8]);
    let url_a = format!("{ws_url}/ws?hints={hint}&token=family-a");
    let url_b = format!("{ws_url}/ws?hints={hint}&token=family-b");

    let (first_socket, _) = tokio_tungstenite::connect_async(&url_a)
        .await
        .expect("first upgrade must succeed");

    // A *different* family token still gets refused -- the global cap, not
    // the per-token cap, is what's saturated here.
    match tokio_tungstenite::connect_async(&url_b).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        }
        other => panic!("expected an HTTP 429 handshake rejection, got {other:?}"),
    }

    drop(first_socket);
}

// FR6: simulates a "silently-dead phone" -- a client that completes the WS
// handshake and then never reads or writes again (unlike a real dead
// socket, the TCP connection itself stays fully open; only the WS peer
// goes silent). The server's keepalive ping still goes out on the wire
// (the OS write succeeds even though nobody reads it), but with nobody to
// answer, `missed_pings` should cross `ping_missed_limit` and the server
// should reap the connection -- freeing its per-token permit. A tiny ping
// interval keeps this test fast instead of waiting on the 45 s production
// default.
#[tokio::test]
async fn ws_dead_peer_is_reaped_by_keepalive_and_frees_its_permit() {
    let alice = generate_identity();
    let bob = generate_identity();
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let ws_limits = WsLimitsConfig {
        per_token_max_connections: 1,
        global_max_connections: 10,
        ping_interval: Duration::from_millis(50),
        ping_missed_limit: 2,
    };
    let (_router, ws_url) = spawn_router(AppState::with_ws_limits(
        store,
        HashSet::from(["family-a".to_string()]),
        ws_limits,
    ))
    .await;

    let env = author_text(&alice, &bob, "keepalive", 1);
    let url = format!(
        "{ws_url}/ws?hints={}&token=family-a&after=0",
        b64(&env.recipient_hint)
    );

    // Connect and immediately stop touching the socket -- never call
    // .next() again, so this client never reads (and thus never answers)
    // the server's pings. Holding the value alive keeps the TCP connection
    // open without polling it.
    let (dead_socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

    // While the dead connection holds the family's only permit, a fresh
    // upgrade attempt for the same token must be refused.
    match tokio_tungstenite::connect_async(&url).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        }
        other => panic!("expected an HTTP 429 handshake rejection, got {other:?}"),
    }

    // Give the keepalive loop time to send `ping_missed_limit` unanswered
    // pings and reap the dead connection (well under the 60 s CI budget
    // used elsewhere in this file).
    tokio::time::sleep(Duration::from_millis(50 * 6)).await;

    // The permit should be free again now.
    let reconnect = tokio::time::timeout(
        Duration::from_secs(5),
        tokio_tungstenite::connect_async(&url),
    )
    .await
    .expect("reconnect attempt timed out")
    .expect("permit should have been freed by the keepalive reap");
    drop(reconnect);
    drop(dead_socket);
}
