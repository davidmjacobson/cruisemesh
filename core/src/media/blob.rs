//! Blob identity: encrypt first, then name the ciphertext.
//!
//! The spec's ordering is the whole security posture of the plane: "blobs are
//! encrypted before they are named". A blob id is the digest of the
//! *ciphertext*, so any party that stores or serves bytes — a peer today, a
//! relay in phase 2 — can verify what it holds and what it hands over without
//! ever being able to read it. The key travels only inside the sealed
//! manifest ([`super::manifest`]), which is ordinary sealed message content.
//!
//! # Chunks are sealed one at a time, deliberately
//!
//! A blob is not one AEAD box. Each chunk is sealed independently with the
//! blob key under a deterministic nonce, so:
//!
//! * a recipient can authenticate a chunk **the moment it arrives**, which is
//!   what makes the corrupted-chunk recovery path in [`super::lan_pull`]
//!   possible at all — a bad chunk is re-marked missing instead of poisoning
//!   an assembly that only fails hours later;
//! * chunk boundaries are a multiplication, identical at every source;
//! * a partial transfer on disk is decryptable as far as it goes, so nothing
//!   has to be buffered in memory to make progress.
//!
//! Reordering and truncation are prevented by binding the chunk index, the
//! chunk count and the plaintext length into each chunk's associated data:
//! a chunk lifted from one blob, or replayed at another index, fails to open.
//!
//! # Determinism
//!
//! Sealing is deterministic given the key and the plaintext — the nonces are
//! derived, not random. That is safe here because the key is fresh per blob
//! (see [`generate_blob_key`]) and never reused, and it buys two things worth
//! having: golden vectors for the encoder, and the guarantee that two devices
//! sealing the same bytes under the same key produce the same blob id, so a
//! transfer that begins against one source can finish against another.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{Key, KeyInit, XChaCha20Poly1305, XNonce};
use rand_core::{OsRng, RngCore};

use super::{
    MediaError, MEDIA_AEAD_TAG_BYTES, MEDIA_BLOB_MAX_BYTES, MEDIA_CHUNK_CIPHERTEXT_BYTES,
    MEDIA_CHUNK_PLAINTEXT_BYTES,
};

pub const BLOB_ID_LEN: usize = 32;
pub const BLOB_KEY_LEN: usize = 32;

/// Domain separator mixed into every chunk's associated data. A future
/// chunking scheme changes this string rather than reusing it, so a chunk
/// sealed under v1 can never be opened as a v2 chunk of the same blob.
const CHUNK_AAD_DOMAIN: &[u8] = b"cruisemesh.media.blob/v1";
const NONCE_PREFIX: &[u8; 8] = b"cmblob01";

/// The permanent name of a blob: a 32-byte digest of its ciphertext.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobId(pub [u8; BLOB_ID_LEN]);

impl BlobId {
    pub fn as_bytes(&self) -> &[u8; BLOB_ID_LEN] {
        &self.0
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self, MediaError> {
        let arr: [u8; BLOB_ID_LEN] = bytes
            .try_into()
            .map_err(|_| MediaError::Malformed(format!("blob id is {} bytes", bytes.len())))?;
        Ok(BlobId(arr))
    }

    /// Short prefix for logs and transcripts. A blob id is a digest of
    /// ciphertext and carries no plaintext, but it is still a correlatable
    /// identifier, so nothing prints more of it than this.
    pub fn short(&self) -> String {
        self.0[..4].iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl std::fmt::Debug for BlobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "BlobId({}…)", self.short())
    }
}

/// A fresh per-blob symmetric key. Lives only inside a sealed manifest.
#[derive(Clone, PartialEq, Eq)]
pub struct BlobKey(pub [u8; BLOB_KEY_LEN]);

impl BlobKey {
    pub fn from_slice(bytes: &[u8]) -> Result<Self, MediaError> {
        let arr: [u8; BLOB_KEY_LEN] = bytes
            .try_into()
            .map_err(|_| MediaError::Malformed(format!("blob key is {} bytes", bytes.len())))?;
        Ok(BlobKey(arr))
    }

    pub fn as_bytes(&self) -> &[u8; BLOB_KEY_LEN] {
        &self.0
    }
}

/// Never print key material, not even a prefix of it. `SECRET-01` is about
/// exports and events, but a `Debug` impl is how key material reaches one.
impl std::fmt::Debug for BlobKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BlobKey(redacted)")
    }
}

