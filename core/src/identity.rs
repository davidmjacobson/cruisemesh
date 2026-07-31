//! Identity, keypairs, UserID derivation, and friend-card encoding.
//!
//! Scheme (DESIGN.md §6.2): each identity is an Ed25519 signing keypair plus an
//! X25519 agreement keypair, generated on-device. UserID = first 16 bytes of
//! BLAKE2b(Ed25519 public key). Friending exchanges a FriendCard (JSON, carried
//! over QR code or pasted text) containing both public keys.

use crate::store::{contact_display_name, Contact};
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use data_encoding::{BASE32_NOPAD, BASE64URL_NOPAD};
use ed25519_dalek::SigningKey;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};

const USER_ID_LEN: usize = 16;
/// Legacy link form: base64url of the FriendCard JSON. Still accepted on scan
/// forever (old cards in the field); no longer emitted (see [`make_friend_link`]).
const FRIEND_LINK_PREFIX: &str = "CMFRIEND1:";
/// Compact link form (T12): base64url of a binary FriendCard layout, roughly
/// half the size of v1 because the 32-byte keys are raw bytes instead of JSON
/// number arrays. This is what [`make_friend_link`] now emits.
const FRIEND_LINK_PREFIX_V2: &str = "CMFRIEND2:";
const MAX_FRIEND_CARD_JSON_BYTES: usize = 16 * 1024;
const MAX_FRIEND_TEXT_BYTES: usize = 24 * 1024;
const MAX_DISPLAY_NAME_BYTES: usize = 128;
const MAX_RELAY_URL_BYTES: usize = 2 * 1024;
const MAX_RELAY_TOKEN_BYTES: usize = 1024;

/// A locally generated identity: both keypairs, private material included.
///
/// The app is responsible for persisting `sign_sk` / `agree_sk` securely
/// (e.g. Android Keystore-backed storage); the core does not persist anything.
#[derive(uniffi::Record, Clone)]
pub struct Identity {
    pub user_id: Vec<u8>,
    pub sign_pk: Vec<u8>,
    pub sign_sk: Vec<u8>,
    pub agree_pk: Vec<u8>,
    pub agree_sk: Vec<u8>,
}

/// The public, shareable half of an identity — what a QR code / friend-request
/// string actually carries. No secret material.
#[derive(uniffi::Record, Clone, Debug, Serialize, Deserialize)]
pub struct FriendCard {
    pub name: String,
    pub sign_pk: Vec<u8>,
    pub agree_pk: Vec<u8>,
    pub relay_url: Option<String>,
    pub relay_token: Option<String>,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreError {
    #[error("invalid friend card: {0}")]
    InvalidFriendCard(String),
    #[error("invalid key length: expected {expected}, got {actual}")]
    InvalidKeyLength { expected: u32, actual: u32 },
    #[error("message store error: {0}")]
    Store(String),
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("signature verification failed")]
    SignatureInvalid,
    #[error("malformed wire data: {0}")]
    Malformed(String),
}

/// Generate a fresh identity: Ed25519 signing keypair + X25519 agreement keypair.
#[uniffi::export]
pub fn generate_identity() -> Identity {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();

    let agree_sk = StaticSecret::random_from_rng(OsRng);
    let agree_pk = XPublicKey::from(&agree_sk);

    let user_id = derive_user_id(verifying_key.as_bytes());

    Identity {
        user_id: user_id.to_vec(),
        sign_pk: verifying_key.as_bytes().to_vec(),
        sign_sk: signing_key.to_bytes().to_vec(),
        agree_pk: agree_pk.as_bytes().to_vec(),
        agree_sk: agree_sk.to_bytes().to_vec(),
    }
}

/// UserID = first 16 bytes of BLAKE2b(Ed25519 public key).
pub(crate) fn derive_user_id(sign_pk: &[u8]) -> [u8; USER_ID_LEN] {
    let mut hasher = Blake2bVar::new(USER_ID_LEN).expect("valid blake2b output length");
    hasher.update(sign_pk);
    let mut out = [0u8; USER_ID_LEN];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    out
}

/// Human-shareable form of a UserID, e.g. `CM-K7QX-9M2P-3F8J-QRTZ-...`.
#[uniffi::export]
pub fn format_user_id(user_id: Vec<u8>) -> String {
    let encoded = BASE32_NOPAD.encode(&user_id);
    let grouped = encoded
        .as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join("-");
    format!("CM-{grouped}")
}

