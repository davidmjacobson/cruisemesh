//! DTN_TODOS.md D7 (N2): resource-limit coverage for the relay mailbox —
//! per-envelope sealed-size cap and per-family storage quota — plus the
//! per-family-token request/byte rate limits (`DEPLOY.md` §10).
//!
//! Companion to `e2e_mailbox.rs` (delivery semantics) and `e2e_ws.rs` (push).
//! These tests use raw `sealed` byte blobs rather than real
//! `cruisemesh-core` sealed envelopes: the size/quota gate in
//! `post_envelope` runs on the decoded byte length alone and never inspects
//! ciphertext (DESIGN.md §9 content-agnostic mailbox), so plain filler
//! bytes exercise the same code path.

use std::collections::HashSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cruisemesh_relayd::{app, AppState, RateLimitConfig, RelayStore, MAX_ENVELOPE_SEALED_BYTES};
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

/// GET /envelopes for a single hint — the cheapest "one request" a family
/// can make, used to spend request allowance without touching the byte one.
async fn fetch(app: &Router, token: &str) -> Response {
    let request = Request::builder()
        .method("GET")
        .uri(format!("/envelopes?hints={}", b64(&hint(1))))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(request).await.unwrap()
}

async fn get_healthz(app: &Router) -> Response {
    let request = Request::builder()
        .method("GET")
        .uri("/healthz")
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(request).await.unwrap()
}

fn retry_after_secs(response: &Response) -> u64 {
    response
        .headers()
        .get("retry-after")
        .expect("a 429 must carry Retry-After so clients back off deterministically")
        .to_str()
        .unwrap()
        .parse()
        .expect("Retry-After must be integer delta-seconds")
}

fn test_app_with_rate_limits(
    tokens: &[&str],
    rate_limits: RateLimitConfig,
) -> (NamedTempFile, Router) {
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let auth: HashSet<String> = tokens.iter().map(|t| (*t).to_string()).collect();
    (
        db,
        app(AppState::with_rate_limits(store, auth, rate_limits)),
    )
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

// ---------------------------------------------------------------------
// Rate limits (DEPLOY.md §10). Production allowances are deliberately too
// generous to exhaust in a test, so every case below builds the router with
// a tiny `RateLimitConfig` — same knobs, same code path, milliseconds
// instead of a minute.
// ---------------------------------------------------------------------

#[tokio::test]
async fn family_over_its_request_allowance_gets_429_with_retry_after() {
    let (_db, router) = test_app_with_rate_limits(
        &["family-a"],
        RateLimitConfig {
            requests_per_min: 4,
            ..RateLimitConfig::default()
        },
    );

    for i in 0..4 {
        let response = fetch(&router, "family-a").await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "request {i} is inside the 4-per-minute allowance"
        );
    }

    let limited = fetch(&router, "family-a").await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    // Retry-After must be read before the body consumes the response.
    assert!(
        retry_after_secs(&limited) >= 1,
        "Retry-After must never tell a client to retry immediately"
    );
    let json = body_json(limited).await;
    assert_eq!(json["code"], "rate_limited");
    assert!(
        json["error"].as_str().unwrap().contains("request rate"),
        "the message should name the dimension that tripped: {}",
        json["error"]
    );
}

