//! The blob plane's only UniFFI boundary.
//!
//! Everything else in this tree is written for Rust and stays that way: a
//! [`BlobId`] is a `[u8; 32]`, a [`MediaManifest`] holds two of them, and a
//! pull session is a `&mut self` state machine. None of those is a shape a
//! binding can carry. So the boundary lives here, as mirrors, and the rest of
//! the plane is spared having its types chosen by what Kotlin and Swift can
//! spell.
//!
//! # What crosses, and what deliberately does not
//!
//! Phase 1 is authoring and recognition: a shell seals bytes, describes them,
//! hands the body to the ordinary message pipeline, and recognizes one coming
//! back. That plus the consent verdict, the filename rule and the sizes is the
//! whole surface. The pull and serve state machines are *not* here. They would
//! need `uniffi::Object` wrappers around a `Mutex` — the shape
//! [`crate::LanNoiseSession`] already uses — and no driver exists to call
//! them, so exporting them now would freeze a surface for nobody. Phase 2 owns
//! that, once it knows what it needs.
//!
//! # Errors
//!
//! [`MediaError`] stays a plain `thiserror` inside the tree and is mapped to
//! [`CoreError`] here. That is one boundary error type for the whole crate
//! rather than a second one to teach both shells, and it costs nothing the
//! plane needs: every media failure is either malformed input or the store,
//! which is exactly what `CoreError` already says.
//!
//! # Lengths are validated, never trusted
//!
//! A mirrored blob id or key arrives as a `Vec<u8>` of whatever length the
//! caller passed. [`BlobId::from_slice`] and [`BlobKey::from_slice`] are the
//! only way back to the fixed arrays, so a wrong length is a
//! [`CoreError::Malformed`] at the boundary rather than a panic behind it.

use crate::CoreError;

use super::blob::{BlobId, BlobKey};
use super::integration::{
    media_manifest_body, recognize_media_manifest, seal_media_blob, MEDIA_MANIFEST_KIND,
};
use super::manifest::{MediaKind, MediaManifest};
use super::MediaError;

/// The message kind a manifest rides, for a shell that authors one.
#[uniffi::export]
pub fn media_manifest_kind() -> u8 {
    MEDIA_MANIFEST_KIND
}

/// Mirror of [`MediaKind`].
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreMediaKind {
    Photo,
    Video,
    File,
}

/// Mirror of [`MediaManifest`]: the same fields, with the two digests widened
/// to byte arrays because a binding has no fixed-width one.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreMediaManifest {
    /// 32 bytes, the digest of the ciphertext.
    pub blob_id: Vec<u8>,
    /// 32 bytes. Sealed to each recipient by the ordinary message pipeline;
    /// nothing on either shell stores it outside a manifest.
    pub blob_key: Vec<u8>,
    pub plaintext_bytes: u64,
    pub kind: CoreMediaKind,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u32,
    /// Empty is the absent case, as on the wire. Display metadata only — a
    /// shell that writes it to disk runs it through
    /// [`super::sanitize_media_filename`] first.
    pub filename: String,
    pub thumbnail: Vec<u8>,
    pub caption: String,
}

/// Mirror of [`super::integration::AuthoredMediaBlob`], flattened: the id, the
/// key, the length the geometry derives from, and the bytes to serve.
///
/// The ciphertext crosses whole. That is honest for phase 1, where the caller
/// already holds the plaintext in memory to hand it here; a driver that seals
/// a 128 MiB clip streams instead, and that is phase 2's boundary to design.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreSealedMediaBlob {
    pub blob_id: Vec<u8>,
    pub blob_key: Vec<u8>,
    pub plaintext_bytes: u64,
    pub ciphertext: Vec<u8>,
}

/// The sender's first step: a fresh key, the ciphertext, and the digest that
/// names it. See [`seal_media_blob`].
#[uniffi::export]
pub fn core_media_seal_blob(plaintext: Vec<u8>) -> Result<CoreSealedMediaBlob, CoreError> {
    let authored = seal_media_blob(&plaintext).map_err(core_error)?;
    Ok(CoreSealedMediaBlob {
        blob_id: authored.sealed.id.as_bytes().to_vec(),
        blob_key: authored.key.as_bytes().to_vec(),
        plaintext_bytes: authored.sealed.geometry.plaintext_bytes,
        ciphertext: authored.sealed.ciphertext,
    })
}

/// The sender's second step: the `content` of a [`media_manifest_kind`]
/// message, to hand to the ordinary authoring call unchanged.
#[uniffi::export]
pub fn core_media_manifest_body(manifest: CoreMediaManifest) -> Result<Vec<u8>, CoreError> {
    media_manifest_body(&manifest_from(manifest)?).map_err(core_error)
}

/// The receive side: a delivered body, or `None` for every kind but the
/// manifest kind and for a legacy inline attachment carried under it. See
/// [`recognize_media_manifest`].
#[uniffi::export]
pub fn core_media_recognize_manifest(kind: u8, content: Vec<u8>) -> Option<CoreMediaManifest> {
    recognize_media_manifest(kind, &content).map(manifest_into)
}

/// Media failures are malformed input or the store, which is what `CoreError`
/// already says. Nothing here reaches for a third meaning.
fn core_error(err: MediaError) -> CoreError {
    match err {
        MediaError::Store(detail) => CoreError::Store(detail),
        other => CoreError::Malformed(other.to_string()),
    }
}

