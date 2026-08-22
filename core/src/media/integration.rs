//! Where the blob plane touches the rest of the app.
//!
//! Every other module in this tree is deliberately unreachable from the code
//! that moves messages. This one is the seam, and it is small on purpose: the
//! four things integration owed the plane, and nothing else.
//!
//! | Owed | Here |
//! |---|---|
//! | Authoring | [`seal_media_blob`] + [`media_manifest_body`] → a `KIND_ATTACHMENT_MANIFEST` body |
//! | Receiving | [`recognize_media_manifest`] + [`begin_media_transfer`] |
//! | Persistence | [`super::store::MEDIA_SCHEMA_SQL`] on `MessageStore`'s connection (`store.rs`) |
//! | Consent | [`blob_transfer_permitted`], composed from [`crate::core_relay_network_permitted`] |
//!
//! # Kind 16 carries two codecs
//!
//! `KIND_ATTACHMENT_MANIFEST` was allocated for the legacy *inline*
//! attachment ([`crate::content`]) and is now also the manifest's kind, which
//! is what the spec's "no new wire protocol for the message plane" rule costs:
//! one kind, two body codecs, told apart by the bodies themselves rather than
//! by a discriminator neither shipped build would understand.
//!
//! Both open with a version byte, and that byte is what tells them apart:
//! the attachment codec is version 1 and only version 1, and the manifest
//! codec is version 2 and only version 2
//! ([`super::manifest::MANIFEST_WIRE_VERSION`] carries the reasoning). So the
//! accept sets are disjoint structurally — for every body either codec will
//! ever emit or accept, at the first byte, with nothing to grind.
//!
//! The alternative was to keep both at version 1 and rely on the rest of the
//! body: a manifest's blob id would have to happen to spell a valid mime
//! length, duration, blob length and caption before the attachment decoder
//! accepted it, which a digest essentially never does. "Essentially never" is
//! a statistical property of a *random* digest, though, and a blob id is
//! something a sender can grind: one ground collision is one body that old
//! builds render as an inline attachment and new builds treat as a manifest.
//! Spending the next version byte was cheaper than reasoning about that.
//! `the_two_kind_16_codecs_never_accept_each_other` pins the separation in
//! both directions, including a hand-built body of exactly that shape.
//!
//! # Consent is composed, not restated
//!
//! [`blob_transfer_permitted`] owns exactly one new thing: that a LAN source
//! is free and an internet source is not. The rule about *when the internet
//! may be spent at all* — roaming, and the Advanced override that clears it —
//! stays in [`crate::core_relay_network_permitted`], which is why a LAN
//! transfer is never fed through it at all. Two copies of that rule is how a
//! family ends up with a plane that roams when the mailbox will not.
//!
//! What the verdict does not decide is how a *degradation* reads here. A blob
//! is a foreground thing someone is looking at, so a constrained path asks
//! rather than defers; the reasoning is on [`blob_transfer_permitted`].

use crate::CoreRelayNetworkVerdict;

use super::bitmap::ChunkBitmap;
use super::blob::{generate_blob_key, seal_blob, BlobId, BlobKey, SealedBlob};
use super::lan_pull::{ServeBudgets, ServePlan};
use super::manifest::{decode_media_manifest, encode_media_manifest, MediaManifest};
use super::store::{BlobOrigin, BlobRecord, BlobStore};
use super::MediaError;

/// The message kind a manifest rides. Named here so integration code does not
/// have to know that the blob plane reuses the attachment kind.
pub const MEDIA_MANIFEST_KIND: u8 = crate::KIND_ATTACHMENT_MANIFEST;

/// A sealed blob and the key that opens it, ready for a manifest.
///
/// The key is returned rather than kept: it belongs in a [`MediaManifest`],
/// which the ordinary message pipeline seals to each recipient. Nothing in
/// this crate stores it.
#[derive(Debug)]
pub struct AuthoredMediaBlob {
    pub sealed: SealedBlob,
    pub key: BlobKey,
}

/// The sender's first step: a fresh key, ciphertext, and the digest that names
/// it. The ciphertext is the caller's to write wherever it will serve it from;
/// core never opens a file.
pub fn seal_media_blob(plaintext: &[u8]) -> Result<AuthoredMediaBlob, MediaError> {
    let key = generate_blob_key();
    let sealed = seal_blob(&key, plaintext)?;
    Ok(AuthoredMediaBlob { sealed, key })
}

