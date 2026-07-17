//! Application-level message content codecs shared by every client.

use crate::CoreError;

const ATTACHMENT_WIRE_VERSION: u8 = 1;
pub const ATTACHMENT_MAX_BLOB_BYTES: usize = 180 * 1024;
const REACTION_WIRE_VERSION: u8 = 1;
const REACTION_MAX_EMOJI_BYTES: usize = 32;

#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentMediaType {
    Image,
    Audio,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CoreAttachmentPayload {
    pub media_type: AttachmentMediaType,
    pub mime_type: String,
    pub duration_ms: i64,
    pub blob: Vec<u8>,
    pub caption: String,
}

#[uniffi::export]
pub fn attachment_max_blob_bytes() -> u32 {
    ATTACHMENT_MAX_BLOB_BYTES as u32
}

#[uniffi::export]
pub fn encode_attachment_payload(payload: CoreAttachmentPayload) -> Result<Vec<u8>, CoreError> {
    if payload.duration_ms < 0 || payload.duration_ms > u32::MAX as i64 {
        return Err(CoreError::Malformed("invalid attachment duration".into()));
    }
    if payload.blob.len() > u32::MAX as usize {
        return Err(CoreError::Malformed("attachment blob is too large".into()));
    }
    let mime = payload.mime_type.as_bytes();
    let caption = payload.caption.as_bytes();
    if mime.len() > u16::MAX as usize || caption.len() > u16::MAX as usize {
        return Err(CoreError::Malformed("attachment string is too long".into()));
    }

    let mut out = Vec::with_capacity(14 + mime.len() + payload.blob.len() + caption.len());
    out.push(ATTACHMENT_WIRE_VERSION);
    out.push(match payload.media_type {
        AttachmentMediaType::Image => 1,
        AttachmentMediaType::Audio => 2,
    });
    write_bytes16(&mut out, mime);
    out.extend_from_slice(&(payload.duration_ms as u32).to_be_bytes());
    out.extend_from_slice(&(payload.blob.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload.blob);
    write_bytes16(&mut out, caption);
    Ok(out)
}

#[uniffi::export]
pub fn decode_attachment_payload(bytes: Vec<u8>) -> Option<CoreAttachmentPayload> {
    let mut cursor = Cursor::new(&bytes);
    if cursor.read_u8()? != ATTACHMENT_WIRE_VERSION {
        return None;
    }
    let media_type = match cursor.read_u8()? {
        1 => AttachmentMediaType::Image,
        2 => AttachmentMediaType::Audio,
        _ => return None,
    };
    let mime_type = cursor.read_string16(None)?;
    let duration_ms = cursor.read_u32()? as i64;
    let blob_len = cursor.read_u32()? as usize;
    if blob_len > ATTACHMENT_MAX_BLOB_BYTES * 2 {
        return None;
    }
    let blob = cursor.read_exact(blob_len)?.to_vec();
    let caption = cursor.read_string16(None)?;
    // Attachment v1 intentionally permits trailing extension bytes.
    Some(CoreAttachmentPayload {
        media_type,
        mime_type,
        duration_ms,
        blob,
        caption,
    })
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreMessageTarget {
    pub sender_user_id: Vec<u8>,
    pub lamport: u64,
    pub kind: u8,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreReactionPayload {
    pub target: CoreMessageTarget,
    pub emoji: String,
}

#[uniffi::export]
pub fn encode_reaction_payload(payload: CoreReactionPayload) -> Result<Vec<u8>, CoreError> {
    let sender = &payload.target.sender_user_id;
    let emoji = payload.emoji.as_bytes();
    if sender.len() > u16::MAX as usize {
        return Err(CoreError::Malformed(
            "reaction sender id is too long".into(),
        ));
    }
    if emoji.len() > REACTION_MAX_EMOJI_BYTES {
        return Err(CoreError::Malformed("reaction emoji is too long".into()));
    }
    let mut out = Vec::with_capacity(14 + sender.len() + emoji.len());
    out.push(REACTION_WIRE_VERSION);
    write_bytes16(&mut out, sender);
    out.extend_from_slice(&payload.target.lamport.to_be_bytes());
    out.push(payload.target.kind);
    write_bytes16(&mut out, emoji);
    Ok(out)
}

#[uniffi::export]
pub fn decode_reaction_payload(bytes: Vec<u8>) -> Option<CoreReactionPayload> {
    let mut cursor = Cursor::new(&bytes);
    if cursor.read_u8()? != REACTION_WIRE_VERSION {
        return None;
    }
    let sender_user_id = cursor.read_bytes16()?.to_vec();
    let lamport = cursor.read_u64()?;
    let kind = cursor.read_u8()?;
    let emoji = cursor.read_string16(Some(REACTION_MAX_EMOJI_BYTES))?;
    if !cursor.is_finished() {
        return None;
    }
    Some(CoreReactionPayload {
        target: CoreMessageTarget {
            sender_user_id,
            lamport,
            kind,
        },
        emoji,
    })
}

fn write_bytes16(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read_exact(&mut self, count: usize) -> Option<&'a [u8]> {
        let end = self.offset.checked_add(count)?;
        let result = self.bytes.get(self.offset..end)?;
        self.offset = end;
        Some(result)
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

    fn read_bytes16(&mut self) -> Option<&'a [u8]> {
        let count = self.read_u16()? as usize;
        self.read_exact(count)
    }

    fn read_string16(&mut self, max_bytes: Option<usize>) -> Option<String> {
        let bytes = self.read_bytes16()?;
        if max_bytes.is_some_and(|max| bytes.len() > max) {
            return None;
        }
        String::from_utf8(bytes.to_vec()).ok()
    }

    fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attachment_round_trip_and_exact_layout() {
        let payload = CoreAttachmentPayload {
            media_type: AttachmentMediaType::Audio,
            mime_type: "audio/mp4".into(),
            duration_ms: 1_234,
            blob: vec![1, 2, 3],
            caption: "memo".into(),
        };
        let encoded = encode_attachment_payload(payload.clone()).unwrap();
        assert_eq!(encoded[0..2], [1, 2]);
        assert_eq!(decode_attachment_payload(encoded), Some(payload));
    }

    #[test]
    fn attachment_rejects_invalid_or_oversized_input() {
        assert!(decode_attachment_payload(vec![]).is_none());
        let payload = CoreAttachmentPayload {
            media_type: AttachmentMediaType::Image,
            mime_type: "image/jpeg".into(),
            duration_ms: -1,
            blob: vec![],
            caption: String::new(),
        };
        assert!(encode_attachment_payload(payload).is_err());
    }

    #[test]
    fn reaction_round_trip_and_removal_value() {
        for emoji in ["👍", ""] {
            let payload = CoreReactionPayload {
                target: CoreMessageTarget {
                    sender_user_id: vec![4; 16],
                    lamport: 42,
                    kind: 1,
                },
                emoji: emoji.into(),
            };
            let encoded = encode_reaction_payload(payload.clone()).unwrap();
            assert_eq!(decode_reaction_payload(encoded), Some(payload));
        }
    }

    #[test]
    fn reaction_rejects_trailing_and_overlong_data() {
        let payload = CoreReactionPayload {
            target: CoreMessageTarget {
                sender_user_id: vec![1; 16],
                lamport: 1,
                kind: 1,
            },
            emoji: "👍".into(),
        };
        let mut encoded = encode_reaction_payload(payload).unwrap();
        encoded.push(0);
        assert!(decode_reaction_payload(encoded).is_none());
        let too_long = CoreReactionPayload {
            target: CoreMessageTarget {
                sender_user_id: vec![],
                lamport: 0,
                kind: 0,
            },
            emoji: "x".repeat(33),
        };
        assert!(encode_reaction_payload(too_long).is_err());
    }
}
