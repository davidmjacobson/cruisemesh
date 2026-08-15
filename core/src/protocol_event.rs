//! Stable, redacted protocol events, and the bounded ring that persists them.
//!
//! This is a release-build product output, not debug logging. When a family
//! member says "messages stopped arriving yesterday afternoon", the honest
//! answer has to come from what the device actually decided at the time, and
//! a prose log line is the wrong shape for that: it cannot be replayed, it
//! cannot be diffed against another phone, and nobody can promise it carries
//! no message text.
//!
//! So the decisions are recorded as *events*: a fixed set of codes, a
//! monotonic sequence, an explicit timestamp, non-negative counts, and one
//! short outcome token. Codes are API — a renamed code breaks the replay
//! command and the fixture corpus, and that is the point. Prose log messages
//! remain free to change.
//!
//! # Redaction is structural, not a review step
//!
//! Look at [`ProtocolEventDraft`]: every field a core call site fills in is
//! either a `&'static str` chosen at that call site (`outcome`, `invariants`,
//! count keys), a non-negative integer, or an actor name that only
//! [`actor_pseudonym`] can produce. There is no `Vec<u8>`, no payload, no
//! `String` a caller can pass through from the wire. A leak would require
//! adding a field, not making a mistake in one.
//!
//! One hook takes a caller-supplied outcome:
//! `MessageStore::note_invariant_violation`, which exists so a decision that
//! has not been hoisted into core yet can still report a broken invariant.
//! It is a `Cow` rather than a `&'static str`, and [`is_stable_token`] gates
//! it -- lowercase, digits and underscores, 48 bytes -- so the field can hold
//! a code and cannot hold a sentence.
//!
//! Two backstops sit behind that. [`redaction_defect`] scans the serialized
//! line for the same canaries the contract's fixture scanner uses, and
//! [`append`] refuses to store a line that trips one — it stores an
//! `invariant_violation` naming `SECRET-01` instead, so the *fact* that
//! something tried survives even though its contents do not.
//!
//! The closed key sets ([`PROTOCOL_EVENT_HEADER_KEYS`],
//! [`PROTOCOL_EVENT_RECORD_KEYS`]) are the structural half of the same rule,
//! and [`validate`] enforces them on every file it reads. The canary list
//! catches a leak that looks like something already known to be secret; this
//! catches one smuggled in under a field name nobody declared, which is the
//! shape a leak of ordinary message prose would actually take.
//!
//! # The ring
//!
//! FIFO, capped at both 2,000 events and 1 MiB, evicting oldest-first when
//! either cap is reached. Both caps are needed: 2,000 spray decisions are
//! small and 2,000 page ingests are not, and a diagnostics archive that a
//! family member is asked to send over ship wi-fi has a size budget as well
//! as a usefulness budget.
//!
//! Eviction is why the exported header carries `first_seq`. The sequence is
//! the store's own monotonic counter and is never renumbered on export — a
//! transcript that silently restarted at 1 after eviction would read as a
//! fresh device rather than as a device that dropped its oldest evidence.
//! `first_seq` is how the reader tells those apart, and it defaults to 1 when
//! absent, so every checked-in fixture means exactly what it did before this
//! field existed.
//!
//! Two properties make the ring safe to have at all. An [`append`] is atomic
//! — see [`with_savepoint`] — because rows committing while the sequence
//! counter does not would jam every later append on its primary key. And no
//! operational call site may fail because of it: they emit through [`note`]
//! and [`note_for`], which return nothing. A full disk costs a diagnostics
//! record, never a receipt, a page ingest, or the store opening at all.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use rusqlite::{params, Connection, OptionalExtension};

use crate::identity::CoreError;

/// The schema every record declares. Shared by the checked-in fixtures, the
/// simulation transcripts, and the archive a person exports from Advanced
/// diagnostics, so an exported archive needs no conversion step to be read.
pub const PROTOCOL_EVENT_SCHEMA: &str = "cruisemesh.protocol-event/v1";

/// FIFO cap on records. Reached first by cheap, frequent decisions.
pub const PROTOCOL_EVENT_MAX_RECORDS: i64 = 2_000;

/// FIFO cap on serialized bytes. Reached first by rich page/pass records.
pub const PROTOCOL_EVENT_MAX_BYTES: i64 = 1_048_576;

/// A single record that serializes larger than this is a bug in a call site,
/// not a legitimate event: the schema has no free-text field, so the only way
/// to reach it is an unbounded `counts` map. Refusing it keeps eviction
/// bounded (a single append can never evict more than a handful of records).
const MAX_RECORD_BYTES: usize = 2_048;

/// The name the exported archive carries, and the stem the replay command
/// reports. Kept in Rust rather than in either shell's resources: it is a
/// file identifier inside a diagnostics zip, not user-facing copy.
pub const PROTOCOL_EVENT_ARCHIVE_STEM: &str = "cruisemesh-protocol-events";

/// The header title of an exported archive. One line, for a stranger — which
/// in practice means whoever at the other end of a support thread opens the
/// zip.
const ARCHIVE_TITLE: &str =
    "Redacted protocol-event ring exported from a CruiseMesh device by hand";

// ---------------------------------------------------------------------------
// Codes
// ---------------------------------------------------------------------------

/// Stable event codes.
///
/// Adding a code is additive and safe. Renaming or removing one is a contract
/// change: it breaks `specs/protocol-contract-v1.md`, the fixture corpus, and
/// every archive already sitting in somebody's mail.
///
/// Some codes here have no emitter yet. They are still API: the fixture
/// corpus uses them to describe incidents that predate this module, and the
/// packages that will emit them are named in the contract's code table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProtocolEventCode {
    ActionEmitted,
    ActionResultAccepted,
    ActionResultStaleIgnored,
    BudgetYield,
    CarriedRowEvicted,
    CarriedRowMarked,
    CarryAdmissionRejected,
    ContinuationScheduled,
    EndpointRecovered,
    EndpointRested,
    FrontierAdvanced,
    FrontierHeld,
    FrontierLowered,
    InvariantViolation,
    OutboundQueueScanned,
    OutboundRowRetired,
    OutboundRowSuperseded,
    PageIngested,
    PassFinish,
    PassStart,
    RateLimitAbort,
    ReceiptWatermarkObserved,
    RequestRejected,
    ShadowMismatch,
    SilenceObserved,
    SprayAdmitted,
    SprayBudgetExhausted,
    SprayDeferred,
    SprayPlanned,
    SpraySuppressed,
    SweepCompleted,
    SweepRestarted,
    SweepResumed,
    SweepStarted,
}

impl ProtocolEventCode {
    /// Every code, in the order the contract's table lists them.
    pub const ALL: &'static [ProtocolEventCode] = &[
        ProtocolEventCode::ActionEmitted,
        ProtocolEventCode::ActionResultAccepted,
        ProtocolEventCode::ActionResultStaleIgnored,
        ProtocolEventCode::BudgetYield,
        ProtocolEventCode::CarriedRowEvicted,
        ProtocolEventCode::CarriedRowMarked,
        ProtocolEventCode::CarryAdmissionRejected,
        ProtocolEventCode::ContinuationScheduled,
        ProtocolEventCode::EndpointRecovered,
        ProtocolEventCode::EndpointRested,
        ProtocolEventCode::FrontierAdvanced,
        ProtocolEventCode::FrontierHeld,
        ProtocolEventCode::FrontierLowered,
        ProtocolEventCode::InvariantViolation,
        ProtocolEventCode::OutboundQueueScanned,
        ProtocolEventCode::OutboundRowRetired,
        ProtocolEventCode::OutboundRowSuperseded,
        ProtocolEventCode::PageIngested,
        ProtocolEventCode::PassFinish,
        ProtocolEventCode::PassStart,
        ProtocolEventCode::RateLimitAbort,
        ProtocolEventCode::ReceiptWatermarkObserved,
        ProtocolEventCode::RequestRejected,
        ProtocolEventCode::ShadowMismatch,
        ProtocolEventCode::SilenceObserved,
        ProtocolEventCode::SprayAdmitted,
        ProtocolEventCode::SprayBudgetExhausted,
        ProtocolEventCode::SprayDeferred,
        ProtocolEventCode::SprayPlanned,
        ProtocolEventCode::SpraySuppressed,
        ProtocolEventCode::SweepCompleted,
        ProtocolEventCode::SweepRestarted,
        ProtocolEventCode::SweepResumed,
        ProtocolEventCode::SweepStarted,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            ProtocolEventCode::ActionEmitted => "action_emitted",
            ProtocolEventCode::ActionResultAccepted => "action_result_accepted",
            ProtocolEventCode::ActionResultStaleIgnored => "action_result_stale_ignored",
            ProtocolEventCode::BudgetYield => "budget_yield",
            ProtocolEventCode::CarriedRowEvicted => "carried_row_evicted",
            ProtocolEventCode::CarriedRowMarked => "carried_row_marked",
            ProtocolEventCode::CarryAdmissionRejected => "carry_admission_rejected",
            ProtocolEventCode::ContinuationScheduled => "continuation_scheduled",
            ProtocolEventCode::EndpointRecovered => "endpoint_recovered",
            ProtocolEventCode::EndpointRested => "endpoint_rested",
            ProtocolEventCode::FrontierAdvanced => "frontier_advanced",
            ProtocolEventCode::FrontierHeld => "frontier_held",
            ProtocolEventCode::FrontierLowered => "frontier_lowered",
            ProtocolEventCode::InvariantViolation => "invariant_violation",
            ProtocolEventCode::OutboundQueueScanned => "outbound_queue_scanned",
            ProtocolEventCode::OutboundRowRetired => "outbound_row_retired",
            ProtocolEventCode::OutboundRowSuperseded => "outbound_row_superseded",
            ProtocolEventCode::PageIngested => "page_ingested",
            ProtocolEventCode::PassFinish => "pass_finish",
            ProtocolEventCode::PassStart => "pass_start",
            ProtocolEventCode::RateLimitAbort => "rate_limit_abort",
            ProtocolEventCode::ReceiptWatermarkObserved => "receipt_watermark_observed",
            ProtocolEventCode::RequestRejected => "request_rejected",
            ProtocolEventCode::ShadowMismatch => "shadow_mismatch",
            ProtocolEventCode::SilenceObserved => "silence_observed",
            ProtocolEventCode::SprayAdmitted => "spray_admitted",
            ProtocolEventCode::SprayBudgetExhausted => "spray_budget_exhausted",
            ProtocolEventCode::SprayDeferred => "spray_deferred",
            ProtocolEventCode::SprayPlanned => "spray_planned",
            ProtocolEventCode::SpraySuppressed => "spray_suppressed",
            ProtocolEventCode::SweepCompleted => "sweep_completed",
            ProtocolEventCode::SweepRestarted => "sweep_restarted",
            ProtocolEventCode::SweepResumed => "sweep_resumed",
            ProtocolEventCode::SweepStarted => "sweep_started",
        }
    }

    pub fn from_code(code: &str) -> Option<ProtocolEventCode> {
        ProtocolEventCode::ALL
            .iter()
            .copied()
            .find(|candidate| candidate.as_str() == code)
    }
}

