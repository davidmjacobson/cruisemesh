//! Revocation: burying a device and rotating what it could read
//! (`specs/multi-device-v1.md` §10).
//!
//! §10 is four steps, and this module owns the first of them end to end:
//!
//! 1. **Roster update.** Tombstone the device, bump `seq` (or `recovery_epoch`
//!    on the recovery path), bump `inbox_key_generation`, rotate the inbox key,
//!    gossip to all contacts and remaining own devices.
//! 2. Rotate the shared relay `family_token`.
//! 3. Receivers refuse signatures from a tombstoned `device_id` on newly
//!    received events.
//! 4. Contacts get the standard changed-safety-state surface treatment.
//! 5. The removed device itself converges when it next meets a sibling on a
//!    link that has proved it belongs to this person, and ejects itself.
//!
//! Step 5 is [`MessageStore::eject_self_from_fleet`] plus its two callers —
//! [`MessageStore::adopt_revocation_handoff`] here and
//! [`MessageStore::apply_own_roster_notice`] in `device_link::activation`, which
//! is the carrier that can actually reach a device this roster has buried. It
//! exists because steps 1 and 2 are the *capability* half: they take away what a
//! thief could use, and deliberately say nothing to whoever is holding the
//! phone. That is right for a thief and wrong for the person who found their own
//! old phone in a drawer — without step 5 that phone believes itself linked
//! forever, keeps advertising, and keeps accepting mail.
//!
//! Step 2 is [`crate::relay_rotation`], which takes this module's
//! [`RevocationCommit`] as its trigger — a relay credential is rotated because
//! a revocation happened, and that module's entry point will not plan one
//! without the commit that caused it. Step 4 is its own slice and is not here.
//! Step 3 already ships for
//! own-device traffic — [`crate::core_sync_record_admit`] and
//! `outer_signer_is_own` both refuse a tombstoned author — and what this module
//! adds is the thing that made those checks unreachable: nothing could produce a
//! roster with a tombstone in it.
//!
//! # The threat model is one sentence
//!
//! **Assume the revoked device is hostile, holds the old inbox key and the old
//! relay credential, and replays everything it ever saw.** Every choice below
//! follows from that:
//!
//! * A revocation *always* rotates the inbox key material, never only the
//!   generation counter. A generation that moved without new keys behind it is a
//!   number the thief can ignore.
//! * The record that announces the rotation is sealed per surviving sibling to
//!   that sibling's device key ([`crate::core_seal_sync_handoff`]), because both
//!   inbox generations are wrong addresses for it — see that function.
//! * The certificates a revoked device signed are re-signed in the same update.
//!   [`crate::core_roster_validate`]'s chain rule requires every certificate to
//!   terminate at the person root through certificates that are *still listed*,
//!   and a tombstone keeps no certificate, so a device buried without re-signing
//!   its orphans produces a roster every contact correctly rejects.
//! * Only the person root — which §14.2 keeps inside the passphrase-encrypted
//!   `.cmbak` and off every device — may raise `recovery_epoch`. That is what
//!   dethrones a *stolen approving device*, and it is why
//!   [`core_recovery_revoke_roster`] takes a root secret rather than a device
//!   one. No device key alone can mint a higher epoch;
//!   [`crate::RosterUpdateReason::RecoveryEpochRequiresRoot`] is the receiving
//!   half of the same rule.
//!
//! # What revocation deliberately does not do
//!
//! It does not touch DTN ack safety, and it cannot: nothing here acks, deletes,
//! or dispatches an envelope. It does not touch endpoint privacy either — a
//! [`Roster`] has no field an endpoint fits in (DL-5), so the document that
//! gossips to every contact is keys, ids and counters, exactly as it was before.
//!
//! And it never resurrects. DL-4 says a revoked `device_id` is gone forever,
//! including through the recovery path: re-linking the same physical hardware
//! mints a fresh key and therefore a different device id. Both builders below
//! refuse to list a tombstoned id, and both carry every tombstone they were
//! given forward untouched.

use rusqlite::OptionalExtension;

use crate::device_roster::{
    core_roster_accept, core_roster_validate, core_sign_device_cert, core_sign_roster,
    roster_head_hash, DeviceCert, DeviceKeypair, DeviceTombstone, Roster, RosterUpdateOutcome,
    RosterUpdateReason, DEVICE_CERT_FLAG_ROSTER_SIGNING, DEVICE_ID_LEN,
};
use crate::identity::derive_user_id;
use crate::sync_record::{
    core_device_sync_identity, core_encode_sync_own_roster, core_mint_inbox_key,
    core_open_sync_handoff, core_rotate_inbox_key, core_seal_sync_handoff, core_seal_sync_record,
    core_sign_sync_record, InboxKey, SealedSyncRecord, SyncOwnRosterPayload, SyncRecord,
    SyncRecordKind,
};
use crate::{CoreError, MessageStore};

/// Which of §3's two authorities signed a revocation.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevocationPath {
    /// "Remove device" on the device that holds the roster-signing role. Bumps
    /// `seq` within the current `recovery_epoch`.
    ApprovingDevice,
    /// §14.2's override: a roster signed with the person root secret opened out
    /// of the encrypted backup, at the next `recovery_epoch`. This is the only
    /// path that can dethrone a stolen approving device, and the only path a
    /// device key alone can never take.
    RecoveryEpoch,
}

/// One revocation, ready to be committed locally and gossiped (§10.1).
///
/// It carries secret material — `inbox_key`'s secret half — under exactly the
/// contract [`crate::Identity`] and [`DeviceKeypair`] already keep: the core
/// mints and never persists, the shell puts it in platform-protected storage.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct RevocationUpdate {
    /// The signed post-revocation roster.
    pub roster: Roster,
    /// [`crate::core_roster_head_hash`] of `roster`.
    pub roster_head: Vec<u8>,
    /// The device ids this update buries, in the order they were named. Already
    /// present tombstones are not repeated here — this is what *changed*, which
    /// is the fact a shell surfaces and a sibling reacts to.
    pub revoked_device_ids: Vec<Vec<u8>>,
    /// §6's rotated key: brand-new material at
    /// `roster.inbox_key_generation`. Nothing of the superseded key survives
    /// into it.
    ///
    /// # What this rotates, and what it leaves exactly where it was
    ///
    /// The inbox key is the key **own devices** seal each other's sync records
    /// to, and rotating it is what stops a revoked sibling reading the fleet's
    /// self-sync traffic from the moment the rotation lands.
    ///
    /// It is not the key contacts seal mail to. Generation 0 *is* the deployed
    /// person agreement key — [`crate::Identity`]'s `agree_pk`, the one on
    /// every friend card and QR code already in the field — which is the whole
    /// reason §3 can promise nobody has to re-friend anybody. A revocation
    /// therefore does **not** rotate the key a contact uses to write to this
    /// person, and cannot: doing so would invalidate every card ever shared
    /// and turn "remove this phone" into "re-friend everyone you know".
    ///
    /// Said plainly, because a reader deserves it in one sentence: a revoked
    /// device that keeps the person's identity secret can still open new
    /// contact mail addressed to that person, and what this rotation takes
    /// away is the fleet's own sync channel, the retained backlog, and — with
    /// §10.2 — the relay mailbox. Cutting the contact channel too is a
    /// re-friending ceremony, which §2 lists as a non-goal for v1 and which
    /// §14.2's recovery path is the eventual answer to.
    pub inbox_key: InboxKey,
    pub path: RevocationPath,
}

/// The device ids `current` buries that `previous` had not (§10.4's fact,
/// without §10.4's surface).
///
/// Nothing in the core diffed tombstone sets before this: DL-4 kept them and
/// DL-1 refused to lose them, but no path asked *which ones are new*. A contact
/// learning that a person just revoked a device is a changed-safety-state fact,
/// and a fact is what this returns — the reason code and the copy that go with
/// it belong to the notification slice and to WP6.
///
/// `previous` is `None` for a first roster, where every tombstone is news.
#[uniffi::export]
pub fn core_roster_newly_revoked(previous: Option<Roster>, current: Roster) -> Vec<Vec<u8>> {
    let known: Vec<Vec<u8>> = previous
        .map(|roster| {
            roster
                .tombstones
                .into_iter()
                .map(|tombstone| tombstone.device_id)
                .collect()
        })
        .unwrap_or_default();
    current
        .tombstones
        .into_iter()
        .filter(|tombstone| !known.contains(&tombstone.device_id))
        .map(|tombstone| tombstone.device_id)
        .collect()
}

// ---------------------------------------------------------------------------
// §10.1, the approving-device path
// ---------------------------------------------------------------------------

/// **"Remove device", signed by the device that holds the roster-signing role.**
///
/// The mirror image of [`crate::core_link_sign_new_device_roster`], and
/// deliberately so: same authority check, same `seq + 1`, same re-validation of
/// what it just produced. What it adds is everything §10.1 asks for beyond the
/// membership change — the tombstone (DL-4), the inbox generation bump (§6), the
/// rotated key material, and the re-signing of whatever the buried device had
/// vouched for.
///
/// `current_inbox_key` must be the key `current.inbox_key_generation` names.
/// Requiring the whole key rather than a counter is what makes the rotation real:
/// a caller that does not hold the current key cannot pretend to have rotated it,
/// and [`core_rotate_inbox_key`] mints material that shares nothing with its
/// input.
///
/// Two refusals worth naming, because each is a different failure with the same
/// shape:
///
/// * **The approving device may not bury itself.** A roster whose
///   `approving_device_id` names no active device is rejected by
///   [`core_roster_validate`], and handing the role to another device on the way
///   out would let a stolen phone nominate its successor. Removing the approving
///   device is §14.2's job — [`core_recovery_revoke_roster`] — where the person
///   root is what says who approves next.
/// * **The last device may not be buried.** A person with no devices has no way
///   back except the recovery path, and producing that state from a device key
///   would be a self-inflicted lockout one tap deep.
#[uniffi::export]
pub fn core_revoke_devices_roster(
    current: Roster,
    person_root_sign_pk: Vec<u8>,
    approving_device_sign_sk: Vec<u8>,
    revoked_device_ids: Vec<Vec<u8>>,
    current_inbox_key: InboxKey,
) -> Result<RevocationUpdate, CoreError> {
    if let Some(rejection) = core_roster_validate(current.clone(), person_root_sign_pk.clone()) {
        return Err(CoreError::Malformed(format!(
            "the roster to revoke from is not acceptable: {rejection:?}"
        )));
    }
    let approving_sign_pk = public_of(&approving_device_sign_sk)?;
    let approving_device_id = derive_user_id(&approving_sign_pk).to_vec();
    if approving_device_id != current.approving_device_id {
        // §3: exactly one device holds the roster-signing role, and this key is
        // not it.
        return Err(CoreError::Malformed(
            "this device does not hold the roster-signing role".to_string(),
        ));
    }
    let revoked = normalize_revoked(&current, &revoked_device_ids)?;
    if revoked.contains(&approving_device_id) {
        return Err(CoreError::Malformed(
            "the approving device cannot revoke itself; that takes the recovery material"
                .to_string(),
        ));
    }
    let survivors: Vec<DeviceCert> = current
        .devices
        .iter()
        .filter(|cert| !revoked.contains(&cert.device_id()))
        .cloned()
        .collect();
    if survivors.is_empty() {
        return Err(CoreError::Malformed(
            "a person must keep at least one device".to_string(),
        ));
    }
    let seq = next_version_component(current.seq, "seq")?;

    // The chain rule, applied at the moment it would otherwise break: a
    // certificate signed by a device this update buries is orphaned, because a
    // tombstone keeps no certificate for the chain to pass through. Re-signing
    // them here — under the approving device, which is itself still listed — is
    // what [`core_roster_validate`]'s doc comment says §10.1 does.
    let revoked_signer_keys: Vec<Vec<u8>> = current
        .devices
        .iter()
        .filter(|cert| revoked.contains(&cert.device_id()))
        .map(|cert| cert.device_sign_pk.clone())
        .collect();
    let mut devices = Vec::with_capacity(survivors.len());
    for cert in survivors {
        if revoked_signer_keys.contains(&cert.signer_sign_pk) {
            devices.push(core_sign_device_cert(
                DeviceCert {
                    signer_sign_pk: Vec::new(),
                    signature: Vec::new(),
                    ..cert
                },
                approving_device_sign_sk.clone(),
            )?);
        } else {
            devices.push(cert);
        }
    }

    let mut tombstones = current.tombstones.clone();
    for device_id in &revoked {
        tombstones.push(DeviceTombstone {
            device_id: device_id.clone(),
            revoked_at_seq: seq,
        });
    }
    let inbox_key = rotated_key(&current, current_inbox_key)?;
    let roster = Roster {
        seq,
        devices,
        tombstones,
        inbox_key_generation: inbox_key.generation,
        signer_sign_pk: Vec::new(),
        signature: Vec::new(),
        ..current
    };
    let roster = validated(
        core_sign_roster(roster, approving_device_sign_sk)?,
        &person_root_sign_pk,
    )?;
    Ok(RevocationUpdate {
        roster_head: roster_head_hash(&roster),
        roster,
        revoked_device_ids: revoked,
        inbox_key,
        path: RevocationPath::ApprovingDevice,
    })
}

// ---------------------------------------------------------------------------
// §10.1, the recovery-epoch path (§14.2)
// ---------------------------------------------------------------------------

