//! Test support: the incident fixtures, made executable **through a platform
//! driver**.
//!
//! # Why this exists
//!
//! `core/tests/relay_pass_replay.rs` already executes the relay-shaped
//! fixtures under `core/tests/fixtures/`: it builds the situation each
//! transcript describes in a real store, drives a real [`CoreRelayPass`]
//! through it against a scripted relay, and asserts the invariants the fixture
//! declares held. That is a claim about the core session, in Rust.
//!
//! It is not a claim about the phones. The Android and iOS relay adapters —
//! `CoreRelayDriver` + `CoreRelayPassRunner`, `RelayActionDriver` +
//! `RelaySyncDriver` — sit between that session and the socket, and the only
//! thing they shared until now was a table of four request shapes
//! ([`crate::core_relay_adapter_vectors`]). A request shape says nothing about
//! whether a whole incident, driven over a real transport by a real shell,
//! ends in the same state on both platforms.
//!
//! This module is that missing half. It hands each shell everything about a
//! fixture scenario except the HTTP: the seeding, the plan, the scripted
//! answer for each request, and the normalisation of what happened. The shell
//! contributes exactly what it owns — putting a request on a wire and
//! reporting what came back — and the resulting transcript is compared against
//! [`core_relay_fixture_expected_transcript`], which is this same scenario run
//! in Rust with the HTTP replaced by the scripted answer directly.
//!
//! Equality therefore means something specific and worth having: *the platform
//! driver reported every response the way core would have, and the pass
//! reached the same store state, the same summary and the same emitted event
//! codes as it does with no shell at all.* A driver that mangled a query
//! string, dropped a body, swallowed a status, mislabelled a transport failure
//! or lost a header changes one of those and the comparison fails.
//!
//! # This is test support, not app API
//!
//! Nothing in here is called by the app on either platform. Every exported
//! name is prefixed `core_relay_fixture_` / `CoreRelayFixture` so that is
//! visible at the call site, and the only callers are
//! `RelayAdapterFixtureTranscriptTest.kt`, `RelayAdapterFixtureTranscriptTests.swift`
//! and `core/tests/relay_fixture_transcript.rs`. It is exported over UniFFI
//! rather than kept in a Rust test because a Rust test cannot be called from a
//! JVM or an XCTest, and the alternative — writing the expected transcript out
//! by hand in Kotlin and again in Swift — is three descriptions of one
//! behaviour, which is the thing this whole seam exists to stop.
//!
//! # Adding a fixture
//!
//! One arm in [`scenario_of`], one arm in [`seed_for`], one arm in
//! [`reply_for`], and the name in [`core_relay_fixture_names`]. Nothing in the
//! shells changes: both suites iterate `core_relay_fixture_names()`.
//!
//! # The shape of a scripted failure
//!
//! [`reply_for`] is keyed on the pass and on `(operation, endpoint)` — never on
//! "the third request of this pass". That is deliberate, and it constrains what
//! a scenario can express: each shell answers every request from this script
//! independently, so a script that turned on a request ordinal would be asking
//! two runtimes to agree on a count they keep separately, and any difference in
//! how a lane interleaves would read as a driver bug rather than as what it is.
//! A scenario that needs *part* of a lane refused therefore stages it across
//! passes rather than inside one — see `group-fanout-partial` below.
//!
//! # What is deliberately not here yet
//!
//! Nothing relay-shaped. The group-lane fixtures arrived once the upload lanes
//! learned to decompose a group-addressed row into one row per member with
//! durable per-member markers; before that, a group transcript would have
//! pinned present behaviour rather than intended behaviour. The two mesh-shaped
//! fixtures in the corpus stay out for the reason `relay_pass_replay.rs` gives:
//! a relay pass has no encounter and no peer link to drive them with.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::session::relay_pass::{
    CoreRelayActionKind, CoreRelayContactConfig, CoreRelayEndpointConfig, CoreRelayHeader,
    CoreRelayHttpRequest, CoreRelayHttpResult, CoreRelayOperation, CoreRelayPass,
    CoreRelayPassPlan, CoreRelayPassSummary, CoreRelayTransportError,
};
use crate::store::{CarriedEnvelope, Contact, MessageStore, OutboundEnvelope, StoredMessage};
use crate::{compute_recipient_hint, core_relay_pass_default_budgets, relay_cursor_key, KIND_TEXT};

// ---------------------------------------------------------------------------
// The identities and endpoints a fixture scenario runs on
// ---------------------------------------------------------------------------

