//! `PRESENCE-01`: what a credential from outside the answering family may ask
//! `POST /presence`, what it is told back, and what the asking costs the
//! family that answers.
//!
//! Companion to `e2e_limits.rs`, which owns the request/byte allowances these
//! tests must prove presence cannot spend. The split is the point of the
//! suite: a friend-card (deposit) credential may put a presence query, and
//! however hard it puts one, the family whose relay answers keeps every
//! request and every byte of its own allowance.

use std::collections::HashSet;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cruisemesh_relayd::{
    app, deposit_token_for, AppState, RateLimitConfig, RelayStore, MAX_DEPOSIT_PRESENCE_QUERY,
};
use tempfile::NamedTempFile;
use tower::util::ServiceExt;

const ADMIN_TOKEN: &str = "admin-token";

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Recipient hints are 8 bytes on the wire; `RECIPIENT_HINT_LEN` is not
/// exported, so the tests build them the same way `e2e_limits.rs` does.
fn hint(byte: u8) -> Vec<u8> {
    vec![byte; 8]
}

async fn body_json(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
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

fn presence(token: &str, announce: &[Vec<u8>], query: &[Vec<u8>]) -> Request<Body> {
    let announce: Vec<String> = announce.iter().map(|h| b64(h)).collect();
    let query: Vec<String> = query.iter().map(|h| b64(h)).collect();
    Request::builder()
        .method("POST")
        .uri("/presence")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "announce": announce, "query": query }).to_string(),
        ))
        .unwrap()
}

/// The cheapest "one request" a family can make, for spending — or proving it
/// still holds — its member-class request allowance.
fn fetch(token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(format!("/envelopes?hints={}", b64(&hint(1))))
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn post_envelope(token: &str, msg_id: u8) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/envelopes")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "msg_id": b64(&[msg_id; 16]),
                "hop_ttl": 7,
                "recipient_hint": b64(&hint(1)),
                "sealed": b64(&[9u8; 32]),
                "expiry_ms": 4_102_444_800_000i64,
            })
            .to_string(),
        ))
        .unwrap()
}