/// **The override: a revocation signed with the person root out of the backup.**
///
/// This is how §3's "a stolen approving device is dethroned" actually happens.
/// The thief holds a device key and can sign rosters at `seq + 1` all day; it
/// cannot sign one at a higher `recovery_epoch`, because §14.2 keeps the person
/// root secret only inside the passphrase-encrypted `.cmbak`. A contact holding
/// the thief's latest roster accepts this one on DL-1's ordering and refuses
/// every later document the thief signs as a
/// [`crate::RosterUpdateReason::Rollback`].
///
/// It differs from [`crate::core_link_recovery_roster`] — which starts a new
/// epoch with the recovering device *alone* and buries nothing — in the one way
/// §10 cares about: it names the devices to bury and buries them, forever
/// (DL-4). Devices not named survive into the new epoch. That distinction is the
/// whole product difference between "I lost my phone" and "I am setting up a
/// replacement": the second must not silently unlink the tablet.
///
/// Every surviving certificate is re-signed under the root, not merely carried
/// over. Whoever signed them before may be the device this update is burying —
/// in the case that matters, it *is* — and a certificate signed by a tombstoned
/// device is an orphan [`core_roster_validate`] rejects. Signing them all from
/// the root is one rule instead of a conditional, and the root is in hand by
/// construction on this path.
///
/// `current_inbox_key` is `None` when recovery happens on a phone that never
/// held the key — opening a backup on a fresh install is the ordinary case — and
/// the new material is minted one generation above what the stored roster
/// announced. Passing the key when it *is* held changes nothing about the
/// result's safety; it only keeps the generation arithmetic in one place.
///
/// # `stored` must be the NEWEST roster in hand, not the backup's snapshot
///
/// DL-4 is absolute and does not bend for the root: `core_roster_accept` refuses
/// an incoming roster that drops a tombstone the receiver already holds, at
/// **any** epoch. So a recovery built from a roster older than a burial a
/// contact has already seen is ignored by that contact, and a higher epoch does
/// not rescue it.
///
/// That matters because the stolen device can plant burials. A thief holding the
/// approving device may tombstone the owner's other phones before the owner ever
/// opens the backup, and a recovery built from the `.cmbak`'s own snapshot would
/// not carry those tombstones forward. The remedy is the input: pass the most
/// recent roster this device holds for the person — from a sibling, from a
/// contact's gossip, from whatever arrived last — and every tombstone travels
/// with it. The burials themselves are not undoable and are not meant to be:
/// DL-4's own remedy is that the buried hardware re-links with a fresh key,
/// which the new epoch's approving device can authorize immediately.
///
/// The residual, stated plainly: a person whose only copy of their roster is an
/// old backup, whose contacts have since seen a thief's burials, recovers for
/// the contacts that had not seen them and has to re-share a card with the rest.
/// Propagation-bounded, never permanent brickage — the same shape as the
/// stale-endpoint repair path this codebase already runs.
#[uniffi::export]
pub fn core_recovery_revoke_roster(
    stored: Roster,
    person_root_sign_sk: Vec<u8>,
    recovering_device_sign_pk: Vec<u8>,
    recovering_device_agree_pk: Vec<u8>,
    revoked_device_ids: Vec<Vec<u8>>,
    current_inbox_key: Option<InboxKey>,
) -> Result<RevocationUpdate, CoreError> {
    let person_root_sign_pk = public_of(&person_root_sign_sk)?;
    let person_id = derive_user_id(&person_root_sign_pk).to_vec();
    if stored.person_id != person_id {
        return Err(CoreError::Malformed(
            "the roster to recover from is for a different person than the backup".to_string(),
        ));
    }
    if let Some(rejection) = core_roster_validate(stored.clone(), person_root_sign_pk.clone()) {
        return Err(CoreError::Malformed(format!(
            "the roster to recover from is not acceptable: {rejection:?}"
        )));
    }
    let revoked = normalize_revoked(&stored, &revoked_device_ids)?;
    let recovering_device_id = derive_user_id(&recovering_device_sign_pk).to_vec();
    if stored
        .tombstones
        .iter()
        .any(|tombstone| tombstone.device_id == recovering_device_id)
        || revoked.contains(&recovering_device_id)
    {
        // DL-4, and the trap this closes is real: a person recovering onto the
        // phone they just told the roster to bury would produce a document
        // every contact refuses as `TombstonedDeviceActive`. Re-linking that
        // hardware mints a fresh key.
        return Err(CoreError::Malformed(
            "this device id was revoked and can never return".to_string(),
        ));
    }
    let recovery_epoch = next_version_component(stored.recovery_epoch, "recovery_epoch")?;

    // Survivors first, re-certified under the root; then this device, added if
    // the stored roster did not already list it.
    let mut devices = Vec::new();
    for cert in &stored.devices {
        let device_id = cert.device_id();
        if revoked.contains(&device_id) {
            continue;
        }
        let is_recovering = device_id == recovering_device_id;
        devices.push(core_sign_device_cert(
            DeviceCert {
                // Exactly one device carries the roster-signing flag, and after
                // a recovery it is the one holding the backup. Every other bit
                // is preserved verbatim.
                flags: if is_recovering {
                    cert.flags | DEVICE_CERT_FLAG_ROSTER_SIGNING
                } else {
                    cert.flags & !DEVICE_CERT_FLAG_ROSTER_SIGNING
                },
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
                ..cert.clone()
            },
            person_root_sign_sk.clone(),
        )?);
    }
    if !devices
        .iter()
        .any(|cert| cert.device_id() == recovering_device_id)
    {
        devices.push(core_sign_device_cert(
            DeviceCert {
                person_id: person_id.clone(),
                device_sign_pk: recovering_device_sign_pk.clone(),
                device_agree_pk: recovering_device_agree_pk,
                added_epoch: recovery_epoch,
                flags: DEVICE_CERT_FLAG_ROSTER_SIGNING,
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
            },
            person_root_sign_sk.clone(),
        )?);
    }

    let mut tombstones = stored.tombstones.clone();
    for device_id in &revoked {
        tombstones.push(DeviceTombstone {
            device_id: device_id.clone(),
            // A new epoch resets `seq` to 0, so that is the seq these burials
            // happened at. The epoch is what orders them against the old ones;
            // the field names where in the document's own history a device was
            // buried, and nothing compares it across epochs (DL-1 compares
            // `(recovery_epoch, seq)`, DL-4 compares ids).
            revoked_at_seq: 0,
        });
    }
    let inbox_key = match current_inbox_key {
        Some(key) => rotated_key(&stored, key)?,
        // §6: the generation always climbs, and the material behind it is
        // always new. `core_link_recovery_roster` moves the counter without the
        // material and says so; this path is §10's, so it does both.
        None => core_mint_inbox_key(stored.inbox_key_generation.saturating_add(1)),
    };
    let roster = Roster {
        person_id,
        recovery_epoch,
        // Genesis of the new epoch, which `core_roster_validate` requires to be
        // root-signed — and it is.
        seq: 0,
        approving_device_id: recovering_device_id,
        devices,
        tombstones,
        inbox_key_generation: inbox_key.generation,
        signer_sign_pk: Vec::new(),
        signature: Vec::new(),
    };
    let roster = validated(
        core_sign_roster(roster, person_root_sign_sk)?,
        &person_root_sign_pk,
    )?;
    Ok(RevocationUpdate {
        roster_head: roster_head_hash(&roster),
        roster,
        revoked_device_ids: revoked,
        inbox_key,
        path: RevocationPath::RecoveryEpoch,
    })
}

// ---------------------------------------------------------------------------
// The store side: commit, re-seal, and the two gossip legs (§10.1)
// ---------------------------------------------------------------------------

pub(crate) const REVOCATION_SCHEMA_SQL: &str = "
-- §10.1's crash-safety journal, and the record that makes a handoff
-- re-issuable. At most one row, ever: a device performs one revocation at a
-- time, and a second `begin` replaces a first that never committed.
--
-- The row is written by `begin_own_revocation` BEFORE the rotated inbox key
-- reaches platform storage, and is what `commit_own_revocation` requires as
-- proof that the shell was handed the key and said it wrote it down. After
-- the commit it stays, carrying the stream slot of the announcement so
-- `revocation_handoffs_for` can re-seal a copy for a sibling that was offline
-- for the ceremony.
--
-- It holds no secret. `inbox_key_generation` is a counter, the device ids are
-- public, and the rotated key itself is never written here -- the core mints
-- and never persists inbox key material, which is what keeps a `.cmbak` of
-- this database from carrying the fleet's mail-reading secret.
CREATE TABLE IF NOT EXISTS own_revocation (
    id                   INTEGER PRIMARY KEY CHECK (id = 1),
    roster_head          BLOB NOT NULL,
    inbox_key_generation INTEGER NOT NULL,
    revoked_device_ids   BLOB NOT NULL,
    stream_seq           INTEGER,
    started_at_ms        INTEGER NOT NULL,
    committed_at_ms      INTEGER
);
";

/// A revocation that has been written down but whose key may not be stored yet
/// (§10.1, crash safety).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PendingRevocation {
    /// [`crate::core_roster_head_hash`] of the roster the revocation produced.
    /// The identity of the ceremony, and what
    /// [`MessageStore::commit_own_revocation`] matches its argument against.
    pub roster_head: Vec<u8>,
    /// The generation the rotated key belongs to. A device that finds a
    /// pending row asks its platform storage for this generation: holding it
    /// means the key survived and the commit can be re-run, and not holding it
    /// means nothing was ever rotated and the ceremony can be replanned from
    /// scratch.
    pub inbox_key_generation: u64,
    pub revoked_device_ids: Vec<Vec<u8>>,
}

/// One sealed copy of the rotation announcement, addressed to one surviving
/// sibling (§10.1, [`core_seal_sync_handoff`]).
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct RevocationHandoff {
    pub device_id: Vec<u8>,
    pub sealed: SealedSyncRecord,
}

/// What a committed revocation left behind, and what still has to go out.
///
/// The two gossip legs are deliberately different in kind, because §10.1's two
/// audiences are:
///
/// * **Own devices** get `handoffs` — a real signed [`SyncRecord`], sealed once
///   per surviving sibling, ready for any of the four transports. WP4's carrier
///   already exists, so this leg is executed rather than described.
/// * **Contacts** get `roster_document` sealed pairwise per contact (DL-3), and
///   `contact_user_ids` is exactly who must be told. Both were produced here
///   before anything could carry them. WP6 added [`crate::KIND_ROSTER_GOSSIP`],
///   so this leg is now executed too: the caller runs
///   [`MessageStore::announce_own_roster`] once this returns, which seals the
///   new document to every contact that does not already hold this head.
///
///   The two fields stay, as the plan a caller can read rather than as the
///   delivery. That is deliberate crash-safety, not duplication: the
///   announcement re-derives both from the roster this store now holds, so a
///   revocation that committed and then died before sending anything is
///   repaired by the next announcement instead of leaving contacts silently
///   un-told.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct RevocationCommit {
    pub roster: Roster,
    pub roster_head: Vec<u8>,
    pub inbox_key_generation: u64,
    /// What changed, for §10.4's surface: the ids this update buried.
    pub revoked_device_ids: Vec<Vec<u8>>,
    /// The stream slot the announcement was authored at, on this device's
    /// [`SyncRecordKind::OwnRoster`] stream.
    pub stream_seq: u64,
    pub handoffs: Vec<RevocationHandoff>,
    pub contact_user_ids: Vec<Vec<u8>>,
    /// [`crate::core_encode_roster`] of the new roster: the DL-3 document.
    pub roster_document: Vec<u8>,
    /// Retained records re-sealed from the superseded generation to the new one.
    pub resealed_records: u32,
    /// Retained records left sealed under a generation this device can no longer
    /// open. Never deleted — see [`MessageStore::commit_own_revocation`].
    pub unresealable_records: u32,
}

/// What adopting somebody else's revocation did to this device.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevocationAdoptionOutcome {
    /// The roster superseded what was held and is now this device's own.
    Adopted,
    /// DL-1: the document does not supersede what is stored. Idempotent gossip,
    /// or a replay.
    NotSuperseding,
    /// This device is the one being buried, and it has ejected itself
    /// ([`MessageStore::eject_self_from_fleet`], §10 step 5): the burying roster
    /// is stored, the fleet projection is cleared, and the activation gate is
    /// [`crate::CoreLinkActivationStage::Revoked`] — so this device no longer
    /// advertises, authors or acks.
    ///
    /// No fleet is *adopted*, because a tombstoned device is not a member of the
    /// fleet the document describes and
    /// [`MessageStore::adopt_own_roster`] refuses it for exactly that reason.
    /// What used to happen here was nothing at all: the outcome was reported and
    /// no byte was written, which left a removed device reporting itself linked
    /// forever.
    RevokedSelf,
    /// The document supersedes what is held and still lists this device, but it
    /// announces an inbox key generation this device does not hold — so there is
    /// a §10.1 rotation still owed, and nothing was written.
    ///
    /// Only [`MessageStore::apply_own_roster_notice`] returns this: a plaintext
    /// link frame carries a roster and never key material, so a device that
    /// adopted the roster there would hold a fleet whose sync traffic it cannot
    /// open. The sealed handoff is what closes this, and reporting the gap is
    /// better than half-applying it.
    AwaitingRotationKey,
    /// The acceptance rules refused the document — DL-2's fork quarantine,
    /// DL-4's tombstones, §6's generation rule, §14.2's epoch authority.
    /// [`RevocationAdoption::reason`] says which.
    Refused,
    /// DL-2: this document forks the roster this device holds, and from here
    /// on this person's roster updates are quarantined until a person resolves
    /// it ([`MessageStore::clear_roster_quarantine`]).
    ForkQuarantined,
}

