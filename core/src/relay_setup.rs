//! Portable Shore Pass setup cards shared by the purchase site and both apps.
//!
//! The card is intentionally carried in a URL fragment. Browsers do not send
//! fragments to the server, which keeps the household relay token out of
//! access logs while still allowing one-tap App Link / Universal Link setup.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use data_encoding::{BASE64URL_NOPAD, HEXLOWER};
use serde::{Deserialize, Serialize};

use crate::{normalize_relay_url, CoreError};

const RELAY_SETUP_PREFIX: &str = "CMRELAY1:";
const MAX_RELAY_SETUP_TEXT_BYTES: usize = 8 * 1024;
const MAX_RELAY_URL_BYTES: usize = 2 * 1024;
const MAX_RELAY_TOKEN_BYTES: usize = 1024;
/// Four bytes, printed as eight hex characters. Wide enough that the handful
/// of passes one support conversation compares never collide by accident,
/// short enough to sit inline in a log line.
const RELAY_TOKEN_FINGERPRINT_LEN: usize = 4;
/// Domain separation, so this digest can never be confused with, or replayed
/// against, any other BLAKE2b use in the protocol.
const RELAY_TOKEN_FINGERPRINT_DOMAIN: &[u8] = b"CruiseMesh relay token fingerprint v1\0";
/// The hosted relay's host name, kept as a macro so both the host and the full
/// URL below come from one literal. Anything needing the official relay must
/// use one of the two consts rather than its own copy of the string: the
/// friend-card v3 encoder compresses that URL to a single tag byte on exact
/// string equality (`specs/friend-card-v3.md`), so a second copy that drifted
/// would quietly cost every shared card ~31 bytes.
macro_rules! official_relay_host {
    () => {
        "relay.cruisemesh.app"
    };
}

const OFFICIAL_RELAY_HOST: &str = official_relay_host!();

