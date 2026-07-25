//! Portable Cruise Pass setup cards shared by the purchase site and both apps.
//!
//! The card is intentionally carried in a URL fragment. Browsers do not send
//! fragments to the server, which keeps the household relay token out of
//! access logs while still allowing one-tap App Link / Universal Link setup.

use data_encoding::BASE64URL_NOPAD;
use serde::{Deserialize, Serialize};

use crate::{normalize_relay_url, CoreError};

const RELAY_SETUP_PREFIX: &str = "CMRELAY1:";
const MAX_RELAY_SETUP_TEXT_BYTES: usize = 8 * 1024;
const MAX_RELAY_URL_BYTES: usize = 2 * 1024;
const MAX_RELAY_TOKEN_BYTES: usize = 1024;

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
}
