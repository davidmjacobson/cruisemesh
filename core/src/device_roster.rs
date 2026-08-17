//! Person/device identity split and the device roster
//! (`specs/multi-device-v1.md` §3, §4, §14).
//!
//! One person, several devices. The person root stays the Ed25519 identity key
//! this fleet already deploys — its public key is still the wire `user_id`
//! (called `person_id` in new code) — and after migration it signs only device
//! certificates and roster genesis, never messages (§3). Each device holds its
//! own Ed25519 signing key and X25519 DH key; a [`DeviceCert`] binds
//! `(person_id, device signing key, added_epoch, flags)` under a
//! person-authorized signature.
//!
//! Everything here is pure: no store, no transport, no clock. The module takes
//! a stored roster and an incoming one and says accept / ignore / quarantine.
//! Persisting the verdict, gossiping the document, and sealing it pairwise all
//! belong to the layers above (DL-3 — a roster travels as ordinary sealed 1:1
//! traffic; there is no directory, and this module never learns of one).
//!
//! Two properties are structural rather than conventional, because a convention
//! is what regresses:
//!
//! * **DL-5 (no field an endpoint fits in).** Every byte field of a [`Roster`]
//!   and a [`DeviceCert`] is a fixed-length key or id: no string, no URL, no
//!   host, nothing free-form. Be precise about what enforces it, though — the
//!   Rust types are all `Vec<u8>`, so the *type* forbids nothing and the single
//!   gate is [`core_roster_validate`]'s fixed-width check, which every stored
//!   and every accepted roster passes through. What that buys is real but
//!   bounded: an address of exactly 16 or 32 bytes is still refusable only in
//!   the sense that it is indistinguishable from a key and cannot be *read* as
//!   an address by anything downstream, because nothing downstream ever treats
//!   these fields as text. Adding a variable-length field here would end that
//!   property, which is the regression the check exists to make loud.
//! * **Domain separation.** Every new-format signature is computed over bytes
//!   that start with a distinct, versioned context string (§3), so a signature
//!   minted for one domain can never be replayed in another. The wire layouts
//!   those strings introduce are frozen by fixed-key golden vectors in this
//!   file's tests, in the same style as `identity.rs`'s CMFRIEND2 vectors.

use crate::crypto::{signing_key_from_bytes, verifying_key_from_bytes};
use crate::identity::derive_user_id;
use crate::CoreError;
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier};
use rand_core::OsRng;
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};

/// Length of a person id and of a device id: both are the first 16 bytes of
/// BLAKE2b over the corresponding Ed25519 public key, so a device is addressed
/// in exactly the namespace a person is (`identity.rs`'s `derive_user_id`).
pub const DEVICE_ID_LEN: usize = 16;
/// §5: every envelope that carries no sealed-body device field, and every row
/// that predates the migration, belongs to this reserved all-zero stream of its
/// person. A real device id can never collide with it — [`core_roster_validate`]
/// refuses a certificate that derives to it.
pub const LEGACY_DEVICE_ID: [u8; DEVICE_ID_LEN] = [0u8; DEVICE_ID_LEN];
/// Ed25519 / X25519 public keys and Ed25519 secret keys are all 32 bytes.
const KEY_LEN: usize = 32;
/// Raw Ed25519 signature length.
const SIGNATURE_LEN: usize = 64;
/// BLAKE2b-256 roster head, the digest a `CMFRIEND4:` card carries
/// (`identity.rs`'s `FriendCard::roster_head_hash`, §12).
pub const ROSTER_HEAD_HASH_LEN: usize = 32;

/// §14.3: a person may hold up to 16 devices; adding one that takes the count
/// past 8 succeeds with a warning (the 9th warns, the 8th does not), and an add
/// that would take it past 16 is refused (the 17th).
pub const DEVICE_SOFT_CAP: u32 = 8;
pub const DEVICE_HARD_CAP: u32 = 16;

/// The one device that currently holds the roster-signing role (§3's authority
/// split). Exactly the device named by [`Roster::approving_device_id`] carries
/// this flag; every other certificate must not. Every other bit is reserved and
/// preserved verbatim through signing and validation, so a later work package
/// can assign one without invalidating rosters minted today.
pub const DEVICE_CERT_FLAG_ROSTER_SIGNING: u32 = 1 << 0;

// ---------------------------------------------------------------------------
// Signing domains (§3)
// ---------------------------------------------------------------------------

/// Device certificates. Distinct from every other domain in this crate,
/// including `identity.rs`'s friend-card and shared-card domains.
const DEVICE_CERT_SIGN_DOMAIN: &[u8] = b"CruiseMesh device certificate v1\0";
/// Roster updates (the document signature itself).
const ROSTER_SIGN_DOMAIN: &[u8] = b"CruiseMesh device roster v1\0";
/// Per-device message authoring (§5). WP1 owns the domain and its framing; the
/// authoring call sites arrive with the sealed-body device field.
const MESSAGE_AUTHORING_SIGN_DOMAIN: &[u8] = b"CruiseMesh device message authoring v1\0";
/// Own-device sync records (§8). Same treatment: the domain is frozen here so
/// WP4 cannot accidentally ship a colliding one.
const SYNC_RECORD_SIGN_DOMAIN: &[u8] = b"CruiseMesh device sync record v1\0";
/// The two frames of §9's linking ceremony a device signs with the key it is
/// being certified for: the offer naming its fresh keys (§9.3), and the
/// acknowledgement of the exact roster hash that closes activation (§9.4).
/// Its own domain because these are the only signatures a device makes
/// *before* it is a device — one minted here must never be replayable as a
/// message, a sync record, or a certificate.
const DEVICE_LINK_ACTIVATION_SIGN_DOMAIN: &[u8] = b"CruiseMesh device link activation v1\0";
/// §9.3's bootstrap export, signed by the approving device over the whole
/// payload, the channel it is crossing, and the moment it stops being valid.
/// Its own domain for the same reason as the one above and one level stricter:
/// this signature is what makes an export *this ceremony's* export, so it must
/// never be mistakable for the activation frames riding the same channel.
const DEVICE_LINK_BOOTSTRAP_SIGN_DOMAIN: &[u8] = b"CruiseMesh device link bootstrap v1\0";
/// Roster head hashing. Not a signing domain — it separates the digest input
/// from the signature input so the head can never be mistaken for a signature
/// pre-image.
const ROSTER_HEAD_HASH_DOMAIN: &[u8] = b"CruiseMesh device roster head v1\0";
/// Per-device recipient-hint namespaces (§7). Also not a signing domain: it
/// keeps a device's routing namespace disjoint from every other digest this
/// file computes over the same ids.
const DEVICE_HINT_NAMESPACE_DOMAIN: &[u8] = b"CruiseMesh device hint namespace v1\0";

/// Which context string a raw device signature is bound to.
///
/// Message authoring and sync records are signed through
/// [`core_device_sign`]; certificates and rosters have their own canonical
/// layouts and use [`core_sign_device_cert`] / [`core_sign_roster`], which
/// reach the same primitive with their own domain.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DeviceSigningDomain {
    DeviceCert,
    RosterUpdate,
    MessageAuthoring,
    SyncRecord,
    /// §9's link-ceremony frames, signed by a device that is not yet in any
    /// roster (`device_link::activation`).
    DeviceLinkActivation,
    /// §9.3's canonical bootstrap, signed by the APPROVING device's
    /// roster-signing key over the export, the channel binding, and the expiry
    /// (`device_link::bootstrap`).
    DeviceLinkBootstrap,
}

fn domain_separator(domain: DeviceSigningDomain) -> &'static [u8] {
    match domain {
        DeviceSigningDomain::DeviceCert => DEVICE_CERT_SIGN_DOMAIN,
        DeviceSigningDomain::RosterUpdate => ROSTER_SIGN_DOMAIN,
        DeviceSigningDomain::MessageAuthoring => MESSAGE_AUTHORING_SIGN_DOMAIN,
        DeviceSigningDomain::SyncRecord => SYNC_RECORD_SIGN_DOMAIN,
        DeviceSigningDomain::DeviceLinkActivation => DEVICE_LINK_ACTIVATION_SIGN_DOMAIN,
        DeviceSigningDomain::DeviceLinkBootstrap => DEVICE_LINK_BOOTSTRAP_SIGN_DOMAIN,
    }
}

// ---------------------------------------------------------------------------
// Device keys
// ---------------------------------------------------------------------------

/// A device's own keypairs, private material included.
///
/// Same contract as [`crate::Identity`]: the core generates and never persists;
/// the shell stores the secrets in platform-protected storage. The person root
/// secret is NOT here and never is — after migration it lives only inside the
/// passphrase-encrypted `.cmbak` backup (§3, §14.2), which is what keeps a
/// stolen phone from revoking the person's real devices.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DeviceKeypair {
    /// 16 bytes, derived from `sign_pk`. This is the id that joins the message
    /// stream key `(chat_id, sender_person_id, sender_device_id, lamport)`.
    pub device_id: Vec<u8>,
    pub sign_pk: Vec<u8>,
    pub sign_sk: Vec<u8>,
    pub agree_pk: Vec<u8>,
    pub agree_sk: Vec<u8>,
}

/// Generate a device's Ed25519 signing keypair and X25519 DH keypair (§3).
#[uniffi::export]
pub fn generate_device_keypair() -> DeviceKeypair {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();
    let agree_sk = StaticSecret::random_from_rng(OsRng);
    let agree_pk = XPublicKey::from(&agree_sk);

    DeviceKeypair {
        device_id: derive_user_id(verifying_key.as_bytes()).to_vec(),
        sign_pk: verifying_key.as_bytes().to_vec(),
        sign_sk: signing_key.to_bytes().to_vec(),
        agree_pk: agree_pk.as_bytes().to_vec(),
        agree_sk: agree_sk.to_bytes().to_vec(),
    }
}

/// Bytes of one [`DeviceKeypair`]: `device_id(16) || sign_pk || sign_sk ||
/// agree_pk || agree_sk`.
const DEVICE_KEYPAIR_LEN: usize = DEVICE_ID_LEN + 4 * KEY_LEN;

/// Flatten a [`DeviceKeypair`] for the shell's platform-protected storage.
///
/// Same division of labour [`encode_identity_bytes`](crate::encode_identity_bytes)
/// established, and here for the same reason: the core generates and never
/// persists, the shell keeps the secrets in the Keystore or the Keychain, and
/// neither of them gets to invent its own layout for the other to misread.
/// Every field is fixed-width, so this is plain concatenation.
#[uniffi::export]
pub fn core_encode_device_keypair(device: DeviceKeypair) -> Result<Vec<u8>, CoreError> {
    check_key_len(&device.device_id, DEVICE_ID_LEN)?;
    for key in [
        &device.sign_pk,
        &device.sign_sk,
        &device.agree_pk,
        &device.agree_sk,
    ] {
        check_key_len(key, KEY_LEN)?;
    }
    let mut out = Vec::with_capacity(DEVICE_KEYPAIR_LEN);
    out.extend_from_slice(&device.device_id);
    out.extend_from_slice(&device.sign_pk);
    out.extend_from_slice(&device.sign_sk);
    out.extend_from_slice(&device.agree_pk);
    out.extend_from_slice(&device.agree_sk);
    Ok(out)
}

/// Read back what [`core_encode_device_keypair`] wrote, re-deriving the device
/// id rather than trusting the stored one: a blob whose id does not follow from
/// its own signing key is corrupt, not a device.
#[uniffi::export]
pub fn core_decode_device_keypair(bytes: Vec<u8>) -> Result<DeviceKeypair, CoreError> {
    if bytes.len() != DEVICE_KEYPAIR_LEN {
        return Err(CoreError::Malformed(format!(
            "expected {DEVICE_KEYPAIR_LEN} device keypair bytes, got {}",
            bytes.len()
        )));
    }
    let sign_pk = bytes[16..48].to_vec();
    let device_id = derive_user_id(&sign_pk).to_vec();
    if device_id != bytes[0..16] {
        return Err(CoreError::Malformed(
            "stored device keypair does not match its own signing key".to_string(),
        ));
    }
    Ok(DeviceKeypair {
        device_id,
        sign_pk,
        sign_sk: bytes[48..80].to_vec(),
        agree_pk: bytes[80..112].to_vec(),
        agree_sk: bytes[112..144].to_vec(),
    })
}

fn check_key_len(key: &[u8], expected: usize) -> Result<(), CoreError> {
    if key.len() != expected {
        return Err(crate::crypto::key_len_err(expected as u32, key.len()));
    }
    Ok(())
}

/// Device id = first 16 bytes of BLAKE2b(device signing pubkey).
///
/// §3 calls the device signing pubkey the `device_id`; on the wire and in the
/// store that key is addressed by this 16-byte derivation, which is the same
/// construction `user_id` already uses and the width the `sender_device_id`
/// column carries.
#[uniffi::export]
pub fn core_derive_device_id(device_sign_pk: Vec<u8>) -> Result<Vec<u8>, CoreError> {
    if device_sign_pk.len() != KEY_LEN {
        return Err(crate::crypto::key_len_err(
            KEY_LEN as u32,
            device_sign_pk.len(),
        ));
    }
    Ok(derive_user_id(&device_sign_pk).to_vec())
}

/// The reserved all-zero legacy stream id (§5).
#[uniffi::export]
pub fn core_legacy_device_id() -> Vec<u8> {
    LEGACY_DEVICE_ID.to_vec()
}

/// Map an optional sealed-body device field onto a stream id: absent (a legacy
/// envelope, or any row minted before the migration) means
/// [`LEGACY_DEVICE_ID`], never an error (§5).
#[uniffi::export]
pub fn core_device_stream_id(sender_device_id: Option<Vec<u8>>) -> Vec<u8> {
    match sender_device_id {
        Some(id) if id.len() == DEVICE_ID_LEN => id,
        _ => LEGACY_DEVICE_ID.to_vec(),
    }
}

