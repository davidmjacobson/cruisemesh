//! Protocol Contract v1 — the index of record.
//!
//! `specs/protocol-contract-v1.md` is the human index. This file is the
//! machine one, and the two are kept in step by a test rather than by
//! discipline: every invariant id in the document must appear in the registry
//! below, and every registry entry must appear in the document.
//!
//! Each invariant lands in exactly one of three states:
//!
//! * **`Owner::Core`** — a Rust test in `core/` already pins it. This file
//!   re-asserts the same rule through the exported core API so that the
//!   *invariant id* is what prints when it breaks. The re-assertion is
//!   deliberately thin: it is a pointer, not a second copy of the owner's
//!   test suite, and the owner named in the entry is where the real coverage
//!   lives.
//! * **`Owner::HoistPending`** — the decision still lives in a platform
//!   shell. The named Kotlin/Swift tests are the real owners today. This file
//!   carries an ignored marker naming both those tests and the work package
//!   that will move the decision into core. It does **not** claim core
//!   coverage that does not exist.
//! * **`Owner::Unimplemented`** — nothing pins the rule anywhere yet. An
//!   ignored marker names the work package that will own it.
//!
//! `cargo test -p cruisemesh-core -- --ignored` lists exactly the work that
//! is still owed. That count is the point of this file; do not make it look
//! smaller than it is.
//!
//! This file also validates the JSONL fixture corpus under
//! `core/tests/fixtures/`. At this revision it checks schema, ordering,
//! declared invariant ids, and redaction only. Executing a fixture's
//! behaviour against a real store arrives with the replay runner.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use cruisemesh_core::{
    core_contact_relay_unreachable_delta, core_is_hidden_spray_kind, core_kind_persists_msg_id_row,
    core_own_capabilities, core_relay_ack_ids, core_relay_queue_reflects_delivery,
    core_should_ack_inbound, encode_hello, encode_hello2, may_start_carried_offer, parse_frame,
    relay_classify_http_error, relay_cursor_advance, relay_fetch_walk_continues,
    relay_frontier_after_completed_sweep, relay_mailbox_walk_action, relay_retry_after_ms,
    CarriedEnvelope, CoreInboundDisposition, CoreRelayEnvelopeDisposition, CoreRelayFault,
    CoreRelayPathState, Frame, MessageStore, RelayMailboxWalkAction, CAP_ACKS_HIDDEN_KINDS,
    KIND_RECEIPT, KIND_RELAY_UPDATE, KIND_TEXT,
};

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Owner {
    /// Pinned by a Rust test in `core/`. The string names it.
    Core(&'static str),
    /// Still decided in a shell. `shell_tests` names the real owners today;
    /// `package` names the work package that will hoist it.
    HoistPending {
        shell_tests: &'static str,
        package: &'static str,
    },
    /// Nothing pins this yet.
    Unimplemented { package: &'static str },
}

struct Invariant {
    id: &'static str,
    /// One line, for a stranger. The document carries the full statement.
    statement: &'static str,
    owner: Owner,
}

const CONTRACT: &[Invariant] = &[
    Invariant {
        id: "ACK-01",
        statement: "Proxy/carry disposition never makes a relay copy ackable; SEEN acks only \
                    with durable local-consumption proof.",
        owner: Owner::Core("core/src/engine.rs ack-eligibility tests"),
    },
    Invariant {
        id: "CARRY-01",
        statement: "Sending or relay-uploading a carried row does not remove it; only digest/\
                    receipt proof or expiry may.",
        owner: Owner::Core("core/src/engine.rs confirm-carried + core/src/store.rs carry tests"),
    },
    Invariant {
        id: "CURSOR-01",
        statement: "A frontier advances only across a fully processed page whose required acks \
                    succeeded, and never backward on a normal pass.",
        owner: Owner::Core("core/src/relay_cursor.rs cursor-advance tests"),
    },
    Invariant {
        id: "PAGE-01",
        statement: "Only an empty page is EOF; a short page continues; a non-advancing page \
                    terminates without unsafe advancement.",
        owner: Owner::Core("core/src/relay_cursor.rs walk-continuation tests"),
    },
    Invariant {
        id: "RATE-01",
        statement: "The first family 429 ends remaining pass network work; Retry-After is a \
                    floor no pending nudge may bypass.",
        owner: Owner::HoistPending {
            shell_tests: "android FamilyRelayBackpressureTest.kt + RelayRerunPolicyTest.kt; \
                          ios FamilyRelayBackpressureTests.swift",
            package: "B0 (relay policy hoist), completed by C0's pass abort",
        },
    },
    Invariant {
        id: "ENDPOINT-01",
        statement: "A phone advertises only its own endpoint; a discovered or third-party \
                    address is never forwarded.",
        owner: Owner::HoistPending {
            shell_tests: "android LanEndpointCacheTest.kt + LanEndpointSendPolicyTest.kt; \
                          ios LanEndpointStoreTests.swift",
            package: "D2/D3 (encounter planning) owns the authoring half",
        },
    },
    Invariant {
        id: "SILENCE-01",
        statement: "Contact silence advances only with same-pass proof another relay answered; \
                    authoritative rejection needs no such proof.",
        owner: Owner::Core("core/src/contact_relay_health.rs silence tests"),
    },
    Invariant {
        id: "UI-01",
        statement: "Delivery and via-transport claims require persisted arrival or receipt \
                    evidence, never a current-link guess.",
        owner: Owner::Core("core/src/connection_health.rs delivery-line tests"),
    },
    Invariant {
        id: "LIVE-01",
        statement: "Every pass terminates inside its declared request, envelope, byte, and \
                    time/yield budgets.",
        owner: Owner::Unimplemented {
            package: "C0 (CoreRelayPass + replay runner)",
        },
    },
    Invariant {
        id: "PROGRESS-01",
        statement: "A continuation must strictly advance a cursor or strictly increase a future \
                    deadline; unchanged-state reschedule loops are forbidden.",
        owner: Owner::Unimplemented {
            package: "C0 (CoreRelayPass + replay runner); the walk-budget half is already core",
        },
    },
    Invariant {
        id: "MARK-01",
        statement: "A relay-uploaded carried row is durably marked before the pass ends; the \
                    marker survives restart and suppresses re-upload.",
        owner: Owner::Core("core/src/store.rs carried-upload-marker tests"),
    },
    Invariant {
        id: "WM-01",
        statement: "Receipt repair is reachable and bounded from every supported stored state; a \
                    zero peer watermark cannot permanently gate it.",
        owner: Owner::HoistPending {
            shell_tests: "android ReceiptRepairTest.kt + PeerStreamWatermarkTest.kt; \
                          ios ReceiptRepairTests.swift + PeerStreamWatermarkTests.swift",
            package: "D2 (mesh_meet) hoists the repair planner",
        },
    },
    Invariant {
        id: "SPRAY-01",
        statement: "Carried-first work toward one peer is bounded per encounter in bytes as well \
                    as rows, and re-offers are cadence-gated.",
        owner: Owner::HoistPending {
            shell_tests: "android PeripheralSprayCooldownTest.kt + \
                          PeripheralSprayCooldownDebounceTest.kt (cadence half; no iOS twin yet)",
            package: "D2 (mesh_meet); the cadence gate itself is issue #280",
        },
    },
    Invariant {
        id: "HELLO-01",
        statement: "Legacy HELLO never gains trailing fields; new capabilities use HELLO2 frame \
                    0x06.",
        owner: Owner::Core(
            "core/src/protocol.rs HELLO/HELLO2 codec tests + protocol_decoders fuzz",
        ),
    },
    Invariant {
        id: "IDEMP-01",
        statement: "Duplicate, late, or replayed external results cannot double-apply a \
                    mutation, regress a cursor, or consume a carried row.",
        owner: Owner::Unimplemented {
            package: "C0 (CoreRelayPass event-permutation tests)",
        },
    },
    Invariant {
        id: "TXN-01",
        statement: "No store transaction spans external I/O; page consume and frontier \
                    advancement stay two short transactions.",
        owner: Owner::Unimplemented {
            package: "C0 (CoreRelayPass fault-injection/restart tests)",
        },
    },
    Invariant {
        id: "QUEUE-01",
        statement: "Proof of delivery permits — and the queue eventually performs — retirement \
                    of a 1:1 outbound envelope, and short-lived payloads are superseded.",
        owner: Owner::Unimplemented {
            package: "the fix for issue #283 (outbound queue retirement)",
        },
    },
    Invariant {
        id: "SECRET-01",
        statement: "Events, fixtures, summaries, and exported diagnostics carry no tokens, \
                    friend cards, plaintext, keys, or endpoint-bearing bodies.",
        owner: Owner::Core("core/tests/protocol_contract.rs fixture canary scan"),
    },
];

/// Assert with the invariant id in the failure message, so a red build names
/// the rule rather than a function.
macro_rules! contract_assert {
    ($id:expr, $cond:expr, $($detail:tt)+) => {
        assert!(
            $cond,
            "{} violated: {}",
            $id,
            format_args!($($detail)+),
        )
    };
}

fn lookup(id: &str) -> &'static Invariant {
    CONTRACT
        .iter()
        .find(|entry| entry.id == id)
        .unwrap_or_else(|| panic!("{id} is not in the contract registry"))
}

// ---------------------------------------------------------------------------
// The registry and the document must agree
// ---------------------------------------------------------------------------

const CONTRACT_DOC: &str = include_str!("../../specs/protocol-contract-v1.md");

/// Ids look like `ACK-01`: uppercase letters, a hyphen, two digits.
fn looks_like_an_invariant_id(candidate: &str) -> bool {
    let Some((word, number)) = candidate.split_once('-') else {
        return false;
    };
    !word.is_empty()
        && word.chars().all(|c| c.is_ascii_uppercase())
        && number.len() == 2
        && number.chars().all(|c| c.is_ascii_digit())
}

/// Ids from the section 1 summary table, in document order.
fn documented_ids() -> Vec<String> {
    let table = CONTRACT_DOC
        .split_once("## 1. Invariants")
        .expect("the contract document must have a section 1")
        .1
        .split_once("### 1.1")
        .expect("section 1 must be followed by 1.1")
        .0;

    table
        .lines()
        .filter_map(|line| {
            let mut cells = line.trim().split('|');
            cells.next()?; // leading empty cell
            let first = cells.next()?.trim().trim_matches('`');
            looks_like_an_invariant_id(first).then(|| first.to_string())
        })
        .collect()
}

#[test]
fn the_registry_and_the_document_name_the_same_invariants() {
    let documented: Vec<String> = documented_ids();
    let registered: Vec<String> = CONTRACT.iter().map(|entry| entry.id.to_string()).collect();

    assert!(
        !documented.is_empty(),
        "parsed no invariant ids out of specs/protocol-contract-v1.md — the table shape changed"
    );
    assert_eq!(
        documented, registered,
        "specs/protocol-contract-v1.md and the registry in this file disagree. \
         There must be no prose-only normative invariant and no orphan test entry."
    );
}

#[test]
fn every_invariant_has_a_prose_block_a_stranger_can_read() {
    for entry in CONTRACT {
        let heading = format!("#### `{}` —", entry.id);
        assert!(
            CONTRACT_DOC.contains(&heading),
            "{} has no `{}` explanation block in specs/protocol-contract-v1.md",
            entry.id,
            heading.trim_start_matches("#### ")
        );
        assert!(
            !entry.statement.trim().is_empty(),
            "{} has an empty registry statement",
            entry.id
        );
    }
}

#[test]
fn every_invariant_names_a_real_owner() {
    for entry in CONTRACT {
        match entry.owner {
            Owner::Core(owner) => assert!(
                !owner.trim().is_empty(),
                "{} claims core ownership without naming the owning test",
                entry.id
            ),
            Owner::HoistPending {
                shell_tests,
                package,
            } => {
                assert!(
                    !shell_tests.trim().is_empty(),
                    "{} is hoist-pending but names no shell test that owns it today",
                    entry.id
                );
                assert!(
                    !package.trim().is_empty(),
                    "{} is hoist-pending with no work package to hoist it",
                    entry.id
                );
            }
            Owner::Unimplemented { package } => assert!(
                !package.trim().is_empty(),
                "{} is unimplemented with no owning work package",
                entry.id
            ),
        }
    }
}

/// The registry's owner class and the document's owner-class column must not
/// drift apart. Coverage optics are the failure mode this guards against.
#[test]
fn the_document_and_the_registry_agree_on_owner_class() {
    for entry in CONTRACT {
        let row = CONTRACT_DOC
            .lines()
            .find(|line| {
                line.trim_start()
                    .starts_with(&format!("| `{}` |", entry.id))
            })
            .unwrap_or_else(|| panic!("{} has no summary-table row", entry.id));
        let class = row.split('|').nth(3).unwrap_or("").trim();
        let expected = match entry.owner {
            Owner::Core(_) => "core",
            Owner::HoistPending { .. } => "hoist-pending",
            Owner::Unimplemented { .. } => "unimplemented",
        };
        assert_eq!(
            class, expected,
            "{} is `{}` in the registry but `{}` in the document",
            entry.id, expected, class
        );
    }
}

// ---------------------------------------------------------------------------
// Core-owned invariants: thin re-assertions that print the id
// ---------------------------------------------------------------------------

fn disposition(relay_id: i64, disposition: CoreInboundDisposition) -> CoreRelayEnvelopeDisposition {
    CoreRelayEnvelopeDisposition {
        relay_id,
        msg_id: vec![relay_id as u8; 16],
        disposition,
        recipient_hint: vec![0xAB; 8],
    }
}

#[test]
fn ack_01_only_consumed_or_expired_mail_is_ackable() {
    let id = lookup("ACK-01").id;

    for never in [
        CoreInboundDisposition::Carried,
        CoreInboundDisposition::Seen,
        CoreInboundDisposition::Rejected,
        CoreInboundDisposition::Failed,
    ] {
        contract_assert!(
            id,
            !core_should_ack_inbound(never),
            "{never:?} must never make a relay copy ackable"
        );
    }
    contract_assert!(
        id,
        core_should_ack_inbound(CoreInboundDisposition::Consumed),
        "a durably consumed envelope is the ackable case"
    );
    contract_assert!(
        id,
        core_should_ack_inbound(CoreInboundDisposition::Expired),
        "an expired envelope has nothing left to preserve"
    );

    let acked = core_relay_ack_ids(vec![
        disposition(1, CoreInboundDisposition::Consumed),
        disposition(2, CoreInboundDisposition::Carried),
        disposition(3, CoreInboundDisposition::Seen),
        disposition(4, CoreInboundDisposition::Failed),
    ]);
    contract_assert!(
        id,
        acked == vec![1],
        "only the consumed row may be acked, got {acked:?}"
    );
}

#[test]
fn carry_01_uploading_a_carried_row_does_not_remove_it() {
    let id = lookup("CARRY-01").id;
    let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
    let now_ms = 1_700_000_000_000;

    let envelope = CarriedEnvelope {
        msg_id: vec![7; 16],
        hop_ttl: 6,
        expiry: now_ms + 60_000,
        recipient_hint: vec![0xCD; 8],
        sealed: vec![9; 128],
    };
    let accepted = store
        .enqueue_carried_envelope(envelope, true, now_ms, 1024 * 1024)
        .expect("enqueue carried");
    contract_assert!(id, accepted, "the fixture row should have been accepted");

    let marked = store
        .mark_carried_envelope_relay_uploaded(vec![7; 16], "https://relay.invalid".to_string())
        .expect("mark uploaded");
    contract_assert!(id, marked, "the upload marker should have been written");

    let remaining = store.carried_len().expect("carried len");
    contract_assert!(
        id,
        remaining == 1,
        "a relay upload must not remove the carried row; {remaining} rows left"
    );

    // Only digest/receipt proof or expiry may remove it. Expiry is the half
    // that needs no peer, so it is the half this index re-asserts.
    let pruned = store
        .prune_expired_carried(now_ms + 120_000)
        .expect("prune expired");
    contract_assert!(id, pruned == 1, "expiry must be able to remove the row");
}

#[test]
fn cursor_01_the_frontier_moves_only_over_fully_processed_ground() {
    let id = lookup("CURSOR-01").id;

    contract_assert!(
        id,
        relay_cursor_advance(100, 140, true) == 140,
        "a fully processed page must advance the frontier"
    );
    contract_assert!(
        id,
        relay_cursor_advance(100, 140, false) == 100,
        "an unfinished page must leave the frontier alone"
    );
    contract_assert!(
        id,
        relay_cursor_advance(140, 40, true) == 140,
        "a lower page cursor must never drag the frontier backwards"
    );
    contract_assert!(
        id,
        relay_cursor_advance(-5, 10, true) == 10,
        "a corrupt negative frontier must clamp, not underflow"
    );

    // The single sanctioned way down, and it needs a completed sweep as proof
    // the server's id space itself regressed.
    contract_assert!(
        id,
        relay_frontier_after_completed_sweep(29_000, 40) == 40,
        "a completed sweep over a rebuilt mailbox must lower the frontier"
    );
    contract_assert!(
        id,
        relay_frontier_after_completed_sweep(29_000, 0) == 29_000,
        "a sweep that proved nothing must not lower the frontier"
    );
}

#[test]
fn page_01_only_an_empty_page_ends_a_walk() {
    let id = lookup("PAGE-01").id;

    contract_assert!(
        id,
        relay_fetch_walk_continues(100, 0, 100),
        "a short page must continue the walk"
    );
    contract_assert!(
        id,
        !relay_fetch_walk_continues(0, 100, 100),
        "an empty page is the only end of the walk"
    );
    contract_assert!(
        id,
        !relay_fetch_walk_continues(40, 100, 100),
        "a non-empty page that does not advance the cursor must terminate, not loop"
    );
    contract_assert!(
        id,
        !relay_fetch_walk_continues(40, 100, 90),
        "a page whose cursor went backwards must terminate"
    );

    // The budget half of the same story: a walk that yields must say so
    // rather than silently ending the mailbox.
    contract_assert!(
        id,
        relay_mailbox_walk_action(0, 0) == RelayMailboxWalkAction::ContinueWalk,
        "a fresh walk is under budget"
    );
    contract_assert!(
        id,
        relay_mailbox_walk_action(4, 0) == RelayMailboxWalkAction::YieldAndScheduleContinuation,
        "an exhausted page budget must yield with a continuation"
    );
}

#[test]
fn silence_01_silence_needs_proof_that_the_internet_worked() {
    let id = lookup("SILENCE-01").id;

    contract_assert!(
        id,
        core_contact_relay_unreachable_delta(false) == 0,
        "silence with no proof another relay answered must not advance the streak"
    );
    contract_assert!(
        id,
        core_contact_relay_unreachable_delta(true) == 1,
        "silence with same-pass proof must advance the streak"
    );

    // An authoritative rejection is the other half: the server answered, so
    // no connectivity proof is required for it to count.
    for (status, code) in [
        (403_u16, Some("family_expired")),
        (403, Some("family_suspended")),
        (401, None),
    ] {
        let fault = relay_classify_http_error(status, code.map(str::to_string));
        contract_assert!(
            id,
            matches!(
                fault,
                CoreRelayFault::PassExpired
                    | CoreRelayFault::PassSuspended
                    | CoreRelayFault::TokenRejected
            ),
            "status {status} / code {code:?} must classify as an authoritative rejection, got \
             {fault:?}"
        );
    }
}

#[test]
fn ui_01_a_number_that_cannot_mean_delivery_is_not_shown_as_delivery() {
    let id = lookup("UI-01").id;

    contract_assert!(
        id,
        core_relay_queue_reflects_delivery(CoreRelayPathState::Connected, true, false),
        "a working pass and a live endpoint is the one case the backlog means something"
    );
    contract_assert!(
        id,
        !core_relay_queue_reflects_delivery(CoreRelayPathState::NotSetUp, true, false),
        "with no pass saved the backlog never drains and is not delivery state"
    );
    contract_assert!(
        id,
        !core_relay_queue_reflects_delivery(CoreRelayPathState::Connected, false, false),
        "a friend with no endpoint has no relay delivery to report"
    );
    contract_assert!(
        id,
        !core_relay_queue_reflects_delivery(CoreRelayPathState::Connected, true, true),
        "a written-off endpoint cannot be drained, so its backlog is not delivery state"
    );
}

#[test]
fn mark_01_an_uploaded_carried_row_is_marked_once_and_durably() {
    let id = lookup("MARK-01").id;
    let store = MessageStore::open(":memory:".to_string()).expect("in-memory store");
    let now_ms = 1_700_000_000_000;

    store
        .enqueue_carried_envelope(
            CarriedEnvelope {
                msg_id: vec![3; 16],
                hop_ttl: 6,
                expiry: now_ms + 600_000,
                recipient_hint: vec![0xEF; 8],
                sealed: vec![1; 64],
            },
            true,
            now_ms,
            1024 * 1024,
        )
        .expect("enqueue carried");

    let before = store
        .family_carried_envelopes(16, now_ms, Vec::new())
        .expect("family carried");
    contract_assert!(
        id,
        before.len() == 1,
        "an unmarked family carry must be offered for upload; got {} rows",
        before.len()
    );

    let first = store
        .mark_carried_envelope_relay_uploaded(vec![3; 16], "https://relay.invalid".to_string())
        .expect("first mark");
    contract_assert!(id, first, "the first upload must write the marker");

    let second = store
        .mark_carried_envelope_relay_uploaded(vec![3; 16], "https://other.invalid".to_string())
        .expect("second mark");
    contract_assert!(
        id,
        !second,
        "markers are first-writer-wins; a second upload must not overwrite the destination"
    );

    let offered = store
        .family_carried_envelopes(16, now_ms, Vec::new())
        .expect("family carried");
    contract_assert!(
        id,
        offered.is_empty(),
        "a marked row must never be offered for upload again, got {} rows",
        offered.len()
    );

    // Endpoint changes are the one sanctioned wholesale clear: "already on the
    // old mailbox" says nothing about a new one.
    let cleared = store
        .clear_carried_relay_upload_markers()
        .expect("clear markers");
    contract_assert!(
        id,
        cleared == 1,
        "an endpoint change must re-offer the queue"
    );
}

#[test]
fn hello_01_the_legacy_handshake_is_frozen_and_hello2_is_the_extension_point() {
    let id = lookup("HELLO-01").id;
    let user_id = vec![0x5A; 16];

    let legacy = encode_hello(user_id.clone());
    contract_assert!(
        id,
        legacy.len() == 1 + user_id.len(),
        "legacy HELLO must be exactly frame type + user_id, got {} bytes",
        legacy.len()
    );
    match parse_frame(legacy).expect("legacy HELLO parses") {
        Frame::Hello { user_id: parsed } => contract_assert!(
            id,
            parsed == user_id,
            "legacy HELLO's user_id is the whole remainder, so any trailing field would be \
             swallowed into it"
        ),
        other => contract_assert!(id, false, "legacy HELLO parsed as {other:?}"),
    }

    let hello2 = encode_hello2(user_id.clone(), core_own_capabilities()).expect("HELLO2 encodes");
    contract_assert!(
        id,
        hello2[0] == 0x06,
        "HELLO2 must ride frame type 0x06, got 0x{:02x}",
        hello2[0]
    );

    // Trailing bytes are the additive extension point, and must be tolerated.
    let mut extended = hello2.clone();
    extended.extend_from_slice(&[0xFF, 0xFF, 0xFF]);
    match parse_frame(extended).expect("extended HELLO2 parses") {
        Frame::Hello2 {
            user_id: parsed,
            capabilities,
        } => {
            contract_assert!(id, parsed == user_id, "HELLO2 user_id round trip");
            contract_assert!(
                id,
                capabilities == core_own_capabilities(),
                "trailing bytes must not disturb the capability word"
            );
        }
        other => contract_assert!(id, false, "extended HELLO2 parsed as {other:?}"),
    }

    contract_assert!(
        id,
        core_own_capabilities() & CAP_ACKS_HIDDEN_KINDS != 0,
        "this build must advertise the hidden-kind acknowledgement bit"
    );

    // Kind classification is what the capability bits are *for*: a new kind
    // needs its own bit precisely because these two sets differ.
    contract_assert!(
        id,
        core_is_hidden_spray_kind(KIND_RELAY_UPDATE) && !core_is_hidden_spray_kind(KIND_RECEIPT),
        "the hidden-spray set must stay distinct from the hidden-evidence set"
    );
    contract_assert!(
        id,
        core_kind_persists_msg_id_row(KIND_TEXT) && !core_kind_persists_msg_id_row(KIND_RECEIPT),
        "only kinds that persist a msg_id row may be used as ACK-01 evidence"
    );
}

#[test]
fn spray_01_the_row_count_half_of_the_bound_exists_in_core() {
    // Deliberately not claiming SPRAY-01 is owned here: the byte budget and
    // the concurrent-offer cap are core, the per-second cadence gate is not.
    // The registry keeps this invariant hoist-pending; this test only pins the
    // part that would otherwise regress silently.
    let id = lookup("SPRAY-01").id;
    contract_assert!(
        id,
        may_start_carried_offer(0),
        "a first carried offer must be allowed"
    );
    contract_assert!(
        id,
        !may_start_carried_offer(2),
        "concurrent carried offers must be capped so one carrier cannot fan out to a whole desk"
    );
    assert!(
        matches!(lookup("SPRAY-01").owner, Owner::HoistPending { .. }),
        "SPRAY-01 must stay hoist-pending until the byte cap and cadence gate land together"
    );
}

#[test]
fn rate_01_the_retry_after_floor_is_core_even_though_the_pacing_is_not() {
    let id = lookup("RATE-01").id;

    contract_assert!(
        id,
        relay_retry_after_ms(Some("15".to_string())) == 15_000,
        "Retry-After delta-seconds must be honoured as a floor"
    );
    contract_assert!(
        id,
        relay_retry_after_ms(None) == 30_000,
        "a missing Retry-After must fall back to a real quiet window, never to zero"
    );
    contract_assert!(
        id,
        relay_retry_after_ms(Some("nonsense".to_string())) == 30_000,
        "an unparseable Retry-After must not collapse the quiet window"
    );
    contract_assert!(
        id,
        relay_retry_after_ms(Some("0".to_string())) == 1_000,
        "a zero Retry-After must clamp up, not disable the window"
    );
    contract_assert!(
        id,
        relay_retry_after_ms(Some("100000".to_string())) == 60_000,
        "an absurd Retry-After must clamp to the server's real maximum"
    );
    contract_assert!(
        id,
        relay_classify_http_error(429, Some("rate_limited".to_string()))
            == CoreRelayFault::RateLimited,
        "a 429 must classify as rate limited, which is the abort trigger"
    );
}

// ---------------------------------------------------------------------------
// Markers for what is not owned yet
// ---------------------------------------------------------------------------

#[test]
#[ignore = "HOIST-PENDING: RATE-01 pacing, exponential backoff, stable jitter and pending-rerun \
            are owned by android FamilyRelayBackpressureTest.kt + RelayRerunPolicyTest.kt and ios \
            FamilyRelayBackpressureTests.swift; hoist in package B0"]
fn rate_01_pacing_and_backoff_are_still_shell_owned() {
    unimplemented!("package B0 moves the pacer, the backoff curve and the rerun action into core");
}

#[test]
#[ignore = "HOIST-PENDING: ENDPOINT-01 hint authoring is owned by android LanEndpointSendPolicyTest.kt \
            and LanEndpointCacheTest.kt (core validates hosts and scopes relay-change notices, but \
            does not author the hint); hoist in package D2/D3"]
fn endpoint_01_hint_authoring_is_still_shell_owned() {
    unimplemented!("package D2/D3 moves LAN endpoint hint authoring into mesh_meet");
}

#[test]
#[ignore = "HOIST-PENDING: WM-01 receipt repair is owned by android ReceiptRepairTest.kt and ios \
            ReceiptRepairTests.swift; no core model or stateful test exists; hoist in package D2"]
fn wm_01_receipt_repair_has_no_core_model() {
    unimplemented!("package D2 gives receipt repair a bounded, reachable core state machine");
}

#[test]
#[ignore = "HOIST-PENDING: SPRAY-01 byte budget is core (digest spray plan) but the per-encounter \
            byte cap toward one peer and the re-offer cadence gate are not built; the cooldown half \
            is owned by android PeripheralSprayCooldownTest.kt; see issue #280; hoist in package D2"]
fn spray_01_has_no_cadence_gate_or_per_peer_byte_cap() {
    unimplemented!(
        "issue #280: 34 copies of an 18,795-byte frame queued to one peer in one second"
    );
}

#[test]
#[ignore = "UNIMPLEMENTED: owned by package C0 (CoreRelayPass declared request/envelope/byte/time \
            budgets and its adversarial property tests)"]
