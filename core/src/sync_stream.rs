//! SYNC-1 anti-entropy: per-stream watermark digests and gap-fill planning
//! (`specs/multi-device-v1.md` §8).
//!
//! SYNC-1 says devices exchange compact digests (per-stream watermarks) and
//! fill gaps, that the existing digest-based patterns generalize, and that **no
//! operation may assume both devices are ever concurrently online**. This
//! module is the arithmetic half of that sentence: it holds no state, does no
//! I/O, and never asks who is reachable. What it produces is a document
//! (`SyncDigest`) and a decision (`SyncBackfillPlan`); persisting either, and
//! putting the first on a wire, belong to `sync_store.rs` and to the driver.
//!
//! ## Why this generalizes the chat digest rather than replacing it
//!
//! [`crate::DigestEntry`] answers "I have this *person's* messages in this chat
//! contiguously through lamport N" — the shape DESIGN.md §7.3 shipped and
//! [`crate::MessageStore::chat_digest`] still produces, unchanged, for
//! contacts. [`SyncStreamDigest`] answers the same question one axis over: "I
//! have this *device's* records of this kind contiguously through stream_seq
//! N". Both are a watermark over a gap-free prefix, both are answered by
//! sending what lies above it, and both are safe to exchange in either
//! direction at any time, because neither says anything about liveness. The
//! difference is only the stream key, which §5 widened by a device dimension
//! and §8 then reuses for sync records.
//!
//! Contiguity, not a plain maximum, is what makes the exchange terminate
//! correctly: a device that received records 1, 2 and 7 advertises 2, is sent
//! 3..=7, and converges. A device that advertised 7 would be told nothing and
//! would keep its hole forever — the same trap
//! [`crate::MessageStore::highest_contiguous_lamport`] documents at length for
//! chat messages, and the reason its per-device generalization was left to this
//! work package rather than guessed at earlier.
//!
//! ## What a digest may say
//!
//! A sync digest names device ids and stream positions — the shape of a
//! person's fleet. §2's first goal is that a person's device count is invisible
//! to other people, so a digest is not a public frame: it is sealed to the
//! person's own inbox key (§6) and travels as ordinary 1:1 traffic, exactly as
//! the records it describes do. Nothing in this module encodes an endpoint,
//! a hostname, or a relay address, which is DL-5 held by construction rather
//! than by review.

use crate::device_roster::{RosterVersion, DEVICE_ID_LEN};
use crate::sync_record::{core_sync_kind_is_stream, core_sync_record_kind_of};
use crate::CoreError;

/// Leading byte of an encoded [`SyncDigest`]. Its own version, independent of
/// the record version, because a digest and a record are exchanged by
/// different halves of a round and may need to move separately.
const SYNC_DIGEST_VERSION: u8 = 1;

/// The most streams one digest may describe.
///
/// A stream is one `(device, record kind)` pair, so this is
/// `DEVICE_HARD_CAP × 6` with room to spare for the tombstoned devices whose
/// records a fleet still legitimately holds. It is a decode bound first —
/// a malformed count must never make a reader allocate — and a reminder
/// second that a digest is meant to stay small enough to ride a BLE frame
/// beside everything else an encounter owes.
pub const SYNC_DIGEST_MAX_STREAMS: usize = 512;

// ---------------------------------------------------------------------------
// The digest
// ---------------------------------------------------------------------------

/// One stream's watermark: "I hold this device's records of this kind
/// contiguously through `through_seq`", plus whether I can actually hand them
/// over.
///
/// `through_seq` is 0 for a stream this device knows exists but holds nothing
/// of, which is the honest thing to advertise for a sibling that has just been
/// linked: it asks for the whole stream from 1 without needing a separate
/// "I have nothing" signal.
///
/// `kind` is the **wire byte**, not [`crate::SyncRecordKind`], and the
/// difference is deliberate. A digest is a claim about what a database
/// contains, and a build that could only name the kinds it understands would
/// have to omit a stream a *newer* sibling's build wrote into this database
/// (a downgrade, a restore from a newer `.cmbak`) — and an omitted stream reads
/// to that sibling as "send me all of it", every round, forever. Carrying the
/// raw byte lets a stream be advertised at its cursor whether or not this build
/// can parse a record of it, which is the honest answer and the one that
/// terminates.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncStreamDigest {
    pub author_device_id: Vec<u8>,
    /// The sealed-body kind byte (`protocol.rs`'s `KIND_SYNC_*`).
    pub kind: u8,
    pub through_seq: u64,
    /// Whether this device can *serve* records of this stream — i.e. holds
    /// their sealed bytes, which only their author does (SYNC-3: only the
    /// author can re-seal after a roster change).
    ///
    /// Advertising the watermark and advertising the ability to answer for it
    /// are two different claims, and collapsing them breaks anti-entropy in one
    /// direction or the other. A device that dropped a stream from its digest
    /// because it cannot serve it would be telling the stream's author "I have
    /// none of it", and would be sent the whole stream again on every single
    /// encounter. A device that claimed it could serve what it only holds
    /// positions for makes every sibling plan a gap-fill against it that comes
    /// back empty, round after round. So both facts travel, and
    /// [`core_sync_digest_gaps`] reads the second one.
    pub can_serve: bool,
}

