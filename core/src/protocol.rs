//! Wire format: message bodies, receipts, and the BLE frame discriminator.
//!
//! This module owns everything that turns DESIGN.md's protocol prose into
//! actual bytes. It sits below the crypto layer conceptually but above it in
//! the code: `encode_message_body` produces the `payload` that
//! [`crate::seal_message`] signs-then-seals (DESIGN.md §6.3), and
//! `parse_frame` is the very first thing a BLE receiver runs on bytes coming
//! off the wire, before anything is decrypted.
//!
//! ## Message body (DESIGN.md §7.1)
//!
//! DESIGN.md §7.1 describes the plaintext body as `version | sender UserID |
//! chat id | lamport counter | timestamp | kind | payload`. Two of those
//! fields are deliberately *not* present in [`MessageBody`] here, because the
//! crypto layer (`crypto.rs`) already provides them without duplication:
//!
//! - **sender UserID**: `seal_message` embeds the sender's Ed25519 public key
//!   and signs the payload; `open_message` returns the verified
//!   `sender_user_id` alongside the decrypted payload. Re-stating the sender
//!   inside the body would be redundant (and an extra place for it to get
//!   out of sync with the signature that actually authenticates it).
//! - **version**: sealed envelopes are the thing that will eventually need a
//!   ratchet/PQ upgrade path (DESIGN.md §6.3's "envelope has a version byte
//!   precisely so..."); that byte belongs to the envelope format in
//!   `crypto.rs` (which carries it as its leading byte), not to the message
//!   body decoded from inside it.
//!
//! What's left — `kind`, `chat_id`, `lamport`, `timestamp`, `content` — is
//! exactly [`MessageBody`]. Wire layout (all multi-byte integers big-endian):
//!
//! ```text
//! offset  size  field
//! 0       1     kind            (u8; text=1, receipt=2, friend-request=3,
//!                               group-invite=4, attachment-manifest=16,
//!                               attachment-chunk=17, reaction=18, per
//!                               DESIGN.md §7.1)
//! 1       2     chat_id_len     (u16 BE)
//! 3       N     chat_id         (N = chat_id_len bytes)
//! 3+N     8     lamport         (u64 BE)
//! 11+N    8     timestamp       (i64 BE; ms since Unix epoch)
//! 19+N    4     content_len     (u32 BE)
//! 23+N    M     content         (M = content_len bytes)
//! then, zero or more private extensions:
//!         1     extension_type  (u8; 1 = reply-to msg_id,
//!                               0x20 = sender_device_id,
//!                               0x21 = sender roster head)
//!         2     extension_len   (u16 BE)
//!         X     extension_value (X = extension_len bytes)
//! ```
//!
//! `chat_id` uses a 16-bit length prefix (not 32-bit) because chat ids are
//! UserIDs or group ids -- tens of bytes at most; `content` uses a 32-bit
//! prefix since text bodies have more headroom (and §8 reserves room for
//! attachment-manifest bodies later). `encode_message_body` rejects fields
//! that do not fit those wire prefixes rather than silently truncating their
//! lengths. Decoding is fully checked
//! and never panics on attacker-controlled input; malformed or truncated
//! bytes return [`CoreError::Malformed`]. Unknown well-formed extensions are
//! skipped so adding future encrypted metadata does not make the base message
//! unreadable. [`decode_message_body`] intentionally returns only the legacy
//! fields; [`decode_extended_message_body`] also surfaces known extensions.
//! The reply-to extension is a 16-byte envelope `msg_id` inside the signed
//! and sealed payload, so public headers reveal no conversation linkage.
//!
//! The multi-device fields (`specs/multi-device-v1.md` §5) ride here for the
//! same reason: the envelope's **public header layout is unchanged**, so a
//! legacy peer sees bytes indistinguishable from today's, while the authoring
//! device and the sender's roster head stay inside the seal where only the
//! recipient reads them. Their absence is not an error and never will be — it
//! maps to [`crate::LEGACY_DEVICE_ID`], the one stream every v1 peer and every
//! pre-migration row already lives on.
//!
//! ## Receipts (DESIGN.md §7.2)
//!
//! A receipt is an ordinary [`MessageBody`] with `kind = KIND_RECEIPT`, whose
//! `content` is itself the encoded [`ReceiptContent`] below. Per §7.2,
//! receipts are **cumulative**: a receipt says "delivered/read through
//! `lamport` in `chat_id`, for messages from `sender_user_id`" -- not "I got
//! message N specifically". Re-sending the same (or an updated, higher)
//! cumulative receipt is always safe and idempotent, which is what lets a
//! lost receipt heal itself on the next peer sync. Layout (big-endian):
//!
//! ```text
//! offset  size  field
//! 0       2     chat_id_len         (u16 BE)
//! 2       N     chat_id             (N = chat_id_len bytes)
//! 2+N     2     sender_user_id_len  (u16 BE)
//! 4+N     M     sender_user_id      (M = sender_user_id_len bytes; whose
//!                                    messages this receipt acknowledges)
//! 4+N+M   8     lamport             (u64 BE; cumulative through this value)
//! 12+N+M  1     receipt_type        (u8; delivered=1, read=2)
//! then, optionally (D9 group receipts):
//!         2     group_id_len        (u16 BE; must be 16)
//!         16    group_id            (the group this watermark is about)
//! ```
//!
//! A 1:1 receipt omits the optional tail, so its bytes are unchanged. A group
//! receipt appends the group id; old decoders reject the trailing bytes
//! (`finish`) and drop the envelope, which is the compatibility path — they
//! must not record it as a 1:1 watermark.
//!
//! ## Group invites (DESIGN.md §6.5, §7.1)
//!
//! A group invite is an ordinary [`MessageBody`] with `kind =
//! KIND_GROUP_INVITE`, whose `content` is the encoded
//! [`crate::Group`] record:
//!
//! ```text
//! offset  size  field
//! 0       16    group_id            (random 16-byte group id)
//! 16      32    key                 (XChaCha20-Poly1305 group key)
//! 48      2     name_len            (u16 BE)
//! 50      N     name_utf8           (N = name_len bytes)
//! 50+N    2     member_count        (u16 BE)
//! then, per member:
//!         2     member_user_id_len  (u16 BE)
//!         M     member_user_id      (M = that length)
//! ```
//!
//! Invites are sent pairwise through the existing 1:1 sign-then-seal path
//! (`crypto.rs::seal_message`). Importing one means decoding this payload and
//! storing the resulting group config locally.
//!
//! ## BLE frame discriminator (DESIGN.md §5.2, §7.3)
//!
//! Every byte string handed to/from the BLE link is a *frame*: a 1-byte
//! frame-type prefix followed by a frame-type-specific body. This module
//! only defines the discriminator and the HELLO frame; the link layer's own
//! length-prefixing/fragmentation (DESIGN.md §5.2) is what delimits a frame
//! on the wire, so frame bodies here carry no additional internal length
//! prefix of their own -- "everything after the type byte" is the body.
//!
//! - `0x01` = HELLO: an **unauthenticated** `user_id` announcement.
//! - `0x02` = sealed envelope: crypto.rs's sealed blob, now wrapped in the
//!   §6.4 public header described below.
//! - `0x03` = DIGEST: a per-chat sync digest (DESIGN.md §7.3; layout below).
//! - `0x04` = LAN_ENDPOINT: an accepted link peer's current TCP listener
//!   candidate. This is reachability data, never authentication.
//! - `0x05` = TRANSPORT_PROBE: a request/response nonce used to measure an
//!   already-established transport without creating a chat message.
//!
//! **Why HELLO is deliberately unauthenticated:** BLE central/peripheral
//! roles only give you a transient, unauthenticated link (a MAC-layer
//! address, no identity). HELLO's only job is to let a receiver map that
//! transient link to a known contact for routing/UI purposes ("oh, this is
//! Dave's phone") before any sync traffic flows. It carries no proof of
//! possession and this is intentional, not an oversight: all real
//! authentication happens inside the sealed envelope, via the Ed25519
//! signature `crypto.rs::open_message` verifies, and confidentiality comes
//! from sealing to the *stored* contact's X25519 key (from a previously
//! verified `FriendCard`/QR scan, DESIGN.md §6.2) -- not from anything a
//! HELLO frame claims. A spoofed HELLO can mislead routing/UI ("who is
//! this") but cannot forge a message or read one: it can, at worst, cause a
//! peer to address a sealed envelope at the wrong recipient, who then simply
//! fails to decrypt it. That failure mode is cheap and already expected
//! under normal DTN operation (§3), so it was judged not worth spending a
//! signature (and the extra round trip / battery cost of verifying one) on
//! every connection handshake.
//!
//! ## Envelope frame header (DESIGN.md §6.4, §5.3)
//!
//! A `0x02` frame's body is no longer just the opaque sealed blob -- it's
//! prefixed with the public header DESIGN.md §6.4 says observers (including
//! future relays/mules, §5.3, §9) are allowed to see: enough to route and
//! dedupe an envelope without decrypting it. Layout (big-endian, fixed-width
//! fields with no length prefixes -- their sizes are part of the wire format,
//! not self-describing):
//!
//! ```text
//! offset  size  field
//! 0       16    msg_id           (random per-envelope id; the seen-ID
//!                                 dedupe key future gossip, §5.3, will use)
//! 16      1     hop_ttl          (u8; DEFAULT_HOP_TTL = 7 when freshly
//!                                 authored; decremented per relay hop --
//!                                 not yet done, since relaying isn't wired
//!                                 up yet)
//! 17      8     expiry           (i64 BE; ms since Unix epoch; carriers
//!                                 drop the envelope past this time)
//! 25      8     recipient_hint   (BLAKE2b-8(recipient UserID || day
//!                                 number); lets a relay/mule cheaply test
//!                                 "could this be for someone I carry for"
//!                                 without decrypting; rotates daily so it
//!                                 isn't a stable tracking identifier)
//! 33      M     sealed           (the rest of the frame; either
//!                                 `crypto.rs::seal_message` for pairwise
//!                                 traffic, including `kind=4` invites, or
//!                                 `groups.rs::seal_group_message` for
//!                                 group-authored traffic)
//! ```
//!
//! `sender_user_id` is deliberately absent from this header (unlike
//! `recipient_hint`, which names the *recipient*): the whole point of
//! sign-then-seal (§6.3) is that sender identity only comes out on
//! successful decryption, so a header-level sender field would undermine
//! that. Today (direct-link delivery only, no gossip/mule engine yet) every
//! header field except `sealed` is inert on receive -- `parse_frame` decodes
//! them so the type exists for §5.3's relay/carry-queue work to consume
//! later, but `MeshService` doesn't act on `hop_ttl`/`expiry`/
//! `recipient_hint` yet. [`generate_msg_id`], [`compute_recipient_hint`], and
//! [`default_expiry`] are the canonical ways to produce these fields.
//! Group messages use the same header unchanged; the only difference is that
//! `recipient_hint` is derived from the group id instead of a user id, and
//! the sealed tail's private format is `version(1) | nonce(24) |
//! ciphertext+tag`, where the ciphertext opens to the same signed+padded
//! inner body shape 1:1 messages use.
//!
//! ## DIGEST frame (DESIGN.md §7.3)
//!
//! On connect, each peer sends a digest summarizing what it already has so
//! the other side can send just the difference (via the store's
//! `messages_after`). One digest frame covers **one** chat -- in the 1:1
//! case the chat is named by the sender's own UserID, per the wire
//! convention the Android `MeshService` already uses for envelopes; a
//! group-scoped digest (D9) uses the group id instead and is dropped by old
//! clients via [`crate::digest_is_expected_chat_id`]. Each frame carries one
//! entry per sender in that chat: "(sender_user_id, through_lamport)", i.e.
//! "I have this sender's messages contiguously through this lamport"
//! ([`crate::DigestEntry`], computed by the store's `chat_digest`). Layout
//! (big-endian, like everything else here):
//!
//! ```text
//! offset  size  field
//! 0       1     frame type          (0x03)
//! 1       2     chat_id_len         (u16 BE)
//! 3       N     chat_id             (N = chat_id_len bytes)
//! 3+N     2     entry_count         (u16 BE)
//! then, per entry:
//!         2     sender_user_id_len  (u16 BE)
//!         M     sender_user_id      (M = sender_user_id_len bytes)
//!         8     through_lamport     (u64 BE)
//! then:
//!         2     recent_msg_id_count (u16 BE)
//! then, per recent msg id:
//!         16    msg_id              (the exact msg_id bytes)
//! ```
//!
//! Unlike HELLO/envelope bodies, the digest body is structured (it holds a
//! list), so it carries internal length prefixes; the frame as a whole is
//! still delimited by the BLE link layer (§5.2), and decoding rejects
//! trailing garbage. `entry_count` may be 0 -- "I have nothing in this
//! chat" is a valid, useful digest (it asks for everything). Like HELLO, a
//! digest is unauthenticated link-layer chatter: lying in one can at worst
//! cause a peer to retransmit sealed envelopes (idempotent by design,
//! §7.3) or withhold them from a link, never to disclose or forge content.
//! §7.3's "recent msg_id bloom filter" component ships here as an **exact**
//! list of recent msg_ids instead of a bloom filter for now. At family scale
//! the exact list is small enough, false positives would be worse than a few
//! extra bytes, and this is enough to unlock mule spray-on-connect without
//! blindly resending a whole carry queue every reconnect. A true bloom filter
//! can replace this field later without revisiting the higher-level sync
//! algorithm.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use ed25519_dalek::{Signature, Signer, Verifier};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::crypto::{signing_key_from_bytes, verifying_key_from_bytes};
use crate::device_roster::{DEVICE_ID_LEN, ROSTER_HEAD_HASH_LEN};
use crate::identity::derive_user_id;
use crate::json_fault::json_fault;
use crate::limits::{MAX_ENVELOPE_SEALED_BYTES, MAX_P2P_FRAME_BYTES};
use crate::store::DigestEntry;
use crate::{CoreError, Identity};

/// `MessageBody.kind` value for an ordinary text message (DESIGN.md §7.1).
pub const KIND_TEXT: u8 = 1;
/// `MessageBody.kind` value for a receipt (DESIGN.md §7.1, §7.2); `content`
/// is an encoded [`ReceiptContent`].
pub const KIND_RECEIPT: u8 = 2;
/// Group ids are 16 random bytes, the same width as a UserID. A group
/// receipt's optional tail must be exactly this long (T4-10).
pub const GROUP_ID_LEN: usize = 16;
/// `MessageBody.kind` value for a signed friend-request envelope (DESIGN.md
/// §6.2, §7.1). The payload is application-defined contact-import content.
pub const KIND_FRIEND_REQUEST: u8 = 3;
/// `MessageBody.kind` value for a pairwise-sealed group invite whose
/// `content` is an encoded [`crate::Group`] record.
pub const KIND_GROUP_INVITE: u8 = 4;
/// `MessageBody.kind` value for a profile-sync: durable contact metadata
/// (display name + avatar), newest epoch wins.
pub const KIND_PROFILE_SYNC: u8 = 5;
/// A replaceable, pairwise-sealed snapshot of friends the sender may
/// introduce to the recipient. Hidden from chat history.
pub const KIND_FRIEND_DIRECTORY: u8 = 6;
/// A friend request authorized by a mutual friend's transferable ticket.
/// Hidden from chat history.
pub const KIND_INTRODUCED_FRIEND_REQUEST: u8 = 7;
/// Encrypted, short-lived same-LAN endpoint candidate for an accepted contact.
pub const KIND_LAN_ENDPOINT_HINT: u8 = 8;
/// T23: the sender's own relay endpoint changed. A friend card is a snapshot
/// of the sharer's relay config at share time, so a contact who buys a Cruise
/// Pass, rotates a token, or migrates servers leaves every peer posting to a
/// dead mailbox forever. This kind is the repair path: newest epoch wins,
/// scoped to the sealing sender, and it carries a *deposit*-class credential
/// only (CP4). Hidden from chat history.
pub const KIND_RELAY_UPDATE: u8 = 9;

// --- §8 self-sync record kinds ---------------------------------------------
//
// Six kinds, filling the 10..=15 gap exactly, one per record kind
// `specs/multi-device-v1.md` §8 enumerates. They are *sealed-body* kinds: the
// envelope's public header (§6.4) is unchanged, so a legacy peer that somehow
// received one sees bytes indistinguishable from ordinary 1:1 mail and drops
// it at its `unhandled kind` arm — the WPT forward-tolerance guarantee, which
// is why no new capability bit is needed here.
//
// They also never legitimately reach a contact at all. A sync record is sealed
// to the person's own inbox key (§6) and addressed only to own devices, so
// [`crate::core_pairwise_sender_authorized`] admits these kinds on the
// `sender_is_self` branch alone — never from a contact, however well
// authenticated. That is SYNC-3's person boundary expressed as an accept rule
// rather than as a comment.
//
// One kind per record kind, rather than one wrapper kind with an inner
// discriminator, because the downstream tables that answer "does this kind
// leave a msg_id row", "is this a hidden spray kind", and "may this sender send
// it" all take a bare `u8` and would otherwise be unable to tell replaceable
// state (watermarks, contacts, settings — newest wins) from gap-filled history
// (which SYNC-1's anti-entropy must deliver exactly once). The u8 space had
// exactly six free values below the reserved attachment kinds, which is what
// makes the split affordable.
//
// ## The kind-number layout, in one place
//
// The 10..=15 block below is *full*, so SYNC-1's digest carrier
// ([`KIND_SYNC_DIGEST`]) could not join it and takes the next value above the
// kinds already spoken for:
//
// | value  | meaning                                                        |
// |--------|----------------------------------------------------------------|
// | 10..15 | §8 sync record kinds (History … Settings)                      |
// | 16, 17 | reserved attachment kinds — never reused, even though 17 is    |
// |        | still unproduced, because a shipped build already routes on it |
// | 18     | reaction                                                       |
// | 19     | group metadata update                                          |
// | 20     | §8 sync **digest** — SYNC-1's watermark exchange, a sync kind   |
// |        | in every respect but a *record* stream in none                 |
// | 21     | DL-3 roster gossip — the one roster-carrying kind addressed to  |
// |        | a CONTACT rather than to a sibling ([`KIND_ROSTER_GOSSIP`])     |
//
// So the sync kinds are deliberately *not* one contiguous range any more, and
// nothing may test for one: [`core_is_sync_record_kind`] is the only membership
// test, and both 10..=15 and 20 answer `true` through it.

/// §8 sync record: message history, authored and received
/// ([`crate::SyncHistoryPayload`]).
pub const KIND_SYNC_HISTORY: u8 = 10;
/// §8 sync record: delivered/read watermarks
/// ([`crate::SyncWatermarkPayload`]).
pub const KIND_SYNC_WATERMARK: u8 = 11;
/// §8 sync record: the contact list and contacts' rosters
/// ([`crate::SyncContactsPayload`]).
pub const KIND_SYNC_CONTACTS: u8 = 12;
/// §8 sync record: the person's own roster and inbox keys
/// ([`crate::SyncOwnRosterPayload`]). This is the one record kind that carries
/// person-scoped *secret* material, which is exactly why every sync record is
/// sealed to a key only own devices hold.
pub const KIND_SYNC_OWN_ROSTER: u8 = 13;
/// §8 sync record: group membership and state
/// ([`crate::SyncGroupsPayload`]). §11 leaves group crypto untouched; a
/// member's new device gets the group key through this record rather than
/// through a re-invite.
pub const KIND_SYNC_GROUPS: u8 = 14;
/// §8 sync record: the settings the product deems shared
/// ([`crate::SyncSettingsPayload`]).
pub const KIND_SYNC_SETTINGS: u8 = 15;

/// Whether `kind` is one of §8's self-sync kinds — the kinds that only ever
/// travel between one person's own devices.
///
/// Both shells and every dispatch table call this instead of listing the
/// values, so the set cannot drift between the accept gate, the routing lanes,
/// and the store. Note the set is **not** a contiguous range: see the layout
/// table above [`KIND_SYNC_HISTORY`].
#[uniffi::export]
pub fn core_is_sync_record_kind(kind: u8) -> bool {
    matches!(
        kind,
        KIND_SYNC_HISTORY
            | KIND_SYNC_WATERMARK
            | KIND_SYNC_CONTACTS
            | KIND_SYNC_OWN_ROSTER
            | KIND_SYNC_GROUPS
            | KIND_SYNC_SETTINGS
            | KIND_SYNC_DIGEST
    )
}

/// `MessageBody.kind` value for an attachment manifest (DESIGN.md §7.1
/// reserved, §8). Android currently embeds the media blob inline in the
/// manifest payload for BLE/relay-friendly sizes; `KIND_ATTACHMENT_CHUNK`
/// is reserved for a future external-chunk transfer path.
pub const KIND_ATTACHMENT_MANIFEST: u8 = 16;
/// Reserved for content-addressed attachment chunks (DESIGN.md §8). Not
/// yet produced or consumed by the current client.
pub const KIND_ATTACHMENT_CHUNK: u8 = 17;
/// Hidden chat-stream event carrying an emoji reaction to another message.
pub const KIND_REACTION: u8 = 18;
/// Hidden group-stream event carrying a convergent name/add-member update.
pub const KIND_GROUP_METADATA_UPDATE: u8 = 19;

