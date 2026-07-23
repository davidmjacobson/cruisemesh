//! Group state and wire/crypto helpers (DESIGN.md §6.5).
//!
//! A group is a random 16-byte id plus a symmetric XChaCha20-Poly1305 key,
//! shared with members via pairwise-sealed `kind=4` invites. Group-authored
//! envelopes reuse the same authenticated inner layout as 1:1 traffic
//! (`sender_sign_pk || signature || payload`, padded to 256-byte buckets),
//! but replace the outer `crypto_box` seal with symmetric XChaCha20-Poly1305.

use chacha20poly1305::aead::{Aead, AeadCore};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use rand_core::{OsRng, RngCore};

use crate::crypto::{key_len_err, open_signed_payload, sign_and_pad};
use crate::{CoreError, Identity};

const GROUP_ID_LEN: usize = 16;
const GROUP_KEY_LEN: usize = 32;
const GROUP_ENVELOPE_VERSION: u8 = 1;
const GROUP_NONCE_LEN: usize = 24;
const GROUP_METADATA_VERSION: u8 = 1;
const USER_ID_LEN: usize = 16;
const MAX_GROUP_MEMBERS: usize = 64;
const MAX_GROUP_NAME_BYTES: usize = 128;

/// A persisted/imported group: id, display name, full member list, and the
/// current symmetric group key.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct Group {
    pub id: Vec<u8>,
    pub name: String,
    pub member_user_ids: Vec<Vec<u8>>,
    pub key: Vec<u8>,
    pub metadata_revision: u64,
    pub metadata_changed_by: Vec<u8>,
}

/// An add-only membership snapshot plus a convergent group-name update.
/// Membership is merged as a set so reordered concurrent additions cannot
/// remove a member. The `(revision, changed_by)` tuple orders name changes.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct GroupMetadataUpdate {
    pub group_id: Vec<u8>,
    pub name: String,
    pub revision: u64,
    pub changed_by: Vec<u8>,
    pub member_user_ids: Vec<Vec<u8>>,
}

/// Generate a fresh group id + key with a canonicalized member list
/// (deduplicated, byte-sorted user ids).
#[uniffi::export]
pub fn create_group(name: String, member_user_ids: Vec<Vec<u8>>) -> Result<Group, CoreError> {
    validate_group_name(&name)?;
    validate_member_user_ids(&member_user_ids)?;
    let mut id = vec![0u8; GROUP_ID_LEN];
    let mut key = vec![0u8; GROUP_KEY_LEN];
    OsRng.fill_bytes(&mut id);
    OsRng.fill_bytes(&mut key);
    Ok(Group {
        id,
        name,
        member_user_ids: canonicalize_members(member_user_ids),
        key,
        metadata_revision: 0,
        metadata_changed_by: Vec::new(),
    })
}

/// Rotate a group's symmetric key and replace its member list, keeping the
/// stable group id/name. This is the v1 membership-change mechanism from
/// DESIGN.md §6.5: generate a new key and re-invite the remaining members.
#[uniffi::export]
pub fn rotate_group(group: Group, member_user_ids: Vec<Vec<u8>>) -> Result<Group, CoreError> {
    validate_group(&group)?;
    validate_member_user_ids(&member_user_ids)?;
    let mut key = vec![0u8; GROUP_KEY_LEN];
    OsRng.fill_bytes(&mut key);
    Ok(Group {
        id: group.id,
        name: group.name,
        member_user_ids: canonicalize_members(member_user_ids),
        key,
        metadata_revision: group.metadata_revision,
        metadata_changed_by: group.metadata_changed_by,
    })
}

/// Encode the `content` of a `kind=4` group-invite body.
///
/// Layout (big-endian):
/// `group_id(16) | key(32) | name_len(u16) | name_utf8 | member_count(u16) |
/// repeated member_user_id_len(u16) | member_user_id`.
#[uniffi::export]
pub fn encode_group_invite_content(group: Group) -> Result<Vec<u8>, CoreError> {
    validate_group(&group)?;
    let member_user_ids = canonicalize_members(group.member_user_ids);
    let mut out = Vec::with_capacity(
        GROUP_ID_LEN
            + GROUP_KEY_LEN
            + 2
            + group.name.len()
            + 2
            + member_user_ids
                .iter()
                .map(|member| 2 + member.len())
                .sum::<usize>(),
    );
    out.extend_from_slice(&group.id);
    out.extend_from_slice(&group.key);
    write_bytes16(&mut out, group.name.as_bytes());
    out.extend_from_slice(&(member_user_ids.len() as u16).to_be_bytes());
    for member_user_id in member_user_ids {
        write_bytes16(&mut out, &member_user_id);
    }
    Ok(out)
}

