//! The session namespace: protocol decisions a relay pass or a mesh encounter
//! makes, owned once, in Rust.
//!
//! Everything here is pure policy over explicit inputs — an explicit `now_ms`,
//! stable public identity bytes, and the ordered results of external work. No
//! module in here opens a socket, holds a database transaction across I/O, or
//! reads an ambient clock. The shells keep the timers, threads, locks and HTTP
//! clients; they stop keeping the arithmetic.
//!
//! The first resident is [`relay_policy`], which owns family request pacing,
//! the 429 backoff curve, the `Retry-After` floor, the stable per-identity
//! jitter offset, the pending-rerun decision, and the pass health fold.

pub mod relay_policy;