/// Short verbal-verification phrase for a UserID (Signal-safety-number style),
/// not a security boundary by itself — a convenience for "read this out loud".
#[uniffi::export]
pub fn fingerprint_words(user_id: Vec<u8>) -> Vec<String> {
    (0..4)
        .map(|i| {
            let byte = user_id.get(i).copied().unwrap_or(0);
            WORDLIST[byte as usize % WORDLIST.len()].to_string()
        })
        .collect()
}

/// Derive the UserID that a FriendCard corresponds to (from its signing key).
#[uniffi::export]
pub fn friend_card_user_id(card: FriendCard) -> Vec<u8> {
    derive_user_id(&card.sign_pk).to_vec()
}

/// What an incoming friend card means relative to the contacts already saved.
///
/// Identity beats name, always. A UserID is derived from the signing key, so a
/// card whose UserID is already on file is the same person re-sharing (new
/// relay details after a Cruise Pass, a fresh card over the air) even when some
/// *other* contact happens to share their display name. Deciding by name first
/// points a key-change warning at the wrong person and teaches a family to tap
/// through the one warning that would ever have mattered.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum FriendCardMatch {
    /// Nobody on file with this identity or this display name.
    New,
    /// This exact identity is already saved; importing refreshes their details.
    AlreadySaved {
        /// What this phone currently shows them as (nickname wins over card name).
        saved_name: String,
        /// A *different* contact also goes by this name — worth saying out loud
        /// so the two are not confused, but not a security warning.
        name_shared_with_other: bool,
    },
    /// A genuinely different identity already uses this display name. This is
    /// the only case where comparing safety words is worth anyone's time.
    NameTaken {
        other_user_id: Vec<u8>,
        other_name: String,
    },
}

/// Classify a pasted/scanned friend card against the contacts already saved,
/// so both shells reach the same verdict from the same rules.
#[uniffi::export]
pub fn friend_card_match(candidate: Contact, existing: Vec<Contact>) -> FriendCardMatch {
    let candidate_name = contact_display_name(&candidate).to_lowercase();
    let same_name = |c: &&Contact| contact_display_name(c).to_lowercase() == candidate_name;

    if let Some(saved) = existing.iter().find(|c| c.user_id == candidate.user_id) {
        return FriendCardMatch::AlreadySaved {
            saved_name: contact_display_name(saved),
            name_shared_with_other: existing
                .iter()
                .filter(|c| c.user_id != candidate.user_id)
                .any(|c| same_name(&c)),
        };
    }

    existing
        .iter()
        .filter(|c| c.user_id != candidate.user_id)
        .find(same_name)
        .map_or(FriendCardMatch::New, |other| FriendCardMatch::NameTaken {
            other_user_id: other.user_id.clone(),
            other_name: contact_display_name(other),
        })
}

/// Build the JSON payload shared via QR code / pasted text when friending.
///
/// CP4 (deposit-token split): a friend card exists so a contact can *post*
/// into this family's mailbox — it never needs fetch/ack/WS capability. The
/// member token passed in (the phone's own saved relay credential) is
/// therefore attenuated to its post-only deposit form before it goes on the
/// card; a publicly re-shared card can then cost the family quota at worst,
/// never read or delete their mail. Attenuation is idempotent, so a token
/// that is already deposit-class passes through unchanged, and the wire
/// layout of the card does not change — old apps parse new cards (and can
/// still post with the deposit token), new apps parse old full-token cards
/// (the relay keeps accepting member tokens for posting).
#[uniffi::export]
pub fn make_friend_card(
    name: String,
    identity: Identity,
    relay_url: Option<String>,
    relay_token: Option<String>,
) -> Result<String, CoreError> {
    let relay_token = relay_token
        .map(crate::relay_deposit_token_for)
        .filter(|token| !token.is_empty());
    let card = FriendCard {
        name,
        sign_pk: identity.sign_pk,
        agree_pk: identity.agree_pk,
        relay_url,
        relay_token,
    };
    validate_friend_card(&card)?;
    let json =
        serde_json::to_string(&card).map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
    if json.len() > MAX_FRIEND_CARD_JSON_BYTES {
        return Err(CoreError::InvalidFriendCard(
            "friend card is too large".to_string(),
        ));
    }
    Ok(json)
}

