//! The per-blob chunk bitmap: what a resumed transfer asks for.
//!
//! This is the entire answer to "did we reinvent TCP": within one connection
//! TCP already orders and retransmits, so the blob plane adds only the thing
//! TCP cannot carry — what is still missing *across* connections, restarts,
//! days and sources. That is a bitmap, and it is nothing more than a bitmap.
//!
//! Two properties this type is responsible for:
//!
//! * **Progress is monotone.** [`ChunkBitmap::set`] never unsets, and
//!   [`ChunkBitmap::merge`] is a union. The only way a chunk goes back to
//!   missing is [`ChunkBitmap::clear`], which exists for exactly one caller —
//!   the corrupted-chunk recovery path — and is never reachable from a plain
//!   receive.
//! * **It is restart-safe.** [`ChunkBitmap::as_bytes`] is what the store
//!   persists; [`ChunkBitmap::from_bytes`] refuses a byte string that does not
//!   match the chunk count it is being loaded for, rather than silently
//!   producing a bitmap whose tail means nothing.

use super::MediaError;

/// A contiguous run of chunk indices, which is what a request asks for and a
/// responder serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkRange {
    pub start: u32,
    pub count: u32,
}

impl ChunkRange {
    pub fn end_exclusive(&self) -> u64 {
        u64::from(self.start) + u64::from(self.count)
    }

    pub fn contains(&self, index: u32) -> bool {
        u64::from(index) >= u64::from(self.start) && u64::from(index) < self.end_exclusive()
    }
}

/// Which chunks of one blob are present on this device.
#[derive(Clone, PartialEq, Eq)]
pub struct ChunkBitmap {
    chunk_count: u32,
    present: u32,
    bits: Vec<u8>,
}

impl std::fmt::Debug for ChunkBitmap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ChunkBitmap({}/{} chunks)",
            self.present, self.chunk_count
        )
    }
}

impl ChunkBitmap {
    pub fn empty(chunk_count: u32) -> Result<Self, MediaError> {
        if chunk_count == 0 {
            return Err(MediaError::Malformed(
                "a blob has at least one chunk".into(),
            ));
        }
        Ok(ChunkBitmap {
            chunk_count,
            present: 0,
            bits: vec![0u8; byte_len(chunk_count)],
        })
    }

