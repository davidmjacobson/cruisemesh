use data_encoding::BASE64URL_NOPAD;
use serde::{Deserialize, Serialize};

use crate::CoreError;

const RELAY_MAX_SEALED_BYTES: usize = 512 * 1024;
const RELAY_FETCH_MAX_ROWS: usize = 16;
const RELAY_FETCH_MAX_DECODED_BYTES: usize = RELAY_FETCH_MAX_ROWS * RELAY_MAX_SEALED_BYTES;
const RELAY_MAX_RESPONSE_BODY_BYTES: usize = 12 * 1024 * 1024;
const RELAY_MAX_PRESENCE_ROWS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayFetchedEnvelope {
    pub id: i64,
    pub msg_id: Vec<u8>,
    pub hop_ttl: u8,
    pub recipient_hint: Vec<u8>,
    pub sealed: Vec<u8>,
    pub expiry_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayFetchPage {
    pub envelopes: Vec<CoreRelayFetchedEnvelope>,
    pub next_cursor: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayPresence {
    pub hint: Vec<u8>,
    pub last_seen_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRelayPresencePage {
    pub now_ms: i64,
    pub presence: Vec<CoreRelayPresence>,
}

#[derive(Serialize, Deserialize)]
struct EnvelopeWire {
    id: i64,
    msg_id: String,
    hop_ttl: u8,
    recipient_hint: String,
    sealed: String,
    expiry_ms: i64,
}

#[derive(Serialize, Deserialize)]
struct PostEnvelopeWire {
    msg_id: String,
    hop_ttl: u8,
    recipient_hint: String,
    sealed: String,
    expiry_ms: i64,
}

#[derive(Deserialize)]
struct PostResponse {
    id: i64,
}

#[derive(Deserialize)]
struct FetchResponse {
    envelopes: Vec<EnvelopeWire>,
    next_cursor: i64,
}

#[derive(Serialize)]
struct AckRequest {
    ids: Vec<i64>,
}

#[derive(Serialize)]
struct PresenceRequest {
    announce: Vec<String>,
    query: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct PresenceWire {
    hint: String,
    last_seen_ms: i64,
}

#[derive(Deserialize)]
struct PresenceResponse {
    now_ms: i64,
    presence: Vec<PresenceWire>,
}

#[uniffi::export]
pub fn normalize_relay_url(value: String) -> String {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{trimmed}")
    }
}

/// Maximum response body that either mobile shell may accumulate before
/// cancelling the relay request. The core repeats this check at every decoder
/// so callers outside the first-party shells cannot bypass it.
#[uniffi::export]
pub fn relay_max_response_bytes() -> u32 {
    RELAY_MAX_RESPONSE_BODY_BYTES as u32
}

/// Fetch pages stay deliberately small because every sealed row is controlled
/// by the relay until it has passed the authenticated envelope ingest path.
#[uniffi::export]
pub fn relay_fetch_batch_limit() -> u32 {
    RELAY_FETCH_MAX_ROWS as u32
}

#[uniffi::export]
pub fn relay_encode_post_envelope(
    msg_id: Vec<u8>,
    hop_ttl: u8,
    recipient_hint: Vec<u8>,
    sealed: Vec<u8>,
    expiry_ms: i64,
) -> Result<Vec<u8>, CoreError> {
    validate_envelope(&msg_id, &recipient_hint, &sealed)?;
    json_encode(&PostEnvelopeWire {
        msg_id: b64(&msg_id),
        hop_ttl,
        recipient_hint: b64(&recipient_hint),
        sealed: b64(&sealed),
        expiry_ms,
    })
}

#[uniffi::export]
pub fn relay_decode_post_response(body: Vec<u8>) -> Result<i64, CoreError> {
    validate_response_body(&body)?;
    let id = json_decode::<PostResponse>(&body)?.id;
    if id < 0 {
        return Err(malformed("relay id cannot be negative"));
    }
    Ok(id)
}

#[uniffi::export]
pub fn relay_build_fetch_path(
    hints: Vec<Vec<u8>>,
    after_id: i64,
    limit: u32,
) -> Result<String, CoreError> {
    if limit == 0 || limit as usize > RELAY_FETCH_MAX_ROWS {
        return Err(malformed("relay fetch limit is out of range"));
    }
    if after_id < 0 {
        return Err(malformed("relay cursor cannot be negative"));
    }
    for hint in &hints {
        validate_hint(hint)?;
    }
    Ok(format!(
        "/envelopes?hints={}&after={after_id}&limit={limit}",
        hints
            .iter()
            .map(|hint| b64(hint))
            .collect::<Vec<_>>()
            .join(",")
    ))
}

#[uniffi::export]
pub fn relay_decode_fetch_page(body: Vec<u8>) -> Result<CoreRelayFetchPage, CoreError> {
    validate_response_body(&body)?;
    let wire = json_decode::<FetchResponse>(&body)?;
    if wire.envelopes.len() > RELAY_FETCH_MAX_ROWS {
        return Err(malformed("relay fetch page contains too many envelopes"));
    }
    if wire.next_cursor < 0 {
        return Err(malformed("relay cursor cannot be negative"));
    }

    let mut envelopes = Vec::with_capacity(wire.envelopes.len());
    let mut decoded_bytes = 0usize;
    let mut previous_id = None;
    for item in wire.envelopes {
        if item.id < 0 || previous_id.is_some_and(|previous| item.id <= previous) {
            return Err(malformed(
                "relay envelope ids must be non-negative and increasing",
            ));
        }
        validate_b64_len(&item.msg_id, 16, "relay msg_id")?;
        validate_b64_len(&item.recipient_hint, 8, "relay recipient hint")?;
        if item.sealed.is_empty() || item.sealed.len() > max_b64_len(RELAY_MAX_SEALED_BYTES) {
            return Err(malformed("relay sealed payload is too large"));
        }

        let msg_id = b64_decode(&item.msg_id)?;
        let recipient_hint = b64_decode(&item.recipient_hint)?;
        let sealed = b64_decode(&item.sealed)?;
        validate_envelope(&msg_id, &recipient_hint, &sealed)?;
        decoded_bytes = decoded_bytes
            .checked_add(sealed.len())
            .ok_or_else(|| malformed("relay fetch page payload size overflow"))?;
        if decoded_bytes > RELAY_FETCH_MAX_DECODED_BYTES {
            return Err(malformed("relay fetch page payload is too large"));
        }
        previous_id = Some(item.id);
        envelopes.push(CoreRelayFetchedEnvelope {
            id: item.id,
            msg_id,
            hop_ttl: item.hop_ttl,
            recipient_hint,
            sealed,
            expiry_ms: item.expiry_ms,
        });
    }
    if previous_id.is_some_and(|last_id| wire.next_cursor != last_id) {
        return Err(malformed("relay cursor does not match the fetch page"));
    }
    Ok(CoreRelayFetchPage {
        envelopes,
        next_cursor: wire.next_cursor,
    })
}

#[uniffi::export]
pub fn relay_encode_ack_request(ids: Vec<i64>) -> Result<Vec<u8>, CoreError> {
    if ids.iter().any(|id| *id < 0) {
        return Err(malformed("relay id cannot be negative"));
    }
    json_encode(&AckRequest { ids })
}

#[uniffi::export]
pub fn relay_encode_presence_request(
    announce: Vec<Vec<u8>>,
    query: Vec<Vec<u8>>,
) -> Result<Vec<u8>, CoreError> {
    for hint in announce.iter().chain(&query) {
        validate_hint(hint)?;
    }
    json_encode(&PresenceRequest {
        announce: announce.iter().map(|v| b64(v)).collect(),
        query: query.iter().map(|v| b64(v)).collect(),
    })
}

#[uniffi::export]
pub fn relay_decode_presence_page(body: Vec<u8>) -> Result<CoreRelayPresencePage, CoreError> {
    validate_response_body(&body)?;
    let wire = json_decode::<PresenceResponse>(&body)?;
    if wire.presence.len() > RELAY_MAX_PRESENCE_ROWS {
        return Err(malformed("relay presence page contains too many entries"));
    }
    let presence = wire
        .presence
        .into_iter()
        .map(|item| {
            let hint = b64_decode(&item.hint)?;
            validate_hint(&hint)?;
            Ok(CoreRelayPresence {
                hint,
                last_seen_ms: item.last_seen_ms,
            })
        })
        .collect::<Result<Vec<_>, CoreError>>()?;
    Ok(CoreRelayPresencePage {
        now_ms: wire.now_ms,
        presence,
    })
}

fn validate_envelope(msg_id: &[u8], hint: &[u8], sealed: &[u8]) -> Result<(), CoreError> {
    if msg_id.len() != 16 {
        return Err(malformed("relay msg_id must be 16 bytes"));
    }
    validate_hint(hint)?;
    if sealed.is_empty() {
        return Err(malformed("relay sealed payload cannot be empty"));
    }
    if sealed.len() > RELAY_MAX_SEALED_BYTES {
        return Err(malformed("relay sealed payload is too large"));
    }
    Ok(())
}

fn validate_hint(hint: &[u8]) -> Result<(), CoreError> {
    if hint.len() != 8 {
        return Err(malformed("relay recipient hint must be 8 bytes"));
    }
    Ok(())
}

fn b64(bytes: &[u8]) -> String {
    BASE64URL_NOPAD.encode(bytes)
}
fn b64_decode(value: &str) -> Result<Vec<u8>, CoreError> {
    BASE64URL_NOPAD
        .decode(value.as_bytes())
        .map_err(|_| malformed("invalid relay base64url"))
}
fn validate_b64_len(value: &str, decoded_len: usize, field: &str) -> Result<(), CoreError> {
    if value.len() != max_b64_len(decoded_len) {
        return Err(malformed(&format!("{field} has invalid encoded length")));
    }
    Ok(())
}
fn max_b64_len(decoded_len: usize) -> usize {
    decoded_len.saturating_mul(4).saturating_add(2) / 3
}
fn validate_response_body(body: &[u8]) -> Result<(), CoreError> {
    if body.len() > RELAY_MAX_RESPONSE_BODY_BYTES {
        return Err(malformed("relay response body is too large"));
    }
    Ok(())
}
fn json_encode<T: Serialize>(value: &T) -> Result<Vec<u8>, CoreError> {
    serde_json::to_vec(value).map_err(|e| malformed(&format!("invalid relay JSON: {e}")))
}
fn json_decode<T: for<'de> Deserialize<'de>>(body: &[u8]) -> Result<T, CoreError> {
    serde_json::from_slice(body).map_err(|e| malformed(&format!("invalid relay JSON: {e}")))
}
fn malformed(message: &str) -> CoreError {
    CoreError::Malformed(message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_urls() {
        assert_eq!(
            normalize_relay_url(" relay.example/ ".into()),
            "https://relay.example"
        );
        assert_eq!(
            normalize_relay_url("http://127.0.0.1:8080/".into()),
            "http://127.0.0.1:8080"
        );
    }

    #[test]
    fn post_and_fetch_wire_round_trip() {
        let body = relay_encode_post_envelope(vec![1; 16], 7, vec![2; 8], vec![3; 20], 99).unwrap();
        let item: PostEnvelopeWire = serde_json::from_slice(&body).unwrap();
        let response = serde_json::to_vec(&serde_json::json!({"envelopes": [{"id": 4,
            "msg_id": item.msg_id, "hop_ttl": item.hop_ttl, "recipient_hint": item.recipient_hint,
            "sealed": item.sealed, "expiry_ms": item.expiry_ms}], "next_cursor": 4}))
        .unwrap();
        let page = relay_decode_fetch_page(response).unwrap();
        assert_eq!(page.envelopes[0].msg_id, vec![1; 16]);
        assert_eq!(page.next_cursor, 4);
    }

    #[test]
    fn rejects_bad_lengths_and_base64() {
        assert!(relay_encode_post_envelope(vec![1; 15], 7, vec![2; 8], vec![3], 9).is_err());
        let bad = br#"{"envelopes":[{"id":1,"msg_id":"!","hop_ttl":7,"recipient_hint":"AgICAgICAgI","sealed":"Aw","expiry_ms":9}],"next_cursor":1}"#.to_vec();
        assert!(relay_decode_fetch_page(bad).is_err());
    }

    #[test]
    fn exposes_bounded_fetch_policy() {
        assert_eq!(relay_fetch_batch_limit(), 16);
        assert_eq!(relay_max_response_bytes(), 12 * 1024 * 1024);
        assert!(relay_build_fetch_path(vec![], 0, 16).is_ok());
        assert!(relay_build_fetch_path(vec![], 0, 17).is_err());
        assert!(relay_build_fetch_path(vec![], -1, 1).is_err());
    }

    #[test]
    fn rejects_oversized_or_non_monotonic_fetch_pages() {
        let encoded_msg_id = b64(&[1; 16]);
        let encoded_hint = b64(&[2; 8]);
        let oversized = b64(&vec![3; RELAY_MAX_SEALED_BYTES + 1]);
        let body = serde_json::to_vec(&serde_json::json!({
            "envelopes": [{"id": 1, "msg_id": encoded_msg_id, "hop_ttl": 7,
                "recipient_hint": encoded_hint, "sealed": oversized, "expiry_ms": 9}],
            "next_cursor": 1
        }))
        .unwrap();
        assert!(relay_decode_fetch_page(body).is_err());

        let sealed = b64(&[3]);
        let repeated = serde_json::to_vec(&serde_json::json!({
            "envelopes": [
                {"id": 2, "msg_id": encoded_msg_id, "hop_ttl": 7,
                    "recipient_hint": encoded_hint, "sealed": sealed, "expiry_ms": 9},
                {"id": 2, "msg_id": encoded_msg_id, "hop_ttl": 7,
                    "recipient_hint": encoded_hint, "sealed": sealed, "expiry_ms": 9}
            ],
            "next_cursor": 2
        }))
        .unwrap();
        assert!(relay_decode_fetch_page(repeated).is_err());
    }

    #[test]
    fn rejects_response_body_above_transport_cap_before_json_decode() {
        let body = vec![b' '; RELAY_MAX_RESPONSE_BODY_BYTES + 1];
        assert!(relay_decode_fetch_page(body).is_err());
    }
}
