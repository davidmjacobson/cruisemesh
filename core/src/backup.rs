//! Portable CruiseMesh identity serialization and encrypted `.cmbak` files.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use pbkdf2::pbkdf2_hmac;
use rand_core::{OsRng, RngCore};
use sha2::Sha256;

use crate::Identity;

const MAGIC: &[u8; 7] = b"CMBAK1\0";
const FORMAT_VERSION: u8 = 1;
const LEGACY_INNER_VERSION: u8 = 1;
const INNER_VERSION: u8 = 2;
const KDF_PBKDF2_HMAC_SHA256: u8 = 3;
const KDF_PARAMS_LEN: usize = 16;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const GCM_TAG_LEN: usize = 16;
const HEADER_LEN: usize = MAGIC.len() + 1 + 1 + KDF_PARAMS_LEN + SALT_LEN + NONCE_LEN;
const IDENTITY_LEN: usize = 16 + 32 + 32 + 32 + 32;
pub const PBKDF2_DEFAULT_ITERATIONS: u32 = 600_000;
pub const BACKUP_MIN_PASSPHRASE_LEN: usize = 10;

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CoreBackupError {
    #[error("not a CruiseMesh backup file")]
    BadMagic,
    #[error("unsupported backup format version {version}")]
    UnsupportedVersion { version: u8 },
    #[error("unsupported backup key derivation ({kdf_id})")]
    UnsupportedKdf { kdf_id: u8 },
    #[error("backup file is truncated or corrupt")]
    Truncated,
    #[error("incorrect passphrase or corrupt backup file")]
    WrongPassphraseOrCorrupt,
    #[error("invalid backup payload: {reason}")]
    InvalidPayload { reason: String },
}

#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CoreBackupPayload {
    pub identity: Vec<u8>,
    pub sqlite: Vec<u8>,
    pub src_version_code: i32,
    pub created_at_ms: i64,
    pub display_name: Option<String>,
    pub own_avatar: Vec<u8>,
    pub own_avatar_epoch: i64,
    pub relay_url: Option<String>,
    pub relay_token: Option<String>,
    pub share_online: bool,
}

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupPassphraseStrength {
    TooShort,
    Weak,
    Fair,
    Strong,
}

#[uniffi::export]
pub fn backup_min_passphrase_length() -> u32 {
    BACKUP_MIN_PASSPHRASE_LEN as u32
}

#[uniffi::export]
pub fn backup_passphrase_strength(passphrase: String) -> BackupPassphraseStrength {
    let chars: Vec<_> = passphrase.chars().collect();
    if chars.len() < BACKUP_MIN_PASSPHRASE_LEN {
        return BackupPassphraseStrength::TooShort;
    }
    let lower = chars.iter().any(|c| c.is_lowercase());
    let upper = chars.iter().any(|c| c.is_uppercase());
    let digit = chars.iter().any(|c| c.is_numeric());
    let symbol = chars
        .iter()
        .any(|c| !c.is_lowercase() && !c.is_uppercase() && !c.is_numeric());
    let classes = [lower, upper, digit, symbol]
        .into_iter()
        .filter(|present| *present)
        .count();
    if chars.len() >= 16 && classes >= 3 {
        BackupPassphraseStrength::Strong
    } else if chars.len() >= 14 || classes >= 3 {
        BackupPassphraseStrength::Fair
    } else {
        BackupPassphraseStrength::Weak
    }
}

#[uniffi::export]
pub fn encode_identity_bytes(identity: Identity) -> Vec<u8> {
    let mut out = Vec::with_capacity(IDENTITY_LEN);
    out.extend_from_slice(&identity.user_id);
    out.extend_from_slice(&identity.sign_pk);
    out.extend_from_slice(&identity.sign_sk);
    out.extend_from_slice(&identity.agree_pk);
    out.extend_from_slice(&identity.agree_sk);
    out
}

