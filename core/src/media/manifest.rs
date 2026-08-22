//! The manifest is the message.
//!
//! Sending a photo or a clip authors one ordinary attachment message. That
//! message carries the media's type and size, a mandatory thumbnail, the
//! digest that names the encrypted blob, and the blob key. It is
//! delay-tolerant like everything else on the message plane: it carries, it
//! mules, it relays, it survives partitions, receipts cover it. Whatever
//! happens to the bytes, the *conversation* is complete on every device.
//!
//! # How the key is sealed
//!
//! It is not sealed here. The manifest body is plaintext at this layer and is
//! sealed exactly like any other message content — [`crate::seal_message`] to
//! each recipient's X25519 key, one sealed copy per recipient, the same
//! sign-then-seal construction `core/src/crypto.rs` has always used. A group
//! send seals the same blob key into each recipient's copy; the ciphertext is
//! uploaded or served once and fetched per recipient.
//!
//! This is a deliberate non-invention. There is no media-specific key
//! wrapping, no second envelope format, and no new suite: possession of a
//! manifest *is* the capability to fetch and read a blob, and possession of a
//! manifest is already governed by the sealed-envelope pipeline.
//!
//! # Wiring status
//!
//! The body format is defined here and nothing dispatches it. The integration
//! phase carries an encoded manifest as the payload of the already-allocated
//! [`crate::KIND_ATTACHMENT_MANIFEST`] kind — the spec's "no new wire protocol
//! for the message plane" rule — which is why this module exposes an encoder
//! and a size bound and touches nothing under `core/src/session/`.
//!
//! # Divergence from the spec's field table
//!
//! Two deliberate departures from `specs/media-two-plane.md` rev 2, both in
//! the direction of the stricter rule:
//!
//! * The spec says "the decoder permits trailing extension bytes, so the
//!   manifest can grow without a version bump". It does not, and should not
//!   while the wire is dark: a manifest is a capability, and room to grow
//!   without a version bump is also room for a sender and a receiver to
//!   disagree about a body neither one rejects. Rev 2's own text only claims
//!   that room "after Phase 2 ships"; until then a format change is an
//!   in-place change, which is what the `file` kind and `filename` are here.
//! * The spec's table lists `ciphertext_bytes` beside `plaintext_bytes`. Only
//!   the plaintext length is carried; the ciphertext length is a function of
//!   it via [`BlobGeometry`], and two sizes on the wire is two sizes that can
//!   disagree. The table is stale, not the codec.

use super::blob::{BlobGeometry, BlobId, BlobKey, BLOB_ID_LEN, BLOB_KEY_LEN};
use super::filename::MAX_FILENAME_BYTES;
use super::{MediaError, MEDIA_MANIFEST_MAX_BYTES, MEDIA_THUMBNAIL_MAX_BYTES};

/// Deliberately **not** 1.
///
/// Kind 16 carries two body codecs (see [`super::integration`]), and telling
/// them apart has to be structural rather than statistical. With both codecs
/// opening on a version byte of 1, disjointness rested on the *rest* of the
/// body: a manifest's blob id would have to happen to spell a valid mime
/// length, duration, blob length and caption for the legacy attachment decoder
/// to accept it. A digest essentially never does — but "essentially never" is
/// a grindable property, not a guarantee, and a sender who ground one would
/// have a single body that old builds render as an inline attachment and new
/// builds treat as a manifest.
///
/// Taking the next version instead makes the first byte alone decide, for
/// every body either codec will ever emit or accept. It costs nothing: this
/// wire is dark, no build has ever authored a manifest, and version 1 was
/// never spent on one. `the_two_kind_16_codecs_never_accept_each_other` pins
/// the separation and the ground-collision shape it replaces.
pub(crate) const MANIFEST_WIRE_VERSION: u8 = 2;
const MEDIA_KIND_PHOTO: u8 = 1;
const MEDIA_KIND_VIDEO: u8 = 2;
const MEDIA_KIND_FILE: u8 = 3;
const MAX_MIME_BYTES: usize = 128;
const MAX_CAPTION_BYTES: usize = 4_096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    Photo,
    Video,
    /// Anything else: a PDF, a spreadsheet, a zip. Everything below the
    /// manifest is content-agnostic, so a generic file is a manifest kind
    /// rather than a second mechanism.
    File,
}