/// A fresh key from the OS CSPRNG. One per blob, never derived from anything
/// the sender reuses: a repeated key with derived nonces would be a nonce
/// reuse across two different plaintexts.
pub fn generate_blob_key() -> BlobKey {
    let mut key = [0u8; BLOB_KEY_LEN];
    OsRng.fill_bytes(&mut key);
    BlobKey(key)
}

/// How a blob's plaintext maps onto chunks and ciphertext offsets.
///
/// Everything here is derived from one number — the plaintext length — so two
/// devices that agree on the manifest agree on the geometry without another
/// round trip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlobGeometry {
    pub plaintext_bytes: u64,
    pub ciphertext_bytes: u64,
    pub chunk_count: u32,
    pub chunk_plaintext_bytes: u32,
    pub chunk_ciphertext_bytes: u32,
}

impl BlobGeometry {
    pub fn for_plaintext_len(plaintext_bytes: u64) -> Result<Self, MediaError> {
        if plaintext_bytes == 0 {
            return Err(MediaError::EmptyBlob);
        }
        if plaintext_bytes > MEDIA_BLOB_MAX_BYTES {
            return Err(MediaError::BlobTooLarge {
                actual: plaintext_bytes,
                max: MEDIA_BLOB_MAX_BYTES,
            });
        }
        let chunk_plaintext = u64::from(MEDIA_CHUNK_PLAINTEXT_BYTES);
        let chunk_count = plaintext_bytes.div_ceil(chunk_plaintext);
        Ok(BlobGeometry {
            plaintext_bytes,
            ciphertext_bytes: plaintext_bytes + chunk_count * u64::from(MEDIA_AEAD_TAG_BYTES),
            chunk_count: chunk_count as u32,
            chunk_plaintext_bytes: MEDIA_CHUNK_PLAINTEXT_BYTES,
            chunk_ciphertext_bytes: MEDIA_CHUNK_CIPHERTEXT_BYTES,
        })
    }

    /// Plaintext length of chunk `index`, or `None` past the end.
    pub fn chunk_plaintext_len(&self, index: u32) -> Option<u32> {
        if index >= self.chunk_count {
            return None;
        }
        let start = u64::from(index) * u64::from(self.chunk_plaintext_bytes);
        let remaining = self.plaintext_bytes - start;
        Some(remaining.min(u64::from(self.chunk_plaintext_bytes)) as u32)
    }

    /// Ciphertext length of chunk `index`, or `None` past the end.
    pub fn chunk_ciphertext_len(&self, index: u32) -> Option<u32> {
        self.chunk_plaintext_len(index)
            .map(|len| len + MEDIA_AEAD_TAG_BYTES)
    }

    /// Byte offset of chunk `index` inside the ciphertext stream.
    pub fn chunk_ciphertext_offset(&self, index: u32) -> Option<u64> {
        (index < self.chunk_count)
            .then(|| u64::from(index) * u64::from(self.chunk_ciphertext_bytes))
    }
}

/// A sealed blob, ready to be written to disk and served.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedBlob {
    pub id: BlobId,
    pub geometry: BlobGeometry,
    pub ciphertext: Vec<u8>,
}

/// Seal one chunk. The index, the chunk count and the plaintext length are
/// authenticated but not transmitted: they come from the manifest, so a chunk
/// that claims to be chunk 3 of a different blob cannot open as chunk 3 of
/// this one.
pub fn seal_chunk(
    key: &BlobKey,
    geometry: &BlobGeometry,
    index: u32,
    plaintext: &[u8],
) -> Result<Vec<u8>, MediaError> {
    let expected = geometry
        .chunk_plaintext_len(index)
        .ok_or_else(|| MediaError::Malformed(format!("chunk index {index} is past the end")))?;
    if plaintext.len() != expected as usize {
        return Err(MediaError::Malformed(format!(
            "chunk {index} is {} bytes, geometry says {expected}",
            plaintext.len()
        )));
    }
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key.as_bytes()));
    let aad = chunk_aad(geometry, index);
    cipher
        .encrypt(
            &chunk_nonce(index),
            Payload {
                msg: plaintext,
                aad: &aad,
            },
        )
        .map_err(|_| MediaError::Malformed(format!("chunk {index} would not seal")))
}

