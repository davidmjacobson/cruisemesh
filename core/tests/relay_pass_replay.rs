//! The behavioural replay runner: incident fixtures **executed**, not just
//! validated.
//!
//! # What changed, and why it matters
//!
//! Package B1 gave the corpus under `core/tests/fixtures/` a validator. It
//! read each file, checked its schema, its ordering, its redaction and its
//! declared invariant ids, and walked the transcript for places where it
//! contradicted itself. That is real, and it is also only a statement about
//! a file. A fixture could be internally perfect and describe behaviour no
//! code in this repository has ever had.
//!
//! This file closes that gap for the relay-shaped incidents. Each executed
//! fixture now names a scenario that is *run*: a temporary `MessageStore`, a
//! fake clock, a scripted driver, and a real [`CoreRelayPass`] driven
//! action-by-action to its summary. The assertions are about what the session
//! did to the store and what it emitted, not about what a file says.
//!
//! # What "executing a fixture" means here — read this before adding one
//!
//! It does **not** mean replaying the fixture's event stream and requiring
//! the session to reproduce it. For most of this corpus that would be exactly
//! backwards: `carry-storm`, `sweep-livelock`, `watchdog-spray`,
//! `watermark-lock` and `zombie-outbound-queue` are transcripts of *bugs*.
//! They contain `invariant_violation` records. A session that reproduced them
//! would be the incident happening again.
//!
//! So a fixture is executed by:
//!
//! 1. building the scenario its title describes, in a real store;
//! 2. driving a real pass through it against a scripted relay;
//! 3. asserting the end state and the work counts the scenario demands; and
//! 4. asserting that every invariant id the fixture declares actually held —
//!    in particular that the session emitted no `invariant_violation` naming
//!    one of them.
//!
//! The fixture stays the human-readable record of what went wrong and the
//! index of which invariants the scenario is about. The scenario is the
//! executable proof that it does not happen here.
//!
//! # Where a fixture disagreed with the session
//!
//! One did, and it was corrected in the same commit as this runner rather
//! than worked around. `oversize-shrink` claimed a 256-row page retried at 64
//! rows; [`cruisemesh_core::relay_fetch_shrunk_limit`] halves, so the retry is
//! 128 and a second refusal is what reaches 64. The correction and its reason
//! are recorded in the contract's 6.6 notes.
//!
//! `short-page` was the other one read closely, because it counts an ack
//! against rows it also calls freshly consumed. It does not disagree: the
//! session acks a re-presented page whose rows this device had already
//! durably consumed, which is the same shape and a state C0 reaches. It is
//! unchanged, and no other fixture needed changing.
//!
//! # Honest scope
//!
//! Two fixtures are mesh-shaped and this session cannot drive them at all:
//! there is no encounter, no peer link and no receipt-repair planner in a
//! relay pass. They stay validate-only, and [`MESH_SHAPED`] names the package
//! that owns each. Claiming otherwise would be the thing this file exists to
//! stop.

use std::collections::BTreeMap;
use std::sync::Arc;

use cruisemesh_core::{
    compute_recipient_hint, core_relay_pass_default_budgets, relay_cursor_key, CarriedEnvelope,
    Contact, CoreRelayAction, CoreRelayActionKind, CoreRelayContactConfig, CoreRelayEndpointConfig,
    CoreRelayHeader, CoreRelayHttpRequest, CoreRelayHttpResult, CoreRelayOperation, CoreRelayPass,
    CoreRelayPassOutcome, CoreRelayPassPlan, CoreRelayPassSummary, CoreRelayProgressReason,
    MessageStore, OutboundEnvelope, OutgoingReceiptEnvelope, StoredMessage, KIND_RECEIPT,
    KIND_TEXT, RECEIPT_TYPE_DELIVERED, RELAY_PASS_MAX_CARRIED_UPLOADS,
};

// ===========================================================================
// Harness
// ===========================================================================

const OWN_URL: &str = "https://relay.example";
const OWN_TOKEN: &str = "member-token-aaaaaaaaaaaa";
const CONTACT_URL: &str = "https://contact-relay.example";
const CONTACT_TOKEN: &str = "member-token-bbbbbbbbbbbb";
const T0: i64 = 1_700_000_000_000;

fn new_store() -> Arc<MessageStore> {
    Arc::new(MessageStore::open(":memory:".to_string()).expect("in-memory store"))
}

fn own_user_id() -> Vec<u8> {
    (0u8..32).collect()
}

fn contact_user_id() -> Vec<u8> {
    vec![9u8; 32]
}

/// The hint this device's own mail arrives under. Used both to address
/// synthetic relay rows and to make a consumed-hidden record ackable.
fn own_hint(now_ms: i64) -> Vec<u8> {
    compute_recipient_hint(own_user_id(), now_ms)
}

fn base_plan(now_ms: i64) -> CoreRelayPassPlan {
    CoreRelayPassPlan {
        own: Some(CoreRelayEndpointConfig {
            url: OWN_URL.to_string(),
            token: OWN_TOKEN.to_string(),
        }),
        contacts: Vec::new(),
        own_user_id: own_user_id(),
        fetch_hints: vec![own_hint(now_ms)],
        presence_announce: Vec::new(),
        presence_query: Vec::new(),
        own_endpoint_changed: false,
        // A fresh process that has not swept. `relay_sweep_due` then answers
        // "yes" for a mailbox with no recorded sweep, which is what a first
        // walk after a restore looks like.
        swept_this_session: true,
        consecutive_rate_limits: 0,
        quiet_until_ms: 0,
        budgets: core_relay_pass_default_budgets(),
    }
}

fn own_cursor_key() -> String {
    relay_cursor_key(OWN_URL.to_string(), OWN_TOKEN.to_string())
}

/// One request the driver was handed, recorded verbatim.
#[derive(Clone, Debug)]
struct Recorded {
    action_id: u64,
    request: CoreRelayHttpRequest,
}

impl Recorded {
    fn is_fetch(&self) -> bool {
        self.request.operation == CoreRelayOperation::FetchPage
    }
    fn is_ack(&self) -> bool {
        self.request.operation == CoreRelayOperation::AckPage
    }
    fn is_post(&self) -> bool {
        self.request.operation == CoreRelayOperation::PostEnvelope
    }
    /// The `after=` this fetch asked from, or `None` for anything else.
    fn after(&self) -> Option<i64> {
        let query = self.request.path.split_once("after=")?.1;
        let value = query.split('&').next()?;
        value.parse().ok()
    }
    fn limit(&self) -> Option<u32> {
        let query = self.request.path.split_once("limit=")?.1;
        let value = query.split('&').next()?;
        value.parse().ok()
    }
}

/// What the scripted driver answers with.
#[derive(Clone, Debug)]
struct Reply {
    status: u16,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
    error: Option<cruisemesh_core::CoreRelayTransportError>,
    elapsed_ms: i64,
}

impl Reply {
    fn ok(body: Vec<u8>) -> Self {
        Reply {
            status: 200,
            headers: Vec::new(),
            body,
            error: None,
            elapsed_ms: 40,
        }
    }
    fn empty_ok() -> Self {
        Reply::ok(b"{}".to_vec())
    }
    fn status(status: u16, code: &str) -> Self {
        Reply {
            status,
            headers: Vec::new(),
            body: format!("{{\"code\":\"{code}\"}}").into_bytes(),
            error: None,
            elapsed_ms: 40,
        }
    }
    fn rate_limited(retry_after_s: u32) -> Self {
        let mut reply = Reply::status(429, "rate_limited");
        reply
            .headers
            .push(("Retry-After", retry_after_s.to_string()));
        reply
    }
    fn transport(error: cruisemesh_core::CoreRelayTransportError) -> Self {
        Reply {
            status: 0,
            headers: Vec::new(),
            body: Vec::new(),
            error: Some(error),
            // A driver-side timeout, well inside the pass deadline: a
            // transport failure that alone blew the wall budget would end the
            // pass before the second config was ever attempted, and it is the
            // second config these scenarios are about.
            elapsed_ms: 3_000,
        }
    }
    fn oversize() -> Self {
        let mut reply = Reply::transport(cruisemesh_core::CoreRelayTransportError::BodyTooLarge);
        reply.elapsed_ms = 2_300;
        reply
    }
}

/// What one driven pass did.
struct Run {
    requests: Vec<Recorded>,
    summary: CoreRelayPassSummary,
}

impl Run {
    fn fetches(&self) -> Vec<&Recorded> {
        self.requests.iter().filter(|r| r.is_fetch()).collect()
    }
    fn acks(&self) -> Vec<&Recorded> {
        self.requests.iter().filter(|r| r.is_ack()).collect()
    }
    fn posts(&self) -> Vec<&Recorded> {
        self.requests.iter().filter(|r| r.is_post()).collect()
    }
}

