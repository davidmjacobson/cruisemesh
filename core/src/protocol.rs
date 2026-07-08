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
//!   `crypto.rs`, not to the message body decoded from inside it. This crate
//!   doesn't yet add that envelope version byte (`crypto.rs` predates this
//!   module and isn't this module's responsibility to change); flagged here
//!   so a future milestone doesn't lose track of it.
//!
//! What's left — `kind`, `chat_id`, `lamport`, `timestamp`, `content` — is
//! exactly [`MessageBody`]. Wire layout (all multi-byte integers big-endian):
//!
//! ```text
//! offset  size  field
//! 0       1     kind            (u8; text=1, receipt=2, per DESIGN.md §7.1)
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
//! - `0x02` = sealed envelope: an opaque sealed blob (crypto.rs's output).
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

use crate::CoreError;

/// `MessageBody.kind` value for an ordinary text message (DESIGN.md §7.1).
pub const KIND_TEXT: u8 = 1;
/// `MessageBody.kind` value for a receipt (DESIGN.md §7.1, §7.2); `content`
/// is an encoded [`ReceiptContent`].
pub const KIND_RECEIPT: u8 = 2;

/// `ReceiptContent.receipt_type` value: recipient's device decrypted and
/// stored the message (the ✓✓ tick, DESIGN.md §7.2).
pub const RECEIPT_TYPE_DELIVERED: u8 = 1;
/// `ReceiptContent.receipt_type` value: recipient viewed the chat (the
/// filled ✓✓ tick, DESIGN.md §7.2).
pub const RECEIPT_TYPE_READ: u8 = 2;

const FRAME_TYPE_HELLO: u8 = 0x01;
const FRAME_TYPE_ENVELOPE: u8 = 0x02;

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
    Ok(MessageBody { kind, chat_id, lamport, timestamp, content })
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
    let mut out = Vec::with_capacity(
        2 + content.chat_id.len() + 2 + content.sender_user_id.len() + 8 + 1,
    );
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
    Ok(ReceiptContent { chat_id, sender_user_id, lamport, receipt_type })
}

/// A parsed BLE frame: either an unauthenticated HELLO (see module docs for
/// why that's a considered choice) or an opaque sealed envelope.
#[derive(uniffi::Enum, Clone, Debug, PartialEq)]
pub enum Frame {
    Hello { user_id: Vec<u8> },
    Envelope { sealed: Vec<u8> },
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

/// Encode a sealed-envelope frame: frame-type byte `0x02` followed by the
/// sealed bytes verbatim (the output of [`crate::seal_message`]).
#[uniffi::export]
pub fn encode_envelope_frame(sealed: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + sealed.len());
    out.push(FRAME_TYPE_ENVELOPE);
    out.extend_from_slice(&sealed);
    out
}

