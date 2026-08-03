//! Identity, keypairs, UserID derivation, and friend-card encoding.
//!
//! Scheme (DESIGN.md §6.2): each identity is an Ed25519 signing keypair plus an
//! X25519 agreement keypair, generated on-device. UserID = first 16 bytes of
//! BLAKE2b(Ed25519 public key). Friending exchanges a FriendCard (JSON, carried
//! over QR code or pasted text) containing both public keys.

use crate::crypto::{signing_key_from_bytes, verifying_key_from_bytes};
use crate::protocol::MS_PER_DAY;
use crate::store::{contact_display_name, Contact};
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use data_encoding::{BASE32_NOPAD, BASE64URL_NOPAD};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
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
/// Shared-contact code (specs/share-contact.md): base64url of a binary
/// [`SharedFriendCard`]. Displayed as a QR only, never emitted as copyable
/// text by any UI surface (decision 2) — the prefix exists so a screenshotted
/// or hand-typed code still parses.
const SHARED_CARD_PREFIX: &str = "CMSHARE1:";
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
#[derive(uniffi::Record, Clone, Debug, PartialEq, Serialize, Deserialize)]
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

/// How long a shared-contact code stays scannable (decision 6): long enough
/// for "we'll add each other tomorrow", short enough that a screenshot in an
/// old chat log goes stale.
pub(crate) const SHARED_CARD_LIFETIME_MS: i64 = 7 * MS_PER_DAY;
/// Device clocks drift; tolerate a day either side when the *shared person*
/// checks expiry, mirroring `INTRODUCTION_CLOCK_SKEW_MS`.
const SHARED_CARD_CLOCK_SKEW_MS: i64 = 24 * 60 * 60 * 1000;
const SHARED_CARD_VERSION: u8 = 1;
const SHARED_CARD_SIGN_DOMAIN: &[u8] = b"CruiseMesh shared contact v1\0";

/// One contact's friend card, deliberately handed to somebody else by a mutual
/// acquaintance (specs/share-contact.md). Carries the stored card
/// byte-identical (decision 8: never substitute the sharer's own credentials),
/// plus who shared it, a validity window, and the sharer's Ed25519 signature
/// over all of it. The signature is what lets the shared person's phone verify
/// the request came from a card one of their own accepted contacts actually
/// issued, rather than from anyone who once saw their link.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct SharedFriendCard {
    pub version: u8,
    pub card: FriendCard,
    pub sharer_user_id: Vec<u8>,
    /// The shared person's discovery-policy revision at issue time. Checked
    /// for equality on their phone (decision 10) so an off-then-on cycle
    /// kills every card issued before it.
    pub shared_policy_revision: u64,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub signature: Vec<u8>,
}

/// What a scanned/pasted friend text turned out to be. Shells route
/// `Direct` through the existing confirmation flow and `Shared` through the
/// shared-card flow (expiry message, "Shared by …" line, tailed request).
#[derive(uniffi::Enum, Clone, Debug)]
pub enum FriendImport {
    Direct { card: FriendCard },
    Shared { shared: SharedFriendCard },
}

/// A decoded `kind=3` friend-request payload: the requester's own card, plus
/// the shared card they imported from, when the request originated from one.
/// The tail rides as an extra JSON field old clients ignore, so a tailless
/// request keeps meaning "this person physically scanned your code".
#[derive(uniffi::Record, Clone, Debug)]
pub struct FriendRequestContent {
    pub card: FriendCard,
    pub shared: Option<SharedFriendCard>,
}

/// Issue a shared card for `card` (a contact's stored friend card), signed by
/// the sharer. Expiry is fixed at seven days from `now_ms`.
#[uniffi::export]
pub fn create_shared_friend_card(
    sharer: Identity,
    card: FriendCard,
    shared_policy_revision: u64,
    now_ms: i64,
) -> Result<SharedFriendCard, CoreError> {
    validate_friend_card(&card)?;
    if sharer.user_id.len() != USER_ID_LEN {
        return Err(CoreError::Malformed("invalid sharer UserID".to_string()));
    }
    let mut shared = SharedFriendCard {
        version: SHARED_CARD_VERSION,
        card,
        sharer_user_id: sharer.user_id.clone(),
        shared_policy_revision,
        issued_at_ms: now_ms,
        expires_at_ms: now_ms.saturating_add(SHARED_CARD_LIFETIME_MS),
        signature: Vec::new(),
    };
    let signing_key = signing_key_from_bytes(&sharer.sign_sk)?;
    shared.signature = signing_key
        .sign(&shared_card_signed_bytes(&shared)?)
        .to_bytes()
        .to_vec();
    Ok(shared)
}

