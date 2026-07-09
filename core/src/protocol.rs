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
//!                               attachment-chunk=17, per DESIGN.md §7.1)
//! 1       2     chat_id_len     (u16 BE)
//! 3       N     chat_id         (N = chat_id_len bytes)
//! 3+N     8     lamport         (u64 BE)
//! 11+N    8     timestamp       (i64 BE; ms since Unix epoch)
//! 19+N    4     content_len     (u32 BE)
//! 23+N    M     content         (M = content_len bytes)
//! ```
//!
//! `chat_id` uses a 16-bit length prefix (not 32-bit) because chat ids are
//! UserIDs or group ids -- tens of bytes at most; `content` uses a 32-bit
//! prefix since text bodies have more headroom (and §8 reserves room for
//! attachment-manifest bodies later). Callers are expected to respect those
//! bounds: `encode_message_body` truncates a length prefix silently if a
//! field is absurdly large (over 64 KiB for `chat_id`, over 4 GiB for
//! `content`), rather than failing, on the theory that a BLE text-messaging
//! app never legitimately produces such a value. Decoding is fully checked
//! and never panics on attacker-controlled input; malformed or truncated
//! bytes return [`CoreError::Malformed`].
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
//! ```
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
//! convention the Android `MeshService` already uses for envelopes -- and
//! carries one entry per sender in that chat: "(sender_user_id,
//! through_lamport)", i.e. "I have this sender's messages contiguously
//! through this lamport" ([`crate::DigestEntry`], computed by the store's
//! `chat_digest`). Layout (big-endian, like everything else here):
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
use rand_core::{OsRng, RngCore};

use crate::store::DigestEntry;
use crate::CoreError;

/// `MessageBody.kind` value for an ordinary text message (DESIGN.md §7.1).
pub const KIND_TEXT: u8 = 1;
/// `MessageBody.kind` value for a receipt (DESIGN.md §7.1, §7.2); `content`
/// is an encoded [`ReceiptContent`].
pub const KIND_RECEIPT: u8 = 2;
/// `MessageBody.kind` value for a signed friend-request envelope (DESIGN.md
/// §6.2, §7.1). The payload is application-defined contact-import content.
pub const KIND_FRIEND_REQUEST: u8 = 3;
/// `MessageBody.kind` value for a pairwise-sealed group invite whose
/// `content` is an encoded [`crate::Group`] record.
pub const KIND_GROUP_INVITE: u8 = 4;
/// `MessageBody.kind` value for an attachment manifest (DESIGN.md §7.1
/// reserved, §8). Android currently embeds the media blob inline in the
/// manifest payload for BLE/relay-friendly sizes; `KIND_ATTACHMENT_CHUNK`
/// is reserved for a future external-chunk transfer path.
pub const KIND_ATTACHMENT_MANIFEST: u8 = 16;
/// Reserved for content-addressed attachment chunks (DESIGN.md §8). Not
/// yet produced or consumed by the current client.
pub const KIND_ATTACHMENT_CHUNK: u8 = 17;

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

const MSG_ID_LEN: usize = 16;
const RECIPIENT_HINT_LEN: usize = 8;
const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;

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

/// Encode a [`MessageBody`] to its wire form (see module docs for layout).
#[uniffi::export]
pub fn encode_message_body(body: MessageBody) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 2 + body.chat_id.len() + 8 + 8 + 4 + body.content.len());
    out.push(body.kind);
    write_bytes16(&mut out, &body.chat_id);
    out.extend_from_slice(&body.lamport.to_be_bytes());
    out.extend_from_slice(&body.timestamp.to_be_bytes());
    write_bytes32(&mut out, &body.content);
    out
}

/// Decode a [`MessageBody`] from its wire form. Rejects truncated input,
/// corrupt length prefixes, and unexpected trailing bytes.
#[uniffi::export]
pub fn decode_message_body(bytes: Vec<u8>) -> Result<MessageBody, CoreError> {
    let mut cursor = Cursor::new(&bytes);
    let kind = cursor.take_u8()?;
    let chat_id = cursor.take_bytes16()?;
    let lamport = cursor.take_u64()?;
    let timestamp = cursor.take_i64()?;
    let content = cursor.take_bytes32()?;
    cursor.finish()?;
    Ok(MessageBody {
        kind,
        chat_id,
        lamport,
        timestamp,
        content,
    })
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
}

/// Encode a [`ReceiptContent`] to its wire form (see module docs for layout).
#[uniffi::export]
pub fn encode_receipt_content(content: ReceiptContent) -> Vec<u8> {
    let mut out =
        Vec::with_capacity(2 + content.chat_id.len() + 2 + content.sender_user_id.len() + 8 + 1);
    write_bytes16(&mut out, &content.chat_id);
    write_bytes16(&mut out, &content.sender_user_id);
    out.extend_from_slice(&content.lamport.to_be_bytes());
    out.push(content.receipt_type);
    out
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
    cursor.finish()?;
    Ok(ReceiptContent {
        chat_id,
        sender_user_id,
        lamport,
        receipt_type,
    })
}

