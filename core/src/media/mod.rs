//! The blob plane: large-media transfer, kept out of the message pipeline.
//!
//! `specs/media-two-plane.md` is the design this implements. Its one
//! architectural rule is that CruiseMesh gains a *second* data plane and the
//! two never mix:
//!
//! * the **message plane** carries text, receipts, service kinds, and — new
//!   here — a media *manifest* with its thumbnail. It is universal: BLE,
//!   LAN, relay, carried hop-by-hop, delay-tolerant, and therefore expensive
//!   per byte because every byte in it is eligible to sit in someone else's
//!   carry queue.
//! * the **blob plane** carries full-resolution photo and video bytes. It is
//!   cheap per byte and therefore *not* universal: bulk TCP only, pulled by
//!   the recipient, resumable, and never touched by a third party.
//!
//! Everything in this module tree exists to make that separation structural
//! rather than aspirational. Nothing here can enter an envelope, a carry
//! queue, a spray plan, or a BLE frame, because nothing here is reachable
//! from the code that builds any of those.
//!
//! # What is wired, and what is not
//!
//! Phase 1 gave the plane exactly three seams into the rest of the crate, all
//! of them through [`integration`]: `protocol.rs` admits a manifest as a body
//! of the already-allocated attachment kind, `store.rs` applies
//! [`store::MEDIA_SCHEMA_SQL`] to the app's one database, and `lan_session.rs`
//! carries pull frames on their own record type. Everything else is still
//! reachable from nothing.
//!
//! A shell can now reach the *authoring* half of that seam and nothing else.
//! [`ffi`] exports the manifest pair, recognition, the consent verdict, the
//! filename rule and the plane's sizes; the pull and serve state machines are
//! deliberately still exported nowhere, because no driver exists to call them
//! and a boundary invented before its caller freezes a surface for nobody.
//! The drivers that would move bytes are phase 2. The checklist below is what
//! is still owed.
//!
//! # The pieces
//!
//! | Module | What it owns |
//! |---|---|
//! | [`blob`] | Per-blob key, chunk geometry, encrypt-then-name, verification |
//! | [`manifest`] | The message-plane body: blob id, geometry, thumbnail, sealed key |
//! | [`filename`] | A manifest filename sanitized into one safe component |
//! | [`bitmap`] | The persisted per-blob chunk bitmap and its missing-range walk |
//! | [`store`] | SQLite-tracked partial-transfer metadata, byte cap, LRU eviction |
//! | [`wire`] | The LAN pull sub-channel's frames and the manifest-possession proof |
//! | [`lan_pull`] | Both roles' state machines: requester and responder |
//! | [`integration`] | The seam: authoring, recognition, and the consent verdict |
//! | [`ffi`] | The UniFFI boundary: mirrors for the types a binding cannot carry |
//!
//! # Phase 1 is LAN-only
//!
//! The spec's phase 2 (a relayd blob store) and phase 3 (group efficiencies,
//! a *study* of consented mule-assist) are not here in any form. There is no
//! relay code in this module, no upload path, and no source-selection policy
//! beyond "the peer that holds it, over a LAN link the mesh already
//! authenticated".
//!
//! # What integration still owes
//!
//! Four of the original eight are done and are named here as done, because
//! this checklist is the plane's own record of what is owed and a stale line
//! is worse than no line:
//!
//! 1. ~~Authoring~~ — [`integration::seal_media_blob`] and
//!    [`integration::media_manifest_body`] produce the body; the caller hands
//!    it to `MessageStore::author_pairwise_message` or `author_group_message`
//!    under [`integration::MEDIA_MANIFEST_KIND`]. What is left is the
//!    thumbnail, which only a platform can generate.
//! 2. ~~Receiving~~ — [`integration::recognize_media_manifest`] tells a
//!    manifest from a legacy inline attachment under the same kind, and
//!    [`integration::begin_media_transfer`] opens the [`store::BlobStore`]
//!    row.
//! 3. ~~Persistence~~ — [`store::MEDIA_SCHEMA_SQL`] is applied on
//!    `MessageStore`'s connection, and the backup posture is decided:
//!    **metadata backs up, chunk files do not**, so a restore clears
//!    `media_blobs` rather than resuming against files that stayed on the
//!    other phone.
//! 4. Drivers: the LAN bulk sub-channel on both shells — a socket, chunk file
//!    writes, and the `take_accepted` drain — plus chunk file deletion, which
//!    this module only ever *plans*. Core names the file
//!    ([`integration::chunk_file_name`]) and never opens it.
//! 5. ~~Consent and cost~~ — [`integration::blob_transfer_permitted`]
//!    (`BLOB-03`), composed from `core_relay_network_permitted` rather than
//!    duplicating it: LAN never consults the network verdict, relay inherits
//!    its roaming deferral, and an expensive path is offered with a size
//!    rather than closed off.
//! 6. ~~Adversarial coverage for `BLOB-01`~~ — the spray, framing and mesh
//!    suites carry blob-flavoured cases now, and `BLOB-01` and `BLOB-03` are
//!    core-owned in `specs/protocol-contract-v1.md` rather than registered
//!    unimplemented.
//! 7. UX: pending/progress copy in `strings.xml` and `Localizable.xcstrings`.
//! 8. Phase 2: relayd blob endpoints, family quota, expiry, per-request range
//!    caps (`BLOB-06`).