/// The sender's second step: the `content` of a [`MEDIA_MANIFEST_KIND`]
/// message, to hand to `MessageStore::author_pairwise_message` or
/// `author_group_message` unchanged.
pub fn media_manifest_body(manifest: &MediaManifest) -> Result<Vec<u8>, MediaError> {
    encode_media_manifest(manifest)
}

/// The receive side of the same seam: a delivered message body, or `None`.
///
/// `None` for every kind but [`MEDIA_MANIFEST_KIND`], and `None` for a legacy
/// inline attachment carried under that kind — see the module docs for why
/// trying the strict codec first is what makes that second `None` reliable.
pub fn recognize_media_manifest(kind: u8, content: &[u8]) -> Option<MediaManifest> {
    if kind != MEDIA_MANIFEST_KIND {
        return None;
    }
    decode_media_manifest(content).ok()
}

/// The name of the file a recipient appends chunks to.
///
/// Core decides the name and never opens the file ([`super::store`]). The
/// whole digest goes in rather than [`BlobId::short`]: `short` is a courtesy
/// for logs, where a truncated identifier costs nothing, but a chunk file name
/// is a key — two blobs that collided on it would be two transfers appending
/// to one file, which is corruption that verifies as a digest mismatch much
/// later and then loops.
pub fn chunk_file_name(blob_id: &BlobId) -> String {
    let hex: String = blob_id
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{hex}.part")
}

/// Open (or re-attach to) the row a recognized manifest needs before any chunk
/// can be recorded. Idempotent, because a manifest can be delivered twice and
/// an app can restart mid-transfer; both resume the existing bitmap.
///
/// The row is [`BlobOrigin::Received`], which is what stops a finished
/// download from turning this device into a second source for someone else's
/// blob — see [`servable_plan`] and `BLOB-01`.
pub fn begin_media_transfer(
    store: &BlobStore<'_>,
    manifest: &MediaManifest,
    now_ms: i64,
) -> Result<BlobRecord, MediaError> {
    let geometry = manifest.geometry()?;
    store.begin(
        &manifest.blob_id,
        &geometry,
        &chunk_file_name(&manifest.blob_id),
        BlobOrigin::Received,
        now_ms,
    )
}

/// Open the row for a blob this device *authored*: the sender's own copy, the
/// one it may serve.
///
/// The authoring side needs a row too — the responder answers a fetch out of
/// the same bitmap and geometry a downloader is tracked by — but it is the one
/// origin that may be offered onward, so it gets its own entry point rather
/// than an origin argument on the receive one. Two doors, each with the origin
/// baked in, is harder to walk through the wrong way than one door with a flag.
pub fn begin_authored_media_blob(
    store: &BlobStore<'_>,
    sealed: &SealedBlob,
    now_ms: i64,
) -> Result<BlobRecord, MediaError> {
    store.begin(
        &sealed.id,
        &sealed.geometry,
        &chunk_file_name(&sealed.id),
        BlobOrigin::AuthoredHere,
        now_ms,
    )
}

/// The plan a responder would serve this blob under, or `None` if it must not
/// be served at all.
///
/// This is `BLOB-01`'s second clause where it can be enforced: *no third party
/// stores, forwards, or serves another person's blob*. A completed download
/// holds every chunk and verifies against the manifest, so nothing about the
/// bitmap distinguishes it from the sender's own copy — only its origin does.
/// Without this gate, the first person to finish a photo becomes a second
/// source for it, every later recipient can pull from them, and v1's
/// two-sources rule becomes "whoever has opened it", which is exactly the
/// third-party carry the spec lists as a non-goal.
///
/// A blob is servable when it was authored here **and** at least one chunk is
/// present. Anything else is `None`, and the responder never opens a session
/// for it in the first place; [`super::lan_pull::ServeSession`] refuses a
/// received-origin plan a second time, so a caller that builds one by hand
/// still cannot serve it.
pub fn servable_plan(
    record: &BlobRecord,
    blob_key: &BlobKey,
    held: ChunkBitmap,
    budgets: ServeBudgets,
) -> Option<ServePlan> {
    if record.origin != BlobOrigin::AuthoredHere || held.present_count() == 0 {
        return None;
    }
    Some(ServePlan {
        blob_id: record.blob_id,
        blob_key: blob_key.clone(),
        geometry: record.geometry,
        origin: record.origin,
        held,
        budgets,
    })
}

