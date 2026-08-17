//! The canonical bootstrap the approving device streams into the new one
//! (`specs/multi-device-v1.md` §9.3).
//!
//! §9.3 is one sentence with a hard edge in it: *a versioned export (identity
//! material incl. inbox key, contacts + their rosters, group state, recent
//! history head) — **NOT** a raw sqlite clone*. The distinction is the whole
//! module. A sqlite clone is a second device pretending to be the first one:
//! same device id, same author stream, same relay rows, which is exactly §1's
//! failure mode and what the clone guard exists to shout about. A canonical
//! export is a *statement of what this person knows*, re-imported by a device
//! that then holds its own key, its own author stream, and its own place in the
//! roster.
//!
//! So the format is declared here field by field, frozen by
//! [`tests::golden_link_bootstrap_payload`], and deliberately built out of the
//! types the rest of the crate already exports ([`Contact`], [`Group`],
//! [`Roster`], [`StoredMessage`]) rather than a parallel set of DTOs. What it
//! carries is what a person's second phone needs to be useful the moment it
//! finishes linking; what it does not carry is everything a clone would have
//! taken.
//!
//! # What may never ride here
//!
//! The **person root signing secret**. §3 and §14.2 put it in exactly one
//! place — inside the passphrase-encrypted `.cmbak` — because a root secret on
//! every device means any stolen phone can revoke the person's real devices.
//! [`tests::bootstrap_carries_no_person_root_secret`] walks the encoded bytes
//! and refuses to find it, in the same style as the QR's payload test.
//!
//! The **person agreement secret** does ride, and is not an exception to that
//! rule: §6 says every linked device holds the person-scoped X25519 inbox key,
//! and on a fleet that upgraded in place (§2 goal 2) generation 0 of that key
//! *is* the deployed agreement keypair. Without it a new device cannot open one
//! byte of the mail its person is already receiving.
//!
//! # What binds an export to its ceremony
//!
//! An export is not a file. Version 2 carries an authentication trailer naming
//! the ceremony's channel binding, an expiry, and the approving device's
//! signature over everything above it. The new device refuses anything whose
//! binding is not the one it recorded when its own pre-activation window opened
//! ([`core_link_bootstrap_verify`]), so a bootstrap captured off one channel is
//! not a bootstrap on another, and an export left lying around stops being one.
//!
//! Without that, a bootstrap was authenticated only by the transport carrying
//! it, and "the transport was a Noise channel" is a claim the *importer* has no
//! way to check after the bytes are in its hands.
//!
//! # Endpoint privacy
//!
//! Contacts ride with their relay endpoints, which is SYNC-3 exactly: "sync
//! records may contain contacts' data the person already legitimately holds
//! (cards, endpoints, history) — that data never transits any third party's
//! device unsealed and never widens beyond the person boundary". This export
//! crosses one Noise channel between two devices of one person and is sealed to
//! it. DL-5 is untouched: the *rosters* in here still carry keys and never
//! endpoints.
//!
//! # The WP4 seam
//!
//! The head is a head. Everything older arrives as ordinary self-sync catch-up
//! (§9.3's last clause), which is WP4's work package and has not landed.
//! [`core_link_catch_up_plan`] is the marked stub: it computes, from the
//! imported head alone, exactly what a catch-up would have to ask for, so the
//! trigger has a shape to be wired to and a test to pin it. Nothing here fetches
//! anything.

use crate::device_roster::{
    core_device_sign, core_device_verify, DeviceCert, DeviceSigningDomain, DeviceTombstone,
};
use crate::identity::derive_user_id;
use crate::{Contact, CoreError, Group, MessageStore, Roster, StoredMessage};

/// Payload magic. Distinct from `.cmbak`'s `CMBAK1\0`, and never mistakable for
/// it: a bootstrap is not a backup and must not be openable as one.
const LINK_BOOTSTRAP_MAGIC: &[u8; 8] = b"CMBOOT1\0";
/// Format version. A higher one is [`CoreError::UnsupportedLink`] — the
/// "update the app" fail-soft WPT established, never a half-imported fleet.
///
/// **Version 2** is the authenticated form. Version 1 carried the export and
/// nothing about *which* ceremony it belonged to, so a captured bootstrap was a
/// valid bootstrap anywhere; v2 binds it to the channel it crossed, to the
/// approving device that signed it, and to a moment it stops being valid. That
/// is a fixed-header change rather than a new skippable section, and — more to
/// the point — a build that skipped an authentication section it did not
/// understand would import an unauthenticated export while believing itself
/// safe. So the version moves, and every build refuses the other's bytes
/// outright. Nothing has shipped, so nothing in the field is stranded by it.
pub const LINK_BOOTSTRAP_VERSION: u16 = 2;

/// How long a signed bootstrap stands by default, from the moment it is built.
/// A ceremony is a person holding two phones; an export that is still valid an
/// hour later is an export that outlived the room it was made in.
pub const LINK_BOOTSTRAP_DEFAULT_LIFETIME_MS: i64 = 10 * 60 * 1000;

/// Ceiling on one encoded bootstrap. Generous next to what a family store
/// holds, and finite so a peer's first frame cannot be an allocation.
pub const LINK_BOOTSTRAP_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Ceiling on any single length-framed field inside it.
const LINK_BOOTSTRAP_MAX_FIELD_BYTES: usize = 1024 * 1024;
/// How many recent messages per chat the head carries by default. A head, not
/// a history: enough that a linked phone opens onto a conversation rather than
/// a blank page, with the rest owed to WP4's catch-up.
pub const LINK_BOOTSTRAP_HISTORY_HEAD_PER_CHAT: u64 = 20;
/// Payload bytes above which a message is left out of the head entirely.
/// Attachment manifests and chunks are what this excludes: they are history in
/// the WP4 sense, not context, and a link ceremony is not the place to move
/// them.
pub const LINK_BOOTSTRAP_MAX_MESSAGE_BYTES: usize = 4 * 1024;

/// Section tags. Unknown tags are skipped on decode (WPT forward tolerance),
/// which is why every section is length-framed.
const SECTION_PERSON: u8 = 0x01;
const SECTION_ROSTER: u8 = 0x02;
const SECTION_CONTACTS: u8 = 0x03;
const SECTION_GROUPS: u8 = 0x04;
const SECTION_HISTORY_HEAD: u8 = 0x05;
/// The authentication trailer (v2). Always last and never skippable: everything
/// before it is what the signature covers, so its position is load-bearing
/// rather than tidy, and a decode that did not find one is a decode that found
/// no export at all.
const SECTION_AUTH: u8 = 0x06;

/// Field widths inside [`SECTION_AUTH`].
const CHANNEL_BINDING_LEN: usize = 32;
const KEY_LEN: usize = 32;
const SIGNATURE_LEN: usize = 64;

