//! Self-sync records: one person's devices as a private mesh
//! (`specs/multi-device-v1.md` §6, §8).
//!
//! §8 says a person's devices converge through signed **sync records** sealed
//! to own devices only — same envelope machinery, same four transports, new
//! sealed kinds. This module owns the records themselves: their kinds, their
//! canonical bytes, the person-scoped inbox key they are sealed to, and the
//! gate that decides whether an opened record may be believed. It is pure: no
//! store, no transport, no clock. Planning *which* records to send, and getting
//! them onto a wire, belong to the layers above.
//!
//! ## SYNC-3 is enforced, not documented
//!
//! "Sealed strictly to the person's own current device set, re-sealed on roster
//! change" is the invariant, and it is worth being exact about which mechanism
//! carries which half of it, because a rule that rests on a comment is a rule
//! that regresses:
//!
//! * **Sealing is possession-gated.** [`core_seal_sync_record`] takes an
//!   [`InboxKey`] — the *whole* key, secret included — and refuses one whose
//!   secret does not match its public half. §6 gives the inbox key to the
//!   person's linked devices and to nobody else, so a caller cannot address a
//!   sync record at a contact even by mistake: there is no parameter that
//!   accepts a bare public key, and a contact's public key is not one this
//!   device holds the secret for. That is the structural half of the boundary.
//!   [`core_seal_sync_handoff`] — §10.1's rotation announcement, the one record
//!   that cannot be addressed to any inbox generation — keeps the same property
//!   by a different route: it takes a device *id* and looks the agreement key up
//!   in the person's own roster, so its reachable addresses are exactly this
//!   person's active devices and nothing else.
//! * **Opening is roster-gated.** [`core_open_sync_record`] needs the inbox
//!   secret to decrypt at all, then re-checks three independent facts against
//!   the *own* roster: the record names this person, it was sealed under the
//!   generation this key actually is, and its author is an active — not
//!   tombstoned, not merely plausible — device of this person. A record that
//!   passes the crypto and fails the roster is refused, which is what keeps a
//!   revoked device from re-entering through a key it still remembers (§10.3).
//! * **Re-seal on roster change is legible.** Every record commits to the
//!   `(recovery_epoch, seq)` it was sealed for, and [`SealedSyncRecord`] hands
//!   that version back to the caller alongside the bytes.
//!   [`core_sync_seal_is_current`] is the one place that answers "may these
//!   bytes still be sent?", so a roster change makes every planned copy
//!   visibly stale rather than silently wrong.
//!
//! What a sync record may *contain* is deliberately broad: §8 allows contacts'
//! data the person already legitimately holds — cards, endpoints, history. What
//! that data may never do is widen the person boundary (DL-5), and this module
//! is where the boundary is drawn: the payload travels only inside a seal no
//! third party can open, and no record kind here has a recipient other than an
//! own device.
//!
//! ## Why the *device*, not the person root, does the outer signature
//!
//! A sealed sync record is an ordinary sealed envelope: [`crate::seal_message`]
//! signs and pads it exactly as it does a text message, so on the wire a
//! sibling's sync traffic is indistinguishable from ordinary 1:1 mail — which
//! is what §12 requires and what keeps a relay or a mule from learning that a
//! person has more than one device. The *inner* [`SyncRecord`] then carries its
//! own device signature in §3's [`DeviceSigningDomain::SyncRecord`] domain, so
//! "which of my devices wrote this" is answered by a signature that can never
//! be replayed as a message, a certificate, or a roster.
//!
//! The outer signature is the seam worth being exact about, because §14.2
//! settles who holds what: **the person root secret lives only inside the
//! passphrase-encrypted backup and is never copied to a linked device.** A
//! linked device therefore *cannot* produce a person-root outer signature, so a
//! rule that demanded one would mean a linked device could never seal a sync
//! record at all — the whole work package, dead on the second phone.
//!
//! [`crate::seal_message`] is structurally an [`Identity`]-shaped API (it wants
//! a signing secret and the public half that goes with it), and re-plumbing it
//! to take a bare keypair would touch every contact-facing path for no gain
//! here. So the layering is stated instead of dodged:
//! [`core_device_sync_identity`] wraps a [`DeviceKeypair`] in the same
//! [`Identity`] shape, whose `user_id` is the **device id** rather than a person
//! id — legal precisely because [`crate::core_derive_device_id`] and
//! [`crate::derive_user_id`] are one derivation over a signing public key, so
//! "the id this signature proves" comes out right either way. What changes is
//! the question the reader asks: [`core_open_sync_record`] no longer asks "is
//! this the person's root key" but "is this an **active device** of this person,
//! or the root itself" — the device layer, which is where §4's roster already
//! answers.
//!
//! The root is still accepted, for the install that has not linked anything yet
//! and whose only key *is* the root (§3's upgrade-in-place). Nothing else is: a
//! tombstoned device's outer signature is refused by the same roster walk that
//! refuses its inner one (§10.3).
//!
//! ## Record kinds
//!
//! Six §8 record kinds carried as the sealed-body kinds
//! [`crate::KIND_SYNC_HISTORY`]..=[`crate::KIND_SYNC_SETTINGS`], plus SYNC-1's
//! digest carrier [`crate::KIND_SYNC_DIGEST`], which the 10..=15 block had no
//! room left for (`protocol.rs` holds the layout table). Each record kind has
//! its own payload codec below — the digest's is `sync_stream.rs`'s
//! [`crate::core_encode_sync_digest`], reused rather than restated — and each
//! codec's bytes are frozen by a fixed-key golden vector in this file's tests,
//! in the same style as `device_roster.rs`'s certificate and roster vectors, so
//! a field-order or framing change fails here rather than quietly
//! desynchronizing two of a person's devices.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use x25519_dalek::{PublicKey as XPublicKey, StaticSecret};

use crate::crypto::{open_sealed_with_agree_sk, signing_key_from_bytes};
use crate::device_roster::{
    core_decode_roster, core_device_sign, core_device_verify, core_encode_roster, DeviceKeypair,
    DeviceSigningDomain, Roster, RosterVersion, DEVICE_ID_LEN, LEGACY_DEVICE_ID,
};
use crate::groups::{decode_group_invite_content, encode_group_invite_content, Group};
use crate::limits::MAX_ENVELOPE_SEALED_BYTES;
use crate::protocol::{
    KIND_SYNC_CONTACTS, KIND_SYNC_DIGEST, KIND_SYNC_GROUPS, KIND_SYNC_HISTORY,
    KIND_SYNC_OWN_ROSTER, KIND_SYNC_SETTINGS, KIND_SYNC_WATERMARK,
};
use crate::{seal_message, CoreError, Identity};

/// Leading byte of every encoded [`SyncRecord`] and of every payload codec in
/// this module. One version byte covers the whole family, so a future record
/// shape is a single deliberate bump rather than six independent ones.
const SYNC_RECORD_VERSION: u8 = 1;

/// X25519 keys are 32 bytes, as everywhere else in this crate.
const KEY_LEN: usize = 32;
/// Raw Ed25519 signature length.
const SIGNATURE_LEN: usize = 64;
/// Envelope `msg_id` width, mirrored from `protocol.rs` because a history entry
/// names one.
const MSG_ID_LEN: usize = 16;

/// The largest payload a sync record may carry.
///
/// Sealing adds the signed-body prologue and pads to the next 256-byte bucket,
/// and the result has to fit [`MAX_ENVELOPE_SEALED_BYTES`] — relayd's
/// independently enforced admission limit — or the record is a document no
/// transport will move. Rather than let a caller discover that at upload time,
/// encoding refuses here, one bucket short of the ceiling so the prologue and
/// padding always fit.
pub const SYNC_RECORD_MAX_PAYLOAD_BYTES: usize = MAX_ENVELOPE_SEALED_BYTES - 1024;

/// Most entries any one sync-record payload may carry. A record is a unit of
/// anti-entropy, not a whole-store dump: SYNC-1 fills gaps by exchanging
/// digests and sending what is missing, so a sender that wants to move more
/// than this sends more records. The cap is what keeps one malformed count
/// from making a decoder allocate.
pub const SYNC_RECORD_MAX_ENTRIES: usize = 4096;

// ---------------------------------------------------------------------------
// Record kinds
// ---------------------------------------------------------------------------

/// Which of §8's record kinds a [`SyncRecord`] carries.
///
/// The enum and the sealed-body `kind` byte are two views of one thing:
/// [`core_sync_record_kind_wire`] and [`core_sync_record_kind_of`] are the only
/// mapping, so neither shell ever writes a bare `10`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SyncRecordKind {
    /// Message history, authored and received ([`SyncHistoryPayload`]).
    History,
    /// Delivered and read watermarks ([`SyncWatermarkPayload`]).
    Watermarks,
    /// The contact list and contacts' rosters ([`SyncContactsPayload`]).
    Contacts,
    /// The person's own roster and inbox keys ([`SyncOwnRosterPayload`]).
    OwnRoster,
    /// Group membership and state ([`SyncGroupsPayload`]).
    Groups,
    /// The settings the product deems shared ([`SyncSettingsPayload`]).
    Settings,
    /// SYNC-1's per-stream watermark digest ([`crate::SyncDigest`], encoded by
    /// [`crate::core_encode_sync_digest`]).
    ///
    /// The one kind that describes streams rather than being one. It is signed,
    /// sealed, admitted and deduped exactly like the others — that is what
    /// makes a watermark exchange a document only this person's devices can
    /// read — but a receiver applies it and files no stream slot: a digest is
    /// stale the moment it lands, so gap-filling yesterday's watermarks would
    /// be anti-entropy chasing its own tail.
    Digest,
}

/// The sealed-body `kind` byte this record kind rides as.
#[uniffi::export]
pub fn core_sync_record_kind_wire(kind: SyncRecordKind) -> u8 {
    match kind {
        SyncRecordKind::History => KIND_SYNC_HISTORY,
        SyncRecordKind::Watermarks => KIND_SYNC_WATERMARK,
        SyncRecordKind::Contacts => KIND_SYNC_CONTACTS,
        SyncRecordKind::OwnRoster => KIND_SYNC_OWN_ROSTER,
        SyncRecordKind::Groups => KIND_SYNC_GROUPS,
        SyncRecordKind::Settings => KIND_SYNC_SETTINGS,
        SyncRecordKind::Digest => KIND_SYNC_DIGEST,
    }
}

/// The record kind a sealed-body `kind` byte names, or `None` for every other
/// kind — including a sync kind a *future* build invents, which this one must
/// fail soft on rather than misfile.
#[uniffi::export]
pub fn core_sync_record_kind_of(kind: u8) -> Option<SyncRecordKind> {
    match kind {
        KIND_SYNC_HISTORY => Some(SyncRecordKind::History),
        KIND_SYNC_WATERMARK => Some(SyncRecordKind::Watermarks),
        KIND_SYNC_CONTACTS => Some(SyncRecordKind::Contacts),
        KIND_SYNC_OWN_ROSTER => Some(SyncRecordKind::OwnRoster),
        KIND_SYNC_GROUPS => Some(SyncRecordKind::Groups),
        KIND_SYNC_SETTINGS => Some(SyncRecordKind::Settings),
        KIND_SYNC_DIGEST => Some(SyncRecordKind::Digest),
        _ => None,
    }
}

/// Whether this kind is a *stream* — a run of records SYNC-1 gap-fills and a
/// device retains so a sibling can be backfilled weeks later.
///
/// Every kind but [`SyncRecordKind::Digest`] is. The digest is the exception
/// the whole anti-entropy exchange rests on: it is the message that *asks*, so
/// treating it as something to be asked for would make a device request last
/// week's watermarks, apply them, advertise them, and be offered them again —
/// anti-entropy over its own control channel. One predicate rather than six
/// `!= Digest` tests scattered through the store, so the exception has one
/// place to be wrong.
#[uniffi::export]
pub fn core_sync_kind_is_stream(kind: SyncRecordKind) -> bool {
    !matches!(kind, SyncRecordKind::Digest)
}

// ---------------------------------------------------------------------------
// The person-scoped inbox key (§6)
// ---------------------------------------------------------------------------

/// §6's person-scoped X25519 inbox key, versioned by `generation`.
///
/// Every linked device of one person holds this key; no one else does. That is
/// the whole of the person boundary in one object, which is why it is also the
/// only thing [`core_seal_sync_record`] will address a record to.
///
/// The secret rides in this record because it *must* travel — §6 distributes
/// the inbox key by link bootstrap (§9.3, WP3) and by self-sync
/// ([`SyncOwnRosterPayload`]), so a type that held only the public half could
/// not express the thing the spec asks for. Handle it exactly as
/// [`crate::Identity`]'s secrets are handled: the core generates and never
/// persists; the shell keeps it in platform-protected storage.
///
/// `generation` is the counter [`Roster::inbox_key_generation`] carries. §10
/// bumps it and rotates the key on every revocation, through
/// [`core_rotate_inbox_key`]; `revocation.rs` is the ceremony that reaches for
/// it, and [`core_seal_sync_handoff`] is how the result gets to a sibling that
/// does not have it yet.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct InboxKey {
    /// Matches [`Roster::inbox_key_generation`] for the roster this key belongs
    /// to.
    pub generation: u64,
    pub agree_pk: Vec<u8>,
    pub agree_sk: Vec<u8>,
}

/// Mint a fresh inbox key at `generation` (§6).
#[uniffi::export]
pub fn core_mint_inbox_key(generation: u64) -> InboxKey {
    let agree_sk = StaticSecret::random_from_rng(rand_core::OsRng);
    let agree_pk = XPublicKey::from(&agree_sk);
    InboxKey {
        generation,
        agree_pk: agree_pk.as_bytes().to_vec(),
        agree_sk: agree_sk.to_bytes().to_vec(),
    }
}

/// Rotate an inbox key: brand-new material at `generation + 1` (§10.1).
///
/// Nothing of the old key survives into the new one. A rotation whose output
/// shared material with its input would leave a revoked device able to read
/// mail sealed after its revocation, which is the exact hole §10 rotates to
/// close.
#[uniffi::export]
pub fn core_rotate_inbox_key(current: InboxKey) -> InboxKey {
    core_mint_inbox_key(current.generation.saturating_add(1))
}