/// The relay this device's own mailbox lives on, in the Rust reference run.
/// A shell substitutes the address of whatever fake server it stood up; the
/// transcript never carries either, so the substitution is invisible.
const REFERENCE_OWN_URL: &str = "https://relay.example";
const REFERENCE_OWN_TOKEN: &str = "member-token-aaaaaaaaaaaa";
const REFERENCE_CONTACT_URL: &str = "https://contact-relay.example";
const REFERENCE_CONTACT_TOKEN: &str = "member-token-bbbbbbbbbbbb";

fn own_user_id() -> Vec<u8> {
    (0u8..32).collect()
}

fn contact_user_id() -> Vec<u8> {
    vec![9u8; 32]
}

// ---------------------------------------------------------------------------
// The scenario surface
// ---------------------------------------------------------------------------

/// One pass in a fixture scenario: the label its transcript is read by and the
/// wall clock it runs on.
///
/// The clock is fixed per pass rather than advanced per request on purpose.
/// A shell cannot make a real socket take a scripted number of milliseconds,
/// so a transcript that depended on elapsed time would be comparing the test
/// machine's speed. Every time-derived field in the normalisation is therefore
/// expressed relative to this.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayFixturePassSpec {
    pub label: String,
    pub now_ms: i64,
}

/// A fixture, as something a shell can execute.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayFixtureScenario {
    /// The fixture file's stem, e.g. `carry-storm`.
    pub name: String,
    /// The invariant ids the fixture declares. The suites assert none of them
    /// is reported violated, which is the same claim `relay_pass_replay.rs`
    /// makes in Rust.
    pub declared_invariants: Vec<String>,
    /// The passes to drive, in order, against one store.
    pub passes: Vec<CoreRelayFixturePassSpec>,
    /// Whether the plan names a contact endpoint, so a shell knows whether it
    /// needs a second fake relay standing.
    pub uses_contact_endpoint: bool,
}

/// Which configured endpoint a request went to. A shell decides this by
/// comparing the request's base URL against the two it configured — the one
/// judgement it makes, and not a protocol one.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreRelayFixtureEndpoint {
    Own,
    Contact,
}

impl CoreRelayFixtureEndpoint {
    /// The pseudonym the fixture corpus already uses for this actor.
    fn pseudonym(self) -> &'static str {
        match self {
            CoreRelayFixtureEndpoint::Own => "mailbox-a",
            CoreRelayFixtureEndpoint::Contact => "contact-b",
        }
    }
}

/// What the scripted relay answers one request with.
///
/// `transport_failure` is a relay that could not be reached at all rather than
/// one that answered badly, and a shell produces it however its fake transport
/// can — Android disconnects the socket before the status line, iOS fails the
/// `URLProtocol` with `cannotConnectToHost`. The distinction matters to
/// `SILENCE-01`, which is exactly why a fixture needs to be able to script it.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayFixtureReply {
    pub status: u16,
    pub headers: Vec<CoreRelayHeader>,
    pub body: Vec<u8>,
    pub transport_failure: bool,
}

impl CoreRelayFixtureReply {
    fn ok(body: Vec<u8>) -> Self {
        CoreRelayFixtureReply {
            status: 200,
            headers: Vec::new(),
            body,
            transport_failure: false,
        }
    }

    fn unreachable() -> Self {
        CoreRelayFixtureReply {
            status: 0,
            headers: Vec::new(),
            body: Vec::new(),
            transport_failure: true,
        }
    }
}

/// The request as the *server* saw it, which is the half of the comparison a
/// shell contributes.
///
/// Recording core's own [`CoreRelayHttpRequest`] on both sides would compare
/// core against core and pass whatever the driver did with it. These are the
/// bytes that actually left the device.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRelayFixtureObservedRequest {
    pub method: String,
    /// Path *including* the query string, exactly as received.
    pub path: String,
    pub body_len: u32,
    /// The `Authorization` header the server received, if any.
    pub authorization: Option<String>,
}

// ---------------------------------------------------------------------------
// Exported scenario lookup
// ---------------------------------------------------------------------------

/// The fixtures both adapter suites execute, in the order they run them.
///
/// Adding a name here without the matching arms in [`scenario_of`],
/// [`seed_for`] and [`reply_for`] fails `core/tests/relay_fixture_transcript.rs`
/// rather than silently doing nothing.
#[uniffi::export]
pub fn core_relay_fixture_names() -> Vec<String> {
    vec![
        "carry-storm".to_string(),
        "contact-silence-no-proof".to_string(),
        "group-fanout-complete".to_string(),
        "group-fanout-partial".to_string(),
    ]
}

