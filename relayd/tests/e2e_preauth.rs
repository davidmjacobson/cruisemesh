//! What an *unauthenticated* request may cost the relay.
//!
//! Every per-family limit (`e2e_limits.rs`) is charged only after a bearer
//! token authorizes, on purpose: keying buckets on unverified tokens would let
//! a caller grow the bucket map without bound. That left the pre-auth path
//! uncharged, and it was the expensive one — the JSON body was buffered and
//! parsed (up to axum's 2 MiB default) before the bearer was even looked at,
//! the bearer then cost a SQLite lookup, and every reject wrote a log line.
//!
//! These tests pin the three bounds that close it:
//!
//! 1. A body is never read until its bearer has authorized.
//! 2. Every body-bearing route has a ceiling sized to its real payload.
//! 3. Credential rejects are charged to a per-client-IP bucket that, once
//!    spent, refuses further requests from that IP before any lookup.

use std::collections::HashSet;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use axum::body::{to_bytes, Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cruisemesh_relayd::{
    app, AppState, RateLimitConfig, RelayStore, DEFAULT_AUTH_REJECTS_PER_IP_PER_MIN, MAX_ACK_IDS,
    MAX_CONTROL_BODY_BYTES, MAX_ENVELOPE_BODY_BYTES, MAX_ENVELOPE_SEALED_BYTES,
};
use tower::util::ServiceExt;

const ADMIN_TOKEN: &str = "admin-token";
const FAMILY: &str = "family-a";

fn test_app(rate_limits: RateLimitConfig) -> Router {
    // In-memory DB: the router owns the store's single connection.
    let store = RelayStore::open(":memory:").unwrap();
    let auth: HashSet<String> = HashSet::from([FAMILY.to_string()]);
    app(AppState::with_rate_limits(store, auth, rate_limits)
        .with_admin_token(Some(ADMIN_TOKEN.to_string())))
}

async fn body_json(response: Response) -> serde_json::Value {
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// A body that records whether anything ever polled it. Large on purpose:
/// if the handler did read it, it would be a full 3 MiB of buffering.
fn tripwire_body(polled: Arc<AtomicBool>) -> Body {
    let stream = futures_util::stream::once(async move {
        polled.store(true, Ordering::SeqCst);
        Ok::<_, Infallible>(Bytes::from(vec![b' '; 3 * 1024 * 1024]))
    });
    Body::from_stream(stream)
}

fn request(method: &str, uri: &str, bearer: &str, body: Body) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {bearer}"))
        .header("content-type", "application/json")
        .body(body)
        .unwrap()
}

/// As if the request arrived on a real socket from `peer` — what
/// `into_make_service_with_connect_info` hands the router in production.
fn from_peer(mut request: Request<Body>, peer: &str) -> Request<Body> {
    let addr: SocketAddr = peer.parse().unwrap();
    request.extensions_mut().insert(ConnectInfo(addr));
    request
}

fn fetch(bearer: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(format!(
            "/envelopes?hints={}",
            URL_SAFE_NO_PAD.encode([1u8; 8])
        ))
        .header("authorization", format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap()
}

// ---------------------------------------------------------------------------
// 1. The bearer is checked before the body is read.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_unknown_bearer_is_refused_without_reading_the_body_on_every_body_route() {
    let app = test_app(RateLimitConfig::default());
    let routes = [
        ("POST", "/envelopes"),
        ("POST", "/envelopes/ack"),
        ("POST", "/presence"),
        ("PUT", "/push/registrations"),
        ("POST", "/family/rotate"),
        ("POST", "/admin/families"),
        ("PATCH", "/admin/families/whoever"),
    ];
    for (method, uri) in routes {
        let polled = Arc::new(AtomicBool::new(false));
        let response = app
            .clone()
            .oneshot(request(
                method,
                uri,
                "not-a-real-token",
                tripwire_body(polled.clone()),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{method} {uri} must refuse an unknown bearer"
        );
        assert!(
            !polled.load(Ordering::SeqCst),
            "{method} {uri} read the body of an unauthenticated request"
        );
    }
}

#[tokio::test]
async fn a_missing_bearer_is_refused_without_reading_the_body() {
    let app = test_app(RateLimitConfig::default());
    let polled = Arc::new(AtomicBool::new(false));
    let request = Request::builder()
        .method("POST")
        .uri("/envelopes/ack")
        .header("content-type", "application/json")
        .body(tripwire_body(polled.clone()))
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert!(!polled.load(Ordering::SeqCst));
}

#[tokio::test]
async fn an_authorized_caller_still_gets_json_errors_for_a_malformed_body() {
    // Moving the parse after authorization must not change what an
    // authorized client sees for a broken body: still a 4xx, never a 500.
    let app = test_app(RateLimitConfig::default());
    let response = app
        .oneshot(request(
            "POST",
            "/envelopes/ack",
            FAMILY,
            Body::from("{not json"),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json = body_json(response).await;
    assert!(json["error"].is_string(), "{json}");
}

// ---------------------------------------------------------------------------
// 2. Per-route body ceilings, sized to the real payload.
// ---------------------------------------------------------------------------

#[test]
fn body_ceilings_are_pinned() {
    // Golden values: a change here is a wire-visible change (a body that used
    // to be accepted becomes a 413), so it must be a deliberate one.
    assert_eq!(MAX_ENVELOPE_BODY_BYTES, 1024 * 1024);
    assert_eq!(MAX_CONTROL_BODY_BYTES, 64 * 1024);
    assert_eq!(DEFAULT_AUTH_REJECTS_PER_IP_PER_MIN, 60);
}

#[test]
fn the_largest_legitimate_bodies_fit_their_ceilings_with_room_for_the_coded_rejects() {
    // An envelope one byte over the sealed cap must still reach the handler,
    // so the client gets the coded `envelope_too_large` 413 rather than the
    // bare body-limit one.
    let envelope = serde_json::json!({
        "msg_id": URL_SAFE_NO_PAD.encode([0u8; 16]),
        "hop_ttl": 255,
        "recipient_hint": URL_SAFE_NO_PAD.encode([0u8; 8]),
        "sealed": URL_SAFE_NO_PAD.encode(vec![0u8; MAX_ENVELOPE_SEALED_BYTES + 1]),
        "expiry_ms": i64::MIN,
    })
    .to_string();
    assert!(
        envelope.len() <= MAX_ENVELOPE_BODY_BYTES,
        "{}",
        envelope.len()
    );

    // One id past the ack cap, each the widest an i64 prints, must still
    // reach the handler's "at most N ids" 400.
    let ack = serde_json::json!({ "ids": vec![i64::MIN; MAX_ACK_IDS + 1] }).to_string();
    assert!(ack.len() <= MAX_CONTROL_BODY_BYTES, "{}", ack.len());
}

#[tokio::test]
async fn an_envelope_body_over_its_ceiling_is_refused_with_413() {
    let app = test_app(RateLimitConfig::default());
    let body = vec![b' '; MAX_ENVELOPE_BODY_BYTES + 1];
    let response = app
        .oneshot(request("POST", "/envelopes", FAMILY, Body::from(body)))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn control_bodies_over_their_ceiling_are_refused_with_413() {
    let app = test_app(RateLimitConfig::default());
    // Valid JSON, so the only thing that can refuse it is the size ceiling:
    // under the old 2 MiB default this parsed and came back a 400 for having
    // too many ids.
    let ids: Vec<i64> = (0..(MAX_CONTROL_BODY_BYTES as i64 / 4)).collect();
    let ack = serde_json::json!({ "ids": ids }).to_string();
    assert!(ack.len() > MAX_CONTROL_BODY_BYTES);
    for (method, uri, bearer) in [
        ("POST", "/envelopes/ack", FAMILY),
        ("POST", "/presence", FAMILY),
        ("PUT", "/push/registrations", FAMILY),
        ("POST", "/family/rotate", FAMILY),
        ("POST", "/admin/families", ADMIN_TOKEN),
    ] {
        let response = app
            .clone()
            .oneshot(request(method, uri, bearer, Body::from(ack.clone())))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::PAYLOAD_TOO_LARGE,
            "{method} {uri}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Per-client-IP bucket for credential rejects.
// ---------------------------------------------------------------------------

fn gate_limits(allowance: u32) -> RateLimitConfig {
    RateLimitConfig {
        auth_rejects_per_ip_per_min: allowance,
        ..RateLimitConfig::default()
    }
}

#[tokio::test]
async fn an_ip_that_spends_its_reject_allowance_is_refused_before_lookup() {
    let app = test_app(gate_limits(3));
    for i in 0..3 {
        let response = app
            .clone()
            .oneshot(from_peer(fetch(&format!("guess-{i}")), "198.51.100.7:4000"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "guess {i}");
    }

    // The fourth guess from the same address is refused without a lookup,
    // and told how long to wait.
    let polled = Arc::new(AtomicBool::new(false));
    let response = app
        .clone()
        .oneshot(from_peer(
            request(
                "POST",
                "/envelopes/ack",
                "guess-3",
                tripwire_body(polled.clone()),
            ),
            "198.51.100.7:4001",
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(!polled.load(Ordering::SeqCst));
    let retry_after: u64 = response
        .headers()
        .get("retry-after")
        .expect("a 429 carries Retry-After")
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&retry_after), "{retry_after}");
    assert_eq!(body_json(response).await["code"], "rate_limited");

    // Someone else is unaffected.
    let response = app
        .clone()
        .oneshot(from_peer(fetch("guess-4"), "203.0.113.9:4000"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn authorized_requests_never_spend_the_reject_allowance() {
    let app = test_app(gate_limits(2));
    for i in 0..10 {
        let response = app
            .clone()
            .oneshot(from_peer(fetch(FAMILY), "198.51.100.7:4000"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "request {i}");
    }
    // The allowance is still whole.
    for i in 0..2 {
        let response = app
            .clone()
            .oneshot(from_peer(fetch("wrong"), "198.51.100.7:4000"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "reject {i}");
    }
}

#[tokio::test]
async fn a_static_credential_is_served_even_from_a_spent_address() {
    // Recognising a static allowlist token costs a hash lookup, not a store
    // read, so a self-hosted family sharing an address with a guesser (a
    // ship's NAT) is never collateral.
    let app = test_app(gate_limits(1));
    let peer = "198.51.100.7:4000";
    let response = app
        .clone()
        .oneshot(from_peer(fetch("wrong"), peer))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(from_peer(fetch("wrong"), peer))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let response = app
        .clone()
        .oneshot(from_peer(fetch(FAMILY), peer))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn ipv6_clients_are_keyed_by_their_slash_64() {
    // One host owns a whole /64; rotating the low bits must not buy fresh
    // allowance.
    let app = test_app(gate_limits(2));
    for (i, peer) in ["[2001:db8:1:2::1]:4000", "[2001:db8:1:2::ffff]:4000"]
        .into_iter()
        .enumerate()
    {
        let response = app
            .clone()
            .oneshot(from_peer(fetch("wrong"), peer))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "reject {i}");
    }
    let response = app
        .clone()
        .oneshot(from_peer(fetch("wrong"), "[2001:db8:1:2:aaaa::1]:4000"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

fn forwarded(request: Request<Body>, peer: &str, forwarded_for: &str) -> Request<Body> {
    let mut request = from_peer(request, peer);
    request
        .headers_mut()
        .insert("x-forwarded-for", forwarded_for.parse().unwrap());
    request
}

#[tokio::test]
async fn behind_the_compose_proxy_clients_are_keyed_by_the_forwarded_address() {
    // relayd is reachable only by Caddy on the compose network, so every
    // socket peer is Caddy's private address. Keying on it would put every
    // client on the internet in one bucket.
    let app = test_app(gate_limits(1));
    let caddy = "172.18.0.3:51000";
    let response = app
        .clone()
        .oneshot(forwarded(fetch("wrong"), caddy, "198.51.100.7"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(forwarded(fetch("wrong"), caddy, "198.51.100.7"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    // A different client behind the same proxy has its own bucket.
    let response = app
        .clone()
        .oneshot(forwarded(fetch("wrong"), caddy, "203.0.113.9"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_public_peer_cannot_dodge_its_bucket_with_a_forged_forwarded_header() {
    let app = test_app(gate_limits(1));
    let peer = "198.51.100.7:4000";
    let response = app
        .clone()
        .oneshot(forwarded(fetch("wrong"), peer, "203.0.113.1"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .clone()
        .oneshot(forwarded(fetch("wrong"), peer, "203.0.113.2"))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::TOO_MANY_REQUESTS,
        "X-Forwarded-For from a peer that is not the local proxy must be ignored"
    );
}

#[tokio::test]
async fn a_real_socket_server_keys_the_gate_on_the_peer_address() {
    // The in-process tests above insert `ConnectInfo` by hand; this one serves
    // the router the way `main.rs` does, so a mismatch between what the server
    // provides and what the gate reads would leave the gate silently open.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = test_app(gate_limits(1));
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let client = reqwest::Client::new();
    let url = format!(
        "http://{addr}/envelopes?hints={}",
        URL_SAFE_NO_PAD.encode([1u8; 8])
    );
    let first = client.get(&url).bearer_auth("wrong").send().await.unwrap();
    assert_eq!(first.status().as_u16(), 401);
    let second = client.get(&url).bearer_auth("wrong").send().await.unwrap();
    assert_eq!(second.status().as_u16(), 429);
    server.abort();
}