fn live_01_pass_budgets_are_not_declared_anywhere_yet() {
    unimplemented!("package C0 gives a pass explicit budgets it must terminate inside");
}

#[test]
#[ignore = "UNIMPLEMENTED: owned by package C0 (CoreRelayPass continuation with an explicit \
            progress reason); the walk-budget yield is already core in relay_cursor.rs"]
fn progress_01_continuations_carry_no_progress_reason_yet() {
    unimplemented!("package C0 requires a strict cursor advance or a strictly later deadline");
}

#[test]
#[ignore = "UNIMPLEMENTED: owned by package C0 (CoreRelayPass duplicate/late/out-of-order event \
            permutation tests)"]
fn idemp_01_external_result_replay_is_not_modelled_yet() {
    unimplemented!("package C0 makes duplicate, late and wrong-pass results provably inert");
}

#[test]
#[ignore = "UNIMPLEMENTED: owned by package C0 (CoreRelayPass two-transaction page ingest and \
            frontier commit under fault injection)"]
fn txn_01_transaction_boundaries_are_not_enforced_by_a_test_yet() {
    unimplemented!("package C0 proves no store transaction spans external I/O");
}

#[test]
#[ignore = "UNIMPLEMENTED: owned by the fix for issue #283 — outbound_envelopes has no \
            receipt-coverage retirement and no supersession, only a flat 7-day expiry for every \
            kind; see fixture zombie-outbound-queue.jsonl"]
