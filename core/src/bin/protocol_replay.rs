//! `protocol-replay` — read a protocol-event transcript and say what is wrong
//! with it.
//!
//! One command for three kinds of file, because there is only one format: a
//! checked-in fixture from `core/tests/fixtures/`, a `mesh_sim` transcript, or
//! the archive a person exported from Advanced diagnostics and mailed to
//! support. The archive is accepted as it comes out of the zip; there is no
//! conversion step, and if one were ever needed the format would have failed.
//!
//! What it does today is stated in `--help` and worth stating here too: it
//! validates the schema, ordering and redaction, checks every invariant id a
//! record claims against Contract v1, walks the transcript looking for the
//! first place it contradicts itself, and prints a redacted summary. It does
//! **not** re-execute the decisions against a `MessageStore` — that arrives
//! with the session work in the C wave. A clean run means "nothing in this
//! file contradicts itself", not "this device behaved correctly".

use std::path::Path;
use std::process::ExitCode;

use cruisemesh_core::{replay, validate, ProtocolEventArchive, ReplaySummary};

const USAGE: &str = "\
protocol-replay — validate and replay a CruiseMesh protocol-event transcript

USAGE:
    protocol-replay [OPTIONS] <FILE>...

    <FILE> is JSONL in the cruisemesh.protocol-event/v1 schema: a fixture from
    core/tests/fixtures/, a simulation transcript, or a diagnostics archive
    exported by hand from a phone's Advanced screen. All three are the same
    format, so an exported archive is passed in directly.

OPTIONS:
    -q, --quiet    Print only failures.
    -h, --help     Print this help.

WHAT IT CHECKS
    * the versioned schema: every required field, and no key on any line that
      the schema does not declare — a leak cannot be smuggled in under a field
      name nobody recognises;
    * sequence numbers, which run consecutively from the header's first_seq
      (absent means 1; an exported ring that has evicted its oldest records
      says so here rather than renumbering and pretending to be new);
    * that time never runs backwards;
    * redaction: no relay token, friend card, deep link, URL, credential
      header, key block or private-address literal;
    * that every invariant id a record claims exists in Contract v1 and is
      declared in the header;
    * transcript self-consistency: a pass that starts twice, one that works on
      after its own rate-limit abort or after finishing, and a frontier that
      claims to advance without moving.

WHAT IT DOES NOT DO YET
    It does not replay the events against a real MessageStore, so it cannot
    tell you the device was correct — only that its own account of itself
    holds together. Behavioural replay lands with the core relay session
    (package C0).

EXIT STATUS
    0  every file validated and replayed with no divergence
    1  at least one file failed
    2  the command was used wrongly
";

fn main() -> ExitCode {
    let mut paths: Vec<String> = Vec::new();
    let mut quiet = false;

    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-q" | "--quiet" => quiet = true,
            other if other.starts_with('-') => {
                eprintln!("protocol-replay: unknown option {other}\n");
                print!("{USAGE}");
                return ExitCode::from(2);
            }
            other => paths.push(other.to_string()),
        }
    }

    if paths.is_empty() {
        eprintln!("protocol-replay: no input file\n");
        print!("{USAGE}");
        return ExitCode::from(2);
    }

    let mut failed = false;
    for path in &paths {
        if !check(Path::new(path), quiet) {
            failed = true;
        }
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn check(path: &Path, quiet: bool) -> bool {
    let name = path.display();
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("{name}: cannot read: {error}");
            return false;
        }
    };

    let archive = match validate(&text) {
        Ok(archive) => archive,
        Err(defects) => {
            eprintln!("{name}: {} problem(s)", defects.len());
            for defect in &defects {
                if defect.line == 0 {
                    eprintln!("  {}", defect.detail);
                } else {
                    eprintln!("  line {}: {}", defect.line, defect.detail);
                }
            }
            return false;
        }
    };

    let summary = replay(&archive);
    if let Some(divergence) = &summary.divergence {
        eprintln!(
            "{name}: first divergence at record {}: {}",
            divergence.line, divergence.detail
        );
        eprintln!("{}", render(&archive, &summary));
        return false;
    }

    if !quiet {
        println!("{name}: ok");
        println!("{}", render(&archive, &summary));
    }
    true
}

/// The redacted summary. Everything here is a count, a pseudonym or a stable
/// code, so the output of this command is as safe to paste into a support
/// thread as the file that produced it.
fn render(archive: &ProtocolEventArchive, summary: &ReplaySummary) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "  {} ({}), {} record(s), seq {}..{}, span {} ms\n",
        archive.header.fixture,
        archive.header.origin,
        summary.records,
        summary.first_seq,
        summary.last_seq,
        summary.span_ms
    ));
    if summary.undated_records > 0 {
        out.push_str(&format!(
            "  {} record(s) carry an inferred timestamp: nothing told the ring the time at that \
             point, so they are ordered, not dated, and the span above excludes them\n",
            summary.undated_records
        ));
    }
    if archive.header.first_seq > 1 {
        out.push_str(&format!(
            "  the ring had evicted {} older record(s) before this export\n",
            archive.header.first_seq - 1
        ));
    }
    out.push_str(&format!(
        "  actors: {}\n",
        archive.header.pseudonyms.join(", ")
    ));
    if summary.invariants_exercised.is_empty() {
        out.push_str("  invariants exercised: none\n");
    } else {
        out.push_str(&format!(
            "  invariants exercised: {}\n",
            summary.invariants_exercised.join(", ")
        ));
    }
    out.push_str("  by code:\n");
    for (code, count) in &summary.by_code {
        out.push_str(&format!("    {code:<28} {count}\n"));
    }
    out.push_str(
        "  note: validated and replayed as a transcript. Behavioural replay against a store \
         arrives with package C0.",
    );
    out
}
