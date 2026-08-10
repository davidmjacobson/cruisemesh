//! The session namespace: protocol decisions a relay pass or a mesh encounter
//! makes, owned once, in Rust.
//!
//! Everything here is pure policy over explicit inputs — an explicit `now_ms`,
//! stable public identity bytes, and the ordered results of external work. No
//! module in here opens a socket, holds a database transaction across I/O, or
//! reads an ambient clock. The shells keep the timers, threads, locks and HTTP
//! clients; they stop keeping the arithmetic.
//!
//! [`relay_policy`] owns family request pacing, the 429 backoff curve, the
//! `Retry-After` floor, the stable per-identity jitter offset, the
//! pending-rerun decision, and the pass health fold.
//!
//! [`relay_pass`] owns the relay pass itself: the ordered stages, request
//! formation, response decoding, store transactions, ack eligibility, cursor
//! advancement, silence evidence, budgets and continuation. It consumes
//! `relay_policy`'s decisions rather than repeating them, and on Android it
//! is reachable only behind a whole-pass engine selection that defaults to
//! the legacy engine.
//!
//! [`mesh_receive`] owns the inbound transaction: one call takes a peer frame
//! and an explicit `now_ms`, runs the production dedupe/expiry/open/authorize/
//! carry disposition in the store's own short transactions, and returns bounded
//! intents. It is the single authority `core/tests/mesh_sim.rs` and (in D1) the
//! shells' inbound processors call, replacing their duplicated receive logic.
//!
//! [`relay_shadow`] is the migration canary's read-only planner: pure
//! functions over values a shell captured from a legacy pass, so the two
//! engines can be compared without either of them running twice. It calls
//! `relay_pass`'s own planning helpers rather than restating them, and it is
//! deleted with the legacy engine it exists to check.

pub mod mesh_receive;
pub mod relay_pass;
pub mod relay_policy;
pub mod relay_shadow;