/// The identity material of §9.3, minus everything §14.2 keeps in the backup.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct LinkBootstrapPerson {
    /// The wire `user_id` (§3's `person_id`), unchanged by linking.
    pub person_id: Vec<u8>,
    /// The person root's *public* signing key: what every roster in this
    /// export chains to, and what the new device verifies later rosters
    /// against. The secret half is not here and never is (§14.2).
    pub person_sign_pk: Vec<u8>,
    /// §6's person-scoped inbox key generation, as the roster names it.
    pub inbox_key_generation: u64,
    /// The person-scoped X25519 inbox keypair (§6). At generation 0 on an
    /// upgraded-in-place fleet this is the deployed person agreement keypair —
    /// the key contacts have been sealing to all along. WP5 rotates it on
    /// revocation and bumps the generation with it.
    pub inbox_agree_pk: Vec<u8>,
    pub inbox_agree_sk: Vec<u8>,
}

/// One contact and what this person knows about that contact's devices.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct LinkBootstrapContact {
    pub contact: Contact,
    /// The contact's last accepted roster (§4), or `None` for a contact who
    /// has never gossiped one — which is every v1 peer, and reads on the new
    /// device as §5's synthetic one-device person.
    pub roster: Option<Roster>,
}

/// The §9.3 export itself.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct LinkBootstrap {
    pub version: u16,
    pub created_at_ms: i64,
    pub person: LinkBootstrapPerson,
    /// The person's OWN roster at `seq + 1`, already naming the new device
    /// (§9.4). This is the document whose head the new device acknowledges to
    /// close activation, so it travels inside the bootstrap rather than beside
    /// it: importing the export and learning which roster to ack are one act.
    pub roster: Roster,
    pub contacts: Vec<LinkBootstrapContact>,
    pub groups: Vec<Group>,
    /// Recent messages, newest chats and all, as §9.3's "recent history head".
    pub history_head: Vec<StoredMessage>,
    /// The Noise handshake hash of the ceremony this export was made for
    /// ([`CoreLinkSummary::channel_binding`](super::ceremony::CoreLinkSummary)).
    /// The new device refuses a bootstrap whose binding is not the one it
    /// recorded when its own window opened, so an export captured from one
    /// ceremony is not an export in another.
    pub channel_binding: Vec<u8>,
    /// When this export stops being importable. A ceremony is minutes long; an
    /// export that is still good tomorrow is one that outlived the room.
    pub expires_at_ms: i64,
    /// The APPROVING device's signing key — the roster-signing device of §3,
    /// which the roster below names as `approving_device_id`. Verified against
    /// that roster rather than trusted, so the signature says "the device that
    /// is allowed to add devices made this", not merely "someone did".
    pub signer_sign_pk: Vec<u8>,
    /// Ed25519 over
    /// [`DeviceSigningDomain::DeviceLinkBootstrap`] ‖ everything above ‖
    /// `channel_binding` ‖ `expires_at_ms`.
    pub signature: Vec<u8>,
}

/// What WP4's catch-up will have to ask for, per chat, once it exists.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreLinkCatchUp {
    pub chat_id: Vec<u8>,
    /// The oldest lamport the head carries for this chat. Everything strictly
    /// below it is what self-sync owes.
    pub head_from_lamport: u64,
    /// The newest lamport the head carries.
    pub head_through_lamport: u64,
}

// ---------------------------------------------------------------------------
// Codec
// ---------------------------------------------------------------------------

/// Encode a bootstrap for the ready link channel (§9.3).
#[uniffi::export]
pub fn core_link_bootstrap_encode(bootstrap: LinkBootstrap) -> Result<Vec<u8>, CoreError> {
    encode_bootstrap(&bootstrap)
}

/// Decode one. A newer version is [`CoreError::UnsupportedLink`]; anything
/// malformed is [`CoreError::Malformed`], never a partial import.
#[uniffi::export]
pub fn core_link_bootstrap_decode(bytes: Vec<u8>) -> Result<LinkBootstrap, CoreError> {
    decode_bootstrap(&bytes)
}

/// Everything the signature covers, as bytes: the header and every content
/// section, in the one order [`encode_bootstrap`] writes them.
///
/// Split out because the signature is made over exactly this and the trailer is
/// appended after — signing the whole encoding would need the signature inside
/// its own input.
fn bootstrap_prefix(bootstrap: &LinkBootstrap) -> Vec<u8> {
    let mut out = Vec::with_capacity(4096);
    out.extend_from_slice(LINK_BOOTSTRAP_MAGIC);
    out.extend_from_slice(&LINK_BOOTSTRAP_VERSION.to_be_bytes());
    out.extend_from_slice(&bootstrap.created_at_ms.to_be_bytes());

    let mut person = Vec::new();
    push_bytes(&mut person, &bootstrap.person.person_id);
    push_bytes(&mut person, &bootstrap.person.person_sign_pk);
    person.extend_from_slice(&bootstrap.person.inbox_key_generation.to_be_bytes());
    push_bytes(&mut person, &bootstrap.person.inbox_agree_pk);
    push_bytes(&mut person, &bootstrap.person.inbox_agree_sk);
    push_section(&mut out, SECTION_PERSON, &person);

    let mut roster = Vec::new();
    push_roster(&mut roster, &bootstrap.roster);
    push_section(&mut out, SECTION_ROSTER, &roster);

    let mut contacts = Vec::new();
    push_count(&mut contacts, bootstrap.contacts.len());
    for entry in &bootstrap.contacts {
        push_contact(&mut contacts, &entry.contact);
        match &entry.roster {
            Some(roster) => {
                contacts.push(1);
                push_roster(&mut contacts, roster);
            }
            None => contacts.push(0),
        }
    }
    push_section(&mut out, SECTION_CONTACTS, &contacts);

    let mut groups = Vec::new();
    push_count(&mut groups, bootstrap.groups.len());
    for group in &bootstrap.groups {
        push_group(&mut groups, group);
    }
    push_section(&mut out, SECTION_GROUPS, &groups);

    let mut history = Vec::new();
    push_count(&mut history, bootstrap.history_head.len());
    for message in &bootstrap.history_head {
        push_message(&mut history, message);
    }
    push_section(&mut out, SECTION_HISTORY_HEAD, &history);
    out
}

/// The bytes the approving device's signature is made over: the export itself,
/// then the channel it is crossing and the moment it expires. Both of the
/// latter are outside the signed *sections* on purpose — they are properties of
/// this ceremony rather than of the export's content, and putting them in the
/// signature input rather than in a section keeps the trailer one flat block.
fn bootstrap_signed_message(prefix: &[u8], channel_binding: &[u8], expires_at_ms: i64) -> Vec<u8> {
    let mut out = Vec::with_capacity(prefix.len() + CHANNEL_BINDING_LEN + 8);
    out.extend_from_slice(prefix);
    out.extend_from_slice(channel_binding);
    out.extend_from_slice(&expires_at_ms.to_be_bytes());
    out
}