/// Parse a frame-type byte + body into a [`Frame`]. Rejects empty input, an
/// unrecognized frame-type byte, and a HELLO/envelope frame with no body.
#[uniffi::export]
pub fn parse_frame(bytes: Vec<u8>) -> Result<Frame, CoreError> {
    let (frame_type, rest) = bytes
        .split_first()
        .ok_or_else(|| CoreError::Malformed("empty frame: missing frame-type byte".to_string()))?;
    match *frame_type {
        FRAME_TYPE_HELLO => {
            if rest.is_empty() {
                return Err(CoreError::Malformed("HELLO frame missing user_id".to_string()));
            }
            Ok(Frame::Hello { user_id: rest.to_vec() })
        }
        FRAME_TYPE_ENVELOPE => {
            if rest.is_empty() {
                return Err(CoreError::Malformed(
                    "envelope frame missing sealed payload".to_string(),
                ));
            }
            Ok(Frame::Envelope { sealed: rest.to_vec() })
        }
        other => Err(CoreError::Malformed(format!("unknown frame type byte: 0x{other:02x}"))),
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
        let end = self.pos.checked_add(n).filter(|&end| end <= self.data.len());
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
        Ok(u16::from_be_bytes(self.take(2)?.try_into().expect("exactly 2 bytes")))
    }

    fn take_u32(&mut self) -> Result<u32, CoreError> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().expect("exactly 4 bytes")))
    }

    fn take_u64(&mut self) -> Result<u64, CoreError> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().expect("exactly 8 bytes")))
    }

    fn take_i64(&mut self) -> Result<i64, CoreError> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().expect("exactly 8 bytes")))
    }

    fn take_bytes16(&mut self) -> Result<Vec<u8>, CoreError> {
        let len = self.take_u16()? as usize;
        Ok(self.take(len)?.to_vec())
    }

    fn take_bytes32(&mut self) -> Result<Vec<u8>, CoreError> {
        let len = self.take_u32()? as usize;
        Ok(self.take(len)?.to_vec())
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
        matches!(err, CoreError::Malformed(_));
    }

    #[test]
    fn message_body_decode_rejects_truncated_chat_id() {
        // kind byte + chat_id_len claiming 10 bytes, but none follow.
        let bytes = vec![KIND_TEXT, 0x00, 0x0A];
        let err = decode_message_body(bytes).unwrap_err();
        matches!(err, CoreError::Malformed(_));
    }

    #[test]
    fn message_body_decode_rejects_truncated_before_timestamp() {
        let mut bytes = vec![KIND_TEXT, 0x00, 0x00]; // empty chat_id
        bytes.extend_from_slice(&1u64.to_be_bytes()); // full lamport
        bytes.push(0); // only 1 of 8 timestamp bytes
        let err = decode_message_body(bytes).unwrap_err();
        matches!(err, CoreError::Malformed(_));
    }

    #[test]
    fn message_body_decode_rejects_trailing_garbage() {
        let mut encoded = encode_message_body(sample_body());
        encoded.push(0xFF);
        let err = decode_message_body(encoded).unwrap_err();
        matches!(err, CoreError::Malformed(_));
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
        matches!(err, CoreError::Malformed(_));
    }

    #[test]
    fn receipt_content_decode_rejects_garbage() {
        let err = decode_receipt_content(vec![0xFF, 0xFF, 0xFF]).unwrap_err();
        matches!(err, CoreError::Malformed(_));
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

    #[test]
    fn envelope_frame_round_trips() {
        let sealed = vec![0x11, 0x22, 0x33, 0x44];
        let framed = encode_envelope_frame(sealed.clone());
        assert_eq!(framed[0], 0x02);
        match parse_frame(framed).expect("parses") {
            Frame::Envelope { sealed: got } => assert_eq!(got, sealed),
            other => panic!("expected Envelope, got {other:?}"),
        }
    }

    #[test]
    fn parse_frame_rejects_empty_input() {
        let err = parse_frame(Vec::new()).unwrap_err();
        matches!(err, CoreError::Malformed(_));
    }

    #[test]
    fn parse_frame_rejects_unknown_type_byte() {
        let err = parse_frame(vec![0x99, 0x01, 0x02]).unwrap_err();
        matches!(err, CoreError::Malformed(_));
    }

    #[test]
    fn parse_frame_rejects_hello_with_no_user_id() {
        let err = parse_frame(vec![0x01]).unwrap_err();
        matches!(err, CoreError::Malformed(_));
    }

    #[test]
    fn parse_frame_rejects_envelope_with_no_sealed_bytes() {
        let err = parse_frame(vec![0x02]).unwrap_err();
        matches!(err, CoreError::Malformed(_));
    }

    #[test]
    fn message_body_survives_seal_and_open_round_trip() {
        let alice = generate_identity();
        let bob = generate_identity();

        let body = sample_body();
        let payload = encode_message_body(body.clone());

        let sealed = seal_message(alice.clone(), bob.agree_pk.clone(), payload).expect("seal succeeds");
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

        let sealed = seal_message(alice.clone(), bob.agree_pk.clone(), payload).expect("seal succeeds");
        let opened = open_message(bob, sealed).expect("open succeeds");

        let decoded_body = decode_message_body(opened.payload).expect("decodes body");
        assert_eq!(decoded_body.kind, KIND_RECEIPT);
        let decoded_receipt =
            decode_receipt_content(decoded_body.content).expect("decodes receipt content");
        assert_eq!(decoded_receipt, receipt);
    }
}