/// Every stable event code, sorted. The contract test asserts this equals the
/// document's table, so neither can drift without the other going red.
pub fn protocol_event_codes() -> Vec<&'static str> {
    ProtocolEventCode::ALL
        .iter()
        .map(|code| code.as_str())
        .collect()
}

// ---------------------------------------------------------------------------
// Invariant ids
// ---------------------------------------------------------------------------

/// The Contract v1 invariant ids, so the replay command can check an event's
/// declared ids without linking the test crate.
///
/// `core/tests/protocol_contract.rs` asserts this list equals its registry —
/// the registry stays the index of record, and this is a mirror pinned by a
/// test rather than by discipline.
pub const PROTOCOL_INVARIANT_IDS: &[&str] = &[
    "ACK-01",
    "ACK-02",
    "CARRY-01",
    "CARRY-02",
    "CURSOR-01",
    "DEDUP-01",
    "EVICT-01",
    "ENDPOINT-01",
    "FANOUT-01",
    "HELLO-01",
    "IDEMP-01",
    "LIVE-01",
    "MARK-01",
    "PAGE-01",
    "PROGRESS-01",
    "QUEUE-01",
    "RATE-01",
    "SECRET-01",
    "SILENCE-01",
    "SPRAY-01",
    "TXN-01",
    "UI-01",
    "WM-01",
];

pub fn is_known_invariant(id: &str) -> bool {
    PROTOCOL_INVARIANT_IDS.contains(&id)
}

// ---------------------------------------------------------------------------
// Drafts and events
// ---------------------------------------------------------------------------

/// What a core decision point fills in. Everything here is either a constant
/// chosen at the call site or a number, so a payload has no field to arrive
/// in. See the module docs.
#[derive(Clone, Debug)]
pub struct ProtocolEventDraft {
    pub code: ProtocolEventCode,
    pub at_ms: i64,
    /// Opaque short pass id. `None` until `CoreRelayPass` exists to mint one.
    pub pass: Option<String>,
    /// Opaque short session id, same story.
    pub session: Option<String>,
    pub action: Option<i64>,
    /// An archive-local pseudonym from [`actor_pseudonym`], never a raw id.
    pub actor: Option<String>,
    pub invariants: Vec<&'static str>,
    pub counts: Vec<(&'static str, i64)>,
    pub outcome: Cow<'static, str>,
}

impl ProtocolEventDraft {
    pub fn new(code: ProtocolEventCode, at_ms: i64, outcome: &'static str) -> Self {
        ProtocolEventDraft {
            code,
            at_ms,
            pass: None,
            session: None,
            action: None,
            actor: None,
            invariants: Vec::new(),
            counts: Vec::new(),
            outcome: Cow::Borrowed(outcome),
        }
    }

    /// The one entry point for an outcome that is not a compile-time
    /// constant. Refuses anything that is not a short stable token, so the
    /// caller cannot widen the field by passing a longer string.
    pub fn with_checked_outcome(
        code: ProtocolEventCode,
        at_ms: i64,
        outcome: &str,
    ) -> Option<Self> {
        if !is_stable_token(outcome) {
            return None;
        }
        let mut draft = ProtocolEventDraft::new(code, at_ms, "");
        draft.outcome = Cow::Owned(outcome.to_string());
        Some(draft)
    }

    pub fn actor(mut self, actor: String) -> Self {
        self.actor = Some(actor);
        self
    }

    /// Tag every record a relay pass emits with that pass's opaque id, so a
    /// transcript can be read one pass at a time. Rejected rather than
    /// truncated if it is not a short opaque token — [`sanitized_line`] would
    /// refuse the record outright, and losing the whole event because an id
    /// was malformed is worse than losing the id.
    pub fn pass(mut self, pass: String) -> Self {
        if is_opaque_id(&pass) {
            self.pass = Some(pass);
        }
        self
    }

    pub fn action(mut self, action: i64) -> Self {
        self.action = Some(action);
        self
    }

    pub fn invariants(mut self, ids: &[&'static str]) -> Self {
        self.invariants = ids.to_vec();
        self
    }

    pub fn count(mut self, key: &'static str, value: i64) -> Self {
        self.counts.push((key, value));
        self
    }
}

/// A parsed record, as the replay command sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolEvent {
    pub seq: i64,
    pub at_ms: i64,
    /// True when `at_ms` was borrowed from the record before this one rather
    /// than measured. Absent in a file means false — a record that says
    /// nothing about its clock was told the time.
    pub inferred_at: bool,
    pub code: ProtocolEventCode,
    pub session: Option<String>,
    pub pass: Option<String>,
    pub action: Option<i64>,
    pub actor: Option<String>,
    pub invariants: Vec<String>,
    pub counts: BTreeMap<String, i64>,
    pub outcome: Option<String>,
}

/// An archive or fixture header.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolEventHeader {
    pub fixture: String,
    pub title: String,
    pub origin: String,
    pub public_reference: Option<String>,
    pub pseudonyms: Vec<String>,
    pub expect_invariants: Vec<String>,
    /// The sequence number of the first surviving record. 1 unless the ring
    /// evicted; absent in a file means 1.
    pub first_seq: i64,
}

impl ProtocolEventHeader {
    pub fn is_field_archive(&self) -> bool {
        self.origin == "redacted-field-archive"
    }
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("a string always serializes")
}

/// Serialize one record. Keys are emitted in schema order rather than
/// alphabetically, because a person reads these files far more often than a
/// parser does, and empty fields are omitted rather than sent as `null`.
fn draft_to_line(seq: i64, draft: &ProtocolEventDraft, inferred_at: bool) -> String {
    let mut line = String::with_capacity(160);
    line.push_str("{\"record\":\"event\",\"seq\":");
    line.push_str(&seq.to_string());
    line.push_str(",\"at_ms\":");
    line.push_str(&draft.at_ms.to_string());
    if inferred_at {
        line.push_str(",\"inferred_at\":true");
    }
    line.push_str(",\"code\":");
    line.push_str(&json_string(draft.code.as_str()));
    if let Some(session) = &draft.session {
        line.push_str(",\"session\":");
        line.push_str(&json_string(session));
    }
    if let Some(pass) = &draft.pass {
        line.push_str(",\"pass\":");
        line.push_str(&json_string(pass));
    }
    if let Some(action) = draft.action {
        line.push_str(",\"action\":");
        line.push_str(&action.to_string());
    }
    if let Some(actor) = &draft.actor {
        line.push_str(",\"actor\":");
        line.push_str(&json_string(actor));
    }
    if !draft.invariants.is_empty() {
        line.push_str(",\"invariants\":[");
        for (index, id) in draft.invariants.iter().enumerate() {
            if index > 0 {
                line.push(',');
            }
            line.push_str(&json_string(id));
        }
        line.push(']');
    }
    if !draft.counts.is_empty() {
        line.push_str(",\"counts\":{");
        for (index, (key, value)) in draft.counts.iter().enumerate() {
            if index > 0 {
                line.push(',');
            }
            line.push_str(&json_string(key));
            line.push(':');
            line.push_str(&value.to_string());
        }
        line.push('}');
    }
    if !draft.outcome.is_empty() {
        line.push_str(",\"outcome\":");
        line.push_str(&json_string(&draft.outcome));
    }
    line.push('}');
    line
}