pub(crate) fn encode_bootstrap(bootstrap: &LinkBootstrap) -> Result<Vec<u8>, CoreError> {
    // A bootstrap that was never signed must not reach a channel at all. This
    // is the one place both the shells' encode call and the internal one pass
    // through, so an unsigned export cannot leave by any door.
    check_len(
        &bootstrap.channel_binding,
        CHANNEL_BINDING_LEN,
        "channel binding",
    )?;
    check_len(&bootstrap.signer_sign_pk, KEY_LEN, "signing key")?;
    check_len(&bootstrap.signature, SIGNATURE_LEN, "signature")?;

    let mut out = bootstrap_prefix(bootstrap);
    let mut auth = Vec::with_capacity(CHANNEL_BINDING_LEN + 8 + KEY_LEN + SIGNATURE_LEN);
    auth.extend_from_slice(&bootstrap.channel_binding);
    auth.extend_from_slice(&bootstrap.expires_at_ms.to_be_bytes());
    auth.extend_from_slice(&bootstrap.signer_sign_pk);
    auth.extend_from_slice(&bootstrap.signature);
    push_section(&mut out, SECTION_AUTH, &auth);

    if out.len() > LINK_BOOTSTRAP_MAX_BYTES {
        return Err(malformed("device-link bootstrap is too large to send"));
    }
    Ok(out)
}

fn check_len(bytes: &[u8], len: usize, what: &str) -> Result<(), CoreError> {
    if bytes.len() != len {
        return Err(malformed(&format!(
            "device-link bootstrap {what} is {} bytes, not {len}",
            bytes.len()
        )));
    }
    Ok(())
}

pub(crate) fn decode_bootstrap(bytes: &[u8]) -> Result<LinkBootstrap, CoreError> {
    if bytes.len() > LINK_BOOTSTRAP_MAX_BYTES {
        return Err(malformed("device-link bootstrap is too large"));
    }
    let mut reader = Reader { bytes, at: 0 };
    if reader.take(LINK_BOOTSTRAP_MAGIC.len())? != LINK_BOOTSTRAP_MAGIC {
        return Err(malformed("not a device-link bootstrap"));
    }
    let version = reader.u16()?;
    if version > LINK_BOOTSTRAP_VERSION {
        return Err(CoreError::UnsupportedLink);
    }
    if version < LINK_BOOTSTRAP_VERSION {
        return Err(malformed("device-link bootstrap has an unknown version"));
    }
    let created_at_ms = reader.i64()?;

    let mut person = None;
    let mut roster = None;
    let mut contacts = Vec::new();
    let mut groups = Vec::new();
    let mut history_head = Vec::new();
    let mut auth = None;
    while !reader.done() {
        let tag = reader.u8()?;
        let len = reader.u32()? as usize;
        let body = reader.take(len)?;
        let mut section = Reader { bytes: body, at: 0 };
        match tag {
            SECTION_PERSON => {
                person = Some(LinkBootstrapPerson {
                    person_id: section.bytes_field()?,
                    person_sign_pk: section.bytes_field()?,
                    inbox_key_generation: section.u64()?,
                    inbox_agree_pk: section.bytes_field()?,
                    inbox_agree_sk: section.bytes_field()?,
                });
            }
            SECTION_ROSTER => roster = Some(section.roster()?),
            SECTION_CONTACTS => {
                let count = section.count()?;
                for _ in 0..count {
                    let contact = section.contact()?;
                    let roster = match section.u8()? {
                        0 => None,
                        1 => Some(section.roster()?),
                        _ => return Err(malformed("device-link bootstrap contact roster flag")),
                    };
                    contacts.push(LinkBootstrapContact { contact, roster });
                }
            }
            SECTION_GROUPS => {
                let count = section.count()?;
                for _ in 0..count {
                    groups.push(section.group()?);
                }
            }
            SECTION_HISTORY_HEAD => {
                let count = section.count()?;
                for _ in 0..count {
                    history_head.push(section.message()?);
                }
            }
            SECTION_AUTH => {
                auth = Some((
                    section.take(CHANNEL_BINDING_LEN)?.to_vec(),
                    section.i64()?,
                    section.take(KEY_LEN)?.to_vec(),
                    section.take(SIGNATURE_LEN)?.to_vec(),
                ));
            }
            // A section a later build added. Length-framed, so it is skipped
            // exactly and this build imports what it does understand.
            _ => {}
        }
    }

    // Not optional, and not "absent means unsigned". An export with no trailer
    // is refused outright: the whole point of v2 is that there is no such thing
    // as an unauthenticated bootstrap this build will look at.
    let (channel_binding, expires_at_ms, signer_sign_pk, signature) =
        auth.ok_or_else(|| malformed("device-link bootstrap is not signed"))?;

    Ok(LinkBootstrap {
        version,
        created_at_ms,
        person: person.ok_or_else(|| malformed("device-link bootstrap has no identity"))?,
        roster: roster.ok_or_else(|| malformed("device-link bootstrap has no roster"))?,
        contacts,
        groups,
        history_head,
        channel_binding,
        expires_at_ms,
        signer_sign_pk,
        signature,
    })
}

/// Verify a decoded bootstrap against the ceremony that was supposed to have
/// produced it (§9.3).
///
/// Three independent things, and none of them is redundant:
///
/// * **The channel.** `channel_binding` must be the binding the importing
///   device recorded when its own pre-activation window opened. This is what
///   makes a captured export useless anywhere else — the binding commits to
///   both statics and every handshake message, so no second ceremony can ever
///   reproduce one.
/// * **The signer.** The signature must verify under a key the export's OWN
///   roster names as the approving device. The roster's chain to the person
///   root is checked separately, by the import path, before this is consulted —
///   so "signed by the approving device" means the device §3 actually put in
///   charge of adding devices, not whichever key the payload nominated.
/// * **The clock.** An export past `expires_at_ms` is refused. A ceremony is a
///   person holding two phones for a minute; anything that arrives long after
///   is not that ceremony.
#[uniffi::export]
pub fn core_link_bootstrap_verify(
    bootstrap: LinkBootstrap,
    channel_binding: Vec<u8>,
    now_ms: i64,
) -> Result<(), CoreError> {
    check_len(&channel_binding, CHANNEL_BINDING_LEN, "channel binding")?;
    if bootstrap.channel_binding != channel_binding {
        return Err(malformed(
            "this bootstrap was made for a different link ceremony",
        ));
    }
    if now_ms > bootstrap.expires_at_ms {
        return Err(malformed("this bootstrap has expired"));
    }
    let signer_device_id = derive_user_id(&bootstrap.signer_sign_pk).to_vec();
    if signer_device_id != bootstrap.roster.approving_device_id {
        return Err(malformed(
            "this bootstrap was not signed by the device that signs this person's rosters",
        ));
    }
    if !bootstrap
        .roster
        .devices
        .iter()
        .any(|cert| cert.device_sign_pk == bootstrap.signer_sign_pk)
    {
        return Err(malformed(
            "this bootstrap's signer is not a device its own roster lists",
        ));
    }
    let message = bootstrap_signed_message(
        &bootstrap_prefix(&bootstrap),
        &bootstrap.channel_binding,
        bootstrap.expires_at_ms,
    );
    core_device_verify(
        DeviceSigningDomain::DeviceLinkBootstrap,
        bootstrap.signer_sign_pk.clone(),
        message,
        bootstrap.signature.clone(),
    )
}

