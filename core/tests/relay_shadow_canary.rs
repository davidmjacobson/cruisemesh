//! The migration canary, checked from the core side.
//!
//! Two things are proved here that neither the shadow module's own unit tests
//! nor an adapter suite can prove alone.
//!
//! **The adapter vector table cannot drift from the pass.** A table of
//! expected requests is only worth having if it is the same bytes a running
//! pass emits. So the `post-envelope` vector is asserted against a real
//! [`CoreRelayPass`] driven over a real store with a row carrying exactly the
//! vector's fields. If someone changes a path, a header or an encoding and
//! updates only one of the two, this goes red.
//!
//! **The canary's one store call writes only diagnostics.** The shadow's
//! whole safety argument is that it cannot be a second writer, so the write
//! it *is* allowed to make is checked for what it touches, for what it says,
//! and for what it does not leak.

use std::sync::Arc;

use cruisemesh_core::{
    compute_recipient_hint, core_relay_adapter_vectors, core_relay_pass_default_budgets,
    core_relay_shadow_compare, CoreRelayActionKind, CoreRelayContactConfig,
    CoreRelayEndpointConfig, CoreRelayHttpRequest, CoreRelayPass, CoreRelayPassPlan,
    CoreRelayShadowCapture, CoreRelayShadowLane, CoreRelayShadowMismatchKind, CoreRelayShadowStep,
    MessageStore, OutboundEnvelope, StoredMessage, KIND_TEXT,
};

const OWN_URL: &str = "https://relay.example";
const OWN_TOKEN: &str = "member-token";
const T0: i64 = 1_700_000_000_000;

fn store() -> Arc<MessageStore> {
    Arc::new(MessageStore::open(":memory:".to_string()).expect("in-memory store"))
}

fn own_user_id() -> Vec<u8> {
    (0u8..32).collect()
}

fn contact_user_id() -> Vec<u8> {
    vec![9u8; 32]
}

fn vector(name: &str) -> CoreRelayHttpRequest {
    core_relay_adapter_vectors()
        .into_iter()
        .find(|candidate| candidate.name == name)
        .unwrap_or_else(|| panic!("no adapter vector named {name}"))
        .request
}

// ---------------------------------------------------------------------------
// The table is the pass
// ---------------------------------------------------------------------------

#[test]
fn the_post_vector_is_the_request_a_live_pass_emits() {
    let expected = vector("post-envelope");
    let store = store();

    // The vector's own field values, so the two requests can only differ if
    // the *construction* differs.
    store
        .insert_outgoing_message(
            StoredMessage {
                chat_id: vec![3u8; 32],
                sender_user_id: own_user_id(),
                lamport: 1,
                timestamp: T0,
                kind: KIND_TEXT,
                payload: b"hello".to_vec(),
            },
            OutboundEnvelope {
                msg_id: vec![0x11; 16],
                recipient_user_id: contact_user_id(),
                chat_id: vec![3u8; 32],
                sender_user_id: own_user_id(),
                kind: KIND_TEXT,
                lamport: 1,
                timestamp: T0,
                hop_ttl: 4,
                expiry: 1_700_000_000_000,
                recipient_hint: vec![0x22; 8],
                sealed: vec![0x33; 48],
            },
            T0,
        )
        .expect("queue an authored row");

    let plan = CoreRelayPassPlan {
        own: Some(CoreRelayEndpointConfig {
            url: OWN_URL.to_string(),
            token: OWN_TOKEN.to_string(),
        }),
        contacts: Vec::new(),
        own_user_id: own_user_id(),
        fetch_hints: Vec::new(),
        presence_announce: Vec::new(),
        presence_query: Vec::new(),
        own_endpoint_changed: false,
        swept_this_session: true,
        consecutive_rate_limits: 0,
        quiet_until_ms: 0,
        budgets: core_relay_pass_default_budgets(),
    };
    let pass = CoreRelayPass::new(store.clone(), plan, "v1".to_string());
    // `expiry` is in the past relative to nothing here: the row is queued at
    // T0 with expiry T0, and the pass runs a moment before it, so the prune
    // stage leaves it alone.
    let action = pass.start(T0 - 1);

    let CoreRelayActionKind::Http { request } = action.kind else {
        panic!("the first action of a pass with one authored row is its post");
    };
    assert_eq!(
        request, expected,
        "the post-envelope vector and the request a live pass emits must be one thing"
    );
}

#[test]
fn every_vector_names_a_distinct_operation_shape() {
    let vectors = core_relay_adapter_vectors();
    assert_eq!(
        vectors.len(),
        4,
        "the table must cover post, fetch, ack and presence"
    );
    for vector in &vectors {
        assert!(
            vector.request.base_url.starts_with("https://"),
            "{} must be https",
            vector.name
        );
        assert!(
            vector
                .request
                .headers
                .iter()
                .any(|header| header.name == "Authorization"),
            "{} must authenticate",
            vector.name
        );
        assert!(
            vector
                .request
                .response_headers_wanted
                .contains(&"Retry-After".to_string()),
            "{} must ask for the header RATE-01 is measured from",
            vector.name
        );
        assert!(
            vector.request.max_response_bytes > 0,
            "{} must declare a response cap the driver can enforce",
            vector.name
        );
    }
    assert_eq!(vector("fetch-page").method, "GET");
    assert!(vector("fetch-page").body.is_empty());
    assert_eq!(vector("ack-page").path, "/envelopes/ack");
    assert_eq!(vector("presence").path, "/presence");
}