fn header_to_line(header: &ProtocolEventHeader) -> String {
    let mut line = String::with_capacity(256);
    line.push_str("{\"schema\":");
    line.push_str(&json_string(PROTOCOL_EVENT_SCHEMA));
    line.push_str(",\"record\":\"header\",\"fixture\":");
    line.push_str(&json_string(&header.fixture));
    line.push_str(",\"title\":");
    line.push_str(&json_string(&header.title));
    line.push_str(",\"origin\":");
    line.push_str(&json_string(&header.origin));
    if let Some(reference) = &header.public_reference {
        line.push_str(",\"public_reference\":");
        line.push_str(&json_string(reference));
    }
    line.push_str(",\"pseudonyms\":[");
    for (index, name) in header.pseudonyms.iter().enumerate() {
        if index > 0 {
            line.push(',');
        }
        line.push_str(&json_string(name));
    }
    line.push_str("],\"expect_invariants\":[");
    for (index, id) in header.expect_invariants.iter().enumerate() {
        if index > 0 {
            line.push(',');
        }
        line.push_str(&json_string(id));
    }
    line.push(']');
    if header.first_seq != 1 {
        line.push_str(",\"first_seq\":");
        line.push_str(&header.first_seq.to_string());
    }
    line.push('}');
    line
}

// ---------------------------------------------------------------------------
// Redaction canaries
// ---------------------------------------------------------------------------

/// The same list the contract's fixture scanner uses. Kept here so the ring
/// can refuse a record *before* it is stored, rather than discovering the
/// problem when somebody runs the tests.
pub const REDACTION_CANARIES: &[(&str, &str)] = &[
    ("cmdep1-", "a deposit-class relay token"),
    ("CMFRIEND", "a raw friend card"),
    ("cruisemesh://", "a friend deep link"),
    ("://", "an endpoint-bearing URL"),
    ("Authorization", "an authorization header"),
    ("Bearer ", "a bearer credential"),
    ("-----BEGIN", "PEM-encoded key material"),
    ("192.168.", "a private address literal"),
    ("10.0.0.", "a private address literal"),
    ("172.16.", "a private address literal"),
    ("fe80:", "a link-local address literal"),
];

/// `None` when the text is clean; otherwise what it leaked.
pub fn redaction_defect(text: &str) -> Option<&'static str> {
    REDACTION_CANARIES
        .iter()
        .find(|(canary, _)| text.contains(canary))
        .map(|(_, what)| *what)
}

/// Every key a header line may carry, and the whole list of them.
///
/// The closed key set is half of `SECRET-01`, and the more important half: the
/// canary scan catches a leak that looks like something already known to be
/// secret, and this catches a leak smuggled in under a field name nobody
/// declared. Both halves have to run over real exported archives, not only
/// over the checked-in corpus, which is why the lists live here beside
/// [`validate`] rather than in the test that reads the fixtures.
pub const PROTOCOL_EVENT_HEADER_KEYS: &[&str] = &[
    "schema",
    "record",
    "fixture",
    "title",
    "origin",
    "public_reference",
    "pseudonyms",
    "expect_invariants",
    "first_seq",
];

/// Every key an event line may carry. See [`PROTOCOL_EVENT_HEADER_KEYS`].
pub const PROTOCOL_EVENT_RECORD_KEYS: &[&str] = &[
    "record",
    "seq",
    "at_ms",
    "inferred_at",
    "code",
    "session",
    "pass",
    "action",
    "actor",
    "invariants",
    "counts",
    "outcome",
];

/// Keys in one JSONL line that the schema does not declare.
///
/// A line this cannot parse reports nothing; the caller's parse step is what
/// reports that, and reporting it twice would be noise.
fn foreign_keys(line: &str, allowed: &[&str]) -> Vec<String> {
    let Ok(serde_json::Value::Object(object)) = serde_json::from_str::<serde_json::Value>(line)
    else {
        return Vec::new();
    };
    object
        .into_iter()
        .map(|(key, _)| key)
        .filter(|key| !allowed.contains(&key.as_str()))
        .collect()
}