/// The canonical bytes of one roster document, as the bootstrap's roster
/// section writes them.
///
/// Shared with `device_link::activation`, which stores the person's OWN roster
/// as exactly these bytes. One encoding, so a roster written to the database
/// and a roster put on a link channel can never drift into two formats that
/// hash differently on the way back.
pub(crate) fn encode_roster_document(roster: &Roster) -> Vec<u8> {
    let mut out = Vec::with_capacity(512);
    push_roster(&mut out, roster);
    out
}

pub(crate) fn decode_roster_document(bytes: &[u8]) -> Result<Roster, CoreError> {
    let mut reader = Reader { bytes, at: 0 };
    reader.roster()
}

// ---------------------------------------------------------------------------
// Chunking for the ready channel
// ---------------------------------------------------------------------------

/// Chunk header: `sequence(u32) || total(u32)`.
const CHUNK_HEADER_LEN: usize = 8;

/// Split an encoded bootstrap into frames that fit
/// [`LINK_CHANNEL_MAX_PLAINTEXT_BYTES`](super::ceremony::LINK_CHANNEL_MAX_PLAINTEXT_BYTES),
/// each one sealed separately by `seal_channel_frame`.
///
/// Each chunk states its own place in the sequence, so a receiver can refuse a
/// reordered, duplicated, or truncated stream rather than reassembling a
/// plausible-looking bootstrap out of it.
#[uniffi::export]
pub fn core_link_bootstrap_chunks(payload: Vec<u8>) -> Result<Vec<Vec<u8>>, CoreError> {
    let body = super::ceremony::LINK_CHANNEL_MAX_PLAINTEXT_BYTES - CHUNK_HEADER_LEN;
    if payload.is_empty() {
        return Err(malformed("device-link bootstrap is empty"));
    }
    if payload.len() > LINK_BOOTSTRAP_MAX_BYTES {
        return Err(malformed("device-link bootstrap is too large to send"));
    }
    let total = payload.len().div_ceil(body);
    let mut chunks = Vec::with_capacity(total);
    for (index, piece) in payload.chunks(body).enumerate() {
        let mut chunk = Vec::with_capacity(CHUNK_HEADER_LEN + piece.len());
        chunk.extend_from_slice(&(index as u32).to_be_bytes());
        chunk.extend_from_slice(&(total as u32).to_be_bytes());
        chunk.extend_from_slice(piece);
        chunks.push(chunk);
    }
    Ok(chunks)
}

/// Reassemble what [`core_link_bootstrap_chunks`] split, refusing anything that
/// is not exactly the sequence that was sent.
#[uniffi::export]
pub fn core_link_bootstrap_join(chunks: Vec<Vec<u8>>) -> Result<Vec<u8>, CoreError> {
    if chunks.is_empty() {
        return Err(malformed("device-link bootstrap arrived empty"));
    }
    let mut payload = Vec::new();
    let mut declared_total = None;
    for (index, chunk) in chunks.iter().enumerate() {
        if chunk.len() < CHUNK_HEADER_LEN {
            return Err(malformed("device-link bootstrap chunk is truncated"));
        }
        let sequence = u32::from_be_bytes(chunk[0..4].try_into().expect("four bytes")) as usize;
        let total = u32::from_be_bytes(chunk[4..8].try_into().expect("four bytes")) as usize;
        if sequence != index {
            return Err(malformed("device-link bootstrap chunk is out of order"));
        }
        match declared_total {
            None => declared_total = Some(total),
            Some(declared) if declared == total => {}
            Some(_) => return Err(malformed("device-link bootstrap chunk count disagrees")),
        }
        payload.extend_from_slice(&chunk[CHUNK_HEADER_LEN..]);
        if payload.len() > LINK_BOOTSTRAP_MAX_BYTES {
            return Err(malformed("device-link bootstrap is too large"));
        }
    }
    if declared_total != Some(chunks.len()) {
        return Err(malformed("device-link bootstrap is missing chunks"));
    }
    Ok(payload)
}

// ---------------------------------------------------------------------------
// Building one (the approving device's side of §9.3)
// ---------------------------------------------------------------------------