/// One device's whole view of one person's sync streams (SYNC-1).
///
/// `person_id` is carried so a receiver can refuse a digest that wandered in
/// from another person rather than silently merging it — the same reason
/// [`crate::encode_digest`] carries its sender's id.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncDigest {
    pub person_id: Vec<u8>,
    pub streams: Vec<SyncStreamDigest>,
}

/// Encode a digest to its canonical bytes.
///
/// Layout: `version(1)=1 | person_id_len(u16) | person_id | stream_count(u16)`,
/// then per stream `author_device_id(16) | kind(1) | flags(1) |
/// through_seq(u64)`, where `flags` bit 0 is [`SyncStreamDigest::can_serve`]
/// and every other bit is reserved and must be zero.
///
/// Streams are sorted by `(author_device_id, kind)` and a repeated stream is an
/// error, so one view encodes to exactly one byte string. That matters more
/// here than it does for a chat digest: a sync digest is re-sent on every
/// encounter, and two encodings of one unchanged view would defeat any
/// dedupe a transport applies to it.
#[uniffi::export]
pub fn core_encode_sync_digest(digest: SyncDigest) -> Result<Vec<u8>, CoreError> {
    if digest.person_id.len() > u16::MAX as usize {
        return Err(CoreError::Malformed(
            "sync digest person id is too long".to_string(),
        ));
    }
    if digest.streams.len() > SYNC_DIGEST_MAX_STREAMS {
        return Err(CoreError::Malformed(format!(
            "sync digest names {} streams, over the {SYNC_DIGEST_MAX_STREAMS} limit",
            digest.streams.len()
        )));
    }
    let mut streams = digest.streams;
    for stream in &streams {
        if stream.author_device_id.len() != DEVICE_ID_LEN {
            return Err(CoreError::Malformed(format!(
                "sync digest device id must be {DEVICE_ID_LEN} bytes"
            )));
        }
    }
    streams.sort_by(|a, b| {
        a.author_device_id
            .cmp(&b.author_device_id)
            .then(a.kind.cmp(&b.kind))
    });
    if streams.windows(2).any(|pair| {
        pair[0].author_device_id == pair[1].author_device_id && pair[0].kind == pair[1].kind
    }) {
        return Err(CoreError::Malformed(
            "sync digest names one stream twice".to_string(),
        ));
    }

    let mut out = vec![SYNC_DIGEST_VERSION];
    out.extend_from_slice(&(digest.person_id.len() as u16).to_be_bytes());
    out.extend_from_slice(&digest.person_id);
    out.extend_from_slice(&(streams.len() as u16).to_be_bytes());
    for stream in &streams {
        out.extend_from_slice(&stream.author_device_id);
        out.push(stream.kind);
        out.push(if stream.can_serve {
            SYNC_STREAM_FLAG_CAN_SERVE
        } else {
            0
        });
        out.extend_from_slice(&stream.through_seq.to_be_bytes());
    }
    Ok(out)
}

/// `flags` bit 0: this device holds the sealed bytes of the stream and can
/// answer a gap-fill for it.
const SYNC_STREAM_FLAG_CAN_SERVE: u8 = 0x01;

/// Decode a digest. Fully bounds-checked; trailing bytes, an unknown version
/// and an unrecognized flag bit are errors rather than partial reads.
///
/// An unknown *kind* is deliberately **not** an error, and this is the one
/// judgement call in the codec. It used to be: a reader that silently dropped a
/// stream it could not name would answer a newer sibling with a watermark that
/// omitted it, which reads as "I have everything you offered" and loses records
/// permanently. Carrying the raw kind byte (see [`SyncStreamDigest::kind`])
/// removes the choice — the stream is preserved with its watermark intact, and
/// the decision about what to *do* with a kind this build cannot parse is
/// [`core_sync_digest_gaps`]'s, where it is one line and testable.
///
/// An unrecognized flag bit still fails closed, for the reason the kind byte no
/// longer needs to: a flag changes the *meaning* of the fields beside it, so a
/// reader that ignored one would act on a claim it had misread.
#[uniffi::export]
pub fn core_decode_sync_digest(bytes: Vec<u8>) -> Result<SyncDigest, CoreError> {
    let mut cursor = DigestCursor::new(&bytes);
    let version = cursor.take_u8()?;
    if version != SYNC_DIGEST_VERSION {
        return Err(CoreError::Malformed(format!(
            "unsupported sync digest version {version}"
        )));
    }
    let person_len = cursor.take_u16()? as usize;
    let person_id = cursor.take(person_len)?.to_vec();
    let count = cursor.take_u16()? as usize;
    if count > SYNC_DIGEST_MAX_STREAMS {
        return Err(CoreError::Malformed(format!(
            "sync digest names {count} streams, over the {SYNC_DIGEST_MAX_STREAMS} limit"
        )));
    }
    let mut streams = Vec::with_capacity(count);
    for _ in 0..count {
        let author_device_id = cursor.take(DEVICE_ID_LEN)?.to_vec();
        let kind = cursor.take_u8()?;
        let flags = cursor.take_u8()?;
        if flags & !SYNC_STREAM_FLAG_CAN_SERVE != 0 {
            return Err(CoreError::Malformed(format!(
                "sync digest stream carries unknown flag bits {flags:#04x}"
            )));
        }
        streams.push(SyncStreamDigest {
            author_device_id,
            kind,
            through_seq: cursor.take_u64()?,
            can_serve: flags & SYNC_STREAM_FLAG_CAN_SERVE != 0,
        });
    }
    cursor.finish()?;
    Ok(SyncDigest { person_id, streams })
}