    /// Load a persisted bitmap. The chunk count comes from the manifest, not
    /// from the stored bytes, so a truncated or padded row is a load error
    /// rather than a bitmap that quietly means something else.
    pub fn from_bytes(chunk_count: u32, bytes: &[u8]) -> Result<Self, MediaError> {
        if chunk_count == 0 {
            return Err(MediaError::Malformed(
                "a blob has at least one chunk".into(),
            ));
        }
        let expected = byte_len(chunk_count);
        if bytes.len() != expected {
            return Err(MediaError::Malformed(format!(
                "bitmap is {} bytes, {chunk_count} chunks needs {expected}",
                bytes.len()
            )));
        }
        let trailing_bits = (expected * 8) as u32 - chunk_count;
        if trailing_bits > 0 {
            // Bits are laid out most-significant-first, so the bits past the
            // last chunk are the low ones.
            let mask = (1u8 << trailing_bits) - 1;
            if bytes[expected - 1] & mask != 0 {
                return Err(MediaError::Malformed(
                    "bitmap sets bits past the last chunk".into(),
                ));
            }
        }
        let present = bytes.iter().map(|b| b.count_ones()).sum::<u32>();
        Ok(ChunkBitmap {
            chunk_count,
            present,
            bits: bytes.to_vec(),
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bits
    }

    pub fn chunk_count(&self) -> u32 {
        self.chunk_count
    }

    pub fn present_count(&self) -> u32 {
        self.present
    }

    pub fn is_complete(&self) -> bool {
        self.present == self.chunk_count
    }

    pub fn has(&self, index: u32) -> bool {
        if index >= self.chunk_count {
            return false;
        }
        let (byte, bit) = position(index);
        self.bits[byte] & bit != 0
    }

    /// Mark a chunk present. Returns whether this call is what changed it, so
    /// a duplicate delivery is visible to the caller as "nothing new" instead
    /// of double-counting progress.
    pub fn set(&mut self, index: u32) -> bool {
        if index >= self.chunk_count || self.has(index) {
            return false;
        }
        let (byte, bit) = position(index);
        self.bits[byte] |= bit;
        self.present += 1;
        true
    }

    /// Re-mark a chunk missing. The **only** legitimate caller is the
    /// corrupted-chunk recovery path: a chunk that failed authentication or
    /// whose blob failed its final digest check. Ordinary receive never
    /// clears, which is what keeps progress monotone.
    pub fn clear(&mut self, index: u32) -> bool {
        if index >= self.chunk_count || !self.has(index) {
            return false;
        }
        let (byte, bit) = position(index);
        self.bits[byte] &= !bit;
        self.present -= 1;
        true
    }

    /// Union with another bitmap for the same blob. Used when a transfer
    /// switches source and the two sides disagree about what landed.
    pub fn merge(&mut self, other: &ChunkBitmap) -> Result<u32, MediaError> {
        if other.chunk_count != self.chunk_count {
            return Err(MediaError::Malformed(
                "cannot merge bitmaps of different blobs".into(),
            ));
        }
        let mut gained = 0;
        for index in 0..self.chunk_count {
            if other.has(index) && self.set(index) {
                gained += 1;
            }
        }
        Ok(gained)
    }

    /// The next missing chunks, as contiguous ranges.
    ///
    /// `max_chunks` is the transfer window — the requester never asks for
    /// more outstanding work than it is willing to hold — and `max_ranges`
    /// bounds how fragmented one request may be, so a bitmap shot through
    /// with single-chunk holes cannot produce a request the responder has to
    /// spend unbounded work parsing.
    pub fn missing_ranges(&self, max_chunks: u32, max_ranges: u32) -> Vec<ChunkRange> {
        let mut ranges = Vec::new();
        if max_chunks == 0 || max_ranges == 0 {
            return ranges;
        }
        let mut budget = max_chunks;
        let mut index = 0;
        while index < self.chunk_count && budget > 0 && (ranges.len() as u32) < max_ranges {
            if self.has(index) {
                index += 1;
                continue;
            }
            let start = index;
            let mut count = 0;
            while index < self.chunk_count && !self.has(index) && count < budget {
                count += 1;
                index += 1;
            }
            budget -= count;
            ranges.push(ChunkRange { start, count });
        }
        ranges
    }
}

fn byte_len(chunk_count: u32) -> usize {
    chunk_count.div_ceil(8) as usize
}

fn position(index: u32) -> (usize, u8) {
    ((index / 8) as usize, 1u8 << (7 - (index % 8)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setting_is_monotone_and_duplicates_report_no_progress() {
        let mut bitmap = ChunkBitmap::empty(10).unwrap();
        assert_eq!(bitmap.present_count(), 0);
        assert!(bitmap.set(3));
        assert!(!bitmap.set(3), "a duplicate chunk is not progress");
        assert_eq!(bitmap.present_count(), 1);
        assert!(bitmap.has(3));
        assert!(!bitmap.has(4));
        assert!(!bitmap.set(10), "out of range is refused, not panicked");
        assert!(!bitmap.is_complete());
        for index in 0..10 {
            bitmap.set(index);
        }
        assert!(bitmap.is_complete());
    }

    #[test]
    fn clearing_is_the_recovery_path_and_nothing_else() {
        let mut bitmap = ChunkBitmap::empty(4).unwrap();
        bitmap.set(1);
        assert!(bitmap.clear(1));
        assert!(!bitmap.clear(1), "clearing a missing chunk changes nothing");
        assert_eq!(bitmap.present_count(), 0);
    }

    #[test]
    fn a_bitmap_survives_a_round_trip_through_storage() {
        let mut bitmap = ChunkBitmap::empty(20).unwrap();
        for index in [0, 5, 6, 7, 19] {
            bitmap.set(index);
        }
        let stored = bitmap.as_bytes().to_vec();
        let loaded = ChunkBitmap::from_bytes(20, &stored).unwrap();
        assert_eq!(loaded, bitmap);
        assert_eq!(loaded.present_count(), 5);
        assert!(loaded.has(19));
    }

    #[test]
    fn a_stored_bitmap_that_does_not_fit_its_blob_is_refused() {
        let bitmap = ChunkBitmap::empty(20).unwrap();
        assert!(ChunkBitmap::from_bytes(25, bitmap.as_bytes()).is_err());
        assert!(ChunkBitmap::from_bytes(20, &[0u8; 2]).is_err());
        assert!(ChunkBitmap::from_bytes(0, &[]).is_err());
        // 20 chunks is 3 bytes; the last 4 bits are past the end.
        assert!(
            ChunkBitmap::from_bytes(20, &[0, 0, 0b0000_0001]).is_err(),
            "a bit past the last chunk means the row is not what it claims"
        );
    }

    #[test]
    fn missing_ranges_are_windowed_and_bounded_in_fragments() {
        let mut bitmap = ChunkBitmap::empty(12).unwrap();
        for index in [2, 3, 8] {
            bitmap.set(index);
        }
        assert_eq!(
            bitmap.missing_ranges(64, 8),
            vec![
                ChunkRange { start: 0, count: 2 },
                ChunkRange { start: 4, count: 4 },
                ChunkRange { start: 9, count: 3 },
            ]
        );
        // The window truncates the tail rather than dropping a range.
        assert_eq!(
            bitmap.missing_ranges(3, 8),
            vec![
                ChunkRange { start: 0, count: 2 },
                ChunkRange { start: 4, count: 1 },
            ]
        );
        assert_eq!(
            bitmap.missing_ranges(64, 1),
            vec![ChunkRange { start: 0, count: 2 }]
        );
        assert!(bitmap.missing_ranges(0, 8).is_empty());
    }

    #[test]
    fn a_complete_bitmap_asks_for_nothing() {
        let mut bitmap = ChunkBitmap::empty(5).unwrap();
        for index in 0..5 {
            bitmap.set(index);
        }
        assert!(bitmap.missing_ranges(64, 8).is_empty());
    }

    #[test]
    fn merging_is_a_union_and_refuses_a_different_blob() {
        let mut mine = ChunkBitmap::empty(8).unwrap();
        mine.set(0);
        mine.set(1);
        let mut theirs = ChunkBitmap::empty(8).unwrap();
        theirs.set(1);
        theirs.set(7);
        assert_eq!(mine.merge(&theirs).unwrap(), 1);
        assert_eq!(mine.present_count(), 3);
        assert!(mine.has(7));
        assert!(mine.merge(&ChunkBitmap::empty(9).unwrap()).is_err());
    }
}
