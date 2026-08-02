//! CruiseMesh relay mailbox (`cruisemesh-relayd`).
//!
//! DESIGN.md §9: a deliberately dumb, content-agnostic mailbox for sealed
//! envelopes. The server stores the public envelope header shape
//! (`msg_id`, `hop_ttl`, `expiry_ms`, `recipient_hint`, `sealed`) and never
//! inspects ciphertext. That means text, cumulative receipts (`kind=2`),
//! friend-request envelopes (`kind=3`), and future kinds all take the same
//! path — important as clients start uploading receipt envelopes over relay.
//!
//! ## Cursor + ack semantics
//!
//! - **Fetch** (`GET /envelopes?hints=...&after=...`) returns rows with
//!   `id > after` matching any of the caller's `recipient_hint`s, ordered by
//!   `id ASC`. The server does **not** mark rows as delivered on fetch.
//! - **Re-fetch is intentional.** A client that crashes after fetch but
//!   before processing can poll again with the same (or a lower) cursor and
//!   see the same envelopes. Nothing assumes one-fetch-only delivery.
//! - **Ack** (`POST /envelopes/ack`) is the only way a row leaves before its
//!   expiry / retention deadline. Ack is scoped to the caller's family token
//!   (cross-family ids are ignored, not errors).
//! - **Cursor vs ack are independent.** Advancing `after` without acking
//!   only affects what a subsequent poll returns for that client; un-acked
//!   rows remain for any client that rewinds the cursor (or a fresh one).
//! - **msg_id dedupe** is per `(family_token, msg_id)`. Re-posting the same
//!   msg_id (e.g. a receipt envelope re-uploaded every sync with a stable
//!   watermark-derived msg_id) is idempotent: the row is kept, hop_ttl and
//!   expiry take the max, sealed bytes are not rewritten.
//!
//! ## WebSocket push (`GET /ws`)
//!
//! Live internet clients can open a WebSocket instead of (or in addition to)
//! polling. Semantics:
//!
//! 1. **Auth** — same family bearer token as REST. Accepted via
//!    `Authorization: Bearer <token>` **or** `?token=<token>` query param.
//!    Query auth exists because browser `WebSocket` cannot set headers on the
//!    handshake; native clients (our phone apps) should prefer the header so
//!    the token is not logged in proxy access logs / browser history.
//! 2. **Subscribe** — `hints=` is required (same comma-separated base64url
//!    list as `GET /envelopes`). Optional `after=` is the cursor (default 0).
//! 3. **Replay then push** — on connect the server sends every row the poll
//!    API would return for those hints since `after` (one envelope per text
//!    frame, JSON shape of a single REST fetch envelope object), then streams
//!    each newly POSTed envelope whose `(family_token, recipient_hint)`
//!    matches.
//! 4. **Acks stay REST-only** — WS is delivery only; clients still
//!    `POST /envelopes/ack`. The poll API is byte-for-byte unchanged.
//! 5. **Backpressure** — a global bounded broadcast channel fans out POSTs.
//!    Slow or dead consumers that lag past the buffer (or fail a write
//!    deadline) are **dropped**. Reconnect with the last known cursor and
//!    replay heals the gap — that is what the cursor is for. Bounded memory
//!    beats trying to buffer forever for a phone that went to sea.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::header::{AUTHORIZATION, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{info, warn};

const RECIPIENT_HINT_LEN: usize = 8;
const MSG_ID_LEN: usize = 16;
const DEFAULT_FETCH_LIMIT: usize = 100;
const MAX_FETCH_LIMIT: usize = 500;

/// The response body ceiling every first-party client enforces before it will
/// decode a fetch page (`core/src/relay_wire.rs`
/// `RELAY_MAX_RESPONSE_BODY_BYTES`, exported as `relay_max_response_bytes()`).
/// Duplicated rather than imported because `cruisemesh-core` is a dev
/// dependency here, not a runtime one; `client_body_cap_matches_the_core`
/// pins the two values together the same way the deposit-token golden vector
/// pins that derivation.
const CLIENT_MAX_RESPONSE_BODY_BYTES: usize = 12 * 1024 * 1024;

/// Upper bound on the JSON scaffolding one `EnvelopeResponse` costs, on top
/// of the base64 `sealed` string.
///
/// Counted, not guessed: field names and punctuation
/// (`{"id":,"msg_id":"","hop_ttl":,"recipient_hint":"","sealed":"","expiry_ms":,"created_at_ms":},`)
/// are 110 bytes; `msg_id` is 16 bytes base64url-unpadded = 22 chars;
/// `recipient_hint` is 8 bytes = 11 chars; the four numbers are at most 20 +
/// 3 + 20 + 20 digits. That is 206. Rounded up to 256 for headroom.
const MAX_FETCH_ROW_OVERHEAD_BYTES: usize = 256;

/// Cumulative budget, in *decoded* `sealed` bytes, for one fetch page.
///
/// ### Why a byte budget exists at all
///
/// `LIMIT` bounds a page's row count, not its size. `sealed` may be up to
/// `MAX_ENVELOPE_SEALED_BYTES` (512 KiB) and goes out base64-encoded inside
/// JSON, so a mailbox holding enough large attachment chunks can fill a
/// row-counted window with a body far past what a client will accept. The
/// client then rejects the body, retries the identical window from the
/// identical cursor on the next pass, and gets the identical answer: the
/// frontier never moves and the mailbox stalls until those rows expire.
///
/// ### Deriving the number
///
/// Work backwards from the client's 12 MiB body cap
/// (`CLIENT_MAX_RESPONSE_BODY_BYTES` = 12,582,912) and require the worst-case
/// page built under the budget to fit inside it:
///
/// 1. base64 of B decoded bytes is `ceil(B / 3) * 4` — a 4/3 expansion.
/// 2. Row scaffolding costs at most `MAX_FETCH_ROW_OVERHEAD_BYTES` (256) per
///    row, and a page holds at most `MAX_FETCH_LIMIT` (500) rows, so at most
///    128,000 bytes.
/// 3. The response wrapper (`{"next_cursor":N,"envelopes":[…]}`) is under 64
///    bytes.
///
/// So the largest B that fits is `(12,582,912 - 128,000 - 64) * 3 / 4` ≈
/// 9,341,136 bytes ≈ 8.9 MiB. Taking **8 MiB (8,388,608)** lands under that
/// with ~10% of the cap left spare: the worst case serializes to
/// 11,184,812 + 128,000 + 64 = 11,312,876 bytes, against a cap of 12,582,912.
/// The margin absorbs a future field on `EnvelopeResponse` and any slop in
/// the per-row estimate. `page_worst_case_fits_the_client_body_cap` computes
/// this rather than restating it, so changing any input fails the test
/// instead of quietly eating the headroom.
///
/// The always-take-the-first-row rule cannot break this for any envelope that
/// could be POSTed: admission caps one at `MAX_ENVELOPE_SEALED_BYTES`, and the
/// assert below pins that under the page budget, so a page forced to carry one
/// such row stays far inside the client's cap.
///
/// It is NOT an unconditional guarantee, and the gap is worth naming. A row
/// written by an older build can exceed today's admission limit (see
/// `a_single_row_over_the_whole_budget_is_still_returned_alone`); returning it
/// alone is deliberate, because refusing would stall every client's cursor on
/// it forever. Such a row only decodes if it fits the client cap on its own —
/// roughly 9 MiB of sealed bytes. Past that it is genuinely unreachable, and no
/// client-side shrink helps, since the limit is already down to one row. Retire
/// it by expiry, not by paging.
const MAX_FETCH_PAGE_SEALED_BYTES: usize = 8 * 1024 * 1024;

/// The derivation above, as a compile-time check rather than a comment:
/// worst-case base64 of the budget, plus worst-case row scaffolding for a
/// maximum-length page, plus the response wrapper, must fit the client's body
/// cap. Raising the budget or the row limit past what a client will decode is
/// then a build failure, not a mailbox that stalls in the field.
const _: () = assert!(
    MAX_FETCH_PAGE_SEALED_BYTES.div_ceil(3) * 4
        + MAX_FETCH_LIMIT * MAX_FETCH_ROW_OVERHEAD_BYTES
        + 64
        <= CLIENT_MAX_RESPONSE_BODY_BYTES,
    "a fetch page built to the byte budget must fit the client's response cap"
);

/// A single envelope must always fit the budget on its own, or the
/// always-take-the-first-row rule in `fetch_envelopes` could hand a client a
/// page it cannot decode.
const _: () = assert!(MAX_ENVELOPE_SEALED_BYTES <= MAX_FETCH_PAGE_SEALED_BYTES);
pub const MAX_FETCH_HINTS: usize = 256;
pub const MAX_ACK_IDS: usize = 512;
const MAX_PRESENCE_ANNOUNCE: usize = 4;
const MAX_PRESENCE_QUERY: usize = 512;
const PRESENCE_RETENTION_MS: i64 = 48 * 60 * 60 * 1000;
pub const WS_MAX_INBOUND_MESSAGE_BYTES: usize = 4 * 1024;

/// Capacity of the global POST→WS broadcast. Lagging subscribers that fall
/// more than this many events behind are disconnected (`Lagged`); they
/// reconnect and replay from their cursor.
pub const WS_BROADCAST_CAPACITY: usize = 64;

/// If a WS write cannot complete within this window the peer is treated as
/// slow/dead and dropped (same heal path as lag: reconnect + replay).
const WS_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

/// FR6: default max concurrent WS connections held by a single family
/// token. Family tokens are semi-public (baked into QR friend cards), so
/// without a cap, anyone who has seen a card could open unboundedly many
/// sockets against the $4 VPS.
pub const DEFAULT_WS_PER_TOKEN_MAX_CONNECTIONS: usize = 16;

/// FR6: default max concurrent WS connections across all family tokens
/// combined -- the coarser backstop behind the per-token cap.
pub const DEFAULT_WS_GLOBAL_MAX_CONNECTIONS: usize = 256;

/// FR6: server-side keepalive cadence. A `Ping` is sent on this interval;
/// see `DEFAULT_WS_PING_MISSED_LIMIT`.
const DEFAULT_WS_PING_INTERVAL: Duration = Duration::from_secs(45);

/// FR6: a peer that answers neither with a `Pong` nor any other client
/// frame within this many consecutive ping intervals is treated as dead
/// and dropped -- same heal path as a lag-drop (reconnect + replay), and it
/// frees the connection-cap permit the dead socket was holding.
const DEFAULT_WS_PING_MISSED_LIMIT: u32 = 2;

/// FR8: how long a store call blocks waiting for SQLite's write lock
/// before giving up with `SQLITE_BUSY`. Store calls already run on a
/// `spawn_blocking` thread (`RelayStore::run_blocking`), so waiting here
/// costs a blocking-pool thread, not a tokio reactor worker.
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// DESIGN.md §9: hard upper bound on how long a row may live on the relay.
/// Client-supplied `expiry_ms` (typically 7 days via core's
/// `DEFAULT_EXPIRY_MS`) is honored when tighter; this caps the rest.
pub const MAX_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// FR7: default cadence for the background maintenance task
/// (`spawn_prune_task`) that prunes expired rows and reclaims disk
/// independent of any client traffic.
pub const DEFAULT_PRUNE_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// DTN_TODOS.md D7 (N2): hard cap on the *decoded* `sealed` ciphertext of a
/// single envelope — the only thing standing between a client and unbounded
/// per-row SQLite growth was previously axum's default 2 MiB request-body
/// limit (which bounds the whole JSON request, not this field).
///
/// Derivation, anchored to the client-side attachment ceiling so a
/// legitimate envelope can never trip this:
///
/// 1. `core/src/content.rs::ATTACHMENT_MAX_BLOB_BYTES` = 180 KiB
///    (184,320 bytes) is the largest inline attachment blob a client will
///    ever produce — enforced before sealing by both shells (Android's
///    `AttachmentPayload.MAX_BLOB_BYTES` calls the same core constant via
///    `attachment_max_blob_bytes()`).
/// 2. `encode_attachment_payload` wire overhead (version + media_type +
///    u16-length-prefixed mime + u32 duration + u32 blob_len +
///    u16-length-prefixed caption) is at most a few dozen bytes for any
///    real mime type/caption; generously budget 1 KiB.
/// 3. `seal_message` overhead (`core/src/crypto.rs`): Ed25519 sign_pk (32
///    bytes) + signature (64 bytes), padded up to the next 256-byte
///    `PAD_BUCKET`, plus the sealed envelope header (1-byte version +
///    32-byte ephemeral X25519 pk + 24-byte nonce = 57 bytes) and the
///    Poly1305 AEAD tag (16 bytes); generously budget 1 KiB.
/// 4. Realistic ceiling: 180 KiB + 1 KiB + 1 KiB ≈ 182 KiB.
/// 5. Round up ~2x for headroom (future envelope kinds, estimation slop):
///    **512 KiB (524,288 bytes)**.
///
/// Base64 inflation of the JSON `sealed` field (this cap applies to the
/// decoded bytes, not the wire string) is handled separately: 512 KiB
/// decoded is ~683 KiB of base64, comfortably inside axum's default 2 MiB
/// request-body limit, so this cap is the one that actually fires.
pub const MAX_ENVELOPE_SEALED_BYTES: usize = 512 * 1024;

/// DTN_TODOS.md D7 (N2): default per-family-token storage quota (sum of
/// `LENGTH(sealed)` across that family's rows), configurable via
/// `CRUISEMESH_RELAY_FAMILY_QUOTA_BYTES` (see `DEPLOY.md`).
///
/// 256 MiB ≈ "a family's whole cruise of photos": at the 180 KiB
/// `ATTACHMENT_MAX_BLOB_BYTES` ceiling that is ~1,450 max-size photo/audio
/// attachments, or many times that for the more typical few-hundred-KB
/// compressed photo `MediaCompressor` actually produces. A family of five
/// phones each sending dozens of photos a day for a week-long cruise, plus
/// text/receipt traffic (which is tiny by comparison), stays well under
/// this on any realistic itinerary while still bounding the $4 VPS's disk.
pub const DEFAULT_FAMILY_QUOTA_BYTES: u64 = 256 * 1024 * 1024;

/// Abuse protection (`DEPLOY.md` §10): default sustained request allowance
/// for one family token, configurable via
/// `CRUISEMESH_RELAY_RATE_REQUESTS_PER_MIN`.
///
/// The family bearer token is semi-public (it rides in QR friend cards), so
/// anyone who has ever seen a card can call the API. Storage is already
/// bounded (`DEFAULT_FAMILY_QUOTA_BYTES`) and live sockets are already
/// bounded (`DEFAULT_WS_PER_TOKEN_MAX_CONNECTIONS`), but nothing bounded how
/// *fast* a leaked token could hammer the $4 VPS — every request costs a
/// SQLite round trip on one connection.
///
/// 600/min is 10 requests/second **for the whole family, sustained forever**,
/// with a full minute (600) burstable at once. A six-phone fleet polling
/// `GET /envelopes` plus `POST /presence` every 15 s is ~50 requests/min; a
/// phone dumping a queued backlog posts one request per envelope, and 600
/// envelopes in a minute is far past anything a family produces (the byte
/// allowance below binds first for media). Set generously on purpose: this
/// is a floodgate, not a plan tier.
pub const DEFAULT_RATE_REQUESTS_PER_MIN: u32 = 600;

/// Abuse protection (`DEPLOY.md` §10): default sustained upload allowance
/// for one family token, counted on *decoded* `sealed` bytes (the wire JSON
/// is ~1.33x that after base64). Configurable via
/// `CRUISEMESH_RELAY_RATE_BYTES_PER_MIN`.
///
/// 64 MiB/min lets a family upload their entire 256 MiB storage quota in
/// about four minutes, so the limiter can never be what keeps a real cruise
/// backlog from landing — it only flattens the peak. In legitimate terms
/// that is ~360 max-size (180 KiB) attachments or ~2,000 typical compressed
/// photos per minute across all of a family's phones. For a leaked token it
/// caps sustained ingest at ~1.1 MiB/s, which the $4 VPS's disk and the
/// 507-quota gate can both absorb.
pub const DEFAULT_RATE_BYTES_PER_MIN: u64 = 64 * 1024 * 1024;

/// Abuse protection (`DEPLOY.md` §10): default request allowance across all
/// family tokens combined — the coarse backstop behind the per-family cap,
/// mirroring how `DEFAULT_WS_GLOBAL_MAX_CONNECTIONS` backs the per-token
/// connection cap. Configurable via
/// `CRUISEMESH_RELAY_RATE_GLOBAL_REQUESTS_PER_MIN`.
///
/// 6,000/min (100 requests/second) is ten families simultaneously at their
/// full per-family allowance — an order of magnitude above the real
/// single-digit-family load (a few hundred requests/minute total), so it
/// only fires when many tokens misbehave at once or the hosted service has
/// outgrown this box and wants a bigger one.
pub const DEFAULT_RATE_GLOBAL_REQUESTS_PER_MIN: u32 = 6_000;

/// CP4 (deposit-token split): default sustained request allowance for one
/// family's *deposit* token, configurable via
/// `CRUISEMESH_RELAY_DEPOSIT_RATE_REQUESTS_PER_MIN`.
///
/// The deposit token is the credential that rides QR friend cards, so it is
/// the one strangers actually end up holding — the whole point of the split
/// is that a posted card can no longer fetch/ack family mail, and this
/// tighter bucket bounds the one thing it still *can* do (post). 60/min is
/// one envelope a second sustained: far more than any real contact produces
/// (a chatty friend sends a few messages a minute at most, and their photos
/// hit the byte bucket first), a tenth of the member allowance, and small
/// enough that a card-scraping flood is throttled to noise while the
/// family's own member-class traffic rides its own untouched buckets.
pub const DEFAULT_DEPOSIT_RATE_REQUESTS_PER_MIN: u32 = 60;

/// CP4: default sustained upload allowance for one family's deposit token,
/// counted on decoded `sealed` bytes like the member allowance, configurable
/// via `CRUISEMESH_RELAY_DEPOSIT_RATE_BYTES_PER_MIN`. Proportionate to the
/// request split (a tenth of the member 64 MiB/min, rounded to a clean
/// figure): ~34 max-size (180 KiB) attachments per minute — generous for a
/// friend sharing photos, useless for filling a 256 MiB quota quickly.
pub const DEFAULT_DEPOSIT_RATE_BYTES_PER_MIN: u64 = 6 * 1024 * 1024;

/// CP4: class prefix that marks a deposit token. Mirrors
/// `core/src/relay_wire.rs::RELAY_DEPOSIT_TOKEN_PREFIX` — golden vectors in
/// both crates pin the two implementations together. Member tokens are
/// forbidden from starting with this prefix at provisioning time so a
/// credential's class is always unambiguous.
pub const DEPOSIT_TOKEN_PREFIX: &str = "cmdep1-";

/// CP4: domain-separation context for the deposit derivation. Must match
/// `core/src/relay_wire.rs::RELAY_DEPOSIT_TOKEN_CONTEXT` byte-for-byte.
const DEPOSIT_TOKEN_CONTEXT: &[u8] = b"cruisemesh relay deposit token v1";

/// CP4: derive a family's post-only deposit token from its member token —
/// `cmdep1-` ‖ base64url(BLAKE2b-256(context ‖ member_token)).
///
/// Derivation (not random minting) is deliberate: phones stamp the identical
/// value onto friend cards entirely offline, knowing only their member token
/// (`core/src/relay_wire.rs::relay_deposit_token_for`), so no new endpoint
/// or credential-distribution channel exists. One-way: a deposit token
/// (semi-public, it rides QR cards) reveals nothing about the member token
/// it came from — provided member tokens are high-entropy, which DEPLOY.md
/// §1 requires (`openssl rand -hex 32`).
pub fn deposit_token_for(member_token: &str) -> String {
    let member = member_token.trim();
    if member.is_empty() || member.starts_with(DEPOSIT_TOKEN_PREFIX) {
        return member.to_string();
    }
    let mut hasher = Blake2bVar::new(32).expect("valid blake2b output length");
    hasher.update(DEPOSIT_TOKEN_CONTEXT);
    hasher.update(member.as_bytes());
    let mut out = [0u8; 32];
    hasher
        .finalize_variable(&mut out)
        .expect("output buffer matches configured length");
    format!("{DEPOSIT_TOKEN_PREFIX}{}", URL_SAFE_NO_PAD.encode(out))
}

/// CP4: is this credential deposit-class? Purely syntactic (the prefix), so
/// it can gate validation before any lookup.
pub fn is_deposit_token(token: &str) -> bool {
    token.trim().starts_with(DEPOSIT_TOKEN_PREFIX)
}

/// CP4: which capability class a presented bearer token resolved to.
/// Enforcement lives in `authorize_family` — the single choke point every
/// authenticated route goes through — so no individual handler can forget
/// the check: a deposit-class credential authorizes `FamilyOp::Post` and
/// nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenClass {
    /// Full family credential (post + fetch + ack + presence + WS). Rides
    /// the Cruise Pass setup card; every pre-CP4 token is this class.
    Member,
    /// Post-only into the family's mailbox. Rides friend cards.
    Deposit,
}

/// Bucket-map size that triggers a lazy eviction sweep of idle families
/// (`evict_idle_rate_buckets`). One entry is a few dozen bytes, so 1,024 of
/// them is trivial memory; the threshold exists so the sweep costs nothing at
/// all on a normal deployment (single-digit families never reach it) instead
/// of running on every request. If a relay ever does hold that many *active*
/// families the sweep becomes a linear scan of a small map per request —
/// still far cheaper than the SQLite round trip that follows it.
const RATE_BUCKET_EVICT_THRESHOLD: usize = 1_024;

/// How long a family's buckets must go untouched before an eviction sweep
/// may drop them. Comfortably longer than the one-minute bucket capacity, so
/// an evicted family is always one that would have refilled to full anyway —
/// eviction can never hand back allowance that was still being used.
const RATE_BUCKET_IDLE_EVICT_AFTER: Duration = Duration::from_secs(5 * 60);

/// Ceiling on the `Retry-After` we advertise on a 429. A bucket's capacity
/// is one minute's allowance, so waiting a full minute always restores it to
/// full — never ask a client to sleep longer than that, even for a cost the
/// bucket could not satisfy at all.
const RATE_LIMIT_MAX_RETRY_AFTER_SECS: u64 = 60;

/// Hosted-relay (Cruise Pass) expiry grace: after a provisioned family's
/// `expires_ms` passes, the family may still FETCH and ACK queued envelopes
/// for this window (so nobody's last messages are stranded mid-cruise), but
/// may no longer POST new ones. Past the grace window every request is
/// rejected. Distinct `code` values (`family_expired`) let clients show
/// "renew your pass" instead of a generic auth failure.
pub const FAMILY_EXPIRY_GRACE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// Upper bound on a provisioned family token's length. Matches core's
/// friend-card `relay_token` validation cap (`core/src/identity.rs`), so any
/// token the admin API accepts is guaranteed to fit in a friend card.
pub const MAX_FAMILY_TOKEN_LEN: usize = 1024;