fn queue_01_the_outbound_queue_has_no_retirement_path_but_expiry() {
    unimplemented!(
        "issue #283: proof of delivery must permit retirement and short-lived payloads must be \
         superseded, so the advertised set shrinks under coverage"
    );
}

// ---------------------------------------------------------------------------
// Fixture corpus
// ---------------------------------------------------------------------------

const SCHEMA: &str = "cruisemesh.protocol-event/v1";

/// The fixture set named by the contract. A file that is not here, or a name
/// here with no file, is a mistake in one of the two places.
const FIXTURES: &[&str] = &[
    "429-mid-receipts",
    "ack-fail-after-consume",
    "carry-storm",
    "contact-silence-no-proof",
    "oversize-shrink",
    "pending-rerun-during-backoff",
    "short-page",
    "sweep-livelock",
    "watchdog-spray",
    "watermark-lock",
    "zombie-outbound-queue",
];

/// Stable event codes. Codes are API; prose log messages are not.
const EVENT_CODES: &[&str] = &[
    "action_emitted",
    "action_result_accepted",
    "action_result_stale_ignored",
    "budget_yield",
    "carried_row_marked",
    "continuation_scheduled",
    "endpoint_recovered",
    "endpoint_rested",
    "frontier_advanced",
    "frontier_held",
    "invariant_violation",
    "outbound_queue_scanned",
    "page_ingested",
    "pass_finish",
    "pass_start",
    "rate_limit_abort",
    "receipt_watermark_observed",
    "shadow_mismatch",
    "silence_observed",
    "spray_planned",
    "request_rejected",
];

