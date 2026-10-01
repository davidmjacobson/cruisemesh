//! `GET /family/status`: what a family may learn about its own Shore Pass,
//! who may ask, and — the point of the route — that the answer survives the
//! two billing states every other authenticated route refuses outright.
//!
//! The shells read this to say when internet delivery runs out and to decide
//! whether to offer a renewal, so a suspended or lapsed family is exactly the
//! one that must still get an answer. Companion to `e2e_presence.rs`, which
//! owns the other `families`-table-aware route and the deposit class boundary
//! these tests re-assert for this one.

use std::collections::HashSet;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use cruisemesh_relayd::{
    app, deposit_token_for, AppState, RateLimitConfig, RelayStore, FAMILY_EXPIRY_GRACE_MS,
};
use tempfile::NamedTempFile;
use tower::util::ServiceExt;

const ADMIN_TOKEN: &str = "admin-token";

async fn body_json(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn status(token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/family/status")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
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

fn test_app(tokens: &[&str]) -> (NamedTempFile, Router) {
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let auth: HashSet<String> = tokens.iter().map(|t| (*t).to_string()).collect();
    (
        db,
        app(
            AppState::with_rate_limits(store, auth, RateLimitConfig::default())
                .with_admin_token(Some(ADMIN_TOKEN.to_string())),
        ),
    )
}

/// Provision a hosted family through the admin API — the same call the
/// purchase flow makes — and assert it took.
async fn provision(router: &Router, body: serde_json::Value) {
    let response = router
        .clone()
        .oneshot(admin_json("POST", "/admin/families", body))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn patch(router: &Router, token: &str, body: serde_json::Value) {
    let response = router
        .clone()
        .oneshot(admin_json(
            "PATCH",
            &format!("/admin/families/{token}"),
            body,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis() as i64
}

/// Read the status route and assert the whole pinned shape at once, so a
/// field quietly changing name or type fails here rather than in a shell.
async fn expect_status(
    router: &Router,
    token: &str,
    plan: serde_json::Value,
    expires_ms: serde_json::Value,
    state: &str,
) -> serde_json::Value {
    let response = router.clone().oneshot(status(token)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let json = body_json(response).await;
    assert_eq!(json["plan"], plan);
    assert_eq!(json["expires_ms"], expires_ms);
    assert_eq!(json["state"], state);
    json
}

// ---------------------------------------------------------------------------
// The ordinary answer, and who is allowed to ask for it
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_member_token_reads_its_own_plan_and_expiry() {
    let (_db, router) = test_app(&[]);
    let expires = now_ms() + 30 * 24 * 60 * 60 * 1000;
    provision(
        &router,
        serde_json::json!({
            "token": "hosted-family",
            "plan": "shore-pass",
            "expires_ms": expires,
        }),
    )
    .await;

    expect_status(
        &router,
        "hosted-family",
        serde_json::json!("shore-pass"),
        serde_json::json!(expires),
        "active",
    )
    .await;
}

#[tokio::test]
async fn a_deposit_token_may_not_read_the_family_expiry() {
    // A friend card carries the deposit token. When someone else's pass runs
    // out is not a friend's business, so this is refused by the same class
    // boundary — and with the same structured code — as fetch and ack.
    let (_db, router) = test_app(&[]);
    provision(
        &router,
        serde_json::json!({ "token": "hosted-family", "plan": "shore-pass" }),
    )
    .await;

    let refused = router
        .clone()
        .oneshot(status(&deposit_token_for("hosted-family")))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(refused).await["code"], "deposit_only");
}

// ---------------------------------------------------------------------------
// The states the route exists for: the ones every other route refuses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_suspended_family_is_still_told_that_it_is_suspended() {
    let (_db, router) = test_app(&[]);
    let expires = now_ms() + 30 * 24 * 60 * 60 * 1000;
    provision(
        &router,
        serde_json::json!({
            "token": "hosted-family",
            "plan": "shore-pass",
            "expires_ms": expires,
        }),
    )
    .await;
    patch(
        &router,
        "hosted-family",
        serde_json::json!({ "status": "suspended" }),
    )
    .await;

    // Every other member-token route is a 403 in this state...
    let refused = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/presence")
                .header("authorization", "Bearer hosted-family")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({ "announce": [], "query": [] }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert_eq!(body_json(refused).await["code"], "family_suspended");

    // ...and this one still answers, because a shell with nothing to show
    // cannot offer the renewal that fixes it. The unexpired `expires_ms` is
    // reported unchanged: the pass did not lapse, it was suspended.
    expect_status(
        &router,
        "hosted-family",
        serde_json::json!("shore-pass"),
        serde_json::json!(expires),
        "suspended",
    )
    .await;
}

#[tokio::test]
async fn expiry_reads_grace_inside_the_window_and_suspended_past_it() {
    let (_db, router) = test_app(&[]);
    let now = now_ms();

    // Expired a second ago: the mailbox still drains, new envelopes are
    // refused — `grace` is the state that names exactly that.
    provision(
        &router,
        serde_json::json!({
            "token": "hosted-family",
            "plan": "shore-pass",
            "expires_ms": now - 1_000,
        }),
    )
    .await;
    expect_status(
        &router,
        "hosted-family",
        serde_json::json!("shore-pass"),
        serde_json::json!(now - 1_000),
        "grace",
    )
    .await;

    // Past the grace window the relay does nothing for this family until it
    // renews, which is the same fact `suspended` reports — the contract has
    // no fourth value, and a shell that wants to tell the two apart has
    // `expires_ms` to do it with.
    let past_grace = now - FAMILY_EXPIRY_GRACE_MS - 10_000;
    patch(
        &router,
        "hosted-family",
        serde_json::json!({ "expires_ms": past_grace }),
    )
    .await;
    expect_status(
        &router,
        "hosted-family",
        serde_json::json!("shore-pass"),
        serde_json::json!(past_grace),
        "suspended",
    )
    .await;
}

// ---------------------------------------------------------------------------
// Families with no expiry at all
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_family_that_never_expires_reports_a_null_expiry() {
    // Two ways to have no expiry, and the shells must render both as "say
    // nothing about delivery running out": a provisioned family whose
    // `expires_ms` was left unset, and a static env-allowlist family, which
    // has no `families` row at all because no pass was ever sold for it.
    let (_db, router) = test_app(&["self-hosted"]);
    provision(
        &router,
        serde_json::json!({ "token": "hosted-family", "plan": "shore-pass" }),
    )
    .await;

    expect_status(
        &router,
        "hosted-family",
        serde_json::json!("shore-pass"),
        serde_json::Value::Null,
        "active",
    )
    .await;
    expect_status(
        &router,
        "self-hosted",
        serde_json::Value::Null,
        serde_json::Value::Null,
        "active",
    )
    .await;
}

// ---------------------------------------------------------------------------
// It costs the family a request, like every other authenticated read
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_status_poll_spends_the_familys_request_allowance() {
    // One request per minute, so an uncharged poll would show up immediately.
    // Charging it is deliberate: this is a per-phone poll, and an
    // authenticated route that cost nothing would be the one a stuck client
    // could spin on forever.
    let db = NamedTempFile::new().unwrap();
    let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
    let router = app(AppState::with_rate_limits(
        store,
        HashSet::from(["family-a".to_string()]),
        RateLimitConfig {
            requests_per_min: 1,
            ..RateLimitConfig::default()
        },
    )
    .with_admin_token(Some(ADMIN_TOKEN.to_string())));

    assert_eq!(
        router
            .clone()
            .oneshot(status("family-a"))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        router
            .clone()
            .oneshot(status("family-a"))
            .await
            .unwrap()
            .status(),
        StatusCode::TOO_MANY_REQUESTS
    );
}