#[uniffi::export]
impl MessageStore {
    /// Build the canonical bootstrap this device will stream into the one it is
    /// adopting (§9.3).
    ///
    /// `roster` is the already-signed roster at `seq + 1`
    /// ([`core_link_sign_new_device_roster`](super::activation::core_link_sign_new_device_roster)),
    /// so the export and the document whose head closes activation are the same
    /// act, and the new device cannot be handed a fleet it was never certified
    /// into.
    ///
    /// `identity` supplies §6's inbox key: at generation 0 on a fleet that
    /// upgraded in place, the person's deployed X25519 agreement keypair. The
    /// person root SIGNING secret is not read here and never leaves the
    /// encrypted backup (§14.2).
    ///
    /// `history_head_per_chat` bounds the head — pass 0 for
    /// [`LINK_BOOTSTRAP_HISTORY_HEAD_PER_CHAT`]. Everything older is WP4's
    /// catch-up, not this ceremony's.
    ///
    /// `approving_device_sign_sk` is the roster-signing device's secret — the
    /// same key that signed the roster being exported, and the only key whose
    /// signature the new device will accept. `channel_binding` is the ceremony's
    /// Noise handshake hash. Together they are what makes this export *this*
    /// ceremony's export rather than a file: see [`core_link_bootstrap_verify`].
    ///
    /// `lifetime_ms` bounds how long it stands — pass 0 for
    /// [`LINK_BOOTSTRAP_DEFAULT_LIFETIME_MS`].
    #[allow(clippy::too_many_arguments)]
    pub fn build_link_bootstrap(
        &self,
        identity: crate::Identity,
        roster: Roster,
        approving_device_sign_sk: Vec<u8>,
        channel_binding: Vec<u8>,
        history_head_per_chat: u64,
        lifetime_ms: i64,
        now_ms: i64,
    ) -> Result<LinkBootstrap, CoreError> {
        if roster.person_id != identity.user_id {
            return Err(CoreError::Malformed(
                "the roster to export is for a different person".to_string(),
            ));
        }
        check_len(&channel_binding, CHANNEL_BINDING_LEN, "channel binding")?;
        // Refused here rather than producing an export the new device would
        // reject: only the roster-signing device may hand a fleet to a phone.
        let signer_sign_pk = crate::crypto::signing_key_from_bytes(&approving_device_sign_sk)?
            .verifying_key()
            .as_bytes()
            .to_vec();
        if derive_user_id(&signer_sign_pk).to_vec() != roster.approving_device_id {
            return Err(CoreError::Malformed(
                "this device does not hold the roster-signing role".to_string(),
            ));
        }
        let per_chat = match history_head_per_chat {
            0 => LINK_BOOTSTRAP_HISTORY_HEAD_PER_CHAT,
            explicit => explicit,
        };
        let mut contacts = Vec::new();
        let mut history_head = Vec::new();
        for contact in self.list_contacts()? {
            // SYNC-3: a contact's card and endpoints ride, because this person
            // already legitimately holds them and this export never widens
            // beyond their own device boundary. DL-5 is untouched — the roster
            // beside the contact still carries keys and no addresses.
            let roster = self.contact_roster_state(contact.user_id.clone())?.roster;
            for message in self.recent_presentation_messages_for_chat(
                contact.user_id.clone(),
                per_chat,
                per_chat,
            )? {
                if message.payload.len() <= LINK_BOOTSTRAP_MAX_MESSAGE_BYTES {
                    history_head.push(message);
                }
            }
            contacts.push(LinkBootstrapContact { contact, roster });
        }
        let groups = self.list_groups()?;
        for group in &groups {
            for message in
                self.recent_presentation_messages_for_chat(group.id.clone(), per_chat, per_chat)?
            {
                if message.payload.len() <= LINK_BOOTSTRAP_MAX_MESSAGE_BYTES {
                    history_head.push(message);
                }
            }
        }
        let expires_at_ms = now_ms.saturating_add(match lifetime_ms {
            0 => LINK_BOOTSTRAP_DEFAULT_LIFETIME_MS,
            explicit => explicit,
        });
        let mut bootstrap = LinkBootstrap {
            version: LINK_BOOTSTRAP_VERSION,
            created_at_ms: now_ms,
            person: LinkBootstrapPerson {
                person_id: identity.user_id,
                person_sign_pk: identity.sign_pk,
                inbox_key_generation: roster.inbox_key_generation,
                inbox_agree_pk: identity.agree_pk,
                inbox_agree_sk: identity.agree_sk,
            },
            roster,
            contacts,
            groups,
            history_head,
            channel_binding,
            expires_at_ms,
            signer_sign_pk,
            signature: Vec::new(),
        };
        let message = bootstrap_signed_message(
            &bootstrap_prefix(&bootstrap),
            &bootstrap.channel_binding,
            bootstrap.expires_at_ms,
        );
        bootstrap.signature = core_device_sign(
            DeviceSigningDomain::DeviceLinkBootstrap,
            approving_device_sign_sk,
            message,
        )?;
        Ok(bootstrap)
    }
}

// ---------------------------------------------------------------------------
// The WP4 seam (§9.3's last clause)
// ---------------------------------------------------------------------------