/// The result of adopting a sibling's rotation announcement.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct RevocationAdoption {
    pub outcome: RevocationAdoptionOutcome,
    /// Why, in [`core_roster_accept`]'s own vocabulary. Carried out rather
    /// than collapsed into the outcome because a shell has to be able to tell
    /// "this arrived twice" from "somebody is replaying a roster that tries to
    /// exhume a device you buried" — the first is silence, the second is a
    /// safety surface.
    pub reason: RosterUpdateReason,
    /// The ids this document buried that were not buried before — §10.4's fact,
    /// seen from a sibling.
    pub revoked_device_ids: Vec<Vec<u8>>,
    /// The rotated key the announcement carried, for the shell to persist in
    /// platform-protected storage. `None` when nothing was adopted.
    pub inbox_key: Option<InboxKey>,
    /// Retained records re-sealed from the superseded generation to the
    /// adopted one, when the caller supplied the superseded key.
    pub resealed_records: u32,
    /// Retained records left sealed under a generation this device can no
    /// longer open. Never deleted — see
    /// [`MessageStore::commit_own_revocation`].
    pub unresealable_records: u32,
}

#[uniffi::export]
impl MessageStore {
    /// **Write the revocation down and hand over its key** (§10.1, crash
    /// safety).
    ///
    /// The first half of the two-call ceremony
    /// [`Self::commit_own_revocation`] documents. It validates the update,
    /// records the journal row, and returns the rotated
    /// [`InboxKey`] — which the caller must put in platform-protected storage
    /// **before** calling the commit. Nothing observable has changed yet: no
    /// row has been re-sealed, the roster has not been adopted, and abandoning
    /// here leaves the device exactly where it was.
    ///
    /// A second `begin` replaces a first that never committed, for the same
    /// reason [`Self::begin_relay_rotation`] does: a rotation that was never
    /// performed is worth nothing, and neither is the key it proposed.
    pub fn begin_own_revocation(
        &self,
        update: RevocationUpdate,
        person_root_sign_pk: Vec<u8>,
        own_device: DeviceKeypair,
        now_ms: i64,
    ) -> Result<InboxKey, CoreError> {
        self.check_revocation_update(&update, &person_root_sign_pk, &own_device)?;
        let conn = self.locked_conn();
        conn.execute(
            "INSERT INTO own_revocation
                (id, roster_head, inbox_key_generation, revoked_device_ids,
                 stream_seq, started_at_ms, committed_at_ms)
             VALUES (1, ?1, ?2, ?3, NULL, ?4, NULL)
             ON CONFLICT(id) DO UPDATE SET
                roster_head = excluded.roster_head,
                inbox_key_generation = excluded.inbox_key_generation,
                revoked_device_ids = excluded.revoked_device_ids,
                stream_seq = NULL,
                started_at_ms = excluded.started_at_ms,
                committed_at_ms = NULL",
            rusqlite::params![
                update.roster_head,
                update.inbox_key.generation as i64,
                crate::relay_rotation::pack_device_ids(&update.revoked_device_ids),
                now_ms,
            ],
        )
        .map_err(crate::store::store_err)?;
        Ok(update.inbox_key)
    }

    /// The revocation this device began and has not committed, if any.
    ///
    /// A shell finding one on launch asks its own key store whether it holds
    /// [`PendingRevocation::inbox_key_generation`]. Holding it means the key
    /// survived and [`Self::commit_own_revocation`] can be re-run with the
    /// same update to finish; not holding it means nothing was re-sealed to a
    /// secret that no longer exists, and the ceremony can be planned again.
    pub fn pending_own_revocation(&self) -> Result<Option<PendingRevocation>, CoreError> {
        let conn = self.locked_conn();
        conn.query_row(
            "SELECT roster_head, inbox_key_generation, revoked_device_ids
               FROM own_revocation WHERE id = 1 AND committed_at_ms IS NULL",
            [],
            |row| {
                Ok(PendingRevocation {
                    roster_head: row.get(0)?,
                    inbox_key_generation: row.get::<_, i64>(1)? as u64,
                    revoked_device_ids: crate::relay_rotation::unpack_device_ids(
                        &row.get::<_, Vec<u8>>(2)?,
                    ),
                })
            },
        )
        .optional()
        .map_err(crate::store::store_err)
    }

    /// Give up on a revocation whose key never reached storage, leaving this
    /// device exactly where it was. Returns whether anything was pending.
    pub fn abandon_own_revocation(&self) -> Result<bool, CoreError> {
        let conn = self.locked_conn();
        let cleared = conn
            .execute(
                "DELETE FROM own_revocation WHERE id = 1 AND committed_at_ms IS NULL",
                [],
            )
            .map_err(crate::store::store_err)?;
        Ok(cleared > 0)
    }