/// Page size for `GET /admin/families` when the caller doesn't ask for one.
pub const DEFAULT_FAMILY_LIST_LIMIT: usize = 100;

/// Ceiling on `GET /admin/families?limit=`. A larger request is clamped to
/// this rather than rejected — the response carries `total`, so a caller that
/// wanted everything can see it got a partial page and ask for the rest.
pub const MAX_FAMILY_LIST_LIMIT: usize = 500;

/// FR4: build-time version identifiers, embedded via Cargo (`VERSION`) and
/// `build.rs` (`GIT_SHA`) so `/healthz` and the startup log always reflect
/// the exact commit running -- there was previously no way to ask a
/// deployed relay which of master's several relayd-affecting changes
/// (`/presence`, D7 quotas, T4-09 limits, ...) it was actually running.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = env!("CRUISEMESH_GIT_SHA");

/// FR6: tunable WS admission-control knobs, pulled out of `AppState`'s
/// constructor parameter list so a test can shrink the connection caps or
/// the ping cadence without changing every other constructor's signature.
#[derive(Clone, Copy, Debug)]
pub struct WsLimitsConfig {
    pub per_token_max_connections: usize,
    pub global_max_connections: usize,
    pub ping_interval: Duration,
    pub ping_missed_limit: u32,
}

impl Default for WsLimitsConfig {
    fn default() -> Self {
        Self {
            per_token_max_connections: DEFAULT_WS_PER_TOKEN_MAX_CONNECTIONS,
            global_max_connections: DEFAULT_WS_GLOBAL_MAX_CONNECTIONS,
            ping_interval: DEFAULT_WS_PING_INTERVAL,
            ping_missed_limit: DEFAULT_WS_PING_MISSED_LIMIT,
        }
    }
}

/// Abuse protection (`DEPLOY.md` §10): tunable request/byte rate-limit
/// allowances, pulled out of `AppState`'s constructor parameter list the same
/// way `WsLimitsConfig` is, so a test can shrink them to something it can
/// exhaust in milliseconds without touching every other constructor.
///
/// All allowances are *per minute*; each is also the burst capacity of its
/// bucket (a family that has been quiet may spend a whole minute's worth at
/// once). CP4: the `deposit_*` pair applies to deposit-class tokens, which
/// get their own (tighter) buckets, keyed by the deposit token itself, so a
/// friend-card flood can never eat the family's own member allowance.
#[derive(Clone, Copy, Debug)]
pub struct RateLimitConfig {
    pub requests_per_min: u32,
    pub bytes_per_min: u64,
    pub deposit_requests_per_min: u32,
    pub deposit_bytes_per_min: u64,
    pub global_requests_per_min: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_min: DEFAULT_RATE_REQUESTS_PER_MIN,
            bytes_per_min: DEFAULT_RATE_BYTES_PER_MIN,
            deposit_requests_per_min: DEFAULT_DEPOSIT_RATE_REQUESTS_PER_MIN,
            deposit_bytes_per_min: DEFAULT_DEPOSIT_RATE_BYTES_PER_MIN,
            global_requests_per_min: DEFAULT_RATE_GLOBAL_REQUESTS_PER_MIN,
        }
    }
}

impl RateLimitConfig {
    /// The (requests/min, bytes/min) pair that applies to one credential
    /// class — reusing the CP2a bucket machinery with per-class capacities.
    fn allowances_for(&self, class: TokenClass) -> (u32, u64) {
        match class {
            TokenClass::Member => (self.requests_per_min, self.bytes_per_min),
            TokenClass::Deposit => (self.deposit_requests_per_min, self.deposit_bytes_per_min),
        }
    }
}

/// A classic token bucket, refilled lazily on use: no timer task, so an idle
/// family costs exactly nothing until its next request.
///
/// `Instant` (monotonic) and never `SystemTime`: an NTP step or a manual
/// clock change on the VPS must not hand out free allowance, nor freeze a
/// family out for however long the clock jumped — both of which a
/// wall-clock bucket would do.
#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_per_sec: f64,
    last_refill: Instant,
}

impl TokenBucket {
    /// Build a bucket for a per-minute allowance: capacity is a full
    /// minute's worth (so a quiet family can burst that much at once) and it
    /// refills at `allowance / 60` per second. Starts full — a brand-new
    /// family is not penalized for being new.
    fn per_minute(per_min: f64, now: Instant) -> Self {
        let capacity = per_min.max(0.0);
        Self {
            tokens: capacity,
            capacity,
            refill_per_sec: capacity / 60.0,
            last_refill: now,
        }
    }

    /// Lazily credit the time elapsed since the last touch, clamped to
    /// capacity (unused allowance does not accumulate past one minute).
    fn refill(&mut self, now: Instant) {
        let elapsed = now
            .saturating_duration_since(self.last_refill)
            .as_secs_f64();
        self.last_refill = now;
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        }
    }

    fn has(&self, cost: f64) -> bool {
        self.tokens >= cost
    }

    fn take(&mut self, cost: f64) {
        self.tokens = (self.tokens - cost).max(0.0);
    }

    /// Refill, then take `cost` if it fits. `false` means the caller is over
    /// its allowance and nothing was charged.
    fn try_take(&mut self, cost: f64, now: Instant) -> bool {
        self.refill(now);
        if !self.has(cost) {
            return false;
        }
        self.take(cost);
        true
    }

    /// Whole seconds until this bucket would hold `cost` tokens — the
    /// `Retry-After` value. At least 1 (a client told to retry in 0 seconds
    /// just hot-loops) and never more than one full window.
    fn retry_after_secs(&self, cost: f64) -> u64 {
        if self.refill_per_sec <= 0.0 {
            return RATE_LIMIT_MAX_RETRY_AFTER_SECS;
        }
        let missing = (cost - self.tokens).max(0.0);
        let secs = (missing / self.refill_per_sec).ceil();
        // `as u64` saturates at 0 for NaN/negative and at u64::MAX for huge
        // values; the clamp turns both into a sane wait either way.
        (secs as u64).clamp(1, RATE_LIMIT_MAX_RETRY_AFTER_SECS)
    }

    fn is_full(&self) -> bool {
        self.tokens >= self.capacity
    }
}

/// The pair of buckets a single family token is charged against.
struct FamilyBuckets {
    requests: TokenBucket,
    bytes: TokenBucket,
}

impl FamilyBuckets {
    /// CP4: capacities are passed per credential class
    /// (`RateLimitConfig::allowances_for`) — one bucket map holds member and
    /// deposit entries side by side, keyed by the presented credential.
    fn new(requests_per_min: u32, bytes_per_min: u64, now: Instant) -> Self {
        Self {
            requests: TokenBucket::per_minute(f64::from(requests_per_min), now),
            bytes: TokenBucket::per_minute(bytes_per_min as f64, now),
        }
    }

    /// Charge both dimensions atomically: both are checked before *either*
    /// is debited, so a post that trips the byte allowance does not also
    /// silently burn a request token it never got to use.
    ///
    /// `Err` carries which dimension tripped and how long to wait.
    fn try_take(
        &mut self,
        requests: f64,
        bytes: f64,
        now: Instant,
    ) -> Result<(), (RateLimitScope, u64)> {
        self.requests.refill(now);
        self.bytes.refill(now);
        if !self.requests.has(requests) {
            return Err((
                RateLimitScope::FamilyRequests,
                self.requests.retry_after_secs(requests),
            ));
        }
        if !self.bytes.has(bytes) {
            return Err((
                RateLimitScope::FamilyBytes,
                self.bytes.retry_after_secs(bytes),
            ));
        }
        self.requests.take(requests);
        self.bytes.take(bytes);
        Ok(())
    }
}

/// Which limit rejected a request. One `code` covers all three on the wire
/// (the client's remedy is identical: back off and retry); the distinction
/// exists for the operator, in the message and the log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RateLimitScope {
    FamilyRequests,
    FamilyBytes,
    GlobalRequests,
}

impl RateLimitScope {
    /// Log-field discriminant. Never contains the token.
    fn label(self) -> &'static str {
        match self {
            Self::FamilyRequests => "family_requests",
            Self::FamilyBytes => "family_bytes",
            Self::GlobalRequests => "global_requests",
        }
    }

    /// Client-facing phrasing: names both which limit (this family's vs the
    /// whole server's) and which dimension (requests vs uploaded bytes).
    fn description(self) -> &'static str {
        match self {
            Self::FamilyRequests => "family request rate limit",
            Self::FamilyBytes => "family upload byte rate limit",
            Self::GlobalRequests => "server-wide request rate limit",
        }
    }
}

/// Lazy cleanup for the per-family bucket map: one entry accumulates per
/// family token ever seen, and a hosted relay churns tokens as passes are
/// sold and expire. Called only when the map crosses
/// `RATE_BUCKET_EVICT_THRESHOLD` — no background task.
///
/// The buckets must be refilled *before* the full-capacity test: `tokens` is
/// only ever updated on use, so a family that spent its allowance and then
/// vanished still reads as empty however long ago that was. Idleness is
/// therefore measured first (from `last_refill`, which refilling resets),
/// and only then is the refilled bucket asked whether it is back at full —
/// which, past an idle window several times the one-minute capacity, it
/// always is. Dropping a full bucket is equivalent to keeping it: a
/// re-created one also starts full.
fn evict_idle_rate_buckets(buckets: &mut HashMap<String, FamilyBuckets>, now: Instant) {
    buckets.retain(|_, family| {
        let idle_for = now.saturating_duration_since(family.requests.last_refill);
        if idle_for < RATE_BUCKET_IDLE_EVICT_AFTER {
            return true;
        }
        family.requests.refill(now);
        family.bytes.refill(now);
        !(family.requests.is_full() && family.bytes.is_full())
    });
}

/// Build the 429 for a tripped limit, logging it first (FR2: every rejection
/// is visible server-side). Logs the token *prefix* only — the full bearer
/// token is a credential and never belongs in the log stream.
fn reject_rate_limited(
    family_token: &str,
    scope: RateLimitScope,
    retry_after_secs: u64,
) -> ApiError {
    warn!(
        family = %token_prefix(family_token),
        scope = scope.label(),
        retry_after_secs,
        "request rejected: rate limit exceeded (429)"
    );
    ApiError::rate_limited(scope, retry_after_secs)
}

#[derive(Clone)]
pub struct AppState {
    store: RelayStore,
    auth_tokens: HashSet<String>,
    /// CP4: derived deposit token → static member token, precomputed from
    /// `auth_tokens` at construction. Static env-allowlist families have no
    /// `families` table row, so their deposit counterparts are resolved from
    /// this map instead of SQLite.
    static_deposit_tokens: Arc<HashMap<String, String>>,
    tx: tokio::sync::broadcast::Sender<std::sync::Arc<BroadcastEnvelope>>,
    family_quota_bytes: u64,
    /// FR6: global concurrent-WS-connection admission gate.
    ws_global: Arc<Semaphore>,
    /// FR6: per-family-token connection gate -- one `Semaphore` per token.
    /// The static env allowlist is prefilled at construction; hosted-relay
    /// families are provisioned at runtime via `/admin/families`, so their
    /// semaphores are created lazily on first WS upgrade (behind a mutex --
    /// the map is touched once per upgrade, not per frame).
    ws_per_token: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
    /// FR6: cap used when lazily creating a per-token semaphore above.
    ws_per_token_max_connections: usize,
    ws_ping_interval: Duration,
    ws_ping_missed_limit: u32,
    /// Abuse protection: the configured per-minute allowances, used when a
    /// family's buckets are created on first (authorized) request.
    rate_limits: RateLimitConfig,
    /// Abuse protection: per-family-token request + byte buckets, created
    /// lazily on first authorized request and evicted once idle
    /// (`evict_idle_rate_buckets`). Touched once per request under a plain
    /// mutex — the critical section is a few float operations, far cheaper
    /// than the SQLite round trip that follows it.
    rate_buckets: Arc<Mutex<HashMap<String, FamilyBuckets>>>,
    /// Abuse protection: coarse request backstop shared by every token.
    rate_global: Arc<Mutex<TokenBucket>>,
    /// Bearer token for the `/admin/families` provisioning API
    /// (`CRUISEMESH_RELAY_ADMIN_TOKEN`). `None` disables the admin routes
    /// entirely (they answer 404), which is the self-hosted default.
    admin_token: Option<String>,
}

#[derive(Clone, Debug)]
pub struct BroadcastEnvelope {
    pub family_token: String,
    pub recipient_hint: String,
    pub envelope: EnvelopeResponse,
}

impl AppState {
    pub fn new(store: RelayStore, auth_tokens: HashSet<String>) -> Self {
        Self::with_config(
            store,
            auth_tokens,
            WS_BROADCAST_CAPACITY,
            DEFAULT_FAMILY_QUOTA_BYTES,
        )
    }

    /// Test helper: custom broadcast capacity for slow-consumer coverage.
    pub fn with_hub_capacity(
        store: RelayStore,
        auth_tokens: HashSet<String>,
        capacity: usize,
    ) -> Self {
        Self::with_config(store, auth_tokens, capacity, DEFAULT_FAMILY_QUOTA_BYTES)
    }

    /// Test helper: custom per-family storage quota (default hub capacity).
    pub fn with_family_quota_bytes(
        store: RelayStore,
        auth_tokens: HashSet<String>,
        family_quota_bytes: u64,
    ) -> Self {
        Self::with_config(
            store,
            auth_tokens,
            WS_BROADCAST_CAPACITY,
            family_quota_bytes,
        )
    }

    /// FR6 test helper: custom WS admission-control knobs (default hub
    /// capacity + family quota) -- lets a test shrink the connection caps
    /// or the ping cadence instead of waiting on production-sized defaults.
    pub fn with_ws_limits(
        store: RelayStore,
        auth_tokens: HashSet<String>,
        ws_limits: WsLimitsConfig,
    ) -> Self {
        Self::with_full_config(
            store,
            auth_tokens,
            WS_BROADCAST_CAPACITY,
            DEFAULT_FAMILY_QUOTA_BYTES,
            ws_limits,
            RateLimitConfig::default(),
        )
    }

    /// Abuse-protection test helper: tiny request/byte allowances (default
    /// hub capacity, family quota and WS knobs) so a test can exhaust and
    /// watch a bucket refill in milliseconds instead of a production minute.
    pub fn with_rate_limits(
        store: RelayStore,
        auth_tokens: HashSet<String>,
        rate_limits: RateLimitConfig,
    ) -> Self {
        Self::with_full_config(
            store,
            auth_tokens,
            WS_BROADCAST_CAPACITY,
            DEFAULT_FAMILY_QUOTA_BYTES,
            WsLimitsConfig::default(),
            rate_limits,
        )
    }

    pub fn with_config(
        store: RelayStore,
        auth_tokens: HashSet<String>,
        hub_capacity: usize,
        family_quota_bytes: u64,
    ) -> Self {
        Self::with_full_config(
            store,
            auth_tokens,
            hub_capacity,
            family_quota_bytes,
            WsLimitsConfig::default(),
            RateLimitConfig::default(),
        )
    }

    /// FR6: the one real constructor; everything above delegates here.
    pub fn with_full_config(
        store: RelayStore,
        auth_tokens: HashSet<String>,
        hub_capacity: usize,
        family_quota_bytes: u64,
        ws_limits: WsLimitsConfig,
        rate_limits: RateLimitConfig,
    ) -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(hub_capacity.max(1));
        let ws_per_token = auth_tokens
            .iter()
            .map(|token| {
                (
                    token.clone(),
                    Arc::new(Semaphore::new(ws_limits.per_token_max_connections)),
                )
            })
            .collect();
        // CP4: static families get deposit counterparts too, derived once
        // here — the same derivation phones apply when stamping friend cards.
        let static_deposit_tokens = auth_tokens
            .iter()
            .map(|token| (deposit_token_for(token), token.clone()))
            .collect();
        Self {
            store,
            auth_tokens,
            static_deposit_tokens: Arc::new(static_deposit_tokens),
            tx,
            family_quota_bytes,
            ws_global: Arc::new(Semaphore::new(ws_limits.global_max_connections)),
            ws_per_token: Arc::new(Mutex::new(ws_per_token)),
            ws_per_token_max_connections: ws_limits.per_token_max_connections,
            ws_ping_interval: ws_limits.ping_interval,
            ws_ping_missed_limit: ws_limits.ping_missed_limit,
            rate_limits,
            // Family buckets are created on first *authorized* request (see
            // `check_rate_limit`), not prefilled from the allowlist: hosted
            // families are provisioned at runtime, so prefilling would only
            // cover half of them anyway.
            rate_buckets: Arc::new(Mutex::new(HashMap::new())),
            rate_global: Arc::new(Mutex::new(TokenBucket::per_minute(
                f64::from(rate_limits.global_requests_per_min),
                Instant::now(),
            ))),
            admin_token: None,
        }
    }

    /// Charge one request (and, for uploads, its sealed bytes) against this
    /// credential's buckets and the global request backstop.
    ///
    /// **Only ever call this after the caller's token has authorized.** The
    /// bucket map is keyed by the presented credential: enforcing before
    /// authorization would let an unauthenticated caller insert one entry
    /// per made-up token and grow the map without bound, turning the abuse
    /// protection into exactly the memory-exhaustion vector it exists to
    /// prevent. Authorization first means an attacker can only ever occupy
    /// entries for tokens they already hold.
    ///
    /// CP4: buckets are keyed by `access.rate_key` — the presented member
    /// *or* deposit token — with per-class capacities, so friend-card
    /// (deposit) traffic exhausts its own tighter allowance and never eats
    /// into the family's member-class buckets.
    fn check_rate_limit(
        &self,
        access: &FamilyAccess,
        requests: f64,
        bytes: f64,
    ) -> Result<(), ApiError> {
        let now = Instant::now();
        let (requests_per_min, bytes_per_min) = self.rate_limits.allowances_for(access.class);
        let family = {
            let mut buckets = self.rate_buckets.lock().unwrap_or_else(|e| e.into_inner());
            if buckets.len() >= RATE_BUCKET_EVICT_THRESHOLD {
                evict_idle_rate_buckets(&mut buckets, now);
            }
            buckets
                .entry(access.rate_key.clone())
                .or_insert_with(|| FamilyBuckets::new(requests_per_min, bytes_per_min, now))
                .try_take(requests, bytes, now)
        };
        if let Err((scope, retry_after_secs)) = family {
            return Err(reject_rate_limited(
                &access.rate_key,
                scope,
                retry_after_secs,
            ));
        }
        // Global backstop, requests only: bytes are already bounded per
        // family, and a request cap bounds how many uploads can arrive at
        // all. Charged after the family buckets so a family that is over its
        // own limit never eats into everyone else's allowance.
        let global_retry_after = {
            let mut global = self.rate_global.lock().unwrap_or_else(|e| e.into_inner());
            if global.try_take(requests, now) {
                None
            } else {
                Some(global.retry_after_secs(requests))
            }
        };
        if let Some(retry_after_secs) = global_retry_after {
            return Err(reject_rate_limited(
                &access.rate_key,
                RateLimitScope::GlobalRequests,
                retry_after_secs,
            ));
        }
        Ok(())
    }

    /// Builder: enable the `/admin/families` API with this bearer token.
    pub fn with_admin_token(mut self, admin_token: Option<String>) -> Self {
        self.admin_token = admin_token;
        self
    }

    /// Test helper: push a synthetic envelope directly onto the WS fan-out
    /// broadcast channel, bypassing `POST /envelopes` (and therefore the
    /// store) entirely. `id`/`msg_id`/etc. are placeholders -- this exists
    /// only to let a test overflow the broadcast buffer deterministically,
    /// independent of the relative speed of HTTP/store handling vs. the WS
    /// handler's drain loop.
    ///
    /// FR8 note: before store calls moved onto `spawn_blocking`, a test
    /// could induce the same overflow indirectly by flooding
    /// `POST /envelopes` on a single-threaded test runtime -- synchronous
    /// DB work on the reactor thread starved the WS handler task of any
    /// chance to drain the channel. Once store calls stopped blocking the
    /// reactor, that trick stopped working (the WS handler now gets
    /// scheduled readily between posts and keeps up). This is a plain,
    /// synchronous, non-`.await`ing call -- a test loop that calls it N
    /// times in a row is guaranteed to run to completion without yielding
    /// to the scheduler even once, so the WS handler task genuinely cannot
    /// drain anything mid-loop.
    pub fn test_broadcast_envelope(&self, family_token: &str, recipient_hint: &[u8], id: i64) {
        let recipient_hint = encode_base64_field(recipient_hint);
        let envelope = EnvelopeResponse {
            id,
            msg_id: String::new(),
            hop_ttl: 0,
            recipient_hint: recipient_hint.clone(),
            sealed: String::new(),
            expiry_ms: 0,
            created_at_ms: 0,
        };
        let _ = self.tx.send(std::sync::Arc::new(BroadcastEnvelope {
            family_token: family_token.to_string(),
            recipient_hint,
            envelope,
        }));
    }
}