/// Decode a `kind=4` group-invite `content` payload.
#[uniffi::export]
pub fn decode_group_invite_content(bytes: Vec<u8>) -> Result<Group, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let id = cursor.take(GROUP_ID_LEN)?.to_vec();
    let key = cursor.take(GROUP_KEY_LEN)?.to_vec();
    let name_bytes = cursor.take_bytes16()?;
    let name = String::from_utf8(name_bytes)
        .map_err(|e| CoreError::Malformed(format!("group invite name is not UTF-8: {e}")))?;
    validate_group_name(&name)?;
    let member_count = cursor.take_u16()? as usize;
    if member_count > MAX_GROUP_MEMBERS {
        return Err(CoreError::Malformed(format!(
            "group has too many members: {member_count}"
        )));
    }
    let mut member_user_ids = Vec::with_capacity(member_count.min(bytes.len()));
    for _ in 0..member_count {
        member_user_ids.push(cursor.take_user_id("group member UserID")?);
    }
    cursor.finish()?;
    let group = Group {
        id,
        name,
        member_user_ids: canonicalize_members(member_user_ids),
        key,
        metadata_revision: 0,
        metadata_changed_by: Vec::new(),
    };
    validate_group(&group)?;
    Ok(group)
}

/// Create the next locally-authored metadata update. Membership is add-only;
/// callers may pass the current list plus newly accepted contacts.
#[uniffi::export]
pub fn create_group_metadata_update(
    group: Group,
    changed_by: Vec<u8>,
    name: String,
    member_user_ids: Vec<Vec<u8>>,
) -> Result<GroupMetadataUpdate, CoreError> {
    validate_group(&group)?;
    if !group
        .member_user_ids
        .iter()
        .any(|member| *member == changed_by)
    {
        return Err(CoreError::Malformed(
            "group metadata author is not a member".to_string(),
        ));
    }
    let name = name.trim().to_string();
    validate_group_name(&name)?;
    validate_member_user_ids(&member_user_ids)?;
    let member_user_ids = canonicalize_members(member_user_ids);
    if !group
        .member_user_ids
        .iter()
        .all(|member| member_user_ids.contains(member))
    {
        return Err(CoreError::Malformed(
            "group metadata update cannot remove members".to_string(),
        ));
    }
    let revision = group
        .metadata_revision
        .checked_add(1)
        .ok_or_else(|| CoreError::Malformed("group metadata revision overflow".to_string()))?;
    Ok(GroupMetadataUpdate {
        group_id: group.id,
        name,
        revision,
        changed_by,
        member_user_ids,
    })
}

/// Apply a verified member-authored update. Name changes use the deterministic
/// `(revision, changed_by)` winner. Member ids are always unioned, even for a
/// losing concurrent name update, so delivery order cannot lose additions.
#[uniffi::export]
pub fn apply_group_metadata_update(
    group: Group,
    update: GroupMetadataUpdate,
    sender_user_id: Vec<u8>,
) -> Result<Option<Group>, CoreError> {
    validate_group(&group)?;
    validate_group_metadata_update(&update)?;
    if update.group_id != group.id {
        return Err(CoreError::Malformed(
            "group metadata update id does not match group".to_string(),
        ));
    }
    if update.changed_by != sender_user_id {
        return Err(CoreError::Malformed(
            "group metadata signer does not match changed_by".to_string(),
        ));
    }
    if !group
        .member_user_ids
        .iter()
        .any(|member| *member == sender_user_id)
    {
        return Err(CoreError::Malformed(
            "group metadata signer is not a member".to_string(),
        ));
    }

    let mut merged_members = group.member_user_ids.clone();
    merged_members.extend(update.member_user_ids.clone());
    merged_members = canonicalize_members(merged_members);
    validate_member_user_ids(&merged_members)?;
    let membership_changed = merged_members != group.member_user_ids;
    let name_wins = (update.revision, update.changed_by.as_slice())
        > (
            group.metadata_revision,
            group.metadata_changed_by.as_slice(),
        );
    if !membership_changed && !name_wins {
        return Ok(None);
    }

    Ok(Some(Group {
        id: group.id,
        name: if name_wins { update.name } else { group.name },
        member_user_ids: merged_members,
        key: group.key,
        metadata_revision: if name_wins {
            update.revision
        } else {
            group.metadata_revision
        },
        metadata_changed_by: if name_wins {
            update.changed_by
        } else {
            group.metadata_changed_by
        },
    }))
}

