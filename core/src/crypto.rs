//! Message sealing (DESIGN.md §6.3): sign-then-seal, no bespoke crypto.
//!
//! 1. The plaintext payload is signed with the sender's Ed25519 key, and the
//!    sender's public key is embedded so the recipient can identify *and*
//!    verify the sender without any separate lookup. From envelope version 2
//!    the signature covers a domain-separated, length-framed transcript that
//!    also names the **recipient's X25519 public key** (see
//!    [`SignatureBinding`]), not the bare payload: a signed body is then only
//!    valid inside an envelope addressed to the key it was written for.
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
//! nonce(24) || ciphertext+tag`. The leading version byte is DESIGN.md §6.3's
//! upgrade hook: a future ratchet/PQ envelope bumps it, and old clients fail
//! closed on envelopes they can't correctly open instead of misparsing them.
//! This is a from-scratch construction, not a byte-for-byte
//! reimplementation of libsodium's `crypto_box_seal` (which derives its
//! nonce from the two public keys to save 24 bytes on the wire) -- here a
//! nonce is generated fresh per message instead, which is simpler and
//! equally secure since the sender key is already one-time-use.
//!
//! ## Why version 2 exists: surreptitious forwarding
//!
//! A version-1 signature covers only the payload, so the signed body
//! `sender_sign_pk || signature || payload` says nothing about who it was for.
//! Bob, holding a message Alice sealed to him, could keep that body verbatim,
//! seal it to Carol's public key, and Carol would open a message that verifies
//! as Alice's -- and, because a pairwise body's `chat_id` is Alice's own id,
//! `DELIVER-01` files it in Alice's thread. Version 2 signs
//! `frame(domain) || frame(recipient_agree_pk) || sender_sign_pk ||
//! frame(payload)` and the opener rebuilds that transcript from *its own*
//! public key, so the re-sealed body fails with
//! [`CoreError::SignatureInvalid`]. The group seal ([`crate::groups`]) uses a
//! distinct domain bound to the group id, so no signed body is valid in both
//! a pairwise and a group envelope.
//!
//! ## Rollout: open both, seal the old one for one release
//!
//! This build *opens* versions 1 and 2 and *seals* [`SEAL_ENVELOPE_VERSION`].
//! A build that predates version 2 cannot open it -- it fails closed, so the
//! envelope looks foreign and is carried rather than delivered -- which for a
//! family messenger is a lost message, the failure DESIGN.md §6.3 refuses a
//! flag day over. So the seal side flips to version 2 in a later release, once
//! the field fleet opens it; the constant is the whole flip. Until then, and
//! until the legacy open path below is removed, a version-1 body can still be
//! forwarded in the way described above.

use crypto_box::aead::{Aead, AeadCore};
use crypto_box::{PublicKey, SalsaBox, SecretKey};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;

use crate::identity::derive_user_id;
use crate::{CoreError, Identity};

/// Legacy pairwise envelope: the signature covers the bare payload and binds
/// no recipient. Opened only through [`SignatureBinding::Legacy`].
///
/// REMOVAL: the legacy open path (this version on open, and
/// [`SignatureBinding::Legacy`]) can be deleted once (a) every supported
/// build seals version 2 ([`SEAL_ENVELOPE_VERSION`] flipped and shipped), (b)
/// longer than [`crate::DEFAULT_EXPIRY_MS`] has passed since the last
/// version-1 sealing build went out of support, so no live envelope still
/// carries it, and (c) nothing re-opens stored version-1 ciphertext (the §8
/// sync-record backlog re-sealed on inbox-key rotation in `revocation.rs`).
const ENVELOPE_VERSION_LEGACY: u8 = 1;
/// Current pairwise envelope: the signature covers a domain-separated
/// transcript binding the recipient's X25519 public key.
const ENVELOPE_VERSION_BOUND: u8 = 2;
/// The highest pairwise envelope version this build opens.
const ENVELOPE_VERSION: u8 = ENVELOPE_VERSION_BOUND;
/// The pairwise envelope version this build seals. Held at the legacy version
/// for one release so builds that predate version 2 keep receiving mail; see
/// the module docs ("Rollout"). Flipping it to [`ENVELOPE_VERSION_BOUND`] is
/// the whole second step.
const SEAL_ENVELOPE_VERSION: u8 = ENVELOPE_VERSION_LEGACY;
const EPHEMERAL_PK_LEN: usize = 32;
const NONCE_LEN: usize = 24;
pub(crate) const PAD_BUCKET: usize = 256;
pub(crate) const SIGN_PK_LEN: usize = 32;
pub(crate) const SIGNATURE_LEN: usize = 64;

