//! Message sealing (DESIGN.md §6.3): sign-then-seal, no bespoke crypto.
//!
//! 1. The plaintext payload is signed with the sender's Ed25519 key, and the
//!    sender's public key is embedded so the recipient can identify *and*
//!    verify the sender without any separate lookup.
//! 2. The signed body is padded to the next 256-byte bucket (so relays can't
//!    distinguish a short "ok" from a long paragraph by ciphertext length).
//! 3. The padded body is encrypted to the recipient's X25519 key with a
//!    fresh, one-time ephemeral sender keypair (forward secrecy against a
//!    later-compromised sender key; DESIGN.md §6.3 explicitly does *not*
//!    ratchet -- see that section for why). This uses RustCrypto's
//!    `crypto_box` crate: a pure-Rust, Cure53-audited implementation of
//!    NaCl/libsodium's `crypto_box` (X25519 + XSalsa20-Poly1305), i.e. the
//!    same vetted construction DESIGN.md §6.1 calls for ("use libsodium
//!    primitives whole, via maintained bindings"), without pulling in an
//!    actual C libsodium build (a much larger, autotools-based cross-compile
//!    surface than `rusqlite`'s bundled SQLite amalgamation).
//!
//! The sealed envelope on the wire is `version(1) || ephemeral_pubkey(32) ||
//! nonce(24) || ciphertext+tag`. The leading version byte (currently 0x01)
//! is DESIGN.md §6.3's upgrade hook: a future ratchet/PQ envelope bumps it,
//! and old clients fail closed on envelopes they can't correctly open
//! instead of misparsing them. This is a from-scratch construction, not a byte-for-byte
//! reimplementation of libsodium's `crypto_box_seal` (which derives its
//! nonce from the two public keys to save 24 bytes on the wire) -- here a
//! nonce is generated fresh per message instead, which is simpler and
//! equally secure since the sender key is already one-time-use.

use crypto_box::aead::{Aead, AeadCore};
use crypto_box::{PublicKey, SalsaBox, SecretKey};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;

use crate::identity::derive_user_id;
use crate::{CoreError, Identity};

const ENVELOPE_VERSION: u8 = 1;
const EPHEMERAL_PK_LEN: usize = 32;
const NONCE_LEN: usize = 24;
pub(crate) const PAD_BUCKET: usize = 256;
pub(crate) const SIGN_PK_LEN: usize = 32;
pub(crate) const SIGNATURE_LEN: usize = 64;

/// A payload recovered from a sealed envelope, with the sender identified
/// and their signature already verified.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct OpenedMessage {
    pub sender_user_id: Vec<u8>,
    pub payload: Vec<u8>,
}