/// Where a recipient would fetch the bytes from.
///
/// Exported as itself rather than mirrored in [`super::ffi`]: it carries no
/// fixed-width array, so a binding can spell it exactly as written.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobTransferSource {
    /// A LAN link the mesh has already authenticated with the sender. Free,
    /// local, and off the internet entirely.
    Lan,
    /// The relay blob store, over whatever internet path is selected.
    Relay,
}

/// What may happen to a blob transfer right now.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlobTransferVerdict {
    /// Start without asking.
    AutoStart,
    /// Do not start, but offer it: the person can price the transfer from
    /// `ciphertext_bytes` and say yes. Never a warning state — the spec's
    /// "Tap to download over the internet (34 MB)".
    AskFirst { ciphertext_bytes: u64 },
    /// Do not start and do not offer: the path itself is closed, which today
    /// means only [`CoreRelayNetworkVerdict::DeferredRoaming`]. `network` is
    /// carried anyway so the bubble reads the reason off the verdict rather
    /// than inventing one, and so a later deferral needs no new variant.
    Deferred {
        network: CoreRelayNetworkVerdict,
        ciphertext_bytes: u64,
    },
}

/// `BLOB-03`'s device half: no blob transfer starts on an expensive or roaming
/// path without an explicit user action.
///
/// Pure, clock-free, and composed:
///
/// * **LAN is always permitted.** It does not touch the internet, so the relay
///   network verdict is not consulted for it — a roaming SIM says nothing
///   about a cabin Wi-Fi. Auto-start is the per-device setting the spec
///   defaults on; with it off the transfer is offered rather than deferred,
///   because nothing is being spent.
/// * **Relay never starts unattended at all.** Even a fully permitted path
///   asks first unless the caller carries a confirmation, which is what makes
///   the action explicit and size-aware. That is already stricter than the
///   mailbox, so the network verdict's remaining job here is narrow.
/// * **A roaming deferral is the one thing a confirmation cannot buy past.**
///   [`CoreRelayNetworkVerdict::DeferredRoaming`] means "do not start a relay
///   pass"; its only override is the Advanced roaming toggle, and that already
///   lives inside [`crate::core_relay_network_permitted`], where the mailbox
///   honours it too. A second copy of that rule is how a family ends up with a
///   plane that roams when the mailbox will not.
/// * **A constrained path is expensive, not closed.**
///   [`CoreRelayNetworkVerdict::DeferredConstrained`] (Android Data Saver, iOS
///   Low Data Mode) defers the mailbox's *unattended* carried-envelope uploads
///   while lightweight sync keeps running — it is a degradation, and nothing
///   ever clears it, since the roaming toggle does not apply. Mapping it to
///   [`BlobTransferVerdict::Deferred`] would leave a phone in Low Data Mode
///   unable to fetch a photo ever, with no offer on the bubble to escape
///   through, on a network the relay itself is still using. So it asks — which
///   is exactly the rule BLOB-03 states for an expensive path.
#[uniffi::export]
pub fn blob_transfer_permitted(
    source: BlobTransferSource,
    network: CoreRelayNetworkVerdict,
    user_confirmed: bool,
    ciphertext_bytes: u64,
    auto_download_on_lan: bool,
) -> BlobTransferVerdict {
    match source {
        BlobTransferSource::Lan => {
            if auto_download_on_lan || user_confirmed {
                BlobTransferVerdict::AutoStart
            } else {
                BlobTransferVerdict::AskFirst { ciphertext_bytes }
            }
        }
        BlobTransferSource::Relay => match network {
            CoreRelayNetworkVerdict::DeferredRoaming => BlobTransferVerdict::Deferred {
                network: CoreRelayNetworkVerdict::DeferredRoaming,
                ciphertext_bytes,
            },
            CoreRelayNetworkVerdict::Permitted | CoreRelayNetworkVerdict::DeferredConstrained => {
                if user_confirmed {
                    BlobTransferVerdict::AutoStart
                } else {
                    BlobTransferVerdict::AskFirst { ciphertext_bytes }
                }
            }
        },
    }
}

