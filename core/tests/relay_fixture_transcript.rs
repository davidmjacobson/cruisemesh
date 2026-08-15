//! The reference half of the paired-platform fixture proof.
//!
//! `relay_pass_replay.rs` executes the relay-shaped fixtures in Rust and
//! asserts what each scenario demands. This file checks the *other* thing the
//! adapter suites depend on: that `core_relay_fixture_*` is a scenario source
//! two shells can execute at all — every wired name resolves, the reference
//! run terminates and is deterministic, the transcript carries no endpoint,
//! and no scenario reports a declared invariant violated.
//!
//! The comparison this file cannot make is the interesting one, and it is
//! deliberately not here: whether the Android and iOS drivers produce the same
//! transcript. That needs a JVM and an XCTest, and it lives in
//! `RelayAdapterFixtureTranscriptTest.kt` and
//! `RelayAdapterFixtureTranscriptTests.swift`, both comparing against
//! `core_relay_fixture_expected_transcript` — the string this file pins the
//! properties of.

use cruisemesh_core::{
    core_relay_fixture_expected_transcript, core_relay_fixture_names, core_relay_fixture_plan,
    core_relay_fixture_scenario, core_relay_fixture_seed_store, MessageStore,
};
use std::sync::Arc;

/// Every wired name has a scenario, a seeding arm and a reply script. A name
/// added to the table without them panics here rather than in a phone suite.
#[test]
fn every_wired_fixture_resolves_end_to_end() {
    let names = core_relay_fixture_names();
    assert!(!names.is_empty(), "the adapter suites iterate this list");
    for name in names {
        let scenario = core_relay_fixture_scenario(name.clone());
        assert_eq!(scenario.name, name);
        assert!(
            !scenario.passes.is_empty(),
            "{name}: a scenario with no pass drives nothing"
        );
        assert!(
            !scenario.declared_invariants.is_empty(),
            "{name}: a fixture executes to prove some invariant held"
        );

        // The seeding and the plan are what a shell asks for first.
        let store = Arc::new(MessageStore::open(":memory:".to_string()).expect("store"));
        core_relay_fixture_seed_store(store.clone(), name.clone());
        for index in 0..scenario.passes.len() {
            let plan = core_relay_fixture_plan(
                name.clone(),
                index as u32,
                "https://own.example".to_string(),
                "own-token".to_string(),
                "https://contact.example".to_string(),
                "contact-token".to_string(),
            );
            assert!(plan.own.is_some(), "{name}: every pass walks own mail");
            assert_eq!(
                plan.contacts.is_empty(),
                !scenario.uses_contact_endpoint,
                "{name}: the contact flag must match the plan it produces"
            );
        }
    }
}

/// Each fixture's name appears on disk. A scenario named for a fixture that
/// does not exist would be a claim about nothing.
#[test]
fn every_wired_fixture_names_a_file_in_the_corpus() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures");
    for name in core_relay_fixture_names() {
        let path = dir.join(format!("{name}.jsonl"));
        assert!(
            path.exists(),
            "{name} is wired for the adapter suites but no fixture file carries that name"
        );
    }
}

/// The expected transcript is what the shells compare against, so it must be
/// the same string every time it is asked for. A run that varied by clock,
/// map order or allocation address would fail a phone suite intermittently and
/// look like a driver bug.
#[test]
fn the_reference_transcript_is_deterministic() {
    for name in core_relay_fixture_names() {
        let first = core_relay_fixture_expected_transcript(name.clone());
        let second = core_relay_fixture_expected_transcript(name.clone());
        assert_eq!(
            first, second,
            "{name}: the reference run is not deterministic"
        );
        assert!(
            first.starts_with(&format!("fixture {name}\n")),
            "{name}: the transcript names its fixture first"
        );
        assert!(
            first.contains("\n  = outcome="),
            "{name}: every scenario ends its passes with a summary"
        );
    }
}

/// `SECRET-01`, against the one artefact of this work a person is most likely
/// to paste somewhere: no URL, no host, no token, no recipient hint.
#[test]
fn the_reference_transcript_carries_no_endpoint() {
    for name in core_relay_fixture_names() {
        let text = core_relay_fixture_expected_transcript(name.clone());
        for canary in [
            "relay.example",
            "contact-relay.example",
            "member-token",
            "Bearer",
            "https://",
        ] {
            assert!(
                !text.contains(canary),
                "{name}: the normalised transcript leaked {canary}\n{text}"
            );
        }
        assert!(
            !text.contains("hints=") || text.contains("hints=<hints>"),
            "{name}: a fetch's recipient hints must be redacted, not printed"
        );
        assert!(
            !text.contains("ALTERED"),
            "{name}: the reference run cannot alter its own request"
        );
    }
}

/// The claim each fixture is about: driving the scenario reports none of the
/// invariants the fixture declares as violated. This is the same assertion
/// `relay_pass_replay.rs` makes per scenario, made here over the scenarios the
/// adapter suites run so a shell failure and a core failure are the same
/// failure — and read off the transcript itself, which is what the two phone
/// suites compare, rather than off a store only this file can see.
#[test]
fn no_scenario_reports_a_declared_invariant_violated() {
    for name in core_relay_fixture_names() {
        let scenario = core_relay_fixture_scenario(name.clone());
        let text = core_relay_fixture_expected_transcript(name.clone());
        let line = text
            .lines()
            .find(|line| line.starts_with("violations "))
            .unwrap_or_else(|| panic!("{name}: the transcript must report its violations"));
        assert_eq!(
            line, "violations ",
            "{name}: the scenario reported an invariant violated"
        );
        for declared in &scenario.declared_invariants {
            assert!(
                !line.contains(declared.as_str()),
                "{name}: the session reported {declared} violated"
            );
        }
    }
}