/// The scenario for a fixture name, or a panic for an unknown one — an unknown
/// name is a test asking for something that does not exist, and returning an
/// empty scenario would make that pass.
#[uniffi::export]
pub fn core_relay_fixture_scenario(name: String) -> CoreRelayFixtureScenario {
    scenario_of(&name)
}

fn scenario_of(name: &str) -> CoreRelayFixtureScenario {
    const T0: i64 = 1_700_000_000_000;
    match name {
        // Two passes across one store: the first uploads the carry queue and
        // marks it, the second is the process restart that #222 turned into a
        // re-upload storm. MARK-01 is the claim that the second offers nothing.
        "carry-storm" => CoreRelayFixtureScenario {
            name: name.to_string(),
            declared_invariants: vec![
                "MARK-01".to_string(),
                "CARRY-01".to_string(),
                "LIVE-01".to_string(),
            ],
            passes: vec![
                CoreRelayFixturePassSpec {
                    label: "p1".to_string(),
                    now_ms: T0,
                },
                CoreRelayFixturePassSpec {
                    label: "p2".to_string(),
                    now_ms: T0 + 60_000,
                },
            ],
            uses_contact_endpoint: false,
        },
        // Three passes: nothing answers, then only the contact does not, then
        // it does again. SILENCE-01 is the claim that only the middle one may
        // rest an endpoint.
        "contact-silence-no-proof" => CoreRelayFixtureScenario {
            name: name.to_string(),
            declared_invariants: vec!["SILENCE-01".to_string()],
            passes: vec![
                CoreRelayFixturePassSpec {
                    label: "p1".to_string(),
                    now_ms: T0,
                },
                CoreRelayFixturePassSpec {
                    label: "p2".to_string(),
                    now_ms: T0 + 120_000,
                },
                CoreRelayFixturePassSpec {
                    label: "p3".to_string(),
                    now_ms: T0 + 240_000,
                },
            ],
            uses_contact_endpoint: true,
        },
        // One group-addressed authored row, three members, everything
        // accepted. FANOUT-01's first half: the row leaves as one row per
        // member rather than as one group-hinted row into a mailbox no member
        // reads, and the envelope is retired exactly once — the second pass
        // offers nothing.
        "group-fanout-complete" => CoreRelayFixtureScenario {
            name: name.to_string(),
            declared_invariants: vec!["FANOUT-01".to_string(), "LIVE-01".to_string()],
            passes: vec![
                CoreRelayFixturePassSpec {
                    label: "p1".to_string(),
                    now_ms: T0,
                },
                CoreRelayFixturePassSpec {
                    label: "p2".to_string(),
                    now_ms: T0 + 60_000,
                },
            ],
            uses_contact_endpoint: true,
        },
        // The same group, half-posted and then resumed. Three passes, because
        // the script cannot refuse the second request of a pass while
        // accepting the first (see the module docs), and the incident needs
        // one member landed and the rest owed:
        //
        // * p1 runs under a one-row authored budget, so exactly one member's
        //   row goes out and is accepted. The envelope stays queued: two
        //   members never received it.
        // * p2 has the mailbox refuse, which is the fault shape that leaves a
        //   fan-out stuck. Nothing lands, and nothing is un-landed either.
        // * p3 lets it through, and FANOUT-01's second half is what the
        //   transcript shows: two posts, not three. The member whose row
        //   landed in p1 is not asked to receive it twice, and only now is the
        //   envelope retired.
        "group-fanout-partial" => CoreRelayFixtureScenario {
            name: name.to_string(),
            declared_invariants: vec!["FANOUT-01".to_string(), "LIVE-01".to_string()],
            passes: vec![
                CoreRelayFixturePassSpec {
                    label: "p1".to_string(),
                    now_ms: T0,
                },
                CoreRelayFixturePassSpec {
                    label: "p2".to_string(),
                    now_ms: T0 + 60_000,
                },
                CoreRelayFixturePassSpec {
                    label: "p3".to_string(),
                    now_ms: T0 + 120_000,
                },
            ],
            uses_contact_endpoint: true,
        },
        other => panic!("no relay fixture scenario is wired for {other}"),
    }
}

/// Build the store this scenario starts from.
///
/// Seeding lives here rather than in each shell for the reason the whole
/// module exists: a Kotlin seeder and a Swift seeder that drifted from the
/// Rust one would produce three different incidents wearing one name.
#[uniffi::export]
pub fn core_relay_fixture_seed_store(store: Arc<MessageStore>, name: String) {
    let scenario = scenario_of(&name);
    let now_ms = scenario.passes[0].now_ms;
    seed_for(&name, &store, now_ms);
}