/// Encode a group metadata update for a hidden group-stream message.
#[uniffi::export]
pub fn encode_group_metadata_update(update: GroupMetadataUpdate) -> Result<Vec<u8>, CoreError> {
    validate_group_metadata_update(&update)?;
    let members = canonicalize_members(update.member_user_ids);
    let mut out = Vec::new();
    out.push(GROUP_METADATA_VERSION);
    out.extend_from_slice(&update.group_id);
    out.extend_from_slice(&update.revision.to_be_bytes());
    write_bytes16(&mut out, &update.changed_by);
    write_bytes16(&mut out, update.name.as_bytes());
    out.extend_from_slice(&(members.len() as u16).to_be_bytes());
    for member in members {
        write_bytes16(&mut out, &member);
    }
    Ok(out)
}

/// Decode a hidden group-stream metadata update.
#[uniffi::export]
pub fn decode_group_metadata_update(bytes: Vec<u8>) -> Result<GroupMetadataUpdate, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let version = cursor.take(1)?[0];
    if version != GROUP_METADATA_VERSION {
        return Err(CoreError::Malformed(format!(
            "unsupported group metadata version {version}"
        )));
    }
    let group_id = cursor.take(GROUP_ID_LEN)?.to_vec();
    let revision = cursor.take_u64()?;
    let changed_by = cursor.take_user_id("group metadata changed_by")?;
    let name = String::from_utf8(cursor.take_bytes16()?)
        .map_err(|e| CoreError::Malformed(format!("group metadata name is not UTF-8: {e}")))?;
    validate_group_name(&name)?;
    let count = cursor.take_u16()? as usize;
    if count > MAX_GROUP_MEMBERS {
        return Err(CoreError::Malformed(format!(
            "group metadata has too many members: {count}"
        )));
    }
    let mut member_user_ids = Vec::with_capacity(count.min(bytes.len()));
    for _ in 0..count {
        member_user_ids.push(cursor.take_user_id("group member UserID")?);
    }
    cursor.finish()?;
    let update = GroupMetadataUpdate {
        group_id,
        name,
        revision,
        changed_by,
        member_user_ids: canonicalize_members(member_user_ids),
    };
    validate_group_metadata_update(&update)?;
    Ok(update)
}

/// Sign `payload` with the sender's Ed25519 key, pad it to the next 256-byte
/// bucket, then encrypt it with the group's XChaCha20-Poly1305 key.
///
/// Wire layout: `version(1) | nonce(24) | ciphertext+tag`.
#[uniffi::export]
pub fn seal_group_message(
    sender: Identity,
    group: Group,
    payload: Vec<u8>,
) -> Result<Vec<u8>, CoreError> {
    validate_group(&group)?;
    let key = group_key_from_bytes(&group.key)?;
    let cipher = XChaCha20Poly1305::new(&key);
    let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
    let padded = sign_and_pad(&sender, &payload)?;
    let ciphertext = cipher
        .encrypt(&nonce, padded.as_slice())
        .map_err(|_| CoreError::Crypto("group seal failed".to_string()))?;

    let mut envelope = Vec::with_capacity(1 + GROUP_NONCE_LEN + ciphertext.len());
    envelope.push(GROUP_ENVELOPE_VERSION);
    envelope.extend_from_slice(nonce.as_slice());
    envelope.extend_from_slice(&ciphertext);
    Ok(envelope)
}

