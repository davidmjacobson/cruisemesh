//! DTN_TODOS.md D7 (N2): resource-limit coverage for the relay mailbox —
//! per-envelope sealed-size cap and per-family storage quota.
//!
//! Companion to `e2e_mailbox.rs` (delivery semantics) and `e2e_ws.rs` (push).
//! These tests use raw `sealed` byte blobs rather than real
//! `cruisemesh-core` sealed envelopes: the size/quota gate in
//! `post_envelope` runs on the decoded byte length alone and never inspects
//! ciphertext (DESIGN.md §9 content-agnostic mailbox), so plain filler
//! bytes exercise the same code path.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cruisemesh_relayd::{app, AppState, RelayStore, MAX_ENVELOPE_SEALED_BYTES};
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

fn hint(byte: u8) -> Vec<u8> {
    vec![byte; 8]
}

fn msg_id(byte: u8) -> Vec<u8> {
    vec![byte; 16]
}

async fn body_json(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// POST /envelopes with an arbitrary raw `sealed` blob. Returns the raw
/// response so callers can assert on either success or a specific
/// rejection kind.
async fn post_sealed(
    app: &Router,
    token: &str,
    msg_id_bytes: &[u8],
    hint_bytes: &[u8],
    sealed_len: usize,
    expiry_ms: i64,
) -> Response {
    let request = Request::builder()
        .method("POST")
        .uri("/envelopes")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "msg_id": b64(msg_id_bytes),
                "hop_ttl": 7,
                "recipient_hint": b64(hint_bytes),
                "sealed": b64(&vec![9u8; sealed_len]),
                "expiry_ms": expiry_ms,
            })
            .to_string(),
        ))
        .unwrap();
    app.clone().oneshot(request).await.unwrap()
}

fn test_app_with_quota(
    tokens: &[&str],
    family_quota_bytes: u64,
) -> (NamedTempFile, Router, RelayStore) {
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let auth: HashSet<String> = tokens.iter().map(|t| (*t).to_string()).collect();
    let router = app(AppState::with_family_quota_bytes(
        store.clone(),
        auth,
        family_quota_bytes,
    ));
    (db, router, store)
}

#[tokio::test]
async fn oversized_envelope_is_rejected_with_413_and_distinct_code() {
    let (_db, router, _store) = test_app_with_quota(&["family-a"], u64::MAX);

    let response = post_sealed(
        &router,
        "family-a",
        &msg_id(1),
        &hint(1),
        MAX_ENVELOPE_SEALED_BYTES + 1,
        now_ms() + 60_000,
    )
    .await;

    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let json = body_json(response).await;
    assert_eq!(json["code"], "envelope_too_large");
}

#[tokio::test]
async fn under_quota_posts_are_unaffected() {
    // Realistic-but-tight quota: room for a handful of envelopes, not one.
    let (_db, router, _store) = test_app_with_quota(&["family-a"], 10_000);

    for i in 0..5u8 {
        let response = post_sealed(
            &router,
            "family-a",
            &msg_id(i),
            &hint(1),
            500,
            now_ms() + 60_000,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "post {i} should be well under the 10,000-byte quota"
        );
    }
}

#[tokio::test]
async fn quota_exceeded_prunes_expired_rows_then_succeeds() {
    let (_db, router, store) = test_app_with_quota(&["family-a"], 1_000);

    // Pre-seed an EXPIRED row that eats most of the quota. Inserted
    // directly via the store (not HTTP) so its expiry is fully controlled.
    store
        .insert_envelope(
            "family-a",
            msg_id(1),
            7,
            hint(1),
            vec![1u8; 900],
            now_ms() - 1, // already expired
            now_ms() - 10_000,
        )
        .unwrap();
    assert_eq!(store.family_sealed_bytes("family-a").unwrap(), 900);

    // 900 (stale) + 200 (new) = 1,100 > 1,000 quota on the naive check, but
    // the stale row is expired, so prune_expired should free enough room.
    let response = post_sealed(
        &router,
        "family-a",
        &msg_id(2),
        &hint(1),
        200,
        now_ms() + 60_000,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "prune should have freed enough quota"
    );

    // The expired row is gone; only the new envelope's bytes count now.
    assert_eq!(store.family_sealed_bytes("family-a").unwrap(), 200);
    assert_eq!(store.count_for_family("family-a").unwrap(), 1);
}

#[tokio::test]
async fn quota_still_exceeded_after_prune_is_rejected_with_507_and_distinct_code() {
    let (_db, router, store) = test_app_with_quota(&["family-a"], 1_000);

    // Pre-seed a LIVE (non-expired) row consuming most of the quota —
    // pruning cannot free this; durability means it must never be evicted.
    store
        .insert_envelope(
            "family-a",
            msg_id(1),
            7,
            hint(1),
            vec![1u8; 900],
            now_ms() + 60_000,
            now_ms(),
        )
        .unwrap();

    let response = post_sealed(
        &router,
        "family-a",
        &msg_id(2),
        &hint(1),
        200,
        now_ms() + 60_000,
    )
    .await;

    assert_eq!(response.status(), StatusCode::INSUFFICIENT_STORAGE);
    let json = body_json(response).await;
    assert_eq!(json["code"], "family_quota_exceeded");

    // Rejected: the original unacked row is untouched (never silently
    // evicted) and the new one was never stored.
    assert_eq!(store.family_sealed_bytes("family-a").unwrap(), 900);
    assert_eq!(store.count_for_family("family-a").unwrap(), 1);
}

#[tokio::test]
async fn quota_is_scoped_per_family() {
    let (_db, router, _store) = test_app_with_quota(&["family-a", "family-b"], 1_000);

    // family-a fills its quota...
    let full = post_sealed(
        &router,
        "family-a",
        &msg_id(1),
        &hint(1),
        950,
        now_ms() + 60_000,
    )
    .await;
    assert_eq!(full.status(), StatusCode::OK);
    let rejected = post_sealed(
        &router,
        "family-a",
        &msg_id(2),
        &hint(1),
        100,
        now_ms() + 60_000,
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::INSUFFICIENT_STORAGE);

    // ...but family-b, sharing the same server, is unaffected.
    let unaffected = post_sealed(
        &router,
        "family-b",
        &msg_id(3),
        &hint(1),
        950,
        now_ms() + 60_000,
    )
    .await;
    assert_eq!(unaffected.status(), StatusCode::OK);
}

#[tokio::test]
async fn dedupe_repost_of_existing_msg_id_is_never_quota_checked() {
    // Quota sized to fit exactly one envelope's worth of bytes.
    let (_db, router, _store) = test_app_with_quota(&["family-a"], 500);

    let first = post_sealed(
        &router,
        "family-a",
        &msg_id(7),
        &hint(1),
        500,
        now_ms() + 60_000,
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);

    // Re-posting the SAME msg_id is dedupe (insert_envelope's ON CONFLICT
    // path, ack_only_deletes-style idempotency) and must not be charged
    // again against the now-full quota, or legitimate retries (e.g. a
    // receipt envelope re-uploaded every sync) would start failing once a
    // family's mailbox is merely full rather than growing.
    let repost = post_sealed(
        &router,
        "family-a",
        &msg_id(7),
        &hint(1),
        500,
        now_ms() + 120_000,
    )
    .await;
    assert_eq!(
        repost.status(),
        StatusCode::OK,
        "re-posting an existing msg_id must not be quota-checked"
    );
}