/// Compact, chat-app-safe text form of a FriendCard (T12). Emits the binary
/// `CMFRIEND2:` form, which is ~half the size of the legacy JSON `CMFRIEND1:`
/// form and so produces a much less dense QR code. `parse_friend_text` still
/// accepts both forms, so cards already shared in the field keep working.
#[uniffi::export]
pub fn make_friend_link(card_json: String) -> Result<String, CoreError> {
    let card = parse_friend_card(card_json)?;
    let binary = encode_friend_card_binary(&card)?;
    Ok(format!(
        "{FRIEND_LINK_PREFIX_V2}{}",
        BASE64URL_NOPAD.encode(&binary)
    ))
}

/// Binary FriendCard layout for the `CMFRIEND2:` link form:
/// `sign_pk[32] ‖ agree_pk[32] ‖ name_len:u8 ‖ name ‖ opt(relay_url) ‖
/// opt(relay_token)`, where `opt` is `0x00` for absent or `0x01 ‖ len:u16_be ‖
/// bytes` for present. Keys are fixed 32 bytes (validated); the display name is
/// capped at 128 bytes so its length always fits in one byte.
fn encode_friend_card_binary(card: &FriendCard) -> Result<Vec<u8>, CoreError> {
    validate_friend_card(card)?;
    let name = card.name.as_bytes();
    // validate_friend_card caps the name at MAX_DISPLAY_NAME_BYTES (128 < 256),
    // so this never truncates; guard anyway rather than silently corrupt.
    if name.len() > u8::MAX as usize {
        return Err(CoreError::InvalidFriendCard(
            "display name too long to encode".to_string(),
        ));
    }
    let mut out = Vec::with_capacity(66 + name.len());
    out.extend_from_slice(&card.sign_pk);
    out.extend_from_slice(&card.agree_pk);
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    encode_opt_field(&mut out, card.relay_url.as_deref());
    encode_opt_field(&mut out, card.relay_token.as_deref());
    Ok(out)
}

fn encode_opt_field(out: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            out.push(1);
            let bytes = value.as_bytes();
            // Relay URL/token are capped well under u16::MAX by
            // validate_friend_card; the cast is safe.
            out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            out.extend_from_slice(bytes);
        }
        None => out.push(0),
    }
}

/// Decode the `CMFRIEND2:` binary layout. Runs on untrusted scan/paste input,
/// so every read is bounds-checked -- a truncated or malformed card returns an
/// error, never panics (adversarial-payload hardening, see T4).
fn decode_friend_card_binary(bytes: &[u8]) -> Result<FriendCard, CoreError> {
    let mut pos = 0usize;
    let sign_pk = read_binary_slice(bytes, &mut pos, 32)?.to_vec();
    let agree_pk = read_binary_slice(bytes, &mut pos, 32)?.to_vec();
    let name_len = read_binary_slice(bytes, &mut pos, 1)?[0] as usize;
    let name_bytes = read_binary_slice(bytes, &mut pos, name_len)?;
    let name = std::str::from_utf8(name_bytes)
        .map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?
        .to_string();
    let relay_url = decode_opt_field(bytes, &mut pos)?;
    let relay_token = decode_opt_field(bytes, &mut pos)?;
    if pos != bytes.len() {
        return Err(CoreError::InvalidFriendCard(
            "trailing bytes after friend card".to_string(),
        ));
    }
    let card = FriendCard {
        name,
        sign_pk,
        agree_pk,
        relay_url,
        relay_token,
    };
    validate_friend_card(&card)?;
    Ok(card)
}

fn decode_opt_field(bytes: &[u8], pos: &mut usize) -> Result<Option<String>, CoreError> {
    let flag = read_binary_slice(bytes, pos, 1)?[0];
    match flag {
        0 => Ok(None),
        1 => {
            let len = u16::from_be_bytes([
                read_binary_slice(bytes, pos, 1)?[0],
                read_binary_slice(bytes, pos, 1)?[0],
            ]) as usize;
            let value = read_binary_slice(bytes, pos, len)?;
            let value = std::str::from_utf8(value)
                .map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
            Ok(Some(value.to_string()))
        }
        other => Err(CoreError::InvalidFriendCard(format!(
            "invalid optional-field flag {other}"
        ))),
    }
}