#[uniffi::export]
pub fn decode_identity_bytes(bytes: Vec<u8>) -> Result<Identity, CoreBackupError> {
    if bytes.len() != IDENTITY_LEN {
        return Err(CoreBackupError::InvalidPayload {
            reason: format!(
                "expected {IDENTITY_LEN} identity bytes, got {}",
                bytes.len()
            ),
        });
    }
    Ok(Identity {
        user_id: bytes[0..16].to_vec(),
        sign_pk: bytes[16..48].to_vec(),
        sign_sk: bytes[48..80].to_vec(),
        agree_pk: bytes[80..112].to_vec(),
        agree_sk: bytes[112..144].to_vec(),
    })
}

#[uniffi::export]
pub fn seal_backup(
    passphrase: String,
    payload: CoreBackupPayload,
    iterations: Option<u32>,
) -> Result<Vec<u8>, CoreBackupError> {
    let iterations = iterations.unwrap_or(PBKDF2_DEFAULT_ITERATIONS);
    if iterations == 0 {
        return Err(CoreBackupError::InvalidPayload {
            reason: "PBKDF2 iterations must be positive".into(),
        });
    }
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut nonce);
    let header = encode_header(iterations, &salt, &nonce);
    let key = derive_key(&passphrase, &salt, iterations);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("fixed AES-256 key size");
    let plaintext = encode_inner(&payload)?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: &header,
            },
        )
        .map_err(|_| CoreBackupError::InvalidPayload {
            reason: "backup encryption failed".into(),
        })?;
    let mut file = header;
    file.extend_from_slice(&ciphertext);
    Ok(file)
}

#[uniffi::export]
pub fn open_backup(
    passphrase: String,
    file: Vec<u8>,
) -> Result<CoreBackupPayload, CoreBackupError> {
    let parsed = decode_header(&file)?;
    if parsed.kdf_id != KDF_PBKDF2_HMAC_SHA256 {
        return Err(CoreBackupError::UnsupportedKdf {
            kdf_id: parsed.kdf_id,
        });
    }
    let iterations = u32::from_be_bytes(parsed.kdf_params[0..4].try_into().unwrap());
    if iterations == 0 {
        return Err(CoreBackupError::InvalidPayload {
            reason: "PBKDF2 iterations must be positive".into(),
        });
    }
    let key = derive_key(&passphrase, parsed.salt, iterations);
    let cipher = Aes256Gcm::new_from_slice(&key).expect("fixed AES-256 key size");
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(parsed.nonce),
            Payload {
                msg: parsed.ciphertext,
                aad: parsed.aad,
            },
        )
        .map_err(|_| CoreBackupError::WrongPassphraseOrCorrupt)?;
    decode_inner(&plaintext)
}

fn derive_key(passphrase: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(passphrase.as_bytes(), salt, iterations, &mut key);
    key
}

fn encode_header(iterations: u32, salt: &[u8; SALT_LEN], nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_LEN);
    out.extend_from_slice(MAGIC);
    out.push(FORMAT_VERSION);
    out.push(KDF_PBKDF2_HMAC_SHA256);
    out.extend_from_slice(&iterations.to_be_bytes());
    out.extend_from_slice(&[0; KDF_PARAMS_LEN - 4]);
    out.extend_from_slice(salt);
    out.extend_from_slice(nonce);
    out
}

struct ParsedHeader<'a> {
    kdf_id: u8,
    kdf_params: &'a [u8],
    salt: &'a [u8],
    nonce: &'a [u8],
    aad: &'a [u8],
    ciphertext: &'a [u8],
}

fn decode_header(file: &[u8]) -> Result<ParsedHeader<'_>, CoreBackupError> {
    if file.len() < HEADER_LEN + GCM_TAG_LEN {
        return Err(CoreBackupError::Truncated);
    }
    if &file[..MAGIC.len()] != MAGIC {
        return Err(CoreBackupError::BadMagic);
    }
    let version = file[MAGIC.len()];
    if version != FORMAT_VERSION {
        return Err(CoreBackupError::UnsupportedVersion { version });
    }
    let kdf_id = file[MAGIC.len() + 1];
    let params_start = MAGIC.len() + 2;
    let salt_start = params_start + KDF_PARAMS_LEN;
    let nonce_start = salt_start + SALT_LEN;
    Ok(ParsedHeader {
        kdf_id,
        kdf_params: &file[params_start..salt_start],
        salt: &file[salt_start..nonce_start],
        nonce: &file[nonce_start..HEADER_LEN],
        aad: &file[..HEADER_LEN],
        ciphertext: &file[HEADER_LEN..],
    })
}