/// Drive a pass to its summary against a scripted responder.
///
/// The loop is bounded well above any declared budget on purpose: a pass that
/// does not terminate is `LIVE-01` failing, and the assertion that says so
/// has to fire rather than the test hanging.
fn drive(
    pass: &CoreRelayPass,
    start_ms: i64,
    mut respond: impl FnMut(&Recorded, usize) -> Reply,
) -> Run {
    let mut clock = start_ms;
    let mut requests: Vec<Recorded> = Vec::new();
    let mut action = pass.start(start_ms);

    loop {
        match action.kind {
            CoreRelayActionKind::Finished { summary } => return Run { requests, summary },
            CoreRelayActionKind::NotStarted => {
                panic!("drive() called start(), so the pass cannot be unstarted")
            }
            CoreRelayActionKind::Sleep { until_ms } => {
                // The only sleep this revision emits accompanies a finished
                // pass that refused to run inside a quiet window.
                assert!(
                    until_ms > clock,
                    "a sleep must name a future time, got {until_ms} at {clock}"
                );
                let summary = pass
                    .summary()
                    .expect("a pass that emits a sleep at stage Finish has a summary");
                return Run { requests, summary };
            }
            CoreRelayActionKind::Http { request } => {
                let recorded = Recorded {
                    action_id: action.action_id,
                    request,
                };
                let index = requests.len();
                assert!(
                    index < 4_096,
                    "LIVE-01: the pass issued {index} requests without terminating"
                );
                if let Some(previous) = requests.last() {
                    assert!(
                        recorded.action_id > previous.action_id,
                        "action ids must increase strictly across emitted actions: {} then {}",
                        previous.action_id,
                        recorded.action_id
                    );
                }
                let reply = respond(&recorded, index);
                clock = clock.saturating_add(reply.elapsed_ms);
                requests.push(recorded);
                action = pass.resume_http(CoreRelayHttpResult {
                    pass_id: action.pass_id.clone(),
                    action_id: action.action_id,
                    status: reply.status,
                    headers: reply
                        .headers
                        .iter()
                        .map(|(name, value)| CoreRelayHeader {
                            name: name.to_string(),
                            value: value.clone(),
                        })
                        .collect(),
                    body: reply.body,
                    error: reply.error,
                    completed_at_ms: clock,
                });
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Synthetic relay pages
// ---------------------------------------------------------------------------

fn b64(bytes: &[u8]) -> String {
    data_encoding::BASE64URL_NOPAD.encode(bytes)
}

fn msg_id(seed: u64) -> Vec<u8> {
    let mut id = vec![0u8; 16];
    id[..8].copy_from_slice(&seed.to_be_bytes());
    id[8] = 0xA5;
    id
}

/// One row as the relay serves it.
struct Row {
    id: i64,
    msg_id: Vec<u8>,
    hint: Vec<u8>,
    expiry_ms: i64,
}

fn page_body(rows: &[Row]) -> Vec<u8> {
    let next_cursor = rows.last().map(|row| row.id).unwrap_or(0);
    let envelopes: Vec<String> = rows
        .iter()
        .map(|row| {
            // Distinct payloads: the carry queue dedupes identical
            // (hint, sealed) pairs, so a page of one repeated byte pattern
            // would ingest as a single row.
            let mut sealed = vec![0x11u8; 96];
            sealed[..8].copy_from_slice(&(row.id as u64).to_be_bytes());
            format!(
                "{{\"id\":{},\"msg_id\":\"{}\",\"hop_ttl\":3,\"recipient_hint\":\"{}\",\
                 \"sealed\":\"{}\",\"expiry_ms\":{}}}",
                row.id,
                b64(&row.msg_id),
                b64(&row.hint),
                b64(&sealed),
                row.expiry_ms
            )
        })
        .collect();
    format!(
        "{{\"envelopes\":[{}],\"next_cursor\":{}}}",
        envelopes.join(","),
        next_cursor
    )
    .into_bytes()
}

fn empty_page() -> Vec<u8> {
    b"{\"envelopes\":[],\"next_cursor\":0}".to_vec()
}

/// Rows this device has already durably consumed, so the ingest reports
/// `Seen` and `core_relay_ack_ids_with_consumed` finds them ack-eligible.
///
/// This is the state a re-presented page is actually in — the page came back
/// because an ack failed, not because the rows are new — and it is the only
/// route to an ack that C0 owns end to end. Opening a sealed payload is
/// package D0's, so a fresh unknown row is `Carried` here and never acked,
/// which `ACK-01` requires anyway.
fn seed_consumed(store: &MessageStore, seeds: std::ops::Range<u64>, now_ms: i64) -> Vec<Row> {
    let hint = own_hint(now_ms);
    let expiry = now_ms + 6 * 24 * 60 * 60 * 1000;
    seeds
        .map(|seed| {
            let id = msg_id(seed);
            let recorded = store
                .core_record_consumed_hidden_msg_id(
                    id.clone(),
                    KIND_RECEIPT,
                    hint.clone(),
                    expiry,
                    own_user_id(),
                    now_ms,
                )
                .expect("record consumed-hidden");
            assert!(recorded, "the consumed set must vouch for the seeded row");
            Row {
                id: seed as i64,
                msg_id: id,
                hint: hint.clone(),
                expiry_ms: expiry,
            }
        })
        .collect()
}

/// Rows this device has never seen: the ingest carries them, and `ACK-01`
/// forbids acking a carried copy.
fn fresh_rows(first_id: i64, count: usize, now_ms: i64) -> Vec<Row> {
    let hint = own_hint(now_ms);
    let expiry = now_ms + 6 * 24 * 60 * 60 * 1000;
    (0..count)
        .map(|index| Row {
            id: first_id + index as i64,
            msg_id: msg_id(0x8000_0000 + first_id as u64 + index as u64),
            hint: hint.clone(),
            expiry_ms: expiry,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Emitted-transcript assertions
// ---------------------------------------------------------------------------

/// The store's protocol-event ring, validated and parsed.
fn transcript(store: &MessageStore) -> Vec<serde_json::Value> {
    let text = store
        .export_protocol_events_jsonl()
        .expect("the ring exports as JSONL");
    cruisemesh_core::validate(&text).expect("the emitted transcript is schema-valid");
    text.lines()
        .skip(1)
        .map(|line| serde_json::from_str(line).expect("event line parses"))
        .collect()
}

fn codes(events: &[serde_json::Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.get("code")?.as_str().map(str::to_string))
        .collect()
}

/// Every invariant the fixture declares must have *held*: no record may
/// report a violation naming one.
///
/// This is the assertion that makes an incident fixture executable. The
/// transcript of `carry-storm` contains `invariant_violation` with
/// `MARK-01`; the whole claim of the scenario below it is that a real pass
/// through the same situation emits none.
fn assert_no_violation_of(store: &MessageStore, declared: &[&str]) {
    for event in transcript(store) {
        if event.get("code").and_then(|code| code.as_str()) != Some("invariant_violation") {
            continue;
        }
        let named: Vec<String> = event
            .get("invariants")
            .and_then(|list| list.as_array())
            .map(|list| {
                list.iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        for id in declared {
            assert!(
                !named.iter().any(|found| found == id),
                "the session reported {id} violated: {event}"
            );
        }
    }
}

/// `SECRET-01`, against everything this pass can put in front of a person: no
/// token, no URL, no host reaches the ring or the summary.
fn assert_no_secrets(store: &MessageStore, summary: &CoreRelayPassSummary) {
    let text = store.export_protocol_events_jsonl().expect("export");
    for canary in [
        OWN_TOKEN,
        CONTACT_TOKEN,
        OWN_URL,
        CONTACT_URL,
        "relay.example",
    ] {
        assert!(
            !text.contains(canary),
            "SECRET-01: the event ring leaked {canary}"
        );
    }
    let rendered = format!("{summary:?}");
    for canary in [
        OWN_TOKEN,
        CONTACT_TOKEN,
        OWN_URL,
        CONTACT_URL,
        "relay.example",
    ] {
        assert!(
            !rendered.contains(canary),
            "SECRET-01: the pass summary leaked {canary}"
        );
    }
}

// ===========================================================================
// The fixture ledger
// ===========================================================================

/// Fixtures this file cannot execute, with the package that will.
///
/// A relay pass has no encounter, no peer link and no receipt-repair planner,
/// so driving either of these against `CoreRelayPass` would be theatre. They
/// keep the B1 validator as their only owner until the named package lands.
const MESH_SHAPED: &[(&str, &str)] = &[
    (
        "watchdog-spray",
        "package D2 (mesh_meet): per-encounter spray planning has no relay pass to run inside. \
         The byte and cadence half is already core in spray_policy.rs (#280); what is missing is \
         a session that plans an encounter, which D2 builds.",
    ),
    (
        "watermark-lock",
        "package D2 (mesh_meet): receipt repair is a peer-link decision. WM-01 is carried as \
         hoist-pending against the two shells' repair planners, and D2 gives it a bounded core \
         state machine to drive.",
    ),
];

/// Every fixture, and how this file treats it. The test below asserts the two
/// lists together cover the directory exactly, so a new fixture cannot be
/// added without a decision about whether it executes.
const EXECUTED: &[&str] = &[
    "429-mid-receipts",
    "ack-fail-after-consume",
    "carry-storm",
    "contact-silence-no-proof",
    "group-fanout-complete",
    "group-fanout-partial",
    "oversize-shrink",
    "pending-rerun-during-backoff",
    "short-page",
    "sweep-livelock",
    "zombie-outbound-queue",
];

#[test]
fn every_fixture_is_either_executed_or_explicitly_scoped_out() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
        .expect("fixtures directory")
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                return None;
            }
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
        })
        .collect();
    on_disk.sort();

    let mut claimed: Vec<String> = EXECUTED.iter().map(|name| name.to_string()).collect();
    claimed.extend(MESH_SHAPED.iter().map(|(name, _)| name.to_string()));
    claimed.sort();

    assert_eq!(
        on_disk, claimed,
        "every fixture must be in EXECUTED or in MESH_SHAPED with the package that will own it. \
         A fixture in neither list is one nobody decided about."
    );

    for (name, reason) in MESH_SHAPED {
        assert!(
            reason.contains("package D"),
            "{name} is scoped out without naming the package that will own it"
        );
    }
}

/// Every executed fixture's declared invariants are exercised by the scenario
/// named for it, and this test says which scenario that is. A fixture whose
/// scenario function is deleted fails here rather than silently becoming
/// validate-only again.
#[test]
fn executed_fixtures_name_their_scenario() {
    let scenarios: BTreeMap<&str, &str> = BTreeMap::from([
        ("429-mid-receipts", "a_family_429_mid_upload_ends_the_pass"),
        (
            "ack-fail-after-consume",
            "a_failed_ack_holds_the_frontier_and_the_restart_replays_inertly",
        ),
        ("carry-storm", "a_marked_carried_row_is_never_reoffered"),
        (
            "contact-silence-no-proof",
            "silence_is_committed_only_with_proof_another_relay_answered",
        ),
        (
            "group-fanout-complete",
            "a_group_envelope_becomes_one_row_per_member_and_is_marked_only_when_all_land",
        ),
        (
            "group-fanout-partial",
            "a_partial_group_fan_out_resumes_with_the_members_that_did_not_land",
        ),
        (
            "oversize-shrink",
            "a_page_over_the_body_cap_is_retried_smaller_at_the_same_cursor",
        ),
        (
            "pending-rerun-during-backoff",
            "a_pass_started_inside_the_quiet_window_spends_nothing",
        ),
        ("short-page", "a_short_page_is_not_the_end_of_the_mailbox"),
        (
            "sweep-livelock",
            "a_yielding_sweep_advances_its_cursor_and_resumes_from_it",
        ),
        (
            "zombie-outbound-queue",
            "the_authored_lane_is_bounded_and_the_queue_it_reads_shrinks",
        ),
    ]);
    let source = include_str!("relay_pass_replay.rs");
    for name in EXECUTED {
        let scenario = scenarios
            .get(name)
            .unwrap_or_else(|| panic!("{name} is executed but names no scenario"));
        assert!(
            source.contains(&format!("fn {scenario}(")),
            "{name} names scenario {scenario}, which does not exist in this file"
        );
    }
}

// ===========================================================================
// Executed scenarios
// ===========================================================================

// ---------------------------------------------------------------------------
// short-page — PAGE-01, CURSOR-01
// ---------------------------------------------------------------------------

#[test]
fn a_short_page_is_not_the_end_of_the_mailbox() {
    let store = new_store();
    let now = T0;
    let rows = seed_consumed(&store, 1..101, now);
    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());

    let run = drive(&pass, now, |request, index| {
        if request.is_ack() {
            return Reply::empty_ok();
        }
        match index {
            // A server that clamps the limit: 100 rows for a 256-row ask.
            0 => Reply::ok(page_body(&rows)),
            // The walk must continue from the short page, not stop on it.
            _ => Reply::ok(empty_page()),
        }
    });

    let fetches = run.fetches();
    assert_eq!(
        fetches.len(),
        2,
        "PAGE-01: a short page continues the walk; only an empty page is EOF"
    );
    assert_eq!(fetches[0].after(), Some(0));
    assert_eq!(
        fetches[0].limit(),
        Some(256),
        "the client asks for its own page size; the server may clamp it"
    );
    assert_eq!(
        fetches[1].after(),
        Some(100),
        "CURSOR-01: the second fetch resumes from the frontier the first page earned"
    );
    assert_eq!(
        run.acks().len(),
        1,
        "the consumed rows earn exactly one ack"
    );
    assert_eq!(run.summary.rows_acked, 100);
    assert_eq!(run.summary.frontier_advances, 1);
    assert_eq!(run.summary.outcome, CoreRelayPassOutcome::Completed);
    let cursor = store.relay_fetch_cursor(own_cursor_key()).expect("cursor");
    assert_eq!(
        cursor.after_id, 100,
        "CURSOR-01: the frontier ends where the fully processed page left it"
    );
    assert_eq!(
        cursor.sweep_after_id, 0,
        "this was an ordinary frontier walk, not a sweep"
    );

    assert_no_violation_of(&store, &["PAGE-01", "CURSOR-01"]);
    assert_no_secrets(&store, &run.summary);
}

// ---------------------------------------------------------------------------
// ack-fail-after-consume — TXN-01, CURSOR-01, IDEMP-01
// ---------------------------------------------------------------------------

#[test]
fn a_failed_ack_holds_the_frontier_and_the_restart_replays_inertly() {
    let store = new_store();
    let now = T0;
    let rows = seed_consumed(&store, 401..441, now);

    // Pass one: the page is durably consumed, then the ack fails.
    let first = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let run_one = drive(&first, now, |request, index| {
        if request.is_ack() {
            return Reply::status(503, "unavailable");
        }
        match index {
            0 => Reply::ok(page_body(&rows)),
            _ => Reply::ok(empty_page()),
        }
    });

    assert_eq!(run_one.acks().len(), 1);
    assert_eq!(
        run_one.summary.rows_acked, 0,
        "an ack that failed acked nothing"
    );
    assert_eq!(
        run_one.summary.frontiers_held, 1,
        "CURSOR-01: a page whose required ack did not succeed holds the frontier"
    );
    assert_eq!(run_one.summary.frontier_advances, 0);
    let cursor = store.relay_fetch_cursor(own_cursor_key()).expect("cursor");
    assert_eq!(
        (cursor.after_id, cursor.sweep_after_id),
        (0, 0),
        "TXN-01: the ingest transaction committed, the frontier transaction did not"
    );

    // The ordering that makes TXN-01 structural: the ack request was formed
    // only after the ingest had returned, so no transaction could have been
    // open across it.
    let emitted = codes(&transcript(&store));
    let ingest_at = emitted
        .iter()
        .position(|code| code == "page_ingested")
        .expect("the page was ingested");
    let ack_at = emitted
        .iter()
        .enumerate()
        .filter(|(_, code)| *code == "action_emitted")
        .map(|(index, _)| index)
        .find(|index| *index > ingest_at)
        .expect("the ack action follows the ingest");
    assert!(
        ingest_at < ack_at,
        "TXN-01: the page-consume transaction must close before an ack is even formed"
    );

    // Pass two: a process restart. Nothing in memory survives; the pass is
    // rebuilt from durable markers alone, the relay re-presents the same
    // page, and the replay must apply nothing.
    let carried_before = store.carried_len().expect("carry depth");
    let second = CoreRelayPass::new(store.clone(), base_plan(now + 9_000), "p2".to_string());
    let run_two = drive(&second, now + 9_000, |request, index| {
        if request.is_ack() {
            return Reply::empty_ok();
        }
        match index {
            0 => Reply::ok(page_body(&rows)),
            _ => Reply::ok(empty_page()),
        }
    });

    assert_eq!(
        run_two.summary.rows_ingested, 0,
        "IDEMP-01: re-presenting an already-ingested page must persist nothing new"
    );
    assert_eq!(
        store.carried_len().expect("carry depth"),
        carried_before,
        "IDEMP-01: a replay must not consume or duplicate a carried row"
    );
    assert_eq!(run_two.summary.rows_acked, 40);
    assert_eq!(
        run_two.summary.frontier_advances, 1,
        "CURSOR-01: the frontier moves once the ack it was waiting on succeeds"
    );

    assert_no_violation_of(&store, &["TXN-01", "CURSOR-01", "IDEMP-01"]);
    assert_no_secrets(&store, &run_two.summary);
}

// ---------------------------------------------------------------------------
// 429-mid-receipts — RATE-01, LIVE-01
// ---------------------------------------------------------------------------

#[test]
fn a_family_429_mid_upload_ends_the_pass() {
    let store = new_store();
    let now = T0;
    seed_receipts(&store, 9, now);
    seed_authored(&store, 4, now);
    seed_carried(&store, 12, now);

    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let run = drive(&pass, now, |_request, index| {
        if index == 0 {
            Reply::empty_ok()
        } else {
            Reply::rate_limited(15)
        }
    });

    assert_eq!(
        run.summary.requests_issued, 2,
        "RATE-01: the first family 429 ends every remaining network stage — no fetch, no \
         presence, no further upload"
    );
    assert_eq!(run.summary.outcome, CoreRelayPassOutcome::RateLimited);
    assert_eq!(run.summary.receipt_uploads, 1);
    assert_eq!(run.summary.authored_uploads, 0);
    assert_eq!(run.summary.carried_uploads, 0);
    assert!(run.fetches().is_empty(), "RATE-01: the walks never ran");
    assert!(
        run.summary.quiet_until_ms >= now + 15_000,
        "Retry-After is a floor: {} is inside the advertised window",
        run.summary.quiet_until_ms
    );
    let continuation = run
        .summary
        .continuation
        .expect("a rate-limited pass defers into the window it just recorded");
    assert_eq!(
        continuation.reason,
        CoreRelayProgressReason::QuietWindowExtended,
        "PROGRESS-01's second permitted shape: a strictly later deadline"
    );
    assert_eq!(continuation.not_before_ms, run.summary.quiet_until_ms);

    // Divergence (c), resolved toward Android: the window is committed at the
    // refusal, not at the end. The case that decides it is a pass that ends
    // abnormally *after* its 429 -- here a cancellation, which is what an app
    // backgrounded mid-pass does. A pass that accumulated the window and only
    // wrote it at the end would report none at all from this path.
    let cancelled_store = new_store();
    seed_receipts(&cancelled_store, 3, now);
    let cancelled = CoreRelayPass::new(cancelled_store.clone(), base_plan(now), "p2".to_string());
    let action = cancelled.start(now);
    assert!(
        matches!(action.kind, CoreRelayActionKind::Http { .. }),
        "the scenario needs a request to refuse"
    );
    let refused_at = now + 40;
    let reply = Reply::rate_limited(45);
    let _ = cancelled.resume_http(CoreRelayHttpResult {
        pass_id: action.pass_id.clone(),
        action_id: action.action_id,
        status: reply.status,
        headers: reply
            .headers
            .iter()
            .map(|(name, value)| CoreRelayHeader {
                name: name.to_string(),
                value: value.clone(),
            })
            .collect(),
        body: reply.body,
        error: None,
        completed_at_ms: refused_at,
    });
    let after_cancel = cancelled.cancel(refused_at + 5);
    assert!(
        after_cancel.quiet_until_ms >= refused_at + 45_000,
        "RATE-01: a pass cancelled after its refusal still carries the window it recorded, got {}",
        after_cancel.quiet_until_ms
    );

    assert_no_violation_of(&store, &["RATE-01", "LIVE-01"]);
    assert_no_secrets(&store, &run.summary);
}

// ---------------------------------------------------------------------------
// pending-rerun-during-backoff — RATE-01, PROGRESS-01
// ---------------------------------------------------------------------------

#[test]
fn a_pass_started_inside_the_quiet_window_spends_nothing() {
    let store = new_store();
    let now = T0;
    seed_carried(&store, 4, now);

    let mut plan = base_plan(now);
    plan.quiet_until_ms = now + 30_000;
    let deferred = CoreRelayPass::new(store.clone(), plan, "p2".to_string());
    let run = drive(&deferred, now, |_request, _index| {
        panic!("RATE-01: a pass inside the quiet window must issue no request at all")
    });
    assert_eq!(run.summary.requests_issued, 0);
    assert_eq!(
        run.summary.outcome,
        CoreRelayPassOutcome::RefusedQuietWindow
    );
    assert_eq!(run.summary.quiet_until_ms, now + 30_000);
    assert!(
        run.summary.continuation.is_none(),
        "the deferral is the caller's coalesced retry, not a second schedule from the pass"
    );

    // Once the window has elapsed the same nudge runs and makes progress:
    // PROGRESS-01's point is that deferral is bounded, not that it is
    // permanent.
    let later = now + 30_001;
    let after = CoreRelayPass::new(store.clone(), base_plan(later), "p3".to_string());
    let run_after = drive(&after, later, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert!(
        run_after.summary.carried_uploads > 0,
        "the deferred work runs once the window elapses"
    );
    assert_eq!(run_after.summary.outcome, CoreRelayPassOutcome::Completed);

    assert_no_violation_of(&store, &["RATE-01", "PROGRESS-01"]);
    assert_no_secrets(&store, &run_after.summary);
}

// ---------------------------------------------------------------------------
// oversize-shrink — PAGE-01, LIVE-01
// ---------------------------------------------------------------------------

#[test]
fn a_page_over_the_body_cap_is_retried_smaller_at_the_same_cursor() {
    let store = new_store();
    let now = T0;
    let rows = seed_consumed(&store, 1..65, now);
    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());

    let run = drive(&pass, now, |request, _index| {
        if request.is_ack() {
            return Reply::empty_ok();
        }
        match request.limit() {
            // The relay's page for this window is over the cap at 256 and at
            // 128; the driver refuses the body before decode both times.
            Some(256) | Some(128) => Reply::oversize(),
            Some(64) => Reply::ok(page_body(&rows)),
            _ => Reply::ok(empty_page()),
        }
    });

    let limits: Vec<Option<u32>> = run.fetches().iter().map(|fetch| fetch.limit()).collect();
    assert_eq!(
        limits,
        vec![Some(256), Some(128), Some(64), Some(64)],
        "PAGE-01: the retry halves rather than skipping, and the last fetch is the empty page \
         that ends the walk"
    );
    let cursors: Vec<Option<i64>> = run
        .fetches()
        .iter()
        .take(3)
        .map(|fetch| fetch.after())
        .collect();
    assert_eq!(
        cursors,
        vec![Some(0), Some(0), Some(0)],
        "PAGE-01: a body over the cap is retried at the SAME cursor. Advancing past it would \
         strand every row in the window."
    );
    assert_eq!(run.summary.rows_acked, 64);
    assert!(
        run.summary.response_bytes_read < run.summary.budgets.max_response_bytes,
        "LIVE-01: the pass stayed inside its declared byte budget"
    );

    assert_no_violation_of(&store, &["PAGE-01", "LIVE-01"]);
    assert_no_secrets(&store, &run.summary);
}

// ---------------------------------------------------------------------------
// carry-storm — MARK-01, CARRY-01, LIVE-01
// ---------------------------------------------------------------------------

#[test]
fn a_marked_carried_row_is_never_reoffered() {
    let store = new_store();
    let now = T0;
    seed_carried(&store, 5, now);
    let carried_before = store.carried_len().expect("carry depth");

    let first = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let run_one = drive(&first, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert_eq!(run_one.summary.carried_uploads, 5);
    assert_eq!(
        run_one.summary.carried_rows_marked, 5,
        "MARK-01: every accepted upload is marked durably, inside the pass that earned it"
    );
    assert_eq!(
        store.carried_len().expect("carry depth"),
        carried_before,
        "CARRY-01: uploading a carried row does not remove it. Only digest/receipt proof or \
         expiry may."
    );

    // The storm: a restart re-offering the whole queue is #222. A pass built
    // from durable markers alone must offer none of it.
    let second = CoreRelayPass::new(store.clone(), base_plan(now + 60_000), "p2".to_string());
    let run_two = drive(&second, now + 60_000, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert_eq!(
        run_two.summary.carried_uploads, 0,
        "MARK-01: the marker survives the pass that wrote it and suppresses re-upload"
    );
    assert_eq!(
        run_two.posts().len(),
        0,
        "the re-upload storm is exactly this count being non-zero on every launch"
    );

    // And the bound holds even with a queue far deeper than one pass may
    // spend on (LIVE-01).
    let deep = new_store();
    seed_carried(&deep, RELAY_PASS_MAX_CARRIED_UPLOADS as usize + 40, now);
    let third = CoreRelayPass::new(deep.clone(), base_plan(now), "p3".to_string());
    let run_three = drive(&third, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert_eq!(
        run_three.summary.carried_uploads, RELAY_PASS_MAX_CARRIED_UPLOADS,
        "LIVE-01: a deep carry queue is bounded per pass, not drained in one"
    );

    assert_no_violation_of(&store, &["MARK-01", "CARRY-01", "LIVE-01"]);
    assert_no_secrets(&store, &run_two.summary);
}

// ---------------------------------------------------------------------------
// contact-silence-no-proof — SILENCE-01
// ---------------------------------------------------------------------------

#[test]
fn silence_is_committed_only_with_proof_another_relay_answered() {
    let store = new_store();
    let now = T0;
    seed_contact(&store);

    let mut plan = base_plan(now);
    plan.contacts = vec![CoreRelayContactConfig {
        user_id: contact_user_id(),
        relay_url: Some(CONTACT_URL.to_string()),
        relay_token: Some(CONTACT_TOKEN.to_string()),
        endpoint_usable: true,
        endpoint_answering: true,
    }];

    // Pass one: nothing answers. This device cannot tell a silent contact
    // from its own dead internet, so it may write nothing off.
    let first = CoreRelayPass::new(store.clone(), plan.clone(), "p1".to_string());
    let run_one = drive(&first, now, |_request, _index| {
        Reply::transport(cruisemesh_core::CoreRelayTransportError::ConnectionFailed)
    });
    assert_eq!(
        run_one.summary.silence_committed, 0,
        "SILENCE-01: no proof, no commit"
    );
    assert_eq!(run_one.summary.silence_discarded, 1);
    assert!(
        store
            .list_contact_relay_unreachable()
            .expect("unreachable list")
            .is_empty(),
        "SILENCE-01: a healthy contact must not be written off because this phone was offline"
    );

    // Pass two: our own mailbox answers and the contact's still does not.
    // Now the silence means something.
    let later = now + 120_000;
    let second = CoreRelayPass::new(store.clone(), plan.clone(), "p2".to_string());
    let run_two = drive(&second, later, |request, _index| {
        if request.request.base_url == OWN_URL {
            Reply::ok(empty_page())
        } else {
            Reply::transport(cruisemesh_core::CoreRelayTransportError::Timeout)
        }
    });
    assert_eq!(
        run_two.summary.silence_committed, 1,
        "SILENCE-01: same-pass proof another relay answered is what licenses the commit"
    );
    let rested = store
        .list_contact_relay_unreachable()
        .expect("unreachable list");
    assert_eq!(rested.len(), 1);
    assert_eq!(rested[0].user_id, contact_user_id());

    // Pass three: the contact answers again and the streak clears, so the
    // rest is a delay with an expiry rather than a verdict.
    let later_still = later + 120_000;
    let third = CoreRelayPass::new(store.clone(), plan, "p3".to_string());
    let run_three = drive(&third, later_still, |_request, _index| {
        Reply::ok(empty_page())
    });
    assert_eq!(run_three.summary.silence_committed, 0);
    assert!(
        store
            .list_contact_relay_unreachable()
            .expect("unreachable list")
            .is_empty(),
        "an endpoint that answers clears its streak"
    );

    assert_no_violation_of(&store, &["SILENCE-01"]);
    assert_no_secrets(&store, &run_three.summary);
}

// ---------------------------------------------------------------------------
// sweep-livelock — PROGRESS-01, LIVE-01, CURSOR-01
// ---------------------------------------------------------------------------

#[test]
fn a_yielding_sweep_advances_its_cursor_and_resumes_from_it() {
    let store = new_store();
    let now = T0;

    // A mailbox of rows this device is only muling: nothing is ack-eligible,
    // which is exactly the shape that used to hold the cursor at 0 forever.
    // `swept_this_session: false` is what makes `relay_sweep_due` answer yes
    // on a mailbox with no recorded sweep -- the first walk after a restore,
    // and the walk #270 livelocked.
    let mut sweeping = base_plan(now);
    sweeping.swept_this_session = false;
    let pass = CoreRelayPass::new(store.clone(), sweeping.clone(), "p1".to_string());
    let run = drive(&pass, now, |request, index| {
        assert!(request.is_fetch(), "a page of carried rows earns no ack");
        let first_id = 1 + (index as i64) * 256;
        Reply::ok(page_body(&fresh_rows(first_id, 256, now)))
    });

    assert_eq!(
        run.summary.outcome,
        CoreRelayPassOutcome::BudgetYield,
        "LIVE-01: the walk stops on its declared budget rather than draining the mailbox"
    );
    assert_eq!(run.summary.envelopes_processed, 512);
    let cursor = store.relay_fetch_cursor(own_cursor_key()).expect("cursor");
    assert!(
        cursor.sweep_after_id > 0,
        "PROGRESS-01: the yield must leave a resume point. Zero here is the livelock: every \
         pass re-walks from the bottom and reaches the same budget."
    );
    let resume_point = cursor.sweep_after_id;
    let continuation = run
        .summary
        .continuation
        .expect("a yield that advanced a cursor earns a continuation");
    assert_eq!(continuation.reason, CoreRelayProgressReason::CursorAdvanced);
    assert!(continuation.not_before_ms > run.summary.finished_at_ms);

    // The continuation resumes from the recorded point rather than from zero.
    let later = continuation.not_before_ms;
    let mut resumed = base_plan(later);
    resumed.swept_this_session = false;
    let second = CoreRelayPass::new(store.clone(), resumed, "p2".to_string());
    let run_two = drive(&second, later, |request, _index| {
        assert!(request.is_fetch());
        Reply::ok(empty_page())
    });
    assert_eq!(
        run_two.fetches()[0].after(),
        Some(resume_point),
        "PROGRESS-01: a continuation that restarted at 0 would be the livelock with a delay"
    );

    // And a pass that buys nothing schedules nothing. This is the property
    // that makes an unchanged-state reschedule unrepresentable rather than
    // merely forbidden.
    let empty = new_store();
    let third = CoreRelayPass::new(empty.clone(), base_plan(now), "p3".to_string());
    let run_three = drive(&third, now, |_request, _index| Reply::ok(empty_page()));
    assert!(
        run_three.summary.continuation.is_none(),
        "PROGRESS-01: nothing advanced, so nothing may be rescheduled"
    );

    assert_no_violation_of(&store, &["PROGRESS-01", "LIVE-01", "CURSOR-01"]);
    assert_no_secrets(&store, &run.summary);
}

// ---------------------------------------------------------------------------
// zombie-outbound-queue — QUEUE-01, LIVE-01
// ---------------------------------------------------------------------------

#[test]
fn the_authored_lane_is_bounded_and_the_queue_it_reads_shrinks() {
    let store = new_store();
    let now = T0;
    let budget = core_relay_pass_default_budgets().max_authored_uploads;
    seed_authored(&store, (budget + 12) as usize, now);

    let first = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let run_one = drive(&first, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert_eq!(
        run_one.summary.authored_uploads, budget,
        "LIVE-01: the authored lane is bounded per pass, whatever the queue depth"
    );

    let still_queued = store
        .pending_relay_outbound_envelopes(1_000, now, Vec::new())
        .expect("pending outbound")
        .len();
    assert_eq!(
        still_queued, 12,
        "QUEUE-01: the advertised set shrinks — every uploaded row is marked posted, so the \
         next pass reads a strictly smaller queue"
    );

    // A pass over a queue with nothing left to upload spends nothing and
    // schedules nothing: no scan cost without progress.
    let drained = new_store();
    let second = CoreRelayPass::new(drained.clone(), base_plan(now), "p2".to_string());
    let run_two = drive(&second, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert_eq!(run_two.summary.authored_uploads, 0);
    assert!(run_two.summary.continuation.is_none());

    assert_no_violation_of(&store, &["QUEUE-01", "LIVE-01"]);
    assert_no_secrets(&store, &run_one.summary);
}

/// DEDUP-01: a relay that answers an authored upload with a 409
/// `msg_id_conflict` — the mailbox already holds a different ciphertext under
/// this envelope's public msg_id — must not retire the send. A conflict is a
/// non-2xx, so it never reaches the mark-posted path; the row stays queued for
/// the next pass and for the mesh/carry paths, and the conflict is
/// per-envelope, so the lane continues rather than writing off the mailbox.
#[test]
fn a_msg_id_conflict_leaves_the_authored_row_queued_and_continues_the_lane() {
    let store = new_store();
    let now = T0;
    seed_authored(&store, 3, now);

    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let mut posts = 0u32;
    let run = drive(&pass, now, |request, _index| {
        if request.is_post() {
            posts += 1;
            // Every authored post to the own mailbox conflicts.
            Reply::status(409, "msg_id_conflict")
        } else if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });

    // The lane did not stop on the first conflict: all three rows were tried.
    assert_eq!(
        posts, 3,
        "a per-envelope conflict is terminal for its row but must not end the lane"
    );
    // None was accepted, so none was marked posted.
    assert_eq!(
        run.summary.authored_uploads, 0,
        "a conflicted upload must not count as an accepted authored upload"
    );

    // Every authored row is still queued — the send state was not retired.
    let still_queued = store
        .pending_relay_outbound_envelopes(1_000, now, Vec::new())
        .expect("pending outbound")
        .len();
    assert_eq!(
        still_queued, 3,
        "DEDUP-01: a msg_id conflict must leave every authored row queued to deliver another way"
    );

    assert_no_secrets(&store, &run.summary);
}

// ===========================================================================
// Group fan-out: one row per member, and `relay_posted_at` only at the end
// ===========================================================================

#[test]
fn a_group_envelope_becomes_one_row_per_member_and_is_marked_only_when_all_land() {
    // The mail-losing shape. A group envelope carries
    // `recipient_user_id = group_id`, which is nobody's contact entry, so a
    // lane that posts one row to one resolved mailbox posts a single
    // group-hinted row into this device's own mailbox — where no member ever
    // looks, because members poll under their own daily hints. That is the
    // shape #140 fixed on the shells.
    let store = new_store();
    let now = T0;
    let group = seed_group(&store, 3);
    seed_group_authored(&store, &group, now);

    let mut plan = base_plan(now);
    plan.contacts = group_contacts(&group);

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });

    let posts = run.posts();
    assert_eq!(
        posts.len(),
        3,
        "one row per member, not one row for the group"
    );
    let bodies: std::collections::BTreeSet<Vec<u8>> =
        posts.iter().map(|post| post.request.body.clone()).collect();
    assert_eq!(
        bodies.len(),
        3,
        "each member's row is addressed to that member: distinct hints and distinct msg_ids"
    );
    assert_eq!(run.summary.authored_uploads, 3);

    assert!(
        store
            .pending_relay_outbound_envelopes(1_000, now, Vec::new())
            .expect("pending outbound")
            .is_empty(),
        "every member's row landed, so the envelope is relay-posted"
    );

    // And a second pass re-posts nothing.
    let second = CoreRelayPass::new(store.clone(), base_plan(now), "p2".to_string());
    let run_two = drive(&second, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert_eq!(run_two.posts().len(), 0);

    assert_no_secrets(&store, &run.summary);
}

#[test]
fn a_partial_group_fan_out_resumes_with_the_members_that_did_not_land() {
    // The property the per-member markers exist for. The legacy engine
    // retries the whole set when one row fails, which costs every member a
    // repeat post on every pass for as long as one row keeps failing. Here
    // the landed rows are remembered durably, so the next pass posts only
    // what is still owed — and the envelope stays queued until it is.
    let store = new_store();
    let now = T0;
    let group = seed_group(&store, 3);
    seed_group_authored(&store, &group, now);
    let mut plan = base_plan(now);
    plan.contacts = group_contacts(&group);

    let first = CoreRelayPass::new(store.clone(), plan.clone(), "p1".to_string());
    let mut posts_seen = 0usize;
    let mut failed_body: Option<Vec<u8>> = None;
    let run_one = drive(&first, now, |request, _index| {
        if request.is_fetch() {
            return Reply::ok(empty_page());
        }
        if !request.is_post() {
            return Reply::empty_ok();
        }
        posts_seen += 1;
        if posts_seen == 2 {
            failed_body = Some(request.request.body.clone());
            // A mailbox-shaped fault: the lane stops spending on that relay
            // for the rest of the pass, which is exactly the shape that
            // leaves a fan-out half-posted.
            return Reply::status(500, "server_error");
        }
        Reply::empty_ok()
    });
    assert_eq!(
        run_one.posts().len(),
        2,
        "the first row landed and the second was refused, ending the lane for that mailbox"
    );
    let failed_body = failed_body.expect("one row was refused");

    let still_queued = store
        .pending_relay_outbound_envelopes(1_000, now, Vec::new())
        .expect("pending outbound");
    assert_eq!(
        still_queued.len(),
        1,
        "one member never received the message, so the envelope is not relay-posted"
    );

    // Pass two: only the member whose row was refused is posted again, and
    // it is the same row — the deterministic fan-out msg_id and the same
    // member hint, byte for byte.
    let later = now + 60_000;
    let second = CoreRelayPass::new(store.clone(), plan, "p2".to_string());
    let run_two = drive(&second, later, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    let posts_two = run_two.posts();
    assert_eq!(
        posts_two.len(),
        2,
        "exactly the two members still owed a row, and not the one that landed"
    );
    assert!(
        posts_two
            .iter()
            .any(|post| post.request.body == failed_body),
        "the refused row is resumed byte for byte: the fan-out msg_id is deterministic"
    );
    assert!(
        store
            .pending_relay_outbound_envelopes(1_000, later, Vec::new())
            .expect("pending outbound")
            .is_empty(),
        "the last member landed, so now the envelope is relay-posted"
    );

    assert_no_secrets(&store, &run_two.summary);
}

#[test]
fn a_blocked_member_gets_no_fan_out_row_and_does_not_hold_the_envelope_open() {
    // Every other outbound fan-out in this codebase drops blocked users
    // before it sends, and a relay row is a send. The second half is the one
    // that matters here: an excluded member is not *owed* a landing, so their
    // absence must not keep the envelope queued forever, re-posting the rest
    // of the group on every pass.
    let store = new_store();
    let now = T0;
    let group = seed_group(&store, 3);
    seed_group_authored(&store, &group, now);
    store
        .block_user(group.member_user_ids[1].clone(), now)
        .expect("block a member");

    let mut plan = base_plan(now);
    plan.contacts = group_contacts(&group);

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });

    assert_eq!(
        run.posts().len(),
        2,
        "a blocked member is excluded from the fan-out"
    );
    assert!(
        store
            .pending_relay_outbound_envelopes(1_000, now, Vec::new())
            .expect("pending outbound")
            .is_empty(),
        "the envelope is complete once every member it still owes has landed"
    );
}

#[test]
fn a_group_whose_only_endpoint_is_resting_posts_nothing_rather_than_misrouting() {
    // `relay_posted_at` is terminal, so posting a cross-family group's rows
    // into our own mailbox is not a retry — it is a permanent misroute. A
    // member resting for silence contributes no fallback, and with no other
    // member resolving, the answer is to post nothing this pass and leave the
    // envelope for a later one and for the mesh paths.
    let store = new_store();
    let now = T0;
    let group = seed_group(&store, 1);
    seed_group_authored(&store, &group, now);

    let mut plan = base_plan(now);
    plan.contacts = group_contacts(&group);
    plan.contacts[0].endpoint_answering = false;

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });

    assert_eq!(run.posts().len(), 0, "nowhere safe to post, so nothing did");
    assert_eq!(
        store
            .pending_relay_outbound_envelopes(1_000, now, Vec::new())
            .expect("pending outbound")
            .len(),
        1,
        "the envelope stays queued: a resting host that comes back still receives it"
    );
}

// ===========================================================================
// contact-silence-no-proof, upload half — rejection and silence diverge
// ===========================================================================

#[test]
fn rejection_falls_back_to_our_own_mailbox_and_silence_declines_to_post() {
    // The two brakes are different answers, and the one flag they used to
    // share could only express the first. Rejection means the *card* is
    // wrong: our own mailbox is a real alternative and the row should go
    // there. Silence means nothing answered: falling back would put a
    // cross-family contact's mail somewhere they never read, and because
    // `relay_posted_at` is terminal that is a permanent misroute, not a
    // retry.
    let now = T0;

    // Rejection: written off, but the endpoint answers. Fall back.
    let rejected = new_store();
    seed_contact(&rejected);
    seed_authored(&rejected, 2, now);
    let mut plan = base_plan(now);
    plan.contacts = vec![CoreRelayContactConfig {
        user_id: contact_user_id(),
        relay_url: Some(CONTACT_URL.to_string()),
        relay_token: Some(CONTACT_TOKEN.to_string()),
        endpoint_usable: false,
        endpoint_answering: true,
    }];
    let pass = CoreRelayPass::new(rejected.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    let posts = run.posts();
    assert_eq!(posts.len(), 2, "a rejection has somewhere else to go");
    assert!(
        posts.iter().all(|post| post.request.base_url == OWN_URL),
        "the fallback is our own mailbox"
    );
    assert!(
        rejected
            .pending_relay_outbound_envelopes(1_000, now, Vec::new())
            .expect("pending outbound")
            .is_empty(),
        "the fallback posts really happened, so the rows are marked"
    );

    // Silence: the card may be perfectly good, but nothing is answering.
    // Post nothing, mark nothing.
    let silent = new_store();
    seed_contact(&silent);
    seed_authored(&silent, 2, now);
    let mut plan = base_plan(now);
    plan.contacts = vec![CoreRelayContactConfig {
        user_id: contact_user_id(),
        relay_url: Some(CONTACT_URL.to_string()),
        relay_token: Some(CONTACT_TOKEN.to_string()),
        endpoint_usable: true,
        endpoint_answering: false,
    }];
    let pass = CoreRelayPass::new(silent.clone(), plan, "p2".to_string());
    let run = drive(&pass, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert_eq!(
        run.posts().len(),
        0,
        "silence declines to post rather than redirecting"
    );
    assert_eq!(
        silent
            .pending_relay_outbound_envelopes(1_000, now, Vec::new())
            .expect("pending outbound")
            .len(),
        2,
        "no terminal marker was written: the rows are still deliverable, by relay or by mesh"
    );
    assert!(
        run.requests
            .iter()
            .all(|request| request.request.base_url == OWN_URL),
        "a resting endpoint is not polled either"
    );

    assert_no_violation_of(&silent, &["SILENCE-01"]);
}

// ===========================================================================
// The skip list — one dead recipient cannot own the batch
// ===========================================================================

#[test]
fn an_unpostable_recipient_does_not_spend_the_upload_batch() {
    // The upload queries take an exclusion list precisely so a recipient this
    // device cannot post to is never *selected*. Passing an empty list
    // instead means their rows fill the bounded batch, get skipped one by
    // one, and fill it again next pass — and the rows behind them that could
    // actually move never do. The batch is bounded, so this is starvation,
    // not merely waste.
    let store = new_store();
    let now = T0;
    let budget = core_relay_pass_default_budgets().max_authored_uploads as usize;
    let dead = vec![0xD0u8; 32];
    let live = contact_user_id();
    seed_contact(&store);
    seed_authored_to(&store, &dead, 0x5000, 30, now);
    seed_authored_to(&store, &live, 0x6000, 30, now);

    let mut plan = base_plan(now);
    plan.contacts = vec![
        CoreRelayContactConfig {
            user_id: dead.clone(),
            relay_url: Some("https://gone.example".to_string()),
            relay_token: Some("member-token-cccccccccccc".to_string()),
            endpoint_usable: true,
            // Resting for silence: nowhere to post, and no fallback.
            endpoint_answering: false,
        },
        CoreRelayContactConfig {
            user_id: live.clone(),
            relay_url: Some(CONTACT_URL.to_string()),
            relay_token: Some(CONTACT_TOKEN.to_string()),
            endpoint_usable: true,
            endpoint_answering: true,
        },
    ];

    let mut posted = 0usize;
    for (index, pass_id) in ["p1", "p2"].iter().enumerate() {
        let pass = CoreRelayPass::new(store.clone(), plan.clone(), pass_id.to_string());
        let run = drive(&pass, now + index as i64 * 60_000, |request, _index| {
            if request.is_fetch() {
                Reply::ok(empty_page())
            } else {
                Reply::empty_ok()
            }
        });
        assert!(
            run.posts()
                .iter()
                .all(|post| post.request.base_url == CONTACT_URL),
            "every upload this pass spent must be one that could actually move"
        );
        posted += run.posts().len();
    }
    assert_eq!(
        posted, 30,
        "the live recipient's whole queue moved in two passes of {budget}; a dead recipient \
         occupying the batch would have left rows waiting"
    );

    let remaining = store
        .pending_relay_outbound_envelopes(1_000, now, Vec::new())
        .expect("pending outbound");
    assert_eq!(
        remaining.len(),
        30,
        "only the unpostable recipient's rows are still queued"
    );
    assert!(
        remaining
            .iter()
            .all(|envelope| envelope.recipient_user_id == dead),
        "the live recipient was not starved by the dead one"
    );
}

// ===========================================================================
// Seeding helpers
// ===========================================================================

/// `create_group` requires 16-byte member ids, so these are that length
/// rather than the 32-byte stand-ins the rest of this file uses.
fn member_user_id(index: usize) -> Vec<u8> {
    vec![0xB0 + index as u8; 16]
}

/// A group of `members` contacts, each with their own card endpoint on the
/// contact relay, persisted as contacts and as a group.
fn seed_group(store: &MessageStore, members: usize) -> cruisemesh_core::Group {
    let member_ids: Vec<Vec<u8>> = (0..members).map(member_user_id).collect();
    for (index, user_id) in member_ids.iter().enumerate() {
        store
            .upsert_contact(Contact {
                user_id: user_id.clone(),
                name: format!("Member {index}"),
                sign_pk: vec![1u8; 32],
                agree_pk: vec![2u8; 32],
                relay_url: Some(CONTACT_URL.to_string()),
                relay_token: Some(CONTACT_TOKEN.to_string()),
                nickname: None,
            })
            .expect("upsert member");
    }
    let group = cruisemesh_core::create_group("Cabin".to_string(), member_ids).expect("group");
    store.upsert_group(group.clone()).expect("upsert group");
    group
}

fn group_contacts(group: &cruisemesh_core::Group) -> Vec<CoreRelayContactConfig> {
    group
        .member_user_ids
        .iter()
        .map(|user_id| CoreRelayContactConfig {
            user_id: user_id.clone(),
            relay_url: Some(CONTACT_URL.to_string()),
            relay_token: Some(CONTACT_TOKEN.to_string()),
            endpoint_usable: true,
            endpoint_answering: true,
        })
        .collect()
}

/// One authored group-addressed envelope: `recipient_user_id` is the group id,
/// which is what makes it nobody's contact and everybody's message.
fn seed_group_authored(store: &MessageStore, group: &cruisemesh_core::Group, now_ms: i64) {
    let expiry = now_ms + 6 * 24 * 60 * 60 * 1000;
    let chat_id = group.id.clone();
    store
        .insert_outgoing_message(
            StoredMessage {
                chat_id: chat_id.clone(),
                sender_user_id: own_user_id(),
                lamport: 1,
                timestamp: now_ms,
                kind: KIND_TEXT,
                payload: b"cabin at seven".to_vec(),
            },
            OutboundEnvelope {
                msg_id: msg_id(0x4000),
                recipient_user_id: group.id.clone(),
                chat_id,
                sender_user_id: own_user_id(),
                kind: KIND_TEXT,
                lamport: 1,
                timestamp: now_ms,
                hop_ttl: 3,
                expiry,
                recipient_hint: compute_recipient_hint(group.id.clone(), now_ms),
                sealed: vec![0x55u8; 96],
            },
            now_ms,
        )
        .expect("queue group envelope");
}

/// `count` authored 1:1 envelopes addressed to `recipient`.
fn seed_authored_to(
    store: &MessageStore,
    recipient: &[u8],
    seed_base: u64,
    count: usize,
    now_ms: i64,
) {
    let expiry = now_ms + 6 * 24 * 60 * 60 * 1000;
    let chat_id = recipient.to_vec();
    for index in 0..count {
        let lamport = index as u64 + 1;
        store
            .insert_outgoing_message(
                StoredMessage {
                    chat_id: chat_id.clone(),
                    sender_user_id: own_user_id(),
                    lamport,
                    timestamp: now_ms,
                    kind: KIND_TEXT,
                    payload: b"hello".to_vec(),
                },
                OutboundEnvelope {
                    msg_id: msg_id(seed_base + index as u64),
                    recipient_user_id: recipient.to_vec(),
                    chat_id: chat_id.clone(),
                    sender_user_id: own_user_id(),
                    kind: KIND_TEXT,
                    lamport,
                    timestamp: now_ms,
                    hop_ttl: 3,
                    expiry,
                    recipient_hint: compute_recipient_hint(recipient.to_vec(), now_ms),
                    sealed: vec![0x33u8; 80],
                },
                now_ms,
            )
            .expect("queue authored");
    }
}

fn seed_contact(store: &MessageStore) {
    store
        .upsert_contact(Contact {
            user_id: contact_user_id(),
            name: "Contact".to_string(),
            sign_pk: vec![1u8; 32],
            agree_pk: vec![2u8; 32],
            relay_url: Some(CONTACT_URL.to_string()),
            relay_token: Some(CONTACT_TOKEN.to_string()),
            nickname: None,
        })
        .expect("upsert contact");
}

fn seed_receipts(store: &MessageStore, count: usize, now_ms: i64) {
    let expiry = now_ms + 6 * 24 * 60 * 60 * 1000;
    for index in 0..count {
        store
            .upsert_outgoing_receipt_envelope(
                OutgoingReceiptEnvelope {
                    msg_id: msg_id(0x1000 + index as u64),
                    recipient_user_id: contact_user_id(),
                    chat_id: vec![index as u8 + 1; 32],
                    sender_user_id: vec![7u8; 32],
                    receipt_type: RECEIPT_TYPE_DELIVERED,
                    through_lamport: 5,
                    timestamp: now_ms,
                    hop_ttl: 3,
                    expiry,
                    recipient_hint: compute_recipient_hint(contact_user_id(), now_ms),
                    sealed: vec![0x22u8; 64],
                },
                now_ms,
            )
            .expect("queue receipt");
    }
}

fn seed_authored(store: &MessageStore, count: usize, now_ms: i64) {
    let expiry = now_ms + 6 * 24 * 60 * 60 * 1000;
    let chat_id = vec![3u8; 32];
    for index in 0..count {
        let lamport = index as u64 + 1;
        store
            .insert_outgoing_message(
                StoredMessage {
                    chat_id: chat_id.clone(),
                    sender_user_id: own_user_id(),
                    lamport,
                    timestamp: now_ms,
                    kind: KIND_TEXT,
                    payload: b"hello".to_vec(),
                },
                OutboundEnvelope {
                    msg_id: msg_id(0x2000 + index as u64),
                    recipient_user_id: contact_user_id(),
                    chat_id: chat_id.clone(),
                    sender_user_id: own_user_id(),
                    kind: KIND_TEXT,
                    lamport,
                    timestamp: now_ms,
                    hop_ttl: 3,
                    expiry,
                    recipient_hint: compute_recipient_hint(contact_user_id(), now_ms),
                    sealed: vec![0x33u8; 80],
                },
                now_ms,
            )
            .expect("queue authored");
    }
}

fn seed_carried(store: &MessageStore, count: usize, now_ms: i64) {
    let expiry = now_ms + 6 * 24 * 60 * 60 * 1000;
    for index in 0..count {
        store
            .enqueue_carried_envelope(
                CarriedEnvelope {
                    msg_id: msg_id(0x3000 + index as u64),
                    hop_ttl: 3,
                    expiry,
                    recipient_hint: compute_recipient_hint(contact_user_id(), now_ms),
                    // Distinct payloads on purpose: the carry queue dedupes
                    // identical (hint, sealed) pairs, so N copies of one byte
                    // pattern is one row, not N.
                    sealed: {
                        let mut sealed = vec![0x44u8; 128];
                        sealed[..8].copy_from_slice(&(index as u64).to_be_bytes());
                        sealed
                    },
                },
                true,
                now_ms,
                1_000_000,
            )
            .expect("enqueue carried");
    }
}

// ===========================================================================
// The unused-import guard
// ===========================================================================

/// Keeps [`CoreRelayAction`] named in this file: the type is the seam's
/// public shape, and a test file that drives the session without ever naming
/// its action type would not notice the type being renamed out from under the
/// adapters that will consume it.
#[test]
fn an_action_carries_its_pass_and_action_ids() {
    let store = new_store();
    let pass = CoreRelayPass::new(store, base_plan(T0), "p1".to_string());
    let action: CoreRelayAction = pass.start(T0);
    assert!(
        action.pass_id.starts_with("p1-"),
        "the label the caller asked for is the root of the derived id, got {}",
        action.pass_id
    );
    assert!(
        action.action_id >= 1 || matches!(action.kind, CoreRelayActionKind::Finished { .. }),
        "an emitted action's id starts at 1"
    );
}

// ===========================================================================
// Adversarial coverage
// ===========================================================================
//
// The fixtures above are the incidents that happened. This section is the
// ones that have not: duplicated, late, out-of-order and replayed results;
// a result that arrives before the pass began; cancellation; a restart
// between a page consume and its ack; a clock that runs backwards; a relay
// that never stops answering; budgets small enough to bite; and bodies at and
// over the declared cap. Each asserts end state, termination, work counts,
// progress and a secret-free transcript rather than only "it did not crash".

/// How a driver can lie about which action it is answering.
///
/// Every one of these is a thing a real driver does: a retry that answers
/// twice, a socket that completes after the pass moved on, a queue that
/// reorders, a stale continuation from the pass before. `IDEMP-01` says all
/// four change nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Perturbation {
    /// The same answer, twice.
    Duplicate,
    /// An answer to an action that has not been emitted yet.
    FutureActionId,
    /// An answer to an action from earlier in this pass.
    StaleActionId,
    /// An answer belonging to another pass, whose action id is stale here too.
    WrongPass,
    /// An answer belonging to another pass, carrying the action id that *is*
    /// outstanding right now.
    ///
    /// This is the case the wrong-pass half of the guard exists for, and the
    /// only permutation the action-id comparison cannot decide by itself.
    /// Action ids restart at 1 in every pass, so a late answer from the pass
    /// before collides with the action this one is waiting for by default
    /// rather than by coincidence. Without the pass-id comparison, a fetch's
    /// `200` from a dead pass lands on an upload this pass has outstanding
    /// and marks a message posted that was never sent.
    WrongPassOutstandingAction,
}

impl Perturbation {
    const ALL: &'static [Perturbation] = &[
        Perturbation::Duplicate,
        Perturbation::FutureActionId,
        Perturbation::StaleActionId,
        Perturbation::WrongPass,
        Perturbation::WrongPassOutstandingAction,
    ];

    /// `outstanding` is the action the pass is waiting for *after* the honest
    /// result was applied — what the last permutation impersonates.
    fn corrupt(
        self,
        mut result: CoreRelayHttpResult,
        outstanding: Option<u64>,
    ) -> CoreRelayHttpResult {
        match self {
            Perturbation::Duplicate => result,
            Perturbation::FutureActionId => {
                result.action_id = result.action_id.saturating_add(7);
                result
            }
            Perturbation::StaleActionId => {
                result.action_id = result.action_id.saturating_sub(1);
                result
            }
            Perturbation::WrongPass => {
                result.pass_id = "pz".to_string();
                result
            }
            Perturbation::WrongPassOutstandingAction => {
                result.pass_id = "pz".to_string();
                if let Some(action_id) = outstanding {
                    result.action_id = action_id;
                }
                result
            }
        }
    }
}

/// A clean run of the standard one-page scenario, for a perturbed run to be
/// compared against exactly.
fn one_page_scenario(store: &Arc<MessageStore>, pass_id: &str, now: i64) -> Run {
    let rows = seed_consumed(store, 1..21, now);
    let pass = CoreRelayPass::new(store.clone(), base_plan(now), pass_id.to_string());
    drive(&pass, now, move |request, index| {
        if request.is_ack() {
            return Reply::empty_ok();
        }
        match index {
            0 => Reply::ok(page_body(&rows)),
            _ => Reply::ok(empty_page()),
        }
    })
}

#[test]
fn every_result_permutation_is_inert_and_leaves_the_same_store() {
    let clean_store = new_store();
    let clean = one_page_scenario(&clean_store, "p1", T0);
    let clean_cursor = clean_store
        .relay_fetch_cursor(own_cursor_key())
        .expect("cursor")
        .after_id;
    let clean_carry = clean_store.carried_len().expect("carry depth");
    assert_eq!(clean.summary.stale_results_ignored, 0);

    for perturbation in Perturbation::ALL {
        let store = new_store();
        let now = T0;
        let rows = seed_consumed(&store, 1..21, now);
        let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());

        let mut clock = now;
        let mut injected = 0u32;
        let mut action = pass.start(now);
        let mut sent = 0usize;

        loop {
            let request = match &action.kind {
                CoreRelayActionKind::Finished { .. } => break,
                CoreRelayActionKind::Sleep { .. } => break,
                CoreRelayActionKind::NotStarted => {
                    panic!("this pass was started, so it can never be unstarted")
                }
                CoreRelayActionKind::Http { request } => request.clone(),
            };
            let recorded = Recorded {
                action_id: action.action_id,
                request,
            };
            let reply = if recorded.is_ack() {
                Reply::empty_ok()
            } else if sent == 0 {
                Reply::ok(page_body(&rows))
            } else {
                Reply::ok(empty_page())
            };
            sent += 1;
            clock = clock.saturating_add(reply.elapsed_ms);
            let good = CoreRelayHttpResult {
                pass_id: action.pass_id.clone(),
                action_id: action.action_id,
                status: reply.status,
                headers: Vec::new(),
                body: reply.body.clone(),
                error: None,
                completed_at_ms: clock,
            };

            // The truth once...
            let next = pass.resume_http(good.clone());

            // ...then the lie. Every perturbation is stale by now: a
            // duplicate of a result already applied, an id from before or
            // after the one outstanding, or another pass's answer — including
            // one carrying the very action id this pass is now waiting for,
            // which only the pass id can reject.
            let outstanding = match &next.kind {
                CoreRelayActionKind::Http { .. } => Some(next.action_id),
                _ => None,
            };
            let echoed = pass.resume_http(perturbation.corrupt(good, outstanding));
            injected += 1;
            match &next.kind {
                CoreRelayActionKind::Http { .. } => {
                    assert_eq!(
                        echoed.action_id, next.action_id,
                        "{perturbation:?}: a stale result must restate the action that is \
                         actually outstanding, not emit a new one"
                    );
                    assert!(
                        matches!(echoed.kind, CoreRelayActionKind::Http { .. }),
                        "{perturbation:?}: the pass must still be waiting for the same request"
                    );
                }
                CoreRelayActionKind::Finished { .. } | CoreRelayActionKind::Sleep { .. } => {
                    assert!(
                        matches!(echoed.kind, CoreRelayActionKind::Finished { .. }),
                        "{perturbation:?}: a finished pass answers with its summary"
                    );
                }
                CoreRelayActionKind::NotStarted => {
                    panic!("{perturbation:?}: a started pass can never be unstarted")
                }
            }

            action = next;
        }

        let summary = pass.summary().expect("the pass finished");
        assert_eq!(
            summary.stale_results_ignored, injected,
            "{perturbation:?}: every lie must be counted"
        );
        assert_eq!(
            summary.rows_acked, clean.summary.rows_acked,
            "{perturbation:?}: IDEMP-01 -- the work done must equal the clean run's"
        );
        assert_eq!(
            summary.rows_ingested, clean.summary.rows_ingested,
            "{perturbation:?}: a replayed result must not double-apply an ingest"
        );
        assert_eq!(
            store
                .relay_fetch_cursor(own_cursor_key())
                .expect("cursor")
                .after_id,
            clean_cursor,
            "{perturbation:?}: IDEMP-01 -- a cursor must not move further, or backwards, for a \
             result nobody was waiting for"
        );
        assert_eq!(
            store.carried_len().expect("carry depth"),
            clean_carry,
            "{perturbation:?}: IDEMP-01 -- a carried row must not be consumed by a stale result"
        );
        assert_no_violation_of(&store, &["IDEMP-01"]);
        assert_no_secrets(&store, &summary);
    }
}

#[test]
fn a_result_arriving_after_the_pass_finished_changes_nothing() {
    let store = new_store();
    let now = T0;
    let run = one_page_scenario(&store, "p1", now);
    let cursor = store
        .relay_fetch_cursor(own_cursor_key())
        .expect("cursor")
        .after_id;

    // A second pass, cancelled with an action still outstanding. The driver's
    // socket completes anyway, minutes later.
    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p2".to_string());
    let action = pass.start(now);
    let summary = pass.cancel(now + 10);
    assert_eq!(summary.outcome, CoreRelayPassOutcome::Cancelled);

    let late = pass.resume_http(CoreRelayHttpResult {
        pass_id: action.pass_id.clone(),
        action_id: action.action_id,
        status: 200,
        headers: Vec::new(),
        body: empty_page(),
        error: None,
        completed_at_ms: now + 5_000,
    });
    match late.kind {
        CoreRelayActionKind::Finished { summary } => {
            assert_eq!(summary.outcome, CoreRelayPassOutcome::Cancelled);
            assert!(
                summary.stale_results_ignored >= 1,
                "IDEMP-01: a late result must be recorded as ignored, not silently dropped"
            );
        }
        other => panic!("a finished pass must answer with its summary, got {other:?}"),
    }
    assert_eq!(
        store
            .relay_fetch_cursor(own_cursor_key())
            .expect("cursor")
            .after_id,
        cursor,
        "IDEMP-01: nothing a finished pass is told may reach the store"
    );

    // Cancelling twice is the same answer, not a second one.
    assert_eq!(
        pass.cancel(now + 60_000).outcome,
        CoreRelayPassOutcome::Cancelled
    );
    assert_no_violation_of(&store, &["IDEMP-01", "TXN-01"]);
    assert_no_secrets(&store, &run.summary);
}

#[test]
fn a_restart_between_the_page_consume_and_its_ack_replays_safely() {
    // TXN-01's fault injection: the page's ingest transaction commits, then
    // the process dies before the ack is answered. The next launch must find
    // the frontier where it was, re-ingest the same page as nothing new, and
    // finish the job.
    let store = new_store();
    let now = T0;
    let rows = seed_consumed(&store, 1..31, now);

    let killed = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let mut action = killed.start(now);
    let mut clock = now;
    loop {
        let CoreRelayActionKind::Http { request } = &action.kind else {
            panic!("the pass finished before the ack was outstanding");
        };
        if request.operation == CoreRelayOperation::AckPage {
            break;
        }
        clock += 40;
        action = killed.resume_http(CoreRelayHttpResult {
            pass_id: action.pass_id.clone(),
            action_id: action.action_id,
            status: 200,
            headers: Vec::new(),
            body: page_body(&rows),
            error: None,
            completed_at_ms: clock,
        });
    }
    // The crash. Nothing answers; the object is simply dropped.
    let carried_at_death = store.carried_len().expect("carry depth");
    let cursor_at_death = store.relay_fetch_cursor(own_cursor_key()).expect("cursor");
    assert_eq!(
        cursor_at_death.after_id, 0,
        "TXN-01: the frontier transaction had not run, so the frontier is where it was"
    );
    drop(killed);

    let relaunch = CoreRelayPass::new(store.clone(), base_plan(now + 30_000), "p2".to_string());
    let run = drive(&relaunch, now + 30_000, |request, index| {
        if request.is_ack() {
            return Reply::empty_ok();
        }
        match index {
            0 => Reply::ok(page_body(&rows)),
            _ => Reply::ok(empty_page()),
        }
    });
    assert_eq!(
        run.summary.rows_ingested, 0,
        "TXN-01/IDEMP-01: the re-presented page persists nothing new"
    );
    assert_eq!(
        store.carried_len().expect("carry depth"),
        carried_at_death,
        "a replay must not duplicate a carried row"
    );
    assert_eq!(run.summary.rows_acked, 30);
    assert_eq!(
        store
            .relay_fetch_cursor(own_cursor_key())
            .expect("cursor")
            .after_id,
        30,
        "CURSOR-01: the frontier moves once, on the pass whose ack succeeded"
    );
    assert_no_violation_of(&store, &["TXN-01", "IDEMP-01", "CURSOR-01"]);
    assert_no_secrets(&store, &run.summary);
}

#[test]
fn no_relay_can_make_a_pass_run_forever() {
    // LIVE-01 as a property rather than an example. Four relays a correct
    // client must survive: one that never runs out of new mail, one that
    // answers every page with a cursor that does not move, one that never
    // answers at all, and one that rejects everything.
    let budgets = core_relay_pass_default_budgets();
    let now = T0;

    let endless = new_store();
    let pass = CoreRelayPass::new(endless.clone(), base_plan(now), "p1".to_string());
    let run = drive(&pass, now, |_request, index| {
        Reply::ok(page_body(&fresh_rows(1 + index as i64 * 64, 64, now)))
    });
    assert!(run.summary.requests_issued <= budgets.max_requests);
    assert!(run.summary.envelopes_processed <= budgets.max_envelopes);
    assert!(run.summary.response_bytes_read <= budgets.max_response_bytes);

    let stuck = new_store();
    let pass = CoreRelayPass::new(stuck.clone(), base_plan(now), "p1".to_string());
    let mut stuck_requests = 0u32;
    let run = drive(&pass, now, |_request, _index| {
        // A page with rows in it whose `next_cursor` does not move past the
        // `after` that asked for them. relayd cannot produce this; a broken
        // or hostile server can, and a client that re-asked from the same
        // cursor would fetch the same row until the battery died. An empty
        // page is a different branch -- that one is EOF -- so this scenario
        // has to return something to be the scenario it claims to be.
        stuck_requests += 1;
        let mut rows = fresh_rows(1, 1, now);
        rows[0].id = 0;
        Reply::ok(page_body(&rows))
    });
    assert!(run.summary.requests_issued <= budgets.max_requests);
    assert_eq!(
        stuck_requests, 1,
        "PAGE-01: a page that did not move the cursor ends the walk rather than being re-asked"
    );
    assert_eq!(run.summary.frontier_advances, 0);
    assert!(run.summary.continuation.is_none());

    let silent = new_store();
    let pass = CoreRelayPass::new(silent.clone(), base_plan(now), "p1".to_string());
    let run = drive(&pass, now, |_request, _index| {
        Reply::transport(cruisemesh_core::CoreRelayTransportError::Timeout)
    });
    assert!(run.summary.requests_issued <= budgets.max_requests);

    let hostile = new_store();
    seed_carried(&hostile, 6, now);
    let pass = CoreRelayPass::new(hostile.clone(), base_plan(now), "p1".to_string());
    let run = drive(&pass, now, |_request, _index| Reply::status(500, "boom"));
    assert!(run.summary.requests_issued <= budgets.max_requests);

    for store in [&endless, &stuck, &silent, &hostile] {
        assert_no_violation_of(store, &["LIVE-01", "PROGRESS-01"]);
    }
}

#[test]
fn every_continuation_buys_a_cursor_or_a_later_deadline() {
    // PROGRESS-01. Rather than assert this scenario by scenario, gather the
    // continuations several different passes produce and check the rule holds
    // across all of them: an unchanged-state reschedule is what emptied
    // batteries before #270, and it must be unreachable, not merely rare.
    let now = T0;

    // 1. A deep mailbox that yields on its walk budget.
    let deep = new_store();
    let mut sweeping = base_plan(now);
    sweeping.swept_this_session = false;
    let pass = CoreRelayPass::new(deep.clone(), sweeping, "p1".to_string());
    let before = deep
        .relay_fetch_cursor(own_cursor_key())
        .expect("cursor")
        .sweep_after_id;
    let yielded = drive(&pass, now, |_request, index| {
        Reply::ok(page_body(&fresh_rows(1 + index as i64 * 256, 256, now)))
    });
    let after = deep
        .relay_fetch_cursor(own_cursor_key())
        .expect("cursor")
        .sweep_after_id;

    // 2. A pass with nothing to do at all.
    let idle = new_store();
    let pass = CoreRelayPass::new(idle.clone(), base_plan(now), "p2".to_string());
    let quiet = drive(&pass, now, |_request, _index| Reply::ok(empty_page()));

    // 3. A pass refused by a rate limit.
    let limited = new_store();
    seed_carried(&limited, 3, now);
    let pass = CoreRelayPass::new(limited.clone(), base_plan(now), "p3".to_string());
    let refused = drive(&pass, now, |_request, _index| Reply::rate_limited(20));

    for (label, summary, cursor_moved) in [
        ("walk yield", &yielded.summary, after > before),
        ("idle", &quiet.summary, false),
        ("rate limited", &refused.summary, false),
    ] {
        let Some(continuation) = summary.continuation else {
            continue;
        };
        let advanced = cursor_moved
            || summary.rows_ingested > 0
            || summary.carried_rows_marked > 0
            || summary.frontier_advances > 0;
        let later_deadline = continuation.reason == CoreRelayProgressReason::QuietWindowExtended
            && continuation.not_before_ms > summary.finished_at_ms;
        assert!(
            advanced || later_deadline,
            "PROGRESS-01: {label} scheduled a continuation that bought nothing: {continuation:?}"
        );
        assert!(
            continuation.not_before_ms > summary.finished_at_ms,
            "PROGRESS-01: {label} scheduled a continuation in the past"
        );
    }
    assert!(
        quiet.summary.continuation.is_none(),
        "PROGRESS-01: a pass that did nothing must schedule nothing"
    );
}

#[test]
fn a_clock_that_runs_backwards_cannot_rewind_a_deadline_or_a_window() {
    let store = new_store();
    let now = T0;
    seed_carried(&store, 3, now);

    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let mut action = pass.start(now);
    let mut answered = 0i64;
    let summary = loop {
        match action.kind {
            CoreRelayActionKind::Finished { summary } => break summary,
            CoreRelayActionKind::Sleep { .. } => break pass.summary().expect("summary"),
            CoreRelayActionKind::NotStarted => panic!("this pass was started"),
            CoreRelayActionKind::Http { .. } => {
                answered += 1;
                // A driver on a phone whose wall clock stepped back a day
                // mid-request: every other answer claims to have completed
                // before the pass began.
                let completed_at_ms = if answered % 2 == 0 {
                    now - 86_400_000
                } else {
                    now + answered * 10
                };
                action = pass.resume_http(CoreRelayHttpResult {
                    pass_id: action.pass_id.clone(),
                    action_id: action.action_id,
                    status: 200,
                    headers: Vec::new(),
                    body: empty_page(),
                    error: None,
                    completed_at_ms,
                });
            }
        }
        assert!(
            answered < 500,
            "LIVE-01: a rewound clock must not extend a pass"
        );
    };
    assert!(
        summary.finished_at_ms >= summary.started_at_ms,
        "a pass cannot finish before it started: {summary:?}"
    );

    // And the quiet window a 429 records is a floor. Repeated refusals widen
    // it; a later, earlier clock reading cannot lower it.
    let rewound = new_store();
    seed_carried(&rewound, 2, now);
    let mut plan = base_plan(now);
    plan.consecutive_rate_limits = 3;
    let pass = CoreRelayPass::new(rewound.clone(), plan, "p2".to_string());
    let run = drive(&pass, now, |_request, _index| Reply::rate_limited(60));
    assert!(
        run.summary.quiet_until_ms >= now + 60_000,
        "Retry-After is a floor and repeated refusals widen it: {}",
        run.summary.quiet_until_ms
    );
    assert_no_secrets(&rewound, &run.summary);
}

#[test]
fn a_body_at_or_over_the_declared_cap_is_handled_rather_than_decoded() {
    let now = T0;
    let cap = cruisemesh_core::relay_max_response_bytes();

    // Every request declares the cap, so a driver can enforce it without
    // knowing anything about relays.
    let store = new_store();
    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        assert_eq!(
            request.request.max_response_bytes, cap,
            "every request must declare what the driver may accumulate"
        );
        Reply::ok(empty_page())
    });
    assert_eq!(run.summary.outcome, CoreRelayPassOutcome::Completed);

    // A page whose row count is past what the decoder accepts is a malformed
    // answer, not a paging problem: the walk terminates rather than advancing
    // over rows it never read.
    let overlong = new_store();
    let pass = CoreRelayPass::new(overlong.clone(), base_plan(now), "p1".to_string());
    let run = drive(&pass, now, |_request, _index| {
        Reply::ok(page_body(&fresh_rows(1, 300, now)))
    });
    assert_eq!(
        run.summary.frontier_advances, 0,
        "PAGE-01: a page that would not decode must not move a frontier"
    );
    assert_eq!(
        overlong
            .relay_fetch_cursor(own_cursor_key())
            .expect("cursor")
            .after_id,
        0
    );

    // A one-row page that still exceeds the cap has nothing left to shrink.
    // The walk stops instead of halving forever.
    let stuck = new_store();
    let pass = CoreRelayPass::new(stuck.clone(), base_plan(now), "p1".to_string());
    let run = drive(&pass, now, |_request, _index| Reply::oversize());
    let limits: Vec<Option<u32>> = run.fetches().iter().map(|fetch| fetch.limit()).collect();
    assert_eq!(
        limits,
        vec![
            Some(256),
            Some(128),
            Some(64),
            Some(32),
            Some(16),
            Some(8),
            Some(4),
            Some(2),
            Some(1)
        ],
        "PAGE-01: halving reaches one row in eight steps and then stops"
    );
    assert!(run.summary.requests_issued <= core_relay_pass_default_budgets().max_requests);
    assert_no_violation_of(&stuck, &["PAGE-01", "LIVE-01"]);
}

#[test]
fn the_token_crosses_the_driver_seam_and_goes_nowhere_else() {
    // SECRET-01 extended to this package: a request cannot authenticate
    // without the credential, so it rides the Authorization header -- and
    // that is the only place in the world it appears.
    let store = new_store();
    let now = T0;
    seed_carried(&store, 2, now);
    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        let authorization = request
            .request
            .headers
            .iter()
            .find(|header| header.name == "Authorization")
            .expect("every relay request is authenticated");
        assert_eq!(authorization.value, format!("Bearer {OWN_TOKEN}"));
        // And nowhere else in the request either: not the path, not the body.
        assert!(!request.request.path.contains(OWN_TOKEN));
        assert!(!String::from_utf8_lossy(&request.request.body).contains(OWN_TOKEN));
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert!(
        run.summary.requests_issued > 0,
        "the scenario must issue work"
    );

    let events = store.export_protocol_events_jsonl().expect("export");
    for canary in [
        OWN_TOKEN,
        OWN_URL,
        "relay.example",
        "Bearer",
        "Authorization",
    ] {
        assert!(
            !events.contains(canary),
            "SECRET-01: the protocol-event ring leaked {canary}"
        );
    }
    assert_no_secrets(&store, &run.summary);

    // The mailbox is still named in events, by an archive-local pseudonym
    // derived from the credential-free cursor key, so a transcript can be
    // read one mailbox at a time.
    let parsed = transcript(&store);
    assert!(
        parsed.iter().any(|event| event
            .get("actor")
            .and_then(|actor| actor.as_str())
            .is_some_and(|actor| actor.starts_with("mailbox-"))),
        "a transcript must still name which mailbox it is talking about"
    );
}

#[test]
fn a_summary_says_where_an_interrupted_pass_stopped() {
    // `stage_reached` is the difference between "the pass ran and found
    // nothing" and "the pass never got as far as looking". A support reader
    // cannot tell those apart from work counts alone, because both are zero.
    let now = T0;

    let completed = new_store();
    let pass = CoreRelayPass::new(completed.clone(), base_plan(now), "p1".to_string());
    let run = drive(&pass, now, |_request, _index| Reply::ok(empty_page()));
    assert_eq!(
        run.summary.stage_reached,
        cruisemesh_core::CoreRelayStage::Finish
    );

    // A 429 during receipt upload stops the pass three stages before the
    // walks, and the summary has to say so.
    let limited = new_store();
    seed_receipts(&limited, 3, now);
    let pass = CoreRelayPass::new(limited.clone(), base_plan(now), "p2".to_string());
    let run = drive(&pass, now, |_request, _index| Reply::rate_limited(10));
    assert_eq!(run.summary.outcome, CoreRelayPassOutcome::RateLimited);
    assert_eq!(
        run.summary.stage_reached,
        cruisemesh_core::CoreRelayStage::UploadReceipts,
        "a rate-limited pass must report the stage it never got past"
    );

    // And a cancellation names where the driver was when it was pulled.
    let cancelled = new_store();
    let pass = CoreRelayPass::new(cancelled.clone(), base_plan(now), "p3".to_string());
    let _ = pass.start(now);
    let summary = pass.cancel(now + 50);
    assert_eq!(summary.outcome, CoreRelayPassOutcome::Cancelled);
    assert_eq!(
        summary.stage_reached,
        cruisemesh_core::CoreRelayStage::MailboxWalk
    );
}

#[test]
fn a_written_off_contact_endpoint_is_neither_polled_nor_posted_to() {
    // A card whose endpoint has already been written off must not keep
    // winning the send path: `resolved_contact_relay` returns it
    // unconditionally, so one dead field would beat a working alternative
    // forever and the queue would never drain. Skipping it falls through to
    // our own mailbox, which is what a card with no relay fields already
    // does.
    let store = new_store();
    let now = T0;
    seed_contact(&store);
    seed_authored(&store, 2, now);

    let mut plan = base_plan(now);
    plan.contacts = vec![CoreRelayContactConfig {
        user_id: contact_user_id(),
        relay_url: Some(CONTACT_URL.to_string()),
        relay_token: Some(CONTACT_TOKEN.to_string()),
        endpoint_usable: false,
        // Rejection, not silence: the endpoint answered, it just answered
        // that it will not serve us.
        endpoint_answering: true,
    }];

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        assert_ne!(
            request.request.base_url, CONTACT_URL,
            "a written-off endpoint must not be reached at all"
        );
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert_eq!(
        run.summary.authored_uploads, 2,
        "the rows still go out — to our own mailbox, which is where a card \
         with no relay fields would have sent them"
    );
    assert!(
        run.posts()
            .iter()
            .all(|post| post.request.base_url == OWN_URL),
        "every post must have fallen back to our own mailbox"
    );
    assert_eq!(
        run.summary.silence_committed + run.summary.silence_discarded,
        0,
        "an endpoint nobody polled is not silent, and must not accrue a streak"
    );
}

#[test]
fn a_page_of_rows_this_device_cannot_take_is_still_terminal_and_still_unacked() {
    // Two rows a relay can legitimately serve that this build will not
    // store: one whose public header it rejects outright, and one whose
    // (hint, sealed) pair the carry queue already holds under a different
    // msg_id. Neither may be acked -- ACK-01 -- and both must be terminal,
    // because a frontier held on a header that will never become acceptable
    // strands every row above it on every ordinary pass.
    let store = new_store();
    let now = T0;
    let hint = own_hint(now);
    let expiry = now + 6 * 24 * 60 * 60 * 1000;

    // The duplicate's twin, already carried under its own msg_id.
    store
        .enqueue_carried_envelope(
            CarriedEnvelope {
                msg_id: msg_id(0x7001),
                hop_ttl: 3,
                expiry,
                recipient_hint: hint.clone(),
                sealed: {
                    let mut sealed = vec![0x11u8; 96];
                    sealed[..8].copy_from_slice(&2u64.to_be_bytes());
                    sealed
                },
            },
            true,
            now,
            1_000_000,
        )
        .expect("enqueue the twin");
    let carried_before = store.carried_len().expect("carry depth");

    // Row 1: a hop_ttl no envelope may carry. Row 2: the duplicate content
    // under a fresh id. Row 3: an ordinary new row, so the page is not
    // entirely unusable.
    let mut rows = fresh_rows(1, 3, now);
    rows[0].expiry_ms = now + 400 * 24 * 60 * 60 * 1000; // far past the carry window
    rows[1].msg_id = msg_id(0x7002);
    let body = page_body_with_hop_ttls(&rows, &[3, 3, 3]);

    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let mut fetches = 0;
    let run = drive(&pass, now, |request, _index| {
        if request.is_ack() {
            panic!("ACK-01: none of these rows is ack-eligible");
        }
        if !request.is_fetch() {
            // The twin's own carried upload, which this scenario does not
            // care about beyond it not interfering.
            return Reply::empty_ok();
        }
        fetches += 1;
        if fetches == 1 {
            Reply::ok(body.clone())
        } else {
            Reply::ok(empty_page())
        }
    });

    assert_eq!(run.acks().len(), 0, "ACK-01: nothing here earned an ack");
    assert_eq!(run.summary.rows_acked, 0);
    assert_eq!(
        run.summary.frontier_advances, 1,
        "the page is terminal: the frontier moves past rows that will never \
         become storable, or the mailbox above them is unreachable forever"
    );
    assert_eq!(
        store
            .relay_fetch_cursor(own_cursor_key())
            .expect("cursor")
            .after_id,
        3
    );
    assert_eq!(
        store.carried_len().expect("carry depth"),
        carried_before + 1,
        "only the ordinary row was stored: the rejected header was not, and the \
         duplicate content was refused by the carry queue's own unique index"
    );

    assert_no_violation_of(&store, &["PAGE-01", "CURSOR-01", "ACK-01"]);
    assert_no_secrets(&store, &run.summary);
}

/// [`page_body`] with an explicit `hop_ttl` per row, for the one scenario
/// that needs a header the store will refuse.
fn page_body_with_hop_ttls(rows: &[Row], hop_ttls: &[u8]) -> Vec<u8> {
    let next_cursor = rows.last().map(|row| row.id).unwrap_or(0);
    let envelopes: Vec<String> = rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let mut sealed = vec![0x11u8; 96];
            sealed[..8].copy_from_slice(&(row.id as u64).to_be_bytes());
            if row.msg_id == msg_id(0x7002) {
                // The duplicate: the same (hint, sealed) pair as the twin
                // already in the queue.
                sealed[..8].copy_from_slice(&2u64.to_be_bytes());
            }
            format!(
                "{{\"id\":{},\"msg_id\":\"{}\",\"hop_ttl\":{},\"recipient_hint\":\"{}\",\
                 \"sealed\":\"{}\",\"expiry_ms\":{}}}",
                row.id,
                b64(&row.msg_id),
                hop_ttls.get(index).copied().unwrap_or(3),
                b64(&row.hint),
                b64(&sealed),
                row.expiry_ms
            )
        })
        .collect();
    format!(
        "{{\"envelopes\":[{}],\"next_cursor\":{}}}",
        envelopes.join(","),
        next_cursor
    )
    .into_bytes()
}