/// The scannable text form of a shared card, for QR rendering only.
#[uniffi::export]
pub fn make_shared_contact_code(shared: SharedFriendCard) -> Result<String, CoreError> {
    let binary = encode_shared_friend_card(&shared)?;
    Ok(format!(
        "{SHARED_CARD_PREFIX}{}",
        BASE64URL_NOPAD.encode(&binary)
    ))
}

/// Parse anything a scan or paste can produce: a shared-contact code, or any
/// of the direct friend-card forms `parse_friend_text` accepts.
#[uniffi::export]
pub fn parse_friend_import(text: String) -> Result<FriendImport, CoreError> {
    if text.len() > MAX_FRIEND_TEXT_BYTES {
        return Err(CoreError::InvalidFriendCard(
            "shared friend text is too large".to_string(),
        ));
    }
    let trimmed = text.trim();
    if let Some(encoded) = extract_link_body(trimmed, SHARED_CARD_PREFIX) {
        let compact: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
        let binary = BASE64URL_NOPAD
            .decode(compact.as_bytes())
            .map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
        let shared = decode_shared_friend_card(&binary)?;
        return Ok(FriendImport::Shared { shared });
    }
    parse_friend_text(text).map(|card| FriendImport::Direct { card })
}

/// Scanner-side expiry check, for the specific "This code has expired. Ask
/// for a new one." message rather than a generic parse failure. The shared
/// person's own verification applies clock skew; the scanner does not need to.
#[uniffi::export]
pub fn shared_card_expired(shared: SharedFriendCard, now_ms: i64) -> bool {
    now_ms > shared.expires_at_ms
}

/// Every check the *shared person's* phone can run from the card alone:
/// the card really is mine, the named sharer signed exactly this card and
/// window, it is unexpired within a day of clock skew, and it was issued
/// under my current discovery-policy revision. The caller supplies the
/// relationship checks (sharer is an accepted, non-blocked contact; my
/// discovery switch is on) because they live in the store, not the card.
/// Any `false` here means: drop the request without a prompt.
#[uniffi::export]
pub fn verify_shared_friend_card(
    shared: SharedFriendCard,
    sharer_sign_pk: Vec<u8>,
    expected_card_user_id: Vec<u8>,
    expected_policy_revision: u64,
    now_ms: i64,
) -> Result<bool, CoreError> {
    if validate_shared_card_shape(&shared).is_err() {
        return Ok(false);
    }
    if derive_user_id(&shared.card.sign_pk).to_vec() != expected_card_user_id
        || derive_user_id(&sharer_sign_pk).to_vec() != shared.sharer_user_id
        || shared.shared_policy_revision != expected_policy_revision
        || now_ms
            < shared
                .issued_at_ms
                .saturating_sub(SHARED_CARD_CLOCK_SKEW_MS)
        || now_ms
            > shared
                .expires_at_ms
                .saturating_add(SHARED_CARD_CLOCK_SKEW_MS)
    {
        return Ok(false);
    }
    let verifying_key = verifying_key_from_bytes(&sharer_sign_pk)?;
    let signature_bytes: [u8; 64] =
        shared.signature.as_slice().try_into().map_err(|_| {
            CoreError::Malformed("invalid shared card signature length".to_string())
        })?;
    let signature = Signature::from_bytes(&signature_bytes);
    Ok(verifying_key
        .verify(&shared_card_signed_bytes(&shared)?, &signature)
        .is_ok())
}

/// Build the `kind=3` payload for a request that originated from a shared
/// card: the requester's ordinary card JSON with the shared card appended as
/// an extra `shared` field. Old clients deserialize the same JSON into a
/// plain FriendCard, ignore the unknown field, and auto-import exactly as
/// today — the confirmation step is a property of updated recipients.
#[uniffi::export]
pub fn make_shared_friend_request_payload(
    card_json: String,
    shared: SharedFriendCard,
) -> Result<String, CoreError> {
    parse_friend_card(card_json.clone())?;
    let mut value: serde_json::Value = serde_json::from_str(&card_json)
        .map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| CoreError::InvalidFriendCard("friend card is not an object".to_string()))?;
    let binary = encode_shared_friend_card(&shared)?;
    object.insert(
        "shared".to_string(),
        serde_json::Value::String(BASE64URL_NOPAD.encode(&binary)),
    );
    let json =
        serde_json::to_string(&value).map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
    if json.len() > MAX_FRIEND_CARD_JSON_BYTES {
        return Err(CoreError::InvalidFriendCard(
            "friend request payload is too large".to_string(),
        ));
    }
    Ok(json)
}