/// Bounds-checked slice read for the binary decoder: advances `pos` by `n` and
/// returns the slice, or an error if the buffer is too short. Never panics.
fn read_binary_slice<'a>(
    bytes: &'a [u8],
    pos: &mut usize,
    n: usize,
) -> Result<&'a [u8], CoreError> {
    let end = pos
        .checked_add(n)
        .ok_or_else(|| CoreError::InvalidFriendCard("friend card length overflow".to_string()))?;
    let slice = bytes
        .get(*pos..end)
        .ok_or_else(|| CoreError::InvalidFriendCard("truncated friend card".to_string()))?;
    *pos = end;
    Ok(slice)
}

/// Parse a friend-card JSON payload received via QR scan or pasted text.
#[uniffi::export]
pub fn parse_friend_card(json: String) -> Result<FriendCard, CoreError> {
    if json.len() > MAX_FRIEND_CARD_JSON_BYTES {
        return Err(CoreError::InvalidFriendCard(
            "friend card is too large".to_string(),
        ));
    }
    let card: FriendCard =
        serde_json::from_str(&json).map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
    validate_friend_card(&card)?;
    Ok(card)
}

/// Parse a shared friend card in any form: the compact binary `CMFRIEND2:`
/// link (what we emit now), the legacy `CMFRIEND1:` JSON link, either one
/// embedded in a `https://cruisemesh.app/f#…` URL or surrounding prose, or a
/// raw FriendCard JSON blob.
#[uniffi::export]
pub fn parse_friend_text(text: String) -> Result<FriendCard, CoreError> {
    if text.len() > MAX_FRIEND_TEXT_BYTES {
        return Err(CoreError::InvalidFriendCard(
            "shared friend text is too large".to_string(),
        ));
    }
    let trimmed = text.trim();

    // Prefer the compact v2 form; fall back to legacy v1. Both may appear bare,
    // wrapped in a URL fragment, or inside prose ("Add me on CruiseMesh: …").
    if let Some(encoded) = extract_link_body(trimmed, FRIEND_LINK_PREFIX_V2) {
        let compact: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
        let binary = BASE64URL_NOPAD
            .decode(compact.as_bytes())
            .map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
        return decode_friend_card_binary(&binary);
    }
    if let Some(encoded) = extract_link_body(trimmed, FRIEND_LINK_PREFIX) {
        let compact: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
        let json = BASE64URL_NOPAD
            .decode(compact.as_bytes())
            .map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
        let json =
            String::from_utf8(json).map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
        return parse_friend_card(json);
    }

    parse_friend_card(trimmed.to_string())
        .map_err(|_| CoreError::InvalidFriendCard("not a CruiseMesh friend card".to_string()))
}

/// If `text` contains `prefix`, return the link body that follows it, else
/// `None`. When `text` *starts* with the prefix the whole remainder is returned
/// (the caller filters internal whitespace, so a link split across lines still
/// parses); when the prefix is embedded in a URL or prose, the body is cut at
/// the first character that can't be part of a base64url token.
fn extract_link_body<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if let Some(rest) = text.strip_prefix(prefix) {
        return Some(rest);
    }
    let start = text.find(prefix)?;
    let tail = &text[start + prefix.len()..];
    let end = tail
        .char_indices()
        .find(|(_, ch)| !ch.is_ascii_alphanumeric() && *ch != '_' && *ch != '-')
        .map_or(tail.len(), |(index, _)| index);
    Some(&tail[..end])
}

fn validate_friend_card(card: &FriendCard) -> Result<(), CoreError> {
    if card.name.len() > MAX_DISPLAY_NAME_BYTES {
        return Err(CoreError::InvalidFriendCard(format!(
            "display name exceeds {MAX_DISPLAY_NAME_BYTES} UTF-8 bytes"
        )));
    }
    if card
        .relay_url
        .as_ref()
        .is_some_and(|value| value.len() > MAX_RELAY_URL_BYTES)
    {
        return Err(CoreError::InvalidFriendCard(
            "relay URL is too long".to_string(),
        ));
    }
    if card
        .relay_token
        .as_ref()
        .is_some_and(|value| value.len() > MAX_RELAY_TOKEN_BYTES)
    {
        return Err(CoreError::InvalidFriendCard(
            "relay token is too long".to_string(),
        ));
    }
    if card.sign_pk.len() != 32 {
        return Err(CoreError::InvalidKeyLength {
            expected: 32,
            actual: card.sign_pk.len() as u32,
        });
    }
    if card.agree_pk.len() != 32 {
        return Err(CoreError::InvalidKeyLength {
            expected: 32,
            actual: card.agree_pk.len() as u32,
        });
    }
    Ok(())
}