/// §8 sync **digest**: SYNC-1's per-stream watermark exchange
/// ([`crate::SyncDigest`]), carried as a sealed record like every other sync
/// kind so the digest a device advertises is itself a document only that
/// person's devices can read (§2's "device count is invisible to other
/// people").
///
/// It sits at 20 rather than inside the 10..=15 block because that block was
/// already full when SYNC-1 needed a carrier, and 16..=19 are spoken for — see
/// the layout table above [`KIND_SYNC_HISTORY`]. The gap is deliberate: nothing
/// may infer sync membership from a range.
///
/// A digest is the one sync kind that is **not** a gap-filled stream. It is
/// authored on its own per-device stream so that it signs, seals, and dedupes
/// exactly like the others, but the receiver consumes it and never files a
/// stream slot for it: yesterday's watermark is worth nothing, so retaining or
/// backfilling one would be storage spent on a document that is stale the
/// moment it lands.
pub const KIND_SYNC_DIGEST: u8 = 20;

/// **DL-3's carrier**: this person's own roster document
/// ([`crate::core_encode_roster`]), sealed pairwise to one contact.
///
/// `specs/multi-device-v1.md` §4 DL-3 says rosters "gossip exactly like other
/// sealed 1:1 traffic — relay, LAN, BLE, and carry equally, sealed pairwise per
/// contact", and §9 step 5 says the person's contacts are told when the roster
/// changes. This is the kind that does it, and it is deliberately **not** one of
/// §8's sync kinds even though it carries the same document a
/// [`KIND_SYNC_OWN_ROSTER`] record does:
///
/// * the recipient set is the opposite one — contacts, not siblings — so the
///   SYNC-3 accept rule that admits a sync record from `sender_is_self` alone
///   would refuse every legitimate copy of this;
/// * the sealing is pairwise to the contact's own agreement key, never to the
///   person inbox key, so no third party and no relay ever sees roster
///   plaintext (DL-3's "the relay never sees roster plaintext"); and
/// * the payload carries **no secret at all** — a roster is public keys,
///   counters and tombstones (DL-5) — whereas the sync record beside it ships
///   inbox key material and must never leave the person boundary.
///
/// The public header is unchanged, so a legacy peer sees ordinary 1:1 mail and
/// declines it — the WPT sealed-body tolerance, which is why the *envelope*
/// needs no new version. What stops a peer *this* build's age is not an
/// unhandled-kind arm at all: `deliver_inbound_body`'s roster arm refuses a
/// gossiped roster whose `person_id` is not the identity that sealed it, which
/// is DL-3's authorization gate and the reason a genuine roster about a third
/// party cannot be replayed onto anyone.
///
/// What the kind does need is [`CAP_ROSTER_GOSSIP`], for the spray-bounding
/// reason T23 established for [`KIND_RELAY_UPDATE`]: a build that predates kind
/// 21 stores no row for it and so never advances its DELIVERED watermark past
/// it. The bit is asked per kind ([`hidden_ack_capability`]), so that costs such
/// a peer the once-per-session bound on this kind alone.
pub const KIND_ROSTER_GOSSIP: u8 = 21;

/// `ReceiptContent.receipt_type` value: recipient's device decrypted and
/// stored the message (the ✓✓ tick, DESIGN.md §7.2).
pub const RECEIPT_TYPE_DELIVERED: u8 = 1;
/// `ReceiptContent.receipt_type` value: recipient viewed the chat (the
/// filled ✓✓ tick, DESIGN.md §7.2).
pub const RECEIPT_TYPE_READ: u8 = 2;

/// DESIGN.md §5.3: hop budget a freshly authored envelope starts with.
pub const DEFAULT_HOP_TTL: u8 = 7;
/// DESIGN.md §5.3: how long (in ms) a freshly authored envelope lives before
/// carriers should drop it. See [`default_expiry`].
pub const DEFAULT_EXPIRY_MS: i64 = 7 * 24 * 60 * 60 * 1000;

const FRAME_TYPE_HELLO: u8 = 0x01;
const FRAME_TYPE_ENVELOPE: u8 = 0x02;
const FRAME_TYPE_DIGEST: u8 = 0x03;
const FRAME_TYPE_LAN_ENDPOINT: u8 = 0x04;
const FRAME_TYPE_TRANSPORT_PROBE: u8 = 0x05;
const FRAME_TYPE_HELLO2: u8 = 0x06;
/// `specs/multi-device-v1.md` §10 step 5: this person's own signed roster
/// document, pushed between two devices of ONE person on a link that has
/// already proved it belongs to that person. See [`encode_own_roster`] for the
/// rule about which links those are — it is the whole safety of the frame.
const FRAME_TYPE_OWN_ROSTER: u8 = 0x07;

/// Wire length of a UserID in HELLO2 (BLAKE2b-16 of the signing key,
/// [`crate::identity`]). Legacy HELLO never hardcoded this because its
/// user_id was the frame remainder; HELLO2 must, since capabilities follow.
const HELLO2_USER_ID_LEN: usize = 16;

/// Capability bit: this client inserts hidden-kind envelopes (friend
/// requests, profile sync, friend directory, introduced requests) as
/// `messages` rows on receipt, so its DELIVERED watermark advances past them
/// and the sender can stop re-spraying. Every build that speaks HELLO2 has
/// this behavior, but future capabilities get their own bits.
pub const CAP_ACKS_HIDDEN_KINDS: u32 = 1;

/// Capability bit (T23): this client understands [`KIND_RELAY_UPDATE`] and
/// stores it as a `messages` row on receipt, so its DELIVERED watermark
/// advances past a relay-change notice.
///
/// This needs its own bit rather than riding [`CAP_ACKS_HIDDEN_KINDS`]
/// because that bit enumerates a *fixed* set of kinds (3/5/6/7). A build
/// that predates kind 9 still advertises `CAP_ACKS_HIDDEN_KINDS` truthfully
/// and still drops kind 9 at its `unhandled kind` arm — so trusting bit 1
/// alone would let the spray plan re-offer a relay-change notice on every
/// digest for the envelope's full 7-day expiry, which is precisely the
/// mixed-version resend chatter HELLO2 was introduced to end.
pub const CAP_RELAY_UPDATE: u32 = 1 << 1;

/// Capability bit for multi-device (`specs/multi-device-v1.md` §12). WPT
/// reserved it so the assignment could not drift and so an unknown future bit
/// on a peer was pinned as ignorable; WP1 flips the advertisement.
///
/// What this bit truthfully claims, and no more: this build understands §5's
/// per-device author streams — it reads the sealed-body `sender_device_id`,
/// keeps each of a person's devices on its own stream, and maps an absent
/// field onto `LEGACY_DEVICE_ID`. It does NOT claim per-device relay fan-out
/// or the ACK-MD rules; those are WP2's, and nothing a peer does with this bit
/// may assume them. Legacy HELLO (frame 0x03) is untouched and never grows a
/// field — the bit rides HELLO2's frame 0x06 only.
///
/// The claim is about the store's stream model, which every path shares. A
/// receive path that has not yet adopted the device-aware insert files its
/// peers on the legacy stream — the same conservative one-device view a v1
/// build has, never a wrong stream and never a dropped message.
pub const CAP_MULTI_DEVICE: u32 = 1 << 2;

/// Capability bit: this client understands [`KIND_ROSTER_GOSSIP`] and stores
/// it as a `messages` row on receipt, so its DELIVERED watermark advances past
/// a roster a contact gossiped (DL-3).
///
/// It gets its own bit rather than riding [`CAP_MULTI_DEVICE`] for the reason
/// T23 wrote down when [`CAP_RELAY_UPDATE`] was split off, and for a second one
/// specific to this spec. The general reason: a bit that covers a fixed set of
/// kinds cannot silently grow a member, or a build that advertised it honestly
/// before the kind existed starts being trusted to ack something it drops
/// unhandled — which is precisely the mixed-version resend chatter HELLO2 was
/// introduced to end. The specific one: [`CAP_MULTI_DEVICE`]'s own doc states
/// exactly what it claims (§5's per-device author streams) and disclaims
/// everything else, and WP1 shipped it under that promise. Widening the promise
/// retroactively would make an already-deployed advertisement mean something
/// its build never implemented.
pub const CAP_ROSTER_GOSSIP: u32 = 1 << 3;

/// Capability bit: this client understands [`Frame::OwnRoster`] (frame type
/// `0x07`), §10 step 5's own-roster notice.
///
/// Its own bit, for the reason every bit above got one: a build that predates
/// the frame refuses the unknown type byte in [`parse_frame`] and both shells
/// drop an unparseable frame without touching the link, so sending it blind is
/// *safe* — but it is also pointless, and a peer that cannot read the notice
/// must not be counted as one that has been told. The bit is what lets a sender
/// tell "this sibling now knows" from "this sibling could not have heard".
///
/// It is not a hidden spray kind and appears in no envelope: the notice is a
/// link-control frame, so it has no relay row, no `msg_id`, and no DELIVERED
/// watermark to advance. [`hidden_ack_capability`] therefore never names it.
pub const CAP_OWN_ROSTER_NOTICE: u32 = 1 << 4;

/// The capability bits this build advertises in HELLO2. Both shells call
/// this instead of hardcoding bits so they can never disagree with core.
#[uniffi::export]
pub fn core_own_capabilities() -> u32 {
    CAP_ACKS_HIDDEN_KINDS
        | CAP_RELAY_UPDATE
        | CAP_MULTI_DEVICE
        | CAP_ROSTER_GOSSIP
        | CAP_OWN_ROSTER_NOTICE
}

/// The sideband kinds that ride `outbound_envelopes` with a `msg_id = NULL`
/// messages row: excluded from digests and recent-msg-id acks, so their only
/// stop condition is the peer's DELIVERED watermark advancing — which never
/// happens against a peer that lacks [`CAP_ACKS_HIDDEN_KINDS`]. The spray
/// plan bounds re-sends of exactly these kinds toward such peers.
///
/// §8's sync record kinds are deliberately absent, and their absence is a
/// decision rather than an omission: this list is about what a *peer* can be
/// trusted to ack, and a sync record never reaches a peer — it is addressed to
/// this person's own devices, which are by construction builds that understand
/// it. SYNC-1's anti-entropy gives own-device traffic its own stop condition
/// (a sibling's stream watermark), so nothing here needs to bound its re-sends.
///
/// [`KIND_ROSTER_GOSSIP`] is present for the mirror-image reason: DL-3 gossip
/// goes to a *contact*, whose build may predate the kind entirely, and a roster
/// is re-offered on every link session until that contact's watermark moves
/// past it. Bounding it is what keeps telling a friend about a new device from
/// costing a re-spray on every digest for the envelope's full seven days.
pub const HIDDEN_SPRAY_KINDS: [u8; 6] = [
    KIND_FRIEND_REQUEST,
    KIND_PROFILE_SYNC,
    KIND_FRIEND_DIRECTORY,
    KIND_INTRODUCED_FRIEND_REQUEST,
    KIND_RELAY_UPDATE,
    KIND_ROSTER_GOSSIP,
];

#[uniffi::export]
pub fn core_is_hidden_spray_kind(kind: u8) -> bool {
    HIDDEN_SPRAY_KINDS.contains(&kind)
}

/// The capability bit a peer must advertise before its DELIVERED watermark can
/// be trusted to advance past this hidden spray kind. `None` for every kind
/// that is not a hidden spray kind — those need no bit, because their rows are
/// stored by every build and acked by the ordinary watermark.
///
/// One bit per kind, mapped here rather than folded into a single all-or-nothing
/// mask, so an advertisement a deployed build made honestly keeps meaning
/// exactly what it meant when that build shipped. A phone in the field today
/// advertises [`CAP_ACKS_HIDDEN_KINDS`] and [`CAP_RELAY_UPDATE`] and means them:
/// it really does store a friend request, a profile sync and a relay-change
/// notice, and its watermark really does move past them. It does not know kind
/// 21. Asking "does this peer ack *everything*?" would answer no and quietly
/// demote all five older kinds to the once-per-session bound, on every peer in
/// the fleet, until the whole fleet updated — a real cost paid by traffic the
/// peer handles perfectly. Asking per kind charges that cost to the one kind
/// that earns it.
pub fn hidden_ack_capability(kind: u8) -> Option<u32> {
    match kind {
        KIND_RELAY_UPDATE => Some(CAP_RELAY_UPDATE),
        KIND_ROSTER_GOSSIP => Some(CAP_ROSTER_GOSSIP),
        KIND_FRIEND_REQUEST
        | KIND_PROFILE_SYNC
        | KIND_FRIEND_DIRECTORY
        | KIND_INTRODUCED_FRIEND_REQUEST => Some(CAP_ACKS_HIDDEN_KINDS),
        _ => None,
    }
}

/// Whether delivering a consumed envelope of this `kind` leaves durable
/// accepted-message or conflict-quarantine evidence carrying the envelope's
/// `msg_id` -- i.e. whether
/// [`crate::MessageStore::message_origin_by_msg_id`] can later be asked "did
/// THIS device consume that exact envelope?" and answer truthfully.
///
/// Exactly the kinds delivered through `insert_incoming_message` (which takes
/// a `msg_id`) qualify: 1:1 and group chat text, attachment manifests,
/// reactions, and group metadata updates. Every other kind -- receipts,
/// friend requests, group invites, profile sync, the friend directory, LAN
/// endpoint hints, relay-change notices, and any kind a future build sends
/// that this one drops as unhandled -- is delivered through paths that
/// persist no `msg_id` (`insert_message`, or a store write of some other
/// shape, or nothing at all). Those are the "hidden kinds" whose relay copies
/// used to be unackable for lack of any evidence, and whose consumption is
/// instead recorded by
/// [`crate::MessageStore::core_record_consumed_hidden_msg_id`].
///
/// Deliberately NOT the same set as [`core_is_hidden_spray_kind`], which
/// answers a different question (which kinds ride `outbound_envelopes` with a
/// NULL-`msg_id` row and so never advance a peer's DELIVERED watermark).
/// `KIND_RECEIPT` is the highest-volume kind in a real mailbox and is hidden
/// *here* while deliberately not being a hidden spray kind.
///
/// §8's sync record kinds answer `false`, which is correct rather than
/// incidental: a sync record is not a chat message and leaves no `messages`
/// row of its own — it *carries* rows, whose own stream keys and msg_ids come
/// from [`crate::SyncHistoryEntry`]. Its consumption is therefore recorded
/// through [`crate::MessageStore::core_record_consumed_hidden_msg_id`], the
/// same evidence route every other row-less kind uses, which is what lets
/// ACK-MD-1 delete the sibling's fan-out row once this device has it.
///
/// [`KIND_ROSTER_GOSSIP`] answers `false` on the same reasoning and takes the
/// same route. A gossiped roster is not a chat message: what it leaves behind
/// is a row in `contact_rosters`, keyed by the person it describes rather than
/// by the envelope that carried it, so there is no `msg_id` to look up
/// afterwards. Its consumed-hidden evidence is what lets the relay copy of a
/// roster this device has already applied be deleted instead of refetched on
/// every poll pass for seven days.
#[uniffi::export]
pub fn core_kind_persists_msg_id_row(kind: u8) -> bool {
    matches!(
        kind,
        KIND_TEXT | KIND_ATTACHMENT_MANIFEST | KIND_REACTION | KIND_GROUP_METADATA_UPDATE
    )
}

const MSG_ID_LEN: usize = 16;
const RECIPIENT_HINT_LEN: usize = 8;
const LAN_INSTANCE_TOKEN_LEN: usize = 8;
const LAN_ENDPOINT_VERSION: u8 = 1;
const TRANSPORT_PROBE_VERSION: u8 = 1;
const MAX_LAN_HOST_BYTES: usize = u8::MAX as usize;
/// Longest interface/scope suffix accepted after `%` in an IPv6 link-local
/// host ("fe80::1%wlan0"). Real interface names are far shorter.
const MAX_LAN_HOST_ZONE_BYTES: usize = 32;
const MESSAGE_EXTENSION_REPLY_TO_MSG_ID: u8 = 1;
/// §5: the 16-byte id of the device that authored this body, the device
/// dimension of the stream key `(chat_id, sender_person_id, sender_device_id,
/// lamport)`. Type `0x20` is the id WPT's tolerance test already earmarked for
/// it, so a build in the field has been skipping exactly this byte since before
/// it meant anything.
const MESSAGE_EXTENSION_SENDER_DEVICE_ID: u8 = 0x20;
/// §12: BLAKE2b-256 of the sender's current roster — the same digest a
/// `CMFRIEND4:` card carries. A recipient whose stored roster head differs
/// knows it is behind and can ask for the roster; it is a reference, never the
/// document, so an envelope never grows by a device list.
const MESSAGE_EXTENSION_SENDER_ROSTER_HEAD: u8 = 0x21;
/// Milliseconds in a day, for the [`compute_recipient_hint`] daily-rotating
/// salt. `pub` (not just `const`) so `engine.rs`'s D2 mule-drain-confirm
/// hint window can reuse the same constant instead of re-deriving it --
/// single source of truth, mirroring [`DEFAULT_EXPIRY_MS`].
pub const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;
const PROFILE_SYNC_VERSION: u8 = 2;
const PROFILE_SYNC_MAX_AVATAR_BYTES: usize = 64 * 1024;
const PROFILE_SYNC_MAX_NAME_BYTES: usize = 128;
const RELAY_UPDATE_VERSION: u8 = 1;
const RELAY_UPDATE_MAX_URL_BYTES: usize = 512;
const RELAY_UPDATE_MAX_TOKEN_BYTES: usize = 256;
const RELAY_UPDATE_MAX_SUBJECT_BYTES: usize = 64;
const FRIEND_DIRECTORY_VERSION: u8 = 1;
const INTRODUCED_FRIEND_REQUEST_VERSION: u8 = 1;
const LAN_ENDPOINT_CONTENT_VERSION: u8 = 1;
const FRIEND_DIRECTORY_MAX_BYTES: usize = 64 * 1024;
const FRIEND_DIRECTORY_MAX_ENTRIES: usize = 64;
const INTRODUCTION_MAX_LIFETIME_MS: i64 = 30 * MS_PER_DAY;
const INTRODUCTION_CLOCK_SKEW_MS: i64 = 24 * 60 * 60 * 1000;

/// The plaintext body that gets encoded, then handed as `payload` to
/// [`crate::seal_message`] (DESIGN.md §7.1). See the module docs for the
/// exact byte layout and for why `version`/sender UserID aren't fields here.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct MessageBody {
    pub kind: u8,
    pub chat_id: Vec<u8>,
    pub lamport: u64,
    pub timestamp: i64,
    pub content: Vec<u8>,
}

/// A decoded message body plus optional encrypted metadata appended after
/// the legacy content field. Unknown extension types are skipped.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ExtendedMessageBody {
    pub kind: u8,
    pub chat_id: Vec<u8>,
    pub lamport: u64,
    pub timestamp: i64,
    pub content: Vec<u8>,
    pub reply_to_msg_id: Option<Vec<u8>>,
    /// §5: the authoring device, or `None` for every legacy sender. `None` is
    /// the permanent, expected case for a v1 peer — map it with
    /// [`crate::core_device_stream_id`] rather than treating it as missing
    /// data.
    pub sender_device_id: Option<Vec<u8>>,
    /// §12: the sender's roster head at authoring time, or `None`. Purely
    /// informational to WP1: nothing here fetches a roster yet.
    pub sender_roster_head: Option<Vec<u8>>,
}

/// Encode a [`MessageBody`] to its wire form (see module docs for layout).
#[uniffi::export]
pub fn encode_message_body(body: MessageBody) -> Result<Vec<u8>, CoreError> {
    validate_message_body_fields(body.kind, &body.chat_id, body.lamport, &body.content)?;
    if body.chat_id.len() > u16::MAX as usize {
        return Err(CoreError::Malformed("message chat id is too long".into()));
    }
    if body.content.len() > u32::MAX as usize {
        return Err(CoreError::Malformed("message content is too long".into()));
    }
    let mut out = Vec::with_capacity(1 + 2 + body.chat_id.len() + 8 + 8 + 4 + body.content.len());
    out.push(body.kind);
    write_bytes16(&mut out, &body.chat_id);
    out.extend_from_slice(&body.lamport.to_be_bytes());
    out.extend_from_slice(&body.timestamp.to_be_bytes());
    write_bytes32(&mut out, &body.content);
    Ok(out)
}

/// Encode a message body with an encrypted reference to the message being
/// replied to. The fixed-width id is the target envelope's public `msg_id`,
/// but it remains private here because the extension is inside the seal.
#[uniffi::export]
pub fn encode_message_body_with_reply(
    body: MessageBody,
    reply_to_msg_id: Vec<u8>,
) -> Result<Vec<u8>, CoreError> {
    encode_message_body_extended(body, Some(reply_to_msg_id), None, None)
}