/// What rides the message plane in place of the bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaManifest {
    pub blob_id: BlobId,
    /// The per-blob key. Sealed to each recipient by the ordinary message
    /// pipeline, never by anything in this module.
    pub blob_key: BlobKey,
    /// Plaintext length. The chunk geometry is derived from it, so the
    /// manifest does not carry a chunk count that could disagree with itself.
    pub plaintext_bytes: u64,
    pub kind: MediaKind,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    /// Zero for a photo, and for a file.
    pub duration_ms: u32,
    /// The sender's display name for the item. Required for
    /// [`MediaKind::File`], optional — empty — otherwise. Empty is the absent
    /// case rather than an `Option` so the encoded field keeps the same
    /// length-prefixed shape as every other variable-width field; the wire
    /// has no presence byte to spend.
    ///
    /// Display metadata, never a path: run it through
    /// [`super::filename::sanitize_media_filename`] before it reaches a
    /// filesystem call.
    pub filename: String,
    /// Mandatory for a photo or a clip, and *absent* for a file. The spec
    /// allows no "full quality over the message plane" escape hatch, and a
    /// bubble that could render blank is the thing the thumbnail exists to
    /// prevent. A file bubble renders from its name and type instead, so a
    /// file manifest carrying a thumbnail is refused rather than tolerated —
    /// tolerating it is the degradation escape hatch by another route.
    pub thumbnail: Vec<u8>,
    pub caption: String,
}

impl MediaManifest {
    /// The chunk geometry both sides derive rather than transmit.
    pub fn geometry(&self) -> Result<BlobGeometry, MediaError> {
        BlobGeometry::for_plaintext_len(self.plaintext_bytes)
    }

    fn validate(&self) -> Result<(), MediaError> {
        self.geometry()?;
        if matches!(self.kind, MediaKind::File) {
            if !self.thumbnail.is_empty() {
                return Err(MediaError::Malformed(
                    "a file manifest carries no thumbnail".into(),
                ));
            }
            if self.filename.is_empty() {
                return Err(MediaError::Malformed(
                    "a file manifest must carry a filename".into(),
                ));
            }
        } else if self.thumbnail.is_empty() {
            return Err(MediaError::Malformed(
                "a media manifest must carry a thumbnail".into(),
            ));
        }
        if self.filename.len() > MAX_FILENAME_BYTES {
            return Err(MediaError::Malformed("media filename is too long".into()));
        }
        if self.thumbnail.len() > MEDIA_THUMBNAIL_MAX_BYTES {
            return Err(MediaError::Malformed(format!(
                "thumbnail is {} bytes, over the {MEDIA_THUMBNAIL_MAX_BYTES}-byte budget",
                self.thumbnail.len()
            )));
        }
        if self.mime_type.is_empty() || self.mime_type.len() > MAX_MIME_BYTES {
            return Err(MediaError::Malformed("media mime type is unusable".into()));
        }
        if self.caption.len() > MAX_CAPTION_BYTES {
            return Err(MediaError::Malformed("media caption is too long".into()));
        }
        if !matches!(self.kind, MediaKind::Video) && self.duration_ms != 0 {
            return Err(MediaError::Malformed("only a clip has a duration".into()));
        }
        if matches!(self.kind, MediaKind::File) && (self.width != 0 || self.height != 0) {
            return Err(MediaError::Malformed("a file has no geometry".into()));
        }
        Ok(())
    }
}

