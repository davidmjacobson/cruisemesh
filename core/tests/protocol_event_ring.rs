//! The protocol-event ring, end to end through the public store API.
//!
//! `core/src/protocol_event.rs` unit-tests the ring's own mechanics (caps,
//! eviction order, the redaction backstop, the replay checks). This file is
//! about the seam the plan actually promises: that a *live* store, driven
//! through its ordinary methods, produces an archive the replay command
//! consumes directly, with no conversion step and nothing secret in it.

use std::sync::Arc;

use cruisemesh_core::{
    generate_identity, replay, validate, Contact, CoreSprayLanePlan, CoreSprayPlanShape,
    CoreSprayPolicy, CoreSprayTrigger, MessageStore, KIND_PROFILE_SYNC, PROTOCOL_EVENT_MAX_BYTES,
    PROTOCOL_EVENT_MAX_RECORDS,
};

fn store() -> Arc<MessageStore> {
    Arc::new(MessageStore::open(":memory:".to_string()).expect("in-memory store"))
}

/// The config key a real walk uses: a relay URL and a token, which is exactly
/// the shape that must never reach an event.
const MAILBOX_KEY: &str = "https://relay.example.invalid/|cmdep1-secrettoken";

#[test]
fn a_mailbox_walk_produces_an_archive_the_replay_command_consumes_directly() {
    let store = store();
    let key = MAILBOX_KEY.to_string();

    // A page that lands: the frontier moves.
    store
        .advance_relay_fetch_cursor(key.clone(), 100, true)
        .expect("advance");
    // A page that returns rows without moving the relay's cursor: held.
    store
        .advance_relay_fetch_cursor(key.clone(), 100, true)
        .expect("held");
    // A sweep starts, resumes, and finishes.
    store
        .advance_relay_sweep_cursor(key.clone(), 40, true, 1_700_000_000_000)
        .expect("sweep start");
    store
        .advance_relay_sweep_cursor(key.clone(), 90, true, 1_700_000_001_000)
        .expect("sweep resume");
    let lowered = store
        .note_relay_sweep_completed(key.clone(), 1_700_000_002_000, 60)
        .expect("sweep complete");
    assert!(
        lowered,
        "a completed sweep above the top lowers the frontier"
    );

    // And the one plug point the shells still own.
    store
        .note_relay_rate_limit_abort(key.clone(), 15_000, 2, 131, 1_700_000_003_000)
        .expect("rate limit");

    let text = store.export_protocol_events_jsonl().expect("export");
    let archive = validate(&text).unwrap_or_else(|defects| panic!("{defects:?}"));
    let summary = replay(&archive);
    assert!(summary.divergence.is_none(), "{:?}", summary.divergence);

    let codes: Vec<&str> = summary.by_code.keys().copied().collect();
    for expected in [
        "frontier_advanced",
        "frontier_held",
        "sweep_started",
        "sweep_resumed",
        "sweep_completed",
        "frontier_lowered",
        "rate_limit_abort",
    ] {
        assert!(
            codes.contains(&expected),
            "{expected} missing from {codes:?}"
        );
    }
    assert_eq!(
        archive.header.origin, "redacted-field-archive",
        "an exported ring must say where it came from"
    );
}