fn seed_for(name: &str, store: &MessageStore, now_ms: i64) {
    match name {
        "carry-storm" => seed_carried(store, 5, now_ms),
        "contact-silence-no-proof" => seed_contact(store),
        "group-fanout-complete" | "group-fanout-partial" => seed_group_authored(store, now_ms),
        other => panic!("no relay fixture seeding is wired for {other}"),
    }
}

/// The plan for one pass of a scenario, addressed at whatever relays the
/// caller stood up.
#[uniffi::export]
pub fn core_relay_fixture_plan(
    name: String,
    pass_index: u32,
    own_url: String,
    own_token: String,
    contact_url: String,
    contact_token: String,
) -> CoreRelayPassPlan {
    let scenario = scenario_of(&name);
    let spec = scenario
        .passes
        .get(pass_index as usize)
        .unwrap_or_else(|| panic!("{name} has no pass {pass_index}"));

    let contacts = match name.as_str() {
        // Every member's card names the same mailbox, which is what a family
        // on one relay looks like, and is why the fan-out lane resolves a
        // single target for the whole group.
        "group-fanout-complete" | "group-fanout-partial" => fixture_group()
            .member_user_ids
            .into_iter()
            .map(|user_id| CoreRelayContactConfig {
                user_id,
                relay_url: Some(contact_url.clone()),
                relay_token: Some(contact_token.clone()),
                endpoint_usable: true,
                endpoint_answering: true,
            })
            .collect(),
        _ if scenario.uses_contact_endpoint => vec![CoreRelayContactConfig {
            user_id: contact_user_id(),
            relay_url: Some(contact_url),
            relay_token: Some(contact_token),
            endpoint_usable: true,
            // These fixtures replay incidents recorded before the resting
            // endpoint was told apart from a written-off one, so their contact
            // endpoint is answering by construction.
            endpoint_answering: true,
        }],
        _ => Vec::new(),
    };

    let mut budgets = core_relay_pass_default_budgets();
    // The one-row budget that makes `group-fanout-partial` partial. It is the
    // deployed budget everywhere else, including in that fixture's later
    // passes, so what the resume posts is decided by the per-member markers
    // rather than by a budget still being narrow.
    if name == "group-fanout-partial" && pass_index == 0 {
        budgets.max_authored_uploads = 1;
    }

    CoreRelayPassPlan {
        own: Some(CoreRelayEndpointConfig {
            url: own_url,
            token: own_token,
        }),
        contacts,
        own_user_id: own_user_id(),
        fetch_hints: vec![compute_recipient_hint(own_user_id(), spec.now_ms)],
        presence_announce: Vec::new(),
        presence_query: Vec::new(),
        own_endpoint_changed: false,
        // Neither scenario is about the sweep, and a sweep walking a mailbox
        // the script answers empty would only add noise to the transcript.
        swept_this_session: true,
        consecutive_rate_limits: 0,
        quiet_until_ms: 0,
        budgets,
    }
}

/// What the scripted relay answers this request with.
///
/// Keyed on the operation and the endpoint rather than on the path, so a
/// driver that mangled a path still gets the scripted answer and the mangling
/// shows up in the transcript comparison instead of being masked by a 404.
#[uniffi::export]
pub fn core_relay_fixture_reply(
    name: String,
    pass_index: u32,
    operation: CoreRelayOperation,
    endpoint: CoreRelayFixtureEndpoint,
) -> CoreRelayFixtureReply {
    reply_for(&name, pass_index, operation, endpoint)
}