/// Whether a peer's advertised HELLO2 capabilities say it speaks the LAN pull
/// sub-channel. A requester opens a pull session only against a peer that
/// advertised the bit; the manifest itself is sent regardless, because it is
/// delay-tolerant mail like any other.
///
/// The bit's meaning lives here rather than beside its allocation so that the
/// blob plane stays the only place that knows what it licenses.
#[uniffi::export]
pub fn peer_speaks_blob_plane(peer_capabilities: u32) -> bool {
    peer_capabilities & crate::protocol::CAP_MEDIA_BLOB != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content::{
        decode_attachment_payload, encode_attachment_payload, AttachmentMediaType,
        CoreAttachmentPayload, ATTACHMENT_WIRE_VERSION,
    };
    use crate::media::manifest::{
        sample_file_manifest, sample_manifest, MediaKind, MANIFEST_WIRE_VERSION,
    };
    use crate::protocol::{core_own_capabilities, CAP_MEDIA_BLOB};
    use crate::{
        core_relay_network_permitted, decode_message_body, encode_message_body, CoreRelayRoaming,
        MessageBody, KIND_TEXT,
    };
    use rusqlite::Connection;

    fn manifest_for(sealed: &AuthoredMediaBlob) -> MediaManifest {
        MediaManifest {
            blob_id: sealed.sealed.id,
            blob_key: sealed.key.clone(),
            plaintext_bytes: sealed.sealed.geometry.plaintext_bytes,
            ..sample_manifest()
        }
    }

    fn legacy_attachments() -> Vec<Vec<u8>> {
        [
            CoreAttachmentPayload {
                media_type: AttachmentMediaType::Image,
                mime_type: "image/jpeg".into(),
                duration_ms: 0,
                blob: vec![0xAB; 4_096],
                caption: "the buffet at six".into(),
            },
            CoreAttachmentPayload {
                media_type: AttachmentMediaType::Audio,
                mime_type: "audio/mp4".into(),
                duration_ms: 4_500,
                blob: vec![0x00; 32],
                caption: String::new(),
            },
            CoreAttachmentPayload {
                media_type: AttachmentMediaType::Image,
                mime_type: "image/png".into(),
                duration_ms: 0,
                blob: Vec::new(),
                caption: "x".repeat(600),
            },
        ]
        .into_iter()
        .map(|payload| encode_attachment_payload(payload).unwrap())
        .collect()
    }

    fn manifests() -> Vec<MediaManifest> {
        let mut clip = sample_manifest();
        clip.kind = MediaKind::Video;
        clip.mime_type = "video/mp4".into();
        clip.duration_ms = 30_000;
        vec![sample_manifest(), clip, sample_file_manifest()]
    }

    #[test]
    fn an_authored_manifest_is_a_body_the_message_plane_accepts() {
        // The whole authoring path: seal, name, describe, encode, and hand the
        // result to the ordinary body codec under the attachment kind. Before
        // the manifest codec was admitted there, this failed at
        // `encode_message_body` — a manifest is not an inline attachment.
        let blob = seal_media_blob(b"a full-resolution picture of the fjords").unwrap();
        let manifest = manifest_for(&blob);
        let content = media_manifest_body(&manifest).unwrap();

        let body = MessageBody {
            kind: MEDIA_MANIFEST_KIND,
            chat_id: b"alice-id".to_vec(),
            lamport: 4,
            timestamp: 1_700_000_000_000,
            content,
        };
        let encoded = encode_message_body(body.clone()).unwrap();
        let decoded = decode_message_body(encoded).unwrap();
        assert_eq!(decoded, body);

        let recognized = recognize_media_manifest(decoded.kind, &decoded.content).unwrap();
        assert_eq!(recognized, manifest);
        // And the geometry a recipient derives is the sender's actual seal.
        assert_eq!(recognized.geometry().unwrap(), blob.sealed.geometry);
        assert_eq!(recognized.blob_id, blob.sealed.id);
    }

    #[test]
    fn the_two_kind_16_codecs_never_accept_each_other() {
        // One kind, two body codecs. Recognition is only reliable if the
        // accept sets are disjoint, so assert it in both directions rather
        // than assuming it from the version bytes differing in intent.
        for legacy in legacy_attachments() {
            assert!(
                decode_media_manifest(&legacy).is_err(),
                "a legacy inline attachment decoded as a media manifest"
            );
            assert!(
                recognize_media_manifest(MEDIA_MANIFEST_KIND, &legacy).is_none(),
                "a legacy inline attachment was recognized as media"
            );
            // Still an authorable body, though: widening kind 16 must not
            // retire the codec the fleet is running.
            assert!(decode_attachment_payload(legacy).is_some());
        }

        for manifest in manifests() {
            let body = media_manifest_body(&manifest).unwrap();
            assert!(
                decode_attachment_payload(body.clone()).is_none(),
                "a media manifest decoded as an inline attachment"
            );
            assert_eq!(
                recognize_media_manifest(MEDIA_MANIFEST_KIND, &body),
                Some(manifest)
            );
            assert_eq!(
                body[0], MANIFEST_WIRE_VERSION,
                "the version byte is what makes the two codecs disjoint"
            );
        }

        assert_ne!(
            MANIFEST_WIRE_VERSION, ATTACHMENT_WIRE_VERSION,
            "one kind, two codecs: their version bytes are the whole separation"
        );
    }

    #[test]
    fn a_ground_blob_id_cannot_make_one_body_both_codecs_accept() {
        // The adversarial shape the version bump exists to close. Every field
        // the legacy attachment decoder reads after its own two-byte header
        // lands inside the blob id here, so this digest is one a sender could
        // grind: an empty mime, a four-byte inline blob, an empty caption, and
        // trailing bytes the attachment codec permits by design.
        let mut ground = [0x00u8; 32];
        ground[6..10].copy_from_slice(&4u32.to_be_bytes()); // inline blob length
        ground[14..16].copy_from_slice(&0u16.to_be_bytes()); // caption length
        let manifest = MediaManifest {
            // Kind 1 is Photo here and Image there: the second byte collides
            // too, which is the point.
            blob_id: BlobId(ground),
            ..sample_manifest()
        };
        let body = media_manifest_body(&manifest).unwrap();

        assert!(
            decode_attachment_payload(body.clone()).is_none(),
            "a ground blob id must not make a manifest readable as an attachment"
        );
        assert_eq!(
            recognize_media_manifest(MEDIA_MANIFEST_KIND, &body),
            Some(manifest)
        );

        // And the version byte is load-bearing rather than incidental: the
        // same bytes under the attachment's version *are* accepted by it, so
        // nothing but that byte was standing between these two codecs.
        let mut collided = body;
        collided[0] = ATTACHMENT_WIRE_VERSION;
        assert!(
            decode_attachment_payload(collided.clone()).is_some(),
            "the shape really is a valid attachment; only the version separates them"
        );
        assert!(
            recognize_media_manifest(MEDIA_MANIFEST_KIND, &collided).is_none(),
            "and that body is not a manifest"
        );
    }

    #[test]
    fn nothing_but_a_manifest_under_the_attachment_kind_is_recognized() {
        let body = media_manifest_body(&sample_manifest()).unwrap();
        assert!(recognize_media_manifest(KIND_TEXT, &body).is_none());
        assert!(recognize_media_manifest(MEDIA_MANIFEST_KIND, b"").is_none());
        assert!(recognize_media_manifest(MEDIA_MANIFEST_KIND, b"hello").is_none());

        let mut trailing = body.clone();
        trailing.push(0);
        assert!(recognize_media_manifest(MEDIA_MANIFEST_KIND, &trailing).is_none());
    }

    #[test]
    fn a_recognized_manifest_opens_a_resumable_row() {
        let db = Connection::open_in_memory().unwrap();
        let store = BlobStore::open(&db).unwrap();
        let bytes = vec![7u8; 700_000];
        let manifest = manifest_for(&seal_media_blob(&bytes).unwrap());

        let record = begin_media_transfer(&store, &manifest, 1_000).unwrap();
        assert_eq!(record.geometry, manifest.geometry().unwrap());
        assert_eq!(record.chunk_file, chunk_file_name(&manifest.blob_id));
        assert!(record.manifest_unread, "nobody has opened the chat yet");
        assert_eq!(
            record.origin,
            BlobOrigin::Received,
            "mail from someone else is someone else's blob"
        );

        store.record_chunk(&manifest.blob_id, 1, 1_100).unwrap();
        // A redelivered manifest, or a restart: a resume, never a reset.
        let again = begin_media_transfer(&store, &manifest, 2_000).unwrap();
        assert_eq!(again.chunks_present, 1);
        assert_eq!(again.chunk_file, record.chunk_file);
    }

    #[test]
    fn only_a_blob_this_device_authored_is_ever_servable() {
        // BLOB-01's second clause at the seam that decides it. The two rows
        // are otherwise identical — same geometry, same chunk file rule, and
        // the received one is *complete* — so origin is the whole difference
        // between a source and a reader.
        let db = Connection::open_in_memory().unwrap();
        let store = BlobStore::open(&db).unwrap();
        let bytes = vec![3u8; 700_000];
        let authored = seal_media_blob(&bytes).unwrap();
        let manifest = manifest_for(&authored);

        let mine = begin_authored_media_blob(&store, &authored.sealed, 1_000).unwrap();
        assert_eq!(mine.origin, BlobOrigin::AuthoredHere);
        let mut held = ChunkBitmap::empty(mine.geometry.chunk_count).unwrap();
        for index in 0..mine.geometry.chunk_count {
            held.set(index);
        }
        assert!(
            servable_plan(&mine, &authored.key, held.clone(), ServeBudgets::default()).is_some(),
            "the sender serves its own copy"
        );

        // The same bytes as a completed download on a second device.
        let other_db = Connection::open_in_memory().unwrap();
        let other = BlobStore::open(&other_db).unwrap();
        let downloaded = begin_media_transfer(&other, &manifest, 1_000).unwrap();
        for index in 0..downloaded.geometry.chunk_count {
            other.record_chunk(&manifest.blob_id, index, 1_100).unwrap();
        }
        other.mark_verified(&manifest.blob_id, 1_200).unwrap();
        let finished = other.record(&manifest.blob_id).unwrap().unwrap();
        assert!(finished.complete && finished.verified);
        assert!(
            servable_plan(
                &finished,
                &manifest.blob_key,
                held.clone(),
                ServeBudgets::default()
            )
            .is_none(),
            "a completed download must not become a second source"
        );

        // Nor does re-opening the row with the authoring door change it: the
        // origin a row was created with is the origin it keeps.
        begin_authored_media_blob(&other, &authored.sealed, 2_000).unwrap();
        let reopened = other.record(&manifest.blob_id).unwrap().unwrap();
        assert_eq!(reopened.origin, BlobOrigin::Received);
        assert!(
            servable_plan(&reopened, &manifest.blob_key, held, ServeBudgets::default()).is_none()
        );
    }

    #[test]
    fn a_chunk_file_name_is_one_component_and_per_blob() {
        let first = chunk_file_name(&BlobId([0x11; 32]));
        let mut other = [0x11u8; 32];
        other[31] = 0x12;
        let second = chunk_file_name(&BlobId(other));

        assert_ne!(
            first, second,
            "two blobs must never append to one chunk file"
        );
        assert!(first.ends_with(".part"));
        assert!(
            !first.contains('/') && !first.contains('\\'),
            "a chunk file name is a name, not a path: {first}"
        );
    }

    #[test]
    fn the_consent_table() {
        use BlobTransferSource::{Lan, Relay};
        use CoreRelayNetworkVerdict::{DeferredConstrained, DeferredRoaming, Permitted};

        let bytes = 34_000_000;
        let cases = [
            // LAN is free: auto-download on is the default and starts. The
            // network verdict is present in the row and irrelevant to it.
            (Lan, Permitted, false, true, BlobTransferVerdict::AutoStart),
            (
                Lan,
                DeferredRoaming,
                false,
                true,
                BlobTransferVerdict::AutoStart,
            ),
            (
                Lan,
                DeferredConstrained,
                false,
                true,
                BlobTransferVerdict::AutoStart,
            ),
            // Auto-download off is an offer, not a deferral: nothing is being
            // spent, so the person is asked rather than made to wait.
            (
                Lan,
                Permitted,
                false,
                false,
                BlobTransferVerdict::AskFirst {
                    ciphertext_bytes: bytes,
                },
            ),
            (
                Lan,
                DeferredRoaming,
                true,
                false,
                BlobTransferVerdict::AutoStart,
            ),
            // Relay on an ordinary path: always an explicit, size-aware action
            // first, and the confirmation is what turns it into a start.
            (
                Relay,
                Permitted,
                false,
                true,
                BlobTransferVerdict::AskFirst {
                    ciphertext_bytes: bytes,
                },
            ),
            (Relay, Permitted, true, true, BlobTransferVerdict::AutoStart),
            // Roaming defers, and a confirmation does not buy past it: the
            // only roaming override is the Advanced toggle, which is already
            // inside the network verdict.
            (
                Relay,
                DeferredRoaming,
                true,
                true,
                BlobTransferVerdict::Deferred {
                    network: DeferredRoaming,
                    ciphertext_bytes: bytes,
                },
            ),
            // A constrained path is an expensive path, so it is offered with a
            // size and started on a tap. Deferring it instead would strand a
            // phone in Low Data Mode with no way to fetch a photo at all.
            (
                Relay,
                DeferredConstrained,
                false,
                true,
                BlobTransferVerdict::AskFirst {
                    ciphertext_bytes: bytes,
                },
            ),
            (
                Relay,
                DeferredConstrained,
                true,
                true,
                BlobTransferVerdict::AutoStart,
            ),
        ];

        for (source, network, user_confirmed, auto_lan, expected) in cases {
            assert_eq!(
                blob_transfer_permitted(source, network, user_confirmed, bytes, auto_lan),
                expected,
                "source={source:?} network={network:?} confirmed={user_confirmed} auto_lan={auto_lan}",
            );
        }
    }

    #[test]
    fn a_relay_transfer_is_never_unattended_and_never_a_dead_end() {
        // BLOB-03 over the whole network matrix, in both directions, because
        // over-restriction is as much a bug as under-restriction:
        //
        //   * nothing on the relay ever starts without an explicit action, and
        //     the roaming deferral is not something an action can buy past —
        //     this is the half that keeps the plane inside the relay policy;
        //   * every path the relay policy has not closed outright is at least
        //     *offerable*, so a bubble can never sit in "Waiting for internet"
        //     with no reachable way out on a network the mailbox still uses.
        for roaming in [
            CoreRelayRoaming::Yes,
            CoreRelayRoaming::No,
            CoreRelayRoaming::Unknown,
        ] {
            for constrained in [false, true] {
                for user_allows_roaming in [false, true] {
                    let network =
                        core_relay_network_permitted(roaming, constrained, user_allows_roaming);
                    for user_confirmed in [false, true] {
                        for auto_lan in [false, true] {
                            let verdict = blob_transfer_permitted(
                                BlobTransferSource::Relay,
                                network,
                                user_confirmed,
                                1,
                                auto_lan,
                            );
                            let context = format!(
                                "roaming={roaming:?} constrained={constrained} \
                                 override={user_allows_roaming} confirmed={user_confirmed}"
                            );
                            if verdict == BlobTransferVerdict::AutoStart {
                                assert!(user_confirmed, "unattended relay start: {context}");
                                assert_ne!(
                                    network,
                                    CoreRelayNetworkVerdict::DeferredRoaming,
                                    "started on a roaming deferral: {context}",
                                );
                            }
                            let deferred = matches!(verdict, BlobTransferVerdict::Deferred { .. });
                            assert_eq!(
                                deferred,
                                network == CoreRelayNetworkVerdict::DeferredRoaming,
                                "only a roaming deferral may leave nothing to tap: {context}",
                            );

                            // And the LAN half of the same matrix: the verdict
                            // is not an input at all, so every row agrees with
                            // the one the relay policy never saw.
                            assert_eq!(
                                blob_transfer_permitted(
                                    BlobTransferSource::Lan,
                                    network,
                                    user_confirmed,
                                    1,
                                    auto_lan,
                                ),
                                blob_transfer_permitted(
                                    BlobTransferSource::Lan,
                                    CoreRelayNetworkVerdict::Permitted,
                                    user_confirmed,
                                    1,
                                    auto_lan,
                                ),
                                "a LAN transfer consulted the relay network verdict",
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_pull_sub_channel_is_gated_on_the_advertised_bit() {
        assert!(peer_speaks_blob_plane(CAP_MEDIA_BLOB));
        assert!(peer_speaks_blob_plane(
            core_own_capabilities() | CAP_MEDIA_BLOB
        ));
        assert!(!peer_speaks_blob_plane(0));
        assert!(
            !peer_speaks_blob_plane(core_own_capabilities()),
            "a build with no pull driver must not invite a pull session at itself"
        );
    }
}