/// The routing namespace one device's relay rows are addressed under (§7):
/// `BLAKE2b-16(domain || person_id || device_id)`, both inputs length-framed.
///
/// Sixteen bytes, because that is exactly the width
/// [`compute_recipient_hint`](crate::compute_recipient_hint) already consumes —
/// a device namespace drops into every existing hint path where a `user_id`
/// goes, and nothing downstream needs to learn a second shape.
///
/// Three properties earn the hash rather than a plain concatenation:
///
/// * **One-way.** The relay stores `BLAKE2b-8(namespace || day)` and nothing
///   else. Hashing the person in means a device's daily hints cannot be walked
///   back to the person they belong to, and two of one person's devices are
///   not visibly siblings — which a `person_id || device_id` namespace would
///   have given away to anyone holding either half.
/// * **Ids only (DL-5).** Both inputs are fixed-width ids. Nothing here has a
///   field an endpoint fits in, and the derivation forwards no addressing.
/// * **Unambiguous.** Length framing means no `(person, device)` pair can
///   collide with another by re-splitting the same bytes.
///
/// [`LEGACY_DEVICE_ID`] — and any absent or malformed device id, which §5 maps
/// to it — returns `person_id` unchanged: the person namespace, which is
/// today's hint, unchanged for every v1 peer. Paired with the same fallback in
/// [`device_fanout_msg_id`](crate::device_fanout_msg_id), so a legacy row's id
/// and the hint it is found under always agree.
#[uniffi::export]
pub fn core_device_namespace_id(person_id: Vec<u8>, device_id: Vec<u8>) -> Vec<u8> {
    if device_id.len() != DEVICE_ID_LEN || device_id[..] == LEGACY_DEVICE_ID[..] {
        return person_id;
    }
    let mut input = DEVICE_HINT_NAMESPACE_DOMAIN.to_vec();
    push_len_prefixed(&mut input, &person_id);
    push_len_prefixed(&mut input, &device_id);
    let mut hasher = Blake2bVar::new(DEVICE_ID_LEN).expect("valid blake2b output length");
    hasher.update(&input);
    let mut out = vec![0u8; DEVICE_ID_LEN];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    out
}

// ---------------------------------------------------------------------------
// Domain-separated raw signing
// ---------------------------------------------------------------------------

/// Domain-separated, length-framed bytes a device signature is computed over.
/// The length prefix means no two `(domain, message)` pairs can ever collide,
/// and the domain prefix means a signature is only ever valid in the context it
/// was minted for.
fn domain_signed_bytes(domain: DeviceSigningDomain, message: &[u8]) -> Vec<u8> {
    let mut out = domain_separator(domain).to_vec();
    push_len_prefixed(&mut out, message);
    out
}

/// Sign `message` under a device's Ed25519 key in one domain (§3).
#[uniffi::export]
pub fn core_device_sign(
    domain: DeviceSigningDomain,
    device_sign_sk: Vec<u8>,
    message: Vec<u8>,
) -> Result<Vec<u8>, CoreError> {
    let signing_key = signing_key_from_bytes(&device_sign_sk)?;
    Ok(signing_key
        .sign(&domain_signed_bytes(domain, &message))
        .to_bytes()
        .to_vec())
}

/// Verify a device signature in one domain. A signature from another domain
/// fails here, which is the whole point of the context strings.
#[uniffi::export]
pub fn core_device_verify(
    domain: DeviceSigningDomain,
    device_sign_pk: Vec<u8>,
    message: Vec<u8>,
    signature: Vec<u8>,
) -> Result<(), CoreError> {
    verify_raw(
        &device_sign_pk,
        &domain_signed_bytes(domain, &message),
        &signature,
    )
}

fn verify_raw(sign_pk: &[u8], signed_bytes: &[u8], signature: &[u8]) -> Result<(), CoreError> {
    let signature_bytes: [u8; SIGNATURE_LEN] = signature
        .try_into()
        .map_err(|_| CoreError::SignatureInvalid)?;
    let verifying_key = verifying_key_from_bytes(sign_pk)?;
    verifying_key
        .verify(signed_bytes, &Signature::from_bytes(&signature_bytes))
        .map_err(|_| CoreError::SignatureInvalid)
}

fn push_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

// ---------------------------------------------------------------------------
// Device certificates
// ---------------------------------------------------------------------------

/// A person-authorized certificate binding one device to one person (§3).
///
/// `signer_sign_pk` records who authorized it: the person root (genesis and
/// recovery) or a device whose own certificate is still listed in the same
/// roster and itself chains back to the root. A revoked signer does not keep
/// vouching: its certificate leaves `devices` when it is tombstoned, so any
/// certificate it signed is orphaned and must be re-signed (see
/// [`core_roster_validate`]).
///
/// Note what is not here: no endpoint, no relay URL, no name. DL-5 keeps a
/// certificate to keys, ids, and integers.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DeviceCert {
    /// The person this device belongs to: 16 bytes, the wire `user_id`.
    pub person_id: Vec<u8>,
    /// The device's Ed25519 signing public key (32 bytes).
    pub device_sign_pk: Vec<u8>,
    /// The device's X25519 DH public key (32 bytes) — what siblings and
    /// contacts seal to.
    pub device_agree_pk: Vec<u8>,
    /// The person's `recovery_epoch` when this device was added.
    pub added_epoch: u64,
    /// [`DEVICE_CERT_FLAG_ROSTER_SIGNING`] and bits reserved for later work
    /// packages, preserved verbatim.
    pub flags: u32,
    /// The key that authorized this certificate.
    pub signer_sign_pk: Vec<u8>,
    /// Raw 64-byte Ed25519 signature over [`device_cert_signed_bytes`].
    pub signature: Vec<u8>,
}

impl DeviceCert {
    /// The 16-byte id this certificate's device is addressed by.
    pub fn device_id(&self) -> Vec<u8> {
        derive_user_id(&self.device_sign_pk).to_vec()
    }
}

/// The certificate body, without the signature: the bytes both the certificate
/// signature and the enclosing roster commit to.
fn device_cert_body(cert: &DeviceCert) -> Vec<u8> {
    let mut out = Vec::new();
    push_len_prefixed(&mut out, &cert.person_id);
    push_len_prefixed(&mut out, &cert.device_sign_pk);
    push_len_prefixed(&mut out, &cert.device_agree_pk);
    out.extend_from_slice(&cert.added_epoch.to_be_bytes());
    out.extend_from_slice(&cert.flags.to_be_bytes());
    push_len_prefixed(&mut out, &cert.signer_sign_pk);
    out
}

/// Canonical, domain-separated bytes a [`DeviceCert`]'s signature covers.
/// The signature itself is excluded, so signing and verifying see identical
/// bytes; `signer_sign_pk` is included, so a certificate can never be
/// re-attributed to a different authorizer.
pub(crate) fn device_cert_signed_bytes(cert: &DeviceCert) -> Vec<u8> {
    let mut out = DEVICE_CERT_SIGN_DOMAIN.to_vec();
    out.extend_from_slice(&device_cert_body(cert));
    out
}

/// Sign a device certificate under the authorizing key (person root, or the
/// approving device). `signer_sign_pk` and `signature` are filled in from
/// `signer_sign_sk`, so the recorded signer can never disagree with the key
/// that actually signed.
#[uniffi::export]
pub fn core_sign_device_cert(
    cert: DeviceCert,
    signer_sign_sk: Vec<u8>,
) -> Result<DeviceCert, CoreError> {
    let signing_key = signing_key_from_bytes(&signer_sign_sk)?;
    let mut cert = cert;
    cert.signer_sign_pk = signing_key.verifying_key().as_bytes().to_vec();
    cert.signature = signing_key
        .sign(&device_cert_signed_bytes(&cert))
        .to_bytes()
        .to_vec();
    Ok(cert)
}

/// Verify a certificate against its own recorded signer. Whether that signer is
/// *person-authorized* is a roster-level question, answered by
/// [`core_roster_validate`].
#[uniffi::export]
pub fn core_verify_device_cert(cert: DeviceCert) -> Result<(), CoreError> {
    verify_raw(
        &cert.signer_sign_pk,
        &device_cert_signed_bytes(&cert),
        &cert.signature,
    )
}

// ---------------------------------------------------------------------------
// Roster
// ---------------------------------------------------------------------------

/// A revoked device, kept forever (DL-4).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct DeviceTombstone {
    /// 16-byte device id. The key itself is deliberately not retained — a
    /// tombstone only has to name what may never come back.
    pub device_id: Vec<u8>,
    pub revoked_at_seq: u64,
}

/// The person-signed, versioned device document of §4.
///
/// DL-5: keys, ids, counters, one signature. An endpoint has nowhere to live.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Roster {
    pub person_id: Vec<u8>,
    /// Raised only by a roster signed with the recovery material — the person
    /// root secret in the encrypted backup (§14.2). A higher epoch always
    /// supersedes anything the approving device signed, which is how a stolen
    /// approving device is dethroned.
    pub recovery_epoch: u64,
    /// Monotone within a `recovery_epoch`.
    pub seq: u64,
    /// Active device certificates.
    pub devices: Vec<DeviceCert>,
    /// Revoked devices, forever (DL-4).
    pub tombstones: Vec<DeviceTombstone>,
    /// The device currently holding the roster-signing role (§3).
    pub approving_device_id: Vec<u8>,
    /// §6; bumped on every revocation. WP5 rotates the key itself.
    pub inbox_key_generation: u64,
    /// The key that signed this document: the approving device, or the person
    /// root (genesis and recovery).
    pub signer_sign_pk: Vec<u8>,
    /// Raw 64-byte Ed25519 signature over [`roster_signed_bytes`].
    pub signature: Vec<u8>,
}

/// `(recovery_epoch, seq)` — the DL-1 ordering key, compared lexicographically.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, uniffi::Record)]
pub struct RosterVersion {
    pub recovery_epoch: u64,
    pub seq: u64,
}

impl Roster {
    pub fn version(&self) -> RosterVersion {
        RosterVersion {
            recovery_epoch: self.recovery_epoch,
            seq: self.seq,
        }
    }
}

/// Canonical, domain-separated bytes a roster's signature covers: every field
/// except the signature, each length-framed, certificates included whole
/// (bodies *and* their signatures) so a roster commits to exactly the
/// certificates it ships.
pub(crate) fn roster_signed_bytes(roster: &Roster) -> Vec<u8> {
    let mut out = ROSTER_SIGN_DOMAIN.to_vec();
    push_len_prefixed(&mut out, &roster.person_id);
    out.extend_from_slice(&roster.recovery_epoch.to_be_bytes());
    out.extend_from_slice(&roster.seq.to_be_bytes());
    out.extend_from_slice(&(roster.devices.len() as u64).to_be_bytes());
    for cert in &roster.devices {
        let mut framed = device_cert_body(cert);
        push_len_prefixed(&mut framed, &cert.signature);
        push_len_prefixed(&mut out, &framed);
    }
    out.extend_from_slice(&(roster.tombstones.len() as u64).to_be_bytes());
    for tombstone in &roster.tombstones {
        push_len_prefixed(&mut out, &tombstone.device_id);
        out.extend_from_slice(&tombstone.revoked_at_seq.to_be_bytes());
    }
    push_len_prefixed(&mut out, &roster.approving_device_id);
    out.extend_from_slice(&roster.inbox_key_generation.to_be_bytes());
    push_len_prefixed(&mut out, &roster.signer_sign_pk);
    out
}

/// Sign a roster under the approving device's key or the person root's (§3).
/// As with certificates, the recorded signer is derived from the secret so the
/// two can never disagree.
#[uniffi::export]
pub fn core_sign_roster(roster: Roster, signer_sign_sk: Vec<u8>) -> Result<Roster, CoreError> {
    let signing_key = signing_key_from_bytes(&signer_sign_sk)?;
    let mut roster = roster;
    roster.signer_sign_pk = signing_key.verifying_key().as_bytes().to_vec();
    roster.signature = signing_key
        .sign(&roster_signed_bytes(&roster))
        .to_bytes()
        .to_vec();
    Ok(roster)
}

/// BLAKE2b-256 of the roster document — the digest a `CMFRIEND4:` card carries
/// (§12) and the value a linking device acknowledges back to the approving
/// device (§9.4).
///
/// It covers the roster's content, not its signature, so the head names *which
/// roster* this is; two documents with the same head are the same document.
/// That is also what makes it the DL-2 fork discriminator.
#[uniffi::export]
pub fn core_roster_head_hash(roster: Roster) -> Vec<u8> {
    roster_head_hash(&roster)
}

pub(crate) fn roster_head_hash(roster: &Roster) -> Vec<u8> {
    let mut hasher = Blake2bVar::new(ROSTER_HEAD_HASH_LEN).expect("valid blake2b output length");
    hasher.update(ROSTER_HEAD_HASH_DOMAIN);
    hasher.update(&roster_signed_bytes(roster));
    let mut out = vec![0u8; ROSTER_HEAD_HASH_LEN];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    out
}

/// The active device ids of a roster, in document order.
#[uniffi::export]
pub fn core_roster_device_ids(roster: Roster) -> Vec<Vec<u8>> {
    roster.devices.iter().map(DeviceCert::device_id).collect()
}

// ---------------------------------------------------------------------------
// Roster wire codec (DL-3)
// ---------------------------------------------------------------------------

/// Leading byte of an encoded roster document.
const ROSTER_WIRE_VERSION: u8 = 1;

/// Encode a roster as a transferable document (DL-3: a roster gossips as
/// ordinary sealed 1:1 traffic, and §8 syncs the *own* roster to siblings
/// inside a sync record — see [`crate::SyncOwnRosterPayload`]).
///
/// Deliberately NOT [`roster_signed_bytes`]: that output is a signature
/// pre-image — domain-prefixed and *without* the signature — so it can never
/// round-trip. This layout carries every field, signature included, each
/// length-framed, so `decode(encode(r)) == r` and so a receiver re-checks
/// through [`core_roster_validate`] exactly the document its signer signed.
///
/// DL-5 survives the codec for the same reason it holds in the type: every
/// field written here is a fixed-width key, id, or counter that the validator
/// re-checks on the way back in. The codec adds no free-form field, so it
/// cannot become the place an endpoint fits.
#[uniffi::export]
pub fn core_encode_roster(roster: Roster) -> Result<Vec<u8>, CoreError> {
    let mut out = vec![ROSTER_WIRE_VERSION];
    push_wire_bytes(&mut out, &roster.person_id)?;
    out.extend_from_slice(&roster.recovery_epoch.to_be_bytes());
    out.extend_from_slice(&roster.seq.to_be_bytes());
    push_wire_count(&mut out, roster.devices.len(), "devices")?;
    for cert in &roster.devices {
        push_wire_bytes(&mut out, &cert.person_id)?;
        push_wire_bytes(&mut out, &cert.device_sign_pk)?;
        push_wire_bytes(&mut out, &cert.device_agree_pk)?;
        out.extend_from_slice(&cert.added_epoch.to_be_bytes());
        out.extend_from_slice(&cert.flags.to_be_bytes());
        push_wire_bytes(&mut out, &cert.signer_sign_pk)?;
        push_wire_bytes(&mut out, &cert.signature)?;
    }
    push_wire_count(&mut out, roster.tombstones.len(), "tombstones")?;
    for tombstone in &roster.tombstones {
        push_wire_bytes(&mut out, &tombstone.device_id)?;
        out.extend_from_slice(&tombstone.revoked_at_seq.to_be_bytes());
    }
    push_wire_bytes(&mut out, &roster.approving_device_id)?;
    out.extend_from_slice(&roster.inbox_key_generation.to_be_bytes());
    push_wire_bytes(&mut out, &roster.signer_sign_pk)?;
    push_wire_bytes(&mut out, &roster.signature)?;
    Ok(out)
}