fn reply_for(
    name: &str,
    pass_index: u32,
    operation: CoreRelayOperation,
    endpoint: CoreRelayFixtureEndpoint,
) -> CoreRelayFixtureReply {
    match name {
        // The mailbox is empty in both passes; everything the carry lane
        // offers is accepted. The incident is entirely about what the *second*
        // pass chooses to offer.
        "carry-storm" => match operation {
            CoreRelayOperation::FetchPage => CoreRelayFixtureReply::ok(empty_page()),
            _ => CoreRelayFixtureReply::ok(b"{}".to_vec()),
        },
        // Pass 1: this phone's own internet is down, so nothing answers and
        // the silence proves nothing. Pass 2: our mailbox answers and the
        // contact's still does not, which is the proof that licenses a rest.
        // Pass 3: the contact answers and the streak clears.
        "contact-silence-no-proof" => {
            let answers = match (pass_index, endpoint) {
                (0, _) => false,
                (1, CoreRelayFixtureEndpoint::Own) => true,
                (1, CoreRelayFixtureEndpoint::Contact) => false,
                _ => true,
            };
            if !answers {
                return CoreRelayFixtureReply::unreachable();
            }
            match operation {
                CoreRelayOperation::FetchPage => CoreRelayFixtureReply::ok(empty_page()),
                _ => CoreRelayFixtureReply::ok(b"{}".to_vec()),
            }
        }
        // Everything is accepted. What the transcript is about is how many
        // posts there were and where they went: three rows to the members'
        // mailbox in the first pass, none at all in the second.
        "group-fanout-complete" => match operation {
            CoreRelayOperation::FetchPage => CoreRelayFixtureReply::ok(empty_page()),
            _ => CoreRelayFixtureReply::ok(b"{}".to_vec()),
        },
        // Pass 2 is a mailbox that answers but will not take a row — a server
        // fault rather than a rejection of this particular message, so nothing
        // about the envelope is retired and nothing already landed is
        // forgotten. Reads keep working throughout, so the refusal cannot be
        // mistaken for the endpoint going quiet.
        "group-fanout-partial" => match (operation, pass_index) {
            (CoreRelayOperation::FetchPage, _) => CoreRelayFixtureReply::ok(empty_page()),
            (CoreRelayOperation::PostEnvelope, 1) => CoreRelayFixtureReply {
                status: 500,
                headers: Vec::new(),
                body: b"{}".to_vec(),
                transport_failure: false,
            },
            _ => CoreRelayFixtureReply::ok(b"{}".to_vec()),
        },
        other => panic!("no relay fixture reply script is wired for {other}"),
    }
}

fn empty_page() -> Vec<u8> {
    b"{\"envelopes\":[],\"next_cursor\":0}".to_vec()
}

// ---------------------------------------------------------------------------
// The normalised transcript
// ---------------------------------------------------------------------------

/// Accumulates one scenario's run into the text both platforms compare.
///
/// A UniFFI object rather than a returned string per step because the shells
/// must not be the ones choosing the shape: a Kotlin formatter and a Swift
/// formatter would differ on a separator eventually, and the difference would
/// read as a behaviour difference.
#[derive(uniffi::Object)]
pub struct CoreRelayFixtureTranscript {
    fixture: String,
    lines: Mutex<BTreeMap<(u32, u32), String>>,
    summaries: Mutex<BTreeMap<u32, String>>,
    next_line: Mutex<u32>,
}

#[uniffi::export]
impl CoreRelayFixtureTranscript {
    #[uniffi::constructor]
    pub fn new(fixture: String) -> Self {
        CoreRelayFixtureTranscript {
            fixture,
            lines: Mutex::new(BTreeMap::new()),
            summaries: Mutex::new(BTreeMap::new()),
            next_line: Mutex::new(0),
        }
    }