/// Short stable tokens only: lowercase, digits, underscore, at most 48 bytes.
/// This is the shape rule that stops an outcome from becoming a sentence, and
/// a sentence from becoming a place to put a message body.
pub fn is_stable_token(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 48
        && text
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

/// Pass and session ids may also carry digits and hyphens (`p1`, `s-3`).
pub(crate) fn is_opaque_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 24
        && text.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

// ---------------------------------------------------------------------------
// The persisted ring
// ---------------------------------------------------------------------------

/// Tables for the ring. Appended to the store's schema; created by
/// `MessageStore::open` like everything else, and forward-only — an older
/// build reading this store simply ignores three tables it does not know.
pub(crate) const PROTOCOL_EVENT_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS protocol_events (
    seq   INTEGER PRIMARY KEY,
    at_ms INTEGER NOT NULL,
    bytes INTEGER NOT NULL,
    line  TEXT    NOT NULL
);
CREATE TABLE IF NOT EXISTS protocol_event_state (
    id          INTEGER PRIMARY KEY CHECK (id = 0),
    next_seq    INTEGER NOT NULL,
    first_seq   INTEGER NOT NULL,
    total_bytes INTEGER NOT NULL,
    next_actor  INTEGER NOT NULL,
    last_at_ms  INTEGER NOT NULL,
    salt        BLOB    NOT NULL
);
CREATE TABLE IF NOT EXISTS protocol_event_actors (
    actor_key TEXT PRIMARY KEY,
    name      TEXT NOT NULL
);
";

#[derive(Clone, Copy, Debug)]
struct RingState {
    next_seq: i64,
    first_seq: i64,
    total_bytes: i64,
    next_actor: i64,
    /// The timestamp of the newest record. Every append clamps forward to it,
    /// which is what makes "time never runs backwards" a property of the ring
    /// rather than a hope about every caller's clock. Two things make that
    /// necessary rather than defensive: a device's wall clock can step
    /// backwards (a time-zone change at sea, an NTP correction), and some
    /// decision points genuinely have no clock in hand -- `MessageStore::open`
    /// runs before anything has told core what time it is. Such a record reads
    /// as "no earlier than the one before it", which is exactly what is known.
    last_at_ms: i64,
}

fn store_err(error: rusqlite::Error) -> CoreError {
    CoreError::Store(error.to_string())
}

/// Read the ring's bookkeeping row, creating it on first use.
///
/// The salt is the reason the row exists before the first event does: actor
/// pseudonyms are `blake2(salt || raw id)` truncated and numbered, so they are
/// stable within one device's archive and meaningless across two. A hash with
/// no salt would still be a stable identifier — the same contact would produce
/// the same string on every phone that knows them — which is exactly what
/// section 6.4 of the contract forbids.
fn ring_state(conn: &Connection) -> Result<RingState, CoreError> {
    let row: Option<(i64, i64, i64, i64, i64)> = conn
        .query_row(
            "SELECT next_seq, first_seq, total_bytes, next_actor, last_at_ms
             FROM protocol_event_state WHERE id = 0",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(store_err)?;
    if let Some((next_seq, first_seq, total_bytes, next_actor, last_at_ms)) = row {
        return Ok(RingState {
            next_seq,
            first_seq,
            total_bytes,
            next_actor,
            last_at_ms,
        });
    }

    let mut salt = [0u8; 16];
    rand_core::RngCore::fill_bytes(&mut rand_core::OsRng, &mut salt);
    conn.execute(
        "INSERT INTO protocol_event_state
             (id, next_seq, first_seq, total_bytes, next_actor, last_at_ms, salt)
         VALUES (0, 1, 1, 0, 1, 0, ?1)",
        params![salt.to_vec()],
    )
    .map_err(store_err)?;
    Ok(RingState {
        next_seq: 1,
        first_seq: 1,
        total_bytes: 0,
        next_actor: 1,
        last_at_ms: 0,
    })
}

/// Run `work` so that either all of its statements land or none of them do.
///
/// A savepoint rather than a transaction because both are true of these call
/// sites: some hand us a raw connection in autocommit mode, and some hand us
/// one already inside their own transaction. A savepoint nests under either.
///
/// This is not tidiness. The ring's row inserts and its bookkeeping row are a
/// read-modify-write of one counter, and if the rows commit while the counter
/// does not, `next_seq` is left below `MAX(seq) + 1` and every later append
/// fails its primary key — permanently, on a store that is otherwise fine.
/// [`reconciled_ring_state`] can repair that state; this is what stops it
/// being created.
fn with_savepoint<T>(
    conn: &Connection,
    work: impl FnOnce(&Connection) -> Result<T, CoreError>,
) -> Result<T, CoreError> {
    conn.execute_batch("SAVEPOINT cruisemesh_protocol_event")
        .map_err(store_err)?;
    match work(conn) {
        Ok(value) => {
            conn.execute_batch("RELEASE cruisemesh_protocol_event")
                .map_err(store_err)?;
            Ok(value)
        }
        Err(error) => {
            // The rollback's own failure must not replace the real reason.
            let _ = conn.execute_batch(
                "ROLLBACK TO cruisemesh_protocol_event; RELEASE cruisemesh_protocol_event;",
            );
            Err(error)
        }
    }
}

/// The bookkeeping row, checked against the table it describes and repaired if
/// they disagree.
///
/// `MIN(seq)` and `MAX(seq)` on an `INTEGER PRIMARY KEY` are two index seeks,
/// so the healthy path costs the same on a full ring as on an empty one and
/// the expensive `COUNT`/`SUM` repair runs only when something is actually
/// wrong. Something can be wrong despite [`with_savepoint`]: a store written
/// by an older build, a file restored from a backup, or a row somebody deleted
/// by hand. The ring recovering by itself matters more here than in most
/// places, because the alternative is a diagnostics table jamming the store
/// methods that emit into it.
fn reconciled_ring_state(conn: &Connection) -> Result<RingState, CoreError> {
    let state = ring_state(conn)?;
    let (min_seq, max_seq): (Option<i64>, Option<i64>) = conn
        .query_row(
            "SELECT MIN(seq), MAX(seq) FROM protocol_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(store_err)?;

    let agrees = match (min_seq, max_seq) {
        (Some(min), Some(max)) => state.first_seq == min && state.next_seq == max.saturating_add(1),
        _ => state.first_seq == state.next_seq && state.total_bytes == 0,
    };
    if agrees {
        return Ok(state);
    }

    let (bytes, newest): (i64, i64) = conn
        .query_row(
            "SELECT COALESCE(SUM(bytes), 0), COALESCE(MAX(at_ms), 0) FROM protocol_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(store_err)?;
    // A sequence never runs backwards, even to repair itself: an archive
    // already exported carries the numbers it carried.
    let next_seq = max_seq.map_or(state.next_seq, |max| {
        state.next_seq.max(max.saturating_add(1))
    });
    let repaired = RingState {
        next_seq,
        first_seq: min_seq.unwrap_or(next_seq),
        total_bytes: bytes,
        next_actor: state.next_actor,
        last_at_ms: state.last_at_ms.max(newest),
    };
    write_ring_state(conn, repaired)?;
    Ok(repaired)
}

fn write_ring_state(conn: &Connection, state: RingState) -> Result<(), CoreError> {
    conn.execute(
        "UPDATE protocol_event_state
         SET next_seq = ?1, first_seq = ?2, total_bytes = ?3, next_actor = ?4, last_at_ms = ?5
         WHERE id = 0",
        params![
            state.next_seq,
            state.first_seq,
            state.total_bytes,
            state.next_actor,
            state.last_at_ms
        ],
    )
    .map_err(store_err)?;
    Ok(())
}

/// The archive-local pseudonym for one raw identifier — a contact user id, a
/// relay mailbox config key, a link address. Allocated in first-seen order and
/// remembered, so the same peer reads as the same `peer-3` throughout an
/// archive and a reader can follow one conversation's story.
///
/// `kind` is the prefix (`peer`, `mailbox`, `link`). The raw bytes never leave
/// this function: only their salted hash is stored, and only the name is
/// returned.
pub(crate) fn actor_pseudonym(
    conn: &Connection,
    kind: &'static str,
    raw: &[u8],
) -> Result<String, CoreError> {
    with_savepoint(conn, |conn| actor_pseudonym_inner(conn, kind, raw))
}

fn actor_pseudonym_inner(
    conn: &Connection,
    kind: &'static str,
    raw: &[u8],
) -> Result<String, CoreError> {
    let state = ring_state(conn)?;
    let salt: Vec<u8> = conn
        .query_row(
            "SELECT salt FROM protocol_event_state WHERE id = 0",
            [],
            |row| row.get(0),
        )
        .map_err(store_err)?;

    let mut hasher = Blake2bVar::new(16).expect("16 is a valid blake2b output length");
    hasher.update(b"cruisemesh/protocol-event/actor");
    hasher.update(&salt);
    hasher.update(kind.as_bytes());
    hasher.update(raw);
    let mut digest = [0u8; 16];
    hasher
        .finalize_variable(&mut digest)
        .expect("16 bytes fits the requested length");
    let actor_key = data_encoding::HEXLOWER.encode(&digest);

    if let Some(name) = conn
        .query_row(
            "SELECT name FROM protocol_event_actors WHERE actor_key = ?1",
            params![actor_key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(store_err)?
    {
        return Ok(name);
    }

    let name = format!("{kind}-{}", state.next_actor);
    conn.execute(
        "INSERT INTO protocol_event_actors (actor_key, name) VALUES (?1, ?2)",
        params![actor_key, name],
    )
    .map_err(store_err)?;
    // Only the actor counter, rather than the whole bookkeeping row: naming a
    // pseudonym has nothing to say about the sequence, and writing back the
    // values this function happened to read would let it undo a repair
    // [`reconciled_ring_state`] had just made.
    conn.execute(
        "UPDATE protocol_event_state SET next_actor = ?1 WHERE id = 0",
        params![state.next_actor.saturating_add(1)],
    )
    .map_err(store_err)?;
    Ok(name)
}

/// Emit from an operational call site, and never let the ring be the reason
/// anything else fails.
///
/// This is the rule for the whole subsystem, and the reason it is a function
/// rather than a convention: a diagnostics ring that could roll back a
/// receipt, refuse a page ingest, fail message authoring, or stop the store
/// opening at all would be far worse than no ring. [`append`] does its work
/// inside a savepoint, so a failure here leaves the caller's own transaction
/// exactly as it was.
///
/// Explicit diagnostics entry points — `record_protocol_events`,
/// `note_invariant_violation` — keep their `Result`: there the ring *is* the
/// operation, and its caller is the one entitled to hear about a failure.
pub(crate) fn note(conn: &Connection, drafts: &[ProtocolEventDraft]) {
    let _ = append(conn, drafts);
}

/// [`note`], for the common case of one pseudonym shared by the batch.
///
/// Allocating a pseudonym is itself a write, so it can fail for the same
/// reasons an append can; folding it in here keeps every operational call site
/// down to one infallible line.
pub(crate) fn note_for(
    conn: &Connection,
    kind: &'static str,
    raw: &[u8],
    build: impl FnOnce(String) -> Vec<ProtocolEventDraft>,
) {
    let Ok(actor) = actor_pseudonym(conn, kind, raw) else {
        return;
    };
    note(conn, &build(actor));
}

/// Append a batch of records, evicting oldest-first until both caps hold.
///
/// One savepoint for the whole batch — rows, evictions and the bookkeeping row
/// land together or not at all — and nothing in it waits on anything: the
/// callers already hold the store's connection lock when they reach here, and
/// the work is a handful of single-row statements against a table capped at
/// 2,000 rows. That is the whole performance contract — this store has an ANR
/// history, so a ring write that could block a page ingest would be worse than
/// no ring at all.
///
/// A record that fails its own shape or redaction checks is not stored.
/// Whatever it was, it is replaced by an `invariant_violation` naming
/// `SECRET-01`, which carries no counts and no actor. Dropping it silently
/// would hide the one failure the ring exists to make visible.
pub(crate) fn append(conn: &Connection, drafts: &[ProtocolEventDraft]) -> Result<(), CoreError> {
    if drafts.is_empty() {
        return Ok(());
    }
    with_savepoint(conn, |conn| append_inner(conn, drafts))
}

fn append_inner(conn: &Connection, drafts: &[ProtocolEventDraft]) -> Result<(), CoreError> {
    let mut state = reconciled_ring_state(conn)?;

    {
        let mut insert = conn
            .prepare_cached(
                "INSERT INTO protocol_events (seq, at_ms, bytes, line) VALUES (?1, ?2, ?3, ?4)",
            )
            .map_err(store_err)?;
        for draft in drafts {
            let seq = state.next_seq;
            // A call site with no clock in hand passes 0. The stored time is
            // then borrowed from the record before it, which keeps the ring
            // ordered — but a borrowed timestamp read as a measured one is a
            // support reader being quietly misled about when something
            // happened, so the record says which it is.
            let inferred = draft.at_ms <= 0;
            let at_ms = draft.at_ms.max(0).max(state.last_at_ms);
            let clamped = ProtocolEventDraft {
                at_ms,
                ..draft.clone()
            };
            let line = match sanitized_line(seq, &clamped, inferred) {
                Ok(line) => line,
                Err(reason) => draft_to_line(
                    seq,
                    &ProtocolEventDraft::new(ProtocolEventCode::InvariantViolation, at_ms, reason)
                        .invariants(&["SECRET-01"]),
                    inferred,
                ),
            };
            let bytes = line.len() as i64;
            insert
                .execute(params![seq, at_ms, bytes, line])
                .map_err(store_err)?;
            state.next_seq = seq.saturating_add(1);
            state.last_at_ms = at_ms;
            state.total_bytes = state.total_bytes.saturating_add(bytes);
        }
    }

    evict(conn, &mut state)?;
    write_ring_state(conn, state)?;
    Ok(())
}

/// Serialize a draft, or say why it may not be stored.
fn sanitized_line(
    seq: i64,
    draft: &ProtocolEventDraft,
    inferred_at: bool,
) -> Result<String, &'static str> {
    if !is_stable_token(&draft.outcome) {
        return Err("outcome_not_a_stable_token");
    }
    if draft.counts.iter().any(|(_, value)| *value < 0) {
        return Err("count_was_negative");
    }
    if !draft.counts.iter().all(|(key, _)| is_stable_token(key)) {
        return Err("count_key_not_a_stable_token");
    }
    if draft.invariants.iter().any(|id| !is_known_invariant(id)) {
        return Err("unknown_invariant_id");
    }
    if let Some(actor) = &draft.actor {
        if !is_opaque_id(actor) {
            return Err("actor_not_a_pseudonym");
        }
    }
    for id in [&draft.pass, &draft.session].into_iter().flatten() {
        if !is_opaque_id(id) {
            return Err("id_not_opaque");
        }
    }
    let line = draft_to_line(seq, draft, inferred_at);
    if line.len() > MAX_RECORD_BYTES {
        return Err("record_over_size_cap");
    }
    if redaction_defect(&line).is_some() {
        return Err("record_tripped_a_redaction_canary");
    }
    Ok(line)
}

/// Delete oldest-first until both caps hold. Deterministic by construction:
/// `seq` is the insertion order and the only ordering used.
fn evict(conn: &Connection, state: &mut RingState) -> Result<(), CoreError> {
    let mut oldest = conn
        .prepare_cached("SELECT seq, bytes FROM protocol_events ORDER BY seq ASC LIMIT 1")
        .map_err(store_err)?;
    let mut delete = conn
        .prepare_cached("DELETE FROM protocol_events WHERE seq = ?1")
        .map_err(store_err)?;

    loop {
        let records = state.next_seq - state.first_seq;
        if records <= PROTOCOL_EVENT_MAX_RECORDS && state.total_bytes <= PROTOCOL_EVENT_MAX_BYTES {
            break;
        }
        let row: Option<(i64, i64)> = oldest
            .query_row([], |row| Ok((row.get(0)?, row.get(1)?)))
            .optional()
            .map_err(store_err)?;
        let Some((seq, bytes)) = row else {
            // Nothing left to evict; trust the table over the counters.
            state.first_seq = state.next_seq;
            state.total_bytes = 0;
            break;
        };
        delete.execute(params![seq]).map_err(store_err)?;
        state.first_seq = seq.saturating_add(1);
        state.total_bytes = (state.total_bytes - bytes).max(0);
    }
    Ok(())
}

/// How many records the ring holds and what they weigh. Used by the caps test
/// and by the shells' "is there anything to send" check.
pub(crate) fn ring_size(conn: &Connection) -> Result<(i64, i64), CoreError> {
    conn.query_row(
        "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM protocol_events",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .map_err(store_err)
}

/// The whole ring as a JSONL archive, header first.
///
/// The header's `expect_invariants` is the union of what the records actually
/// reference, so it stays the file's index rather than a claim. For a field
/// archive that union may be empty — a device that had a quiet week declares
/// nothing, which is the honest reading and is why the empty case is allowed
/// for `redacted-field-archive` origins and not for checked-in fixtures.
pub(crate) fn export_jsonl(conn: &Connection) -> Result<String, CoreError> {
    let state = ring_state(conn)?;
    let mut stmt = conn
        .prepare("SELECT seq, line FROM protocol_events ORDER BY seq ASC")
        .map_err(store_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(store_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_err)?;

    let mut actors: BTreeSet<String> = BTreeSet::new();
    let mut invariants: BTreeSet<String> = BTreeSet::new();
    for (_, line) in &rows {
        if let Ok(event) = parse_event(line) {
            if let Some(actor) = event.actor {
                actors.insert(actor);
            }
            invariants.extend(event.invariants);
        }
    }
    // A header must name at least one actor even when nothing has happened
    // yet, and inventing one for an empty ring is better than emitting a
    // header the schema rejects.
    if actors.is_empty() {
        actors.insert("device".to_string());
    }

    let header = ProtocolEventHeader {
        fixture: PROTOCOL_EVENT_ARCHIVE_STEM.to_string(),
        title: ARCHIVE_TITLE.to_string(),
        origin: "redacted-field-archive".to_string(),
        public_reference: None,
        pseudonyms: actors.into_iter().collect(),
        expect_invariants: invariants.into_iter().collect(),
        first_seq: rows.first().map(|(seq, _)| *seq).unwrap_or(state.next_seq),
    };

    let mut out = String::with_capacity(rows.len() * 160 + 256);
    out.push_str(&header_to_line(&header));
    out.push('\n');
    for (_, line) in rows {
        out.push_str(&line);
        out.push('\n');
    }
    Ok(out)
}

pub(crate) fn clear(conn: &Connection) -> Result<(), CoreError> {
    conn.execute("DELETE FROM protocol_events", [])
        .map_err(store_err)?;
    let state = ring_state(conn)?;
    write_ring_state(
        conn,
        RingState {
            first_seq: state.next_seq,
            total_bytes: 0,
            ..state
        },
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Parsing and validation
// ---------------------------------------------------------------------------

/// One thing wrong with an archive, at a line the reader can open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolEventDefect {
    pub line: usize,
    pub detail: String,
}

impl ProtocolEventDefect {
    fn new(line: usize, detail: impl Into<String>) -> Self {
        ProtocolEventDefect {
            line,
            detail: detail.into(),
        }
    }
}

fn parse_event(line: &str) -> Result<ProtocolEvent, String> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let object = value.as_object().ok_or("record is not a JSON object")?;
    let code = object
        .get("code")
        .and_then(|value| value.as_str())
        .ok_or("event has no code")?;
    let code = ProtocolEventCode::from_code(code)
        .ok_or_else(|| format!("{code} is not a stable event code"))?;
    let seq = object
        .get("seq")
        .and_then(|value| value.as_i64())
        .ok_or("event has no integer seq")?;
    let at_ms = object
        .get("at_ms")
        .and_then(|value| value.as_i64())
        .ok_or("event has no integer at_ms")?;

    let text = |key: &str| {
        object
            .get(key)
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
    };

    let mut counts = BTreeMap::new();
    if let Some(map) = object.get("counts") {
        let map = map.as_object().ok_or("counts is not an object")?;
        for (key, value) in map {
            let number = value
                .as_i64()
                .ok_or_else(|| format!("counts.{key} is not an integer"))?;
            counts.insert(key.clone(), number);
        }
    }

    let mut invariants = Vec::new();
    if let Some(list) = object.get("invariants") {
        for value in list.as_array().ok_or("invariants is not a list")? {
            invariants.push(
                value
                    .as_str()
                    .ok_or("invariant ids must be strings")?
                    .to_string(),
            );
        }
    }

    Ok(ProtocolEvent {
        seq,
        at_ms,
        inferred_at: object
            .get("inferred_at")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
        code,
        session: text("session"),
        pass: text("pass"),
        action: object.get("action").and_then(|value| value.as_i64()),
        actor: text("actor"),
        invariants,
        counts,
        outcome: text("outcome"),
    })
}

fn parse_header(line: &str) -> Result<ProtocolEventHeader, String> {
    let value: serde_json::Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
    let object = value.as_object().ok_or("header is not a JSON object")?;
    let schema = object
        .get("schema")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if schema != PROTOCOL_EVENT_SCHEMA {
        return Err(format!(
            "header declares {schema:?}, not {PROTOCOL_EVENT_SCHEMA}"
        ));
    }
    let list = |key: &str| -> Result<Vec<String>, String> {
        let Some(value) = object.get(key) else {
            return Err(format!("header has no {key}"));
        };
        let mut out = Vec::new();
        for item in value
            .as_array()
            .ok_or_else(|| format!("{key} is not a list"))?
        {
            out.push(
                item.as_str()
                    .ok_or_else(|| format!("{key} entries must be strings"))?
                    .to_string(),
            );
        }
        Ok(out)
    };
    let required = |key: &str| -> Result<String, String> {
        object
            .get(key)
            .and_then(|value| value.as_str())
            .map(|value| value.to_string())
            .ok_or_else(|| format!("header has no {key}"))
    };

    Ok(ProtocolEventHeader {
        fixture: required("fixture")?,
        title: required("title")?,
        origin: required("origin")?,
        public_reference: object
            .get("public_reference")
            .and_then(|value| value.as_str())
            .map(|value| value.to_string()),
        pseudonyms: list("pseudonyms")?,
        expect_invariants: list("expect_invariants")?,
        first_seq: object
            .get("first_seq")
            .and_then(|value| value.as_i64())
            .unwrap_or(1),
    })
}

/// A validated archive: its header, its records, and what a first read of the
/// transcript found.
#[derive(Clone, Debug)]
pub struct ProtocolEventArchive {
    pub header: ProtocolEventHeader,
    pub events: Vec<ProtocolEvent>,
}

/// Validate schema, ordering, declared invariant ids, and redaction.
///
/// Every defect is reported, not just the first, because a file with three
/// problems takes three runs to fix otherwise. The replay pass that follows
/// stops at the first *divergence* — that one is a story, and only its
/// beginning is trustworthy.
pub fn validate(text: &str) -> Result<ProtocolEventArchive, Vec<ProtocolEventDefect>> {
    let mut defects = Vec::new();

    if let Some(what) = redaction_defect(text) {
        defects.push(ProtocolEventDefect::new(
            0,
            format!("SECRET-01 violated: the archive contains {what}"),
        ));
    }

    let mut lines = text.lines().enumerate();
    let Some((_, first)) = lines.next() else {
        defects.push(ProtocolEventDefect::new(0, "the archive is empty"));
        return Err(defects);
    };
    let header = match parse_header(first) {
        Ok(header) => header,
        Err(error) => {
            defects.push(ProtocolEventDefect::new(1, error));
            return Err(defects);
        }
    };

    let foreign = foreign_keys(first, PROTOCOL_EVENT_HEADER_KEYS);
    if !foreign.is_empty() {
        defects.push(ProtocolEventDefect::new(
            1,
            format!("SECRET-01 violated: header carries keys outside the schema: {foreign:?}"),
        ));
    }

    if header.title.len() <= 20 {
        defects.push(ProtocolEventDefect::new(
            1,
            "title must be one line a stranger can read",
        ));
    }
    if header.origin != "synthetic" && !header.is_field_archive() {
        defects.push(ProtocolEventDefect::new(
            1,
            format!(
                "origin must be synthetic or redacted-field-archive, got {:?}",
                header.origin
            ),
        ));
    }
    if header.pseudonyms.is_empty() {
        defects.push(ProtocolEventDefect::new(1, "header declares no actors"));
    }
    if header.expect_invariants.is_empty() && !header.is_field_archive() {
        defects.push(ProtocolEventDefect::new(
            1,
            "a fixture must declare at least one expected invariant",
        ));
    }
    for id in &header.expect_invariants {
        if !is_known_invariant(id) {
            defects.push(ProtocolEventDefect::new(
                1,
                format!("{id} is not a Contract v1 invariant"),
            ));
        }
    }
    if header.first_seq < 1 {
        defects.push(ProtocolEventDefect::new(1, "first_seq must be at least 1"));
    }

    let declared: BTreeSet<&str> = header
        .expect_invariants
        .iter()
        .map(|id| id.as_str())
        .collect();
    let pseudonyms: BTreeSet<&str> = header.pseudonyms.iter().map(|id| id.as_str()).collect();

    let mut events = Vec::new();
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    let mut expected_seq = header.first_seq;
    let mut previous_at_ms = i64::MIN;

    for (index, raw) in lines {
        let number = index + 1;
        if raw.trim().is_empty() {
            defects.push(ProtocolEventDefect::new(number, "JSONL has no blank lines"));
            continue;
        }
        let foreign = foreign_keys(raw, PROTOCOL_EVENT_RECORD_KEYS);
        if !foreign.is_empty() {
            defects.push(ProtocolEventDefect::new(
                number,
                format!("SECRET-01 violated: record carries keys outside the schema: {foreign:?}"),
            ));
        }
        let event = match parse_event(raw) {
            Ok(event) => event,
            Err(error) => {
                defects.push(ProtocolEventDefect::new(number, error));
                continue;
            }
        };
        if event.seq != expected_seq {
            defects.push(ProtocolEventDefect::new(
                number,
                format!(
                    "seq must run consecutively from the header's first_seq: expected {expected_seq}, got {}",
                    event.seq
                ),
            ));
        }
        expected_seq = event.seq.saturating_add(1);
        if event.at_ms < previous_at_ms {
            defects.push(ProtocolEventDefect::new(number, "time runs backwards here"));
        }
        previous_at_ms = event.at_ms;

        if let Some(actor) = &event.actor {
            if !pseudonyms.contains(actor.as_str()) {
                defects.push(ProtocolEventDefect::new(
                    number,
                    format!("{actor} is not a declared pseudonym"),
                ));
            }
        }
        if let Some(action) = event.action {
            if action < 0 {
                defects.push(ProtocolEventDefect::new(
                    number,
                    "action ids are non-negative",
                ));
            }
        }
        for (key, value) in &event.counts {
            if *value < 0 {
                defects.push(ProtocolEventDefect::new(
                    number,
                    format!("counts.{key} must be non-negative"),
                ));
            }
        }
        if let Some(outcome) = &event.outcome {
            if !is_stable_token(outcome) {
                defects.push(ProtocolEventDefect::new(
                    number,
                    format!("outcome must be a short stable token, got {outcome:?}"),
                ));
            }
        }
        for id in &event.invariants {
            if !is_known_invariant(id) {
                defects.push(ProtocolEventDefect::new(
                    number,
                    format!("{id} is not a Contract v1 invariant"),
                ));
            } else if !declared.contains(id.as_str()) {
                defects.push(ProtocolEventDefect::new(
                    number,
                    format!("{id} is referenced but not declared in the header"),
                ));
            }
            referenced.insert(id.clone());
        }
        events.push(event);
    }

    if events.is_empty() {
        defects.push(ProtocolEventDefect::new(0, "the archive has no records"));
    }

    let unreferenced: Vec<&str> = declared
        .iter()
        .copied()
        .filter(|id| !referenced.contains(*id))
        .collect();
    if !unreferenced.is_empty() {
        defects.push(ProtocolEventDefect::new(
            1,
            format!(
                "the header declares {unreferenced:?}, which no record references; the header is \
                 the file's index"
            ),
        ));
    }

    if defects.is_empty() {
        Ok(ProtocolEventArchive { header, events })
    } else {
        Err(defects)
    }
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

/// What replaying a transcript found.
///
/// Scope, stated plainly because it is easy to over-read: this walks the event
/// stream and checks the rules a transcript can prove on its own — sequencing,
/// pass lifecycle, monotonic frontiers, rate-limit abort, and that every
/// invariant a record claims exists in the contract. It does **not** re-execute
/// the decisions against a `MessageStore`. That arrives with the session work
/// in the C wave, and until then a clean replay means "nothing in this file
/// contradicts itself", not "this device behaved correctly".
#[derive(Clone, Debug, Default)]
pub struct ReplaySummary {
    pub records: usize,
    pub first_seq: i64,
    pub last_seq: i64,
    pub span_ms: i64,
    /// Records whose timestamp the ring inferred rather than measured: a
    /// decision point with no clock in hand, whose stored time is borrowed
    /// from the record before it. They are ordered correctly; they are not
    /// dated, and they are excluded from `span_ms` for that reason.
    pub undated_records: usize,
    /// Records per code, for the redacted summary the command prints.
    pub by_code: BTreeMap<&'static str, usize>,
    pub actors: Vec<String>,
    pub invariants_exercised: Vec<String>,
    /// The first place the transcript contradicts itself, if any.
    pub divergence: Option<ProtocolEventDefect>,
}

#[derive(Default)]
struct PassState {
    started: bool,
    finished: bool,
    rate_limited: bool,
}

/// Fold a validated archive and report the first divergence.
pub fn replay(archive: &ProtocolEventArchive) -> ReplaySummary {
    let mut summary = ReplaySummary {
        records: archive.events.len(),
        first_seq: archive.header.first_seq,
        actors: archive.header.pseudonyms.clone(),
        ..ReplaySummary::default()
    };
    // A ring that has evicted starts mid-story: a pass that finishes here may
    // legitimately have started in a record that no longer exists, so the
    // lifecycle rules that need a beginning are only applied to passes this
    // file actually opens.
    let partial = archive.header.first_seq > 1;

    let mut passes: BTreeMap<String, PassState> = BTreeMap::new();
    let mut frontier: BTreeMap<String, i64> = BTreeMap::new();
    let mut exercised: BTreeSet<String> = BTreeSet::new();

    let diverge = |summary: &mut ReplaySummary, event: &ProtocolEvent, detail: String| {
        if summary.divergence.is_none() {
            summary.divergence = Some(ProtocolEventDefect::new(event.seq as usize, detail));
        }
    };

    for event in &archive.events {
        *summary.by_code.entry(event.code.as_str()).or_default() += 1;
        exercised.extend(event.invariants.iter().cloned());
        if summary.last_seq == 0 {
            summary.first_seq = event.seq;
        }
        summary.last_seq = event.seq;

        let pass_key = event.pass.clone();
        if let Some(key) = &pass_key {
            let state = passes.entry(key.clone()).or_default();
            match event.code {
                ProtocolEventCode::PassStart => {
                    if state.started {
                        let detail = format!("pass {key} starts twice");
                        diverge(&mut summary, event, detail);
                    }
                    state.started = true;
                }
                ProtocolEventCode::PassFinish => {
                    if !state.started && !partial {
                        let detail = format!("pass {key} finishes without ever starting");
                        diverge(&mut summary, event, detail);
                    }
                    state.finished = true;
                }
                ProtocolEventCode::RateLimitAbort => {
                    state.rate_limited = true;
                }
                ProtocolEventCode::ActionEmitted => {
                    if state.rate_limited {
                        let detail = format!(
                            "RATE-01: pass {key} emits another request after its rate-limit abort"
                        );
                        diverge(&mut summary, event, detail);
                    }
                    if state.finished {
                        let detail = format!("LIVE-01: pass {key} does work after finishing");
                        diverge(&mut summary, event, detail);
                    }
                }
                _ => {}
            }
        }

        if event.code == ProtocolEventCode::FrontierAdvanced {
            let key = event.actor.clone().unwrap_or_else(|| "-".to_string());
            let after = event
                .counts
                .get("frontier_after")
                .or_else(|| event.counts.get("cursor_after"));
            if let Some(after) = after.copied() {
                if let Some(previous) = frontier.get(&key) {
                    if after <= *previous {
                        let detail = format!(
                            "CURSOR-01: {key} reports frontier_advanced to {after}, which is not \
                             beyond {previous}"
                        );
                        diverge(&mut summary, event, detail);
                    }
                }
                frontier.insert(key, after);
            }
        }
    }

    if let Some(first) = archive.events.first() {
        summary.first_seq = first.seq;
    }
    // From records that carry a real clock, and only those. A decision point
    // with no clock argument passes 0; the ring stores the previous record's
    // time so the transcript stays ordered, and says so with `inferred_at`.
    // Measuring a span across such a record would report either fifty-four
    // years (before anything dated the ring) or, worse, a plausible time that
    // nothing actually observed.
    let dated: Vec<i64> = archive
        .events
        .iter()
        .filter(|event| event.at_ms > 0 && !event.inferred_at)
        .map(|event| event.at_ms)
        .collect();
    if let (Some(first), Some(last)) = (dated.first(), dated.last()) {
        summary.span_ms = last.saturating_sub(*first);
    }
    summary.undated_records = archive.events.len() - dated.len();
    summary.invariants_exercised = exercised.into_iter().collect();
    summary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory sqlite");
        conn.execute_batch(PROTOCOL_EVENT_SCHEMA_SQL)
            .expect("ring schema");
        conn
    }

    fn draft(at_ms: i64, outcome: &'static str) -> ProtocolEventDraft {
        ProtocolEventDraft::new(ProtocolEventCode::FrontierHeld, at_ms, outcome)
    }

    #[test]
    fn codes_round_trip_through_their_stable_strings() {
        for code in ProtocolEventCode::ALL {
            assert_eq!(ProtocolEventCode::from_code(code.as_str()), Some(*code));
        }
        let mut sorted = protocol_event_codes();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            protocol_event_codes(),
            "ALL must stay sorted so the contract's table can be diffed against it"
        );
    }

    #[test]
    fn a_record_that_would_leak_is_replaced_rather_than_stored() {
        let conn = new_conn();
        // No caller can reach this through the builder — `outcome` is
        // `&'static str` — so the test constructs it directly, which is
        // exactly the case the backstop exists for.
        let mut leaky = draft(1, "ok");
        leaky.actor = Some("192.168.1.4".to_string());
        append(&conn, &[leaky]).expect("append");

        let text = export_jsonl(&conn).expect("export");
        assert!(!text.contains("192.168."), "the address must not be stored");
        assert!(
            text.contains("invariant_violation") && text.contains("SECRET-01"),
            "the refusal itself must be visible: {text}"
        );
    }

    #[test]
    fn the_ring_evicts_oldest_first_at_the_record_cap() {
        let conn = new_conn();
        let drafts: Vec<_> = (0..PROTOCOL_EVENT_MAX_RECORDS + 50)
            .map(|index| draft(1_700_000_000_000 + index, "held"))
            .collect();
        append(&conn, &drafts).expect("append");

        let (records, bytes) = ring_size(&conn).expect("size");
        assert_eq!(records, PROTOCOL_EVENT_MAX_RECORDS);
        assert!(bytes <= PROTOCOL_EVENT_MAX_BYTES);

        let archive = validate(&export_jsonl(&conn).expect("export")).expect("valid archive");
        assert_eq!(archive.header.first_seq, 51, "the oldest 50 went first");
        assert_eq!(archive.events.first().expect("first").seq, 51);
        assert_eq!(
            archive.events.last().expect("last").seq,
            PROTOCOL_EVENT_MAX_RECORDS + 50
        );
    }

    #[test]
    fn the_ring_evicts_at_the_byte_cap_before_the_record_cap() {
        let conn = new_conn();
        // Wide records: enough counts that 1 MiB arrives well before 2,000.
        let drafts: Vec<_> = (0..1_500)
            .map(|index| {
                let mut draft = draft(1_700_000_000_000 + index, "held");
                for key in [
                    "rows_returned",
                    "rows_consumed",
                    "rows_acked",
                    "frontier_before",
                    "frontier_after",
                    "requested_rows",
                    "envelope_bytes",
                    "requests",
                ] {
                    draft = draft.count(key, 1_234_567);
                }
                draft
            })
            .collect();
        append(&conn, &drafts).expect("append");

        let (records, bytes) = ring_size(&conn).expect("size");
        assert!(
            records < PROTOCOL_EVENT_MAX_RECORDS,
            "the byte cap should have bitten first, got {records} records"
        );
        assert!(bytes <= PROTOCOL_EVENT_MAX_BYTES, "{bytes} over the cap");
    }

    #[test]
    fn appending_many_events_stays_bounded_in_time_and_size() {
        // The soak: this store has an ANR history, so the ring's cost must not
        // grow with how long the device has been running. 20,000 appends is
        // ten times the ring's own capacity, so all but the last 2,000 are
        // paying the eviction path as well as the insert.
        let conn = new_conn();
        let started = std::time::Instant::now();
        for batch in 0..200 {
            let drafts: Vec<_> = (0..100)
                .map(|index| draft(1_700_000_000_000 + batch * 100 + index, "held"))
                .collect();
            append(&conn, &drafts).expect("append");
            let (records, bytes) = ring_size(&conn).expect("size");
            assert!(records <= PROTOCOL_EVENT_MAX_RECORDS);
            assert!(bytes <= PROTOCOL_EVENT_MAX_BYTES);
        }
        let elapsed = started.elapsed();
        // Deliberately loose: this is a runaway detector, not a benchmark. A
        // per-append table scan or an unbounded eviction loop blows through
        // this by orders of magnitude; ordinary CI jitter does not.
        assert!(
            elapsed.as_secs() < 20,
            "20,000 ring appends took {elapsed:?}, which is not a bounded cost"
        );
    }

    #[test]
    fn pseudonyms_are_stable_per_store_and_never_carry_the_raw_id() {
        let conn = new_conn();
        let alice = b"alice-raw-user-id";
        let bob = b"bob-raw-user-id";
        let first = actor_pseudonym(&conn, "peer", alice).expect("pseudonym");
        let again = actor_pseudonym(&conn, "peer", alice).expect("pseudonym");
        let other = actor_pseudonym(&conn, "peer", bob).expect("pseudonym");
        assert_eq!(first, again, "the same peer reads the same way throughout");
        assert_ne!(first, other);
        assert!(first.starts_with("peer-"));
        assert!(!first.contains("alice"));

        // A second store gives the same peer its own name, because the salt is
        // per-store. Two archives cannot be joined on a pseudonym.
        let second_store = new_conn();
        let elsewhere = actor_pseudonym(&second_store, "peer", alice).expect("pseudonym");
        assert!(elsewhere.starts_with("peer-"));
    }

    #[test]
    fn an_empty_ring_still_exports_a_valid_archive() {
        let conn = new_conn();
        append(
            &conn,
            &[ProtocolEventDraft::new(
                ProtocolEventCode::PassStart,
                1_700_000_000_000,
                "nudged",
            )],
        )
        .expect("append");
        let text = export_jsonl(&conn).expect("export");
        validate(&text).expect("a one-record archive is valid");
    }

    #[test]
    fn replay_catches_a_pass_that_keeps_working_after_its_rate_limit_abort() {
        let text = concat!(
            r#"{"schema":"cruisemesh.protocol-event/v1","record":"header","fixture":"t","#,
            r#""title":"A pass that ignores its own rate-limit abort","origin":"synthetic","#,
            r#""pseudonyms":["mailbox-a"],"expect_invariants":["RATE-01"]}"#,
            "\n",
            r#"{"record":"event","seq":1,"at_ms":1,"code":"pass_start","pass":"p1"}"#,
            "\n",
            r#"{"record":"event","seq":2,"at_ms":2,"code":"rate_limit_abort","pass":"p1","invariants":["RATE-01"]}"#,
            "\n",
            r#"{"record":"event","seq":3,"at_ms":3,"code":"action_emitted","pass":"p1"}"#,
            "\n",
        );
        let archive = validate(text).expect("schema-valid");
        let summary = replay(&archive);
        let divergence = summary.divergence.expect("the third record diverges");
        assert!(divergence.detail.contains("RATE-01"), "{divergence:?}");
    }

    #[test]
    fn replay_catches_a_frontier_that_does_not_move_forward() {
        let text = concat!(
            r#"{"schema":"cruisemesh.protocol-event/v1","record":"header","fixture":"t","#,
            r#""title":"A frontier that claims to advance to where it already was","#,
            r#""origin":"synthetic","pseudonyms":["mailbox-a"],"expect_invariants":["CURSOR-01"]}"#,
            "\n",
            r#"{"record":"event","seq":1,"at_ms":1,"code":"frontier_advanced","actor":"mailbox-a","invariants":["CURSOR-01"],"counts":{"frontier_after":100}}"#,
            "\n",
            r#"{"record":"event","seq":2,"at_ms":2,"code":"frontier_advanced","actor":"mailbox-a","invariants":["CURSOR-01"],"counts":{"frontier_after":100}}"#,
            "\n",
        );
        let archive = validate(text).expect("schema-valid");
        let divergence = replay(&archive).divergence.expect("the second record");
        assert!(divergence.detail.contains("CURSOR-01"), "{divergence:?}");
    }

    #[test]
    fn validation_reports_every_defect_rather_than_only_the_first() {
        let text = concat!(
            r#"{"schema":"cruisemesh.protocol-event/v1","record":"header","fixture":"t","#,
            r#""title":"A file with more than one thing wrong with it","origin":"synthetic","#,
            r#""pseudonyms":["mailbox-a"],"expect_invariants":["CURSOR-01","NOPE-01"]}"#,
            "\n",
            r#"{"record":"event","seq":4,"at_ms":1,"code":"frontier_advanced","actor":"who","invariants":["CURSOR-01"]}"#,
            "\n",
        );
        let defects = validate(text).expect_err("several defects");
        let joined = defects
            .iter()
            .map(|defect| defect.detail.as_str())
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(joined.contains("NOPE-01"), "{joined}");
        assert!(joined.contains("expected 1, got 4"), "{joined}");
        assert!(
            joined.contains("who is not a declared pseudonym"),
            "{joined}"
        );
    }

    #[test]
    fn the_canary_scanner_would_catch_a_leak_planted_in_a_finished_archive() {
        // The negative control for the redaction test above: prove the
        // scanner is capable of failing, so a passing scan means something.
        let text = concat!(
            r#"{"schema":"cruisemesh.protocol-event/v1","record":"header","fixture":"t","#,
            r#""title":"An archive with a token smuggled into an outcome token","#,
            r#""origin":"synthetic","pseudonyms":["mailbox-a"],"expect_invariants":["SECRET-01"]}"#,
            "\n",
            r#"{"record":"event","seq":1,"at_ms":1,"code":"pass_start","outcome":"cmdep1-abc","invariants":["SECRET-01"]}"#,
            "\n",
        );
        let defects = validate(text).expect_err("the canary must fire");
        assert!(
            defects
                .iter()
                .any(|defect| defect.detail.contains("SECRET-01")),
            "{defects:?}"
        );
    }

    #[test]
    fn secret_01_a_key_the_schema_never_declared_is_a_leak_and_is_rejected() {
        // The structural half of SECRET-01, on the path that reads *real*
        // archives rather than the checked-in corpus. Both lines here carry a
        // sentence under a field name nobody declared, which is exactly how a
        // leak would arrive: not as a token the canary list recognises, but as
        // prose in a field that was never part of the schema.
        let text = concat!(
            r#"{"schema":"cruisemesh.protocol-event/v1","record":"header","fixture":"t","#,
            r#""title":"An archive with prose smuggled in under undeclared keys","#,
            r#""origin":"synthetic","pseudonyms":["mailbox-a"],"expect_invariants":["CURSOR-01"],"#,
            r#""note":"the passphrase and where to meet"}"#,
            "\n",
            r#"{"record":"event","seq":1,"at_ms":1,"code":"frontier_held","invariants":["CURSOR-01"],"#,
            r#""body":"we docked at four and the children are fine"}"#,
            "\n",
        );
        let defects = validate(text).expect_err("an undeclared key must fail validation");
        let joined = format!("{defects:?}");
        assert!(joined.contains("note"), "{joined}");
        assert!(joined.contains("body"), "{joined}");
        assert!(
            defects
                .iter()
                .filter(|defect| defect.detail.contains("SECRET-01"))
                .count()
                >= 2,
            "both the header and the record must be reported: {joined}"
        );
    }

    #[test]
    fn the_key_lists_stay_a_superset_of_what_the_ring_actually_writes() {
        // The other direction of the same rule: a field added to the writer
        // and not to the list would make the ring's own export fail the
        // validator that gates it.
        let conn = new_conn();
        let mut rich = draft(1, "ok");
        rich.session = Some("s1".to_string());
        rich.pass = Some("p1".to_string());
        rich.action = Some(3);
        rich.actor = Some(actor_pseudonym(&conn, "peer", b"whoever").expect("pseudonym"));
        rich.invariants = vec!["CURSOR-01"];
        rich.counts = vec![("rows", 2)];
        append(&conn, &[rich]).expect("append");
        // And one with no clock, which is the only field the writer adds on
        // its own rather than at a call site's request.
        append(&conn, &[draft(0, "clockless")]).expect("append");

        let text = export_jsonl(&conn).expect("export");
        assert!(text.contains("\"inferred_at\":true"), "{text}");
        validate(&text).unwrap_or_else(|defects| panic!("{defects:?}"));
    }

    #[test]
    fn a_sequence_counter_left_behind_its_rows_repairs_itself_rather_than_jamming() {
        // The state a torn write used to leave behind: rows durably committed,
        // the counter that names the next one still pointing inside them.
        // Every later append then failed its primary key, forever — and the
        // store methods that emit are the mailbox walk, so the frontier would
        // stop advancing for the life of the install.
        let conn = new_conn();
        append(&conn, &[draft(10, "first"), draft(20, "second")]).expect("append");
        conn.execute(
            "UPDATE protocol_event_state SET next_seq = 1, first_seq = 1, total_bytes = 0
             WHERE id = 0",
            [],
        )
        .expect("poison the counter");

        append(&conn, &[draft(30, "third")]).expect("a stale counter must not jam the ring");
        append(&conn, &[draft(40, "fourth")]).expect("and must not jam it on the call after");

        let text = export_jsonl(&conn).expect("export");
        let archive = validate(&text).unwrap_or_else(|defects| panic!("{defects:?}"));
        assert_eq!(archive.events.len(), 4, "nothing was lost by the repair");
        let (records, bytes) = ring_size(&conn).expect("size");
        assert_eq!(records, 4);
        let state = ring_state(&conn).expect("state");
        assert_eq!(state.total_bytes, bytes, "the byte count was repaired too");
        assert_eq!(state.next_seq, 5);
        assert_eq!(state.first_seq, 1);
    }

    #[test]
    fn a_batch_that_fails_part_way_leaves_the_ring_exactly_as_it_was() {
        // Fault injection at the one place a partial write can happen. A
        // trigger that aborts the third insert stands in for the disk filling
        // up or the process being killed: the earlier rows of the same batch
        // must not survive it, or the counter and the table part company
        // again.
        let conn = new_conn();
        append(&conn, &[draft(1, "before")]).expect("append");
        let before = export_jsonl(&conn).expect("export");
        conn.execute_batch(
            "CREATE TRIGGER boom BEFORE INSERT ON protocol_events WHEN NEW.seq = 3
             BEGIN SELECT RAISE(ABORT, 'injected'); END;",
        )
        .expect("trigger");

        append(&conn, &[draft(2, "a"), draft(3, "b"), draft(4, "c")])
            .expect_err("the injected failure must surface");
        assert_eq!(
            export_jsonl(&conn).expect("export"),
            before,
            "a failed batch leaves no partial records behind"
        );

        conn.execute_batch("DROP TRIGGER boom").expect("drop");
        append(&conn, &[draft(5, "after")]).expect("the ring still works");
        let archive = validate(&export_jsonl(&conn).expect("export"))
            .unwrap_or_else(|defects| panic!("{defects:?}"));
        assert_eq!(archive.events.len(), 2);
    }

    #[test]
    fn a_failed_append_leaves_the_callers_own_transaction_intact() {
        // The rule the whole subsystem turns on: the ring is never the reason
        // anything else fails. The caller's row is written inside its own
        // transaction, the ring is broken under it, and the commit still
        // holds what the caller put there.
        let conn = new_conn();
        conn.execute_batch("CREATE TABLE caller_work (id INTEGER PRIMARY KEY)")
            .expect("caller table");
        conn.execute_batch("DROP TABLE protocol_events")
            .expect("break the ring");

        conn.execute_batch("BEGIN").expect("begin");
        conn.execute("INSERT INTO caller_work (id) VALUES (1)", [])
            .expect("caller work");
        note(&conn, &[draft(1, "ok")]);
        note_for(&conn, "peer", b"whoever", |peer| {
            vec![draft(2, "ok").actor(peer)]
        });
        conn.execute_batch("COMMIT").expect("commit");

        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM caller_work", [], |row| row.get(0))
            .expect("count");
        assert_eq!(
            rows, 1,
            "a diagnostics failure must not roll back real work"
        );
    }

    #[test]
    fn a_borrowed_timestamp_is_reported_as_borrowed_and_left_out_of_the_span() {
        let conn = new_conn();
        append(&conn, &[draft(1_700_000_000_000, "clocked")]).expect("append");
        // No clock in hand: stored ordered, marked, and not counted as a
        // measurement of when anything happened.
        append(&conn, &[draft(0, "clockless")]).expect("append");
        append(&conn, &[draft(1_700_000_060_000, "clocked_again")]).expect("append");

        let archive = validate(&export_jsonl(&conn).expect("export"))
            .unwrap_or_else(|defects| panic!("{defects:?}"));
        assert!(archive.events[1].inferred_at);
        assert_eq!(archive.events[1].at_ms, archive.events[0].at_ms);
        assert!(!archive.events[0].inferred_at);

        let summary = replay(&archive);
        assert_eq!(summary.undated_records, 1);
        assert_eq!(summary.span_ms, 60_000);
    }
}