/// Open a group-authored envelope with the imported group key and verify the
/// embedded sender signature.
///
/// SECURITY CONTRACT: this verifies only that the payload was sealed with the
/// group key and signed by the key embedded in it — it does NOT check that
/// the signer is a group member. Anyone holding the group key (an invite
/// leak, a removed member before rotation) can produce an envelope that opens
/// successfully under any identity they mint. Every caller MUST verify
/// `sender_user_id ∈ group.member_user_ids` before trusting the body; both
/// shells do this in `deliverOpenedGroupEnvelope`
/// (InboundEnvelopeProcessor.kt / MeshController.swift), and
/// `outsider_with_group_key_opens_but_is_not_a_member` pins this contract so
/// a change here is deliberate, not accidental.
#[uniffi::export]
pub fn open_group_message(
    group: Group,
    sealed: Vec<u8>,
) -> Result<crate::OpenedMessage, CoreError> {
    validate_group(&group)?;
    if sealed.len() < 1 + GROUP_NONCE_LEN {
        return Err(CoreError::Crypto("group envelope too short".to_string()));
    }
    let (version, rest) = sealed.split_at(1);
    if version[0] != GROUP_ENVELOPE_VERSION {
        return Err(CoreError::Crypto(format!(
            "unsupported group envelope version {} (this client speaks {GROUP_ENVELOPE_VERSION})",
            version[0]
        )));
    }
    let (nonce_bytes, ciphertext) = rest.split_at(GROUP_NONCE_LEN);
    let key = group_key_from_bytes(&group.key)?;
    let cipher = XChaCha20Poly1305::new(&key);
    let nonce = *XNonce::from_slice(nonce_bytes);
    let padded = cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|_| CoreError::Crypto("group open failed".to_string()))?;
    open_signed_payload(&padded)
}

pub(crate) fn validate_group(group: &Group) -> Result<(), CoreError> {
    validate_group_id(&group.id)?;
    validate_group_name(&group.name)?;
    validate_member_user_ids(&group.member_user_ids)?;
    if group.key.len() != GROUP_KEY_LEN {
        return Err(key_len_err(GROUP_KEY_LEN as u32, group.key.len()));
    }
    if !group.metadata_changed_by.is_empty() {
        validate_user_id(&group.metadata_changed_by, "group metadata changed_by")?;
    }
    Ok(())
}

fn validate_group_metadata_update(update: &GroupMetadataUpdate) -> Result<(), CoreError> {
    validate_group_id(&update.group_id)?;
    if update.revision == 0 {
        return Err(CoreError::Malformed(
            "group metadata revision must be positive".to_string(),
        ));
    }
    validate_user_id(&update.changed_by, "group metadata changed_by")?;
    validate_group_name(&update.name)?;
    validate_member_user_ids(&update.member_user_ids)?;
    Ok(())
}

fn validate_group_name(name: &str) -> Result<(), CoreError> {
    if name.trim().is_empty() {
        return Err(CoreError::Malformed(
            "group name must not be empty".to_string(),
        ));
    }
    if name.len() > MAX_GROUP_NAME_BYTES {
        return Err(CoreError::Malformed(format!(
            "group name exceeds {MAX_GROUP_NAME_BYTES} UTF-8 bytes"
        )));
    }
    Ok(())
}

fn validate_member_user_ids(member_user_ids: &[Vec<u8>]) -> Result<(), CoreError> {
    if member_user_ids.len() > MAX_GROUP_MEMBERS {
        return Err(CoreError::Malformed(format!(
            "group exceeds {MAX_GROUP_MEMBERS} members"
        )));
    }
    for member in member_user_ids {
        validate_user_id(member, "group member UserID")?;
    }
    Ok(())
}

fn validate_user_id(user_id: &[u8], label: &str) -> Result<(), CoreError> {
    if user_id.len() != USER_ID_LEN {
        return Err(CoreError::Malformed(format!(
            "{label} must be {USER_ID_LEN} bytes, got {}",
            user_id.len()
        )));
    }
    Ok(())
}

fn validate_group_id(group_id: &[u8]) -> Result<(), CoreError> {
    if group_id.len() != GROUP_ID_LEN {
        return Err(CoreError::Malformed(format!(
            "group id must be {GROUP_ID_LEN} bytes, got {}",
            group_id.len()
        )));
    }
    Ok(())
}

pub(crate) fn canonicalize_members(mut member_user_ids: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    member_user_ids.sort();
    member_user_ids.dedup();
    member_user_ids
}

fn group_key_from_bytes(bytes: &[u8]) -> Result<Key, CoreError> {
    let arr: [u8; GROUP_KEY_LEN] = bytes
        .try_into()
        .map_err(|_| key_len_err(GROUP_KEY_LEN as u32, bytes.len()))?;
    Ok(arr.into())
}