/// What self-sync catch-up owes this device after the head lands, per chat.
///
/// **This is a stub, and deliberately a computed one.** WP4 owns the record
/// kinds, the digests, and the trigger; none of that exists yet. What exists is
/// the question WP4 will be asked — "which chats have history below the head I
/// was given, and from where" — so it is answered here, from the head alone,
/// with no transport and no store. When WP4 lands, its trigger consumes this
/// and this comment goes away; until then the seam is visible rather than
/// implied, and `catch_up_plan_names_every_chat_the_head_truncated` pins it.
#[uniffi::export]
pub fn core_link_catch_up_plan(bootstrap: LinkBootstrap) -> Vec<CoreLinkCatchUp> {
    let mut plan: Vec<CoreLinkCatchUp> = Vec::new();
    for message in &bootstrap.history_head {
        match plan
            .iter_mut()
            .find(|entry| entry.chat_id == message.chat_id)
        {
            Some(entry) => {
                entry.head_from_lamport = entry.head_from_lamport.min(message.lamport);
                entry.head_through_lamport = entry.head_through_lamport.max(message.lamport);
            }
            None => plan.push(CoreLinkCatchUp {
                chat_id: message.chat_id.clone(),
                head_from_lamport: message.lamport,
                head_through_lamport: message.lamport,
            }),
        }
    }
    plan
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

fn push_section(out: &mut Vec<u8>, tag: u8, body: &[u8]) {
    out.push(tag);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
}

fn push_bytes(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

fn push_text(out: &mut Vec<u8>, value: &str) {
    push_bytes(out, value.as_bytes());
}

fn push_optional_text(out: &mut Vec<u8>, value: Option<&String>) {
    match value {
        Some(text) => {
            out.push(1);
            push_text(out, text);
        }
        None => out.push(0),
    }
}

fn push_count(out: &mut Vec<u8>, count: usize) {
    out.extend_from_slice(&(count as u32).to_be_bytes());
}

fn push_cert(out: &mut Vec<u8>, cert: &DeviceCert) {
    push_bytes(out, &cert.person_id);
    push_bytes(out, &cert.device_sign_pk);
    push_bytes(out, &cert.device_agree_pk);
    out.extend_from_slice(&cert.added_epoch.to_be_bytes());
    out.extend_from_slice(&cert.flags.to_be_bytes());
    push_bytes(out, &cert.signer_sign_pk);
    push_bytes(out, &cert.signature);
}

fn push_roster(out: &mut Vec<u8>, roster: &Roster) {
    push_bytes(out, &roster.person_id);
    out.extend_from_slice(&roster.recovery_epoch.to_be_bytes());
    out.extend_from_slice(&roster.seq.to_be_bytes());
    push_count(out, roster.devices.len());
    for cert in &roster.devices {
        push_cert(out, cert);
    }
    push_count(out, roster.tombstones.len());
    for tombstone in &roster.tombstones {
        push_bytes(out, &tombstone.device_id);
        out.extend_from_slice(&tombstone.revoked_at_seq.to_be_bytes());
    }
    push_bytes(out, &roster.approving_device_id);
    out.extend_from_slice(&roster.inbox_key_generation.to_be_bytes());
    push_bytes(out, &roster.signer_sign_pk);
    push_bytes(out, &roster.signature);
}

fn push_contact(out: &mut Vec<u8>, contact: &Contact) {
    push_bytes(out, &contact.user_id);
    push_text(out, &contact.name);
    push_bytes(out, &contact.sign_pk);
    push_bytes(out, &contact.agree_pk);
    push_optional_text(out, contact.relay_url.as_ref());
    push_optional_text(out, contact.relay_token.as_ref());
    push_optional_text(out, contact.nickname.as_ref());
}

fn push_group(out: &mut Vec<u8>, group: &Group) {
    push_bytes(out, &group.id);
    push_text(out, &group.name);
    push_count(out, group.member_user_ids.len());
    for member in &group.member_user_ids {
        push_bytes(out, member);
    }
    push_bytes(out, &group.key);
    out.extend_from_slice(&group.metadata_revision.to_be_bytes());
    push_bytes(out, &group.metadata_changed_by);
}

fn push_message(out: &mut Vec<u8>, message: &StoredMessage) {
    push_bytes(out, &message.chat_id);
    push_bytes(out, &message.sender_user_id);
    out.extend_from_slice(&message.lamport.to_be_bytes());
    out.extend_from_slice(&message.timestamp.to_be_bytes());
    out.push(message.kind);
    push_bytes(out, &message.payload);
    push_bytes(out, &message.sender_device_id);
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn done(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CoreError> {
        let end = self
            .at
            .checked_add(len)
            .ok_or_else(|| malformed("device-link bootstrap is truncated"))?;
        if end > self.bytes.len() {
            return Err(malformed("device-link bootstrap is truncated"));
        }
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, CoreError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, CoreError> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("two bytes"),
        ))
    }

    fn u32(&mut self) -> Result<u32, CoreError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("four bytes"),
        ))
    }

    fn u64(&mut self) -> Result<u64, CoreError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }

    fn i64(&mut self) -> Result<i64, CoreError> {
        Ok(i64::from_be_bytes(
            self.take(8)?.try_into().expect("eight bytes"),
        ))
    }

    fn bytes_field(&mut self) -> Result<Vec<u8>, CoreError> {
        let len = self.u32()? as usize;
        if len > LINK_BOOTSTRAP_MAX_FIELD_BYTES {
            return Err(malformed("device-link bootstrap field is too large"));
        }
        Ok(self.take(len)?.to_vec())
    }

    fn text(&mut self) -> Result<String, CoreError> {
        String::from_utf8(self.bytes_field()?)
            .map_err(|_| malformed("device-link bootstrap text is not utf-8"))
    }

    fn optional_text(&mut self) -> Result<Option<String>, CoreError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.text()?)),
            _ => Err(malformed("device-link bootstrap optional field flag")),
        }
    }

    /// A count, bounded by what is left to read: every element costs at least
    /// one byte, so a declared count larger than the remaining bytes is a lie
    /// and is refused before anything is allocated for it.
    fn count(&mut self) -> Result<usize, CoreError> {
        let count = self.u32()? as usize;
        if count > self.bytes.len().saturating_sub(self.at) {
            return Err(malformed("device-link bootstrap declares impossible count"));
        }
        Ok(count)
    }

    fn cert(&mut self) -> Result<DeviceCert, CoreError> {
        Ok(DeviceCert {
            person_id: self.bytes_field()?,
            device_sign_pk: self.bytes_field()?,
            device_agree_pk: self.bytes_field()?,
            added_epoch: self.u64()?,
            flags: self.u32()?,
            signer_sign_pk: self.bytes_field()?,
            signature: self.bytes_field()?,
        })
    }

    fn roster(&mut self) -> Result<Roster, CoreError> {
        let person_id = self.bytes_field()?;
        let recovery_epoch = self.u64()?;
        let seq = self.u64()?;
        let device_count = self.count()?;
        let mut devices = Vec::with_capacity(device_count.min(64));
        for _ in 0..device_count {
            devices.push(self.cert()?);
        }
        let tombstone_count = self.count()?;
        let mut tombstones = Vec::with_capacity(tombstone_count.min(64));
        for _ in 0..tombstone_count {
            tombstones.push(DeviceTombstone {
                device_id: self.bytes_field()?,
                revoked_at_seq: self.u64()?,
            });
        }
        Ok(Roster {
            person_id,
            recovery_epoch,
            seq,
            devices,
            tombstones,
            approving_device_id: self.bytes_field()?,
            inbox_key_generation: self.u64()?,
            signer_sign_pk: self.bytes_field()?,
            signature: self.bytes_field()?,
        })
    }

    fn contact(&mut self) -> Result<Contact, CoreError> {
        Ok(Contact {
            user_id: self.bytes_field()?,
            name: self.text()?,
            sign_pk: self.bytes_field()?,
            agree_pk: self.bytes_field()?,
            relay_url: self.optional_text()?,
            relay_token: self.optional_text()?,
            nickname: self.optional_text()?,
        })
    }

    fn group(&mut self) -> Result<Group, CoreError> {
        let id = self.bytes_field()?;
        let name = self.text()?;
        let member_count = self.count()?;
        let mut member_user_ids = Vec::with_capacity(member_count.min(64));
        for _ in 0..member_count {
            member_user_ids.push(self.bytes_field()?);
        }
        Ok(Group {
            id,
            name,
            member_user_ids,
            key: self.bytes_field()?,
            metadata_revision: self.u64()?,
            metadata_changed_by: self.bytes_field()?,
        })
    }

    fn message(&mut self) -> Result<StoredMessage, CoreError> {
        Ok(StoredMessage {
            chat_id: self.bytes_field()?,
            sender_user_id: self.bytes_field()?,
            lamport: self.u64()?,
            timestamp: self.i64()?,
            kind: self.u8()?,
            payload: self.bytes_field()?,
            sender_device_id: self.bytes_field()?,
        })
    }
}