#[test]
fn the_export_is_a_file_the_command_reads_off_disk_with_no_conversion() {
    // The requirement is verbatim: "export from a live store -> command
    // consumes it directly". Writing it to a temp file and running the same
    // entry point the binary runs is the only way to prove there is no step in
    // between -- a test that only called `validate` on a `String` would still
    // pass if the archive needed reformatting before it could be a file.
    let store = store();
    store
        .advance_relay_fetch_cursor(MAILBOX_KEY.to_string(), 12, true)
        .expect("advance");
    let text = store.export_protocol_events_jsonl().expect("export");

    let path = std::env::temp_dir().join(format!(
        "cruisemesh-protocol-events-{}.jsonl",
        std::process::id()
    ));
    std::fs::write(&path, &text).expect("write archive");
    let read_back = std::fs::read_to_string(&path).expect("read archive");
    std::fs::remove_file(&path).ok();

    assert_eq!(read_back, text, "the archive survives a round trip to disk");
    let archive = validate(&read_back).unwrap_or_else(|defects| panic!("{defects:?}"));
    assert!(replay(&archive).divergence.is_none());
    assert!(
        read_back.ends_with('\n'),
        "JSONL ends with a newline so an append stays well-formed"
    );
    assert!(
        !read_back.contains('\r'),
        "the archive must be the same bytes on every host"
    );
}

#[test]
fn secret_01_a_planted_token_cannot_reach_the_ring() {
    // The canary is planted where a leak would realistically come from: the
    // relay config key (which genuinely holds a deposit token), a message
    // payload, and a contact's raw user id. All three drive real emit points
    // in this test.
    let store = store();
    let token = "cmdep1-plantedcanary";
    let key = format!("https://relay.example.invalid/|{token}");

    store
        .advance_relay_fetch_cursor(key.clone(), 5, true)
        .expect("advance");
    store
        .advance_relay_sweep_cursor(key.clone(), 5, true, 1_700_000_000_000)
        .expect("sweep");

    let contact = b"contact-user-id-with-cmdep1-in-it".to_vec();
    store
        .note_contact_relay_rejected(contact.clone(), 1_700_000_001_000)
        .expect("rejected");
    store
        .note_contact_relay_unreachable(
            contact.clone(),
            "https://192.168.1.9/".to_string(),
            1_700_000_002_000,
        )
        .expect("unreachable");

    let text = store.export_protocol_events_jsonl().expect("export");
    for canary in [
        token,
        "cmdep1-",
        "://",
        "192.168.",
        "contact-user-id",
        "relay.example.invalid",
    ] {
        assert!(
            !text.contains(canary),
            "the exported ring leaked {canary:?}:\n{text}"
        );
    }
    validate(&text).unwrap_or_else(|defects| panic!("{defects:?}"));
}

#[test]
fn secret_01_the_scanner_would_catch_a_leak_if_one_got_through() {
    // Negative control. Without this, the test above proves only that the
    // scanner never fires -- which an empty scanner also achieves.
    let store = store();
    store
        .advance_relay_fetch_cursor(MAILBOX_KEY.to_string(), 5, true)
        .expect("advance");
    let clean = store.export_protocol_events_jsonl().expect("export");
    validate(&clean).expect("the real archive is clean");

    let tampered = clean.replace(
        "\"outcome\":\"page_fully_processed\"",
        "\"outcome\":\"cmdep1-leaked\"",
    );
    assert_ne!(tampered, clean, "the tamper must actually change the file");
    let defects = validate(&tampered).expect_err("a planted token must fail validation");
    assert!(
        defects
            .iter()
            .any(|defect| defect.detail.contains("SECRET-01")),
        "{defects:?}"
    );
}

#[test]
fn a_message_payload_has_no_route_into_the_ring_at_all() {
    // Two generations of a profile snapshot authored to the same contact: the
    // second supersedes the first, which is a real emit point reached through
    // the real authoring path. Both carry a canary in the payload, and the
    // sealed envelope, chat id and msg id all pass through the same
    // transaction the event is written in. If any of them had a field in the
    // event schema to arrive in, this is where it would show up.
    let store = store();
    let me = generate_identity();
    let peer = generate_identity();
    let secret = "meet me at cmdep1-nowhere, the code is Bearer hunter2";
    store
        .upsert_contact(Contact {
            user_id: peer.user_id.clone(),
            name: "Robin".to_string(),
            sign_pk: peer.sign_pk.clone(),
            agree_pk: peer.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        })
        .expect("accept contact");
    let contact = store
        .get_contact(peer.user_id.clone())
        .expect("read contact")
        .expect("the contact is there");

    for generation in 0..2 {
        store
            .author_pairwise_message(
                me.clone(),
                contact.clone(),
                KIND_PROFILE_SYNC,
                secret.as_bytes().to_vec(),
                None,
                1_700_000_000_000 + generation,
            )
            .expect("author");
    }

    let text = store.export_protocol_events_jsonl().expect("export");
    assert!(!text.contains("meet me"), "{text}");
    assert!(!text.contains("cmdep1-"), "{text}");
    assert!(!text.contains("Bearer"), "{text}");
    assert!(
        text.contains("outbound_row_superseded"),
        "the supersession itself must still be recorded: {text}"
    );
    validate(&text).unwrap_or_else(|defects| panic!("{defects:?}"));
}