/// A parsed BLE frame: an unauthenticated HELLO (see module docs for why
/// that's a considered choice), an opaque sealed envelope, or a per-chat
/// sync digest (DESIGN.md §7.3).
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum Frame {
    Hello {
        user_id: Vec<u8>,
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
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        1 + 2
            + chat_id.len()
            + 2
            + entries
                .iter()
                .map(|e| 2 + e.sender_user_id.len() + 8)
                .sum::<usize>()
            + 2
            + recent_msg_ids.len() * MSG_ID_LEN,
    );
    out.push(FRAME_TYPE_DIGEST);
    write_bytes16(&mut out, &chat_id);
    out.extend_from_slice(&(entries.len() as u16).to_be_bytes());
    for entry in &entries {
        write_bytes16(&mut out, &entry.sender_user_id);
        out.extend_from_slice(&entry.through_lamport.to_be_bytes());
    }
    out.extend_from_slice(&(recent_msg_ids.len() as u16).to_be_bytes());
    for msg_id in &recent_msg_ids {
        assert_eq!(
            msg_id.len(),
            MSG_ID_LEN,
            "digest msg_id must be exactly {} bytes",
            MSG_ID_LEN
        );
        out.extend_from_slice(msg_id);
    }
    out
}

/// Parse a frame-type byte + body into a [`Frame`]. Rejects empty input, an
/// unrecognized frame-type byte, a HELLO/envelope frame with no body, and a
/// truncated or trailing-garbage DIGEST body.
#[uniffi::export]
pub fn parse_frame(bytes: Vec<u8>) -> Result<Frame, CoreError> {
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
        let encoded = encode_message_body(body.clone());
        let decoded = decode_message_body(encoded).expect("decodes");
        assert_eq!(decoded, body);
    }

    #[test]
    fn message_body_round_trips_with_empty_fields() {
        let body = MessageBody {
            kind: KIND_RECEIPT,
            chat_id: Vec::new(),
            lamport: 0,
            timestamp: 0,
            content: Vec::new(),
        };
        let encoded = encode_message_body(body.clone());
        let decoded = decode_message_body(encoded).expect("decodes");
        assert_eq!(decoded, body);
    }

    #[test]
    fn message_body_round_trips_with_negative_timestamp() {
        // i64 timestamps aren't clamped to non-negative -- pre-epoch values
        // decode fine even if the app never produces them.
        let mut body = sample_body();
        body.timestamp = -1;
        let encoded = encode_message_body(body.clone());
        let decoded = decode_message_body(encoded).expect("decodes");
        assert_eq!(decoded, body);
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
        let mut encoded = encode_message_body(sample_body());
        encoded.push(0xFF);
        let err = decode_message_body(encoded).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    fn sample_receipt() -> ReceiptContent {
        ReceiptContent {
            chat_id: b"chat-1".to_vec(),
            sender_user_id: b"alice-user-id-16".to_vec(),
            lamport: 7,
            receipt_type: RECEIPT_TYPE_DELIVERED,
        }
    }

    #[test]
    fn receipt_content_round_trips() {
        let receipt = sample_receipt();
        let encoded = encode_receipt_content(receipt.clone());
        let decoded = decode_receipt_content(encoded).expect("decodes");
        assert_eq!(decoded, receipt);
    }

    #[test]
    fn receipt_content_round_trips_for_read_type() {
        let mut receipt = sample_receipt();
        receipt.receipt_type = RECEIPT_TYPE_READ;
        let encoded = encode_receipt_content(receipt.clone());
        let decoded = decode_receipt_content(encoded).expect("decodes");
        assert_eq!(decoded, receipt);
    }

    #[test]
    fn receipt_content_decode_rejects_truncated() {
        let mut encoded = encode_receipt_content(sample_receipt());
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
        let framed = encode_digest(chat_id.clone(), entries.clone(), recent_msg_ids.clone());
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
        let framed = encode_digest(b"chat-1".to_vec(), Vec::new(), Vec::new());
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
        let framed = encode_digest(Vec::new(), entries.clone(), sample_recent_msg_ids());
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
        let mut bytes = encode_digest(b"chat-1".to_vec(), sample_entries(), Vec::new());
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
        );
        framed.push(0xFF);
        let err = parse_frame(framed).unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn message_body_survives_seal_and_open_round_trip() {
        let alice = generate_identity();
        let bob = generate_identity();

        let body = sample_body();
        let payload = encode_message_body(body.clone());

        let sealed =
            seal_message(alice.clone(), bob.agree_pk.clone(), payload).expect("seal succeeds");
        let opened = open_message(bob, sealed).expect("open succeeds");
        assert_eq!(opened.sender_user_id, alice.user_id);

        let decoded = decode_message_body(opened.payload).expect("decodes");
        assert_eq!(decoded, body);
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
            content: encode_receipt_content(receipt.clone()),
        };
        let payload = encode_message_body(body.clone());

        let sealed =
            seal_message(alice.clone(), bob.agree_pk.clone(), payload).expect("seal succeeds");
        let opened = open_message(bob, sealed).expect("open succeeds");

        let decoded_body = decode_message_body(opened.payload).expect("decodes body");
        assert_eq!(decoded_body.kind, KIND_RECEIPT);
        let decoded_receipt =
            decode_receipt_content(decoded_body.content).expect("decodes receipt content");
        assert_eq!(decoded_receipt, receipt);
    }
}
