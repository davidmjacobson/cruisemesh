//! What opening a `.cmbak` on a fresh install may mean, in core
//! (`specs/multi-device-v1.md` §9's closing paragraph, §14.2).
//!
//! > Restore-from-backup UX changes to match: opening a `.cmbak` on a fresh
//! > install offers **"Replace this device"** (old semantics, same device
//! > count) or **"Link as new device"** (routes into this ceremony). A raw
//! > clone running live alongside its source is the §1 failure mode.
//!
//! The two intents are modelled here rather than in a shell because the
//! difference between them is not presentation. One says "this phone *is* the
//! phone in the backup" and takes its device identity, its author stream, and
//! its relay rows. The other says "this phone is a *new* phone belonging to the
//! person in the backup" and takes none of those: it mints a fresh device key
//! and either meets the person's live approving device (§9's ceremony) or, if
//! there is no live device left to ask, uses the root secret the backup carries
//! to sign a recovery-epoch roster (§14.2).
//!
//! Choosing the first while the source phone is still running is exactly §1's
//! two-devices-one-identity failure, which is why the plan says so in a field
//! a shell can put on screen rather than leaving it to whoever writes the copy.
//! WP6 owns that copy; the decision lives here.

use crate::backup::{decode_identity_bytes, CoreBackupPayload};
use crate::device_roster::Roster;
use crate::{CoreError, Identity, MessageStore};

/// The two things a restore can mean (§9).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreRestoreIntent {
    /// Old semantics, unchanged: this phone becomes the phone in the backup.
    /// The device count does not change, because no device was added.
    ReplaceThisDevice,
    /// This phone becomes an ADDITIONAL device of the person in the backup, by
    /// way of §9's linking ceremony.
    LinkAsNewDevice,
}

/// What one intent commits to. Every field is a decision the caller would
/// otherwise have to re-derive, and getting one of them wrong is how a clone
/// happens.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRestorePlan {
    pub intent: CoreRestoreIntent,
    /// The person this backup belongs to — the wire `user_id`, unchanged by
    /// either intent (§2 goal 2: no re-friending, ever).
    pub person_id: Vec<u8>,
    /// Restore the backup's message store as this device's history.
    pub restores_stored_history: bool,
    /// Take the backup's own-device record as this device's own (§9: a
    /// replacement really is the same device, with the same id and siblings).
    pub keeps_existing_device_identity: bool,
    /// Mint a fresh device keypair, because this is a different device.
    pub mints_new_device_key: bool,
    /// Hand off to §9's ceremony rather than finishing here.
    pub routes_to_link_ceremony: bool,
    /// §14.2: the backup carries a person root secret that can actually sign,
    /// so this device can mint a recovery-epoch roster if no approving device
    /// is left to ask. False when the root secret in the identity block does
    /// not produce the root key beside it — a backup that will restore history
    /// and will not recover a fleet.
    pub carries_recovery_material: bool,
    /// §1: choosing this intent while the source device is still running
    /// creates two devices signing one author stream. True for
    /// [`CoreRestoreIntent::ReplaceThisDevice`], which is the intent that has
    /// to be chosen with the old phone switched off.
    pub clone_hazard_if_source_is_live: bool,
}

/// Both intents, in the order a person should be offered them.
///
/// "Link as new device" comes first deliberately: it is the safe one. A person
/// restoring a backup onto a second phone that still has a first phone in the
/// house wants this, and the intent that can strand two live clones should not
/// be the one their thumb lands on.
#[uniffi::export]
pub fn core_backup_restore_plans(
    payload: CoreBackupPayload,
) -> Result<Vec<CoreRestorePlan>, CoreError> {
    let identity = backup_identity(&payload)?;
    Ok(vec![
        plan(CoreRestoreIntent::LinkAsNewDevice, &identity),
        plan(CoreRestoreIntent::ReplaceThisDevice, &identity),
    ])
}

/// One intent's plan.
#[uniffi::export]
pub fn core_backup_restore_plan(
    payload: CoreBackupPayload,
    intent: CoreRestoreIntent,
) -> Result<CoreRestorePlan, CoreError> {
    Ok(plan(intent, &backup_identity(&payload)?))
}