/// Decode a roster document. Fully bounds-checked and never panics on
/// attacker-controlled bytes; trailing bytes are an error. Nothing here judges
/// the document — [`core_roster_validate`] and [`core_roster_accept`] remain
/// the only authorities on whether a decoded roster may be believed.
#[uniffi::export]
pub fn core_decode_roster(bytes: Vec<u8>) -> Result<Roster, CoreError> {
    let mut cursor = RosterCursor::new(&bytes);
    let version = cursor.take_u8()?;
    if version != ROSTER_WIRE_VERSION {
        return Err(CoreError::Malformed(format!(
            "unsupported roster wire version {version}"
        )));
    }
    let person_id = cursor.take_bytes()?;
    let recovery_epoch = cursor.take_u64()?;
    let seq = cursor.take_u64()?;
    let device_count = cursor.take_u16()? as usize;
    // The capacity hint is capped at the §14.3 hard cap on purpose: a lying
    // count must not be able to make this allocate before a single certificate
    // has been read.
    let mut devices = Vec::with_capacity(device_count.min(DEVICE_HARD_CAP as usize));
    for _ in 0..device_count {
        devices.push(DeviceCert {
            person_id: cursor.take_bytes()?,
            device_sign_pk: cursor.take_bytes()?,
            device_agree_pk: cursor.take_bytes()?,
            added_epoch: cursor.take_u64()?,
            flags: cursor.take_u32()?,
            signer_sign_pk: cursor.take_bytes()?,
            signature: cursor.take_bytes()?,
        });
    }
    let tombstone_count = cursor.take_u16()? as usize;
    let mut tombstones = Vec::with_capacity(tombstone_count.min(DEVICE_HARD_CAP as usize));
    for _ in 0..tombstone_count {
        tombstones.push(DeviceTombstone {
            device_id: cursor.take_bytes()?,
            revoked_at_seq: cursor.take_u64()?,
        });
    }
    let approving_device_id = cursor.take_bytes()?;
    let inbox_key_generation = cursor.take_u64()?;
    let signer_sign_pk = cursor.take_bytes()?;
    let signature = cursor.take_bytes()?;
    cursor.finish()?;
    Ok(Roster {
        person_id,
        recovery_epoch,
        seq,
        devices,
        tombstones,
        approving_device_id,
        inbox_key_generation,
        signer_sign_pk,
        signature,
    })
}