/// Whether an inbox key's secret really is the secret for its public half.
///
/// The check exists because the seal path is the person boundary: sealing to a
/// public key this device does not hold the secret for is precisely "sealing to
/// somebody else", and a mismatched pair is the one shape that could express it
/// through an [`InboxKey`]-typed parameter.
fn inbox_key_is_consistent(key: &InboxKey) -> bool {
    let Ok(secret): Result<[u8; KEY_LEN], _> = key.agree_sk.as_slice().try_into() else {
        return false;
    };
    XPublicKey::from(&StaticSecret::from(secret)).as_bytes()[..] == key.agree_pk[..]
}

// ---------------------------------------------------------------------------
// The record
// ---------------------------------------------------------------------------

/// One signed §8 sync record: a header naming the stream it belongs to, an
/// opaque kind-specific payload, and the authoring device's signature.
///
/// The header is what SYNC-1's anti-entropy reads. `author_device_id` plus
/// `kind` name the stream; `stream_seq` is that stream's watermark, monotone
/// per `(author_device_id, kind)` and gap-free, so a sibling can say "I have
/// your Watermarks stream through 41" and be answered with exactly 42 onward.
/// Nothing here assumes the two devices are ever online together, which is
/// SYNC-1's standing constraint.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncRecord {
    pub kind: SyncRecordKind,
    /// The person whose devices this record is for. Always this person: a sync
    /// record is never about anybody else's device set.
    pub person_id: Vec<u8>,
    /// The 16-byte id of the device that authored the record. Never
    /// [`LEGACY_DEVICE_ID`] — a device with no device key cannot author a sync
    /// record at all, which is §9.4's two-phase activation seen from this side.
    pub author_device_id: Vec<u8>,
    /// The own-roster version this record was authored and sealed against
    /// (SYNC-3's "re-sealed on roster change").
    pub roster_version: RosterVersion,
    /// The [`InboxKey::generation`] this record is sealed under (§6).
    pub inbox_key_generation: u64,
    /// Monotone within `(author_device_id, kind)` — SYNC-1's per-stream
    /// watermark.
    pub stream_seq: u64,
    pub timestamp_ms: i64,
    /// The kind-specific payload, encoded by this module's matching codec.
    pub payload: Vec<u8>,
    /// Raw 64-byte Ed25519 signature over [`sync_record_signed_bytes`], in
    /// [`DeviceSigningDomain::SyncRecord`].
    pub signature: Vec<u8>,
}

/// The record without its signature: the bytes the device signature covers and
/// the prefix of the encoded record.
///
/// Every field is committed to, so a record cannot be re-attributed to another
/// device, re-dated, moved to another stream position, or replayed against
/// another roster version or inbox generation without breaking its signature.
fn sync_record_signed_bytes(record: &SyncRecord) -> Result<Vec<u8>, CoreError> {
    if record.payload.len() > SYNC_RECORD_MAX_PAYLOAD_BYTES {
        return Err(CoreError::Malformed(format!(
            "sync record payload is {} bytes, over the {SYNC_RECORD_MAX_PAYLOAD_BYTES}-byte limit",
            record.payload.len()
        )));
    }
    let mut out = vec![SYNC_RECORD_VERSION, core_sync_record_kind_wire(record.kind)];
    push_bytes16(&mut out, &record.person_id, "sync record person id")?;
    push_bytes16(
        &mut out,
        &record.author_device_id,
        "sync record author device id",
    )?;
    out.extend_from_slice(&record.roster_version.recovery_epoch.to_be_bytes());
    out.extend_from_slice(&record.roster_version.seq.to_be_bytes());
    out.extend_from_slice(&record.inbox_key_generation.to_be_bytes());
    out.extend_from_slice(&record.stream_seq.to_be_bytes());
    out.extend_from_slice(&record.timestamp_ms.to_be_bytes());
    out.extend_from_slice(&(record.payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&record.payload);
    Ok(out)
}

/// Sign a sync record under the authoring device's Ed25519 key (§3's
/// [`DeviceSigningDomain::SyncRecord`] domain).
///
/// `author_device_id` is derived from the signing secret rather than trusted
/// from the caller, exactly as [`crate::core_sign_device_cert`] fills in its
/// signer: the recorded author and the key that actually signed can then never
/// disagree, which is what makes the stream key
/// `(author_device_id, kind, stream_seq)` something a sibling can rely on. The
/// public half is not a parameter for the same reason — one secret in, one
/// author out, with no pair to get wrong.
#[uniffi::export]
pub fn core_sign_sync_record(
    record: SyncRecord,
    author_device_sign_sk: Vec<u8>,
) -> Result<SyncRecord, CoreError> {
    let author_device_sign_pk = signing_key_from_bytes(&author_device_sign_sk)?
        .verifying_key()
        .as_bytes()
        .to_vec();
    let device_id = crate::core_derive_device_id(author_device_sign_pk)?;
    if device_id[..] == LEGACY_DEVICE_ID[..] {
        return Err(CoreError::Malformed(
            "a sync record cannot be authored on the legacy device stream".to_string(),
        ));
    }
    let mut record = record;
    record.author_device_id = device_id;
    record.signature = core_device_sign(
        DeviceSigningDomain::SyncRecord,
        author_device_sign_sk,
        sync_record_signed_bytes(&record)?,
    )?;
    Ok(record)
}

/// Verify a sync record's device signature against a device signing key.
///
/// Whether that device is *this person's* — and still is — is a roster-level
/// question, answered by [`core_sync_record_admit`], in the same split
/// [`crate::core_verify_device_cert`] and [`crate::core_roster_validate`] keep.
#[uniffi::export]
pub fn core_verify_sync_record(
    record: SyncRecord,
    author_device_sign_pk: Vec<u8>,
) -> Result<(), CoreError> {
    let signed = sync_record_signed_bytes(&record)?;
    core_device_verify(
        DeviceSigningDomain::SyncRecord,
        author_device_sign_pk,
        signed,
        record.signature,
    )
}

/// Encode a signed sync record: [`sync_record_signed_bytes`] followed by the
/// length-framed signature.
#[uniffi::export]
pub fn core_encode_sync_record(record: SyncRecord) -> Result<Vec<u8>, CoreError> {
    let mut out = sync_record_signed_bytes(&record)?;
    push_bytes16(&mut out, &record.signature, "sync record signature")?;
    Ok(out)
}

/// Decode a sync record. Fully bounds-checked; trailing bytes are an error, and
/// an unknown version or an unknown record kind fails closed rather than
/// half-parsing — a sibling running a newer build gets "I cannot read this
/// yet", never a misfiled stream.
#[uniffi::export]
pub fn core_decode_sync_record(bytes: Vec<u8>) -> Result<SyncRecord, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let version = cursor.take_u8()?;
    if version != SYNC_RECORD_VERSION {
        return Err(CoreError::Malformed(format!(
            "unsupported sync record version {version}"
        )));
    }
    let wire_kind = cursor.take_u8()?;
    let kind = core_sync_record_kind_of(wire_kind)
        .ok_or_else(|| CoreError::Malformed(format!("unknown sync record kind {wire_kind}")))?;
    let person_id = cursor.take_bytes16()?;
    let author_device_id = cursor.take_bytes16()?;
    let roster_version = RosterVersion {
        recovery_epoch: cursor.take_u64()?,
        seq: cursor.take_u64()?,
    };
    let inbox_key_generation = cursor.take_u64()?;
    let stream_seq = cursor.take_u64()?;
    let timestamp_ms = cursor.take_i64()?;
    let payload = cursor.take_bytes32()?;
    if payload.len() > SYNC_RECORD_MAX_PAYLOAD_BYTES {
        return Err(CoreError::Malformed(format!(
            "sync record payload is {} bytes, over the {SYNC_RECORD_MAX_PAYLOAD_BYTES}-byte limit",
            payload.len()
        )));
    }
    let signature = cursor.take_bytes16()?;
    cursor.finish()?;
    Ok(SyncRecord {
        kind,
        person_id,
        author_device_id,
        roster_version,
        inbox_key_generation,
        stream_seq,
        timestamp_ms,
        payload,
        signature,
    })
}

// ---------------------------------------------------------------------------
// SYNC-3: sealing to the person's own current device set
// ---------------------------------------------------------------------------

/// A sealed sync record, together with the two facts that decide whether it may
/// still be sent.
///
/// The sealed bytes are an ordinary sealed envelope payload — hand them
/// straight to the transport that carries any other 1:1 body. `sealed_for` and
/// `inbox_key_generation` are what make SYNC-3's "re-sealed on roster change"
/// checkable: a planner holding a queue of these asks
/// [`core_sync_seal_is_current`] before each send instead of assuming the
/// roster it sealed under is still the roster it has.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SealedSyncRecord {
    pub sealed: Vec<u8>,
    /// The own-roster version these bytes were sealed for.
    pub sealed_for: RosterVersion,
    /// The [`InboxKey::generation`] these bytes were sealed under.
    pub inbox_key_generation: u64,
}

/// The [`Identity`] shape a device signs a sealed sync record's *outer*
/// envelope with (§14.2: a linked device holds no person root).
///
/// It is an `Identity` only because [`crate::seal_message`] is one — see the
/// module docs. Read its fields for what they are: `user_id` is this
/// **device's** id, not a person id, and the signature it produces proves a
/// device rather than a person. [`core_open_sync_record`] is the only reader
/// that resolves it, and it resolves it against the roster's device layer.
///
/// The agreement half is carried through unchanged so the value round-trips a
/// `DeviceKeypair`, but nothing in the sync path uses it: a sync record is
/// sealed *to* the person's [`InboxKey`], never to a device key.
#[uniffi::export]
pub fn core_device_sync_identity(device: DeviceKeypair) -> Identity {
    Identity {
        user_id: device.device_id,
        sign_pk: device.sign_pk,
        sign_sk: device.sign_sk,
        agree_pk: device.agree_pk,
        agree_sk: device.agree_sk,
    }
}

/// Seal a signed sync record to the person's own current device set (SYNC-3).
///
/// The only address this function accepts is an [`InboxKey`] whose secret half
/// this device holds and whose two halves agree — see the module docs for why
/// that, and not a doc comment, is what keeps a sync record from ever being
/// addressed at a contact. Everything else is the ordinary sealed-envelope
/// path: [`crate::seal_message`] signs and pads exactly as it does for a text
/// message, so what leaves this device is indistinguishable on the wire from
/// ordinary 1:1 traffic (§12).
///
/// `author` is the material the *outer* signature is made with, and it may be
/// either of the two things a real install can actually hold:
///
/// * the **authoring device's** identity ([`core_device_sync_identity`]) — the
///   normal case, and the only one available to a linked device, because §14.2
///   keeps the person root inside the encrypted backup and off every phone; or
/// * the **person root** — the un-linked install whose only key is the root
///   (§3's upgrade-in-place), authoring on its own device stream.
///
/// Anything else is refused here rather than at the far end of a DTN hop.
/// Note what the device branch requires: `author.user_id` must be the record's
/// own `author_device_id`, which [`core_sign_sync_record`] derived from the
/// signing secret — so the device that signed the record is provably the device
/// that sealed it, and one cannot be swapped for the other.
///
/// The record must already carry its device signature — sealing an unsigned
/// record would produce a document every sibling correctly refuses, and
/// discovering that on the far side of a DTN hop is not a good way to find out.
#[uniffi::export]
pub fn core_seal_sync_record(
    record: SyncRecord,
    author: Identity,
    inbox_key: InboxKey,
) -> Result<SealedSyncRecord, CoreError> {
    if record.person_id != author.user_id && record.author_device_id != author.user_id {
        return Err(CoreError::Crypto(
            "a sync record is sealed either by the person it names or by the device that authored \
             it, and this key is neither"
                .to_string(),
        ));
    }
    if record.inbox_key_generation != inbox_key.generation {
        return Err(CoreError::Crypto(format!(
            "sync record is for inbox key generation {} but this key is generation {}",
            record.inbox_key_generation, inbox_key.generation
        )));
    }
    if record.signature.len() != SIGNATURE_LEN {
        return Err(CoreError::SignatureInvalid);
    }
    if !inbox_key_is_consistent(&inbox_key) {
        return Err(CoreError::Crypto(
            "inbox key secret does not match its public half; a sync record is sealed only to a \
             key this device holds"
                .to_string(),
        ));
    }
    let sealed_for = record.roster_version;
    let inbox_key_generation = record.inbox_key_generation;
    let sealed = seal_message(author, inbox_key.agree_pk, core_encode_sync_record(record)?)?;
    Ok(SealedSyncRecord {
        sealed,
        sealed_for,
        inbox_key_generation,
    })
}

/// Why an opened sync record may not be believed. `None` from
/// [`core_sync_record_admit`] means every SYNC-3 condition holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SyncRecordRejection {
    /// The record names a different person than the roster it is checked
    /// against. Sync records are within one person, always.
    ForeignPerson,
    /// Sealed under an inbox key generation this key is not. A record from
    /// before a §10 rotation is refused rather than accepted late: the old
    /// generation is exactly the one a revoked device still holds.
    StaleInboxKey,
    /// Authored on [`LEGACY_DEVICE_ID`] — the reserved stream of a person with
    /// no device keys, which by definition cannot have signed anything.
    LegacyAuthorDevice,
    /// The author is tombstoned in this person's roster (DL-4, §10.3): a
    /// revoked device's newly received events are refused, and the history it
    /// already wrote stays.
    RevokedAuthorDevice,
    /// The author is not an active device of this person.
    UnknownAuthorDevice,
    /// The device signature does not verify in the sync-record domain.
    SignatureInvalid,
    /// §10.1's rotation handoff carries one kind and one kind only
    /// ([`SyncRecordKind::OwnRoster`]). The handoff channel is sealed to a
    /// sibling's *device* key rather than to the inbox key, so it is the one
    /// channel that survives a rotation — and therefore the one channel that
    /// must never become a general-purpose way to bypass
    /// [`SyncRecordRejection::StaleInboxKey`].
    NotARotationHandoff,
}

