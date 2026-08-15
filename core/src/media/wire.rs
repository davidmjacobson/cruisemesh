//! The LAN pull sub-channel: frames, and the proof a request must carry.
//!
//! These frames ride a bulk sub-channel of a LAN link the mesh has *already*
//! authenticated with the peer. Nothing here establishes identity, discovers
//! an address, or names a third party: the endpoint-privacy invariant is
//! untouched because this layer never learns an address at all.
//!
//! # What a request must prove, and why
//!
//! A responder answers only for a blob it holds, and only to a requester that
//! proves it holds the blob's *manifest*. The proof is a keyed digest over a
//! nonce the responder chose:
//!
//! ```text
//! challenge : responder -> requester   nonce (16 random bytes)
//! proof     = BLAKE2b-256("cruisemesh.media.pull-proof/v1" || blob_id || nonce || blob_key)
//! ```
//!
//! Only a manifest holder knows `blob_key`, and the responder can check the
//! proof because it is the sender and holds the key too.
//!
//! It is worth being precise about what this is and is not for.
//!
//! **It is not confidentiality.** Serving ciphertext to a non-holder leaks
//! nothing readable: `BLOB-02` puts the key exclusively inside sealed
//! manifests, so bytes without a key are noise. If the proof were dropped
//! entirely, no plaintext would escape.
//!
//! **It is abuse resistance**, which the spec asks for on three counts:
//!
//! * *bandwidth.* Without a proof, any authenticated LAN peer could pull 128
//!   MB of someone else's ciphertext repeatedly and spend the holder's radio
//!   and battery on it. The budgets in [`super::lan_pull`] bound one session;
//!   the proof bounds who may open one.
//! * *existence probing.* An unproofed responder answers "I have blob X",
//!   which is a fact about what its owner was sent, discoverable by anyone on
//!   the same ship Wi-Fi who can guess or observe a blob id.
//! * *conversation-scoped consent.* A blob is offered to the people the
//!   sender sealed a manifest to. The proof is what makes "possession of a
//!   manifest is the capability to fetch" — the spec's own words — true of
//!   the fetch path rather than only of the read path.
//!
//! The nonce is chosen by the responder and is single-use per session, so a
//! proof captured from one session is worthless in the next.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use rand_core::{OsRng, RngCore};

use super::bitmap::ChunkRange;
use super::blob::{BlobId, BlobKey, BLOB_ID_LEN};
use super::MediaError;

pub const PULL_NONCE_LEN: usize = 16;
pub const PULL_PROOF_LEN: usize = 32;

/// The most ranges one fetch frame may carry. A bound on parser work, and the
/// same number [`super::bitmap::ChunkBitmap::missing_ranges`] is asked for.
pub const PULL_MAX_RANGES_PER_FETCH: u32 = 8;

const PROOF_DOMAIN: &[u8] = b"cruisemesh.media.pull-proof/v1";

const FRAME_OPEN: u8 = 1;
const FRAME_CHALLENGE: u8 = 2;
const FRAME_FETCH: u8 = 3;
const FRAME_CHUNK: u8 = 4;
const FRAME_BATCH_DONE: u8 = 5;
const FRAME_REFUSED: u8 = 6;
const FRAME_CLOSE: u8 = 7;

/// Why a responder will not serve. Every one of these is a *terminal* answer
/// for the session: the requester stops rather than retrying into a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalReason {
    /// This device does not hold that blob (or holds none of the range).
    NotHeld,
    /// The proof did not verify: the requester has no manifest for the blob.
    ProofInvalid,
    /// The request was malformed, out of range, or asked for too much.
    BadRequest,
    /// A session budget or the deadline is spent. Not an error — the
    /// requester opens a new session later and resumes from its bitmap.
    BudgetSpent,
    /// The responder is busy with other transfers and is protecting the link
    /// the mesh itself is using.
    Busy,
}

/// One frame of the pull sub-channel.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PullFrame {
    /// Requester → responder. "Do you have this blob?"
    Open {
        blob_id: BlobId,
    },
    /// Responder → requester. "I do; prove you hold its manifest."
    Challenge {
        nonce: [u8; PULL_NONCE_LEN],
        /// How many chunks the responder actually holds, so a requester
        /// talking to a partial holder does not ask for what cannot come.
        chunks_held: u32,
    },
    /// Requester → responder. Proof, plus the ranges still missing.
    Fetch {
        proof: [u8; PULL_PROOF_LEN],
        ranges: Vec<ChunkRange>,
    },
    /// Responder → requester. One chunk of ciphertext.
    Chunk {
        index: u32,
        ciphertext: Vec<u8>,
    },
    /// Responder → requester. This fetch is served; ask again or close.
    BatchDone {
        chunks_served: u32,
    },
    Refused {
        reason: RefusalReason,
    },
    /// Either side. Ends the session.
    Close,
}