/// Encode a message body with any combination of the private extensions,
/// including the multi-device ones (`specs/multi-device-v1.md` §5, §12).
///
/// Passing `None` for `sender_device_id` emits no device TLV at all, which is
/// byte-for-byte what every build in the field emits today and what a recipient
/// maps onto [`crate::LEGACY_DEVICE_ID`]. That is the WP1 default: a device may
/// not claim a device id before it has one, and §9.4's two-phase activation
/// says a device authors nothing until the roster that names it is
/// acknowledged, so the authoring call sites start passing a real id in the
/// work package that mints one.
#[uniffi::export]
pub fn encode_message_body_extended(
    body: MessageBody,
    reply_to_msg_id: Option<Vec<u8>>,
    sender_device_id: Option<Vec<u8>>,
    sender_roster_head: Option<Vec<u8>>,
) -> Result<Vec<u8>, CoreError> {
    let mut out = encode_message_body(body)?;
    // Extension order is fixed here so one body always encodes to one byte
    // string; the decoder accepts any order, because a peer's encoder is not
    // ours to constrain.
    if let Some(reply_to_msg_id) = reply_to_msg_id {
        push_extension(
            &mut out,
            MESSAGE_EXTENSION_REPLY_TO_MSG_ID,
            &reply_to_msg_id,
            MSG_ID_LEN,
            "reply_to_msg_id",
        )?;
    }
    if let Some(sender_device_id) = sender_device_id {
        push_extension(
            &mut out,
            MESSAGE_EXTENSION_SENDER_DEVICE_ID,
            &sender_device_id,
            DEVICE_ID_LEN,
            "sender_device_id",
        )?;
    }
    if let Some(sender_roster_head) = sender_roster_head {
        push_extension(
            &mut out,
            MESSAGE_EXTENSION_SENDER_ROSTER_HEAD,
            &sender_roster_head,
            ROSTER_HEAD_HASH_LEN,
            "sender_roster_head",
        )?;
    }
    Ok(out)
}

/// Append one fixed-width extension TLV, refusing a value of the wrong width
/// rather than emitting a field a recipient would have to reject.
fn push_extension(
    out: &mut Vec<u8>,
    extension_type: u8,
    value: &[u8],
    expected_len: usize,
    field: &str,
) -> Result<(), CoreError> {
    if value.len() != expected_len {
        return Err(CoreError::Malformed(format!(
            "{field} must be exactly {expected_len} bytes"
        )));
    }
    out.push(extension_type);
    out.extend_from_slice(&(expected_len as u16).to_be_bytes());
    out.extend_from_slice(value);
    Ok(())
}

/// Decode a [`MessageBody`] from its wire form. Rejects truncated input,
/// corrupt length prefixes, and malformed extension TLVs while ignoring
/// well-formed extensions the legacy record does not surface.
#[uniffi::export]
pub fn decode_message_body(bytes: Vec<u8>) -> Result<MessageBody, CoreError> {
    let extended = decode_extended_message_body(bytes)?;
    Ok(MessageBody {
        kind: extended.kind,
        chat_id: extended.chat_id,
        lamport: extended.lamport,
        timestamp: extended.timestamp,
        content: extended.content,
    })
}

/// Decode the legacy message fields and any known append-only extensions.
/// Unknown well-formed TLVs are ignored for forward compatibility.
#[uniffi::export]
pub fn decode_extended_message_body(bytes: Vec<u8>) -> Result<ExtendedMessageBody, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let kind = cursor.take_u8()?;
    let chat_id = cursor.take_bytes16()?;
    let lamport = cursor.take_u64()?;
    let timestamp = cursor.take_i64()?;
    let content = cursor.take_bytes32()?;
    let mut reply_to_msg_id = None;
    let mut sender_device_id = None;
    let mut sender_roster_head = None;
    while !cursor.is_finished() {
        let extension_type = cursor.take_u8()?;
        let extension_len = cursor.take_u16()? as usize;
        let value = cursor.take(extension_len)?;
        // Known types are checked; everything else is skipped, and that
        // asymmetry is the whole forward-compatibility contract (WPT, §5).
        match extension_type {
            MESSAGE_EXTENSION_REPLY_TO_MSG_ID => {
                take_extension(&mut reply_to_msg_id, value, MSG_ID_LEN, "reply-to")?;
            }
            MESSAGE_EXTENSION_SENDER_DEVICE_ID => {
                take_extension(
                    &mut sender_device_id,
                    value,
                    DEVICE_ID_LEN,
                    "sender device id",
                )?;
            }
            MESSAGE_EXTENSION_SENDER_ROSTER_HEAD => {
                take_extension(
                    &mut sender_roster_head,
                    value,
                    ROSTER_HEAD_HASH_LEN,
                    "sender roster head",
                )?;
            }
            _ => {}
        }
    }
    validate_message_body_fields(kind, &chat_id, lamport, &content)?;
    Ok(ExtendedMessageBody {
        kind,
        chat_id,
        lamport,
        timestamp,
        content,
        reply_to_msg_id,
        sender_device_id,
        sender_roster_head,
    })
}

/// Accept one fixed-width extension into its slot. A wrong width or a repeat
/// is malformed rather than ignored: unlike an unknown type, a known type is a
/// field this build understands, and understanding it half-way is worse than
/// not shipping it.
fn take_extension(
    slot: &mut Option<Vec<u8>>,
    value: &[u8],
    expected_len: usize,
    field: &str,
) -> Result<(), CoreError> {
    if value.len() != expected_len {
        return Err(CoreError::Malformed(format!(
            "{field} extension must be exactly {expected_len} bytes"
        )));
    }
    if slot.is_some() {
        return Err(CoreError::Malformed(format!("duplicate {field} extension")));
    }
    *slot = Some(value.to_vec());
    Ok(())
}

/// The decoded form of a receipt's `content` (a `MessageBody` with
/// `kind = KIND_RECEIPT`). See module docs for the exact byte layout and for
/// the cumulative-acknowledgement semantics (DESIGN.md §7.2).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ReceiptContent {
    pub chat_id: Vec<u8>,
    pub sender_user_id: Vec<u8>,
    pub lamport: u64,
    pub receipt_type: u8,
    /// When set, this receipt is about `sender_user_id`'s stream **in this
    /// group**, not the 1:1 chat with the envelope sender. Absent on every
    /// 1:1 receipt so those bytes stay identical to the pre-D9 layout.
    pub group_id: Option<Vec<u8>>,
}

/// The decoded form of a profile-sync `content` (a `MessageBody` with
/// `kind = KIND_PROFILE_SYNC`). Empty `avatar` means the sender removed
/// their profile photo.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ProfileSyncContent {
    pub avatar_epoch: i64,
    pub name: String,
    pub avatar: Vec<u8>,
    pub friends_of_friends_version: u8,
    pub friends_of_friends_enabled: bool,
    pub friends_of_friends_revision: u64,
}

/// The decoded form of a relay-change notice's `content` (a `MessageBody`
/// with `kind = KIND_RELAY_UPDATE`, T23).
///
/// `subject_user_id` is the UserID whose endpoint this notice claims to
/// change. It is always the sender's own: sealing already guarantees that,
/// but carrying it explicitly makes
/// [`crate::MessageStore::apply_contact_relay_update`] able to *reject* a
/// mis-scoped notice instead of trusting whichever id its caller happened to
/// pass. Endpoint privacy (CLAUDE.md) means a device announces only its own
/// endpoint and never forwards a third party's; this field is what lets core
/// enforce that rather than assume it.
///
/// `relay_token` is always a **deposit-class** credential (CP4). Empty
/// `relay_url` *and* `relay_token` mean "I no longer have internet delivery"
/// — an honest downgrade to nearby-only, not a no-op.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct RelayUpdateContent {
    pub subject_user_id: Vec<u8>,
    pub relay_epoch: i64,
    pub relay_url: String,
    pub relay_token: String,
}

/// Public identity forwarded by a mutual friend. Relay credentials and avatar
/// bytes are deliberately absent.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SuggestedFriendCard {
    pub name: String,
    pub user_id: Vec<u8>,
    pub sign_pk: Vec<u8>,
    pub agree_pk: Vec<u8>,
}

/// Transferable proof that an accepted contact introduced one exact invitee
/// to one exact candidate under the candidate's current discovery revision.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntroductionTicket {
    pub version: u8,
    pub introducer_user_id: Vec<u8>,
    pub candidate_user_id: Vec<u8>,
    pub invitee_user_id: Vec<u8>,
    pub candidate_policy_revision: u64,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub offer_id: Vec<u8>,
    pub signature: Vec<u8>,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FriendDirectoryEntry {
    pub candidate: SuggestedFriendCard,
    pub candidate_policy_revision: u64,
    pub ticket: IntroductionTicket,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FriendDirectoryContent {
    pub version: u8,
    pub revision: u64,
    pub entries: Vec<FriendDirectoryEntry>,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IntroducedFriendRequest {
    pub version: u8,
    pub friend_card_json: String,
    pub ticket: IntroductionTicket,
}

/// Short-lived endpoint candidate sent inside a sealed `kind = 8` message.
/// `network_id` is a hashed local-network fingerprint, never a raw SSID.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct LanEndpointContent {
    pub instance_token: Vec<u8>,
    pub network_id: Vec<u8>,
    pub host: String,
    pub port: u16,
    pub expires_at_ms: i64,
}

/// Encode a [`RelayUpdateContent`] to its wire form.
///
/// The credential is attenuated here, unconditionally, with
/// [`relay_deposit_token_for`] — callers hand over whatever their relay
/// config holds (normally the family's **member** token) and the deposit
/// form is what reaches the wire. Doing it in the encoder rather than
/// asking every call site to remember is the whole point: CP4 exists to keep
/// member tokens off anything a contact receives, and a member token
/// broadcast to every contact would re-open exactly the hole CP4 closed (a
/// member credential can fetch *and ack* — i.e. delete — a family's mail).
/// The derivation is idempotent, so a caller that already attenuated is
/// unaffected.
///
/// A half-configured endpoint (url without token, or the reverse) is not a
/// usable endpoint — [`crate::relay_wire::resolved_contact_relay`] would
/// discard it anyway — so it is normalized to the "no internet delivery"
/// form rather than emitted as a partial update.
#[uniffi::export]
pub fn encode_relay_update_content(content: RelayUpdateContent) -> Result<Vec<u8>, CoreError> {
    if content.subject_user_id.is_empty()
        || content.subject_user_id.len() > RELAY_UPDATE_MAX_SUBJECT_BYTES
    {
        return Err(CoreError::Malformed(
            "relay update subject user id is out of range".into(),
        ));
    }
    let url = crate::relay_wire::normalize_relay_url(content.relay_url);
    let token = crate::relay_wire::relay_deposit_token_for(content.relay_token);
    let (url, token) = if url.is_empty() || token.is_empty() {
        (String::new(), String::new())
    } else {
        (url, token)
    };
    if url.len() > RELAY_UPDATE_MAX_URL_BYTES {
        return Err(CoreError::Malformed(format!(
            "relay update url exceeds {RELAY_UPDATE_MAX_URL_BYTES} bytes"
        )));
    }
    if token.len() > RELAY_UPDATE_MAX_TOKEN_BYTES {
        return Err(CoreError::Malformed(format!(
            "relay update token exceeds {RELAY_UPDATE_MAX_TOKEN_BYTES} bytes"
        )));
    }
    let mut out = Vec::with_capacity(1 + 2 + content.subject_user_id.len() + 8 + 4 + url.len() + 2);
    out.push(RELAY_UPDATE_VERSION);
    write_bytes16(&mut out, &content.subject_user_id);
    out.extend_from_slice(&content.relay_epoch.to_be_bytes());
    write_bytes16(&mut out, url.as_bytes());
    write_bytes16(&mut out, token.as_bytes());
    Ok(out)
}

/// Decode a [`RelayUpdateContent`] from its wire form.
///
/// Rejects a member-class credential outright. Nothing in the field emits
/// kind 9 yet, so there is no legacy sender to stay compatible with, and
/// making the decoder refuse the member class means no relay-change notice
/// — however malformed, replayed, or hostile the sender — can ever install a
/// fetch/ack-capable credential for a contact.
#[uniffi::export]
pub fn decode_relay_update_content(bytes: Vec<u8>) -> Result<RelayUpdateContent, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let version = cursor.take_u8()?;
    if version != RELAY_UPDATE_VERSION {
        return Err(CoreError::Malformed(format!(
            "unknown relay-update version: {version}"
        )));
    }
    let subject_user_id = cursor.take_bytes16()?;
    if subject_user_id.is_empty() || subject_user_id.len() > RELAY_UPDATE_MAX_SUBJECT_BYTES {
        return Err(CoreError::Malformed(
            "relay update subject user id is out of range".into(),
        ));
    }
    let relay_epoch = cursor.take_i64()?;
    let url_bytes = cursor.take_bytes16()?;
    if url_bytes.len() > RELAY_UPDATE_MAX_URL_BYTES {
        return Err(CoreError::Malformed(format!(
            "relay update url exceeds {RELAY_UPDATE_MAX_URL_BYTES} bytes"
        )));
    }
    let token_bytes = cursor.take_bytes16()?;
    if token_bytes.len() > RELAY_UPDATE_MAX_TOKEN_BYTES {
        return Err(CoreError::Malformed(format!(
            "relay update token exceeds {RELAY_UPDATE_MAX_TOKEN_BYTES} bytes"
        )));
    }
    cursor.finish()?;
    let relay_url =
        String::from_utf8(url_bytes).map_err(|e| CoreError::Malformed(e.to_string()))?;
    let relay_token =
        String::from_utf8(token_bytes).map_err(|e| CoreError::Malformed(e.to_string()))?;
    validate_relay_update_credential(&relay_url, &relay_token)?;
    Ok(RelayUpdateContent {
        subject_user_id,
        relay_epoch,
        relay_url,
        relay_token,
    })
}

/// CP4 gate shared by the decoder and
/// [`crate::MessageStore::apply_contact_relay_update`]: a relay-change notice
/// carries either no endpoint at all, or a complete one whose credential is
/// deposit-class. Checked twice on purpose — the store must not depend on its
/// caller having gone through the decoder.
pub(crate) fn validate_relay_update_credential(
    relay_url: &str,
    relay_token: &str,
) -> Result<(), CoreError> {
    if relay_url.is_empty() != relay_token.is_empty() {
        return Err(CoreError::Malformed(
            "relay update must carry both a url and a token, or neither".into(),
        ));
    }
    if !relay_token.is_empty()
        && !crate::relay_wire::relay_token_is_deposit(relay_token.to_string())
    {
        return Err(CoreError::Malformed(
            "relay update credential must be deposit-class".into(),
        ));
    }
    Ok(())
}

/// Encode a [`ReceiptContent`] to its wire form (see module docs for layout).
#[uniffi::export]
pub fn encode_receipt_content(content: ReceiptContent) -> Result<Vec<u8>, CoreError> {
    validate_receipt_content(&content)?;
    if content.chat_id.len() > u16::MAX as usize || content.sender_user_id.len() > u16::MAX as usize
    {
        return Err(CoreError::Malformed("receipt identity is too long".into()));
    }
    let mut out = Vec::with_capacity(
        2 + content.chat_id.len()
            + 2
            + content.sender_user_id.len()
            + 8
            + 1
            + content
                .group_id
                .as_ref()
                .map(|id| 2 + id.len())
                .unwrap_or(0),
    );
    write_bytes16(&mut out, &content.chat_id);
    write_bytes16(&mut out, &content.sender_user_id);
    out.extend_from_slice(&content.lamport.to_be_bytes());
    out.push(content.receipt_type);
    if let Some(group_id) = &content.group_id {
        write_bytes16(&mut out, group_id);
    }
    Ok(out)
}

/// Decode a [`ReceiptContent`] from its wire form. Rejects truncated input,
/// corrupt length prefixes, and unexpected trailing bytes.
#[uniffi::export]
pub fn decode_receipt_content(bytes: Vec<u8>) -> Result<ReceiptContent, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let chat_id = cursor.take_bytes16()?;
    let sender_user_id = cursor.take_bytes16()?;
    let lamport = cursor.take_u64()?;
    let receipt_type = cursor.take_u8()?;
    let group_id = if cursor.is_finished() {
        None
    } else {
        Some(cursor.take_bytes16()?)
    };
    cursor.finish()?;
    let content = ReceiptContent {
        chat_id,
        sender_user_id,
        lamport,
        receipt_type,
        group_id,
    };
    validate_receipt_content(&content)?;
    Ok(content)
}

fn validate_message_body_fields(
    kind: u8,
    chat_id: &[u8],
    lamport: u64,
    content: &[u8],
) -> Result<(), CoreError> {
    if lamport > i64::MAX as u64 {
        return Err(CoreError::Malformed(
            "message lamport exceeds the supported range".into(),
        ));
    }
    match kind {
        KIND_RECEIPT => {
            let receipt = decode_receipt_content(content.to_vec())?;
            if receipt.chat_id != chat_id {
                return Err(CoreError::Malformed(
                    "receipt chat id does not match its message body".into(),
                ));
            }
        }
        KIND_ATTACHMENT_MANIFEST => {
            if crate::content::decode_attachment_payload(content.to_vec()).is_none() {
                return Err(CoreError::Malformed("invalid attachment payload".into()));
            }
        }
        KIND_REACTION if crate::content::decode_reaction_payload(content.to_vec()).is_none() => {
            return Err(CoreError::Malformed("invalid reaction payload".into()));
        }
        // DL-3: the body of a roster gossip is a roster document and nothing
        // else. Checked here, on the same pass as every other structured kind,
        // so bytes that could never be applied are refused at the codec instead
        // of being authored, queued, sprayed for a week, and dropped on arrival.
        // Whether the decoded document may be *believed* is still
        // `core_roster_accept`'s alone — this only says it is a roster.
        KIND_ROSTER_GOSSIP => {
            crate::core_decode_roster(content.to_vec())?;
        }
        _ => {}
    }
    Ok(())
}

fn validate_receipt_content(content: &ReceiptContent) -> Result<(), CoreError> {
    if content.receipt_type != RECEIPT_TYPE_DELIVERED && content.receipt_type != RECEIPT_TYPE_READ {
        return Err(CoreError::Malformed("invalid receipt type".into()));
    }
    if content.lamport > i64::MAX as u64 {
        return Err(CoreError::Malformed(
            "receipt lamport exceeds the supported range".into(),
        ));
    }
    if let Some(group_id) = &content.group_id {
        if group_id.len() != GROUP_ID_LEN {
            return Err(CoreError::Malformed(format!(
                "group receipt id must be exactly {GROUP_ID_LEN} bytes"
            )));
        }
    }
    Ok(())
}

/// Encode a [`ProfileSyncContent`] to its wire form.
#[uniffi::export]
pub fn encode_profile_sync_content(content: ProfileSyncContent) -> Result<Vec<u8>, CoreError> {
    let name = content.name.as_bytes();
    if name.len() > PROFILE_SYNC_MAX_NAME_BYTES {
        return Err(CoreError::Malformed(format!(
            "profile name exceeds {PROFILE_SYNC_MAX_NAME_BYTES} UTF-8 bytes"
        )));
    }
    if content.avatar.len() > PROFILE_SYNC_MAX_AVATAR_BYTES {
        return Err(CoreError::Malformed(format!(
            "profile avatar exceeds {PROFILE_SYNC_MAX_AVATAR_BYTES} bytes"
        )));
    }
    let mut out = Vec::with_capacity(1 + 8 + 2 + name.len() + 4 + content.avatar.len() + 10);
    out.push(PROFILE_SYNC_VERSION);
    out.extend_from_slice(&content.avatar_epoch.to_be_bytes());
    write_bytes16(&mut out, name);
    write_bytes32(&mut out, &content.avatar);
    out.push(content.friends_of_friends_version);
    out.push(u8::from(content.friends_of_friends_enabled));
    out.extend_from_slice(&content.friends_of_friends_revision.to_be_bytes());
    Ok(out)
}

/// Decode a [`ProfileSyncContent`] from its wire form.
#[uniffi::export]
pub fn decode_profile_sync_content(bytes: Vec<u8>) -> Result<ProfileSyncContent, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let version = cursor.take_u8()?;
    if version != 1 && version != PROFILE_SYNC_VERSION {
        return Err(CoreError::Malformed(format!(
            "unknown profile-sync version: {version}"
        )));
    }
    let avatar_epoch = cursor.take_i64()?;
    let name_bytes = cursor.take_bytes16()?;
    if name_bytes.len() > PROFILE_SYNC_MAX_NAME_BYTES {
        return Err(CoreError::Malformed(format!(
            "profile name exceeds {PROFILE_SYNC_MAX_NAME_BYTES} UTF-8 bytes"
        )));
    }
    let name = String::from_utf8(name_bytes).map_err(|e| CoreError::Malformed(e.to_string()))?;
    let avatar_len = cursor.take_u32()? as usize;
    if avatar_len > PROFILE_SYNC_MAX_AVATAR_BYTES {
        return Err(CoreError::Malformed(format!(
            "profile avatar too large: {avatar_len} bytes"
        )));
    }
    let avatar = cursor.take(avatar_len)?.to_vec();
    let (friends_of_friends_version, friends_of_friends_enabled, friends_of_friends_revision) =
        if version >= 2 {
            let protocol_version = cursor.take_u8()?;
            let enabled = match cursor.take_u8()? {
                0 => false,
                1 => true,
                value => {
                    return Err(CoreError::Malformed(format!(
                        "invalid friends-of-friends enabled value: {value}"
                    )))
                }
            };
            let revision = cursor.take_u64()?;
            (protocol_version, enabled, revision)
        } else {
            (0, false, 0)
        };
    cursor.finish()?;
    Ok(ProfileSyncContent {
        avatar_epoch,
        name,
        avatar,
        friends_of_friends_version,
        friends_of_friends_enabled,
        friends_of_friends_revision,
    })
}