fn encode_inner(payload: &CoreBackupPayload) -> Result<Vec<u8>, CoreBackupError> {
    if payload.identity.len() != IDENTITY_LEN {
        return Err(CoreBackupError::InvalidPayload {
            reason: format!("identity must be {IDENTITY_LEN} bytes"),
        });
    }
    let display_name = payload.display_name.as_deref().unwrap_or("").as_bytes();
    let relay_url = payload.relay_url.as_deref().unwrap_or("").as_bytes();
    let relay_token = payload.relay_token.as_deref().unwrap_or("").as_bytes();
    for (label, value) in [
        ("display name", display_name),
        ("relay URL", relay_url),
        ("relay token", relay_token),
    ] {
        if value.len() > u16::MAX as usize {
            return Err(CoreBackupError::InvalidPayload {
                reason: format!("{label} is too long"),
            });
        }
    }
    if payload.sqlite.len() > u32::MAX as usize || payload.own_avatar.len() > u32::MAX as usize {
        return Err(CoreBackupError::InvalidPayload {
            reason: "backup blob is too large".into(),
        });
    }

    let mut out = Vec::new();
    out.push(INNER_VERSION);
    out.extend_from_slice(&payload.src_version_code.to_be_bytes());
    out.extend_from_slice(&payload.created_at_ms.to_be_bytes());
    write_bytes16(&mut out, &payload.identity);
    write_bytes32(&mut out, &payload.sqlite);
    write_bytes16(&mut out, display_name);
    write_bytes32(&mut out, &payload.own_avatar);
    out.extend_from_slice(&payload.own_avatar_epoch.to_be_bytes());
    write_bytes16(&mut out, relay_url);
    write_bytes16(&mut out, relay_token);
    out.push(u8::from(payload.share_online));
    Ok(out)
}

fn decode_inner(bytes: &[u8]) -> Result<CoreBackupPayload, CoreBackupError> {
    let mut cursor = Cursor::new(bytes);
    let version = cursor.u8()?;
    if version != LEGACY_INNER_VERSION && version != INNER_VERSION {
        return Err(CoreBackupError::UnsupportedVersion { version });
    }
    let src_version_code = cursor.i32()?;
    let created_at_ms = cursor.i64()?;
    let identity = cursor.bytes16()?.to_vec();
    if identity.len() != IDENTITY_LEN {
        return Err(CoreBackupError::Truncated);
    }
    let sqlite = cursor.bytes32()?.to_vec();
    if version == LEGACY_INNER_VERSION {
        return Ok(CoreBackupPayload {
            identity,
            sqlite,
            src_version_code,
            created_at_ms,
            display_name: None,
            own_avatar: Vec::new(),
            own_avatar_epoch: 0,
            relay_url: None,
            relay_token: None,
            share_online: true,
        });
    }
    let display_name = empty_to_none(cursor.string16()?);
    let own_avatar = cursor.bytes32()?.to_vec();
    let own_avatar_epoch = cursor.i64()?;
    let relay_url = empty_to_none(cursor.string16()?);
    let relay_token = empty_to_none(cursor.string16()?);
    let share_online = match cursor.u8()? {
        0 => false,
        1 => true,
        _ => return Err(CoreBackupError::Truncated),
    };
    if !cursor.finished() {
        return Err(CoreBackupError::Truncated);
    }
    Ok(CoreBackupPayload {
        identity,
        sqlite,
        src_version_code,
        created_at_ms,
        display_name,
        own_avatar,
        own_avatar_epoch,
        relay_url,
        relay_token,
        share_online,
    })
}