fn push_wire_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), CoreError> {
    if bytes.len() > u16::MAX as usize {
        return Err(CoreError::Malformed(
            "roster field is too long to encode".to_string(),
        ));
    }
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn push_wire_count(out: &mut Vec<u8>, count: usize, field: &str) -> Result<(), CoreError> {
    if count > u16::MAX as usize {
        return Err(CoreError::Malformed(format!(
            "roster has too many {field} to encode"
        )));
    }
    out.extend_from_slice(&(count as u16).to_be_bytes());
    Ok(())
}

/// A bounds-checked cursor, in the shape every other codec in this crate keeps
/// privately, so a truncated roster is a [`CoreError::Malformed`] and never a
/// panic.
struct RosterCursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> RosterCursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], CoreError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&end| end <= self.data.len())
            .ok_or_else(|| {
                CoreError::Malformed(format!(
                    "truncated roster: need {n} more byte(s) at offset {}",
                    self.pos
                ))
            })?;
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn take_u8(&mut self) -> Result<u8, CoreError> {
        Ok(self.take(1)?[0])
    }

    fn take_u16(&mut self) -> Result<u16, CoreError> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("exactly 2 bytes"),
        ))
    }

    fn take_u32(&mut self) -> Result<u32, CoreError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("exactly 4 bytes"),
        ))
    }

    fn take_u64(&mut self) -> Result<u64, CoreError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("exactly 8 bytes"),
        ))
    }

    fn take_bytes(&mut self) -> Result<Vec<u8>, CoreError> {
        let len = self.take_u16()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn finish(self) -> Result<(), CoreError> {
        if self.pos != self.data.len() {
            return Err(CoreError::Malformed(format!(
                "{} unexpected trailing byte(s) after the roster",
                self.data.len() - self.pos
            )));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Validation (DL-1 chain, DL-4 tombstones, DL-5 shape, §14.3 cap)
// ---------------------------------------------------------------------------

/// Why a roster is not acceptable on its own terms. `None` from
/// [`core_roster_validate`] means the document is well-formed and every
/// signature in it verifies to a person-authorized key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RosterRejection {
    /// The document names a different person than the root key it is checked
    /// against.
    PersonMismatch,
    /// A key, id, or signature is the wrong length — the DL-5 shape check.
    MalformedField,
    /// A certificate derives to the reserved [`LEGACY_DEVICE_ID`].
    ReservedDeviceId,
    /// The same device appears twice in `devices` or in `tombstones`.
    DuplicateDevice,
    /// A certificate's own signature does not verify.
    CertSignatureInvalid,
    /// DL-1: a certificate's signature chain does not terminate at the person
    /// root — the signer is a stranger, a device whose own certificate is no
    /// longer listed (a revoked predecessor's orphan), or a cycle of
    /// certificates vouching only for each other.
    ChainBroken,
    /// The roster's own signature does not verify.
    SignatureInvalid,
    /// The roster was signed by neither the person root nor its approving
    /// device.
    SignerNotAuthorized,
    /// `approving_device_id` names no active device.
    ApprovingDeviceMissing,
    /// The roster-signing flag is not on exactly the approving device.
    ApprovingRoleMismatch,
    /// Roster genesis (`seq == 0`) must be signed by the person root (§3). A
    /// recovery roster takes the same shape — a new `recovery_epoch` resets the
    /// seq — so this rule refuses a device-signed recovery document too.
    GenesisNotRootSigned,
    /// DL-4: a tombstoned device id is listed as active.
    TombstonedDeviceActive,
    /// §14.3: more than 16 active devices.
    DeviceCapExceeded,
}

fn is_len(bytes: &[u8], len: usize) -> bool {
    bytes.len() == len
}

/// Validate a roster against the person root key it claims to descend from.
///
/// This answers DL-1's second half — "the signature chain verifies back to the
/// person root" — plus the structural rules that keep the document what §4 says
/// it is. Ordering against a stored roster is [`core_roster_accept`]'s job.
///
/// The chain rule: the document must be signed by the person root or by its
/// approving device, and every certificate must *terminate at the person root*.
/// That is computed rather than asserted — the vouched set starts as the root
/// alone and grows to a fixpoint, admitting a certificate only once the key
/// that signed it is already vouched for. A certificate left over when the
/// fixpoint stops is [`RosterRejection::ChainBroken`], which is what stops two
/// smuggled certificates from vouching for each other, or one from vouching for
/// itself: neither can ever reach the seed.
///
/// The deliberate representational consequence: **tombstoned devices do not
/// carry their certificates** — a [`DeviceTombstone`] names an id and nothing
/// else (DL-4), so a revoked device is not a link the chain can pass through.
/// Every active certificate must therefore chain to the root through
/// certificates that are *still listed*. Re-rostering after a revocation must
/// consequently re-sign whatever the revoked device had signed, which is
/// exactly what §10.1's revocation step does at the moment it buries the
/// device: the approving device tombstones the revoked one and re-signs its
/// orphans in the same roster update. Revoking the approving device itself is
/// §10's recovery-code path, where the person root signs the new epoch's
/// genesis and re-issues the certificates directly.
#[uniffi::export]
pub fn core_roster_validate(
    roster: Roster,
    person_root_sign_pk: Vec<u8>,
) -> Option<RosterRejection> {
    validate(&roster, &person_root_sign_pk).err()
}

fn validate(roster: &Roster, person_root_sign_pk: &[u8]) -> Result<(), RosterRejection> {
    // DL-5 shape: every byte field is a fixed-length key or id. A roster that
    // fails this check is not a roster with a bad field, it is a document that
    // could carry something a roster may never carry.
    if !is_len(person_root_sign_pk, KEY_LEN)
        || !is_len(&roster.person_id, DEVICE_ID_LEN)
        || !is_len(&roster.approving_device_id, DEVICE_ID_LEN)
        || !is_len(&roster.signer_sign_pk, KEY_LEN)
        || !is_len(&roster.signature, SIGNATURE_LEN)
    {
        return Err(RosterRejection::MalformedField);
    }
    if derive_user_id(person_root_sign_pk).to_vec() != roster.person_id {
        return Err(RosterRejection::PersonMismatch);
    }
    // §14.3 through the one function that owns the boundary, so the cap has a
    // single implementation and the refusal here and the outcome the caller
    // reads on [`RosterUpdateDecision::device_count_outcome`] can never disagree:
    // a document holding more devices than the hard cap is the 17th-device add.
    if core_device_add_outcome(roster.devices.len() as u32) == DeviceAddOutcome::Refused {
        return Err(RosterRejection::DeviceCapExceeded);
    }

    let mut device_ids: Vec<Vec<u8>> = Vec::with_capacity(roster.devices.len());
    for cert in &roster.devices {
        if !is_len(&cert.device_sign_pk, KEY_LEN)
            || !is_len(&cert.device_agree_pk, KEY_LEN)
            || !is_len(&cert.signer_sign_pk, KEY_LEN)
            || !is_len(&cert.signature, SIGNATURE_LEN)
        {
            return Err(RosterRejection::MalformedField);
        }
        if cert.person_id != roster.person_id {
            return Err(RosterRejection::PersonMismatch);
        }
        let device_id = cert.device_id();
        if device_id == LEGACY_DEVICE_ID {
            return Err(RosterRejection::ReservedDeviceId);
        }
        if device_ids.contains(&device_id) {
            return Err(RosterRejection::DuplicateDevice);
        }
        device_ids.push(device_id);
    }

    let mut tombstone_ids: Vec<Vec<u8>> = Vec::with_capacity(roster.tombstones.len());
    for tombstone in &roster.tombstones {
        if !is_len(&tombstone.device_id, DEVICE_ID_LEN) {
            return Err(RosterRejection::MalformedField);
        }
        if tombstone_ids.contains(&tombstone.device_id) {
            return Err(RosterRejection::DuplicateDevice);
        }
        // DL-4: a revoked device_id never returns to `devices`.
        if device_ids.contains(&tombstone.device_id) {
            return Err(RosterRejection::TombstonedDeviceActive);
        }
        tombstone_ids.push(tombstone.device_id.clone());
    }

    // Exactly the approving device carries the roster-signing flag, so the one
    // authority §3 describes cannot be silently held by two devices at once.
    let approving_index = device_ids
        .iter()
        .position(|id| id == &roster.approving_device_id)
        .ok_or(RosterRejection::ApprovingDeviceMissing)?;
    for (index, cert) in roster.devices.iter().enumerate() {
        let signs = cert.flags & DEVICE_CERT_FLAG_ROSTER_SIGNING != 0;
        if signs != (index == approving_index) {
            return Err(RosterRejection::ApprovingRoleMismatch);
        }
    }

    // DL-1 chain, first half: every certificate verifies under the key it
    // records as its authorizer.
    for cert in &roster.devices {
        verify_raw(
            &cert.signer_sign_pk,
            &device_cert_signed_bytes(cert),
            &cert.signature,
        )
        .map_err(|_| RosterRejection::CertSignatureInvalid)?;
    }
    // Second half: those authorizers must chain back to the person root. The
    // vouched set is seeded with the root ALONE and grown to a fixpoint, so
    // membership is derived from the root rather than from "appears somewhere
    // in this document" — a set that a pair of mutually-signed certificates, or
    // a single self-signed one, could otherwise satisfy on their own.
    // Tombstoned ids are deliberately not seeds: a burial keeps no certificate,
    // so it is not a link (see this function's doc comment).
    let signer_ids: Vec<Vec<u8>> = roster
        .devices
        .iter()
        .map(|cert| derive_user_id(&cert.signer_sign_pk).to_vec())
        .collect();
    let mut vouched = vec![false; roster.devices.len()];
    loop {
        let mut grew = false;
        for index in 0..roster.devices.len() {
            if vouched[index] {
                continue;
            }
            let reaches_root = roster.devices[index].signer_sign_pk == person_root_sign_pk
                || device_ids
                    .iter()
                    .enumerate()
                    .any(|(other, id)| vouched[other] && id == &signer_ids[index]);
            if reaches_root {
                vouched[index] = true;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    if vouched.iter().any(|reached| !reached) {
        return Err(RosterRejection::ChainBroken);
    }

    // The document's own signature: root (genesis and recovery) or the
    // approving device.
    let signed_by_root = roster.signer_sign_pk == person_root_sign_pk;
    let signed_by_approver =
        roster.signer_sign_pk == roster.devices[approving_index].device_sign_pk;
    if !signed_by_root && !signed_by_approver {
        return Err(RosterRejection::SignerNotAuthorized);
    }
    if roster.seq == 0 && !signed_by_root {
        return Err(RosterRejection::GenesisNotRootSigned);
    }
    verify_raw(
        &roster.signer_sign_pk,
        &roster_signed_bytes(roster),
        &roster.signature,
    )
    .map_err(|_| RosterRejection::SignatureInvalid)
}

// ---------------------------------------------------------------------------
// Acceptance (DL-1 ordering, DL-2 fork quarantine, DL-4 tombstone persistence)
// ---------------------------------------------------------------------------

/// What a contact does with an incoming roster.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RosterUpdateOutcome {
    /// Store the incoming roster in place of what was held.
    Accepted,
    /// Keep what was stored (DL-1 idempotent gossip, or a document that does
    /// not verify).
    Ignored,
    /// DL-2: a fork. Keep the stored roster and quarantine this person's roster
    /// updates from here on — never auto-resolved.
    ForkQuarantined,
}

/// Which rule produced the outcome. Diagnostic, and the shape the WP0 vectors
/// read when they name a rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum RosterUpdateReason {
    /// Nothing was stored for this person yet, so the document is judged on
    /// its own merits: it must validate against the person root this device
    /// already has for that contact, and then it is adopted at whatever
    /// `(recovery_epoch, seq)` it names.
    ///
    /// This is trust-on-first-gossip, and it is deliberate. The safety it
    /// rests on is the chain: a first roster still has to terminate at the
    /// person root (§3), which is the very key that already authenticates
    /// every message from that contact — so a stranger cannot seed one.
    ///
    /// KNOWN RESIDUAL, owned by WP5's recovery flow: a *stolen approving
    /// device* can seed a brand-new contact with a roster at an inflated
    /// `recovery_epoch`, and that contact, having no baseline, adopts it.
    /// Every later honest roster from the real person is then a
    /// [`RosterUpdateReason::Rollback`] until the person recovers past the
    /// inflated epoch. §14.2 supremacy still holds — only the root secret in
    /// the encrypted backup can climb — so the recovery path resolves it; what
    /// WP1 does not have is a way to notice it at adoption time.
    FirstRoster,
    /// DL-1: strictly higher `(recovery_epoch, seq)`, and it verifies.
    Superseded,
    /// DL-1: lower `(recovery_epoch, seq)` — a replay or a stale gossip copy.
    Rollback,
    /// DL-1: the same version and the same document.
    IdempotentRepeat,
    /// DL-2: the same version, a different document.
    ForkedContent,
    /// DL-2: this person's rosters are already quarantined.
    PersonQuarantined,
    /// The document itself is not acceptable; see `rejection`.
    Invalid,
    /// §14.2: raising `recovery_epoch` requires the recovery material — the
    /// person root secret from the encrypted backup. An approving device
    /// cannot mint itself a higher epoch.
    ///
    /// Scope, deliberately: this rule compares an incoming roster against a
    /// **stored** one, so it can only refuse an epoch that climbs above a
    /// baseline this device already holds. It says nothing about the first
    /// roster ever seen for a person — there is no baseline to climb above —
    /// and [`RosterUpdateReason::FirstRoster`] documents what happens there.
    RecoveryEpochRequiresRoot,
    /// DL-4: the incoming roster drops a tombstone the stored one carries, or
    /// re-activates a device it already buried.
    TombstoneResurrected,
    /// A roster's `inbox_key_generation` never goes backwards within one
    /// recovery epoch (§6).
    InboxGenerationRegressed,
}

/// The verdict, plus the quarantine bit the caller must persist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct RosterUpdateDecision {
    pub outcome: RosterUpdateOutcome,
    pub reason: RosterUpdateReason,
    /// Why the document was rejected, when `reason` is
    /// [`RosterUpdateReason::Invalid`].
    pub rejection: Option<RosterRejection>,
    /// DL-2: once true this stays true. A later, perfectly good roster is not a
    /// resolution — a fork is resolved by a person, never by arithmetic.
    pub quarantined: bool,
    /// §14.3 applied to the device count the *incoming* document carries —
    /// named for what it measures (a count) rather than for an add, because
    /// this decision is about a document that arrived, not about a device this
    /// user chose to add.
    ///
    /// [`DeviceAddOutcome::Refused`] is exactly the
    /// [`RosterRejection::DeviceCapExceeded`] refusal above — the same
    /// [`core_device_add_outcome`] call decides both, so they cannot drift.
    ///
    /// **[`DeviceAddOutcome::AddedWithWarning`] is not for the contact path.**
    /// The soft cap is advice to a person about their OWN fleet ("you now have
    /// 9 or more devices"), and §2 goal 1 says a person's device count is
    /// invisible to other users — so surfacing it about a *contact* would leak
    /// exactly what the goal protects, from a document that arrived by gossip.
    /// [`MessageStore::apply_contact_roster`](crate::MessageStore::apply_contact_roster)
    /// therefore reports a non-surfacing verdict on that path. The warning
    /// surface belongs to the own-roster ADD path — §9's linking ceremony,
    /// which is WP3's — where the count is the user's own business and there is
    /// a person to tell.
    pub device_count_outcome: DeviceAddOutcome,
}

fn decision(
    outcome: RosterUpdateOutcome,
    reason: RosterUpdateReason,
    quarantined: bool,
) -> RosterUpdateDecision {
    RosterUpdateDecision {
        outcome,
        reason,
        rejection: None,
        quarantined,
        // Stamped by [`core_roster_accept`] on the way out: no inner rule
        // decides §14.3, so no inner path can forget to.
        device_count_outcome: DeviceAddOutcome::Added,
    }
}

fn invalid(rejection: RosterRejection) -> RosterUpdateDecision {
    RosterUpdateDecision {
        rejection: Some(rejection),
        ..decision(
            RosterUpdateOutcome::Ignored,
            RosterUpdateReason::Invalid,
            false,
        )
    }
}

/// Decide what to do with an incoming roster (DL-1, DL-2, DL-4).
///
/// Pure: `stored` and `stored_quarantined` are what the caller has persisted
/// for this person, and the returned [`RosterUpdateDecision::quarantined`] is
/// what it must persist afterwards. Nothing here reads a clock, a store, or a
/// transport — DL-3 gossip is the caller's business, and a roster arriving over
/// relay, LAN, BLE, or carry is judged identically.
#[uniffi::export]
pub fn core_roster_accept(
    stored: Option<Roster>,
    stored_quarantined: bool,
    incoming: Roster,
    person_root_sign_pk: Vec<u8>,
) -> RosterUpdateDecision {
    // §14.3 is stamped once, here, for every path below — including the ones
    // that never reach [`validate`] — so the cap answer travels with the
    // verdict rather than being recomputed by each caller from the roster it
    // may or may not have kept.
    let device_count_outcome = core_device_add_outcome(incoming.devices.len() as u32);
    RosterUpdateDecision {
        device_count_outcome,
        ..ordering_verdict(stored, stored_quarantined, incoming, person_root_sign_pk)
    }
}

/// §2 goal 1 through the contact path: a person's device count is invisible to
/// other users, so a decision about a document that merely arrived about a
/// CONTACT never carries the soft-cap warning a shell would surface.
///
/// [`DeviceAddOutcome::Refused`] survives untouched. It is not a fact about the
/// person being disclosed — it is this device refusing a document, and a caller
/// has to be able to tell a refusal from an acceptance.
pub(crate) fn non_surfacing_device_count_outcome(outcome: DeviceAddOutcome) -> DeviceAddOutcome {
    match outcome {
        DeviceAddOutcome::AddedWithWarning => DeviceAddOutcome::Added,
        other => other,
    }
}

/// Everything [`core_roster_accept`] decides *except* §14.3: DL-1 ordering,
/// DL-2 quarantine, DL-4 tombstones, and the [`validate`] gate.
fn ordering_verdict(
    stored: Option<Roster>,
    stored_quarantined: bool,
    incoming: Roster,
    person_root_sign_pk: Vec<u8>,
) -> RosterUpdateDecision {
    // DL-2: quarantine is sticky and is checked before anything else, so no
    // amount of later well-formed traffic can lift it.
    if stored_quarantined {
        return decision(
            RosterUpdateOutcome::ForkQuarantined,
            RosterUpdateReason::PersonQuarantined,
            true,
        );
    }

    if let Err(rejection) = validate(&incoming, &person_root_sign_pk) {
        return invalid(rejection);
    }

    let Some(stored) = stored else {
        return decision(
            RosterUpdateOutcome::Accepted,
            RosterUpdateReason::FirstRoster,
            false,
        );
    };

    // A roster for a different person is not an update to this one.
    if stored.person_id != incoming.person_id {
        return invalid(RosterRejection::PersonMismatch);
    }

    match incoming.version().cmp(&stored.version()) {
        // DL-1: lower is a rollback attempt or stale gossip.
        std::cmp::Ordering::Less => {
            return decision(
                RosterUpdateOutcome::Ignored,
                RosterUpdateReason::Rollback,
                false,
            )
        }
        std::cmp::Ordering::Equal => {
            // DL-1 vs DL-2: the same version is either the same document
            // (idempotent gossip) or a fork.
            return if roster_head_hash(&stored) == roster_head_hash(&incoming) {
                decision(
                    RosterUpdateOutcome::Ignored,
                    RosterUpdateReason::IdempotentRepeat,
                    false,
                )
            } else {
                decision(
                    RosterUpdateOutcome::ForkQuarantined,
                    RosterUpdateReason::ForkedContent,
                    true,
                )
            };
        }
        std::cmp::Ordering::Greater => {}
    }

    // §14.2: only the recovery material may raise the epoch. The approving
    // device signs within an epoch; it can never dethrone the backup.
    if incoming.recovery_epoch > stored.recovery_epoch
        && incoming.signer_sign_pk != person_root_sign_pk
    {
        return decision(
            RosterUpdateOutcome::Ignored,
            RosterUpdateReason::RecoveryEpochRequiresRoot,
            false,
        );
    }

    // DL-4: tombstones are forever, across versions as well as within one. A
    // roster that forgets a burial, or exhumes it, is not a later version of
    // this person's roster.
    let incoming_ids: Vec<Vec<u8>> = incoming.devices.iter().map(DeviceCert::device_id).collect();
    for tombstone in &stored.tombstones {
        let kept = incoming
            .tombstones
            .iter()
            .any(|later| later.device_id == tombstone.device_id);
        if !kept || incoming_ids.contains(&tombstone.device_id) {
            return decision(
                RosterUpdateOutcome::Ignored,
                RosterUpdateReason::TombstoneResurrected,
                false,
            );
        }
    }

    // §6: within one recovery epoch the inbox generation only ever climbs, so a
    // replayed pre-revocation generation cannot pull sealing back.
    if incoming.recovery_epoch == stored.recovery_epoch
        && incoming.inbox_key_generation < stored.inbox_key_generation
    {
        return decision(
            RosterUpdateOutcome::Ignored,
            RosterUpdateReason::InboxGenerationRegressed,
            false,
        );
    }

    decision(
        RosterUpdateOutcome::Accepted,
        RosterUpdateReason::Superseded,
        false,
    )
}

// ---------------------------------------------------------------------------
// Device cap (§14.3)
// ---------------------------------------------------------------------------

/// What adding a device does, given the count the roster would hold afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum DeviceAddOutcome {
    Added,
    /// Allowed, but the person is past the soft cap and should be told.
    AddedWithWarning,
    Refused,
}

/// §14.3, boundary as resolved on 2026-08-16: the count is the roster size
/// AFTER the add. Up to 8 is silent, the 9th warns, 16 is the last allowed, the
/// 17th is refused.
#[uniffi::export]
pub fn core_device_add_outcome(resulting_device_count: u32) -> DeviceAddOutcome {
    if resulting_device_count > DEVICE_HARD_CAP {
        DeviceAddOutcome::Refused
    } else if resulting_device_count > DEVICE_SOFT_CAP {
        DeviceAddOutcome::AddedWithWarning
    } else {
        DeviceAddOutcome::Added
    }
}

// ---------------------------------------------------------------------------
// This device's own fleet (§7, §9)
// ---------------------------------------------------------------------------

/// Which devices this person holds, and which one of them is *this* device.
///
/// Deliberately not a second copy of the own [`Roster`]: it is the projection
/// of one that routing and acks read, and nothing more. Two rules need exactly
/// these two fields and nothing else in the document:
///
/// * ACK-MD-1 — "a device acks only rows addressed to its own
///   `device_fanout_msg_id` namespace" — needs to know which namespace is its
///   own, which takes `own_device_id`.
/// * ACK-MD-2 — "a multi-device recipient NEVER acks a legacy person-addressed
///   row" — needs to know whether there is anyone to leave it for, which takes
///   the count.
///
/// §9's two-phase activation is what writes it: a new device may not
/// "advertise, author, or ack ANYTHING" until it has imported the bootstrap and
/// confirmed the roster back to the approving device, and this record is how
/// the ack planner is told that happened. §10's revocation rewrites it. Until
/// either ceremony exists (WP3/WP5), every install reads
/// [`OwnDeviceFleet::default`] — no own device id, no siblings — which is §5's
/// synthetic one-device person and behaves exactly as today's fleet does.
///
/// It rides a `.cmbak` unsanitized, deliberately: restoring a backup is §9's
/// "Replace this device", and a replacement really is the same device with the
/// same id and the same siblings. The other restore branch — "Link as new
/// device" — mints a fresh device key and must therefore overwrite this record
/// as part of activating, which it does by construction, since activation is
/// what writes it. WP3 owns that; the note is here so it cannot be missed.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnDeviceFleet {
    /// This device's own device id, or `None` on an install that has never
    /// linked — which is every device in the field today.
    pub own_device_id: Option<Vec<u8>>,
    /// Every active device id of this person, this device included, in roster
    /// order. Revoked devices are absent: DL-4 keeps tombstones in the roster
    /// document, but a tombstoned device has no rows worth fetching or
    /// withholding on its behalf.
    pub device_ids: Vec<Vec<u8>>,
    /// The `(recovery_epoch, seq)` of the OWN roster this projection was taken
    /// from — DL-1's ordering key, applied to the projection as well as to the
    /// document.
    ///
    /// [`MessageStore::set_own_device_fleet`](crate::MessageStore::set_own_device_fleet)
    /// refuses a projection at or below the stored version, for DL-1's reason
    /// one level down: this record decides which relay rows this device may
    /// delete and which siblings it must withhold acks for, so a *stale* copy
    /// of it is not a harmless out-of-date cache. The concrete hazard is a
    /// backup restore: a `.cmbak` carries this record unsanitized (§9's
    /// "Replace this device"), and without an ordering key a restore of an old
    /// backup could resurrect a fleet that has since been narrowed by §10's
    /// revocation — reinstating a revoked sibling as a device whose mail this
    /// one keeps politely refusing to ack, forever.
    ///
    /// `RosterVersion::default()` — `(0, 0)` — is the genesis version, so the
    /// FIRST write on an install with nothing stored is always accepted; only a
    /// second write has a baseline to be judged against.
    pub projected_from: RosterVersion,
}