// ---------------------------------------------------------------------------
// The seam's own rules: who may start a pass, and what a result may reach
// ---------------------------------------------------------------------------

#[test]
fn two_passes_in_one_process_never_share_an_id() {
    // The wrong-pass half of IDEMP-01 is a comparison against this id, so two
    // live passes sharing one would leave it deciding nothing — with action
    // ids restarting at 1 in every pass to collide underneath it. A caller
    // that hands over a label this module cannot use (a UUID, a device name)
    // used to get the same constant every time.
    let store = new_store();
    let first = CoreRelayPass::new(store.clone(), base_plan(T0), "PASS-A".to_string());
    let second = CoreRelayPass::new(store.clone(), base_plan(T0), "PASS-B".to_string());
    let same_label = CoreRelayPass::new(store.clone(), base_plan(T0), "p1".to_string());
    let same_label_again = CoreRelayPass::new(store.clone(), base_plan(T0), "p1".to_string());

    let a = first.start(T0);
    let b = second.start(T0);
    let c = same_label.start(T0);
    let d = same_label_again.start(T0);
    assert_ne!(
        a.pass_id, b.pass_id,
        "two passes built from unusable labels must not collapse onto one id"
    );
    assert_ne!(
        c.pass_id, d.pass_id,
        "two passes built from the same usable label must still be distinguishable"
    );
    for id in [&a.pass_id, &b.pass_id, &c.pass_id, &d.pass_id] {
        assert!(
            id.len() <= 24
                && id.bytes().all(|byte| byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || byte == b'_'
                    || byte == b'-'),
            "a pass id reaches protocol events, so it must stay an opaque token: {id}"
        );
    }

    // And the collision the derivation prevents: A's answer, arriving late,
    // named with A's id and the action id B is waiting for.
    let late = second.resume_http(CoreRelayHttpResult {
        pass_id: a.pass_id.clone(),
        action_id: b.action_id,
        status: 200,
        headers: Vec::new(),
        body: empty_page(),
        error: None,
        completed_at_ms: T0 + 50,
    });
    assert_eq!(
        late.action_id, b.action_id,
        "B must still be waiting for its own request"
    );
    assert!(matches!(late.kind, CoreRelayActionKind::Http { .. }));
    let summary = second.cancel(T0 + 60);
    assert_eq!(
        summary.stale_results_ignored, 1,
        "the other pass's answer must be counted as ignored, not applied"
    );
}