// ---------------------------------------------------------------------------
// The canary's one write
// ---------------------------------------------------------------------------

fn one_step(status: u16, marked: bool) -> CoreRelayShadowStep {
    CoreRelayShadowStep {
        lane: CoreRelayShadowLane::Authored,
        msg_id: vec![0x11; 16],
        hop_ttl: 4,
        recipient_hint: vec![0x22; 8],
        recipient_user_id: contact_user_id(),
        sealed: vec![0x33; 48],
        expiry_ms: T0,
        legacy_endpoint: Some(CoreRelayEndpointConfig {
            url: OWN_URL.to_string(),
            token: OWN_TOKEN.to_string(),
        }),
        status,
        relay_code: None,
        transport_error: None,
        legacy_marked_posted: marked,
        legacy_continued_lane: true,
    }
}

fn capture(steps: Vec<CoreRelayShadowStep>) -> CoreRelayShadowCapture {
    CoreRelayShadowCapture {
        own: Some(CoreRelayEndpointConfig {
            url: OWN_URL.to_string(),
            token: OWN_TOKEN.to_string(),
        }),
        contacts: vec![CoreRelayContactConfig {
            user_id: contact_user_id(),
            relay_url: None,
            relay_token: None,
            endpoint_usable: true,
        }],
        steps,
        skipped_recipients: Vec::new(),
        rows_unshadowed: 0,
    }
}

#[test]
fn an_agreeing_sample_still_says_it_ran() {
    let store = store();
    let report = core_relay_shadow_compare(capture(vec![one_step(200, true)]));
    assert!(report.mismatches.is_empty());
    store.note_relay_shadow_report(report, T0);

    let archive = store
        .export_protocol_events_jsonl()
        .expect("export the ring");
    assert!(
        archive.contains("\"outcome\":\"shadow_agreed\""),
        "a clean sample must leave evidence that it ran:\n{archive}"
    );
    assert!(
        archive.contains("\"steps_compared\":1"),
        "the summary must carry what it compared:\n{archive}"
    );
}

#[test]
fn each_disagreement_gets_its_own_record_and_no_secret_rides_along() {
    let store = store();
    // A row the relay refused that the legacy engine retired anyway: the
    // shape that loses mail.
    let report = core_relay_shadow_compare(capture(vec![one_step(500, true)]));
    let kinds: Vec<_> = report.mismatches.iter().map(|m| m.kind).collect();
    assert!(
        kinds.contains(&CoreRelayShadowMismatchKind::SuccessMarkingDiffers),
        "a retired row the relay refused must be reported, got {kinds:?}"
    );
    store.note_relay_shadow_report(report, T0);

    let archive = store
        .export_protocol_events_jsonl()
        .expect("export the ring");
    assert!(archive.contains("\"outcome\":\"shadow_diverged\""));
    assert!(archive.contains("\"outcome\":\"shadow_success_marking_differs\""));
    // SECRET-01, against the live ring rather than against a claim: the
    // capture that produced this carried a bearer token and a sealed body.
    assert!(
        !archive.contains(OWN_TOKEN),
        "the ring must not carry the credential the capture held"
    );
    assert!(
        !archive.contains(OWN_URL),
        "the ring must not carry the endpoint the capture held"
    );
    assert!(
        cruisemesh_core::redaction_defect(&archive).is_none(),
        "the ring tripped a redaction canary:\n{archive}"
    );
}

#[test]
fn the_canary_write_touches_nothing_operational() {
    let store = store();
    store
        .insert_outgoing_message(
            StoredMessage {
                chat_id: vec![3u8; 32],
                sender_user_id: own_user_id(),
                lamport: 1,
                timestamp: T0,
                kind: KIND_TEXT,
                payload: b"hello".to_vec(),
            },
            OutboundEnvelope {
                msg_id: vec![0x11; 16],
                recipient_user_id: contact_user_id(),
                chat_id: vec![3u8; 32],
                sender_user_id: own_user_id(),
                kind: KIND_TEXT,
                lamport: 1,
                timestamp: T0,
                hop_ttl: 4,
                expiry: T0 + 86_400_000,
                recipient_hint: compute_recipient_hint(contact_user_id(), T0),
                sealed: vec![0x33; 48],
            },
            T0,
        )
        .expect("queue an authored row");

    let before = store
        .pending_relay_outbound_envelopes(64, T0, Vec::new())
        .expect("read the queue");
    assert_eq!(before.len(), 1);

    // Every mismatch kind at once, so no branch of the emit path is untested
    // against the queue.
    let mut steps = vec![one_step(500, true), one_step(507, false)];
    steps[1].relay_code = Some("mailbox_full".to_string());
    steps.push(CoreRelayShadowStep {
        lane: CoreRelayShadowLane::Receipt,
        ..one_step(200, true)
    });
    let mut taken = capture(steps);
    taken.skipped_recipients = vec![contact_user_id()];
    taken.rows_unshadowed = 3;
    let report = core_relay_shadow_compare(taken);
    assert!(report.mismatches.len() >= 3);
    store.note_relay_shadow_report(report, T0);

    let after = store
        .pending_relay_outbound_envelopes(64, T0, Vec::new())
        .expect("read the queue");
    assert_eq!(
        after.len(),
        before.len(),
        "the canary retired a row; it may only write diagnostics"
    );
    assert_eq!(after[0].msg_id, before[0].msg_id);
}