// ---------------------------------------------------------------------------
// Gap detection
// ---------------------------------------------------------------------------

/// A run of one stream that one side holds and the other does not.
///
/// Half-open on the low side and inclusive on the high: the missing records are
/// `after_seq + 1 ..= through_seq`. That is the same shape
/// [`crate::MessageStore::messages_after`] takes, and for the same reason —
/// the requester states the watermark it can prove, never the list of ids it
/// lacks, so a gap of ten thousand records costs the same bytes as a gap of
/// one.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncGap {
    pub author_device_id: Vec<u8>,
    /// The sealed-body kind byte. Always one this build can name — see
    /// [`core_sync_digest_gaps`] for why an unnameable kind never becomes a gap.
    pub kind: u8,
    /// The watermark the side that lacks these records can prove.
    pub after_seq: u64,
    /// The watermark the side that holds them advertised.
    pub through_seq: u64,
}

/// The streams `peer` advertises beyond what `have` does (SYNC-1).
///
/// Deliberately one function used in both directions, because anti-entropy is
/// symmetric and a second copy would be a second thing to get wrong:
///
/// * `core_sync_digest_gaps(mine, theirs)` — what **I need**, to request.
/// * `core_sync_digest_gaps(theirs, mine)` — what **I owe**, to send.
///
/// The second form is the one that makes SYNC-1's standing constraint hold. A
/// device that has just read a sibling's digest can compute everything it owes
/// and hand it to a mule, a relay row, or a carry queue; nothing about the
/// answer depends on the sibling still being there, and nothing about it
/// changes if the sibling is unreachable for a week.
///
/// A stream `have` knows nothing about is a gap from 0, so a freshly linked
/// device asks for whole streams without a distinct "send me everything"
/// message. A stream `have` is *ahead* on produces no gap: the reverse call is
/// where that difference is answered, which is also why answering a digest
/// never obliges the answerer to ask for one back.
///
/// Two peer streams are deliberately skipped, and both skips exist to stop a
/// round that can never do anything from being planned on every encounter for
/// the rest of the fleet's life:
///
/// * **`can_serve == false`** — the peer holds this stream's *positions* but
///   not its bytes, which is every stream it did not author. Only an author can
///   re-seal its own records (SYNC-3), so a gap addressed at a non-author is a
///   plan that comes back empty. The records are still coming: their author
///   answers for them on its own next encounter.
/// * **a kind this build cannot name** — the downgrade case
///   [`SyncStreamDigest::kind`] describes. Requesting records this build's
///   decoder refuses would spend the round's budget on bytes that cannot be
///   applied, leaving the watermark exactly where it was, so the same gap would
///   be requested again next round, forever. Not asking is the terminating
///   answer; the *advertising* side still names the stream, which is what stops
///   the newer sibling re-offering it.
///
/// Neither skip loses anything: a stream is only unreachable through one peer,
/// never through its author.
#[uniffi::export]
pub fn core_sync_digest_gaps(
    have: SyncDigest,
    peer: SyncDigest,
) -> Result<Vec<SyncGap>, CoreError> {
    if have.person_id != peer.person_id {
        return Err(CoreError::Malformed(
            "sync digests describe two different people".to_string(),
        ));
    }
    let mut gaps = Vec::new();
    for stream in &peer.streams {
        if !stream.can_serve {
            continue;
        }
        let Some(kind) = core_sync_record_kind_of(stream.kind) else {
            continue;
        };
        if !core_sync_kind_is_stream(kind) {
            // A digest is not gap-filled — it is the thing that asks. Nothing
            // ever advertises one, and a peer that did is answered with silence
            // rather than with a backfill of last week's watermarks.
            continue;
        }
        let held = have
            .streams
            .iter()
            .find(|mine| {
                mine.author_device_id == stream.author_device_id && mine.kind == stream.kind
            })
            .map(|mine| mine.through_seq)
            .unwrap_or(0);
        if stream.through_seq > held {
            gaps.push(SyncGap {
                author_device_id: stream.author_device_id.clone(),
                kind: stream.kind,
                after_seq: held,
                through_seq: stream.through_seq,
            });
        }
    }
    gaps.sort_by(|a, b| {
        a.author_device_id
            .cmp(&b.author_device_id)
            .then(a.kind.cmp(&b.kind))
    });
    Ok(gaps)
}