#[test]
fn spray_decisions_reach_the_ring_once_a_journal_is_attached() {
    let store = store();
    // `MessageStore::open` records its own launch-time retirement sweep, so
    // start from a clean ring to make the "nothing is listening" claim about
    // the spray policy and not about the store.
    store.clear_protocol_events().expect("clear");
    let policy = CoreSprayPolicy::new();

    // Unattached, the policy behaves exactly as it did before the ring.
    let plan = CoreSprayPlanShape {
        carried: CoreSprayLanePlan {
            set_digest: 11,
            bytes: 4_096,
        },
        own_outbound: CoreSprayLanePlan {
            set_digest: 22,
            bytes: 512,
        },
        own_receipts: CoreSprayLanePlan {
            set_digest: 0,
            bytes: 0,
        },
    };
    policy.admit_plan(
        "aabbcc".to_string(),
        "link-1".to_string(),
        plan,
        1_700_000_000_000,
    );
    assert!(
        !store.has_protocol_events().expect("has"),
        "nothing is listening yet"
    );

    policy.attach_event_journal(Arc::clone(&store));
    // The same set again inside the re-offer interval: suppressed.
    policy.admit_plan(
        "aabbcc".to_string(),
        "link-1".to_string(),
        plan,
        1_700_000_001_000,
    );
    // A different set: admitted.
    let changed = CoreSprayPlanShape {
        carried: CoreSprayLanePlan {
            set_digest: 33,
            bytes: 8_192,
        },
        ..plan
    };
    policy.admit_plan(
        "aabbcc".to_string(),
        "link-1".to_string(),
        changed,
        1_700_000_002_000,
    );

    let text = store.export_protocol_events_jsonl().expect("export");
    assert!(text.contains("spray_suppressed"), "{text}");
    assert!(text.contains("spray_admitted"), "{text}");
    assert!(!text.contains("aabbcc"), "the raw peer key must not appear");
    let archive = validate(&text).unwrap_or_else(|defects| panic!("{defects:?}"));
    assert!(replay(&archive).divergence.is_none());
}

#[test]
fn a_spray_gate_never_blocks_on_the_ring() {
    // The policy holds its own mutex, and the store holds another. If the
    // spray lock were still held when the ring was written, this pattern --
    // gate, admit, gate again, from the same thread -- would deadlock rather
    // than fail an assertion. It runs in the ordinary test harness precisely
    // so a regression shows up as a hung test rather than as a field ANR.
    let store = store();
    let policy = CoreSprayPolicy::new();
    policy.attach_event_journal(Arc::clone(&store));
    for round in 0..8 {
        let now = 1_700_000_000_000 + round * 250;
        policy.may_spray(
            "ddeeff".to_string(),
            "link-2".to_string(),
            CoreSprayTrigger::Reconnect,
            now,
        );
        policy.admit_plan(
            "ddeeff".to_string(),
            "link-2".to_string(),
            CoreSprayPlanShape {
                carried: CoreSprayLanePlan {
                    set_digest: round as u64,
                    bytes: 1_024,
                },
                own_outbound: CoreSprayLanePlan {
                    set_digest: 0,
                    bytes: 0,
                },
                own_receipts: CoreSprayLanePlan {
                    set_digest: 0,
                    bytes: 0,
                },
            },
            now,
        );
    }
    assert!(store.has_protocol_events().expect("has"));
}