    /// **Re-issue §10.1's rotation announcement to one sibling.**
    ///
    /// The ceremony hands out [`RevocationCommit::handoffs`] once, at the
    /// moment the person taps "Remove device", to whichever siblings a
    /// transport can reach right then. In a family that is roughly nobody: the
    /// other phone is in a bag, the tablet is at home, and the *normal* case
    /// is a sibling that was not present for the ceremony at all. Without a
    /// way to re-issue, that sibling never learns the rotation — it cannot
    /// read the retained copy (sealed to a generation it does not have), it
    /// cannot read the Settings stream carrying the new relay credential for
    /// the same reason, and it is stranded until somebody re-links it.
    ///
    /// So the journal keeps the announcement's stream slot, and this re-seals
    /// a fresh handoff copy from the retained record whenever a sibling turns
    /// up. It is not a cached blob: re-sealing means the copy is addressed
    /// against the roster this device holds **now**, so a sibling that has
    /// since been revoked itself gets nothing (`core_seal_sync_handoff`
    /// refuses a tombstoned address, and this returns an empty list rather
    /// than an error), and a fleet that rotated twice hands out the latest
    /// announcement rather than a stale one.
    ///
    /// `inbox_key` is the current key, and it has to be passed in for the
    /// reason the whole module turns on: the core mints inbox key material and
    /// never persists it, so the retained record — sealed to that key — can
    /// only be reopened by a caller that holds it. Handing it in is the same
    /// contract [`DeviceKeypair`] already has.
    ///
    /// An empty list is the honest answer to "nothing has been revoked here",
    /// "this device did not sign that revocation", and "that device is not a
    /// sibling of mine" alike; none of them is an error.
    pub fn revocation_handoffs_for(
        &self,
        sibling_device_id: Vec<u8>,
        own_device: DeviceKeypair,
        inbox_key: InboxKey,
    ) -> Result<Vec<RevocationHandoff>, CoreError> {
        let Some(roster) = self.own_roster()? else {
            return Ok(Vec::new());
        };
        let stream_seq: Option<i64> = {
            let conn = self.locked_conn();
            conn.query_row(
                "SELECT stream_seq FROM own_revocation
                  WHERE id = 1 AND committed_at_ms IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(crate::store::store_err)?
            .flatten()
        };
        let Some(stream_seq) = stream_seq else {
            return Ok(Vec::new());
        };
        let stored = {
            let conn = self.locked_conn();
            crate::sync_store::sealed_record_at_slot(
                &conn,
                &own_device.device_id,
                SyncRecordKind::OwnRoster,
                stream_seq as u64,
            )?
        };
        let Some(stored) = stored else {
            return Ok(Vec::new());
        };
        // Addressable at all? A sibling this roster has buried since, or one
        // it never listed, is not owed an announcement.
        if !roster
            .devices
            .iter()
            .any(|cert| cert.device_id() == sibling_device_id)
        {
            return Ok(Vec::new());
        }
        let record = crate::core_open_sync_record(stored.sealed, inbox_key, roster.clone())?;
        Ok(vec![RevocationHandoff {
            sealed: core_seal_sync_handoff(
                record,
                core_device_sync_identity(own_device),
                roster,
                sibling_device_id.clone(),
            )?,
            device_id: sibling_device_id,
        }])
    }

    /// **Commit §10.1 on the device that signed it.**
    ///
    /// Order is load-bearing and is the "rotate, then drain" rule made concrete:
    ///
    /// 1. **Re-seal the backlog first**, from `superseded_inbox_key` to the
    ///    rotated one, in place. A retained record's stream slot — and therefore
    ///    its [`crate::core_sync_record_id`] — never moves, so a sibling that
    ///    has been dark for a fortnight is still answered out of storage and no
    ///    un-fetched record is dropped to make the rotation happen. Doing it
    ///    before the roster is adopted means a crash halfway leaves rows sealed
    ///    for a roster this device has not adopted yet: they read stale to
    ///    [`crate::core_sync_seal_is_current`], nothing sends them, and running
    ///    the same `update` again finishes the job.
    /// 2. Adopt the roster and project the fleet
    ///    ([`Self::adopt_own_roster`]).
    /// 3. Point the inbound sync gate at the new roster and generation
    ///    ([`Self::core_set_own_sync_context`]), which is what makes
    ///    [`crate::core_sync_record_admit`] start refusing the revoked device
    ///    (§10.3) and everything sealed under the old key
    ///    ([`crate::SyncRecordRejection::StaleInboxKey`]).
    /// 4. Author, sign and seal the announcement.
    ///
    /// Records this device cannot re-seal — sealed under a generation older than
    /// `superseded_inbox_key`, or of a kind this build cannot name — are counted
    /// and left exactly where they are. Deleting a record because a rotation
    /// could not re-address it would be the rotation losing mail, which is the
    /// one thing it may not do.
    ///
    /// `superseded_inbox_key` is `None` on a recovery from a phone that never
    /// held the old key; there is then no backlog it could open, and none is
    /// touched.
    ///
    /// # Hard precondition: the rotated key must already be durable
    ///
    /// [`Self::begin_own_revocation`] must have run for exactly this update,
    /// and this refuses to proceed until it has. The ordering is not
    /// bookkeeping — it is the difference between a recoverable crash and a
    /// fleet that has locked itself out of its own backlog.
    ///
    /// Step (1) re-seals every retained record *to the rotated key*. If the
    /// process dies after that and the shell never wrote the key to platform
    /// storage, the rows are addressed to a secret that no longer exists
    /// anywhere: the superseded key opens none of them, the rotated key is
    /// gone, and every one of them is unreadable forever. That is the rotation
    /// losing mail by another route, and it is the one thing §10 may not do.
    ///
    /// So the ceremony is two calls with a durable step between them, exactly
    /// like [`Self::begin_relay_rotation`] / [`Self::commit_relay_rotation`]:
    /// `begin_own_revocation` writes the journal row and hands the shell the
    /// key, the shell puts it in platform-protected storage, and only then
    /// does this run. A device that crashes between the two wakes to
    /// [`Self::pending_own_revocation`] and asks its own key store the one
    /// question that settles it — do I hold this generation? Yes means re-run
    /// this and finish; no means nothing was re-sealed, nothing was adopted,
    /// and the revocation can be planned again from the roster it still holds.
    pub fn commit_own_revocation(
        &self,
        update: RevocationUpdate,
        person_root_sign_pk: Vec<u8>,
        own_device: DeviceKeypair,
        superseded_inbox_key: Option<InboxKey>,
        now_ms: i64,
    ) -> Result<RevocationCommit, CoreError> {
        self.check_revocation_update(&update, &person_root_sign_pk, &own_device)?;
        let roster = update.roster.clone();
        let own_device_id = own_device.device_id.clone();

        // (0) The rotated key is durable, or nothing below is safe to do.
        let pending = self.pending_own_revocation()?.ok_or_else(|| {
            CoreError::Store(
                "no revocation is pending: begin_own_revocation must write the rotation down \
                 and its key must reach platform storage before the backlog is re-sealed to it"
                    .to_string(),
            )
        })?;
        if pending.roster_head != update.roster_head
            || pending.inbox_key_generation != update.inbox_key.generation
        {
            return Err(CoreError::Store(
                "the pending revocation is a different one than this update; the key that was \
                 stored is not the key this would re-seal to"
                    .to_string(),
            ));
        }

        // (1) Re-seal, before anything else moves.
        let (resealed_records, unresealable_records) = self.reseal_own_backlog(
            &own_device,
            &superseded_inbox_key,
            &update.roster,
            &update.inbox_key,
        )?;

        // (2) and (3): adopt, then point the inbound gate at it.
        self.adopt_own_roster(roster.clone(), person_root_sign_pk, own_device_id.clone())?;
        self.core_set_own_sync_context(roster.clone(), update.inbox_key.generation)?;

        // (4) The announcement, on this device's own-roster stream.
        let stream_seq =
            self.core_sync_next_stream_seq(own_device_id.clone(), SyncRecordKind::OwnRoster)?;
        let payload = core_encode_sync_own_roster(SyncOwnRosterPayload {
            roster: roster.clone(),
            // The rotated key ALONE. §6 allows a caller to send the generations
            // a sibling could still need, and after a revocation that set is
            // exactly one: shipping the superseded key beside its replacement
            // would hand a freshly-linked device the key the thief also holds.
            inbox_keys: vec![update.inbox_key.clone()],
        })?;
        let record = core_sign_sync_record(
            SyncRecord {
                kind: SyncRecordKind::OwnRoster,
                person_id: roster.person_id.clone(),
                author_device_id: own_device_id.clone(),
                roster_version: roster.version(),
                inbox_key_generation: update.inbox_key.generation,
                stream_seq,
                timestamp_ms: now_ms,
                payload,
                signature: Vec::new(),
            },
            own_device.sign_sk.clone(),
        )?;
        let author = core_device_sync_identity(own_device.clone());

        // The retained copy is sealed to the NEW inbox key: it is what SYNC-1
        // backfills to a sibling that already made the rotation, and it must be
        // unreadable to the device this update buried.
        let retained =
            core_seal_sync_record(record.clone(), author.clone(), update.inbox_key.clone())?;
        self.core_sync_retain_record(record.clone(), retained, now_ms)?;

        // The handoff copies are what a sibling that has NOT made the rotation
        // can actually open — see `core_seal_sync_handoff`.
        let mut handoffs = Vec::new();
        for cert in &roster.devices {
            let device_id = cert.device_id();
            if device_id == own_device_id {
                continue;
            }
            handoffs.push(RevocationHandoff {
                sealed: core_seal_sync_handoff(
                    record.clone(),
                    author.clone(),
                    // The POST-revocation roster, so the address set is the
                    // survivors and the buried device is structurally
                    // unreachable — see `core_seal_sync_handoff`.
                    roster.clone(),
                    device_id.clone(),
                )?,
                device_id,
            });
        }

        // The journal now knows where the announcement lives, which is what
        // `revocation_handoffs_for` re-seals from months later.
        {
            let conn = self.locked_conn();
            conn.execute(
                "UPDATE own_revocation SET stream_seq = ?2, committed_at_ms = ?3
                  WHERE id = 1 AND roster_head = ?1",
                rusqlite::params![update.roster_head, stream_seq as i64, now_ms],
            )
            .map_err(crate::store::store_err)?;
        }

        let contact_user_ids = self
            .list_contacts()?
            .into_iter()
            .map(|contact| contact.user_id)
            .collect();
        Ok(RevocationCommit {
            roster_head: update.roster_head,
            inbox_key_generation: update.inbox_key.generation,
            revoked_device_ids: update.revoked_device_ids,
            stream_seq,
            handoffs,
            contact_user_ids,
            roster_document: crate::core_encode_roster(roster.clone())?,
            resealed_records,
            unresealable_records,
            roster,
        })
    }

    /// **Adopt a sibling's rotation announcement** (§10.1, receiving side).
    ///
    /// `sealed` is one [`RevocationHandoff::sealed`] copy, opened with this
    /// device's own X25519 secret because both inbox generations are the wrong
    /// address for it. The roster inside is adopted through the same monotone
    /// writers an activation uses, so DL-1 ordering is enforced by the writers
    /// rather than restated here.
    ///
    /// A device that finds *itself* tombstoned adopts nothing — a revoked device
    /// is not a member of the fleet the document describes, so it has no fleet
    /// projection to write and no sync context to keep, and
    /// [`Self::adopt_own_roster`] would refuse the document anyway. What it does
    /// instead is **eject itself** ([`Self::eject_self_from_fleet`], §10 step 5):
    /// the burying roster is stored, the projection is cleared, and the
    /// activation gate goes to [`crate::CoreLinkActivationStage::Revoked`], which
    /// stops this device advertising, authoring and acking. `now_ms` stamps that
    /// transition. What the shell does with the answer (WP6: say so, offer to set
    /// up again under a fresh key per DL-4) is above this line.
    ///
    /// # The real acceptance rules run here, not a version comparison
    ///
    /// This used to decide with `incoming.version() <= held.version()`, which
    /// is DL-1 and only DL-1. Every other rule in
    /// [`core_roster_accept`] — DL-2's sticky fork quarantine, DL-4's
    /// "tombstones are forever", §6's inbox-generation floor, §14.2's "only
    /// the root raises the epoch" — was simply not consulted on this path.
    /// That is a hole shaped exactly like the threat model. A revoked device
    /// holds no sibling's device secret and so cannot mint a handoff — but it
    /// holds every byte it ever saw, and a *sibling's* genuine handoff
    /// replayed against a later state, or a document that quietly drops a
    /// burial the fleet has since made, is precisely what those rules exist to
    /// refuse. A stricter gate on the contact path than on this device's own
    /// fleet is the wrong way round.
    ///
    /// So the decision is [`core_roster_accept`]'s, run with the quarantine
    /// state this device has stored for its own person, and only
    /// [`RosterUpdateOutcome::Accepted`] adopts. Everything else comes back
    /// with the reason it was refused, and a fork records the sticky bit
    /// before returning.
    ///
    /// `superseded_inbox_key` is the key this device holds *now*, and passing
    /// it is what lets the adopting sibling re-seal its own retained backlog
    /// into the new generation — the same work the revoking device did for its
    /// own. Without it a sibling adopts the rotation and then answers every
    /// backfill request out of rows sealed under a key the rotation retired,
    /// which reads stale to [`crate::core_sync_seal_is_current`] and quietly
    /// stops flowing. `None` is honest when the device never held the key;
    /// nothing is touched then.
    pub fn adopt_revocation_handoff(
        &self,
        sealed: Vec<u8>,
        person_root_sign_pk: Vec<u8>,
        own_device: DeviceKeypair,
        superseded_inbox_key: Option<InboxKey>,
        now_ms: i64,
    ) -> Result<RevocationAdoption, CoreError> {
        let held = self.own_roster()?.ok_or_else(|| {
            CoreError::Store(
                "this device has no roster of its own, so it has no fleet to be revoked from"
                    .to_string(),
            )
        })?;
        let record = core_open_sync_handoff(
            sealed,
            own_device.agree_sk.clone(),
            held.clone(),
            person_root_sign_pk.clone(),
        )?;
        let payload = crate::core_decode_sync_own_roster(record.payload)?;
        let incoming = payload.roster;
        if incoming.person_id != held.person_id {
            return Err(CoreError::Malformed(
                "a rotation handoff names a different person".to_string(),
            ));
        }
        let stored_quarantined = {
            let conn = self.locked_conn();
            crate::roster_store::load_state(&conn, &held.person_id)?.quarantined
        };
        let decision = core_roster_accept(
            Some(held.clone()),
            stored_quarantined,
            incoming.clone(),
            person_root_sign_pk.clone(),
        );
        let revoked_device_ids = core_roster_newly_revoked(Some(held.clone()), incoming.clone());
        let refusal = |outcome, reason| RevocationAdoption {
            outcome,
            reason,
            revoked_device_ids: Vec::new(),
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
                return Ok(refusal(
                    RevocationAdoptionOutcome::ForkQuarantined,
                    decision.reason,
                ));
            }
            RosterUpdateOutcome::Ignored => {
                return Ok(refusal(
                    match decision.reason {
                        // DL-1's two ordinary silences, kept apart from a
                        // refusal a shell should react to.
                        RosterUpdateReason::Rollback | RosterUpdateReason::IdempotentRepeat => {
                            RevocationAdoptionOutcome::NotSuperseding
                        }
                        _ => RevocationAdoptionOutcome::Refused,
                    },
                    decision.reason,
                ));
            }
        }
        if incoming
            .tombstones
            .iter()
            .any(|tombstone| tombstone.device_id == own_device.device_id)
        {
            // §10 step 5. This arm used to return the outcome and write
            // nothing, which meant a device that had been handed proof of its
            // own burial went on advertising, authoring and acking exactly as
            // before. The same writer serves the notice path in
            // `device_link::activation`, so a device converges identically
            // however the roster reached it.
            self.eject_self_from_fleet(&incoming, &own_device.device_id, now_ms)?;
            return Ok(RevocationAdoption {
                outcome: RevocationAdoptionOutcome::RevokedSelf,
                reason: decision.reason,
                revoked_device_ids,
                inbox_key: None,
                resealed_records: 0,
                unresealable_records: 0,
            });
        }
        let inbox_key = payload
            .inbox_keys
            .into_iter()
            .find(|key| key.generation == incoming.inbox_key_generation)
            .ok_or_else(|| {
                CoreError::Malformed(
                    "a rotation handoff announced a generation it did not carry the key for"
                        .to_string(),
                )
            })?;
        // Same order as `commit_own_revocation`: the backlog is re-addressed
        // before the roster moves under it, so a crash halfway leaves rows
        // sealed for a roster this device has not adopted yet — stale to
        // `core_sync_seal_is_current`, sent by nobody, and finished by running
        // the same handoff again.
        let (resealed_records, unresealable_records) =
            self.reseal_own_backlog(&own_device, &superseded_inbox_key, &incoming, &inbox_key)?;
        self.adopt_own_roster(
            incoming.clone(),
            person_root_sign_pk,
            own_device.device_id.clone(),
        )?;
        self.core_set_own_sync_context(incoming, inbox_key.generation)?;
        Ok(RevocationAdoption {
            outcome: RevocationAdoptionOutcome::Adopted,
            reason: decision.reason,
            revoked_device_ids,
            inbox_key: Some(inbox_key),
            resealed_records,
            unresealable_records,
        })
    }
}

impl MessageStore {
    /// What both halves of the ceremony must agree about the update before
    /// either touches the store, in one place so they cannot drift.
    fn check_revocation_update(
        &self,
        update: &RevocationUpdate,
        person_root_sign_pk: &[u8],
        own_device: &DeviceKeypair,
    ) -> Result<(), CoreError> {
        if let Some(rejection) =
            core_roster_validate(update.roster.clone(), person_root_sign_pk.to_vec())
        {
            return Err(CoreError::Malformed(format!(
                "this revocation's roster is not acceptable: {rejection:?}"
            )));
        }
        if update.inbox_key.generation != update.roster.inbox_key_generation {
            return Err(CoreError::Malformed(
                "the rotated inbox key is not the generation this roster announces".to_string(),
            ));
        }
        if !update
            .roster
            .devices
            .iter()
            .any(|cert| cert.device_id() == own_device.device_id)
        {
            return Err(CoreError::Store(
                "this revocation's roster does not list this device".to_string(),
            ));
        }
        Ok(())
    }