/// Encode a manifest body. Deterministic, big-endian, length-prefixed.
pub fn encode_media_manifest(manifest: &MediaManifest) -> Result<Vec<u8>, MediaError> {
    manifest.validate()?;
    let mime = manifest.mime_type.as_bytes();
    let filename = manifest.filename.as_bytes();
    let caption = manifest.caption.as_bytes();

    let mut out = Vec::with_capacity(
        64 + mime.len() + filename.len() + manifest.thumbnail.len() + caption.len(),
    );
    out.push(MANIFEST_WIRE_VERSION);
    out.push(match manifest.kind {
        MediaKind::Photo => MEDIA_KIND_PHOTO,
        MediaKind::Video => MEDIA_KIND_VIDEO,
        MediaKind::File => MEDIA_KIND_FILE,
    });
    out.extend_from_slice(manifest.blob_id.as_bytes());
    out.extend_from_slice(manifest.blob_key.as_bytes());
    out.extend_from_slice(&manifest.plaintext_bytes.to_be_bytes());
    out.extend_from_slice(&manifest.width.to_be_bytes());
    out.extend_from_slice(&manifest.height.to_be_bytes());
    out.extend_from_slice(&manifest.duration_ms.to_be_bytes());
    write_bytes16(&mut out, filename);
    write_bytes16(&mut out, mime);
    write_bytes32(&mut out, &manifest.thumbnail);
    write_bytes16(&mut out, caption);

    if out.len() > MEDIA_MANIFEST_MAX_BYTES {
        return Err(MediaError::Malformed(format!(
            "manifest is {} bytes, over the {MEDIA_MANIFEST_MAX_BYTES}-byte body budget",
            out.len()
        )));
    }
    Ok(out)
}

/// Decode a manifest body. Strict: an unknown version, a trailing byte, an
/// oversized thumbnail or a geometry that cannot exist are all rejections
/// rather than best-effort reads. A manifest is a capability; a lenient
/// decoder is how a malformed one becomes a fetch loop.
pub fn decode_media_manifest(bytes: &[u8]) -> Result<MediaManifest, MediaError> {
    if bytes.len() > MEDIA_MANIFEST_MAX_BYTES {
        return Err(MediaError::Malformed("manifest body is too long".into()));
    }
    let mut cursor = Cursor::new(bytes);
    let malformed = || MediaError::Malformed("truncated media manifest".into());

    if cursor.read_u8().ok_or_else(malformed)? != MANIFEST_WIRE_VERSION {
        return Err(MediaError::Malformed(
            "unsupported media manifest version".into(),
        ));
    }
    let kind = match cursor.read_u8().ok_or_else(malformed)? {
        MEDIA_KIND_PHOTO => MediaKind::Photo,
        MEDIA_KIND_VIDEO => MediaKind::Video,
        MEDIA_KIND_FILE => MediaKind::File,
        other => return Err(MediaError::Malformed(format!("unknown media kind {other}"))),
    };
    let blob_id = BlobId::from_slice(cursor.read_exact(BLOB_ID_LEN).ok_or_else(malformed)?)?;
    let blob_key = BlobKey::from_slice(cursor.read_exact(BLOB_KEY_LEN).ok_or_else(malformed)?)?;
    let plaintext_bytes = cursor.read_u64().ok_or_else(malformed)?;
    let width = cursor.read_u32().ok_or_else(malformed)?;
    let height = cursor.read_u32().ok_or_else(malformed)?;
    let duration_ms = cursor.read_u32().ok_or_else(malformed)?;
    let filename = cursor
        .read_string16(MAX_FILENAME_BYTES)
        .ok_or_else(malformed)?;
    let mime_type = cursor.read_string16(MAX_MIME_BYTES).ok_or_else(malformed)?;
    let thumbnail = cursor
        .read_bytes32(MEDIA_THUMBNAIL_MAX_BYTES)
        .ok_or_else(malformed)?
        .to_vec();
    let caption = cursor
        .read_string16(MAX_CAPTION_BYTES)
        .ok_or_else(malformed)?;
    if !cursor.is_finished() {
        return Err(MediaError::Malformed(
            "trailing bytes after a media manifest".into(),
        ));
    }

    let manifest = MediaManifest {
        blob_id,
        blob_key,
        plaintext_bytes,
        kind,
        mime_type,
        width,
        height,
        duration_ms,
        filename,
        thumbnail,
        caption,
    };
    manifest.validate()?;
    Ok(manifest)
}