#[test]
fn a_result_before_start_starts_nothing_and_spends_nothing() {
    // The restart-recovery shape: a driver persisted an in-flight result,
    // the process died, and the replay arrives against a freshly built pass.
    // The promise is that such a result performs no store mutation — and a
    // pass it started would run from time zero, straight through the quiet
    // window this plan was built inside.
    let store = new_store();
    let now = T0;
    let rows = seed_consumed(&store, 1..11, now);
    let mut plan = base_plan(now);
    plan.quiet_until_ms = now + 600_000;

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let action = pass.resume_http(CoreRelayHttpResult {
        pass_id: "p1-nope".to_string(),
        action_id: 1,
        status: 200,
        headers: Vec::new(),
        body: page_body(&rows),
        error: None,
        completed_at_ms: now + 10,
    });
    assert!(
        matches!(action.kind, CoreRelayActionKind::NotStarted),
        "only start() may start a pass, got {:?}",
        action.kind
    );
    assert_eq!(action.action_id, 0, "nothing was emitted");
    assert_eq!(
        store.carried_len().expect("carry depth"),
        0,
        "IDEMP-01: a result nobody was waiting for reached the store"
    );
    assert_eq!(
        store
            .relay_fetch_cursor(own_cursor_key())
            .expect("cursor")
            .after_id,
        0
    );
    assert!(
        pass.summary().is_none(),
        "the pass has neither started nor finished"
    );

    // And starting it properly still honours the window: the stale result did
    // not consume the pass's one chance to refuse.
    let started = pass.start(now + 20);
    assert!(matches!(started.kind, CoreRelayActionKind::Sleep { .. }));
    let summary = pass.summary().expect("a refused pass has a summary");
    assert_eq!(summary.outcome, CoreRelayPassOutcome::RefusedQuietWindow);
    assert_eq!(summary.requests_issued, 0, "RATE-01: nothing was spent");
}

