//! One LAN blob transfer, as two explicit state machines.
//!
//! This module is shaped after `core/src/session/relay_pass.rs` and for the
//! same reasons: typed actions out, typed results in, an explicit `now_ms`,
//! declared budgets, one outstanding action at a time, and no operating
//! system anywhere. There is no socket here, no file, no timer, no thread.
//!
//! ```text
//! requester                              responder
//! ---------                              ---------
//! start(now)        -> Send(Open)
//!                                        on_frame(Open)   -> Reply(Challenge)
//! resume(Challenge) -> Send(Fetch)
//!                                        on_frame(Fetch)  -> ReadChunk(i)
//!                                        provide_chunk    -> Reply(Chunk i)
//!                                        next()           -> Reply(BatchDone)
//! resume(Chunk)     -> Await
//! resume(BatchDone) -> Send(Fetch) | Finish(Close)
//! ```
//!
//! # Why the requester authenticates every chunk on arrival
//!
//! Each chunk is an independent AEAD box ([`super::blob`]), so a chunk that
//! was corrupted in transit, truncated, replayed from another position, or
//! fabricated by a peer fails to open the moment it lands. It is then counted
//! as **rejected** and the bitmap is left alone — the chunk stays missing and
//! will be requested again. A rejected chunk never becomes progress, which is
//! `BLOB-05` at chunk granularity; the whole-blob digest check on completion
//! is the same rule at blob granularity.
//!
//! # Why the responder demands a proof
//!
//! See [`super::wire`]. Short version: serving ciphertext to a stranger leaks
//! nothing readable, but it does spend the holder's link and admit that the
//! holder has that blob at all. The proof binds the session's nonce, so it is
//! a capability check rather than a password.
//!
//! # Budgets
//!
//! Both roles declare their budgets up front and both report every count they
//! bound in their summary, so a transcript shows whether a session stayed
//! inside them rather than a reader having to trust the loop. A session that
//! runs out of budget ends in [`PullOutcome::BudgetSpent`] and the transfer
//! *resumes later from the bitmap* — a spent budget is a pause, never a
//! failure, and never a restart from zero.

use super::bitmap::{ChunkBitmap, ChunkRange};
use super::blob::{open_chunk, BlobGeometry, BlobId, BlobKey};
use super::wire::{
    pull_proof, verify_pull_proof, PullFrame, RefusalReason, PULL_MAX_RANGES_PER_FETCH,
    PULL_NONCE_LEN, PULL_PROOF_LEN,
};

// ---------------------------------------------------------------------------
// Budgets
// ---------------------------------------------------------------------------

/// Most fetch requests one requester session issues.
pub const PULL_MAX_REQUESTS: u32 = 64;
/// Most ciphertext one requester session accepts, in bytes. Roughly a third
/// of the blob cap: a 128 MB clip takes a few sessions, which is the point —
/// each one is bounded, and the bitmap makes the next one cheap.
pub const PULL_MAX_SESSION_BYTES: u64 = 48 * 1024 * 1024;
/// Chunks one fetch may ask for. At a 256 KiB chunk this is a 4 MiB window,
/// small enough to interleave with the mesh traffic sharing the link.
pub const PULL_WINDOW_CHUNKS: u32 = 16;
/// Wall-clock bound on one session.
pub const PULL_DEADLINE_MS: i64 = 60_000;
/// Most chunks one responder session serves.
pub const SERVE_MAX_CHUNKS: u32 = 512;
/// Most ciphertext one responder session serves.
pub const SERVE_MAX_SESSION_BYTES: u64 = 48 * 1024 * 1024;
/// Most fetch frames one responder session answers.
pub const SERVE_MAX_FETCHES: u32 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullBudgets {
    pub max_requests: u32,
    pub max_bytes: u64,
    pub window_chunks: u32,
    pub max_ranges: u32,
    pub deadline_ms: i64,
}