/// Canonical base URL of the hosted relay — the single source of truth.
pub(crate) const OFFICIAL_RELAY_URL: &str = concat!("https://", official_relay_host!());

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RelaySetup {
    pub relay_url: String,
    pub relay_token: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RelaySetupWire {
    v: u8,
    relay_url: String,
    relay_token: String,
}

/// Encode a canonical `CMRELAY1:` setup card.
#[uniffi::export]
pub fn make_relay_setup_card(relay_url: String, relay_token: String) -> Result<String, CoreError> {
    let setup = validate_setup(relay_url, relay_token)?;
    let json = serde_json::to_vec(&RelaySetupWire {
        v: 1,
        relay_url: setup.relay_url,
        relay_token: setup.relay_token,
    })
    .map_err(|error| invalid_setup(error.to_string()))?;
    Ok(format!(
        "{RELAY_SETUP_PREFIX}{}",
        BASE64URL_NOPAD.encode(&json)
    ))
}

/// Parse a bare setup card, an `https://cruisemesh.app/r#...` link, or prose
/// containing either form.
#[uniffi::export]
pub fn parse_relay_setup_text(text: String) -> Result<RelaySetup, CoreError> {
    if text.len() > MAX_RELAY_SETUP_TEXT_BYTES {
        return Err(invalid_setup("relay setup text is too large"));
    }
    let start = text
        .find(RELAY_SETUP_PREFIX)
        .ok_or_else(|| invalid_setup("missing CMRELAY1 card"))?;
    let encoded_start = start + RELAY_SETUP_PREFIX.len();
    let encoded = text[encoded_start..]
        .split(|character: char| {
            !character.is_ascii_alphanumeric() && character != '-' && character != '_'
        })
        .next()
        .unwrap_or_default();
    if encoded.is_empty() {
        return Err(invalid_setup("relay setup card is incomplete"));
    }
    let json = BASE64URL_NOPAD
        .decode(encoded.as_bytes())
        .map_err(|_| invalid_setup("relay setup card is not valid base64url"))?;
    if json.len() > MAX_RELAY_SETUP_TEXT_BYTES {
        return Err(invalid_setup("relay setup card is too large"));
    }
    let wire: RelaySetupWire = serde_json::from_slice(&json)
        .map_err(|_| invalid_setup("relay setup card payload is not valid JSON"))?;
    if wire.v != 1 {
        return Err(invalid_setup(format!(
            "unsupported relay setup version {}",
            wire.v
        )));
    }
    validate_setup(wire.relay_url, wire.relay_token)
}

/// True when a setup card points at the hosted Shore Pass service.
///
/// URL fragments are unsigned, so anyone can mint a `cruisemesh.app/r#…`
/// link naming their own relay. The shells auto-accept first-time setup only
/// for the official host and require an explicit host confirmation for
/// everything else. Deliberately strict: any port, path, userinfo, or
/// trailing-dot variant is "not official" — the only cost of a false
/// negative is one confirmation tap.
#[uniffi::export]
pub fn relay_setup_is_official(relay_url: String) -> bool {
    match normalize_relay_url(relay_url).strip_prefix("https://") {
        Some(host) => host.eq_ignore_ascii_case(OFFICIAL_RELAY_HOST),
        None => false,
    }
}

/// Short, stable, non-reversible label for a Shore Pass token, for logs.
///
/// A shared diagnostics log has to be able to answer "which pass is this
/// phone using, and is it the same one as in yesterday's log" without the
/// file carrying the pass itself. Truncation cannot do both jobs: every
/// character it prints is a character of a live bearer credential. A digest
/// can — the same token always produces the same label, and the label says
/// nothing about the token that produced it.
///
/// Both shells call this rather than hashing on their own: two hand-written
/// digests would drift, and the moment they did, a support person comparing
/// an Android archive against an iPhone's would stop seeing a match with
/// nothing failing to say so. Changing the domain string or the output length
/// breaks that same correlation across app versions, so don't.
#[uniffi::export]
pub fn relay_token_fingerprint(relay_token: String) -> String {
    let mut hasher =
        Blake2bVar::new(RELAY_TOKEN_FINGERPRINT_LEN).expect("valid blake2b output length");
    hasher.update(RELAY_TOKEN_FINGERPRINT_DOMAIN);
    hasher.update(relay_token.as_bytes());
    let mut out = [0u8; RELAY_TOKEN_FINGERPRINT_LEN];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    HEXLOWER.encode(&out)
}

fn validate_setup(relay_url: String, relay_token: String) -> Result<RelaySetup, CoreError> {
    let relay_url = normalize_relay_url(relay_url);
    let relay_token = relay_token.trim().to_string();
    if relay_url.is_empty() {
        return Err(invalid_setup("relay URL is required"));
    }
    if !relay_url.starts_with("https://") {
        return Err(invalid_setup("relay URL must use HTTPS"));
    }
    if relay_url.len() > MAX_RELAY_URL_BYTES {
        return Err(invalid_setup("relay URL is too large"));
    }
    if relay_url.chars().any(char::is_whitespace) {
        return Err(invalid_setup("relay URL contains whitespace"));
    }
    if relay_token.is_empty() {
        return Err(invalid_setup("relay token is required"));
    }
    if relay_token.len() > MAX_RELAY_TOKEN_BYTES {
        return Err(invalid_setup("relay token is too large"));
    }
    if relay_token.chars().any(char::is_whitespace) {
        return Err(invalid_setup("relay token contains whitespace"));
    }
    Ok(RelaySetup {
        relay_url,
        relay_token,
    })
}

fn invalid_setup(message: impl Into<String>) -> CoreError {
    CoreError::Malformed(format!("invalid relay setup: {}", message.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOLDEN: &str = "CMRELAY1:eyJ2IjoxLCJyZWxheV91cmwiOiJodHRwczovL3JlbGF5LmV4YW1wbGUiLCJyZWxheV90b2tlbiI6ImFiYzEyMyJ9";

    #[test]
    fn canonical_card_matches_web_golden_vector() {
        assert_eq!(
            make_relay_setup_card("relay.example/".into(), "abc123".into()).unwrap(),
            GOLDEN
        );
    }

    #[test]
    fn parses_card_from_fragment_link_and_prose() {
        let setup = parse_relay_setup_text(format!(
            "Open https://cruisemesh.app/r#{GOLDEN} on your phone."
        ))
        .unwrap();
        assert_eq!(setup.relay_url, "https://relay.example");
        assert_eq!(setup.relay_token, "abc123");
    }

    #[test]
    fn official_host_matches_exactly_and_case_insensitively() {
        assert!(relay_setup_is_official(
            "https://relay.cruisemesh.app".into()
        ));
        assert!(relay_setup_is_official(
            "https://relay.cruisemesh.app/".into()
        ));
        assert!(relay_setup_is_official(
            "https://RELAY.CRUISEMESH.APP".into()
        ));
        assert!(relay_setup_is_official("relay.cruisemesh.app".into()));

        assert!(!relay_setup_is_official("https://relay.example".into()));
        assert!(!relay_setup_is_official(
            "https://relay.cruisemesh.app.evil.example".into()
        ));
        assert!(!relay_setup_is_official(
            "https://evil.example/relay.cruisemesh.app".into()
        ));
        assert!(!relay_setup_is_official(
            "https://relay.cruisemesh.app:8443".into()
        ));
        assert!(!relay_setup_is_official(
            "https://relay.cruisemesh.app/path".into()
        ));
        assert!(!relay_setup_is_official(
            "https://user@relay.cruisemesh.app".into()
        ));
        assert!(!relay_setup_is_official(
            "https://relay.cruisemesh.app.".into()
        ));
        assert!(!relay_setup_is_official(
            "http://relay.cruisemesh.app".into()
        ));
        assert!(!relay_setup_is_official(String::new()));
    }

    #[test]
    fn rejects_insecure_unknown_and_oversized_cards() {
        let insecure = RelaySetupWire {
            v: 1,
            relay_url: "http://relay.example".into(),
            relay_token: "abc123".into(),
        };
        let insecure = format!(
            "{RELAY_SETUP_PREFIX}{}",
            BASE64URL_NOPAD.encode(&serde_json::to_vec(&insecure).unwrap())
        );
        assert!(parse_relay_setup_text(insecure).is_err());

        let unknown = format!(
            "{RELAY_SETUP_PREFIX}{}",
            BASE64URL_NOPAD
                .encode(br#"{"v":2,"relay_url":"https://relay.example","relay_token":"abc123"}"#)
        );
        assert!(parse_relay_setup_text(unknown).is_err());
        assert!(parse_relay_setup_text("x".repeat(MAX_RELAY_SETUP_TEXT_BYTES + 1)).is_err());
    }

    const HEX_TOKEN: &str = "4ac9f24f8b1e4d7fae0c3b19d6725f88";
    const FAMILY_TOKEN: &str = "cmfam1-9d41c0b7e2a54f16";

    #[test]
    fn token_fingerprint_is_stable_and_distinguishes_passes() {
        for token in [HEX_TOKEN, FAMILY_TOKEN] {
            let first = relay_token_fingerprint(token.to_string());
            assert_eq!(first, relay_token_fingerprint(token.to_string()));
            assert_eq!(first.len(), RELAY_TOKEN_FINGERPRINT_LEN * 2);
            assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        }
        assert_ne!(
            relay_token_fingerprint(HEX_TOKEN.to_string()),
            relay_token_fingerprint(FAMILY_TOKEN.to_string())
        );
    }

    #[test]
    fn token_fingerprint_leaks_no_run_of_the_token() {
        for token in [HEX_TOKEN, FAMILY_TOKEN] {
            let fingerprint = relay_token_fingerprint(token.to_string());
            // No window of the token, down to a two-character run, survives
            // into the label -- which is what separates a digest from a
            // prefix, and the property a future "just make it shorter"
            // refactor would break.
            for width in 2..=token.len() {
                for window in token.as_bytes().windows(width) {
                    let run = std::str::from_utf8(window).unwrap();
                    assert!(
                        !fingerprint.contains(run),
                        "fingerprint {fingerprint} contains token run {run}"
                    );
                }
            }
        }
    }

    /// Pins the exact bytes. Both shells restate these vectors in their own
    /// suites; if the domain string or the length ever moves, all three fail
    /// together rather than two archives silently stopping matching.
    #[test]
    fn token_fingerprint_matches_golden_vectors() {
        assert_eq!(relay_token_fingerprint(HEX_TOKEN.to_string()), "056855d3");
        assert_eq!(
            relay_token_fingerprint(FAMILY_TOKEN.to_string()),
            "6ae48e6b"
        );
    }
}