/// The SYNC-3 gate on an opened record, against this person's own roster.
///
/// Pure and separate from the crypto so it can be tested — and reasoned about —
/// on its own, in the same split `device_roster.rs` keeps between
/// [`crate::core_roster_validate`] and [`crate::core_roster_accept`].
///
/// The checks run oldest-question-first: is this even my person, is this key
/// the current one, is the author a device of mine that is still allowed to
/// speak, and only then does the signature get verified. Ordering matters for
/// what a caller learns, not for safety — every one of them must pass.
#[uniffi::export]
pub fn core_sync_record_admit(
    record: SyncRecord,
    inbox_key_generation: u64,
    own_roster: Roster,
) -> Option<SyncRecordRejection> {
    admit(record, Some(inbox_key_generation), own_roster)
}

/// The SYNC-3 gate for §10.1's **rotation handoff**: the same checks as
/// [`core_sync_record_admit`] with exactly two differences, and both are forced
/// by what the handoff is for.
///
/// * The [`SyncRecordRejection::StaleInboxKey`] check is dropped. A handoff is
///   sealed to a sibling's device key, not to an inbox key, precisely because
///   the generation it announces is one the receiver does not hold yet —
///   refusing it for naming a generation the receiver has not got would refuse
///   every rotation there will ever be.
/// * The kind is pinned to [`SyncRecordKind::OwnRoster`]. Dropping the
///   generation check is a real weakening, so it is confined to the one payload
///   that carries a roster and its new key. History, watermarks, contacts,
///   groups and settings keep the strict gate; there is no way to smuggle them
///   through this door.
///
/// Everything else is unchanged and load-bearing: the record must name this
/// person, its author must be an active — not tombstoned (DL-4, §10.3) —
/// device of the roster the receiver holds *now*, and its device signature must
/// verify. So a revoked device cannot announce a rotation of its own.
#[uniffi::export]
pub fn core_sync_handoff_admit(
    record: SyncRecord,
    own_roster: Roster,
) -> Option<SyncRecordRejection> {
    if record.kind != SyncRecordKind::OwnRoster {
        return Some(SyncRecordRejection::NotARotationHandoff);
    }
    admit(record, None, own_roster)
}

/// The shared body of the two gates above. `inbox_key_generation` is `None` on
/// the §10.1 handoff path and `Some` everywhere else; nothing else differs, so
/// the two can never drift on the checks they do share.
fn admit(
    record: SyncRecord,
    inbox_key_generation: Option<u64>,
    own_roster: Roster,
) -> Option<SyncRecordRejection> {
    if record.person_id != own_roster.person_id {
        return Some(SyncRecordRejection::ForeignPerson);
    }
    if let Some(generation) = inbox_key_generation {
        if record.inbox_key_generation != generation {
            return Some(SyncRecordRejection::StaleInboxKey);
        }
    }
    if record.author_device_id.len() != DEVICE_ID_LEN
        || record.author_device_id[..] == LEGACY_DEVICE_ID[..]
    {
        return Some(SyncRecordRejection::LegacyAuthorDevice);
    }
    if own_roster
        .tombstones
        .iter()
        .any(|tombstone| tombstone.device_id == record.author_device_id)
    {
        return Some(SyncRecordRejection::RevokedAuthorDevice);
    }
    let Some(author) = own_roster
        .devices
        .iter()
        .find(|cert| cert.device_id() == record.author_device_id)
    else {
        return Some(SyncRecordRejection::UnknownAuthorDevice);
    };
    let author_sign_pk = author.device_sign_pk.clone();
    match core_verify_sync_record(record, author_sign_pk) {
        Ok(()) => None,
        Err(_) => Some(SyncRecordRejection::SignatureInvalid),
    }
}

/// Whether an outer envelope signature that derived `signer_id` is one this
/// person's own device set may have produced (SYNC-3, §14.2).
///
/// Two acceptable answers, and the order matters only for what a reader learns:
///
/// * an **active device** of this roster — the ordinary case, because §14.2
///   keeps the person root off every linked device, so a device signature is
///   the only outer signature a linked install can make; or
/// * the **person root** itself, for the install that has not linked anything
///   and whose only key is the root (§3's upgrade-in-place).
///
/// A *tombstoned* device is neither, checked explicitly rather than left to the
/// active-device walk: §10.3 revokes a device's ability to speak, and an outer
/// signature is speech. The inner record signature is refused by the same
/// tombstone in [`core_sync_record_admit`]; this is the same rule one layer out,
/// so a revoked device cannot even wrap somebody else's still-valid record.
pub(crate) fn outer_signer_is_own(signer_id: &[u8], own_roster: &Roster) -> bool {
    if own_roster
        .tombstones
        .iter()
        .any(|tombstone| tombstone.device_id[..] == *signer_id)
    {
        return false;
    }
    if own_roster.person_id[..] == *signer_id {
        return true;
    }
    own_roster
        .devices
        .iter()
        .any(|cert| cert.device_id()[..] == *signer_id)
}

/// Open a sealed sync record with the person's inbox key and admit it against
/// the person's own roster (SYNC-3).
///
/// Four independent things must hold, and a record that fails any of them is an
/// error rather than a degraded success:
///
/// 1. the inbox secret decrypts it — only own devices hold that key;
/// 2. the outer envelope signature belongs to this person's own device set
///    ([`outer_signer_is_own`]), so a record sealed *to* the inbox key by
///    somebody who somehow learned its public half is still refused;
/// 3. the record decodes; and
/// 4. [`core_sync_record_admit`] passes.
///
/// Condition 2 is the one worth naming twice. The inbox key's public half
/// travels with the key, so possession of the *public* half is not evidence of
/// anything; requiring an own outer signature is what turns "somebody sealed
/// this to my inbox" into "one of my devices sealed this". And it is checked at
/// the **device** layer rather than against the person root, because §14.2
/// leaves the root inside the encrypted backup: a person-root-only rule would
/// have meant no linked device could ever seal a sync record, which is the
/// entire mechanism.
#[uniffi::export]
pub fn core_open_sync_record(
    sealed: Vec<u8>,
    inbox_key: InboxKey,
    own_roster: Roster,
) -> Result<SyncRecord, CoreError> {
    let opened = open_sealed_with_agree_sk(&inbox_key.agree_sk, &sealed)?;
    if !outer_signer_is_own(&opened.sender_user_id, &own_roster) {
        return Err(CoreError::Crypto(
            "sync record was not sealed by this person's own device set".to_string(),
        ));
    }
    let record = core_decode_sync_record(opened.payload)?;
    match core_sync_record_admit(record.clone(), inbox_key.generation, own_roster) {
        None => Ok(record),
        Some(rejection) => Err(sync_rejection_error(rejection)),
    }
}

fn sync_rejection_error(rejection: SyncRecordRejection) -> CoreError {
    match rejection {
        SyncRecordRejection::SignatureInvalid => CoreError::SignatureInvalid,
        SyncRecordRejection::ForeignPerson => {
            CoreError::Crypto("sync record names another person".to_string())
        }
        SyncRecordRejection::StaleInboxKey => {
            CoreError::Crypto("sync record is sealed under a superseded inbox key".to_string())
        }
        SyncRecordRejection::LegacyAuthorDevice => {
            CoreError::Crypto("sync record claims the legacy device stream".to_string())
        }
        SyncRecordRejection::RevokedAuthorDevice => {
            CoreError::Crypto("sync record is authored by a revoked device".to_string())
        }
        SyncRecordRejection::UnknownAuthorDevice => {
            CoreError::Crypto("sync record is authored by an unknown device".to_string())
        }
        SyncRecordRejection::NotARotationHandoff => CoreError::Crypto(
            "only an own-roster record may ride the rotation handoff channel".to_string(),
        ),
    }
}

// ---------------------------------------------------------------------------
// §10.1's rotation handoff
// ---------------------------------------------------------------------------

/// **Seal §10.1's rotation announcement to ONE sibling device.**
///
/// Every other sync record is sealed to the person's [`InboxKey`], and a
/// revocation is precisely the moment that key stops being a safe address. The
/// record that *announces* the rotation cannot use either generation of it:
///
/// * sealed under the **old** generation it hands the device being buried the
///   very secret the rotation exists to take away — §10's threat model assumes
///   the revoked device is hostile and keeps everything it ever saw, so this is
///   not a small window, it is the whole revocation undone; and
/// * sealed under the **new** generation it cannot be opened by the surviving
///   siblings, who do not have that key yet. That is the announcement's job.
///
/// So the announcement — and, by [`core_sync_handoff_admit`]'s kind check, only
/// the announcement — is sealed once per surviving sibling to that sibling's
/// `device_agree_pk`. A revoked device holds no sibling's device secret, which
/// is what makes this the one channel the thief cannot read. It costs one copy
/// per surviving device, bounded by §14.3's hard cap of 16, once per revocation.
///
/// # The address is a roster lookup, never a key the caller supplies
///
/// This module's boundary rests on [`core_seal_sync_record`] having no parameter
/// that accepts a bare public key, so a sync record cannot be addressed at a
/// contact even by mistake. That property is preserved here rather than spent:
/// the caller names a `recipient_device_id`, and the agreement key is read out
/// of the certificate `own_roster` carries for it (§4). The only reachable
/// addresses are therefore this person's own active devices — a contact's key is
/// unreachable because no certificate in this person's roster holds one, and,
/// which is the point of the whole exercise, **a device this roster has
/// tombstoned is unreachable too** (DL-4, §10.3). The announcement cannot be
/// addressed to the phone whose removal it announces.
///
/// Pass the roster the revocation *produced*, not the one it superseded: the
/// surviving devices are the ones the new document lists, and that is exactly
/// the set to be told.
///
/// Note what is *not* relaxed. The record still carries its ordinary device
/// signature, the outer envelope is still [`crate::seal_message`]'s, and the
/// receiver still runs the full roster gate. This changes the address, not the
/// authority.
#[uniffi::export]
pub fn core_seal_sync_handoff(
    record: SyncRecord,
    author: Identity,
    own_roster: Roster,
    recipient_device_id: Vec<u8>,
) -> Result<SealedSyncRecord, CoreError> {
    if record.kind != SyncRecordKind::OwnRoster {
        return Err(sync_rejection_error(
            SyncRecordRejection::NotARotationHandoff,
        ));
    }
    if record.person_id != author.user_id && record.author_device_id != author.user_id {
        return Err(CoreError::Crypto(
            "a rotation handoff is sealed either by the person it names or by the device that \
             authored it, and this key is neither"
                .to_string(),
        ));
    }
    if record.signature.len() != SIGNATURE_LEN {
        return Err(CoreError::SignatureInvalid);
    }
    if record.person_id != own_roster.person_id {
        return Err(CoreError::Crypto(
            "a rotation handoff is addressed inside one person's device set, and this roster is \
             somebody else's"
                .to_string(),
        ));
    }
    if own_roster
        .tombstones
        .iter()
        .any(|tombstone| tombstone.device_id == recipient_device_id)
    {
        return Err(CoreError::Crypto(
            "a rotation handoff is never addressed to a device this roster has revoked".to_string(),
        ));
    }
    let recipient = own_roster
        .devices
        .iter()
        .find(|cert| cert.device_id() == recipient_device_id)
        .ok_or_else(|| {
            CoreError::Crypto(
                "a rotation handoff is addressed to a device this roster lists, and it lists no \
                 such device"
                    .to_string(),
            )
        })?;
    let sealed_for = record.roster_version;
    let inbox_key_generation = record.inbox_key_generation;
    let sealed = seal_message(
        author,
        recipient.device_agree_pk.clone(),
        core_encode_sync_record(record)?,
    )?;
    Ok(SealedSyncRecord {
        sealed,
        sealed_for,
        inbox_key_generation,
    })
}

