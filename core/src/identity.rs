//! Identity, keypairs, UserID derivation, and friend-card encoding.
//!
//! Scheme (DESIGN.md §6.2): each identity is an Ed25519 signing keypair plus an
//! X25519 agreement keypair, generated on-device. UserID = first 16 bytes of
//! BLAKE2b(Ed25519 public key). Friending exchanges a FriendCard (JSON, carried
//! over QR code or pasted text) containing both public keys.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use data_encoding::BASE32_NOPAD;
use ed25519_dalek::SigningKey;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};

const USER_ID_LEN: usize = 16;

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

/// Build the JSON payload shared via QR code / pasted text when friending.
#[uniffi::export]
pub fn make_friend_card(name: String, identity: Identity, relay_url: Option<String>) -> String {
    let card = FriendCard {
        name,
        sign_pk: identity.sign_pk,
        agree_pk: identity.agree_pk,
        relay_url,
    };
    // Construction is infallible: every field is already valid UTF-8 / bytes.
    serde_json::to_string(&card).expect("FriendCard always serializes")
}

/// Parse a friend-card JSON payload received via QR scan or pasted text.
#[uniffi::export]
pub fn parse_friend_card(json: String) -> Result<FriendCard, CoreError> {
    let card: FriendCard =
        serde_json::from_str(&json).map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
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
    Ok(card)
}

/// Small nautical/travel-themed wordlist for fingerprint phrases. Not
/// security-critical (only 4 words are shown), so a compact list is fine.
const WORDLIST: [&str; 64] = [
    "anchor", "atoll", "beacon", "bilge", "boatswain", "bosun", "bow", "breeze",
    "bridge", "buoy", "cabin", "captain", "chart", "clipper", "coast", "compass",
    "coral", "current", "dock", "dolphin", "ferry", "fjord", "flag", "fleet",
    "galley", "gangway", "harbor", "helm", "horizon", "island", "jetty", "keel",
    "knot", "lagoon", "lantern", "latitude", "lighthouse", "longitude", "mast", "mate",
    "moor", "navigate", "ocean", "oar", "pier", "port", "quay", "reef",
    "rudder", "sail", "sextant", "shore", "starboard", "stern", "swell", "tide",
    "tropic", "vessel", "voyage", "wake", "wave", "wharf", "wind", "yacht",
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
        let json = make_friend_card("Dave".to_string(), id.clone(), None);
        let card = parse_friend_card(json).expect("valid card");
        assert_eq!(card.name, "Dave");
        assert_eq!(card.sign_pk, id.sign_pk);
        assert_eq!(card.agree_pk, id.agree_pk);
        assert_eq!(friend_card_user_id(card), id.user_id);
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
}
