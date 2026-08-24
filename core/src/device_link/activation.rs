//! Two-phase activation, and the silence that comes before it
//! (`specs/multi-device-v1.md` §9.4).
//!
//! §9.4 is the strictest sentence in the whole spec: *the new device may not
//! advertise, author, or ack **ANYTHING** until it (a) has imported the
//! bootstrap and (b) has acknowledged the exact new roster hash back to the
//! approving device. Until then it is invisible on the mesh.*
//!
//! Two halves, and they are deliberately different in kind:
//!
//! * **The ceremony half** is pure: sign a roster at `seq + 1` including the
//!   new device's certificate ([`core_link_sign_new_device_roster`]), let the
//!   new device name its keys inside the confirmed channel
//!   ([`core_link_device_offer`]), and let it acknowledge one exact roster head
//!   back ([`core_link_activation_ack`]). No store, no clock, no transport.
//! * **The gate half** is stateful, because invisibility is a property of a
//!   device rather than of a message. [`CoreLinkActivationStage`] is persisted,
//!   and every core path that advertises, authors, or acks asks
//!   [`MessageStore::link_gate`] first.
//!
//! # Why the default is "allowed"
//!
//! Every install in the field has never linked anything, and §5's synthetic
//! one-device person must keep behaving exactly as it does today. So
//! [`CoreLinkActivationStage::NotLinking`] — the state of a store that has
//! never begun a link — permits everything, and only a device that has
//! *started* being adopted goes quiet. The refusal is not a general safety
//! interlock bolted onto the app; it is the narrow window between "a stranger's
//! phone opened a channel to my phone" and "that phone is a device of mine",
//! and it closes by acknowledging a hash.
//!
//! An unrecognised stage — one a later build wrote and this one cannot read —
//! is treated as *not activated*. A downgrade that cannot tell whether it has
//! finished being adopted has not finished being adopted.
//!
//! # What the gate is, and what it is not
//!
//! It is not a second opinion about DTN ack safety. ACK-MD-3 and the carry
//! queue's digest-proof rule are untouched: this only ever *subtracts* — an
//! empty hint set, an empty ack list, a refused author — so no path that was
//! safe becomes unsafe, and a pre-activation device that would have acked
//! something now acks nothing at all.

use rusqlite::{params, OptionalExtension};

use super::bootstrap::{decode_roster_document, encode_roster_document, LinkBootstrap};
use crate::device_roster::{
    core_device_add_outcome, core_device_sign, core_device_verify, core_roster_validate,
    core_sign_device_cert, core_sign_roster, roster_head_hash, DeviceAddOutcome, DeviceCert,
    DeviceSigningDomain, OwnDeviceFleet, Roster, DEVICE_CERT_FLAG_ROSTER_SIGNING, DEVICE_ID_LEN,
    ROSTER_HEAD_HASH_LEN,
};
use crate::identity::derive_user_id;
use crate::store::store_err;
use crate::{CoreError, MessageStore};

/// X25519 / Ed25519 public keys and Ed25519 secret keys are all 32 bytes.
const KEY_LEN: usize = 32;
/// The Noise handshake hash both halves of the ceremony hold.
const CHANNEL_BINDING_LEN: usize = 32;
const SIGNATURE_LEN: usize = 64;

/// Frame tags for the two signed control frames that ride the confirmed
/// channel. Distinct from the ceremony's own `CTRL_*` tags by construction:
/// those are consumed before [`CoreLinkPhase::ChannelReady`](super::ceremony::CoreLinkPhase),
/// these only after.
const FRAME_DEVICE_OFFER: u8 = 0x11;
const FRAME_ACTIVATION_ACK: u8 = 0x12;

pub(crate) const DEVICE_LINK_SCHEMA_SQL: &str = "
-- §9.4's two-phase activation, as one row. A store with no row here has never
-- begun a link, which is every install in the field and is the state that
-- permits everything (see `CoreLinkActivationStage::NotLinking`).
--
-- `stage` is text rather than an integer so a database dump reads as English
-- during a field investigation; an unreadable value fails closed.
--
-- It rides a `.cmbak` with the rest of the store, and correctly so: restoring a
-- backup is §9's \"Replace this device\", and a replacement really is the device
-- that was already activated. The other branch — \"Link as new device\"
-- (`device_link::restore`) — does not restore the store at all, so it starts
-- from an empty table and is adopted like any other new phone.
CREATE TABLE IF NOT EXISTS device_link_activation (
    id                       INTEGER PRIMARY KEY CHECK (id = 0),
    stage                    TEXT    NOT NULL,
    expected_roster_head     BLOB,
    own_device_id            BLOB,
    channel_binding          BLOB,
    started_at_ms            INTEGER NOT NULL DEFAULT 0,
    bootstrap_imported_at_ms INTEGER NOT NULL DEFAULT 0,
    activated_at_ms          INTEGER NOT NULL DEFAULT 0
);

-- This person's OWN roster document (§4), as the canonical bytes
-- `device_link::bootstrap` encodes. Deliberately NOT a row in
-- `contact_rosters`: that table is swept against the contact list at every
-- open, and a person is not their own contact.
--
-- One row. The projection routing and acks read is `own_device_fleet`; this is
-- the document that projection came from, kept whole because the next roster
-- update has to be signed against it and a head hash must be recomputable
-- byte-for-byte.
CREATE TABLE IF NOT EXISTS own_roster (
    id       INTEGER PRIMARY KEY CHECK (id = 0),
    document BLOB    NOT NULL
);
";

// ---------------------------------------------------------------------------
// Roster authorship at link time (§9.4, first half)
// ---------------------------------------------------------------------------

/// A roster update that adds one device, plus everything the caller needs to
/// drive §9.4 from it.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct LinkRosterUpdate {
    /// The signed roster at `seq + 1`.
    pub roster: Roster,
    /// The 16-byte id of the device just added.
    pub new_device_id: Vec<u8>,
    /// [`core_roster_head_hash`](crate::core_roster_head_hash) of `roster` —
    /// the exact value §9.4(b) has the new device acknowledge.
    pub roster_head: Vec<u8>,
    /// §14.3 for THIS add, on the own-roster path where the soft-cap warning
    /// belongs (a person's own device count is their business; a contact's is
    /// not — see [`RosterUpdateDecision`](crate::RosterUpdateDecision)).
    pub add_outcome: DeviceAddOutcome,
}

/// Genesis: the first roster a person ever has, naming the device they are
/// holding as the approving device (§3, §4).
///
/// Root-signed, because [`core_roster_validate`] requires `seq == 0` to be —
/// and because there is nothing else yet to sign it. This is the identity
/// upgrade of §2 goal 2: the deployed Ed25519 key becomes the person root and
/// the phone it is on becomes device one, with `person_id` (the wire
/// `user_id`), chat ids, and fingerprints all unchanged.
#[uniffi::export]
pub fn core_link_genesis_roster(
    person_root_sign_sk: Vec<u8>,
    device_sign_pk: Vec<u8>,
    device_agree_pk: Vec<u8>,
) -> Result<Roster, CoreError> {
    let person_root_sign_pk = public_of(&person_root_sign_sk)?;
    let person_id = derive_user_id(&person_root_sign_pk).to_vec();
    let cert = sign_cert(
        &person_id,
        &device_sign_pk,
        &device_agree_pk,
        0,
        DEVICE_CERT_FLAG_ROSTER_SIGNING,
        &person_root_sign_sk,
    )?;
    let roster = Roster {
        person_id,
        recovery_epoch: 0,
        seq: 0,
        approving_device_id: cert.device_id(),
        devices: vec![cert],
        tombstones: Vec::new(),
        inbox_key_generation: 0,
        signer_sign_pk: Vec::new(),
        signature: Vec::new(),
    };
    validated(
        core_sign_roster(roster, person_root_sign_sk)?,
        &person_root_sign_pk,
    )
}

/// §14.2's recovery path: a roster at the NEXT recovery epoch, signed with the
/// person root secret that was just opened out of a `.cmbak`.
///
/// This is what "Link as new device" reaches for when there is no approving
/// device left to ask — a lost or stolen phone, and a backup in hand. A higher
/// `recovery_epoch` supersedes anything the old approving device ever signed
/// (DL-1, and `core_roster_accept`'s rule that only the root may climb), which
/// is precisely how §3 dethrones a stolen approving device.
///
/// It starts the epoch with this device alone. Note what it deliberately does
/// NOT do: it does not tombstone the devices it drops. Burying a device is
/// §10's revocation — [`crate::core_recovery_revoke_roster`] is the same epoch
/// climb that *does* bury, and names which devices it is burying — while
/// dropping one here simply means the new epoch does not vouch for it. The
/// distinction is the product difference between "I lost my phone" and "I am
/// setting up a replacement": the second must not silently unlink the tablet.
/// Tombstones already recorded are carried forward untouched either way,
/// because DL-4 is forever.
///
/// **The inbox key generation moves, the key material does not.** §14.2 is
/// reached because a device was lost or stolen, so every contact must be told
/// to stop sealing to what that device could open — and §6 says the way a
/// person says that is by advancing `inbox_key_generation`. So this bumps it.
/// What it does not do is mint the new keypair: rotating the actual inbox key,
/// and re-sealing to it, belongs to §10's revocation
/// ([`crate::core_recovery_revoke_roster`], which does both), so on THIS path
/// the generation this roster announces runs ahead of the material behind it.
/// That is a deliberate, stated gap — a generation that never moved would be
/// worse, because contacts would have no signal at all that anything had
/// changed — and a caller that means "cut that phone off" should be reaching for
/// the revocation instead.
///
/// The `.cmbak` restore path (`ReplaceThisDevice`) does not come through here
/// and must not: nothing was recovered, so nothing is announced.
#[uniffi::export]
pub fn core_link_recovery_roster(
    stored: Option<Roster>,
    person_root_sign_sk: Vec<u8>,
    device_sign_pk: Vec<u8>,
    device_agree_pk: Vec<u8>,
) -> Result<Roster, CoreError> {
    let person_root_sign_pk = public_of(&person_root_sign_sk)?;
    let person_id = derive_user_id(&person_root_sign_pk).to_vec();
    let Some(stored) = stored else {
        // Nothing to supersede: this person has no roster at all, so recovery
        // and genesis are the same document.
        return core_link_genesis_roster(person_root_sign_sk, device_sign_pk, device_agree_pk);
    };
    if stored.person_id != person_id {
        return Err(CoreError::Malformed(
            "recovery roster is for a different person than the backup".to_string(),
        ));
    }
    let recovery_epoch = stored.recovery_epoch.saturating_add(1);
    let cert = sign_cert(
        &person_id,
        &device_sign_pk,
        &device_agree_pk,
        recovery_epoch,
        DEVICE_CERT_FLAG_ROSTER_SIGNING,
        &person_root_sign_sk,
    )?;
    if stored
        .tombstones
        .iter()
        .any(|tombstone| tombstone.device_id == cert.device_id())
    {
        // DL-4: a revoked device id never comes back, not even through the
        // recovery path. Re-linking the same hardware mints a fresh key.
        return Err(CoreError::Malformed(
            "this device id was revoked and can never return".to_string(),
        ));
    }
    let roster = Roster {
        person_id,
        recovery_epoch,
        // A new epoch resets the sequence; `core_roster_validate` requires the
        // resulting genesis document to be root-signed, which it is.
        seq: 0,
        approving_device_id: cert.device_id(),
        devices: vec![cert],
        tombstones: stored.tombstones,
        inbox_key_generation: stored.inbox_key_generation.saturating_add(1),
        signer_sign_pk: Vec::new(),
        signature: Vec::new(),
    };
    validated(
        core_sign_roster(roster, person_root_sign_sk)?,
        &person_root_sign_pk,
    )
}