    /// SYNC-3's "re-sealed on roster change", applied to everything this device
    /// has retained under the key being rotated away from.
    ///
    /// Each row is opened with the superseded key, re-stamped with the new
    /// roster version and generation, re-signed, re-sealed to the rotated key,
    /// and written back into the same slot. Re-signing is not optional: a sync
    /// record's signature covers its `roster_version` and `inbox_key_generation`,
    /// which is exactly what stops one from being replayed against a roster it
    /// was never authored for.
    fn reseal_own_backlog(
        &self,
        own_device: &DeviceKeypair,
        superseded_inbox_key: &Option<InboxKey>,
        roster: &Roster,
        rotated_inbox_key: &InboxKey,
    ) -> Result<(u32, u32), CoreError> {
        let Some(superseded) = superseded_inbox_key.clone() else {
            return Ok((0, 0));
        };
        if superseded.generation >= rotated_inbox_key.generation {
            return Err(CoreError::Malformed(
                "the superseded inbox key is not older than the rotated one".to_string(),
            ));
        }
        let stale = {
            let conn = self.locked_conn();
            crate::sync_store::sealed_records_at_generation(
                &conn,
                &own_device.device_id,
                superseded.generation,
            )?
        };
        let author = core_device_sync_identity(own_device.clone());
        let mut resealed = 0_u32;
        let mut unresealable = 0_u32;
        for stored in stale {
            let opened = match crate::crypto::open_sealed_with_agree_sk(
                &superseded.agree_sk,
                &stored.sealed,
            ) {
                Ok(opened) => opened,
                // Left in place, deliberately. See the caller's doc comment: a
                // rotation that deleted what it could not re-address would be a
                // rotation that loses mail.
                Err(_) => {
                    unresealable += 1;
                    continue;
                }
            };
            let Ok(record) = crate::core_decode_sync_record(opened.payload) else {
                unresealable += 1;
                continue;
            };
            let record = core_sign_sync_record(
                SyncRecord {
                    roster_version: roster.version(),
                    inbox_key_generation: rotated_inbox_key.generation,
                    signature: Vec::new(),
                    ..record
                },
                own_device.sign_sk.clone(),
            )?;
            let sealed =
                core_seal_sync_record(record.clone(), author.clone(), rotated_inbox_key.clone())?;
            let conn = self.locked_conn();
            if crate::sync_store::reseal(&conn, &record, &sealed)? {
                resealed += 1;
            }
        }
        Ok((resealed, unresealable))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The named ids as a deduplicated list, each checked to be an active device of
/// `roster`.
///
/// Revoking something the roster does not list is refused rather than ignored: a
/// "Remove device" that silently removed nothing would look identical to one
/// that worked, and the person would believe a phone had been cut off.
fn normalize_revoked(roster: &Roster, revoked: &[Vec<u8>]) -> Result<Vec<Vec<u8>>, CoreError> {
    if revoked.is_empty() {
        return Err(CoreError::Malformed(
            "a revocation must name at least one device".to_string(),
        ));
    }
    let active: Vec<Vec<u8>> = roster.devices.iter().map(DeviceCert::device_id).collect();
    let mut out: Vec<Vec<u8>> = Vec::with_capacity(revoked.len());
    for device_id in revoked {
        if device_id.len() != DEVICE_ID_LEN {
            return Err(CoreError::Malformed(format!(
                "a device id is {} bytes, not {DEVICE_ID_LEN}",
                device_id.len()
            )));
        }
        if !active.contains(device_id) {
            return Err(CoreError::Malformed(
                "this roster does not list the device being revoked".to_string(),
            ));
        }
        if !out.contains(device_id) {
            out.push(device_id.clone());
        }
    }
    Ok(out)
}

/// Raise one half of a roster's `(recovery_epoch, seq)` by one, or refuse.
///
/// `saturating_add` is the wrong primitive here and it took a security review
/// to say so. DL-1 orders rosters by `(recovery_epoch, seq)` and DL-2 calls two
/// *different* documents at one version a fork — so a counter that saturates
/// mints a document at the same version as the one it was meant to supersede,
/// with different content. That is not a clamped number, it is a fork this
/// device manufactured against itself, and DL-2 would quarantine the person for
/// it: every later roster refused, and the ceremony that produced it would look
/// like it had worked.
///
/// A `u64` reached honestly is not a thing that happens — one revocation per
/// millisecond for half a billion years — so the only way to arrive here is a
/// document that was crafted to. Refusing is the honest answer, and it is also
/// the safe one: nothing is written and the caller still holds the roster it
/// started from.
///
/// The receiving half of the same rule is
/// [`crate::device_roster::ROSTER_MAX_VERSION_JUMP`].
fn next_version_component(current: u64, field: &str) -> Result<u64, CoreError> {
    current.checked_add(1).ok_or_else(|| {
        CoreError::Malformed(format!(
            "this roster's {field} is at the maximum a u64 holds and cannot be raised; a \
             document that arrived there was crafted, not counted"
        ))
    })
}

/// §6/§10.1: the rotation itself, refusing a key that is not the one the roster
/// being superseded announced.
fn rotated_key(current: &Roster, current_inbox_key: InboxKey) -> Result<InboxKey, CoreError> {
    if current_inbox_key.generation != current.inbox_key_generation {
        return Err(CoreError::Malformed(format!(
            "this roster is at inbox key generation {} but the key offered is generation {}",
            current.inbox_key_generation, current_inbox_key.generation
        )));
    }
    Ok(core_rotate_inbox_key(current_inbox_key))
}

fn public_of(sign_sk: &[u8]) -> Result<Vec<u8>, CoreError> {
    Ok(crate::crypto::signing_key_from_bytes(sign_sk)?
        .verifying_key()
        .as_bytes()
        .to_vec())
}

/// Re-validate a document this module just built, exactly as
/// `device_link::activation` does: a roster minted here that a contact would
/// reject is a bug worth failing on now.
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
    use crate::device_link::activation::{
        core_link_genesis_roster, core_link_sign_new_device_roster,
    };
    use crate::device_roster::{
        core_roster_accept, generate_device_keypair, RosterUpdateOutcome, RosterUpdateReason,
    };
    use crate::identity::generate_identity;
    use crate::sync_record::{
        core_encode_sync_history, core_open_sync_record, core_sync_record_admit,
        core_sync_seal_is_current, SyncHistoryPayload, SyncRecordRejection,
    };
    use crate::{Contact, Identity};

    const NOW: i64 = 1_755_000_000_000;

    /// A person, their approving device, and one sibling — the smallest fleet a
    /// revocation can happen in, built through the shipped link ceremony so the
    /// rosters under test are the rosters WP3 really produces.
    struct Fleet {
        person: Identity,
        approver: DeviceKeypair,
        sibling: DeviceKeypair,
        roster: Roster,
        inbox_key: InboxKey,
    }

    fn fleet() -> Fleet {
        let person = generate_identity();
        let approver = generate_device_keypair();
        let sibling = generate_device_keypair();
        let genesis = core_link_genesis_roster(
            person.sign_sk.clone(),
            approver.sign_pk.clone(),
            approver.agree_pk.clone(),
        )
        .expect("genesis");
        let roster = core_link_sign_new_device_roster(
            genesis,
            person.sign_pk.clone(),
            approver.sign_sk.clone(),
            sibling.sign_pk.clone(),
            sibling.agree_pk.clone(),
        )
        .expect("link")
        .roster;
        let inbox_key = core_mint_inbox_key(roster.inbox_key_generation);
        Fleet {
            person,
            approver,
            sibling,
            roster,
            inbox_key,
        }
    }

    impl Fleet {
        fn revoke_sibling(&self) -> RevocationUpdate {
            core_revoke_devices_roster(
                self.roster.clone(),
                self.person.sign_pk.clone(),
                self.approver.sign_sk.clone(),
                vec![self.sibling.device_id.clone()],
                self.inbox_key.clone(),
            )
            .expect("revocation")
        }
    }

    // -----------------------------------------------------------------------
    // §10.1: the roster half
    // -----------------------------------------------------------------------

    #[test]
    fn revoking_buries_the_device_bumps_the_seq_and_rotates_the_key() {
        let fleet = fleet();
        let update = fleet.revoke_sibling();

        assert_eq!(update.path, RevocationPath::ApprovingDevice);
        assert_eq!(update.revoked_device_ids, vec![fleet.sibling.device_id]);
        assert_eq!(update.roster.seq, fleet.roster.seq + 1);
        assert_eq!(update.roster.recovery_epoch, fleet.roster.recovery_epoch);
        assert_eq!(update.roster.devices.len(), 1);
        assert_eq!(update.roster.tombstones.len(), 1);
        assert_eq!(
            update.roster.tombstones[0].revoked_at_seq,
            update.roster.seq
        );
        // §6: the generation moves AND the material behind it does. A generation
        // that climbed over the same key is a number the revoked device ignores.
        assert_eq!(
            update.roster.inbox_key_generation,
            fleet.roster.inbox_key_generation + 1
        );
        assert_eq!(
            update.inbox_key.generation,
            update.roster.inbox_key_generation
        );
        assert_ne!(update.inbox_key.agree_pk, fleet.inbox_key.agree_pk);
        assert_ne!(update.inbox_key.agree_sk, fleet.inbox_key.agree_sk);
        assert!(
            core_roster_validate(update.roster.clone(), fleet.person.sign_pk.clone()).is_none()
        );

        // DL-1 through the contact's eyes: this supersedes what they held.
        let decision = core_roster_accept(
            Some(fleet.roster),
            false,
            update.roster,
            fleet.person.sign_pk,
        );
        assert_eq!(decision.outcome, RosterUpdateOutcome::Accepted);
        assert_eq!(decision.reason, RosterUpdateReason::Superseded);
    }

    #[test]
    fn a_revoked_device_id_can_never_come_back_but_a_fresh_key_can() {
        let fleet = fleet();
        let update = fleet.revoke_sibling();

        // DL-4: the same hardware, the same key — refused.
        assert!(core_link_sign_new_device_roster(
            update.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            fleet.sibling.sign_pk.clone(),
            fleet.sibling.agree_pk.clone(),
        )
        .is_err());

        // DL-4's other half: the same hardware with a fresh key is a different
        // device id, and re-links normally.
        let relinked = generate_device_keypair();
        let relinked_roster = core_link_sign_new_device_roster(
            update.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            relinked.sign_pk,
            relinked.agree_pk,
        )
        .expect("a fresh key re-links")
        .roster;
        assert_eq!(relinked_roster.tombstones.len(), 1);
        let decision = core_roster_accept(
            Some(update.roster),
            false,
            relinked_roster,
            fleet.person.sign_pk,
        );
        assert_eq!(decision.outcome, RosterUpdateOutcome::Accepted);
    }

    #[test]
    fn the_approving_device_cannot_bury_itself_or_the_last_device() {
        let fleet = fleet();
        assert!(core_revoke_devices_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            vec![fleet.approver.device_id.clone()],
            fleet.inbox_key.clone(),
        )
        .is_err());
        // Both devices at once is the same lockout by another route.
        assert!(core_revoke_devices_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            vec![
                fleet.approver.device_id.clone(),
                fleet.sibling.device_id.clone(),
            ],
            fleet.inbox_key.clone(),
        )
        .is_err());
        // A device this roster never listed is refused rather than ignored.
        assert!(core_revoke_devices_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            vec![generate_device_keypair().device_id],
            fleet.inbox_key.clone(),
        )
        .is_err());
        // And so is a caller with no roster-signing role.
        assert!(core_revoke_devices_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.sibling.sign_sk.clone(),
            vec![fleet.approver.device_id.clone()],
            fleet.inbox_key,
        )
        .is_err());
    }

    #[test]
    fn a_key_from_the_wrong_generation_cannot_pretend_to_rotate() {
        let fleet = fleet();
        // Holding *a* key is not holding the current one. Refusing here is what
        // makes `inbox_key_generation` a claim about material rather than a
        // counter anybody can increment.
        assert!(core_revoke_devices_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            vec![fleet.sibling.device_id.clone()],
            core_mint_inbox_key(fleet.roster.inbox_key_generation + 7),
        )
        .is_err());
    }

    // -----------------------------------------------------------------------
    // §14.2: the override
    // -----------------------------------------------------------------------

    #[test]
    fn the_recovery_epoch_dethrones_a_stolen_approving_device() {
        let fleet = fleet();

        // The thief holds the approving device and uses it exactly as the real
        // owner would: a perfectly valid roster at seq + 1 that buries the
        // owner's other phone. Contacts accept it, because it IS validly signed.
        let stolen = fleet.revoke_sibling();
        let contact_holds = stolen.roster.clone();
        assert_eq!(
            core_roster_accept(
                Some(fleet.roster.clone()),
                false,
                contact_holds.clone(),
                fleet.person.sign_pk.clone()
            )
            .outcome,
            RosterUpdateOutcome::Accepted
        );

        // The thief cannot climb the epoch: a device key alone never mints one.
        let forged = core_sign_roster(
            Roster {
                recovery_epoch: contact_holds.recovery_epoch + 1,
                seq: contact_holds.seq + 1,
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
                ..contact_holds.clone()
            },
            fleet.approver.sign_sk.clone(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_accept(
                Some(contact_holds.clone()),
                false,
                forged,
                fleet.person.sign_pk.clone()
            )
            .reason,
            RosterUpdateReason::RecoveryEpochRequiresRoot
        );

        // The owner opens the `.cmbak`, and the root secret inside signs a
        // roster at the next epoch that buries the stolen phone.
        let rescue = generate_device_keypair();
        let recovery = core_recovery_revoke_roster(
            contact_holds.clone(),
            fleet.person.sign_sk.clone(),
            rescue.sign_pk.clone(),
            rescue.agree_pk.clone(),
            vec![fleet.approver.device_id.clone()],
            None,
        )
        .expect("recovery revocation");

        assert_eq!(recovery.path, RevocationPath::RecoveryEpoch);
        assert_eq!(
            recovery.roster.recovery_epoch,
            contact_holds.recovery_epoch + 1
        );
        assert_eq!(recovery.roster.seq, 0);
        assert_eq!(recovery.roster.approving_device_id, rescue.device_id);
        assert_eq!(recovery.roster.signer_sign_pk, fleet.person.sign_pk);
        assert!(recovery
            .roster
            .tombstones
            .iter()
            .any(|t| t.device_id == fleet.approver.device_id));
        // DL-4 is carried forward, including the burial the thief performed.
        assert!(recovery
            .roster
            .tombstones
            .iter()
            .any(|t| t.device_id == fleet.sibling.device_id));
        assert!(recovery.roster.inbox_key_generation > contact_holds.inbox_key_generation);
        assert_eq!(
            recovery.inbox_key.generation,
            recovery.roster.inbox_key_generation
        );

        // The contact takes it, and from here the thief's next roster is a
        // rollback rather than an update.
        assert_eq!(
            core_roster_accept(
                Some(contact_holds.clone()),
                false,
                recovery.roster.clone(),
                fleet.person.sign_pk.clone()
            )
            .outcome,
            RosterUpdateOutcome::Accepted
        );
        let thief_carries_on = core_sign_roster(
            Roster {
                seq: contact_holds.seq + 1,
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
                ..contact_holds
            },
            fleet.approver.sign_sk.clone(),
        )
        .expect("signs");
        assert_eq!(
            core_roster_accept(
                Some(recovery.roster),
                false,
                thief_carries_on,
                fleet.person.sign_pk
            )
            .reason,
            RosterUpdateReason::Rollback
        );
    }

    #[test]
    fn recovery_re_signs_the_survivors_the_buried_approver_had_vouched_for() {
        let fleet = fleet();
        // The sibling's certificate was signed by the approving device. Burying
        // that device orphans it, and a roster full of orphans is one every
        // contact rejects as `ChainBroken` — so the recovery path re-signs.
        assert_eq!(
            fleet.roster.devices[1].signer_sign_pk,
            fleet.approver.sign_pk
        );
        let rescue = generate_device_keypair();
        let recovery = core_recovery_revoke_roster(
            fleet.roster.clone(),
            fleet.person.sign_sk.clone(),
            rescue.sign_pk.clone(),
            rescue.agree_pk,
            vec![fleet.approver.device_id.clone()],
            Some(fleet.inbox_key.clone()),
        )
        .expect("recovery revocation");

        assert!(
            core_roster_validate(recovery.roster.clone(), fleet.person.sign_pk.clone()).is_none()
        );
        // The sibling survived, under a certificate the root now vouches for.
        let survivor = recovery
            .roster
            .devices
            .iter()
            .find(|cert| cert.device_id() == fleet.sibling.device_id)
            .expect("the sibling survived the recovery");
        assert_eq!(survivor.signer_sign_pk, fleet.person.sign_pk);
        // Exactly one device holds the role, and it is the one with the backup.
        assert_eq!(recovery.roster.approving_device_id, rescue.device_id);
        assert_eq!(
            survivor.flags & DEVICE_CERT_FLAG_ROSTER_SIGNING,
            0,
            "the role does not stay with a device the recovery did not name"
        );
        // The rotation is real on this path too.
        assert_ne!(recovery.inbox_key.agree_sk, fleet.inbox_key.agree_sk);
    }

    #[test]
    fn recovery_refuses_to_land_on_a_device_id_that_was_buried() {
        let fleet = fleet();
        let update = fleet.revoke_sibling();
        // DL-4: recovering onto the phone that was just revoked would produce a
        // document every contact refuses as `TombstonedDeviceActive`.
        assert!(core_recovery_revoke_roster(
            update.roster,
            fleet.person.sign_sk.clone(),
            fleet.sibling.sign_pk.clone(),
            fleet.sibling.agree_pk.clone(),
            vec![fleet.approver.device_id.clone()],
            None,
        )
        .is_err());
    }

    #[test]
    fn a_recovery_built_from_a_stale_roster_is_refused_by_a_contact_that_saw_the_burial() {
        // The residual `core_recovery_revoke_roster` documents, pinned so a
        // future change to it is deliberate. DL-4 does not bend for the root: a
        // higher epoch cannot un-bury, so a recovery that never learned of a
        // burial is not a later version of the roster that made it.
        let fleet = fleet();
        let thief_buried_the_sibling = fleet.revoke_sibling().roster;

        // The owner recovers from the BACKUP's snapshot, which predates that.
        let rescue = generate_device_keypair();
        let from_the_backup = core_recovery_revoke_roster(
            fleet.roster.clone(),
            fleet.person.sign_sk.clone(),
            rescue.sign_pk.clone(),
            rescue.agree_pk.clone(),
            vec![fleet.approver.device_id.clone()],
            None,
        )
        .expect("recovery revocation");
        assert!(from_the_backup.roster.recovery_epoch > thief_buried_the_sibling.recovery_epoch);
        assert_eq!(
            core_roster_accept(
                Some(thief_buried_the_sibling.clone()),
                false,
                from_the_backup.roster,
                fleet.person.sign_pk.clone(),
            )
            .reason,
            RosterUpdateReason::TombstoneResurrected
        );

        // Built from the newest roster in hand, the same recovery is accepted.
        let from_the_latest = core_recovery_revoke_roster(
            thief_buried_the_sibling.clone(),
            fleet.person.sign_sk.clone(),
            rescue.sign_pk,
            rescue.agree_pk,
            vec![fleet.approver.device_id.clone()],
            None,
        )
        .expect("recovery revocation");
        assert_eq!(
            core_roster_accept(
                Some(thief_buried_the_sibling),
                false,
                from_the_latest.roster,
                fleet.person.sign_pk,
            )
            .outcome,
            RosterUpdateOutcome::Accepted
        );
    }

    #[test]
    fn newly_revoked_names_only_what_changed() {
        let fleet = fleet();
        let update = fleet.revoke_sibling();
        assert_eq!(
            core_roster_newly_revoked(Some(fleet.roster.clone()), update.roster.clone()),
            vec![fleet.sibling.device_id.clone()]
        );
        // Idempotent gossip is not news.
        assert!(
            core_roster_newly_revoked(Some(update.roster.clone()), update.roster.clone())
                .is_empty()
        );
        // A first roster's burials are all news.
        assert_eq!(
            core_roster_newly_revoked(None, update.roster),
            vec![fleet.sibling.device_id]
        );
    }

    // -----------------------------------------------------------------------
    // The store side
    // -----------------------------------------------------------------------

    /// §10.1 as a shell really performs it: begin (which hands over the
    /// rotated key), persist it, then commit. Every test below goes through
    /// this, so the durable-key precondition is exercised by all of them
    /// rather than by one.
    fn commit(
        store: &MessageStore,
        fleet: &Fleet,
        update: &RevocationUpdate,
        own: &DeviceKeypair,
        superseded: Option<InboxKey>,
    ) -> RevocationCommit {
        let handed_over = store
            .begin_own_revocation(
                update.clone(),
                fleet.person.sign_pk.clone(),
                own.clone(),
                NOW,
            )
            .expect("begin");
        assert_eq!(handed_over, update.inbox_key, "the key the shell stores");
        store
            .commit_own_revocation(
                update.clone(),
                fleet.person.sign_pk.clone(),
                own.clone(),
                superseded,
                NOW,
            )
            .expect("commit")
    }

    /// The same fleet plus the phone that gets stolen — three devices, which
    /// is the smallest fleet where a revocation still leaves somebody to hand
    /// the rotated key to.
    fn three_device_fleet() -> Fleet {
        let fleet = fleet();
        let stolen = generate_device_keypair();
        let roster = core_link_sign_new_device_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            stolen.sign_pk,
            stolen.agree_pk,
        )
        .expect("link")
        .roster;
        Fleet { roster, ..fleet }
    }

    /// Hand `store` a rotation announcement carrying `roster`, sealed to the
    /// sibling's device key against `addressed_against`, and return what the
    /// acceptance rules decided. The record is genuine in every respect the
    /// crypto can check, so what the test is measuring is the ROSTER rules.
    fn offer_handoff(
        store: &MessageStore,
        fleet: &Fleet,
        roster: &Roster,
        inbox_key: InboxKey,
        addressed_against: &Roster,
    ) -> RevocationAdoption {
        let record = core_sign_sync_record(
            SyncRecord {
                kind: SyncRecordKind::OwnRoster,
                person_id: fleet.person.user_id.clone(),
                author_device_id: Vec::new(),
                roster_version: roster.version(),
                inbox_key_generation: roster.inbox_key_generation,
                stream_seq: 7,
                timestamp_ms: NOW,
                payload: core_encode_sync_own_roster(SyncOwnRosterPayload {
                    roster: roster.clone(),
                    inbox_keys: vec![inbox_key],
                })
                .expect("payload"),
                signature: Vec::new(),
            },
            fleet.approver.sign_sk.clone(),
        )
        .expect("signs");
        let sealed = core_seal_sync_handoff(
            record,
            core_device_sync_identity(fleet.approver.clone()),
            addressed_against.clone(),
            fleet.sibling.device_id.clone(),
        )
        .expect("seals");
        store
            .adopt_revocation_handoff(
                sealed.sealed,
                fleet.person.sign_pk.clone(),
                fleet.sibling.clone(),
                None,
                NOW,
            )
            .expect("decides")
    }

    fn store_for(fleet: &Fleet, own: &DeviceKeypair) -> MessageStore {
        let store = MessageStore::open(":memory:".to_string()).expect("open");
        store
            .adopt_own_roster(
                fleet.roster.clone(),
                fleet.person.sign_pk.clone(),
                own.device_id.clone(),
            )
            .expect("adopt");
        store
            .core_set_own_sync_context(fleet.roster.clone(), fleet.roster.inbox_key_generation)
            .expect("sync context");
        store
    }

    fn a_contact() -> Contact {
        let other = generate_identity();
        Contact {
            user_id: other.user_id,
            name: "Bob".to_string(),
            sign_pk: other.sign_pk,
            agree_pk: other.agree_pk,
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    #[test]
    fn committing_a_revocation_narrows_the_fleet_and_moves_the_sync_context() {
        let fleet = fleet();
        let store = store_for(&fleet, &fleet.approver);
        let contact = a_contact();
        store.upsert_contact(contact.clone()).expect("contact");

        let update = fleet.revoke_sibling();
        let commit = commit(
            &store,
            &fleet,
            &update,
            &fleet.approver,
            Some(fleet.inbox_key.clone()),
        );

        assert_eq!(
            commit.revoked_device_ids,
            vec![fleet.sibling.device_id.clone()]
        );
        assert_eq!(commit.roster, update.roster);
        assert_eq!(commit.inbox_key_generation, update.inbox_key.generation);
        // The projection routing and acks read no longer names the buried device.
        let projected = store.own_device_fleet().expect("fleet");
        assert_eq!(
            projected.own_device_id,
            Some(fleet.approver.device_id.clone())
        );
        assert_eq!(projected.device_ids, vec![fleet.approver.device_id.clone()]);
        // The inbound gate is pointed at the new roster and the new generation.
        let context = store
            .core_own_sync_context()
            .expect("context")
            .expect("some");
        assert_eq!(context.roster, update.roster);
        assert_eq!(context.inbox_key_generation, update.inbox_key.generation);
        // DL-3's contact leg: exactly who to tell, and the document to tell them.
        assert_eq!(commit.contact_user_ids, vec![contact.user_id]);
        assert_eq!(
            crate::core_decode_roster(commit.roster_document).expect("decodes"),
            update.roster
        );
        // No sibling survives this particular revocation, so there is nobody to
        // hand the rotated key to.
        assert!(commit.handoffs.is_empty());
    }

    #[test]
    fn a_revoked_device_stops_being_admitted_while_its_history_stays() {
        let fleet = fleet();
        let store = store_for(&fleet, &fleet.approver);
        let update = fleet.revoke_sibling();
        commit(
            &store,
            &fleet,
            &update,
            &fleet.approver,
            Some(fleet.inbox_key.clone()),
        );

        // §10.3: a NEWLY received event signed by the buried device is refused.
        let record = core_sign_sync_record(
            SyncRecord {
                kind: SyncRecordKind::Watermarks,
                person_id: fleet.person.user_id.clone(),
                author_device_id: Vec::new(),
                roster_version: update.roster.version(),
                inbox_key_generation: update.inbox_key.generation,
                stream_seq: 1,
                timestamp_ms: NOW,
                payload: crate::core_encode_sync_watermarks(crate::SyncWatermarkPayload {
                    entries: Vec::new(),
                })
                .expect("payload"),
                signature: Vec::new(),
            },
            fleet.sibling.sign_sk.clone(),
        )
        .expect("signs");
        assert_eq!(
            core_sync_record_admit(
                record,
                update.inbox_key.generation,
                store.core_own_sync_context().unwrap().unwrap().roster,
            ),
            Some(SyncRecordRejection::RevokedAuthorDevice)
        );
    }

    #[test]
    fn rotation_re_seals_the_backlog_in_place_and_loses_nothing() {
        let fleet = fleet();
        let store = store_for(&fleet, &fleet.approver);

        // One record already on this device's history stream, sealed under the
        // key the revocation is about to rotate away from.
        let stream_seq = store
            .core_sync_next_stream_seq(fleet.approver.device_id.clone(), SyncRecordKind::History)
            .expect("seq");
        let before = core_sign_sync_record(
            SyncRecord {
                kind: SyncRecordKind::History,
                person_id: fleet.person.user_id.clone(),
                author_device_id: Vec::new(),
                roster_version: fleet.roster.version(),
                inbox_key_generation: fleet.inbox_key.generation,
                stream_seq,
                timestamp_ms: NOW,
                payload: core_encode_sync_history(SyncHistoryPayload {
                    entries: Vec::new(),
                })
                .expect("payload"),
                signature: Vec::new(),
            },
            fleet.approver.sign_sk.clone(),
        )
        .expect("signs");
        let sealed = core_seal_sync_record(
            before.clone(),
            core_device_sync_identity(fleet.approver.clone()),
            fleet.inbox_key.clone(),
        )
        .expect("seals");
        store
            .core_sync_retain_record(before.clone(), sealed, NOW)
            .expect("retain");

        let update = fleet.revoke_sibling();
        let commit = commit(
            &store,
            &fleet,
            &update,
            &fleet.approver,
            Some(fleet.inbox_key.clone()),
        );
        assert_eq!(commit.resealed_records, 1);
        assert_eq!(commit.unresealable_records, 0);

        // Rotate, then drain: the slot never moved, so nothing a sibling had not
        // fetched was lost — and the bytes now open under the NEW key alone.
        let gaps = vec![crate::SyncGap {
            author_device_id: fleet.approver.device_id.clone(),
            kind: crate::core_sync_record_kind_wire(SyncRecordKind::History),
            after_seq: stream_seq - 1,
            through_seq: stream_seq,
        }];
        let offered = store
            .core_sync_backfill_records(gaps, 10)
            .expect("backfill");
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].stream_seq, stream_seq);
        assert!(core_sync_seal_is_current(
            offered[0].sealed_for,
            offered[0].inbox_key_generation,
            update.roster.version(),
            update.inbox_key.generation,
        ));
        let reopened = core_open_sync_record(
            offered[0].sealed.clone(),
            update.inbox_key.clone(),
            update.roster.clone(),
        )
        .expect("opens under the rotated key");
        assert_eq!(reopened.stream_seq, stream_seq);
        assert_eq!(reopened.kind, SyncRecordKind::History);
        // The revoked device kept the old key and every byte it ever saw. It is
        // exactly as useless as the rotation intends.
        assert!(core_open_sync_record(
            offered[0].sealed.clone(),
            fleet.inbox_key.clone(),
            update.roster,
        )
        .is_err());
    }

    #[test]
    fn the_rotation_handoff_reaches_a_sibling_and_not_the_device_being_buried() {
        // Three devices: the approver, the sibling that survives, and the phone
        // that is being cut off.
        let fleet = fleet();
        let stolen = generate_device_keypair();
        let roster = core_link_sign_new_device_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            stolen.sign_pk.clone(),
            stolen.agree_pk.clone(),
        )
        .expect("link")
        .roster;
        let fleet = Fleet { roster, ..fleet };
        let store = store_for(&fleet, &fleet.approver);

        let update = core_revoke_devices_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            vec![stolen.device_id.clone()],
            fleet.inbox_key.clone(),
        )
        .expect("revocation");
        let commit = commit(
            &store,
            &fleet,
            &update,
            &fleet.approver,
            Some(fleet.inbox_key.clone()),
        );

        assert_eq!(commit.handoffs.len(), 1);
        let handoff = &commit.handoffs[0];
        assert_eq!(handoff.device_id, fleet.sibling.device_id);

        // The buried device holds the OLD inbox key and its own device key.
        // Neither opens the announcement, which is the whole point of sealing it
        // to a sibling's device key rather than to any inbox generation.
        assert!(core_open_sync_handoff(
            handoff.sealed.sealed.clone(),
            stolen.agree_sk.clone(),
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
        )
        .is_err());
        assert!(core_open_sync_record(
            handoff.sealed.sealed.clone(),
            fleet.inbox_key.clone(),
            fleet.roster.clone(),
        )
        .is_err());

        // The sibling — still holding the PRE-revocation roster and the old key —
        // opens it, adopts it, and comes away with the rotated key.
        let sibling_store = store_for(&fleet, &fleet.sibling);
        let adoption = sibling_store
            .adopt_revocation_handoff(
                handoff.sealed.sealed.clone(),
                fleet.person.sign_pk.clone(),
                fleet.sibling.clone(),
                Some(fleet.inbox_key.clone()),
                NOW,
            )
            .expect("adopts");
        assert_eq!(adoption.outcome, RevocationAdoptionOutcome::Adopted);
        assert_eq!(adoption.revoked_device_ids, vec![stolen.device_id.clone()]);
        assert_eq!(adoption.inbox_key, Some(update.inbox_key.clone()));
        assert_eq!(
            sibling_store.own_roster().unwrap().unwrap(),
            update.roster.clone()
        );
        assert_eq!(
            sibling_store
                .core_own_sync_context()
                .unwrap()
                .unwrap()
                .inbox_key_generation,
            update.inbox_key.generation
        );
        // Idempotent gossip changes nothing.
        assert_eq!(
            sibling_store
                .adopt_revocation_handoff(
                    handoff.sealed.sealed.clone(),
                    fleet.person.sign_pk.clone(),
                    fleet.sibling.clone(),
                    None,
                    NOW,
                )
                .expect("re-offered")
                .outcome,
            RevocationAdoptionOutcome::NotSuperseding
        );
    }

    /// §10 step 5, through the sealed-handoff door. The one thing this test
    /// used to assert — "it keeps the roster it had" — was the bug: a device
    /// handed proof of its own burial wrote nothing at all and went on
    /// advertising, authoring and acking. It now ejects itself.
    #[test]
    fn a_device_that_finds_itself_buried_ejects_itself() {
        let fleet = fleet();
        let store = store_for(&fleet, &fleet.approver);
        let update = fleet.revoke_sibling();
        let commit = commit(
            &store,
            &fleet,
            &update,
            &fleet.approver,
            Some(fleet.inbox_key.clone()),
        );
        assert!(commit.handoffs.is_empty());

        // Nothing was addressed to the buried device, so hand it the most
        // generous thing an attacker could arrange — the announcement sealed to
        // its own device key — and it still adopts nothing.
        let record = core_sign_sync_record(
            SyncRecord {
                kind: SyncRecordKind::OwnRoster,
                person_id: fleet.person.user_id.clone(),
                author_device_id: Vec::new(),
                roster_version: update.roster.version(),
                inbox_key_generation: update.inbox_key.generation,
                stream_seq: 1,
                timestamp_ms: NOW,
                payload: core_encode_sync_own_roster(SyncOwnRosterPayload {
                    roster: update.roster.clone(),
                    inbox_keys: vec![update.inbox_key.clone()],
                })
                .expect("payload"),
                signature: Vec::new(),
            },
            fleet.approver.sign_sk.clone(),
        )
        .expect("signs");
        // Against the post-revocation roster this is structurally impossible --
        // `core_seal_sync_handoff` will not address a tombstoned device -- so
        // the seal is made against the PRE-revocation roster, which is the best
        // an attacker replaying old state could manage.
        assert!(core_seal_sync_handoff(
            record.clone(),
            core_device_sync_identity(fleet.approver.clone()),
            update.roster.clone(),
            fleet.sibling.device_id.clone(),
        )
        .is_err());
        let sealed = core_seal_sync_handoff(
            record,
            core_device_sync_identity(fleet.approver.clone()),
            fleet.roster.clone(),
            fleet.sibling.device_id.clone(),
        )
        .expect("seals");

        let buried_store = store_for(&fleet, &fleet.sibling);
        let adoption = buried_store
            .adopt_revocation_handoff(
                sealed.sealed,
                fleet.person.sign_pk.clone(),
                fleet.sibling.clone(),
                None,
                NOW,
            )
            .expect("reads its own eviction notice");
        assert_eq!(adoption.outcome, RevocationAdoptionOutcome::RevokedSelf);
        assert_eq!(
            adoption.revoked_device_ids,
            vec![fleet.sibling.device_id.clone()]
        );
        assert_eq!(adoption.inbox_key, None);
        // No fleet is ADOPTED — a device does not join a fleet it is not in,
        // and nothing hands it the rotated key. What it does instead is write
        // the burial down and go quiet.
        assert_eq!(
            buried_store.own_roster().unwrap().unwrap(),
            update.roster,
            "the burying roster is stored, so the phone stops reporting a fleet it is not in"
        );
        assert_eq!(
            buried_store.own_device_fleet().unwrap(),
            crate::OwnDeviceFleet {
                own_device_id: None,
                device_ids: Vec::new(),
                projected_from: update.roster.version(),
            },
            "the projection routing and acks read is cleared, at the burying roster's version"
        );
        let activation = buried_store.link_activation().unwrap();
        assert_eq!(
            activation.stage,
            crate::CoreLinkActivationStage::Revoked,
            "the gate is shut"
        );
        assert_eq!(
            activation.own_device_id,
            Some(fleet.sibling.device_id.clone()),
            "a surface has to be able to say which device this was"
        );
        for action in [
            crate::CoreLinkGatedAction::Advertise,
            crate::CoreLinkGatedAction::Author,
            crate::CoreLinkGatedAction::Ack,
        ] {
            let verdict = buried_store.link_gate(action).unwrap();
            assert!(!verdict.allowed, "a removed device may not {action:?}");
            assert_eq!(verdict.reason, crate::CoreLinkGateReason::DeviceRevoked);
        }
        // DL-4: the way back is a fresh install under a fresh device key, not a
        // local reset of the state the person's decision put this device in.
        assert!(buried_store
            .begin_link_activation(vec![0x5C; 32], NOW)
            .is_err());
        assert!(buried_store.abandon_link_activation(NOW).is_err());
    }

    /// The handoff path runs [`core_roster_accept`], not a version
    /// comparison, so every acceptance rule that guards the contact path
    /// guards this one too. The two a replay can actually reach are pinned
    /// here.
    #[test]
    fn the_handoff_path_refuses_what_the_acceptance_rules_refuse() {
        let fleet = three_device_fleet();
        let stolen = fleet.roster.devices[2].device_id();
        let store = store_for(&fleet, &fleet.approver);
        let update = core_revoke_devices_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            vec![stolen.clone()],
            fleet.inbox_key.clone(),
        )
        .expect("revocation");
        let committed = commit(
            &store,
            &fleet,
            &update,
            &fleet.approver,
            Some(fleet.inbox_key.clone()),
        );

        // The sibling adopts the real revocation first, so it now holds a
        // roster with a burial in it.
        let sibling_store = store_for(&fleet, &fleet.sibling);
        assert_eq!(
            sibling_store
                .adopt_revocation_handoff(
                    committed.handoffs[0].sealed.sealed.clone(),
                    fleet.person.sign_pk.clone(),
                    fleet.sibling.clone(),
                    Some(fleet.inbox_key.clone()),
                    NOW,
                )
                .expect("adopts")
                .outcome,
            RevocationAdoptionOutcome::Adopted
        );

        // DL-4 through the handoff door: a genuine, correctly signed, strictly
        // HIGHER roster that quietly forgets the burial. A version comparison
        // alone would have adopted it; the acceptance rules refuse it.
        let forgetful = core_sign_roster(
            Roster {
                seq: update.roster.seq + 1,
                tombstones: Vec::new(),
                devices: fleet.roster.devices.clone(),
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
                ..update.roster.clone()
            },
            fleet.approver.sign_sk.clone(),
        )
        .expect("signs");
        let refused = offer_handoff(
            &sibling_store,
            &fleet,
            &forgetful,
            core_mint_inbox_key(forgetful.inbox_key_generation),
            &update.roster,
        );
        assert_eq!(refused.outcome, RevocationAdoptionOutcome::Refused);
        assert_eq!(refused.reason, RosterUpdateReason::TombstoneResurrected);
        assert_eq!(
            sibling_store.own_roster().unwrap().unwrap(),
            update.roster,
            "the sibling keeps the roster that buried the phone"
        );

        // DL-2 through the same door: two different documents at one version.
        let forked = core_sign_roster(
            Roster {
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
                tombstones: vec![
                    update.roster.tombstones[0].clone(),
                    DeviceTombstone {
                        device_id: fleet.sibling.device_id.clone(),
                        revoked_at_seq: update.roster.seq,
                    },
                ],
                devices: vec![update.roster.devices[0].clone()],
                ..update.roster.clone()
            },
            fleet.approver.sign_sk.clone(),
        )
        .expect("signs");
        let forked_adoption = offer_handoff(
            &sibling_store,
            &fleet,
            &forked,
            update.inbox_key.clone(),
            &update.roster,
        );
        assert_eq!(
            forked_adoption.outcome,
            RevocationAdoptionOutcome::ForkQuarantined
        );
        assert_eq!(forked_adoption.reason, RosterUpdateReason::ForkedContent);
        // DL-2's sticky bit is recorded even though this person's roster lives
        // in `own_roster` rather than in the contact roster table.
        assert!(
            sibling_store
                .contact_roster_state(fleet.person.user_id.clone())
                .expect("state")
                .quarantined
        );
        // And a person, never arithmetic, is what clears it.
        assert!(sibling_store
            .clear_roster_quarantine(fleet.person.user_id.clone())
            .expect("cleared"));
        assert!(!sibling_store
            .clear_roster_quarantine(fleet.person.user_id)
            .expect("already clear"));
    }

    /// §10.1's normal case, which the ceremony's one-shot handoffs do not
    /// serve at all: the other phone was in a bag when the person tapped
    /// "Remove device". It has to be able to complete later.
    #[test]
    fn a_sibling_that_missed_the_ceremony_adopts_from_a_re_issued_handoff() {
        let fleet = three_device_fleet();
        let stolen = fleet.roster.devices[2].device_id();
        let store = store_for(&fleet, &fleet.approver);
        let update = core_revoke_devices_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            vec![stolen.clone()],
            fleet.inbox_key.clone(),
        )
        .expect("revocation");
        // The ceremony happens with nobody around to receive it: the copies it
        // produced are dropped on the floor, exactly as they are when no
        // transport can reach the sibling.
        let _discarded = commit(
            &store,
            &fleet,
            &update,
            &fleet.approver,
            Some(fleet.inbox_key.clone()),
        );

        // Days later the sibling turns up, and the announcement is re-sealed
        // for it out of the retained record.
        let reissued = store
            .revocation_handoffs_for(
                fleet.sibling.device_id.clone(),
                fleet.approver.clone(),
                update.inbox_key.clone(),
            )
            .expect("re-issue");
        assert_eq!(reissued.len(), 1);
        assert_eq!(reissued[0].device_id, fleet.sibling.device_id);

        let sibling_store = store_for(&fleet, &fleet.sibling);
        let adoption = sibling_store
            .adopt_revocation_handoff(
                reissued[0].sealed.sealed.clone(),
                fleet.person.sign_pk.clone(),
                fleet.sibling.clone(),
                Some(fleet.inbox_key.clone()),
                NOW,
            )
            .expect("adopts");
        assert_eq!(adoption.outcome, RevocationAdoptionOutcome::Adopted);
        assert_eq!(adoption.inbox_key, Some(update.inbox_key.clone()));
        assert_eq!(sibling_store.own_roster().unwrap().unwrap(), update.roster);

        // The device being buried is never an address, however it asks.
        assert!(store
            .revocation_handoffs_for(stolen, fleet.approver.clone(), update.inbox_key.clone(),)
            .expect("re-issue")
            .is_empty());
        // Nor is a stranger.
        assert!(store
            .revocation_handoffs_for(
                generate_device_keypair().device_id,
                fleet.approver.clone(),
                update.inbox_key,
            )
            .expect("re-issue")
            .is_empty());
        // A store that never revoked anything has nothing to re-issue.
        assert!(store_for(&fleet, &fleet.approver)
            .revocation_handoffs_for(
                fleet.sibling.device_id.clone(),
                fleet.approver.clone(),
                fleet.inbox_key,
            )
            .expect("re-issue")
            .is_empty());
    }

    /// The adopting sibling re-seals its OWN retained backlog, not only the
    /// revoking device's. Without it a sibling adopts the rotation and then
    /// answers every backfill request out of rows sealed under a key the
    /// rotation retired -- stale to `core_sync_seal_is_current`, so they
    /// quietly stop flowing.
    #[test]
    fn the_adopting_sibling_re_seals_its_own_backlog_too() {
        let fleet = three_device_fleet();
        let stolen = fleet.roster.devices[2].device_id();
        let store = store_for(&fleet, &fleet.approver);
        let update = core_revoke_devices_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            vec![stolen],
            fleet.inbox_key.clone(),
        )
        .expect("revocation");
        let committed = commit(
            &store,
            &fleet,
            &update,
            &fleet.approver,
            Some(fleet.inbox_key.clone()),
        );

        // One record of the sibling's own, sealed under the key the revocation
        // is about to rotate away from.
        let sibling_store = store_for(&fleet, &fleet.sibling);
        let stream_seq = sibling_store
            .core_sync_next_stream_seq(fleet.sibling.device_id.clone(), SyncRecordKind::History)
            .expect("seq");
        let before = core_sign_sync_record(
            SyncRecord {
                kind: SyncRecordKind::History,
                person_id: fleet.person.user_id.clone(),
                author_device_id: Vec::new(),
                roster_version: fleet.roster.version(),
                inbox_key_generation: fleet.inbox_key.generation,
                stream_seq,
                timestamp_ms: NOW,
                payload: core_encode_sync_history(SyncHistoryPayload {
                    entries: Vec::new(),
                })
                .expect("payload"),
                signature: Vec::new(),
            },
            fleet.sibling.sign_sk.clone(),
        )
        .expect("signs");
        let sealed = core_seal_sync_record(
            before.clone(),
            core_device_sync_identity(fleet.sibling.clone()),
            fleet.inbox_key.clone(),
        )
        .expect("seals");
        sibling_store
            .core_sync_retain_record(before, sealed, NOW)
            .expect("retain");

        let adoption = sibling_store
            .adopt_revocation_handoff(
                committed.handoffs[0].sealed.sealed.clone(),
                fleet.person.sign_pk.clone(),
                fleet.sibling.clone(),
                Some(fleet.inbox_key.clone()),
                NOW,
            )
            .expect("adopts");
        assert_eq!(adoption.outcome, RevocationAdoptionOutcome::Adopted);
        assert_eq!(adoption.resealed_records, 1);
        assert_eq!(adoption.unresealable_records, 0);

        // The slot never moved, and the bytes now open under the ROTATED key
        // alone -- so the sibling's backlog is still offerable, and the buried
        // phone's copy of the old key opens none of it.
        let offered = sibling_store
            .core_sync_backfill_records(
                vec![crate::SyncGap {
                    author_device_id: fleet.sibling.device_id.clone(),
                    kind: crate::core_sync_record_kind_wire(SyncRecordKind::History),
                    after_seq: stream_seq - 1,
                    through_seq: stream_seq,
                }],
                10,
            )
            .expect("backfill");
        assert_eq!(offered.len(), 1);
        assert!(core_sync_seal_is_current(
            offered[0].sealed_for,
            offered[0].inbox_key_generation,
            update.roster.version(),
            update.inbox_key.generation,
        ));
        assert!(core_open_sync_record(
            offered[0].sealed.clone(),
            update.inbox_key.clone(),
            update.roster.clone(),
        )
        .is_ok());
        assert!(
            core_open_sync_record(offered[0].sealed.clone(), fleet.inbox_key, update.roster)
                .is_err()
        );
    }

    /// The durable-key precondition, and the crash it exists for.
    #[test]
    fn a_crash_between_storing_the_key_and_committing_is_recoverable() {
        let fleet = fleet();
        let store = store_for(&fleet, &fleet.approver);
        let update = fleet.revoke_sibling();

        // Committing without having written the rotation down is refused
        // outright: step (1) re-seals the whole backlog TO the rotated key, so
        // a crash after it with the key never persisted would leave every row
        // addressed to a secret that exists nowhere.
        assert!(store
            .commit_own_revocation(
                update.clone(),
                fleet.person.sign_pk.clone(),
                fleet.approver.clone(),
                Some(fleet.inbox_key.clone()),
                NOW,
            )
            .is_err());

        let handed_over = store
            .begin_own_revocation(
                update.clone(),
                fleet.person.sign_pk.clone(),
                fleet.approver.clone(),
                NOW,
            )
            .expect("begin");
        assert_eq!(handed_over, update.inbox_key);

        // This is the state a device wakes up in after dying mid-ceremony. It
        // knows which generation to ask its own key store about, and nothing
        // observable has changed: the roster it holds is still the old one.
        let pending = store
            .pending_own_revocation()
            .expect("pending")
            .expect("a revocation is in flight");
        assert_eq!(pending.roster_head, update.roster_head);
        assert_eq!(pending.inbox_key_generation, update.inbox_key.generation);
        assert_eq!(pending.revoked_device_ids, update.revoked_device_ids);
        assert_eq!(store.own_roster().unwrap().unwrap(), fleet.roster);

        // Holding the key, it finishes the job by re-running the commit.
        store
            .commit_own_revocation(
                update.clone(),
                fleet.person.sign_pk.clone(),
                fleet.approver.clone(),
                Some(fleet.inbox_key.clone()),
                NOW,
            )
            .expect("commit");
        assert!(store.pending_own_revocation().expect("pending").is_none());
        assert_eq!(store.own_roster().unwrap().unwrap(), update.roster);

        // A pending row for a DIFFERENT revocation does not authorize this
        // one: the key that was stored is not the key this would re-seal to.
        store
            .begin_own_revocation(
                update.clone(),
                fleet.person.sign_pk.clone(),
                fleet.approver.clone(),
                NOW,
            )
            .expect("begin");
        assert!(store
            .commit_own_revocation(
                RevocationUpdate {
                    roster_head: vec![0x55; 32],
                    ..update
                },
                fleet.person.sign_pk.clone(),
                fleet.approver.clone(),
                None,
                NOW,
            )
            .is_err());
        // And a device whose key never reached storage abandons cleanly.
        assert!(store.abandon_own_revocation().expect("abandon"));
        assert!(!store.abandon_own_revocation().expect("nothing pending"));
    }

    /// A counter at its ceiling is refused, never saturated. Saturating would
    /// mint a document at the SAME version as the one it was meant to
    /// supersede with different content -- a fork this device manufactured
    /// against itself, which DL-2 would quarantine the person for.
    /// §14.2's rescue, end to end, on the leg that used to drop it: the
    /// recovery happens on a **new** phone, so the announcement is signed by a
    /// device no surviving sibling has ever seen a certificate for. A gate
    /// that only knew the roster held now refused it as
    /// `UnknownAuthorDevice`, which meant the one ceremony that rescues a
    /// fleet from a stolen phone was the one ceremony the fleet could not
    /// hear.
    #[test]
    fn a_recovery_rotation_reaches_a_surviving_sibling() {
        let fleet = fleet();
        let sibling_store = store_for(&fleet, &fleet.sibling);

        // The owner opens the `.cmbak` on a phone that has never been linked,
        // and the root signs a roster at the next epoch that buries the stolen
        // approving device. The sibling survives it.
        let rescue = generate_device_keypair();
        let recovery = core_recovery_revoke_roster(
            fleet.roster.clone(),
            fleet.person.sign_sk.clone(),
            rescue.sign_pk.clone(),
            rescue.agree_pk.clone(),
            vec![fleet.approver.device_id.clone()],
            None,
        )
        .expect("recovery revocation");
        assert!(recovery
            .roster
            .devices
            .iter()
            .any(|cert| cert.device_id() == fleet.sibling.device_id));

        let record = core_sign_sync_record(
            SyncRecord {
                kind: SyncRecordKind::OwnRoster,
                person_id: fleet.person.user_id.clone(),
                author_device_id: Vec::new(),
                roster_version: recovery.roster.version(),
                inbox_key_generation: recovery.inbox_key.generation,
                stream_seq: 1,
                timestamp_ms: NOW,
                payload: core_encode_sync_own_roster(SyncOwnRosterPayload {
                    roster: recovery.roster.clone(),
                    inbox_keys: vec![recovery.inbox_key.clone()],
                })
                .expect("payload"),
                signature: Vec::new(),
            },
            rescue.sign_sk.clone(),
        )
        .expect("signs");
        let sealed = core_seal_sync_handoff(
            record,
            core_device_sync_identity(rescue.clone()),
            // Addressed against the roster the recovery PRODUCED, which is the
            // only document that lists the rescuing device at all.
            recovery.roster.clone(),
            fleet.sibling.device_id.clone(),
        )
        .expect("seals");

        let adoption = sibling_store
            .adopt_revocation_handoff(
                sealed.sealed.clone(),
                fleet.person.sign_pk.clone(),
                fleet.sibling.clone(),
                Some(fleet.inbox_key.clone()),
                NOW,
            )
            .expect("adopts");
        assert_eq!(adoption.outcome, RevocationAdoptionOutcome::Adopted);
        assert_eq!(
            adoption.revoked_device_ids,
            vec![fleet.approver.device_id.clone()]
        );
        assert_eq!(adoption.inbox_key, Some(recovery.inbox_key.clone()));
        assert_eq!(
            sibling_store.own_roster().unwrap().unwrap(),
            recovery.roster
        );

        // The door is opened by the acceptance rules, not by "an unknown
        // signer is fine". A stranger who mints their own fleet cannot even
        // address this device -- their roster lists no certificate for it --
        // and the roster inside names a different person, which is the second
        // refusal waiting behind the first.
        let stranger_person = generate_identity();
        let stranger_device = generate_device_keypair();
        let stranger_roster = core_link_genesis_roster(
            stranger_person.sign_sk.clone(),
            stranger_device.sign_pk.clone(),
            stranger_device.agree_pk.clone(),
        )
        .expect("genesis");
        let record = core_sign_sync_record(
            SyncRecord {
                kind: SyncRecordKind::OwnRoster,
                person_id: fleet.person.user_id.clone(),
                author_device_id: Vec::new(),
                roster_version: recovery.roster.version(),
                inbox_key_generation: recovery.inbox_key.generation,
                stream_seq: 2,
                timestamp_ms: NOW,
                payload: core_encode_sync_own_roster(SyncOwnRosterPayload {
                    roster: stranger_roster.clone(),
                    inbox_keys: vec![recovery.inbox_key],
                })
                .expect("payload"),
                signature: Vec::new(),
            },
            stranger_device.sign_sk.clone(),
        )
        .expect("signs");
        assert!(core_seal_sync_handoff(
            record,
            core_device_sync_identity(stranger_device),
            stranger_roster,
            fleet.sibling.device_id.clone(),
        )
        .is_err());
    }

    #[test]
    fn a_version_counter_at_its_ceiling_is_an_error_not_a_silent_fork() {
        let fleet = fleet();
        let maxed = core_sign_roster(
            Roster {
                seq: u64::MAX,
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
                ..fleet.roster.clone()
            },
            fleet.approver.sign_sk.clone(),
        )
        .expect("signs");
        assert!(core_revoke_devices_roster(
            maxed,
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            vec![fleet.sibling.device_id.clone()],
            fleet.inbox_key.clone(),
        )
        .is_err());

        // Same for the recovery path's epoch.
        let maxed_epoch = core_sign_roster(
            Roster {
                recovery_epoch: u64::MAX,
                seq: 0,
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
                ..fleet.roster.clone()
            },
            fleet.person.sign_sk.clone(),
        )
        .expect("signs");
        let rescue = generate_device_keypair();
        assert!(core_recovery_revoke_roster(
            maxed_epoch,
            fleet.person.sign_sk.clone(),
            rescue.sign_pk,
            rescue.agree_pk,
            vec![fleet.sibling.device_id.clone()],
            None,
        )
        .is_err());
    }

    #[test]
    fn only_an_own_roster_record_may_ride_the_handoff_channel() {
        let fleet = fleet();
        let record = core_sign_sync_record(
            SyncRecord {
                kind: SyncRecordKind::Watermarks,
                person_id: fleet.person.user_id.clone(),
                author_device_id: Vec::new(),
                roster_version: fleet.roster.version(),
                inbox_key_generation: fleet.roster.inbox_key_generation,
                stream_seq: 1,
                timestamp_ms: NOW,
                payload: crate::core_encode_sync_watermarks(crate::SyncWatermarkPayload {
                    entries: Vec::new(),
                })
                .expect("payload"),
                signature: Vec::new(),
            },
            fleet.approver.sign_sk.clone(),
        )
        .expect("signs");
        // Dropping the stale-generation check is a real weakening, so it is
        // confined to the one kind that carries a roster and its new key.
        assert!(core_seal_sync_handoff(
            record.clone(),
            core_device_sync_identity(fleet.approver.clone()),
            fleet.roster.clone(),
            fleet.sibling.device_id.clone(),
        )
        .is_err());
        assert_eq!(
            crate::core_sync_handoff_admit(record, fleet.roster),
            Some(SyncRecordRejection::NotARotationHandoff)
        );
    }
}