fn write_bytes16(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

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
            .filter(|&end| end <= self.data.len());
        match end {
            Some(end) => {
                let slice = &self.data[self.pos..end];
                self.pos = end;
                Ok(slice)
            }
            None => Err(CoreError::Malformed(format!(
                "truncated: need {n} more byte(s) at offset {}, have {}",
                self.pos,
                self.data.len().saturating_sub(self.pos)
            ))),
        }
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

    fn take_bytes16(&mut self) -> Result<Vec<u8>, CoreError> {
        let len = self.take_u16()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn take_user_id(&mut self, label: &str) -> Result<Vec<u8>, CoreError> {
        let user_id = self.take_bytes16()?;
        validate_user_id(&user_id, label)?;
        Ok(user_id)
    }

    fn finish(self) -> Result<(), CoreError> {
        if self.pos != self.data.len() {
            return Err(CoreError::Malformed(format!(
                "{} unexpected trailing byte(s) after decoding",
                self.data.len() - self.pos
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::identity::generate_identity;

    use super::*;

    fn user(byte: u8) -> Vec<u8> {
        vec![byte; USER_ID_LEN]
    }

    fn sample_group() -> Group {
        Group {
            id: vec![0x11; GROUP_ID_LEN],
            name: "Bridge Crew".to_string(),
            member_user_ids: vec![user(3), user(1), user(1)],
            key: vec![0x22; GROUP_KEY_LEN],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        }
    }

    /// Pins open_group_message's SECURITY CONTRACT (see its doc comment):
    /// possession of the group key is sufficient to seal an envelope that
    /// opens successfully — membership of the signer is deliberately NOT
    /// checked here, because rejection at this layer would push the envelope
    /// into the shells' carry-foreign path and spread it through the mesh.
    /// The membership guard lives in both shells' deliverOpenedGroupEnvelope
    /// (drop-with-log, terminal). If this test starts failing because a
    /// membership check was added here, make sure the shells' disposition
    /// semantics were reworked to match — do not just delete the test.
    #[test]
    fn outsider_with_group_key_opens_but_is_not_a_member() {
        let outsider = generate_identity();
        let group = create_group("Family".to_string(), vec![user(1), user(2)]).unwrap();
        assert!(!group.member_user_ids.contains(&outsider.user_id));

        let sealed =
            seal_group_message(outsider.clone(), group.clone(), b"injected".to_vec()).unwrap();
        let opened = open_group_message(group, sealed).unwrap();
        assert_eq!(opened.sender_user_id, outsider.user_id);
        assert_eq!(opened.payload, b"injected");
    }

    #[test]
    fn create_group_generates_lengths_and_canonical_members() {
        let group = create_group("Family".to_string(), vec![user(2), user(1), user(1)]).unwrap();
        assert_eq!(group.id.len(), GROUP_ID_LEN);
        assert_eq!(group.key.len(), GROUP_KEY_LEN);
        assert_eq!(group.member_user_ids, vec![user(1), user(2)]);
    }

    #[test]
    fn rotate_group_preserves_id_and_name_but_changes_key() {
        let group = sample_group();
        let rotated = rotate_group(group.clone(), vec![user(1), user(4)]).unwrap();
        assert_eq!(rotated.id, group.id);
        assert_eq!(rotated.name, group.name);
        assert_ne!(rotated.key, group.key);
        assert_eq!(rotated.member_user_ids, vec![user(1), user(4)]);
    }

    #[test]
    fn group_invite_content_round_trips() {
        let encoded = encode_group_invite_content(sample_group()).unwrap();
        let decoded = decode_group_invite_content(encoded).unwrap();
        assert_eq!(
            decoded,
            Group {
                id: vec![0x11; GROUP_ID_LEN],
                name: "Bridge Crew".to_string(),
                member_user_ids: vec![user(1), user(3)],
                key: vec![0x22; GROUP_KEY_LEN],
                metadata_revision: 0,
                metadata_changed_by: Vec::new(),
            }
        );
    }

    #[test]
    fn group_metadata_round_trips_and_applies_rename() {
        let group = sample_group();
        let update = create_group_metadata_update(
            group.clone(),
            user(1),
            "Night Watch".to_string(),
            group.member_user_ids.clone(),
        )
        .unwrap();
        let decoded =
            decode_group_metadata_update(encode_group_metadata_update(update).unwrap()).unwrap();
        let applied = apply_group_metadata_update(group, decoded, user(1))
            .unwrap()
            .unwrap();
        assert_eq!(applied.name, "Night Watch");
        assert_eq!(applied.metadata_revision, 1);
        assert_eq!(applied.metadata_changed_by, user(1));
    }

    #[test]
    fn group_metadata_rejects_non_member_and_member_removal_at_authoring() {
        let group = sample_group();
        assert!(create_group_metadata_update(
            group.clone(),
            user(9),
            "Nope".to_string(),
            group.member_user_ids.clone(),
        )
        .is_err());
        assert!(
            create_group_metadata_update(group, user(1), "Nope".to_string(), vec![user(1)],)
                .is_err()
        );
    }

    #[test]
    fn concurrent_metadata_updates_converge_without_losing_member_additions() {
        let group = sample_group();
        let alice_update = create_group_metadata_update(
            group.clone(),
            user(1),
            "Alice name".to_string(),
            vec![user(1), user(3), user(4)],
        )
        .unwrap();
        let carol_update = create_group_metadata_update(
            group.clone(),
            user(3),
            "Carol name".to_string(),
            vec![user(1), user(3), user(5)],
        )
        .unwrap();

        let a_then_c = apply_group_metadata_update(
            apply_group_metadata_update(group.clone(), alice_update.clone(), user(1))
                .unwrap()
                .unwrap(),
            carol_update.clone(),
            user(3),
        )
        .unwrap()
        .unwrap();
        let c_then_a = apply_group_metadata_update(
            apply_group_metadata_update(group, carol_update, user(3))
                .unwrap()
                .unwrap(),
            alice_update,
            user(1),
        )
        .unwrap()
        .unwrap();

        assert_eq!(a_then_c, c_then_a);
        assert_eq!(a_then_c.name, "Carol name");
        assert_eq!(
            a_then_c.member_user_ids,
            vec![user(1), user(3), user(4), user(5),]
        );
    }

    #[test]
    fn group_invite_decode_rejects_truncated_input() {
        let err = decode_group_invite_content(vec![0xAA; GROUP_ID_LEN]).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn group_shapes_are_bounded_before_canonicalization() {
        assert!(create_group("Family".into(), vec![vec![1; USER_ID_LEN - 1]]).is_err());
        assert!(create_group(
            "Family".into(),
            vec![vec![1; USER_ID_LEN]; MAX_GROUP_MEMBERS + 1],
        )
        .is_err());
        assert!(create_group("x".repeat(MAX_GROUP_NAME_BYTES + 1), vec![user(1)],).is_err());

        let mut invite = vec![0x11; GROUP_ID_LEN];
        invite.extend_from_slice(&vec![0x22; GROUP_KEY_LEN]);
        write_bytes16(&mut invite, b"Family");
        invite.extend_from_slice(&((MAX_GROUP_MEMBERS + 1) as u16).to_be_bytes());
        assert!(decode_group_invite_content(invite).is_err());

        let mut metadata = vec![GROUP_METADATA_VERSION];
        metadata.extend_from_slice(&vec![0x11; GROUP_ID_LEN]);
        metadata.extend_from_slice(&1u64.to_be_bytes());
        write_bytes16(&mut metadata, &user(1));
        write_bytes16(&mut metadata, b"Family");
        metadata.extend_from_slice(&((MAX_GROUP_MEMBERS + 1) as u16).to_be_bytes());
        assert!(decode_group_metadata_update(metadata).is_err());
    }

    #[test]
    fn seal_then_open_group_message_round_trips_and_identifies_sender() {
        let alice = generate_identity();
        let group = sample_group();

        let sealed =
            seal_group_message(alice.clone(), group.clone(), b"group dinner at 7".to_vec())
                .expect("seal succeeds");
        let opened = open_group_message(group, sealed).expect("open succeeds");

        assert_eq!(opened.payload, b"group dinner at 7");
        assert_eq!(opened.sender_user_id, alice.user_id);
    }

    #[test]
    fn wrong_group_key_fails_closed() {
        let alice = generate_identity();
        let group = sample_group();
        let sealed = seal_group_message(alice, group.clone(), b"secret plan".to_vec())
            .expect("seal succeeds");
        let mut wrong_group = group;
        wrong_group.key = vec![0x33; GROUP_KEY_LEN];

        let err = open_group_message(wrong_group, sealed).unwrap_err();
        assert!(matches!(err, CoreError::Crypto(_)));
    }

    #[test]
    fn unknown_group_envelope_version_is_rejected() {
        let alice = generate_identity();
        let group = sample_group();
        let mut sealed = seal_group_message(alice, group.clone(), b"hi".to_vec()).unwrap();
        sealed[0] = 0x7F;

        let err = open_group_message(group, sealed).unwrap_err();
        assert!(matches!(err, CoreError::Crypto(_)));
    }
}