/// The shapes an [`OwnDeviceFleet`] may take, refused rather than normalized.
///
/// The refusals matter more than they look: this record is what tells the ack
/// planner whose rows it may delete, so a fleet that is internally inconsistent
/// must never be storable. In particular a fleet that names siblings but not
/// *itself* is refused — a device that cannot say which device it is could not
/// tell its own rows from a sibling's, and ACK-MD-1 would have nothing to name.
pub(crate) fn validate_own_device_fleet(fleet: &OwnDeviceFleet) -> Result<(), CoreError> {
    let invalid = |detail: &str| Err(CoreError::Store(format!("own device fleet {detail}")));
    for device_id in &fleet.device_ids {
        if device_id.len() != DEVICE_ID_LEN || device_id[..] == LEGACY_DEVICE_ID[..] {
            return invalid("lists a device id that is not a real device id");
        }
    }
    let mut unique = fleet.device_ids.clone();
    unique.sort();
    unique.dedup();
    if unique.len() != fleet.device_ids.len() {
        return invalid("lists the same device twice");
    }
    // §14.3 through the one function that owns the boundary, exactly as
    // [`validate`] applies it to a roster document.
    if core_device_add_outcome(fleet.device_ids.len() as u32) == DeviceAddOutcome::Refused {
        return invalid("holds more devices than the hard cap");
    }
    match &fleet.own_device_id {
        Some(own_device_id) => {
            if !fleet.device_ids.contains(own_device_id) {
                return invalid("does not list this device among its own devices");
            }
        }
        None if !fleet.device_ids.is_empty() => {
            return invalid("names siblings without naming which device is this one");
        }
        None => {}
    }
    Ok(())
}

/// What a peer presenting this person's own identity actually is.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreOwnIdentityPeer {
    /// A second phone running this person's identity that this person did not
    /// link: the `.cmbak` clone of §1, two devices signing one author stream.
    /// Worth interrupting someone over.
    Clone,
    /// A device this person's own roster lists. Deliberately linked, expected
    /// to be here, and NOT worth a warning — §6 makes the inbox key
    /// person-scoped, so a sibling legitimately holds the very key the clone
    /// guard was built to recognise.
    Sibling,
}