fn empty_to_none(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn write_bytes16(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}

fn write_bytes32(out: &mut Vec<u8>, value: &[u8]) {
    out.extend_from_slice(&(value.len() as u32).to_be_bytes());
    out.extend_from_slice(value);
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], CoreBackupError> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(CoreBackupError::Truncated)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(CoreBackupError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, CoreBackupError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, CoreBackupError> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, CoreBackupError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, CoreBackupError> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i64(&mut self) -> Result<i64, CoreBackupError> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn bytes16(&mut self) -> Result<&'a [u8], CoreBackupError> {
        let count = self.u16()? as usize;
        self.take(count)
    }

    fn bytes32(&mut self) -> Result<&'a [u8], CoreBackupError> {
        let count = self.u32()? as usize;
        self.take(count)
    }

    fn string16(&mut self) -> Result<String, CoreBackupError> {
        String::from_utf8(self.bytes16()?.to_vec()).map_err(|_| CoreBackupError::Truncated)
    }

    fn finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_identity;

    fn payload() -> CoreBackupPayload {
        CoreBackupPayload {
            identity: encode_identity_bytes(generate_identity()),
            sqlite: vec![1, 2, 3, 4],
            src_version_code: 17,
            created_at_ms: 1_700_000_000_000,
            display_name: Some("Alice".into()),
            own_avatar: vec![9, 8, 7],
            own_avatar_epoch: 3,
            relay_url: Some("https://relay.example".into()),
            relay_token: Some("secret".into()),
            share_online: false,
        }
    }

    #[test]
    fn identity_round_trips() {
        let identity = generate_identity();
        assert_eq!(
            encode_identity_bytes(
                decode_identity_bytes(encode_identity_bytes(identity.clone())).unwrap()
            ),
            encode_identity_bytes(identity)
        );
        assert!(decode_identity_bytes(vec![0; 143]).is_err());
    }

    #[test]
    fn backup_round_trips_and_is_randomized() {
        let payload = payload();
        let one = seal_backup(
            "correct horse battery staple".into(),
            payload.clone(),
            Some(10),
        )
        .unwrap();
        let two = seal_backup(
            "correct horse battery staple".into(),
            payload.clone(),
            Some(10),
        )
        .unwrap();
        assert_ne!(one, two);
        assert_eq!(
            open_backup("correct horse battery staple".into(), one).unwrap(),
            payload
        );
    }

    #[test]
    fn wrong_passphrase_and_tampering_fail() {
        let mut file = seal_backup("right".into(), payload(), Some(10)).unwrap();
        assert!(matches!(
            open_backup("wrong".into(), file.clone()),
            Err(CoreBackupError::WrongPassphraseOrCorrupt)
        ));
        let last = file.len() - 1;
        file[last] ^= 1;
        assert!(matches!(
            open_backup("right".into(), file),
            Err(CoreBackupError::WrongPassphraseOrCorrupt)
        ));
    }

    #[test]
    fn header_errors_are_typed() {
        assert!(matches!(
            open_backup("pw".into(), vec![]),
            Err(CoreBackupError::Truncated)
        ));
        assert!(matches!(
            open_backup("pw".into(), vec![0x55; 200]),
            Err(CoreBackupError::BadMagic)
        ));
    }

    #[test]
    fn passphrase_policy_matches_existing_clients() {
        assert_eq!(
            backup_passphrase_strength("short".into()),
            BackupPassphraseStrength::TooShort
        );
        assert_eq!(
            backup_passphrase_strength("abcdefghijkl".into()),
            BackupPassphraseStrength::Weak
        );
        assert_eq!(
            backup_passphrase_strength("Abcdef12jk".into()),
            BackupPassphraseStrength::Fair
        );
        assert_eq!(
            backup_passphrase_strength("Correct-Horse-99-Battery".into()),
            BackupPassphraseStrength::Strong
        );
    }
}