#[test]
fn a_long_running_device_never_exceeds_either_cap_and_stays_valid() {
    // The soak at the store level rather than the ring level: 6,000 real emit
    // points, three times the record cap, through the same locked path a page
    // walk uses.
    let store = store();
    let key = MAILBOX_KEY.to_string();
    let started = std::time::Instant::now();
    for index in 0..3_000i64 {
        store
            .advance_relay_fetch_cursor(key.clone(), index + 1, true)
            .expect("advance");
        // And one that holds, so both branches are exercised.
        store
            .advance_relay_fetch_cursor(key.clone(), index + 1, true)
            .expect("held");
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed.as_secs() < 60,
        "6,000 emitting store calls took {elapsed:?}"
    );

    let text = store.export_protocol_events_jsonl().expect("export");
    let archive = validate(&text).unwrap_or_else(|defects| panic!("{defects:?}"));
    assert_eq!(archive.events.len() as i64, PROTOCOL_EVENT_MAX_RECORDS);
    assert!(text.len() as i64 <= PROTOCOL_EVENT_MAX_BYTES + 1_024);
    assert!(
        archive.header.first_seq > 1,
        "the ring evicted, and the header must say so"
    );
    assert!(replay(&archive).divergence.is_none());
}

#[test]
fn clearing_the_ring_leaves_nothing_to_export() {
    let store = store();
    store
        .advance_relay_fetch_cursor(MAILBOX_KEY.to_string(), 7, true)
        .expect("advance");
    assert!(store.has_protocol_events().expect("has"));
    store.clear_protocol_events().expect("clear");
    assert!(
        !store.has_protocol_events().expect("has"),
        "delete captured diagnostics must be true of the ring too"
    );
    let text = store.export_protocol_events_jsonl().expect("export");
    assert!(
        !text.contains("frontier_advanced"),
        "the cleared records must not come back: {text}"
    );
}

#[test]
fn the_generic_violation_hook_refuses_anything_that_is_not_a_code() {
    let store = store();
    store
        .note_invariant_violation(
            "CURSOR-01".to_string(),
            "frontier_went_backwards".to_string(),
            1,
        )
        .expect("a real id and a real token");
    assert!(store
        .note_invariant_violation("NOPE-01".to_string(), "whatever".to_string(), 1)
        .is_err());
    assert!(
        store
            .note_invariant_violation(
                "CURSOR-01".to_string(),
                "the frontier moved backwards after a 429 from relay.example.invalid".to_string(),
                1,
            )
            .is_err(),
        "prose is the easiest place in the system to leak a message body"
    );

    let text = store.export_protocol_events_jsonl().expect("export");
    assert!(text.contains("frontier_went_backwards"), "{text}");
    assert!(!text.contains("relay.example.invalid"), "{text}");
}

#[test]
fn a_clock_that_steps_backwards_cannot_make_the_ring_invalid() {
    // Explicit test case from the plan's determinism rules. A device whose
    // wall clock jumps back an hour mid-cruise still produces a transcript
    // whose time never runs backwards, because the ring clamps forward.
    let store = store();
    let key = MAILBOX_KEY.to_string();
    store
        .advance_relay_sweep_cursor(key.clone(), 10, true, 1_700_000_100_000)
        .expect("sweep");
    store
        .advance_relay_sweep_cursor(key.clone(), 20, true, 1_699_000_000_000)
        .expect("sweep after the clock stepped back");
    let text = store.export_protocol_events_jsonl().expect("export");
    validate(&text).unwrap_or_else(|defects| panic!("{defects:?}"));
}