/// Classify a peer that just presented this person's own identity
/// (`specs/multi-device-v1.md` §1, §6).
///
/// The clone guard predates linking, and its whole test was "does this peer
/// hold my agreement key". That was a sound proxy while a person was a device.
/// It stops being one the moment a person has two: a sibling holds the
/// person-scoped inbox key by design, so the guard would greet every deliberate
/// link with "another phone is using your backup" — the most alarming sentence
/// the app can say, about the thing the person just did on purpose. A warning
/// that fires on the normal case is a warning people learn to dismiss, and then
/// it is not there for the real clone either.
///
/// `peer_device_id` is what separates the two, and there is no substitute for
/// it: the keys are identical by construction. `None` means the transport could
/// not tell which device it was talking to, and the verdict is
/// [`CoreOwnIdentityPeer::Clone`] — fail loud, because an unidentified peer
/// holding this person's identity is exactly the situation the guard exists
/// for, and a person told about a sibling once is better served than a person
/// never told about a clone.
///
/// WP4's own-device sync records are what will put a device id on this wire.
/// Until then the shells pass `None` and the guard behaves precisely as it does
/// today; the rule is implemented and pinned here so the day a device id
/// arrives, the answer is already right.
#[uniffi::export]
pub fn core_own_identity_peer(
    fleet: OwnDeviceFleet,
    peer_device_id: Option<Vec<u8>>,
) -> CoreOwnIdentityPeer {
    let Some(peer_device_id) = peer_device_id else {
        return CoreOwnIdentityPeer::Clone;
    };
    // A peer claiming to be THIS device is not a sibling; it is the clone case
    // in its purest form, and the fleet listing that id says nothing else.
    if fleet.own_device_id.as_ref() == Some(&peer_device_id) {
        return CoreOwnIdentityPeer::Clone;
    }
    if fleet.device_ids.contains(&peer_device_id) {
        return CoreOwnIdentityPeer::Sibling;
    }
    CoreOwnIdentityPeer::Clone
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{parse_friend_card, FriendCard};

    /// Fixed keys, never `generate_*`: the golden vectors below are only worth
    /// anything if every byte that feeds them is pinned here.
    const ROOT_SK: [u8; 32] = [0x11; 32];
    const DEVICE_A_SK: [u8; 32] = [0x22; 32];
    const DEVICE_B_SK: [u8; 32] = [0x33; 32];
    const DEVICE_A_AGREE_PK: [u8; 32] = [0x44; 32];
    const DEVICE_B_AGREE_PK: [u8; 32] = [0x55; 32];
    const STRANGER_SK: [u8; 32] = [0x66; 32];

    fn sign_pk(sk: &[u8; 32]) -> Vec<u8> {
        SigningKey::from_bytes(sk)
            .verifying_key()
            .as_bytes()
            .to_vec()
    }

    fn person_id() -> Vec<u8> {
        derive_user_id(&sign_pk(&ROOT_SK)).to_vec()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn cert(
        device_sk: &[u8; 32],
        agree_pk: &[u8; 32],
        flags: u32,
        signer_sk: &[u8; 32],
    ) -> DeviceCert {
        core_sign_device_cert(
            DeviceCert {
                person_id: person_id(),
                device_sign_pk: sign_pk(device_sk),
                device_agree_pk: agree_pk.to_vec(),
                added_epoch: 1,
                flags,
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
            },
            signer_sk.to_vec(),
        )
        .expect("fixed-key cert signs")
    }

    /// The approving device (A) alone, root-signed.
    fn approving_cert() -> DeviceCert {
        cert(
            &DEVICE_A_SK,
            &DEVICE_A_AGREE_PK,
            DEVICE_CERT_FLAG_ROSTER_SIGNING,
            &ROOT_SK,
        )
    }

    /// A second device (B), certified by the approving device.
    fn sibling_cert() -> DeviceCert {
        cert(&DEVICE_B_SK, &DEVICE_B_AGREE_PK, 0, &DEVICE_A_SK)
    }

    fn unsigned_roster(
        recovery_epoch: u64,
        seq: u64,
        devices: Vec<DeviceCert>,
        tombstones: Vec<DeviceTombstone>,
    ) -> Roster {
        let approving_device_id = devices
            .iter()
            .find(|cert| cert.flags & DEVICE_CERT_FLAG_ROSTER_SIGNING != 0)
            .map(DeviceCert::device_id)
            .unwrap_or_default();
        Roster {
            person_id: person_id(),
            recovery_epoch,
            seq,
            devices,
            tombstones,
            approving_device_id,
            inbox_key_generation: 1,
            signer_sign_pk: Vec::new(),
            signature: Vec::new(),
        }
    }

    /// A roster at `(recovery_epoch, seq)` signed by the approving device.
    fn roster_at(recovery_epoch: u64, seq: u64) -> Roster {
        core_sign_roster(
            unsigned_roster(recovery_epoch, seq, vec![approving_cert()], Vec::new()),
            DEVICE_A_SK.to_vec(),
        )
        .expect("fixed-key roster signs")
    }

    /// The same version, different content: A alone vs A plus the sibling.
    fn forked_roster_at(recovery_epoch: u64, seq: u64) -> Roster {
        core_sign_roster(
            unsigned_roster(
                recovery_epoch,
                seq,
                vec![approving_cert(), sibling_cert()],
                Vec::new(),
            ),
            DEVICE_A_SK.to_vec(),
        )
        .expect("fixed-key roster signs")
    }

    fn accept(stored: Option<Roster>, quarantined: bool, incoming: Roster) -> RosterUpdateDecision {
        core_roster_accept(stored, quarantined, incoming, sign_pk(&ROOT_SK))
    }

    // -----------------------------------------------------------------------
    // Golden vectors: the new wire and signature formats, frozen
    // -----------------------------------------------------------------------

    /// Fixed-key golden vectors for every format this module introduces, in the
    /// style of `identity.rs`'s CMFRIEND2 vectors: the assertions are literal
    /// byte strings, so an accidental field-order, framing, or domain change
    /// fails here instead of quietly breaking every roster in the field.
    #[test]
    fn device_cert_and_roster_golden_vectors() {
        // Layout, in order: domain ‖ len(person_id)‖person_id ‖
        // len(device_sign_pk)‖device_sign_pk ‖ len(device_agree_pk)‖
        // device_agree_pk ‖ added_epoch ‖ flags ‖ len(signer)‖signer.
        const CERT_SIGNED_BYTES: &str = "4372756973654d65736820646576696365206365727469666963617465207631000000000000000010c0c5ecd7f1ee33f526dd27d34c3e1daa0000000000000020a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f0000000000000002044444444444444444444444444444444444444444444444444444444444444440000000000000001000000010000000000000020d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c9778737";
        const CERT_SIGNATURE: &str = "209755df19023c6622e38972314e986ce4dd6b90a96f73239ba276e78ab79becf8b3e8b7efcb215ffda887ebf724c62bdbccf6c72415cba995adfda213e28005";
        // Layout: domain ‖ len(person_id)‖person_id ‖ recovery_epoch ‖ seq ‖
        // device count ‖ len(cert body ‖ len(cert sig)‖cert sig) per device ‖
        // tombstone count ‖ (len(device_id)‖device_id ‖ revoked_at_seq)* ‖
        // len(approving_device_id)‖approving_device_id ‖ inbox_key_generation ‖
        // len(signer)‖signer.
        const ROSTER_SIGNED_BYTES: &str = "4372756973654d6573682064657669636520726f73746572207631000000000000000010c0c5ecd7f1ee33f526dd27d34c3e1daa00000000000000010000000000000001000000000000000100000000000000e40000000000000010c0c5ecd7f1ee33f526dd27d34c3e1daa0000000000000020a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f0000000000000002044444444444444444444444444444444444444444444444444444444444444440000000000000001000000010000000000000020d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c97787370000000000000040209755df19023c6622e38972314e986ce4dd6b90a96f73239ba276e78ab79becf8b3e8b7efcb215ffda887ebf724c62bdbccf6c72415cba995adfda213e28005000000000000000000000000000000104d3ae03a986747b7644cbf85c243495100000000000000010000000000000020a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f0";
        const ROSTER_SIGNATURE: &str = "d95f84ac165f8ca3c6488e7691d6ad116eb4be7b416d51b7078881dcc777071dffca09048e1278ed8402fd9ad17db6648e60fa89020cd4526e442e9a69ae1b04";
        const ROSTER_HEAD: &str =
            "9912c1e31a3c11e822f09590c815d05ffecd92ffe165614623f13f4256295cc7";

        let cert = approving_cert();
        assert_eq!(hex(&device_cert_signed_bytes(&cert)), CERT_SIGNED_BYTES);
        assert_eq!(hex(&cert.signature), CERT_SIGNATURE);
        core_verify_device_cert(cert).expect("golden cert verifies");

        let roster = roster_at(1, 1);
        assert_eq!(hex(&roster_signed_bytes(&roster)), ROSTER_SIGNED_BYTES);
        assert_eq!(hex(&roster.signature), ROSTER_SIGNATURE);
        assert_eq!(hex(&core_roster_head_hash(roster.clone())), ROSTER_HEAD);
        assert_eq!(core_roster_validate(roster, sign_pk(&ROOT_SK)), None);
    }

    /// The transferable roster document (DL-3), frozen the same way.
    ///
    /// Separate from `ROSTER_SIGNED_BYTES` above on purpose: that is the
    /// signature pre-image and this is the document, and the difference between
    /// them — a domain prefix, u64 framing versus u16, no signature versus a
    /// trailing one — is exactly the kind of thing that goes wrong silently if
    /// only one of the two is pinned.
    #[test]
    fn roster_wire_golden_vector() {
        const ROSTER_WIRE: &str = "010010c0c5ecd7f1ee33f526dd27d34c3e1daa0000000000000001000000000000000100010010c0c5ecd7f1ee33f526dd27d34c3e1daa0020a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f0002044444444444444444444444444444444444444444444444444444444444444440000000000000001000000010020d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c97787370040209755df19023c6622e38972314e986ce4dd6b90a96f73239ba276e78ab79becf8b3e8b7efcb215ffda887ebf724c62bdbccf6c72415cba995adfda213e28005000000104d3ae03a986747b7644cbf85c243495100000000000000010020a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f00040d95f84ac165f8ca3c6488e7691d6ad116eb4be7b416d51b7078881dcc777071dffca09048e1278ed8402fd9ad17db6648e60fa89020cd4526e442e9a69ae1b04";

        let roster = roster_at(1, 1);
        assert_eq!(
            hex(&core_encode_roster(roster.clone()).expect("encodes")),
            ROSTER_WIRE
        );
        // The decoded document is the same document, signature and all, so a
        // receiver validates exactly what its signer signed.
        let decoded = core_decode_roster(core_encode_roster(roster.clone()).expect("encodes"))
            .expect("decodes");
        assert_eq!(decoded, roster);
        assert_eq!(core_roster_validate(decoded, sign_pk(&ROOT_SK)), None);
    }

    /// A roster with tombstones and a second device round-trips too, and the
    /// codec refuses the two shapes a decoder must never guess at.
    #[test]
    fn roster_wire_round_trips_and_refuses_malformed_bytes() {
        let mut roster = unsigned_roster(
            2,
            9,
            vec![approving_cert(), sibling_cert()],
            vec![DeviceTombstone {
                device_id: vec![0x7Au8; DEVICE_ID_LEN],
                revoked_at_seq: 4,
            }],
        );
        roster = core_sign_roster(roster, DEVICE_A_SK.to_vec()).expect("signs");
        let encoded = core_encode_roster(roster.clone()).expect("encodes");
        assert_eq!(
            core_decode_roster(encoded.clone()).expect("decodes"),
            roster
        );

        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(matches!(
            core_decode_roster(trailing),
            Err(CoreError::Malformed(_))
        ));
        assert!(matches!(
            core_decode_roster(encoded[..encoded.len() - 1].to_vec()),
            Err(CoreError::Malformed(_))
        ));
        // A lying device count must not be able to make the decoder allocate
        // before it has read a single certificate. The count sits right after
        // version(1) ‖ len(person_id)(2) ‖ person_id(16) ‖ recovery_epoch(8) ‖
        // seq(8).
        let mut lying = encoded;
        lying[35] = 0xFF;
        lying[36] = 0xFF;
        assert!(matches!(
            core_decode_roster(lying),
            Err(CoreError::Malformed(_))
        ));
    }

    /// The two raw-signing domains WP4 and the authoring path will use, pinned
    /// the same way.
    #[test]
    fn raw_domain_signature_golden_vectors() {
        const AUTHORING: &str = "16f7f33bf801dd62c422aa1f92ea85d16afec69d8ce8ca130cec6cd62a2003bbb74e9d1c0280a0555786ec29d9fa18d374ef3c54e98d7bface53dc5e34fcb108";
        const SYNC: &str = "02d23aa5bb4d46cb3d3393016dc0f7554f2497f01740d635f7d76c6b07a400778fb803f38f0d18d5f5858e1db9d410f9efff949cafb710abf2c8ec827e5c8300";

        let message = b"stream-fixture".to_vec();
        let authoring = core_device_sign(
            DeviceSigningDomain::MessageAuthoring,
            DEVICE_A_SK.to_vec(),
            message.clone(),
        )
        .expect("authoring signature");
        let sync = core_device_sign(
            DeviceSigningDomain::SyncRecord,
            DEVICE_A_SK.to_vec(),
            message.clone(),
        )
        .expect("sync signature");

        assert_eq!(hex(&authoring), AUTHORING);
        assert_eq!(hex(&sync), SYNC);
        core_device_verify(
            DeviceSigningDomain::MessageAuthoring,
            sign_pk(&DEVICE_A_SK),
            message.clone(),
            authoring.clone(),
        )
        .expect("authoring signature verifies in its own domain");
        core_device_verify(
            DeviceSigningDomain::SyncRecord,
            sign_pk(&DEVICE_A_SK),
            message,
            sync,
        )
        .expect("sync signature verifies in its own domain");
    }

    // -----------------------------------------------------------------------
    // Domain separation (§3)
    // -----------------------------------------------------------------------

    #[test]
    fn every_signing_domain_is_distinct_and_prefixes_its_signed_bytes() {
        let domains: [&[u8]; 9] = [
            DEVICE_CERT_SIGN_DOMAIN,
            ROSTER_SIGN_DOMAIN,
            MESSAGE_AUTHORING_SIGN_DOMAIN,
            SYNC_RECORD_SIGN_DOMAIN,
            DEVICE_LINK_ACTIVATION_SIGN_DOMAIN,
            DEVICE_LINK_BOOTSTRAP_SIGN_DOMAIN,
            ROSTER_HEAD_HASH_DOMAIN,
            // The pre-existing identity domains this must never collide with.
            b"CruiseMesh friend card self-signature v1\0",
            b"CruiseMesh shared contact v1\0",
        ];
        for (i, a) in domains.iter().enumerate() {
            for b in domains.iter().skip(i + 1) {
                assert_ne!(a, b, "signing domains must be distinct");
                assert!(
                    !a.starts_with(b) && !b.starts_with(a),
                    "no domain may be a prefix of another"
                );
            }
        }

        assert!(device_cert_signed_bytes(&approving_cert()).starts_with(DEVICE_CERT_SIGN_DOMAIN));
        assert!(roster_signed_bytes(&roster_at(1, 1)).starts_with(ROSTER_SIGN_DOMAIN));
        assert!(
            domain_signed_bytes(DeviceSigningDomain::MessageAuthoring, b"x")
                .starts_with(MESSAGE_AUTHORING_SIGN_DOMAIN)
        );
        assert!(domain_signed_bytes(DeviceSigningDomain::SyncRecord, b"x")
            .starts_with(SYNC_RECORD_SIGN_DOMAIN));
    }

    #[test]
    fn a_signature_from_one_domain_never_verifies_in_another() {
        let message = b"replay me".to_vec();
        let authored = core_device_sign(
            DeviceSigningDomain::MessageAuthoring,
            DEVICE_A_SK.to_vec(),
            message.clone(),
        )
        .expect("authoring signature");

        for domain in [
            DeviceSigningDomain::DeviceCert,
            DeviceSigningDomain::RosterUpdate,
            DeviceSigningDomain::SyncRecord,
            DeviceSigningDomain::DeviceLinkActivation,
            DeviceSigningDomain::DeviceLinkBootstrap,
        ] {
            assert!(matches!(
                core_device_verify(
                    domain,
                    sign_pk(&DEVICE_A_SK),
                    message.clone(),
                    authored.clone()
                ),
                Err(CoreError::SignatureInvalid)
            ));
        }
    }

    #[test]
    fn length_framing_stops_message_boundary_collisions() {
        let a = domain_signed_bytes(DeviceSigningDomain::SyncRecord, b"ab");
        let b = domain_signed_bytes(DeviceSigningDomain::SyncRecord, b"abc");
        assert_ne!(a, b);
        assert!(!b.starts_with(&a[..]));
    }

    // -----------------------------------------------------------------------
    // Device keys and the legacy stream (§5)
    // -----------------------------------------------------------------------

    #[test]
    fn generated_device_keys_are_distinct_and_derive_their_id() {
        let one = generate_device_keypair();
        let two = generate_device_keypair();
        assert_ne!(one.sign_pk, two.sign_pk);
        assert_ne!(one.agree_pk, two.agree_pk);
        assert_eq!(one.device_id.len(), DEVICE_ID_LEN);
        assert_eq!(
            core_derive_device_id(one.sign_pk.clone()).expect("derives"),
            one.device_id
        );
        assert_ne!(one.device_id, LEGACY_DEVICE_ID.to_vec());
    }

    #[test]
    fn a_device_keypair_never_carries_the_person_root_secret() {
        // §3: the root secret lives only in the encrypted backup. The device
        // keypair type has no field it could ride in, and the generated device
        // key is its own key, not a copy of anything.
        let device = generate_device_keypair();
        assert_ne!(device.sign_sk, ROOT_SK.to_vec());
        assert_eq!(device.sign_sk.len(), KEY_LEN);
    }

    #[test]
    fn absent_device_field_maps_to_the_legacy_stream() {
        // §5 / MD-STREAM-LEGACY-ID: absence is the legacy stream, never an error.
        assert_eq!(core_legacy_device_id(), LEGACY_DEVICE_ID.to_vec());
        assert_eq!(core_legacy_device_id().len(), DEVICE_ID_LEN);
        assert_eq!(core_device_stream_id(None), LEGACY_DEVICE_ID.to_vec());
        // A malformed device field is treated as absent rather than rejected: a
        // legacy peer must never become undeliverable.
        assert_eq!(
            core_device_stream_id(Some(vec![1, 2, 3])),
            LEGACY_DEVICE_ID.to_vec()
        );
        let device = generate_device_keypair();
        assert_eq!(
            core_device_stream_id(Some(device.device_id.clone())),
            device.device_id
        );
    }

    /// §7: the routing namespace is a wire-visible derivation — two devices
    /// converge on the same relay row only if they agree on these bytes — so
    /// it is frozen by a fixed-key vector like every other derivation here.
    #[test]
    fn device_namespace_id_golden_vector() {
        let namespace = core_device_namespace_id(vec![0x01; 16], vec![0x02; 16]);
        assert_eq!(namespace.len(), DEVICE_ID_LEN);
        assert_eq!(hex(&namespace), "676f185b83cefc5c8e5af09ea06d430e");
    }

    #[test]
    fn device_namespaces_are_distinct_per_device_and_per_person() {
        let alice = person_id();
        let a = generate_device_keypair();
        let b = generate_device_keypair();
        assert_ne!(
            core_device_namespace_id(alice.clone(), a.device_id.clone()),
            core_device_namespace_id(alice.clone(), b.device_id.clone()),
        );
        // The same physical device id under a different person is a different
        // namespace: a namespace names a (person, device) pair, not a device.
        assert_ne!(
            core_device_namespace_id(alice.clone(), a.device_id.clone()),
            core_device_namespace_id(vec![0x09; 16], a.device_id.clone()),
        );
        // DL-5 / endpoint privacy: the namespace is one-way, so neither half
        // of the pair it was derived from can be read back out of it.
        let namespace = core_device_namespace_id(alice.clone(), a.device_id.clone());
        assert!(!namespace
            .windows(4)
            .any(|w| alice.starts_with(w) || a.device_id.starts_with(w)));
    }

    #[test]
    fn a_legacy_device_id_addresses_the_person_namespace() {
        // §5 / ACK-MD-2: legacy and malformed device fields resolve to the
        // person namespace, which is exactly today's hint input, so a v1
        // sender's single person-addressed row stays findable unchanged.
        let alice = person_id();
        assert_eq!(
            core_device_namespace_id(alice.clone(), LEGACY_DEVICE_ID.to_vec()),
            alice,
        );
        assert_eq!(core_device_namespace_id(alice.clone(), Vec::new()), alice);
        assert_eq!(
            core_device_namespace_id(alice.clone(), vec![0x02; 8]),
            alice,
        );
    }

    #[test]
    fn sibling_devices_of_one_person_have_distinct_stream_ids() {
        // §5 / MD-STREAM-SIBLING-LAMPORT: two devices of the same person are
        // two streams. The store side of this lands with the migration slice;
        // what WP1 owns is that the ids themselves never collide.
        let a = approving_cert().device_id();
        let b = sibling_cert().device_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), DEVICE_ID_LEN);
        assert_eq!(b.len(), DEVICE_ID_LEN);
    }

    // -----------------------------------------------------------------------
    // Certificates
    // -----------------------------------------------------------------------

    #[test]
    fn cert_round_trips_and_records_its_signer() {
        let cert = approving_cert();
        assert_eq!(cert.signer_sign_pk, sign_pk(&ROOT_SK));
        assert_eq!(cert.signature.len(), SIGNATURE_LEN);
        core_verify_device_cert(cert).expect("root-signed cert verifies");

        let by_device = sibling_cert();
        assert_eq!(by_device.signer_sign_pk, sign_pk(&DEVICE_A_SK));
        core_verify_device_cert(by_device).expect("device-signed cert verifies");
    }

    #[test]
    fn tampering_with_any_covered_cert_field_breaks_the_signature() {
        let base = approving_cert();
        let mut mutations = vec![base.clone(); 5];
        mutations[0].device_agree_pk = DEVICE_B_AGREE_PK.to_vec();
        mutations[1].added_epoch += 1;
        mutations[2].flags ^= DEVICE_CERT_FLAG_ROSTER_SIGNING;
        mutations[3].person_id = vec![0x99; DEVICE_ID_LEN];
        mutations[4].signer_sign_pk = sign_pk(&STRANGER_SK);
        for cert in mutations {
            assert!(
                matches!(
                    core_verify_device_cert(cert),
                    Err(CoreError::SignatureInvalid)
                ),
                "a covered field changed without the signature failing"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Validation
    // -----------------------------------------------------------------------

    #[test]
    fn a_well_formed_roster_validates_against_its_person_root() {
        assert_eq!(
            core_roster_validate(roster_at(1, 1), sign_pk(&ROOT_SK)),
            None
        );
        assert_eq!(
            core_roster_validate(forked_roster_at(1, 2), sign_pk(&ROOT_SK)),
            None
        );
    }

    #[test]
    fn genesis_must_be_root_signed() {
        // §3: seq 0 is roster genesis, and only the person root can mint it --
        // this is the anchor every later chain terminates at.
        let device_signed = core_sign_roster(
            unsigned_roster(1, 0, vec![approving_cert()], Vec::new()),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_validate(device_signed, sign_pk(&ROOT_SK)),
            Some(RosterRejection::GenesisNotRootSigned)
        );

        let root_signed = core_sign_roster(
            unsigned_roster(1, 0, vec![approving_cert()], Vec::new()),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(core_roster_validate(root_signed, sign_pk(&ROOT_SK)), None);
    }

    #[test]
    fn a_roster_for_another_person_is_rejected() {
        assert_eq!(
            core_roster_validate(roster_at(1, 1), sign_pk(&STRANGER_SK)),
            Some(RosterRejection::PersonMismatch)
        );
    }

    #[test]
    fn a_cert_signed_by_an_unvouched_key_breaks_the_chain() {
        // DL-1: version ordering is never sufficient. A stranger's signature on
        // a certificate is a broken chain even though every length is right.
        let stranger_cert = cert(&DEVICE_B_SK, &DEVICE_B_AGREE_PK, 0, &STRANGER_SK);
        let roster = core_sign_roster(
            unsigned_roster(1, 1, vec![approving_cert(), stranger_cert], Vec::new()),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_validate(roster, sign_pk(&ROOT_SK)),
            Some(RosterRejection::ChainBroken)
        );
    }

    #[test]
    fn a_cert_orphaned_by_a_revocation_must_be_re_signed() {
        // A revoked device keeps no certificate in the document (a tombstone
        // names an id and nothing else), so it is no longer a link the chain
        // can pass through. B's certificate, signed by the now-buried A, is an
        // orphan -- and an orphan is a broken chain, not a grandfathered one.
        let successor = |flags: u32| {
            core_sign_device_cert(
                DeviceCert {
                    person_id: person_id(),
                    device_sign_pk: sign_pk(&DEVICE_B_SK),
                    device_agree_pk: DEVICE_B_AGREE_PK.to_vec(),
                    added_epoch: 1,
                    flags,
                    signer_sign_pk: Vec::new(),
                    signature: Vec::new(),
                },
                DEVICE_A_SK.to_vec(),
            )
            .expect("signs")
        };
        let buried_a = vec![DeviceTombstone {
            device_id: approving_cert().device_id(),
            revoked_at_seq: 2,
        }];
        let orphaned = core_sign_roster(
            unsigned_roster(
                1,
                2,
                vec![successor(DEVICE_CERT_FLAG_ROSTER_SIGNING)],
                buried_a.clone(),
            ),
            DEVICE_B_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_validate(orphaned, sign_pk(&ROOT_SK)),
            Some(RosterRejection::ChainBroken)
        );

        // §10: revoking the approving device is the recovery-code path, and
        // the root re-issues the surviving certificate as it signs the new
        // epoch's genesis. That document chains.
        let recovered = core_sign_roster(
            unsigned_roster(
                2,
                0,
                vec![cert(
                    &DEVICE_B_SK,
                    &DEVICE_B_AGREE_PK,
                    DEVICE_CERT_FLAG_ROSTER_SIGNING,
                    &ROOT_SK,
                )],
                buried_a,
            ),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(core_roster_validate(recovered, sign_pk(&ROOT_SK)), None);
    }

    #[test]
    fn revoking_a_middle_device_orphans_what_it_signed_until_the_approver_re_signs() {
        // A (root-signed, approving) certifies B, B certifies C. Burying B
        // costs C its link to the root; §10.1's revocation step re-signs the
        // orphan with the approving device in the same roster update.
        const DEVICE_C_SK: [u8; 32] = [0x77; 32];
        const DEVICE_C_AGREE_PK: [u8; 32] = [0x88; 32];
        let b = sibling_cert();
        let c_via_b = cert(&DEVICE_C_SK, &DEVICE_C_AGREE_PK, 0, &DEVICE_B_SK);
        let three = core_sign_roster(
            unsigned_roster(
                1,
                1,
                vec![approving_cert(), b.clone(), c_via_b.clone()],
                Vec::new(),
            ),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(core_roster_validate(three, sign_pk(&ROOT_SK)), None);

        let buried_b = vec![DeviceTombstone {
            device_id: b.device_id(),
            revoked_at_seq: 2,
        }];
        let orphaned = core_sign_roster(
            unsigned_roster(1, 2, vec![approving_cert(), c_via_b], buried_b.clone()),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_validate(orphaned, sign_pk(&ROOT_SK)),
            Some(RosterRejection::ChainBroken)
        );

        let repaired = core_sign_roster(
            unsigned_roster(
                1,
                2,
                vec![
                    approving_cert(),
                    cert(&DEVICE_C_SK, &DEVICE_C_AGREE_PK, 0, &DEVICE_A_SK),
                ],
                buried_b,
            ),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(core_roster_validate(repaired, sign_pk(&ROOT_SK)), None);
    }

    #[test]
    fn certificates_may_not_vouch_for_themselves_or_for_each_other() {
        // The chain terminates at the root or it does not terminate. A
        // self-signed certificate, and a pair of certificates that sign each
        // other, are both fully well-formed documents whose signatures all
        // verify -- and neither can ever reach the seed.
        let self_signed = cert(
            &DEVICE_B_SK,
            &DEVICE_B_AGREE_PK,
            DEVICE_CERT_FLAG_ROSTER_SIGNING,
            &DEVICE_B_SK,
        );
        core_verify_device_cert(self_signed.clone()).expect("a self-signed cert still verifies");
        let alone = core_sign_roster(
            unsigned_roster(1, 1, vec![self_signed], Vec::new()),
            DEVICE_B_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_validate(alone, sign_pk(&ROOT_SK)),
            Some(RosterRejection::ChainBroken)
        );

        // The mutual-vouch pair: A's certificate signed by B, B's by A, with
        // the person root nowhere in the chain.
        let a_via_b = cert(
            &DEVICE_A_SK,
            &DEVICE_A_AGREE_PK,
            DEVICE_CERT_FLAG_ROSTER_SIGNING,
            &DEVICE_B_SK,
        );
        let b_via_a = cert(&DEVICE_B_SK, &DEVICE_B_AGREE_PK, 0, &DEVICE_A_SK);
        let cycle = core_sign_roster(
            unsigned_roster(1, 1, vec![a_via_b, b_via_a], Vec::new()),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_validate(cycle, sign_pk(&ROOT_SK)),
            Some(RosterRejection::ChainBroken)
        );
    }

    #[test]
    fn a_tombstoned_id_does_not_vouch_for_a_smuggled_certificate() {
        // The narrow hole the fixpoint closes: listing a tombstone for the key
        // that signed a smuggled certificate must not make that certificate
        // chain. Anyone can put any id in `tombstones`.
        let smuggled = cert(&DEVICE_B_SK, &DEVICE_B_AGREE_PK, 0, &STRANGER_SK);
        let roster = core_sign_roster(
            unsigned_roster(
                1,
                1,
                vec![approving_cert(), smuggled],
                vec![DeviceTombstone {
                    device_id: derive_user_id(&sign_pk(&STRANGER_SK)).to_vec(),
                    revoked_at_seq: 1,
                }],
            ),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_validate(roster, sign_pk(&ROOT_SK)),
            Some(RosterRejection::ChainBroken)
        );
    }

    #[test]
    fn a_roster_signed_by_neither_root_nor_approver_is_rejected() {
        let roster = core_sign_roster(
            unsigned_roster(1, 1, vec![approving_cert(), sibling_cert()], Vec::new()),
            DEVICE_B_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_validate(roster, sign_pk(&ROOT_SK)),
            Some(RosterRejection::SignerNotAuthorized)
        );
    }

    #[test]
    fn the_roster_signing_role_sits_on_exactly_the_approving_device() {
        // Two flagged devices: §3's single authority cannot be held twice.
        let both = core_sign_roster(
            unsigned_roster(
                1,
                1,
                vec![
                    approving_cert(),
                    cert(
                        &DEVICE_B_SK,
                        &DEVICE_B_AGREE_PK,
                        DEVICE_CERT_FLAG_ROSTER_SIGNING,
                        &DEVICE_A_SK,
                    ),
                ],
                Vec::new(),
            ),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_validate(both, sign_pk(&ROOT_SK)),
            Some(RosterRejection::ApprovingRoleMismatch)
        );

        // approving_device_id naming a device that is not listed.
        let mut orphaned = roster_at(1, 1);
        orphaned.approving_device_id = sibling_cert().device_id();
        let orphaned = core_sign_roster(orphaned, ROOT_SK.to_vec()).expect("signs");
        assert_eq!(
            core_roster_validate(orphaned, sign_pk(&ROOT_SK)),
            Some(RosterRejection::ApprovingDeviceMissing)
        );
    }

    #[test]
    fn a_tombstoned_device_may_not_be_listed_active() {
        // DL-4 / MD-ROSTER-TOMBSTONE at (2, 1): the buried device_id itself can
        // never return to `devices`.
        let roster = core_sign_roster(
            unsigned_roster(
                2,
                1,
                vec![approving_cert(), sibling_cert()],
                vec![DeviceTombstone {
                    device_id: sibling_cert().device_id(),
                    revoked_at_seq: 1,
                }],
            ),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_validate(roster, sign_pk(&ROOT_SK)),
            Some(RosterRejection::TombstonedDeviceActive)
        );
    }

    #[test]
    fn a_relinked_device_mints_a_fresh_key_and_is_accepted() {
        // DL-4 / MD-ROSTER-RELINK-FRESH-KEY at (2, 2): the same hardware comes
        // back as a different key, so nothing is resurrected.
        let fresh = generate_device_keypair();
        let relinked = core_sign_device_cert(
            DeviceCert {
                person_id: person_id(),
                device_sign_pk: fresh.sign_pk.clone(),
                device_agree_pk: fresh.agree_pk.clone(),
                added_epoch: 2,
                flags: 0,
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
            },
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_ne!(relinked.device_id(), sibling_cert().device_id());

        let roster = core_sign_roster(
            unsigned_roster(
                2,
                2,
                vec![approving_cert(), relinked],
                vec![DeviceTombstone {
                    device_id: sibling_cert().device_id(),
                    revoked_at_seq: 1,
                }],
            ),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(core_roster_validate(roster, sign_pk(&ROOT_SK)), None);
    }

    #[test]
    fn the_legacy_stream_id_is_reserved_for_device_less_traffic() {
        // §5: no real device may claim the all-zero stream. No key derives
        // there in practice; [`RosterRejection::ReservedDeviceId`] is the
        // belt-and-braces refusal for a hand-built certificate that tries.
        assert_eq!(LEGACY_DEVICE_ID, [0u8; DEVICE_ID_LEN]);
        for _ in 0..64 {
            assert_ne!(
                generate_device_keypair().device_id,
                LEGACY_DEVICE_ID.to_vec()
            );
        }
    }

    #[test]
    fn malformed_lengths_are_refused_rather_than_interpreted() {
        let mut short_id = roster_at(1, 1);
        short_id.person_id = vec![0x01; 4];
        assert_eq!(
            core_roster_validate(short_id, sign_pk(&ROOT_SK)),
            Some(RosterRejection::MalformedField)
        );

        let mut short_key = roster_at(1, 1);
        short_key.devices[0].device_agree_pk = vec![0x01; 8];
        assert_eq!(
            core_roster_validate(short_key, sign_pk(&ROOT_SK)),
            Some(RosterRejection::MalformedField)
        );
    }

    #[test]
    fn a_tampered_roster_signature_is_refused() {
        let mut roster = roster_at(1, 1);
        roster.inbox_key_generation += 1;
        assert_eq!(
            core_roster_validate(roster, sign_pk(&ROOT_SK)),
            Some(RosterRejection::SignatureInvalid)
        );
    }

    #[test]
    fn a_roster_past_the_hard_cap_is_refused() {
        // §14.3: 16 devices validate, 17 do not.
        let mut devices = vec![approving_cert()];
        while devices.len() < DEVICE_HARD_CAP as usize {
            let key = generate_device_keypair();
            devices.push(
                core_sign_device_cert(
                    DeviceCert {
                        person_id: person_id(),
                        device_sign_pk: key.sign_pk,
                        device_agree_pk: key.agree_pk,
                        added_epoch: 1,
                        flags: 0,
                        signer_sign_pk: Vec::new(),
                        signature: Vec::new(),
                    },
                    DEVICE_A_SK.to_vec(),
                )
                .expect("signs"),
            );
        }
        let full = core_sign_roster(
            unsigned_roster(1, 1, devices.clone(), Vec::new()),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(core_roster_validate(full, sign_pk(&ROOT_SK)), None);

        let key = generate_device_keypair();
        devices.push(
            core_sign_device_cert(
                DeviceCert {
                    person_id: person_id(),
                    device_sign_pk: key.sign_pk,
                    device_agree_pk: key.agree_pk,
                    added_epoch: 1,
                    flags: 0,
                    signer_sign_pk: Vec::new(),
                    signature: Vec::new(),
                },
                DEVICE_A_SK.to_vec(),
            )
            .expect("signs"),
        );
        let over = core_sign_roster(
            unsigned_roster(1, 1, devices, Vec::new()),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_validate(over, sign_pk(&ROOT_SK)),
            Some(RosterRejection::DeviceCapExceeded)
        );
    }

    /// The shell stores these bytes and hands them back a reboot later; a
    /// round trip that quietly lost a field would cost a device its identity.
    #[test]
    fn a_device_keypair_survives_the_shell_storing_it() {
        let device = generate_device_keypair();
        let bytes = core_encode_device_keypair(device.clone()).expect("encodes");
        assert_eq!(bytes.len(), DEVICE_KEYPAIR_LEN);
        assert_eq!(core_decode_device_keypair(bytes.clone()).unwrap(), device);

        // A blob whose id does not follow from its own signing key is corrupt,
        // not a device this build should sign anything with.
        let mut tampered = bytes.clone();
        tampered[0] ^= 0x01;
        assert!(core_decode_device_keypair(tampered).is_err());
        assert!(core_decode_device_keypair(bytes[..DEVICE_KEYPAIR_LEN - 1].to_vec()).is_err());
        assert!(core_decode_device_keypair(Vec::new()).is_err());

        let mut short = device.clone();
        short.agree_sk.pop();
        assert!(core_encode_device_keypair(short).is_err());
    }

    // -----------------------------------------------------------------------
    // DL-1 / DL-2 acceptance, mirroring the WP0 vector scenarios
    // -----------------------------------------------------------------------

    #[test]
    fn dl1_a_higher_recovery_epoch_supersedes_a_higher_seq() {
        // MD-ROSTER-GREATER: stored (1, 1000), incoming (2, 0). The epoch wins
        // even though the seq resets -- and the incoming must be root-signed,
        // because only the recovery material may raise the epoch (§14.2).
        let stored = roster_at(1, 1000);
        let incoming = core_sign_roster(
            unsigned_roster(2, 0, vec![approving_cert()], Vec::new()),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        let decision = accept(Some(stored), false, incoming);
        assert_eq!(decision.outcome, RosterUpdateOutcome::Accepted);
        assert_eq!(decision.reason, RosterUpdateReason::Superseded);
        assert!(!decision.quarantined);
    }

    #[test]
    fn dl1_a_lower_version_never_rolls_back() {
        // MD-ROSTER-LOWER: stored (2, 0), incoming (1, 1001).
        let stored = core_sign_roster(
            unsigned_roster(2, 0, vec![approving_cert()], Vec::new()),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        let decision = accept(Some(stored), false, roster_at(1, 1001));
        assert_eq!(decision.outcome, RosterUpdateOutcome::Ignored);
        assert_eq!(decision.reason, RosterUpdateReason::Rollback);
    }

    #[test]
    fn dl1_an_identical_repeat_is_idempotent_gossip() {
        // MD-ROSTER-EQUAL: stored (2, 0), incoming (2, 0), same document.
        let stored = core_sign_roster(
            unsigned_roster(2, 0, vec![approving_cert()], Vec::new()),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        let decision = accept(Some(stored.clone()), false, stored);
        assert_eq!(decision.outcome, RosterUpdateOutcome::Ignored);
        assert_eq!(decision.reason, RosterUpdateReason::IdempotentRepeat);
        assert!(!decision.quarantined);
    }

    #[test]
    fn dl1_a_higher_seq_within_one_epoch_is_the_ordinary_update() {
        // MD-ROSTER-SAME-EPOCH-ADVANCE: (2, 0) -> (2, 1), approving-signed.
        let stored = core_sign_roster(
            unsigned_roster(2, 0, vec![approving_cert()], Vec::new()),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        let decision = accept(Some(stored), false, roster_at(2, 1));
        assert_eq!(decision.outcome, RosterUpdateOutcome::Accepted);
        assert_eq!(decision.reason, RosterUpdateReason::Superseded);
    }

    #[test]
    fn dl1_a_lower_seq_within_one_epoch_is_ignored() {
        // MD-ROSTER-SAME-EPOCH-ROLLBACK: stored (2, 5), incoming (2, 3).
        let decision = accept(Some(roster_at(2, 5)), false, roster_at(2, 3));
        assert_eq!(decision.outcome, RosterUpdateOutcome::Ignored);
        assert_eq!(decision.reason, RosterUpdateReason::Rollback);
    }

    #[test]
    fn dl1_a_strictly_higher_roster_with_a_broken_chain_is_ignored() {
        // MD-ROSTER-CHAIN-BROKEN: stored (2, 0), incoming (3, 0) whose chain
        // does not verify back to the person root.
        let stored = core_sign_roster(
            unsigned_roster(2, 0, vec![approving_cert()], Vec::new()),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        // A stranger's certificate inside an otherwise perfectly signed roster.
        let smuggled = core_sign_roster(
            unsigned_roster(
                3,
                0,
                vec![
                    approving_cert(),
                    cert(&DEVICE_B_SK, &DEVICE_B_AGREE_PK, 0, &STRANGER_SK),
                ],
                Vec::new(),
            ),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        let decision = accept(Some(stored.clone()), false, smuggled);
        assert_eq!(decision.outcome, RosterUpdateOutcome::Ignored);
        assert_eq!(decision.reason, RosterUpdateReason::Invalid);
        assert_eq!(decision.rejection, Some(RosterRejection::ChainBroken));

        // And the whole document signed by a key the person never vouched for.
        let forged = core_sign_roster(
            unsigned_roster(3, 0, vec![approving_cert()], Vec::new()),
            STRANGER_SK.to_vec(),
        )
        .expect("signs");
        let decision = accept(Some(stored), false, forged);
        assert_eq!(decision.outcome, RosterUpdateOutcome::Ignored);
        assert_eq!(decision.reason, RosterUpdateReason::Invalid);
        assert_eq!(
            decision.rejection,
            Some(RosterRejection::SignerNotAuthorized)
        );
    }

    #[test]
    fn dl2_equal_versions_with_different_content_fork() {
        // MD-ROSTER-FORK at (2, 0): both sides verify, the versions match, the
        // documents differ. Keep the stored one; quarantine the person.
        let stored = roster_at(2, 0);
        let stored = core_sign_roster(stored, ROOT_SK.to_vec()).expect("signs");
        let incoming = core_sign_roster(
            unsigned_roster(2, 0, vec![approving_cert(), sibling_cert()], Vec::new()),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        assert_ne!(
            core_roster_head_hash(stored.clone()),
            core_roster_head_hash(incoming.clone())
        );
        let decision = accept(Some(stored), false, incoming);
        assert_eq!(decision.outcome, RosterUpdateOutcome::ForkQuarantined);
        assert_eq!(decision.reason, RosterUpdateReason::ForkedContent);
        assert!(decision.quarantined);
    }

    #[test]
    fn dl2_quarantine_survives_a_legitimately_higher_roster() {
        // MD-ROSTER-FORK-QUARANTINE-PERSISTS: quarantined at (2, 0), a good
        // (2, 1) arrives. A later good version is not a resolution.
        let stored = core_sign_roster(
            unsigned_roster(2, 0, vec![approving_cert()], Vec::new()),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        let higher = roster_at(2, 1);
        assert_eq!(
            core_roster_validate(higher.clone(), sign_pk(&ROOT_SK)),
            None
        );
        let decision = accept(Some(stored), true, higher);
        assert_eq!(decision.outcome, RosterUpdateOutcome::ForkQuarantined);
        assert_eq!(decision.reason, RosterUpdateReason::PersonQuarantined);
        assert!(
            decision.quarantined,
            "DL-2 quarantine is never auto-resolved"
        );
    }

    #[test]
    fn the_first_roster_for_a_person_is_accepted_on_its_own_merits() {
        let decision = accept(None, false, roster_at(2, 3));
        assert_eq!(decision.outcome, RosterUpdateOutcome::Accepted);
        assert_eq!(decision.reason, RosterUpdateReason::FirstRoster);

        let forged = core_sign_roster(
            unsigned_roster(2, 3, vec![approving_cert()], Vec::new()),
            STRANGER_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            accept(None, false, forged).outcome,
            RosterUpdateOutcome::Ignored
        );
    }

    #[test]
    fn only_the_recovery_material_may_raise_the_recovery_epoch() {
        // MD-RECOVERY-BACKUP-AUTHORITY at (3, 0): the approving device's key
        // alone cannot mint a higher epoch -- the root secret lives in the
        // encrypted backup (§14.2), and that is the whole dethroning story.
        let stored = core_sign_roster(
            unsigned_roster(2, 0, vec![approving_cert()], Vec::new()),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        // At seq 0 -- the shape a recovery roster actually takes, since a new
        // epoch resets the seq -- the genesis rule already refuses a
        // device-signed document.
        let device_minted = core_sign_roster(
            unsigned_roster(3, 0, vec![approving_cert()], Vec::new()),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        let decision = accept(Some(stored.clone()), false, device_minted);
        assert_eq!(decision.outcome, RosterUpdateOutcome::Ignored);
        assert_eq!(
            decision.rejection,
            Some(RosterRejection::GenesisNotRootSigned)
        );

        // And past seq 0, where the genesis rule no longer applies, the epoch
        // itself is what the approving device may not raise.
        let device_minted = core_sign_roster(
            unsigned_roster(3, 1, vec![approving_cert()], Vec::new()),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        let decision = accept(Some(stored.clone()), false, device_minted);
        assert_eq!(decision.outcome, RosterUpdateOutcome::Ignored);
        assert_eq!(
            decision.reason,
            RosterUpdateReason::RecoveryEpochRequiresRoot
        );

        let recovered = core_sign_roster(
            unsigned_roster(3, 0, vec![approving_cert()], Vec::new()),
            ROOT_SK.to_vec(),
        )
        .expect("signs");
        let decision = accept(Some(stored), false, recovered);
        assert_eq!(decision.outcome, RosterUpdateOutcome::Accepted);
        assert_eq!(decision.reason, RosterUpdateReason::Superseded);
    }

    #[test]
    fn a_later_roster_may_not_forget_or_exhume_a_tombstone() {
        // DL-4 across versions: the stored burial must still be there, and the
        // buried id must not be back among the active devices.
        let stored = core_sign_roster(
            unsigned_roster(
                2,
                1,
                vec![approving_cert()],
                vec![DeviceTombstone {
                    device_id: sibling_cert().device_id(),
                    revoked_at_seq: 1,
                }],
            ),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");

        let forgetful = roster_at(2, 2);
        let decision = accept(Some(stored.clone()), false, forgetful);
        assert_eq!(decision.outcome, RosterUpdateOutcome::Ignored);
        assert_eq!(decision.reason, RosterUpdateReason::TombstoneResurrected);

        let honest = core_sign_roster(
            unsigned_roster(
                2,
                2,
                vec![approving_cert()],
                vec![DeviceTombstone {
                    device_id: sibling_cert().device_id(),
                    revoked_at_seq: 1,
                }],
            ),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            accept(Some(stored), false, honest).outcome,
            RosterUpdateOutcome::Accepted
        );
    }

    #[test]
    fn the_inbox_generation_never_regresses_within_an_epoch() {
        // §6: a replayed pre-revocation generation must not pull sealing back.
        let mut stored = unsigned_roster(2, 1, vec![approving_cert()], Vec::new());
        stored.inbox_key_generation = 5;
        let stored = core_sign_roster(stored, DEVICE_A_SK.to_vec()).expect("signs");

        let mut regressed = unsigned_roster(2, 2, vec![approving_cert()], Vec::new());
        regressed.inbox_key_generation = 4;
        let regressed = core_sign_roster(regressed, DEVICE_A_SK.to_vec()).expect("signs");
        let decision = accept(Some(stored), false, regressed);
        assert_eq!(decision.outcome, RosterUpdateOutcome::Ignored);
        assert_eq!(
            decision.reason,
            RosterUpdateReason::InboxGenerationRegressed
        );
    }

    // -----------------------------------------------------------------------
    // DL-5, head hashing, and the device cap
    // -----------------------------------------------------------------------

    #[test]
    fn a_roster_carries_keys_and_never_endpoints() {
        // DL-5 structurally: every byte field of a roster and its certificates
        // is a fixed-length key or id, and validation enforces those lengths.
        // There is no free-form field an address could hide in, which is the
        // property the WP0 fixture asks WP1 to keep.
        let roster = forked_roster_at(2, 4);
        assert_eq!(roster.person_id.len(), DEVICE_ID_LEN);
        assert_eq!(roster.approving_device_id.len(), DEVICE_ID_LEN);
        assert_eq!(roster.signer_sign_pk.len(), KEY_LEN);
        assert_eq!(roster.signature.len(), SIGNATURE_LEN);
        for cert in &roster.devices {
            assert_eq!(cert.person_id.len(), DEVICE_ID_LEN);
            assert_eq!(cert.device_sign_pk.len(), KEY_LEN);
            assert_eq!(cert.device_agree_pk.len(), KEY_LEN);
            assert_eq!(cert.signer_sign_pk.len(), KEY_LEN);
            assert_eq!(cert.signature.len(), SIGNATURE_LEN);
        }
        // And a document whose "key" is an address-shaped blob does not
        // validate at all.
        let mut smuggled = roster.clone();
        smuggled.devices[1].device_agree_pk = b"192.168.1.42:8443".to_vec();
        assert_eq!(
            core_roster_validate(smuggled, sign_pk(&ROOT_SK)),
            Some(RosterRejection::MalformedField)
        );
    }

    #[test]
    fn the_roster_head_is_a_32_byte_digest_a_v4_card_can_carry() {
        // §12: this is the value `CMFRIEND4:` carries in
        // `FriendCard::roster_head_hash`.
        let head = core_roster_head_hash(roster_at(2, 3));
        assert_eq!(head.len(), ROSTER_HEAD_HASH_LEN);

        let card = FriendCard {
            name: "Dave".to_string(),
            sign_pk: sign_pk(&ROOT_SK),
            agree_pk: DEVICE_A_AGREE_PK.to_vec(),
            relay_url: None,
            relay_token: None,
            signature: None,
            roster_head_hash: Some(head.clone()),
        };
        let json = serde_json::to_string(&card).expect("card serializes");
        let parsed = parse_friend_card(json).expect("card parses");
        assert_eq!(parsed.roster_head_hash, Some(head));
    }

    #[test]
    fn the_head_names_content_and_changes_with_it() {
        assert_eq!(
            core_roster_head_hash(roster_at(2, 3)),
            core_roster_head_hash(roster_at(2, 3))
        );
        assert_ne!(
            core_roster_head_hash(roster_at(2, 3)),
            core_roster_head_hash(roster_at(2, 4))
        );
        assert_ne!(
            core_roster_head_hash(roster_at(2, 3)),
            core_roster_head_hash(forked_roster_at(2, 3))
        );
    }

    #[test]
    fn the_device_cap_warns_at_nine_and_refuses_at_seventeen() {
        // §14.3 / MD-DEVICE-CAP-7..17, counts being the roster size AFTER the
        // add: up to 8 silent, the 9th warns, 16 allowed, the 17th refused.
        assert_eq!(core_device_add_outcome(1), DeviceAddOutcome::Added);
        assert_eq!(core_device_add_outcome(7), DeviceAddOutcome::Added);
        assert_eq!(core_device_add_outcome(8), DeviceAddOutcome::Added);
        assert_eq!(
            core_device_add_outcome(9),
            DeviceAddOutcome::AddedWithWarning
        );
        assert_eq!(
            core_device_add_outcome(16),
            DeviceAddOutcome::AddedWithWarning
        );
        assert_eq!(core_device_add_outcome(17), DeviceAddOutcome::Refused);
        assert_eq!(DEVICE_SOFT_CAP, 8);
        assert_eq!(DEVICE_HARD_CAP, 16);
    }

    /// §7/§9: the fleet projection is what tells the ack planner whose relay
    /// rows this device may delete, so every shape that could make that
    /// question unanswerable is refused rather than normalized.
    #[test]
    fn an_inconsistent_own_fleet_is_refused() {
        let own = vec![0xA1; DEVICE_ID_LEN];
        let sibling = vec![0xB2; DEVICE_ID_LEN];
        let fleet = |own_device_id: Option<Vec<u8>>, device_ids: Vec<Vec<u8>>| OwnDeviceFleet {
            projected_from: RosterVersion::default(),
            own_device_id,
            device_ids,
        };

        // The install every device in the field is: no fleet at all.
        assert!(validate_own_device_fleet(&OwnDeviceFleet::default()).is_ok());
        assert!(validate_own_device_fleet(&fleet(
            Some(own.clone()),
            vec![own.clone(), sibling.clone()]
        ))
        .is_ok());

        // A device that cannot say which device it is could not tell its own
        // rows from a sibling's.
        assert!(validate_own_device_fleet(&fleet(None, vec![own.clone()])).is_err());
        // Nor one that is not in its own fleet.
        assert!(
            validate_own_device_fleet(&fleet(Some(own.clone()), vec![sibling.clone()])).is_err()
        );
        // The legacy id is a stream marker, never a device that holds rows.
        assert!(validate_own_device_fleet(
            &fleet(
                Some(LEGACY_DEVICE_ID.to_vec()),
                vec![LEGACY_DEVICE_ID.to_vec()]
            )
            .clone()
        )
        .is_err());
        assert!(validate_own_device_fleet(&fleet(
            Some(own.clone()),
            vec![own.clone(), own.clone()]
        ))
        .is_err());
        assert!(
            validate_own_device_fleet(&fleet(Some(vec![0xA1; 8]), vec![vec![0xA1; 8]])).is_err()
        );

        // §14.3's hard cap, through the same function the roster uses.
        let capped: Vec<Vec<u8>> = (0..DEVICE_HARD_CAP)
            .map(|i| vec![i as u8 + 1; DEVICE_ID_LEN])
            .collect();
        assert!(validate_own_device_fleet(&fleet(Some(capped[0].clone()), capped.clone())).is_ok());
        let mut over_cap = capped.clone();
        over_cap.push(vec![0xFF; DEVICE_ID_LEN]);
        assert!(validate_own_device_fleet(&fleet(Some(capped[0].clone()), over_cap)).is_err());
    }

    /// The clone guard must not fire on the thing the person just deliberately
    /// did — and must still fire on everything else.
    #[test]
    fn a_roster_listed_sibling_is_not_a_clone_and_a_stranger_still_is() {
        let own = vec![0x01; DEVICE_ID_LEN];
        let sibling = vec![0x02; DEVICE_ID_LEN];
        let stranger = vec![0x03; DEVICE_ID_LEN];
        let fleet = OwnDeviceFleet {
            own_device_id: Some(own.clone()),
            device_ids: vec![own.clone(), sibling.clone()],
            projected_from: RosterVersion::default(),
        };

        assert_eq!(
            core_own_identity_peer(fleet.clone(), Some(sibling)),
            CoreOwnIdentityPeer::Sibling,
            "a device this person's own roster lists is not a clone"
        );
        assert_eq!(
            core_own_identity_peer(fleet.clone(), Some(stranger.clone())),
            CoreOwnIdentityPeer::Clone,
            "a device the fleet does not list is exactly what the guard is for"
        );
        // A peer claiming to be this very device is the purest clone case.
        assert_eq!(
            core_own_identity_peer(fleet.clone(), Some(own)),
            CoreOwnIdentityPeer::Clone
        );
        // And an unidentified peer fails loud, not quiet.
        assert_eq!(
            core_own_identity_peer(fleet, None),
            CoreOwnIdentityPeer::Clone
        );
        // On an install that never linked, nothing is ever a sibling.
        assert_eq!(
            core_own_identity_peer(OwnDeviceFleet::default(), Some(stranger)),
            CoreOwnIdentityPeer::Clone
        );
    }

    #[test]
    fn unknown_cert_flags_survive_signing_and_validation() {
        // Reserved bits must round-trip so a later work package can assign one
        // without invalidating today's rosters.
        let reserved = DEVICE_CERT_FLAG_ROSTER_SIGNING | 1 << 9;
        let cert = cert(&DEVICE_A_SK, &DEVICE_A_AGREE_PK, reserved, &ROOT_SK);
        assert_eq!(cert.flags, reserved);
        let roster = core_sign_roster(
            unsigned_roster(1, 1, vec![cert], Vec::new()),
            DEVICE_A_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(core_roster_validate(roster, sign_pk(&ROOT_SK)), None);
    }
}