const HEADER_KEYS: &[&str] = &[
    "schema",
    "record",
    "fixture",
    "title",
    "origin",
    "public_reference",
    "pseudonyms",
    "expect_invariants",
];

const EVENT_KEYS: &[&str] = &[
    "record",
    "seq",
    "at_ms",
    "code",
    "session",
    "pass",
    "action",
    "actor",
    "invariants",
    "counts",
    "outcome",
];

/// Substrings that must never appear in anything exportable. Checked against
/// the raw file bytes, not the parsed model, so a leak in an unexpected place
/// is still caught.
const CANARIES: &[(&str, &str)] = &[
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

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

fn read_fixture(stem: &str) -> String {
    let path = fixtures_dir().join(format!("{stem}.jsonl"));
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn parse_records(stem: &str, raw: &str) -> Vec<serde_json::Value> {
    raw.lines()
        .enumerate()
        .map(|(index, line)| {
            assert!(
                !line.trim().is_empty(),
                "{stem}.jsonl line {}: JSONL has no blank lines",
                index + 1
            );
            serde_json::from_str(line)
                .unwrap_or_else(|error| panic!("{stem}.jsonl line {}: {error}", index + 1))
        })
        .collect()
}

fn object_keys(value: &serde_json::Value) -> BTreeSet<String> {
    value
        .as_object()
        .expect("record is a JSON object")
        .keys()
        .cloned()
        .collect()
}

#[test]
fn the_fixture_directory_holds_exactly_the_named_corpus() {
    let mut found: Vec<String> = fs::read_dir(fixtures_dir())
        .expect("fixtures directory exists")
        .map(|entry| entry.expect("readable dir entry").path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
        .map(|path| {
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .expect("utf-8 fixture name")
                .to_string()
        })
        .collect();
    found.sort();

    let expected: Vec<String> = FIXTURES.iter().map(|name| name.to_string()).collect();
    assert_eq!(
        found, expected,
        "the fixture directory and the named corpus disagree"
    );

    // And the contract document must name each one, so a fixture cannot be
    // added without saying which incident it stands for.
    for name in FIXTURES {
        assert!(
            CONTRACT_DOC.contains(&format!("`{name}.jsonl`")),
            "{name}.jsonl is not listed in specs/protocol-contract-v1.md section 6.5"
        );
    }
}

#[test]
fn every_fixture_matches_the_versioned_schema() {
    for stem in FIXTURES {
        let raw = read_fixture(stem);
        assert!(
            raw.ends_with('\n'),
            "{stem}.jsonl must end with a newline so appends stay well-formed"
        );
        assert!(
            !raw.contains('\r'),
            "{stem}.jsonl must use LF line endings; a CRLF file is not the same bytes on two hosts"
        );

        let records = parse_records(stem, &raw);
        assert!(
            records.len() >= 2,
            "{stem}.jsonl needs a header and at least one event"
        );

        // --- header ---
        let header = &records[0];
        assert_eq!(
            header["schema"].as_str(),
            Some(SCHEMA),
            "{stem}.jsonl header must declare {SCHEMA}"
        );
        assert_eq!(
            header["record"].as_str(),
            Some("header"),
            "{stem}.jsonl line 1 must be the header record"
        );
        assert_eq!(
            header["fixture"].as_str(),
            Some(*stem),
            "{stem}.jsonl header fixture name must match the filename"
        );
        let title = header["title"].as_str().unwrap_or("");
        assert!(
            title.len() > 20,
            "{stem}.jsonl needs a title a stranger can read, got {title:?}"
        );
        let origin = header["origin"].as_str().unwrap_or("");
        assert!(
            origin == "synthetic" || origin == "redacted-field-archive",
            "{stem}.jsonl origin must be synthetic or redacted-field-archive, got {origin:?}"
        );

        let allowed_header: BTreeSet<String> =
            HEADER_KEYS.iter().map(|key| key.to_string()).collect();
        let header_keys = object_keys(header);
        assert!(
            header_keys.is_subset(&allowed_header),
            "{stem}.jsonl header has keys outside the schema: {:?}",
            header_keys.difference(&allowed_header).collect::<Vec<_>>()
        );

        let pseudonyms: BTreeSet<String> = header["pseudonyms"]
            .as_array()
            .unwrap_or_else(|| panic!("{stem}.jsonl header needs a pseudonyms array"))
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("{stem}.jsonl pseudonyms must be strings"))
                    .to_string()
            })
            .collect();
        assert!(
            !pseudonyms.is_empty(),
            "{stem}.jsonl must declare its actors"
        );

        let declared: BTreeSet<String> = header["expect_invariants"]
            .as_array()
            .unwrap_or_else(|| panic!("{stem}.jsonl header needs expect_invariants"))
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("{stem}.jsonl expect_invariants must be strings"))
                    .to_string()
            })
            .collect();
        assert!(
            !declared.is_empty(),
            "{stem}.jsonl must declare at least one expected invariant"
        );
        for id in &declared {
            assert!(
                CONTRACT.iter().any(|entry| entry.id == id),
                "{stem}.jsonl declares unknown invariant {id}"
            );
        }

        // --- events ---
        let allowed_event: BTreeSet<String> =
            EVENT_KEYS.iter().map(|key| key.to_string()).collect();
        let mut previous_at_ms = i64::MIN;
        let mut referenced: BTreeSet<String> = BTreeSet::new();

        for (offset, record) in records[1..].iter().enumerate() {
            let line = offset + 2;
            assert_eq!(
                record["record"].as_str(),
                Some("event"),
                "{stem}.jsonl line {line}: only line 1 may be a header"
            );

            let keys = object_keys(record);
            assert!(
                keys.is_subset(&allowed_event),
                "{stem}.jsonl line {line} has keys outside the schema: {:?}",
                keys.difference(&allowed_event).collect::<Vec<_>>()
            );

            let seq = record["seq"]
                .as_i64()
                .unwrap_or_else(|| panic!("{stem}.jsonl line {line}: seq must be an integer"));
            assert_eq!(
                seq,
                offset as i64 + 1,
                "{stem}.jsonl line {line}: seq must start at 1 and increase by exactly 1"
            );

            let at_ms = record["at_ms"]
                .as_i64()
                .unwrap_or_else(|| panic!("{stem}.jsonl line {line}: at_ms must be an integer"));
            assert!(
                at_ms >= previous_at_ms,
                "{stem}.jsonl line {line}: time must not run backwards"
            );
            previous_at_ms = at_ms;

            let code = record["code"]
                .as_str()
                .unwrap_or_else(|| panic!("{stem}.jsonl line {line}: code must be a string"));
            assert!(
                EVENT_CODES.contains(&code),
                "{stem}.jsonl line {line}: {code} is not a stable event code"
            );

            if let Some(actor) = record.get("actor") {
                let actor = actor
                    .as_str()
                    .unwrap_or_else(|| panic!("{stem}.jsonl line {line}: actor must be a string"));
                assert!(
                    pseudonyms.contains(actor),
                    "{stem}.jsonl line {line}: {actor} is not a declared pseudonym"
                );
            }

            if let Some(action) = record.get("action") {
                let action = action
                    .as_i64()
                    .unwrap_or_else(|| panic!("{stem}.jsonl line {line}: action must be an int"));
                assert!(
                    action >= 0,
                    "{stem}.jsonl line {line}: action ids are non-negative"
                );
            }

            if let Some(counts) = record.get("counts") {
                let counts = counts.as_object().unwrap_or_else(|| {
                    panic!("{stem}.jsonl line {line}: counts must be a flat object")
                });
                for (key, value) in counts {
                    let number = value.as_i64().unwrap_or_else(|| {
                        panic!("{stem}.jsonl line {line}: counts.{key} must be an integer")
                    });
                    assert!(
                        number >= 0,
                        "{stem}.jsonl line {line}: counts.{key} must be non-negative"
                    );
                }
            }

            if let Some(outcome) = record.get("outcome") {
                let outcome = outcome.as_str().unwrap_or_else(|| {
                    panic!("{stem}.jsonl line {line}: outcome must be a string")
                });
                assert!(
                    !outcome.contains(' ') && outcome.len() <= 48,
                    "{stem}.jsonl line {line}: outcome must be a short stable token, not prose \
                     ({outcome:?})"
                );
            }

            if let Some(ids) = record.get("invariants") {
                for value in ids.as_array().unwrap_or_else(|| {
                    panic!("{stem}.jsonl line {line}: invariants must be a list")
                }) {
                    let id = value.as_str().unwrap_or_else(|| {
                        panic!("{stem}.jsonl line {line}: invariant ids must be strings")
                    });
                    assert!(
                        declared.contains(id),
                        "{stem}.jsonl line {line}: {id} is referenced but not declared in the \
                         header's expect_invariants"
                    );
                    referenced.insert(id.to_string());
                }
            }
        }

        assert_eq!(
            referenced, declared,
            "{stem}.jsonl declares invariants no event references; the header is the file's index"
        );
    }
}