/// Open one chunk, or reject it. A rejection is never a partial result: the
/// caller gets no bytes at all, which is what lets [`super::lan_pull`] treat a
/// failure as "this chunk is still missing" rather than as "this chunk is
/// present but wrong".
pub fn open_chunk(
    key: &BlobKey,
    geometry: &BlobGeometry,
    index: u32,
    ciphertext: &[u8],
) -> Result<Vec<u8>, MediaError> {
    let expected = geometry
        .chunk_ciphertext_len(index)
        .ok_or_else(|| MediaError::Malformed(format!("chunk index {index} is past the end")))?;
    if ciphertext.len() != expected as usize {
        return Err(MediaError::ChunkAuthFailed { index });
    }
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key.as_bytes()));
    let aad = chunk_aad(geometry, index);
    cipher
        .decrypt(
            &chunk_nonce(index),
            Payload {
                msg: ciphertext,
                aad: &aad,
            },
        )
        .map_err(|_| MediaError::ChunkAuthFailed { index })
}

/// Seal a whole blob and name it. The sender's path.
pub fn seal_blob(key: &BlobKey, plaintext: &[u8]) -> Result<SealedBlob, MediaError> {
    let geometry = BlobGeometry::for_plaintext_len(plaintext.len() as u64)?;
    let mut ciphertext = Vec::with_capacity(geometry.ciphertext_bytes as usize);
    let mut hasher = BlobIdHasher::new();
    for index in 0..geometry.chunk_count {
        let start = index as usize * geometry.chunk_plaintext_bytes as usize;
        let len = geometry
            .chunk_plaintext_len(index)
            .expect("index is inside chunk_count") as usize;
        let sealed = seal_chunk(key, &geometry, index, &plaintext[start..start + len])?;
        hasher.update(&sealed);
        ciphertext.extend_from_slice(&sealed);
    }
    Ok(SealedBlob {
        id: hasher.finish(),
        geometry,
        ciphertext,
    })
}

/// Open a whole blob whose ciphertext has already been verified against its
/// manifest digest. `BLOB-05` is the reason this takes the id: it re-checks
/// rather than trusting the caller to have done it.
pub fn open_blob(
    key: &BlobKey,
    id: &BlobId,
    geometry: &BlobGeometry,
    ciphertext: &[u8],
) -> Result<Vec<u8>, MediaError> {
    verify_assembled(id, geometry, ciphertext)?;
    let mut plaintext = Vec::with_capacity(geometry.plaintext_bytes as usize);
    for index in 0..geometry.chunk_count {
        let offset = geometry
            .chunk_ciphertext_offset(index)
            .expect("index is inside chunk_count") as usize;
        let len = geometry
            .chunk_ciphertext_len(index)
            .expect("index is inside chunk_count") as usize;
        plaintext.extend_from_slice(&open_chunk(
            key,
            geometry,
            index,
            &ciphertext[offset..offset + len],
        )?);
    }
    Ok(plaintext)
}

/// The digest of a complete ciphertext stream.
pub fn blob_id_for_ciphertext(ciphertext: &[u8]) -> BlobId {
    let mut hasher = BlobIdHasher::new();
    hasher.update(ciphertext);
    hasher.finish()
}

/// `BLOB-05`, as one function: assembled bytes are only ever trusted after
/// their length *and* their digest match the manifest.
pub fn verify_assembled(
    id: &BlobId,
    geometry: &BlobGeometry,
    ciphertext: &[u8],
) -> Result<(), MediaError> {
    if ciphertext.len() as u64 != geometry.ciphertext_bytes {
        return Err(MediaError::DigestMismatch);
    }
    if blob_id_for_ciphertext(ciphertext) != *id {
        return Err(MediaError::DigestMismatch);
    }
    Ok(())
}

/// Streaming form of the blob digest, for a recipient that assembles chunks
/// on disk and has no reason to hold a 128 MB buffer to verify them.
/// Chunks must be fed in index order.
pub struct BlobIdHasher {
    hasher: Blake2bVar,
}

impl Default for BlobIdHasher {
    fn default() -> Self {
        Self::new()
    }
}