#[test]
fn a_cancelled_pass_cannot_be_started_again() {
    // `cancel` freezes the summary. A pass that could be started afterwards
    // would issue requests and touch the store with no summary willing to
    // report any of it, and LIVE-01 stops being checkable from a transcript.
    let store = new_store();
    let now = T0;
    seed_consumed(&store, 1..6, now);
    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());

    let cancelled = pass.cancel(now);
    assert_eq!(cancelled.outcome, CoreRelayPassOutcome::Cancelled);
    assert_eq!(cancelled.requests_issued, 0);

    let after = pass.start(now + 5);
    match after.kind {
        CoreRelayActionKind::Finished { summary } => {
            assert_eq!(summary.outcome, CoreRelayPassOutcome::Cancelled);
            assert_eq!(summary.requests_issued, 0);
        }
        other => panic!("a cancelled pass must answer with its summary, got {other:?}"),
    }
    assert_eq!(
        store
            .relay_fetch_cursor(own_cursor_key())
            .expect("cursor")
            .after_id,
        0,
        "a resurrected pass would have walked the mailbox"
    );
}

#[test]
fn cancelling_a_pass_writes_no_contact_endpoint_off() {
    // SILENCE-01. Silence is the absence of an answer; a pass pulled while a
    // contact's request was still outstanding never gave the endpoint its
    // chance to answer. An app backgrounded during relay passes would
    // otherwise rest a healthy endpoint one cancellation at a time — the
    // stale-endpoint demotion harm again, arriving through the cancel path.
    let store = new_store();
    let now = T0;
    seed_contact(&store);
    let mut plan = base_plan(now);
    plan.contacts = vec![CoreRelayContactConfig {
        user_id: contact_user_id(),
        relay_url: Some(CONTACT_URL.to_string()),
        relay_token: Some(CONTACT_TOKEN.to_string()),
        endpoint_usable: true,
        endpoint_answering: true,
    }];

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let mut action = pass.start(now);
    let mut clock = now;
    // Drive until the contact's mailbox is the outstanding request, answering
    // our own mailbox honestly so `own_relay_succeeded` is true — which is
    // exactly the state in which silence *would* otherwise be committed.
    let cancelled_at = loop {
        let CoreRelayActionKind::Http { ref request } = action.kind else {
            panic!("the pass finished before the contact was asked anything");
        };
        if request.base_url == CONTACT_URL {
            break clock + 10;
        }
        clock += 40;
        action = pass.resume_http(CoreRelayHttpResult {
            pass_id: action.pass_id.clone(),
            action_id: action.action_id,
            status: 200,
            headers: Vec::new(),
            body: empty_page(),
            error: None,
            completed_at_ms: clock,
        });
    };

    let summary = pass.cancel(cancelled_at);
    assert_eq!(summary.outcome, CoreRelayPassOutcome::Cancelled);
    assert_eq!(
        summary.silence_committed, 0,
        "SILENCE-01: a cancellation is not evidence about an endpoint"
    );
    assert!(
        store
            .list_contact_relay_unreachable()
            .expect("unreachable list")
            .is_empty(),
        "a healthy contact endpoint was rested because the app was backgrounded"
    );
}