#[test]
fn secret_01_fixtures_carry_no_credentials_endpoints_or_plaintext() {
    let id = lookup("SECRET-01").id;

    for stem in FIXTURES {
        let raw = read_fixture(stem);
        for (canary, what) in CANARIES {
            contract_assert!(
                id,
                !raw.contains(canary),
                "{stem}.jsonl contains {what} ({canary:?})"
            );
        }

        // Structural half: only schema keys may appear, so a leak cannot be
        // smuggled in under an unrecognised field name.
        let allowed: BTreeSet<String> = HEADER_KEYS
            .iter()
            .chain(EVENT_KEYS.iter())
            .map(|key| key.to_string())
            .collect();
        for (index, record) in parse_records(stem, &raw).iter().enumerate() {
            let extra: Vec<String> = object_keys(record).difference(&allowed).cloned().collect();
            contract_assert!(
                id,
                extra.is_empty(),
                "{stem}.jsonl line {} carries non-schema keys {extra:?}",
                index + 1
            );
        }

        // Every string value is either a schema token or a declared
        // pseudonym; none of them is bytes.
        for (index, record) in parse_records(stem, &raw).iter().enumerate() {
            for (key, value) in record.as_object().expect("object record") {
                let Some(text) = value.as_str() else { continue };
                contract_assert!(
                    id,
                    text.len() <= 120,
                    "{stem}.jsonl line {} field {key} is long enough to be a payload",
                    index + 1
                );
                contract_assert!(
                    id,
                    !text.chars().any(|c| c.is_control()),
                    "{stem}.jsonl line {} field {key} contains control bytes",
                    index + 1
                );
            }
        }
    }
}