#[uniffi::export]
impl MessageStore {
    /// §14.2's path out of "my only other phone is gone": sign a roster at the
    /// next recovery epoch with the root secret inside the opened backup,
    /// naming this fresh device as the approving device.
    ///
    /// **Call this on a store opened over the backup's own sqlite** — the bytes
    /// in [`CoreBackupPayload::sqlite`], written to a file and opened. The last
    /// roster the person's fleet was known to hold is read from there, by this
    /// function, because the caller has no other way to know it: the
    /// `LinkAsNewDevice` intent deliberately does not restore that store as its
    /// own history, so nothing else in the flow ever holds the document. It
    /// used to be a parameter, which meant every caller was asked for a value
    /// it could only produce by guessing — and a guess of `None` here silently
    /// mints epoch 1 over a fleet that had already recovered twice, producing a
    /// roster every contact ignores.
    ///
    /// A backup with no roster at all is genesis, not an error: a person whose
    /// fleet predates rosters entirely has nothing to supersede.
    ///
    /// This is deliberately NOT reachable from
    /// [`CoreRestoreIntent::ReplaceThisDevice`]: a replacement is the same
    /// device and needs no new epoch, and minting one would tell every contact
    /// that something was recovered when nothing was.
    pub fn backup_recovery_roster(
        &self,
        payload: CoreBackupPayload,
        device_sign_pk: Vec<u8>,
        device_agree_pk: Vec<u8>,
    ) -> Result<Roster, CoreError> {
        let identity = backup_identity(&payload)?;
        super::activation::core_link_recovery_roster(
            self.own_roster()?,
            identity.sign_sk,
            device_sign_pk,
            device_agree_pk,
        )
    }
}

fn plan(intent: CoreRestoreIntent, identity: &Identity) -> CoreRestorePlan {
    let replacing = intent == CoreRestoreIntent::ReplaceThisDevice;
    CoreRestorePlan {
        intent,
        person_id: identity.user_id.clone(),
        restores_stored_history: replacing,
        keeps_existing_device_identity: replacing,
        mints_new_device_key: !replacing,
        routes_to_link_ceremony: !replacing,
        carries_recovery_material: carries_recovery_material(identity),
        clone_hazard_if_source_is_live: replacing,
    }
}

/// Whether the root secret in this backup can actually sign §14.2's recovery
/// roster.
///
/// It used to be the constant `true`, which made it a field that said nothing:
/// the only stated false case — an identity block that will not decode — is
/// unreachable, because a payload that fails to decode never produces a plan at
/// all. So it is answered honestly here instead. A structurally valid identity
/// block whose secret half does not match its public half decodes fine and
/// cannot sign anything; a person holding that backup has a `.cmbak` that will
/// restore their history and will NOT get them out of a lost approving device,
/// and the difference is exactly what this field is for.
fn carries_recovery_material(identity: &Identity) -> bool {
    crate::crypto::signing_key_from_bytes(&identity.sign_sk)
        .map(|key| key.verifying_key().as_bytes().to_vec() == identity.sign_pk)
        .unwrap_or(false)
}