pub mod bitmap;
pub mod blob;
pub mod ffi;
pub mod filename;
pub mod integration;
pub mod lan_pull;
pub mod manifest;
pub mod store;
pub mod wire;

// The plane's root names exactly what crosses UniFFI, and nothing else. A
// reader who wants to know what a shell can reach reads this list; everything
// absent from it is reachable only as `media::…` inside the crate and its
// tests, which is the same separation the module doc claims, expressed where
// it can be checked.
pub use ffi::{
    core_media_manifest_body, core_media_recognize_manifest, core_media_seal_blob,
    media_manifest_kind, CoreMediaKind, CoreMediaManifest, CoreSealedMediaBlob,
};
pub use filename::sanitize_media_filename;
pub use integration::{
    blob_transfer_permitted, peer_speaks_blob_plane, BlobTransferSource, BlobTransferVerdict,
};

/// Largest blob the plane will name, seal, or fetch. The spec's v1 sizing:
/// "covers phone video clips; not a movie service".
pub const MEDIA_BLOB_MAX_BYTES: u64 = 128 * 1024 * 1024;

/// Plaintext bytes per chunk. The spec's LAN chunk size, chosen small enough
/// to interleave with the mesh traffic sharing the same link.
pub const MEDIA_CHUNK_PLAINTEXT_BYTES: u32 = 256 * 1024;

/// XChaCha20-Poly1305 tag length. Each chunk is sealed independently, so this
/// is charged once per chunk rather than once per blob.
pub const MEDIA_AEAD_TAG_BYTES: u32 = 16;

/// Ciphertext bytes in a full chunk. Every chunk but the last is exactly this
/// long, which is what makes a chunk's ciphertext offset a multiplication
/// rather than a lookup — the property the spec calls "chunk boundaries are
/// identical at every source".
pub const MEDIA_CHUNK_CIPHERTEXT_BYTES: u32 = MEDIA_CHUNK_PLAINTEXT_BYTES + MEDIA_AEAD_TAG_BYTES;

/// Largest thumbnail the message plane will carry. The spec's rule is
/// relative ("manifest + thumbnail fit comfortably inside today's attachment
/// envelope bound"); this is that rule made a number, and
/// `manifest::tests::a_manifest_fits_the_attachment_envelope_with_room` pins
/// the relationship rather than the constant.
pub const MEDIA_THUMBNAIL_MAX_BYTES: usize = 64 * 1024;

/// Largest encoded manifest body, thumbnail included.
pub const MEDIA_MANIFEST_MAX_BYTES: usize = 96 * 1024;

/// The device's budget for *partially fetched* blobs, garbage-collected
/// oldest-use-first when exceeded. A completed blob is not charged here: it
/// has left for the platform media store, which is the user's own space.
pub const MEDIA_PARTIAL_BUDGET_BYTES: u64 = 512 * 1024 * 1024;

// UniFFI carries functions, not constants, so the bounds a shell can actually
// violate get accessors. These three are exactly the ones an authoring shell
// can walk into: an original too large to name, a thumbnail too large to
// carry, and a body the codec would refuse. The chunk geometry and the
// partial-transfer budget are deliberately absent — they belong to the
// drivers and the Advanced screen, and neither exists yet.

/// See [`MEDIA_BLOB_MAX_BYTES`]. A picker checks against this before it asks
/// core to seal anything.
#[uniffi::export]
pub fn media_blob_max_bytes() -> u64 {
    MEDIA_BLOB_MAX_BYTES
}

/// See [`MEDIA_THUMBNAIL_MAX_BYTES`]. The bound a shell's thumbnail encoder
/// targets; a thumbnail over it is a refused manifest, not a resized one.
#[uniffi::export]
pub fn media_thumbnail_max_bytes() -> u32 {
    MEDIA_THUMBNAIL_MAX_BYTES as u32
}

/// See [`MEDIA_MANIFEST_MAX_BYTES`].
#[uniffi::export]
pub fn media_manifest_max_bytes() -> u32 {
    MEDIA_MANIFEST_MAX_BYTES as u32
}

/// Everything that can go wrong inside the blob plane.
///
/// Plain `thiserror`, not a `uniffi::Error`: no boundary crosses here yet,
/// and inventing one before the integration phase knows what it needs would
/// freeze a surface for no caller.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MediaError {
    #[error("blob is empty")]
    EmptyBlob,
    #[error("blob is {actual} bytes, over the {max}-byte cap")]
    BlobTooLarge { actual: u64, max: u64 },
    #[error("malformed media data: {0}")]
    Malformed(String),
    #[error("chunk {index} failed authentication")]
    ChunkAuthFailed { index: u32 },
    #[error("assembled blob does not match its manifest digest")]
    DigestMismatch,
    #[error("media store: {0}")]
    Store(String),
}