    /// One request, as core formed it and as the server received it.
    pub fn record_request(
        &self,
        pass_index: u32,
        request: CoreRelayHttpRequest,
        endpoint: CoreRelayFixtureEndpoint,
        observed: CoreRelayFixtureObservedRequest,
    ) {
        let expected_auth = request
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case("authorization"))
            .map(|header| header.value.clone());
        let auth = if observed.authorization == expected_auth {
            "echoed"
        } else {
            "ALTERED"
        };
        let line = format!(
            "  > {actor} {op} {method} {path} body={len} auth={auth}",
            actor = endpoint.pseudonym(),
            op = operation_name(request.operation),
            method = observed.method,
            path = normalize_path(&observed.path),
            len = observed.body_len,
        );
        self.push(pass_index, line);
    }

    /// What the driver reported for the request just recorded.
    pub fn record_result(&self, pass_index: u32, result: CoreRelayHttpResult) {
        let line = match result.error {
            None => format!("  < status={}", result.status),
            Some(error) => format!("  < status={} error={}", result.status, error_class(error)),
        };
        self.push(pass_index, line);
    }

    /// The pass's summary, folded to the fields a shell can affect.
    pub fn record_summary(
        &self,
        pass_index: u32,
        spec: CoreRelayFixturePassSpec,
        summary: CoreRelayPassSummary,
    ) {
        let quiet = if summary.quiet_until_ms == 0 {
            "none".to_string()
        } else {
            format!("+{}", summary.quiet_until_ms - spec.now_ms)
        };
        let text = format!(
            "  = outcome={outcome:?} requests={requests} envelopes={envelopes} \
             receipts={receipts} authored={authored} carried={carried} marked={marked} \
             ingested={ingested} acked={acked} advances={advances} held={held} \
             stale_ignored={stale} configs_walked={walked} configs_faulted={faulted} \
             silence_committed={committed} silence_discarded={discarded} quiet={quiet} \
             continuation={continuation}",
            outcome = summary.outcome,
            requests = summary.requests_issued,
            envelopes = summary.envelopes_processed,
            receipts = summary.receipt_uploads,
            authored = summary.authored_uploads,
            carried = summary.carried_uploads,
            marked = summary.carried_rows_marked,
            ingested = summary.rows_ingested,
            acked = summary.rows_acked,
            advances = summary.frontier_advances,
            held = summary.frontiers_held,
            stale = summary.stale_results_ignored,
            walked = summary.configs_walked,
            faulted = summary.configs_faulted,
            committed = summary.silence_committed,
            discarded = summary.silence_discarded,
            continuation = summary
                .continuation
                .map(|c| format!("{:?}", c.reason))
                .unwrap_or_else(|| "none".to_string()),
        );
        self.summaries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(pass_index, text);
    }

    /// The finished transcript: every pass in order, then what the store and
    /// the event ring are left holding.
    ///
    /// `own_url` and `own_token` are needed only to name the cursor row, which
    /// is keyed on the endpoint; neither reaches the text.
    pub fn finish(&self, store: Arc<MessageStore>, own_url: String, own_token: String) -> String {
        let lines = self
            .lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let summaries = self
            .summaries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let mut out = vec![format!("fixture {}", self.fixture)];
        let mut passes: Vec<u32> = lines.keys().map(|(pass, _)| *pass).collect();
        passes.extend(summaries.keys().copied());
        passes.sort_unstable();
        passes.dedup();
        for pass in passes {
            out.push(format!("pass {pass}"));
            for ((line_pass, _), text) in lines.iter() {
                if *line_pass == pass {
                    out.push(text.clone());
                }
            }
            if let Some(summary) = summaries.get(&pass) {
                out.push(summary.clone());
            }
        }

        let cursor = store
            .relay_fetch_cursor(relay_cursor_key(own_url, own_token))
            .expect("cursor");
        out.push(format!(
            "store carried={carried} unreachable={unreachable} pending_outbound={pending} \
             cursor_after={after} cursor_sweep={sweep}",
            carried = store.carried_len().expect("carry depth"),
            unreachable = store
                .list_contact_relay_unreachable()
                .expect("unreachable list")
                .len(),
            pending = store
                .pending_relay_outbound_envelopes(1_000, i64::MAX / 2, Vec::new())
                .expect("pending outbound")
                .len(),
            after = cursor.after_id,
            sweep = cursor.sweep_after_id,
        ));
        out.push(format!("events {}", emitted_codes(&store).join(",")));
        // Last, and the line a reader should look at first: every invariant the
        // session reported violated while this scenario ran. Empty is the
        // passing case, and it is inside the compared text rather than beside
        // it so a shell cannot produce a matching transcript while its store
        // recorded a violation the reference run did not.
        out.push(format!(
            "violations {}",
            core_relay_fixture_violated_invariants(store).join(",")
        ));
        out.join("\n")
    }
}

impl CoreRelayFixtureTranscript {
    fn push(&self, pass_index: u32, line: String) {
        let mut next = self
            .next_line
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let ordinal = *next;
        *next += 1;
        drop(next);
        self.lines
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert((pass_index, ordinal), line);
    }
}

/// The path with everything identifying stripped.
///
/// A fetch carries base64url recipient hints in its query. They are derived
/// from a user id and a clock, so they are stable — and printing them into a
/// transcript that gets pasted into a bug report is exactly the habit
/// `SECRET-01` exists to break. The `after` and `limit` a walk asked from are
/// the load-bearing part and stay.
fn normalize_path(path: &str) -> String {
    let (base, query) = match path.split_once('?') {
        Some(split) => split,
        None => return path.to_string(),
    };
    let kept: Vec<String> = query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            match key {
                "after" | "limit" => Some(format!("{key}={value}")),
                "hints" => Some("hints=<hints>".to_string()),
                _ => Some(format!("{key}=<redacted>")),
            }
        })
        .collect();
    format!("{base}?{}", kept.join("&"))
}

fn operation_name(operation: CoreRelayOperation) -> &'static str {
    match operation {
        CoreRelayOperation::PostEnvelope => "post_envelope",
        CoreRelayOperation::FetchPage => "fetch_page",
        CoreRelayOperation::AckPage => "ack_page",
        CoreRelayOperation::Presence => "presence",
    }
}