/// **§9.4's first half.** The approving device signs the roster at `seq + 1`,
/// including the new device's certificate.
///
/// Signed by the approving device's key, never the root: §3's authority split
/// says roster changes take the approving-device key or the recovery material,
/// and the root secret is in the backup where §14.2 keeps it. The result is
/// re-validated here against the person root before it is handed back, so a
/// document that could not be accepted by a contact is never one this device
/// produced.
#[uniffi::export]
pub fn core_link_sign_new_device_roster(
    current: Roster,
    person_root_sign_pk: Vec<u8>,
    approving_device_sign_sk: Vec<u8>,
    new_device_sign_pk: Vec<u8>,
    new_device_agree_pk: Vec<u8>,
) -> Result<LinkRosterUpdate, CoreError> {
    if let Some(rejection) = core_roster_validate(current.clone(), person_root_sign_pk.clone()) {
        return Err(CoreError::Malformed(format!(
            "the roster to extend is not acceptable: {rejection:?}"
        )));
    }
    let approving_pk = public_of(&approving_device_sign_sk)?;
    if derive_user_id(&approving_pk).to_vec() != current.approving_device_id {
        // §3: exactly one device holds the roster-signing role, and this key is
        // not it. Refused here rather than producing a document every contact
        // would reject as `SignerNotAuthorized`.
        return Err(CoreError::Malformed(
            "this device does not hold the roster-signing role".to_string(),
        ));
    }
    let cert = sign_cert(
        &current.person_id,
        &new_device_sign_pk,
        &new_device_agree_pk,
        current.recovery_epoch,
        0,
        &approving_device_sign_sk,
    )?;
    let new_device_id = cert.device_id();
    if current
        .devices
        .iter()
        .any(|listed| listed.device_id() == new_device_id)
    {
        return Err(CoreError::Malformed(
            "this device is already in the roster".to_string(),
        ));
    }
    if current
        .tombstones
        .iter()
        .any(|tombstone| tombstone.device_id == new_device_id)
    {
        // DL-4.
        return Err(CoreError::Malformed(
            "this device id was revoked and can never return".to_string(),
        ));
    }
    // §14.3 through the one function that owns the boundary, on the count AFTER
    // the add — the same call `core_roster_validate` makes about the resulting
    // document, so a refusal here and a refusal there can never disagree.
    let add_outcome = core_device_add_outcome(current.devices.len() as u32 + 1);
    if add_outcome == DeviceAddOutcome::Refused {
        return Err(CoreError::Malformed(
            "this person already holds the maximum number of devices".to_string(),
        ));
    }

    let mut devices = current.devices.clone();
    devices.push(cert);
    let roster = Roster {
        seq: current.seq.saturating_add(1),
        devices,
        signer_sign_pk: Vec::new(),
        signature: Vec::new(),
        ..current
    };
    let roster = validated(
        core_sign_roster(roster, approving_device_sign_sk)?,
        &person_root_sign_pk,
    )?;
    Ok(LinkRosterUpdate {
        roster_head: roster_head_hash(&roster),
        roster,
        new_device_id,
        add_outcome,
    })
}

// ---------------------------------------------------------------------------
// The two signed frames that ride the confirmed channel
// ---------------------------------------------------------------------------

/// What the new device says first once the channel is confirmed (§9.3): the
/// keys it just minted, bound to this channel and to nothing else.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct LinkDeviceOffer {
    pub device_sign_pk: Vec<u8>,
    pub device_agree_pk: Vec<u8>,
    /// The 16-byte id derived from `device_sign_pk`.
    pub device_id: Vec<u8>,
}

/// §9.4(b): one exact roster head, acknowledged by the device that imported it.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct LinkActivationAck {
    pub device_id: Vec<u8>,
    pub roster_head: Vec<u8>,
}

/// Build the new device's offer frame, signed with the key it is asking to have
/// certified.
///
/// The signature is what makes the certificate mean something: without it the
/// approving device would be certifying a public key that arrived over a
/// channel, with no evidence anyone holds the secret half. The channel binding
/// is inside the signed bytes, so an offer captured from one ceremony is not a
/// valid offer in another.
#[uniffi::export]
pub fn core_link_device_offer(
    device_sign_sk: Vec<u8>,
    device_agree_pk: Vec<u8>,
    channel_binding: Vec<u8>,
) -> Result<Vec<u8>, CoreError> {
    let device_sign_pk = public_of(&device_sign_sk)?;
    check_len(&device_agree_pk, KEY_LEN, "device agreement key")?;
    check_len(&channel_binding, CHANNEL_BINDING_LEN, "channel binding")?;
    let signature = core_device_sign(
        DeviceSigningDomain::DeviceLinkActivation,
        device_sign_sk,
        signed_message(FRAME_DEVICE_OFFER, &device_agree_pk, &channel_binding),
    )?;
    let mut frame = vec![FRAME_DEVICE_OFFER];
    frame.extend_from_slice(&device_sign_pk);
    frame.extend_from_slice(&device_agree_pk);
    frame.extend_from_slice(&channel_binding);
    frame.extend_from_slice(&signature);
    Ok(frame)
}

/// Open an offer frame on the approving device, refusing anything that is not
/// signed by the key it names, for this channel.
#[uniffi::export]
pub fn core_link_open_device_offer(
    frame: Vec<u8>,
    channel_binding: Vec<u8>,
) -> Result<LinkDeviceOffer, CoreError> {
    check_len(&channel_binding, CHANNEL_BINDING_LEN, "channel binding")?;
    let parts = split_frame(&frame, FRAME_DEVICE_OFFER)?;
    let LinkFrameParts {
        device_sign_pk,
        field: device_agree_pk,
        channel_binding: frame_binding,
        signature,
    } = parts;
    if frame_binding != channel_binding {
        return Err(CoreError::Malformed(
            "device link offer is bound to a different channel".to_string(),
        ));
    }
    core_device_verify(
        DeviceSigningDomain::DeviceLinkActivation,
        device_sign_pk.clone(),
        signed_message(FRAME_DEVICE_OFFER, &device_agree_pk, &frame_binding),
        signature,
    )?;
    Ok(LinkDeviceOffer {
        device_id: derive_user_id(&device_sign_pk).to_vec(),
        device_sign_pk,
        device_agree_pk,
    })
}

/// **§9.4(b).** The new device acknowledges the exact roster hash it imported.
#[uniffi::export]
pub fn core_link_activation_ack(
    device_sign_sk: Vec<u8>,
    roster_head: Vec<u8>,
    channel_binding: Vec<u8>,
) -> Result<Vec<u8>, CoreError> {
    let device_sign_pk = public_of(&device_sign_sk)?;
    check_len(&roster_head, ROSTER_HEAD_HASH_LEN, "roster head")?;
    check_len(&channel_binding, CHANNEL_BINDING_LEN, "channel binding")?;
    let signature = core_device_sign(
        DeviceSigningDomain::DeviceLinkActivation,
        device_sign_sk,
        signed_message(FRAME_ACTIVATION_ACK, &roster_head, &channel_binding),
    )?;
    let mut frame = vec![FRAME_ACTIVATION_ACK];
    frame.extend_from_slice(&device_sign_pk);
    frame.extend_from_slice(&roster_head);
    frame.extend_from_slice(&channel_binding);
    frame.extend_from_slice(&signature);
    Ok(frame)
}

/// Open an acknowledgement on the approving device.
///
/// Four things must all hold, and the second is the point of §9.4: the frame is
/// bound to this channel, it names **exactly** the head of the roster that was
/// signed for this ceremony — not a later one, not a re-derived one, the same
/// 32 bytes — it is signed by the very device that made the offer this ceremony
/// certified, and that device is one the roster lists.
///
/// `offered_device_sign_pk` is what closes the last gap. Roster membership
/// alone is a weaker claim than it looks: on a fleet that already holds
/// siblings, *any* listed device's signature would satisfy it, so an existing
/// phone could close a ceremony that was opened for a different one — and the
/// approving device would record the link as done while the phone in the
/// person's hand stayed silent forever. Requiring the offered key means the
/// device that finishes the ceremony is the device that started it. Membership
/// stays as the second check rather than being replaced by this one: they fail
/// for different reasons, and a roster that does not list the offering device
/// is a bug worth hearing about separately.
///
/// Anything else leaves the new device un-activated and therefore silent.
#[uniffi::export]
pub fn core_link_open_activation_ack(
    frame: Vec<u8>,
    roster: Roster,
    offered_device_sign_pk: Vec<u8>,
    channel_binding: Vec<u8>,
) -> Result<LinkActivationAck, CoreError> {
    check_len(&channel_binding, CHANNEL_BINDING_LEN, "channel binding")?;
    check_len(&offered_device_sign_pk, KEY_LEN, "offered device key")?;
    let LinkFrameParts {
        device_sign_pk,
        field: roster_head,
        channel_binding: frame_binding,
        signature,
    } = split_frame(&frame, FRAME_ACTIVATION_ACK)?;
    if frame_binding != channel_binding {
        return Err(CoreError::Malformed(
            "activation acknowledgement is bound to a different channel".to_string(),
        ));
    }
    if roster_head != roster_head_hash(&roster) {
        return Err(CoreError::Malformed(
            "activation acknowledgement names a different roster".to_string(),
        ));
    }
    if device_sign_pk != offered_device_sign_pk {
        return Err(CoreError::Malformed(
            "activation acknowledgement is from a device other than the one this ceremony offered"
                .to_string(),
        ));
    }
    let device_id = derive_user_id(&device_sign_pk).to_vec();
    if !roster
        .devices
        .iter()
        .any(|cert| cert.device_sign_pk == device_sign_pk)
    {
        return Err(CoreError::Malformed(
            "activation acknowledgement is from a device this roster does not list".to_string(),
        ));
    }
    core_device_verify(
        DeviceSigningDomain::DeviceLinkActivation,
        device_sign_pk,
        signed_message(FRAME_ACTIVATION_ACK, &roster_head, &frame_binding),
        signature,
    )?;
    Ok(LinkActivationAck {
        device_id,
        roster_head,
    })
}

// ---------------------------------------------------------------------------
// The gate (§9.4's "invisible on the mesh")
// ---------------------------------------------------------------------------

/// Where this device is in its own adoption.
#[derive(uniffi::Enum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CoreLinkActivationStage {
    /// No link ceremony has begun on this install. Every device in the field
    /// today, and §5's synthetic one-device person: everything is permitted,
    /// exactly as before this gate existed.
    #[default]
    NotLinking,
    /// A channel was confirmed and the bootstrap has not landed (§9.4a).
    AwaitingBootstrap,
    /// The bootstrap landed; the exact roster head has not been acknowledged
    /// back (§9.4b).
    AwaitingRosterAck,
    /// Both halves done. This device is a device.
    Activated,
    /// **§10 step 5.** This device read a signed roster of its own person that
    /// tombstones it, and ejected itself: it no longer advertises, authors or
    /// acks, and it holds no fleet projection.
    ///
    /// Terminal. DL-4 says a revoked `device_id` is gone forever, so the only
    /// way out is a fresh ceremony under a fresh device key — which is a fresh
    /// install, not a flag flip. [`MessageStore::begin_link_activation`] and
    /// [`MessageStore::abandon_link_activation`] both refuse from here for that
    /// reason.
    ///
    /// Note what it is NOT: an opinion of this device's about whether it should
    /// still be trusted. It is the person's decision, arriving as a document
    /// signed under their root and strictly superseding the one this device
    /// held. Nothing a stranger can mint reaches this state.
    Revoked,
}

/// The three things §9.4 forbids before activation.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreLinkGatedAction {
    /// Announcing this device's existence or reachability: HELLO, LAN
    /// endpoints, relay hint sets, carry offers.
    Advertise,
    /// Minting anything signed and outbound: messages, receipts, invites.
    Author,
    /// Deleting a relay row or retiring a carried envelope on the person's
    /// behalf.
    Ack,
}

/// Why the gate answered the way it did.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreLinkGateReason {
    /// This install has never linked; nothing to gate.
    NeverLinked,
    /// Activation completed.
    Activated,
    /// §9.4a: the bootstrap has not been imported.
    BootstrapPending,
    /// §9.4b: the exact roster head has not been acknowledged.
    RosterAckPending,
    /// §10 step 5: this device's person removed it, and it has read the signed
    /// roster that says so. Not a window that closes — see
    /// [`CoreLinkActivationStage::Revoked`].
    DeviceRevoked,
}

/// One gate answer.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreLinkGateVerdict {
    pub allowed: bool,
    pub stage: CoreLinkActivationStage,
    pub action: CoreLinkGatedAction,
    pub reason: CoreLinkGateReason,
}

/// This device's activation record.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq, Default)]
pub struct CoreLinkActivation {
    pub stage: CoreLinkActivationStage,
    /// §9.4(b)'s target: the exact head that must come back, once the
    /// bootstrap has been imported.
    pub expected_roster_head: Option<Vec<u8>>,
    /// The device id this install was certified under, once it knows one.
    pub own_device_id: Option<Vec<u8>>,
    /// The ceremony this window belongs to: the Noise handshake hash recorded
    /// by [`MessageStore::begin_link_activation`]. A bootstrap whose own
    /// binding is not this one is not this ceremony's bootstrap, whatever else
    /// is true about it. `None` on a store that never began a link, and on a
    /// dev install that began one under the pre-binding schema — where it means
    /// "no ceremony recorded", and the import is refused.
    pub channel_binding: Option<Vec<u8>>,
    /// When this row was last written: the moment the window opened, or — on a
    /// `NotLinking` row that is not simply absent — the moment a failed
    /// ceremony was abandoned and the gates reopened.
    pub started_at_ms: i64,
    pub bootstrap_imported_at_ms: i64,
    pub activated_at_ms: i64,
}

/// **The gate.** Pure, so the rule can be read in one place and tested without
/// a database; [`MessageStore::link_gate`] is the same rule applied to what is
/// stored.
#[uniffi::export]
pub fn core_link_activation_gate(
    activation: CoreLinkActivation,
    action: CoreLinkGatedAction,
) -> CoreLinkGateVerdict {
    let (allowed, reason) = match activation.stage {
        CoreLinkActivationStage::NotLinking => (true, CoreLinkGateReason::NeverLinked),
        CoreLinkActivationStage::Activated => (true, CoreLinkGateReason::Activated),
        CoreLinkActivationStage::AwaitingBootstrap => (false, CoreLinkGateReason::BootstrapPending),
        CoreLinkActivationStage::AwaitingRosterAck => (false, CoreLinkGateReason::RosterAckPending),
        // §10 step 5. All three, not just Advertise: a removed device that
        // stopped announcing itself but kept authoring, or kept acking, would
        // still be deleting relay rows and minting signed mail on a person's
        // behalf after that person removed it.
        CoreLinkActivationStage::Revoked => (false, CoreLinkGateReason::DeviceRevoked),
    };
    CoreLinkGateVerdict {
        allowed,
        stage: activation.stage,
        action,
        reason,
    }
}