fn write_bytes16(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn write_bytes32(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, offset: 0 }
    }

    fn read_exact(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(count)?;
        let out = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(out)
    }

    fn read_u8(&mut self) -> Option<u8> {
        Some(self.read_exact(1)?[0])
    }

    fn read_u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.read_exact(2)?.try_into().ok()?))
    }

    fn read_u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.read_exact(4)?.try_into().ok()?))
    }

    fn read_u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.read_exact(8)?.try_into().ok()?))
    }

    fn read_bytes16(&mut self, max: usize) -> Option<&'a [u8]> {
        let count = self.read_u16()? as usize;
        if count > max {
            return None;
        }
        self.read_exact(count)
    }

    fn read_bytes32(&mut self, max: usize) -> Option<&'a [u8]> {
        let count = self.read_u32()? as usize;
        if count > max {
            return None;
        }
        self.read_exact(count)
    }

    fn read_string16(&mut self, max: usize) -> Option<String> {
        String::from_utf8(self.read_bytes16(max)?.to_vec()).ok()
    }

    fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
pub(crate) fn sample_manifest() -> MediaManifest {
    MediaManifest {
        blob_id: BlobId([0x11; BLOB_ID_LEN]),
        blob_key: BlobKey([0x22; BLOB_KEY_LEN]),
        plaintext_bytes: 700_000,
        kind: MediaKind::Photo,
        mime_type: "image/jpeg".into(),
        width: 4_032,
        height: 3_024,
        duration_ms: 0,
        filename: String::new(),
        thumbnail: vec![0xAB; 2_048],
        caption: "the buffet at six".into(),
    }
}