/// Open §10.1's rotation announcement with this device's own X25519 secret.
///
/// `own_roster` is the roster this device holds **now** — the pre-rotation one,
/// because the whole point of the record inside is to replace it. It is what
/// answers "is the device that sealed this one of mine, and is it still allowed
/// to speak" (DL-4, §10.3): a device this roster has already buried cannot
/// announce anything, inner signature or outer.
///
/// Deciding whether the roster *inside* supersedes what is stored is not this
/// function's business and deliberately so — that is DL-1's ordering, owned by
/// the monotone writers
/// ([`crate::MessageStore::adopt_own_roster`],
/// [`crate::MessageStore::core_set_own_sync_context`]) that refuse to go
/// backwards. This function answers only "may I believe these bytes came from a
/// device of mine".
///
/// # The recovery path's handoff is signed by a device this roster never listed
///
/// The rule above — the signer must be an active device of the roster held
/// **now** — is exactly right for the ordinary revocation and exactly wrong for
/// §14.2's. A recovery happens on a *new* phone: the person opens the
/// passphrase-encrypted `.cmbak`, and the roster the root signs at the next
/// epoch introduces a device nobody has ever seen. Its rotation announcement is
/// therefore signed by a device no surviving sibling holds a certificate for,
/// so a strict held-roster gate refuses it as
/// [`SyncRecordRejection::UnknownAuthorDevice`] — and the recovery reaches
/// nobody. The one ceremony that exists to rescue a fleet from a stolen phone
/// would be the one ceremony the fleet could not hear.
///
/// So the gate has a second acceptable answer, and every part of it is
/// load-bearing. The signer may instead be an active device of the roster
/// carried **inside** the record, provided that roster:
///
/// * validates to `person_root_sign_pk` — the root the receiver *already*
///   holds, never a key the document supplies, which is what stops a stranger
///   from bootstrapping their own fleet into this person's boundary; and
/// * is accepted over the held one by [`crate::core_roster_accept`], so DL-1's
///   ordering, DL-4's tombstones and §14.2's "only the root raises the epoch"
///   all apply before a single new signer is trusted. A stolen device cannot
///   take this door: it cannot sign a higher epoch, and within its own epoch it
///   is already listed, so it gains nothing it did not have.
///
/// The quarantine bit is deliberately not consulted here — `false` is passed —
/// because this function answers "may I believe these bytes", and a quarantined
/// person's document is believable and merely not adoptable.
/// [`crate::MessageStore::adopt_revocation_handoff`] runs the same acceptance
/// again with the stored quarantine state, and that is the call that decides.
#[uniffi::export]
pub fn core_open_sync_handoff(
    sealed: Vec<u8>,
    own_device_agree_sk: Vec<u8>,
    own_roster: Roster,
    person_root_sign_pk: Vec<u8>,
) -> Result<SyncRecord, CoreError> {
    let opened = open_sealed_with_agree_sk(&own_device_agree_sk, &sealed)?;
    let record = core_decode_sync_record(opened.payload)?;
    if outer_signer_is_own(&opened.sender_user_id, &own_roster)
        && core_sync_handoff_admit(record.clone(), own_roster.clone()).is_none()
    {
        return Ok(record);
    }
    // The recovery door. Anything that fails it reports the ORDINARY gate's
    // rejection, because that is the one a caller can act on: "this came from
    // a device I do not know" is the truth about a forgery, and burying it
    // under a recovery-specific error would make every ordinary failure read
    // like a recovery that went wrong.
    let ordinary = || match core_sync_handoff_admit(record.clone(), own_roster.clone()) {
        Some(rejection) => sync_rejection_error(rejection),
        None => CoreError::Crypto(
            "rotation handoff was not sealed by this person's own device set".to_string(),
        ),
    };
    let Ok(payload) = crate::core_decode_sync_own_roster(record.payload.clone()) else {
        return Err(ordinary());
    };
    let carried = payload.roster;
    let decision = crate::core_roster_accept(
        Some(own_roster.clone()),
        false,
        carried.clone(),
        person_root_sign_pk,
    );
    if decision.outcome != crate::RosterUpdateOutcome::Accepted {
        return Err(ordinary());
    }
    if !outer_signer_is_own(&opened.sender_user_id, &carried) {
        return Err(ordinary());
    }
    match core_sync_handoff_admit(record.clone(), carried) {
        None => Ok(record),
        Some(_) => Err(ordinary()),
    }
}

/// Whether bytes sealed for `sealed_for` at `sealed_generation` may still be
/// sent under `current` / `current_generation` (SYNC-3's "re-sealed on roster
/// change").
///
/// Any difference — not merely a lower version — means re-seal. A roster that
/// moved *backwards* relative to what a queued record was sealed for is not a
/// safe thing to keep sending either: DL-1 says the stored roster only advances,
/// so a lower current version means this device's own roster was replaced (a
/// restore, a recovery-epoch supersession), and the right response is to
/// re-author against what is actually stored now.
///
/// **Both halves of the seal are compared**, because SYNC-3's device set is
/// named by two independent numbers and a roster change is not the only way to
/// change it. §10.1 rotates the inbox key on revocation, and the rotation is
/// what actually cuts the revoked device off: bytes sealed under generation *n*
/// are readable by everyone who held generation *n*, revoked or not, so
/// continuing to send them because "the roster version still matches" would
/// hand the just-revoked device every subsequent record. The planner already
/// holds the current generation for exactly this comparison — it is not derived
/// here, so there is no second source of truth about which key is live.
#[uniffi::export]
pub fn core_sync_seal_is_current(
    sealed_for: RosterVersion,
    sealed_generation: u64,
    current: RosterVersion,
    current_generation: u64,
) -> bool {
    sealed_for == current && sealed_generation == current_generation
}

/// A stable 16-byte id for one record of one stream: `BLAKE2b-16(domain ‖
/// person ‖ author device ‖ kind ‖ stream_seq)`.
///
/// SYNC-1 needs an id that both devices compute identically without having
/// exchanged anything, so that a record re-sent after a dropped link dedupes
/// server-side instead of arriving twice — the same discipline
/// [`crate::device_fanout_msg_id`] keeps for fan-out rows, and the same reason:
/// relayd scopes rows by `(family_token, msg_id, recipient_hint)`, so a
/// deterministic id makes an identical re-upload free.
///
/// The payload is deliberately *not* hashed in. The id names the stream slot,
/// not the bytes: re-sealing the same slot after a roster change (SYNC-3) must
/// produce the same id, or every re-seal would spend a fresh relay row.
#[uniffi::export]
pub fn core_sync_record_id(
    person_id: Vec<u8>,
    author_device_id: Vec<u8>,
    kind: SyncRecordKind,
    stream_seq: u64,
) -> Vec<u8> {
    let mut input = SYNC_RECORD_ID_DOMAIN.to_vec();
    push_len_prefixed(&mut input, &person_id);
    push_len_prefixed(&mut input, &author_device_id);
    input.push(core_sync_record_kind_wire(kind));
    input.extend_from_slice(&stream_seq.to_be_bytes());
    let mut hasher = Blake2bVar::new(MSG_ID_LEN).expect("valid blake2b output length");
    hasher.update(&input);
    let mut out = vec![0u8; MSG_ID_LEN];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    out
}

/// Not a signing domain: it keeps a sync record's id space disjoint from every
/// other digest this crate computes over the same ids.
const SYNC_RECORD_ID_DOMAIN: &[u8] = b"CruiseMesh sync record id v1\0";

fn push_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
    out.extend_from_slice(bytes);
}

// ---------------------------------------------------------------------------
// Payload: message history (§8, KIND_SYNC_HISTORY)
// ---------------------------------------------------------------------------

/// Which side of a conversation an entry came from.
///
/// Kept explicit rather than inferred from `sender_person_id == person_id`,
/// because the receiving sibling needs the distinction before it has decided
/// anything about identity, and because SYNC-2's outbound dedup reads it: an
/// `Authored` entry from a sibling is exactly the evidence that says "do not
/// re-author this text, it already has a stream position".
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SyncHistoryDirection {
    /// Authored by one of this person's own devices.
    Authored,
    /// Received from a contact or a group.
    Received,
}

fn history_direction_wire(direction: SyncHistoryDirection) -> u8 {
    match direction {
        SyncHistoryDirection::Authored => 1,
        SyncHistoryDirection::Received => 2,
    }
}

fn history_direction_of(wire: u8) -> Result<SyncHistoryDirection, CoreError> {
    match wire {
        1 => Ok(SyncHistoryDirection::Authored),
        2 => Ok(SyncHistoryDirection::Received),
        other => Err(CoreError::Malformed(format!(
            "unknown sync history direction {other}"
        ))),
    }
}

/// One message, carried whole to a sibling.
///
/// `body` is the original's encoded message body — the very bytes
/// [`crate::encode_message_body_extended`] produced or
/// [`crate::decode_extended_message_body`] accepted — so `chat_id`, `lamport`,
/// `timestamp`, `kind` and `content` are not restated here and cannot drift
/// from the message they describe. What the body *cannot* carry is the sender's
/// person id: on the original envelope that came from the outer signature,
/// which does not survive re-sealing to a sibling. So it rides explicitly, and
/// with it the device dimension, completing §5's stream key
/// `(chat_id, sender_person_id, sender_device_id, lamport)` in one place.
///
/// `sender_device_id` is [`LEGACY_DEVICE_ID`] for every message from a v1 peer
/// and for every row that predates the migration — the same synthetic
/// one-device view §5 gives those rows everywhere else. Where the body also
/// carries a device extension, the two agree; if a malformed sender ever made
/// them disagree, this field wins, because it is the stream the authoring
/// device actually filed the row under.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncHistoryEntry {
    /// The original envelope's 16-byte `msg_id`, so a sibling that already
    /// consumed the message over another transport recognizes it rather than
    /// storing it twice.
    pub origin_msg_id: Vec<u8>,
    pub direction: SyncHistoryDirection,
    pub sender_person_id: Vec<u8>,
    pub sender_device_id: Vec<u8>,
    pub body: Vec<u8>,
}

/// A run of history for one or more streams (§8, [`SyncRecordKind::History`]).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncHistoryPayload {
    pub entries: Vec<SyncHistoryEntry>,
}

/// Encode a history payload.
///
/// Layout: `version(1) | entry_count(u16)`, then per entry
/// `origin_msg_id(16) | direction(1) | sender_person_id_len(u16) |
/// sender_person_id | sender_device_id(16) | body_len(u32) | body`.
#[uniffi::export]
pub fn core_encode_sync_history(payload: SyncHistoryPayload) -> Result<Vec<u8>, CoreError> {
    let mut out = payload_header(payload.entries.len())?;
    for entry in &payload.entries {
        push_fixed(&mut out, &entry.origin_msg_id, MSG_ID_LEN, "origin msg id")?;
        out.push(history_direction_wire(entry.direction));
        push_bytes16(&mut out, &entry.sender_person_id, "sender person id")?;
        push_fixed(
            &mut out,
            &entry.sender_device_id,
            DEVICE_ID_LEN,
            "sender device id",
        )?;
        push_bytes32(&mut out, &entry.body, "history body")?;
    }
    Ok(out)
}

/// Decode a history payload.
#[uniffi::export]
pub fn core_decode_sync_history(bytes: Vec<u8>) -> Result<SyncHistoryPayload, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let count = take_payload_header(&mut cursor)?;
    let mut entries = Vec::with_capacity(count.min(SYNC_RECORD_MAX_ENTRIES));
    for _ in 0..count {
        entries.push(SyncHistoryEntry {
            origin_msg_id: cursor.take(MSG_ID_LEN)?.to_vec(),
            direction: history_direction_of(cursor.take_u8()?)?,
            sender_person_id: cursor.take_bytes16()?,
            sender_device_id: cursor.take(DEVICE_ID_LEN)?.to_vec(),
            body: cursor.take_bytes32()?,
        });
    }
    cursor.finish()?;
    Ok(SyncHistoryPayload { entries })
}

// ---------------------------------------------------------------------------
// Payload: delivered/read watermarks (§8, KIND_SYNC_WATERMARK)
// ---------------------------------------------------------------------------

/// One chat's read state, as one device knows it.
///
/// Cumulative, exactly like [`crate::ReceiptContent`]: "delivered/read through
/// this lamport, for messages from this person". That shape is what makes the
/// merge trivial and order-independent — a sibling takes the maximum per
/// `(chat_id, subject_person_id)` — which is the property SYNC-1 needs, since
/// two devices' watermark records can arrive in either order, days apart, or
/// twice.
///
/// §8's surface rule lives one layer up: what a *contact* is shown is
/// any-device, so the value a contact sees is the maximum across the person's
/// devices, and per-device detail stays behind Advanced.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncWatermarkEntry {
    pub chat_id: Vec<u8>,
    /// Whose messages this watermark is about.
    pub subject_person_id: Vec<u8>,
    pub delivered_through_lamport: u64,
    pub read_through_lamport: u64,
}

/// Watermarks for one or more chats (§8, [`SyncRecordKind::Watermarks`]).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncWatermarkPayload {
    pub entries: Vec<SyncWatermarkEntry>,
}

/// Encode a watermark payload.
///
/// Layout: `version(1) | entry_count(u16)`, then per entry
/// `chat_id_len(u16) | chat_id | subject_person_id_len(u16) |
/// subject_person_id | delivered_through_lamport(u64) |
/// read_through_lamport(u64)`.
#[uniffi::export]
pub fn core_encode_sync_watermarks(payload: SyncWatermarkPayload) -> Result<Vec<u8>, CoreError> {
    let mut out = payload_header(payload.entries.len())?;
    for entry in &payload.entries {
        push_bytes16(&mut out, &entry.chat_id, "watermark chat id")?;
        push_bytes16(
            &mut out,
            &entry.subject_person_id,
            "watermark subject person id",
        )?;
        out.extend_from_slice(&entry.delivered_through_lamport.to_be_bytes());
        out.extend_from_slice(&entry.read_through_lamport.to_be_bytes());
    }
    Ok(out)
}

/// Decode a watermark payload.
#[uniffi::export]
pub fn core_decode_sync_watermarks(bytes: Vec<u8>) -> Result<SyncWatermarkPayload, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let count = take_payload_header(&mut cursor)?;
    let mut entries = Vec::with_capacity(count.min(SYNC_RECORD_MAX_ENTRIES));
    for _ in 0..count {
        entries.push(SyncWatermarkEntry {
            chat_id: cursor.take_bytes16()?,
            subject_person_id: cursor.take_bytes16()?,
            delivered_through_lamport: cursor.take_u64()?,
            read_through_lamport: cursor.take_u64()?,
        });
    }
    cursor.finish()?;
    Ok(SyncWatermarkPayload { entries })
}

// ---------------------------------------------------------------------------
// Payload: contacts and their rosters (§8, KIND_SYNC_CONTACTS)
// ---------------------------------------------------------------------------

/// One contact, as this person already holds them.
///
/// `card_json` is the contact's [`crate::FriendCard`] in the JSON form
/// [`crate::parse_friend_card`] already accepts — the same bytes a friend
/// request carries. Reused rather than re-specified so that a synced contact
/// lands through the existing import path, signature check included, and so
/// that a field added to a card is a field synced without touching this codec.
///
/// This is the record kind §8 means by "may contain contacts' data the person
/// already legitimately holds". It is exactly that and no more: what the person
/// was given. It never travels unsealed, never reaches a third party, and
/// widens nothing (DL-5) — the sealing model above is what makes those
/// statements true rather than aspirational.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncContactEntry {
    pub person_id: Vec<u8>,
    pub card_json: String,
    /// The contact's roster as this device believes it, or `None` for a contact
    /// whose roster has not been gossiped yet — which is every legacy contact,
    /// permanently, and is not an error.
    pub roster: Option<Roster>,
}

/// A contact list slice (§8, [`SyncRecordKind::Contacts`]).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncContactsPayload {
    pub entries: Vec<SyncContactEntry>,
}