/// What a refused action looks like to a caller. One sentence, no jargon: the
/// shells surface their own copy, and this is the log line.
pub(crate) fn refusal(verdict: CoreLinkGateVerdict) -> CoreError {
    let action = match verdict.action {
        CoreLinkGatedAction::Advertise => "appear on the mesh",
        CoreLinkGatedAction::Author => "send",
        CoreLinkGatedAction::Ack => "acknowledge mail",
    };
    // Two refusals, not one sentence with a wrong word in it: "still being set
    // up" is a window that closes, and §10's is a door that does not.
    if verdict.stage == CoreLinkActivationStage::Revoked {
        return CoreError::Store(format!(
            "this device was removed from its person's devices and cannot {action} ({:?})",
            verdict.reason
        ));
    }
    CoreError::Store(format!(
        "this device is still being set up and cannot {action} yet ({:?})",
        verdict.reason
    ))
}

// ---------------------------------------------------------------------------
// The store side
// ---------------------------------------------------------------------------

/// Whether this store may take a bootstrap at all (§9.3).
///
/// The question is asked *before* a ceremony starts as well as during the
/// import, so a shell can say "not this phone" on the setup screen rather than
/// after a person has held two phones together and compared six digits.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreLinkImportReadiness {
    /// A phone with nothing of its own to lose, or one already known to belong
    /// to the person being restored.
    Ready,
    /// This store already holds contacts, groups or messages, and the caller
    /// did not say whose they are. Importing would fold one person's world into
    /// another's — a silent, unrecoverable merge, and the exact shape of §1's
    /// failure with the identities swapped.
    StoreHoldsSomeone,
    /// This store already holds a roster, and it is a different person's.
    StoreHoldsAnotherPerson,
}

/// What an import left behind.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CoreLinkBootstrapImport {
    /// The identity material the shell must persist in platform-protected
    /// storage, exactly as it persists a generated [`Identity`](crate::Identity).
    /// The core does not keep secrets.
    pub person: super::bootstrap::LinkBootstrapPerson,
    /// The person's own name and photo, straight off the export
    /// ([`LinkBootstrapProfile`](super::bootstrap::LinkBootstrapProfile)). A
    /// passthrough for the same reason `person` is: core keeps no profile row,
    /// and the shell that owns the name is the one that must write it. A shell
    /// that drops this is a shell that asks a person their own name on a phone
    /// that already knows it.
    pub profile: super::bootstrap::LinkBootstrapProfile,
    /// This device's own id, taken from the certificate the roster carries for
    /// it — never from the caller.
    pub own_device_id: Vec<u8>,
    /// §9.4(b)'s target: acknowledge exactly this.
    pub roster_head: Vec<u8>,
    pub contacts_imported: u32,
    pub contact_rosters_imported: u32,
    pub groups_imported: u32,
    pub messages_imported: u32,
    /// The WP4 seam, computed from what just landed.
    pub catch_up: Vec<super::bootstrap::CoreLinkCatchUp>,
}

/// How often a live own-device link is re-offered §10 step 5's roster.
///
/// See [`core_own_roster_notice_reoffer_due`]. Small because the document is
/// small and the link is a LAN socket to the person's own phone; long enough
/// that it is a heartbeat rather than chatter.
pub const OWN_ROSTER_NOTICE_REOFFER_INTERVAL_MS: i64 = 60_000;

/// [`OWN_ROSTER_NOTICE_REOFFER_INTERVAL_MS`], for the shells (a `const` does
/// not cross the binding; a function does).
#[uniffi::export]
pub fn core_own_roster_notice_reoffer_interval_ms() -> i64 {
    OWN_ROSTER_NOTICE_REOFFER_INTERVAL_MS
}

/// **§10 step 5, the part that was missing.** Whether an own-device link that
/// has already been offered this person's roster is owed it again.
///
/// The notice shipped edge-triggered: built and pushed at the instant a HELLO2
/// arrived on an own-device link, and at no other moment in either shell. So a
/// removal that happened while such a link was *already up* was never announced
/// on it — no new HELLO, no new offer, and the removed phone kept believing it
/// was linked for as long as the link lasted. In the field that was 26 minutes,
/// two force-stops and a reboot.
///
/// Making it level-triggered is what fixes that, and it is safe to do bluntly
/// because the frame is idempotent in both directions: the sender's copy is
/// rebuilt from the store every time, and
/// [`MessageStore::apply_own_roster_notice`] refuses anything that does not
/// strictly supersede what the receiver holds. Re-offering costs one signed
/// document per minute per own-device link and needs no event plumbing to reach
/// it — which also means it survives an app restart, a reboot, and a roster
/// change this process never saw.
///
/// `None` means never offered on this link, which is always due.
#[uniffi::export]
pub fn core_own_roster_notice_reoffer_due(last_offered_at_ms: Option<i64>, now_ms: i64) -> bool {
    let Some(last) = last_offered_at_ms else {
        return true;
    };
    // Saturating, and a clock that jumped backwards is due rather than stuck:
    // the notice is idempotent, so the failure worth avoiding is silence.
    now_ms < last || now_ms.saturating_sub(last) >= OWN_ROSTER_NOTICE_REOFFER_INTERVAL_MS
}

#[uniffi::export]
impl MessageStore {
    /// This device's activation record (§9.4). A store that has never begun a
    /// link reads [`CoreLinkActivationStage::NotLinking`].
    pub fn link_activation(&self) -> Result<CoreLinkActivation, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let row = conn
            .query_row(
                "SELECT stage, expected_roster_head, own_device_id, channel_binding,
                        started_at_ms, bootstrap_imported_at_ms, activated_at_ms
                 FROM device_link_activation WHERE id = 0",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<Vec<u8>>>(1)?,
                        row.get::<_, Option<Vec<u8>>>(2)?,
                        row.get::<_, Option<Vec<u8>>>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, i64>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(store_err)?;
        let Some((
            stage,
            expected_roster_head,
            own_device_id,
            channel_binding,
            started_at_ms,
            bootstrap_imported_at_ms,
            activated_at_ms,
        )) = row
        else {
            return Ok(CoreLinkActivation::default());
        };
        Ok(CoreLinkActivation {
            stage: stage_from_text(&stage),
            expected_roster_head,
            own_device_id,
            channel_binding,
            started_at_ms,
            bootstrap_imported_at_ms,
            activated_at_ms,
        })
    }

    /// Ask the gate about one action (§9.4). Every core path that advertises,
    /// authors, or acks goes through this; shells consult it for the paths core
    /// cannot see, above all their own BLE and LAN advertising.
    pub fn link_gate(&self, action: CoreLinkGatedAction) -> Result<CoreLinkGateVerdict, CoreError> {
        Ok(core_link_activation_gate(self.link_activation()?, action))
    }

    /// Whether this store may take a bootstrap (§9.3), and if not, why.
    ///
    /// `expected_person_id` is the person the caller believes this phone is
    /// being set up as — the id read out of an opened `.cmbak`, or whatever the
    /// restore flow already knows. `None` means "the caller does not know", and
    /// is only acceptable on a phone with nothing of its own: a bootstrap
    /// imported over a store that already holds someone's contacts and messages
    /// merges two people's worlds with no way back.
    ///
    /// Asked here as well as inside [`Self::import_link_bootstrap`] so a shell
    /// can refuse on the screen that offers the ceremony instead of at the end
    /// of one.
    pub fn link_import_readiness(
        &self,
        expected_person_id: Option<Vec<u8>>,
    ) -> Result<CoreLinkImportReadiness, CoreError> {
        if let Some(existing) = self.own_roster()? {
            return Ok(match &expected_person_id {
                Some(expected) if *expected == existing.person_id => CoreLinkImportReadiness::Ready,
                _ => CoreLinkImportReadiness::StoreHoldsAnotherPerson,
            });
        }
        if self.store_is_factory_fresh()? {
            return Ok(CoreLinkImportReadiness::Ready);
        }
        match expected_person_id {
            // The caller vouched for whose phone this is. There is no roster to
            // check it against, so this is as far as core can see; the shell
            // that opened the backup is the one that knows.
            Some(_) => Ok(CoreLinkImportReadiness::Ready),
            None => Ok(CoreLinkImportReadiness::StoreHoldsSomeone),
        }
    }

    /// Enter §9.4's pre-activation window: from here this device is silent
    /// until it has imported a bootstrap and acknowledged the roster head.
    ///
    /// Called by the NEW device when its ceremony reaches a confirmed channel.
    /// Deliberately its own step rather than a side effect of importing: the
    /// silence has to start before the bootstrap arrives, or a device would be
    /// free to act during the very window the export is crossing the channel.
    ///
    /// `channel_binding` is that ceremony's Noise handshake hash, recorded here
    /// so the import can insist the export it is handed was made for *this*
    /// ceremony rather than merely arriving on it.
    pub fn begin_link_activation(
        &self,
        channel_binding: Vec<u8>,
        now_ms: i64,
    ) -> Result<CoreLinkActivation, CoreError> {
        check_len(&channel_binding, CHANNEL_BINDING_LEN, "channel binding")?;
        let current = self.link_activation()?;
        if current.stage == CoreLinkActivationStage::Activated {
            // An activated device is not a new device. Being adopted again is a
            // fresh install's business, so this refuses rather than quietly
            // unlinking a working phone.
            return Err(CoreError::Store(
                "this device is already linked; linking it again starts from a fresh install"
                    .to_string(),
            ));
        }
        if current.stage == CoreLinkActivationStage::Revoked {
            // DL-4: the id this install held is buried forever, and a ceremony
            // begun on top of this store would run under the device key that id
            // was derived from. Re-linking this hardware means a fresh install
            // with a fresh device key, which is a fresh store and therefore no
            // row here at all.
            return Err(CoreError::Store(
                "this device was removed; setting it up again starts from a fresh install"
                    .to_string(),
            ));
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO device_link_activation
                (id, stage, expected_roster_head, own_device_id, channel_binding,
                 started_at_ms, bootstrap_imported_at_ms, activated_at_ms)
             VALUES (0, ?1, NULL, NULL, ?2, ?3, 0, 0)
             ON CONFLICT(id) DO UPDATE SET
                 stage = excluded.stage,
                 expected_roster_head = NULL,
                 own_device_id = NULL,
                 channel_binding = excluded.channel_binding,
                 started_at_ms = excluded.started_at_ms,
                 bootstrap_imported_at_ms = 0,
                 activated_at_ms = 0",
            params![
                stage_text(CoreLinkActivationStage::AwaitingBootstrap),
                channel_binding,
                now_ms
            ],
        )
        .map_err(store_err)?;
        drop(conn);
        self.link_activation()
    }