/// Signature domain for a version-2 pairwise body.
const PAIRWISE_SIGNATURE_DOMAIN: &[u8] = b"cruisemesh/sealed-body/pairwise/v2";
/// Signature domain for a version-2 group body ([`crate::groups`]).
const GROUP_SIGNATURE_DOMAIN: &[u8] = b"cruisemesh/sealed-body/group/v2";

/// What a signature is bound to, beyond the payload itself.
///
/// The wire body is the same for every variant (`sender_sign_pk ||
/// signature || payload`); only the bytes the signature covers differ. The
/// opener chooses the variant from the envelope version and supplies the
/// binding from its *own* state (its own public key, the group it opened
/// with), never from anything the sender wrote.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SignatureBinding<'a> {
    /// Version 1 (pairwise and group): the bare payload. Opening only, in
    /// practice; see [`ENVELOPE_VERSION_LEGACY`] for when it can be removed.
    Legacy,
    /// Version 2 pairwise: bound to the recipient's X25519 public key.
    Pairwise { recipient_agree_pk: &'a [u8] },
    /// Version 2 group: bound to the group id.
    Group { group_id: &'a [u8] },
}

impl SignatureBinding<'_> {
    /// The exact bytes the Ed25519 signature covers.
    ///
    /// Version 2 transcript: `frame(domain) || frame(binding) ||
    /// sender_sign_pk(32) || frame(payload)`, where `frame(x)` is a u32
    /// big-endian length followed by `x`. Every variable field is framed, so
    /// no two distinct `(domain, binding, payload)` triples share a
    /// transcript.
    fn transcript(&self, sender_sign_pk: &[u8], payload: &[u8]) -> Result<Vec<u8>, CoreError> {
        let (domain, binding) = match *self {
            SignatureBinding::Legacy => return Ok(payload.to_vec()),
            SignatureBinding::Pairwise { recipient_agree_pk } => {
                (PAIRWISE_SIGNATURE_DOMAIN, recipient_agree_pk)
            }
            SignatureBinding::Group { group_id } => (GROUP_SIGNATURE_DOMAIN, group_id),
        };
        let mut out = Vec::with_capacity(
            12 + domain.len() + binding.len() + sender_sign_pk.len() + payload.len(),
        );
        write_framed(&mut out, domain)?;
        write_framed(&mut out, binding)?;
        out.extend_from_slice(sender_sign_pk);
        write_framed(&mut out, payload)?;
        Ok(out)
    }
}

fn write_framed(out: &mut Vec<u8>, field: &[u8]) -> Result<(), CoreError> {
    let len = u32::try_from(field.len())
        .map_err(|_| CoreError::Crypto("signed field too long".to_string()))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(field);
    Ok(())
}

/// True when `payload` begins exactly like a version-2 transcript. A legacy
/// open refuses such a payload: otherwise Bob could take a version-2 body,
/// rebuild its transcript (every input is public to him), and present the
/// transcript as the "payload" of a version-1 body, which would verify.
fn looks_like_bound_transcript(payload: &[u8]) -> bool {
    [PAIRWISE_SIGNATURE_DOMAIN, GROUP_SIGNATURE_DOMAIN]
        .iter()
        .any(|domain| {
            let mut prefix = (domain.len() as u32).to_be_bytes().to_vec();
            prefix.extend_from_slice(domain);
            payload.starts_with(&prefix)
        })
}

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
    seal_message_as_version(
        &sender,
        &recipient_agree_pk,
        &payload,
        SEAL_ENVELOPE_VERSION,
    )
}