impl BlobIdHasher {
    pub fn new() -> Self {
        BlobIdHasher {
            hasher: Blake2bVar::new(BLOB_ID_LEN).expect("valid blake2b output length"),
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        Update::update(&mut self.hasher, bytes);
    }

    pub fn finish(self) -> BlobId {
        let mut out = [0u8; BLOB_ID_LEN];
        self.hasher
            .finalize_variable(&mut out)
            .expect("output buffer is BLOB_ID_LEN");
        BlobId(out)
    }
}

fn chunk_nonce(index: u32) -> XNonce {
    let mut nonce = [0u8; 24];
    nonce[..NONCE_PREFIX.len()].copy_from_slice(NONCE_PREFIX);
    nonce[16..24].copy_from_slice(&u64::from(index).to_be_bytes());
    *XNonce::from_slice(&nonce)
}

fn chunk_aad(geometry: &BlobGeometry, index: u32) -> Vec<u8> {
    let mut aad = Vec::with_capacity(CHUNK_AAD_DOMAIN.len() + 16);
    aad.extend_from_slice(CHUNK_AAD_DOMAIN);
    aad.extend_from_slice(&geometry.chunk_count.to_be_bytes());
    aad.extend_from_slice(&geometry.plaintext_bytes.to_be_bytes());
    aad.extend_from_slice(&index.to_be_bytes());
    aad
}

#[cfg(test)]
pub(crate) fn test_key(seed: u8) -> BlobKey {
    BlobKey([seed; BLOB_KEY_LEN])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plaintext(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn geometry_is_derived_from_one_number() {
        let one_chunk = BlobGeometry::for_plaintext_len(1_000).unwrap();
        assert_eq!(one_chunk.chunk_count, 1);
        assert_eq!(one_chunk.ciphertext_bytes, 1_000 + 16);
        assert_eq!(one_chunk.chunk_plaintext_len(0), Some(1_000));
        assert_eq!(one_chunk.chunk_plaintext_len(1), None);

        let exact =
            BlobGeometry::for_plaintext_len(u64::from(MEDIA_CHUNK_PLAINTEXT_BYTES)).unwrap();
        assert_eq!(
            exact.chunk_count, 1,
            "an exact multiple gains no empty tail"
        );

        let three = BlobGeometry::for_plaintext_len(u64::from(MEDIA_CHUNK_PLAINTEXT_BYTES) * 2 + 7)
            .unwrap();
        assert_eq!(three.chunk_count, 3);
        assert_eq!(three.chunk_plaintext_len(2), Some(7));
        assert_eq!(three.chunk_ciphertext_len(2), Some(23));
        // Every chunk but the last is full, so an offset is a multiplication.
        assert_eq!(
            three.chunk_ciphertext_offset(2),
            Some(2 * u64::from(MEDIA_CHUNK_CIPHERTEXT_BYTES))
        );
        assert_eq!(three.chunk_ciphertext_offset(3), None);
    }

    #[test]
    fn empty_and_oversized_blobs_are_refused() {
        assert_eq!(
            BlobGeometry::for_plaintext_len(0).unwrap_err(),
            MediaError::EmptyBlob
        );
        assert_eq!(
            BlobGeometry::for_plaintext_len(MEDIA_BLOB_MAX_BYTES + 1).unwrap_err(),
            MediaError::BlobTooLarge {
                actual: MEDIA_BLOB_MAX_BYTES + 1,
                max: MEDIA_BLOB_MAX_BYTES,
            }
        );
        assert!(BlobGeometry::for_plaintext_len(MEDIA_BLOB_MAX_BYTES).is_ok());
    }

    #[test]
    fn seal_then_open_round_trips_across_chunk_boundaries() {
        let key = test_key(7);
        for len in [1usize, 4_096, 262_144, 262_145, 700_000] {
            let bytes = plaintext(len);
            let sealed = seal_blob(&key, &bytes).unwrap();
            assert_eq!(
                sealed.ciphertext.len() as u64,
                sealed.geometry.ciphertext_bytes
            );
            let opened = open_blob(&key, &sealed.id, &sealed.geometry, &sealed.ciphertext).unwrap();
            assert_eq!(opened, bytes, "round trip failed at {len} bytes");
        }
    }

    #[test]
    fn sealing_is_deterministic_and_names_the_ciphertext() {
        let key = test_key(3);
        let bytes = plaintext(5_000);
        let first = seal_blob(&key, &bytes).unwrap();
        let second = seal_blob(&key, &bytes).unwrap();
        assert_eq!(
            first, second,
            "derived nonces must make sealing reproducible"
        );

        // BLOB-02: the id names the ciphertext, so it is computable by a
        // party that cannot read the blob.
        assert_eq!(first.id, blob_id_for_ciphertext(&first.ciphertext));
        assert_ne!(
            first.id,
            blob_id_for_ciphertext(&bytes),
            "the plaintext must not be what is named"
        );
    }

    #[test]
    fn a_golden_vector_pins_the_chunk_layout() {
        // Deterministic sealing means a byte-for-byte vector is possible, and
        // a vector is the only thing that catches an accidental nonce, AAD or
        // ordering change that still round-trips against itself.
        let key = test_key(0xA5);
        let sealed = seal_blob(&key, b"cruisemesh media golden vector").unwrap();
        assert_eq!(hex(&sealed.ciphertext), "8256de9e3b0a1a8f6f62edc8270493af7fb84323d81e382f7b4b5173ffe9b58a64f6febfa37923fec6cd91d91c73");
        assert_eq!(
            hex(sealed.id.as_bytes()),
            "ef0111e2bb06e6bba3f173587113056d7e43e7331495d183f02c865d01c7f350"
        );
    }

    #[test]
    fn a_chunk_cannot_be_replayed_at_another_index_or_from_another_blob() {
        let key = test_key(9);
        let bytes = plaintext(600_000);
        let sealed = seal_blob(&key, &bytes).unwrap();
        let geometry = sealed.geometry;
        let chunk0 = &sealed.ciphertext[..geometry.chunk_ciphertext_len(0).unwrap() as usize];

        assert!(open_chunk(&key, &geometry, 0, chunk0).is_ok());
        assert_eq!(
            open_chunk(&key, &geometry, 1, chunk0).unwrap_err(),
            MediaError::ChunkAuthFailed { index: 1 },
            "the index is authenticated, so a reorder must not open"
        );

        // A chunk from a blob of a different length has different associated
        // data even under the same key.
        let other = seal_blob(&key, &plaintext(600_001)).unwrap();
        let other_chunk0 =
            &other.ciphertext[..other.geometry.chunk_ciphertext_len(0).unwrap() as usize];
        assert_eq!(
            open_chunk(&key, &geometry, 0, other_chunk0).unwrap_err(),
            MediaError::ChunkAuthFailed { index: 0 }
        );
    }

    #[test]
    fn a_flipped_bit_fails_its_chunk_and_the_whole_blob() {
        let key = test_key(11);
        let sealed = seal_blob(&key, &plaintext(300_000)).unwrap();
        let mut corrupt = sealed.ciphertext.clone();
        corrupt[10] ^= 0x01;

        assert_eq!(
            verify_assembled(&sealed.id, &sealed.geometry, &corrupt).unwrap_err(),
            MediaError::DigestMismatch
        );
        assert_eq!(
            open_chunk(
                &key,
                &sealed.geometry,
                0,
                &corrupt[..sealed.geometry.chunk_ciphertext_len(0).unwrap() as usize]
            )
            .unwrap_err(),
            MediaError::ChunkAuthFailed { index: 0 },
            "a corrupt chunk is detectable on arrival, not only at assembly"
        );
        assert_eq!(
            open_blob(&key, &sealed.id, &sealed.geometry, &corrupt).unwrap_err(),
            MediaError::DigestMismatch
        );
    }

    #[test]
    fn a_truncated_blob_never_verifies() {
        let key = test_key(13);
        let sealed = seal_blob(&key, &plaintext(300_000)).unwrap();
        let truncated = &sealed.ciphertext[..sealed.ciphertext.len() - 1];
        assert_eq!(
            verify_assembled(&sealed.id, &sealed.geometry, truncated).unwrap_err(),
            MediaError::DigestMismatch
        );
    }

    #[test]
    fn the_wrong_key_opens_nothing() {
        let sealed = seal_blob(&test_key(1), &plaintext(1_000)).unwrap();
        assert_eq!(
            open_blob(
                &test_key(2),
                &sealed.id,
                &sealed.geometry,
                &sealed.ciphertext
            )
            .unwrap_err(),
            MediaError::ChunkAuthFailed { index: 0 },
            "the digest still matches — the ciphertext is the same bytes — but the key does not"
        );
    }

    #[test]
    fn the_streaming_hasher_matches_the_one_shot_digest() {
        let sealed = seal_blob(&test_key(5), &plaintext(700_000)).unwrap();
        let mut hasher = BlobIdHasher::new();
        for index in 0..sealed.geometry.chunk_count {
            let offset = sealed.geometry.chunk_ciphertext_offset(index).unwrap() as usize;
            let len = sealed.geometry.chunk_ciphertext_len(index).unwrap() as usize;
            hasher.update(&sealed.ciphertext[offset..offset + len]);
        }
        assert_eq!(hasher.finish(), sealed.id);
    }

    #[test]
    fn key_material_never_reaches_a_debug_string() {
        let key = BlobKey([0xAB; BLOB_KEY_LEN]);
        let printed = format!("{key:?}");
        assert_eq!(printed, "BlobKey(redacted)");
        assert!(!printed.contains("ab"));
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