#[derive(Clone)]
pub struct RelayStore {
    conn: std::sync::Arc<Mutex<Connection>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredEnvelope {
    pub id: i64,
    pub msg_id: Vec<u8>,
    pub hop_ttl: u8,
    pub recipient_hint: Vec<u8>,
    pub sealed: Vec<u8>,
    pub expiry_ms: i64,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoredPresence {
    pub hint: Vec<u8>,
    pub last_seen_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuotaInsertResult {
    Stored { id: i64 },
    QuotaExceeded { usage_bytes: u64 },
}

/// A provisioned (hosted / Cruise Pass) family, stored in the `families`
/// table. Static env-var tokens (`CRUISEMESH_RELAY_TOKENS`) never appear
/// here — they behave as implicit always-active families.
#[derive(Clone, Debug, PartialEq)]
pub struct FamilyRow {
    pub token: String,
    /// `active` or `suspended`. Revoked families are deleted outright.
    pub status: String,
    pub plan: Option<String>,
    /// Per-family override of the server-wide sealed-byte quota.
    pub quota_bytes: Option<u64>,
    pub created_ms: i64,
    /// `None` = never expires. Expiry semantics: see `FAMILY_EXPIRY_GRACE_MS`.
    pub expires_ms: Option<i64>,
    pub note: Option<String>,
    /// CP4: the family's post-only credential, derived from `token` at
    /// provisioning (or by the startup migration for pre-CP4 rows). Rides
    /// friend cards; rejected for everything except `POST /envelopes`.
    pub deposit_token: String,
}

/// A family plus its stored-usage figures, as returned by `list_families`.
#[derive(Clone, Debug, PartialEq)]
pub struct FamilyUsage {
    pub family: FamilyRow,
    pub usage_bytes: u64,
    pub envelope_count: u64,
}

/// One page of `list_families` output. `total` is the match count across the
/// whole table, so the caller knows whether to ask for another page.
#[derive(Clone, Debug, PartialEq)]
pub struct FamilyPage {
    pub families: Vec<FamilyUsage>,
    pub total: u64,
}

impl RelayStore {
    pub fn open(path: &str) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        // FR8: default SQLite settings are journal_mode=DELETE (readers and
        // the one writer block each other for the duration of a
        // transaction) and busy_timeout=0 (a lock collision fails
        // immediately with SQLITE_BUSY instead of waiting). Every store
        // call now runs on a spawn_blocking thread (see `run_blocking`)
        // rather than serialized onto whichever tokio worker happened to
        // be running the handler, so concurrent store calls are a real
        // possibility, not just a theoretical one -- WAL lets readers
        // proceed while a write is in progress, and a nonzero busy_timeout
        // makes a transient writer-vs-writer collision retry-and-block
        // instead of surfacing as a bogus 500.
        //
        // Best-effort, not asserted: `:memory:` databases (used throughout
        // this crate's own unit tests) cannot use WAL -- SQLite silently
        // keeps them in "memory" journal mode -- so this deliberately does
        // not verify the resulting mode the way `pragma_update_and_check`
        // would; doing so would make every in-memory test fail.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|e| e.to_string())?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)
            .map_err(|e| e.to_string())?;
        conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        // FR7: convert any pre-existing database to incremental
        // auto-vacuum. See `ensure_incremental_auto_vacuum` for why this
        // can't just be a pragma statement inside `SCHEMA`.
        ensure_incremental_auto_vacuum(&conn)?;
        // CP4: token-class migration — add `families.deposit_token` on a
        // pre-existing database and derive it for every existing family, so
        // all existing tokens become member class with zero behavior change
        // and their deposit counterparts exist the moment the process is up.
        migrate_families_deposit_token(&conn)?;
        Ok(Self {
            conn: std::sync::Arc::new(Mutex::new(conn)),
        })
    }

    /// FR8: run a synchronous rusqlite call -- every `RelayStore` method is
    /// synchronous, guarded by `self.conn`'s `std::sync::Mutex` -- on a
    /// dedicated blocking-pool thread instead of whatever tokio worker is
    /// driving the calling handler. Before this, every request handler
    /// called `self.conn.lock()` + rusqlite directly from async code: a
    /// lock wait or a slow disk write would stall that worker thread and,
    /// with it, every other task cooperatively scheduled on it (other
    /// requests, WS keepalive pings, ...).
    ///
    /// Pattern: `RelayStore` is a cheap `Clone` (one `Arc` bump), so clone
    /// it and move the closure (which gets a `&RelayStore` to call the
    /// real, synchronous method on) into `spawn_blocking`.
    async fn run_blocking<F, T>(&self, f: F) -> Result<T, String>
    where
        F: FnOnce(&RelayStore) -> Result<T, String> + Send + 'static,
        T: Send + 'static,
    {
        let store = self.clone();
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .map_err(|join_err| format!("blocking store task panicked: {join_err}"))?
    }

    /// Clamp client `expiry_ms` to the 30-day retention ceiling relative to
    /// `created_at_ms`. Exposed for tests.
    pub fn effective_expiry(created_at_ms: i64, expiry_ms: i64) -> i64 {
        expiry_ms.min(created_at_ms.saturating_add(MAX_RETENTION_MS))
    }

    pub fn insert_envelope(
        &self,
        family_token: &str,
        msg_id: Vec<u8>,
        hop_ttl: u8,
        recipient_hint: Vec<u8>,
        sealed: Vec<u8>,
        expiry_ms: i64,
        created_at_ms: i64,
    ) -> Result<i64, String> {
        let expiry_ms = Self::effective_expiry(created_at_ms, expiry_ms);
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        // ON CONFLICT: keep the row; take the longer hop budget / later
        // expiry. Sealed bytes are intentionally NOT rewritten — re-posts
        // of the same msg_id are treated as pure dedupe (receipt retries
        // with a stable msg_id land here).
        conn.query_row(
            "INSERT INTO envelopes
                (family_token, msg_id, hop_ttl, recipient_hint, sealed, expiry_ms, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(family_token, msg_id) DO UPDATE SET
                hop_ttl = MAX(hop_ttl, excluded.hop_ttl),
                expiry_ms = MAX(expiry_ms, excluded.expiry_ms)
             RETURNING id",
            params![
                family_token,
                msg_id,
                hop_ttl as i64,
                recipient_hint,
                sealed,
                expiry_ms,
                created_at_ms,
            ],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())
    }

    /// Atomically admit a new row under the per-family sealed-byte quota.
    /// The dedupe check, usage calculation, optional expiry pruning, and insert
    /// all run while holding one store lock and one SQLite transaction.
    pub fn insert_envelope_with_quota(
        &self,
        family_token: &str,
        msg_id: Vec<u8>,
        hop_ttl: u8,
        recipient_hint: Vec<u8>,
        sealed: Vec<u8>,
        expiry_ms: i64,
        created_at_ms: i64,
        family_quota_bytes: u64,
    ) -> Result<QuotaInsertResult, String> {
        if sealed.len() > MAX_ENVELOPE_SEALED_BYTES {
            return Err(format!(
                "sealed envelope of {} bytes exceeds the {}-byte cap",
                sealed.len(),
                MAX_ENVELOPE_SEALED_BYTES
            ));
        }
        let expiry_ms = Self::effective_expiry(created_at_ms, expiry_ms);
        let mut conn = self.conn.lock().expect("relay store mutex poisoned");
        let tx = conn.transaction().map_err(|e| e.to_string())?;

        let existing_id: Option<i64> = tx
            .query_row(
                "SELECT id FROM envelopes WHERE family_token = ?1 AND msg_id = ?2 LIMIT 1",
                params![family_token, msg_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(id) = existing_id {
            tx.execute(
                "UPDATE envelopes SET
                    hop_ttl = MAX(hop_ttl, ?3),
                    expiry_ms = MAX(expiry_ms, ?4)
                 WHERE family_token = ?1 AND msg_id = ?2",
                params![family_token, msg_id, hop_ttl as i64, expiry_ms],
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(QuotaInsertResult::Stored { id });
        }

        let candidate_bytes = sealed.len() as u64;
        let mut usage_bytes = family_sealed_bytes_on(&tx, family_token)?;
        if usage_bytes.saturating_add(candidate_bytes) > family_quota_bytes {
            prune_expired_on(&tx, created_at_ms)?;
            usage_bytes = family_sealed_bytes_on(&tx, family_token)?;
        }
        if usage_bytes.saturating_add(candidate_bytes) > family_quota_bytes {
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(QuotaInsertResult::QuotaExceeded { usage_bytes });
        }

        let id = tx
            .query_row(
                "INSERT INTO envelopes
                    (family_token, msg_id, hop_ttl, recipient_hint, sealed, expiry_ms, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 RETURNING id",
                params![
                    family_token,
                    msg_id,
                    hop_ttl as i64,
                    recipient_hint,
                    sealed,
                    expiry_ms,
                    created_at_ms,
                ],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(QuotaInsertResult::Stored { id })
    }

    /// Bulk insert inside a single transaction (index/plan benchmarks).
    ///
    /// Not reachable from any HTTP route today (only the query-plan tests
    /// call it), but it is the crate's other envelope-ingest path, so it
    /// gets the same per-envelope size cap as `POST /envelopes`
    /// (`MAX_ENVELOPE_SEALED_BYTES`, DTN_TODOS.md D7) as defense-in-depth
    /// for whenever/if it is wired to a real endpoint. It intentionally
    /// does NOT enforce the per-family storage quota — that check needs a
    /// prune-then-recheck decision per row (see `post_envelope`), which
    /// doesn't make sense to run per-row inside one bulk transaction.
    pub fn insert_envelopes_batch(
        &self,
        rows: &[(String, Vec<u8>, u8, Vec<u8>, Vec<u8>, i64, i64)],
    ) -> Result<(), String> {
        for (_, _, _, _, sealed, _, _) in rows {
            if sealed.len() > MAX_ENVELOPE_SEALED_BYTES {
                return Err(format!(
                    "sealed envelope of {} bytes exceeds the {}-byte cap",
                    sealed.len(),
                    MAX_ENVELOPE_SEALED_BYTES
                ));
            }
        }
        let mut conn = self.conn.lock().expect("relay store mutex poisoned");
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO envelopes
                        (family_token, msg_id, hop_ttl, recipient_hint, sealed, expiry_ms, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                     ON CONFLICT(family_token, msg_id) DO UPDATE SET
                        hop_ttl = MAX(hop_ttl, excluded.hop_ttl),
                        expiry_ms = MAX(expiry_ms, excluded.expiry_ms)",
                )
                .map_err(|e| e.to_string())?;
            for (family, msg_id, hop_ttl, hint, sealed, expiry_ms, created_at_ms) in rows {
                let expiry_ms = Self::effective_expiry(*created_at_ms, *expiry_ms);
                stmt.execute(params![
                    family,
                    msg_id,
                    *hop_ttl as i64,
                    hint,
                    sealed,
                    expiry_ms,
                    created_at_ms,
                ])
                .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn fetch_envelopes(
        &self,
        family_token: &str,
        hints: Vec<Vec<u8>>,
        after_id: i64,
        limit: usize,
        now_ms: i64,
    ) -> Result<Vec<StoredEnvelope>, String> {
        if hints.is_empty() {
            return Ok(Vec::new());
        }
        if hints.len() > MAX_FETCH_HINTS {
            return Err(format!("at most {MAX_FETCH_HINTS} hints are allowed"));
        }
        // FR7: this used to eagerly `DELETE` expired rows before every
        // fetch -- a write transaction on the hottest read path in the
        // service (every `GET /envelopes` poll ran it). Physical deletion
        // now happens only in the hourly background maintenance task
        // (`spawn_prune_task`); the `expiry_ms > ?` predicate below is what
        // keeps an already-expired-but-not-yet-purged row out of the
        // response in the meantime. (The 30-day retention ceiling needs no
        // separate predicate: `effective_expiry` clamps every stored
        // `expiry_ms` to at most `created_at_ms + MAX_RETENTION_MS` at
        // insert time, so `expiry_ms > now` already implies the row is
        // also within the 30-day ceiling.)
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let hint_placeholders = (0..hints.len())
            .map(|index| format!("?{}", index + 3))
            .collect::<Vec<_>>()
            .join(",");
        let now_placeholder = hints.len() + 3;
        let limit_placeholder = hints.len() + 4;
        // Content-agnostic: sealed is returned as-is; no kind/type filter.
        let sql = format!(
            "SELECT id, msg_id, hop_ttl, recipient_hint, sealed, expiry_ms, created_at_ms
             FROM envelopes
             WHERE family_token = ?1 AND id > ?2 AND recipient_hint IN ({hint_placeholders})
                   AND expiry_ms > ?{now_placeholder}
             ORDER BY id ASC
             LIMIT ?{limit_placeholder}"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let fetch_limit = limit.min(MAX_FETCH_LIMIT) as i64;
        let mut bindings: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(hints.len() + 4);
        bindings.push(&family_token);
        bindings.push(&after_id);
        for hint in &hints {
            bindings.push(hint);
        }
        bindings.push(&now_ms);
        bindings.push(&fetch_limit);
        let rows = stmt
            .query_map(bindings.as_slice(), |row| {
                Ok(StoredEnvelope {
                    id: row.get(0)?,
                    msg_id: row.get(1)?,
                    hop_ttl: row.get::<_, i64>(2)? as u8,
                    recipient_hint: row.get(3)?,
                    sealed: row.get(4)?,
                    expiry_ms: row.get(5)?,
                    created_at_ms: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        // The row limit alone is not a bound on the *response*, only on its
        // length. Stop filling the page once one more row would push its
        // cumulative sealed bytes past the budget, so no client is ever handed
        // a page it will refuse to decode (see `MAX_FETCH_PAGE_SEALED_BYTES`).
        // The first matching row is always taken, whatever its size: a page
        // may be short but never empty while rows match, or an oversized
        // envelope would be permanently unreachable and would stall the
        // caller's cursor on it forever.
        let mut page: Vec<StoredEnvelope> = Vec::new();
        let mut sealed_bytes = 0usize;
        for row in rows {
            let row = row.map_err(|e| e.to_string())?;
            let next = sealed_bytes.saturating_add(row.sealed.len());
            if !page.is_empty() && next > MAX_FETCH_PAGE_SEALED_BYTES {
                break;
            }
            sealed_bytes = next;
            page.push(row);
        }
        Ok(page)
    }

    pub fn ack_envelopes(&self, family_token: &str, ids: Vec<i64>) -> Result<u64, String> {
        if ids.is_empty() {
            return Ok(0);
        }
        if ids.len() > MAX_ACK_IDS {
            return Err(format!("at most {MAX_ACK_IDS} ack ids are allowed"));
        }
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let placeholders = std::iter::repeat("?")
            .take(ids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "DELETE FROM envelopes
             WHERE family_token = ?1 AND id IN ({placeholders})"
        );
        let mut bindings: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(ids.len() + 1);
        bindings.push(&family_token);
        for id in &ids {
            bindings.push(id);
        }
        let deleted = conn
            .execute(&sql, bindings.as_slice())
            .map_err(|e| e.to_string())?;
        Ok(deleted as u64)
    }

    /// Drop rows past either their per-envelope `expiry_ms` or the 30-day
    /// server retention ceiling (`created_at_ms + MAX_RETENTION_MS`).
    ///
    /// FR7: as of the background-maintenance change, callers are the
    /// hourly `spawn_prune_task`, `sync_presence` (presence rows have their
    /// own, much shorter, retention window -- see `PRESENCE_RETENTION_MS`
    /// -- so pruning them on every presence sync is still cheap and
    /// keeps that table small), and the quota-overflow path in
    /// `insert_envelope_with_quota` (which needs an immediate prune-then-
    /// recheck decision, not an hourly one). `fetch_envelopes` no longer
    /// calls this -- see its doc comment.
    pub fn prune_expired(&self, now_ms: i64) -> Result<u64, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let pruned = prune_expired_on(&conn, now_ms)?;
        // FR2: only log when something actually happened -- this runs
        // hourly regardless of traffic, so a zero-count line would be the
        // dominant log entry and drown out everything else.
        if pruned > 0 {
            info!(pruned, "pruned expired envelope/presence rows");
        }
        Ok(pruned)
    }

    /// FR7: reclaim pages freed by deletes back to the OS (shrinks the file
    /// on disk). Only takes effect once the database is in
    /// `auto_vacuum = INCREMENTAL` mode -- guaranteed for every database
    /// this process opens by `ensure_incremental_auto_vacuum` -- and is a
    /// harmless no-op otherwise. Called from the hourly background
    /// maintenance task (`spawn_prune_task`), never from a request path.
    ///
    /// Unbounded (no page-count argument): for the family-scale relay DB
    /// this targets (single-digit families, a few hundred MiB ceiling
    /// each), one hourly pass over the current free list is expected to be
    /// fast. If relayd ever serves a scale where that stops being true,
    /// bound it (`PRAGMA incremental_vacuum(N)`) to cap how long this
    /// holds the store mutex.
    pub fn incremental_vacuum(&self) -> Result<(), String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        conn.execute_batch("PRAGMA incremental_vacuum;")
            .map_err(|e| e.to_string())
    }

    pub fn sync_presence(
        &self,
        family_token: &str,
        announce: &[Vec<u8>],
        query: &[Vec<u8>],
        now_ms: i64,
    ) -> Result<Vec<StoredPresence>, String> {
        self.prune_expired(now_ms)?;
        let mut conn = self.conn.lock().expect("relay store mutex poisoned");
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO presence (family_token, hint, last_seen_ms)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(family_token, hint) DO UPDATE SET
                        last_seen_ms = excluded.last_seen_ms",
                )
                .map_err(|e| e.to_string())?;
            for hint in announce {
                stmt.execute(params![family_token, hint, now_ms])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())?;

        if query.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (0..query.len())
            .map(|index| format!("?{}", index + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT hint, last_seen_ms
             FROM presence
             WHERE family_token = ?1 AND hint IN ({placeholders})
             ORDER BY last_seen_ms DESC"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let mut bindings: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(query.len() + 1);
        bindings.push(&family_token);
        for hint in query {
            bindings.push(hint);
        }
        let rows = stmt
            .query_map(bindings.as_slice(), |row| {
                Ok(StoredPresence {
                    hint: row.get(0)?,
                    last_seen_ms: row.get(1)?,
                })
            })
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn count_for_family(&self, family_token: &str) -> Result<u64, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let count: Option<i64> = conn
            .query_row(
                "SELECT COUNT(*) FROM envelopes WHERE family_token = ?1",
                params![family_token],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(count.unwrap_or(0) as u64)
    }

    /// Sum of `LENGTH(sealed)` across a family's rows — the quota-relevant
    /// storage figure (DTN_TODOS.md D7). Sealed ciphertext dominates row
    /// size; header columns (msg_id, hints, timestamps) are a few dozen
    /// bytes each and are not counted, so this is a conservative (slight
    /// under-)estimate of actual disk usage, which is fine for a soft quota.
    pub fn family_sealed_bytes(&self, family_token: &str) -> Result<u64, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        family_sealed_bytes_on(&conn, family_token)
    }

    /// Whether a `(family_token, msg_id)` row already exists. Used to skip
    /// the quota check on dedupe re-posts: `insert_envelope`'s
    /// `ON CONFLICT` path never rewrites `sealed`, so a re-post of an
    /// existing msg_id adds zero bytes and must not be charged against the
    /// quota (a receipt envelope re-uploaded every sync would otherwise
    /// eventually get rejected for growth that never happened).
    pub fn envelope_exists(&self, family_token: &str, msg_id: &[u8]) -> Result<bool, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM envelopes WHERE family_token = ?1 AND msg_id = ?2 LIMIT 1",
                params![family_token, msg_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        Ok(found.is_some())
    }

    /// Provision (or re-provision) a family. Idempotent by design: the
    /// billing webhook retries on failure, so posting the same token again
    /// must converge on the same row. A re-provision refreshes plan, quota,
    /// expiry, and note, and reactivates a suspended family (renewal after
    /// a lapsed pass takes this path).
    pub fn upsert_family(
        &self,
        token: &str,
        plan: Option<&str>,
        quota_bytes: Option<u64>,
        expires_ms: Option<i64>,
        note: Option<&str>,
        now_ms: i64,
    ) -> Result<FamilyRow, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        // CP4: both credentials are minted together — the deposit token is
        // the deterministic attenuation of the member token, so re-provision
        // (webhook retry, renewal) converges on the same pair.
        conn.execute(
            "INSERT INTO families
                (token, status, plan, quota_bytes, created_ms, expires_ms, note, deposit_token)
             VALUES (?1, 'active', ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(token) DO UPDATE SET
                status = 'active',
                plan = excluded.plan,
                quota_bytes = excluded.quota_bytes,
                expires_ms = excluded.expires_ms,
                note = excluded.note,
                deposit_token = excluded.deposit_token",
            params![
                token,
                plan,
                quota_bytes.map(|q| q as i64),
                now_ms,
                expires_ms,
                note,
                deposit_token_for(token),
            ],
        )
        .map_err(|e| e.to_string())?;
        get_family_on(&conn, token)?.ok_or_else(|| "family vanished after upsert".to_string())
    }

    pub fn get_family(&self, token: &str) -> Result<Option<FamilyRow>, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        get_family_on(&conn, token)
    }

    /// CP4: resolve a presented bearer credential (member or deposit) to its
    /// family row and token class. See `get_family_by_credential_on`.
    pub fn get_family_by_credential(
        &self,
        credential: &str,
    ) -> Result<Option<(FamilyRow, TokenClass)>, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        get_family_by_credential_on(&conn, credential)
    }

    /// One page of families, newest-provisioned last, each with the usage
    /// figures `get_family`'s handler reports.
    ///
    /// The usage columns are computed in the same statement rather than by
    /// calling `family_sealed_bytes` / `count_for_family` per row: a page of
    /// 500 families would otherwise be 1001 separate queries, each retaking
    /// the store mutex. The aggregate must stay byte-identical to those two
    /// helpers (no expiry predicate — they count every stored row, expired
    /// or not) or the same family would report different usage depending on
    /// whether you listed it or fetched it.
    ///
    /// `total` counts every family matching `status_filter`, not just this
    /// page, so a caller can tell a full page from the end of the list.
    pub fn list_families(
        &self,
        status_filter: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<FamilyPage, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM families WHERE (?1 IS NULL OR status = ?1)",
                params![status_filter],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT f.token, f.status, f.plan, f.quota_bytes, f.created_ms,
                        f.expires_ms, f.note, f.deposit_token,
                        COALESCE(SUM(LENGTH(e.sealed)), 0), COUNT(e.id)
                 FROM families f
                 LEFT JOIN envelopes e ON e.family_token = f.token
                 WHERE (?1 IS NULL OR f.status = ?1)
                 GROUP BY f.token
                 ORDER BY f.created_ms ASC, f.token ASC
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| e.to_string())?;
        let families = stmt
            .query_map(params![status_filter, limit as i64, offset as i64], |row| {
                Ok(FamilyUsage {
                    family: family_row_from(row)?,
                    usage_bytes: row.get::<_, i64>(8)? as u64,
                    envelope_count: row.get::<_, i64>(9)? as u64,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(FamilyPage {
            families,
            total: total as u64,
        })
    }

    /// Partial update: `Some` fields are applied, `None` fields keep their
    /// stored value (no way to clear a field — re-provision for that).
    /// Returns the updated row, or `None` if the family does not exist.
    pub fn patch_family(
        &self,
        token: &str,
        status: Option<&str>,
        plan: Option<&str>,
        quota_bytes: Option<u64>,
        expires_ms: Option<i64>,
        note: Option<&str>,
    ) -> Result<Option<FamilyRow>, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        conn.execute(
            "UPDATE families SET
                status = COALESCE(?2, status),
                plan = COALESCE(?3, plan),
                quota_bytes = COALESCE(?4, quota_bytes),
                expires_ms = COALESCE(?5, expires_ms),
                note = COALESCE(?6, note)
             WHERE token = ?1",
            params![
                token,
                status,
                plan,
                quota_bytes.map(|q| q as i64),
                expires_ms,
                note,
            ],
        )
        .map_err(|e| e.to_string())?;
        get_family_on(&conn, token)
    }

    /// Revoke a family and purge everything it stored (envelopes and
    /// presence). Returns `false` if no such family existed.
    pub fn delete_family(&self, token: &str) -> Result<bool, String> {
        let mut conn = self.conn.lock().expect("relay store mutex poisoned");
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM envelopes WHERE family_token = ?1",
            params![token],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM presence WHERE family_token = ?1",
            params![token],
        )
        .map_err(|e| e.to_string())?;
        let deleted = tx
            .execute("DELETE FROM families WHERE token = ?1", params![token])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(deleted > 0)
    }

    /// `EXPLAIN QUERY PLAN` for the fetch path. Used by tests to ensure the
    /// family+hint+id index is used instead of a table scan. Mirrors
    /// `fetch_envelopes`'s real query (including the FR7 `expiry_ms`
    /// predicate) so the plan tested here is the plan that actually runs.
    pub fn explain_fetch_plan(
        &self,
        family_token: &str,
        hints: &[Vec<u8>],
        after_id: i64,
        limit: usize,
        now_ms: i64,
    ) -> Result<String, String> {
        if hints.is_empty() {
            return Ok(String::new());
        }
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let hint_placeholders = (0..hints.len())
            .map(|index| format!("?{}", index + 3))
            .collect::<Vec<_>>()
            .join(",");
        let now_placeholder = hints.len() + 3;
        let limit_placeholder = hints.len() + 4;
        let sql = format!(
            "EXPLAIN QUERY PLAN
             SELECT id, msg_id, hop_ttl, recipient_hint, sealed, expiry_ms, created_at_ms
             FROM envelopes
             WHERE family_token = ?1 AND id > ?2 AND recipient_hint IN ({hint_placeholders})
                   AND expiry_ms > ?{now_placeholder}
             ORDER BY id ASC
             LIMIT ?{limit_placeholder}"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let fetch_limit = limit.min(MAX_FETCH_LIMIT) as i64;
        let mut bindings: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(hints.len() + 4);
        bindings.push(&family_token);
        bindings.push(&after_id);
        for hint in hints {
            bindings.push(hint);
        }
        bindings.push(&now_ms);
        bindings.push(&fetch_limit);
        let mut lines = Vec::new();
        let rows = stmt
            .query_map(bindings.as_slice(), |row| {
                // EXPLAIN QUERY PLAN columns: id, parent, notused, detail
                let detail: String = row.get(3)?;
                Ok(detail)
            })
            .map_err(|e| e.to_string())?;
        for row in rows {
            lines.push(row.map_err(|e| e.to_string())?);
        }
        Ok(lines.join("\n"))
    }
}

fn family_row_from(row: &rusqlite::Row<'_>) -> Result<FamilyRow, rusqlite::Error> {
    let token: String = row.get(0)?;
    // Backfilled at startup (`migrate_families_deposit_token`), so NULL is
    // only reachable in a torn mid-migration read; derive defensively rather
    // than fail the whole request.
    let deposit_token = row
        .get::<_, Option<String>>(7)?
        .unwrap_or_else(|| deposit_token_for(&token));
    Ok(FamilyRow {
        token,
        status: row.get(1)?,
        plan: row.get(2)?,
        quota_bytes: row.get::<_, Option<i64>>(3)?.map(|q| q as u64),
        created_ms: row.get(4)?,
        expires_ms: row.get(5)?,
        note: row.get(6)?,
        deposit_token,
    })
}

fn get_family_on(conn: &Connection, token: &str) -> Result<Option<FamilyRow>, String> {
    conn.query_row(
        "SELECT token, status, plan, quota_bytes, created_ms, expires_ms, note, deposit_token
         FROM families WHERE token = ?1",
        params![token],
        family_row_from,
    )
    .optional()
    .map_err(|e| e.to_string())
}

/// CP4: resolve a presented bearer credential to its family row and class —
/// member (`token` column) or deposit (`deposit_token` column). An exact
/// member match wins deterministically if a credential somehow appeared in
/// both columns (provisioning validation forbids creating that state).
fn get_family_by_credential_on(
    conn: &Connection,
    credential: &str,
) -> Result<Option<(FamilyRow, TokenClass)>, String> {
    let family = conn
        .query_row(
            "SELECT token, status, plan, quota_bytes, created_ms, expires_ms, note, deposit_token
             FROM families WHERE token = ?1 OR deposit_token = ?1
             ORDER BY (token = ?1) DESC LIMIT 1",
            params![credential],
            family_row_from,
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(family.map(|row| {
        let class = if row.token == credential {
            TokenClass::Member
        } else {
            TokenClass::Deposit
        };
        (row, class)
    }))
}

fn family_sealed_bytes_on(conn: &Connection, family_token: &str) -> Result<u64, String> {
    // SUM() over zero matching rows returns one row with a SQL NULL.
    let total: Option<Option<i64>> = conn
        .query_row(
            "SELECT SUM(LENGTH(sealed)) FROM envelopes WHERE family_token = ?1",
            params![family_token],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(total.flatten().unwrap_or(0) as u64)
}

/// FR7: `SCHEMA`'s leading `PRAGMA auto_vacuum = INCREMENTAL` only takes
/// effect on a database with *no tables yet* -- for a brand-new
/// `RelayStore::open`, that pragma runs (as the first statement of the
/// `execute_batch` call) before `CREATE TABLE`, and the mode sticks. Every
/// relayd database created before this change defaulted to
/// `auto_vacuum = NONE`; for those, the pragma statement in `SCHEMA` is a
/// silent no-op once tables already exist. Converting an existing database
/// requires re-running the pragma immediately followed by a full `VACUUM`
/// -- SQLite's documented way to toggle auto-vacuum on an existing
/// database (https://www.sqlite.org/pragma.html#pragma_auto_vacuum:
/// "turning it from off to on requires a VACUUM ... to reorganize the
/// database and initialize the pointer-map pages"). See DEPLOY.md for the
/// deploy-facing version of this note.
///
/// We check the *current* mode first so the `VACUUM` -- which holds an
/// exclusive lock and rewrites the whole file -- runs at most once per
/// database, not on every process start: once converted, `PRAGMA
/// auto_vacuum` reports `incremental` on every later `open()` and this
/// becomes a single cheap read. (On a genuinely fresh database the SCHEMA
/// pragma has already set the mode by the time we get here, so this is
/// also a cheap no-op-VACUUM path for new installs, not just repeat opens
/// of an existing one.)
fn ensure_incremental_auto_vacuum(conn: &Connection) -> Result<(), String> {
    const INCREMENTAL: i64 = 2;
    let mode: i64 = conn
        .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
        .map_err(|e| e.to_string())?;
    if mode != INCREMENTAL {
        conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL; VACUUM;")
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// CP4 startup migration, following the same pattern as every previous
/// schema change (idempotent, self-applying, no operator step):
///
/// 1. `ALTER TABLE families ADD COLUMN deposit_token` when the column is
///    missing (databases created before CP4; `SCHEMA`'s `CREATE TABLE IF NOT
///    EXISTS` is a no-op for them and cannot add columns).
/// 2. Backfill `deposit_token = deposit_token_for(token)` for every row
///    where it is NULL — the migration default is therefore *member class*
///    for every existing token: nothing about how those tokens authenticate
///    changes, they simply gain a derived post-only counterpart.
/// 3. A UNIQUE index on `deposit_token`, created here rather than in
///    `SCHEMA` because on a pre-CP4 database `SCHEMA` runs before the column
///    exists.
fn migrate_families_deposit_token(conn: &Connection) -> Result<(), String> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(families)")
        .map_err(|e| e.to_string())?;
    let has_column = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .any(|name| name == "deposit_token");
    drop(stmt);
    if !has_column {
        conn.execute("ALTER TABLE families ADD COLUMN deposit_token TEXT", [])
            .map_err(|e| e.to_string())?;
    }
    let missing: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT token FROM families WHERE deposit_token IS NULL")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    for token in &missing {
        conn.execute(
            "UPDATE families SET deposit_token = ?2 WHERE token = ?1",
            params![token, deposit_token_for(token)],
        )
        .map_err(|e| e.to_string())?;
    }
    if !missing.is_empty() {
        info!(
            families = missing.len(),
            "CP4 migration: derived deposit tokens for existing families"
        );
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_families_deposit_token
             ON families(deposit_token);",
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn prune_expired_on(conn: &Connection, now_ms: i64) -> Result<u64, String> {
    let retention_floor = now_ms.saturating_sub(MAX_RETENTION_MS);
    let deleted = conn
        .execute(
            "DELETE FROM envelopes
             WHERE expiry_ms <= ?1 OR created_at_ms <= ?2",
            params![now_ms, retention_floor],
        )
        .map_err(|e| e.to_string())?;
    let presence_floor = now_ms.saturating_sub(PRESENCE_RETENTION_MS);
    let deleted_presence = conn
        .execute(
            "DELETE FROM presence WHERE last_seen_ms <= ?1",
            params![presence_floor],
        )
        .map_err(|e| e.to_string())?;
    Ok((deleted + deleted_presence) as u64)
}

/// FR7: hourly (by default) background maintenance -- expiry pruning used
/// to run only inside request handlers (`fetch_envelopes` ran a `DELETE`
/// on every poll, `sync_presence` and the quota-overflow path also
/// pruned), so a mailbox nobody was actively polling would just grow
/// forever, and there was no `VACUUM`/`incremental_vacuum` call anywhere,
/// so the SQLite file never shrank even after mass expiry -- disk-full on
/// the $4 VPS would have surfaced as an unlogged raw 500. This spawns a
/// detached task that runs `prune_expired` + `incremental_vacuum` on
/// `interval`, independent of client traffic.
///
/// `interval` is a parameter rather than a hardcoded constant so a test
/// can use a millisecond-scale interval instead of waiting on the
/// hour-scale production cadence (`DEFAULT_PRUNE_INTERVAL`); the returned
/// `JoinHandle` lets a test `.abort()` the task during cleanup.
pub fn spawn_prune_task(store: RelayStore, interval: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(interval);
        // `interval` fires its first tick immediately; skip it so the
        // very first sweep happens one interval after startup, not the
        // instant the task is spawned (racing schema/table setup).
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(detail) = store.prune_expired(now_ms()) {
                // prune_expired already logs a nonzero deleted count (FR2
                // style); a failure here is the only thing worth an extra
                // log line.
                tracing::error!(detail = %detail, "background maintenance: prune_expired failed");
                continue;
            }
            if let Err(detail) = store.incremental_vacuum() {
                tracing::error!(detail = %detail, "background maintenance: incremental_vacuum failed");
            }
        }
    })
}

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_handler))
        .route("/envelopes", post(post_envelope).get(get_envelopes))
        .route("/envelopes/ack", post(ack_envelopes))
        .route("/presence", post(sync_presence))
        .route(
            "/admin/families",
            post(admin_provision_family).get(admin_list_families),
        )
        .route(
            "/admin/families/{token}",
            get(admin_get_family)
                .patch(admin_patch_family)
                .delete(admin_delete_family),
        )
        .with_state(state)
}

pub fn parse_tokens(raw: &str) -> HashSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn parse_bind(raw: &str) -> Result<SocketAddr, String> {
    raw.parse::<SocketAddr>().map_err(|e| e.to_string())
}

/// Parse `CRUISEMESH_RELAY_FAMILY_QUOTA_BYTES` (DTN_TODOS.md D7,
/// `DEPLOY.md` §5). `0` is rejected — a family with a zero quota could
/// never post anything, which is never what an operator means; unset the
/// env var (or pass the default) to disable an override.
pub fn parse_family_quota_bytes(raw: &str) -> Result<u64, String> {
    let value: u64 = raw
        .parse()
        .map_err(|_| format!("not a valid byte count: {raw:?}"))?;
    if value == 0 {
        return Err("family quota must be greater than 0 bytes".to_string());
    }
    Ok(value)
}

/// FR6: parse `CRUISEMESH_RELAY_WS_PER_TOKEN_MAX_CONNECTIONS` /
/// `CRUISEMESH_RELAY_WS_GLOBAL_MAX_CONNECTIONS` (see `DEPLOY.md`). `0` is
/// rejected for the same reason as the family quota above -- it would mean
/// "no client can ever open a websocket", never what an operator means;
/// unset the env var to keep the default.
pub fn parse_ws_connection_cap(raw: &str) -> Result<usize, String> {
    let value: usize = raw
        .parse()
        .map_err(|_| format!("not a valid connection count: {raw:?}"))?;
    if value == 0 {
        return Err("websocket connection cap must be greater than 0".to_string());
    }
    Ok(value)
}

/// Parse `CRUISEMESH_RELAY_RATE_REQUESTS_PER_MIN` /
/// `CRUISEMESH_RELAY_RATE_GLOBAL_REQUESTS_PER_MIN` (see `DEPLOY.md` §10).
/// `0` is rejected for the same reason as the caps above -- it would mean
/// "no client may ever call the API", never what an operator means; unset
/// the env var to keep the default.
pub fn parse_rate_requests_per_min(raw: &str) -> Result<u32, String> {
    let value: u32 = raw
        .parse()
        .map_err(|_| format!("not a valid request rate: {raw:?}"))?;
    if value == 0 {
        return Err("request rate limit must be greater than 0 per minute".to_string());
    }
    Ok(value)
}

/// Parse `CRUISEMESH_RELAY_RATE_BYTES_PER_MIN` (see `DEPLOY.md` §10). `0` is
/// rejected -- a family that may upload zero bytes per minute could never
/// post anything, same reasoning as the family storage quota above.
pub fn parse_rate_bytes_per_min(raw: &str) -> Result<u64, String> {
    let value: u64 = raw
        .parse()
        .map_err(|_| format!("not a valid byte rate: {raw:?}"))?;
    if value == 0 {
        return Err("byte rate limit must be greater than 0 per minute".to_string());
    }
    Ok(value)
}

#[derive(Serialize)]
struct HealthzResponse {
    status: &'static str,
    version: &'static str,
    commit: &'static str,
}

async fn healthz() -> Json<HealthzResponse> {
    Json(HealthzResponse {
        status: "ok",
        version: VERSION,
        commit: GIT_SHA,
    })
}

#[derive(Deserialize)]
struct PostEnvelopeRequest {
    msg_id: String,
    hop_ttl: u8,
    recipient_hint: String,
    sealed: String,
    expiry_ms: i64,
}

#[derive(Serialize)]
struct PostEnvelopeResponse {
    id: i64,
}

async fn post_envelope(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PostEnvelopeRequest>,
) -> Result<Json<PostEnvelopeResponse>, ApiError> {
    let access = authorize_bearer(&state, &headers, FamilyOp::Post).await?;
    // CP4: rows are always stored (and broadcast) under the canonical
    // *member* token — a deposit credential deposits into the family's one
    // mailbox, where member-class fetches actually look.
    let family_token = access.token.clone();
    // Rate limit the request unit the moment the caller is known-good, and
    // before any decoding work: a token spraying malformed payloads is still
    // spending server time, so it must still be charged. The byte dimension
    // is charged separately below, once the payload's real size is known.
    // Charged per presented credential class (member vs deposit buckets).
    state.check_rate_limit(&access, 1.0, 0.0)?;
    let msg_id = decode_base64_field(&request.msg_id, "msg_id")?;
    if msg_id.len() != MSG_ID_LEN {
        return Err(ApiError::bad_request(format!(
            "msg_id must be {MSG_ID_LEN} bytes after base64url decoding"
        )));
    }
    let recipient_hint = decode_base64_field(&request.recipient_hint, "recipient_hint")?;
    if recipient_hint.len() != RECIPIENT_HINT_LEN {
        return Err(ApiError::bad_request(format!(
            "recipient_hint must be {RECIPIENT_HINT_LEN} bytes after base64url decoding"
        )));
    }
    let sealed = decode_base64_field(&request.sealed, "sealed")?;
    if sealed.is_empty() {
        return Err(ApiError::bad_request(
            "sealed must not be empty".to_string(),
        ));
    }
    // DTN_TODOS.md D7: per-envelope size cap, checked before any storage
    // work (see MAX_ENVELOPE_SEALED_BYTES doc comment for the derivation).
    if sealed.len() > MAX_ENVELOPE_SEALED_BYTES {
        warn!(
            family = %token_prefix(&family_token),
            bytes = sealed.len(),
            cap = MAX_ENVELOPE_SEALED_BYTES,
            "envelope rejected: over the per-envelope size cap (413)"
        );
        return Err(ApiError::envelope_too_large(sealed.len()));
    }
    // Byte allowance, charged after the per-envelope cap so a family is
    // never billed for bytes the server has already refused to store. A
    // dedupe re-post *is* charged (unlike the storage quota, which exempts
    // it): the bytes crossed the wire and were decoded either way.
    state.check_rate_limit(&access, 0.0, sealed.len() as f64)?;
    let now = now_ms();

    // Dedupe, quota accounting, expiry pruning, and insertion are one store
    // transaction so concurrent posts cannot all pass the same usage check.
    // FR8: off the tokio reactor via run_blocking; clone what the closure
    // needs so `family_token`/`msg_id`/`recipient_hint`/`sealed` stay
    // available below for logging, the response body, and the broadcast.
    let insert_family = family_token.clone();
    let insert_msg_id = msg_id.clone();
    let insert_hint = recipient_hint.clone();
    let insert_sealed = sealed.clone();
    let hop_ttl = request.hop_ttl;
    let expiry_ms_req = request.expiry_ms;
    // Per-family quota override (hosted families) falls back to the server
    // default inside `authorize_family`; FR8 keeps the write off the reactor.
    let family_quota_bytes = access.quota_bytes;
    let result = state
        .store
        .run_blocking(move |store| {
            store.insert_envelope_with_quota(
                &insert_family,
                insert_msg_id,
                hop_ttl,
                insert_hint,
                insert_sealed,
                expiry_ms_req,
                now,
                family_quota_bytes,
            )
        })
        .await
        .map_err(ApiError::internal)?;
    let id = match result {
        QuotaInsertResult::Stored { id } => id,
        QuotaInsertResult::QuotaExceeded { usage_bytes } => {
            warn!(
                family = %token_prefix(&family_token),
                usage_bytes,
                quota_bytes = access.quota_bytes,
                "envelope rejected: family storage quota exceeded (507)"
            );
            return Err(ApiError::family_quota_exceeded(
                usage_bytes,
                access.quota_bytes,
            ));
        }
    };
    // FR2: never log envelope contents (msg_id/sealed bytes) -- only the
    // family-token prefix (for correlation, not the full semi-public
    // bearer token) and the stored size.
    info!(
        family = %token_prefix(&family_token),
        bytes = sealed.len(),
        id,
        "envelope stored"
    );

    let envelope = EnvelopeResponse {
        id,
        msg_id: encode_base64_field(&msg_id),
        hop_ttl: request.hop_ttl,
        recipient_hint: encode_base64_field(&recipient_hint),
        sealed: encode_base64_field(&sealed),
        expiry_ms: RelayStore::effective_expiry(now, request.expiry_ms),
        created_at_ms: now,
    };
    // Fan-out for live WS subscribers. Lagging peers are dropped (module docs).
    let _ = state.tx.send(std::sync::Arc::new(BroadcastEnvelope {
        family_token,
        recipient_hint: encode_base64_field(&recipient_hint),
        envelope,
    }));

    Ok(Json(PostEnvelopeResponse { id }))
}

#[derive(Deserialize)]
struct GetEnvelopesQuery {
    hints: String,
    after: Option<i64>,
    limit: Option<usize>,
}

#[derive(serde::Serialize, Clone, Debug)]
pub struct EnvelopeResponse {
    id: i64,
    msg_id: String,
    hop_ttl: u8,
    recipient_hint: String,
    sealed: String,
    expiry_ms: i64,
    created_at_ms: i64,
}

#[derive(Serialize)]
struct GetEnvelopesResponse {
    envelopes: Vec<EnvelopeResponse>,
    next_cursor: i64,
}

async fn get_envelopes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<GetEnvelopesQuery>,
) -> Result<Json<GetEnvelopesResponse>, ApiError> {
    let access = authorize_bearer(&state, &headers, FamilyOp::Read).await?;
    // Enforced only once the token has authorized (see `check_rate_limit`).
    state.check_rate_limit(&access, 1.0, 0.0)?;
    let family_token = access.token;
    let (hints, _) = decode_fetch_hints(&query.hints)?;
    let after = query.after.unwrap_or(0);
    if after < 0 {
        return Err(ApiError::bad_request(
            "after must be non-negative".to_string(),
        ));
    }
    let limit = query
        .limit
        .unwrap_or(DEFAULT_FETCH_LIMIT)
        .min(MAX_FETCH_LIMIT);
    // FR8: off the tokio reactor -- this is the hottest read path in the
    // service (every client poll).
    let fetch_family = family_token.clone();
    let now = now_ms();
    let rows = state
        .store
        .run_blocking(move |store| store.fetch_envelopes(&fetch_family, hints, after, limit, now))
        .await
        .map_err(ApiError::internal)?;
    // next_cursor stays at `after` when the page is empty so clients can
    // keep polling without inventing a sentinel. Rows remain until ack —
    // advancing the cursor does not delete.
    let next_cursor = rows.last().map(|row| row.id).unwrap_or(after);
    Ok(Json(GetEnvelopesResponse {
        next_cursor,
        envelopes: rows
            .into_iter()
            .map(|row| EnvelopeResponse {
                id: row.id,
                msg_id: encode_base64_field(&row.msg_id),
                hop_ttl: row.hop_ttl,
                recipient_hint: encode_base64_field(&row.recipient_hint),
                sealed: encode_base64_field(&row.sealed),
                expiry_ms: row.expiry_ms,
                created_at_ms: row.created_at_ms,
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
struct AckRequest {
    ids: Vec<i64>,
}

#[derive(Serialize)]
struct AckResponse {
    deleted: u64,
}

async fn ack_envelopes(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AckRequest>,
) -> Result<Json<AckResponse>, ApiError> {
    let access = authorize_bearer(&state, &headers, FamilyOp::Read).await?;
    // Enforced only once the token has authorized (see `check_rate_limit`).
    state.check_rate_limit(&access, 1.0, 0.0)?;
    let family_token = access.token;
    if request.ids.len() > MAX_ACK_IDS {
        return Err(ApiError::bad_request(format!(
            "ids must contain at most {MAX_ACK_IDS} entries"
        )));
    }
    if request.ids.iter().any(|id| *id <= 0) {
        return Err(ApiError::bad_request(
            "ids must contain only positive relay ids".to_string(),
        ));
    }
    let mut ids = request.ids;
    ids.sort_unstable();
    ids.dedup();
    let deleted = state
        .store
        .run_blocking(move |store| store.ack_envelopes(&family_token, ids))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(AckResponse { deleted }))
}

#[derive(Deserialize)]
struct PresenceRequest {
    announce: Vec<String>,
    query: Vec<String>,
}

#[derive(Serialize)]
struct PresenceItem {
    hint: String,
    last_seen_ms: i64,
}

#[derive(Serialize)]
struct PresenceResponse {
    now_ms: i64,
    presence: Vec<PresenceItem>,
}

async fn sync_presence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PresenceRequest>,
) -> Result<Json<PresenceResponse>, ApiError> {
    let access = authorize_bearer(&state, &headers, FamilyOp::Read).await?;
    // Enforced only once the token has authorized (see `check_rate_limit`).
    state.check_rate_limit(&access, 1.0, 0.0)?;
    let family_token = access.token;
    if request.announce.len() > MAX_PRESENCE_ANNOUNCE {
        return Err(ApiError::bad_request(format!(
            "announce must contain at most {MAX_PRESENCE_ANNOUNCE} hints"
        )));
    }
    if request.query.len() > MAX_PRESENCE_QUERY {
        return Err(ApiError::bad_request(format!(
            "query must contain at most {MAX_PRESENCE_QUERY} hints"
        )));
    }
    let announce = decode_presence_hints(&request.announce, "announce")?;
    let query = decode_presence_hints(&request.query, "query")?;
    let now = now_ms();
    let rows = state
        .store
        .run_blocking(move |store| store.sync_presence(&family_token, &announce, &query, now))
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(PresenceResponse {
        now_ms: now,
        presence: rows
            .into_iter()
            .map(|row| PresenceItem {
                hint: encode_base64_field(&row.hint),
                last_seen_ms: row.last_seen_ms,
            })
            .collect(),
    }))
}

fn decode_presence_hints(values: &[String], field: &str) -> Result<Vec<Vec<u8>>, ApiError> {
    let mut seen = HashSet::with_capacity(values.len());
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let hint = decode_base64_field(value, field)?;
        if hint.len() != RECIPIENT_HINT_LEN {
            return Err(ApiError::bad_request(format!(
                "{field} entries must be {RECIPIENT_HINT_LEN} bytes after base64url decoding"
            )));
        }
        if seen.insert(hint.clone()) {
            out.push(hint);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Admin API — hosted-relay ("Cruise Pass") provisioning. Every route requires
// `Authorization: Bearer <CRUISEMESH_RELAY_ADMIN_TOKEN>` and answers 404 when
// no admin token is configured (the self-hosted default). The caller is the
// cruisemesh.app purchase Worker; all operations are idempotent because
// Stripe webhooks retry on failure.

#[derive(Deserialize)]
struct ProvisionFamilyRequest {
    token: String,
    plan: Option<String>,
    quota_bytes: Option<u64>,
    expires_ms: Option<i64>,
    note: Option<String>,
}

#[derive(Deserialize)]
struct PatchFamilyRequest {
    status: Option<String>,
    plan: Option<String>,
    quota_bytes: Option<u64>,
    expires_ms: Option<i64>,
    note: Option<String>,
}

#[derive(Serialize)]
struct FamilyResponse {
    token: String,
    /// CP4: the family's post-only credential, minted alongside `token` at
    /// provisioning. The purchase flow puts `token` on the Cruise Pass setup
    /// card; `deposit_token` is what friend cards carry (phones derive the
    /// same value locally, so nothing needs to distribute it — it is
    /// returned here so the operator can see/verify it).
    deposit_token: String,
    status: String,
    plan: Option<String>,
    /// Per-family override, if any (`null` = server default applies).
    quota_bytes: Option<u64>,
    /// The quota actually enforced (override or server default).
    effective_quota_bytes: u64,
    created_ms: i64,
    expires_ms: Option<i64>,
    note: Option<String>,
    usage_bytes: u64,
    envelope_count: u64,
}

/// Shape one family for the wire. Split from `family_response` so the list
/// endpoint, which already has the usage figures from its aggregate query,
/// produces byte-identical JSON without re-querying per row.
fn family_response_with_usage(
    state: &AppState,
    row: FamilyRow,
    usage_bytes: u64,
    envelope_count: u64,
) -> FamilyResponse {
    FamilyResponse {
        effective_quota_bytes: row.quota_bytes.unwrap_or(state.family_quota_bytes),
        token: row.token,
        deposit_token: row.deposit_token,
        status: row.status,
        plan: row.plan,
        quota_bytes: row.quota_bytes,
        created_ms: row.created_ms,
        expires_ms: row.expires_ms,
        note: row.note,
        usage_bytes,
        envelope_count,
    }
}

fn family_response(state: &AppState, row: FamilyRow) -> Result<FamilyResponse, ApiError> {
    let usage_bytes = state
        .store
        .family_sealed_bytes(&row.token)
        .map_err(ApiError::internal)?;
    let envelope_count = state
        .store
        .count_for_family(&row.token)
        .map_err(ApiError::internal)?;
    Ok(family_response_with_usage(
        state,
        row,
        usage_bytes,
        envelope_count,
    ))
}

fn validate_quota_bytes(quota_bytes: Option<u64>) -> Result<(), ApiError> {
    if quota_bytes == Some(0) {
        return Err(ApiError::bad_request(
            "quota_bytes must be greater than 0 (omit it for the server default)".to_string(),
        ));
    }
    Ok(())
}

async fn admin_provision_family(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ProvisionFamilyRequest>,
) -> Result<Json<FamilyResponse>, ApiError> {
    authorize_admin(&state, &headers)?;
    let token = request.token.trim();
    if token.is_empty() || token.len() > MAX_FAMILY_TOKEN_LEN {
        return Err(ApiError::bad_request(format!(
            "token must be 1..={MAX_FAMILY_TOKEN_LEN} characters"
        )));
    }
    if token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ApiError::bad_request(
            "token must not contain whitespace or control characters".to_string(),
        ));
    }
    // A family token that shadows an operator credential would let a paying
    // customer call the admin API (or vice versa) — never allow the overlap.
    if state.admin_token.as_deref() == Some(token) || state.auth_tokens.contains(token) {
        return Err(ApiError::bad_request(
            "token collides with a server-configured token".to_string(),
        ));
    }
    // CP4: every deposit token carries the class prefix, so forbidding it on
    // member tokens is what keeps a presented credential's class — and the
    // `WHERE token = ? OR deposit_token = ?` auth lookup — unambiguous.
    if is_deposit_token(token) {
        return Err(ApiError::bad_request(format!(
            "token must not start with the deposit-token prefix {DEPOSIT_TOKEN_PREFIX:?}"
        )));
    }
    validate_quota_bytes(request.quota_bytes)?;
    let row = state
        .store
        .upsert_family(
            token,
            request.plan.as_deref(),
            request.quota_bytes,
            request.expires_ms,
            request.note.as_deref(),
            now_ms(),
        )
        .map_err(ApiError::internal)?;
    info!(
        family = %token_prefix(&row.token),
        plan = row.plan.as_deref().unwrap_or("-"),
        expires_ms = row.expires_ms.unwrap_or(0),
        "family provisioned"
    );
    Ok(Json(family_response(&state, row)?))
}

/// Every field is `Option<String>` and parsed by hand below, deliberately:
/// a typed `Option<usize>` would make axum's `Query` extractor reject
/// `?limit=abc` with a 400 *before* the handler runs `authorize_admin`,
/// which would tell an unauthenticated prober that the route exists on a
/// deploy whose admin API is off (those must always answer 404).
#[derive(Deserialize)]
struct ListFamiliesQuery {
    status: Option<String>,
    limit: Option<String>,
    offset: Option<String>,
}

#[derive(Serialize)]
struct FamilyListResponse {
    families: Vec<FamilyResponse>,
    /// Families matching `status` across the whole table, not just this page.
    total: u64,
    limit: usize,
    offset: usize,
}

fn parse_list_bound(raw: Option<&String>, field: &str) -> Result<Option<usize>, ApiError> {
    let Some(raw) = raw else { return Ok(None) };
    raw.parse::<usize>()
        .map(Some)
        .map_err(|_| ApiError::bad_request(format!("{field} must be a non-negative integer")))
}

async fn admin_list_families(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListFamiliesQuery>,
) -> Result<Json<FamilyListResponse>, ApiError> {
    authorize_admin(&state, &headers)?;
    if let Some(status) = query.status.as_deref() {
        if status != "active" && status != "suspended" {
            return Err(ApiError::bad_request(
                "status must be \"active\" or \"suspended\"".to_string(),
            ));
        }
    }
    // Clamped, not rejected — see MAX_FAMILY_LIST_LIMIT.
    let limit = parse_list_bound(query.limit.as_ref(), "limit")?
        .unwrap_or(DEFAULT_FAMILY_LIST_LIMIT)
        .clamp(1, MAX_FAMILY_LIST_LIMIT);
    let offset = parse_list_bound(query.offset.as_ref(), "offset")?.unwrap_or(0);
    let status = query.status.clone();
    let page = state
        .store
        .run_blocking(move |store| store.list_families(status.as_deref(), limit, offset))
        .await
        .map_err(ApiError::internal)?;
    let families = page
        .families
        .into_iter()
        // Full tokens, same as GET /admin/families/{token} — the caller is
        // already holding the admin credential and needs them to re-issue a
        // setup link. Mask at the display layer, not here.
        .map(|entry| {
            family_response_with_usage(
                &state,
                entry.family,
                entry.usage_bytes,
                entry.envelope_count,
            )
        })
        .collect();
    Ok(Json(FamilyListResponse {
        families,
        total: page.total,
        limit,
        offset,
    }))
}

async fn admin_get_family(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Json<FamilyResponse>, ApiError> {
    authorize_admin(&state, &headers)?;
    let row = state
        .store
        .get_family(&token)
        .map_err(ApiError::internal)?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(family_response(&state, row)?))
}

async fn admin_patch_family(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Json(request): Json<PatchFamilyRequest>,
) -> Result<Json<FamilyResponse>, ApiError> {
    authorize_admin(&state, &headers)?;
    if let Some(status) = request.status.as_deref() {
        if status != "active" && status != "suspended" {
            return Err(ApiError::bad_request(
                "status must be \"active\" or \"suspended\"".to_string(),
            ));
        }
    }
    validate_quota_bytes(request.quota_bytes)?;
    let row = state
        .store
        .patch_family(
            &token,
            request.status.as_deref(),
            request.plan.as_deref(),
            request.quota_bytes,
            request.expires_ms,
            request.note.as_deref(),
        )
        .map_err(ApiError::internal)?
        .ok_or_else(ApiError::not_found)?;
    info!(
        family = %token_prefix(&row.token),
        status = %row.status,
        expires_ms = row.expires_ms.unwrap_or(0),
        "family updated"
    );
    Ok(Json(family_response(&state, row)?))
}

#[derive(Serialize)]
struct DeleteFamilyResponse {
    deleted: bool,
}

async fn admin_delete_family(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> Result<Json<DeleteFamilyResponse>, ApiError> {
    authorize_admin(&state, &headers)?;
    let deleted = state
        .store
        .delete_family(&token)
        .map_err(ApiError::internal)?;
    if !deleted {
        return Err(ApiError::not_found());
    }
    info!(family = %token_prefix(&token), "family revoked and purged");
    Ok(Json(DeleteFamilyResponse { deleted: true }))
}

#[derive(Deserialize)]
struct WsQuery {
    hints: String,
    after: Option<i64>,
    token: Option<String>,
}

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<WsQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    // Prefer Authorization header when present (native clients); fall back to
    // ?token= for browsers that cannot set WS handshake headers. WS is
    // delivery-only, so it counts as a Read op for expiry-grace purposes —
    // and, CP4, deposit-class credentials are therefore rejected with the
    // same structured 403 `deposit_only` as every other read. When the
    // header fails and no query token exists, the header's real error is
    // propagated (not a generic "missing token") so that 403 stays visible.
    let header_result = authorize_bearer(&state, &headers, FamilyOp::Read).await;
    let access = match (
        header_result,
        query.token.as_deref().filter(|t| !t.is_empty()),
    ) {
        (Ok(access), _) => access,
        (Err(_), Some(query_token)) => {
            authorize_family(&state, query_token, FamilyOp::Read, now_ms()).await?
        }
        (Err(header_error), None) => {
            if headers.contains_key(AUTHORIZATION) {
                return Err(header_error);
            }
            return Err(ApiError::unauthorized(
                "missing family token (Authorization: Bearer or ?token=)".to_string(),
            ));
        }
    };
    // One request unit per *upgrade attempt* (frames on an established
    // socket are not charged -- the connection caps below bound those).
    // Enforced only once the token has authorized, whichever of the two auth
    // paths above resolved it (see `check_rate_limit`).
    state.check_rate_limit(&access, 1.0, 0.0)?;
    let token = access.token;
    let (hints, hints_base64) = decode_fetch_hints(&query.hints)?;
    let after = query.after.unwrap_or(0);
    if after < 0 {
        return Err(ApiError::bad_request(
            "after must be non-negative".to_string(),
        ));
    }

    // FR6: admission control before the upgrade -- reject fast under both
    // the coarse global cap and the per-token cap. Acquiring *owned*
    // permits lets them move into the socket task and live exactly as long
    // as that task; whichever path ends the connection (client close,
    // lag-drop, write-timeout, keepalive reap) drops the permit and frees
    // the slot automatically.
    let global_permit = match state.ws_global.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            warn!(
                family = %token_prefix(&token),
                "ws upgrade rejected: global connection cap reached (429)"
            );
            return Err(ApiError::too_many_ws_connections("global"));
        }
    };
    // Hosted-relay families are provisioned at runtime (they pass
    // `authorize_family` via the `families` table, not the static
    // allowlist), so their semaphore is created lazily on first upgrade
    // rather than looked up from a construction-time map.
    let per_token_semaphore = {
        let mut per_token = state.ws_per_token.lock().unwrap_or_else(|e| e.into_inner());
        per_token
            .entry(token.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(state.ws_per_token_max_connections)))
            .clone()
    };
    let per_token_permit = match per_token_semaphore.try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            warn!(
                family = %token_prefix(&token),
                "ws upgrade rejected: per-token connection cap reached (429)"
            );
            return Err(ApiError::too_many_ws_connections("token"));
        }
    };

    Ok(ws
        .max_message_size(WS_MAX_INBOUND_MESSAGE_BYTES)
        .max_frame_size(WS_MAX_INBOUND_MESSAGE_BYTES)
        .on_upgrade(move |socket| {
            handle_ws(
                socket,
                state,
                token,
                hints,
                hints_base64,
                after,
                global_permit,
                per_token_permit,
            )
        })
        .into_response())
}

fn decode_fetch_hints(value: &str) -> Result<(Vec<Vec<u8>>, HashSet<String>), ApiError> {
    let mut hints = Vec::with_capacity(MAX_FETCH_HINTS.min(16));
    let mut canonical = HashSet::with_capacity(MAX_FETCH_HINTS.min(16));
    let mut submitted = 0usize;
    for value in value.split(',').filter(|hint| !hint.is_empty()) {
        submitted += 1;
        if submitted > MAX_FETCH_HINTS {
            return Err(ApiError::bad_request(format!(
                "hints must contain at most {MAX_FETCH_HINTS} entries"
            )));
        }
        let hint = decode_base64_field(value, "hints")?;
        if hint.len() != RECIPIENT_HINT_LEN {
            return Err(ApiError::bad_request(format!(
                "each hint must be {RECIPIENT_HINT_LEN} bytes after base64url decoding"
            )));
        }
        let encoded = encode_base64_field(&hint);
        if canonical.insert(encoded) {
            hints.push(hint);
        }
    }
    if hints.is_empty() {
        return Err(ApiError::bad_request(
            "at least one hint is required".to_string(),
        ));
    }
    Ok((hints, canonical))
}

async fn ws_send_text(socket: &mut WebSocket, text: String) -> bool {
    matches!(
        tokio::time::timeout(WS_WRITE_TIMEOUT, socket.send(Message::Text(text.into())),).await,
        Ok(Ok(()))
    )
}

/// FR6: server-initiated keepalive ping. Reuses the same write-timeout as
/// every other socket write, so a peer that can't even accept a ping is
/// dropped through the existing write-timeout path, not a bespoke one.
async fn ws_send_ping(socket: &mut WebSocket) -> bool {
    matches!(
        tokio::time::timeout(
            WS_WRITE_TIMEOUT,
            socket.send(Message::Ping(Vec::new().into())),
        )
        .await,
        Ok(Ok(()))
    )
}

async fn handle_ws(
    mut socket: WebSocket,
    state: AppState,
    family_token: String,
    hints: Vec<Vec<u8>>,
    hints_base64: HashSet<String>,
    mut after: i64,
    // FR6: RAII connection-cap permits -- held for the socket's whole
    // lifetime; dropped (and the slot freed) whenever this function
    // returns, on any disconnect path.
    _global_permit: OwnedSemaphorePermit,
    _per_token_permit: OwnedSemaphorePermit,
) {
    // FR2: WS lifecycle logging. `family` is a short, non-secret prefix
    // (see `token_prefix`) so log lines correlate a session across
    // connect/disconnect without printing the bearer token.
    let family = token_prefix(&family_token);
    info!(family = %family, hints = hints.len(), after, "ws connect");

    // Subscribe before replay so POSTs that land during replay are not lost;
    // the live loop skips ids already covered by `after`.
    let mut rx = state.tx.subscribe();

    // --- Replay: same rows GET /envelopes would return ---
    loop {
        // FR8: off the tokio reactor -- a fresh WS connection can replay
        // an arbitrarily large backlog before the live-push loop even
        // starts.
        let replay_family = family_token.clone();
        let replay_hints = hints.clone();
        let replay_now = now_ms();
        let rows = match state
            .store
            .run_blocking(move |store| {
                store.fetch_envelopes(
                    &replay_family,
                    replay_hints,
                    after,
                    DEFAULT_FETCH_LIMIT,
                    replay_now,
                )
            })
            .await
        {
            Ok(r) => r,
            Err(_) => return,
        };
        if rows.is_empty() {
            break;
        }
        for row in rows {
            let env = EnvelopeResponse {
                id: row.id,
                msg_id: encode_base64_field(&row.msg_id),
                hop_ttl: row.hop_ttl,
                recipient_hint: encode_base64_field(&row.recipient_hint),
                sealed: encode_base64_field(&row.sealed),
                expiry_ms: row.expiry_ms,
                created_at_ms: row.created_at_ms,
            };
            after = after.max(env.id);
            let Ok(msg) = serde_json::to_string(&env) else {
                return;
            };
            if !ws_send_text(&mut socket, msg).await {
                info!(family = %family, "ws disconnect: write failed/timed out during replay");
                return;
            }
        }
        // Replay ends on an EMPTY batch (checked above), never on a short
        // one. This used to break out at `rows.len() < DEFAULT_FETCH_LIMIT`,
        // which was sound only while a batch was bounded by row count alone.
        // `fetch_envelopes` now also stops on a cumulative byte budget, so a
        // mailbox of large attachment chunks returns short batches routinely
        // — and treating one as end-of-backlog would silently truncate the
        // replay, stranding exactly the newest mail (an ascending-id mailbox
        // puts it last). The cost of dropping the shortcut is one extra
        // empty query per WebSocket connect. Both mobile shells apply the
        // same rule to HTTP paging (`relay_fetch_walk_continues`).
    }

    // --- Live push ---
    // FR6: server-side keepalive. Without this, a silently-dead phone's
    // socket lingers until the next broadcast happens to hit the write
    // timeout -- for an idle family that can be hours or days. `interval`
    // fires its first tick immediately on creation; consume that tick so a
    // freshly-opened connection isn't pinged the instant it connects.
    let mut ping_timer = tokio::time::interval(state.ws_ping_interval);
    ping_timer.tick().await;
    let mut missed_pings: u32 = 0;

    loop {
        tokio::select! {
            _ = ping_timer.tick() => {
                if missed_pings >= state.ws_ping_missed_limit {
                    warn!(
                        family = %family,
                        missed_pings,
                        "ws disconnect: missed keepalive pings"
                    );
                    break;
                }
                if !ws_send_ping(&mut socket).await {
                    info!(family = %family, "ws disconnect: keepalive ping write failed/timed out");
                    break;
                }
                missed_pings += 1;
            }
            res = rx.recv() => {
                match res {
                    Ok(broadcast) => {
                        if broadcast.family_token == family_token
                            && hints_base64.contains(&broadcast.recipient_hint)
                            && broadcast.envelope.id > after
                        {
                            after = after.max(broadcast.envelope.id);
                            let Ok(msg) = serde_json::to_string(&broadcast.envelope) else {
                                break;
                            };
                            if !ws_send_text(&mut socket, msg).await {
                                info!(family = %family, "ws disconnect: write failed/timed out during live push");
                                break;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        // Bound memory: drop slow/dead consumers; reconnect
                        // + replay from cursor heals (module docs).
                        warn!(family = %family, skipped, "ws lag-drop: consumer fell behind the broadcast buffer");
                        break;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        info!(family = %family, "ws disconnect: broadcast channel closed");
                        break;
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    None | Some(Ok(Message::Close(_))) | Some(Err(_)) => {
                        info!(family = %family, "ws disconnect: client closed");
                        break;
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        // FR6: a client-initiated ping still proves the
                        // peer is alive -- counts toward keepalive too.
                        missed_pings = 0;
                        if tokio::time::timeout(
                            WS_WRITE_TIMEOUT,
                            socket.send(Message::Pong(payload)),
                        )
                        .await
                        .is_err()
                        {
                            break;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        // FR6: keepalive answer -- peer is alive.
                        missed_pings = 0;
                    }
                    // Other client->server traffic ignored (acks are
                    // REST-only) but still counts as liveness.
                    Some(Ok(_)) => {
                        missed_pings = 0;
                    }
                }
            }
        }
    }
}
fn decode_base64_field(value: &str, field: &str) -> Result<Vec<u8>, ApiError> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::bad_request(format!("{field} must be base64url without padding")))
}

fn encode_base64_field(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Short, non-secret prefix of a family token for correlating log lines
/// without printing the full bearer token (semi-public via QR friend cards,
/// but still a credential -- FR2 asks for correlation, not disclosure).
fn token_prefix(token: &str) -> String {
    token.chars().take(6).collect()
}

fn raw_bearer_token(headers: &HeaderMap) -> Result<String, ApiError> {
    let auth = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("missing Authorization header".to_string()))?;
    let token = auth.strip_prefix("Bearer ").ok_or_else(|| {
        ApiError::unauthorized("Authorization must be Bearer <token>".to_string())
    })?;
    Ok(token.to_string())
}

/// What a family request is trying to do; expiry grace treats them
/// differently (see `FAMILY_EXPIRY_GRACE_MS`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FamilyOp {
    /// Store a new envelope (`POST /envelopes`).
    Post,
    /// Fetch/ack/presence/WS — draining the mailbox.
    Read,
}

/// The result of authenticating a family bearer credential.
struct FamilyAccess {
    /// The canonical family (member) token — the key envelopes, presence,
    /// quotas, and WS broadcasts are scoped by. When a *deposit* credential
    /// authorized, this is the member token it was derived from, so a
    /// friend's deposited envelope lands where the family actually fetches.
    token: String,
    /// CP4: which capability class the presented credential carried.
    class: TokenClass,
    /// CP4: the credential as presented — the rate-limit bucket key, so
    /// member and deposit traffic charge separate per-class buckets.
    rate_key: String,
    quota_bytes: u64,
}

/// Resolve a family credential against the static env allowlist first
/// (implicit always-active families, the self-hosted path — zero behavior
/// change), then the static tokens' derived deposit counterparts, then the
/// provisioned `families` table (member or deposit column; status + expiry +
/// per-family quota).
///
/// CP4 enforcement lives HERE, not in handlers: every authenticated route
/// funnels through this function with its `FamilyOp`, and a deposit-class
/// credential authorizes `FamilyOp::Post` only — fetch/ack/presence/WS all
/// pass `FamilyOp::Read` and get a structured 403 `deposit_only` before any
/// handler code runs, so no individual handler can forget the check.
async fn authorize_family(
    state: &AppState,
    token: &str,
    op: FamilyOp,
    now_ms: i64,
) -> Result<FamilyAccess, ApiError> {
    if state.auth_tokens.contains(token) {
        return Ok(FamilyAccess {
            token: token.to_string(),
            class: TokenClass::Member,
            rate_key: token.to_string(),
            quota_bytes: state.family_quota_bytes,
        });
    }
    if let Some(member) = state.static_deposit_tokens.get(token) {
        if op != FamilyOp::Post {
            return Err(ApiError::deposit_only(token));
        }
        return Ok(FamilyAccess {
            token: member.clone(),
            class: TokenClass::Deposit,
            rate_key: token.to_string(),
            quota_bytes: state.family_quota_bytes,
        });
    }
    // FR8: the families lookup is a store read on the request hot path --
    // keep it off the reactor like every other store call. The static
    // allowlist fast paths above stay synchronous and allocation-free.
    let lookup_token = token.to_string();
    let family = state
        .store
        .run_blocking(move |store| store.get_family_by_credential(&lookup_token))
        .await
        .map_err(ApiError::internal)?;
    let Some((family, class)) = family else {
        return Err(ApiError::unauthorized("unknown family token".to_string()));
    };
    // Class boundary first: the op simply does not exist for a deposit
    // credential, regardless of the family's billing state.
    if class == TokenClass::Deposit && op != FamilyOp::Post {
        return Err(ApiError::deposit_only(token));
    }
    if family.status != "active" {
        return Err(ApiError::family_suspended(&family.token));
    }
    if let Some(expires_ms) = family.expires_ms {
        if now_ms > expires_ms.saturating_add(FAMILY_EXPIRY_GRACE_MS) {
            return Err(ApiError::family_expired(&family.token, false));
        }
        if now_ms > expires_ms && op == FamilyOp::Post {
            return Err(ApiError::family_expired(&family.token, true));
        }
    }
    Ok(FamilyAccess {
        quota_bytes: family.quota_bytes.unwrap_or(state.family_quota_bytes),
        token: family.token,
        class,
        rate_key: token.to_string(),
    })
}

async fn authorize_bearer(
    state: &AppState,
    headers: &HeaderMap,
    op: FamilyOp,
) -> Result<FamilyAccess, ApiError> {
    let token = raw_bearer_token(headers)?;
    authorize_family(state, &token, op, now_ms()).await
}

/// Constant-time string comparison for the admin bearer token.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Admin routes answer 404 when no admin token is configured — the
/// self-hosted default neither exposes nor advertises the provisioning API.
fn authorize_admin(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(expected) = state.admin_token.as_deref() else {
        return Err(ApiError::not_found());
    };
    let provided = raw_bearer_token(headers)?;
    if !constant_time_eq(&provided, expected) {
        return Err(ApiError::unauthorized("unknown admin token".to_string()));
    }
    Ok(())
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before unix epoch")
        .as_millis() as i64
}

struct ApiError {
    status: StatusCode,
    message: String,
    /// Stable machine-readable discriminant for the two new D7 rejection
    /// kinds, so a client can distinguish "shrink the envelope" from "the
    /// family mailbox is full" without parsing `message` or relying on
    /// `status` alone (413 vs 507 also differ, but `code` is meant to be
    /// the primary, forward-compatible signal). `None` for pre-existing
    /// error kinds — omitted from the response body, so their wire shape
    /// is unchanged.
    code: Option<&'static str>,
    /// Delta-seconds for a `Retry-After` response header, following the same
    /// convention as `code` above: `Some` only for the rate-limit 429 that
    /// introduced it, `None` (header omitted entirely) for every other error
    /// kind, so their wire shape is likewise unchanged.
    retry_after_secs: Option<u64>,
}

impl ApiError {
    fn bad_request(message: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
            code: None,
            retry_after_secs: None,
        }
    }

    fn unauthorized(message: String) -> Self {
        // FR2: log every auth reject so a field incident (wrong token
        // rolled out, QR card typo'd, family fleet locked out) is visible
        // server-side instead of only as a client-side error toast.
        warn!(reason = %message, "auth reject");
        Self {
            status: StatusCode::UNAUTHORIZED,
            message,
            code: None,
            retry_after_secs: None,
        }
    }

    /// FR2/FR8: log the real error server-side (may contain rusqlite text
    /// or DB paths) and return a generic body -- clients must never see
    /// internal error detail.
    fn internal(detail: String) -> Self {
        tracing::error!(detail = %detail, "internal error");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "internal server error".to_string(),
            code: None,
            retry_after_secs: None,
        }
    }

    /// FR6: WS upgrade admission control -- either the per-token or the
    /// global concurrent-connection cap was already saturated. 429 Too Many
    /// Requests is the standard status for "this resource is temporarily
    /// exhausted, retry later" -- deliberately distinct from the D7 507
    /// (that one means the family's *mailbox storage* is full; this one
    /// means the family/server's *live connection* budget is full).
    fn too_many_ws_connections(scope: &'static str) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!("too many concurrent websocket connections ({scope} cap reached)"),
            code: Some("ws_connection_cap"),
            retry_after_secs: None,
        }
    }

    /// Abuse protection (`DEPLOY.md` §10): a token bucket refused this
    /// request. 429 Too Many Requests is the standard status, and this one
    /// carries a `Retry-After` in delta-seconds so a client backs off by a
    /// known amount instead of hot-looping into the same rejection.
    ///
    /// Deliberately distinct from the two other exhaustion shapes: 507
    /// `family_quota_exceeded` means the family's *stored* bytes are full
    /// (drain the mailbox), 429 `ws_connection_cap` means too many *live
    /// sockets* (close one), and this means too much traffic *per unit
    /// time* (wait). A single `rate_limited` code covers all three scopes —
    /// the client's remedy is identical — while the message names which
    /// limit and which dimension tripped, for the operator reading a
    /// support report.
    fn rate_limited(scope: RateLimitScope, retry_after_secs: u64) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!(
                "{} exceeded: retry after {retry_after_secs}s",
                scope.description()
            ),
            code: Some("rate_limited"),
            retry_after_secs: Some(retry_after_secs),
        }
    }

    fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: "not found".to_string(),
            code: None,
            retry_after_secs: None,
        }
    }

    /// Hosted-relay pass expired. `read_only_grace` distinguishes "you can
    /// still drain the mailbox" (POST rejected during the grace window)
    /// from "everything is locked" (grace window over). Same 403 + `code`
    /// either way so clients need exactly one renewal UX.
    fn family_expired(token: &str, read_only_grace: bool) -> Self {
        warn!(
            family = %token_prefix(token),
            read_only_grace,
            "family reject: pass expired (403 family_expired)"
        );
        let message = if read_only_grace {
            "relay pass expired: fetching queued envelopes still works during \
             the grace window, but new envelopes are rejected until renewal"
        } else {
            "relay pass expired"
        };
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.to_string(),
            code: Some("family_expired"),
            retry_after_secs: None,
        }
    }

    /// CP4: a deposit-class credential attempted anything other than
    /// posting an envelope. 403 (not 401): the credential is real and
    /// recognized — the operation is simply outside its class. The stable
    /// `deposit_only` code lets a client distinguish "you scanned a friend
    /// card into the Cruise Pass slot" from a revoked or mistyped token.
    fn deposit_only(token: &str) -> Self {
        warn!(
            family = %token_prefix(token),
            "family reject: deposit token used for a member-only operation (403 deposit_only)"
        );
        Self {
            status: StatusCode::FORBIDDEN,
            message: "deposit tokens can only post envelopes; fetch, ack, presence, \
                      and websocket access require the family's member token"
                .to_string(),
            code: Some("deposit_only"),
            retry_after_secs: None,
        }
    }

    /// Family administratively suspended (payment dispute, abuse, …).
    fn family_suspended(token: &str) -> Self {
        warn!(
            family = %token_prefix(token),
            "family reject: suspended (403 family_suspended)"
        );
        Self {
            status: StatusCode::FORBIDDEN,
            message: "family is suspended".to_string(),
            code: Some("family_suspended"),
            retry_after_secs: None,
        }
    }

    /// DTN_TODOS.md D7: sealed ciphertext exceeds `MAX_ENVELOPE_SEALED_BYTES`.
    /// 413 Payload Too Large is the standard HTTP status for exactly this.
    fn envelope_too_large(sealed_len: usize) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: format!(
                "sealed envelope of {sealed_len} bytes exceeds the \
                 {MAX_ENVELOPE_SEALED_BYTES}-byte per-envelope cap"
            ),
            code: Some("envelope_too_large"),
            retry_after_secs: None,
        }
    }