/// [`seal_message`] at an explicit envelope version. Private so the rollout
/// step stays one constant; tests use it to exercise both versions.
fn seal_message_as_version(
    sender: &Identity,
    recipient_agree_pk: &[u8],
    payload: &[u8],
    version: u8,
) -> Result<Vec<u8>, CoreError> {
    let recipient_pk = public_key_from_bytes(recipient_agree_pk)?;
    let binding = match version {
        ENVELOPE_VERSION_LEGACY => SignatureBinding::Legacy,
        ENVELOPE_VERSION_BOUND => SignatureBinding::Pairwise {
            recipient_agree_pk: recipient_pk.as_bytes(),
        },
        other => {
            return Err(CoreError::Crypto(format!(
                "cannot seal envelope version {other}"
            )))
        }
    };
    let padded = sign_and_pad(sender, binding, payload)?;

    let ephemeral_sk = SecretKey::generate(&mut OsRng);
    let ephemeral_pk = ephemeral_sk.public_key();
    let sealing_box = SalsaBox::new(&recipient_pk, &ephemeral_sk);
    let nonce = SalsaBox::generate_nonce(&mut OsRng);
    let ciphertext = sealing_box
        .encrypt(&nonce, padded.as_slice())
        .map_err(|_| CoreError::Crypto("seal failed".to_string()))?;

    let mut envelope = Vec::with_capacity(1 + EPHEMERAL_PK_LEN + NONCE_LEN + ciphertext.len());
    envelope.push(version);
    envelope.extend_from_slice(ephemeral_pk.as_bytes());
    envelope.extend_from_slice(nonce.as_slice());
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

/// Open a sealed envelope with `recipient`'s X25519 key, verify the embedded
/// signature, and return the sender's UserID alongside the plaintext payload.
#[uniffi::export]
pub fn open_message(recipient: Identity, sealed: Vec<u8>) -> Result<OpenedMessage, CoreError> {
    open_sealed_with_agree_sk(&recipient.agree_sk, &sealed)
}

/// The same open, addressed by a bare X25519 secret rather than by an
/// [`Identity`].
///
/// [`open_message`] is the whole-identity form every contact-facing path uses;
/// this one exists because `specs/multi-device-v1.md` §6 gives a *person* — not
/// a device identity — a scoped inbox key, and §8's sync records are sealed to
/// exactly that key ([`crate::core_open_sync_record`]). The envelope format,
/// the padding, and the embedded-signature check are identical; only the
/// question "which secret opens this" differs, so it stays one implementation
/// rather than two that could drift.
pub(crate) fn open_sealed_with_agree_sk(
    agree_sk: &[u8],
    sealed: &[u8],
) -> Result<OpenedMessage, CoreError> {
    if sealed.len() < 1 + EPHEMERAL_PK_LEN + NONCE_LEN {
        return Err(CoreError::Crypto("envelope too short".to_string()));
    }
    let (version, rest) = sealed.split_at(1);
    let version = version[0];
    if version != ENVELOPE_VERSION_LEGACY && version != ENVELOPE_VERSION_BOUND {
        return Err(CoreError::Crypto(format!(
            "unsupported envelope version {version} (this client speaks up to {ENVELOPE_VERSION})"
        )));
    }
    let (ephemeral_pk_bytes, rest) = rest.split_at(EPHEMERAL_PK_LEN);
    let (nonce_bytes, ciphertext) = rest.split_at(NONCE_LEN);

    let ephemeral_pk = public_key_from_bytes(ephemeral_pk_bytes)?;
    let recipient_sk = secret_key_from_bytes(agree_sk)?;
    let opening_box = SalsaBox::new(&ephemeral_pk, &recipient_sk);
    // `nonce_bytes` is exactly NONCE_LEN bytes: it came from splitting at a
    // fixed offset after the length check above, so this can't panic.
    let nonce = *crypto_box::Nonce::from_slice(nonce_bytes);
    let padded = opening_box
        .decrypt(&nonce, ciphertext)
        .map_err(|_| CoreError::Crypto("open failed".to_string()))?;
    // The binding is this device's own public key, derived from the secret
    // that just opened the box -- never a value read from the envelope. A body
    // signed for anyone else's key fails here. A version-2 envelope is never
    // checked under the legacy rule, whatever its body looks like.
    let own_agree_pk = recipient_sk.public_key();
    let binding = if version == ENVELOPE_VERSION_BOUND {
        SignatureBinding::Pairwise {
            recipient_agree_pk: own_agree_pk.as_bytes(),
        }
    } else {
        SignatureBinding::Legacy
    };
    open_signed_payload(&padded, binding)
}

pub(crate) fn sign_and_pad(
    sender: &Identity,
    binding: SignatureBinding<'_>,
    payload: &[u8],
) -> Result<Vec<u8>, CoreError> {
    let signing_key = signing_key_from_bytes(&sender.sign_sk)?;
    let signature = signing_key.sign(&binding.transcript(&sender.sign_pk, payload)?);

    let mut signed_body = Vec::with_capacity(SIGN_PK_LEN + SIGNATURE_LEN + payload.len());
    signed_body.extend_from_slice(&sender.sign_pk);
    signed_body.extend_from_slice(&signature.to_bytes());
    signed_body.extend_from_slice(payload);
    Ok(pad_to_bucket(&signed_body))
}

pub(crate) fn open_signed_payload(
    padded: &[u8],
    binding: SignatureBinding<'_>,
) -> Result<OpenedMessage, CoreError> {
    let signed_body = unpad(padded)?;
    if signed_body.len() < SIGN_PK_LEN + SIGNATURE_LEN {
        return Err(CoreError::Crypto("signed body too short".to_string()));
    }
    let (sender_sign_pk, rest) = signed_body.split_at(SIGN_PK_LEN);
    let (signature_bytes, message_payload) = rest.split_at(SIGNATURE_LEN);

    if matches!(binding, SignatureBinding::Legacy) && looks_like_bound_transcript(message_payload) {
        return Err(CoreError::SignatureInvalid);
    }

    let verifying_key = verifying_key_from_bytes(sender_sign_pk)?;
    let signature = Signature::from_bytes(
        signature_bytes
            .try_into()
            .map_err(|_| CoreError::Crypto("invalid signature length".to_string()))?,
    );
    verifying_key
        .verify(
            &binding.transcript(sender_sign_pk, message_payload)?,
            &signature,
        )
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
        assert_eq!(sealed[0], SEAL_ENVELOPE_VERSION);
        sealed[0] = 0x7F; // a future version this client doesn't speak

        let err = open_message(bob, sealed).unwrap_err();
        assert!(matches!(err, CoreError::Crypto(_)));
    }

    /// Bob's half of the forwarding attack: open an envelope sealed to him,
    /// keep the decrypted (signed, padded) body byte-for-byte, and seal it
    /// again to `new_recipient_agree_pk` under `new_version`. Everything here
    /// is something any recipient can do with their own key.
    fn reseal_body(
        sealed: &[u8],
        opener_agree_sk: &[u8],
        new_recipient_agree_pk: &[u8],
        new_version: u8,
    ) -> Vec<u8> {
        let padded = decrypt_body(sealed, opener_agree_sk);
        seal_padded_body(&padded, new_recipient_agree_pk, new_version)
    }

    fn decrypt_body(sealed: &[u8], opener_agree_sk: &[u8]) -> Vec<u8> {
        let ephemeral_pk = public_key_from_bytes(&sealed[1..1 + EPHEMERAL_PK_LEN]).unwrap();
        let nonce_start = 1 + EPHEMERAL_PK_LEN;
        let nonce = *crypto_box::Nonce::from_slice(&sealed[nonce_start..nonce_start + NONCE_LEN]);
        let opener = secret_key_from_bytes(opener_agree_sk).unwrap();
        SalsaBox::new(&ephemeral_pk, &opener)
            .decrypt(&nonce, &sealed[nonce_start + NONCE_LEN..])
            .expect("the original recipient can decrypt")
    }

    fn seal_padded_body(padded: &[u8], recipient_agree_pk: &[u8], version: u8) -> Vec<u8> {
        let recipient_pk = public_key_from_bytes(recipient_agree_pk).unwrap();
        let ephemeral_sk = SecretKey::generate(&mut OsRng);
        let nonce = SalsaBox::generate_nonce(&mut OsRng);
        let ciphertext = SalsaBox::new(&recipient_pk, &ephemeral_sk)
            .encrypt(&nonce, padded)
            .unwrap();
        let mut out = vec![version];
        out.extend_from_slice(ephemeral_sk.public_key().as_bytes());
        out.extend_from_slice(nonce.as_slice());
        out.extend_from_slice(&ciphertext);
        out
    }

    #[test]
    fn default_seal_uses_the_rollout_version_and_round_trips() {
        let alice = generate_identity();
        let bob = generate_identity();

        let sealed = seal_message(alice.clone(), bob.agree_pk.clone(), b"hi".to_vec()).unwrap();
        assert_eq!(sealed[0], SEAL_ENVELOPE_VERSION);
        let opened = open_message(bob, sealed).unwrap();
        assert_eq!(opened.payload, b"hi");
        assert_eq!(opened.sender_user_id, alice.user_id);
    }

    #[test]
    fn bound_envelope_round_trips_and_identifies_sender() {
        let alice = generate_identity();
        let bob = generate_identity();

        let sealed = seal_message_as_version(
            &alice,
            &bob.agree_pk,
            b"meet at the buffet at 6",
            ENVELOPE_VERSION_BOUND,
        )
        .unwrap();
        assert_eq!(sealed[0], ENVELOPE_VERSION_BOUND);
        // Same size as the legacy envelope: the binding is signed, not sent.
        assert_eq!(sealed.len(), 1 + 32 + 24 + 256 + 16);
        let opened = open_message(bob, sealed).unwrap();
        assert_eq!(opened.payload, b"meet at the buffet at 6");
        assert_eq!(opened.sender_user_id, alice.user_id);
    }

    /// The finding this version exists for: Bob re-seals Alice's signed body to
    /// Carol, and Carol must not accept it as Alice's.
    #[test]
    fn bound_body_resealed_to_a_third_party_fails_signature() {
        let alice = generate_identity();
        let bob = generate_identity();
        let carol = generate_identity();

        let to_bob = seal_message_as_version(
            &alice,
            &bob.agree_pk,
            b"only for bob",
            ENVELOPE_VERSION_BOUND,
        )
        .unwrap();
        let forwarded = reseal_body(
            &to_bob,
            &bob.agree_sk,
            &carol.agree_pk,
            ENVELOPE_VERSION_BOUND,
        );

        let err = open_message(carol, forwarded).unwrap_err();
        assert!(matches!(err, CoreError::SignatureInvalid), "{err:?}");
    }

    /// Bob cannot dodge the binding by relabelling the forward as version 1.
    #[test]
    fn bound_body_downgraded_to_legacy_fails_signature() {
        let alice = generate_identity();
        let bob = generate_identity();
        let carol = generate_identity();

        let to_bob = seal_message_as_version(
            &alice,
            &bob.agree_pk,
            b"only for bob",
            ENVELOPE_VERSION_BOUND,
        )
        .unwrap();
        let forwarded = reseal_body(
            &to_bob,
            &bob.agree_sk,
            &carol.agree_pk,
            ENVELOPE_VERSION_LEGACY,
        );

        let err = open_message(carol, forwarded).unwrap_err();
        assert!(matches!(err, CoreError::SignatureInvalid), "{err:?}");
    }

    /// Nor by rebuilding the version-2 transcript (all public to him) and
    /// presenting it as a version-1 payload, which Alice's signature does
    /// cover byte-for-byte.
    #[test]
    fn bound_transcript_smuggled_as_a_legacy_payload_is_refused() {
        let alice = generate_identity();
        let bob = generate_identity();
        let carol = generate_identity();

        let to_bob = seal_message_as_version(
            &alice,
            &bob.agree_pk,
            b"only for bob",
            ENVELOPE_VERSION_BOUND,
        )
        .unwrap();
        let body = unpad(&decrypt_body(&to_bob, &bob.agree_sk)).unwrap();
        let (sign_pk, rest) = body.split_at(SIGN_PK_LEN);
        let (signature, payload) = rest.split_at(SIGNATURE_LEN);
        let transcript = SignatureBinding::Pairwise {
            recipient_agree_pk: &bob.agree_pk,
        }
        .transcript(sign_pk, payload)
        .unwrap();
        let mut smuggled = sign_pk.to_vec();
        smuggled.extend_from_slice(signature);
        smuggled.extend_from_slice(&transcript);
        let forwarded = seal_padded_body(
            &pad_to_bucket(&smuggled),
            &carol.agree_pk,
            ENVELOPE_VERSION_LEGACY,
        );

        let err = open_message(carol, forwarded).unwrap_err();
        assert!(matches!(err, CoreError::SignatureInvalid), "{err:?}");
    }

    /// A pairwise body is not a group body: with the same binding bytes,
    /// signer and payload, the two domains still produce different
    /// transcripts, and both are recognised by the legacy-path guard.
    #[test]
    fn pairwise_and_group_transcripts_never_coincide() {
        let sign_pk = [7u8; 32];
        let binding = [9u8; 32];
        let pairwise = SignatureBinding::Pairwise {
            recipient_agree_pk: &binding,
        }
        .transcript(&sign_pk, b"x")
        .unwrap();
        let group = SignatureBinding::Group { group_id: &binding }
            .transcript(&sign_pk, b"x")
            .unwrap();
        assert_ne!(pairwise, group);
        assert!(looks_like_bound_transcript(&pairwise));
        assert!(looks_like_bound_transcript(&group));
        assert!(!looks_like_bound_transcript(b"x"));
    }

    /// A version-1 envelope built exactly the way pre-version-2 builds built
    /// it -- signature over the bare payload, written out here independently
    /// of `sign_and_pad` -- still opens, because sender fleets upgrade at
    /// different times.
    #[test]
    fn envelope_from_a_pre_binding_build_still_opens() {
        let alice = generate_identity();
        let bob = generate_identity();
        let payload = b"sent from an older build";

        let signature = signing_key_from_bytes(&alice.sign_sk)
            .unwrap()
            .sign(payload);
        let mut body = alice.sign_pk.clone();
        body.extend_from_slice(&signature.to_bytes());
        body.extend_from_slice(payload);
        let sealed = seal_padded_body(&pad_to_bucket(&body), &bob.agree_pk, 1);

        let opened = open_message(bob, sealed).unwrap();
        assert_eq!(opened.payload, payload);
        assert_eq!(opened.sender_user_id, alice.user_id);
    }

    /// The residual exposure while the legacy open path exists, pinned so
    /// removing it is a visible decision: a version-1 body (what this build
    /// still seals during the rollout) can be forwarded. Delete this test with
    /// the legacy path.
    #[test]
    fn legacy_body_remains_forwardable_until_the_legacy_path_is_removed() {
        let alice = generate_identity();
        let bob = generate_identity();
        let carol = generate_identity();

        let to_bob = seal_message_as_version(
            &alice,
            &bob.agree_pk,
            b"only for bob",
            ENVELOPE_VERSION_LEGACY,
        )
        .unwrap();
        let forwarded = reseal_body(
            &to_bob,
            &bob.agree_sk,
            &carol.agree_pk,
            ENVELOPE_VERSION_LEGACY,
        );
        assert!(open_message(carol, forwarded).is_ok());
    }

    #[test]
    fn sealing_an_unknown_version_is_refused() {
        let alice = generate_identity();
        let bob = generate_identity();
        assert!(seal_message_as_version(&alice, &bob.agree_pk, b"x", 3).is_err());
    }
}