    /// **Leave §9.4's window the way it was entered.** A ceremony that began
    /// and did not finish gives the gates back.
    ///
    /// Without this the window is a one-way door: the moment
    /// [`Self::begin_link_activation`] runs, this device stops advertising,
    /// authoring and acking, and the ONLY exit is a successful
    /// [`Self::complete_link_activation`]. A declined confirm, a dropped
    /// socket, a peer that stopped answering, a person who put the phone down —
    /// each of those left a phone permanently silent with no way back except
    /// reinstalling the app. That is a worse failure than the one the gate
    /// exists to prevent.
    ///
    /// It refuses from [`CoreLinkActivationStage::Activated`] and from nothing
    /// else. An activated device is a device: unlinking one is §10's revocation
    /// with its key rotations and its roster update, not a local flag flip, and
    /// a call that could quietly undo a completed link would be a far easier
    /// mistake to make than the one it fixes. From `NotLinking` it is a no-op,
    /// so a failure path may call it without first asking where it is.
    ///
    /// Both `expected_roster_head` and `own_device_id` are cleared: a device id
    /// this install was never activated under must not survive to be read as
    /// evidence that it was.
    pub fn abandon_link_activation(&self, now_ms: i64) -> Result<CoreLinkActivation, CoreError> {
        let current = self.link_activation()?;
        match current.stage {
            CoreLinkActivationStage::Activated => {
                return Err(CoreError::Store(
                    "this device is linked; removing it is a roster change, not a local reset"
                        .to_string(),
                ));
            }
            CoreLinkActivationStage::Revoked => {
                // Not a ceremony that failed — a device that was removed. The
                // gates it closed are §10's, and giving them back locally would
                // be this device overruling its person's decision.
                return Err(CoreError::Store(
                    "this device was removed from its person's devices; that is not a local state \
                     to reset"
                        .to_string(),
                ));
            }
            CoreLinkActivationStage::NotLinking => return Ok(current),
            CoreLinkActivationStage::AwaitingBootstrap
            | CoreLinkActivationStage::AwaitingRosterAck => {}
        }
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "UPDATE device_link_activation
                SET stage = ?1, expected_roster_head = NULL, own_device_id = NULL,
                    channel_binding = NULL, started_at_ms = ?2,
                    bootstrap_imported_at_ms = 0, activated_at_ms = 0
              WHERE id = 0",
            params![stage_text(CoreLinkActivationStage::NotLinking), now_ms],
        )
        .map_err(store_err)?;
        drop(conn);
        self.link_activation()
    }

    /// **§9.4(a).** Import a canonical bootstrap (§9.3) into this device.
    ///
    /// `own_device_sign_pk` is the key this device offered; the roster must
    /// carry a certificate for it, and the device id this install adopts is
    /// taken from that certificate rather than from the caller — a device does
    /// not get to name itself.
    ///
    /// Everything is checked before anything is written: the roster validates
    /// against the person root the export names, the person id agrees with that
    /// root, and the inbox key is the right shape. Then contacts, their
    /// rosters, groups, and the history head land through ordinary store calls
    /// — the same paths a friend import and a group invite already use, because
    /// a bootstrap is a statement of what this person knows, not a database
    /// image.
    ///
    /// Those writes are individually transactional rather than collectively so,
    /// and that is survivable rather than sloppy: every one of them is an upsert
    /// or a duplicate-tolerant insert, the stage only advances after all of them
    /// land, and a device whose import died halfway is still in
    /// [`CoreLinkActivationStage::AwaitingBootstrap`] — silent, and free to be
    /// handed the same export again.
    ///
    /// It does NOT write [`OwnDeviceFleet`](crate::OwnDeviceFleet). The fleet
    /// record is what the ack planner reads to decide whose relay rows this
    /// device may delete; writing it here would make the device act on a
    /// membership it has not yet acknowledged. It is written by
    /// [`Self::complete_link_activation`], and by nothing else.
    pub fn import_link_bootstrap(
        &self,
        bootstrap: LinkBootstrap,
        own_device_sign_pk: Vec<u8>,
        expected_person_id: Option<Vec<u8>>,
        now_ms: i64,
    ) -> Result<CoreLinkBootstrapImport, CoreError> {
        let activation = self.link_activation()?;
        if activation.stage != CoreLinkActivationStage::AwaitingBootstrap {
            return Err(CoreError::Store(format!(
                "a bootstrap can only be imported while awaiting one, not in {:?}",
                activation.stage
            )));
        }
        // §9.3's binding, checked against the ceremony THIS device recorded when
        // its own window opened. Everything below is about whether the export is
        // well formed and whose it is; this is about whether it is ours.
        let recorded_binding = activation.channel_binding.clone().ok_or_else(|| {
            CoreError::Store(
                "no link ceremony was recorded for this window, so no bootstrap belongs to it"
                    .to_string(),
            )
        })?;
        super::bootstrap::core_link_bootstrap_verify(bootstrap.clone(), recorded_binding, now_ms)?;
        let person = bootstrap.person.clone();
        let profile = bootstrap.profile.clone();
        // Whose fleet is this, and is this store a phone that may join it?
        if let Some(expected) = &expected_person_id {
            if *expected != person.person_id {
                return Err(CoreError::Malformed(
                    "this bootstrap is for a different person than the one being restored"
                        .to_string(),
                ));
            }
        }
        match self.link_import_readiness(expected_person_id)? {
            CoreLinkImportReadiness::Ready => {}
            CoreLinkImportReadiness::StoreHoldsSomeone => {
                return Err(CoreError::Store(
                    "this phone already holds someone's contacts and messages, so it cannot be \
                     adopted as a new device"
                        .to_string(),
                ));
            }
            CoreLinkImportReadiness::StoreHoldsAnotherPerson => {
                return Err(CoreError::Store(
                    "this phone already belongs to a different person".to_string(),
                ));
            }
        }
        if let Some(existing) = self.own_roster()? {
            if existing.person_id != person.person_id {
                return Err(CoreError::Store(
                    "this phone already belongs to a different person".to_string(),
                ));
            }
        }
        check_len(&person.person_sign_pk, KEY_LEN, "person root key")?;
        check_len(&person.inbox_agree_pk, KEY_LEN, "inbox key")?;
        check_len(&person.inbox_agree_sk, KEY_LEN, "inbox key")?;
        if derive_user_id(&person.person_sign_pk).to_vec() != person.person_id {
            return Err(CoreError::Malformed(
                "bootstrap identity does not match its own person id".to_string(),
            ));
        }
        if bootstrap.roster.person_id != person.person_id {
            return Err(CoreError::Malformed(
                "bootstrap roster is for a different person".to_string(),
            ));
        }
        if let Some(rejection) =
            core_roster_validate(bootstrap.roster.clone(), person.person_sign_pk.clone())
        {
            return Err(CoreError::Malformed(format!(
                "bootstrap roster is not acceptable: {rejection:?}"
            )));
        }
        let own_cert = bootstrap
            .roster
            .devices
            .iter()
            .find(|cert| cert.device_sign_pk == own_device_sign_pk)
            .ok_or_else(|| {
                CoreError::Malformed(
                    "bootstrap roster does not carry a certificate for this device".to_string(),
                )
            })?;
        let own_device_id = own_cert.device_id();
        let roster_head = roster_head_hash(&bootstrap.roster);

        // Contacts first: a contact roster is stored against a contact row
        // (`apply_contact_roster` looks the person root up there), so the order
        // is load-bearing rather than tidy.
        let mut contacts_imported = 0_u32;
        let mut contact_rosters_imported = 0_u32;
        for entry in &bootstrap.contacts {
            self.upsert_contact(entry.contact.clone())?;
            if entry.contact.nickname.is_some() {
                // `upsert_contact` deliberately leaves a local-only nickname
                // alone — importing a friend card must never overwrite one.
                // This is not a card, it is the person's own name for their own
                // contact arriving on their own second phone, so it is applied.
                self.set_contact_nickname(
                    entry.contact.user_id.clone(),
                    entry.contact.nickname.clone(),
                )?;
            }
            contacts_imported += 1;
        }
        for entry in &bootstrap.contacts {
            if let Some(roster) = &entry.roster {
                let decision = self.apply_contact_roster(roster.clone())?;
                if decision.outcome == crate::RosterUpdateOutcome::Accepted {
                    contact_rosters_imported += 1;
                }
            }
        }
        let mut groups_imported = 0_u32;
        for group in &bootstrap.groups {
            self.upsert_group(group.clone())?;
            groups_imported += 1;
        }
        let mut messages_imported = 0_u32;
        for message in &bootstrap.history_head {
            // §5: the row keeps the device stream it was authored in. The head
            // is history this person already holds, so it is inserted as the
            // received rows they are, never re-authored.
            let sender_device_id = message.sender_device_id.clone();
            if self.insert_message_from_device(message.clone(), Some(sender_device_id))? {
                messages_imported += 1;
            }
        }

        self.store_own_roster(&bootstrap.roster)?;
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "UPDATE device_link_activation
                SET stage = ?1, expected_roster_head = ?2, own_device_id = ?3,
                    bootstrap_imported_at_ms = ?4
              WHERE id = 0",
            params![
                stage_text(CoreLinkActivationStage::AwaitingRosterAck),
                roster_head,
                own_device_id,
                now_ms
            ],
        )
        .map_err(store_err)?;
        drop(conn);

        Ok(CoreLinkBootstrapImport {
            catch_up: super::bootstrap::core_link_catch_up_plan(bootstrap),
            person,
            profile,
            own_device_id,
            roster_head,
            contacts_imported,
            contact_rosters_imported,
            groups_imported,
            messages_imported,
        })
    }

    /// **§9.4(b), and the moment the device becomes visible.**
    ///
    /// `acked_roster_head` is the head this device actually put on the wire
    /// back to the approving device (the frame [`core_link_activation_ack`]
    /// built). It must equal the head of the roster that was imported —
    /// exactly, all 32 bytes — or activation does not close and the device
    /// stays silent. Call it once the acknowledgement has been sealed onto the
    /// confirmed channel: the ack is the last thing §9.4 asks for.
    ///
    /// The [`OwnDeviceFleet`](crate::OwnDeviceFleet) is written here, and only
    /// here. That is the whole of "may not ack ANYTHING until": before this
    /// call the ack planner sees an unlinked install with nothing to ack for,
    /// and the gate refuses the paths anyway.
    ///
    /// # §9 step 5 belongs immediately after this call
    ///
    /// This is the line that first makes a fleet larger than one device real,
    /// and until the person's CONTACTS know it, ACK-MD-2 makes that fleet
    /// expensive: a contact who has not heard about the roster keeps uploading
    /// exactly ONE person-addressed relay row, and no member of a multi-device
    /// fleet may delete it — whichever sibling fetches it first must leave it
    /// for the others. Those rows churned until their 7-day expiry for as long
    /// as DL-3's send side did not exist.
    ///
    /// It exists now. A driver that has just activated a device calls
    /// [`MessageStore::announce_own_roster`], which seals the roster to every
    /// contact not already holding this head and queues one envelope each; the
    /// contact's next fan-out to this person is then per-device, each row with
    /// exactly one consumer, and the churn stops. The same call is the right
    /// one after a revocation and after adding a contact — it is idempotent, so
    /// calling it when nothing is owed authors nothing, and a moment a platform
    /// forgets is repaired by the next call rather than lost.
    ///
    /// Both halves are pinned by `MD-ROSTER-GOSSIP-TO-CONTACTS` in
    /// `core/tests/multi_device_contract.rs`, which asserts the churn before the
    /// document lands and its absence afterwards.
    pub fn complete_link_activation(
        &self,
        acked_roster_head: Vec<u8>,
        now_ms: i64,
    ) -> Result<CoreLinkActivation, CoreError> {
        let activation = self.link_activation()?;
        if activation.stage != CoreLinkActivationStage::AwaitingRosterAck {
            return Err(CoreError::Store(format!(
                "activation closes only after a bootstrap import, not in {:?}",
                activation.stage
            )));
        }
        let expected = activation.expected_roster_head.clone().ok_or_else(|| {
            CoreError::Store("no roster head is awaiting acknowledgement".to_string())
        })?;
        if acked_roster_head != expected {
            return Err(CoreError::Malformed(
                "the acknowledged roster is not the roster that was imported".to_string(),
            ));
        }
        let own_device_id = activation
            .own_device_id
            .clone()
            .ok_or_else(|| CoreError::Store("no device id was imported".to_string()))?;
        let roster = self
            .own_roster()?
            .ok_or_else(|| CoreError::Store("no own roster was imported".to_string()))?;
        if roster_head_hash(&roster) != expected {
            return Err(CoreError::Store(
                "the stored own roster is not the one that was acknowledged".to_string(),
            ));
        }
        self.project_own_fleet(&roster, &own_device_id)?;

        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "UPDATE device_link_activation SET stage = ?1, activated_at_ms = ?2 WHERE id = 0",
            params![stage_text(CoreLinkActivationStage::Activated), now_ms],
        )
        .map_err(store_err)?;
        drop(conn);
        self.link_activation()
    }

    /// This person's own roster document (§4), or `None` on an install that has
    /// never linked.
    pub fn own_roster(&self) -> Result<Option<Roster>, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        let document: Option<Vec<u8>> = conn
            .query_row("SELECT document FROM own_roster WHERE id = 0", [], |row| {
                row.get(0)
            })
            .optional()
            .map_err(store_err)?;
        drop(conn);
        match document {
            Some(document) => Ok(Some(decode_roster_document(&document)?)),
            None => Ok(None),
        }
    }

    /// Adopt a roster of this person's own devices: store the document and
    /// project the fleet routing and acks read from it.
    ///
    /// The approving device calls this when its own roster changes under it —
    /// at genesis, and again the moment a new device's acknowledgement lands
    /// (§9.4). The new device does not: its adoption goes through
    /// [`Self::import_link_bootstrap`] and [`Self::complete_link_activation`],
    /// which are the same two writes with §9.4's ordering enforced around them.
    pub fn adopt_own_roster(
        &self,
        roster: Roster,
        person_root_sign_pk: Vec<u8>,
        own_device_id: Vec<u8>,
    ) -> Result<(), CoreError> {
        if let Some(rejection) = core_roster_validate(roster.clone(), person_root_sign_pk) {
            return Err(CoreError::Malformed(format!(
                "own roster is not acceptable: {rejection:?}"
            )));
        }
        if !roster
            .devices
            .iter()
            .any(|cert| cert.device_id() == own_device_id)
        {
            return Err(CoreError::Store(
                "own roster does not list this device".to_string(),
            ));
        }
        self.store_own_roster(&roster)?;
        self.project_own_fleet(&roster, &own_device_id)
    }

    /// **§10 step 5, the sending half.** This person's own roster document, in
    /// the frame [`crate::encode_own_roster`] defines, or `None` when there is
    /// nothing to say.
    ///
    /// `None` on an install that has never linked (no roster), and on one the
    /// gate has silenced — a device that may not advertise may not announce a
    /// roster either, and that includes a device this very mechanism has
    /// ejected. The news travels from the fleet toward the removed device, not
    /// out of it.
    ///
    /// Read [`crate::encode_own_roster`] before calling this: **which links a
    /// notice may be written to is the whole of its safety**, and that rule
    /// lives in the shell that owns the socket, because core cannot see a Noise
    /// static key.
    pub fn own_roster_notice_frame(&self) -> Result<Option<Vec<u8>>, CoreError> {
        if !self.link_gate_allows(CoreLinkGatedAction::Advertise)? {
            return Ok(None);
        }
        let Some(roster) = self.own_roster()? else {
            return Ok(None);
        };
        Ok(Some(crate::encode_own_roster(crate::core_encode_roster(
            roster,
        )?)?))
    }

    /// **§10 step 5, the receiving half: how a removed device finds out.**
    ///
    /// `document` is a [`crate::Frame::OwnRoster`] body that arrived on a link
    /// the caller has already proved belongs to this person (see
    /// [`crate::encode_own_roster`] — that test is a precondition, not an
    /// option). `person_root_sign_pk` is this device's OWN identity signing key:
    /// §3 makes the person root the deployed identity key, so the anchor is
    /// already on the phone and no new trust material is introduced.
    ///
    /// # Why a push, and why this is not a tip-off
    ///
    /// §10.1 addresses its gossip to contacts and to *remaining* own devices,
    /// and §11's SYNC-3 seals self-sync to the person's *current* device set.
    /// Every carrier v1 has is therefore addressed to a set the removed device
    /// is not in — [`crate::core_seal_sync_handoff`] refuses a tombstoned
    /// address outright and says so — which is why an honest "I found my old
    /// phone" device otherwise believes itself linked forever, keeps
    /// advertising, and keeps accepting mail.
    ///
    /// A device learns this only from a document signed under the person root
    /// and strictly superseding the one it held, so "you're out" is never a bare
    /// hint a stranger can inject. And it cannot outrun step 1: the inbox key
    /// rotates at the moment of removal, before any meeting, so the fleet's
    /// self-sync channel and its retained backlog are already shut by the time
    /// this runs.
    ///
    /// Step 2 joined that sentence with a window rather than a guarantee, and
    /// the difference belongs where somebody reading this will see it. The
    /// relay `family_token` rotation has a driver on both shells now: the
    /// removal writes [`crate::MessageStore::begin_relay_rotation`]'s journal
    /// row as it commits, and the relay sync pass performs the call and
    /// commits it. What it cannot promise is *when*, because a removal on a
    /// ship with no internet is a real removal and may not wait for a network.
    /// So a removed device keeps a working family relay credential from the
    /// moment of removal until the removing phone next reaches the relay — and
    /// a meeting inside that window (same Wi-Fi, no internet, which is
    /// precisely the ship) tells the holder of that phone they are out while
    /// the credential is still live. It grants them nothing new; the token was
    /// in their hands the whole time. But "removed" means cut off from the
    /// fleet's own traffic *at once* and from the relay mailbox *as soon as the
    /// rotation lands*, and nothing here or on any surface may collapse those
    /// two into one.
    ///
    /// # What it does, in [`core_roster_accept`]'s vocabulary
    ///
    /// The decision is the ordinary one — DL-1 ordering, DL-2's sticky fork
    /// quarantine, DL-4's tombstones, §6's generation floor, §14.2's epoch
    /// authority — run against what this device has stored. Only
    /// [`crate::RosterUpdateOutcome::Accepted`] does anything, and then:
    ///
    /// * the document buries THIS device → it ejects itself
    ///   ([`crate::RevocationAdoptionOutcome::RevokedSelf`]);
    /// * the document still lists this device and announces the inbox key
    ///   generation this device already holds → ordinary convergence, adopted;
    /// * the document still lists this device but announces a NEWER generation
    ///   → nothing is written and
    ///   [`crate::RevocationAdoptionOutcome::AwaitingRotationKey`] is returned.
    ///   A plaintext link frame carries no key material and never will, so
    ///   adopting here would leave a device holding a roster whose sync traffic
    ///   it cannot open. §10.1's sealed handoff is what carries that rotation,
    ///   and this says so rather than half-applying it.
    ///
    /// [`crate::RevocationAdoption::inbox_key`] is therefore always `None` on
    /// this path.
    ///
    /// # What this does *not* converge
    ///
    /// The third arm is not a corner case, and the scope it leaves should be
    /// read exactly: **this converges the removed device, not the rest of the
    /// fleet.** [`crate::core_revoke_devices_roster`] always mints a new inbox
    /// key, so every revocation roster announces a rotated generation, so a
    /// *sibling* that was offline at removal time takes the third arm on every
    /// later meeting and keeps the pre-revocation roster — with the removed
    /// device still in its fleet projection — until §10.1's sealed handoff
    /// reaches it. That handoff rides self-sync, which has no shell transport
    /// yet. WP5's gate is written against the removed device for that reason.
    pub fn apply_own_roster_notice(
        &self,
        document: Vec<u8>,
        person_root_sign_pk: Vec<u8>,
        own_device_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<crate::RevocationAdoption, CoreError> {
        use crate::device_roster::{RosterUpdateOutcome, RosterUpdateReason};
        use crate::revocation::{core_roster_newly_revoked, RevocationAdoptionOutcome};

        let held = self.own_roster()?.ok_or_else(|| {
            CoreError::Store(
                "this device has no roster of its own, so it has no fleet to be removed from"
                    .to_string(),
            )
        })?;
        // The DL-3 wire codec, not this module's storage codec: a notice
        // carries exactly the document `KIND_ROSTER_GOSSIP` already puts on a
        // wire, so one roster has one shape wherever it travels.
        let incoming = crate::core_decode_roster(document)?;
        if incoming.person_id != held.person_id {
            return Err(CoreError::Malformed(
                "an own-roster notice names a different person".to_string(),
            ));
        }
        let stored_quarantined = {
            let conn = self.locked_conn();
            crate::roster_store::load_state(&conn, &held.person_id)?.quarantined
        };
        let decision = crate::core_roster_accept(
            Some(held.clone()),
            stored_quarantined,
            incoming.clone(),
            person_root_sign_pk,
        );
        let revoked_device_ids = core_roster_newly_revoked(Some(held.clone()), incoming.clone());
        let answer = |outcome, reason, revoked_device_ids| crate::RevocationAdoption {
            outcome,
            reason,
            revoked_device_ids,
            inbox_key: None,
            resealed_records: 0,
            unresealable_records: 0,
        };
        match decision.outcome {
            RosterUpdateOutcome::Accepted => {}
            RosterUpdateOutcome::ForkQuarantined => {
                {
                    let conn = self.locked_conn();
                    crate::roster_store::mark_quarantined(&conn, &held.person_id)?;
                }
                // Deliberately fails toward NOT ejecting: DL-2 says a person
                // resolves a fork, and bricking a device on the strength of one
                // branch of it would let a fork be weaponised into a remote
                // stop. The shell surfaces the quarantine instead of swallowing
                // it — a device left live in this corner is the reported
                // symptom, and it is the safer half of the trade.
                return Ok(answer(
                    RevocationAdoptionOutcome::ForkQuarantined,
                    decision.reason,
                    Vec::new(),
                ));
            }
            RosterUpdateOutcome::Ignored => {
                return Ok(answer(
                    match decision.reason {
                        RosterUpdateReason::Rollback | RosterUpdateReason::IdempotentRepeat => {
                            RevocationAdoptionOutcome::NotSuperseding
                        }
                        _ => RevocationAdoptionOutcome::Refused,
                    },
                    decision.reason,
                    Vec::new(),
                ));
            }
        }
        if incoming
            .tombstones
            .iter()
            .any(|tombstone| tombstone.device_id == own_device_id)
        {
            self.eject_self_from_fleet(&incoming, &own_device_id, now_ms)?;
            return Ok(answer(
                RevocationAdoptionOutcome::RevokedSelf,
                decision.reason,
                revoked_device_ids,
            ));
        }
        if !incoming
            .devices
            .iter()
            .any(|cert| cert.device_id() == own_device_id)
        {
            // Neither listed nor buried. Not this device's roster to adopt, and
            // not evidence it was removed either — `adopt_own_roster` refuses
            // exactly this document, and so does this.
            return Ok(answer(
                RevocationAdoptionOutcome::Refused,
                RosterUpdateReason::Invalid,
                Vec::new(),
            ));
        }
        if incoming.inbox_key_generation != held.inbox_key_generation {
            return Ok(answer(
                RevocationAdoptionOutcome::AwaitingRotationKey,
                decision.reason,
                revoked_device_ids,
            ));
        }
        self.store_own_roster(&incoming)?;
        self.project_own_fleet(&incoming, &own_device_id)?;
        Ok(answer(
            RevocationAdoptionOutcome::Adopted,
            decision.reason,
            revoked_device_ids,
        ))
    }
}

