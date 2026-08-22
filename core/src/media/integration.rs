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
//! by a discriminator neither shipped build would understand. Both open with a
//! version byte and a small type byte, so the telling-apart is done by trying
//! the strict codec first — the manifest's header is fixed-width, its geometry
//! is validated, its thumbnail rules are enforced and it refuses trailing
//! bytes, where the attachment decoder reads a length prefix out of what would
//! be the first two bytes of a blob id and requires UTF-8 after it. A digest
//! is essentially never that. `the_two_kind_16_codecs_never_accept_each_other`
//! pins the separation in both directions.
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

use super::blob::{generate_blob_key, seal_blob, BlobId, BlobKey, SealedBlob};
use super::manifest::{decode_media_manifest, encode_media_manifest, MediaManifest};
use super::store::{BlobRecord, BlobStore};
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
        now_ms,
    )
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
        CoreAttachmentPayload,
    };
    use crate::media::manifest::{sample_file_manifest, sample_manifest, MediaKind};
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
        }
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

        store.record_chunk(&manifest.blob_id, 1, 1_100).unwrap();
        // A redelivered manifest, or a restart: a resume, never a reset.
        let again = begin_media_transfer(&store, &manifest, 2_000).unwrap();
        assert_eq!(again.chunks_present, 1);
        assert_eq!(again.chunk_file, record.chunk_file);
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