/// The proof a fetch must carry.
pub fn pull_proof(
    key: &BlobKey,
    blob_id: &BlobId,
    nonce: &[u8; PULL_NONCE_LEN],
) -> [u8; PULL_PROOF_LEN] {
    let mut hasher = Blake2bVar::new(PULL_PROOF_LEN).expect("valid blake2b output length");
    Update::update(&mut hasher, PROOF_DOMAIN);
    Update::update(&mut hasher, blob_id.as_bytes());
    Update::update(&mut hasher, nonce);
    Update::update(&mut hasher, key.as_bytes());
    let mut out = [0u8; PULL_PROOF_LEN];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer is PULL_PROOF_LEN");
    out
}

/// Constant-time comparison. A proof check that returned early would leak the
/// matching prefix, which over a fast LAN link is a practical oracle.
pub fn verify_pull_proof(
    key: &BlobKey,
    blob_id: &BlobId,
    nonce: &[u8; PULL_NONCE_LEN],
    presented: &[u8; PULL_PROOF_LEN],
) -> bool {
    let expected = pull_proof(key, blob_id, nonce);
    let mut diff = 0u8;
    for (a, b) in expected.iter().zip(presented.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

pub fn generate_pull_nonce() -> [u8; PULL_NONCE_LEN] {
    let mut nonce = [0u8; PULL_NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    nonce
}

pub fn encode_pull_frame(frame: &PullFrame) -> Result<Vec<u8>, MediaError> {
    let mut out = Vec::new();
    match frame {
        PullFrame::Open { blob_id } => {
            out.push(FRAME_OPEN);
            out.extend_from_slice(blob_id.as_bytes());
        }
        PullFrame::Challenge { nonce, chunks_held } => {
            out.push(FRAME_CHALLENGE);
            out.extend_from_slice(nonce);
            out.extend_from_slice(&chunks_held.to_be_bytes());
        }
        PullFrame::Fetch { proof, ranges } => {
            if ranges.is_empty() || ranges.len() as u32 > PULL_MAX_RANGES_PER_FETCH {
                return Err(MediaError::Malformed(format!(
                    "a fetch carries 1..={PULL_MAX_RANGES_PER_FETCH} ranges, got {}",
                    ranges.len()
                )));
            }
            out.push(FRAME_FETCH);
            out.extend_from_slice(proof);
            out.push(ranges.len() as u8);
            for range in ranges {
                if range.count == 0 {
                    return Err(MediaError::Malformed("an empty chunk range".into()));
                }
                out.extend_from_slice(&range.start.to_be_bytes());
                out.extend_from_slice(&range.count.to_be_bytes());
            }
        }
        PullFrame::Chunk { index, ciphertext } => {
            if ciphertext.is_empty()
                || ciphertext.len() > super::MEDIA_CHUNK_CIPHERTEXT_BYTES as usize
            {
                return Err(MediaError::Malformed(
                    "a chunk frame carries one chunk's ciphertext".into(),
                ));
            }
            out.push(FRAME_CHUNK);
            out.extend_from_slice(&index.to_be_bytes());
            out.extend_from_slice(&(ciphertext.len() as u32).to_be_bytes());
            out.extend_from_slice(ciphertext);
        }
        PullFrame::BatchDone { chunks_served } => {
            out.push(FRAME_BATCH_DONE);
            out.extend_from_slice(&chunks_served.to_be_bytes());
        }
        PullFrame::Refused { reason } => {
            out.push(FRAME_REFUSED);
            out.push(match reason {
                RefusalReason::NotHeld => 1,
                RefusalReason::ProofInvalid => 2,
                RefusalReason::BadRequest => 3,
                RefusalReason::BudgetSpent => 4,
                RefusalReason::Busy => 5,
            });
        }
        PullFrame::Close => out.push(FRAME_CLOSE),
    }
    Ok(out)
}

pub fn decode_pull_frame(bytes: &[u8]) -> Result<PullFrame, MediaError> {
    let malformed = |what: &str| MediaError::Malformed(format!("pull frame: {what}"));
    let (&tag, rest) = bytes.split_first().ok_or_else(|| malformed("empty"))?;
    let frame = match tag {
        FRAME_OPEN => {
            let blob_id = BlobId::from_slice(
                take(rest, 0, BLOB_ID_LEN).ok_or_else(|| malformed("short open"))?,
            )?;
            exact(rest, BLOB_ID_LEN, malformed)?;
            PullFrame::Open { blob_id }
        }
        FRAME_CHALLENGE => {
            let nonce: [u8; PULL_NONCE_LEN] = take(rest, 0, PULL_NONCE_LEN)
                .ok_or_else(|| malformed("short challenge"))?
                .try_into()
                .expect("slice is PULL_NONCE_LEN");
            let chunks_held = u32::from_be_bytes(
                take(rest, PULL_NONCE_LEN, 4)
                    .ok_or_else(|| malformed("short challenge"))?
                    .try_into()
                    .expect("slice is 4"),
            );
            exact(rest, PULL_NONCE_LEN + 4, malformed)?;
            PullFrame::Challenge { nonce, chunks_held }
        }
        FRAME_FETCH => {
            let proof: [u8; PULL_PROOF_LEN] = take(rest, 0, PULL_PROOF_LEN)
                .ok_or_else(|| malformed("short fetch"))?
                .try_into()
                .expect("slice is PULL_PROOF_LEN");
            let count = *rest
                .get(PULL_PROOF_LEN)
                .ok_or_else(|| malformed("short fetch"))?;
            if count == 0 || u32::from(count) > PULL_MAX_RANGES_PER_FETCH {
                return Err(malformed("range count out of bounds"));
            }
            let mut ranges = Vec::with_capacity(count as usize);
            let mut offset = PULL_PROOF_LEN + 1;
            for _ in 0..count {
                let start = u32::from_be_bytes(
                    take(rest, offset, 4)
                        .ok_or_else(|| malformed("short range"))?
                        .try_into()
                        .expect("slice is 4"),
                );
                let chunk_count = u32::from_be_bytes(
                    take(rest, offset + 4, 4)
                        .ok_or_else(|| malformed("short range"))?
                        .try_into()
                        .expect("slice is 4"),
                );
                if chunk_count == 0 {
                    return Err(malformed("empty chunk range"));
                }
                ranges.push(ChunkRange {
                    start,
                    count: chunk_count,
                });
                offset += 8;
            }
            exact(rest, offset, malformed)?;
            PullFrame::Fetch { proof, ranges }
        }
        FRAME_CHUNK => {
            let index = u32::from_be_bytes(
                take(rest, 0, 4)
                    .ok_or_else(|| malformed("short chunk"))?
                    .try_into()
                    .expect("slice is 4"),
            );
            let len = u32::from_be_bytes(
                take(rest, 4, 4)
                    .ok_or_else(|| malformed("short chunk"))?
                    .try_into()
                    .expect("slice is 4"),
            ) as usize;
            if len == 0 || len > super::MEDIA_CHUNK_CIPHERTEXT_BYTES as usize {
                return Err(malformed("chunk length out of bounds"));
            }
            let ciphertext = take(rest, 8, len)
                .ok_or_else(|| malformed("truncated chunk"))?
                .to_vec();
            exact(rest, 8 + len, malformed)?;
            PullFrame::Chunk { index, ciphertext }
        }
        FRAME_BATCH_DONE => {
            let chunks_served = u32::from_be_bytes(
                take(rest, 0, 4)
                    .ok_or_else(|| malformed("short batch-done"))?
                    .try_into()
                    .expect("slice is 4"),
            );
            exact(rest, 4, malformed)?;
            PullFrame::BatchDone { chunks_served }
        }
        FRAME_REFUSED => {
            let reason = match rest.first().ok_or_else(|| malformed("short refusal"))? {
                1 => RefusalReason::NotHeld,
                2 => RefusalReason::ProofInvalid,
                3 => RefusalReason::BadRequest,
                4 => RefusalReason::BudgetSpent,
                5 => RefusalReason::Busy,
                _ => return Err(malformed("unknown refusal reason")),
            };
            exact(rest, 1, malformed)?;
            PullFrame::Refused { reason }
        }
        FRAME_CLOSE => {
            exact(rest, 0, malformed)?;
            PullFrame::Close
        }
        other => return Err(malformed(&format!("unknown frame type {other}"))),
    };
    Ok(frame)
}

fn take(bytes: &[u8], offset: usize, len: usize) -> Option<&[u8]> {
    bytes.get(offset..offset.checked_add(len)?)
}

fn exact(
    bytes: &[u8],
    consumed: usize,
    malformed: impl Fn(&str) -> MediaError,
) -> Result<(), MediaError> {
    if bytes.len() != consumed {
        return Err(malformed("trailing bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::blob::test_key;

    fn frames() -> Vec<PullFrame> {
        vec![
            PullFrame::Open {
                blob_id: BlobId([0x31; BLOB_ID_LEN]),
            },
            PullFrame::Challenge {
                nonce: [0x44; PULL_NONCE_LEN],
                chunks_held: 12,
            },
            PullFrame::Fetch {
                proof: [0x55; PULL_PROOF_LEN],
                ranges: vec![
                    ChunkRange { start: 0, count: 4 },
                    ChunkRange { start: 9, count: 1 },
                ],
            },
            PullFrame::Chunk {
                index: 3,
                ciphertext: vec![1, 2, 3, 4],
            },
            PullFrame::BatchDone { chunks_served: 5 },
            PullFrame::Refused {
                reason: RefusalReason::ProofInvalid,
            },
            PullFrame::Close,
        ]
    }

    #[test]
    fn every_frame_round_trips() {
        for frame in frames() {
            let encoded = encode_pull_frame(&frame).unwrap();
            assert_eq!(decode_pull_frame(&encoded).unwrap(), frame);
        }
    }

    #[test]
    fn decoding_refuses_truncation_trailing_bytes_and_unknown_tags() {
        for frame in frames() {
            let encoded = encode_pull_frame(&frame).unwrap();
            let mut trailing = encoded.clone();
            trailing.push(0);
            assert!(
                decode_pull_frame(&trailing).is_err(),
                "trailing bytes accepted for {frame:?}"
            );
            if encoded.len() > 1 {
                assert!(
                    decode_pull_frame(&encoded[..encoded.len() - 1]).is_err(),
                    "truncation accepted for {frame:?}"
                );
            }
        }
        assert!(decode_pull_frame(&[]).is_err());
        assert!(decode_pull_frame(&[0xFE]).is_err());
    }

    #[test]
    fn a_fetch_is_bounded_in_ranges_at_both_ends() {
        let too_many = PullFrame::Fetch {
            proof: [0; PULL_PROOF_LEN],
            ranges: (0..=PULL_MAX_RANGES_PER_FETCH)
                .map(|start| ChunkRange { start, count: 1 })
                .collect(),
        };
        assert!(encode_pull_frame(&too_many).is_err());
        assert!(encode_pull_frame(&PullFrame::Fetch {
            proof: [0; PULL_PROOF_LEN],
            ranges: Vec::new(),
        })
        .is_err());

        // And a hand-built frame claiming more ranges than the cap is refused
        // by the decoder rather than trusted because it decoded.
        let mut hostile = vec![FRAME_FETCH];
        hostile.extend_from_slice(&[0u8; PULL_PROOF_LEN]);
        hostile.push(200);
        assert!(decode_pull_frame(&hostile).is_err());
    }

    #[test]
    fn a_chunk_frame_cannot_claim_more_than_one_chunk() {
        assert!(encode_pull_frame(&PullFrame::Chunk {
            index: 0,
            ciphertext: vec![0; super::super::MEDIA_CHUNK_CIPHERTEXT_BYTES as usize + 1],
        })
        .is_err());

        let mut hostile = vec![FRAME_CHUNK];
        hostile.extend_from_slice(&0u32.to_be_bytes());
        hostile.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode_pull_frame(&hostile).is_err());
    }

    #[test]
    fn a_proof_binds_the_key_the_blob_and_the_nonce() {
        let key = test_key(4);
        let blob_id = BlobId([0x77; BLOB_ID_LEN]);
        let nonce = [0x09; PULL_NONCE_LEN];
        let proof = pull_proof(&key, &blob_id, &nonce);

        assert!(verify_pull_proof(&key, &blob_id, &nonce, &proof));
        assert!(!verify_pull_proof(&test_key(5), &blob_id, &nonce, &proof));
        assert!(!verify_pull_proof(
            &key,
            &BlobId([0x78; BLOB_ID_LEN]),
            &nonce,
            &proof
        ));
        assert!(
            !verify_pull_proof(&key, &blob_id, &[0x0A; PULL_NONCE_LEN], &proof),
            "a proof from one session must be worthless in the next"
        );
    }

    #[test]
    fn nonces_are_not_a_constant() {
        assert_ne!(generate_pull_nonce(), generate_pull_nonce());
    }
}