/// Encode a contacts payload.
///
/// Layout: `version(1) | entry_count(u16)`, then per entry
/// `person_id_len(u16) | person_id | card_json_len(u32) | card_json_utf8 |
/// roster_len(u32) | roster`, where a zero-length roster means "none".
#[uniffi::export]
pub fn core_encode_sync_contacts(payload: SyncContactsPayload) -> Result<Vec<u8>, CoreError> {
    let mut out = payload_header(payload.entries.len())?;
    for entry in &payload.entries {
        push_bytes16(&mut out, &entry.person_id, "contact person id")?;
        push_bytes32(&mut out, entry.card_json.as_bytes(), "contact card")?;
        let roster_bytes = match &entry.roster {
            Some(roster) => core_encode_roster(roster.clone())?,
            None => Vec::new(),
        };
        push_bytes32(&mut out, &roster_bytes, "contact roster")?;
    }
    Ok(out)
}

/// Decode a contacts payload.
#[uniffi::export]
pub fn core_decode_sync_contacts(bytes: Vec<u8>) -> Result<SyncContactsPayload, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let count = take_payload_header(&mut cursor)?;
    let mut entries = Vec::with_capacity(count.min(SYNC_RECORD_MAX_ENTRIES));
    for _ in 0..count {
        let person_id = cursor.take_bytes16()?;
        let card_bytes = cursor.take_bytes32()?;
        let card_json = String::from_utf8(card_bytes)
            .map_err(|e| CoreError::Malformed(format!("synced contact card is not UTF-8: {e}")))?;
        let roster_bytes = cursor.take_bytes32()?;
        let roster = if roster_bytes.is_empty() {
            None
        } else {
            Some(core_decode_roster(roster_bytes)?)
        };
        entries.push(SyncContactEntry {
            person_id,
            card_json,
            roster,
        });
    }
    cursor.finish()?;
    Ok(SyncContactsPayload { entries })
}

// ---------------------------------------------------------------------------
// Payload: own roster and inbox keys (§8, KIND_SYNC_OWN_ROSTER)
// ---------------------------------------------------------------------------

/// The person's own roster plus the inbox keys that go with it (§6, §8).
///
/// This is the one payload that carries secret material, and it is the reason
/// the whole boundary above is drawn the way it is: an inbox key sealed to
/// anything but an own device would hand a stranger the ability to read every
/// subsequent sync record, permanently.
///
/// `inbox_keys` is a list rather than a single key because a device that has
/// been offline across a §10 rotation still needs the older generation to open
/// records sealed before it — a DTN store does not get to assume the newest key
/// is the only one in flight. Callers send the generations a sibling could
/// plausibly still need and no more; the roster's own
/// [`Roster::inbox_key_generation`] names the current one.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncOwnRosterPayload {
    pub roster: Roster,
    pub inbox_keys: Vec<InboxKey>,
}

/// Encode an own-roster payload.
///
/// Layout: `version(1) | key_count(u16) | roster_len(u32) | roster`, then per
/// key `generation(u64) | agree_pk(32) | agree_sk(32)`.
#[uniffi::export]
pub fn core_encode_sync_own_roster(payload: SyncOwnRosterPayload) -> Result<Vec<u8>, CoreError> {
    let mut out = payload_header(payload.inbox_keys.len())?;
    push_bytes32(&mut out, &core_encode_roster(payload.roster)?, "own roster")?;
    for key in &payload.inbox_keys {
        out.extend_from_slice(&key.generation.to_be_bytes());
        push_fixed(&mut out, &key.agree_pk, KEY_LEN, "inbox key public half")?;
        push_fixed(&mut out, &key.agree_sk, KEY_LEN, "inbox key secret half")?;
    }
    Ok(out)
}

/// Decode an own-roster payload.
#[uniffi::export]
pub fn core_decode_sync_own_roster(bytes: Vec<u8>) -> Result<SyncOwnRosterPayload, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let count = take_payload_header(&mut cursor)?;
    let roster = core_decode_roster(cursor.take_bytes32()?)?;
    let mut inbox_keys = Vec::with_capacity(count.min(SYNC_RECORD_MAX_ENTRIES));
    for _ in 0..count {
        inbox_keys.push(InboxKey {
            generation: cursor.take_u64()?,
            agree_pk: cursor.take(KEY_LEN)?.to_vec(),
            agree_sk: cursor.take(KEY_LEN)?.to_vec(),
        });
    }
    cursor.finish()?;
    Ok(SyncOwnRosterPayload { roster, inbox_keys })
}

// ---------------------------------------------------------------------------
// Payload: group membership and state (§8, KIND_SYNC_GROUPS)
// ---------------------------------------------------------------------------

/// Group state for a sibling (§8, [`SyncRecordKind::Groups`]).
///
/// §11 leaves group crypto untouched in v1: `member_user_ids` stay person ids
/// and the group keeps its shared symmetric key. This record is how a member's
/// new device obtains that key and the membership snapshot — through the
/// member's own self-sync, with no re-invites and no M×D sender-side fan-out.
/// Each group is carried in exactly the bytes
/// [`crate::encode_group_invite_content`] already produces, so a synced group
/// and an invited group are the same document by construction.
///
/// **The metadata revision rides alongside those bytes rather than inside
/// them**, and both halves of that sentence are deliberate. An invite is a
/// document handed to somebody who has never seen the group, so it has no
/// revision to state and its format is not this record's to change. But
/// `(metadata_revision, metadata_changed_by)` is exactly the pair
/// [`crate::apply_group_metadata_update`] and the store's group upsert use to
/// decide a name conflict, and a record that dropped it would hand a sibling a
/// group at revision 0 — which the sibling's own store then *correctly* refuses
/// as older than what it holds. The visible symptom is a rename that converges
/// once and then never again: the fleet sits on two names, each device certain
/// its own is newer. So the pair travels, and the merge on the far side is the
/// shipped one rather than a second rule invented here.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SyncGroupsPayload {
    pub groups: Vec<Group>,
}

/// Encode a groups payload.
///
/// Layout: `version(1) | group_count(u16)`, then per group
/// `group_len(u32) | encode_group_invite_content(group) |
/// metadata_revision(u64) | changed_by_len(u16) | metadata_changed_by`.
#[uniffi::export]
pub fn core_encode_sync_groups(payload: SyncGroupsPayload) -> Result<Vec<u8>, CoreError> {
    let mut out = payload_header(payload.groups.len())?;
    for group in payload.groups {
        let metadata_revision = group.metadata_revision;
        let metadata_changed_by = group.metadata_changed_by.clone();
        push_bytes32(
            &mut out,
            &encode_group_invite_content(group)?,
            "synced group",
        )?;
        out.extend_from_slice(&metadata_revision.to_be_bytes());
        push_bytes16(&mut out, &metadata_changed_by, "group metadata author")?;
    }
    Ok(out)
}

/// Decode a groups payload.
#[uniffi::export]
pub fn core_decode_sync_groups(bytes: Vec<u8>) -> Result<SyncGroupsPayload, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let count = take_payload_header(&mut cursor)?;
    let mut groups = Vec::with_capacity(count.min(SYNC_RECORD_MAX_ENTRIES));
    for _ in 0..count {
        let mut group = decode_group_invite_content(cursor.take_bytes32()?)?;
        group.metadata_revision = cursor.take_u64()?;
        group.metadata_changed_by = cursor.take_bytes16()?;
        groups.push(group);
    }
    cursor.finish()?;
    Ok(SyncGroupsPayload { groups })
}

// ---------------------------------------------------------------------------
// Payload: shared settings (§8, KIND_SYNC_SETTINGS)
// ---------------------------------------------------------------------------

/// One shared setting, resolved by a **total order** so two devices writing the
/// same key converge whatever order the records arrive in.
///
/// `key` is an application-defined identifier and `value` is opaque to core: §8
/// says "settings the product deems shared", and which those are is a product
/// decision that must be changeable without a wire change.
///
/// The merge rule is `(epoch, author_device_id, value)`, compared in that order,
/// highest wins. `epoch` alone — the shape [`crate::ProfileSyncContent`] uses —
/// is *not* enough here and that is the whole reason this field exists. A
/// profile has one author; a person's shared settings have as many authors as
/// the person has devices, and two of them writing the same key in the same
/// millisecond is not a contrived case: a shell stamps `epoch` from `now_ms`,
/// and the phone and the tablet toggling a setting inside one minute of each
/// other while both offline is an ordinary afternoon. With `epoch` alone each
/// device would keep its own value forever — every incoming record losing the
/// strictly-greater test — and the fleet would sit permanently forked on a
/// difference neither device could see.
///
/// `author_device_id` is that tie's arbiter because it is the one field both
/// devices already agree on and neither can choose to win with: it is derived
/// from the signing key in [`core_sign_sync_record`]. `value` breaks the
/// remaining tie (one device re-publishing another's entry), so the order is
/// total rather than merely usually-decisive.
///
/// It travels on the wire rather than being stamped by the receiver, and that
/// distinction is load-bearing: a device that re-published a sibling's entry
/// under its *own* id would make the winner depend on who spoke last, which is
/// exactly the property a total order is supposed to remove.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncSettingEntry {
    pub key: String,
    pub value: Vec<u8>,
    pub epoch: u64,
    /// The 16-byte id of the device that wrote this value —
    /// [`LEGACY_DEVICE_ID`] on an install that has never linked, which is a
    /// legitimate participant in the order and not a placeholder.
    pub author_device_id: Vec<u8>,
}

impl SyncSettingEntry {
    /// This entry's position in the total order above. Borrowed rather than
    /// cloned: the comparison runs once per applied entry and the values can be
    /// as large as a draft.
    pub(crate) fn order_key(&self) -> (u64, &[u8], &[u8]) {
        (self.epoch, &self.author_device_id, &self.value)
    }

    /// Whether this entry supersedes `stored` under the total order. Equal
    /// entries do not: re-applying an identical entry must report "nothing
    /// changed" so SYNC-1's routine re-offers stay free.
    pub(crate) fn supersedes(&self, stored: &SyncSettingEntry) -> bool {
        self.order_key() > stored.order_key()
    }
}

/// Shared settings (§8, [`SyncRecordKind::Settings`]).
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncSettingsPayload {
    pub entries: Vec<SyncSettingEntry>,
}

/// Encode a settings payload.
///
/// Layout: `version(1) | entry_count(u16)`, then per entry
/// `key_len(u16) | key_utf8 | epoch(u64) | author_device_id(16) |
/// value_len(u32) | value`.
#[uniffi::export]
pub fn core_encode_sync_settings(payload: SyncSettingsPayload) -> Result<Vec<u8>, CoreError> {
    let mut out = payload_header(payload.entries.len())?;
    for entry in &payload.entries {
        push_bytes16(&mut out, entry.key.as_bytes(), "setting key")?;
        out.extend_from_slice(&entry.epoch.to_be_bytes());
        push_fixed(
            &mut out,
            &entry.author_device_id,
            DEVICE_ID_LEN,
            "setting author device id",
        )?;
        push_bytes32(&mut out, &entry.value, "setting value")?;
    }
    Ok(out)
}

/// Decode a settings payload.
#[uniffi::export]
pub fn core_decode_sync_settings(bytes: Vec<u8>) -> Result<SyncSettingsPayload, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let count = take_payload_header(&mut cursor)?;
    let mut entries = Vec::with_capacity(count.min(SYNC_RECORD_MAX_ENTRIES));
    for _ in 0..count {
        let key_bytes = cursor.take_bytes16()?;
        let key = String::from_utf8(key_bytes)
            .map_err(|e| CoreError::Malformed(format!("setting key is not UTF-8: {e}")))?;
        entries.push(SyncSettingEntry {
            key,
            epoch: cursor.take_u64()?,
            author_device_id: cursor.take(DEVICE_ID_LEN)?.to_vec(),
            value: cursor.take_bytes32()?,
        });
    }
    cursor.finish()?;
    Ok(SyncSettingsPayload { entries })
}

// ---------------------------------------------------------------------------
// Shared codec helpers
// ---------------------------------------------------------------------------

/// Every payload in this module opens the same way: one version byte and one
/// u16 count. One shape means one place to bounds-check the count, and it is
/// what lets a reader of any of the six codecs above know the first three bytes
/// without reading them.
fn payload_header(count: usize) -> Result<Vec<u8>, CoreError> {
    if count > SYNC_RECORD_MAX_ENTRIES {
        return Err(CoreError::Malformed(format!(
            "sync payload has {count} entries, over the {SYNC_RECORD_MAX_ENTRIES} limit"
        )));
    }
    let mut out = Vec::with_capacity(3);
    out.push(SYNC_RECORD_VERSION);
    out.extend_from_slice(&(count as u16).to_be_bytes());
    Ok(out)
}

fn take_payload_header(cursor: &mut Cursor<'_>) -> Result<usize, CoreError> {
    let version = cursor.take_u8()?;
    if version != SYNC_RECORD_VERSION {
        return Err(CoreError::Malformed(format!(
            "unsupported sync payload version {version}"
        )));
    }
    let count = cursor.take_u16()? as usize;
    if count > SYNC_RECORD_MAX_ENTRIES {
        return Err(CoreError::Malformed(format!(
            "sync payload claims {count} entries, over the {SYNC_RECORD_MAX_ENTRIES} limit"
        )));
    }
    Ok(count)
}