fn manifest_from(manifest: CoreMediaManifest) -> Result<MediaManifest, CoreError> {
    Ok(MediaManifest {
        blob_id: BlobId::from_slice(&manifest.blob_id).map_err(core_error)?,
        blob_key: BlobKey::from_slice(&manifest.blob_key).map_err(core_error)?,
        plaintext_bytes: manifest.plaintext_bytes,
        kind: match manifest.kind {
            CoreMediaKind::Photo => MediaKind::Photo,
            CoreMediaKind::Video => MediaKind::Video,
            CoreMediaKind::File => MediaKind::File,
        },
        mime_type: manifest.mime_type,
        width: manifest.width,
        height: manifest.height,
        duration_ms: manifest.duration_ms,
        filename: manifest.filename,
        thumbnail: manifest.thumbnail,
        caption: manifest.caption,
    })
}

fn manifest_into(manifest: MediaManifest) -> CoreMediaManifest {
    CoreMediaManifest {
        blob_id: manifest.blob_id.as_bytes().to_vec(),
        blob_key: manifest.blob_key.as_bytes().to_vec(),
        plaintext_bytes: manifest.plaintext_bytes,
        kind: match manifest.kind {
            MediaKind::Photo => CoreMediaKind::Photo,
            MediaKind::Video => CoreMediaKind::Video,
            MediaKind::File => CoreMediaKind::File,
        },
        mime_type: manifest.mime_type,
        width: manifest.width,
        height: manifest.height,
        duration_ms: manifest.duration_ms,
        filename: manifest.filename,
        thumbnail: manifest.thumbnail,
        caption: manifest.caption,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{media_blob_max_bytes, media_thumbnail_max_bytes, MEDIA_BLOB_MAX_BYTES};
    use crate::{decode_message_body, encode_message_body, MessageBody};

    fn photo(sealed: &CoreSealedMediaBlob) -> CoreMediaManifest {
        CoreMediaManifest {
            blob_id: sealed.blob_id.clone(),
            blob_key: sealed.blob_key.clone(),
            plaintext_bytes: sealed.plaintext_bytes,
            kind: CoreMediaKind::Photo,
            mime_type: "image/jpeg".into(),
            width: 4_032,
            height: 3_024,
            duration_ms: 0,
            filename: String::new(),
            thumbnail: vec![0xAB; 2_048],
            caption: "the fjords".into(),
        }
    }

    #[test]
    fn the_authoring_pair_round_trips_through_the_boundary() {
        // The whole phase-1 surface as a shell will use it: seal, describe,
        // encode under the manifest kind, and recognize what comes back.
        let sealed = core_media_seal_blob(b"a full-resolution picture".to_vec()).unwrap();
        let manifest = photo(&sealed);
        let content = core_media_manifest_body(manifest.clone()).unwrap();

        let body = MessageBody {
            kind: media_manifest_kind(),
            chat_id: b"alice-id".to_vec(),
            lamport: 4,
            timestamp: 1_700_000_000_000,
            content,
        };
        let decoded = decode_message_body(encode_message_body(body).unwrap()).unwrap();
        assert_eq!(
            core_media_recognize_manifest(decoded.kind, decoded.content),
            Some(manifest)
        );
    }

    #[test]
    fn a_file_manifest_carries_its_filename_and_no_thumbnail() {
        let sealed = core_media_seal_blob(b"%PDF-1.7 the itinerary".to_vec()).unwrap();
        let manifest = CoreMediaManifest {
            kind: CoreMediaKind::File,
            mime_type: "application/pdf".into(),
            width: 0,
            height: 0,
            filename: "Itinerary.pdf".into(),
            thumbnail: Vec::new(),
            ..photo(&sealed)
        };
        let body = core_media_manifest_body(manifest.clone()).unwrap();
        let back = core_media_recognize_manifest(media_manifest_kind(), body).unwrap();
        assert_eq!(back, manifest);
        assert_eq!(back.filename, "Itinerary.pdf");
    }

    #[test]
    fn a_digest_of_the_wrong_length_is_refused_rather_than_panicked_on() {
        // The mirror widens two fixed arrays into byte vectors, so the length
        // check is the boundary's job and nothing behind it may assume.
        let sealed = core_media_seal_blob(b"bytes".to_vec()).unwrap();
        for broken in [Vec::new(), vec![0u8; 31], vec![0u8; 33]] {
            let short_id = CoreMediaManifest {
                blob_id: broken.clone(),
                ..photo(&sealed)
            };
            assert!(matches!(
                core_media_manifest_body(short_id),
                Err(CoreError::Malformed(_))
            ));
            let short_key = CoreMediaManifest {
                blob_key: broken,
                ..photo(&sealed)
            };
            assert!(matches!(
                core_media_manifest_body(short_key),
                Err(CoreError::Malformed(_))
            ));
        }
    }

    #[test]
    fn a_media_failure_reports_as_a_core_error_rather_than_a_second_error_type() {
        assert!(matches!(
            core_media_seal_blob(Vec::new()),
            Err(CoreError::Malformed(_))
        ));
        // A store failure keeps its own meaning rather than being flattened
        // into "malformed", which is the one distinction the mapping owes.
        assert!(matches!(
            core_error(MediaError::Store("locked".into())),
            CoreError::Store(detail) if detail == "locked"
        ));
    }

    #[test]
    fn the_exported_sizes_are_the_constants_and_not_a_second_copy() {
        assert_eq!(media_blob_max_bytes(), MEDIA_BLOB_MAX_BYTES);
        assert_eq!(
            media_thumbnail_max_bytes() as usize,
            crate::media::MEDIA_THUMBNAIL_MAX_BYTES
        );
        assert_eq!(media_manifest_kind(), crate::KIND_ATTACHMENT_MANIFEST);
    }
}