/// Decode an inbound `kind=3` payload: the requester's card plus the shared
/// tail when present. A malformed tail is an error, not a silent downgrade to
/// the auto-import path — dropping a bad request outright is the fail-closed
/// direction here.
#[uniffi::export]
pub fn parse_friend_request_content(json: String) -> Result<FriendRequestContent, CoreError> {
    let card = parse_friend_card(json.clone())?;
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
    let shared = match value.get("shared") {
        None => None,
        Some(serde_json::Value::String(encoded)) => {
            let binary = BASE64URL_NOPAD
                .decode(encoded.as_bytes())
                .map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
            Some(decode_shared_friend_card(&binary)?)
        }
        Some(_) => {
            return Err(CoreError::InvalidFriendCard(
                "invalid shared tail".to_string(),
            ))
        }
    };
    Ok(FriendRequestContent { card, shared })
}

/// Binary SharedFriendCard layout: `version:u8 ‖ card_len:u16_be ‖
/// card_binary ‖ sharer_user_id[16] ‖ policy_revision:u64_be ‖
/// issued_at:i64_be ‖ expires_at:i64_be ‖ signature[64]`, where `card_binary`
/// is the existing CMFRIEND2 layout.
fn encode_shared_friend_card(shared: &SharedFriendCard) -> Result<Vec<u8>, CoreError> {
    validate_shared_card_shape(shared)?;
    let mut out = shared_card_body_bytes(shared)?;
    out.extend_from_slice(&shared.signature);
    Ok(out)
}

/// Everything except the signature, in wire order. The signed bytes are the
/// domain separator followed by exactly this, so encode and sign can never
/// drift apart.
fn shared_card_body_bytes(shared: &SharedFriendCard) -> Result<Vec<u8>, CoreError> {
    if shared.sharer_user_id.len() != USER_ID_LEN {
        return Err(CoreError::Malformed("invalid sharer UserID".to_string()));
    }
    let card_binary = encode_friend_card_binary(&shared.card)?;
    if card_binary.len() > u16::MAX as usize {
        return Err(CoreError::InvalidFriendCard(
            "shared card is too large".to_string(),
        ));
    }
    let mut out = Vec::with_capacity(3 + card_binary.len() + 16 + 24 + 64);
    out.push(shared.version);
    out.extend_from_slice(&(card_binary.len() as u16).to_be_bytes());
    out.extend_from_slice(&card_binary);
    out.extend_from_slice(&shared.sharer_user_id);
    out.extend_from_slice(&shared.shared_policy_revision.to_be_bytes());
    out.extend_from_slice(&shared.issued_at_ms.to_be_bytes());
    out.extend_from_slice(&shared.expires_at_ms.to_be_bytes());
    Ok(out)
}

fn shared_card_signed_bytes(shared: &SharedFriendCard) -> Result<Vec<u8>, CoreError> {
    let mut out = SHARED_CARD_SIGN_DOMAIN.to_vec();
    out.extend_from_slice(&shared_card_body_bytes(shared)?);
    Ok(out)
}

/// Decode runs on untrusted scan input: every read is bounds-checked and a
/// truncated, trailing-byte, or unknown-version card errors, never panics.
fn decode_shared_friend_card(bytes: &[u8]) -> Result<SharedFriendCard, CoreError> {
    let mut pos = 0usize;
    let version = read_binary_slice(bytes, &mut pos, 1)?[0];
    if version != SHARED_CARD_VERSION {
        return Err(CoreError::InvalidFriendCard(format!(
            "unknown shared card version: {version}"
        )));
    }
    let card_len = u16::from_be_bytes([
        read_binary_slice(bytes, &mut pos, 1)?[0],
        read_binary_slice(bytes, &mut pos, 1)?[0],
    ]) as usize;
    let card = decode_friend_card_binary(read_binary_slice(bytes, &mut pos, card_len)?)?;
    let sharer_user_id = read_binary_slice(bytes, &mut pos, USER_ID_LEN)?.to_vec();
    let shared_policy_revision = u64::from_be_bytes(
        read_binary_slice(bytes, &mut pos, 8)?
            .try_into()
            .expect("read_binary_slice returns exactly 8 bytes"),
    );
    let issued_at_ms = i64::from_be_bytes(
        read_binary_slice(bytes, &mut pos, 8)?
            .try_into()
            .expect("read_binary_slice returns exactly 8 bytes"),
    );
    let expires_at_ms = i64::from_be_bytes(
        read_binary_slice(bytes, &mut pos, 8)?
            .try_into()
            .expect("read_binary_slice returns exactly 8 bytes"),
    );
    let signature = read_binary_slice(bytes, &mut pos, 64)?.to_vec();
    if pos != bytes.len() {
        return Err(CoreError::InvalidFriendCard(
            "trailing bytes after shared card".to_string(),
        ));
    }
    let shared = SharedFriendCard {
        version,
        card,
        sharer_user_id,
        shared_policy_revision,
        issued_at_ms,
        expires_at_ms,
        signature,
    };
    validate_shared_card_shape(&shared)?;
    Ok(shared)
}

