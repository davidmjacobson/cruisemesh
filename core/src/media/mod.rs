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
//! # This module is dark
//!
//! It compiles and it is tested, and **nothing calls it**. It is not exported
//! over UniFFI, no binding mentions it, and neither shell can reach it. The
//! module is declared `pub` so the crate's own integration tests — chiefly
//! `core/tests/protocol_contract.rs` — can assert the invariants it owns, not
//! because an app is meant to use it. Wiring it up is a later phase; see
//! "What integration still owes" below, which is deliberately written as a
//! checklist rather than as prose.
//!
//! # The pieces
//!
//! | Module | What it owns |
//! |---|---|
//! | [`blob`] | Per-blob key, chunk geometry, encrypt-then-name, verification |
//! | [`manifest`] | The message-plane body: blob id, geometry, thumbnail, sealed key |
//! | [`bitmap`] | The persisted per-blob chunk bitmap and its missing-range walk |
//! | [`store`] | SQLite-tracked partial-transfer metadata, byte cap, LRU eviction |
//! | [`wire`] | The LAN pull sub-channel's frames and the manifest-possession proof |
//! | [`lan_pull`] | Both roles' state machines: requester and responder |
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
//! 1. Authoring: generate a thumbnail, seal the blob with [`blob::seal_blob`],
//!    encode a [`manifest::MediaManifest`], and author it as an ordinary
//!    `KIND_ATTACHMENT_MANIFEST` message. Nothing in this module touches
//!    `authoring.rs`.
//! 2. Receiving: recognise a media manifest inside a delivered attachment
//!    body and open a [`store::BlobStore`] row for it.
//! 3. Persistence: apply [`store::MEDIA_SCHEMA_SQL`] to `MessageStore`'s
//!    connection so the metadata lives in the one database the app already
//!    backs up and sanitizes, and decide the backup posture for partial
//!    transfers (this module deliberately does not edit `store.rs`).
//! 4. Drivers: the LAN bulk sub-channel on both shells — a socket, chunk file
//!    writes, and the `take_accepted` drain — plus chunk file naming and
//!    deletion, which this module only ever *plans*.
//! 5. Consent and cost: the expensive-path verdict (`BLOB-03`), composed with
//!    the existing roaming deferral rather than duplicated.
//! 6. Adversarial coverage for `BLOB-01`: the spray and carry suites gain
//!    blob-flavoured cases proving no blob byte can reach them.
//! 7. UX: pending/progress copy in `strings.xml` and `Localizable.xcstrings`.
//! 8. Phase 2: relayd blob endpoints, family quota, expiry, per-request range
//!    caps (`BLOB-06`).

pub mod bitmap;
pub mod blob;
pub mod lan_pull;
pub mod manifest;
pub mod store;
pub mod wire;

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