/// Small nautical/travel-themed wordlist for fingerprint phrases. Not
/// security-critical (only 4 words are shown), so a compact list is fine.
const WORDLIST: [&str; 64] = [
    "anchor",
    "atoll",
    "beacon",
    "bilge",
    "boatswain",
    "bosun",
    "bow",
    "breeze",
    "bridge",
    "buoy",
    "cabin",
    "captain",
    "chart",
    "clipper",
    "coast",
    "compass",
    "coral",
    "current",
    "dock",
    "dolphin",
    "ferry",
    "fjord",
    "flag",
    "fleet",
    "galley",
    "gangway",
    "harbor",
    "helm",
    "horizon",
    "island",
    "jetty",
    "keel",
    "knot",
    "lagoon",
    "lantern",
    "latitude",
    "lighthouse",
    "longitude",
    "mast",
    "mate",
    "moor",
    "navigate",
    "ocean",
    "oar",
    "pier",
    "port",
    "quay",
    "reef",
    "rudder",
    "sail",
    "sextant",
    "shore",
    "starboard",
    "stern",
    "swell",
    "tide",
    "tropic",
    "vessel",
    "voyage",
    "wake",
    "wave",
    "wharf",
    "wind",
    "yacht",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_id_is_stable_for_same_key() {
        let id = generate_identity();
        let a = derive_user_id(&id.sign_pk);
        let b = derive_user_id(&id.sign_pk);
        assert_eq!(a, b);
        assert_eq!(id.user_id, a.to_vec());
    }

    #[test]
    fn friend_card_round_trips() {
        let id = generate_identity();
        let json = make_friend_card(
            "Dave".to_string(),
            id.clone(),
            Some("https://relay.example".to_string()),
            Some("family-token".to_string()),
        )
        .unwrap();
        let card = parse_friend_card(json).expect("valid card");
        assert_eq!(card.name, "Dave");
        assert_eq!(card.sign_pk, id.sign_pk);
        assert_eq!(card.agree_pk, id.agree_pk);
        assert_eq!(card.relay_url, Some("https://relay.example".to_string()));
        // CP4: the shared card carries the post-only deposit form, never the
        // full member token that was passed in.
        assert_eq!(
            card.relay_token,
            Some(crate::relay_deposit_token_for("family-token".to_string()))
        );
        assert_eq!(friend_card_user_id(card), id.user_id);
    }

    #[test]
    fn make_friend_card_attenuates_member_token_to_deposit_class() {
        let id = generate_identity();
        let deposit = crate::relay_deposit_token_for("family-token".to_string());

        // Member in → deposit out; the member token never reaches the card.
        let json = make_friend_card(
            "Dave".to_string(),
            id.clone(),
            Some("https://relay.example".to_string()),
            Some("family-token".to_string()),
        )
        .unwrap();
        assert!(!json.contains("family-token"));
        let card = parse_friend_card(json).unwrap();
        assert_eq!(card.relay_token, Some(deposit.clone()));
        assert!(crate::relay_token_is_deposit(deposit.clone()));

        // Deposit in → unchanged (re-encoding a card never double-attenuates).
        let json = make_friend_card(
            "Dave".to_string(),
            id.clone(),
            Some("https://relay.example".to_string()),
            Some(deposit.clone()),
        )
        .unwrap();
        assert_eq!(parse_friend_card(json).unwrap().relay_token, Some(deposit));

        // Blank token in → omitted, not attenuated into garbage.
        let json = make_friend_card(
            "Dave".to_string(),
            id,
            Some("https://relay.example".to_string()),
            Some("  ".to_string()),
        )
        .unwrap();
        assert_eq!(parse_friend_card(json).unwrap().relay_token, None);
    }

    /// Fixed-key golden vectors for the CMFRIEND2 wire form. The layout is
    /// deliberately UNCHANGED by CP4 (an appended field would hard-fail the
    /// trailing-bytes check in every pre-CP4 decoder in the field); the rev
    /// is what the `relay_token` slot carries. Old-format cards (full member
    /// token) must keep parsing forever; new cards pin the deposit form.
    #[test]
    fn cmfriend2_golden_vectors_old_and_new_format() {
        const OLD_FORMAT: &str = "CMFRIEND2:EREREREREREREREREREREREREREREREREREREREREREiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIgREYXZlAQAVaHR0cHM6Ly9yZWxheS5leGFtcGxlAQAMZmFtaWx5LXRva2Vu";
        const NEW_FORMAT: &str = "CMFRIEND2:EREREREREREREREREREREREREREREREREREREREREREiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIiIgREYXZlAQAVaHR0cHM6Ly9yZWxheS5leGFtcGxlAQAyY21kZXAxLTYzaFd2eDFrSExLaXJmbDlHVjU3NmVBaV9yVVJweVppeHBzQ1ZVQ1hOSms";

        // Old-format card (minted by a pre-CP4 app, full member token):
        // still parses, member token intact — the relay keeps accepting it
        // for posting.
        let card = parse_friend_text(OLD_FORMAT.to_string()).expect("old-format card must parse");
        assert_eq!(card.name, "Dave");
        assert_eq!(card.sign_pk, vec![0x11; 32]);
        assert_eq!(card.agree_pk, vec![0x22; 32]);
        assert_eq!(card.relay_url, Some("https://relay.example".to_string()));
        assert_eq!(card.relay_token, Some("family-token".to_string()));

        // New-format card: byte-identical layout, deposit token in the same
        // slot. Pinned end-to-end from make_friend_card + make_friend_link
        // so any accidental format or derivation change fails here.
        let identity = Identity {
            user_id: derive_user_id(&[0x11; 32]).to_vec(),
            sign_pk: vec![0x11; 32],
            sign_sk: vec![0; 32],
            agree_pk: vec![0x22; 32],
            agree_sk: vec![0; 32],
        };
        let json = make_friend_card(
            "Dave".to_string(),
            identity,
            Some("https://relay.example".to_string()),
            Some("family-token".to_string()),
        )
        .unwrap();
        assert_eq!(make_friend_link(json).unwrap(), NEW_FORMAT);

        let card = parse_friend_text(NEW_FORMAT.to_string()).expect("new-format card must parse");
        assert_eq!(
            card.relay_token,
            Some(crate::relay_deposit_token_for("family-token".to_string()))
        );
    }

    #[test]
    fn friend_link_round_trips() {
        let id = generate_identity();
        let json = make_friend_card("Dave".to_string(), id.clone(), None, None).unwrap();
        let link = make_friend_link(json).unwrap();
        assert!(link.starts_with(FRIEND_LINK_PREFIX_V2));
        let card = parse_friend_text(link).expect("valid link");
        assert_eq!(friend_card_user_id(card), id.user_id);
    }

    #[test]
    fn parse_friend_text_accepts_raw_json_and_whitespace() {
        let id = generate_identity();
        let json = make_friend_card("Dave".to_string(), id.clone(), None, None).unwrap();
        let card = parse_friend_text(format!("\n  {json} \t")).expect("valid raw json");
        assert_eq!(friend_card_user_id(card), id.user_id);
    }

    #[test]
    fn parse_friend_text_strips_wrapped_link_body() {
        let id = generate_identity();
        let json = make_friend_card(
            "Dave".to_string(),
            id.clone(),
            Some("https://relay.example".to_string()),
            Some("token".to_string()),
        )
        .unwrap();
        let link = make_friend_link(json).unwrap();
        let wrapped = format!("  {}\n{}\t  ", &link[..24], &link[24..]);
        let card = parse_friend_text(wrapped).expect("valid wrapped link");
        assert_eq!(friend_card_user_id(card.clone()), id.user_id);
        assert_eq!(card.relay_url, Some("https://relay.example".to_string()));
        assert_eq!(
            card.relay_token,
            Some(crate::relay_deposit_token_for("token".to_string()))
        );
    }

    #[test]
    fn parse_friend_text_extracts_link_from_shared_prose() {
        let identity = generate_identity();
        let json = make_friend_card("Alice".to_string(), identity, None, None).unwrap();
        let link = make_friend_link(json).unwrap();
        let card = parse_friend_text(format!("Add me on CruiseMesh: {link}. Thanks!"))
            .expect("embedded link");
        assert_eq!(card.name, "Alice");
    }

    #[test]
    fn parse_friend_text_rejects_bad_link() {
        let err = parse_friend_text("CMFRIEND1:not valid base64".to_string()).unwrap_err();
        assert!(matches!(err, CoreError::InvalidFriendCard(_)));
    }

    #[test]
    fn parse_friend_text_rejects_unknown_prefix() {
        let err = parse_friend_text("CMFRIEND7:abc".to_string()).unwrap_err();
        assert!(matches!(err, CoreError::InvalidFriendCard(_)));
    }

    #[test]
    fn friend_link_emits_compact_v2_and_round_trips_with_relay() {
        let id = generate_identity();
        let json = make_friend_card(
            "Dave".to_string(),
            id.clone(),
            Some("https://relay.example".to_string()),
            Some("family-token".to_string()),
        )
        .unwrap();
        let link = make_friend_link(json.clone()).unwrap();
        assert!(link.starts_with(FRIEND_LINK_PREFIX_V2));

        let card = parse_friend_text(link.clone()).expect("valid v2 link");
        assert_eq!(card.name, "Dave");
        assert_eq!(card.sign_pk, id.sign_pk);
        assert_eq!(card.agree_pk, id.agree_pk);
        assert_eq!(card.relay_url, Some("https://relay.example".to_string()));
        assert_eq!(
            card.relay_token,
            Some(crate::relay_deposit_token_for("family-token".to_string()))
        );
        assert_eq!(friend_card_user_id(card), id.user_id);

        // The whole point of T12: the v2 link is much smaller than the v1 form
        // it replaces, so the QR is far less dense.
        let v1 = format!(
            "{FRIEND_LINK_PREFIX}{}",
            BASE64URL_NOPAD.encode(json.as_bytes())
        );
        assert!(
            link.len() * 2 < v1.len(),
            "v2 link {} bytes should be < half of v1 {} bytes",
            link.len(),
            v1.len()
        );
    }

    #[test]
    fn parse_friend_text_accepts_v2_inside_app_url() {
        let id = generate_identity();
        let json = make_friend_card("Alice".to_string(), id.clone(), None, None).unwrap();
        let link = make_friend_link(json).unwrap();
        let url = format!("https://cruisemesh.app/f#{link}");
        let card = parse_friend_text(url).expect("valid wrapped v2 link");
        assert_eq!(card.name, "Alice");
        assert_eq!(friend_card_user_id(card), id.user_id);
    }

    #[test]
    fn parse_friend_text_still_accepts_legacy_v1_links() {
        let id = generate_identity();
        let json = make_friend_card(
            "Bob".to_string(),
            id.clone(),
            Some("https://relay.example".to_string()),
            None,
        )
        .unwrap();
        // A card shared before T12: the raw v1 (JSON-in-base64) form.
        let v1 = format!(
            "{FRIEND_LINK_PREFIX}{}",
            BASE64URL_NOPAD.encode(json.as_bytes())
        );
        let card = parse_friend_text(v1).expect("legacy v1 link still parses");
        assert_eq!(card.name, "Bob");
        assert_eq!(card.relay_url, Some("https://relay.example".to_string()));
        assert_eq!(card.relay_token, None);
        assert_eq!(friend_card_user_id(card), id.user_id);
    }

    #[test]
    fn decode_v2_rejects_truncated_binary_without_panicking() {
        let id = generate_identity();
        let json = make_friend_card("Dave".to_string(), id, None, None).unwrap();
        let card = parse_friend_card(json).unwrap();
        let binary = encode_friend_card_binary(&card).unwrap();
        // Every truncation of a valid card must be a clean error, never a panic.
        for cut in 0..binary.len() {
            assert!(decode_friend_card_binary(&binary[..cut]).is_err());
        }
        // Trailing garbage is rejected too.
        let mut extra = binary.clone();
        extra.push(0);
        assert!(decode_friend_card_binary(&extra).is_err());
    }

    #[test]
    fn friend_cards_reject_oversized_strings_before_sharing_or_import() {
        let identity = generate_identity();
        assert!(make_friend_card(
            "x".repeat(MAX_DISPLAY_NAME_BYTES + 1),
            identity.clone(),
            None,
            None,
        )
        .is_err());
        assert!(make_friend_card(
            "Alice".into(),
            identity.clone(),
            Some("x".repeat(MAX_RELAY_URL_BYTES + 1)),
            None,
        )
        .is_err());
        assert!(parse_friend_card("x".repeat(MAX_FRIEND_CARD_JSON_BYTES + 1)).is_err());
        assert!(parse_friend_text("x".repeat(MAX_FRIEND_TEXT_BYTES + 1)).is_err());

        let json = make_friend_card("Alice".into(), identity, None, None).unwrap();
        let mut card: FriendCard = serde_json::from_str(&json).unwrap();
        card.name = "x".repeat(MAX_DISPLAY_NAME_BYTES + 1);
        assert!(parse_friend_card(serde_json::to_string(&card).unwrap()).is_err());
    }

    #[test]
    fn rejects_malformed_card() {
        let err = parse_friend_card("not json".to_string()).unwrap_err();
        matches!(err, CoreError::InvalidFriendCard(_));
    }

    #[test]
    fn format_user_id_has_prefix_and_groups() {
        let id = generate_identity();
        let formatted = format_user_id(id.user_id);
        assert!(formatted.starts_with("CM-"));
        assert!(formatted.contains('-'));
    }

    #[test]
    fn fingerprint_words_are_deterministic() {
        let id = generate_identity();
        let a = fingerprint_words(id.user_id.clone());
        let b = fingerprint_words(id.user_id);
        assert_eq!(a, b);
        assert_eq!(a.len(), 4);
    }

    fn contact(user_id: &[u8], name: &str) -> Contact {
        Contact {
            user_id: user_id.to_vec(),
            name: name.to_string(),
            sign_pk: vec![1; 32],
            agree_pk: vec![2; 32],
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    #[test]
    fn friend_card_match_reports_a_brand_new_contact() {
        let existing = vec![contact(b"dad", "iPhone")];
        assert_eq!(
            friend_card_match(contact(b"joan", "Joan"), existing),
            FriendCardMatch::New
        );
    }

    #[test]
    fn friend_card_match_reports_a_name_collision_with_the_other_contact() {
        let existing = vec![contact(b"dad", "iphone"), contact(b"lynn", "Lynn")];
        assert_eq!(
            friend_card_match(contact(b"joan", "iPhone"), existing),
            FriendCardMatch::NameTaken {
                other_user_id: b"dad".to_vec(),
                other_name: "iphone".to_string(),
            }
        );
    }

    /// The bug this exists to stop: Aunt Joan (already saved as "iPhone")
    /// re-sends her card after setting up a Cruise Pass, and Dad — a different
    /// person who also shows up as "iPhone" — makes it look like her keys
    /// changed. Her UserID is on file, so this is a refresh, not a stranger.
    #[test]
    fn a_resent_card_from_a_saved_contact_is_not_a_key_change() {
        let existing = vec![contact(b"dad", "iPhone"), contact(b"joan", "iPhone")];
        let mut resent = contact(b"joan", "iPhone");
        resent.relay_url = Some("https://relay.example".to_string());
        assert_eq!(
            friend_card_match(resent, existing),
            FriendCardMatch::AlreadySaved {
                saved_name: "iPhone".to_string(),
                name_shared_with_other: true,
            }
        );
    }

    #[test]
    fn a_resent_card_with_a_unique_name_does_not_claim_a_shared_name() {
        let existing = vec![contact(b"dad", "iPhone"), contact(b"joan", "Joan")];
        assert_eq!(
            friend_card_match(contact(b"joan", "Joan"), existing),
            FriendCardMatch::AlreadySaved {
                saved_name: "Joan".to_string(),
                name_shared_with_other: false,
            }
        );
    }

    #[test]
    fn friend_card_match_compares_the_nickname_the_user_actually_sees() {
        // Dad's card says "iPhone" but this phone shows him as "Dad", so an
        // incoming "iPhone" collides with nobody the user would recognise.
        let mut dad = contact(b"dad", "iPhone");
        dad.nickname = Some("Dad".to_string());
        assert_eq!(
            friend_card_match(contact(b"joan", "iPhone"), vec![dad.clone()]),
            FriendCardMatch::New
        );
        // ...and a card claiming the nickname does collide.
        assert_eq!(
            friend_card_match(contact(b"joan", "dad"), vec![dad]),
            FriendCardMatch::NameTaken {
                other_user_id: b"dad".to_vec(),
                other_name: "Dad".to_string(),
            }
        );
    }
}