/// The transport failure class, coarsened to what two different fake
/// transports can agree on.
///
/// A socket disconnected before its status line is `CONNECTION_FAILED` on one
/// runtime and `TIMEOUT` on another, and neither is wrong; the distinction
/// core actually acts on is "no answer" versus "a page too big to take" versus
/// "we gave up on purpose". Coarsening here keeps the comparison about
/// behaviour rather than about which JVM the CI runner has. The exact mapping
/// stays pinned per platform by `CoreRelayDriverTest` and `RelaySyncDriverTests`.
fn error_class(error: CoreRelayTransportError) -> &'static str {
    match error {
        CoreRelayTransportError::Cancelled => "cancelled",
        CoreRelayTransportError::BodyTooLarge => "body_too_large",
        _ => "unreachable",
    }
}

fn emitted_codes(store: &MessageStore) -> Vec<String> {
    let text = store
        .export_protocol_events_jsonl()
        .expect("the ring exports as JSONL");
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_str(line).ok()?;
            value.get("code")?.as_str().map(str::to_string)
        })
        .collect()
}

/// Every invariant id a violation record named, for a suite to check the
/// fixture's declared ones against. Empty is the passing case.
#[uniffi::export]
pub fn core_relay_fixture_violated_invariants(store: Arc<MessageStore>) -> Vec<String> {
    let text = store
        .export_protocol_events_jsonl()
        .expect("the ring exports as JSONL");
    let mut violated = Vec::new();
    for line in text.lines().skip(1) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("code").and_then(|code| code.as_str()) != Some("invariant_violation") {
            continue;
        }
        if let Some(list) = value.get("invariants").and_then(|list| list.as_array()) {
            violated.extend(list.iter().filter_map(|id| id.as_str().map(str::to_string)));
        }
    }
    violated
}

// ---------------------------------------------------------------------------
// The reference run
// ---------------------------------------------------------------------------

/// The observed form of a request that reached the server unaltered — what the
/// reference run uses in place of a recording, and what a correct driver
/// produces.
#[uniffi::export]
pub fn core_relay_fixture_ideal_observation(
    request: CoreRelayHttpRequest,
) -> CoreRelayFixtureObservedRequest {
    CoreRelayFixtureObservedRequest {
        method: request.method.clone(),
        path: request.path.clone(),
        body_len: request.body.len() as u32,
        authorization: request
            .headers
            .iter()
            .find(|header| header.name.eq_ignore_ascii_case("authorization"))
            .map(|header| header.value.clone()),
    }
}