    /// DTN_TODOS.md D7: per-family storage quota exceeded even after
    /// pruning expired rows. 507 Insufficient Storage is the standard HTTP
    /// status for "server understood the request but cannot store the
    /// result" — deliberately distinct from 413 (which means "this one
    /// request is malformed") since the client's remedy is different
    /// (wait for space / ack backlog vs. shrink the payload).
    fn family_quota_exceeded(usage_bytes: u64, quota_bytes: u64) -> Self {
        Self {
            status: StatusCode::INSUFFICIENT_STORAGE,
            message: format!(
                "family storage quota exceeded: {usage_bytes} bytes used, \
                 {quota_bytes} byte quota (expired rows already pruned)"
            ),
            code: Some("family_quota_exceeded"),
            retry_after_secs: None,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = match self.code {
            Some(code) => serde_json::json!({ "error": self.message, "code": code }),
            None => serde_json::json!({ "error": self.message }),
        };
        let mut response = (self.status, Json(body)).into_response();
        // `Retry-After` in delta-seconds (RFC 9110 §10.2.3). Set only by the
        // rate-limit 429; every other error omits the header entirely and
        // keeps its exact pre-existing wire shape. The value is a plain
        // integer, so `from_str` cannot realistically fail -- if it somehow
        // did, the body still carries the wait in its message.
        if let Some(secs) = self.retry_after_secs {
            if let Ok(value) = HeaderValue::from_str(&secs.to_string()) {
                response.headers_mut().insert(RETRY_AFTER, value);
            }
        }
        response
    }
}

const SCHEMA: &str = "
PRAGMA auto_vacuum = INCREMENTAL;
CREATE TABLE IF NOT EXISTS envelopes (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    family_token   TEXT NOT NULL,
    msg_id         BLOB NOT NULL,
    hop_ttl        INTEGER NOT NULL,
    recipient_hint BLOB NOT NULL,
    sealed         BLOB NOT NULL,
    expiry_ms      INTEGER NOT NULL,
    created_at_ms  INTEGER NOT NULL,
    UNIQUE(family_token, msg_id)
);
CREATE INDEX IF NOT EXISTS idx_envelopes_family_hint_id
    ON envelopes(family_token, recipient_hint, id);
CREATE INDEX IF NOT EXISTS idx_envelopes_expiry ON envelopes(expiry_ms);
CREATE INDEX IF NOT EXISTS idx_envelopes_created_at ON envelopes(created_at_ms);
CREATE TABLE IF NOT EXISTS presence (
    family_token  TEXT NOT NULL,
    hint          BLOB NOT NULL,
    last_seen_ms  INTEGER NOT NULL,
    PRIMARY KEY(family_token, hint)
);
CREATE INDEX IF NOT EXISTS idx_presence_last_seen ON presence(last_seen_ms);
CREATE TABLE IF NOT EXISTS families (
    token         TEXT PRIMARY KEY,
    status        TEXT NOT NULL DEFAULT 'active',
    plan          TEXT,
    quota_bytes   INTEGER,
    created_ms    INTEGER NOT NULL,
    expires_ms    INTEGER,
    note          TEXT,
    deposit_token TEXT
);
";

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tempfile::NamedTempFile;
    use tower::util::ServiceExt;