#[uniffi::export]
pub fn encode_lan_endpoint_content(content: LanEndpointContent) -> Result<Vec<u8>, CoreError> {
    validate_lan_endpoint_fields(&content.instance_token, &content.host, content.port)?;
    if content.network_id.len() > 32 {
        return Err(CoreError::Malformed(
            "LAN network id exceeds 32 bytes".to_string(),
        ));
    }
    let host = content.host.as_bytes();
    let mut out = Vec::with_capacity(
        1 + LAN_INSTANCE_TOKEN_LEN + 2 + 8 + 1 + content.network_id.len() + 1 + host.len(),
    );
    out.push(LAN_ENDPOINT_CONTENT_VERSION);
    out.extend_from_slice(&content.instance_token);
    out.extend_from_slice(&content.port.to_be_bytes());
    out.extend_from_slice(&content.expires_at_ms.to_be_bytes());
    out.push(content.network_id.len() as u8);
    out.extend_from_slice(&content.network_id);
    out.push(host.len() as u8);
    out.extend_from_slice(host);
    Ok(out)
}

#[uniffi::export]
pub fn decode_lan_endpoint_content(bytes: Vec<u8>) -> Result<LanEndpointContent, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let version = cursor.take_u8()?;
    if version != LAN_ENDPOINT_CONTENT_VERSION {
        return Err(CoreError::Malformed(format!(
            "unsupported LAN endpoint content version: {version}"
        )));
    }
    let instance_token = cursor.take(LAN_INSTANCE_TOKEN_LEN)?.to_vec();
    let port = cursor.take_u16()?;
    let expires_at_ms = cursor.take_i64()?;
    let network_id_len = cursor.take_u8()? as usize;
    if network_id_len > 32 {
        return Err(CoreError::Malformed(
            "LAN network id exceeds 32 bytes".to_string(),
        ));
    }
    let network_id = cursor.take(network_id_len)?.to_vec();
    let host_len = cursor.take_u8()? as usize;
    let host = std::str::from_utf8(cursor.take(host_len)?)
        .map_err(|_| CoreError::Malformed("LAN endpoint host is not UTF-8".to_string()))?
        .to_string();
    cursor.finish()?;
    validate_lan_endpoint_fields(&instance_token, &host, port)?;
    Ok(LanEndpointContent {
        instance_token,
        network_id,
        host,
        port,
        expires_at_ms,
    })
}

/// Create a short-lived introduction ticket signed by the mutual friend.
#[uniffi::export]
pub fn create_introduction_ticket(
    introducer: Identity,
    candidate_user_id: Vec<u8>,
    invitee_user_id: Vec<u8>,
    candidate_policy_revision: u64,
    issued_at_ms: i64,
    expires_at_ms: i64,
    offer_id: Vec<u8>,
) -> Result<IntroductionTicket, CoreError> {
    validate_id(&candidate_user_id, "candidate UserID")?;
    validate_id(&invitee_user_id, "invitee UserID")?;
    validate_id(&introducer.user_id, "introducer UserID")?;
    validate_id(&offer_id, "offer ID")?;
    if expires_at_ms <= issued_at_ms
        || expires_at_ms.saturating_sub(issued_at_ms) > INTRODUCTION_MAX_LIFETIME_MS
    {
        return Err(CoreError::Malformed(
            "introduction ticket validity window must be between 1 ms and 30 days".to_string(),
        ));
    }
    let mut ticket = IntroductionTicket {
        version: 1,
        introducer_user_id: introducer.user_id.clone(),
        candidate_user_id,
        invitee_user_id,
        candidate_policy_revision,
        issued_at_ms,
        expires_at_ms,
        offer_id,
        signature: Vec::new(),
    };
    let signing_key = signing_key_from_bytes(&introducer.sign_sk)?;
    ticket.signature = signing_key
        .sign(&introduction_ticket_bytes(&ticket)?)
        .to_bytes()
        .to_vec();
    Ok(ticket)
}

/// Verify the ticket signature and all bindings needed by the candidate.
#[uniffi::export]
pub fn verify_introduction_ticket(
    ticket: IntroductionTicket,
    introducer_sign_pk: Vec<u8>,
    expected_candidate_user_id: Vec<u8>,
    expected_invitee_user_id: Vec<u8>,
    expected_candidate_policy_revision: u64,
    now_ms: i64,
) -> Result<bool, CoreError> {
    validate_ticket_shape(&ticket)?;
    if ticket.candidate_user_id != expected_candidate_user_id
        || ticket.invitee_user_id != expected_invitee_user_id
        || ticket.candidate_policy_revision != expected_candidate_policy_revision
        || derive_user_id(&introducer_sign_pk).to_vec() != ticket.introducer_user_id
        || now_ms
            < ticket
                .issued_at_ms
                .saturating_sub(INTRODUCTION_CLOCK_SKEW_MS)
        || now_ms
            > ticket
                .expires_at_ms
                .saturating_add(INTRODUCTION_CLOCK_SKEW_MS)
    {
        return Ok(false);
    }
    let verifying_key = verifying_key_from_bytes(&introducer_sign_pk)?;
    let signature_bytes: [u8; 64] = ticket
        .signature
        .as_slice()
        .try_into()
        .map_err(|_| CoreError::Malformed("invalid ticket signature length".to_string()))?;
    let signature = Signature::from_bytes(&signature_bytes);
    Ok(verifying_key
        .verify(&introduction_ticket_bytes(&ticket)?, &signature)
        .is_ok())
}

#[uniffi::export]
pub fn encode_friend_directory_content(content: FriendDirectoryContent) -> Vec<u8> {
    serde_json::to_vec(&content).expect("FriendDirectoryContent always serializes")
}

#[uniffi::export]
pub fn decode_friend_directory_content(
    bytes: Vec<u8>,
) -> Result<FriendDirectoryContent, CoreError> {
    if bytes.len() > FRIEND_DIRECTORY_MAX_BYTES {
        return Err(CoreError::Malformed(
            "friend directory is too large".to_string(),
        ));
    }
    // These bytes came off a mesh link from a peer, and both shells log this
    // message when they drop the frame, so the failure is described by shape
    // and position (`json_fault`) rather than by quoting the document.
    let content: FriendDirectoryContent = serde_json::from_slice(&bytes).map_err(|e| {
        CoreError::Malformed(format!(
            "invalid friend directory: {}",
            json_fault(&e, bytes.len())
        ))
    })?;
    validate_friend_directory(&content)?;
    Ok(content)
}

#[uniffi::export]
pub fn encode_introduced_friend_request(request: IntroducedFriendRequest) -> Vec<u8> {
    serde_json::to_vec(&request).expect("IntroducedFriendRequest always serializes")
}

#[uniffi::export]
pub fn decode_introduced_friend_request(
    bytes: Vec<u8>,
) -> Result<IntroducedFriendRequest, CoreError> {
    if bytes.len() > 16 * 1024 {
        return Err(CoreError::Malformed(
            "introduced friend request is too large".to_string(),
        ));
    }
    let request: IntroducedFriendRequest = serde_json::from_slice(&bytes).map_err(|e| {
        CoreError::Malformed(format!(
            "invalid introduced friend request: {}",
            json_fault(&e, bytes.len())
        ))
    })?;
    if request.version != INTRODUCED_FRIEND_REQUEST_VERSION {
        return Err(CoreError::Malformed(format!(
            "unknown introduced friend request version: {}",
            request.version
        )));
    }
    validate_ticket_shape(&request.ticket)?;
    crate::identity::parse_friend_card(request.friend_card_json.clone())?;
    Ok(request)
}