#[tokio::test]
async fn byte_allowance_is_charged_and_scoped_per_family() {
    // Room for two 2,000-byte posts, not three. Request allowance left at
    // the default so only the byte dimension can trip.
    let (_db, router) = test_app_with_rate_limits(
        &["family-a", "family-b"],
        RateLimitConfig {
            bytes_per_min: 4_096,
            ..RateLimitConfig::default()
        },
    );

    for i in 1..=2u8 {
        let response = post_sealed(
            &router,
            "family-a",
            &msg_id(i),
            &hint(1),
            2_000,
            now_ms() + 60_000,
        )
        .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "post {i} is inside the 4,096-byte-per-minute allowance"
        );
    }

    let limited = post_sealed(
        &router,
        "family-a",
        &msg_id(3),
        &hint(1),
        2_000,
        now_ms() + 60_000,
    )
    .await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(retry_after_secs(&limited) >= 1);
    let json = body_json(limited).await;
    assert_eq!(json["code"], "rate_limited");
    assert!(
        json["error"].as_str().unwrap().contains("byte rate"),
        "the byte dimension should be named, not the request one: {}",
        json["error"]
    );

    // ...and family-b, sharing the same server, is completely unaffected:
    // buckets are per family token, so one family cannot spend another's
    // allowance (the same isolation the storage quota has).
    let unaffected = post_sealed(
        &router,
        "family-b",
        &msg_id(4),
        &hint(1),
        2_000,
        now_ms() + 60_000,
    )
    .await;
    assert_eq!(unaffected.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_limited_family_recovers_as_its_bucket_refills() {
    // 60/min == 1 token/sec: exhaustible in 60 cheap fetches, and one token
    // is back roughly a second later — no minute-long sleep required.
    let (_db, router) = test_app_with_rate_limits(
        &["family-a"],
        RateLimitConfig {
            requests_per_min: 60,
            ..RateLimitConfig::default()
        },
    );

    for _ in 0..60 {
        assert_eq!(fetch(&router, "family-a").await.status(), StatusCode::OK);
    }
    assert_eq!(
        fetch(&router, "family-a").await.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the minute's allowance is spent"
    );

    tokio::time::sleep(Duration::from_millis(1_200)).await;

    assert_eq!(
        fetch(&router, "family-a").await.status(),
        StatusCode::OK,
        "one second of refill should hand back one request"
    );
}

#[tokio::test]
async fn healthz_and_admin_routes_are_never_rate_limited() {
    // One request per minute for everything that *is* limited, so any
    // accidental charging of these routes shows up immediately.
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let router = app(AppState::with_rate_limits(
        store,
        HashSet::from(["family-a".to_string()]),
        RateLimitConfig {
            requests_per_min: 1,
            bytes_per_min: 1,
            global_requests_per_min: 1,
        },
    )
    .with_admin_token(Some("admin-token".to_string())));

    // Spend the family's entire allowance, and prove it is really spent.
    assert_eq!(fetch(&router, "family-a").await.status(), StatusCode::OK);
    assert_eq!(
        fetch(&router, "family-a").await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    // /healthz must stay free: uptime monitors poll it constantly, and a
    // 429 there would read as an outage.
    for _ in 0..5 {
        assert_eq!(get_healthz(&router).await.status(), StatusCode::OK);
    }

    // Admin is a trusted operator path behind its own token, and the
    // purchase flow provisions passes through it — never rate limited.
    let provision = Request::builder()
        .method("POST")
        .uri("/admin/families")
        .header("authorization", "Bearer admin-token")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "token": "hosted-family" }).to_string(),
        ))
        .unwrap();
    assert_eq!(
        router.clone().oneshot(provision).await.unwrap().status(),
        StatusCode::OK
    );
    for _ in 0..5 {
        let lookup = Request::builder()
            .method("GET")
            .uri("/admin/families/hosted-family")
            .header("authorization", "Bearer admin-token")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            router.clone().oneshot(lookup).await.unwrap().status(),
            StatusCode::OK
        );
    }
}

#[tokio::test]
async fn the_global_backstop_trips_independently_of_any_single_family() {
    // Three requests server-wide per minute, but a hundred per family: the
    // only way to trip this is in aggregate.
    let (_db, router) = test_app_with_rate_limits(
        &["family-a", "family-b"],
        RateLimitConfig {
            requests_per_min: 100,
            global_requests_per_min: 3,
            ..RateLimitConfig::default()
        },
    );

    assert_eq!(fetch(&router, "family-a").await.status(), StatusCode::OK);
    assert_eq!(fetch(&router, "family-a").await.status(), StatusCode::OK);
    assert_eq!(fetch(&router, "family-b").await.status(), StatusCode::OK);

    // family-b has spent 1 of its own 100, so only the shared backstop can
    // be what rejects this.
    let limited = fetch(&router, "family-b").await;
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(retry_after_secs(&limited) >= 1);
    let json = body_json(limited).await;
    assert_eq!(json["code"], "rate_limited");
    assert!(
        json["error"].as_str().unwrap().contains("server-wide"),
        "the global scope should be named so an operator can tell them apart: {}",
        json["error"]
    );
}