fn admin_json(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn test_app(tokens: &[&str], rate_limits: RateLimitConfig) -> (NamedTempFile, Router) {
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let auth: HashSet<String> = tokens.iter().map(|t| (*t).to_string()).collect();
    (
        db,
        app(AppState::with_rate_limits(store, auth, rate_limits)
            .with_admin_token(Some(ADMIN_TOKEN.to_string()))),
    )
}

/// Put a presence row on the board for `hint`, announced by the family's own
/// member token, so a cross-family query has something to find.
async fn announce_as_member(router: &Router, token: &str, hint_bytes: &[u8]) {
    let response = router
        .clone()
        .oneshot(presence(token, &[hint_bytes.to_vec()], &[]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

// ---------------------------------------------------------------------------
// The cap, and its Retry-After
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cross_family_query_is_capped_per_credential_and_says_when_to_retry() {
    // Two queries per fifteen-minute window: small enough to exhaust in a
    // test, the same *shape* as the deployed allowance (small burst, long
    // window) rather than a per-minute figure.
    let (_db, router) = test_app(
        &["family-a"],
        RateLimitConfig {
            deposit_presence_queries: 2,
            deposit_presence_window_secs: 900,
            ..RateLimitConfig::default()
        },
    );
    let deposit = deposit_token_for("family-a");

    for i in 1..=2 {
        let response = router
            .clone()
            .oneshot(presence(&deposit, &[], &[hint(1)]))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "cross-family query {i} is inside the allowance"
        );
    }

    let limited = router
        .clone()
        .oneshot(presence(&deposit, &[], &[hint(1)]))
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);
    // A long window makes the honest wait longer than the advertised ceiling;
    // the client is told a real number it can sleep on either way.
    assert!(retry_after_secs(&limited) >= 1);
    assert_eq!(body_json(limited).await["code"], "rate_limited");
}

// ---------------------------------------------------------------------------
// Budget separation — the paired assertion
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hammering_presence_never_spends_the_queried_family_s_own_allowance() {
    // The family gets exactly four requests a minute and four envelope-sized
    // bytes... which is to say, an allowance so tight that *any* leakage from
    // the presence flood into it would show up as a 429 on the family's own
    // traffic. The presence allowance is generous by comparison, so the flood
    // is limited by the loop rather than by the bucket.
    let (_db, router) = test_app(
        &["family-a"],
        RateLimitConfig {
            requests_per_min: 4,
            deposit_requests_per_min: 4,
            deposit_presence_queries: 64,
            deposit_presence_window_secs: 900,
            ..RateLimitConfig::default()
        },
    );
    let deposit = deposit_token_for("family-a");

    for i in 0..40 {
        let response = router
            .clone()
            .oneshot(presence(&deposit, &[], &[hint(1)]))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "presence query {i} is inside the presence allowance"
        );
    }

    // The paired assertion. Forty cross-family queries have gone through, and
    // the family's own four-request allowance is untouched: four member
    // requests still succeed, and only the fifth — the one past the family's
    // own limit — is refused.
    for i in 1..=4 {
        assert_eq!(
            router
                .clone()
                .oneshot(fetch("family-a"))
                .await
                .unwrap()
                .status(),
            StatusCode::OK,
            "member request {i} of the family's own four-per-minute allowance"
        );
    }
    assert_eq!(
        router
            .clone()
            .oneshot(fetch("family-a"))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS,
        "the fifth member request is refused by the family's own limit, not the presence flood"
    );

    // And the deposit credential's *post* allowance is equally untouched: the
    // friend whose card this is can still deliver mail after asking.
    for i in 1..=4u8 {
        assert_eq!(
            router
                .clone()
                .oneshot(post_envelope(&deposit, i))
                .await
                .unwrap()
                .status(),
            StatusCode::OK,
            "deposit post {i} of the deposit class's own four-per-minute allowance"
        );
    }
}

#[tokio::test]
async fn a_member_presence_call_still_charges_the_ordinary_request_bucket() {
    // The separation is for cross-family callers only. A family asking its
    // own relay is making an ordinary read, and it must keep costing one.
    let (_db, router) = test_app(
        &["family-a"],
        RateLimitConfig {
            requests_per_min: 2,
            ..RateLimitConfig::default()
        },
    );

    for i in 1..=2 {
        assert_eq!(
            router
                .clone()
                .oneshot(presence("family-a", &[hint(1)], &[hint(2)]))
                .await
                .unwrap()
                .status(),
            StatusCode::OK,
            "member presence {i} inside the two-per-minute allowance"
        );
    }
    assert_eq!(
        router
            .clone()
            .oneshot(presence("family-a", &[hint(1)], &[hint(2)]))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS,
        "member presence is charged to the family request bucket like any other read"
    );
}

// ---------------------------------------------------------------------------
// What a cross-family caller may ask
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cross_family_caller_may_query_but_never_announce() {
    let (_db, router) = test_app(&["family-a"], RateLimitConfig::default());
    let deposit = deposit_token_for("family-a");

    let refused = router
        .clone()
        .oneshot(presence(&deposit, &[hint(7)], &[hint(1)]))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(refused).await["code"], "presence_query_only");

    // Nothing was recorded by the refused call: a member querying that hint
    // afterwards finds no row, so the announcement did not land before the
    // check.
    let response = router
        .clone()
        .oneshot(presence("family-a", &[], &[hint(7)]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_json(response).await["presence"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn a_cross_family_query_cannot_sweep_a_dictionary_of_hints() {
    let (_db, router) = test_app(&["family-a"], RateLimitConfig::default());
    let deposit = deposit_token_for("family-a");

    let ok: Vec<Vec<u8>> = (0..MAX_DEPOSIT_PRESENCE_QUERY as u8).map(hint).collect();
    assert_eq!(
        router
            .clone()
            .oneshot(presence(&deposit, &[], &ok))
            .await
            .unwrap()
            .status(),
        StatusCode::OK,
        "a full contact's worth of rotating hints is a legitimate ask"
    );

    let too_many: Vec<Vec<u8>> = (0..=MAX_DEPOSIT_PRESENCE_QUERY as u8).map(hint).collect();
    assert_eq!(
        router
            .clone()
            .oneshot(presence(&deposit, &[], &too_many))
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST,
        "one hint past the cross-family cap is refused, even though a member could ask it"
    );

    // The member cap is unchanged: a family still asks about everyone it
    // knows in one call.
    let many: Vec<Vec<u8>> = (0..64u8).map(hint).collect();
    assert_eq!(
        router
            .clone()
            .oneshot(presence("family-a", &[], &many))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

// ---------------------------------------------------------------------------
// Coarse for them, precise for us
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cross_family_answer_is_a_bucket_and_a_same_family_answer_is_a_stamp() {
    let (_db, router) = test_app(&["family-a"], RateLimitConfig::default());
    let deposit = deposit_token_for("family-a");
    announce_as_member(&router, "family-a", &hint(3)).await;

    // Same family: the exact stamp, no bucket label.
    let response = router
        .clone()
        .oneshot(presence("family-a", &[], &[hint(3)]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let precise = &json["presence"][0];
    assert_eq!(precise["hint"], b64(&hint(3)));
    assert!(
        precise["recency"].is_null(),
        "a same-family answer carries no bucket label: it is not a bucket"
    );
    let precise_age = json["now_ms"].as_i64().unwrap() - precise["last_seen_ms"].as_i64().unwrap();
    assert!(
        (0..1_000).contains(&precise_age),
        "the row was announced moments ago and the same-family answer says so exactly, \
         got {precise_age}ms"
    );

    // Cross family: the same row, rounded to the "active" bucket, reported as
    // the oldest instant still inside it.
    let response = router
        .clone()
        .oneshot(presence(&deposit, &[], &[hint(3)]))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    let coarse = &json["presence"][0];
    assert_eq!(coarse["hint"], b64(&hint(3)));
    assert_eq!(coarse["recency"], "active");
    let coarse_age = json["now_ms"].as_i64().unwrap() - coarse["last_seen_ms"].as_i64().unwrap();
    assert_eq!(
        coarse_age, 149_999,
        "the coarse answer is the whole active window, one millisecond in, whatever the \
         precise age was"
    );
    assert!(
        coarse_age > precise_age,
        "coarsening may only ever round a stamp older, never newer"
    );
}

#[tokio::test]
async fn an_older_row_lands_in_an_older_bucket() {
    // Straight through the store, so the age is controlled rather than
    // observed: a row last seen an hour ago is a "day" answer to a
    // cross-family caller and an exact stamp to the family itself.
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let hour_ago = chrono_free_now_ms() - 60 * 60 * 1000;
    store
        .sync_presence("family-a", &[hint(4)], &[], hour_ago)
        .unwrap();
    let router = app(AppState::with_rate_limits(
        store,
        HashSet::from(["family-a".to_string()]),
        RateLimitConfig::default(),
    ));
    let deposit = deposit_token_for("family-a");

    let response = router
        .clone()
        .oneshot(presence(&deposit, &[], &[hint(4)]))
        .await
        .unwrap();
    let json = body_json(response).await;
    assert_eq!(json["presence"][0]["recency"], "day");
    let age =
        json["now_ms"].as_i64().unwrap() - json["presence"][0]["last_seen_ms"].as_i64().unwrap();
    assert_eq!(age, 24 * 60 * 60 * 1000 - 1);
}

fn chrono_free_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis() as i64
}

// ---------------------------------------------------------------------------
// A credential that should get nothing, gets nothing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_suspended_family_answers_no_presence_to_anyone() {
    let (_db, router) = test_app(&[], RateLimitConfig::default());
    assert_eq!(
        router
            .clone()
            .oneshot(admin_json(
                "POST",
                "/admin/families",
                serde_json::json!({ "token": "hosted-family" }),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let deposit = deposit_token_for("hosted-family");

    // Active: the query is answered.
    assert_eq!(
        router
            .clone()
            .oneshot(presence(&deposit, &[], &[hint(1)]))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    assert_eq!(
        router
            .clone()
            .oneshot(admin_json(
                "PATCH",
                "/admin/families/hosted-family",
                serde_json::json!({ "status": "suspended" }),
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // Suspended: nothing, and for the same stated reason the family's own
    // member token gets nothing. The class boundary does not become a way
    // around the billing state.
    for (token, who) in [(deposit.as_str(), "deposit"), ("hosted-family", "member")] {
        let response = router
            .clone()
            .oneshot(presence(token, &[], &[hint(1)]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{who}");
        assert_eq!(
            body_json(response).await["code"],
            "family_suspended",
            "{who}"
        );
    }
}

#[tokio::test]
async fn presence_did_not_open_the_mailbox() {
    // The point of the class split is unchanged by this route existing: a
    // deposit credential still cannot read a single envelope.
    let (_db, router) = test_app(&["family-a"], RateLimitConfig::default());
    let deposit = deposit_token_for("family-a");

    assert_eq!(
        router
            .clone()
            .oneshot(presence(&deposit, &[], &[hint(1)]))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let refused = router.clone().oneshot(fetch(&deposit)).await.unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(refused).await["code"], "deposit_only");
}