/// The transcript this scenario produces with no shell in the loop.
///
/// Same store seeding, same plan, same script, same normalisation — the only
/// thing replaced is the HTTP, which is answered from
/// [`core_relay_fixture_reply`] directly. A platform suite that produces a
/// different string has found a difference its driver introduced.
#[uniffi::export]
pub fn core_relay_fixture_expected_transcript(name: String) -> String {
    let scenario = scenario_of(&name);
    let store = Arc::new(MessageStore::open(":memory:".to_string()).expect("in-memory store"));
    seed_for(&name, &store, scenario.passes[0].now_ms);

    let transcript = CoreRelayFixtureTranscript::new(name.clone());

    for (index, spec) in scenario.passes.iter().enumerate() {
        let pass_index = index as u32;
        let plan = core_relay_fixture_plan(
            name.clone(),
            pass_index,
            REFERENCE_OWN_URL.to_string(),
            REFERENCE_OWN_TOKEN.to_string(),
            REFERENCE_CONTACT_URL.to_string(),
            REFERENCE_CONTACT_TOKEN.to_string(),
        );
        let pass = CoreRelayPass::new(store.clone(), plan, spec.label.clone());
        let mut action = pass.start(spec.now_ms);
        let mut issued = 0u32;
        loop {
            match action.kind {
                CoreRelayActionKind::Finished { summary } => {
                    transcript.record_summary(pass_index, spec.clone(), summary);
                    break;
                }
                CoreRelayActionKind::Sleep { .. } => {
                    let summary = pass.summary().unwrap_or_else(|| pass.cancel(spec.now_ms));
                    transcript.record_summary(pass_index, spec.clone(), summary);
                    break;
                }
                CoreRelayActionKind::NotStarted => {
                    let summary = pass.cancel(spec.now_ms);
                    transcript.record_summary(pass_index, spec.clone(), summary);
                    break;
                }
                CoreRelayActionKind::Http { request } => {
                    // The same guard the shell runners keep, for the same
                    // reason: a session that would not terminate must fail this
                    // in seconds rather than hang a CI job.
                    assert!(
                        issued < 4_096,
                        "LIVE-01: the reference run did not terminate"
                    );
                    issued += 1;
                    let endpoint = if request.base_url == REFERENCE_OWN_URL {
                        CoreRelayFixtureEndpoint::Own
                    } else {
                        CoreRelayFixtureEndpoint::Contact
                    };
                    let reply = reply_for(&name, pass_index, request.operation, endpoint);
                    transcript.record_request(
                        pass_index,
                        request.clone(),
                        endpoint,
                        core_relay_fixture_ideal_observation(request.clone()),
                    );
                    let result = CoreRelayHttpResult {
                        pass_id: action.pass_id.clone(),
                        action_id: action.action_id,
                        status: reply.status,
                        headers: reply.headers.clone(),
                        body: reply.body.clone(),
                        error: reply
                            .transport_failure
                            .then_some(CoreRelayTransportError::ConnectionFailed),
                        completed_at_ms: spec.now_ms,
                    };
                    transcript.record_result(pass_index, result.clone());
                    action = pass.resume_http(result);
                }
            }
        }
    }

    transcript.finish(
        store,
        REFERENCE_OWN_URL.to_string(),
        REFERENCE_OWN_TOKEN.to_string(),
    )
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

fn msg_id(seed: u64) -> Vec<u8> {
    let mut id = vec![0u8; 16];
    id[..8].copy_from_slice(&seed.to_be_bytes());
    id[8] = 0xA5;
    id
}

fn seed_contact(store: &MessageStore) {
    store
        .upsert_contact(Contact {
            user_id: contact_user_id(),
            name: "Contact".to_string(),
            sign_pk: vec![1u8; 32],
            agree_pk: vec![2u8; 32],
            relay_url: Some(REFERENCE_CONTACT_URL.to_string()),
            relay_token: Some(REFERENCE_CONTACT_TOKEN.to_string()),
            nickname: None,
        })
        .expect("upsert contact");
}

// --- the group the fan-out fixtures run on -------------------------------

/// Members are 16-byte user ids because that is what a group accepts, and they
/// are in ascending order because a group canonicalises its membership — the
/// plan below has to name the same members in the same order as the stored
/// group without reading the store, since a shell asks for a plan and a store
/// seeding independently.
const GROUP_MEMBERS: usize = 3;

fn group_member_user_id(index: usize) -> Vec<u8> {
    vec![0xB0 + index as u8; 16]
}

/// A fixed group id, not a generated one: `create_group` draws its id from the
/// OS random source, and a transcript whose recipient hint changed per run
/// would fail the determinism check the shells depend on.
fn fixture_group() -> crate::Group {
    crate::Group {
        id: vec![0x77u8; 16],
        name: "Cabin".to_string(),
        member_user_ids: (0..GROUP_MEMBERS).map(group_member_user_id).collect(),
        key: vec![0x33u8; 32],
        metadata_revision: 0,
        metadata_changed_by: Vec::new(),
    }
}

/// The group, its members as contacts, and one authored group-addressed
/// envelope waiting to go out.
///
/// The envelope's `recipient_user_id` is the group id, which is nobody's
/// contact entry — that is the whole shape the fan-out lane exists to handle.
fn seed_group_authored(store: &MessageStore, now_ms: i64) {
    let group = fixture_group();
    for (index, user_id) in group.member_user_ids.iter().enumerate() {
        store
            .upsert_contact(Contact {
                user_id: user_id.clone(),
                name: format!("Member {index}"),
                sign_pk: vec![1u8; 32],
                agree_pk: vec![2u8; 32],
                relay_url: Some(REFERENCE_CONTACT_URL.to_string()),
                relay_token: Some(REFERENCE_CONTACT_TOKEN.to_string()),
                nickname: None,
            })
            .expect("upsert member");
    }
    store.upsert_group(group.clone()).expect("upsert group");

    let expiry = now_ms + 6 * 24 * 60 * 60 * 1000;
    store
        .insert_outgoing_message(
            StoredMessage {
                chat_id: group.id.clone(),
                sender_user_id: own_user_id(),
                lamport: 1,
                timestamp: now_ms,
                kind: KIND_TEXT,
                payload: b"cabin at seven".to_vec(),
            },
            OutboundEnvelope {
                msg_id: msg_id(0x4000),
                recipient_user_id: group.id.clone(),
                chat_id: group.id.clone(),
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
        .expect("queue the group envelope");
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
                    // pattern would enqueue as one row rather than N.
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