fn validate_friend_directory(content: &FriendDirectoryContent) -> Result<(), CoreError> {
    if content.version != FRIEND_DIRECTORY_VERSION {
        return Err(CoreError::Malformed(format!(
            "unknown friend directory version: {}",
            content.version
        )));
    }
    if content.entries.len() > FRIEND_DIRECTORY_MAX_ENTRIES {
        return Err(CoreError::Malformed(
            "too many friend directory entries".to_string(),
        ));
    }
    for entry in &content.entries {
        validate_id(&entry.candidate.user_id, "candidate UserID")?;
        if entry.candidate.name.len() > PROFILE_SYNC_MAX_NAME_BYTES {
            return Err(CoreError::Malformed(
                "candidate name is too long".to_string(),
            ));
        }
        if entry.candidate.sign_pk.len() != 32 || entry.candidate.agree_pk.len() != 32 {
            return Err(CoreError::Malformed(
                "candidate key has invalid length".to_string(),
            ));
        }
        if derive_user_id(&entry.candidate.sign_pk).to_vec() != entry.candidate.user_id {
            return Err(CoreError::Malformed(
                "candidate UserID does not match signing key".to_string(),
            ));
        }
        validate_ticket_shape(&entry.ticket)?;
        if entry.ticket.candidate_user_id != entry.candidate.user_id
            || entry.ticket.candidate_policy_revision != entry.candidate_policy_revision
        {
            return Err(CoreError::Malformed(
                "directory entry does not match introduction ticket".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_ticket_shape(ticket: &IntroductionTicket) -> Result<(), CoreError> {
    if ticket.version != 1 {
        return Err(CoreError::Malformed(format!(
            "unknown introduction ticket version: {}",
            ticket.version
        )));
    }
    validate_id(&ticket.introducer_user_id, "introducer UserID")?;
    validate_id(&ticket.candidate_user_id, "candidate UserID")?;
    validate_id(&ticket.invitee_user_id, "invitee UserID")?;
    validate_id(&ticket.offer_id, "offer ID")?;
    if ticket.signature.len() != 64 {
        return Err(CoreError::Malformed(
            "invalid ticket signature length".to_string(),
        ));
    }
    if ticket.expires_at_ms <= ticket.issued_at_ms
        || ticket.expires_at_ms.saturating_sub(ticket.issued_at_ms) > INTRODUCTION_MAX_LIFETIME_MS
    {
        return Err(CoreError::Malformed(
            "invalid ticket validity window".to_string(),
        ));
    }
    Ok(())
}

fn validate_id(bytes: &[u8], label: &str) -> Result<(), CoreError> {
    if bytes.len() != 16 {
        return Err(CoreError::Malformed(format!(
            "invalid {label} length: expected 16, got {}",
            bytes.len()
        )));
    }
    Ok(())
}

fn introduction_ticket_bytes(ticket: &IntroductionTicket) -> Result<Vec<u8>, CoreError> {
    validate_id(&ticket.introducer_user_id, "introducer UserID")?;
    validate_id(&ticket.candidate_user_id, "candidate UserID")?;
    validate_id(&ticket.invitee_user_id, "invitee UserID")?;
    validate_id(&ticket.offer_id, "offer ID")?;
    let mut out = b"CruiseMesh introduction ticket v1\0".to_vec();
    out.push(ticket.version);
    out.extend_from_slice(&ticket.introducer_user_id);
    out.extend_from_slice(&ticket.candidate_user_id);
    out.extend_from_slice(&ticket.invitee_user_id);
    out.extend_from_slice(&ticket.candidate_policy_revision.to_be_bytes());
    out.extend_from_slice(&ticket.issued_at_ms.to_be_bytes());
    out.extend_from_slice(&ticket.expires_at_ms.to_be_bytes());
    out.extend_from_slice(&ticket.offer_id);
    Ok(out)
}

/// A parsed link frame. HELLO, DIGEST, LAN endpoint hints, and probes are
/// link-control traffic; message content remains inside sealed envelopes.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum Frame {
    Hello {
        user_id: Vec<u8>,
    },
    /// Capability HELLO (frame type 0x06), sent immediately after the legacy
    /// HELLO on every link. Legacy clients reject the unknown frame type in
    /// `parse_frame` and both shells drop unparseable frames without
    /// touching the link, so this is safe to send blind. The legacy HELLO
    /// must never grow trailing fields instead: its parser swallows the
    /// whole remainder into `user_id`, so appended bytes would corrupt the
    /// peer identity on old clients.
    Hello2 {
        user_id: Vec<u8>,
        capabilities: u32,
    },
    /// **§10 step 5's own-roster notice** (frame type `0x07`): one person's own
    /// signed roster document, as [`crate::core_encode_roster`] writes it.
    ///
    /// The document is the DL-3 document and nothing else — keys, ids, counters
    /// and one signature. DL-5 keeps an endpoint out of a roster structurally
    /// (there is no field one fits in), so this frame cannot leak a discovered
    /// or third-party address however it is routed.
    ///
    /// It is plaintext on the link, so **who may be sent one is the whole of its
    /// safety**: only a peer that has already proved, cryptographically, that it
    /// holds this person's own agreement secret. See [`encode_own_roster`].
    OwnRoster {
        document: Vec<u8>,
    },
    Envelope {
        msg_id: Vec<u8>,
        hop_ttl: u8,
        expiry: i64,
        recipient_hint: Vec<u8>,
        sealed: Vec<u8>,
    },
    Digest {
        chat_id: Vec<u8>,
        entries: Vec<DigestEntry>,
        recent_msg_ids: Vec<Vec<u8>>,
    },
    LanEndpoint {
        instance_token: Vec<u8>,
        host: String,
        port: u16,
    },
    TransportProbe {
        nonce: u64,
        response: bool,
    },
}

/// Encode a HELLO frame: frame-type byte `0x01` followed by `user_id`
/// verbatim (whatever length the caller's UserID scheme uses; see
/// [`crate::generate_identity`] -- this module doesn't hardcode a UserID
/// length, since the frame boundary is already delimited by the BLE link
/// layer's own framing, DESIGN.md §5.2).
#[uniffi::export]
pub fn encode_hello(user_id: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + user_id.len());
    out.push(FRAME_TYPE_HELLO);
    out.extend_from_slice(&user_id);
    out
}

/// Encode a HELLO2 frame: `0x06 ‖ user_id[16] ‖ capabilities:u32-LE`. Send
/// right after the legacy HELLO (see [`Frame::Hello2`] for why the legacy
/// frame can't carry this). Parsers ignore trailing bytes past the
/// capabilities word — that is HELLO2's designated forward-extension point.
#[uniffi::export]
pub fn encode_hello2(user_id: Vec<u8>, capabilities: u32) -> Result<Vec<u8>, CoreError> {
    if user_id.len() != HELLO2_USER_ID_LEN {
        return Err(CoreError::Malformed(format!(
            "HELLO2 user_id must be {HELLO2_USER_ID_LEN} bytes"
        )));
    }
    let mut out = Vec::with_capacity(1 + HELLO2_USER_ID_LEN + 4);
    out.push(FRAME_TYPE_HELLO2);
    out.extend_from_slice(&user_id);
    out.extend_from_slice(&capabilities.to_le_bytes());
    Ok(out)
}

/// Encode an own-roster notice: `0x07 ‖ roster document`
/// (`specs/multi-device-v1.md` §10 step 5).
///
/// # The one rule that makes this frame safe
///
/// **Send it only on a link whose remote party has already proved it holds this
/// person's own agreement secret** — in practice a LAN Noise session whose
/// remote static key is this identity's `agree_pk` — and refuse it on arrival
/// under the same test. Both halves, or neither.
///
/// The reason is that the body is plaintext and the roster is a private fact:
/// how many devices this person has and what their keys are. Anyone who passes
/// the test already holds the person key and therefore already holds everything
/// the document says; anyone who does not must never be able to elicit it by
/// claiming this person's `user_id` in a HELLO. BLE HELLO is cleartext and
/// cannot prove anything, so a BLE meeting never carries this frame — see the
/// note in §10 of the spec, which records that limitation rather than weakening
/// the test to cover it.
///
/// Gated by [`CAP_OWN_ROSTER_NOTICE`] on the peer's HELLO2, so a build that
/// predates the frame is never sent one.
#[uniffi::export]
pub fn encode_own_roster(document: Vec<u8>) -> Result<Vec<u8>, CoreError> {
    if document.is_empty() {
        return Err(CoreError::Malformed(
            "own roster notice carries no document".to_string(),
        ));
    }
    if document.len() + 1 > MAX_P2P_FRAME_BYTES {
        return Err(CoreError::Malformed(format!(
            "own roster notice exceeds {MAX_P2P_FRAME_BYTES}-byte limit"
        )));
    }
    let mut out = Vec::with_capacity(1 + document.len());
    out.push(FRAME_TYPE_OWN_ROSTER);
    out.extend_from_slice(&document);
    Ok(out)
}

/// Encode a sealed-envelope frame: frame-type byte `0x02`, then the §6.4
/// public header (`msg_id`, `hop_ttl`, `expiry`, `recipient_hint` -- see
/// module docs for exact byte layout), then the sealed bytes verbatim (the
/// output of [`crate::seal_message`]). Use [`generate_msg_id`],
/// [`DEFAULT_HOP_TTL`], [`default_expiry`], and [`compute_recipient_hint`] to
/// produce the header fields for a freshly authored envelope.
#[uniffi::export]
pub fn encode_envelope_frame(
    msg_id: Vec<u8>,
    hop_ttl: u8,
    expiry: i64,
    recipient_hint: Vec<u8>,
    sealed: Vec<u8>,
) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(1 + msg_id.len() + 1 + 8 + recipient_hint.len() + sealed.len());
    out.push(FRAME_TYPE_ENVELOPE);
    out.extend_from_slice(&msg_id);
    out.push(hop_ttl);
    out.extend_from_slice(&expiry.to_be_bytes());
    out.extend_from_slice(&recipient_hint);
    out.extend_from_slice(&sealed);
    out
}

/// Generate a fresh, random 16-byte `msg_id` for an envelope's §6.4 header
/// (the seen-ID dedupe key future gossip, §5.3, will use).
#[uniffi::export]
pub fn generate_msg_id() -> Vec<u8> {
    let mut id = vec![0u8; MSG_ID_LEN];
    OsRng.fill_bytes(&mut id);
    id
}

/// `recipient_hint` for an envelope's §6.4 header: `BLAKE2b-8(recipient
/// UserID || day number)`, where the day number is `timestamp_ms` divided
/// into whole days since the Unix epoch. Deterministic given the same
/// `(recipient_user_id, timestamp_ms)` pair, so both the sender (authoring
/// the envelope "today") and the true recipient (recomputing this with their
/// own UserID and current time) land on the same hint without coordination
/// -- while an observer who doesn't hold `recipient_user_id` gets no
/// stable, long-lived identifier to track, since the hint rotates daily.
#[uniffi::export]
pub fn compute_recipient_hint(recipient_user_id: Vec<u8>, timestamp_ms: i64) -> Vec<u8> {
    let day_number = timestamp_ms.div_euclid(MS_PER_DAY);
    let mut hasher = Blake2bVar::new(RECIPIENT_HINT_LEN).expect("valid blake2b output length");
    hasher.update(&recipient_user_id);
    hasher.update(&day_number.to_be_bytes());
    let mut out = vec![0u8; RECIPIENT_HINT_LEN];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    out
}

/// Deterministic per-member relay-post id for group fan-out
/// (`specs/group-relay-durability.md` §4.1, DTN_TODOS.md N1):
/// `BLAKE2b-16(prologue || original_msg_id || member_user_id)`, where
/// `prologue` is the fixed ASCII string `"cruisemesh group fanout v1"`. Two
/// properties matter here, mirroring why [`compute_recipient_hint`] is
/// keyed the way it is:
///
/// - **Distinct per member**: hashing in `member_user_id` gives every
///   member of the group their own relay row id for the same logical
///   message, so the shared-mailbox dedupe key `(family_token, msg_id)`
///   naturally becomes one row per member instead of one row for everyone.
/// - **Deterministic across calls**: the same `(original_msg_id,
///   member_user_id)` pair always yields the same id, so re-uploading the
///   same group message (the author retrying, or a different member's
///   phone muling it) re-derives the identical N ids and the relay's
///   existing `ON CONFLICT` dedupe on `msg_id` absorbs the retry with no
///   server-side change.
///
/// The versioned prologue is a domain separator: it keeps this id space
/// disjoint from [`generate_msg_id`]'s random 16-byte ids and from any
/// future derived-id scheme that might hash different fields together.
#[uniffi::export]
pub fn fanout_msg_id(original_msg_id: Vec<u8>, member_user_id: Vec<u8>) -> Vec<u8> {
    const FANOUT_PROLOGUE: &[u8] = b"cruisemesh group fanout v1";
    let mut hasher = Blake2bVar::new(MSG_ID_LEN).expect("valid blake2b output length");
    hasher.update(FANOUT_PROLOGUE);
    hasher.update(&original_msg_id);
    hasher.update(&member_user_id);
    let mut out = vec![0u8; MSG_ID_LEN];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    out
}

/// Deterministic per-device relay-post id for multi-device fan-out
/// (`specs/multi-device-v1.md` §7): `BLAKE2b-16(prologue || original_msg_id ||
/// device_id)`, where `prologue` is the fixed ASCII string
/// `"cruisemesh device fanout v1"`.
///
/// Same construction discipline as [`fanout_msg_id`], and for the same two
/// reasons one level down:
///
/// - **Distinct per device**: every recipient device gets its own relay row id
///   for the same logical message, so each row has exactly one true consumer
///   and ACK-MD-1 ("a device acks only rows in its own
///   `device_fanout_msg_id` namespace, and only on CONSUMED") has something
///   concrete to name.
/// - **Deterministic across calls**: a retried upload, or a sibling muling the
///   same envelope, re-derives the identical ids and the relay's existing
///   `ON CONFLICT` dedupe on `msg_id` absorbs the repeat with no server change.
///
/// The prologue differs from the group one deliberately and is not decoration:
/// a `device_id` and a `member_user_id` are both 16 bytes drawn from the same
/// derivation, so the domain separator is the *only* thing keeping the two id
/// spaces disjoint.
///
/// [`LEGACY_DEVICE_ID`](crate::LEGACY_DEVICE_ID) — and any absent or malformed
/// device id, which §5 maps to it — returns `original_msg_id` unchanged: the
/// single person-addressed row a v1 sender uploads today, byte for byte. That
/// fallback is paired with the one in
/// [`core_device_namespace_id`](crate::core_device_namespace_id), so the id a
/// legacy row is keyed by and the hint it is found under fall back together
/// and a mixed fleet can never mint half a legacy row. Refusing to *ack* such
/// a row is ACK-MD-2's job, not this function's.
#[uniffi::export]
pub fn device_fanout_msg_id(original_msg_id: Vec<u8>, device_id: Vec<u8>) -> Vec<u8> {
    const DEVICE_FANOUT_PROLOGUE: &[u8] = b"cruisemesh device fanout v1";
    if device_id.len() != crate::DEVICE_ID_LEN || device_id[..] == crate::LEGACY_DEVICE_ID[..] {
        return original_msg_id;
    }
    let mut hasher = Blake2bVar::new(MSG_ID_LEN).expect("valid blake2b output length");
    hasher.update(DEVICE_FANOUT_PROLOGUE);
    hasher.update(&original_msg_id);
    hasher.update(&device_id);
    let mut out = vec![0u8; MSG_ID_LEN];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    out
}

/// `expiry` for a freshly authored envelope's §6.4 header:
/// `timestamp_ms + DEFAULT_EXPIRY_MS` (7 days, DESIGN.md §5.3), saturating
/// rather than overflowing for pathological inputs.
#[uniffi::export]
pub fn default_expiry(timestamp_ms: i64) -> i64 {
    timestamp_ms.saturating_add(DEFAULT_EXPIRY_MS)
}

/// Encode a DIGEST frame for one chat (see module docs for layout and for
/// the one-chat-per-frame convention): frame-type byte `0x03`, then
/// `chat_id` (16-bit length prefix), then `entries` as a 16-bit count
/// followed by each entry's `sender_user_id` (16-bit length prefix) and
/// `through_lamport` (u64 BE), then `recent_msg_ids` as a 16-bit count plus
/// each fixed-width 16-byte `msg_id`. `entries` is typically the output of
/// the store's `chat_digest`; an empty list is valid ("send me everything").
#[uniffi::export]
pub fn encode_digest(
    chat_id: Vec<u8>,
    entries: Vec<DigestEntry>,
    recent_msg_ids: Vec<Vec<u8>>,
) -> Result<Vec<u8>, CoreError> {
    if chat_id.len() > u16::MAX as usize {
        return Err(CoreError::Malformed("digest chat_id is too long".into()));
    }
    if entries.len() > u16::MAX as usize {
        return Err(CoreError::Malformed(
            "digest contains too many entries".into(),
        ));
    }
    if recent_msg_ids.len() > u16::MAX as usize {
        return Err(CoreError::Malformed(
            "digest contains too many recent message ids".into(),
        ));
    }
    for entry in &entries {
        if entry.sender_user_id.len() > u16::MAX as usize {
            return Err(CoreError::Malformed(
                "digest sender_user_id is too long".into(),
            ));
        }
    }
    for msg_id in &recent_msg_ids {
        if msg_id.len() != MSG_ID_LEN {
            return Err(CoreError::Malformed(format!(
                "digest msg_id must be exactly {MSG_ID_LEN} bytes"
            )));
        }
    }

    let capacity = 1usize
        .checked_add(2)
        .and_then(|value| value.checked_add(chat_id.len()))
        .and_then(|value| value.checked_add(2))
        .and_then(|value| {
            entries.iter().try_fold(value, |total, entry| {
                total
                    .checked_add(2)
                    .and_then(|next| next.checked_add(entry.sender_user_id.len()))
                    .and_then(|next| next.checked_add(8))
            })
        })
        .and_then(|value| value.checked_add(2))
        .and_then(|value| value.checked_add(recent_msg_ids.len().checked_mul(MSG_ID_LEN)?))
        .ok_or_else(|| CoreError::Malformed("digest frame is too large".into()))?;
    let mut out = Vec::with_capacity(capacity);
    out.push(FRAME_TYPE_DIGEST);
    write_bytes16(&mut out, &chat_id);
    out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for entry in &entries {
        write_bytes16(&mut out, &entry.sender_user_id);
        out.extend_from_slice(&entry.through_lamport.to_be_bytes());
    }
    out.extend_from_slice(&(recent_msg_ids.len() as u16).to_be_bytes());
    for msg_id in &recent_msg_ids {
        out.extend_from_slice(msg_id);
    }
    Ok(out)
}

/// Encode a LAN endpoint introduction. The opaque 8-byte instance token is
/// the same connection-election value advertised through DNS-SD. The host is
/// the sender's own address on the local network, and only that: an address
/// literal in a local range (see [`is_local_lan_host`]), never a name. A
/// receiver must never trust the hint by itself either: the resulting TCP
/// connection still has to authenticate the expected accepted contact through
/// Noise.
#[uniffi::export]
pub fn encode_lan_endpoint(
    instance_token: Vec<u8>,
    host: String,
    port: u16,
) -> Result<Vec<u8>, CoreError> {
    validate_lan_endpoint_fields(&instance_token, &host, port)?;
    let host_bytes = host.as_bytes();
    let mut out = Vec::with_capacity(1 + 1 + LAN_INSTANCE_TOKEN_LEN + 2 + 1 + host_bytes.len());
    out.push(FRAME_TYPE_LAN_ENDPOINT);
    out.push(LAN_ENDPOINT_VERSION);
    out.extend_from_slice(&instance_token);
    out.extend_from_slice(&port.to_be_bytes());
    out.push(host_bytes.len() as u8);
    out.extend_from_slice(host_bytes);
    Ok(out)
}

fn validate_lan_endpoint_fields(
    instance_token: &[u8],
    host: &str,
    port: u16,
) -> Result<(), CoreError> {
    if instance_token.len() != LAN_INSTANCE_TOKEN_LEN {
        return Err(CoreError::Malformed(format!(
            "LAN instance token must be {LAN_INSTANCE_TOKEN_LEN} bytes"
        )));
    }
    if port == 0 {
        return Err(CoreError::Malformed(
            "LAN endpoint port must be non-zero".to_string(),
        ));
    }
    let host_bytes = host.as_bytes();
    if host_bytes.is_empty()
        || host_bytes.len() > MAX_LAN_HOST_BYTES
        || host.chars().any(char::is_whitespace)
    {
        return Err(CoreError::Malformed(
            "LAN endpoint host is empty, too long, or contains whitespace".to_string(),
        ));
    }
    if !is_local_lan_host(host) {
        return Err(CoreError::Malformed(
            "LAN endpoint host must be a local network address".to_string(),
        ));
    }
    Ok(())
}

/// Whether `host` is something a phone can legitimately advertise as *its own*
/// address on the local network.
///
/// A LAN endpoint hint only ever carries the sender's own LAN address, so the
/// receiver holds it to exactly that: an address literal in a range that a
/// phone's own interface address lands in. Two consequences matter:
///
/// - **No names.** A hostname would make the receiving phone resolve a string
///   chosen by someone else before it dials, and DNS resolution is not
///   something an endpoint hint needs. (The Advanced "connect manually"
///   field, [`crate::core_parse_lan_endpoint`], is a separate, user-typed
///   path and still accepts names.)
/// - **No public addresses.** Nothing off the local network can be this
///   phone's own LAN address, so a hint may not point at one.
///
/// Old senders are unaffected: the addresses both shells actually advertise
/// (the interface address of the joined Wi-Fi network) already pass.
///
/// [`crate::lan_endpoint_host_is_local`] exports this rule to the apps so the
/// endpoint cache can apply it to entries written before it existed. This
/// function is the authority; nothing else should restate it.
pub(crate) fn is_local_lan_host(host: &str) -> bool {
    // Android hands back Inet6Address.getHostAddress(), which appends the
    // scope id of a link-local address ("fe80::1%wlan0", or "%3"). Split it
    // off before parsing and accept it only where it is meaningful.
    let (literal, zone) = match host.split_once('%') {
        Some((literal, zone)) => (literal, Some(zone)),
        None => (host, None),
    };
    if let Some(zone) = zone {
        let plausible_zone = !zone.is_empty()
            && zone.len() <= MAX_LAN_HOST_ZONE_BYTES
            && zone
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if !plausible_zone {
            return false;
        }
    }
    match literal.parse::<IpAddr>() {
        Ok(IpAddr::V4(addr)) => zone.is_none() && is_local_ipv4(addr),
        // A scope id belongs to a link-local address; anywhere else it is
        // noise this phone never emits.
        Ok(IpAddr::V6(addr)) => (zone.is_none() || is_ipv6_link_local(addr)) && is_local_ipv6(addr),
        Err(_) => false,
    }
}

/// [`is_local_lan_host`], minus the addresses that are local but that nobody
/// else can dial: IPv6 link-local.
///
/// An `fe80::/10` address only resolves against the *dialer's* scope id, and
/// the scope a phone reads off its own interface means nothing to the phone it
/// hands the address to. Still accepted as an incoming hint -- shipped builds
/// emit one and refusing it would drop an otherwise good frame -- but refused
/// as something this phone chooses to publish about itself. See
/// [`crate::core_lan_host_is_reachable_endpoint`].
pub(crate) fn is_reachable_lan_host(host: &str) -> bool {
    if !is_local_lan_host(host) {
        return false;
    }
    let literal = host.split_once('%').map_or(host, |(literal, _)| literal);
    match literal.parse::<IpAddr>() {
        Ok(IpAddr::V6(addr)) => !is_ipv6_link_local(addr),
        Ok(IpAddr::V4(_)) => true,
        Err(_) => false,
    }
}

fn is_local_ipv4(addr: Ipv4Addr) -> bool {
    let octets = addr.octets();
    // 10/8, 172.16/12, 192.168/16.
    addr.is_private()
        // 169.254/16: self-assigned when DHCP is absent, still same-link.
        || addr.is_link_local()
        // 100.64/10 (RFC 6598): what shared Wi-Fi -- hotels, ships, campus
        // networks, phone tethering -- hands its clients when it has run out
        // of RFC1918 space. A phone's own address genuinely lands here, and
        // the subnet sweep already treats such a network as local.
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
}

fn is_local_ipv6(addr: Ipv6Addr) -> bool {
    // fe80::/10 link-local, or fc00::/7 unique local.
    is_ipv6_link_local(addr) || (addr.segments()[0] & 0xfe00) == 0xfc00
}

fn is_ipv6_link_local(addr: Ipv6Addr) -> bool {
    (addr.segments()[0] & 0xffc0) == 0xfe80
}

/// Encode an encrypted-link health probe. Callers choose a unique nonce and
/// echo it back with `response = true`; no timestamps or identities cross the
/// wire.
#[uniffi::export]
pub fn encode_transport_probe(nonce: u64, response: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(11);
    out.push(FRAME_TYPE_TRANSPORT_PROBE);
    out.push(TRANSPORT_PROBE_VERSION);
    out.push(u8::from(response));
    out.extend_from_slice(&nonce.to_be_bytes());
    out
}

/// Parse a frame-type byte + body into a [`Frame`]. Rejects empty input, an
/// unrecognized frame-type byte, a HELLO/envelope frame with no body, and a
/// truncated or trailing-garbage DIGEST body.
#[uniffi::export]
pub fn parse_frame(bytes: Vec<u8>) -> Result<Frame, CoreError> {
    if bytes.len() > MAX_P2P_FRAME_BYTES {
        return Err(CoreError::Malformed(format!(
            "frame exceeds {MAX_P2P_FRAME_BYTES}-byte limit"
        )));
    }
    let (frame_type, rest) = bytes
        .split_first()
        .ok_or_else(|| CoreError::Malformed("empty frame: missing frame-type byte".to_string()))?;
    match *frame_type {
        FRAME_TYPE_HELLO => {
            if rest.is_empty() {
                return Err(CoreError::Malformed(
                    "HELLO frame missing user_id".to_string(),
                ));
            }
            Ok(Frame::Hello {
                user_id: rest.to_vec(),
            })
        }
        FRAME_TYPE_HELLO2 => {
            if rest.len() < HELLO2_USER_ID_LEN + 4 {
                return Err(CoreError::Malformed(
                    "HELLO2 frame too short for user_id + capabilities".to_string(),
                ));
            }
            let user_id = rest[..HELLO2_USER_ID_LEN].to_vec();
            let caps_bytes: [u8; 4] = rest[HELLO2_USER_ID_LEN..HELLO2_USER_ID_LEN + 4]
                .try_into()
                .expect("length checked above");
            // Trailing bytes are deliberately tolerated: future builds append
            // fields here, and this parser must keep working against them.
            Ok(Frame::Hello2 {
                user_id,
                capabilities: u32::from_le_bytes(caps_bytes),
            })
        }
        FRAME_TYPE_OWN_ROSTER => {
            if rest.is_empty() {
                return Err(CoreError::Malformed(
                    "own roster notice missing document".to_string(),
                ));
            }
            // Deliberately not decoded here. `parse_frame` is the shape layer;
            // whether these bytes are an acceptable roster is
            // `MessageStore::apply_own_roster_notice`'s decision, and it is the
            // one that also knows whose roster this device holds.
            Ok(Frame::OwnRoster {
                document: rest.to_vec(),
            })
        }
        FRAME_TYPE_ENVELOPE => {
            let mut cursor = Cursor::new(rest);
            let msg_id = cursor.take(MSG_ID_LEN)?.to_vec();
            let hop_ttl = cursor.take_u8()?;
            let expiry = cursor.take_i64()?;
            let recipient_hint = cursor.take(RECIPIENT_HINT_LEN)?.to_vec();
            let sealed = cursor.take_remaining();
            if sealed.is_empty() {
                return Err(CoreError::Malformed(
                    "envelope frame missing sealed payload".to_string(),
                ));
            }
            if sealed.len() > MAX_ENVELOPE_SEALED_BYTES {
                return Err(CoreError::Malformed(format!(
                    "sealed envelope exceeds {MAX_ENVELOPE_SEALED_BYTES}-byte limit"
                )));
            }
            Ok(Frame::Envelope {
                msg_id,
                hop_ttl,
                expiry,
                recipient_hint,
                sealed: sealed.to_vec(),
            })
        }
        FRAME_TYPE_DIGEST => {
            let mut cursor = Cursor::new(rest);
            let chat_id = cursor.take_bytes16()?;
            let entry_count = cursor.take_u16()? as usize;
            let mut entries = Vec::with_capacity(entry_count.min(rest.len()));
            for _ in 0..entry_count {
                let sender_user_id = cursor.take_bytes16()?;
                let through_lamport = cursor.take_u64()?;
                entries.push(DigestEntry {
                    sender_user_id,
                    through_lamport,
                });
            }
            let recent_msg_id_count = cursor.take_u16()? as usize;
            let mut recent_msg_ids =
                Vec::with_capacity(recent_msg_id_count.min(rest.len() / MSG_ID_LEN));
            for _ in 0..recent_msg_id_count {
                recent_msg_ids.push(cursor.take(MSG_ID_LEN)?.to_vec());
            }
            cursor.finish()?;
            Ok(Frame::Digest {
                chat_id,
                entries,
                recent_msg_ids,
            })
        }
        FRAME_TYPE_LAN_ENDPOINT => {
            let mut cursor = Cursor::new(rest);
            let version = cursor.take_u8()?;
            if version != LAN_ENDPOINT_VERSION {
                return Err(CoreError::Malformed(format!(
                    "unsupported LAN endpoint version: {version}"
                )));
            }
            let instance_token = cursor.take(LAN_INSTANCE_TOKEN_LEN)?.to_vec();
            let port = cursor.take_u16()?;
            let host_len = cursor.take_u8()? as usize;
            let host_bytes = cursor.take(host_len)?;
            cursor.finish()?;
            let host = std::str::from_utf8(host_bytes)
                .map_err(|_| CoreError::Malformed("LAN endpoint host is not UTF-8".to_string()))?
                .to_string();
            // Same rules as the sealed hint: a non-zero port and a local
            // address literal, checked before any caller sees the frame.
            validate_lan_endpoint_fields(&instance_token, &host, port)?;
            Ok(Frame::LanEndpoint {
                instance_token,
                host,
                port,
            })
        }
        FRAME_TYPE_TRANSPORT_PROBE => {
            let mut cursor = Cursor::new(rest);
            let version = cursor.take_u8()?;
            if version != TRANSPORT_PROBE_VERSION {
                return Err(CoreError::Malformed(format!(
                    "unsupported transport probe version: {version}"
                )));
            }
            let response = match cursor.take_u8()? {
                0 => false,
                1 => true,
                other => {
                    return Err(CoreError::Malformed(format!(
                        "invalid transport probe response flag: {other}"
                    )))
                }
            };
            let nonce = cursor.take_u64()?;
            cursor.finish()?;
            Ok(Frame::TransportProbe { nonce, response })
        }
        other => Err(CoreError::Malformed(format!(
            "unknown frame type byte: 0x{other:02x}"
        ))),
    }
}

// --- shared encode/decode helpers ------------------------------------------

fn write_bytes16(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn write_bytes32(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// A tiny bounds-checked cursor over a byte slice, so every decode path
/// above reports a [`CoreError::Malformed`] instead of panicking on
/// attacker-controlled/truncated input.
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

    fn take_u8(&mut self) -> Result<u8, CoreError> {
        Ok(self.take(1)?[0])
    }

    fn take_u16(&mut self) -> Result<u16, CoreError> {
        Ok(u16::from_be_bytes(
            self.take(2)?.try_into().expect("exactly 2 bytes"),
        ))
    }

    fn take_u32(&mut self) -> Result<u32, CoreError> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().expect("exactly 4 bytes"),
        ))
    }

    fn take_u64(&mut self) -> Result<u64, CoreError> {
        Ok(u64::from_be_bytes(
            self.take(8)?.try_into().expect("exactly 8 bytes"),
        ))
    }

    fn take_i64(&mut self) -> Result<i64, CoreError> {
        Ok(i64::from_be_bytes(
            self.take(8)?.try_into().expect("exactly 8 bytes"),
        ))
    }

    fn take_bytes16(&mut self) -> Result<Vec<u8>, CoreError> {
        let len = self.take_u16()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn take_bytes32(&mut self) -> Result<Vec<u8>, CoreError> {
        let len = self.take_u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn is_finished(&self) -> bool {
        self.pos == self.data.len()
    }

    /// Consumes and returns every remaining byte (no length prefix -- used
    /// where, like the envelope frame's `sealed` tail, the field's length is
    /// implicitly "whatever's left").
    fn take_remaining(&mut self) -> &'a [u8] {
        let rest = &self.data[self.pos..];
        self.pos = self.data.len();
        rest
    }

    /// Consumes the cursor, erroring if any bytes remain unread.
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
    use super::*;
    use crate::identity::generate_identity;
    use crate::{open_message, seal_message};

    fn sample_body() -> MessageBody {
        MessageBody {
            kind: KIND_TEXT,
            chat_id: b"chat-1".to_vec(),
            lamport: 42,
            timestamp: 1_700_000_000_123,
            content: b"meet at the buffet at 6".to_vec(),
        }
    }

    #[test]
    fn message_body_round_trips() {
        let body = sample_body();
        let encoded = encode_message_body(body.clone()).unwrap();
        let decoded = decode_message_body(encoded).expect("decodes");
        assert_eq!(decoded, body);
    }

    #[test]
    fn message_body_round_trips_with_empty_fields() {
        let body = MessageBody {
            kind: KIND_TEXT,
            chat_id: Vec::new(),
            lamport: 0,
            timestamp: 0,
            content: Vec::new(),
        };
        let encoded = encode_message_body(body.clone()).unwrap();
        let decoded = decode_message_body(encoded).expect("decodes");
        assert_eq!(decoded, body);
    }

    #[test]
    fn message_body_round_trips_with_negative_timestamp() {
        // i64 timestamps aren't clamped to non-negative -- pre-epoch values
        // decode fine even if the app never produces them.
        let mut body = sample_body();
        body.timestamp = -1;
        let encoded = encode_message_body(body.clone()).unwrap();
        let decoded = decode_message_body(encoded).expect("decodes");
        assert_eq!(decoded, body);
    }

    #[test]
    fn reply_extension_round_trips_without_changing_legacy_body() {
        let body = sample_body();
        let reply_to = vec![7; MSG_ID_LEN];
        let encoded = encode_message_body_with_reply(body.clone(), reply_to.clone()).unwrap();

        let extended = decode_extended_message_body(encoded.clone()).unwrap();
        assert_eq!(extended.kind, body.kind);
        assert_eq!(extended.chat_id, body.chat_id);
        assert_eq!(extended.lamport, body.lamport);
        assert_eq!(extended.timestamp, body.timestamp);
        assert_eq!(extended.content, body.content);
        assert_eq!(extended.reply_to_msg_id, Some(reply_to));

        // Compatibility bridge: code that only understands the original
        // fields can still render the reply's text and ignores the quote.
        assert_eq!(decode_message_body(encoded).unwrap(), body);
    }

    #[test]
    fn unknown_message_extensions_are_skipped() {
        let body = sample_body();
        let mut encoded = encode_message_body(body.clone()).unwrap();
        encoded.push(99);
        encoded.extend_from_slice(&3u16.to_be_bytes());
        encoded.extend_from_slice(b"new");

        let extended = decode_extended_message_body(encoded).unwrap();
        assert_eq!(extended.reply_to_msg_id, None);
        assert_eq!(extended.content, body.content);
    }

    /// WPT: the open path must skip unknown trailing sealed-body TLVs rather
    /// than reject the envelope. §5 of multi-device-v1 depends on every
    /// fielded build behaving this way.
    ///
    /// WP1 claimed types 0x20/0x21 — the ids WPT's version of this test used
    /// as its unknown probes — for `sender_device_id` and the roster head, so
    /// the probes moved up to types that really are unknown and the now-known
    /// fields joined the same sealed round trip. The coverage grows: unknown
    /// TLVs are still skipped, AND the multi-device fields survive intact
    /// beside them, in an order no encoder here emits.
    #[test]
    fn unknown_sealed_body_fields_survive_seal_and_open() {
        let sender = generate_identity();
        let recipient = generate_identity();
        let body = sample_body();
        let reply_to = vec![7; MSG_ID_LEN];
        let device_id = vec![0x33; DEVICE_ID_LEN];
        let roster_head = vec![0x44; ROSTER_HEAD_HASH_LEN];
        let mut payload = encode_message_body_extended(
            body.clone(),
            Some(reply_to.clone()),
            Some(device_id.clone()),
            Some(roster_head.clone()),
        )
        .unwrap();
        // Well-formed, genuinely unassigned types, one with a payload and one
        // empty. Both must be consumed and discarded.
        payload.push(0x40);
        payload.extend_from_slice(&8u16.to_be_bytes());
        payload.extend_from_slice(&[0x11; 8]);
        payload.push(0x41);
        payload.extend_from_slice(&0u16.to_be_bytes());

        let sealed = seal_message(sender, recipient.agree_pk.clone(), payload).expect("seals");
        let opened = open_message(recipient, sealed).expect("opens");
        let decoded = decode_extended_message_body(opened.payload.clone()).expect("decodes");
        assert_eq!(decoded.content, body.content);
        assert_eq!(decoded.reply_to_msg_id, Some(reply_to));
        assert_eq!(decoded.sender_device_id, Some(device_id));
        assert_eq!(decoded.sender_roster_head, Some(roster_head));
        assert_eq!(decode_message_body(opened.payload).unwrap(), body);
    }

    /// §5: a body with no device TLV is not a body with missing data — it is
    /// every legacy sender, forever, and it resolves to the reserved all-zero
    /// stream.
    #[test]
    fn absent_device_field_maps_to_the_legacy_stream() {
        let encoded = encode_message_body(sample_body()).unwrap();
        let decoded = decode_extended_message_body(encoded).unwrap();
        assert_eq!(decoded.sender_device_id, None);
        assert_eq!(decoded.sender_roster_head, None);
        assert_eq!(
            crate::core_device_stream_id(decoded.sender_device_id),
            crate::LEGACY_DEVICE_ID.to_vec()
        );
    }

    /// The multi-device TLVs are fixed-width, single-shot fields: a wrong
    /// width or a repeat is malformed, exactly as the reply-to extension is.
    #[test]
    fn device_extensions_reject_wrong_widths_and_repeats() {
        assert!(matches!(
            encode_message_body_extended(
                sample_body(),
                None,
                Some(vec![1; DEVICE_ID_LEN - 1]),
                None
            )
            .unwrap_err(),
            CoreError::Malformed(_)
        ));
        assert!(matches!(
            encode_message_body_extended(
                sample_body(),
                None,
                None,
                Some(vec![1; ROSTER_HEAD_HASH_LEN + 1])
            )
            .unwrap_err(),
            CoreError::Malformed(_)
        ));

        let mut short = encode_message_body(sample_body()).unwrap();
        short.push(MESSAGE_EXTENSION_SENDER_DEVICE_ID);
        short.extend_from_slice(&4u16.to_be_bytes());
        short.extend_from_slice(&[9; 4]);
        assert!(matches!(
            decode_extended_message_body(short).unwrap_err(),
            CoreError::Malformed(_)
        ));

        let mut duplicated =
            encode_message_body_extended(sample_body(), None, Some(vec![1; DEVICE_ID_LEN]), None)
                .unwrap();
        duplicated.push(MESSAGE_EXTENSION_SENDER_DEVICE_ID);
        duplicated.extend_from_slice(&(DEVICE_ID_LEN as u16).to_be_bytes());
        duplicated.extend_from_slice(&[2; DEVICE_ID_LEN]);
        assert!(matches!(
            decode_extended_message_body(duplicated).unwrap_err(),
            CoreError::Malformed(_)
        ));
    }

    /// Golden vector: the exact bytes a body with both multi-device TLVs
    /// encodes to. Fixed inputs, a literal expectation — a field reorder, a
    /// changed type id, or a different length prefix fails here rather than in
    /// the field a release later.
    #[test]
    fn multi_device_extension_golden_vector() {
        let body = MessageBody {
            kind: KIND_TEXT,
            chat_id: vec![0xAA; 4],
            lamport: 1,
            timestamp: 2,
            content: b"hi".to_vec(),
        };
        let encoded = encode_message_body_extended(
            body,
            None,
            Some(vec![0x33; DEVICE_ID_LEN]),
            Some(vec![0x44; ROSTER_HEAD_HASH_LEN]),
        )
        .unwrap();
        let hex: String = encoded.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            concat!(
                // kind, chat_id (u16 len + bytes), lamport, timestamp,
                // content (u32 len + bytes) -- the unchanged legacy prefix.
                "01",
                "0004aaaaaaaa",
                "0000000000000001",
                "0000000000000002",
                "000000026869",
                // 0x20 | len 16 | device id
                "200010",
                "33333333333333333333333333333333",
                // 0x21 | len 32 | roster head
                "210020",
                "4444444444444444444444444444444444444444444444444444444444444444",
            )
        );
    }

    #[test]
    fn reply_extension_requires_one_msg_id() {
        let error =
            encode_message_body_with_reply(sample_body(), vec![1; MSG_ID_LEN - 1]).unwrap_err();
        assert!(matches!(error, CoreError::Malformed(_)));

        let mut duplicated =
            encode_message_body_with_reply(sample_body(), vec![1; MSG_ID_LEN]).unwrap();
        duplicated.push(MESSAGE_EXTENSION_REPLY_TO_MSG_ID);
        duplicated.extend_from_slice(&(MSG_ID_LEN as u16).to_be_bytes());
        duplicated.extend_from_slice(&[2; MSG_ID_LEN]);
        let error = decode_extended_message_body(duplicated).unwrap_err();
        assert!(matches!(error, CoreError::Malformed(_)));
    }

    #[test]
    fn message_body_decode_rejects_empty_input() {
        let err = decode_message_body(Vec::new()).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn message_body_decode_rejects_truncated_chat_id() {
        // kind byte + chat_id_len claiming 10 bytes, but none follow.
        let bytes = vec![KIND_TEXT, 0x00, 0x0A];
        let err = decode_message_body(bytes).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn message_body_decode_rejects_truncated_before_timestamp() {
        let mut bytes = vec![KIND_TEXT, 0x00, 0x00]; // empty chat_id
        bytes.extend_from_slice(&1u64.to_be_bytes()); // full lamport
        bytes.push(0); // only 1 of 8 timestamp bytes
        let err = decode_message_body(bytes).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn message_body_decode_rejects_trailing_garbage() {
        let mut encoded = encode_message_body(sample_body()).unwrap();
        encoded.push(0xFF);
        let err = decode_message_body(encoded).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn message_body_rejects_unrepresentable_lamports_and_lengths() {
        let mut body = sample_body();
        body.lamport = i64::MAX as u64 + 1;
        assert!(encode_message_body(body).is_err());

        let mut body = sample_body();
        body.chat_id = vec![0; u16::MAX as usize + 1];
        assert!(encode_message_body(body).is_err());

        let mut encoded = encode_message_body(sample_body()).unwrap();
        let lamport_offset = 3 + sample_body().chat_id.len();
        encoded[lamport_offset..lamport_offset + 8].copy_from_slice(&u64::MAX.to_be_bytes());
        assert!(decode_message_body(encoded).is_err());
    }

    #[test]
    fn message_body_validates_structured_content_before_dispatch() {
        let mut attachment = sample_body();
        attachment.kind = KIND_ATTACHMENT_MANIFEST;
        attachment.content = b"not an attachment".to_vec();
        assert!(encode_message_body(attachment).is_err());

        let receipt = sample_receipt();
        let mut body = MessageBody {
            kind: KIND_RECEIPT,
            chat_id: b"different-chat".to_vec(),
            lamport: 0,
            timestamp: 1,
            content: encode_receipt_content(receipt).unwrap(),
        };
        assert!(encode_message_body(body.clone()).is_err());

        body.kind = KIND_TEXT;
        let mut encoded = encode_message_body(body).unwrap();
        encoded[0] = KIND_RECEIPT;
        assert!(decode_message_body(encoded).is_err());
    }

    fn sample_receipt() -> ReceiptContent {
        ReceiptContent {
            chat_id: b"chat-1".to_vec(),
            sender_user_id: b"alice-user-id-16".to_vec(),
            lamport: 7,
            receipt_type: RECEIPT_TYPE_DELIVERED,
            group_id: None,
        }
    }

    /// The pre-D9 decoder: after `receipt_type` the input must be exhausted.
    /// Group receipts append a length-prefixed group id, so this is what an
    /// old client does with them — reject, drop, do not record as 1:1.
    fn decode_receipt_content_v1(bytes: Vec<u8>) -> Result<ReceiptContent, CoreError> {
        let mut cursor = Cursor::new(&bytes);
        let chat_id = cursor.take_bytes16()?;
        let sender_user_id = cursor.take_bytes16()?;
        let lamport = cursor.take_u64()?;
        let receipt_type = cursor.take_u8()?;
        cursor.finish()?;
        let content = ReceiptContent {
            chat_id,
            sender_user_id,
            lamport,
            receipt_type,
            group_id: None,
        };
        validate_receipt_content(&content)?;
        Ok(content)
    }

    fn sample_profile_sync() -> ProfileSyncContent {
        ProfileSyncContent {
            avatar_epoch: 1_700_000_123_456,
            name: "Alice".to_string(),
            avatar: vec![0xFF, 0xD8, 0x11, 0x22, 0xFF, 0xD9],
            friends_of_friends_version: 1,
            friends_of_friends_enabled: true,
            friends_of_friends_revision: 7,
        }
    }

    // -- T23 relay-change notices (kind 9) ------------------------------

    const MEMBER_TOKEN: &str = "family-member-token-abc123";

    fn sample_relay_update() -> RelayUpdateContent {
        RelayUpdateContent {
            subject_user_id: b"alice-user-id-16".to_vec(),
            relay_epoch: 1_700_000_123_456,
            relay_url: "https://new.relay.example".to_string(),
            relay_token: crate::relay_wire::relay_deposit_token_for(MEMBER_TOKEN.to_string()),
        }
    }

    #[test]
    fn relay_update_content_round_trips() {
        let content = sample_relay_update();
        let encoded = encode_relay_update_content(content.clone()).unwrap();
        let decoded = decode_relay_update_content(encoded).expect("decodes");
        assert_eq!(decoded, content);
    }

    /// CP4's whole point: a member token can fetch *and ack* -- i.e. delete --
    /// a family's mail, so it must never reach a contact. A relay-change
    /// notice fans out to every contact at once, which makes it the single
    /// worst place to leak one. The encoder attenuates rather than trusting
    /// its caller, so even a shell that naively hands over `RelayConfigStore`'s
    /// member token emits the deposit form.
    #[test]
    fn relay_update_encoding_never_carries_a_member_class_token() {
        let naive = RelayUpdateContent {
            relay_token: MEMBER_TOKEN.to_string(),
            ..sample_relay_update()
        };
        let encoded = encode_relay_update_content(naive).unwrap();

        assert!(
            !String::from_utf8_lossy(&encoded).contains(MEMBER_TOKEN),
            "encoded relay update leaked the member token"
        );
        let decoded = decode_relay_update_content(encoded).expect("decodes");
        assert!(crate::relay_wire::relay_token_is_deposit(
            decoded.relay_token.clone()
        ));
        assert_eq!(
            decoded.relay_token,
            crate::relay_wire::relay_deposit_token_for(MEMBER_TOKEN.to_string())
        );
    }

    #[test]
    fn relay_update_decode_rejects_a_member_class_token() {
        // Hand-built payload: a hostile or buggy sender cannot install a
        // fetch/ack-capable credential for a contact even by bypassing the
        // encoder.
        let mut encoded = vec![RELAY_UPDATE_VERSION];
        write_bytes16(&mut encoded, b"alice-user-id-16");
        encoded.extend_from_slice(&1_i64.to_be_bytes());
        write_bytes16(&mut encoded, b"https://new.relay.example");
        write_bytes16(&mut encoded, MEMBER_TOKEN.as_bytes());
        let err = decode_relay_update_content(encoded).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn relay_update_encodes_a_cleared_endpoint_as_no_internet_delivery() {
        let cleared = RelayUpdateContent {
            relay_url: String::new(),
            relay_token: String::new(),
            ..sample_relay_update()
        };
        let decoded =
            decode_relay_update_content(encode_relay_update_content(cleared.clone()).unwrap())
                .expect("decodes");
        assert_eq!(decoded, cleared);

        // A half-configured endpoint is not a usable one, so it normalizes to
        // the same "cleared" form rather than a partial update.
        let half = RelayUpdateContent {
            relay_url: "https://new.relay.example".to_string(),
            relay_token: String::new(),
            ..sample_relay_update()
        };
        let decoded =
            decode_relay_update_content(encode_relay_update_content(half).unwrap()).unwrap();
        assert!(decoded.relay_url.is_empty() && decoded.relay_token.is_empty());
    }

    #[test]
    fn relay_update_encoding_normalizes_the_url() {
        let content = RelayUpdateContent {
            relay_url: " new.relay.example/ ".to_string(),
            ..sample_relay_update()
        };
        let decoded =
            decode_relay_update_content(encode_relay_update_content(content).unwrap()).unwrap();
        assert_eq!(decoded.relay_url, "https://new.relay.example");
    }

    #[test]
    fn relay_update_decode_rejects_truncation_trailing_bytes_and_bad_versions() {
        let encoded = encode_relay_update_content(sample_relay_update()).unwrap();
        for len in 0..encoded.len() {
            assert!(
                decode_relay_update_content(encoded[..len].to_vec()).is_err(),
                "truncation at {len} decoded"
            );
        }
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(decode_relay_update_content(trailing).is_err());

        let mut bad_version = encoded;
        bad_version[0] = RELAY_UPDATE_VERSION + 1;
        assert!(decode_relay_update_content(bad_version).is_err());
    }

    #[test]
    fn relay_update_rejects_an_absent_or_oversized_subject() {
        let empty = RelayUpdateContent {
            subject_user_id: Vec::new(),
            ..sample_relay_update()
        };
        assert!(encode_relay_update_content(empty).is_err());
        let huge = RelayUpdateContent {
            subject_user_id: vec![7; RELAY_UPDATE_MAX_SUBJECT_BYTES + 1],
            ..sample_relay_update()
        };
        assert!(encode_relay_update_content(huge).is_err());
    }

    #[test]
    fn relay_update_body_survives_seal_and_open_round_trip() {
        let alice = generate_identity();
        let bob = generate_identity();
        let content = RelayUpdateContent {
            subject_user_id: alice.user_id.clone(),
            ..sample_relay_update()
        };
        let body = MessageBody {
            kind: KIND_RELAY_UPDATE,
            chat_id: alice.user_id.clone(),
            lamport: 3,
            timestamp: 1_700_000_001_000,
            content: encode_relay_update_content(content.clone()).unwrap(),
        };
        let payload = encode_message_body(body).unwrap();
        let sealed = seal_message(alice.clone(), bob.agree_pk.clone(), payload).expect("seals");
        let opened = open_message(bob, sealed).expect("opens");
        assert_eq!(opened.sender_user_id, alice.user_id);
        let decoded_body = decode_message_body(opened.payload).expect("decodes body");
        assert_eq!(decoded_body.kind, KIND_RELAY_UPDATE);
        assert_eq!(
            decode_relay_update_content(decoded_body.content).unwrap(),
            content
        );
    }

    /// Backward compatibility: a build that predates kind 9 must treat the
    /// notice as an ordinary unknown kind -- the body decodes cleanly, the
    /// dispatcher finds no handler and drops it. Nothing errors at the frame
    /// or body layer, so the link is never poisoned and the peer keeps
    /// working exactly as before.
    #[test]
    fn an_unrecognized_kind_decodes_cleanly_so_old_builds_can_just_drop_it() {
        for kind in [KIND_RELAY_UPDATE, 0x5A, 0xFE] {
            let body = MessageBody {
                kind,
                chat_id: b"alice-user-id-16".to_vec(),
                lamport: 4,
                timestamp: 1_700_000_002_000,
                content: b"payload an old build cannot interpret".to_vec(),
            };
            let encoded = encode_message_body(body.clone()).expect("encodes");
            let decoded = decode_message_body(encoded).expect("an old build still decodes it");
            assert_eq!(decoded, body);
            // ...and it is never mistaken for chat history.
            assert!(!crate::core_is_visible_chat_kind(kind));
        }
    }

    #[test]
    fn profile_sync_content_round_trips() {
        let content = sample_profile_sync();
        let encoded = encode_profile_sync_content(content.clone()).unwrap();
        let decoded = decode_profile_sync_content(encoded).expect("decodes");
        assert_eq!(decoded, content);
    }

    #[test]
    fn profile_sync_content_round_trips_with_empty_fields() {
        let content = ProfileSyncContent {
            avatar_epoch: 0,
            name: String::new(),
            avatar: Vec::new(),
            friends_of_friends_version: 1,
            friends_of_friends_enabled: false,
            friends_of_friends_revision: 8,
        };
        let encoded = encode_profile_sync_content(content.clone()).unwrap();
        let decoded = decode_profile_sync_content(encoded).expect("decodes");
        assert_eq!(decoded, content);
    }

    #[test]
    fn profile_sync_content_decode_rejects_truncation_at_each_field() {
        let encoded = encode_profile_sync_content(sample_profile_sync()).unwrap();
        for len in 0..encoded.len() {
            let err = decode_profile_sync_content(encoded[..len].to_vec()).unwrap_err();
            assert!(matches!(err, CoreError::Malformed(_)), "len {len}");
        }
    }

    #[test]
    fn profile_sync_content_decode_rejects_trailing_garbage() {
        let mut encoded = encode_profile_sync_content(sample_profile_sync()).unwrap();
        encoded.push(0);
        let err = decode_profile_sync_content(encoded).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn profile_sync_content_decode_rejects_unknown_version() {
        let mut encoded = encode_profile_sync_content(sample_profile_sync()).unwrap();
        encoded[0] = 3;
        let err = decode_profile_sync_content(encoded).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn profile_sync_v1_decodes_with_unknown_discovery_policy() {
        let content = sample_profile_sync();
        let mut encoded = Vec::new();
        encoded.push(1);
        encoded.extend_from_slice(&content.avatar_epoch.to_be_bytes());
        write_bytes16(&mut encoded, content.name.as_bytes());
        write_bytes32(&mut encoded, &content.avatar);
        let decoded = decode_profile_sync_content(encoded).expect("v1 decodes");
        assert_eq!(decoded.friends_of_friends_version, 0);
        assert!(!decoded.friends_of_friends_enabled);
        assert_eq!(decoded.friends_of_friends_revision, 0);
    }

    #[test]
    fn sealed_lan_endpoint_content_round_trips() {
        let content = LanEndpointContent {
            instance_token: vec![0xAB; LAN_INSTANCE_TOKEN_LEN],
            network_id: b"hashed-network-id".to_vec(),
            host: "10.154.189.58".to_string(),
            port: 45_892,
            expires_at_ms: 1_700_000_900_000,
        };
        let encoded = encode_lan_endpoint_content(content.clone()).unwrap();
        assert_eq!(decode_lan_endpoint_content(encoded).unwrap(), content);
    }

    #[test]
    fn sealed_lan_endpoint_content_rejects_invalid_fields() {
        let valid = LanEndpointContent {
            instance_token: vec![0xAB; LAN_INSTANCE_TOKEN_LEN],
            network_id: vec![1; 16],
            host: "10.0.0.2".to_string(),
            port: 45_892,
            expires_at_ms: 1_700_000_900_000,
        };
        let mut bad_token = valid.clone();
        bad_token.instance_token.pop();
        assert!(encode_lan_endpoint_content(bad_token).is_err());

        let mut bad_network = valid.clone();
        bad_network.network_id = vec![1; 33];
        assert!(encode_lan_endpoint_content(bad_network).is_err());

        let mut trailing = encode_lan_endpoint_content(valid).unwrap();
        trailing.push(0xFF);
        assert!(decode_lan_endpoint_content(trailing).is_err());
    }

    fn sample_directory() -> (Identity, Identity, Identity, FriendDirectoryContent) {
        let alice = crate::generate_identity();
        let bob = crate::generate_identity();
        let carol = crate::generate_identity();
        let ticket = create_introduction_ticket(
            alice.clone(),
            carol.user_id.clone(),
            bob.user_id.clone(),
            9,
            1_700_000_000_000,
            1_702_000_000_000,
            vec![4; 16],
        )
        .expect("ticket");
        let directory = FriendDirectoryContent {
            version: 1,
            revision: 3,
            entries: vec![FriendDirectoryEntry {
                candidate: SuggestedFriendCard {
                    name: "Carol".to_string(),
                    user_id: carol.user_id.clone(),
                    sign_pk: carol.sign_pk.clone(),
                    agree_pk: carol.agree_pk.clone(),
                },
                candidate_policy_revision: 9,
                ticket,
            }],
        };
        (alice, bob, carol, directory)
    }

    /// A directory and an introduction both arrive over a mesh link from a
    /// peer, and both shells log the reason they dropped the frame. The frame
    /// itself must not be that reason.
    ///
    /// Whole-message equality on purpose: "does not contain the frame" is the
    /// property, but only pinning the message rules out a later edit
    /// appending something else that came off the link.
    #[test]
    fn a_malformed_directory_is_named_without_quoting_the_frame() {
        let bytes = br#"{"entries":"MARKER-cabin-8042"}"#.to_vec();
        let len = bytes.len();
        let error = decode_friend_directory_content(bytes).unwrap_err();
        let CoreError::Malformed(message) = error else {
            panic!("expected a malformed friend directory");
        };
        assert_eq!(
            message,
            format!("invalid friend directory: data error at line 1 column 30 of {len}B")
        );
    }

    #[test]
    fn a_malformed_introduction_is_named_without_quoting_the_frame() {
        let bytes = br#"{"version":"MARKER-cabin-8042"}"#.to_vec();
        let len = bytes.len();
        let error = decode_introduced_friend_request(bytes).unwrap_err();
        let CoreError::Malformed(message) = error else {
            panic!("expected a malformed introduced friend request");
        };
        assert_eq!(
            message,
            format!("invalid introduced friend request: data error at line 1 column 30 of {len}B")
        );
    }

    #[test]
    fn friend_directory_and_ticket_round_trip() {
        let (alice, bob, carol, directory) = sample_directory();
        let decoded =
            decode_friend_directory_content(encode_friend_directory_content(directory.clone()))
                .expect("directory decodes");
        assert_eq!(decoded, directory);
        assert!(verify_introduction_ticket(
            decoded.entries[0].ticket.clone(),
            alice.sign_pk,
            carol.user_id,
            bob.user_id,
            9,
            1_701_000_000_000,
        )
        .expect("ticket verifies"));
    }

    #[test]
    fn introduction_ticket_is_bound_to_invitee_and_policy() {
        let (alice, bob, carol, directory) = sample_directory();
        let ticket = directory.entries[0].ticket.clone();
        assert!(!verify_introduction_ticket(
            ticket.clone(),
            alice.sign_pk.clone(),
            carol.user_id.clone(),
            vec![8; 16],
            9,
            1_701_000_000_000,
        )
        .unwrap());
        assert!(!verify_introduction_ticket(
            ticket,
            alice.sign_pk,
            carol.user_id,
            bob.user_id,
            10,
            1_701_000_000_000,
        )
        .unwrap());
    }

    #[test]
    fn introduction_ticket_fails_closed_for_tampering_and_time_bounds() {
        let (alice, bob, carol, directory) = sample_directory();
        let ticket = directory.entries[0].ticket.clone();

        let mut forged = ticket.clone();
        forged.signature[0] ^= 0x80;
        assert!(!verify_introduction_ticket(
            forged,
            alice.sign_pk.clone(),
            carol.user_id.clone(),
            bob.user_id.clone(),
            9,
            1_701_000_000_000,
        )
        .unwrap());

        assert!(!verify_introduction_ticket(
            ticket.clone(),
            alice.sign_pk.clone(),
            carol.user_id.clone(),
            bob.user_id.clone(),
            9,
            ticket.issued_at_ms - INTRODUCTION_CLOCK_SKEW_MS - 1,
        )
        .unwrap());
        assert!(!verify_introduction_ticket(
            ticket.clone(),
            alice.sign_pk.clone(),
            carol.user_id.clone(),
            bob.user_id.clone(),
            9,
            ticket.expires_at_ms + INTRODUCTION_CLOCK_SKEW_MS + 1,
        )
        .unwrap());

        let mut malformed = ticket.clone();
        malformed.offer_id.pop();
        assert!(verify_introduction_ticket(
            malformed,
            alice.sign_pk.clone(),
            carol.user_id.clone(),
            bob.user_id.clone(),
            9,
            1_701_000_000_000,
        )
        .is_err());

        let mut unknown = ticket;
        unknown.version = 2;
        assert!(verify_introduction_ticket(
            unknown,
            alice.sign_pk,
            carol.user_id,
            bob.user_id,
            9,
            1_701_000_000_000,
        )
        .is_err());
    }

    #[test]
    fn introduction_ticket_rejects_more_than_thirty_days() {
        let alice = crate::generate_identity();
        let bob = crate::generate_identity();
        let carol = crate::generate_identity();
        let result = create_introduction_ticket(
            alice,
            carol.user_id,
            bob.user_id,
            1,
            1_700_000_000_000,
            1_700_000_000_000 + INTRODUCTION_MAX_LIFETIME_MS + 1,
            vec![4; 16],
        );
        assert!(matches!(result, Err(CoreError::Malformed(_))));
    }

    #[test]
    fn introduced_friend_request_round_trips() {
        let (alice, bob, _carol, directory) = sample_directory();
        let card = crate::make_friend_card("Bob".to_string(), bob, None, None).unwrap();
        let request = IntroducedFriendRequest {
            version: 1,
            friend_card_json: card,
            ticket: directory.entries[0].ticket.clone(),
        };
        let decoded =
            decode_introduced_friend_request(encode_introduced_friend_request(request.clone()))
                .expect("request decodes");
        assert_eq!(decoded, request);
        assert_eq!(decoded.ticket.introducer_user_id, alice.user_id);
    }

    #[test]
    fn profile_sync_content_decode_rejects_oversized_avatar() {
        let mut encoded = Vec::new();
        encoded.push(PROFILE_SYNC_VERSION);
        encoded.extend_from_slice(&1i64.to_be_bytes());
        encoded.extend_from_slice(&0u16.to_be_bytes());
        encoded.extend_from_slice(&((PROFILE_SYNC_MAX_AVATAR_BYTES + 1) as u32).to_be_bytes());
        let err = decode_profile_sync_content(encoded).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn profile_sync_rejects_oversized_names_and_authored_avatars() {
        let mut content = sample_profile_sync();
        content.name = "x".repeat(PROFILE_SYNC_MAX_NAME_BYTES + 1);
        assert!(encode_profile_sync_content(content).is_err());

        let mut content = sample_profile_sync();
        content.avatar = vec![0; PROFILE_SYNC_MAX_AVATAR_BYTES + 1];
        assert!(encode_profile_sync_content(content).is_err());

        let mut encoded = vec![PROFILE_SYNC_VERSION];
        encoded.extend_from_slice(&1i64.to_be_bytes());
        write_bytes16(&mut encoded, &[b'x'; PROFILE_SYNC_MAX_NAME_BYTES + 1]);
        encoded.extend_from_slice(&0u32.to_be_bytes());
        encoded.extend_from_slice(&[1, 0]);
        encoded.extend_from_slice(&0u64.to_be_bytes());
        assert!(decode_profile_sync_content(encoded).is_err());
    }

    #[test]
    fn receipt_content_round_trips() {
        let receipt = sample_receipt();
        let encoded = encode_receipt_content(receipt.clone()).unwrap();
        let decoded = decode_receipt_content(encoded).expect("decodes");
        assert_eq!(decoded, receipt);
    }

    #[test]
    fn receipt_content_round_trips_for_read_type() {
        let mut receipt = sample_receipt();
        receipt.receipt_type = RECEIPT_TYPE_READ;
        let encoded = encode_receipt_content(receipt.clone()).unwrap();
        let decoded = decode_receipt_content(encoded).expect("decodes");
        assert_eq!(decoded, receipt);
    }

    #[test]
    fn receipt_content_decode_rejects_truncated() {
        let mut encoded = encode_receipt_content(sample_receipt()).unwrap();
        encoded.truncate(encoded.len() - 1); // drop the receipt_type byte
        let err = decode_receipt_content(encoded).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn receipt_content_decode_rejects_garbage() {
        let err = decode_receipt_content(vec![0xFF, 0xFF, 0xFF]).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn one_to_one_receipt_bytes_are_unchanged_without_a_group_id() {
        let receipt = sample_receipt();
        let encoded = encode_receipt_content(receipt.clone()).unwrap();
        // chat_id_len(2) + chat_id + sender_len(2) + sender + lamport(8) + type(1)
        let expected_len = 2 + receipt.chat_id.len() + 2 + receipt.sender_user_id.len() + 8 + 1;
        assert_eq!(encoded.len(), expected_len);
        assert_eq!(decode_receipt_content_v1(encoded.clone()).unwrap(), receipt);
        assert_eq!(decode_receipt_content(encoded).unwrap(), receipt);
    }

    #[test]
    fn group_receipt_round_trips_and_is_dropped_by_the_v1_decoder() {
        let mut receipt = sample_receipt();
        receipt.group_id = Some(vec![0x11; GROUP_ID_LEN]);
        let encoded = encode_receipt_content(receipt.clone()).unwrap();
        assert!(
            decode_receipt_content_v1(encoded.clone()).is_err(),
            "old clients must reject the trailing group id"
        );
        assert_eq!(decode_receipt_content(encoded).unwrap(), receipt);
    }

    #[test]
    fn group_receipt_rejects_wrong_id_width() {
        let mut receipt = sample_receipt();
        receipt.group_id = Some(vec![0x11; 8]);
        assert!(encode_receipt_content(receipt).is_err());

        let mut encoded = encode_receipt_content(sample_receipt()).unwrap();
        encoded.extend_from_slice(&8u16.to_be_bytes());
        encoded.extend_from_slice(&[0x11; 8]);
        assert!(decode_receipt_content(encoded).is_err());
    }

    #[test]
    fn receipt_content_rejects_unknown_type_and_unrepresentable_lamport() {
        let mut encoded = encode_receipt_content(sample_receipt()).unwrap();
        *encoded.last_mut().unwrap() = 0xff;
        assert!(decode_receipt_content(encoded).is_err());

        let mut receipt = sample_receipt();
        receipt.receipt_type = 0xff;
        assert!(encode_receipt_content(receipt).is_err());

        let mut receipt = sample_receipt();
        receipt.lamport = i64::MAX as u64 + 1;
        assert!(encode_receipt_content(receipt).is_err());
    }

    #[test]
    fn hello_frame_round_trips() {
        let user_id = vec![0xAB; 16];
        let framed = encode_hello(user_id.clone());
        assert_eq!(framed[0], 0x01);
        match parse_frame(framed).expect("parses") {
            Frame::Hello { user_id: got } => assert_eq!(got, user_id),
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    fn sample_envelope_header() -> (Vec<u8>, u8, i64, Vec<u8>) {
        (
            vec![0xCD; MSG_ID_LEN],
            DEFAULT_HOP_TTL,
            1_700_000_600_000,
            vec![0xEF; RECIPIENT_HINT_LEN],
        )
    }

    #[test]
    fn envelope_frame_round_trips() {
        let (msg_id, hop_ttl, expiry, recipient_hint) = sample_envelope_header();
        let sealed = vec![0x11, 0x22, 0x33, 0x44];
        let framed = encode_envelope_frame(
            msg_id.clone(),
            hop_ttl,
            expiry,
            recipient_hint.clone(),
            sealed.clone(),
        );
        assert_eq!(framed[0], 0x02);
        match parse_frame(framed).expect("parses") {
            Frame::Envelope {
                msg_id: got_msg_id,
                hop_ttl: got_hop_ttl,
                expiry: got_expiry,
                recipient_hint: got_hint,
                sealed: got_sealed,
            } => {
                assert_eq!(got_msg_id, msg_id);
                assert_eq!(got_hop_ttl, hop_ttl);
                assert_eq!(got_expiry, expiry);
                assert_eq!(got_hint, recipient_hint);
                assert_eq!(got_sealed, sealed);
            }
            other => panic!("expected Envelope, got {other:?}"),
        }
    }

    #[test]
    fn generate_msg_id_produces_distinct_16_byte_ids() {
        let a = generate_msg_id();
        let b = generate_msg_id();
        assert_eq!(a.len(), MSG_ID_LEN);
        assert_ne!(a, b);
    }

    #[test]
    fn compute_recipient_hint_is_deterministic_and_8_bytes() {
        let user_id = vec![0x42; 16];
        let a = compute_recipient_hint(user_id.clone(), 1_700_000_000_000);
        let b = compute_recipient_hint(user_id, 1_700_000_000_000);
        assert_eq!(a, b);
        assert_eq!(a.len(), RECIPIENT_HINT_LEN);
    }

    #[test]
    fn compute_recipient_hint_rotates_across_day_boundary_but_not_within_a_day() {
        let user_id = vec![0x42; 16];
        let morning = compute_recipient_hint(user_id.clone(), 0);
        let evening = compute_recipient_hint(user_id.clone(), MS_PER_DAY - 1);
        let next_day = compute_recipient_hint(user_id, MS_PER_DAY);
        assert_eq!(morning, evening);
        assert_ne!(morning, next_day);
    }

    #[test]
    fn compute_recipient_hint_differs_per_recipient() {
        let a = compute_recipient_hint(vec![0x01; 16], 1_700_000_000_000);
        let b = compute_recipient_hint(vec![0x02; 16], 1_700_000_000_000);
        assert_ne!(a, b);
    }

    // -- fanout_msg_id (specs/group-relay-durability.md §4.1) ---------------

    #[test]
    fn fanout_msg_id_is_deterministic_and_16_bytes() {
        let original = vec![0x11; 16];
        let member = vec![0x22; 16];
        let a = fanout_msg_id(original.clone(), member.clone());
        let b = fanout_msg_id(original, member);
        assert_eq!(a, b);
        assert_eq!(a.len(), MSG_ID_LEN);
    }

    #[test]
    fn fanout_msg_id_differs_across_members_for_the_same_message() {
        let original = vec![0x11; 16];
        let member_a = vec![0x01; 16];
        let member_b = vec![0x02; 16];
        let a = fanout_msg_id(original.clone(), member_a);
        let b = fanout_msg_id(original, member_b);
        assert_ne!(a, b);
    }

    #[test]
    fn fanout_msg_id_differs_across_original_messages_for_the_same_member() {
        let member = vec![0x22; 16];
        let a = fanout_msg_id(vec![0x01; 16], member.clone());
        let b = fanout_msg_id(vec![0x02; 16], member);
        assert_ne!(a, b);
    }

    // -- device_fanout_msg_id (specs/multi-device-v1.md §7) -----------------

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Golden vector over fixed bytes: this id space is a wire format. Two
    /// devices of a mixed fleet only converge on the same relay row if they
    /// derive the same 16 bytes from the same pair, so the derivation is
    /// frozen here rather than left to whatever the current implementation
    /// happens to produce.
    #[test]
    fn device_fanout_msg_id_golden_vector() {
        let id = device_fanout_msg_id(vec![0x11; 16], vec![0x22; 16]);
        assert_eq!(hex(&id), "cd7061816a7028164af5f089a30d82b7");
    }

    #[test]
    fn device_fanout_msg_id_is_deterministic_and_16_bytes() {
        let original = vec![0x11; 16];
        let device = vec![0x22; 16];
        let a = device_fanout_msg_id(original.clone(), device.clone());
        let b = device_fanout_msg_id(original, device);
        assert_eq!(a, b);
        assert_eq!(a.len(), MSG_ID_LEN);
    }

    #[test]
    fn device_fanout_msg_id_differs_across_devices_for_the_same_message() {
        let original = vec![0x11; 16];
        let a = device_fanout_msg_id(original.clone(), vec![0x01; 16]);
        let b = device_fanout_msg_id(original, vec![0x02; 16]);
        assert_ne!(a, b);
    }

    #[test]
    fn device_fanout_msg_id_differs_across_original_messages_for_the_same_device() {
        let device = vec![0x22; 16];
        let a = device_fanout_msg_id(vec![0x01; 16], device.clone());
        let b = device_fanout_msg_id(vec![0x02; 16], device);
        assert_ne!(a, b);
    }

    /// The prologue is the only separator between the two 16-byte id spaces:
    /// a device id and a member user id are indistinguishable as bytes, so
    /// this is what stops a group row and a device row from colliding.
    #[test]
    fn device_fanout_msg_id_is_disjoint_from_the_group_fanout_space() {
        let original = vec![0x11; 16];
        let id = vec![0x22; 16];
        assert_ne!(
            device_fanout_msg_id(original.clone(), id.clone()),
            fanout_msg_id(original, id),
        );
    }

    /// §5 / ACK-MD-2: a legacy device id (and the absent-or-malformed field
    /// that maps to it) leaves the row exactly as a v1 sender uploads it
    /// today, so legacy peers stay indistinguishable from today's behaviour.
    #[test]
    fn device_fanout_msg_id_leaves_a_legacy_row_alone() {
        let original = vec![0x11; 16];
        assert_eq!(
            device_fanout_msg_id(original.clone(), crate::LEGACY_DEVICE_ID.to_vec()),
            original,
        );
        assert_eq!(
            device_fanout_msg_id(original.clone(), Vec::new()),
            original.clone(),
        );
        assert_eq!(
            device_fanout_msg_id(original.clone(), vec![0x22; 8]),
            original
        );
    }

    #[test]
    fn default_expiry_adds_the_default_window() {
        assert_eq!(default_expiry(1_000), 1_000 + DEFAULT_EXPIRY_MS);
    }

    #[test]
    fn parse_frame_rejects_empty_input() {
        let err = parse_frame(Vec::new()).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn parse_frame_rejects_oversized_input_before_dispatch() {
        let err = parse_frame(vec![0x99; MAX_P2P_FRAME_BYTES + 1]).unwrap_err();
        assert!(err.to_string().contains("frame exceeds"));
    }

    #[test]
    fn parse_frame_accepts_envelope_at_sealed_limit() {
        let framed = encode_envelope_frame(
            vec![1; MSG_ID_LEN],
            DEFAULT_HOP_TTL,
            default_expiry(0),
            vec![2; RECIPIENT_HINT_LEN],
            vec![3; MAX_ENVELOPE_SEALED_BYTES],
        );
        assert!(matches!(parse_frame(framed), Ok(Frame::Envelope { .. })));
    }

    #[test]
    fn parse_frame_rejects_unknown_type_byte() {
        let err = parse_frame(vec![0x99, 0x01, 0x02]).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn parse_frame_rejects_hello_with_no_user_id() {
        let err = parse_frame(vec![0x01]).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn hello2_round_trips_and_tolerates_trailing_extension_bytes() {
        let user_id = vec![7_u8; 16];
        let frame = encode_hello2(user_id.clone(), 0x0000_0001).unwrap();
        assert_eq!(
            parse_frame(frame.clone()).unwrap(),
            Frame::Hello2 {
                user_id: user_id.clone(),
                capabilities: 1,
            }
        );

        // Trailing bytes are HELLO2's designated forward-extension point: a
        // future build appending fields must still parse on this build.
        let mut extended = frame;
        extended.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        assert_eq!(
            parse_frame(extended).unwrap(),
            Frame::Hello2 {
                user_id,
                capabilities: 1,
            }
        );
    }

    #[test]
    fn hello2_rejects_short_frames_and_wrong_user_id_length() {
        let err = parse_frame(vec![0x06, 1, 2, 3]).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
        assert!(encode_hello2(vec![1_u8; 15], 1).is_err());
    }

    /// §10 step 5's frame: the body is carried verbatim, an empty one is not a
    /// frame at either end, and the type byte is the next one after HELLO2 so a
    /// legacy peer meets it at `parse_frame`'s unknown-type arm.
    #[test]
    fn own_roster_notice_round_trips_and_refuses_an_empty_document() {
        let document = vec![0xA5_u8; 300];
        let framed = encode_own_roster(document.clone()).unwrap();
        assert_eq!(framed[0], 0x07);
        match parse_frame(framed).expect("parses") {
            Frame::OwnRoster { document: parsed } => assert_eq!(parsed, document),
            other => panic!("own roster notice parsed as {other:?}"),
        }
        assert!(encode_own_roster(Vec::new()).is_err());
        assert!(matches!(
            parse_frame(vec![0x07]).unwrap_err(),
            CoreError::Malformed(_)
        ));
        // A body that could never fit on a link is refused before it is built,
        // not discovered on the way back in.
        assert!(encode_own_roster(vec![0_u8; MAX_P2P_FRAME_BYTES]).is_err());
    }

    /// The notice gets its own capability bit, for the reason every bit above
    /// it did: an advertisement a deployed build made honestly must keep meaning
    /// what it meant when that build shipped.
    #[test]
    fn the_own_roster_notice_has_its_own_capability_bit() {
        assert_eq!(CAP_OWN_ROSTER_NOTICE, 1 << 4);
        assert_ne!(core_own_capabilities() & CAP_OWN_ROSTER_NOTICE, 0);
        for other in [
            CAP_ACKS_HIDDEN_KINDS,
            CAP_RELAY_UPDATE,
            CAP_MULTI_DEVICE,
            CAP_ROSTER_GOSSIP,
        ] {
            assert_ne!(CAP_OWN_ROSTER_NOTICE, other);
        }
        // It is a link-control frame, not a hidden spray kind: there is no
        // envelope, no relay row, and no DELIVERED watermark to advance.
        for kind in HIDDEN_SPRAY_KINDS {
            assert_ne!(hidden_ack_capability(kind), Some(CAP_OWN_ROSTER_NOTICE));
        }
    }

    #[test]
    fn own_capabilities_advertise_hidden_kind_acks() {
        assert_ne!(core_own_capabilities() & CAP_ACKS_HIDDEN_KINDS, 0);
        // T23: kind 9 gets its own bit rather than riding bit 1, because a
        // build that predates it advertises bit 1 truthfully and still drops
        // a relay-change notice unhandled.
        assert_ne!(core_own_capabilities() & CAP_RELAY_UPDATE, 0);
        assert_ne!(CAP_ACKS_HIDDEN_KINDS, CAP_RELAY_UPDATE);
        // WP1 (multi-device-v1 §12): the reserved bit is now advertised,
        // because this build really does read §5's sealed-body device field
        // and keep a person's devices on separate author streams. A peer that
        // sets any other unknown high bit must still parse.
        assert_ne!(core_own_capabilities() & CAP_MULTI_DEVICE, 0);
        assert_eq!(CAP_MULTI_DEVICE, 1 << 2);
        let user_id = vec![7_u8; 16];
        let future_caps = core_own_capabilities() | (1 << 31);
        let frame = encode_hello2(user_id.clone(), future_caps).unwrap();
        match parse_frame(frame).unwrap() {
            Frame::Hello2 {
                user_id: parsed,
                capabilities,
            } => {
                assert_eq!(parsed, user_id);
                assert_eq!(capabilities, future_caps);
            }
            other => panic!("HELLO2 with unknown cap bits parsed as {other:?}"),
        }
    }

    #[test]
    fn hidden_spray_kind_classification_matches_the_sideband_set() {
        for kind in [
            KIND_FRIEND_REQUEST,
            KIND_PROFILE_SYNC,
            KIND_FRIEND_DIRECTORY,
            KIND_INTRODUCED_FRIEND_REQUEST,
            KIND_RELAY_UPDATE,
        ] {
            assert!(core_is_hidden_spray_kind(kind), "kind {kind} is sideband");
        }
        for kind in [
            KIND_TEXT,
            KIND_GROUP_INVITE,
            KIND_ATTACHMENT_MANIFEST,
            KIND_RECEIPT,
        ] {
            assert!(!core_is_hidden_spray_kind(kind), "kind {kind} is not gated");
        }
    }

    #[test]
    fn parse_frame_rejects_envelope_with_truncated_header() {
        // Type byte alone: not even a full msg_id follows.
        let err = parse_frame(vec![0x02]).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn parse_frame_rejects_envelope_with_no_sealed_bytes() {
        // A complete header but nothing after it.
        let (msg_id, hop_ttl, expiry, recipient_hint) = sample_envelope_header();
        let framed = encode_envelope_frame(msg_id, hop_ttl, expiry, recipient_hint, Vec::new());
        let err = parse_frame(framed).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    fn sample_entries() -> Vec<DigestEntry> {
        vec![
            DigestEntry {
                sender_user_id: b"alice-user-id-16".to_vec(),
                through_lamport: 12,
            },
            DigestEntry {
                sender_user_id: b"bob-user-id-1616".to_vec(),
                through_lamport: 3,
            },
        ]
    }

    fn sample_recent_msg_ids() -> Vec<Vec<u8>> {
        vec![vec![0x11; MSG_ID_LEN], vec![0x22; MSG_ID_LEN]]
    }

    #[test]
    fn digest_frame_round_trips() {
        let chat_id = b"chat-1".to_vec();
        let entries = sample_entries();
        let recent_msg_ids = sample_recent_msg_ids();
        let framed =
            encode_digest(chat_id.clone(), entries.clone(), recent_msg_ids.clone()).unwrap();
        assert_eq!(framed[0], 0x03);
        match parse_frame(framed).expect("parses") {
            Frame::Digest {
                chat_id: got_chat,
                entries: got_entries,
                recent_msg_ids: got_recent,
            } => {
                assert_eq!(got_chat, chat_id);
                assert_eq!(got_entries, entries);
                assert_eq!(got_recent, recent_msg_ids);
            }
            other => panic!("expected Digest, got {other:?}"),
        }
    }

    #[test]
    fn digest_frame_round_trips_with_no_entries() {
        // "I have nothing in this chat" is a valid digest (asks for everything).
        let framed = encode_digest(b"chat-1".to_vec(), Vec::new(), Vec::new()).unwrap();
        match parse_frame(framed).expect("parses") {
            Frame::Digest {
                chat_id,
                entries,
                recent_msg_ids,
            } => {
                assert_eq!(chat_id, b"chat-1".to_vec());
                assert!(entries.is_empty());
                assert!(recent_msg_ids.is_empty());
            }
            other => panic!("expected Digest, got {other:?}"),
        }
    }

    #[test]
    fn digest_frame_round_trips_with_empty_chat_id_and_max_lamport() {
        let entries = vec![DigestEntry {
            sender_user_id: b"alice".to_vec(),
            through_lamport: u64::MAX,
        }];
        let framed = encode_digest(Vec::new(), entries.clone(), sample_recent_msg_ids()).unwrap();
        match parse_frame(framed).expect("parses") {
            Frame::Digest {
                chat_id,
                entries: got,
                recent_msg_ids,
            } => {
                assert!(chat_id.is_empty());
                assert_eq!(got, entries);
                assert_eq!(recent_msg_ids, sample_recent_msg_ids());
            }
            other => panic!("expected Digest, got {other:?}"),
        }
    }

    #[test]
    fn lan_endpoint_frame_round_trips() {
        let token = vec![0xAB; LAN_INSTANCE_TOKEN_LEN];
        let framed =
            encode_lan_endpoint(token.clone(), "10.154.189.58".to_string(), 45_892).unwrap();
        assert_eq!(framed[0], FRAME_TYPE_LAN_ENDPOINT);
        match parse_frame(framed).expect("parses") {
            Frame::LanEndpoint {
                instance_token,
                host,
                port,
            } => {
                assert_eq!(instance_token, token);
                assert_eq!(host, "10.154.189.58");
                assert_eq!(port, 45_892);
            }
            other => panic!("expected LAN endpoint, got {other:?}"),
        }
    }

    #[test]
    fn lan_endpoint_rejects_invalid_fields_and_trailing_data() {
        assert!(encode_lan_endpoint(vec![1; 7], "10.0.0.2".to_string(), 45_892).is_err());
        assert!(
            encode_lan_endpoint(vec![1; LAN_INSTANCE_TOKEN_LEN], "10.0.0.2".to_string(), 0,)
                .is_err()
        );
        assert!(encode_lan_endpoint(
            vec![1; LAN_INSTANCE_TOKEN_LEN],
            "not a host".to_string(),
            45_892,
        )
        .is_err());

        let mut framed = encode_lan_endpoint(
            vec![1; LAN_INSTANCE_TOKEN_LEN],
            "10.0.0.2".to_string(),
            45_892,
        )
        .unwrap();
        framed.push(0xFF);
        assert!(parse_frame(framed).is_err());
    }

    /// Hosts a phone can genuinely have as its own address on a local
    /// network, including the shapes Android's `getHostAddress()` produces.
    const LOCAL_LAN_HOSTS: &[&str] = &[
        "10.0.0.2",
        "10.154.189.58",
        "172.16.0.9",
        "172.31.255.254",
        "192.168.1.7",
        "169.254.10.3",
        "100.64.0.5",
        "100.127.255.254",
        "fe80::1",
        "fe80::4ff:fe12:3456%wlan0",
        "fe80::1%3",
        "fc00::1",
        "fd12:3456:789a::1",
    ];

    /// Everything else: no address off the local network can be the sender's
    /// own LAN address, and nothing that needs resolving is an address at all.
    const NON_LOCAL_LAN_HOSTS: &[&str] = &[
        // Public and otherwise non-local literals.
        "8.8.8.8",
        "1.1.1.1",
        "203.0.113.5",
        "172.32.0.1",
        "192.169.1.1",
        "100.128.0.1",
        "127.0.0.1",
        "0.0.0.0",
        "255.255.255.255",
        "2606:4700:4700::1111",
        "2001:db8::1",
        "::1",
        "::",
        "::ffff:10.0.0.1",
        // Names -- a receiver must never resolve a string a sender chose.
        "localhost",
        "phone.local",
        "cruisemesh.app",
        "10.0.0.2.example.com",
        "010.0.0.2",
        // Malformed, or a scope id where none belongs.
        "",
        "10.0.0.2%wlan0",
        "fe80::1%",
        "fd00::1%wlan0",
        "fe80::1%wlan0!",
        "10.0.0.2:45892",
    ];

    fn lan_endpoint_frame_bytes(host: &str) -> Vec<u8> {
        let mut out = vec![FRAME_TYPE_LAN_ENDPOINT, LAN_ENDPOINT_VERSION];
        out.extend_from_slice(&[0xAB; LAN_INSTANCE_TOKEN_LEN]);
        out.extend_from_slice(&45_892u16.to_be_bytes());
        out.push(host.len() as u8);
        out.extend_from_slice(host.as_bytes());
        out
    }

    fn lan_endpoint_content_bytes(host: &str) -> Vec<u8> {
        let mut out = vec![LAN_ENDPOINT_CONTENT_VERSION];
        out.extend_from_slice(&[0xAB; LAN_INSTANCE_TOKEN_LEN]);
        out.extend_from_slice(&45_892u16.to_be_bytes());
        out.extend_from_slice(&1_700_000_900_000i64.to_be_bytes());
        out.push(4);
        out.extend_from_slice(b"netz");
        out.push(host.len() as u8);
        out.extend_from_slice(host.as_bytes());
        out
    }

    #[test]
    fn lan_endpoint_accepts_every_local_address_a_phone_can_have() {
        for host in LOCAL_LAN_HOSTS {
            let token = vec![0xAB; LAN_INSTANCE_TOKEN_LEN];
            encode_lan_endpoint(token.clone(), host.to_string(), 45_892)
                .unwrap_or_else(|error| panic!("{host} should encode: {error:?}"));
            match parse_frame(lan_endpoint_frame_bytes(host)) {
                Ok(Frame::LanEndpoint { host: parsed, .. }) => assert_eq!(&parsed, host),
                other => panic!("{host} should parse, got {other:?}"),
            }
            let content = LanEndpointContent {
                instance_token: token,
                network_id: b"netz".to_vec(),
                host: host.to_string(),
                port: 45_892,
                expires_at_ms: 1_700_000_900_000,
            };
            let encoded = encode_lan_endpoint_content(content.clone())
                .unwrap_or_else(|error| panic!("{host} should encode sealed: {error:?}"));
            assert_eq!(decode_lan_endpoint_content(encoded).unwrap(), content);
        }
    }

    #[test]
    fn lan_endpoint_rejects_hosts_that_are_not_local_addresses() {
        for host in NON_LOCAL_LAN_HOSTS {
            let token = vec![0xAB; LAN_INSTANCE_TOKEN_LEN];
            assert!(
                encode_lan_endpoint(token.clone(), host.to_string(), 45_892).is_err(),
                "{host} must not encode",
            );
            assert!(
                parse_frame(lan_endpoint_frame_bytes(host)).is_err(),
                "{host} must not parse",
            );
            assert!(
                encode_lan_endpoint_content(LanEndpointContent {
                    instance_token: token,
                    network_id: b"netz".to_vec(),
                    host: host.to_string(),
                    port: 45_892,
                    expires_at_ms: 1_700_000_900_000,
                })
                .is_err(),
                "{host} must not encode sealed",
            );
            assert!(
                decode_lan_endpoint_content(lan_endpoint_content_bytes(host)).is_err(),
                "{host} must not decode sealed",
            );
        }
    }

    #[test]
    fn lan_endpoint_rejects_whitespace_and_oversized_zone() {
        for host in [" ", "10.0.0.2 ", "10.0.0.2\n", "\t"] {
            assert!(
                parse_frame(lan_endpoint_frame_bytes(host)).is_err(),
                "{host:?} must not parse",
            );
        }
        let long_zone = format!("fe80::1%{}", "w".repeat(MAX_LAN_HOST_ZONE_BYTES + 1));
        assert!(parse_frame(lan_endpoint_frame_bytes(&long_zone)).is_err());
    }

    #[test]
    fn transport_probe_request_and_response_round_trip() {
        for response in [false, true] {
            let framed = encode_transport_probe(0x0102_0304_0506_0708, response);
            assert_eq!(framed[0], FRAME_TYPE_TRANSPORT_PROBE);
            assert_eq!(
                parse_frame(framed).unwrap(),
                Frame::TransportProbe {
                    nonce: 0x0102_0304_0506_0708,
                    response,
                },
            );
        }
    }

    #[test]
    fn transport_probe_rejects_bad_flag_and_trailing_data() {
        let mut bad_flag = encode_transport_probe(7, false);
        bad_flag[2] = 2;
        assert!(parse_frame(bad_flag).is_err());

        let mut trailing = encode_transport_probe(7, false);
        trailing.push(0xFF);
        assert!(parse_frame(trailing).is_err());
    }

    #[test]
    fn parse_frame_rejects_digest_with_empty_body() {
        // Type byte alone: not even a chat_id length prefix.
        let err = parse_frame(vec![0x03]).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn parse_frame_rejects_digest_with_truncated_chat_id() {
        // chat_id_len claims 10 bytes, none follow.
        let err = parse_frame(vec![0x03, 0x00, 0x0A]).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn parse_frame_rejects_digest_missing_entry_count() {
        // Valid empty chat_id, then nothing where entry_count should be.
        let err = parse_frame(vec![0x03, 0x00, 0x00]).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn parse_frame_rejects_digest_with_fewer_entries_than_claimed() {
        // Empty chat_id, entry_count = 2, but only one (complete) entry follows.
        let mut bytes = vec![0x03, 0x00, 0x00, 0x00, 0x02];
        bytes.extend_from_slice(&(5u16).to_be_bytes());
        bytes.extend_from_slice(b"alice");
        bytes.extend_from_slice(&7u64.to_be_bytes());
        let err = parse_frame(bytes).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn parse_frame_rejects_digest_with_truncated_lamport() {
        // One entry whose through_lamport is cut short.
        let mut bytes = vec![0x03, 0x00, 0x00, 0x00, 0x01];
        bytes.extend_from_slice(&(5u16).to_be_bytes());
        bytes.extend_from_slice(b"alice");
        bytes.extend_from_slice(&[0x00, 0x00, 0x00]); // 3 of 8 lamport bytes
        let err = parse_frame(bytes).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn parse_frame_rejects_digest_missing_recent_msg_id_count() {
        let mut bytes = vec![0x03, 0x00, 0x00, 0x00, 0x01];
        bytes.extend_from_slice(&(5u16).to_be_bytes());
        bytes.extend_from_slice(b"alice");
        bytes.extend_from_slice(&7u64.to_be_bytes());
        let err = parse_frame(bytes).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn parse_frame_rejects_digest_with_truncated_recent_msg_id() {
        let mut bytes = encode_digest(b"chat-1".to_vec(), sample_entries(), Vec::new()).unwrap();
        bytes.extend_from_slice(&(1u16).to_be_bytes());
        bytes.extend_from_slice(&[0xAA; MSG_ID_LEN - 1]);
        let err = parse_frame(bytes).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn parse_frame_rejects_digest_with_trailing_garbage() {
        let mut framed = encode_digest(
            b"chat-1".to_vec(),
            sample_entries(),
            sample_recent_msg_ids(),
        )
        .unwrap();
        framed.push(0xFF);
        let err = parse_frame(framed).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn digest_encoder_rejects_invalid_message_ids_without_panicking() {
        let result = encode_digest(Vec::new(), Vec::new(), vec![vec![0; MSG_ID_LEN - 1]]);
        assert!(matches!(result, Err(CoreError::Malformed(_))));
    }

    #[test]
    fn message_body_survives_seal_and_open_round_trip() {
        let alice = generate_identity();
        let bob = generate_identity();

        let body = sample_body();
        let payload = encode_message_body(body.clone()).unwrap();

        let sealed =
            seal_message(alice.clone(), bob.agree_pk.clone(), payload).expect("seal succeeds");
        let opened = open_message(bob, sealed).expect("open succeeds");
        assert_eq!(opened.sender_user_id, alice.user_id);

        let decoded = decode_message_body(opened.payload).expect("decodes");
        assert_eq!(decoded, body);
    }

    #[test]
    fn reply_reference_survives_inside_the_sealed_payload() {
        let sender = generate_identity();
        let recipient = generate_identity();
        let reply_to = vec![8; MSG_ID_LEN];
        let payload =
            encode_message_body_with_reply(sample_body(), reply_to.clone()).expect("encodes");

        let sealed = seal_message(sender, recipient.agree_pk.clone(), payload).expect("seals");
        let opened = open_message(recipient, sealed).expect("opens");
        let decoded = decode_extended_message_body(opened.payload).expect("decodes");

        assert_eq!(decoded.reply_to_msg_id, Some(reply_to));
    }

    #[test]
    fn receipt_body_survives_seal_and_open_round_trip() {
        let alice = generate_identity();
        let bob = generate_identity();

        let receipt = sample_receipt();
        let body = MessageBody {
            kind: KIND_RECEIPT,
            chat_id: receipt.chat_id.clone(),
            lamport: 99,
            timestamp: 1_700_000_001_000,
            content: encode_receipt_content(receipt.clone()).unwrap(),
        };
        let payload = encode_message_body(body.clone()).unwrap();

        let sealed =
            seal_message(alice.clone(), bob.agree_pk.clone(), payload).expect("seal succeeds");
        let opened = open_message(bob, sealed).expect("open succeeds");

        let decoded_body = decode_message_body(opened.payload).expect("decodes body");
        assert_eq!(decoded_body.kind, KIND_RECEIPT);
        let decoded_receipt =
            decode_receipt_content(decoded_body.content).expect("decodes receipt content");
        assert_eq!(decoded_receipt, receipt);
    }

    #[test]
    fn profile_sync_body_survives_seal_and_open_round_trip() {
        let alice = generate_identity();
        let bob = generate_identity();

        let content = sample_profile_sync();
        let body = MessageBody {
            kind: KIND_PROFILE_SYNC,
            chat_id: bob.user_id.clone(),
            lamport: 1,
            timestamp: 1_700_000_001_000,
            content: encode_profile_sync_content(content.clone()).unwrap(),
        };
        let payload = encode_message_body(body.clone()).unwrap();

        let sealed =
            seal_message(alice.clone(), bob.agree_pk.clone(), payload).expect("seal succeeds");
        let opened = open_message(bob, sealed).expect("open succeeds");
        assert_eq!(opened.sender_user_id, alice.user_id);

        let decoded_body = decode_message_body(opened.payload).expect("decodes body");
        assert_eq!(decoded_body.kind, KIND_PROFILE_SYNC);
        let decoded_content =
            decode_profile_sync_content(decoded_body.content).expect("decodes profile sync");
        assert_eq!(decoded_content, content);
    }
}
