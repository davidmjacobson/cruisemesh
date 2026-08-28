//! What a JSON decode failure is allowed to say about the bytes that caused
//! it.
//!
//! Every JSON body this crate decodes arrived from somewhere else: a relay's
//! HTTP response, a friend card handed over a mesh link, a directory or an
//! introduction forwarded by a peer. `serde_json::Error`'s own `Display` is
//! written for a developer holding the document, so it quotes the document:
//! an invalid-type message carries the offending string, and the
//! unknown-variant and unknown-field messages carry the name that was not
//! recognised. All three reproduce a run of the input verbatim.
//! Interpolating one into a `CoreError` message puts that run inside a string
//! both shells log, and the log is a file a user exports and mails to whoever
//! is helping them.
//!
//! So the message is rebuilt here from the parts of the failure that describe
//! the *shape* of it and nothing else. What survives is what a reader
//! actually works from: which of the four things went wrong, where in the
//! document it went wrong, and how much document there was.
//!
//! Deliberately not the offending token, not the field name, not a preview.
//! A field name is usually one of ours and would often be safe — but "usually"
//! is not a property a call site can check, and one helper with a single rule
//! is what keeps the next decode site from having to decide.

/// Describes a `serde_json` failure over `len` bytes without reproducing any
/// of them.
///
/// The three parts, in the order a reader uses them:
///
/// * the category — `syntax` (not JSON at all), `data` (JSON, but not this
///   shape), `eof` (it stopped early), `io` (the reader failed under us);
/// * the position, which is what turns "somewhere in a 40 KB page" into a
///   line and column to look at;
/// * the total size, which separates a truncated body from a complete one
///   that disagrees with us, and is the number the response-cap and
///   page-shrink paths are reasoned about in.
///
/// A position of line 0 column 0 is what `serde_json` reports when the
/// failure has no location (an I/O error, chiefly), so it is left off rather
/// than printed as a coordinate nobody can look up.
pub(crate) fn json_fault(error: &serde_json::Error, len: usize) -> String {
    let category = match error.classify() {
        serde_json::error::Category::Io => "io",
        serde_json::error::Category::Syntax => "syntax",
        serde_json::error::Category::Data => "data",
        serde_json::error::Category::Eof => "eof",
    };
    let (line, column) = (error.line(), error.column());
    if line == 0 && column == 0 {
        return format!("{category} error in {len}B");
    }
    format!("{category} error at line {line} column {column} of {len}B")
}

#[cfg(test)]
mod tests {
    use super::json_fault;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[allow(dead_code)]
    struct Shape {
        id: i64,
    }

    /// The marker is the whole point: `serde_json` would have quoted it, and
    /// this is the assertion that stops a future edit from letting it back
    /// in. Whole-string equality rather than "does not contain", so appending
    /// anything at all to the message has to be a deliberate act.
    #[test]
    fn a_value_the_parser_rejected_is_not_reproduced() {
        let body = br#"{"id":"MARKER-4a1f-not-a-number"}"#;
        let error = serde_json::from_slice::<Shape>(body).unwrap_err();
        assert!(
            error.to_string().contains("MARKER-4a1f-not-a-number"),
            "serde_json stopped quoting the input; this helper's reason to \
             exist changed, not just its expected text: {error}"
        );
        assert_eq!(
            json_fault(&error, body.len()),
            "data error at line 1 column 32 of 33B"
        );
    }

    /// An unknown field is the other way input gets quoted, and the one a
    /// peer running a newer build produces by accident rather than on
    /// purpose.
    #[test]
    fn an_unexpected_field_name_is_not_reproduced() {
        #[derive(Debug, Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct Strict {
            id: i64,
        }
        let body = br#"{"id":1,"MARKERFIELD":2}"#;
        let error = serde_json::from_slice::<Strict>(body).unwrap_err();
        assert!(error.to_string().contains("MARKERFIELD"));
        assert_eq!(
            json_fault(&error, body.len()),
            "data error at line 1 column 21 of 24B"
        );
    }

    #[test]
    fn bytes_that_are_not_json_at_all_are_a_syntax_error() {
        let body = b"<html>MARKERPORTAL</html>";
        let error = serde_json::from_slice::<Shape>(body).unwrap_err();
        assert_eq!(
            json_fault(&error, body.len()),
            "syntax error at line 1 column 1 of 25B"
        );
    }

    /// A body that stopped part-way reads as `eof`, which is the difference
    /// between "the relay disagrees with us" and "the link cut the page in
    /// half" -- and the reason the size travels with the category.
    #[test]
    fn a_truncated_body_is_an_eof_error() {
        let body = br#"{"id":1"#;
        let error = serde_json::from_slice::<Shape>(body).unwrap_err();
        assert_eq!(
            json_fault(&error, body.len()),
            "eof error at line 1 column 7 of 7B"
        );
    }

    #[test]
    fn a_later_line_keeps_its_position() {
        let body = b"{\n  \"id\": \"MARKER\"\n}";
        let error = serde_json::from_slice::<Shape>(body).unwrap_err();
        assert_eq!(
            json_fault(&error, body.len()),
            "data error at line 2 column 16 of 20B"
        );
    }
}