// ---------------------------------------------------------------------------
// Presence: recorded, never a reason to skip the walk
// ---------------------------------------------------------------------------

#[test]
fn a_presence_failure_is_recorded_and_the_mailbox_is_still_walked() {
    // Decision (b), and the half that is easy to lose. Presence runs before
    // the walk (decision (a)), so a presence failure that ended the config
    // would mean a device fetches no mail at all, on any pass, forever. A
    // relay without the route answers 404; a proxy in front of one answers
    // 502; relayd itself answers 400 on a malformed announce.
    for status in [400u16, 404, 500, 503] {
        let store = new_store();
        let now = T0;
        let rows = seed_consumed(&store, 1..6, now);
        let mut plan = base_plan(now);
        plan.presence_announce = vec![own_hint(now)];
        plan.presence_query = vec![own_hint(now)];

        let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
        let mut fetches = 0;
        let run = drive(&pass, now, |request, _index| {
            if request.request.operation == CoreRelayOperation::Presence {
                return Reply::status(status, "boom");
            }
            if request.is_ack() {
                return Reply::empty_ok();
            }
            fetches += 1;
            if fetches == 1 {
                Reply::ok(page_body(&rows))
            } else {
                Reply::ok(empty_page())
            }
        });

        assert!(
            !run.fetches().is_empty(),
            "a {status} on /presence must not skip the walk: the device would never fetch its \
             own mail again"
        );
        assert_eq!(
            run.summary.rows_acked, 5,
            "the walk ran to the end and did its work despite the presence fault"
        );
        assert_eq!(
            store
                .relay_fetch_cursor(own_cursor_key())
                .expect("cursor")
                .after_id,
            5
        );
        assert_eq!(
            run.summary.configs_faulted, 1,
            "the presence fault is recorded rather than swallowed"
        );
    }

    // A transport failure on presence was already handled this way; the two
    // paths must not disagree about it.
    let store = new_store();
    let now = T0;
    let rows = seed_consumed(&store, 1..6, now);
    let mut plan = base_plan(now);
    plan.presence_announce = vec![own_hint(now)];
    let pass = CoreRelayPass::new(store.clone(), plan, "p2".to_string());
    let mut fetches = 0;
    let run = drive(&pass, now, |request, _index| {
        if request.request.operation == CoreRelayOperation::Presence {
            return Reply::transport(cruisemesh_core::CoreRelayTransportError::ConnectionFailed);
        }
        if request.is_ack() {
            return Reply::empty_ok();
        }
        fetches += 1;
        if fetches == 1 {
            Reply::ok(page_body(&rows))
        } else {
            Reply::ok(empty_page())
        }
    });
    assert_eq!(run.summary.rows_acked, 5);
}