/// Sign `payload` with `sender`'s Ed25519 key, pad it, and seal it to
/// `recipient_agree_pk` (an X25519 public key, e.g. from a `FriendCard`).
#[uniffi::export]
pub fn seal_message(
    sender: Identity,
    recipient_agree_pk: Vec<u8>,
    payload: Vec<u8>,
) -> Result<Vec<u8>, CoreError> {
    let padded = sign_and_pad(&sender, &payload)?;

    let recipient_pk = public_key_from_bytes(&recipient_agree_pk)?;
    let ephemeral_sk = SecretKey::generate(&mut OsRng);
    let ephemeral_pk = ephemeral_sk.public_key();
    let sealing_box = SalsaBox::new(&recipient_pk, &ephemeral_sk);
    let nonce = SalsaBox::generate_nonce(&mut OsRng);
    let ciphertext = sealing_box
        .encrypt(&nonce, padded.as_slice())
        .map_err(|_| CoreError::Crypto("seal failed".to_string()))?;

    let mut envelope = Vec::with_capacity(1 + EPHEMERAL_PK_LEN + NONCE_LEN + ciphertext.len());
    envelope.push(ENVELOPE_VERSION);
    envelope.extend_from_slice(ephemeral_pk.as_bytes());
    envelope.extend_from_slice(nonce.as_slice());
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

/// Open a sealed envelope with `recipient`'s X25519 key, verify the embedded
/// signature, and return the sender's UserID alongside the plaintext payload.
#[uniffi::export]
pub fn open_message(recipient: Identity, sealed: Vec<u8>) -> Result<OpenedMessage, CoreError> {
    if sealed.len() < 1 + EPHEMERAL_PK_LEN + NONCE_LEN {
        return Err(CoreError::Crypto("envelope too short".to_string()));
    }
    let (version, rest) = sealed.split_at(1);
    if version[0] != ENVELOPE_VERSION {
        return Err(CoreError::Crypto(format!(
            "unsupported envelope version {} (this client speaks {ENVELOPE_VERSION})",
            version[0]
        )));
    }
    let (ephemeral_pk_bytes, rest) = rest.split_at(EPHEMERAL_PK_LEN);
    let (nonce_bytes, ciphertext) = rest.split_at(NONCE_LEN);

    let ephemeral_pk = public_key_from_bytes(ephemeral_pk_bytes)?;
    let recipient_sk = secret_key_from_bytes(&recipient.agree_sk)?;
    let opening_box = SalsaBox::new(&ephemeral_pk, &recipient_sk);
    // `nonce_bytes` is exactly NONCE_LEN bytes: it came from splitting at a
    // fixed offset after the length check above, so this can't panic.
    let nonce = *crypto_box::Nonce::from_slice(nonce_bytes);
    let padded = opening_box
        .decrypt(&nonce, ciphertext)
        .map_err(|_| CoreError::Crypto("open failed".to_string()))?;
    open_signed_payload(&padded)
}

pub(crate) fn sign_and_pad(sender: &Identity, payload: &[u8]) -> Result<Vec<u8>, CoreError> {
    let signing_key = signing_key_from_bytes(&sender.sign_sk)?;
    let signature = signing_key.sign(payload);

    let mut signed_body = Vec::with_capacity(SIGN_PK_LEN + SIGNATURE_LEN + payload.len());
    signed_body.extend_from_slice(&sender.sign_pk);
    signed_body.extend_from_slice(&signature.to_bytes());
    signed_body.extend_from_slice(payload);
    Ok(pad_to_bucket(&signed_body))
}

pub(crate) fn open_signed_payload(padded: &[u8]) -> Result<OpenedMessage, CoreError> {
    let signed_body = unpad(padded)?;
    if signed_body.len() < SIGN_PK_LEN + SIGNATURE_LEN {
        return Err(CoreError::Crypto("signed body too short".to_string()));
    }
    let (sender_sign_pk, rest) = signed_body.split_at(SIGN_PK_LEN);
    let (signature_bytes, message_payload) = rest.split_at(SIGNATURE_LEN);

    let verifying_key = verifying_key_from_bytes(sender_sign_pk)?;
    let signature = Signature::from_bytes(
        signature_bytes
            .try_into()
            .map_err(|_| CoreError::Crypto("invalid signature length".to_string()))?,
    );
    verifying_key
        .verify(message_payload, &signature)
        .map_err(|_| CoreError::SignatureInvalid)?;

    Ok(OpenedMessage {
        sender_user_id: derive_user_id(sender_sign_pk).to_vec(),
        payload: message_payload.to_vec(),
    })
}

pub(crate) fn pad_to_bucket(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    let padded_len = out.len().div_ceil(PAD_BUCKET) * PAD_BUCKET;
    out.resize(padded_len.max(PAD_BUCKET), 0);
    out
}

pub(crate) fn unpad(data: &[u8]) -> Result<Vec<u8>, CoreError> {
    if data.len() < 4 {
        return Err(CoreError::Crypto("padded body too short".to_string()));
    }
    let len = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
    if 4 + len > data.len() {
        return Err(CoreError::Crypto("corrupt padding length".to_string()));
    }
    Ok(data[4..4 + len].to_vec())
}

pub(crate) fn signing_key_from_bytes(bytes: &[u8]) -> Result<SigningKey, CoreError> {
    let arr: [u8; 32] = bytes.try_into().map_err(|_| key_len_err(32, bytes.len()))?;
    Ok(SigningKey::from_bytes(&arr))
}

pub(crate) fn verifying_key_from_bytes(bytes: &[u8]) -> Result<VerifyingKey, CoreError> {
    let arr: [u8; 32] = bytes.try_into().map_err(|_| key_len_err(32, bytes.len()))?;
    VerifyingKey::from_bytes(&arr)
        .map_err(|_| CoreError::Crypto("invalid Ed25519 public key".to_string()))
}

fn public_key_from_bytes(bytes: &[u8]) -> Result<PublicKey, CoreError> {
    let arr: [u8; 32] = bytes.try_into().map_err(|_| key_len_err(32, bytes.len()))?;
    Ok(PublicKey::from(arr))
}

fn secret_key_from_bytes(bytes: &[u8]) -> Result<SecretKey, CoreError> {
    let arr: [u8; 32] = bytes.try_into().map_err(|_| key_len_err(32, bytes.len()))?;
    Ok(SecretKey::from(arr))
}

pub(crate) fn key_len_err(expected: u32, actual: usize) -> CoreError {
    CoreError::InvalidKeyLength {
        expected,
        actual: actual as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::generate_identity;

    #[test]
    fn seal_then_open_round_trips_and_identifies_sender() {
        let alice = generate_identity();
        let bob = generate_identity();

        let sealed = seal_message(
            alice.clone(),
            bob.agree_pk.clone(),
            b"meet at the buffet at 6".to_vec(),
        )
        .expect("seal succeeds");
        let opened = open_message(bob, sealed).expect("open succeeds");

        assert_eq!(opened.payload, b"meet at the buffet at 6");
        assert_eq!(opened.sender_user_id, alice.user_id);
    }

    #[test]
    fn wrong_recipient_cannot_open() {
        let alice = generate_identity();
        let bob = generate_identity();
        let mallory = generate_identity();

        let sealed =
            seal_message(alice, bob.agree_pk.clone(), b"secret".to_vec()).expect("seal succeeds");
        let err = open_message(mallory, sealed).unwrap_err();
        assert!(matches!(err, CoreError::Crypto(_)));
    }

    #[test]
    fn tampered_ciphertext_fails_to_open() {
        let alice = generate_identity();
        let bob = generate_identity();

        let mut sealed = seal_message(alice, bob.agree_pk.clone(), b"meet at the buffet".to_vec())
            .expect("seal succeeds");
        let last = sealed.len() - 1;
        sealed[last] ^= 0xFF;

        let err = open_message(bob, sealed).unwrap_err();
        assert!(matches!(err, CoreError::Crypto(_)));
    }

    #[test]
    fn short_message_is_padded_to_bucket_size() {
        let alice = generate_identity();
        let bob = generate_identity();

        let sealed =
            seal_message(alice, bob.agree_pk.clone(), b"hi".to_vec()).expect("seal succeeds");
        // envelope = version byte + 32-byte ephemeral pk + 24-byte nonce + (256-byte padded body + 16-byte AEAD tag)
        assert_eq!(sealed.len(), 1 + 32 + 24 + 256 + 16);
    }

    #[test]
    fn message_spanning_two_buckets_pads_to_second_bucket() {
        let alice = generate_identity();
        let bob = generate_identity();

        // signed body = 32 (sender pk) + 64 (sig) + payload; push just past one 256-byte bucket.
        let payload = vec![0u8; 200];
        let sealed = seal_message(alice, bob.agree_pk.clone(), payload).expect("seal succeeds");
        assert_eq!(sealed.len(), 1 + 32 + 24 + 512 + 16);
    }

    #[test]
    fn unknown_envelope_version_is_rejected() {
        let alice = generate_identity();
        let bob = generate_identity();

        let mut sealed =
            seal_message(alice, bob.agree_pk.clone(), b"hi".to_vec()).expect("seal succeeds");
        assert_eq!(sealed[0], ENVELOPE_VERSION);
        sealed[0] = 0x7F; // a future version this client doesn't speak

        let err = open_message(bob, sealed).unwrap_err();
        assert!(matches!(err, CoreError::Crypto(_)));
    }
}