impl MessageStore {
    /// The gate as core's own paths use it: refuse, rather than report.
    pub(crate) fn guard_link_gate(&self, action: CoreLinkGatedAction) -> Result<(), CoreError> {
        let verdict = self.link_gate(action)?;
        if verdict.allowed {
            return Ok(());
        }
        Err(refusal(verdict))
    }

    /// The gate for paths whose honest refusal is emptiness rather than an
    /// error: a hint set nobody is in, an ack list naming nothing.
    pub(crate) fn link_gate_allows(&self, action: CoreLinkGatedAction) -> Result<bool, CoreError> {
        Ok(self.link_gate(action)?.allowed)
    }

    /// Nothing of anyone's in here yet: no contacts, no groups, no messages.
    ///
    /// Deliberately three cheap existence checks rather than one clever query —
    /// each is a different way a store stops being a blank phone, and a
    /// `LIMIT 1` on each costs nothing next to being wrong about it.
    fn store_is_factory_fresh(&self) -> Result<bool, CoreError> {
        let conn = self.conn.lock().expect("store mutex poisoned");
        for table in ["contacts", "groups", "messages"] {
            let occupied: Option<i64> = conn
                .query_row(&format!("SELECT 1 FROM {table} LIMIT 1"), [], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(store_err)?;
            if occupied.is_some() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn store_own_roster(&self, roster: &Roster) -> Result<(), CoreError> {
        let document = encode_roster_document(roster);
        let conn = self.conn.lock().expect("store mutex poisoned");
        conn.execute(
            "INSERT INTO own_roster (id, document) VALUES (0, ?1)
             ON CONFLICT(id) DO UPDATE SET document = excluded.document",
            params![document],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// **The self-eject.** One device, told by a signed roster of its own person
    /// that it has been buried, taking itself off the mesh (§10 step 5).
    ///
    /// Three writes, and they are one transaction because the intermediate
    /// states are each worse than either end. A device with the news written
    /// down but the gate still open keeps advertising under a roster that
    /// disowns it; a device with the gate shut but its fleet projection intact
    /// still names siblings whose relay rows it must withhold acks for, out of a
    /// membership that no longer exists.
    ///
    /// 1. **Store the document.** Not through [`Self::adopt_own_roster`], which
    ///    correctly refuses a roster that does not list this device. Written
    ///    directly, so the news is durable and DL-1-monotone: the same notice
    ///    arriving again reads as an idempotent repeat instead of re-running
    ///    this, and the phone stops reporting a fleet it is not in.
    /// 2. **Clear the projection.** No `own_device_id`, no siblings — which is
    ///    [`OwnDeviceFleet::default`]'s shape and passes its validator, since a
    ///    fleet that names nobody names no self either. Its version is raised to
    ///    the burying roster's rather than reset, because
    ///    [`MessageStore::set_own_device_fleet`](crate::MessageStore::set_own_device_fleet)'s
    ///    DL-1 monotonicity is exactly what stops an old `.cmbak` from
    ///    resurrecting a fleet a revocation has narrowed — and this is the
    ///    narrowest one there is. The version is never lowered: if a later
    ///    projection is somehow already stored, the rows still go, and only the
    ///    counter stands still.
    /// 3. **Shut the gate**, by writing [`CoreLinkActivationStage::Revoked`].
    ///
    /// The store's own `set_own_device_fleet` is deliberately not called: it
    /// takes the connection mutex itself, and this has to hold it across all
    /// three writes for the transaction to mean anything.
    pub(crate) fn eject_self_from_fleet(
        &self,
        roster: &Roster,
        own_device_id: &[u8],
        now_ms: i64,
    ) -> Result<(), CoreError> {
        let document = encode_roster_document(roster);
        let version = roster.version();
        let mut conn = self.locked_conn();
        let tx = conn.transaction().map_err(store_err)?;
        tx.execute(
            "INSERT INTO own_roster (id, document) VALUES (0, ?1)
             ON CONFLICT(id) DO UPDATE SET document = excluded.document",
            params![document],
        )
        .map_err(store_err)?;
        let stored: Option<(i64, i64)> = tx
            .query_row(
                "SELECT recovery_epoch, seq FROM own_device_fleet_version WHERE id = 0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(store_err)?;
        let supersedes = match stored {
            Some((recovery_epoch, seq)) => {
                version
                    > crate::RosterVersion {
                        recovery_epoch: recovery_epoch as u64,
                        seq: seq as u64,
                    }
            }
            None => true,
        };
        if supersedes {
            tx.execute(
                "INSERT INTO own_device_fleet_version (id, recovery_epoch, seq) VALUES (0, ?1, ?2)
                 ON CONFLICT(id) DO UPDATE SET recovery_epoch = excluded.recovery_epoch,
                                               seq = excluded.seq",
                params![version.recovery_epoch as i64, version.seq as i64],
            )
            .map_err(store_err)?;
        }
        tx.execute("DELETE FROM own_device_fleet", [])
            .map_err(store_err)?;
        // `own_device_id` is kept: a surface has to be able to say which device
        // this was, and DL-4 means the id is never reissued to anything else.
        tx.execute(
            "INSERT INTO device_link_activation
                (id, stage, expected_roster_head, own_device_id, channel_binding,
                 started_at_ms, bootstrap_imported_at_ms, activated_at_ms)
             VALUES (0, ?1, NULL, ?2, NULL, ?3, 0, 0)
             ON CONFLICT(id) DO UPDATE SET
                 stage = excluded.stage,
                 expected_roster_head = NULL,
                 own_device_id = excluded.own_device_id,
                 channel_binding = NULL",
            params![
                stage_text(CoreLinkActivationStage::Revoked),
                own_device_id,
                now_ms
            ],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(())
    }

    fn project_own_fleet(&self, roster: &Roster, own_device_id: &[u8]) -> Result<(), CoreError> {
        self.set_own_device_fleet(OwnDeviceFleet {
            own_device_id: Some(own_device_id.to_vec()),
            device_ids: roster.devices.iter().map(DeviceCert::device_id).collect(),
            projected_from: roster.version(),
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn stage_text(stage: CoreLinkActivationStage) -> &'static str {
    match stage {
        CoreLinkActivationStage::NotLinking => "not_linking",
        CoreLinkActivationStage::AwaitingBootstrap => "awaiting_bootstrap",
        CoreLinkActivationStage::AwaitingRosterAck => "awaiting_roster_ack",
        CoreLinkActivationStage::Activated => "activated",
        CoreLinkActivationStage::Revoked => "revoked",
    }
}

/// Fails closed: a stage this build cannot read is not an activated one. A
/// downgrade that does not know whether it finished being adopted has not
/// finished being adopted.
fn stage_from_text(text: &str) -> CoreLinkActivationStage {
    match text {
        "not_linking" => CoreLinkActivationStage::NotLinking,
        "awaiting_roster_ack" => CoreLinkActivationStage::AwaitingRosterAck,
        "activated" => CoreLinkActivationStage::Activated,
        "revoked" => CoreLinkActivationStage::Revoked,
        // Unreadable fails closed onto a silent stage, which is why a downgrade
        // that cannot spell "revoked" still stops advertising rather than
        // waking up as an activated device.
        _ => CoreLinkActivationStage::AwaitingBootstrap,
    }
}

/// `tag || 32-byte field || 32-byte channel binding`, the bytes a link frame's
/// signature covers. The tag is inside the signature, so an offer can never be
/// replayed as an acknowledgement.
fn signed_message(tag: u8, field: &[u8], channel_binding: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + field.len() + channel_binding.len());
    out.push(tag);
    out.extend_from_slice(field);
    out.extend_from_slice(channel_binding);
    out
}

/// The four fixed-width parts both link frames are made of. `field` is the
/// agreement key in an offer and the roster head in an acknowledgement — the
/// one place the two layouts differ, which is why one splitter serves both.
struct LinkFrameParts {
    device_sign_pk: Vec<u8>,
    field: Vec<u8>,
    channel_binding: Vec<u8>,
    signature: Vec<u8>,
}

/// `tag(1) || device_sign_pk(32) || field(32) || channel_binding(32) || signature(64)`.
fn split_frame(frame: &[u8], tag: u8) -> Result<LinkFrameParts, CoreError> {
    const FIELD_LEN: usize = 32;
    let expected = 1 + KEY_LEN + FIELD_LEN + CHANNEL_BINDING_LEN + SIGNATURE_LEN;
    if frame.len() != expected || frame[0] != tag {
        return Err(CoreError::Malformed(
            "device link frame is not the frame it should be".to_string(),
        ));
    }
    let key_end = 1 + KEY_LEN;
    let field_end = key_end + FIELD_LEN;
    let binding_end = field_end + CHANNEL_BINDING_LEN;
    Ok(LinkFrameParts {
        device_sign_pk: frame[1..key_end].to_vec(),
        field: frame[key_end..field_end].to_vec(),
        channel_binding: frame[field_end..binding_end].to_vec(),
        signature: frame[binding_end..].to_vec(),
    })
}

fn public_of(sign_sk: &[u8]) -> Result<Vec<u8>, CoreError> {
    Ok(crate::crypto::signing_key_from_bytes(sign_sk)?
        .verifying_key()
        .as_bytes()
        .to_vec())
}

fn check_len(bytes: &[u8], len: usize, what: &str) -> Result<(), CoreError> {
    if bytes.len() != len {
        return Err(CoreError::Malformed(format!(
            "device link {what} is {} bytes, not {len}",
            bytes.len()
        )));
    }
    Ok(())
}

fn sign_cert(
    person_id: &[u8],
    device_sign_pk: &[u8],
    device_agree_pk: &[u8],
    added_epoch: u64,
    flags: u32,
    signer_sign_sk: &[u8],
) -> Result<DeviceCert, CoreError> {
    check_len(device_sign_pk, KEY_LEN, "device signing key")?;
    check_len(device_agree_pk, KEY_LEN, "device agreement key")?;
    check_len(person_id, DEVICE_ID_LEN, "person id")?;
    core_sign_device_cert(
        DeviceCert {
            person_id: person_id.to_vec(),
            device_sign_pk: device_sign_pk.to_vec(),
            device_agree_pk: device_agree_pk.to_vec(),
            added_epoch,
            flags,
            signer_sign_pk: Vec::new(),
            signature: Vec::new(),
        },
        signer_sign_sk.to_vec(),
    )
}

/// Re-validate a document this module just built. A roster minted here that a
/// contact would reject is a bug worth failing on now rather than on a stranger's
/// phone a week later.
fn validated(roster: Roster, person_root_sign_pk: &[u8]) -> Result<Roster, CoreError> {
    match core_roster_validate(roster.clone(), person_root_sign_pk.to_vec()) {
        None => Ok(roster),
        Some(rejection) => Err(CoreError::Malformed(format!(
            "the roster this device just signed is not acceptable: {rejection:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device_roster::generate_device_keypair;
    use crate::identity::generate_identity;

    const NOW: i64 = 1_755_000_000_000;
    const BINDING: [u8; 32] = [0x7C; 32];

    #[test]
    fn genesis_names_the_holder_as_the_approving_device() {
        let identity = generate_identity();
        let device = generate_device_keypair();
        let roster = core_link_genesis_roster(
            identity.sign_sk.clone(),
            device.sign_pk.clone(),
            device.agree_pk.clone(),
        )
        .unwrap();

        assert_eq!(roster.person_id, identity.user_id);
        assert_eq!(roster.seq, 0);
        assert_eq!(roster.approving_device_id, device.device_id);
        assert_eq!(roster.signer_sign_pk, identity.sign_pk);
        assert!(core_roster_validate(roster, identity.sign_pk).is_none());
    }

    #[test]
    fn a_new_device_roster_is_seq_plus_one_signed_by_the_approving_device() {
        let identity = generate_identity();
        let first = generate_device_keypair();
        let second = generate_device_keypair();
        let genesis = core_link_genesis_roster(
            identity.sign_sk.clone(),
            first.sign_pk.clone(),
            first.agree_pk.clone(),
        )
        .unwrap();

        let update = core_link_sign_new_device_roster(
            genesis.clone(),
            identity.sign_pk.clone(),
            first.sign_sk.clone(),
            second.sign_pk.clone(),
            second.agree_pk.clone(),
        )
        .unwrap();

        assert_eq!(update.roster.seq, genesis.seq + 1);
        assert_eq!(update.roster.devices.len(), 2);
        assert_eq!(update.new_device_id, second.device_id);
        assert_eq!(update.roster.signer_sign_pk, first.sign_pk);
        assert_eq!(update.add_outcome, DeviceAddOutcome::Added);
        assert_eq!(
            update.roster_head,
            crate::core_roster_head_hash(update.roster.clone())
        );
        // The approving role does not move: exactly one device signs rosters.
        assert_eq!(update.roster.approving_device_id, first.device_id);
        assert!(core_roster_validate(update.roster, identity.sign_pk).is_none());
    }

    #[test]
    fn only_the_approving_device_may_extend_the_roster() {
        let identity = generate_identity();
        let first = generate_device_keypair();
        let second = generate_device_keypair();
        let genesis = core_link_genesis_roster(
            identity.sign_sk.clone(),
            first.sign_pk.clone(),
            first.agree_pk.clone(),
        )
        .unwrap();

        // The person root is not the approving device (§3's split), and neither
        // is a stranger.
        assert!(core_link_sign_new_device_roster(
            genesis.clone(),
            identity.sign_pk.clone(),
            identity.sign_sk.clone(),
            second.sign_pk.clone(),
            second.agree_pk.clone(),
        )
        .is_err());
        assert!(core_link_sign_new_device_roster(
            genesis.clone(),
            identity.sign_pk.clone(),
            generate_device_keypair().sign_sk,
            second.sign_pk.clone(),
            second.agree_pk.clone(),
        )
        .is_err());
        // And the same device cannot be added twice.
        let update = core_link_sign_new_device_roster(
            genesis,
            identity.sign_pk.clone(),
            first.sign_sk.clone(),
            second.sign_pk.clone(),
            second.agree_pk.clone(),
        )
        .unwrap();
        assert!(core_link_sign_new_device_roster(
            update.roster,
            identity.sign_pk,
            first.sign_sk,
            second.sign_pk,
            second.agree_pk,
        )
        .is_err());
    }

    /// §14.3, on the own-add path where the warning belongs.
    #[test]
    fn the_soft_cap_warns_and_the_hard_cap_refuses() {
        let identity = generate_identity();
        let first = generate_device_keypair();
        let mut roster = core_link_genesis_roster(
            identity.sign_sk.clone(),
            first.sign_pk.clone(),
            first.agree_pk.clone(),
        )
        .unwrap();

        let mut outcomes = Vec::new();
        for _ in 0..(crate::DEVICE_HARD_CAP - 1) {
            let device = generate_device_keypair();
            let update = core_link_sign_new_device_roster(
                roster,
                identity.sign_pk.clone(),
                first.sign_sk.clone(),
                device.sign_pk,
                device.agree_pk,
            )
            .unwrap();
            outcomes.push(update.add_outcome);
            roster = update.roster;
        }
        // The 8th device is silent, the 9th warns (§14.3's boundary).
        assert_eq!(
            outcomes[crate::DEVICE_SOFT_CAP as usize - 2],
            DeviceAddOutcome::Added
        );
        assert_eq!(
            outcomes[crate::DEVICE_SOFT_CAP as usize - 1],
            DeviceAddOutcome::AddedWithWarning
        );
        assert_eq!(roster.devices.len(), crate::DEVICE_HARD_CAP as usize);

        let seventeenth = generate_device_keypair();
        assert!(
            core_link_sign_new_device_roster(
                roster,
                identity.sign_pk,
                first.sign_sk,
                seventeenth.sign_pk,
                seventeenth.agree_pk,
            )
            .is_err(),
            "the 17th device is refused"
        );
    }

    /// §14.2: the backup's root secret climbs the recovery epoch, and the epoch
    /// it produces is one a contact accepts over anything the old approving
    /// device signed.
    #[test]
    fn recovery_starts_a_higher_epoch_from_the_backup() {
        let identity = generate_identity();
        let lost = generate_device_keypair();
        let replacement = generate_device_keypair();
        let stored = core_link_genesis_roster(
            identity.sign_sk.clone(),
            lost.sign_pk.clone(),
            lost.agree_pk.clone(),
        )
        .unwrap();

        let recovered = core_link_recovery_roster(
            Some(stored.clone()),
            identity.sign_sk.clone(),
            replacement.sign_pk.clone(),
            replacement.agree_pk.clone(),
        )
        .unwrap();

        assert_eq!(recovered.recovery_epoch, stored.recovery_epoch + 1);
        assert_eq!(recovered.seq, 0);
        assert_eq!(recovered.approving_device_id, replacement.device_id);
        assert_eq!(recovered.signer_sign_pk, identity.sign_pk);
        assert!(core_roster_validate(recovered.clone(), identity.sign_pk.clone()).is_none());

        // A contact holding the old roster takes the recovery one, because only
        // the root may climb the epoch.
        let decision = crate::core_roster_accept(Some(stored), false, recovered, identity.sign_pk);
        assert_eq!(decision.outcome, crate::RosterUpdateOutcome::Accepted);
    }

    #[test]
    fn the_offer_and_the_ack_are_bound_to_their_channel_and_their_roster() {
        let identity = generate_identity();
        let first = generate_device_keypair();
        let second = generate_device_keypair();
        let genesis = core_link_genesis_roster(
            identity.sign_sk.clone(),
            first.sign_pk.clone(),
            first.agree_pk.clone(),
        )
        .unwrap();

        let offer_frame = core_link_device_offer(
            second.sign_sk.clone(),
            second.agree_pk.clone(),
            BINDING.to_vec(),
        )
        .unwrap();
        let offer = core_link_open_device_offer(offer_frame.clone(), BINDING.to_vec()).unwrap();
        assert_eq!(offer.device_id, second.device_id);
        assert_eq!(offer.device_agree_pk, second.agree_pk);
        // Another ceremony's channel does not open this offer.
        assert!(core_link_open_device_offer(offer_frame.clone(), vec![0x01; 32]).is_err());
        // Nor does a flipped byte anywhere in it.
        let mut tampered = offer_frame.clone();
        tampered[40] ^= 0x01;
        assert!(core_link_open_device_offer(tampered, BINDING.to_vec()).is_err());
        // An offer is not an acknowledgement, even with the right signature.
        let update = core_link_sign_new_device_roster(
            genesis,
            identity.sign_pk.clone(),
            first.sign_sk.clone(),
            offer.device_sign_pk.clone(),
            offer.device_agree_pk.clone(),
        )
        .unwrap();
        assert!(core_link_open_activation_ack(
            offer_frame,
            update.roster.clone(),
            second.sign_pk.clone(),
            BINDING.to_vec()
        )
        .is_err());

        let ack_frame = core_link_activation_ack(
            second.sign_sk.clone(),
            update.roster_head.clone(),
            BINDING.to_vec(),
        )
        .unwrap();
        let ack = core_link_open_activation_ack(
            ack_frame,
            update.roster.clone(),
            second.sign_pk.clone(),
            BINDING.to_vec(),
        )
        .unwrap();
        assert_eq!(ack.device_id, second.device_id);
        assert_eq!(ack.roster_head, update.roster_head);

        // §9.4's "exact": the head of ANY other roster is refused, including
        // the one this device came from.
        let stale = core_link_activation_ack(
            second.sign_sk.clone(),
            crate::core_roster_head_hash(
                core_link_genesis_roster(
                    identity.sign_sk.clone(),
                    first.sign_pk.clone(),
                    first.agree_pk.clone(),
                )
                .unwrap(),
            ),
            BINDING.to_vec(),
        )
        .unwrap();
        assert!(core_link_open_activation_ack(
            stale,
            update.roster.clone(),
            second.sign_pk.clone(),
            BINDING.to_vec()
        )
        .is_err());

        // And a device the roster does not list cannot acknowledge for it.
        let stranger = generate_device_keypair();
        let forged = core_link_activation_ack(
            stranger.sign_sk.clone(),
            update.roster_head.clone(),
            BINDING.to_vec(),
        )
        .unwrap();
        assert!(core_link_open_activation_ack(
            forged,
            update.roster.clone(),
            stranger.sign_pk.clone(),
            BINDING.to_vec()
        )
        .is_err());

        // Nor may a device the roster DOES list close a ceremony that was
        // opened for a different one: the approving device would record the
        // link as done while the phone in the person's hand stayed silent.
        let sibling_ack = core_link_activation_ack(
            first.sign_sk.clone(),
            update.roster_head.clone(),
            BINDING.to_vec(),
        )
        .unwrap();
        assert!(core_link_open_activation_ack(
            sibling_ack,
            update.roster,
            second.sign_pk,
            BINDING.to_vec()
        )
        .is_err());
    }

    /// A signature minted for one link frame is not valid in any other domain
    /// (§3's domain separation, applied to the two frames only an uncertified
    /// device signs).
    #[test]
    fn link_signatures_do_not_cross_domains() {
        let device = generate_device_keypair();
        let message = signed_message(FRAME_ACTIVATION_ACK, &[0x0A; 32], &BINDING);
        let signature = core_device_sign(
            DeviceSigningDomain::DeviceLinkActivation,
            device.sign_sk.clone(),
            message.clone(),
        )
        .unwrap();
        for domain in [
            DeviceSigningDomain::DeviceCert,
            DeviceSigningDomain::RosterUpdate,
            DeviceSigningDomain::MessageAuthoring,
            DeviceSigningDomain::SyncRecord,
        ] {
            assert!(
                core_device_verify(
                    domain,
                    device.sign_pk.clone(),
                    message.clone(),
                    signature.clone()
                )
                .is_err(),
                "a link signature verified as {domain:?}"
            );
        }
        assert!(core_device_verify(
            DeviceSigningDomain::DeviceLinkActivation,
            device.sign_pk,
            message,
            signature
        )
        .is_ok());
    }

    #[test]
    fn the_gate_permits_an_install_that_never_linked_and_refuses_a_pending_one() {
        for action in [
            CoreLinkGatedAction::Advertise,
            CoreLinkGatedAction::Author,
            CoreLinkGatedAction::Ack,
        ] {
            assert!(
                core_link_activation_gate(CoreLinkActivation::default(), action).allowed,
                "an unlinked install must behave exactly as it does today"
            );
            for stage in [
                CoreLinkActivationStage::AwaitingBootstrap,
                CoreLinkActivationStage::AwaitingRosterAck,
            ] {
                let verdict = core_link_activation_gate(
                    CoreLinkActivation {
                        stage,
                        ..CoreLinkActivation::default()
                    },
                    action,
                );
                assert!(!verdict.allowed, "{stage:?} must not {action:?}");
            }
            assert!(
                core_link_activation_gate(
                    CoreLinkActivation {
                        stage: CoreLinkActivationStage::Activated,
                        ..CoreLinkActivation::default()
                    },
                    action
                )
                .allowed
            );
        }
    }

    /// An unreadable stage — one a later build wrote — is not an activated one.
    #[test]
    fn an_unknown_stage_fails_closed() {
        assert_eq!(
            stage_from_text("a stage from a later work package"),
            CoreLinkActivationStage::AwaitingBootstrap
        );
        for stage in [
            CoreLinkActivationStage::NotLinking,
            CoreLinkActivationStage::AwaitingBootstrap,
            CoreLinkActivationStage::AwaitingRosterAck,
            CoreLinkActivationStage::Activated,
        ] {
            assert_eq!(stage_from_text(stage_text(stage)), stage);
        }
    }

    #[test]
    fn activation_walks_exactly_one_way_through_the_store() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(
            store.link_activation().unwrap().stage,
            CoreLinkActivationStage::NotLinking
        );
        assert!(store.own_roster().unwrap().is_none());

        // Nothing may be imported before the window opens.
        assert!(store.complete_link_activation(vec![0x00; 32], NOW).is_err());

        store.begin_link_activation(BINDING.to_vec(), NOW).unwrap();
        assert_eq!(
            store.link_activation().unwrap().stage,
            CoreLinkActivationStage::AwaitingBootstrap
        );
        assert_eq!(
            store.link_activation().unwrap().channel_binding,
            Some(BINDING.to_vec()),
            "the window remembers which ceremony it belongs to"
        );
        assert!(
            !store
                .link_gate(CoreLinkGatedAction::Author)
                .unwrap()
                .allowed
        );
        // The bootstrap has not landed, so there is nothing to acknowledge.
        assert!(store.complete_link_activation(vec![0x00; 32], NOW).is_err());
    }

    /// **The window is a window, not a door.** A ceremony that opened it and
    /// then failed — a declined confirm, a dropped socket, a person who put the
    /// phone down — must give the gates back, or a failed link leaves a
    /// permanently silent phone with no way out but a reinstall.
    #[test]
    fn an_abandoned_ceremony_reopens_the_gates() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // Abandoning is a no-op on a store that never began anything, so a
        // failure path may call it without first asking where it is.
        assert_eq!(
            store.abandon_link_activation(NOW).unwrap().stage,
            CoreLinkActivationStage::NotLinking
        );

        store.begin_link_activation(BINDING.to_vec(), NOW).unwrap();
        for action in [
            CoreLinkGatedAction::Advertise,
            CoreLinkGatedAction::Author,
            CoreLinkGatedAction::Ack,
        ] {
            assert!(!store.link_gate(action).unwrap().allowed);
        }

        let abandoned = store.abandon_link_activation(NOW + 10).unwrap();
        assert_eq!(abandoned.stage, CoreLinkActivationStage::NotLinking);
        assert!(abandoned.expected_roster_head.is_none());
        assert!(abandoned.own_device_id.is_none());
        assert!(abandoned.channel_binding.is_none());
        for action in [
            CoreLinkGatedAction::Advertise,
            CoreLinkGatedAction::Author,
            CoreLinkGatedAction::Ack,
        ] {
            assert!(
                store.link_gate(action).unwrap().allowed,
                "{action:?} must be permitted again once the ceremony is abandoned"
            );
        }
        // And the phone can be offered to another ceremony afterwards.
        store
            .begin_link_activation(vec![0x5D; 32], NOW + 20)
            .unwrap();
        assert_eq!(
            store.link_activation().unwrap().stage,
            CoreLinkActivationStage::AwaitingBootstrap
        );
    }

    /// An activated device is a device. Unlinking one is §10's revocation, with
    /// a roster update and key rotations behind it — never a local flag flip.
    #[test]
    fn an_activated_device_is_never_abandonable() {
        let identity = generate_identity();
        let first = generate_device_keypair();
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let roster = core_link_genesis_roster(
            identity.sign_sk.clone(),
            first.sign_pk.clone(),
            first.agree_pk.clone(),
        )
        .unwrap();
        store
            .adopt_own_roster(roster, identity.sign_pk, first.device_id)
            .unwrap();
        // Drive the row straight to Activated: what is under test is the
        // refusal, not the path that got there.
        {
            let conn = store.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO device_link_activation (id, stage) VALUES (0, ?1)
                 ON CONFLICT(id) DO UPDATE SET stage = excluded.stage",
                params![stage_text(CoreLinkActivationStage::Activated)],
            )
            .unwrap();
        }
        assert!(store.abandon_link_activation(NOW).is_err());
        assert_eq!(
            store.link_activation().unwrap().stage,
            CoreLinkActivationStage::Activated
        );
    }

    /// §9.3 must not fold one person's world into another's. A phone that
    /// already holds someone's contacts is not a blank phone, and nobody said
    /// whose it is.
    #[test]
    fn a_store_that_already_holds_someone_is_not_ready_to_be_adopted() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(
            store.link_import_readiness(None).unwrap(),
            CoreLinkImportReadiness::Ready
        );

        store
            .upsert_contact(crate::Contact {
                user_id: vec![0xC1; 16],
                name: "Bob".to_string(),
                sign_pk: vec![0xC2; 32],
                agree_pk: vec![0xC3; 32],
                relay_url: None,
                relay_token: None,
                nickname: None,
            })
            .unwrap();
        assert_eq!(
            store.link_import_readiness(None).unwrap(),
            CoreLinkImportReadiness::StoreHoldsSomeone
        );
        // A caller that DOES know whose phone this is may proceed: it opened
        // the backup, and core has nothing better to check against.
        assert_eq!(
            store.link_import_readiness(Some(vec![0xAB; 16])).unwrap(),
            CoreLinkImportReadiness::Ready
        );

        // Once there is a roster, the roster is the answer, and it is exact.
        let identity = generate_identity();
        let device = generate_device_keypair();
        let roster = core_link_genesis_roster(
            identity.sign_sk.clone(),
            device.sign_pk.clone(),
            device.agree_pk.clone(),
        )
        .unwrap();
        store
            .adopt_own_roster(roster, identity.sign_pk, device.device_id)
            .unwrap();
        assert_eq!(
            store
                .link_import_readiness(Some(identity.user_id.clone()))
                .unwrap(),
            CoreLinkImportReadiness::Ready
        );
        assert_eq!(
            store.link_import_readiness(None).unwrap(),
            CoreLinkImportReadiness::StoreHoldsAnotherPerson
        );
        assert_eq!(
            store.link_import_readiness(Some(vec![0xAB; 16])).unwrap(),
            CoreLinkImportReadiness::StoreHoldsAnotherPerson
        );
    }

    /// §14.2: recovery tells every contact to stop sealing to what the lost
    /// device could open, and §6's way of saying that is the generation. The
    /// key MATERIAL rotation is WP5's; the announcement is not.
    #[test]
    fn recovery_moves_the_inbox_key_generation() {
        let identity = generate_identity();
        let lost = generate_device_keypair();
        let replacement = generate_device_keypair();
        let stored = core_link_genesis_roster(
            identity.sign_sk.clone(),
            lost.sign_pk.clone(),
            lost.agree_pk.clone(),
        )
        .unwrap();
        assert_eq!(stored.inbox_key_generation, 0);

        let recovered = core_link_recovery_roster(
            Some(stored),
            identity.sign_sk,
            replacement.sign_pk,
            replacement.agree_pk,
        )
        .unwrap();
        assert_eq!(recovered.inbox_key_generation, 1);
    }

    // -----------------------------------------------------------------------
    // §10 step 5: how a removed device finds out, and what it does about it
    // -----------------------------------------------------------------------

    /// One person's fleet, built through the shipped ceremonies so every
    /// signature is a real one, and the four rosters the notice tests need.
    struct Fleet {
        person: crate::Identity,
        approver: crate::DeviceKeypair,
        /// The device whose store the tests below are, and the one that gets
        /// removed.
        me: crate::DeviceKeypair,
        /// Approver + me. What this device holds before anything happens.
        held: Roster,
        /// Approver + me + a third device: strictly later, still lists me, same
        /// inbox key generation.
        grown: Roster,
        /// The third device revoked: strictly later than `grown`, still lists
        /// me, and a rotated inbox key generation this device does not hold.
        rotated: Roster,
        /// ME revoked. This is the document the field session's removed phone
        /// never saw.
        burying: Roster,
    }

    fn fleet() -> Fleet {
        let person = generate_identity();
        let approver = generate_device_keypair();
        let me = generate_device_keypair();
        let third = generate_device_keypair();
        let genesis = core_link_genesis_roster(
            person.sign_sk.clone(),
            approver.sign_pk.clone(),
            approver.agree_pk.clone(),
        )
        .unwrap();
        let held = core_link_sign_new_device_roster(
            genesis,
            person.sign_pk.clone(),
            approver.sign_sk.clone(),
            me.sign_pk.clone(),
            me.agree_pk.clone(),
        )
        .unwrap()
        .roster;
        let grown = core_link_sign_new_device_roster(
            held.clone(),
            person.sign_pk.clone(),
            approver.sign_sk.clone(),
            third.sign_pk.clone(),
            third.agree_pk.clone(),
        )
        .unwrap()
        .roster;
        let rotated = crate::core_revoke_devices_roster(
            grown.clone(),
            person.sign_pk.clone(),
            approver.sign_sk.clone(),
            vec![third.device_id.clone()],
            crate::core_mint_inbox_key(grown.inbox_key_generation),
        )
        .unwrap()
        .roster;
        let burying = crate::core_revoke_devices_roster(
            held.clone(),
            person.sign_pk.clone(),
            approver.sign_sk.clone(),
            vec![me.device_id.clone()],
            crate::core_mint_inbox_key(held.inbox_key_generation),
        )
        .unwrap()
        .roster;
        Fleet {
            person,
            approver,
            me,
            held,
            grown,
            rotated,
            burying,
        }
    }

    /// The removed device's own store, as it stands the moment before the news
    /// reaches it: holding the pre-revocation roster, projected into a fleet of
    /// two, and permitted to do everything.
    fn my_store(fleet: &Fleet) -> MessageStore {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .adopt_own_roster(
                fleet.held.clone(),
                fleet.person.sign_pk.clone(),
                fleet.me.device_id.clone(),
            )
            .unwrap();
        store
    }

    fn notice(roster: &Roster) -> Vec<u8> {
        crate::core_encode_roster(roster.clone()).unwrap()
    }

    fn apply(store: &MessageStore, fleet: &Fleet, roster: &Roster) -> crate::RevocationAdoption {
        store
            .apply_own_roster_notice(
                notice(roster),
                fleet.person.sign_pk.clone(),
                fleet.me.device_id.clone(),
                NOW,
            )
            .unwrap()
    }

    /// **The field bug, closed.** A removed phone that meets its approver reads
    /// a signed roster burying it, and stops: no advertising, no authoring, no
    /// acking, no fleet, and a stage a shell can surface.
    #[test]
    fn a_removed_device_learns_from_a_signed_roster_and_ejects_itself() {
        let fleet = fleet();
        let store = my_store(&fleet);
        assert_eq!(store.own_device_fleet().unwrap().device_ids.len(), 2);
        assert!(
            store
                .link_gate(CoreLinkGatedAction::Advertise)
                .unwrap()
                .allowed
        );

        let adoption = apply(&store, &fleet, &fleet.burying);
        assert_eq!(
            adoption.outcome,
            crate::RevocationAdoptionOutcome::RevokedSelf
        );
        assert_eq!(
            adoption.revoked_device_ids,
            vec![fleet.me.device_id.clone()]
        );
        // A plaintext link frame carries no key material and never will.
        assert_eq!(adoption.inbox_key, None);

        assert_eq!(store.own_roster().unwrap().unwrap(), fleet.burying);
        assert_eq!(
            store.own_device_fleet().unwrap(),
            OwnDeviceFleet {
                own_device_id: None,
                device_ids: Vec::new(),
                projected_from: fleet.burying.version(),
            }
        );
        let activation = store.link_activation().unwrap();
        assert_eq!(activation.stage, CoreLinkActivationStage::Revoked);
        assert_eq!(activation.own_device_id, Some(fleet.me.device_id.clone()));
        for action in [
            CoreLinkGatedAction::Advertise,
            CoreLinkGatedAction::Author,
            CoreLinkGatedAction::Ack,
        ] {
            let verdict = store.link_gate(action).unwrap();
            assert!(!verdict.allowed);
            assert_eq!(verdict.reason, CoreLinkGateReason::DeviceRevoked);
        }
        // DL-4: the way back is a fresh install under a fresh device key.
        assert!(store.begin_link_activation(BINDING.to_vec(), NOW).is_err());
        assert!(store.abandon_link_activation(NOW).is_err());
    }

    /// The notice is authenticated by the document, not by the link: a roster
    /// this person's root did not vouch for buries nobody, however it arrived.
    #[test]
    fn an_own_roster_notice_the_person_root_did_not_sign_changes_nothing() {
        let fleet = fleet();
        let stranger = generate_identity();
        let forged = crate::core_sign_roster(fleet.burying.clone(), stranger.sign_sk).unwrap();

        let store = my_store(&fleet);
        let adoption = apply(&store, &fleet, &forged);
        assert_eq!(adoption.outcome, crate::RevocationAdoptionOutcome::Refused);
        assert_eq!(adoption.reason, crate::RosterUpdateReason::Invalid);
        assert_eq!(store.own_roster().unwrap().unwrap(), fleet.held);
        assert_eq!(store.own_device_fleet().unwrap().device_ids.len(), 2);
        assert_eq!(
            store.link_activation().unwrap().stage,
            CoreLinkActivationStage::NotLinking
        );

        // The same document under a signature that IS the person's does bury.
        assert_eq!(
            apply(&store, &fleet, &fleet.burying).outcome,
            crate::RevocationAdoptionOutcome::RevokedSelf
        );
    }

    /// DL-1 through the notice path: the roster this device already holds is
    /// not news, and a burial replayed at a device that has already ejected is
    /// idempotent rather than a second ejection.
    #[test]
    fn a_stale_or_repeated_own_roster_notice_changes_nothing() {
        let fleet = fleet();
        let store = my_store(&fleet);

        let repeat = apply(&store, &fleet, &fleet.held);
        assert_eq!(
            repeat.outcome,
            crate::RevocationAdoptionOutcome::NotSuperseding
        );
        assert_eq!(repeat.reason, crate::RosterUpdateReason::IdempotentRepeat);
        assert!(
            store
                .link_gate(CoreLinkGatedAction::Author)
                .unwrap()
                .allowed
        );

        assert_eq!(
            apply(&store, &fleet, &fleet.burying).outcome,
            crate::RevocationAdoptionOutcome::RevokedSelf
        );
        // Replayed. The burial is stored, so DL-1 answers before anything is
        // written a second time, and the stage does not move.
        let replay = apply(&store, &fleet, &fleet.burying);
        assert_eq!(
            replay.outcome,
            crate::RevocationAdoptionOutcome::NotSuperseding
        );
        assert_eq!(
            store.link_activation().unwrap().stage,
            CoreLinkActivationStage::Revoked
        );
        // And an OLDER roster does not un-bury it.
        let rollback = apply(&store, &fleet, &fleet.held);
        assert_eq!(
            rollback.outcome,
            crate::RevocationAdoptionOutcome::NotSuperseding
        );
        assert_eq!(rollback.reason, crate::RosterUpdateReason::Rollback);
        assert_eq!(store.own_roster().unwrap().unwrap(), fleet.burying);
    }

    /// The same carrier fixes ordinary sibling lag for free: a device that is
    /// still listed adopts a later roster and re-projects its fleet.
    #[test]
    fn an_own_roster_notice_that_still_lists_this_device_converges() {
        let fleet = fleet();
        let store = my_store(&fleet);

        let adoption = apply(&store, &fleet, &fleet.grown);
        assert_eq!(adoption.outcome, crate::RevocationAdoptionOutcome::Adopted);
        assert_eq!(store.own_roster().unwrap().unwrap(), fleet.grown);
        let projected = store.own_device_fleet().unwrap();
        assert_eq!(projected.own_device_id, Some(fleet.me.device_id.clone()));
        assert_eq!(projected.device_ids.len(), 3);
        assert_eq!(projected.projected_from, fleet.grown.version());
        assert!(
            store
                .link_gate(CoreLinkGatedAction::Author)
                .unwrap()
                .allowed
        );
    }

    /// §6: a roster that announces a rotated inbox key is not adopted off a
    /// plaintext frame, because the frame cannot carry the key. The gap is
    /// reported instead of half-applied — §10.1's sealed handoff closes it.
    #[test]
    fn an_own_roster_notice_announcing_a_rotation_waits_for_its_key() {
        let fleet = fleet();
        let store = my_store(&fleet);
        assert_eq!(
            apply(&store, &fleet, &fleet.grown).outcome,
            crate::RevocationAdoptionOutcome::Adopted
        );

        let adoption = apply(&store, &fleet, &fleet.rotated);
        assert_eq!(
            adoption.outcome,
            crate::RevocationAdoptionOutcome::AwaitingRotationKey
        );
        assert_eq!(adoption.inbox_key, None);
        assert_eq!(
            store.own_roster().unwrap().unwrap(),
            fleet.grown,
            "nothing is written: a roster whose sync traffic this device cannot open is not adopted"
        );
    }

    /// A roster about somebody else is never this device's news, whatever the
    /// link claimed.
    #[test]
    fn an_own_roster_notice_must_be_about_this_person() {
        let mine = fleet();
        let theirs = fleet();
        let store = my_store(&mine);
        assert!(store
            .apply_own_roster_notice(
                notice(&theirs.burying),
                mine.person.sign_pk.clone(),
                mine.me.device_id.clone(),
                NOW,
            )
            .is_err());
        assert_eq!(store.own_roster().unwrap().unwrap(), mine.held);
    }

    /// §10 step 5 has to be level-triggered, not edge-triggered: the removal
    /// the field missed happened while the own-device link was already up, so
    /// the one HELLO2 that would have carried the notice had come and gone.
    #[test]
    fn a_live_own_device_link_is_re_offered_the_roster_on_a_timer() {
        // Never offered on this link: due immediately.
        assert!(core_own_roster_notice_reoffer_due(None, 0));
        // Just offered: not again yet.
        assert!(!core_own_roster_notice_reoffer_due(Some(1_000), 1_001));
        assert!(!core_own_roster_notice_reoffer_due(
            Some(1_000),
            1_000 + OWN_ROSTER_NOTICE_REOFFER_INTERVAL_MS - 1
        ));
        assert!(core_own_roster_notice_reoffer_due(
            Some(1_000),
            1_000 + OWN_ROSTER_NOTICE_REOFFER_INTERVAL_MS
        ));
        // A clock that jumped backwards is due rather than wedged shut: the
        // frame is idempotent, so silence is the only failure worth avoiding.
        assert!(core_own_roster_notice_reoffer_due(Some(1_000), 4));
    }

    /// The sending half: what goes out, and the states in which nothing does. A
    /// removed device never announces — the news travels from the fleet toward
    /// it, never out of it.
    #[test]
    fn the_own_roster_notice_frame_carries_the_document_and_stops_at_the_gate() {
        let fleet = fleet();
        let store = my_store(&fleet);

        let frame = store.own_roster_notice_frame().unwrap().unwrap();
        match crate::parse_frame(frame).unwrap() {
            crate::Frame::OwnRoster { document } => {
                assert_eq!(crate::core_decode_roster(document).unwrap(), fleet.held);
            }
            other => panic!("own roster notice parsed as {other:?}"),
        }

        // An install that has never linked has no roster to announce.
        let fresh = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(fresh.own_roster_notice_frame().unwrap(), None);

        // And a device this mechanism has ejected says nothing at all.
        assert_eq!(
            apply(&store, &fleet, &fleet.burying).outcome,
            crate::RevocationAdoptionOutcome::RevokedSelf
        );
        assert_eq!(store.own_roster_notice_frame().unwrap(), None);

        // Nor does one still being adopted (§9.4's silence).
        let adopting = MessageStore::open(":memory:".to_string()).unwrap();
        adopting
            .adopt_own_roster(
                fleet.held.clone(),
                fleet.person.sign_pk.clone(),
                fleet.approver.device_id.clone(),
            )
            .unwrap();
        adopting
            .begin_link_activation(BINDING.to_vec(), NOW)
            .unwrap();
        assert_eq!(adopting.own_roster_notice_frame().unwrap(), None);
    }

    /// DL-5, asserted on the bytes that actually go out rather than inherited
    /// from the type: an own-roster notice is keys, ids and counters. There is
    /// no field an endpoint fits in today, and this is what notices it if one
    /// is ever added.
    #[test]
    fn an_own_roster_notice_carries_no_endpoint() {
        let fleet = fleet();
        let store = my_store(&fleet);
        let frame = store.own_roster_notice_frame().unwrap().unwrap();
        for needle in [
            "http".as_bytes(),
            "192.168".as_bytes(),
            ".local".as_bytes(),
            "relay".as_bytes(),
        ] {
            assert!(
                !frame.windows(needle.len()).any(|window| window == needle),
                "an own-roster notice must never carry an address"
            );
        }
    }
}