// ---------------------------------------------------------------------------
// A mailbox is a (url, token) pair, not a host
// ---------------------------------------------------------------------------

#[test]
fn two_mailboxes_on_one_host_are_both_walked() {
    // One relay hosts every family, so a contact whose card names our own
    // host with their own family's credential is a *different mailbox* with
    // different mail in it — the legacy member-class card that
    // `resolved_contact_poll_relay` deliberately keeps working. Deduping the
    // walk list by host alone dropped exactly that contact and left the pass
    // sending only our own token.
    let store = new_store();
    let now = T0;
    seed_contact(&store);
    let mut plan = base_plan(now);
    plan.contacts = vec![CoreRelayContactConfig {
        user_id: contact_user_id(),
        // Same host as our own mailbox, different family credential.
        relay_url: Some(OWN_URL.to_string()),
        relay_token: Some(CONTACT_TOKEN.to_string()),
        endpoint_usable: true,
        endpoint_answering: true,
    }];

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |_request, _index| Reply::ok(empty_page()));

    let tokens: Vec<String> = run
        .requests
        .iter()
        .filter(|request| request.is_fetch())
        .filter_map(|request| {
            request
                .request
                .headers
                .iter()
                .find(|header| header.name == "Authorization")
                .map(|header| header.value.clone())
        })
        .collect();
    assert!(
        tokens.contains(&format!("Bearer {OWN_TOKEN}")),
        "our own mailbox must still be walked: {tokens:?}"
    );
    assert!(
        tokens.contains(&format!("Bearer {CONTACT_TOKEN}")),
        "a contact mailbox on the same host is a different mailbox and must be walked: {tokens:?}"
    );
    assert_eq!(run.summary.configs_walked, 2);
}

// ---------------------------------------------------------------------------
// A relay-fetched row costs a hop
// ---------------------------------------------------------------------------

#[test]
fn a_relay_fetched_row_is_carried_with_one_hop_already_spent() {
    // Both shells store `carriedHopTtl(hop_ttl)` — one less than the header —
    // for a relay-sourced carried row, precisely so a pure mule leg is
    // counted. A row stored verbatim reports a single-mule delivery as zero
    // hops taken and re-floods with a hop of the sender's budget this device
    // never paid for.
    let store = new_store();
    let now = T0;
    let rows = fresh_rows(1, 3, now);

    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let mut fetches = 0;
    let run = drive(&pass, now, |request, _index| {
        if !request.is_fetch() {
            return Reply::empty_ok();
        }
        fetches += 1;
        if fetches == 1 {
            // page_body writes hop_ttl 3 for every row.
            Reply::ok(page_body(&rows))
        } else {
            Reply::ok(empty_page())
        }
    });
    assert_eq!(run.summary.rows_ingested, 3);

    let carried = store
        .carried_envelopes_for_hints(vec![own_hint(now)], now)
        .expect("carried rows");
    assert_eq!(carried.len(), 3);
    for row in carried {
        assert_eq!(
            row.hop_ttl, 2,
            "a relay-fetched row is stored with one hop already spent, as both shells store it"
        );
    }
}

// ---------------------------------------------------------------------------
// Budgets that actually bite
// ---------------------------------------------------------------------------

/// The plan's budgets, cut down to whatever this scenario needs to reach.
fn plan_held_to(now: i64, budgets: cruisemesh_core::CoreRelayPassBudgets) -> CoreRelayPassPlan {
    let mut plan = base_plan(now);
    plan.budgets = budgets;
    plan
}

#[test]
fn each_declared_budget_is_what_stops_the_pass() {
    // LIVE-01's executable half, as evidence rather than assertion. The
    // deployed budgets sit far above what the per-mailbox walk budget lets a
    // pass reach, so a scenario at default settings proves nothing about the
    // pass-level gate: remove the gate entirely and it still passes. These
    // scenarios are the ones that go red.
    let now = T0;
    let default_budgets = core_relay_pass_default_budgets();

    // 1. Requests. A relay with endless mail against a three-request pass.
    let requests_store = new_store();
    let plan = plan_held_to(
        now,
        cruisemesh_core::CoreRelayPassBudgets {
            max_requests: 3,
            ..default_budgets
        },
    );
    let pass = CoreRelayPass::new(requests_store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |_request, index| {
        Reply::ok(page_body(&fresh_rows(1 + index as i64 * 8, 8, now)))
    });
    assert_eq!(
        run.summary.requests_issued, 3,
        "the request budget is exact: {:?}",
        run.summary
    );
    assert_eq!(run.summary.outcome, CoreRelayPassOutcome::BudgetYield);

    // 2. Requests, including the ack a consumed page earns. The ack is
    // emitted from inside a result, so it is the one request nothing else
    // gates; a page that cannot afford it holds its frontier instead.
    let ack_store = new_store();
    let rows = seed_consumed(&ack_store, 1..21, now);
    let mut plan = plan_held_to(
        now,
        cruisemesh_core::CoreRelayPassBudgets {
            max_requests: 2,
            ..default_budgets
        },
    );
    plan.presence_announce = vec![own_hint(now)];
    let pass = CoreRelayPass::new(ack_store.clone(), plan, "p2".to_string());
    let run = drive(&pass, now, |request, _index| {
        if request.is_ack() {
            return Reply::empty_ok();
        }
        if request.is_fetch() {
            Reply::ok(page_body(&rows))
        } else {
            Reply::empty_ok()
        }
    });
    assert!(
        run.summary.requests_issued <= 2,
        "a pass held to 2 requests issued {} — the ack bypassed the budget",
        run.summary.requests_issued
    );
    assert_eq!(
        run.acks().len(),
        0,
        "the page could not afford its ack, so none was sent"
    );
    assert_eq!(
        ack_store
            .relay_fetch_cursor(own_cursor_key())
            .expect("cursor")
            .after_id,
        0,
        "CURSOR-01: a page whose ack was never sent holds its frontier"
    );
    assert!(
        run.summary.frontiers_held >= 1,
        "the hold must be written down, not merely not-done"
    );

    // 3. Envelopes. An admission limit: no request is admitted once the count
    // is reached, so the pass ends within one page of the bound.
    let envelope_store = new_store();
    let plan = plan_held_to(
        now,
        cruisemesh_core::CoreRelayPassBudgets {
            max_envelopes: 10,
            ..default_budgets
        },
    );
    let pass = CoreRelayPass::new(envelope_store.clone(), plan, "p3".to_string());
    let run = drive(&pass, now, |_request, index| {
        Reply::ok(page_body(&fresh_rows(1 + index as i64 * 8, 8, now)))
    });
    assert!(
        run.summary.envelopes_processed >= 10 && run.summary.envelopes_processed <= 10 + 256,
        "the envelope budget must stop the pass within one page of itself, got {}",
        run.summary.envelopes_processed
    );
    assert_eq!(
        run.summary.requests_issued, 2,
        "eight rows a page, ten allowed"
    );
    assert_eq!(run.summary.outcome, CoreRelayPassOutcome::BudgetYield);

    // 4. Response bytes, the same shape.
    let byte_store = new_store();
    let plan = plan_held_to(
        now,
        cruisemesh_core::CoreRelayPassBudgets {
            max_response_bytes: 512,
            ..default_budgets
        },
    );
    let pass = CoreRelayPass::new(byte_store.clone(), plan, "p4".to_string());
    let run = drive(&pass, now, |_request, index| {
        Reply::ok(page_body(&fresh_rows(1 + index as i64 * 8, 8, now)))
    });
    assert!(
        run.summary.response_bytes_read >= 512,
        "the scenario must reach the byte budget"
    );
    assert!(
        run.summary.requests_issued <= 3,
        "the byte budget must stop the pass, got {} requests",
        run.summary.requests_issued
    );
    assert_eq!(run.summary.outcome, CoreRelayPassOutcome::BudgetYield);

    // 5. The wall clock. A driver whose every answer takes longer than the
    // whole deadline ends the pass after one of them.
    let clock_store = new_store();
    let plan = plan_held_to(
        now,
        cruisemesh_core::CoreRelayPassBudgets {
            deadline_ms: 25,
            ..default_budgets
        },
    );
    let pass = CoreRelayPass::new(clock_store.clone(), plan, "p5".to_string());
    let run = drive(&pass, now, |_request, index| {
        let mut reply = Reply::ok(page_body(&fresh_rows(1 + index as i64 * 8, 8, now)));
        reply.elapsed_ms = 900;
        reply
    });
    assert_eq!(
        run.summary.requests_issued, 1,
        "a 25ms deadline against a 900ms answer must end the pass after one request"
    );
    assert_eq!(run.summary.outcome, CoreRelayPassOutcome::BudgetYield);

    // And every one of them still reports the budgets it was held to, which
    // is what makes a transcript checkable without reading this file.
    assert_eq!(run.summary.budgets.deadline_ms, 25);
}

// ---------------------------------------------------------------------------
// A fixture's arithmetic is checkable even when its transcript is not
// ---------------------------------------------------------------------------