impl Default for PullBudgets {
    fn default() -> Self {
        PullBudgets {
            max_requests: PULL_MAX_REQUESTS,
            max_bytes: PULL_MAX_SESSION_BYTES,
            window_chunks: PULL_WINDOW_CHUNKS,
            max_ranges: PULL_MAX_RANGES_PER_FETCH,
            deadline_ms: PULL_DEADLINE_MS,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServeBudgets {
    pub max_fetches: u32,
    pub max_chunks: u32,
    pub max_bytes: u64,
    pub deadline_ms: i64,
}

impl Default for ServeBudgets {
    fn default() -> Self {
        ServeBudgets {
            max_fetches: SERVE_MAX_FETCHES,
            max_chunks: SERVE_MAX_CHUNKS,
            max_bytes: SERVE_MAX_SESSION_BYTES,
            deadline_ms: PULL_DEADLINE_MS,
        }
    }
}

// ---------------------------------------------------------------------------
// Requester
// ---------------------------------------------------------------------------

/// Why a pull session ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullOutcome {
    /// Every chunk is present. The caller verifies the assembled ciphertext
    /// against the manifest digest before anything is decrypted or shown.
    Complete,
    /// Nothing was missing when the session started.
    NothingMissing,
    /// A budget stopped the session. Resume later from the bitmap.
    BudgetSpent,
    /// The deadline passed. Same story as a spent budget.
    DeadlineReached,
    /// The responder refused.
    Refused {
        reason: RefusalReason,
    },
    /// The peer sent something the protocol does not allow.
    ProtocolError,
    /// The link failed.
    TransportFailed,
    Cancelled,
}

/// What a chunk the requester accepted looks like.
///
/// The **ciphertext** is what the device stores: it is what the manifest
/// digest names, it is what a later verification re-hashes, and it is what a
/// future phase could serve onward. Plaintext is produced at render time by
/// [`super::blob::open_blob`], not here. Core authenticated this chunk before
/// handing it over, so a driver that persists it is persisting bytes that are
/// already known to belong to this blob at this index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptedChunk {
    pub index: u32,
    pub ciphertext: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullSummary {
    pub blob_id: BlobId,
    pub outcome: PullOutcome,
    pub requests_issued: u32,
    pub chunks_accepted: u32,
    pub chunks_rejected: u32,
    pub chunks_duplicate: u32,
    pub bytes_received: u64,
    pub chunks_present_at_end: u32,
    pub chunk_count: u32,
    pub stale_results_ignored: u32,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
}

impl PullSummary {
    pub fn is_complete(&self) -> bool {
        matches!(self.outcome, PullOutcome::Complete)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PullActionKind {
    /// Send this frame and read for a reply.
    Send { frame: PullFrame },
    /// Nothing to send; keep reading. A batch delivers many frames against
    /// one outstanding request.
    Await,
    /// Send this last frame; the session is over either way.
    Finish {
        close_frame: PullFrame,
        summary: PullSummary,
    },
    /// The session was already over. Nothing to send.
    Finished { summary: PullSummary },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullAction {
    /// Strictly increasing. A result naming anything else is stale and
    /// changes nothing — the same `IDEMP-01` comparison the relay pass makes.
    pub action_id: u64,
    pub kind: PullActionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PullTransportError {
    Timeout,
    ConnectionLost,
    Cancelled,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PullResult {
    Frame {
        action_id: u64,
        frame: PullFrame,
    },
    TransportError {
        action_id: u64,
        error: PullTransportError,
    },
}

/// What a requester needs to know before it opens a session: the manifest's
/// content, and what is already on disk.
#[derive(Clone, Debug)]
pub struct PullPlan {
    pub blob_id: BlobId,
    pub blob_key: BlobKey,
    pub geometry: BlobGeometry,
    pub bitmap: ChunkBitmap,
    pub budgets: PullBudgets,
}

/// The recipient's half of a transfer.
pub struct PullSession {
    plan: PullPlan,
    started: bool,
    started_at_ms: i64,
    now_ms: i64,
    next_action_id: u64,
    outstanding: Option<PullAction>,
    nonce: Option<[u8; PULL_NONCE_LEN]>,
    proof: Option<[u8; PULL_PROOF_LEN]>,
    outstanding_ranges: Vec<ChunkRange>,
    accepted: Vec<AcceptedChunk>,
    finished: Option<PullSummary>,
    requests_issued: u32,
    chunks_accepted: u32,
    chunks_rejected: u32,
    chunks_duplicate: u32,
    bytes_received: u64,
    stale_results_ignored: u32,
}

impl PullSession {
    pub fn new(plan: PullPlan) -> Self {
        PullSession {
            plan,
            started: false,
            started_at_ms: 0,
            now_ms: 0,
            next_action_id: 1,
            outstanding: None,
            nonce: None,
            proof: None,
            outstanding_ranges: Vec::new(),
            accepted: Vec::new(),
            finished: None,
            requests_issued: 0,
            chunks_accepted: 0,
            chunks_rejected: 0,
            chunks_duplicate: 0,
            bytes_received: 0,
            stale_results_ignored: 0,
        }
    }

    /// Open the session. Calling it twice restates rather than restarting.
    pub fn start(&mut self, now_ms: i64) -> PullAction {
        if self.finished.is_some() || self.started {
            return self.restate();
        }
        self.started = true;
        self.started_at_ms = now_ms;
        self.now_ms = now_ms;
        if self.plan.bitmap.is_complete() {
            return self.finish(PullOutcome::NothingMissing);
        }
        self.emit(PullActionKind::Send {
            frame: PullFrame::Open {
                blob_id: self.plan.blob_id,
            },
        })
    }

    /// Apply one result. A result whose `action_id` is not the outstanding
    /// one mutates nothing and is counted.
    pub fn resume(&mut self, result: PullResult, now_ms: i64) -> PullAction {
        if self.finished.is_some() {
            return self.restate();
        }
        self.now_ms = now_ms;
        let outstanding_id = match &self.outstanding {
            Some(action) => action.action_id,
            None => {
                self.stale_results_ignored += 1;
                return self.restate();
            }
        };
        let result_id = match &result {
            PullResult::Frame { action_id, .. } => *action_id,
            PullResult::TransportError { action_id, .. } => *action_id,
        };
        if result_id != outstanding_id {
            self.stale_results_ignored += 1;
            return self.restate();
        }
        if self.past_deadline() {
            return self.finish(PullOutcome::DeadlineReached);
        }

        match result {
            PullResult::TransportError { error, .. } => match error {
                PullTransportError::Cancelled => self.finish(PullOutcome::Cancelled),
                _ => self.finish(PullOutcome::TransportFailed),
            },
            PullResult::Frame { frame, .. } => self.apply_frame(frame),
        }
    }

    /// End the session on the caller's terms — the app is backgrounding, the
    /// user paused, the link is being torn down. The bitmap keeps everything
    /// accepted so far, so the next session resumes.
    pub fn cancel(&mut self, now_ms: i64) -> PullSummary {
        self.now_ms = now_ms;
        if self.finished.is_none() {
            self.finish(PullOutcome::Cancelled);
        }
        self.summary()
    }

    /// Take the authenticated chunks accepted so far. The driver persists
    /// them and marks the bitmap through [`super::store::BlobStore`]. Bounded
    /// by the transfer window, so this never grows without limit.
    pub fn take_accepted(&mut self) -> Vec<AcceptedChunk> {
        std::mem::take(&mut self.accepted)
    }

    /// The bitmap as this session has updated it. Persisting it is the
    /// caller's job; a crash before that costs only re-fetching the chunks
    /// this session accepted, never correctness.
    pub fn bitmap(&self) -> &ChunkBitmap {
        &self.plan.bitmap
    }

    pub fn summary(&self) -> PullSummary {
        self.finished
            .clone()
            .unwrap_or_else(|| self.snapshot(PullOutcome::Cancelled))
    }

    fn apply_frame(&mut self, frame: PullFrame) -> PullAction {
        match frame {
            PullFrame::Challenge { nonce, chunks_held } => {
                if self.nonce.is_some() {
                    return self.finish(PullOutcome::ProtocolError);
                }
                if chunks_held == 0 {
                    return self.finish(PullOutcome::Refused {
                        reason: RefusalReason::NotHeld,
                    });
                }
                self.nonce = Some(nonce);
                self.proof = Some(pull_proof(&self.plan.blob_key, &self.plan.blob_id, &nonce));
                self.next_fetch()
            }
            PullFrame::Chunk { index, ciphertext } => self.apply_chunk(index, ciphertext),
            PullFrame::BatchDone { .. } => {
                self.outstanding_ranges.clear();
                if self.plan.bitmap.is_complete() {
                    self.finish(PullOutcome::Complete)
                } else {
                    self.next_fetch()
                }
            }
            PullFrame::Refused { reason } => self.finish(PullOutcome::Refused { reason }),
            PullFrame::Close => {
                if self.plan.bitmap.is_complete() {
                    self.finish(PullOutcome::Complete)
                } else {
                    self.finish(PullOutcome::TransportFailed)
                }
            }
            // A requester never receives these: they are its own frames.
            PullFrame::Open { .. } | PullFrame::Fetch { .. } => {
                self.finish(PullOutcome::ProtocolError)
            }
        }
    }

    fn apply_chunk(&mut self, index: u32, ciphertext: Vec<u8>) -> PullAction {
        // Bytes count against the budget whatever they turn out to be: a peer
        // that spends the link on rejects must not get an unbounded number of
        // tries at it.
        self.bytes_received = self.bytes_received.saturating_add(ciphertext.len() as u64);

        let requested = self
            .outstanding_ranges
            .iter()
            .any(|range| range.contains(index));
        if !requested {
            self.chunks_rejected += 1;
            return self.after_chunk();
        }
        if self.plan.bitmap.has(index) {
            self.chunks_duplicate += 1;
            return self.after_chunk();
        }
        match open_chunk(&self.plan.blob_key, &self.plan.geometry, index, &ciphertext) {
            Ok(_plaintext) => {
                // The plaintext proved the chunk; the ciphertext is what is
                // kept, because the ciphertext is what the digest names.
                self.plan.bitmap.set(index);
                self.chunks_accepted += 1;
                self.accepted.push(AcceptedChunk { index, ciphertext });
            }
            Err(_) => {
                // BLOB-05: a chunk that fails authentication is not stored,
                // not counted as progress, and stays missing in the bitmap so
                // the next fetch asks for it again.
                self.chunks_rejected += 1;
            }
        }
        self.after_chunk()
    }

    fn after_chunk(&mut self) -> PullAction {
        if self.bytes_received > self.plan.budgets.max_bytes {
            return self.finish(PullOutcome::BudgetSpent);
        }
        if self.past_deadline() {
            return self.finish(PullOutcome::DeadlineReached);
        }
        self.restate()
    }

    fn next_fetch(&mut self) -> PullAction {
        if self.plan.bitmap.is_complete() {
            return self.finish(PullOutcome::Complete);
        }
        if self.requests_issued >= self.plan.budgets.max_requests {
            return self.finish(PullOutcome::BudgetSpent);
        }
        if self.bytes_received >= self.plan.budgets.max_bytes {
            return self.finish(PullOutcome::BudgetSpent);
        }
        if self.past_deadline() {
            return self.finish(PullOutcome::DeadlineReached);
        }
        let ranges = self.plan.bitmap.missing_ranges(
            self.plan.budgets.window_chunks,
            self.plan.budgets.max_ranges,
        );
        if ranges.is_empty() {
            return self.finish(PullOutcome::Complete);
        }
        let Some(proof) = self.proof else {
            return self.finish(PullOutcome::ProtocolError);
        };
        self.outstanding_ranges = ranges.clone();
        self.requests_issued += 1;
        self.emit(PullActionKind::Send {
            frame: PullFrame::Fetch { proof, ranges },
        })
    }

    fn past_deadline(&self) -> bool {
        self.now_ms.saturating_sub(self.started_at_ms) >= self.plan.budgets.deadline_ms
    }

    fn finish(&mut self, outcome: PullOutcome) -> PullAction {
        let summary = self.snapshot(outcome);
        self.finished = Some(summary.clone());
        self.outstanding_ranges.clear();
        let action = PullAction {
            action_id: self.next_action_id,
            kind: PullActionKind::Finish {
                close_frame: PullFrame::Close,
                summary,
            },
        };
        self.next_action_id += 1;
        self.outstanding = None;
        action
    }

    fn snapshot(&self, outcome: PullOutcome) -> PullSummary {
        PullSummary {
            blob_id: self.plan.blob_id,
            outcome,
            requests_issued: self.requests_issued,
            chunks_accepted: self.chunks_accepted,
            chunks_rejected: self.chunks_rejected,
            chunks_duplicate: self.chunks_duplicate,
            bytes_received: self.bytes_received,
            chunks_present_at_end: self.plan.bitmap.present_count(),
            chunk_count: self.plan.bitmap.chunk_count(),
            stale_results_ignored: self.stale_results_ignored,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.now_ms,
        }
    }

    fn emit(&mut self, kind: PullActionKind) -> PullAction {
        let action = PullAction {
            action_id: self.next_action_id,
            kind,
        };
        self.next_action_id += 1;
        self.outstanding = Some(action.clone());
        action
    }

    /// Re-state what is outstanding without emitting anything new.
    fn restate(&mut self) -> PullAction {
        if let Some(summary) = &self.finished {
            return PullAction {
                action_id: self.next_action_id,
                kind: PullActionKind::Finished {
                    summary: summary.clone(),
                },
            };
        }
        match &self.outstanding {
            Some(action) => PullAction {
                action_id: action.action_id,
                kind: PullActionKind::Await,
            },
            None => PullAction {
                action_id: 0,
                kind: PullActionKind::Await,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Responder
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServeOutcome {
    /// The peer closed after being served.
    Closed,
    Refused {
        reason: RefusalReason,
    },
    BudgetSpent,
    DeadlineReached,
    ProtocolError,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServeSummary {
    pub blob_id: BlobId,
    pub outcome: ServeOutcome,
    pub fetches_answered: u32,
    pub chunks_served: u32,
    pub bytes_served: u64,
    pub chunks_skipped_not_held: u32,
    pub proof_failures: u32,
    pub started_at_ms: i64,
    pub ended_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServeActionKind {
    /// Send this frame.
    Reply {
        frame: PullFrame,
    },
    /// Read exactly these ciphertext bytes from the blob's file and hand them
    /// back through [`ServeSession::provide_chunk`]. Core never opens a file;
    /// the offset and length come from the geometry, so the driver does no
    /// arithmetic of its own.
    ReadChunk {
        index: u32,
        offset: u64,
        len: u32,
    },
    /// Nothing to do until the peer sends something.
    Idle,
    Finished {
        summary: ServeSummary,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServeAction {
    pub action_id: u64,
    pub kind: ServeActionKind,
}

/// What a responder needs before it answers: which blob, what it holds, and
/// the key — used **only** to verify a requester's manifest-possession proof.
/// The bytes it serves are never decrypted.
#[derive(Clone, Debug)]
pub struct ServePlan {
    pub blob_id: BlobId,
    pub blob_key: BlobKey,
    pub geometry: BlobGeometry,
    pub held: ChunkBitmap,
    pub budgets: ServeBudgets,
}

/// The holder's half of a transfer.
pub struct ServeSession {
    plan: ServePlan,
    nonce: [u8; PULL_NONCE_LEN],
    started_at_ms: i64,
    now_ms: i64,
    next_action_id: u64,
    opened: bool,
    challenged: bool,
    queue: Vec<u32>,
    serving: Option<u32>,
    finished: Option<ServeSummary>,
    fetches_answered: u32,
    /// Whether the batch-done for the current run of answered fetches has
    /// already been emitted; reset by the next accepted fetch. Without it,
    /// an idle driver polling `next()` would be handed duplicate frames.
    batch_done_sent: bool,
    chunks_served: u32,
    bytes_served: u64,
    chunks_skipped_not_held: u32,
    proof_failures: u32,
}

impl ServeSession {
    /// `nonce` is the session challenge; production callers pass
    /// [`super::wire::generate_pull_nonce`]. It is an argument rather than
    /// generated inside so a test can drive both roles deterministically.
    pub fn new(plan: ServePlan, nonce: [u8; PULL_NONCE_LEN], now_ms: i64) -> Self {
        ServeSession {
            plan,
            nonce,
            started_at_ms: now_ms,
            now_ms,
            next_action_id: 1,
            opened: false,
            challenged: false,
            queue: Vec::new(),
            serving: None,
            finished: None,
            fetches_answered: 0,
            batch_done_sent: false,
            chunks_served: 0,
            bytes_served: 0,
            chunks_skipped_not_held: 0,
            proof_failures: 0,
        }
    }

    pub fn on_frame(&mut self, frame: PullFrame, now_ms: i64) -> ServeAction {
        if self.finished.is_some() {
            return self.finished_action();
        }
        self.now_ms = now_ms;
        if self.past_deadline() {
            return self.refuse(RefusalReason::BudgetSpent, ServeOutcome::DeadlineReached);
        }
        match frame {
            PullFrame::Open { blob_id } => {
                if self.opened {
                    return self.refuse(RefusalReason::BadRequest, ServeOutcome::ProtocolError);
                }
                self.opened = true;
                if blob_id != self.plan.blob_id || self.plan.held.present_count() == 0 {
                    // BLOB-01/BLOB-03, from the serving side: a device offers
                    // only what it holds for itself. It never goes looking,
                    // never asks a third party, and never carries a blob for
                    // someone else.
                    return self.refuse(
                        RefusalReason::NotHeld,
                        ServeOutcome::Refused {
                            reason: RefusalReason::NotHeld,
                        },
                    );
                }
                self.challenged = true;
                self.emit(ServeActionKind::Reply {
                    frame: PullFrame::Challenge {
                        nonce: self.nonce,
                        chunks_held: self.plan.held.present_count(),
                    },
                })
            }
            PullFrame::Fetch { proof, ranges } => self.apply_fetch(proof, ranges),
            PullFrame::Close => {
                let summary = self.snapshot(ServeOutcome::Closed);
                self.finished = Some(summary);
                self.finished_action()
            }
            // A responder never receives these.
            PullFrame::Challenge { .. }
            | PullFrame::Chunk { .. }
            | PullFrame::BatchDone { .. }
            | PullFrame::Refused { .. } => {
                self.refuse(RefusalReason::BadRequest, ServeOutcome::ProtocolError)
            }
        }
    }

    /// Hand back the bytes a [`ServeActionKind::ReadChunk`] asked for.
    pub fn provide_chunk(&mut self, index: u32, ciphertext: Vec<u8>, now_ms: i64) -> ServeAction {
        if self.finished.is_some() {
            return self.finished_action();
        }
        self.now_ms = now_ms;
        if self.serving != Some(index) {
            // A driver answering a read this session did not ask for changes
            // nothing, exactly as a stale HTTP result does in a relay pass.
            return self.next(now_ms);
        }
        let expected = self.plan.geometry.chunk_ciphertext_len(index);
        if expected != Some(ciphertext.len() as u32) {
            // The file on disk does not match the geometry. Serving it would
            // hand a peer bytes that cannot verify, so the session ends.
            return self.refuse(RefusalReason::NotHeld, ServeOutcome::ProtocolError);
        }
        self.serving = None;
        self.chunks_served += 1;
        self.bytes_served = self.bytes_served.saturating_add(ciphertext.len() as u64);
        self.emit(ServeActionKind::Reply {
            frame: PullFrame::Chunk { index, ciphertext },
        })
    }

    /// What to do after the last action was carried out.
    pub fn next(&mut self, now_ms: i64) -> ServeAction {
        if self.finished.is_some() {
            return self.finished_action();
        }
        self.now_ms = now_ms;
        if self.serving.is_some() {
            // Still waiting on the driver's read.
            return self.restate_read();
        }
        if self.queue.is_empty() {
            if self.challenged && self.fetches_answered > 0 && !self.batch_done_sent {
                self.batch_done_sent = true;
                return self.emit(ServeActionKind::Reply {
                    frame: PullFrame::BatchDone {
                        chunks_served: self.chunks_served,
                    },
                });
            }
            return self.emit(ServeActionKind::Idle);
        }
        if self.past_deadline() {
            return self.refuse(RefusalReason::BudgetSpent, ServeOutcome::DeadlineReached);
        }
        if self.chunks_served >= self.plan.budgets.max_chunks
            || self.bytes_served >= self.plan.budgets.max_bytes
        {
            return self.refuse(RefusalReason::BudgetSpent, ServeOutcome::BudgetSpent);
        }
        let index = self.queue.remove(0);
        self.serving = Some(index);
        self.restate_read()
    }

    pub fn cancel(&mut self, now_ms: i64) -> ServeSummary {
        self.now_ms = now_ms;
        if self.finished.is_none() {
            self.finished = Some(self.snapshot(ServeOutcome::Cancelled));
        }
        self.summary()
    }

    pub fn summary(&self) -> ServeSummary {
        self.finished
            .clone()
            .unwrap_or_else(|| self.snapshot(ServeOutcome::Cancelled))
    }

    fn apply_fetch(&mut self, proof: [u8; PULL_PROOF_LEN], ranges: Vec<ChunkRange>) -> ServeAction {
        if !self.challenged {
            return self.refuse(RefusalReason::BadRequest, ServeOutcome::ProtocolError);
        }
        if !verify_pull_proof(&self.plan.blob_key, &self.plan.blob_id, &self.nonce, &proof) {
            self.proof_failures += 1;
            return self.refuse(
                RefusalReason::ProofInvalid,
                ServeOutcome::Refused {
                    reason: RefusalReason::ProofInvalid,
                },
            );
        }
        if self.fetches_answered >= self.plan.budgets.max_fetches {
            return self.refuse(RefusalReason::BudgetSpent, ServeOutcome::BudgetSpent);
        }
        if ranges.is_empty() || ranges.len() as u32 > PULL_MAX_RANGES_PER_FETCH {
            return self.refuse(RefusalReason::BadRequest, ServeOutcome::ProtocolError);
        }
        let mut queue = Vec::new();
        for range in &ranges {
            if range.count == 0 || range.end_exclusive() > u64::from(self.plan.geometry.chunk_count)
            {
                return self.refuse(RefusalReason::BadRequest, ServeOutcome::ProtocolError);
            }
            for index in range.start..range.start + range.count {
                if self.plan.held.has(index) {
                    queue.push(index);
                } else {
                    self.chunks_skipped_not_held += 1;
                }
            }
        }
        self.fetches_answered += 1;
        self.batch_done_sent = false;
        self.queue = queue;
        self.next(self.now_ms)
    }

    fn refuse(&mut self, reason: RefusalReason, outcome: ServeOutcome) -> ServeAction {
        let summary = self.snapshot(outcome);
        self.finished = Some(summary);
        self.queue.clear();
        self.serving = None;
        let action = ServeAction {
            action_id: self.next_action_id,
            kind: ServeActionKind::Reply {
                frame: PullFrame::Refused { reason },
            },
        };
        self.next_action_id += 1;
        action
    }

    fn restate_read(&mut self) -> ServeAction {
        let index = self.serving.expect("a read is outstanding");
        let offset = self
            .plan
            .geometry
            .chunk_ciphertext_offset(index)
            .expect("held chunks are inside the geometry");
        let len = self
            .plan
            .geometry
            .chunk_ciphertext_len(index)
            .expect("held chunks are inside the geometry");
        self.emit(ServeActionKind::ReadChunk { index, offset, len })
    }

    fn past_deadline(&self) -> bool {
        self.now_ms.saturating_sub(self.started_at_ms) >= self.plan.budgets.deadline_ms
    }

    fn finished_action(&mut self) -> ServeAction {
        let summary = self.summary();
        let action = ServeAction {
            action_id: self.next_action_id,
            kind: ServeActionKind::Finished { summary },
        };
        self.next_action_id += 1;
        action
    }

    fn snapshot(&self, outcome: ServeOutcome) -> ServeSummary {
        ServeSummary {
            blob_id: self.plan.blob_id,
            outcome,
            fetches_answered: self.fetches_answered,
            chunks_served: self.chunks_served,
            bytes_served: self.bytes_served,
            chunks_skipped_not_held: self.chunks_skipped_not_held,
            proof_failures: self.proof_failures,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.now_ms,
        }
    }

    fn emit(&mut self, kind: ServeActionKind) -> ServeAction {
        let action = ServeAction {
            action_id: self.next_action_id,
            kind,
        };
        self.next_action_id += 1;
        action
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::blob::{seal_blob, test_key, SealedBlob};
    use crate::media::wire::{decode_pull_frame, encode_pull_frame};
    use crate::media::MEDIA_CHUNK_PLAINTEXT_BYTES;

    fn sealed(chunks: u64) -> SealedBlob {
        let len = chunks * u64::from(MEDIA_CHUNK_PLAINTEXT_BYTES) - 17;
        let plaintext: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        seal_blob(&test_key(42), &plaintext).unwrap()
    }

    fn full_bitmap(chunks: u32) -> ChunkBitmap {
        let mut bitmap = ChunkBitmap::empty(chunks).unwrap();
        for index in 0..chunks {
            bitmap.set(index);
        }
        bitmap
    }

    fn pull_plan(blob: &SealedBlob, bitmap: ChunkBitmap) -> PullPlan {
        PullPlan {
            blob_id: blob.id,
            blob_key: test_key(42),
            geometry: blob.geometry,
            bitmap,
            budgets: PullBudgets::default(),
        }
    }

    fn serve_plan(blob: &SealedBlob, held: ChunkBitmap) -> ServePlan {
        ServePlan {
            blob_id: blob.id,
            blob_key: test_key(42),
            geometry: blob.geometry,
            held,
            budgets: ServeBudgets::default(),
        }
    }

    fn chunk_bytes(blob: &SealedBlob, index: u32) -> Vec<u8> {
        let offset = blob.geometry.chunk_ciphertext_offset(index).unwrap() as usize;
        let len = blob.geometry.chunk_ciphertext_len(index).unwrap() as usize;
        blob.ciphertext[offset..offset + len].to_vec()
    }

    /// Drive both state machines against each other. `corrupt` names chunks
    /// the "link" damages on the way across; `clock` is a fake one.
    struct Wire {
        blob: SealedBlob,
        corrupt: Vec<u32>,
        now_ms: i64,
        step_ms: i64,
        accepted: Vec<AcceptedChunk>,
        frames: u32,
    }

    impl Wire {
        fn new(blob: SealedBlob) -> Self {
            Wire {
                blob,
                corrupt: Vec::new(),
                now_ms: 1_000,
                step_ms: 10,
                accepted: Vec::new(),
                frames: 0,
            }
        }

        fn run(&mut self, pull: &mut PullSession, serve: &mut ServeSession) -> PullSummary {
            let mut action = pull.start(self.now_ms);
            loop {
                self.frames += 1;
                assert!(self.frames < 10_000, "the pull loop must terminate");
                self.now_ms += self.step_ms;
                let outbound = match action.kind {
                    PullActionKind::Send { frame } => frame,
                    PullActionKind::Await => panic!("nothing was sent and nothing is pending"),
                    PullActionKind::Finish { summary, .. }
                    | PullActionKind::Finished { summary } => {
                        self.accepted.extend(pull.take_accepted());
                        return summary;
                    }
                };
                // Every frame crosses the wire encoded, so the codec is on the
                // path a test exercises rather than beside it.
                let outbound = decode_pull_frame(&encode_pull_frame(&outbound).unwrap()).unwrap();

                let mut replies = Vec::new();
                let mut serve_action = serve.on_frame(outbound, self.now_ms);
                loop {
                    match serve_action.kind {
                        ServeActionKind::Reply { frame } => {
                            let done = matches!(frame, PullFrame::BatchDone { .. })
                                || matches!(frame, PullFrame::Refused { .. });
                            replies.push(frame);
                            if done {
                                break;
                            }
                            serve_action = serve.next(self.now_ms);
                        }
                        ServeActionKind::ReadChunk { index, offset, len } => {
                            let mut bytes = chunk_bytes(&self.blob, index);
                            assert_eq!(
                                offset,
                                self.blob.geometry.chunk_ciphertext_offset(index).unwrap()
                            );
                            assert_eq!(len as usize, bytes.len());
                            if self.corrupt.contains(&index) {
                                bytes[0] ^= 0xFF;
                            }
                            serve_action = serve.provide_chunk(index, bytes, self.now_ms);
                        }
                        ServeActionKind::Idle | ServeActionKind::Finished { .. } => break,
                    }
                }

                let action_id = action.action_id;
                let mut next = None;
                for frame in replies {
                    self.now_ms += self.step_ms;
                    let frame = decode_pull_frame(&encode_pull_frame(&frame).unwrap()).unwrap();
                    let produced = pull.resume(PullResult::Frame { action_id, frame }, self.now_ms);
                    self.accepted.extend(pull.take_accepted());
                    if !matches!(produced.kind, PullActionKind::Await) {
                        next = Some(produced);
                        break;
                    }
                }
                action = match next {
                    Some(action) => action,
                    None => panic!("the responder stopped answering mid-batch"),
                };
            }
        }

        /// Reassemble what the requester accepted, in index order.
        fn assembled(&self) -> Vec<u8> {
            let mut chunks = self.accepted.clone();
            chunks.sort_by_key(|chunk| chunk.index);
            chunks
                .into_iter()
                .flat_map(|chunk| chunk.ciphertext)
                .collect()
        }
    }

    #[test]
    fn a_whole_blob_transfers_and_verifies() {
        let blob = sealed(5);
        let mut wire = Wire::new(blob.clone());
        let mut pull = PullSession::new(pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        ));
        let mut serve = ServeSession::new(
            serve_plan(&blob, full_bitmap(blob.geometry.chunk_count)),
            [0x01; PULL_NONCE_LEN],
            1_000,
        );

        let summary = wire.run(&mut pull, &mut serve);
        assert_eq!(summary.outcome, PullOutcome::Complete);
        assert_eq!(summary.chunks_accepted, blob.geometry.chunk_count);
        assert_eq!(summary.chunks_rejected, 0);
        assert_eq!(summary.bytes_received, blob.geometry.ciphertext_bytes);
        assert!(pull.bitmap().is_complete());

        // BLOB-05: the assembled ciphertext verifies against the manifest
        // digest, and only then is it opened.
        let assembled = wire.assembled();
        crate::media::blob::verify_assembled(&blob.id, &blob.geometry, &assembled).unwrap();
        let plaintext =
            crate::media::blob::open_blob(&test_key(42), &blob.id, &blob.geometry, &assembled)
                .unwrap();
        assert_eq!(plaintext.len() as u64, blob.geometry.plaintext_bytes);
    }

    #[test]
    fn a_transfer_resumes_from_a_partial_bitmap_and_asks_only_for_the_gaps() {
        let blob = sealed(6);
        let mut bitmap = ChunkBitmap::empty(blob.geometry.chunk_count).unwrap();
        bitmap.set(0);
        bitmap.set(1);
        bitmap.set(4);

        let mut wire = Wire::new(blob.clone());
        let mut pull = PullSession::new(pull_plan(&blob, bitmap));
        let mut serve = ServeSession::new(
            serve_plan(&blob, full_bitmap(blob.geometry.chunk_count)),
            [0x02; PULL_NONCE_LEN],
            1_000,
        );

        let summary = wire.run(&mut pull, &mut serve);
        assert_eq!(summary.outcome, PullOutcome::Complete);
        assert_eq!(
            summary.chunks_accepted, 3,
            "only the missing chunks cross the wire"
        );
        assert_eq!(
            wire.accepted
                .iter()
                .map(|chunk| chunk.index)
                .collect::<Vec<_>>(),
            vec![2, 3, 5]
        );
        assert_eq!(serve.summary().chunks_served, 3);
    }

    #[test]
    fn a_corrupted_chunk_is_rejected_re_requested_and_never_stored() {
        let blob = sealed(3);
        let mut wire = Wire::new(blob.clone());
        wire.corrupt = vec![1];
        let mut pull = PullSession::new(pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        ));
        let mut serve = ServeSession::new(
            serve_plan(&blob, full_bitmap(blob.geometry.chunk_count)),
            [0x03; PULL_NONCE_LEN],
            1_000,
        );

        let summary = wire.run(&mut pull, &mut serve);
        // The link damages chunk 1 every single time, so the transfer never
        // completes — but it also never accepts the bad bytes, never marks
        // chunk 1 present, and terminates inside a budget rather than looping
        // for ever.
        assert!(summary.chunks_rejected > 0);
        assert!(!pull.bitmap().has(1));
        assert!(wire.accepted.iter().all(|chunk| chunk.index != 1));
        assert!(
            matches!(
                summary.outcome,
                PullOutcome::BudgetSpent | PullOutcome::DeadlineReached
            ),
            "a chunk that can never be fetched must end in a budget, got {:?}",
            summary.outcome
        );
    }

    #[test]
    fn a_requester_without_the_manifest_is_refused() {
        let blob = sealed(2);
        let mut pull = PullSession::new(PullPlan {
            // The right blob id, the wrong key: someone who saw a digest but
            // was never sealed a manifest.
            blob_key: test_key(99),
            ..pull_plan(
                &blob,
                ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
            )
        });
        let mut serve = ServeSession::new(
            serve_plan(&blob, full_bitmap(blob.geometry.chunk_count)),
            [0x04; PULL_NONCE_LEN],
            1_000,
        );
        let mut wire = Wire::new(blob.clone());

        let summary = wire.run(&mut pull, &mut serve);
        assert_eq!(
            summary.outcome,
            PullOutcome::Refused {
                reason: RefusalReason::ProofInvalid
            }
        );
        assert_eq!(serve.summary().chunks_served, 0);
        assert_eq!(serve.summary().proof_failures, 1);
        assert!(wire.accepted.is_empty(), "not one byte was served");
    }

    #[test]
    fn a_responder_that_does_not_hold_the_blob_says_so_and_serves_nothing() {
        let blob = sealed(2);
        let mut serve = ServeSession::new(
            serve_plan(
                &blob,
                ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
            ),
            [0x05; PULL_NONCE_LEN],
            1_000,
        );
        let action = serve.on_frame(PullFrame::Open { blob_id: blob.id }, 1_000);
        assert_eq!(
            action.kind,
            ServeActionKind::Reply {
                frame: PullFrame::Refused {
                    reason: RefusalReason::NotHeld
                }
            }
        );
        assert_eq!(serve.summary().chunks_served, 0);
    }

    #[test]
    fn a_partial_holder_serves_what_it_has_and_says_what_it_could_not() {
        let blob = sealed(4);
        let mut held = ChunkBitmap::empty(blob.geometry.chunk_count).unwrap();
        held.set(0);
        held.set(2);
        let mut serve = ServeSession::new(serve_plan(&blob, held), [0x06; PULL_NONCE_LEN], 1_000);
        let mut pull = PullSession::new(pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        ));
        let mut wire = Wire::new(blob.clone());

        let summary = wire.run(&mut pull, &mut serve);
        assert_eq!(
            wire.accepted
                .iter()
                .map(|chunk| chunk.index)
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
        assert!(!summary.is_complete());
        assert!(serve.summary().chunks_skipped_not_held > 0);
        assert_eq!(pull.bitmap().present_count(), 2);
    }

    #[test]
    fn a_fetch_beyond_the_blob_is_refused_rather_than_clamped() {
        let blob = sealed(2);
        let mut serve = ServeSession::new(
            serve_plan(&blob, full_bitmap(blob.geometry.chunk_count)),
            [0x07; PULL_NONCE_LEN],
            1_000,
        );
        serve.on_frame(PullFrame::Open { blob_id: blob.id }, 1_000);
        let proof = pull_proof(&test_key(42), &blob.id, &[0x07; PULL_NONCE_LEN]);
        let action = serve.on_frame(
            PullFrame::Fetch {
                proof,
                ranges: vec![ChunkRange {
                    start: 0,
                    count: u32::MAX,
                }],
            },
            1_100,
        );
        assert_eq!(
            action.kind,
            ServeActionKind::Reply {
                frame: PullFrame::Refused {
                    reason: RefusalReason::BadRequest
                }
            }
        );
    }

    #[test]
    fn a_fetch_before_the_challenge_is_refused() {
        let blob = sealed(2);
        let mut serve = ServeSession::new(
            serve_plan(&blob, full_bitmap(blob.geometry.chunk_count)),
            [0x08; PULL_NONCE_LEN],
            1_000,
        );
        let action = serve.on_frame(
            PullFrame::Fetch {
                proof: [0; PULL_PROOF_LEN],
                ranges: vec![ChunkRange { start: 0, count: 1 }],
            },
            1_000,
        );
        assert_eq!(
            action.kind,
            ServeActionKind::Reply {
                frame: PullFrame::Refused {
                    reason: RefusalReason::BadRequest
                }
            }
        );
    }

    #[test]
    fn the_requester_stays_inside_its_declared_budgets() {
        let blob = sealed(40);
        let mut plan = pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        );
        plan.budgets.max_bytes = u64::from(blob.geometry.chunk_ciphertext_bytes) * 4;
        let mut pull = PullSession::new(plan);
        let mut serve = ServeSession::new(
            serve_plan(&blob, full_bitmap(blob.geometry.chunk_count)),
            [0x09; PULL_NONCE_LEN],
            1_000,
        );
        let mut wire = Wire::new(blob.clone());

        let summary = wire.run(&mut pull, &mut serve);
        assert_eq!(summary.outcome, PullOutcome::BudgetSpent);
        assert!(
            summary.bytes_received <= u64::from(blob.geometry.chunk_ciphertext_bytes) * 5,
            "the byte budget is a stop, not a target: {}",
            summary.bytes_received
        );
        assert!(!pull.bitmap().is_complete());
        assert!(
            pull.bitmap().present_count() > 0,
            "a spent budget is a pause; the next session resumes from here"
        );
    }

    #[test]
    fn a_request_budget_of_one_stops_after_one_window() {
        let blob = sealed(40);
        let mut plan = pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        );
        plan.budgets.max_requests = 1;
        let mut pull = PullSession::new(plan);
        let mut serve = ServeSession::new(
            serve_plan(&blob, full_bitmap(blob.geometry.chunk_count)),
            [0x0A; PULL_NONCE_LEN],
            1_000,
        );
        let mut wire = Wire::new(blob.clone());

        let summary = wire.run(&mut pull, &mut serve);
        assert_eq!(summary.outcome, PullOutcome::BudgetSpent);
        assert_eq!(summary.requests_issued, 1);
        assert_eq!(summary.chunks_accepted, PULL_WINDOW_CHUNKS);
    }

    #[test]
    fn the_deadline_ends_a_session_that_would_otherwise_keep_going() {
        let blob = sealed(40);
        let mut plan = pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        );
        plan.budgets.deadline_ms = 500;
        let mut pull = PullSession::new(plan);
        let mut serve = ServeSession::new(
            serve_plan(&blob, full_bitmap(blob.geometry.chunk_count)),
            [0x0B; PULL_NONCE_LEN],
            1_000,
        );
        let mut wire = Wire::new(blob.clone());
        wire.step_ms = 40;

        let summary = wire.run(&mut pull, &mut serve);
        assert_eq!(summary.outcome, PullOutcome::DeadlineReached);
        assert!(summary.ended_at_ms - summary.started_at_ms >= 500);
    }

    #[test]
    fn the_responder_stays_inside_its_declared_budgets() {
        let blob = sealed(40);
        let mut plan = serve_plan(&blob, full_bitmap(blob.geometry.chunk_count));
        plan.budgets.max_chunks = 3;
        let mut serve = ServeSession::new(plan, [0x0C; PULL_NONCE_LEN], 1_000);
        let mut pull = PullSession::new(pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        ));
        let mut wire = Wire::new(blob.clone());

        let summary = wire.run(&mut pull, &mut serve);
        assert_eq!(
            summary.outcome,
            PullOutcome::Refused {
                reason: RefusalReason::BudgetSpent
            }
        );
        assert_eq!(serve.summary().chunks_served, 3);
        assert_eq!(serve.summary().outcome, ServeOutcome::BudgetSpent);
    }

    #[test]
    fn batch_done_is_emitted_once_and_further_polls_idle() {
        // The driver contract says to call next() after carrying out the last
        // action. A driver that does exactly that after sending BatchDone must
        // be told there is nothing to do -- not handed the frame again forever.
        let blob = sealed(4);
        let plan = serve_plan(&blob, full_bitmap(blob.geometry.chunk_count));
        let mut serve = ServeSession::new(plan, [0x0C; PULL_NONCE_LEN], 1_000);
        let mut pull = PullSession::new(pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        ));
        let mut wire = Wire::new(blob.clone());
        let summary = wire.run(&mut pull, &mut serve);
        assert_eq!(summary.outcome, PullOutcome::Complete);

        for _ in 0..3 {
            let action = serve.next(2_000);
            assert!(
                matches!(action.kind, ServeActionKind::Idle),
                "after batch-done the responder idles, got {:?}",
                action.kind
            );
        }
    }

    #[test]
    fn a_stale_or_duplicate_result_changes_nothing() {
        let blob = sealed(3);
        let mut pull = PullSession::new(pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        ));
        let open = pull.start(1_000);
        let PullActionKind::Send { .. } = open.kind else {
            panic!("the first action opens the session");
        };

        // A result for an action id this session never emitted.
        let ignored = pull.resume(
            PullResult::Frame {
                action_id: open.action_id + 7,
                frame: PullFrame::Challenge {
                    nonce: [0x0D; PULL_NONCE_LEN],
                    chunks_held: 3,
                },
            },
            1_010,
        );
        assert_eq!(ignored.kind, PullActionKind::Await);
        assert_eq!(ignored.action_id, open.action_id);

        // Now the real one, then the same one again.
        let fetch = pull.resume(
            PullResult::Frame {
                action_id: open.action_id,
                frame: PullFrame::Challenge {
                    nonce: [0x0D; PULL_NONCE_LEN],
                    chunks_held: 3,
                },
            },
            1_020,
        );
        assert!(matches!(fetch.kind, PullActionKind::Send { .. }));
        let replayed = pull.resume(
            PullResult::Frame {
                action_id: open.action_id,
                frame: PullFrame::Challenge {
                    nonce: [0x0E; PULL_NONCE_LEN],
                    chunks_held: 3,
                },
            },
            1_030,
        );
        assert_eq!(
            replayed.kind,
            PullActionKind::Await,
            "a replayed challenge must not re-open the session or change the nonce"
        );
        assert_eq!(pull.summary().stale_results_ignored, 2);
        assert_eq!(pull.summary().requests_issued, 1);
    }

    #[test]
    fn an_unrequested_chunk_is_refused_even_though_it_would_authenticate() {
        let blob = sealed(4);
        let mut pull = PullSession::new(pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        ));
        let open = pull.start(1_000);
        let fetch = pull.resume(
            PullResult::Frame {
                action_id: open.action_id,
                frame: PullFrame::Challenge {
                    nonce: [0x0F; PULL_NONCE_LEN],
                    chunks_held: 4,
                },
            },
            1_010,
        );
        let PullActionKind::Send {
            frame: PullFrame::Fetch { ranges, .. },
        } = &fetch.kind
        else {
            panic!("a fetch follows the challenge");
        };
        assert_eq!(ranges, &vec![ChunkRange { start: 0, count: 4 }]);

        // A perfectly valid chunk of a blob nobody asked for right now.
        let mut narrow = pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        );
        narrow.budgets.window_chunks = 1;
        let mut narrow_pull = PullSession::new(narrow);
        let open = narrow_pull.start(1_000);
        narrow_pull.resume(
            PullResult::Frame {
                action_id: open.action_id,
                frame: PullFrame::Challenge {
                    nonce: [0x10; PULL_NONCE_LEN],
                    chunks_held: 4,
                },
            },
            1_010,
        );
        let outstanding = narrow_pull.resume(
            PullResult::Frame {
                action_id: 2,
                frame: PullFrame::Chunk {
                    index: 3,
                    ciphertext: chunk_bytes(&blob, 3),
                },
            },
            1_020,
        );
        assert_eq!(outstanding.kind, PullActionKind::Await);
        assert!(
            !narrow_pull.bitmap().has(3),
            "a chunk outside the window is not progress, however valid it is"
        );
        assert_eq!(narrow_pull.summary().chunks_rejected, 1);
    }

    #[test]
    fn a_complete_bitmap_opens_no_session_at_all() {
        let blob = sealed(2);
        let mut pull = PullSession::new(pull_plan(&blob, full_bitmap(blob.geometry.chunk_count)));
        let action = pull.start(1_000);
        let PullActionKind::Finish { summary, .. } = action.kind else {
            panic!("a complete bitmap finishes immediately");
        };
        assert_eq!(summary.outcome, PullOutcome::NothingMissing);
        assert_eq!(summary.requests_issued, 0);
    }

    #[test]
    fn cancellation_keeps_what_landed() {
        let blob = sealed(8);
        let mut pull = PullSession::new(pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        ));
        let open = pull.start(1_000);
        pull.resume(
            PullResult::Frame {
                action_id: open.action_id,
                frame: PullFrame::Challenge {
                    nonce: [0x11; PULL_NONCE_LEN],
                    chunks_held: 8,
                },
            },
            1_010,
        );
        pull.resume(
            PullResult::Frame {
                action_id: 2,
                frame: PullFrame::Chunk {
                    index: 0,
                    ciphertext: chunk_bytes(&blob, 0),
                },
            },
            1_020,
        );
        let summary = pull.cancel(1_030);
        assert_eq!(summary.outcome, PullOutcome::Cancelled);
        assert_eq!(summary.chunks_accepted, 1);
        assert!(pull.bitmap().has(0));
        // And a cancelled session stays cancelled.
        let after = pull.start(1_040);
        assert!(matches!(after.kind, PullActionKind::Finished { .. }));
    }

    #[test]
    fn a_transport_failure_ends_the_session_without_losing_progress() {
        let blob = sealed(4);
        let mut pull = PullSession::new(pull_plan(
            &blob,
            ChunkBitmap::empty(blob.geometry.chunk_count).unwrap(),
        ));
        let open = pull.start(1_000);
        let action = pull.resume(
            PullResult::TransportError {
                action_id: open.action_id,
                error: PullTransportError::ConnectionLost,
            },
            1_010,
        );
        let PullActionKind::Finish { summary, .. } = action.kind else {
            panic!("a lost link ends the session");
        };
        assert_eq!(summary.outcome, PullOutcome::TransportFailed);
    }

    #[test]
    fn a_responder_never_answers_a_frame_only_a_responder_sends() {
        let blob = sealed(2);
        let mut serve = ServeSession::new(
            serve_plan(&blob, full_bitmap(blob.geometry.chunk_count)),
            [0x12; PULL_NONCE_LEN],
            1_000,
        );
        let action = serve.on_frame(PullFrame::BatchDone { chunks_served: 3 }, 1_000);
        assert_eq!(
            action.kind,
            ServeActionKind::Reply {
                frame: PullFrame::Refused {
                    reason: RefusalReason::BadRequest
                }
            }
        );
        assert_eq!(serve.summary().outcome, ServeOutcome::ProtocolError);
    }
}