#[test]
fn every_invariant_is_exercised_by_at_least_one_fixture_or_is_explicitly_not_yet() {
    // Fixtures are the field-evidence half of the contract. Not every rule
    // needs one (SECRET-01 is checked over the whole corpus rather than by a
    // trace of its own), but the mapping must be deliberate rather than
    // accidental, so the exemptions are named here.
    const NO_FIXTURE_NEEDED: &[&str] = &["ACK-01", "SECRET-01", "HELLO-01", "ENDPOINT-01", "UI-01"];

    let mut covered: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for stem in FIXTURES {
        let raw = read_fixture(stem);
        let records = parse_records(stem, &raw);
        for value in records[0]["expect_invariants"]
            .as_array()
            .expect("expect_invariants")
        {
            covered
                .entry(value.as_str().expect("string id").to_string())
                .or_default()
                .push(stem);
        }
    }

    for entry in CONTRACT {
        if NO_FIXTURE_NEEDED.contains(&entry.id) {
            assert!(
                !covered.contains_key(entry.id),
                "{} is listed as needing no fixture but one declares it; update the list",
                entry.id
            );
            continue;
        }
        assert!(
            covered.contains_key(entry.id),
            "{} has no incident fixture. Either add one or add it to NO_FIXTURE_NEEDED with a \
             reason.",
            entry.id
        );
    }
}