fn malformed(detail: &str) -> CoreError {
    CoreError::Malformed(detail.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_roster::DEVICE_CERT_FLAG_ROSTER_SIGNING;
    use crate::identity::generate_identity;

    pub(crate) const PERSON_ID: [u8; 16] = [0x5A; 16];
    const PERSON_SIGN_PK: [u8; 32] = [0x11; 32];
    const INBOX_PK: [u8; 32] = [0x22; 32];
    const INBOX_SK: [u8; 32] = [0x33; 32];
    const BINDING: [u8; 32] = [0x7C; 32];
    const SIGNER_PK: [u8; 32] = [0xAA; 32];
    const EXPIRES_AT_MS: i64 = 1_755_000_600_000;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn roster() -> Roster {
        Roster {
            person_id: PERSON_ID.to_vec(),
            recovery_epoch: 0,
            seq: 1,
            devices: vec![DeviceCert {
                person_id: PERSON_ID.to_vec(),
                device_sign_pk: vec![0x44; 32],
                device_agree_pk: vec![0x55; 32],
                added_epoch: 0,
                flags: DEVICE_CERT_FLAG_ROSTER_SIGNING,
                signer_sign_pk: PERSON_SIGN_PK.to_vec(),
                signature: vec![0x66; 64],
            }],
            tombstones: vec![DeviceTombstone {
                device_id: vec![0x77; 16],
                revoked_at_seq: 3,
            }],
            approving_device_id: vec![0x88; 16],
            inbox_key_generation: 0,
            signer_sign_pk: PERSON_SIGN_PK.to_vec(),
            signature: vec![0x99; 64],
        }
    }

    fn fixture() -> LinkBootstrap {
        LinkBootstrap {
            version: LINK_BOOTSTRAP_VERSION,
            created_at_ms: 1_755_000_000_000,
            person: LinkBootstrapPerson {
                person_id: PERSON_ID.to_vec(),
                person_sign_pk: PERSON_SIGN_PK.to_vec(),
                inbox_key_generation: 0,
                inbox_agree_pk: INBOX_PK.to_vec(),
                inbox_agree_sk: INBOX_SK.to_vec(),
            },
            roster: roster(),
            contacts: vec![LinkBootstrapContact {
                contact: Contact {
                    user_id: vec![0xC1; 16],
                    name: "Bob".to_string(),
                    sign_pk: vec![0xC2; 32],
                    agree_pk: vec![0xC3; 32],
                    relay_url: Some("https://relay.example".to_string()),
                    relay_token: Some("token".to_string()),
                    nickname: None,
                },
                roster: None,
            }],
            groups: vec![Group {
                id: vec![0xD1; 16],
                name: "Cabin 8".to_string(),
                member_user_ids: vec![PERSON_ID.to_vec(), vec![0xC1; 16]],
                key: vec![0xD2; 32],
                metadata_revision: 2,
                metadata_changed_by: PERSON_ID.to_vec(),
            }],
            history_head: vec![StoredMessage {
                chat_id: vec![0xC1; 16],
                sender_user_id: vec![0xC1; 16],
                lamport: 7,
                timestamp: 1_754_900_000_000,
                kind: crate::KIND_TEXT,
                payload: b"hi".to_vec(),
                sender_device_id: crate::LEGACY_DEVICE_ID.to_vec(),
            }],
            channel_binding: BINDING.to_vec(),
            expires_at_ms: EXPIRES_AT_MS,
            signer_sign_pk: SIGNER_PK.to_vec(),
            signature: vec![0xBB; 64],
        }
    }

    /// BLAKE2b-256 of the whole payload — the shortest honest way to freeze a
    /// few hundred bytes of layout.
    fn digest(bytes: &[u8]) -> String {
        use blake2::digest::{Update, VariableOutput};
        let mut hasher = blake2::Blake2bVar::new(32).expect("valid blake2b output length");
        hasher.update(bytes);
        let mut out = [0u8; 32];
        hasher
            .finalize_variable(&mut out)
            .expect("output buffer matches configured length");
        hex(&out)
    }

    /// The export layout, frozen. A deliberate format change edits this vector;
    /// an accidental one fails here — which is what "versioned export, not a
    /// sqlite clone" has to mean in practice for two builds to agree.
    ///
    /// **Edited deliberately for version 2** (the authenticated form): the
    /// version word moves from `0001` to `0002` and the payload grows by the
    /// [`SECTION_AUTH`] trailer — 32 bytes of channel binding, 8 of expiry, 32
    /// of signer key, 64 of signature, plus the section's own 5-byte header.
    /// That is the change; anything else that moves this digest is not.
    #[test]
    fn golden_link_bootstrap_payload() {
        let encoded = encode_bootstrap(&fixture()).unwrap();
        // The header, in full: magic, version, created_at_ms, then the person
        // section's tag and length.
        assert_eq!(
            hex(&encoded[..LINK_BOOTSTRAP_MAGIC.len() + 2 + 8 + 1 + 4]),
            "434d424f4f54310000020000019\
             89e26ce000100000088"
                .replace(['\\', '\n', ' '], "")
        );
        // The trailer, in full: tag, length, binding, expiry, signer, signature.
        let auth_len = CHANNEL_BINDING_LEN + 8 + KEY_LEN + SIGNATURE_LEN;
        let auth = &encoded[encoded.len() - auth_len - 5..];
        assert_eq!(auth[0], SECTION_AUTH);
        assert_eq!(
            u32::from_be_bytes(auth[1..5].try_into().unwrap()) as usize,
            auth_len
        );
        assert_eq!(&auth[5..5 + CHANNEL_BINDING_LEN], &BINDING);
        assert_eq!(
            i64::from_be_bytes(
                auth[5 + CHANNEL_BINDING_LEN..5 + CHANNEL_BINDING_LEN + 8]
                    .try_into()
                    .unwrap()
            ),
            EXPIRES_AT_MS
        );
        // And the whole payload, by digest, so every later byte is pinned too.
        assert_eq!(encoded.len(), 962 + 5 + auth_len);
        assert_eq!(
            digest(&encoded),
            "312619b63d13ae7a7dc11f02bfdfaf6a7e0b277430cab58164a7104e237beba7"
        );
    }

    #[test]
    fn bootstrap_round_trips_every_field() {
        let encoded = encode_bootstrap(&fixture()).unwrap();
        assert_eq!(decode_bootstrap(&encoded).unwrap(), fixture());
    }

    /// An export that was never signed must not reach a channel, and an export
    /// with no trailer must not come off one. Both directions, because an
    /// unauthenticated bootstrap is only useful to whoever made it that way.
    #[test]
    fn an_unsigned_bootstrap_neither_encodes_nor_decodes() {
        for broken in [
            LinkBootstrap {
                signature: Vec::new(),
                ..fixture()
            },
            LinkBootstrap {
                channel_binding: Vec::new(),
                ..fixture()
            },
            LinkBootstrap {
                signer_sign_pk: vec![0xAA; 31],
                ..fixture()
            },
        ] {
            assert!(encode_bootstrap(&broken).is_err());
        }

        // A payload assembled without the trailer at all: every content section
        // present, nothing to authenticate it. Refused outright rather than
        // read as "unsigned".
        let unsigned = bootstrap_prefix(&fixture());
        assert!(decode_bootstrap(&unsigned).is_err());
    }

    /// §9.3's binding, expiry and signer, each refused on its own.
    #[test]
    fn a_bootstrap_belongs_to_one_ceremony_one_signer_and_one_hour() {
        use crate::device_roster::generate_device_keypair;
        use crate::identity::generate_identity;

        let identity = generate_identity();
        let approving = generate_device_keypair();
        let store = crate::MessageStore::open(":memory:".to_string()).unwrap();
        let roster = super::super::activation::core_link_genesis_roster(
            identity.sign_sk.clone(),
            approving.sign_pk.clone(),
            approving.agree_pk.clone(),
        )
        .unwrap();

        let now = 1_755_000_000_000_i64;
        let bootstrap = store
            .build_link_bootstrap(
                identity.clone(),
                roster.clone(),
                approving.sign_sk.clone(),
                BINDING.to_vec(),
                0,
                0,
                now,
            )
            .unwrap();

        // The happy path, through the encode/decode the channel really uses.
        let arrived = decode_bootstrap(&encode_bootstrap(&bootstrap).unwrap()).unwrap();
        assert!(core_link_bootstrap_verify(arrived.clone(), BINDING.to_vec(), now).is_ok());

        // Another ceremony's channel does not open it.
        assert!(core_link_bootstrap_verify(arrived.clone(), vec![0x01; 32], now).is_err());
        // Nor does the right channel after the export has expired.
        assert!(core_link_bootstrap_verify(
            arrived.clone(),
            BINDING.to_vec(),
            arrived.expires_at_ms
        )
        .is_ok());
        assert!(core_link_bootstrap_verify(
            arrived.clone(),
            BINDING.to_vec(),
            arrived.expires_at_ms + 1
        )
        .is_err());

        // A flipped byte anywhere in the content breaks the signature.
        let mut tampered = arrived.clone();
        tampered.groups.push(Group {
            id: vec![0xE1; 16],
            name: "Not yours".to_string(),
            member_user_ids: Vec::new(),
            key: vec![0xE2; 32],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        });
        assert!(core_link_bootstrap_verify(tampered, BINDING.to_vec(), now).is_err());

        // And a device that is not the roster-signing one cannot make an export
        // at all, nor pass one off as the approving device's.
        let sibling = generate_device_keypair();
        assert!(store
            .build_link_bootstrap(
                identity,
                roster,
                sibling.sign_sk.clone(),
                BINDING.to_vec(),
                0,
                0,
                now,
            )
            .is_err());
        let mut impostor = arrived;
        impostor.signer_sign_pk = sibling.sign_pk;
        assert!(core_link_bootstrap_verify(impostor, BINDING.to_vec(), now).is_err());
    }

    /// **The §14.2 gate.** The person root signing secret lives in the
    /// encrypted backup and nowhere else, so it may not appear in an export
    /// that crosses a link channel — not in any field, and not by accident in
    /// the bytes.
    #[test]
    fn bootstrap_carries_no_person_root_secret() {
        let identity = generate_identity();
        let mut bootstrap = fixture();
        bootstrap.person.person_id = identity.user_id.clone();
        bootstrap.person.person_sign_pk = identity.sign_pk.clone();
        // The inbox key IS the person agreement keypair at generation 0 (§6),
        // so that secret is expected in here; the signing secret is not.
        bootstrap.person.inbox_agree_pk = identity.agree_pk.clone();
        bootstrap.person.inbox_agree_sk = identity.agree_sk.clone();

        let encoded = encode_bootstrap(&bootstrap).unwrap();
        assert!(
            !encoded
                .windows(identity.sign_sk.len())
                .any(|window| window == identity.sign_sk.as_slice()),
            "the person root signing secret reached the bootstrap"
        );
        assert!(
            encoded
                .windows(identity.agree_sk.len())
                .any(|window| window == identity.agree_sk.as_slice()),
            "the inbox key must ride, or a linked device can open nothing"
        );
    }

    /// WPT forward tolerance for this format: a section a later build adds is
    /// skipped, and a newer version is the "update the app" fail-soft.
    #[test]
    fn unknown_sections_are_skipped_and_a_newer_version_is_refused() {
        let mut encoded = encode_bootstrap(&fixture()).unwrap();
        push_section(&mut encoded, 0x7f, b"a section from a later work package");
        assert_eq!(decode_bootstrap(&encoded).unwrap(), fixture());

        let mut newer = encode_bootstrap(&fixture()).unwrap();
        newer[8] = 0;
        newer[9] = (LINK_BOOTSTRAP_VERSION + 1) as u8;
        assert!(matches!(
            decode_bootstrap(&newer),
            Err(CoreError::UnsupportedLink)
        ));
    }

    #[test]
    fn truncated_and_impossible_payloads_are_refused() {
        let encoded = encode_bootstrap(&fixture()).unwrap();
        for cut in [0, 4, 8, 20, 60, 120, 200] {
            assert!(
                decode_bootstrap(&encoded[..cut]).is_err(),
                "a {cut}-byte export decoded"
            );
        }
        assert!(decode_bootstrap(b"CMBAK1\0\0\0\x01").is_err());
        // A count that claims more elements than there are bytes left.
        let mut lying = Vec::new();
        lying.extend_from_slice(LINK_BOOTSTRAP_MAGIC);
        lying.extend_from_slice(&LINK_BOOTSTRAP_VERSION.to_be_bytes());
        lying.extend_from_slice(&0_i64.to_be_bytes());
        let mut section = Vec::new();
        push_count(&mut section, u32::MAX as usize);
        push_section(&mut lying, SECTION_CONTACTS, &section);
        assert!(decode_bootstrap(&lying).is_err());
    }

    #[test]
    fn chunks_round_trip_and_refuse_a_damaged_stream() {
        let payload = encode_bootstrap(&fixture()).unwrap();
        let chunks = core_link_bootstrap_chunks(payload.clone()).unwrap();
        assert_eq!(chunks.len(), 1, "a small export is one frame");
        assert_eq!(core_link_bootstrap_join(chunks).unwrap(), payload);

        let big = vec![0x5A_u8; 200_000];
        let chunks = core_link_bootstrap_chunks(big.clone()).unwrap();
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(
                chunk.len() <= super::super::ceremony::LINK_CHANNEL_MAX_PLAINTEXT_BYTES,
                "a chunk must fit one sealed frame"
            );
        }
        assert_eq!(core_link_bootstrap_join(chunks.clone()).unwrap(), big);

        let mut reordered = chunks.clone();
        reordered.swap(0, 1);
        assert!(core_link_bootstrap_join(reordered).is_err());

        let mut short = chunks.clone();
        short.pop();
        assert!(core_link_bootstrap_join(short).is_err());

        assert!(core_link_bootstrap_join(Vec::new()).is_err());
        assert!(core_link_bootstrap_chunks(Vec::new()).is_err());
    }

    /// The WP4 seam, computed from the head: every chat the head touched, and
    /// the lamport window it covers.
    #[test]
    fn catch_up_plan_names_every_chat_the_head_truncated() {
        let mut bootstrap = fixture();
        bootstrap.history_head.push(StoredMessage {
            lamport: 3,
            ..bootstrap.history_head[0].clone()
        });
        bootstrap.history_head.push(StoredMessage {
            chat_id: vec![0xE1; 16],
            lamport: 11,
            ..bootstrap.history_head[0].clone()
        });

        let plan = core_link_catch_up_plan(bootstrap);
        assert_eq!(plan.len(), 2);
        assert_eq!(
            plan[0],
            CoreLinkCatchUp {
                chat_id: vec![0xC1; 16],
                head_from_lamport: 3,
                head_through_lamport: 7,
            }
        );
        assert_eq!(plan[1].chat_id, vec![0xE1; 16]);
        assert!(core_link_catch_up_plan(LinkBootstrap {
            history_head: Vec::new(),
            ..fixture()
        })
        .is_empty());
    }
}