    fn sample_hint(byte: u8) -> Vec<u8> {
        vec![byte; RECIPIENT_HINT_LEN]
    }

    fn sample_msg_id(byte: u8) -> Vec<u8> {
        vec![byte; MSG_ID_LEN]
    }

    fn sample_sealed(byte: u8) -> Vec<u8> {
        vec![byte; 48]
    }

    fn test_store() -> (NamedTempFile, RelayStore) {
        let db = NamedTempFile::new().unwrap();
        let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
        (db, store)
    }

    fn test_app() -> Router {
        // In-memory DB: the router owns the store's single connection, and a
        // NamedTempFile guard dropped here would unlink the file and turn
        // every write into SQLITE_READONLY_DBMOVED.
        let store = RelayStore::open(":memory:").unwrap();
        app(AppState::new(
            store,
            HashSet::from(["family-a".to_string(), "family-b".to_string()]),
        ))
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    const ADMIN_TOKEN: &str = "admin-secret";

    /// Router with the admin API enabled plus one static env-style token
    /// ("family-a") so static/provisioned interplay is covered.
    fn admin_app() -> Router {
        let store = RelayStore::open(":memory:").unwrap();
        app(
            AppState::new(store, HashSet::from(["family-a".to_string()]))
                .with_admin_token(Some(ADMIN_TOKEN.to_string())),
        )
    }

    fn admin_json(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn admin_bare(method: &str, uri: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
            .body(Body::empty())
            .unwrap()
    }

    fn envelope_request(token: &str, msg_byte: u8, sealed_len: usize) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/envelopes")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "msg_id": encode_base64_field(&sample_msg_id(msg_byte)),
                    "hop_ttl": 3,
                    "recipient_hint": encode_base64_field(&sample_hint(1)),
                    "sealed": encode_base64_field(&vec![7u8; sealed_len]),
                    "expiry_ms": now_ms() + 60_000,
                })
                .to_string(),
            ))
            .unwrap()
    }

    fn fetch_request(token: &str) -> Request<Body> {
        Request::builder()
            .uri(format!(
                "/envelopes?hints={}",
                encode_base64_field(&sample_hint(1))
            ))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }

    fn ack_request(token: &str, ids: &[i64]) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/envelopes/ack")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(serde_json::json!({ "ids": ids }).to_string()))
            .unwrap()
    }

    fn presence_request(token: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/presence")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "announce": [encode_base64_field(&sample_hint(1))],
                    "query": [encode_base64_field(&sample_hint(1))],
                })
                .to_string(),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn admin_routes_hidden_without_admin_token() {
        let app = test_app();
        let request = Request::builder()
            .method("POST")
            .uri("/admin/families")
            .header("authorization", "Bearer anything")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"token": "fam-pass"}).to_string(),
            ))
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn admin_rejects_wrong_and_family_bearer_tokens() {
        let app = admin_app();
        for bearer in ["wrong", "family-a"] {
            let request = Request::builder()
                .method("POST")
                .uri("/admin/families")
                .header("authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"token": "fam-pass"}).to_string(),
                ))
                .unwrap();
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::UNAUTHORIZED,
                "bearer {bearer:?} must not reach the admin API"
            );
        }
    }

    #[tokio::test]
    async fn admin_provision_validation() {
        let app = admin_app();
        for (body, why) in [
            (serde_json::json!({"token": ""}), "empty token"),
            (serde_json::json!({"token": "has space"}), "whitespace"),
            (serde_json::json!({"token": ADMIN_TOKEN}), "admin collision"),
            (serde_json::json!({"token": "family-a"}), "static collision"),
            (
                serde_json::json!({"token": "fam-pass", "quota_bytes": 0}),
                "zero quota",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(admin_json("POST", "/admin/families", body))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{why}");
        }
    }

    #[tokio::test]
    async fn provisioned_family_lifecycle() {
        let app = admin_app();

        // Unknown before provisioning.
        assert_eq!(
            app.clone()
                .oneshot(envelope_request("fam-pass", 1, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );

        // Provision → posting works; the admin token itself is not a family.
        let response = app
            .clone()
            .oneshot(admin_json(
                "POST",
                "/admin/families",
                serde_json::json!({"token": "fam-pass", "plan": "cruise-pass-30d"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let family = body_json(response).await;
        assert_eq!(family["status"], "active");
        assert_eq!(family["plan"], "cruise-pass-30d");
        assert_eq!(
            app.clone()
                .oneshot(envelope_request("fam-pass", 1, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.clone()
                .oneshot(envelope_request(ADMIN_TOKEN, 2, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );

        // Usage shows up on GET.
        let response = app
            .clone()
            .oneshot(admin_bare("GET", "/admin/families/fam-pass"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let family = body_json(response).await;
        assert_eq!(family["usage_bytes"], 48);
        assert_eq!(family["envelope_count"], 1);

        // Suspend → both post and fetch are 403 with a stable code.
        let response = app
            .clone()
            .oneshot(admin_json(
                "PATCH",
                "/admin/families/fam-pass",
                serde_json::json!({"status": "suspended"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(envelope_request("fam-pass", 3, 48))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "family_suspended");
        assert_eq!(
            app.clone()
                .oneshot(fetch_request("fam-pass"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );

        // Reactivate → works again.
        let response = app
            .clone()
            .oneshot(admin_json(
                "PATCH",
                "/admin/families/fam-pass",
                serde_json::json!({"status": "active"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            app.clone()
                .oneshot(envelope_request("fam-pass", 4, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // Revoke → token dead, data purged, admin GET is 404.
        let response = app
            .clone()
            .oneshot(admin_bare("DELETE", "/admin/families/fam-pass"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            app.clone()
                .oneshot(envelope_request("fam-pass", 5, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            app.clone()
                .oneshot(admin_bare("GET", "/admin/families/fam-pass"))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            app.clone()
                .oneshot(admin_bare("DELETE", "/admin/families/fam-pass"))
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );

        // Re-provisioning the same token starts from an empty, purged mailbox.
        let response = app
            .clone()
            .oneshot(admin_json(
                "POST",
                "/admin/families",
                serde_json::json!({"token": "fam-pass"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app.oneshot(fetch_request("fam-pass")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_json(response).await["envelopes"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    // -----------------------------------------------------------------
    // CP4 — deposit-token split

    /// Golden vectors shared verbatim with
    /// `core/src/relay_wire.rs::deposit_token_derivation_matches_golden_vector`,
    /// plus a live parity check against the core implementation phones
    /// actually run: if either derivation drifts, this fails.
    #[test]
    fn deposit_derivation_matches_core_golden_vector() {
        assert_eq!(
            deposit_token_for("abc123"),
            "cmdep1-0uq69OqNyMo1Dd3vQcspqLlRY6bCCjTWvPyehXd6Ezs"
        );
        assert_eq!(
            deposit_token_for("family-token"),
            "cmdep1-63hWvx1kHLKirfl9GV576eAi_rURpyZixpsCVUCXNJk"
        );
        for token in ["family-a", "0123456789abcdef0123456789abcdef", "x"] {
            assert_eq!(
                deposit_token_for(token),
                cruisemesh_core::relay_deposit_token_for(token.to_string()),
                "relayd and core derivations must agree for {token:?}"
            );
        }
        // Idempotent + classifiable.
        let deposit = deposit_token_for("family-a");
        assert_eq!(deposit_token_for(&deposit), deposit);
        assert!(is_deposit_token(&deposit));
        assert!(!is_deposit_token("family-a"));
    }

    #[tokio::test]
    async fn deposit_token_posts_into_the_family_mailbox_but_never_reads() {
        let app = admin_app();
        let response = app
            .clone()
            .oneshot(admin_json(
                "POST",
                "/admin/families",
                serde_json::json!({"token": "fam-pass"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let family = body_json(response).await;
        let deposit = family["deposit_token"].as_str().unwrap().to_string();
        assert_eq!(deposit, deposit_token_for("fam-pass"));

        // Deposit can post...
        assert_eq!(
            app.clone()
                .oneshot(envelope_request(&deposit, 1, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        // ...and the row lands in the family's one mailbox, keyed by the
        // member token, where the family actually fetches.
        let response = app
            .clone()
            .oneshot(fetch_request("fam-pass"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page = body_json(response).await;
        assert_eq!(page["envelopes"].as_array().unwrap().len(), 1);
        let relay_id = page["envelopes"][0]["id"].as_i64().unwrap();

        // Every read-class operation is a structured 403 for the deposit
        // token: fetch, ack, presence. (WS is covered in e2e_ws.rs — the
        // upgrade needs a real socket.)
        for (request, what) in [
            (fetch_request(&deposit), "fetch"),
            (ack_request(&deposit, &[relay_id]), "ack"),
            (presence_request(&deposit), "presence"),
        ] {
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{what}");
            assert_eq!(body_json(response).await["code"], "deposit_only", "{what}");
        }

        // The failed deposit ack must not have deleted anything: the member
        // still sees the row, and member-class operations are unchanged.
        let response = app
            .clone()
            .oneshot(fetch_request("fam-pass"))
            .await
            .unwrap();
        let page = body_json(response).await;
        assert_eq!(page["envelopes"].as_array().unwrap().len(), 1);
        let response = app
            .clone()
            .oneshot(ack_request("fam-pass", &[relay_id]))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["deleted"], 1);
        assert_eq!(
            app.clone()
                .oneshot(presence_request("fam-pass"))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // Suspension and expiry still bind the deposit token (it is the
        // same family): suspended → family_suspended, expired → no posting
        // even inside the fetch-grace window.
        let response = app
            .clone()
            .oneshot(admin_json(
                "PATCH",
                "/admin/families/fam-pass",
                serde_json::json!({"status": "suspended"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(envelope_request(&deposit, 2, 48))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "family_suspended");
        let response = app
            .clone()
            .oneshot(admin_json(
                "PATCH",
                "/admin/families/fam-pass",
                serde_json::json!({"status": "active", "expires_ms": now_ms() - 1_000}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(envelope_request(&deposit, 3, 48))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "family_expired");
    }

    #[tokio::test]
    async fn static_family_deposit_token_behaves_like_a_provisioned_one() {
        // Static env-allowlist families have no table row; their deposit
        // counterparts come from the precomputed map in AppState.
        let app = test_app();
        let deposit = deposit_token_for("family-a");

        assert_eq!(
            app.clone()
                .oneshot(envelope_request(&deposit, 1, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let response = app
            .clone()
            .oneshot(fetch_request("family-a"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_json(response).await["envelopes"]
                .as_array()
                .unwrap()
                .len(),
            1,
            "a static family's deposit post must land in the member mailbox"
        );
        let response = app.clone().oneshot(fetch_request(&deposit)).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "deposit_only");
        // Another family's deposit token must not cross mailboxes.
        let response = app
            .clone()
            .oneshot(fetch_request(&deposit_token_for("family-b")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn migration_backfills_deposit_tokens_and_keeps_existing_tokens_member() {
        let db = NamedTempFile::new().unwrap();
        let path = db.path().to_str().unwrap().to_string();
        {
            // A database exactly as a pre-CP4 relayd left it: families table
            // without the deposit_token column, one provisioned row.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE families (
                    token        TEXT PRIMARY KEY,
                    status       TEXT NOT NULL DEFAULT 'active',
                    plan         TEXT,
                    quota_bytes  INTEGER,
                    created_ms   INTEGER NOT NULL,
                    expires_ms   INTEGER,
                    note         TEXT
                );
                INSERT INTO families (token, status, created_ms)
                    VALUES ('fam-old', 'active', 123);",
            )
            .unwrap();
        }

        let store = RelayStore::open(&path).unwrap();
        let row = store.get_family("fam-old").unwrap().unwrap();
        assert_eq!(row.deposit_token, deposit_token_for("fam-old"));

        // Migration default is member: the pre-existing token resolves to
        // member class (zero behavior change), the derived one to deposit.
        let (_, class) = store.get_family_by_credential("fam-old").unwrap().unwrap();
        assert_eq!(class, TokenClass::Member);
        let (row, class) = store
            .get_family_by_credential(&deposit_token_for("fam-old"))
            .unwrap()
            .unwrap();
        assert_eq!(class, TokenClass::Deposit);
        assert_eq!(row.token, "fam-old");

        // Reopening is a no-op (idempotent migration).
        drop(store);
        let store = RelayStore::open(&path).unwrap();
        assert_eq!(
            store.get_family("fam-old").unwrap().unwrap().deposit_token,
            deposit_token_for("fam-old")
        );
    }

    #[tokio::test]
    async fn admin_returns_both_tokens_and_rejects_deposit_prefixed_member_tokens() {
        let app = admin_app();
        provision(&app, "fam-pass", serde_json::json!({})).await;
        let expected_deposit = deposit_token_for("fam-pass");

        let response = app
            .clone()
            .oneshot(admin_bare("GET", "/admin/families/fam-pass"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["deposit_token"], expected_deposit);

        let page = list_families(&app, "").await;
        assert_eq!(page["families"][0]["deposit_token"], expected_deposit);

        // A member token wearing the deposit prefix would make credential
        // classification ambiguous — rejected at provisioning.
        let response = app
            .clone()
            .oneshot(admin_json(
                "POST",
                "/admin/families",
                serde_json::json!({"token": "cmdep1-lookslikeadeposit"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        // Admin routes are keyed by the member token; the deposit token is
        // a mailbox credential, not an admin handle.
        let response = app
            .clone()
            .oneshot(admin_bare(
                "GET",
                &format!("/admin/families/{expected_deposit}"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    /// Provision `token` on `app`, asserting it took.
    async fn provision(app: &Router, token: &str, extra: serde_json::Value) {
        let mut body = serde_json::json!({ "token": token });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        let response = app
            .clone()
            .oneshot(admin_json("POST", "/admin/families", body))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "provisioning {token}");
    }

    async fn list_families(app: &Router, query: &str) -> serde_json::Value {
        let response = app
            .clone()
            .oneshot(admin_bare("GET", &format!("/admin/families{query}")))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "GET /admin/families{query}"
        );
        body_json(response).await
    }

    #[tokio::test]
    async fn admin_list_families_reports_usage_and_pages() {
        let app = admin_app();
        provision(
            &app,
            "fam-a",
            serde_json::json!({"plan": "cruise-pass-30d"}),
        )
        .await;
        provision(&app, "fam-b", serde_json::json!({})).await;
        provision(&app, "fam-c", serde_json::json!({})).await;
        for msg_byte in [1u8, 2] {
            assert_eq!(
                app.clone()
                    .oneshot(envelope_request("fam-a", msg_byte, 48))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }

        let page = list_families(&app, "").await;
        assert_eq!(page["total"], 3);
        assert_eq!(page["limit"], DEFAULT_FAMILY_LIST_LIMIT);
        assert_eq!(page["offset"], 0);
        let families = page["families"].as_array().unwrap();
        // The static env token ("family-a") is an implicit family, not a row
        // in the table — it must not show up here.
        assert_eq!(
            families
                .iter()
                .map(|f| f["token"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["fam-a", "fam-b", "fam-c"]
        );
        assert_eq!(families[0]["usage_bytes"], 96);
        assert_eq!(families[0]["envelope_count"], 2);
        assert_eq!(families[0]["plan"], "cruise-pass-30d");
        assert_eq!(families[1]["usage_bytes"], 0);
        assert_eq!(families[1]["envelope_count"], 0);

        // The aggregate in list_families must not drift from the per-row
        // helpers behind GET /admin/families/{token}.
        let response = app
            .clone()
            .oneshot(admin_bare("GET", "/admin/families/fam-a"))
            .await
            .unwrap();
        assert_eq!(families[0], body_json(response).await);

        let page = list_families(&app, "?limit=2").await;
        assert_eq!(page["total"], 3, "total counts matches, not the page");
        assert_eq!(page["families"].as_array().unwrap().len(), 2);
        let page = list_families(&app, "?limit=2&offset=2").await;
        assert_eq!(page["families"][0]["token"], "fam-c");
        assert_eq!(page["families"].as_array().unwrap().len(), 1);
        let page = list_families(&app, "?offset=99").await;
        assert_eq!(page["total"], 3);
        assert!(page["families"].as_array().unwrap().is_empty());

        // Over-large limits clamp instead of failing; total still tells the
        // caller how much is really there.
        let page = list_families(&app, "?limit=100000").await;
        assert_eq!(page["limit"], MAX_FAMILY_LIST_LIMIT);
        assert_eq!(page["families"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn admin_list_families_filters_by_status() {
        let app = admin_app();
        provision(&app, "fam-a", serde_json::json!({})).await;
        provision(&app, "fam-b", serde_json::json!({})).await;
        let response = app
            .clone()
            .oneshot(admin_json(
                "PATCH",
                "/admin/families/fam-b",
                serde_json::json!({"status": "suspended"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let page = list_families(&app, "?status=active").await;
        assert_eq!(page["total"], 1);
        assert_eq!(page["families"][0]["token"], "fam-a");
        let page = list_families(&app, "?status=suspended").await;
        assert_eq!(page["total"], 1);
        assert_eq!(page["families"][0]["token"], "fam-b");
        assert_eq!(list_families(&app, "").await["total"], 2);
    }

    #[tokio::test]
    async fn admin_list_families_rejects_bad_query() {
        let app = admin_app();
        for query in ["?status=deleted", "?limit=abc", "?limit=-1", "?offset=x"] {
            let response = app
                .clone()
                .oneshot(admin_bare("GET", &format!("/admin/families{query}")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        }
    }

    #[tokio::test]
    async fn admin_list_hidden_without_admin_token_even_when_malformed() {
        // A malformed query must not out the route's existence on a deploy
        // with the admin API off: authorize_admin has to run first, so every
        // list query param is parsed by hand rather than by the extractor.
        let app = test_app();
        for query in ["", "?limit=abc", "?status=deleted"] {
            let request = Request::builder()
                .uri(format!("/admin/families{query}"))
                .header("authorization", "Bearer anything")
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::NOT_FOUND,
                "{query}"
            );
        }
        // Same for a wrong admin token on a deploy that does have one: 401,
        // never a 400 that confirms the query shape.
        let app = admin_app();
        let request = Request::builder()
            .uri("/admin/families?limit=abc")
            .header("authorization", "Bearer wrong")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn expired_family_is_read_only_then_locked() {
        let app = admin_app();
        let now = now_ms();

        // Expired 1s ago: inside the grace window → read-only.
        let response = app
            .clone()
            .oneshot(admin_json(
                "POST",
                "/admin/families",
                serde_json::json!({"token": "fam-pass", "expires_ms": now - 1_000}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app
            .clone()
            .oneshot(envelope_request("fam-pass", 1, 48))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "family_expired");
        assert_eq!(
            app.clone()
                .oneshot(fetch_request("fam-pass"))
                .await
                .unwrap()
                .status(),
            StatusCode::OK,
            "grace window must keep the mailbox drainable"
        );

        // Past the grace window → everything is locked.
        let response = app
            .clone()
            .oneshot(admin_json(
                "PATCH",
                "/admin/families/fam-pass",
                serde_json::json!({"expires_ms": now - FAMILY_EXPIRY_GRACE_MS - 10_000}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = app.oneshot(fetch_request("fam-pass")).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "family_expired");
    }

    #[tokio::test]
    async fn renewal_extends_an_expired_family() {
        let app = admin_app();
        let now = now_ms();
        for _ in 0..2 {
            // Provisioning twice (webhook retry) must be a clean no-op.
            let response = app
                .clone()
                .oneshot(admin_json(
                    "POST",
                    "/admin/families",
                    serde_json::json!({"token": "fam-pass", "expires_ms": now - 1_000}),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert_eq!(
            app.clone()
                .oneshot(envelope_request("fam-pass", 1, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        // Renewal = PATCH the expiry forward.
        let response = app
            .clone()
            .oneshot(admin_json(
                "PATCH",
                "/admin/families/fam-pass",
                serde_json::json!({"expires_ms": now + 60_000}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            app.oneshot(envelope_request("fam-pass", 1, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn per_family_quota_override_is_enforced() {
        let app = admin_app();
        let response = app
            .clone()
            .oneshot(admin_json(
                "POST",
                "/admin/families",
                serde_json::json!({"token": "fam-small", "quota_bytes": 100}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        assert_eq!(
            app.clone()
                .oneshot(envelope_request("fam-small", 1, 60))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        let response = app
            .clone()
            .oneshot(envelope_request("fam-small", 2, 60))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INSUFFICIENT_STORAGE);
        assert_eq!(body_json(response).await["code"], "family_quota_exceeded");

        // The static env family still gets the (default) server quota.
        assert_eq!(
            app.oneshot(envelope_request("family-a", 1, 60 + 60))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    #[tokio::test]
    async fn fetch_and_ack_cardinality_caps_fail_before_dynamic_sql() {
        let app = test_app();
        let hint = encode_base64_field(&sample_hint(1));
        let hints = std::iter::repeat(hint)
            .take(MAX_FETCH_HINTS + 1)
            .collect::<Vec<_>>()
            .join(",");
        let fetch = Request::builder()
            .uri(format!("/envelopes?hints={hints}"))
            .header("authorization", "Bearer family-a")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone().oneshot(fetch).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );

        let ack = Request::builder()
            .method("POST")
            .uri("/envelopes/ack")
            .header("authorization", "Bearer family-a")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"ids": (1..=MAX_ACK_IDS + 1).collect::<Vec<_>>()}).to_string(),
            ))
            .unwrap();
        assert_eq!(
            app.oneshot(ack).await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn store_cardinality_caps_are_defense_in_depth() {
        let (_db, store) = test_store();
        assert!(store
            .fetch_envelopes(
                "family-a",
                vec![sample_hint(1); MAX_FETCH_HINTS + 1],
                0,
                1,
                1_000,
            )
            .is_err());
        assert!(store
            .ack_envelopes("family-a", vec![1; MAX_ACK_IDS + 1])
            .is_err());
    }

    #[test]
    fn concurrent_quota_admission_cannot_overcommit_a_family() {
        let (_db, store) = test_store();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let handles = (1..=2u8)
            .map(|byte| {
                let store = store.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .insert_envelope_with_quota(
                            "family-a",
                            sample_msg_id(byte),
                            7,
                            sample_hint(1),
                            vec![byte; 60],
                            2_000,
                            1_000,
                            100,
                        )
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        let outcomes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, QuotaInsertResult::Stored { .. }))
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, QuotaInsertResult::QuotaExceeded { .. }))
                .count(),
            1
        );
        assert_eq!(store.family_sealed_bytes("family-a").unwrap(), 60);
    }

    #[tokio::test]
    async fn post_requires_a_valid_bearer_token() {
        let request = Request::builder()
            .method("POST")
            .uri("/envelopes")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "msg_id": encode_base64_field(&sample_msg_id(1)),
                    "hop_ttl": 7,
                    "recipient_hint": encode_base64_field(&sample_hint(1)),
                    "sealed": encode_base64_field(&sample_sealed(2)),
                    "expiry_ms": now_ms() + 60_000,
                })
                .to_string(),
            ))
            .unwrap();

        let response = test_app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn post_then_get_filters_by_family_hint_and_cursor() {
        let app = test_app();
        let hint_a = encode_base64_field(&sample_hint(1));
        let hint_b = encode_base64_field(&sample_hint(2));

        for (family, hint, msg_byte, sealed_byte) in [
            ("family-a", &hint_a, 21u8, 9u8),
            ("family-a", &hint_b, 22u8, 10u8),
            ("family-b", &hint_a, 23u8, 11u8),
        ] {
            let request = Request::builder()
                .method("POST")
                .uri("/envelopes")
                .header("authorization", format!("Bearer {family}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "msg_id": encode_base64_field(&sample_msg_id(msg_byte)),
                        "hop_ttl": 7,
                        "recipient_hint": hint,
                        "sealed": encode_base64_field(&sample_sealed(sealed_byte)),
                        "expiry_ms": now_ms() + 60_000,
                    })
                    .to_string(),
                ))
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        let get_request = Request::builder()
            .uri(format!("/envelopes?hints={hint_a}&after=0&limit=10"))
            .header("authorization", "Bearer family-a")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(get_request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        let envelopes = json["envelopes"].as_array().unwrap();
        assert_eq!(envelopes.len(), 1);
        assert_eq!(
            envelopes[0]["msg_id"],
            encode_base64_field(&sample_msg_id(21))
        );
        assert_eq!(envelopes[0]["hop_ttl"].as_u64().unwrap(), 7);
        assert_eq!(envelopes[0]["recipient_hint"], hint_a);
        assert_eq!(
            envelopes[0]["sealed"],
            encode_base64_field(&sample_sealed(9))
        );
        assert!(json["next_cursor"].as_i64().unwrap() > 0);
    }

    #[tokio::test]
    async fn ack_only_deletes_the_callers_family_rows() {
        let app = test_app();
        let hint = encode_base64_field(&sample_hint(1));

        let post = |family: &str, msg_byte: u8, sealed_byte: u8| {
            Request::builder()
                .method("POST")
                .uri("/envelopes")
                .header("authorization", format!("Bearer {family}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "msg_id": encode_base64_field(&sample_msg_id(msg_byte)),
                        "hop_ttl": 7,
                        "recipient_hint": hint,
                        "sealed": encode_base64_field(&sample_sealed(sealed_byte)),
                        "expiry_ms": now_ms() + 60_000,
                    })
                    .to_string(),
                ))
                .unwrap()
        };

        let first = body_json(app.clone().oneshot(post("family-a", 1, 1)).await.unwrap()).await;
        let second = body_json(app.clone().oneshot(post("family-b", 2, 2)).await.unwrap()).await;
        let first_id = first["id"].as_i64().unwrap();
        let second_id = second["id"].as_i64().unwrap();

        let ack = Request::builder()
            .method("POST")
            .uri("/envelopes/ack")
            .header("authorization", "Bearer family-a")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({ "ids": [first_id, second_id] }).to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(ack).await.unwrap();
        let json = body_json(response).await;
        assert_eq!(json["deleted"].as_u64().unwrap(), 1);

        let fetch_family_b = Request::builder()
            .uri(format!("/envelopes?hints={hint}"))
            .header("authorization", "Bearer family-b")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(fetch_family_b).await.unwrap();
        let json = body_json(response).await;
        assert_eq!(json["envelopes"].as_array().unwrap().len(), 1);
        assert_eq!(json["envelopes"][0]["id"].as_i64().unwrap(), second_id);
    }

    /// FR7: fetch is now purely a `SELECT` -- expired rows are filtered
    /// out of the response (`expiry_ms > now` predicate) but are no longer
    /// physically deleted by the act of fetching. Physical deletion is the
    /// background maintenance task's job (`spawn_prune_task_reaps_...`
    /// below); `count_for_family` here must therefore stay at 2, not drop
    /// to 1 the way it did before FR7.
    #[tokio::test]
    async fn expired_rows_are_filtered_from_fetch_without_a_write() {
        let (_db, store) = test_store();
        let app = app(AppState::new(
            store.clone(),
            HashSet::from(["family-a".to_string()]),
        ));
        let now = now_ms();
        store
            .insert_envelope(
                "family-a",
                sample_msg_id(1),
                7,
                sample_hint(1),
                sample_sealed(2),
                now - 1,
                now - 5_000,
            )
            .unwrap();
        store
            .insert_envelope(
                "family-a",
                sample_msg_id(2),
                7,
                sample_hint(1),
                sample_sealed(3),
                now + 60_000,
                now,
            )
            .unwrap();

        let request = Request::builder()
            .uri(format!(
                "/envelopes?hints={}",
                encode_base64_field(&sample_hint(1))
            ))
            .header("authorization", "Bearer family-a")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let json = body_json(response).await;
        assert_eq!(json["envelopes"].as_array().unwrap().len(), 1);
        // Both rows are still on disk -- fetch did not delete the expired
        // one. It's still filtered out of the *response* above.
        assert_eq!(store.count_for_family("family-a").unwrap(), 2);
    }

    #[tokio::test]
    async fn spawn_prune_task_reaps_expired_rows_with_no_client_fetch() {
        let (_db, store) = test_store();
        let now = now_ms();
        store
            .insert_envelope(
                "family-a",
                sample_msg_id(1),
                7,
                sample_hint(1),
                sample_sealed(2),
                now - 1,
                now - 5_000,
            )
            .unwrap();
        assert_eq!(store.count_for_family("family-a").unwrap(), 1);

        // Short interval so the test doesn't wait on the hour-scale
        // production default (DEFAULT_PRUNE_INTERVAL).
        let handle = spawn_prune_task(store.clone(), Duration::from_millis(20));

        // No client ever calls GET /envelopes or POST /presence here --
        // the row must disappear purely from the background task ticking.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if store.count_for_family("family-a").unwrap() == 0 {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "background prune task did not reap the expired row in time"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        handle.abort();
    }

    #[tokio::test]
    async fn duplicate_post_by_msg_id_reuses_the_same_row() {
        let (_db, store) = test_store();
        let first_id = store
            .insert_envelope(
                "family-a",
                sample_msg_id(9),
                4,
                sample_hint(1),
                sample_sealed(2),
                5_000,
                1_000,
            )
            .unwrap();
        let second_id = store
            .insert_envelope(
                "family-a",
                sample_msg_id(9),
                7,
                sample_hint(1),
                sample_sealed(99), // different sealed — must not rewrite
                9_000,
                2_000,
            )
            .unwrap();

        assert_eq!(first_id, second_id);
        assert_eq!(store.count_for_family("family-a").unwrap(), 1);

        let rows = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], 0, 10, 2_000)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hop_ttl, 7);
        assert_eq!(rows[0].expiry_ms, 9_000);
        // Sealed stays from the first insert (idempotent re-upload).
        assert_eq!(rows[0].sealed, sample_sealed(2));
    }

    /// Fetch is not destructive: without an ack, the same rows reappear.
    /// Receipt clients that crash mid-sync rely on this.
    #[test]
    fn fetch_without_ack_is_idempotent() {
        let (_db, store) = test_store();
        let now = 1_700_000_000_000i64;
        store
            .insert_envelope(
                "family-a",
                sample_msg_id(1),
                7,
                sample_hint(1),
                sample_sealed(1),
                now + 60_000,
                now,
            )
            .unwrap();
        store
            .insert_envelope(
                "family-a",
                sample_msg_id(2),
                7,
                sample_hint(1),
                sample_sealed(2),
                now + 60_000,
                now,
            )
            .unwrap();

        let first = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], 0, 10, now)
            .unwrap();
        assert_eq!(first.len(), 2);
        let second = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], 0, 10, now)
            .unwrap();
        assert_eq!(second, first);
        assert_eq!(store.count_for_family("family-a").unwrap(), 2);

        // Partial ack leaves the other row for a re-fetch from after=0.
        let deleted = store.ack_envelopes("family-a", vec![first[0].id]).unwrap();
        assert_eq!(deleted, 1);
        let remaining = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], 0, 10, now)
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].id, first[1].id);
    }

    #[test]
    fn insert_clamps_expiry_to_thirty_day_retention() {
        let created = 1_700_000_000_000i64;
        let far_future = created + MAX_RETENTION_MS + 86_400_000;
        assert_eq!(
            RelayStore::effective_expiry(created, far_future),
            created + MAX_RETENTION_MS
        );
        // Tighter client expiry (e.g. 7-day envelope TTL) is preserved.
        let seven_days = created + 7 * 24 * 60 * 60 * 1000;
        assert_eq!(
            RelayStore::effective_expiry(created, seven_days),
            seven_days
        );
    }

    #[test]
    fn prune_honors_per_envelope_expiry_and_thirty_day_retention() {
        let (_db, store) = test_store();
        let now = 1_700_000_000_000i64;

        // Per-envelope expiry already past.
        store
            .insert_envelope(
                "family-a",
                sample_msg_id(1),
                7,
                sample_hint(1),
                sample_sealed(1),
                now - 1,
                now - 60_000,
            )
            .unwrap();

        // Still within client expiry, but created_at is past 30-day retention.
        // Insert with a short client expiry first would get clamped; instead
        // poke a row whose created_at is ancient relative to `now` by using
        // an insert time of now - MAX_RETENTION - 1 and a far expiry that
        // gets clamped to created + 30d, which is still < now.
        let ancient = now - MAX_RETENTION_MS - 1;
        store
            .insert_envelope(
                "family-a",
                sample_msg_id(2),
                7,
                sample_hint(1),
                sample_sealed(2),
                ancient + MAX_RETENTION_MS + 86_400_000, // clamped to ancient+30d < now
                ancient,
            )
            .unwrap();

        // Live row: created now, expires later.
        store
            .insert_envelope(
                "family-a",
                sample_msg_id(3),
                7,
                sample_hint(1),
                sample_sealed(3),
                now + 60_000,
                now,
            )
            .unwrap();

        assert_eq!(store.count_for_family("family-a").unwrap(), 3);
        let pruned = store.prune_expired(now).unwrap();
        assert_eq!(pruned, 2);
        assert_eq!(store.count_for_family("family-a").unwrap(), 1);

        let live = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], 0, 10, now)
            .unwrap();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].msg_id, sample_msg_id(3));
    }

    #[test]
    fn fetch_query_plan_uses_family_hint_index() {
        let (_db, store) = test_store();
        let now = 1_700_000_000_000i64;
        // Seed enough rows that a bad plan would matter; SQLite still
        // reports the index choice for small tables once the index exists.
        for i in 0..200u16 {
            let mut msg_id = sample_msg_id((i % 250) as u8);
            msg_id[0] = (i >> 8) as u8;
            msg_id[1] = (i & 0xff) as u8;
            let hint = sample_hint(if i % 2 == 0 { 1 } else { 2 });
            store
                .insert_envelope(
                    "family-a",
                    msg_id,
                    7,
                    hint,
                    sample_sealed(3),
                    now + 60_000,
                    now,
                )
                .unwrap();
        }

        let plan = store
            .explain_fetch_plan("family-a", &[sample_hint(1)], 0, 50, now)
            .unwrap();
        // Accept either a direct SEARCH on the composite index or a cover
        // that names it — reject a plain SCAN of envelopes with no index.
        assert!(
            plan.contains("idx_envelopes_family_hint_id")
                || plan.to_ascii_lowercase().contains("using index"),
            "expected index-backed plan, got:\n{plan}"
        );
        assert!(
            !plan.contains("SCAN TABLE envelopes")
                || plan.contains("idx_envelopes_family_hint_id")
                || plan.contains("USING INDEX"),
            "unexpected table-scan plan:\n{plan}"
        );
    }

    #[test]
    fn fetch_query_plan_at_ten_thousand_rows_still_uses_index() {
        let (_db, store) = test_store();
        let now = 1_700_000_000_000i64;
        let sealed = sample_sealed(1);
        let rows: Vec<_> = (0..10_000u32)
            .map(|i| {
                let mut msg_id = vec![0u8; MSG_ID_LEN];
                msg_id[..4].copy_from_slice(&i.to_be_bytes());
                let hint_byte = (i % 16) as u8;
                (
                    "family-a".to_string(),
                    msg_id,
                    7u8,
                    sample_hint(hint_byte),
                    sealed.clone(),
                    now + 60_000,
                    now,
                )
            })
            .collect();
        store.insert_envelopes_batch(&rows).unwrap();

        let plan = store
            .explain_fetch_plan("family-a", &[sample_hint(3)], 100, 100, now)
            .unwrap();
        assert!(
            plan.contains("idx_envelopes_family_hint_id")
                || plan.to_ascii_lowercase().contains("using index"),
            "expected index at ~10k rows, got:\n{plan}"
        );
    }

    #[tokio::test]
    async fn cursor_pagination_does_not_delete_and_rewinds_work() {
        let (_db, store) = test_store();
        let app = app(AppState::new(
            store.clone(),
            HashSet::from(["family-a".to_string()]),
        ));
        let hint = encode_base64_field(&sample_hint(1));
        let now = now_ms();

        for msg_byte in 1u8..=5 {
            store
                .insert_envelope(
                    "family-a",
                    sample_msg_id(msg_byte),
                    7,
                    sample_hint(1),
                    sample_sealed(msg_byte),
                    now + 60_000,
                    now,
                )
                .unwrap();
        }

        let page1 = Request::builder()
            .uri(format!("/envelopes?hints={hint}&after=0&limit=2"))
            .header("authorization", "Bearer family-a")
            .body(Body::empty())
            .unwrap();
        let json1 = body_json(app.clone().oneshot(page1).await.unwrap()).await;
        assert_eq!(json1["envelopes"].as_array().unwrap().len(), 2);
        let cursor = json1["next_cursor"].as_i64().unwrap();

        let page2 = Request::builder()
            .uri(format!("/envelopes?hints={hint}&after={cursor}&limit=2"))
            .header("authorization", "Bearer family-a")
            .body(Body::empty())
            .unwrap();
        let json2 = body_json(app.clone().oneshot(page2).await.unwrap()).await;
        assert_eq!(json2["envelopes"].as_array().unwrap().len(), 2);

        // All five still present — cursor advance is not an implicit ack.
        assert_eq!(store.count_for_family("family-a").unwrap(), 5);

        let rewind = Request::builder()
            .uri(format!("/envelopes?hints={hint}&after=0&limit=10"))
            .header("authorization", "Bearer family-a")
            .body(Body::empty())
            .unwrap();
        let json3 = body_json(app.oneshot(rewind).await.unwrap()).await;
        assert_eq!(json3["envelopes"].as_array().unwrap().len(), 5);
    }

    #[tokio::test]
    async fn presence_announce_then_query_is_scoped_to_family() {
        let app = test_app();
        let hint = encode_base64_field(&sample_hint(7));
        let other = encode_base64_field(&sample_hint(8));

        let announce = Request::builder()
            .method("POST")
            .uri("/presence")
            .header("authorization", "Bearer family-a")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "announce": [hint],
                    "query": [hint, other],
                })
                .to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(announce).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let json = body_json(response).await;
        assert_eq!(json["presence"].as_array().unwrap().len(), 1);
        assert_eq!(json["presence"][0]["hint"], hint);
        let server_now = json["now_ms"].as_i64().unwrap();
        let last_seen = json["presence"][0]["last_seen_ms"].as_i64().unwrap();
        assert!(last_seen <= server_now);

        let cross_family = Request::builder()
            .method("POST")
            .uri("/presence")
            .header("authorization", "Bearer family-b")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "announce": [],
                    "query": [hint],
                })
                .to_string(),
            ))
            .unwrap();
        let json = body_json(app.oneshot(cross_family).await.unwrap()).await;
        assert!(json["presence"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn presence_validates_hint_lengths_and_limits() {
        let too_many_announce = Request::builder()
            .method("POST")
            .uri("/presence")
            .header("authorization", "Bearer family-a")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "announce": (0..5).map(|_| encode_base64_field(&sample_hint(1))).collect::<Vec<_>>(),
                    "query": [],
                })
                .to_string(),
            ))
            .unwrap();
        let response = test_app().oneshot(too_many_announce).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let bad_hint = Request::builder()
            .method("POST")
            .uri("/presence")
            .header("authorization", "Bearer family-a")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "announce": [encode_base64_field(&[1, 2, 3])],
                    "query": [],
                })
                .to_string(),
            ))
            .unwrap();
        let response = test_app().oneshot(bad_hint).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn prune_expired_removes_stale_presence_rows() {
        let (_db, store) = test_store();
        let now = 1_700_000_000_000i64;
        store
            .sync_presence(
                "family-a",
                &[sample_hint(1)],
                &[],
                now - PRESENCE_RETENTION_MS - 1,
            )
            .unwrap();
        store
            .sync_presence(
                "family-a",
                &[sample_hint(2)],
                &[],
                now - PRESENCE_RETENTION_MS + 1,
            )
            .unwrap();

        store.prune_expired(now).unwrap();
        let rows = store
            .sync_presence("family-a", &[], &[sample_hint(1), sample_hint(2)], now)
            .unwrap();
        assert_eq!(
            rows,
            vec![StoredPresence {
                hint: sample_hint(2),
                last_seen_ms: now - PRESENCE_RETENTION_MS + 1,
            }]
        );
    }

    // --- DTN_TODOS.md D7: per-envelope size cap + per-family quota ---

    #[test]
    fn family_sealed_bytes_is_zero_for_an_untouched_family() {
        let (_db, store) = test_store();
        // Regression guard for the SUM(...) over zero rows -> SQL NULL
        // footgun (Invalid column type Null) rather than 0.
        assert_eq!(store.family_sealed_bytes("family-a").unwrap(), 0);
    }

    #[test]
    fn family_sealed_bytes_sums_only_the_callers_family() {
        let (_db, store) = test_store();
        let now = 1_700_000_000_000i64;
        store
            .insert_envelope(
                "family-a",
                sample_msg_id(1),
                7,
                sample_hint(1),
                vec![0u8; 100],
                now + 60_000,
                now,
            )
            .unwrap();
        store
            .insert_envelope(
                "family-a",
                sample_msg_id(2),
                7,
                sample_hint(1),
                vec![0u8; 250],
                now + 60_000,
                now,
            )
            .unwrap();
        store
            .insert_envelope(
                "family-b",
                sample_msg_id(3),
                7,
                sample_hint(1),
                vec![0u8; 9_999],
                now + 60_000,
                now,
            )
            .unwrap();

        assert_eq!(store.family_sealed_bytes("family-a").unwrap(), 350);
        assert_eq!(store.family_sealed_bytes("family-b").unwrap(), 9_999);
    }

    // -- fetch page byte budget ------------------------------------------

    /// The budget is only meaningful if the page it permits actually fits
    /// what a client will decode. Recomputed here from its inputs rather than
    /// restated, so raising the row limit, the per-row overhead, or the
    /// budget itself fails here instead of in somebody's mailbox.
    #[test]
    fn page_worst_case_fits_the_client_body_cap() {
        let base64_of_budget = MAX_FETCH_PAGE_SEALED_BYTES.div_ceil(3) * 4;
        let scaffolding = MAX_FETCH_LIMIT * MAX_FETCH_ROW_OVERHEAD_BYTES;
        let wrapper = 64;
        let worst_case = base64_of_budget + scaffolding + wrapper;
        assert!(
            worst_case <= CLIENT_MAX_RESPONSE_BODY_BYTES,
            "worst-case page of {worst_case} bytes exceeds the {CLIENT_MAX_RESPONSE_BODY_BYTES}-byte client cap"
        );
        // And it is not so tight that a single added field would break it:
        // keep at least 5% of the cap spare.
        assert!(worst_case + CLIENT_MAX_RESPONSE_BODY_BYTES / 20 <= CLIENT_MAX_RESPONSE_BODY_BYTES);
        // That one maximum-size envelope fits the budget on its own -- the
        // premise of the always-return-one-row rule -- is asserted at compile
        // time next to the constants themselves.
    }

    /// Duplicated from `core/src/relay_wire.rs`'s
    /// `RELAY_MAX_RESPONSE_BODY_BYTES`; the core's own
    /// `exposes_bounded_fetch_policy` pins its side. If either moves without
    /// the other, one of the two tests fails.
    #[test]
    fn client_body_cap_matches_the_core() {
        assert_eq!(
            CLIENT_MAX_RESPONSE_BODY_BYTES as u32,
            cruisemesh_core::relay_max_response_bytes()
        );
    }

    #[test]
    fn a_byte_heavy_page_is_truncated_by_bytes_and_the_cursor_still_advances() {
        let (_db, store) = test_store();
        let now = 1_700_000_000_000i64;
        // 20 maximum-size envelopes = 10 MiB, comfortably past the 8 MiB
        // page budget but only 20 rows — nothing a row limit would catch.
        let mut ids = Vec::new();
        for index in 0..20u8 {
            ids.push(
                store
                    .insert_envelope(
                        "family-a",
                        sample_msg_id(index),
                        7,
                        sample_hint(1),
                        vec![index; MAX_ENVELOPE_SEALED_BYTES],
                        now + 60_000,
                        now,
                    )
                    .unwrap(),
            );
        }

        let rows_per_page = MAX_FETCH_PAGE_SEALED_BYTES / MAX_ENVELOPE_SEALED_BYTES;
        let first = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], 0, MAX_FETCH_LIMIT, now)
            .unwrap();
        assert_eq!(
            first.len(),
            rows_per_page,
            "the page must be cut by bytes, not by the row limit"
        );
        let first_bytes: usize = first.iter().map(|row| row.sealed.len()).sum();
        assert!(first_bytes <= MAX_FETCH_PAGE_SEALED_BYTES);
        assert_eq!(first.first().unwrap().id, ids[0]);

        // The cursor the handler derives is the last returned row's id, and
        // resuming from it picks up exactly where the truncation stopped —
        // no row skipped, none repeated.
        let cursor = first.last().unwrap().id;
        assert_eq!(cursor, ids[rows_per_page - 1]);
        let second = store
            .fetch_envelopes(
                "family-a",
                vec![sample_hint(1)],
                cursor,
                MAX_FETCH_LIMIT,
                now,
            )
            .unwrap();
        assert_eq!(second.len(), 20 - rows_per_page);
        assert_eq!(second.first().unwrap().id, ids[rows_per_page]);

        // Walking to the end sees every row exactly once.
        let mut walked: Vec<i64> = first
            .iter()
            .chain(second.iter())
            .map(|row| row.id)
            .collect();
        walked.dedup();
        assert_eq!(walked, ids);
        assert!(store
            .fetch_envelopes(
                "family-a",
                vec![sample_hint(1)],
                *ids.last().unwrap(),
                MAX_FETCH_LIMIT,
                now
            )
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_single_row_over_the_whole_budget_is_still_returned_alone() {
        // Rows this large cannot be posted today (`MAX_ENVELOPE_SEALED_BYTES`
        // rejects them at the door) but may exist in a database written by an
        // older build. Refusing to return one would make it permanently
        // unreachable AND stall every client's cursor on it forever, which is
        // strictly worse than handing over one oversized page.
        let (_db, store) = test_store();
        let now = 1_700_000_000_000i64;
        let big = store
            .insert_envelope(
                "family-a",
                sample_msg_id(1),
                7,
                sample_hint(1),
                vec![9u8; MAX_FETCH_PAGE_SEALED_BYTES + 1],
                now + 60_000,
                now,
            )
            .unwrap();
        let follower = store
            .insert_envelope(
                "family-a",
                sample_msg_id(2),
                7,
                sample_hint(1),
                sample_sealed(2),
                now + 60_000,
                now,
            )
            .unwrap();

        let page = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], 0, MAX_FETCH_LIMIT, now)
            .unwrap();
        assert_eq!(page.len(), 1, "the oversized row must come back on its own");
        assert_eq!(page[0].id, big);

        // And the row behind it is not stranded: the next page returns it.
        let next = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], big, MAX_FETCH_LIMIT, now)
            .unwrap();
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].id, follower);
    }

    #[tokio::test]
    async fn get_envelopes_returns_a_short_page_that_the_client_can_decode() {
        let app = test_app();
        let expiry = now_ms() + 600_000;
        // Twenty maximum-size envelopes: 10 MiB, which a row-counted page
        // would hand back in one response of roughly 13.6 MiB -- past the
        // client's 12 MiB cap, and so a page it would refuse to decode on
        // this pass and identically on every pass after it.
        let total_rows = 20u8;
        for index in 0..total_rows {
            let request = Request::builder()
                .method("POST")
                .uri("/envelopes")
                .header("authorization", "Bearer family-a")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({
                        "msg_id": encode_base64_field(&sample_msg_id(index)),
                        "hop_ttl": 7,
                        "recipient_hint": encode_base64_field(&sample_hint(1)),
                        "sealed": encode_base64_field(&vec![index; MAX_ENVELOPE_SEALED_BYTES]),
                        "expiry_ms": expiry,
                    })
                    .to_string(),
                ))
                .unwrap();
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::OK
            );
        }

        let fetch = Request::builder()
            .method("GET")
            .uri(format!(
                "/envelopes?hints={}&after=0&limit=500",
                encode_base64_field(&sample_hint(1))
            ))
            .header("authorization", "Bearer family-a")
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(fetch).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(
            bytes.len() <= CLIENT_MAX_RESPONSE_BODY_BYTES,
            "a page the client would refuse to decode is a stalled mailbox"
        );
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let envelopes = json["envelopes"].as_array().unwrap();
        assert!(!envelopes.is_empty());
        // Short of the ask, and short of what is in the mailbox: the client's
        // walk continues from the cursor rather than reading this as the end
        // (core `relay_fetch_walk_continues`).
        assert!(envelopes.len() < usize::from(total_rows));
        assert_eq!(
            json["next_cursor"].as_i64().unwrap(),
            envelopes.last().unwrap()["id"].as_i64().unwrap(),
            "the cursor must name the last row actually returned"
        );

        // Resuming from that cursor drains the rest, so nothing is stranded.
        let mut cursor = json["next_cursor"].as_i64().unwrap();
        let mut seen = envelopes.len();
        loop {
            let next = Request::builder()
                .method("GET")
                .uri(format!(
                    "/envelopes?hints={}&after={cursor}&limit=500",
                    encode_base64_field(&sample_hint(1))
                ))
                .header("authorization", "Bearer family-a")
                .body(Body::empty())
                .unwrap();
            let json = body_json(app.clone().oneshot(next).await.unwrap()).await;
            let page = json["envelopes"].as_array().unwrap().clone();
            if page.is_empty() {
                break;
            }
            seen += page.len();
            cursor = json["next_cursor"].as_i64().unwrap();
        }
        assert_eq!(
            seen,
            usize::from(total_rows),
            "every row must be reachable across pages"
        );
    }

    #[test]
    fn envelope_exists_is_scoped_to_family_and_msg_id() {
        let (_db, store) = test_store();
        let now = 1_700_000_000_000i64;
        store
            .insert_envelope(
                "family-a",
                sample_msg_id(1),
                7,
                sample_hint(1),
                sample_sealed(1),
                now + 60_000,
                now,
            )
            .unwrap();

        assert!(store
            .envelope_exists("family-a", &sample_msg_id(1))
            .unwrap());
        assert!(!store
            .envelope_exists("family-a", &sample_msg_id(2))
            .unwrap());
        // Same msg_id, different family: not the same row.
        assert!(!store
            .envelope_exists("family-b", &sample_msg_id(1))
            .unwrap());
    }

    #[tokio::test]
    async fn post_rejects_a_sealed_payload_over_the_cap_with_413() {
        let app = test_app();
        let request = Request::builder()
            .method("POST")
            .uri("/envelopes")
            .header("authorization", "Bearer family-a")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "msg_id": encode_base64_field(&sample_msg_id(1)),
                    "hop_ttl": 7,
                    "recipient_hint": encode_base64_field(&sample_hint(1)),
                    "sealed": encode_base64_field(&vec![7u8; MAX_ENVELOPE_SEALED_BYTES + 1]),
                    "expiry_ms": now_ms() + 60_000,
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let json = body_json(response).await;
        assert_eq!(json["code"], "envelope_too_large");
    }

    #[tokio::test]
    async fn post_accepts_a_sealed_payload_exactly_at_the_cap() {
        let app = test_app();
        let request = Request::builder()
            .method("POST")
            .uri("/envelopes")
            .header("authorization", "Bearer family-a")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "msg_id": encode_base64_field(&sample_msg_id(1)),
                    "hop_ttl": 7,
                    "recipient_hint": encode_base64_field(&sample_hint(1)),
                    "sealed": encode_base64_field(&vec![7u8; MAX_ENVELOPE_SEALED_BYTES]),
                    "expiry_ms": now_ms() + 60_000,
                })
                .to_string(),
            ))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    // --- FR8: WAL + busy_timeout + spawn_blocking ---

    #[test]
    fn open_enables_wal_journal_mode_for_a_file_backed_database() {
        let db = NamedTempFile::new().unwrap();
        let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
        let mode: String = {
            let conn = store.conn.lock().unwrap();
            conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }

    #[test]
    fn open_sets_the_configured_busy_timeout() {
        let db = NamedTempFile::new().unwrap();
        let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
        let timeout_ms: i64 = {
            let conn = store.conn.lock().unwrap();
            conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(timeout_ms, SQLITE_BUSY_TIMEOUT.as_millis() as i64);
    }

    /// `:memory:` databases can't use WAL (SQLite silently keeps them in
    /// "memory" journal mode) -- `RelayStore::open` must not error on that,
    /// since the rest of this test module opens `:memory:` stores
    /// throughout.
    #[test]
    fn open_does_not_error_on_an_in_memory_database() {
        let store = RelayStore::open(":memory:").unwrap();
        let mode: String = {
            let conn = store.conn.lock().unwrap();
            conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap()
        };
        assert_eq!(mode.to_ascii_lowercase(), "memory");
    }

    /// FR8: store calls run on a spawn_blocking thread, not the calling
    /// task's own worker -- if `run_blocking` were accidentally a no-op
    /// wrapper (e.g. calling `f` inline instead of inside
    /// `spawn_blocking`), this test would still pass functionally but the
    /// point of the change would be silently lost. Assert the closure
    /// actually executes on a different OS thread than the caller.
    #[tokio::test]
    async fn run_blocking_executes_off_the_calling_thread() {
        let (_db, store) = test_store();
        let caller_thread = std::thread::current().id();
        let worker_thread = store
            .run_blocking(move |_store| Ok(std::thread::current().id()))
            .await
            .unwrap();
        assert_ne!(
            caller_thread, worker_thread,
            "store call should run on a spawn_blocking thread, not the caller's"
        );
    }

    /// Bucket math, driven off explicit `Instant`s instead of sleeping: a
    /// full minute's allowance bursts at once, refills at allowance/60 per
    /// second, and never accumulates past capacity while idle.
    #[test]
    fn token_bucket_bursts_a_minute_then_refills_without_accumulating() {
        let start = Instant::now();
        let mut bucket = TokenBucket::per_minute(60.0, start);

        // A quiet family may spend the whole minute's worth immediately.
        assert!(bucket.try_take(60.0, start));
        assert!(!bucket.try_take(1.0, start), "bucket is empty at t=0");

        // 60/min == 1/sec.
        assert!(bucket.try_take(1.0, start + Duration::from_secs(1)));
        assert!(!bucket.try_take(1.0, start + Duration::from_secs(1)));

        // Ten idle minutes credit one minute's worth, not ten.
        let later = start + Duration::from_secs(600);
        assert!(bucket.try_take(60.0, later));
        assert!(!bucket.try_take(1.0, later));
    }

    /// `Retry-After` must be a whole number of seconds, never 0 (a client
    /// told to retry immediately just hot-loops), and never longer than the
    /// one-minute window that always refills the bucket completely.
    #[test]
    fn token_bucket_retry_after_is_a_sane_whole_second_wait() {
        let start = Instant::now();
        let mut bucket = TokenBucket::per_minute(60.0, start);
        assert!(bucket.try_take(60.0, start));

        // Empty bucket, 1 token/sec: 4 tokens are 4 seconds away.
        assert_eq!(bucket.retry_after_secs(4.0), 4);
        // A fractional wait rounds up, and never reports 0.
        assert_eq!(bucket.retry_after_secs(0.25), 1);
        // A cost the bucket could never satisfy is clamped, not absurd.
        assert_eq!(
            bucket.retry_after_secs(10_000.0),
            RATE_LIMIT_MAX_RETRY_AFTER_SECS
        );
    }

    /// Eviction drops families that have been quiet long enough to have
    /// refilled anyway, and keeps the ones still spending their allowance —
    /// including a bucket whose stale `tokens` field only *looks* empty
    /// until it is refilled.
    #[test]
    fn evict_idle_rate_buckets_drops_only_the_long_idle_families() {
        let start = Instant::now();
        let mut buckets = HashMap::new();
        // Both families spend everything at t=0, so both look empty.
        for token in ["idle-family", "busy-family"] {
            let mut family = FamilyBuckets::new(60, 60, start);
            assert!(family.try_take(60.0, 60.0, start).is_ok());
            buckets.insert(token.to_string(), family);
        }
        // ...but the busy one was touched a second ago.
        let now = start + RATE_BUCKET_IDLE_EVICT_AFTER + Duration::from_secs(1);
        let busy = buckets.get_mut("busy-family").unwrap();
        assert!(busy
            .try_take(1.0, 1.0, now - Duration::from_secs(1))
            .is_ok());

        evict_idle_rate_buckets(&mut buckets, now);

        assert!(!buckets.contains_key("idle-family"));
        assert!(buckets.contains_key("busy-family"));
    }

    /// `0` is meaningless for every rate knob (it locks every client out
    /// forever), so the parsers reject it rather than let an operator brick
    /// the relay with a typo; unset means "use the default".
    #[test]
    fn rate_limit_parsers_reject_zero_and_garbage() {
        assert_eq!(parse_rate_requests_per_min("600"), Ok(600));
        assert!(parse_rate_requests_per_min("0").is_err());
        assert!(parse_rate_requests_per_min("-1").is_err());
        assert!(parse_rate_requests_per_min("lots").is_err());

        assert_eq!(parse_rate_bytes_per_min("67108864"), Ok(67_108_864));
        assert!(parse_rate_bytes_per_min("0").is_err());
        assert!(parse_rate_bytes_per_min("64MiB").is_err());
    }

    /// FR8 (verifying FR2 already covers this): `ApiError::internal` must
    /// never leak rusqlite error text (which can include the DB file path)
    /// into a client-visible response body.
    #[test]
    fn internal_error_body_never_contains_the_raw_detail() {
        let detail = "disk I/O error: unable to open database file /secret/path/db.sqlite";
        let error = ApiError::internal(detail.to_string());
        assert_eq!(error.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!error.message.contains("secret"));
        assert!(!error.message.contains("sqlite"));
        assert_eq!(error.message, "internal server error");
    }
}