fn backup_identity(payload: &CoreBackupPayload) -> Result<Identity, CoreError> {
    decode_identity_bytes(payload.identity.clone())
        .map_err(|error| CoreError::Malformed(format!("backup identity: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backup::encode_identity_bytes;
    use crate::device_roster::generate_device_keypair;
    use crate::identity::generate_identity;

    fn payload(identity: &Identity) -> CoreBackupPayload {
        CoreBackupPayload {
            identity: encode_identity_bytes(identity.clone()),
            sqlite: b"not read here".to_vec(),
            src_version_code: 1,
            created_at_ms: 1_755_000_000_000,
            display_name: Some("Alice".to_string()),
            own_avatar: Vec::new(),
            own_avatar_epoch: 0,
            relay_url: None,
            relay_token: None,
            share_online: false,
            friends_of_friends_enabled: false,
        }
    }

    /// The two intents differ in every way that matters, and the safe one is
    /// offered first.
    #[test]
    fn opening_a_backup_offers_link_before_replace() {
        let identity = generate_identity();
        let plans = core_backup_restore_plans(payload(&identity)).unwrap();

        assert_eq!(plans[0].intent, CoreRestoreIntent::LinkAsNewDevice);
        assert_eq!(plans[1].intent, CoreRestoreIntent::ReplaceThisDevice);
        for plan in &plans {
            assert_eq!(
                plan.person_id, identity.user_id,
                "§2 goal 2: no re-friending"
            );
            assert!(
                plan.carries_recovery_material,
                "§14.2: this backup's root secret really can sign"
            );
        }

        let link = &plans[0];
        assert!(link.mints_new_device_key && link.routes_to_link_ceremony);
        assert!(!link.keeps_existing_device_identity && !link.restores_stored_history);
        assert!(!link.clone_hazard_if_source_is_live);

        let replace = &plans[1];
        assert!(replace.keeps_existing_device_identity && replace.restores_stored_history);
        assert!(!replace.mints_new_device_key && !replace.routes_to_link_ceremony);
        assert!(replace.clone_hazard_if_source_is_live, "§1");
    }

    /// §14.2, end to end in the shape the `LinkAsNewDevice` intent really has:
    /// a backup is opened, its plan says this is a new phone that mints a fresh
    /// device key, and the recovery roster is signed against the roster read out
    /// of the backup's OWN store — the value no caller could have supplied.
    #[test]
    fn linking_from_a_backup_can_sign_a_recovery_roster() {
        let identity = generate_identity();
        let lost = crate::device_roster::generate_device_keypair();
        let replacement = generate_device_keypair();
        let payload = payload(&identity);

        let plan = core_backup_restore_plan(payload.clone(), CoreRestoreIntent::LinkAsNewDevice)
            .expect("a decodable backup has a plan");
        assert!(plan.mints_new_device_key && plan.routes_to_link_ceremony);
        assert!(plan.carries_recovery_material);

        // The backup's own store, as it would be after its sqlite bytes were
        // written out and opened: it holds the fleet's last roster and nothing
        // is restored from it as this phone's history.
        let backup_store = crate::MessageStore::open(":memory:".to_string()).unwrap();
        let stored = super::super::activation::core_link_genesis_roster(
            identity.sign_sk.clone(),
            lost.sign_pk.clone(),
            lost.agree_pk.clone(),
        )
        .unwrap();
        backup_store
            .adopt_own_roster(
                stored.clone(),
                identity.sign_pk.clone(),
                lost.device_id.clone(),
            )
            .unwrap();

        let recovered = backup_store
            .backup_recovery_roster(
                payload,
                replacement.sign_pk.clone(),
                replacement.agree_pk.clone(),
            )
            .unwrap();

        assert_eq!(recovered.recovery_epoch, stored.recovery_epoch + 1);
        assert_eq!(recovered.approving_device_id, replacement.device_id);
        assert_eq!(
            recovered.inbox_key_generation,
            stored.inbox_key_generation + 1,
            "§6: contacts must be told to stop sealing to what the lost phone could open"
        );
        assert!(crate::core_roster_validate(recovered, identity.sign_pk).is_none());
    }

    /// A backup with no roster in it at all is genesis, not a failure: a person
    /// whose fleet predates rosters has nothing to supersede.
    #[test]
    fn a_backup_with_no_roster_recovers_as_genesis() {
        let identity = generate_identity();
        let replacement = generate_device_keypair();
        let empty = crate::MessageStore::open(":memory:".to_string()).unwrap();

        let recovered = empty
            .backup_recovery_roster(
                payload(&identity),
                replacement.sign_pk,
                replacement.agree_pk.clone(),
            )
            .unwrap();
        assert_eq!(recovered.recovery_epoch, 0);
        assert_eq!(recovered.seq, 0);
        assert_eq!(recovered.approving_device_id, replacement.device_id);
    }

    /// `carries_recovery_material` says something, or it says nothing. A backup
    /// whose identity block decodes but whose secret half cannot produce its own
    /// public half will restore history and will NOT get anyone out of a lost
    /// approving device, and the plan must say so.
    #[test]
    fn recovery_material_is_a_claim_about_the_key_not_a_constant() {
        let identity = generate_identity();
        assert!(
            core_backup_restore_plan(payload(&identity), CoreRestoreIntent::LinkAsNewDevice)
                .unwrap()
                .carries_recovery_material
        );

        let mut mismatched = identity.clone();
        mismatched.sign_sk = generate_identity().sign_sk;
        assert!(
            !core_backup_restore_plan(payload(&mismatched), CoreRestoreIntent::LinkAsNewDevice)
                .unwrap()
                .carries_recovery_material,
            "a backup whose root secret does not match its root key cannot recover anything"
        );
    }

    #[test]
    fn a_backup_whose_identity_will_not_decode_has_no_plans() {
        let mut broken = payload(&generate_identity());
        broken.identity = vec![0x01; 7];
        assert!(core_backup_restore_plans(broken.clone()).is_err());
        assert!(core_backup_restore_plan(broken, CoreRestoreIntent::LinkAsNewDevice).is_err());
    }
}