fn push_bytes16(out: &mut Vec<u8>, bytes: &[u8], field: &str) -> Result<(), CoreError> {
    if bytes.len() > u16::MAX as usize {
        return Err(CoreError::Malformed(format!(
            "{field} is too long to encode"
        )));
    }
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

fn push_bytes32(out: &mut Vec<u8>, bytes: &[u8], field: &str) -> Result<(), CoreError> {
    if bytes.len() > u32::MAX as usize {
        return Err(CoreError::Malformed(format!(
            "{field} is too long to encode"
        )));
    }
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

/// Write a fixed-width field, refusing the wrong width rather than emitting
/// bytes a decoder would have to reject — the same discipline
/// `protocol.rs`'s `push_extension` keeps.
fn push_fixed(
    out: &mut Vec<u8>,
    bytes: &[u8],
    expected_len: usize,
    field: &str,
) -> Result<(), CoreError> {
    if bytes.len() != expected_len {
        return Err(CoreError::Malformed(format!(
            "{field} must be exactly {expected_len} bytes"
        )));
    }
    out.extend_from_slice(bytes);
    Ok(())
}

/// A bounds-checked cursor, kept privately here as every other codec in this
/// crate keeps one, so a truncated payload is a [`CoreError::Malformed`] and
/// never a panic.
struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
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
                    "truncated sync payload: need {n} more byte(s) at offset {}",
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

    fn take_u64(&mut self) -> Result<u64, CoreError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("exactly 8 bytes"),
        ))
    }

    fn take_i64(&mut self) -> Result<i64, CoreError> {
        Ok(i64::from_be_bytes(
            self.take(8)?.try_into().expect("exactly 8 bytes"),
        ))
    }

    fn take_bytes16(&mut self) -> Result<Vec<u8>, CoreError> {
        let len = self.take_u16()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn take_bytes32(&mut self) -> Result<Vec<u8>, CoreError> {
        let len = u32::from_be_bytes(self.take(4)?.try_into().expect("exactly 4 bytes")) as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn finish(self) -> Result<(), CoreError> {
        if self.pos != self.data.len() {
            return Err(CoreError::Malformed(format!(
                "{} unexpected trailing byte(s) after the sync payload",
                self.data.len() - self.pos
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_roster::{
        core_sign_device_cert, core_sign_roster, DeviceCert, DeviceTombstone,
        DEVICE_CERT_FLAG_ROSTER_SIGNING,
    };
    use crate::identity::derive_user_id;
    use crate::protocol::KIND_SYNC_HISTORY;
    use crate::protocol::{
        core_is_hidden_spray_kind, core_is_sync_record_kind, core_kind_persists_msg_id_row,
        encode_message_body_extended, MessageBody, KIND_ATTACHMENT_CHUNK, KIND_ATTACHMENT_MANIFEST,
        KIND_TEXT,
    };
    use crate::{core_pairwise_sender_authorized, create_group, make_friend_card, open_message};
    use ed25519_dalek::SigningKey;

    /// Fixed keys, never `generate_*`: the golden vectors below are only worth
    /// anything if every byte that feeds them is pinned here. Same discipline
    /// as `device_roster.rs`'s certificate and roster vectors.
    const ROOT_SK: [u8; 32] = [0x11; 32];
    const ROOT_AGREE_SK: [u8; 32] = [0x1a; 32];
    const DEVICE_A_SK: [u8; 32] = [0x22; 32];
    const DEVICE_B_SK: [u8; 32] = [0x33; 32];
    const DEVICE_A_AGREE_PK: [u8; 32] = [0x44; 32];
    const DEVICE_B_AGREE_PK: [u8; 32] = [0x55; 32];
    const STRANGER_SK: [u8; 32] = [0x66; 32];
    const INBOX_SK: [u8; 32] = [0x77; 32];
    const CONTACT_SK: [u8; 32] = [0x99; 32];
    const CONTACT_AGREE_SK: [u8; 32] = [0x9a; 32];

    fn sign_pk(sk: &[u8; 32]) -> Vec<u8> {
        SigningKey::from_bytes(sk)
            .verifying_key()
            .as_bytes()
            .to_vec()
    }

    fn agree_pk_of(sk: &[u8; 32]) -> Vec<u8> {
        XPublicKey::from(&StaticSecret::from(*sk))
            .as_bytes()
            .to_vec()
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn person_id() -> Vec<u8> {
        derive_user_id(&sign_pk(&ROOT_SK)).to_vec()
    }

    /// The person's own identity — the key the outer sealed envelope is signed
    /// with, exactly as it is for a text message.
    fn person() -> Identity {
        Identity {
            user_id: person_id(),
            sign_pk: sign_pk(&ROOT_SK),
            sign_sk: ROOT_SK.to_vec(),
            agree_pk: agree_pk_of(&ROOT_AGREE_SK),
            agree_sk: ROOT_AGREE_SK.to_vec(),
        }
    }

    fn contact() -> Identity {
        Identity {
            user_id: derive_user_id(&sign_pk(&CONTACT_SK)).to_vec(),
            sign_pk: sign_pk(&CONTACT_SK),
            sign_sk: CONTACT_SK.to_vec(),
            agree_pk: agree_pk_of(&CONTACT_AGREE_SK),
            agree_sk: CONTACT_AGREE_SK.to_vec(),
        }
    }

    /// The person's inbox key at generation 3, from fixed material so the
    /// vectors that mention a generation stay reproducible.
    fn inbox_key() -> InboxKey {
        InboxKey {
            generation: 3,
            agree_pk: agree_pk_of(&INBOX_SK),
            agree_sk: INBOX_SK.to_vec(),
        }
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

    fn device_a_id() -> Vec<u8> {
        derive_user_id(&sign_pk(&DEVICE_A_SK)).to_vec()
    }

    /// A two-device own roster: A holds the roster-signing role, B is the
    /// sibling every SYNC-3 test seals toward.
    fn own_roster() -> Roster {
        let devices = vec![
            cert(
                &DEVICE_A_SK,
                &DEVICE_A_AGREE_PK,
                DEVICE_CERT_FLAG_ROSTER_SIGNING,
                &ROOT_SK,
            ),
            cert(&DEVICE_B_SK, &DEVICE_B_AGREE_PK, 0, &DEVICE_A_SK),
        ];
        core_sign_roster(
            Roster {
                person_id: person_id(),
                recovery_epoch: 1,
                seq: 7,
                devices,
                tombstones: Vec::new(),
                approving_device_id: device_a_id(),
                inbox_key_generation: 3,
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
            },
            DEVICE_A_SK.to_vec(),
        )
        .expect("fixed-key roster signs")
    }

    fn roster_version() -> RosterVersion {
        RosterVersion {
            recovery_epoch: 1,
            seq: 7,
        }
    }

    /// An unsigned record on device A's Watermarks stream, with a fixed
    /// payload, ready for [`core_sign_sync_record`].
    fn unsigned_record(kind: SyncRecordKind, payload: Vec<u8>) -> SyncRecord {
        SyncRecord {
            kind,
            person_id: person_id(),
            // Deliberately wrong: signing must overwrite it from the key.
            author_device_id: LEGACY_DEVICE_ID.to_vec(),
            roster_version: roster_version(),
            inbox_key_generation: 3,
            stream_seq: 42,
            timestamp_ms: 1_700_000_000_000,
            payload,
            signature: Vec::new(),
        }
    }

    fn signed_record(kind: SyncRecordKind, payload: Vec<u8>) -> SyncRecord {
        core_sign_sync_record(unsigned_record(kind, payload), DEVICE_A_SK.to_vec())
            .expect("fixed-key sync record signs")
    }

    fn watermark_payload() -> Vec<u8> {
        core_encode_sync_watermarks(SyncWatermarkPayload {
            entries: vec![SyncWatermarkEntry {
                chat_id: vec![0xAB; 16],
                subject_person_id: vec![0xCD; 16],
                delivered_through_lamport: 19,
                read_through_lamport: 12,
            }],
        })
        .expect("watermark payload encodes")
    }

    // -----------------------------------------------------------------------
    // Golden vectors: every format this module introduces, frozen
    // -----------------------------------------------------------------------

    /// The sync record header and its device signature, byte for byte.
    ///
    /// The *sealed* bytes are deliberately not pinned: `seal_message` draws a
    /// fresh ephemeral key and nonce per call (DESIGN.md §6.3), so a sealed
    /// record is never twice the same bytes. What has to be frozen is what both
    /// devices compute deterministically — the record, its signature, and the
    /// stream-slot id.
    #[test]
    fn sync_record_golden_vectors() {
        let record = signed_record(SyncRecordKind::Watermarks, watermark_payload());

        // Layout: version ‖ kind ‖ len(person_id)‖person_id ‖
        // len(author_device_id)‖author_device_id ‖ recovery_epoch ‖ seq ‖
        // inbox_key_generation ‖ stream_seq ‖ timestamp_ms ‖ len32(payload)‖
        // payload.
        const SIGNED_BYTES: &str = "010b0010c0c5ecd7f1ee33f526dd27d34c3e1daa00104d3ae03a986747b7644cbf85c2434951000000000000000100000000000000070000000000000003000000000000002a0000018bcfe56800000000370100010010abababababababababababababababab0010cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd0000000000000013000000000000000c";
        const SIGNATURE: &str = "b9c8e05f668ca9b657ef026c37b4daeae38791cab495683b7c63599d648a32eac2e1d8b9d1521ba8c1399745a7b38a3fed80e9e31fbcfc86c0e4d737757d470e";
        const ENCODED: &str = "010b0010c0c5ecd7f1ee33f526dd27d34c3e1daa00104d3ae03a986747b7644cbf85c2434951000000000000000100000000000000070000000000000003000000000000002a0000018bcfe56800000000370100010010abababababababababababababababab0010cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd0000000000000013000000000000000c0040b9c8e05f668ca9b657ef026c37b4daeae38791cab495683b7c63599d648a32eac2e1d8b9d1521ba8c1399745a7b38a3fed80e9e31fbcfc86c0e4d737757d470e";
        const RECORD_ID: &str = "b120da774e21ff8189fa5b3c894d9dda";

        assert_eq!(
            hex(&sync_record_signed_bytes(&record).expect("signed bytes")),
            SIGNED_BYTES
        );
        assert_eq!(hex(&record.signature), SIGNATURE);
        assert_eq!(
            hex(&core_encode_sync_record(record.clone()).expect("encodes")),
            ENCODED
        );
        assert_eq!(
            hex(&core_sync_record_id(
                person_id(),
                device_a_id(),
                SyncRecordKind::Watermarks,
                42
            )),
            RECORD_ID
        );
    }

    /// One vector per §8 record kind's payload, so a field-order or framing
    /// change fails here instead of quietly desynchronizing two of a person's
    /// devices — a failure that would otherwise surface days later, as history
    /// that will not converge.
    #[test]
    fn sync_payload_golden_vectors() {
        // History: version ‖ count ‖ (origin_msg_id(16) ‖ direction ‖
        // len(sender_person_id)‖sender_person_id ‖ sender_device_id(16) ‖
        // len32(body)‖body)*
        const HISTORY: &str = "01000201010101010101010101010101010101010010c0c5ecd7f1ee33f526dd27d34c3e1daa4d3ae03a986747b7644cbf85c24349510000004f010010abababababababababababababababab00000000000000050000018bcfe568000000001573656520796f7520617420746865206275666665742000104d3ae03a986747b7644cbf85c243495102020202020202020202020202020202020010cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd0000000000000000000000000000000000000030010010abababababababababababababababab00000000000000060000018bcfe56be8000000096f6e206d7920776179";
        // Watermarks: version ‖ count ‖ (len(chat_id)‖chat_id ‖
        // len(subject)‖subject ‖ delivered ‖ read)*
        const WATERMARKS: &str = "0100010010abababababababababababababababab0010cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd0000000000000013000000000000000c";
        // Contacts: version ‖ count ‖ (len(person_id)‖person_id ‖
        // len32(card)‖card ‖ len32(roster)‖roster)*
        const CONTACTS: &str = "0100010010c1975ba225e58bf4ed248c85bc3de3400000000e7b226e616d65223a22426f62227d00000000";
        // Own roster: version ‖ key count ‖ len32(roster)‖roster ‖
        // (generation ‖ agree_pk(32) ‖ agree_sk(32))*
        const OWN_ROSTER: &str = "01000100000231010010c0c5ecd7f1ee33f526dd27d34c3e1daa0000000000000001000000000000000700020010c0c5ecd7f1ee33f526dd27d34c3e1daa0020a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f0002044444444444444444444444444444444444444444444444444444444444444440000000000000001000000010020d04ab232742bb4ab3a1368bd4615e4e6d0224ab71a016baf8520a332c97787370040209755df19023c6622e38972314e986ce4dd6b90a96f73239ba276e78ab79becf8b3e8b7efcb215ffda887ebf724c62bdbccf6c72415cba995adfda213e280050010c0c5ecd7f1ee33f526dd27d34c3e1daa002017cb79fb2b4120f2b1ec65e4198d6e08b28e813feb01e4a400839b85e18080ce002055555555555555555555555555555555555555555555555555555555555555550000000000000001000000000020a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f000407f388f6fe17fce1ccc350c7eb2391c0f3c3b3fdc67d7e9756e4c078867c13f13e0e0a438561bd5298fe03670f72584e20efbc72d0112e53cfffa2b35056b7605000000104d3ae03a986747b7644cbf85c243495100000000000000030020a09aa5f47a6759802ff955f8dc2d2a14a5c99d23be97f864127ff9383455a4f00040b5adeac88f0f4174d9b6df4e7c337abd34f99932ac50dabd8c900d09b2177f9d9f71cb0acbb04409a4a98f5d2046970154ddc6e2e6aed7fad220574934c40a0b00000000000000031cf579aba45a10ba1d1ef06d91fca2aa9ed0a1150515653155405d0b18cb9a677777777777777777777777777777777777777777777777777777777777777777";
        // Groups: version ‖ count ‖ (len32(invite content)‖invite content ‖
        // metadata_revision ‖ len(changed_by)‖changed_by)*
        const GROUPS: &str = "0100010000005ee1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e1e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2e2000643727569736500020010c0c5ecd7f1ee33f526dd27d34c3e1daa0010cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd00000000000000000000";
        // Settings: version ‖ count ‖ (len(key)‖key ‖ epoch ‖
        // author_device_id(16) ‖ len32(value)‖value)*
        const SETTINGS: &str = "01000100196e6f74696669636174696f6e732e71756965745f686f75727300000000000000094d3ae03a986747b7644cbf85c24349510000000101";

        assert_eq!(hex(&golden_history()), HISTORY);
        assert_eq!(hex(&watermark_payload()), WATERMARKS);
        assert_eq!(hex(&golden_contacts()), CONTACTS);
        assert_eq!(hex(&golden_own_roster()), OWN_ROSTER);
        assert_eq!(hex(&golden_groups()), GROUPS);
        assert_eq!(hex(&golden_settings()), SETTINGS);
    }

    fn golden_history() -> Vec<u8> {
        core_encode_sync_history(SyncHistoryPayload {
            entries: vec![
                SyncHistoryEntry {
                    origin_msg_id: vec![0x01; MSG_ID_LEN],
                    direction: SyncHistoryDirection::Authored,
                    sender_person_id: person_id(),
                    sender_device_id: device_a_id(),
                    body: encode_message_body_extended(
                        MessageBody {
                            kind: KIND_TEXT,
                            chat_id: vec![0xAB; 16],
                            lamport: 5,
                            timestamp: 1_700_000_000_000,
                            content: b"see you at the buffet".to_vec(),
                        },
                        None,
                        Some(device_a_id()),
                        None,
                    )
                    .expect("body encodes"),
                },
                SyncHistoryEntry {
                    origin_msg_id: vec![0x02; MSG_ID_LEN],
                    direction: SyncHistoryDirection::Received,
                    sender_person_id: vec![0xCD; 16],
                    // A v1 peer: §5's synthetic one-device stream, forever.
                    sender_device_id: LEGACY_DEVICE_ID.to_vec(),
                    body: encode_message_body_extended(
                        MessageBody {
                            kind: KIND_TEXT,
                            chat_id: vec![0xAB; 16],
                            lamport: 6,
                            timestamp: 1_700_000_001_000,
                            content: b"on my way".to_vec(),
                        },
                        None,
                        None,
                        None,
                    )
                    .expect("body encodes"),
                },
            ],
        })
        .expect("history payload encodes")
    }

    /// A short synthetic card, not a real one: `identity.rs` already freezes
    /// the friend-card format, and what this vector is about is *this* codec's
    /// framing. The real card gets exercised where it matters, in the DL-5 test
    /// below.
    fn golden_contacts() -> Vec<u8> {
        core_encode_sync_contacts(SyncContactsPayload {
            entries: vec![SyncContactEntry {
                person_id: contact().user_id,
                card_json: "{\"name\":\"Bob\"}".to_string(),
                roster: None,
            }],
        })
        .expect("contacts payload encodes")
    }

    /// The same payload carrying a real, self-signed card — relay endpoint
    /// included — for the boundary test.
    fn contacts_payload_with_real_card() -> Vec<u8> {
        core_encode_sync_contacts(SyncContactsPayload {
            entries: vec![SyncContactEntry {
                person_id: contact().user_id,
                card_json: contact_card(),
                roster: None,
            }],
        })
        .expect("contacts payload encodes")
    }

    fn contact_card() -> String {
        make_friend_card(
            "Bob".to_string(),
            contact(),
            Some("https://relay.example".to_string()),
            None,
        )
        .expect("fixed-key card")
    }

    fn golden_own_roster() -> Vec<u8> {
        core_encode_sync_own_roster(SyncOwnRosterPayload {
            roster: own_roster(),
            inbox_keys: vec![inbox_key()],
        })
        .expect("own roster payload encodes")
    }

    fn golden_groups() -> Vec<u8> {
        // A fixed group, not `create_group`'s random id/key: the vector has to
        // be reproducible.
        let mut group = create_group("Cruise".to_string(), vec![person_id(), vec![0xCD; 16]])
            .expect("group validates");
        group.id = vec![0xE1; 16];
        group.key = vec![0xE2; 32];
        core_encode_sync_groups(SyncGroupsPayload {
            groups: vec![group],
        })
        .expect("groups payload encodes")
    }

    fn golden_settings() -> Vec<u8> {
        core_encode_sync_settings(SyncSettingsPayload {
            entries: vec![SyncSettingEntry {
                key: "notifications.quiet_hours".to_string(),
                value: vec![0x01],
                epoch: 9,
                author_device_id: device_a_id(),
            }],
        })
        .expect("settings payload encodes")
    }

    // -----------------------------------------------------------------------
    // Kinds
    // -----------------------------------------------------------------------

    /// Every sync kind, including the digest that could not fit the 10..=15
    /// block. The set is deliberately not a range any more, so this iterates
    /// the enum rather than the numbers.
    const ALL_SYNC_KINDS: [SyncRecordKind; 7] = [
        SyncRecordKind::History,
        SyncRecordKind::Watermarks,
        SyncRecordKind::Contacts,
        SyncRecordKind::OwnRoster,
        SyncRecordKind::Groups,
        SyncRecordKind::Settings,
        SyncRecordKind::Digest,
    ];

    #[test]
    fn every_record_kind_maps_both_ways_and_collides_with_nothing() {
        let mut wires = Vec::new();
        for kind in ALL_SYNC_KINDS {
            let wire = core_sync_record_kind_wire(kind);
            assert_eq!(core_sync_record_kind_of(wire), Some(kind));
            assert!(core_is_sync_record_kind(wire));
            assert!(!wires.contains(&wire), "kind byte {wire} is used twice");
            wires.push(wire);
        }
        assert_eq!(wires, vec![10, 11, 12, 13, 14, 15, 20]);
        // The reserved attachment kinds sit between the record block and the
        // digest; none of the three may drift into another.
        for kind in [
            KIND_TEXT,
            KIND_ATTACHMENT_MANIFEST,
            KIND_ATTACHMENT_CHUNK,
            18,
            19,
        ] {
            assert!(!core_is_sync_record_kind(kind));
            assert_eq!(core_sync_record_kind_of(kind), None);
        }
    }

    /// A digest is a sync kind in every respect but one, and that one is what
    /// keeps anti-entropy from chasing its own control channel.
    #[test]
    fn the_digest_is_the_only_kind_that_is_not_a_stream() {
        for kind in ALL_SYNC_KINDS {
            assert_eq!(
                core_sync_kind_is_stream(kind),
                kind != SyncRecordKind::Digest
            );
        }
    }

    /// A sync kind never reaches a contact, so it is neither a hidden spray
    /// kind nor a kind that leaves a `msg_id` row. Both answers are decisions
    /// (see those functions' docs); pinning them here means changing one is a
    /// deliberate edit rather than a side effect.
    #[test]
    fn sync_kinds_leave_no_msg_id_row_and_are_not_spray_kinds() {
        for kind in ALL_SYNC_KINDS.map(core_sync_record_kind_wire) {
            assert!(core_is_sync_record_kind(kind));
            assert!(!core_kind_persists_msg_id_row(kind));
            assert!(!core_is_hidden_spray_kind(kind));
        }
    }

    /// SYNC-3 as an accept rule: own devices only, never a contact, however
    /// well authenticated that contact is.
    #[test]
    fn only_this_persons_own_devices_may_send_a_sync_record() {
        for kind in ALL_SYNC_KINDS.map(core_sync_record_kind_wire) {
            assert!(core_pairwise_sender_authorized(kind, false, true));
            assert!(!core_pairwise_sender_authorized(kind, true, false));
            assert!(!core_pairwise_sender_authorized(kind, false, false));
        }
    }

    // -----------------------------------------------------------------------
    // Codec round-trips and hostile input
    // -----------------------------------------------------------------------

    #[test]
    fn every_payload_round_trips() {
        let history = golden_history();
        assert_eq!(
            core_encode_sync_history(core_decode_sync_history(history.clone()).expect("decodes"))
                .expect("re-encodes"),
            history
        );
        let watermarks = watermark_payload();
        assert_eq!(
            core_encode_sync_watermarks(
                core_decode_sync_watermarks(watermarks.clone()).expect("decodes")
            )
            .expect("re-encodes"),
            watermarks
        );
        let contacts = golden_contacts();
        assert_eq!(
            core_encode_sync_contacts(
                core_decode_sync_contacts(contacts.clone()).expect("decodes")
            )
            .expect("re-encodes"),
            contacts
        );
        let own_roster = golden_own_roster();
        assert_eq!(
            core_encode_sync_own_roster(
                core_decode_sync_own_roster(own_roster.clone()).expect("decodes")
            )
            .expect("re-encodes"),
            own_roster
        );
        let groups = golden_groups();
        assert_eq!(
            core_encode_sync_groups(core_decode_sync_groups(groups.clone()).expect("decodes"))
                .expect("re-encodes"),
            groups
        );
        let settings = golden_settings();
        assert_eq!(
            core_encode_sync_settings(
                core_decode_sync_settings(settings.clone()).expect("decodes")
            )
            .expect("re-encodes"),
            settings
        );
    }

    #[test]
    fn a_record_round_trips_and_verifies() {
        let record = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        let encoded = core_encode_sync_record(record.clone()).expect("encodes");
        assert_eq!(core_decode_sync_record(encoded).expect("decodes"), record);
        core_verify_sync_record(record, sign_pk(&DEVICE_A_SK)).expect("signature verifies");
    }

    /// Signing derives the author from the key, so a caller cannot file a
    /// record on another device's stream — including the legacy stream this
    /// fixture deliberately starts on.
    #[test]
    fn signing_fills_in_the_author_device_id_from_the_key() {
        let unsigned = unsigned_record(SyncRecordKind::Watermarks, watermark_payload());
        assert_eq!(unsigned.author_device_id, LEGACY_DEVICE_ID.to_vec());
        let signed = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        assert_eq!(signed.author_device_id, device_a_id());
    }

    #[test]
    fn truncated_and_trailing_bytes_are_refused_everywhere() {
        let record = core_encode_sync_record(signed_record(
            SyncRecordKind::Watermarks,
            watermark_payload(),
        ))
        .expect("encodes");
        let mut trailing = record.clone();
        trailing.push(0);
        assert!(matches!(
            core_decode_sync_record(trailing),
            Err(CoreError::Malformed(_))
        ));
        assert!(matches!(
            core_decode_sync_record(record[..record.len() - 1].to_vec()),
            Err(CoreError::Malformed(_))
        ));
        let payload = watermark_payload();
        assert!(matches!(
            core_decode_sync_watermarks(payload[..payload.len() - 1].to_vec()),
            Err(CoreError::Malformed(_))
        ));
    }

    /// A record kind a *future* build invents fails closed here rather than
    /// being misfiled onto a stream this build does understand.
    #[test]
    fn an_unknown_record_kind_fails_closed() {
        let mut encoded = core_encode_sync_record(signed_record(
            SyncRecordKind::Watermarks,
            watermark_payload(),
        ))
        .expect("encodes");
        encoded[1] = 21;
        assert!(matches!(
            core_decode_sync_record(encoded),
            Err(CoreError::Malformed(_))
        ));
    }

    // -----------------------------------------------------------------------
    // SYNC-3: the person boundary
    // -----------------------------------------------------------------------

    #[test]
    fn a_sibling_opens_what_this_device_sealed() {
        let record = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        let sealed = core_seal_sync_record(record.clone(), person(), inbox_key())
            .expect("seals to the person's own device set");
        assert_eq!(sealed.sealed_for, roster_version());
        assert_eq!(sealed.inbox_key_generation, 3);
        assert_eq!(
            core_open_sync_record(sealed.sealed, inbox_key(), own_roster()).expect("sibling opens"),
            record
        );
    }

    /// The crypto half of SYNC-3: a device outside the person boundary holds no
    /// inbox key, so it holds nothing that opens a sync record.
    #[test]
    fn a_record_never_opens_for_a_non_own_device() {
        let sealed = core_seal_sync_record(
            signed_record(SyncRecordKind::Watermarks, watermark_payload()),
            person(),
            inbox_key(),
        )
        .expect("seals");

        // A contact's own identity key: the key a contact actually has.
        assert!(matches!(
            open_message(contact(), sealed.sealed.clone()),
            Err(CoreError::Crypto(_))
        ));
        // And any other inbox key, including one at the same generation.
        let stranger_key = InboxKey {
            generation: 3,
            agree_pk: agree_pk_of(&STRANGER_SK),
            agree_sk: STRANGER_SK.to_vec(),
        };
        assert!(matches!(
            core_open_sync_record(sealed.sealed, stranger_key, own_roster()),
            Err(CoreError::Crypto(_))
        ));
    }

    /// The structural half: there is no way to address a sync record at a key
    /// this device does not hold the secret for, because sealing takes the
    /// whole key and checks that its halves agree.
    #[test]
    fn sealing_refuses_a_key_this_device_does_not_hold() {
        let contact_public_half = InboxKey {
            generation: 3,
            agree_pk: contact().agree_pk,
            agree_sk: INBOX_SK.to_vec(),
        };
        let err = core_seal_sync_record(
            signed_record(SyncRecordKind::Watermarks, watermark_payload()),
            person(),
            contact_public_half,
        )
        .expect_err("a key whose halves disagree is refused");
        assert!(matches!(err, CoreError::Crypto(_)));
    }

    /// Someone who learned the inbox key's *public* half — it travels with the
    /// key — still cannot inject a record: the outer envelope signature has to
    /// be the person's own.
    #[test]
    fn a_record_sealed_by_someone_else_to_the_inbox_key_is_refused() {
        let record = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        let forged = seal_message(
            contact(),
            inbox_key().agree_pk,
            core_encode_sync_record(record).expect("encodes"),
        )
        .expect("a contact can always seal to a public key");
        assert!(matches!(
            core_open_sync_record(forged, inbox_key(), own_roster()),
            Err(CoreError::Crypto(_))
        ));
    }

    /// Contacts' data may legitimately ride a sync record (§8), and this pins
    /// what "never widens the person boundary" (DL-5) means for it: the
    /// contact's own card — relay endpoint included — is nowhere in the bytes
    /// that leave this device, and the contact it describes cannot open them.
    #[test]
    fn contacts_data_travels_only_inside_the_person_seal() {
        let payload = contacts_payload_with_real_card();
        let card = contact_card();
        assert!(
            payload
                .windows(card.len())
                .any(|window| window == card.as_bytes()),
            "the plaintext payload really does carry the contact's card"
        );

        let sealed = core_seal_sync_record(
            signed_record(SyncRecordKind::Contacts, payload),
            person(),
            inbox_key(),
        )
        .expect("seals");
        assert!(
            !sealed
                .sealed
                .windows(card.len())
                .any(|window| window == card.as_bytes()),
            "the card must not appear in the sealed bytes"
        );
        assert!(matches!(
            open_message(contact(), sealed.sealed.clone()),
            Err(CoreError::Crypto(_))
        ));
        // The person's own sibling gets it back intact.
        let opened = core_open_sync_record(sealed.sealed, inbox_key(), own_roster())
            .expect("a sibling opens it");
        let contacts = core_decode_sync_contacts(opened.payload).expect("decodes");
        assert_eq!(contacts.entries[0].card_json, card);
    }

    /// SYNC-3's "re-sealed on roster change": a queued copy sealed under one
    /// roster version is stale the moment the roster moves, in either
    /// direction.
    #[test]
    fn a_roster_change_makes_a_sealed_record_stale() {
        let sealed = core_seal_sync_record(
            signed_record(SyncRecordKind::Watermarks, watermark_payload()),
            person(),
            inbox_key(),
        )
        .expect("seals");
        assert!(core_sync_seal_is_current(
            sealed.sealed_for,
            sealed.inbox_key_generation,
            roster_version(),
            3
        ));
        // A device added: seq advances.
        assert!(!core_sync_seal_is_current(
            sealed.sealed_for,
            sealed.inbox_key_generation,
            RosterVersion {
                recovery_epoch: 1,
                seq: 8
            },
            3
        ));
        // A recovery-epoch supersession.
        assert!(!core_sync_seal_is_current(
            sealed.sealed_for,
            sealed.inbox_key_generation,
            RosterVersion {
                recovery_epoch: 2,
                seq: 1
            },
            3
        ));
        // And a roster that moved backwards under this device (a restore) is
        // not a licence to keep sending either.
        assert!(!core_sync_seal_is_current(
            sealed.sealed_for,
            sealed.inbox_key_generation,
            RosterVersion {
                recovery_epoch: 1,
                seq: 6
            },
            3
        ));
        // §10.1: the roster can stand perfectly still while the device set
        // changes underneath it, and bytes sealed under the superseded
        // generation are exactly what the revoked device can still read.
        assert!(!core_sync_seal_is_current(
            sealed.sealed_for,
            sealed.inbox_key_generation,
            roster_version(),
            4
        ));
    }

    /// §10.1's rotation, from the reader's side: a record sealed under the old
    /// generation is refused rather than accepted late — the old generation is
    /// exactly the one a revoked device still holds.
    #[test]
    fn a_rotated_inbox_key_refuses_records_sealed_under_the_old_one() {
        let rotated = core_rotate_inbox_key(inbox_key());
        assert_eq!(rotated.generation, 4);
        assert_ne!(rotated.agree_pk, inbox_key().agree_pk);
        assert_ne!(rotated.agree_sk, inbox_key().agree_sk);

        let record = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        assert_eq!(
            core_sync_record_admit(record, rotated.generation, own_roster()),
            Some(SyncRecordRejection::StaleInboxKey)
        );
    }

    #[test]
    fn the_admit_gate_names_every_way_a_record_fails() {
        let good = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        assert_eq!(core_sync_record_admit(good.clone(), 3, own_roster()), None);

        let mut foreign = good.clone();
        foreign.person_id = contact().user_id;
        assert_eq!(
            core_sync_record_admit(foreign, 3, own_roster()),
            Some(SyncRecordRejection::ForeignPerson)
        );

        let mut legacy = good.clone();
        legacy.author_device_id = LEGACY_DEVICE_ID.to_vec();
        assert_eq!(
            core_sync_record_admit(legacy, 3, own_roster()),
            Some(SyncRecordRejection::LegacyAuthorDevice)
        );

        // A device of this person that this build has never heard of.
        let stranger = core_sign_sync_record(
            unsigned_record(SyncRecordKind::Watermarks, watermark_payload()),
            STRANGER_SK.to_vec(),
        )
        .expect("signs");
        assert_eq!(
            core_sync_record_admit(stranger, 3, own_roster()),
            Some(SyncRecordRejection::UnknownAuthorDevice)
        );

        // DL-4 / §10.3: a tombstoned device's new events are refused, and the
        // tombstone is checked before membership so the verdict is the precise
        // one.
        let mut revoked_roster = own_roster();
        revoked_roster.tombstones.push(DeviceTombstone {
            device_id: device_a_id(),
            revoked_at_seq: 6,
        });
        assert_eq!(
            core_sync_record_admit(good.clone(), 3, revoked_roster),
            Some(SyncRecordRejection::RevokedAuthorDevice)
        );

        let mut tampered = good;
        tampered.stream_seq = 43;
        assert_eq!(
            core_sync_record_admit(tampered, 3, own_roster()),
            Some(SyncRecordRejection::SignatureInvalid)
        );
    }

    /// A sync-record signature is minted in its own domain (§3), so it can
    /// never be replayed as a message-authoring signature and vice versa.
    #[test]
    fn a_sync_record_signature_does_not_verify_in_another_domain() {
        let record = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        let signed_bytes = sync_record_signed_bytes(&record).expect("signed bytes");
        core_device_verify(
            DeviceSigningDomain::SyncRecord,
            sign_pk(&DEVICE_A_SK),
            signed_bytes.clone(),
            record.signature.clone(),
        )
        .expect("verifies in its own domain");
        assert!(matches!(
            core_device_verify(
                DeviceSigningDomain::MessageAuthoring,
                sign_pk(&DEVICE_A_SK),
                signed_bytes,
                record.signature,
            ),
            Err(CoreError::SignatureInvalid)
        ));
    }

    /// Sealing refuses a record whose generation is not the key's, so a caller
    /// cannot ship bytes labelled for a generation they were not sealed under.
    #[test]
    fn sealing_refuses_a_generation_mismatch() {
        let mut record = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        record.inbox_key_generation = 2;
        assert!(matches!(
            core_seal_sync_record(record, person(), inbox_key()),
            Err(CoreError::Crypto(_))
        ));
    }

    #[test]
    fn sealing_refuses_an_unsigned_record() {
        let record = unsigned_record(SyncRecordKind::Watermarks, watermark_payload());
        assert!(matches!(
            core_seal_sync_record(record, person(), inbox_key()),
            Err(CoreError::SignatureInvalid)
        ));
    }

    /// A record id names the stream slot, not the bytes: re-sealing the same
    /// slot after a roster change must not spend a fresh relay row.
    #[test]
    fn a_record_id_is_stable_across_a_reseal() {
        let first = core_sync_record_id(person_id(), device_a_id(), SyncRecordKind::Watermarks, 42);
        let same = core_sync_record_id(person_id(), device_a_id(), SyncRecordKind::Watermarks, 42);
        assert_eq!(first, same);
        // Every dimension separates.
        assert_ne!(
            first,
            core_sync_record_id(person_id(), device_a_id(), SyncRecordKind::Watermarks, 43)
        );
        assert_ne!(
            first,
            core_sync_record_id(person_id(), device_a_id(), SyncRecordKind::History, 42)
        );
        assert_ne!(
            first,
            core_sync_record_id(
                person_id(),
                derive_user_id(&sign_pk(&DEVICE_B_SK)).to_vec(),
                SyncRecordKind::Watermarks,
                42
            )
        );
    }

    // -----------------------------------------------------------------------
    // The device layer: who may seal, and whose seal is believed (§14.2)
    // -----------------------------------------------------------------------

    /// The keys a linked device actually has. §14.2 keeps the person root inside
    /// the encrypted backup, so this — and not `person()` — is what every
    /// production seal is made with.
    fn device_a() -> Identity {
        core_device_sync_identity(DeviceKeypair {
            device_id: device_a_id(),
            sign_pk: sign_pk(&DEVICE_A_SK),
            sign_sk: DEVICE_A_SK.to_vec(),
            agree_pk: DEVICE_A_AGREE_PK.to_vec(),
            agree_sk: DEVICE_A_AGREE_PK.to_vec(),
        })
    }

    /// The fix this whole seam exists for: a linked device holds no person root,
    /// so if the outer signature had to be the root's, no linked device could
    /// ever seal a sync record and self-sync would be dead on the second phone.
    #[test]
    fn a_linked_device_seals_and_a_sibling_opens_it() {
        let record = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        let sealed = core_seal_sync_record(record.clone(), device_a(), inbox_key())
            .expect("a device seals with the only key it has");
        assert_eq!(
            core_open_sync_record(sealed.sealed, inbox_key(), own_roster()).expect("sibling opens"),
            record
        );
    }

    /// And the un-linked install, whose only key *is* the root (§3's
    /// upgrade-in-place), is still accepted — otherwise a person who has never
    /// linked anything could not author their own first records.
    #[test]
    fn the_person_root_is_still_accepted_for_a_pre_link_install() {
        let record = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        let sealed = core_seal_sync_record(record.clone(), person(), inbox_key()).expect("seals");
        assert_eq!(
            core_open_sync_record(sealed.sealed, inbox_key(), own_roster()).expect("opens"),
            record
        );
    }

    /// The device that seals must be the device that signed the record. Sealing
    /// with a *different* own device's key is refused at the source rather than
    /// discovered on the far side of a DTN hop.
    #[test]
    fn a_device_may_not_seal_another_devices_record() {
        let record = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        let device_b = core_device_sync_identity(DeviceKeypair {
            device_id: derive_user_id(&sign_pk(&DEVICE_B_SK)).to_vec(),
            sign_pk: sign_pk(&DEVICE_B_SK),
            sign_sk: DEVICE_B_SK.to_vec(),
            agree_pk: DEVICE_B_AGREE_PK.to_vec(),
            agree_sk: DEVICE_B_AGREE_PK.to_vec(),
        });
        assert!(matches!(
            core_seal_sync_record(record, device_b, inbox_key()),
            Err(CoreError::Crypto(_))
        ));
    }

    /// §10.3 from the outer layer: a revoked device's signature is refused as an
    /// envelope signature too, not only as a record signature. Otherwise a
    /// tombstoned device could still wrap a sibling's perfectly valid record and
    /// keep speaking.
    #[test]
    fn a_revoked_devices_seal_is_refused_even_around_a_valid_record() {
        let record = signed_record(SyncRecordKind::Watermarks, watermark_payload());
        let sealed = core_seal_sync_record(record, device_a(), inbox_key()).expect("seals");
        let mut revoked_roster = own_roster();
        revoked_roster.tombstones.push(DeviceTombstone {
            device_id: device_a_id(),
            revoked_at_seq: 6,
        });
        assert!(matches!(
            core_open_sync_record(sealed.sealed, inbox_key(), revoked_roster),
            Err(CoreError::Crypto(_))
        ));
    }

    /// A device of nobody: real keys, a real signature, and no cert in this
    /// person's roster. The inbox key's public half travels with the key, so
    /// possession of it is not evidence of anything — the outer signature is.
    #[test]
    fn a_stranger_device_that_learned_the_inbox_key_still_cannot_seal_one() {
        let stranger = core_device_sync_identity(DeviceKeypair {
            device_id: derive_user_id(&sign_pk(&STRANGER_SK)).to_vec(),
            sign_pk: sign_pk(&STRANGER_SK),
            sign_sk: STRANGER_SK.to_vec(),
            agree_pk: agree_pk_of(&STRANGER_SK),
            agree_sk: STRANGER_SK.to_vec(),
        });
        let record = core_sign_sync_record(
            unsigned_record(SyncRecordKind::Watermarks, watermark_payload()),
            STRANGER_SK.to_vec(),
        )
        .expect("a stranger can always sign");
        let sealed = core_seal_sync_record(record, stranger, inbox_key())
            .expect("and can always seal to a key it holds");
        assert!(matches!(
            core_open_sync_record(sealed.sealed, inbox_key(), own_roster()),
            Err(CoreError::Crypto(_))
        ));
    }

    /// A device-derived identity's `user_id` is the device id, which is the one
    /// thing that makes the whole layering legal: `core_derive_device_id` and
    /// `derive_user_id` are one derivation over a signing public key, so the id
    /// a signature proves comes out right whichever name it is read under.
    #[test]
    fn a_device_sync_identity_is_named_by_its_device_id() {
        let identity = device_a();
        assert_eq!(identity.user_id, device_a_id());
        assert_eq!(
            identity.user_id,
            crate::core_derive_device_id(sign_pk(&DEVICE_A_SK)).expect("derives")
        );
        assert_ne!(identity.user_id, person_id());
    }

    /// A digest is a record in every respect the crypto cares about: it signs,
    /// it seals, and a sibling admits it under the same roster gate.
    #[test]
    fn a_digest_record_seals_and_admits_like_any_other() {
        let payload = crate::core_encode_sync_digest(crate::SyncDigest {
            person_id: person_id(),
            streams: vec![crate::SyncStreamDigest {
                author_device_id: device_a_id(),
                kind: KIND_SYNC_HISTORY,
                through_seq: 4,
                can_serve: true,
            }],
        })
        .expect("digest encodes");
        let record = signed_record(SyncRecordKind::Digest, payload);
        let sealed = core_seal_sync_record(record.clone(), device_a(), inbox_key()).expect("seals");
        assert_eq!(
            core_open_sync_record(sealed.sealed, inbox_key(), own_roster()).expect("opens"),
            record
        );
    }

    /// The inbox key is the one payload carrying secret material, so the
    /// round-trip that hands it to a sibling is worth pinning end to end.
    #[test]
    fn the_own_roster_record_carries_the_inbox_key_to_a_sibling() {
        let payload = core_encode_sync_own_roster(SyncOwnRosterPayload {
            roster: own_roster(),
            inbox_keys: vec![inbox_key()],
        })
        .expect("encodes");
        let sealed = core_seal_sync_record(
            signed_record(SyncRecordKind::OwnRoster, payload),
            person(),
            inbox_key(),
        )
        .expect("seals");
        let opened = core_open_sync_record(sealed.sealed, inbox_key(), own_roster())
            .expect("a sibling opens it");
        let decoded = core_decode_sync_own_roster(opened.payload).expect("decodes");
        assert_eq!(decoded.roster, own_roster());
        assert_eq!(decoded.inbox_keys, vec![inbox_key()]);
    }
}
