//! Group relay durability regression pins (`specs/group-relay-durability.md`
//! §5.5/§6, DTN_TODOS.md D6/N1). The relay itself needs NO changes for
//! per-member fan-out -- these tests pin the two existing server behaviors
//! the design leans on, so a future relayd change can't silently break them:
//!
//! 1. Acking one row never touches sibling rows (per-member fan-out rows are
//!    independent `(family_token, msg_id)` entries, so each member's ack
//!    deletes only their own copy).
//! 2. Re-posting the same row set (author retry, a second member mule) is
//!    absorbed by the `ON CONFLICT` dedupe -- same ids, no duplicate rows.
//!
//! Like `e2e_limits.rs`, raw filler bytes stand in for sealed ciphertext:
//! the mailbox is content-agnostic (DESIGN.md §9).

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
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

async fn body_json(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn test_app() -> Router {
    let file = NamedTempFile::new().unwrap();
    let store = RelayStore::open(file.path().to_str().unwrap()).unwrap();
    let tokens: HashSet<String> = ["family-a".to_string()].into_iter().collect();
    app(AppState::new(store, tokens))
}

async fn post_row(app: &Router, msg_id: &[u8], hint: &[u8]) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri("/envelopes")
        .header("authorization", "Bearer family-a")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "msg_id": b64(msg_id),
                "hop_ttl": 7,
                "recipient_hint": b64(hint),
                "sealed": b64(&vec![9u8; 64]),
                "expiry_ms": now_ms() + 60_000,
            })
            .to_string(),
        ))
        .unwrap();
    app.clone().oneshot(request).await.unwrap()
}

async fn fetch_ids(app: &Router, hint: &[u8]) -> Vec<i64> {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/envelopes?hints={}&after=0&limit=100", b64(hint)))
        .header("authorization", "Bearer family-a")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET /envelopes failed");
    body_json(response).await["envelopes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|env| env["id"].as_i64().unwrap())
        .collect()
}

async fn ack(app: &Router, ids: &[i64]) {
    let request = Request::builder()
        .method("POST")
        .uri("/envelopes/ack")
        .header("authorization", "Bearer family-a")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::json!({ "ids": ids }).to_string()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK, "ack failed");
}

/// Three members' fan-out rows for one group message; member C acks theirs;
/// members A and B can still fetch their own rows afterward.
#[tokio::test]
async fn per_row_ack_leaves_sibling_fanout_rows_intact() {
    let app = test_app();
    // Distinct derived msg_ids + per-member hints, as core_group_fanout_rows
    // produces (the derivation itself is pinned in core's own tests).
    let rows: Vec<(Vec<u8>, Vec<u8>)> = (1u8..=3)
        .map(|member| (vec![member; 16], vec![member; 8]))
        .collect();
    for (msg_id, hint) in &rows {
        assert_eq!(post_row(&app, msg_id, hint).await.status(), StatusCode::OK);
    }

    let c_ids = fetch_ids(&app, &rows[2].1).await;
    assert_eq!(c_ids.len(), 1);
    ack(&app, &c_ids).await;

    assert!(
        fetch_ids(&app, &rows[2].1).await.is_empty(),
        "C's row acked away"
    );
    assert_eq!(
        fetch_ids(&app, &rows[0].1).await.len(),
        1,
        "A's row survives"
    );
    assert_eq!(
        fetch_ids(&app, &rows[1].1).await.len(),
        1,
        "B's row survives"
    );
}

/// A retried upload (author retry / second member mule) re-posts the same
/// deterministic row set; the mailbox absorbs it with no duplicates.
#[tokio::test]
async fn reposting_the_same_fanout_rows_dedupes() {
    let app = test_app();
    let msg_id = vec![0x44u8; 16];
    let hint = vec![0x44u8; 8];
    for _ in 0..3 {
        assert_eq!(
            post_row(&app, &msg_id, &hint).await.status(),
            StatusCode::OK
        );
    }
    assert_eq!(
        fetch_ids(&app, &hint).await.len(),
        1,
        "same derived msg_id must stay one row across retries"
    );
}
