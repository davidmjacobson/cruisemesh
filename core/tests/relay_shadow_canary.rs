//! The migration canary, checked from the core side.
//!
//! Two things are proved here that neither the shadow module's own unit tests
//! nor an adapter suite can prove alone.
//!
//! **The adapter vector table cannot drift from the pass.** A table of
//! expected requests is only worth having if it is the same bytes a running
//! pass emits. So *every* vector is asserted against a real [`CoreRelayPass`]
//! driven over a real store, arranged so the pass forms exactly the request
//! the table names: the post from a queued row carrying the vector's fields,
//! and the presence, ack and fetch from a walk seeded to ask, consume and
//! resume at the vector's cursor. If someone changes a path, a header, a
//! wanted response header or an encoding and updates only one of the two,
//! this goes red — which is the only reason an adapter suite asserting the
//! table proves anything about what core actually sends.
//!
//! **The canary's one store call writes only diagnostics.** The shadow's
//! whole safety argument is that it cannot be a second writer, so the write
//! it *is* allowed to make is checked for what it touches, for what it says,
//! and for what it does not leak.

use std::sync::Arc;

use cruisemesh_core::{
    compute_recipient_hint, core_relay_adapter_vectors, core_relay_pass_default_budgets,
    core_relay_shadow_compare, CoreRelayAction, CoreRelayActionKind, CoreRelayContactConfig,
    CoreRelayEndpointConfig, CoreRelayHttpRequest, CoreRelayHttpResult, CoreRelayPass,
    CoreRelayPassPlan, CoreRelayShadowCapture, CoreRelayShadowLane, CoreRelayShadowMismatchKind,
    CoreRelayShadowStep, MessageStore, OutboundEnvelope, StoredMessage, KIND_RECEIPT, KIND_TEXT,
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

/// The hints the `fetch-page` and `presence` vectors are built from.
fn hint_a() -> Vec<u8> {
    vec![0x22; 8]
}

fn hint_b() -> Vec<u8> {
    vec![0x44; 8]
}

/// The relay row id the `fetch-page` vector resumes from, which is the last
/// of the ids the `ack-page` vector acks: a walk resumes from the end of the
/// page it just retired, so the two vectors describe consecutive moments of
/// one mailbox rather than two unrelated numbers.
const VECTOR_CURSOR: i64 = 8;

fn b64(bytes: &[u8]) -> String {
    data_encoding::BASE64URL_NOPAD.encode(bytes)
}

/// One relay page carrying rows this device has already durably consumed, so
/// the ingest finds them ack-eligible and the pass forms an ack for exactly
/// these ids. `next_cursor` is the relay's own, which is what the following
/// pass resumes from.
fn consumed_page(
    store: &MessageStore,
    ids: &[i64],
    hint: &[u8],
    now_ms: i64,
    next_cursor: i64,
) -> Vec<u8> {
    let hint = hint.to_vec();
    let expiry = now_ms + 6 * 24 * 60 * 60 * 1_000;
    let rows: Vec<String> = ids
        .iter()
        .map(|id| {
            let mut msg_id = vec![0u8; 16];
            msg_id[..8].copy_from_slice(&(*id as u64).to_be_bytes());
            msg_id[8] = 0xA5;
            let mut sealed = vec![0x11u8; 96];
            sealed[..8].copy_from_slice(&(*id as u64).to_be_bytes());
            assert!(
                store
                    .core_record_consumed_hidden_msg_id(
                        msg_id.clone(),
                        KIND_RECEIPT,
                        hint.clone(),
                        expiry,
                        own_user_id(),
                        now_ms,
                    )
                    .expect("record consumed-hidden"),
                "the consumed set must vouch for the seeded row"
            );
            format!(
                "{{\"id\":{},\"msg_id\":\"{}\",\"hop_ttl\":3,\"recipient_hint\":\"{}\",\
                 \"sealed\":\"{}\",\"expiry_ms\":{}}}",
                id,
                b64(&msg_id),
                b64(&hint),
                b64(&sealed),
                expiry
            )
        })
        .collect();
    format!(
        "{{\"envelopes\":[{}],\"next_cursor\":{}}}",
        rows.join(","),
        next_cursor
    )
    .into_bytes()
}

fn ok(action: &CoreRelayAction, body: Vec<u8>, now_ms: i64) -> CoreRelayHttpResult {
    CoreRelayHttpResult {
        pass_id: action.pass_id.clone(),
        action_id: action.action_id,
        status: 200,
        headers: Vec::new(),
        body,
        error: None,
        completed_at_ms: now_ms,
    }
}

fn http(action: &CoreRelayAction) -> CoreRelayHttpRequest {
    match &action.kind {
        CoreRelayActionKind::Http { request } => request.clone(),
        other => panic!("expected an HTTP action, got {other:?}"),
    }
}

/// Every vector except the post, pinned against the requests a live walk
/// emits.
///
/// Arranged so the pass has no choice but to form exactly the table's
/// requests. The first pass announces the vector's presence hints and
/// consumes a page whose rows carry the vector's ack ids, so its presence and
/// ack requests are the table's. Its relay answers with `next_cursor` at the
/// row id the `fetch-page` vector resumes from, and the ack succeeds, so the
/// frontier moves there — which makes the *second* pass's first fetch, taken
/// under the vector's own hints, that vector byte for byte.
///
/// The two passes are needed because a row is only ack-eligible when its hint
/// is one of this device's own, and the fetch vector's hints are fixed bytes
/// no device would ever derive. The mailbox cursor is keyed by endpoint
/// rather than by hint, so the frontier the first pass earns is the one the
/// second resumes from.
#[test]
fn the_walk_vectors_are_the_requests_a_live_pass_emits() {
    let store = store();
    let own_hint = compute_recipient_hint(own_user_id(), T0);
    let plan = CoreRelayPassPlan {
        own: Some(CoreRelayEndpointConfig {
            url: OWN_URL.to_string(),
            token: OWN_TOKEN.to_string(),
        }),
        contacts: Vec::new(),
        own_user_id: own_user_id(),
        fetch_hints: vec![own_hint.clone()],
        presence_announce: vec![hint_a()],
        presence_query: vec![hint_b()],
        own_endpoint_changed: false,
        swept_this_session: true,
        consecutive_rate_limits: 0,
        quiet_until_ms: 0,
        budgets: core_relay_pass_default_budgets(),
    };

    let pass = CoreRelayPass::new(store.clone(), plan.clone(), "walk".to_string());
    let action = pass.start(T0);

    assert_eq!(
        http(&action),
        vector("presence"),
        "the presence vector and the request a live pass emits must be one thing"
    );

    let action = pass.resume_http(ok(
        &action,
        br#"{"now_ms":1700000000000,"presence":[]}"#.to_vec(),
        T0,
    ));
    let fetch = http(&action);
    assert_eq!(fetch.method, "GET");
    assert!(
        fetch.path.starts_with("/envelopes?hints="),
        "a walk's first request is a fetch, got {}",
        fetch.path
    );

    // A page of rows this device already consumed: the ingest finds them
    // ack-eligible, so the next action is the ack the table names.
    let page = consumed_page(&store, &[3, 5, 8], &own_hint, T0, VECTOR_CURSOR);
    let action = pass.resume_http(ok(&action, page, T0));
    assert_eq!(
        http(&action),
        vector("ack-page"),
        "the ack vector and the request a live pass emits must be one thing"
    );

    // The ack succeeds, so the frontier moves to the relay's cursor.
    let mut action = pass.resume_http(ok(&action, b"{}".to_vec(), T0));
    while let CoreRelayActionKind::Http { .. } = action.kind {
        action = pass.resume_http(ok(
            &action,
            br#"{"envelopes":[],"next_cursor":0}"#.to_vec(),
            T0,
        ));
    }

    let resumed = CoreRelayPass::new(
        store,
        CoreRelayPassPlan {
            fetch_hints: vec![hint_a(), hint_b()],
            // Nothing to announce, so the walk is the first thing this pass does.
            presence_announce: Vec::new(),
            presence_query: Vec::new(),
            ..plan
        },
        "resumed".to_string(),
    );
    let action = resumed.start(T0 + 1);
    assert_eq!(
        http(&action),
        vector("fetch-page"),
        "the fetch vector and the request a live pass emits must be one thing"
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
        sealed_len: 48,
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
            endpoint_answering: true,
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
    // capture that produced this carried a bearer token.
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

#[test]
fn a_device_that_diverges_on_every_row_still_costs_the_ring_a_handful_of_records() {
    let store = store();
    // The worst realistic sample: every captured row wrong on every axis, and
    // a long skip list on top. A per-row emitter would write hundreds of
    // records here and evict most of the archive with them.
    let mut steps: Vec<CoreRelayShadowStep> = Vec::new();
    for index in 0..2_000 {
        let mut row = one_step(507, true);
        row.lane = if index == 0 {
            CoreRelayShadowLane::Authored
        } else {
            CoreRelayShadowLane::Receipt
        };
        row.relay_code = Some("mailbox_full".to_string());
        row.legacy_endpoint = Some(CoreRelayEndpointConfig {
            url: "https://elsewhere.example".to_string(),
            token: "other-token".to_string(),
        });
        row.msg_id = Vec::new();
        steps.push(row);
    }
    let mut taken = capture(steps);
    taken.skipped_recipients = (0..500).map(|_| contact_user_id()).collect();

    let report = core_relay_shadow_compare(taken);
    store.note_relay_shadow_report(report, T0);

    let archive = store
        .export_protocol_events_jsonl()
        .expect("export the ring");
    let records = archive
        .lines()
        .filter(|line| line.contains("\"record\":\"event\""))
        .count();
    assert!(
        records <= 7,
        "one sample may cost the ring at most a summary plus one record per \
         mismatch kind, got {records}:\n{archive}"
    );
    // And it still says how widespread the trouble was.
    assert!(
        archive.contains("\"rows\":"),
        "a bounded record must carry the count it stands for:\n{archive}"
    );
}