#[cfg(test)]
pub(crate) fn sample_file_manifest() -> MediaManifest {
    MediaManifest {
        blob_id: BlobId([0x33; BLOB_ID_LEN]),
        blob_key: BlobKey([0x44; BLOB_KEY_LEN]),
        plaintext_bytes: 4_096,
        kind: MediaKind::File,
        mime_type: "application/pdf".into(),
        width: 0,
        height: 0,
        duration_ms: 0,
        filename: "trip.pdf".into(),
        thumbnail: Vec::new(),
        caption: "the boarding passes".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::ATTACHMENT_MAX_BLOB_BYTES;
    use crate::media::blob::{seal_blob, test_key};
    use crate::{generate_identity, open_message, seal_message, MAX_ENVELOPE_SEALED_BYTES};

    #[test]
    fn a_manifest_round_trips() {
        let manifest = sample_manifest();
        let encoded = encode_media_manifest(&manifest).unwrap();
        assert_eq!(decode_media_manifest(&encoded).unwrap(), manifest);
    }

    #[test]
    fn a_file_manifest_round_trips() {
        let manifest = sample_file_manifest();
        let encoded = encode_media_manifest(&manifest).unwrap();
        assert_eq!(decode_media_manifest(&encoded).unwrap(), manifest);
    }

    #[test]
    fn a_golden_vector_pins_the_body_layout() {
        // A small manifest, byte for byte. The header is fixed-width, so the
        // vector catches a reordered or resized field even when both sides of
        // a round trip change together.
        let manifest = MediaManifest {
            blob_id: BlobId([0x01; BLOB_ID_LEN]),
            blob_key: BlobKey([0x02; BLOB_KEY_LEN]),
            plaintext_bytes: 1_048_576,
            kind: MediaKind::Video,
            mime_type: "video/mp4".into(),
            width: 1_920,
            height: 1_080,
            duration_ms: 30_000,
            filename: String::new(),
            thumbnail: vec![0xFE, 0xED],
            caption: "hi".into(),
        };
        let encoded = encode_media_manifest(&manifest).unwrap();
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            concat!(
                "0202",
                "0101010101010101010101010101010101010101010101010101010101010101",
                "0202020202020202020202020202020202020202020202020202020202020202",
                "0000000000100000",
                "00000780",
                "00000438",
                "00007530",
                "0000",
                "0009",
                "766964656f2f6d7034",
                "00000002",
                "feed",
                "0002",
                "6869",
            )
        );
        assert_eq!(decode_media_manifest(&encoded).unwrap(), manifest);
    }

    #[test]
    fn a_golden_vector_pins_the_file_body_layout() {
        // The same layout with the two rev-2 additions carrying real values:
        // kind 3, and a filename between the fixed-width header and the mime.
        // A file has no thumbnail, so its length prefix is a zero.
        let manifest = MediaManifest {
            blob_id: BlobId([0x03; BLOB_ID_LEN]),
            blob_key: BlobKey([0x04; BLOB_KEY_LEN]),
            plaintext_bytes: 4_096,
            kind: MediaKind::File,
            mime_type: "application/pdf".into(),
            width: 0,
            height: 0,
            duration_ms: 0,
            filename: "trip.pdf".into(),
            thumbnail: Vec::new(),
            caption: String::new(),
        };
        let encoded = encode_media_manifest(&manifest).unwrap();
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            concat!(
                "0203",
                "0303030303030303030303030303030303030303030303030303030303030303",
                "0404040404040404040404040404040404040404040404040404040404040404",
                "0000000000001000",
                "00000000",
                "00000000",
                "00000000",
                "0008",
                "747269702e706466",
                "000f",
                "6170706c69636174696f6e2f706466",
                "00000000",
                "0000",
            )
        );
        assert_eq!(decode_media_manifest(&encoded).unwrap(), manifest);
    }

    #[test]
    fn the_geometry_is_derived_not_carried() {
        let manifest = sample_manifest();
        let geometry = manifest.geometry().unwrap();
        assert_eq!(geometry.chunk_count, 3);
        assert_eq!(geometry.plaintext_bytes, 700_000);
        // The sender's actual seal agrees with what a recipient derives from
        // the manifest alone.
        let sealed = seal_blob(&test_key(1), &vec![7u8; 700_000]).unwrap();
        assert_eq!(sealed.geometry, geometry);
    }

    #[test]
    fn a_manifest_is_sealed_by_the_ordinary_message_pipeline() {
        // The blob key crosses the wire only inside this envelope, using the
        // same sign-then-seal construction as every other message. No
        // media-specific crypto exists.
        let alice = generate_identity();
        let bob = generate_identity();
        let manifest = sample_manifest();
        let body = encode_media_manifest(&manifest).unwrap();

        let sealed = seal_message(alice.clone(), bob.agree_pk.clone(), body).unwrap();
        let opened = open_message(bob, sealed.clone()).unwrap();
        assert_eq!(opened.sender_user_id, alice.user_id);
        assert_eq!(decode_media_manifest(&opened.payload).unwrap(), manifest);
        assert!(
            sealed.len() < MAX_ENVELOPE_SEALED_BYTES,
            "a manifest envelope must fit the pipeline it rides"
        );
    }

    #[test]
    fn a_manifest_fits_the_attachment_envelope_with_room() {
        // The spec states the thumbnail budget relatively: manifest plus
        // thumbnail fit comfortably inside today's attachment bound. Pin the
        // relationship, so raising the thumbnail budget past the envelope is
        // a red build rather than a field surprise.
        let mut manifest = sample_manifest();
        manifest.thumbnail = vec![0u8; MEDIA_THUMBNAIL_MAX_BYTES];
        manifest.filename = "f".repeat(MAX_FILENAME_BYTES);
        manifest.caption = "c".repeat(MAX_CAPTION_BYTES);
        let encoded = encode_media_manifest(&manifest).unwrap();
        assert!(encoded.len() <= MEDIA_MANIFEST_MAX_BYTES);
        assert!(
            encoded.len() * 2 < ATTACHMENT_MAX_BLOB_BYTES,
            "the largest manifest is {} bytes against a {ATTACHMENT_MAX_BLOB_BYTES}-byte \
             attachment bound; 'comfortably' means at least half spare",
            encoded.len()
        );
    }

    #[test]
    fn a_manifest_without_a_thumbnail_is_refused() {
        let mut manifest = sample_manifest();
        manifest.thumbnail.clear();
        assert!(encode_media_manifest(&manifest).is_err());

        manifest.thumbnail = vec![0u8; MEDIA_THUMBNAIL_MAX_BYTES + 1];
        assert!(encode_media_manifest(&manifest).is_err());

        let mut clip = sample_manifest();
        clip.kind = MediaKind::Video;
        clip.thumbnail.clear();
        assert!(encode_media_manifest(&clip).is_err());
    }

    #[test]
    fn a_file_manifest_carrying_a_thumbnail_is_refused() {
        // Refused, not ignored. A file bubble that could show a
        // sender-supplied picture is the degradation escape hatch the spec
        // rules out, arriving by a different door.
        let mut manifest = sample_file_manifest();
        manifest.thumbnail = vec![0xAB; 16];
        assert!(encode_media_manifest(&manifest).is_err());

        // And a hand-built body cannot smuggle one past the decoder either.
        let good = encode_media_manifest(&sample_file_manifest()).unwrap();
        let mut smuggled = good.clone();
        let thumbnail_len_at = good.len() - 2 - "the boarding passes".len() - 4;
        smuggled[thumbnail_len_at..thumbnail_len_at + 4].copy_from_slice(&1u32.to_be_bytes());
        smuggled.insert(thumbnail_len_at + 4, 0xAB);
        assert!(decode_media_manifest(&smuggled).is_err());
    }

    #[test]
    fn an_impossible_blob_size_is_refused_at_both_ends() {
        let mut manifest = sample_manifest();
        manifest.plaintext_bytes = 0;
        assert_eq!(
            encode_media_manifest(&manifest).unwrap_err(),
            MediaError::EmptyBlob
        );

        manifest.plaintext_bytes = crate::media::MEDIA_BLOB_MAX_BYTES + 1;
        assert!(encode_media_manifest(&manifest).is_err());

        // And a hand-built body claiming an impossible size does not decode.
        let mut good = encode_media_manifest(&sample_manifest()).unwrap();
        let size_at = 2 + BLOB_ID_LEN + BLOB_KEY_LEN;
        good[size_at..size_at + 8].copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(decode_media_manifest(&good).is_err());
    }

    #[test]
    fn decoding_is_strict_about_versions_kinds_and_trailing_bytes() {
        let good = encode_media_manifest(&sample_manifest()).unwrap();

        // 1 is the legacy inline attachment's version and 3 is the next one to
        // allocate. Neither is a manifest, and the version byte is the only
        // thing separating the two kind-16 codecs, so both are rejections.
        for version in [0u8, 1, 3, 255] {
            let mut wrong_version = good.clone();
            wrong_version[0] = version;
            assert!(decode_media_manifest(&wrong_version).is_err());
        }

        let mut wrong_kind = good.clone();
        wrong_kind[1] = 9;
        assert!(decode_media_manifest(&wrong_kind).is_err());

        // 4 is the next kind to allocate. Pinning it keeps that allocation a
        // deliberate act rather than something a decoder quietly accepts.
        wrong_kind[1] = 4;
        assert!(decode_media_manifest(&wrong_kind).is_err());

        let mut trailing = good.clone();
        trailing.push(0);
        assert!(decode_media_manifest(&trailing).is_err());

        assert!(decode_media_manifest(&good[..good.len() - 1]).is_err());
        assert!(decode_media_manifest(&[]).is_err());
    }

    #[test]
    fn a_photo_carrying_a_duration_is_refused() {
        let mut manifest = sample_manifest();
        manifest.duration_ms = 1;
        assert!(encode_media_manifest(&manifest).is_err());
    }

    #[test]
    fn a_file_carrying_geometry_or_no_filename_is_refused() {
        // A file is not a frame: width, height and duration are all zero, the
        // same rule "a photo has no duration" already states for its kind.
        for mutate in [
            |m: &mut MediaManifest| m.width = 1,
            |m: &mut MediaManifest| m.height = 1,
            |m: &mut MediaManifest| m.duration_ms = 1,
            |m: &mut MediaManifest| m.filename.clear(),
            |m: &mut MediaManifest| m.filename = "f".repeat(MAX_FILENAME_BYTES + 1),
        ] {
            let mut manifest = sample_file_manifest();
            mutate(&mut manifest);
            assert!(encode_media_manifest(&manifest).is_err());
        }
    }

    #[test]
    fn a_filename_is_display_metadata_not_a_path() {
        // The codec carries whatever the sender wrote — sanitizing on the wire
        // would hide the sender's real name from the bubble — so the guarantee
        // is that nothing reaches a filesystem without going through the
        // sanitizer, which round-trips a hostile name into one component.
        let mut manifest = sample_file_manifest();
        manifest.filename = "../../etc/passwd".into();
        let decoded = decode_media_manifest(&encode_media_manifest(&manifest).unwrap()).unwrap();
        assert_eq!(decoded.filename, "../../etc/passwd");
        assert_eq!(
            crate::media::filename::sanitize_media_filename(&decoded.filename, &decoded.mime_type),
            "passwd.pdf"
        );
    }
}