fn validate_shared_card_shape(shared: &SharedFriendCard) -> Result<(), CoreError> {
    if shared.version != SHARED_CARD_VERSION {
        return Err(CoreError::InvalidFriendCard(format!(
            "unknown shared card version: {}",
            shared.version
        )));
    }
    validate_friend_card(&shared.card)?;
    if shared.sharer_user_id.len() != USER_ID_LEN {
        return Err(CoreError::Malformed("invalid sharer UserID".to_string()));
    }
    if shared.signature.len() != 64 {
        return Err(CoreError::Malformed(
            "invalid shared card signature length".to_string(),
        ));
    }
    if shared.expires_at_ms <= shared.issued_at_ms
        || shared.expires_at_ms.saturating_sub(shared.issued_at_ms) > SHARED_CARD_LIFETIME_MS
    {
        return Err(CoreError::Malformed(
            "invalid shared card validity window".to_string(),
        ));
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

    /// A stored contact card plus the identities on both ends of a share:
    /// `sharer` holds `card` (the shared person's card) and hands it on.
    fn shared_card_fixture() -> (Identity, Identity, FriendCard, SharedFriendCard) {
        let sharer = generate_identity();
        let shared_person = generate_identity();
        let card = FriendCard {
            name: "Avery".to_string(),
            sign_pk: shared_person.sign_pk.clone(),
            agree_pk: shared_person.agree_pk.clone(),
            relay_url: Some("https://relay.example".to_string()),
            relay_token: Some("deposit-token".to_string()),
        };
        let shared = create_shared_friend_card(sharer.clone(), card.clone(), 7, 1_000_000).unwrap();
        (sharer, shared_person, card, shared)
    }

    #[test]
    fn shared_card_round_trips_through_code() {
        let (_, _, card, shared) = shared_card_fixture();
        let code = make_shared_contact_code(shared.clone()).unwrap();
        assert!(code.starts_with("CMSHARE1:"));
        match parse_friend_import(code).unwrap() {
            FriendImport::Shared { shared: decoded } => {
                assert_eq!(decoded, shared);
                assert_eq!(decoded.card, card);
            }
            FriendImport::Direct { .. } => panic!("expected a shared card"),
        }
    }

    #[test]
    fn parse_friend_import_still_routes_direct_forms() {
        let id = generate_identity();
        let json = make_friend_card("Dave".to_string(), id, None, None).unwrap();
        let link = make_friend_link(json.clone()).unwrap();
        for text in [json, link] {
            match parse_friend_import(text).unwrap() {
                FriendImport::Direct { card } => assert_eq!(card.name, "Dave"),
                FriendImport::Shared { .. } => panic!("expected a direct card"),
            }
        }
    }

    #[test]
    fn old_parser_rejects_shared_codes() {
        // Old clients show "that doesn't look like a friend code" — the
        // shared form must never half-parse into a direct card there.
        let (_, _, _, shared) = shared_card_fixture();
        let code = make_shared_contact_code(shared).unwrap();
        assert!(parse_friend_text(code).is_err());
    }

    #[test]
    fn shared_card_decode_rejects_every_truncation_and_trailing_byte() {
        let (_, _, _, shared) = shared_card_fixture();
        let binary = encode_shared_friend_card(&shared).unwrap();
        for len in 0..binary.len() {
            assert!(
                decode_shared_friend_card(&binary[..len]).is_err(),
                "truncation to {len} bytes must fail"
            );
        }
        let mut trailing = binary.clone();
        trailing.push(0);
        assert!(decode_shared_friend_card(&trailing).is_err());
    }

    #[test]
    fn shared_card_decode_rejects_unknown_version() {
        let (_, _, _, shared) = shared_card_fixture();
        let mut binary = encode_shared_friend_card(&shared).unwrap();
        binary[0] = 2;
        assert!(decode_shared_friend_card(&binary).is_err());
    }

    #[test]
    fn shared_card_verifies_and_fails_closed_on_every_tamper() {
        let (sharer, shared_person, _, shared) = shared_card_fixture();
        let own_id = shared_person.user_id.clone();
        let now = shared.issued_at_ms + 1;
        let verify = |card: SharedFriendCard| {
            verify_shared_friend_card(card, sharer.sign_pk.clone(), own_id.clone(), 7, now).unwrap()
        };

        assert!(verify(shared.clone()));

        // Forged signature.
        let mut forged = shared.clone();
        forged.signature[0] ^= 1;
        assert!(!verify(forged));

        // Swapped inner card: signed for somebody else entirely.
        let other = generate_identity();
        let mut swapped = shared.clone();
        swapped.card.sign_pk = other.sign_pk.clone();
        swapped.card.agree_pk = other.agree_pk.clone();
        assert!(!verify(swapped));

        // Changed sharer id.
        let mut resharered = shared.clone();
        resharered.sharer_user_id = other.user_id.clone();
        assert!(!verify(resharered));

        // Changed expiry (signature no longer covers it).
        let mut extended = shared.clone();
        extended.expires_at_ms += 1000;
        assert!(!verify(extended));

        // Wrong policy revision.
        assert!(!verify_shared_friend_card(
            shared.clone(),
            sharer.sign_pk.clone(),
            own_id.clone(),
            8,
            now
        )
        .unwrap());

        // Wrong sharer key: the card names one sharer, the key is another's.
        assert!(!verify_shared_friend_card(shared, other.sign_pk, own_id, 7, now).unwrap());
    }

    #[test]
    fn shared_card_expiry_tolerates_a_day_of_skew_and_no_more() {
        let (sharer, shared_person, _, shared) = shared_card_fixture();
        let own_id = shared_person.user_id.clone();
        let at = |now: i64| {
            verify_shared_friend_card(
                shared.clone(),
                sharer.sign_pk.clone(),
                own_id.clone(),
                7,
                now,
            )
            .unwrap()
        };
        assert!(at(shared.expires_at_ms + SHARED_CARD_CLOCK_SKEW_MS - 1));
        assert!(!at(shared.expires_at_ms + SHARED_CARD_CLOCK_SKEW_MS + 1));
        assert!(at(shared.issued_at_ms - SHARED_CARD_CLOCK_SKEW_MS + 1));
        assert!(!at(shared.issued_at_ms - SHARED_CARD_CLOCK_SKEW_MS - 1));
    }

    #[test]
    fn scanner_side_expiry_check_is_plain() {
        let (_, _, _, shared) = shared_card_fixture();
        assert!(!shared_card_expired(shared.clone(), shared.expires_at_ms));
        assert!(shared_card_expired(
            shared.clone(),
            shared.expires_at_ms + 1
        ));
    }

    #[test]
    fn shared_tail_rides_kind3_payload_and_old_clients_ignore_it() {
        let (_, _, _, shared) = shared_card_fixture();
        let requester = generate_identity();
        let card_json = make_friend_card("Riley".to_string(), requester, None, None).unwrap();
        let payload =
            make_shared_friend_request_payload(card_json.clone(), shared.clone()).unwrap();

        // New clients see both halves.
        let content = parse_friend_request_content(payload.clone()).unwrap();
        assert_eq!(content.card.name, "Riley");
        assert_eq!(content.shared, Some(shared));

        // Old clients parse the very same payload as a plain card (the
        // auto-import compatibility path) — the tail is an ignored field.
        assert_eq!(parse_friend_card(payload).unwrap().name, "Riley");

        // A tailless payload decodes with no shared half.
        let content = parse_friend_request_content(card_json).unwrap();
        assert!(content.shared.is_none());
    }

    #[test]
    fn malformed_shared_tail_is_an_error_not_an_auto_import() {
        let requester = generate_identity();
        let card_json = make_friend_card("Riley".to_string(), requester, None, None).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&card_json).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("shared".to_string(), serde_json::json!("not-base64!!"));
        assert!(parse_friend_request_content(value.to_string()).is_err());
        value
            .as_object_mut()
            .unwrap()
            .insert("shared".to_string(), serde_json::json!(42));
        assert!(parse_friend_request_content(value.to_string()).is_err());
    }

    #[test]
    fn create_shared_card_pins_the_seven_day_window() {
        let (_, _, _, shared) = shared_card_fixture();
        assert_eq!(
            shared.expires_at_ms - shared.issued_at_ms,
            SHARED_CARD_LIFETIME_MS
        );
        // A hand-built longer window fails shape validation on decode.
        let mut stretched = shared;
        stretched.expires_at_ms = stretched.issued_at_ms + SHARED_CARD_LIFETIME_MS + 1;
        assert!(encode_shared_friend_card(&stretched).is_err());
    }

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