// ---------------------------------------------------------------------------
// Backfill planning
// ---------------------------------------------------------------------------

/// One stored record this device could send, described by everything the
/// planner needs and nothing it does not.
///
/// The sealed bytes are deliberately absent: a plan is computed over hundreds
/// of candidates and copying half a megabyte per candidate to decide against it
/// would be the expensive part of a cheap decision. The caller holds the rows;
/// the plan hands back indices into the list it passed.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncBackfillOffer {
    pub author_device_id: Vec<u8>,
    /// The sealed-body kind byte, matching [`SyncGap::kind`].
    pub kind: u8,
    pub stream_seq: u64,
    /// The own-roster version these bytes were sealed for (SYNC-3).
    pub sealed_for: RosterVersion,
    /// The inbox key generation these bytes were sealed under (§6, §10.1). The
    /// other half of "may these bytes still be sent" — see
    /// [`crate::core_sync_seal_is_current`].
    pub inbox_key_generation: u64,
    /// Sealed size, charged against the round's budget.
    pub byte_len: u64,
}

/// What the caller must do with an offer before it can go out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SyncBackfillAction {
    /// The stored seal is still current — send the bytes as they are.
    Send,
    /// SYNC-3: the own roster has moved since these bytes were sealed, so the
    /// record must be re-sealed to the current device set before it is sent.
    /// The re-seal keeps the record's stream slot and therefore its
    /// [`crate::core_sync_record_id`], so it costs no extra relay row.
    Reseal,
}

/// One planned record, in send order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncBackfillStep {
    /// Index into the `offers` list the plan was computed over.
    pub offer_index: u32,
    pub action: SyncBackfillAction,
}

/// One round's answer to a sibling's digest.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct SyncBackfillPlan {
    /// Records to send, **in this order**. The order is load-bearing: a
    /// receiver's watermark only advances across a gap-free prefix, so a round
    /// that is cut short mid-stream still moves the sibling forward if and only
    /// if what was sent was the oldest missing run.
    pub steps: Vec<SyncBackfillStep>,
    /// Records that answer a real gap but did not fit this round. Not a
    /// failure and not a retry list: the sibling's next digest will ask for
    /// them again from whatever watermark this round actually achieved.
    pub deferred: Vec<u32>,
    /// Sealed bytes the steps add up to, for a caller pacing several links.
    pub planned_bytes: u64,
}

/// Plan one round of gap-fill (SYNC-1).
///
/// `gaps` is what the peer is missing — [`core_sync_digest_gaps`] called with
/// the peer's digest as `have`. `offers` is what this device can actually
/// produce; anything in `offers` that answers no gap is simply not planned.
///
/// Three rules, in this order:
///
/// 1. **Oldest first, per stream.** Steps come out sorted by
///    `(author_device_id, kind, stream_seq)`, so the prefix a truncated round
///    delivers is the prefix that advances a watermark. Sending the newest
///    records first would move the same bytes and converge nothing.
/// 2. **A truncated stream stays truncated.** Once one record of a stream is
///    deferred, every later record of *that* stream is deferred too — sending
///    them would put records above a hole the receiver cannot close, which
///    costs the budget and advances no watermark. Other streams keep being
///    planned, because a hole in one stream says nothing about another.
/// 3. **Stale seals are re-sealed, not skipped.** SYNC-3 requires a record
///    sealed under a superseded roster to be re-sealed rather than sent, and a
///    planner that dropped it instead would strand exactly the records a
///    just-linked device most needs.
///
/// `budget_bytes` of 0 plans nothing and defers everything, which is a
/// legitimate answer for a link with no room left this encounter.
///
/// **A head record larger than the whole budget stalls its stream, and that is
/// correct.** Rule 2 defers it and everything above it, so the round moves other
/// streams instead of spending its budget on a record that advances nothing.
/// The caller's escape is a larger budget on the next encounter, not a partial
/// record: a sealed record is atomic, and half of one is not a thing a sibling
/// can open.
#[uniffi::export]
pub fn core_plan_sync_backfill(
    gaps: Vec<SyncGap>,
    offers: Vec<SyncBackfillOffer>,
    current_roster: RosterVersion,
    current_inbox_key_generation: u64,
    budget_bytes: u64,
) -> SyncBackfillPlan {
    let mut matched: Vec<(usize, &SyncBackfillOffer)> = offers
        .iter()
        .enumerate()
        .filter(|(_, offer)| {
            gaps.iter().any(|gap| {
                gap.author_device_id == offer.author_device_id
                    && gap.kind == offer.kind
                    && offer.stream_seq > gap.after_seq
                    && offer.stream_seq <= gap.through_seq
            })
        })
        .collect();
    matched.sort_by(|(_, a), (_, b)| {
        a.author_device_id
            .cmp(&b.author_device_id)
            .then(a.kind.cmp(&b.kind))
            .then(a.stream_seq.cmp(&b.stream_seq))
    });

    let mut steps = Vec::new();
    let mut deferred = Vec::new();
    let mut planned_bytes: u64 = 0;
    // Rule 2's memory: the streams that have already been cut this round.
    let mut truncated: Vec<(Vec<u8>, u8)> = Vec::new();
    for (index, offer) in matched {
        let stream = (offer.author_device_id.clone(), offer.kind);
        let already_cut = truncated.contains(&stream);
        let next_bytes = planned_bytes.saturating_add(offer.byte_len);
        if already_cut || next_bytes > budget_bytes {
            if !already_cut {
                truncated.push(stream);
            }
            deferred.push(index as u32);
            continue;
        }
        planned_bytes = next_bytes;
        steps.push(SyncBackfillStep {
            offer_index: index as u32,
            action: if crate::core_sync_seal_is_current(
                offer.sealed_for,
                offer.inbox_key_generation,
                current_roster,
                current_inbox_key_generation,
            ) {
                SyncBackfillAction::Send
            } else {
                SyncBackfillAction::Reseal
            },
        });
    }
    SyncBackfillPlan {
        steps,
        deferred,
        planned_bytes,
    }
}