/// Fixture counts are not a golden trace — 6.6 of the contract says so, and
/// says why — but the ones that are *derived from a core rule* can be checked
/// against that rule, and those are exactly the ones a hand edit gets wrong.
///
/// The `oversize-shrink` correction in this package was found by reading:
/// the fixture claimed a 256-row page retried at 64 rows, and
/// [`cruisemesh_core::relay_fetch_shrunk_limit`] halves. Reading is not a
/// regression test, so here is one. It is deliberately narrow: a rule this
/// repository owns, applied to every fixture that states a number the rule
/// produces.
#[test]
fn a_fixture_that_states_a_derived_number_states_the_one_core_would_produce() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    let mut checked = 0u32;
    for entry in std::fs::read_dir(&dir)
        .expect("fixtures directory")
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let name = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or_default()
            .to_string();
        let text = std::fs::read_to_string(&path).expect("fixture reads");
        for line in text.lines().skip(1) {
            let event: serde_json::Value = serde_json::from_str(line).expect("event line parses");
            let Some(counts) = event.get("counts") else {
                continue;
            };
            let count = |key: &str| counts.get(key).and_then(|value| value.as_i64());

            // The page-shrink rule. A fixture may not invent a step.
            if let (Some(requested), Some(retry)) = (count("requested_rows"), count("retry_rows")) {
                let expected = cruisemesh_core::relay_fetch_shrunk_limit(requested as u32);
                assert_eq!(
                    Some(retry as u32),
                    expected,
                    "{name}: a retry of {retry} rows after {requested} is not the step \
                     relay_fetch_shrunk_limit produces ({expected:?})"
                );
                checked += 1;
            }

            // A frontier never moves backwards, and a held one does not move
            // at all. Both are CURSOR-01 stated as arithmetic.
            if let (Some(before), Some(after)) = (count("frontier_before"), count("frontier_after"))
            {
                assert!(
                    after >= before,
                    "{name}: CURSOR-01 — a frontier moved backwards, {before} to {after}"
                );
                if event.get("code").and_then(|code| code.as_str()) == Some("frontier_held") {
                    assert_eq!(
                        before, after,
                        "{name}: a held frontier that moved is not a held frontier"
                    );
                }
                checked += 1;
            }

            // A page cannot consume or ack more rows than it returned.
            if let Some(returned) = count("rows_returned") {
                for key in ["rows_consumed", "rows_acked", "ack_ids", "rows_carried"] {
                    if let Some(value) = count(key) {
                        assert!(
                            value <= returned,
                            "{name}: {key} is {value} against a page of {returned} rows"
                        );
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(
        checked >= 8,
        "the guard must actually reach the corpus; it checked {checked} numbers"
    );
}

// ===========================================================================
// PRESENCE-01 — the cross-family query, and what it is allowed to cost
// ===========================================================================

/// A friend card from another family: their relay, and the post-only
/// credential a card carries. `resolved_contact_poll_relay` drops this
/// endpoint from the walk — a deposit token cannot read a mailbox — which is
/// exactly the case that stopped yielding a last-seen.
fn cross_family_contact(user_id: Vec<u8>, url: &str) -> CoreRelayContactConfig {
    CoreRelayContactConfig {
        user_id,
        relay_url: Some(url.to_string()),
        relay_token: Some(cruisemesh_core::relay_deposit_token_for(
            CONTACT_TOKEN.to_string(),
        )),
        endpoint_usable: true,
        endpoint_answering: true,
    }
}

fn presence_requests(run: &Run) -> Vec<&Recorded> {
    run.requests
        .iter()
        .filter(|r| r.request.operation == CoreRelayOperation::Presence)
        .collect()
}

fn announce_and_query(recorded: &Recorded) -> (usize, usize) {
    let body: serde_json::Value =
        serde_json::from_slice(&recorded.request.body).expect("a presence body is JSON");
    (
        body["announce"].as_array().expect("announce").len(),
        body["query"].as_array().expect("query").len(),
    )
}

#[test]
fn a_cross_family_contact_is_asked_after_once_a_pass_and_never_told_anything() {
    let store = new_store();
    let now = T0;
    seed_contact(&store);
    let mut plan = base_plan(now);
    plan.contacts = vec![cross_family_contact(contact_user_id(), CONTACT_URL)];

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });

    let queries = presence_requests(&run);
    assert_eq!(
        queries.len(),
        1,
        "one contact, one question, one pass — got {} requests",
        queries.len()
    );
    assert_eq!(queries[0].request.base_url, CONTACT_URL);
    let (announce, query) = announce_and_query(queries[0]);
    assert_eq!(
        announce, 0,
        "the announce half stays dropped: a cross-family query tells the answering \
         family nothing about who is asking"
    );
    assert!(
        query > 0,
        "the query half is the whole point of the request"
    );

    // And the endpoint is still not a mailbox. Nothing about being allowed to
    // ask made it fetchable.
    assert!(
        run.requests
            .iter()
            .all(|r| r.request.base_url != CONTACT_URL || !r.is_fetch()),
        "a deposit-class endpoint is not polled, before or after this change"
    );
}

#[test]
fn the_freshest_presence_row_wins_when_a_probe_covers_several_hints() {
    let store = new_store();
    let now = T0;
    seed_contact(&store);
    let mut plan = base_plan(now);
    plan.contacts = vec![cross_family_contact(contact_user_id(), CONTACT_URL)];

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        if request.request.operation == CoreRelayOperation::Presence {
            let body: serde_json::Value =
                serde_json::from_slice(&request.request.body).expect("a presence body is JSON");
            let hints: Vec<String> = body["query"]
                .as_array()
                .expect("query")
                .iter()
                .map(|h| h.as_str().expect("a hint is a string").to_string())
                .collect();
            let stale = hints.first().cloned().expect("at least one hint");
            let fresh = hints.get(1).cloned().unwrap_or_else(|| stale.clone());
            // The stale row deliberately comes FIRST: an implementation that
            // takes the first row instead of the freshest caches this contact
            // a day old while another hint saw them a minute ago.
            let reply = serde_json::json!({
                "now_ms": now,
                "presence": [
                    { "hint": stale, "last_seen_ms": now - 20 * 60 * 60 * 1000 },
                    { "hint": fresh, "last_seen_ms": now - 60_000 },
                ]
            });
            return Reply::ok(reply.to_string().into_bytes());
        }
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });

    assert_eq!(presence_requests(&run).len(), 1);
    let (bucket, _recorded_at) = store
        .contact_presence(contact_user_id())
        .expect("presence readable")
        .expect("the answer was recorded");
    assert_eq!(
        bucket, "active",
        "the freshest returned row decides the bucket, not whichever row is first"
    );
}

#[test]
fn a_second_pass_inside_the_client_floor_asks_nothing() {
    let store = new_store();
    let now = T0;
    seed_contact(&store);
    let contacts = vec![cross_family_contact(contact_user_id(), CONTACT_URL)];

    let respond = |request: &Recorded, _index: usize| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    };

    let mut plan = base_plan(now);
    plan.contacts = contacts.clone();
    let first = drive(
        &CoreRelayPass::new(store.clone(), plan, "p1".to_string()),
        now,
        respond,
    );
    assert_eq!(presence_requests(&first).len(), 1);

    // A minute later — well inside the floor — the pass runs everything else
    // and asks nothing. The cached bucket is the answer.
    let soon = now + 60_000;
    let mut plan = base_plan(soon);
    plan.contacts = contacts.clone();
    let second = drive(
        &CoreRelayPass::new(store.clone(), plan, "p2".to_string()),
        soon,
        respond,
    );
    assert_eq!(
        presence_requests(&second).len(),
        0,
        "a limit is not a schedule: the client holds its own floor"
    );

    // Past the floor, the question is worth asking again.
    let later = now + cruisemesh_core::RELAY_CROSS_FAMILY_PRESENCE_MIN_INTERVAL_MS + 1;
    let mut plan = base_plan(later);
    plan.contacts = contacts;
    let third = drive(
        &CoreRelayPass::new(store.clone(), plan, "p3".to_string()),
        later,
        respond,
    );
    assert_eq!(presence_requests(&third).len(), 1);
}

#[test]
fn a_relay_that_never_answers_is_not_asked_again_next_pass() {
    // The floor is stamped when the query is *sent*. A relay that times out,
    // or an older one that refuses the route outright, therefore costs the
    // same wait as one that answered — a failing endpoint cannot be turned
    // into a retry storm by failing.
    let store = new_store();
    let now = T0;
    seed_contact(&store);
    let contacts = vec![cross_family_contact(contact_user_id(), CONTACT_URL)];

    let respond = |request: &Recorded, _index: usize| {
        if request.request.base_url == CONTACT_URL {
            // What an older relayd says: this credential may only post.
            return Reply::status(403, "deposit_only");
        }
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    };

    let mut plan = base_plan(now);
    plan.contacts = contacts.clone();
    let first = drive(
        &CoreRelayPass::new(store.clone(), plan, "p1".to_string()),
        now,
        respond,
    );
    assert_eq!(presence_requests(&first).len(), 1);
    assert_eq!(
        first.summary.configs_faulted, 0,
        "a refused presence query is not a faulted mailbox: there is no mailbox here"
    );
    assert_eq!(
        first.summary.silence_committed + first.summary.silence_discarded,
        0,
        "and it is not silence evidence either — a relay upgrade schedule must not be \
         able to write off every cross-family endpoint in an address book"
    );

    let soon = now + 60_000;
    let mut plan = base_plan(soon);
    plan.contacts = contacts;
    let second = drive(
        &CoreRelayPass::new(store.clone(), plan, "p2".to_string()),
        soon,
        respond,
    );
    assert_eq!(presence_requests(&second).len(), 0);
}

#[test]
fn a_resting_endpoint_contributes_no_query() {
    let store = new_store();
    let now = T0;
    seed_contact(&store);
    let mut plan = base_plan(now);
    let mut contact = cross_family_contact(contact_user_id(), CONTACT_URL);
    contact.endpoint_usable = false;
    plan.contacts = vec![contact];

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        assert_ne!(
            request.request.base_url, CONTACT_URL,
            "an endpoint this device has written off is asked nothing at all"
        );
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert_eq!(presence_requests(&run).len(), 0);
}

#[test]
fn the_queries_are_bounded_however_large_the_address_book_is() {
    let store = new_store();
    let now = T0;
    let mut plan = base_plan(now);
    plan.contacts = (0..24u8)
        .map(|i| cross_family_contact(vec![i; 32], &format!("https://relay-{i}.example")))
        .collect();

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });

    assert_eq!(
        presence_requests(&run).len() as u32,
        cruisemesh_core::RELAY_PASS_MAX_PRESENCE_PROBES,
        "advisory work is capped by something that does not grow with an address book"
    );
    assert!(
        run.summary.requests_issued <= run.summary.budgets.max_requests,
        "LIVE-01: every query is charged against the pass's own request budget"
    );
}

#[test]
fn a_rate_limited_query_ends_the_pass_and_honours_retry_after() {
    // RATE-01 is not weakened by the query being advisory: a 429 on one ends
    // the remaining network work and the quiet window it names is a floor.
    let store = new_store();
    let now = T0;
    seed_contact(&store);
    let mut plan = base_plan(now);
    plan.contacts = vec![cross_family_contact(contact_user_id(), CONTACT_URL)];

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        if request.request.base_url == CONTACT_URL {
            return Reply::rate_limited(30);
        }
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });

    assert_eq!(run.summary.outcome, CoreRelayPassOutcome::RateLimited);
    assert!(
        run.summary.quiet_until_ms >= now + 30_000,
        "the advertised Retry-After is a floor, not a suggestion: {} against {}",
        run.summary.quiet_until_ms,
        now + 30_000
    );
}

#[test]
fn a_same_family_contact_is_answered_by_its_own_config_not_a_second_request() {
    // A contact whose card names a mailbox this device may poll already has
    // its presence answered through that config. Asking twice would spend a
    // request to learn nothing.
    let store = new_store();
    let now = T0;
    seed_contact(&store);
    let mut plan = base_plan(now);
    plan.contacts = vec![CoreRelayContactConfig {
        user_id: contact_user_id(),
        relay_url: Some(CONTACT_URL.to_string()),
        relay_token: Some(CONTACT_TOKEN.to_string()),
        endpoint_usable: true,
        endpoint_answering: true,
    }];

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let run = drive(&pass, now, |request, _index| {
        if request.is_fetch() {
            Reply::ok(empty_page())
        } else {
            Reply::empty_ok()
        }
    });
    assert_eq!(
        presence_requests(&run).len(),
        0,
        "a pollable endpoint is walked, and its presence rides that config"
    );
}

// ---------------------------------------------------------------------------
// What the pass hands the shell to project
// ---------------------------------------------------------------------------

/// The rows a shell must project are the rows the ingest transaction newly
/// took, and only those.
///
/// This is the seam that closes "the message arrived and nobody knew" under
/// the core engine: core cannot open a sealed body, so unless it says which
/// rows it just persisted, the shell has nothing to open, deliver or raise a
/// notification for. Handing over a row the store already had would be worse
/// than handing over none — that is a second notification for one message.
#[test]
fn a_pass_reports_the_rows_it_newly_persisted_and_nothing_else() {
    let store = new_store();
    let now = T0;
    let fresh = fresh_rows(1, 2, now);
    let known = seed_consumed(&store, 50..51, now);
    let mut page: Vec<Row> = Vec::new();
    for row in &fresh {
        page.push(Row {
            id: row.id,
            msg_id: row.msg_id.clone(),
            hint: row.hint.clone(),
            expiry_ms: row.expiry_ms,
        });
    }
    page.push(Row {
        id: 3,
        msg_id: known[0].msg_id.clone(),
        hint: known[0].hint.clone(),
        expiry_ms: known[0].expiry_ms,
    });

    let pass = CoreRelayPass::new(store.clone(), base_plan(now), "p1".to_string());
    let mut served = false;
    drive(&pass, now, |request, _index| {
        if request.is_fetch() && !served {
            served = true;
            return Reply::ok(page_body(&page));
        }
        if request.is_fetch() {
            return Reply::ok(empty_page());
        }
        Reply::empty_ok()
    });

    let projection = pass.take_projection();
    let reported: Vec<Vec<u8>> = projection
        .ingested
        .iter()
        .map(|envelope| envelope.msg_id.clone())
        .collect();
    assert_eq!(
        reported,
        vec![fresh[0].msg_id.clone(), fresh[1].msg_id.clone()],
        "only the rows this transaction persisted may be projected"
    );
    assert!(
        projection.ingested.iter().all(|e| !e.sealed.is_empty()),
        "a projected row must carry the sealed body the shell has to open"
    );

    // Drained, not accumulated: a driver that asks twice must not project the
    // same page twice.
    assert!(pass.take_projection().ingested.is_empty());
}

/// A mailbox's presence answer reaches the shell, which is the only way a
/// contact's "last seen" moves while the core engine is driving.
#[test]
fn a_presence_answer_is_reported_as_an_age_rather_than_a_relay_timestamp() {
    let store = new_store();
    let now = T0;
    let mut plan = base_plan(now);
    let hint = compute_recipient_hint(contact_user_id(), now);
    plan.presence_query = vec![hint.clone()];

    let pass = CoreRelayPass::new(store.clone(), plan, "p1".to_string());
    let body = format!(
        "{{\"now_ms\":{},\"presence\":[{{\"hint\":\"{}\",\"last_seen_ms\":{}}}]}}",
        // A relay whose clock runs an hour ahead of ours: the answer must
        // still be reported as an age, so nothing downstream can adopt the
        // relay's timestamp as a local one.
        now + 3_600_000,
        b64(&hint),
        now + 3_600_000 - 5_000
    );
    drive(&pass, now, |request, _index| {
        if request.request.operation == CoreRelayOperation::Presence {
            return Reply::ok(body.clone().into_bytes());
        }
        if request.is_fetch() {
            return Reply::ok(empty_page());
        }
        Reply::empty_ok()
    });

    let projection = pass.take_projection();
    assert_eq!(
        projection.presence.len(),
        1,
        "the answer must reach the shell"
    );
    let observation = &projection.presence[0];
    assert_eq!(observation.hint, hint);
    assert!(
        observation.user_id.is_empty(),
        "a mailbox answers about hints, not people"
    );
    assert_eq!(observation.age_ms, 5_000);
    // This device's clock as the pass has it -- never the relay's, which is an
    // hour ahead in this scenario.
    assert!(
        observation.observed_at_ms >= now && observation.observed_at_ms < now + 60_000,
        "observed on this device's clock, got {}",
        observation.observed_at_ms
    );
}