// ---------------------------------------------------------------------------
// Codec cursor
// ---------------------------------------------------------------------------

/// A bounds-checked reader. A private twin of `sync_record.rs`'s cursor rather
/// than a shared one: the two codecs are frozen independently, and a helper
/// that both depend on is a helper that can change one of them by accident.
struct DigestCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> DigestCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        DigestCursor { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CoreError> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| CoreError::Malformed("sync digest length overflows".to_string()))?;
        if end > self.bytes.len() {
            return Err(CoreError::Malformed(
                "sync digest ended mid-field".to_string(),
            ));
        }
        let out = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(out)
    }

    fn take_u8(&mut self) -> Result<u8, CoreError> {
        Ok(self.take(1)?[0])
    }

    fn take_u16(&mut self) -> Result<u16, CoreError> {
        let bytes = self.take(2)?;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    fn take_u64(&mut self) -> Result<u64, CoreError> {
        let bytes = self.take(8)?;
        let mut out = [0u8; 8];
        out.copy_from_slice(bytes);
        Ok(u64::from_be_bytes(out))
    }

    fn finish(&self) -> Result<(), CoreError> {
        if self.offset == self.bytes.len() {
            Ok(())
        } else {
            Err(CoreError::Malformed(
                "sync digest has trailing bytes".to_string(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_record::{core_sync_record_kind_wire, SyncRecordKind};

    fn device(byte: u8) -> Vec<u8> {
        vec![byte; DEVICE_ID_LEN]
    }

    /// A stream this device authored, and can therefore answer for.
    fn stream(byte: u8, kind: SyncRecordKind, through_seq: u64) -> SyncStreamDigest {
        SyncStreamDigest {
            author_device_id: device(byte),
            kind: core_sync_record_kind_wire(kind),
            through_seq,
            can_serve: true,
        }
    }

    /// A stream this device holds *positions* for but did not author: it knows
    /// where it is, and cannot hand any of it over.
    fn positions_only(byte: u8, kind: SyncRecordKind, through_seq: u64) -> SyncStreamDigest {
        SyncStreamDigest {
            can_serve: false,
            ..stream(byte, kind, through_seq)
        }
    }

    fn digest(streams: Vec<SyncStreamDigest>) -> SyncDigest {
        SyncDigest {
            person_id: b"person".to_vec(),
            streams,
        }
    }

    fn offer(byte: u8, kind: SyncRecordKind, stream_seq: u64, byte_len: u64) -> SyncBackfillOffer {
        SyncBackfillOffer {
            author_device_id: device(byte),
            kind: core_sync_record_kind_wire(kind),
            stream_seq,
            sealed_for: RosterVersion {
                recovery_epoch: 0,
                seq: 1,
            },
            inbox_key_generation: GENERATION,
            byte_len,
        }
    }

    const CURRENT: RosterVersion = RosterVersion {
        recovery_epoch: 0,
        seq: 1,
    };
    const GENERATION: u64 = 3;

    #[test]
    fn a_digest_round_trips_and_is_canonical_whatever_order_it_was_built_in() {
        let ordered = digest(vec![
            stream(0x11, SyncRecordKind::History, 4),
            stream(0x11, SyncRecordKind::Contacts, 1),
            stream(0x22, SyncRecordKind::History, 9),
        ]);
        let shuffled = digest(vec![
            stream(0x22, SyncRecordKind::History, 9),
            stream(0x11, SyncRecordKind::Contacts, 1),
            stream(0x11, SyncRecordKind::History, 4),
        ]);
        let bytes = core_encode_sync_digest(ordered.clone()).expect("encode");
        assert_eq!(
            bytes,
            core_encode_sync_digest(shuffled).expect("encode"),
            "one view of the streams encodes to exactly one byte string"
        );
        let decoded = core_decode_sync_digest(bytes).expect("decode");
        assert_eq!(decoded.person_id, ordered.person_id);
        assert_eq!(decoded.streams.len(), 3);
        // Sorted by (device, kind): History = 10 sorts before Contacts = 12.
        assert_eq!(decoded.streams[0], stream(0x11, SyncRecordKind::History, 4));
        assert_eq!(
            decoded.streams[1],
            stream(0x11, SyncRecordKind::Contacts, 1)
        );
        assert_eq!(decoded.streams[2], stream(0x22, SyncRecordKind::History, 9));
    }

    #[test]
    fn the_serve_flag_survives_the_round_trip() {
        let bytes = core_encode_sync_digest(digest(vec![
            positions_only(0x11, SyncRecordKind::History, 4),
            stream(0x22, SyncRecordKind::History, 9),
        ]))
        .expect("encode");
        let decoded = core_decode_sync_digest(bytes).expect("decode");
        assert!(!decoded.streams[0].can_serve);
        assert!(decoded.streams[1].can_serve);
    }

    #[test]
    fn digest_bytes_are_frozen() {
        // A golden vector in the style of `sync_record.rs`'s: the digest is
        // exchanged between builds that may ship months apart, so a framing
        // change has to fail here rather than in a family's living room.
        let bytes = core_encode_sync_digest(SyncDigest {
            person_id: vec![0xAA, 0xBB],
            streams: vec![stream(0x01, SyncRecordKind::Watermarks, 258)],
        })
        .expect("encode");
        assert_eq!(
            hex(&bytes),
            // version | person | count | device(16) | kind=0x0b | flags=0x01 |
            // through_seq
            "010002aabb0001010101010101010101010101010101010b010000000000000102"
        );
    }

    #[test]
    fn a_repeated_stream_is_refused_rather_than_merged() {
        let err = core_encode_sync_digest(digest(vec![
            stream(0x11, SyncRecordKind::History, 4),
            stream(0x11, SyncRecordKind::History, 9),
        ]))
        .expect_err("a stream named twice has no single watermark");
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn decoding_refuses_trailing_bytes_and_unknown_flags_but_keeps_an_unknown_kind() {
        let bytes = core_encode_sync_digest(digest(vec![stream(0x11, SyncRecordKind::History, 4)]))
            .expect("encode");
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(core_decode_sync_digest(trailing).is_err());

        // The kind byte sits after the version, person frame and count, then
        // the 16-byte device id; the flags byte follows it.
        let kind_at = 1 + 2 + 6 + 2 + DEVICE_ID_LEN;
        let mut unknown_kind = bytes.clone();
        unknown_kind[kind_at] = 99;
        let decoded = core_decode_sync_digest(unknown_kind).expect(
            "a stream this build cannot name is still a stream this build holds, and dropping it \
             would read to a newer sibling as 'send me all of it', every round",
        );
        assert_eq!(decoded.streams[0].kind, 99);
        assert_eq!(decoded.streams[0].through_seq, 4);

        let mut unknown_flag = bytes;
        unknown_flag[kind_at + 1] = 0x02;
        assert!(
            core_decode_sync_digest(unknown_flag).is_err(),
            "a flag changes what the fields beside it mean, so an unread one is \
             a misread claim"
        );
    }

    #[test]
    fn gaps_are_what_the_peer_has_beyond_us_in_either_direction() {
        let mine = digest(vec![
            stream(0x11, SyncRecordKind::History, 4),
            stream(0x22, SyncRecordKind::History, 9),
        ]);
        let theirs = digest(vec![
            stream(0x11, SyncRecordKind::History, 7),
            stream(0x22, SyncRecordKind::History, 2),
            stream(0x33, SyncRecordKind::Settings, 3),
        ]);

        let need = core_sync_digest_gaps(mine.clone(), theirs.clone()).expect("gaps");
        assert_eq!(
            need,
            vec![
                SyncGap {
                    author_device_id: device(0x11),
                    kind: core_sync_record_kind_wire(SyncRecordKind::History),
                    after_seq: 4,
                    through_seq: 7,
                },
                SyncGap {
                    author_device_id: device(0x33),
                    kind: core_sync_record_kind_wire(SyncRecordKind::Settings),
                    // A stream we have never heard of is a gap from zero, so a
                    // freshly linked device needs no separate "send me
                    // everything".
                    after_seq: 0,
                    through_seq: 3,
                },
            ]
        );

        let owed = core_sync_digest_gaps(theirs, mine).expect("gaps");
        assert_eq!(
            owed,
            vec![SyncGap {
                author_device_id: device(0x22),
                kind: core_sync_record_kind_wire(SyncRecordKind::History),
                after_seq: 2,
                through_seq: 9,
            }],
            "the same function answers what we owe; nothing here provokes a \
             digest back"
        );
    }

    /// The churn this flag exists to stop: a third device holding a sibling's
    /// stream positions is asked for those records on every encounter forever,
    /// and answers every one of them with an empty plan, because only the
    /// author holds bytes it could re-seal.
    #[test]
    fn a_stream_a_peer_only_holds_positions_for_is_never_requested_from_it() {
        let mine = digest(vec![]);
        let relay_device = digest(vec![
            positions_only(0x11, SyncRecordKind::History, 9),
            stream(0x33, SyncRecordKind::Settings, 2),
        ]);
        assert_eq!(
            core_sync_digest_gaps(mine, relay_device).expect("gaps"),
            vec![SyncGap {
                author_device_id: device(0x33),
                kind: core_sync_record_kind_wire(SyncRecordKind::Settings),
                after_seq: 0,
                through_seq: 2,
            }],
            "device 0x11's records come from device 0x11, not from whoever \
             happens to have applied them"
        );
    }

    /// The author still sees the positions-only advertisement, which is the
    /// half that must NOT be dropped: it is the whole reason a device stops
    /// re-sending a stream a sibling already has.
    #[test]
    fn positions_only_still_tells_the_author_it_has_nothing_to_send() {
        let author = digest(vec![stream(0x11, SyncRecordKind::History, 9)]);
        let holder = digest(vec![positions_only(0x11, SyncRecordKind::History, 9)]);
        assert!(
            core_sync_digest_gaps(holder, author)
                .expect("gaps")
                .is_empty(),
            "the holder proved it has the records; the author owes nothing"
        );
    }

    /// A kind this build cannot parse is advertised (so the newer sibling stops
    /// re-offering it) and never requested (so the round does not spend its
    /// budget on bytes the decoder will refuse).
    #[test]
    fn an_unnameable_kind_is_advertised_but_never_requested() {
        let mine = digest(vec![]);
        let newer_sibling = SyncDigest {
            person_id: b"person".to_vec(),
            streams: vec![
                SyncStreamDigest {
                    author_device_id: device(0x11),
                    kind: 250,
                    through_seq: 4,
                    can_serve: true,
                },
                stream(0x11, SyncRecordKind::History, 2),
            ],
        };
        assert_eq!(
            core_sync_digest_gaps(mine, newer_sibling.clone()).expect("gaps"),
            vec![SyncGap {
                author_device_id: device(0x11),
                kind: core_sync_record_kind_wire(SyncRecordKind::History),
                after_seq: 0,
                through_seq: 2,
            }]
        );

        // And the reverse direction: holding the unnameable stream at its
        // cursor is what stops the sibling re-sending it.
        let holder = SyncDigest {
            person_id: b"person".to_vec(),
            streams: vec![
                SyncStreamDigest {
                    author_device_id: device(0x11),
                    kind: 250,
                    through_seq: 4,
                    can_serve: false,
                },
                positions_only(0x11, SyncRecordKind::History, 2),
            ],
        };
        assert!(core_sync_digest_gaps(holder, newer_sibling)
            .expect("gaps")
            .is_empty());
    }

    /// A digest describes streams; it is not one. Nothing advertises it, and a
    /// peer that did is answered with silence rather than with a backfill of
    /// last week's watermarks.
    #[test]
    fn a_digest_stream_is_never_gap_filled() {
        let mine = digest(vec![]);
        let odd_peer = digest(vec![stream(0x11, SyncRecordKind::Digest, 40)]);
        assert!(core_sync_digest_gaps(mine, odd_peer)
            .expect("gaps")
            .is_empty());
    }

    #[test]
    fn two_peoples_digests_never_merge() {
        let mine = digest(vec![]);
        let theirs = SyncDigest {
            person_id: b"someone else".to_vec(),
            streams: vec![stream(0x11, SyncRecordKind::History, 1)],
        };
        assert!(core_sync_digest_gaps(mine, theirs).is_err());
    }

    #[test]
    fn a_plan_sends_the_oldest_missing_run_first() {
        let gaps = vec![SyncGap {
            author_device_id: device(0x11),
            kind: core_sync_record_kind_wire(SyncRecordKind::History),
            after_seq: 2,
            through_seq: 5,
        }];
        let offers = vec![
            offer(0x11, SyncRecordKind::History, 5, 10),
            offer(0x11, SyncRecordKind::History, 3, 10),
            offer(0x11, SyncRecordKind::History, 4, 10),
            // Below the peer's watermark: they already have it.
            offer(0x11, SyncRecordKind::History, 1, 10),
            // A stream they did not ask about.
            offer(0x22, SyncRecordKind::Groups, 1, 10),
        ];
        let plan = core_plan_sync_backfill(gaps, offers, CURRENT, GENERATION, 1_000);
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| step.offer_index)
                .collect::<Vec<_>>(),
            vec![1, 2, 0],
            "3, 4, 5 -- ascending, so a cut round still advances a watermark"
        );
        assert_eq!(plan.planned_bytes, 30);
        assert!(plan.deferred.is_empty());
    }

    #[test]
    fn a_budget_cut_truncates_one_stream_and_leaves_the_others_planned() {
        let gaps = vec![
            SyncGap {
                author_device_id: device(0x11),
                kind: core_sync_record_kind_wire(SyncRecordKind::History),
                after_seq: 0,
                through_seq: 3,
            },
            SyncGap {
                author_device_id: device(0x22),
                kind: core_sync_record_kind_wire(SyncRecordKind::Settings),
                after_seq: 0,
                through_seq: 1,
            },
        ];
        let offers = vec![
            offer(0x11, SyncRecordKind::History, 1, 40),
            offer(0x11, SyncRecordKind::History, 2, 40),
            offer(0x11, SyncRecordKind::History, 3, 5),
            offer(0x22, SyncRecordKind::Settings, 1, 5),
        ];
        let plan = core_plan_sync_backfill(gaps, offers, CURRENT, GENERATION, 50);
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| step.offer_index)
                .collect::<Vec<_>>(),
            vec![0, 3],
            "seq 3 would have fit the budget, but it sits above the hole seq 2 \
             left -- sending it advances nothing"
        );
        assert_eq!(plan.deferred, vec![1, 2]);
        assert_eq!(plan.planned_bytes, 45);
    }

    /// A record bigger than the whole round stalls its own stream and nothing
    /// else. Pinned because the alternative -- planning it anyway and blowing
    /// the budget -- is the one a reader might assume from "oldest first".
    #[test]
    fn a_head_record_larger_than_the_budget_stalls_only_its_own_stream() {
        let gaps = vec![
            SyncGap {
                author_device_id: device(0x11),
                kind: core_sync_record_kind_wire(SyncRecordKind::History),
                after_seq: 0,
                through_seq: 2,
            },
            SyncGap {
                author_device_id: device(0x22),
                kind: core_sync_record_kind_wire(SyncRecordKind::Settings),
                after_seq: 0,
                through_seq: 1,
            },
        ];
        let offers = vec![
            offer(0x11, SyncRecordKind::History, 1, 9_999),
            offer(0x11, SyncRecordKind::History, 2, 1),
            offer(0x22, SyncRecordKind::Settings, 1, 1),
        ];
        let plan = core_plan_sync_backfill(gaps, offers, CURRENT, GENERATION, 100);
        assert_eq!(
            plan.steps
                .iter()
                .map(|step| step.offer_index)
                .collect::<Vec<_>>(),
            vec![2],
        );
        assert_eq!(plan.deferred, vec![0, 1]);
        assert_eq!(plan.planned_bytes, 1);
    }

    #[test]
    fn a_stale_seal_is_planned_for_reseal_rather_than_dropped() {
        let gaps = vec![SyncGap {
            author_device_id: device(0x11),
            kind: core_sync_record_kind_wire(SyncRecordKind::OwnRoster),
            after_seq: 0,
            through_seq: 2,
        }];
        let mut stale = offer(0x11, SyncRecordKind::OwnRoster, 1, 10);
        stale.sealed_for = RosterVersion {
            recovery_epoch: 0,
            seq: 0,
        };
        let offers = vec![stale, offer(0x11, SyncRecordKind::OwnRoster, 2, 10)];
        let plan = core_plan_sync_backfill(gaps, offers, CURRENT, GENERATION, 1_000);
        assert_eq!(
            plan.steps,
            vec![
                SyncBackfillStep {
                    offer_index: 0,
                    action: SyncBackfillAction::Reseal,
                },
                SyncBackfillStep {
                    offer_index: 1,
                    action: SyncBackfillAction::Send,
                },
            ],
            "SYNC-3 re-seals on roster change; it does not strand the record"
        );
    }

    /// §10.1: a rotation changes the device set without touching the roster
    /// version, and sending bytes sealed under the superseded generation would
    /// hand the just-revoked device everything it was revoked from reading.
    #[test]
    fn a_rotated_inbox_key_forces_a_reseal_even_when_the_roster_stood_still() {
        let gaps = vec![SyncGap {
            author_device_id: device(0x11),
            kind: core_sync_record_kind_wire(SyncRecordKind::Settings),
            after_seq: 0,
            through_seq: 1,
        }];
        let offers = vec![offer(0x11, SyncRecordKind::Settings, 1, 10)];
        let plan = core_plan_sync_backfill(gaps, offers, CURRENT, GENERATION + 1, 1_000);
        assert_eq!(
            plan.steps,
            vec![SyncBackfillStep {
                offer_index: 0,
                action: SyncBackfillAction::Reseal,
            }]
        );
    }

    #[test]
    fn a_zero_budget_defers_everything_without_losing_it() {
        let gaps = vec![SyncGap {
            author_device_id: device(0x11),
            kind: core_sync_record_kind_wire(SyncRecordKind::History),
            after_seq: 0,
            through_seq: 2,
        }];
        let offers = vec![
            offer(0x11, SyncRecordKind::History, 1, 1),
            offer(0x11, SyncRecordKind::History, 2, 1),
        ];
        let plan = core_plan_sync_backfill(gaps, offers, CURRENT, GENERATION, 0);
        assert!(plan.steps.is_empty());
        assert_eq!(plan.deferred, vec![0, 1]);
        assert_eq!(plan.planned_bytes, 0);
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}
