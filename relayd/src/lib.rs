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

pub mod apns;

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::header::{AUTHORIZATION, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use ed25519_dalek::{Signature, VerifyingKey};
use rand_core::RngCore;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{info, warn};

const RECIPIENT_HINT_LEN: usize = 8;
const MSG_ID_LEN: usize = 16;
const DEFAULT_FETCH_LIMIT: usize = 100;
const MAX_FETCH_LIMIT: usize = 500;

type EnvelopeBatchRow = (String, Vec<u8>, u8, Vec<u8>, Vec<u8>, i64, i64);

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
///
/// This budget has a floor as well as a ceiling: clients already in the field
/// read a short page as the end of the mailbox, so it must stay large enough
/// that their 16-row ask can never be truncated. See
/// `the_page_budget_can_never_truncate_a_sixteen_row_ask`.
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
pub const MAX_PUSH_HINTS: usize = 256;
const PUSH_REGISTRATION_RETENTION_MS: i64 = 45 * 24 * 60 * 60 * 1000;
const MAX_PRESENCE_ANNOUNCE: usize = 4;
const MAX_PRESENCE_QUERY: usize = 512;
const PRESENCE_RETENTION_MS: i64 = 48 * 60 * 60 * 1000;

/// Cross-family presence: the most query hints one *deposit*-class call may
/// carry.
///
/// A member token asks on behalf of a whole family and legitimately carries
/// hundreds of hints ([`MAX_PRESENCE_QUERY`]). A deposit credential asks on
/// behalf of the one contact whose friend card it came from, and a contact is
/// four rotating hints (`core/src/recipient_hints.rs::recent_presence_hints_for`).
/// Eight is that with room for a rotation boundary, and it is the cap that
/// keeps this route from being a bulk oracle: a holder cannot sweep a
/// dictionary of hints per request, only ask about the person whose card they
/// already hold.
pub const MAX_DEPOSIT_PRESENCE_QUERY: usize = 8;

/// Cross-family presence recency buckets, in milliseconds of age. A
/// deposit-class caller is told which of these a hint falls in, never when it
/// was actually seen.
///
/// The edges are the windows the shells already draw their last-seen copy
/// from, so a coarse answer lands in the same sentence a precise one would:
/// `core/src/connection_health.rs::CONNECTION_PRESENCE_ONLINE_WINDOW_MS`
/// (2.5 min, "seen online") and Android's `ContactReachability.RECENT_WINDOW_MS`
/// (15 min, "seen recently"), then a day, then whatever is left of the 48-hour
/// [`PRESENCE_RETENTION_MS`] window.
const PRESENCE_BUCKET_ACTIVE_MS: i64 = 150_000;
const PRESENCE_BUCKET_RECENT_MS: i64 = 15 * 60 * 1000;
const PRESENCE_BUCKET_DAY_MS: i64 = 24 * 60 * 60 * 1000;
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

/// Retention ceiling for a row posted with a *deposit* credential. A friend
/// card only ever needs the honest client's 7-day envelope life (core's
/// `DEFAULT_EXPIRY_MS`); one extra day absorbs clock skew. Without this, a
/// leaked card could park quota-filling rows for the full member-class
/// [`MAX_RETENTION_MS`] (30 days), turning the family's self-healing
/// week-long storage squeeze into a month-long one. Member-class posts keep
/// the 30-day ceiling.
pub const MAX_DEPOSIT_RETENTION_MS: i64 = 8 * 24 * 60 * 60 * 1000;

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

/// Percentage of a family's storage quota that *all* deposit-class rows
/// combined may occupy at once.
///
/// The quota above is a single family-wide pool, and before this constant
/// existed a deposit-class post drew on it exactly like a member-class one.
/// That is fine as an accounting rule and wrong as a fairness rule: a
/// family's deposit credential is stamped onto every friend card it has ever
/// handed out, so the pool the family's own phones depend on could be filled
/// entirely by traffic none of those phones sent — after which the family's
/// own posts start failing too, and keep failing until the deposited rows
/// age out (up to [`MAX_DEPOSIT_RETENTION_MS`] later).
///
/// Reserving half the pool for member-class traffic is the smallest rule
/// that makes that impossible. The reservation is one-sided: member-class
/// posts still see the *whole* quota, so a family whose friends post nothing
/// is unaffected, and a family whose friends post constantly still has half
/// a mailbox of its own. Half is deliberately generous rather than minimal —
/// friend traffic is traffic the family wants, and 128 MiB of it is ~700
/// max-size attachments, well past a cruise's worth of friend photos.
pub const DEPOSIT_QUOTA_TOTAL_PERCENT: u64 = 50;

/// Percentage of a family's storage quota that any *one* depositor may
/// occupy, checked in addition to [`DEPOSIT_QUOTA_TOTAL_PERCENT`].
///
/// The aggregate share protects the family from its friends; this one
/// protects the friends from each other. Without it the first depositor to
/// fill the deposit half shuts out every other depositor, which is the same
/// starvation one level down. A quarter means it takes at least two
/// depositors to reach the aggregate ceiling, and four before any single one
/// of them could have been the whole cause.
///
/// It is a percentage of the *quota*, not of the aggregate share, so the two
/// numbers can be read directly against each other. Shares deliberately do
/// **not** shrink as depositors arrive: a share divided by a live depositor
/// count would let any credential holder shrink everyone else's allowance
/// just by inventing depositors, and would make an honest friend's admission
/// depend on strangers. Fixed shares oversubscribe instead (four depositors
/// at a quarter each sum to the whole quota), which is exactly why the
/// aggregate ceiling above exists and is checked as well — the sum of the
/// shares can never become a family lockout, because the family's half is
/// never on offer to any of them.
pub const DEPOSIT_QUOTA_PER_DEPOSITOR_PERCENT: u64 = 25;

/// `value * percent / 100`, computed in `u128` so a large quota cannot
/// overflow the multiplication before the division brings it back in range.
fn percent_of(value: u64, percent: u64) -> u64 {
    ((u128::from(value) * u128::from(percent)) / 100) as u64
}

/// Ceiling on the sealed bytes all deposit-class rows together may hold for
/// a family whose storage quota is `family_quota_bytes`.
pub fn deposit_total_share_bytes(family_quota_bytes: u64) -> u64 {
    percent_of(family_quota_bytes, DEPOSIT_QUOTA_TOTAL_PERCENT)
}

/// Ceiling on the sealed bytes one depositor may hold for a family whose
/// storage quota is `family_quota_bytes`.
pub fn deposit_per_depositor_share_bytes(family_quota_bytes: u64) -> u64 {
    percent_of(family_quota_bytes, DEPOSIT_QUOTA_PER_DEPOSITOR_PERCENT)
}

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

/// Cross-family presence: how many `/presence` queries one deposit
/// credential may make per [`DEFAULT_DEPOSIT_PRESENCE_WINDOW_SECS`],
/// configurable via `CRUISEMESH_RELAY_DEPOSIT_PRESENCE_QUERIES`.
///
/// Small burst, long window — the opposite shape to every other allowance
/// here, and deliberately so. A presence answer is advisory; nothing breaks
/// if one is refused, so this can be sized to what a client legitimately
/// needs rather than to what a client might want. The client-side floor is
/// one query per contact per fifteen minutes
/// (`core/src/session/relay_pass.rs::RELAY_CROSS_FAMILY_PRESENCE_MIN_INTERVAL_MS`),
/// so four in a window is a device asking on schedule with three spare —
/// enough to absorb a reinstall, a clock jump, or a second device in the
/// household holding the same friend card, and far too few to sample anyone's
/// activity.
pub const DEFAULT_DEPOSIT_PRESENCE_QUERIES: u32 = 4;

/// The window [`DEFAULT_DEPOSIT_PRESENCE_QUERIES`] is spread over, in
/// seconds. Matches the client's own re-ask floor.
pub const DEFAULT_DEPOSIT_PRESENCE_WINDOW_SECS: u64 = 900;

/// `POST /family/rotate` allowance, per family per
/// [`DEFAULT_ROTATION_WINDOW_SECS`], charged against a bucket of its own.
///
/// It cannot share the family's request bucket, and the reason is the whole
/// point of the route: rotation is the *remedy* for a family whose credential
/// is in hostile hands, and the device holding that credential can burn the
/// family's shared request allowance at will. Charging the remedy to the
/// bucket the attacker controls would let the attacker make the remedy
/// unreachable — the family would be rate-limited out of locking its own
/// thief out.
///
/// Small on purpose in the other direction too: a rotation is a rare ceremony
/// (a device revocation, not a sync), each one rewrites every row the family
/// owns, and a client that loses a response retries with the *same* token,
/// which converges rather than rotating again.
///
/// The bucket is charged per *attempt*, before the request shape is even
/// validated, which is deliberate — a caller must not be able to hammer the
/// route for free by sending garbage — and it is what sets the number. Ten
/// per hour leaves a real ceremony (one rotation, a couple of retries through
/// a bad connection) room to spare even if the client also fumbles the
/// request shape several times on the way, and leaves nothing else room at
/// all.
pub const DEFAULT_ROTATION_REQUESTS: u32 = 10;

/// The window [`DEFAULT_ROTATION_REQUESTS`] is spread over, in seconds.
pub const DEFAULT_ROTATION_WINDOW_SECS: u64 = 3600;

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

/// Whether a string is shaped like a stable family id rather than a token.
/// Provisioning and rotation both refuse tokens that answer `true` here, which
/// is what keeps `/admin/families/{id}` unambiguous (`resolve_family_on`).
pub fn is_family_id(value: &str) -> bool {
    value.trim().starts_with(FAMILY_ID_PREFIX)
}

/// Mint a fresh stable family id. Called once per family, at provisioning.
fn mint_family_id() -> String {
    let mut bytes = [0u8; FAMILY_ID_RANDOM_BYTES];
    rand_core::OsRng.fill_bytes(&mut bytes);
    format!("{FAMILY_ID_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes))
}

/// The exact bytes a rotation authority signs (`ROTATION_SIGNING_DOMAIN`).
///
/// Both tokens are length-prefixed with a big-endian `u16`, which
/// `MAX_FAMILY_TOKEN_LEN` (1024) guarantees fits. That framing is what makes
/// the message injective in the pair: without it, a signature over
/// (`ab`, `c`) would also read as a signature over (`a`, `bc`), and a
/// signature captured from one rotation could authorize a different one.
///
/// On an idempotent retry the caller presents the new token as its bearer
/// credential, so `current_token` and `new_token` are the same string and the
/// signed message binds (new, new). That is a *different* message from the
/// original (current, new), which is deliberate: the retry is a separate
/// assertion and has to be signed as one.
fn rotation_signed_bytes(current_token: &str, new_token: &str) -> Vec<u8> {
    let mut message = Vec::with_capacity(
        ROTATION_SIGNING_DOMAIN.len() + 4 + current_token.len() + new_token.len(),
    );
    message.extend_from_slice(ROTATION_SIGNING_DOMAIN);
    for token in [current_token, new_token] {
        message.extend_from_slice(&(token.len() as u16).to_be_bytes());
        message.extend_from_slice(token.as_bytes());
    }
    message
}

/// The Ed25519 keypair half a rotation presents, already length-checked by
/// the handler. Raw bytes rather than a parsed `VerifyingKey` because a
/// malformed key is an authorization failure to be answered with a 403, not a
/// parse error to be handled at the HTTP boundary — `verify_rotation_signature`
/// makes that decision in one place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RotationAuthority {
    pub public_key: [u8; ROTATION_PK_LEN],
    pub signature: [u8; ROTATION_SIG_LEN],
}

/// Verify a rotation signature, returning `false` for every failure mode
/// rather than distinguishing them.
///
/// A key that is not a canonical Ed25519 point, a low-order key, a signature
/// that does not verify — all of them mean the same thing to the caller: this
/// request does not carry the family's rotation authority. Nothing here can
/// panic on attacker-chosen bytes; `VerifyingKey::from_bytes` rejects
/// non-canonical encodings and `verify_strict` rejects the low-order-key
/// malleability cases the plain `verify` accepts.
fn verify_rotation_signature(
    public_key: &[u8; ROTATION_PK_LEN],
    message: &[u8],
    signature: &[u8; ROTATION_SIG_LEN],
) -> bool {
    let Ok(verifying_key) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    verifying_key
        .verify_strict(message, &Signature::from_bytes(signature))
        .is_ok()
}

/// What checking a presented rotation authority against the stored one
/// concluded. `Register` is the trust-on-first-rotation case and is the only
/// one that writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RotationAuthorityCheck {
    /// The signature verified against the key already on the row.
    Accepted,
    /// The row carried no key; the signature verified against the presented
    /// one, which is therefore this family's rotation key from now on.
    Register,
    /// No authority. Everything else lands here.
    Refused,
}

/// Decide whether a presented rotation authority may re-key this family.
///
/// The stored key wins absolutely once it exists: a presented key that
/// differs from it is refused without the signature even being examined, so
/// holding the member token buys nothing. Only when the row carries no key at
/// all does the presented key get to prove itself — and then it is registered,
/// which is what closes the door behind it.
fn check_rotation_authority(
    stored_pk: Option<&[u8]>,
    message: &[u8],
    authority: &RotationAuthority,
) -> RotationAuthorityCheck {
    match stored_pk {
        Some(stored) => {
            if stored != authority.public_key.as_slice() {
                return RotationAuthorityCheck::Refused;
            }
            if verify_rotation_signature(&authority.public_key, message, &authority.signature) {
                RotationAuthorityCheck::Accepted
            } else {
                RotationAuthorityCheck::Refused
            }
        }
        None => {
            if verify_rotation_signature(&authority.public_key, message, &authority.signature) {
                RotationAuthorityCheck::Register
            } else {
                RotationAuthorityCheck::Refused
            }
        }
    }
}

/// CP4: which capability class a presented bearer token resolved to.
/// Enforcement lives in `authorize_family` — the single choke point every
/// authenticated route goes through — so no individual handler can forget
/// the check: a deposit-class credential authorizes `FamilyOp::Post` and
/// nothing else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenClass {
    /// Full family credential (post + fetch + ack + presence + WS). Rides
    /// the Shore Pass setup card; every pre-CP4 token is this class.
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

/// Hosted-relay (Shore Pass) expiry grace: after a provisioned family's
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

/// Shortest credential `POST /family/rotate` accepts as a family's next member
/// token (`specs/multi-device-v1.md` §10 step 2).
///
/// The rotating client picks its own replacement (see `rotate_family`), which
/// is what makes the ceremony crash-safe: it writes the candidate down before
/// the call, so a lost response is recoverable by retrying rather than being a
/// family locked out of its own mailbox. The cost of letting the client choose
/// is that the server can no longer vouch for the entropy, so it enforces the
/// one property it can check from outside: length.
/// `core_mint_relay_member_token` emits 32 random bytes base64url-encoded
/// behind a class prefix, comfortably above this floor.
///
/// Operator provisioning (`POST /admin/families`) is deliberately not held to
/// this — that is a different trust relationship, and tokens already in the
/// field predate the rule.
pub const MIN_ROTATION_TOKEN_LEN: usize = 24;

/// Ed25519 domain separator for the `POST /family/rotate` authority
/// signature. Shape matches every other signing context in this project
/// (`core/src/device_roster.rs`): a human-readable sentence, versioned,
/// terminated by a NUL so no context can ever be a prefix of another.
///
/// The signed bytes are this, then each token length-prefixed as a big-endian
/// `u16` followed by its UTF-8 bytes — `current_token` first, `new_token`
/// second (`rotation_signed_bytes`). Length prefixes rather than a separator
/// because a separator byte can appear inside a token: two different
/// (current, new) pairs must never produce the same message, or a signature
/// captured for one rotation would authorize a different one.
const ROTATION_SIGNING_DOMAIN: &[u8] = b"CruiseMesh family token rotation v1\0";

/// An Ed25519 public key is 32 bytes and a signature is 64. Both arrive
/// base64url without padding, and a wrong length is a malformed request (400)
/// rather than a failed authorization (403): the caller's remedy is to fix its
/// encoder, not to find a different key.
const ROTATION_PK_LEN: usize = 32;
const ROTATION_SIG_LEN: usize = 64;

/// Class prefix on `families.family_id`, the stable handle a family keeps
/// across rotations.
///
/// It is deliberately its own prefix, distinct from `cmfam1-` (member tokens)
/// and `DEPOSIT_TOKEN_PREFIX` (`cmdep1-`), because
/// `GET/PATCH/DELETE /admin/families/{id}` resolves its path segment as
/// *either* a family id or a current token. Two namespaces sharing one path
/// segment are only unambiguous if no string can plausibly be in both, so
/// provisioning and rotation both refuse a token wearing this prefix — the
/// same discipline CP4 already applies to the deposit prefix.
pub const FAMILY_ID_PREFIX: &str = "cmfid1-";

/// Random bytes behind `FAMILY_ID_PREFIX`. Nine bytes is twelve base64url
/// characters — short enough for an operator to read off a dashboard and type
/// into a curl, and 72 bits is far past what a relay holding a few thousand
/// families could collide by accident.
const FAMILY_ID_RANDOM_BYTES: usize = 9;

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
    /// Cross-family presence: queries allowed per deposit credential per
    /// `deposit_presence_window_secs`. Its own dimension, charged by
    /// `AppState::check_presence_rate_limit` and by nothing else, so a
    /// presence flood cannot spend a single request or byte of the queried
    /// family's allowance (`PRESENCE-01`).
    pub deposit_presence_queries: u32,
    pub deposit_presence_window_secs: u64,
    /// `POST /family/rotate`: rotations allowed per family per
    /// `rotation_window_secs`. Its own dimension for the reason spelled out
    /// on [`DEFAULT_ROTATION_REQUESTS`] — the remedy must not be charged to
    /// the bucket the device being revoked can exhaust.
    pub rotation_requests: u32,
    pub rotation_window_secs: u64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            requests_per_min: DEFAULT_RATE_REQUESTS_PER_MIN,
            bytes_per_min: DEFAULT_RATE_BYTES_PER_MIN,
            deposit_requests_per_min: DEFAULT_DEPOSIT_RATE_REQUESTS_PER_MIN,
            deposit_bytes_per_min: DEFAULT_DEPOSIT_RATE_BYTES_PER_MIN,
            global_requests_per_min: DEFAULT_RATE_GLOBAL_REQUESTS_PER_MIN,
            deposit_presence_queries: DEFAULT_DEPOSIT_PRESENCE_QUERIES,
            deposit_presence_window_secs: DEFAULT_DEPOSIT_PRESENCE_WINDOW_SECS,
            rotation_requests: DEFAULT_ROTATION_REQUESTS,
            rotation_window_secs: DEFAULT_ROTATION_WINDOW_SECS,
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
        Self::per_window(per_min, 60.0, now)
    }

    /// The same bucket over an arbitrary window: capacity is the whole
    /// window's allowance and it refills across the window.
    ///
    /// A long window is what makes a *tight* allowance usable. The presence
    /// bucket wants "four, then roughly one every four minutes", which
    /// `per_minute` cannot express — rounded down to a per-minute figure it
    /// would either be zero (refusing everything) or one (letting a holder
    /// ask sixty times an hour). The refill rate is the allowance, and the
    /// capacity is only how much of it may arrive at once.
    fn per_window(allowance: f64, window_secs: f64, now: Instant) -> Self {
        let capacity = allowance.max(0.0);
        let window = window_secs.max(1.0);
        Self {
            tokens: capacity,
            capacity,
            refill_per_sec: capacity / window,
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

/// The buckets a single family token is charged against.
struct FamilyBuckets {
    requests: TokenBucket,
    bytes: TokenBucket,
    /// Cross-family presence queries. A third *dimension*, not a share of
    /// the first: `try_take_presence` charges this and only this, so a
    /// presence flood arriving on a friend-card credential cannot spend the
    /// request or byte allowance the family's own traffic rides on
    /// (`PRESENCE-01`). Member-class presence is an ordinary read and still
    /// charges `requests`.
    presence: TokenBucket,
    /// `POST /family/rotate`. A fourth dimension for the same reason presence
    /// is a third one, but pointing the other way: presence is separate so a
    /// stranger's flood cannot starve the family, and rotation is separate so
    /// the family's own exhausted (or maliciously exhausted) request
    /// allowance cannot deny it the one call that evicts the thief. Charged
    /// by `AppState::check_rotation_rate_limit` and by nothing else.
    rotation: TokenBucket,
}

impl FamilyBuckets {
    /// CP4: capacities are passed per credential class
    /// (`RateLimitConfig::allowances_for`) — one bucket map holds member and
    /// deposit entries side by side, keyed by the presented credential.
    fn new(
        requests_per_min: u32,
        bytes_per_min: u64,
        presence: (u32, u64),
        rotation: (u32, u64),
        now: Instant,
    ) -> Self {
        let (presence_queries, presence_window_secs) = presence;
        let (rotation_requests, rotation_window_secs) = rotation;
        Self {
            requests: TokenBucket::per_minute(f64::from(requests_per_min), now),
            bytes: TokenBucket::per_minute(bytes_per_min as f64, now),
            presence: TokenBucket::per_window(
                f64::from(presence_queries),
                presence_window_secs as f64,
                now,
            ),
            rotation: TokenBucket::per_window(
                f64::from(rotation_requests),
                rotation_window_secs as f64,
                now,
            ),
        }
    }

    /// Charge one rotation attempt. Touches neither `requests` nor `bytes`;
    /// see the field comment for why that separation is load-bearing.
    fn try_take_rotation(&mut self, now: Instant) -> Result<(), (RateLimitScope, u64)> {
        if self.rotation.try_take(1.0, now) {
            Ok(())
        } else {
            Err((
                RateLimitScope::RotationRequests,
                self.rotation.retry_after_secs(1.0),
            ))
        }
    }

    /// Charge one cross-family presence query. Deliberately touches neither
    /// `requests` nor `bytes`: the separation is the point, and it is what
    /// the paired e2e assertion checks.
    fn try_take_presence(&mut self, now: Instant) -> Result<(), (RateLimitScope, u64)> {
        if self.presence.try_take(1.0, now) {
            Ok(())
        } else {
            Err((
                RateLimitScope::PresenceQueries,
                self.presence.retry_after_secs(1.0),
            ))
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
    PresenceQueries,
    RotationRequests,
}

impl RateLimitScope {
    /// Log-field discriminant. Never contains the token.
    fn label(self) -> &'static str {
        match self {
            Self::FamilyRequests => "family_requests",
            Self::FamilyBytes => "family_bytes",
            Self::GlobalRequests => "global_requests",
            Self::PresenceQueries => "presence_queries",
            Self::RotationRequests => "rotation_requests",
        }
    }

    /// Client-facing phrasing: names both which limit (this family's vs the
    /// whole server's) and which dimension (requests vs uploaded bytes).
    fn description(self) -> &'static str {
        match self {
            Self::FamilyRequests => "family request rate limit",
            Self::FamilyBytes => "family upload byte rate limit",
            Self::GlobalRequests => "server-wide request rate limit",
            Self::PresenceQueries => "cross-family presence query rate limit",
            Self::RotationRequests => "family token rotation rate limit",
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
///
/// *Every* dimension has to answer that question, not just the two
/// per-minute ones. The presence and rotation buckets refill across windows
/// (fifteen minutes, an hour) far longer than the idle threshold, so a caller
/// that spent them and then went quiet for five minutes would have its entry
/// dropped and re-created full — turning eviction into a way to buy back an
/// allowance that had not actually refilled. Asking all four means an entry
/// is only ever dropped when re-creating it hands back nothing.
fn evict_idle_rate_buckets(buckets: &mut HashMap<String, FamilyBuckets>, now: Instant) {
    buckets.retain(|_, family| {
        let idle_for = now.saturating_duration_since(family.requests.last_refill);
        if idle_for < RATE_BUCKET_IDLE_EVICT_AFTER {
            return true;
        }
        family.requests.refill(now);
        family.bytes.refill(now);
        family.presence.refill(now);
        family.rotation.refill(now);
        !(family.requests.is_full()
            && family.bytes.is_full()
            && family.presence.is_full()
            && family.rotation.is_full())
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
    /// Optional APNs worker queue. Self-hosted relays without Apple provider
    /// credentials leave this disabled; registration remains harmless and
    /// the existing poll/WebSocket paths continue unchanged.
    push_wake_tx: Option<tokio::sync::mpsc::Sender<PushWake>>,
}

#[derive(Clone, Debug)]
pub struct BroadcastEnvelope {
    pub family_token: String,
    pub recipient_hint: String,
    pub envelope: EnvelopeResponse,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PushWake {
    pub device_tokens: Vec<String>,
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
            push_wake_tx: None,
        }
    }

    pub fn with_push_wake_sender(mut self, sender: tokio::sync::mpsc::Sender<PushWake>) -> Self {
        self.push_wake_tx = Some(sender);
        self
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
    /// CP4: buckets are keyed by `access.rate_key` — the family's stable key
    /// namespaced by the presented credential's class — with per-class
    /// capacities, so friend-card (deposit) traffic exhausts its own tighter
    /// allowance and never eats into the family's member-class buckets.
    fn check_rate_limit(
        &self,
        access: &FamilyAccess,
        requests: f64,
        bytes: f64,
    ) -> Result<(), ApiError> {
        let now = Instant::now();
        let family = self.charge_family_buckets(access, now, |buckets| {
            buckets.try_take(requests, bytes, now)
        });
        if let Err((scope, retry_after_secs)) = family {
            return Err(reject_rate_limited(&access.token, scope, retry_after_secs));
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
                &access.token,
                RateLimitScope::GlobalRequests,
                retry_after_secs,
            ));
        }
        Ok(())
    }

    /// Look this credential's buckets up (creating them at its class's
    /// capacities) and charge them with `charge`.
    ///
    /// Extracted so a new dimension cannot accidentally be added to a second
    /// bucket map, or keyed by anything but `access.rate_key`. That key is
    /// class-namespaced for a reason worth restating: dropping the class and
    /// keying on the family alone would put a friend-card holder's traffic
    /// and the family's own traffic in one bucket, which is precisely the
    /// starvation this exists to prevent.
    fn charge_family_buckets<T>(
        &self,
        access: &FamilyAccess,
        now: Instant,
        charge: impl FnOnce(&mut FamilyBuckets) -> T,
    ) -> T {
        let (requests_per_min, bytes_per_min) = self.rate_limits.allowances_for(access.class);
        let presence = (
            self.rate_limits.deposit_presence_queries,
            self.rate_limits.deposit_presence_window_secs,
        );
        let rotation = (
            self.rate_limits.rotation_requests,
            self.rate_limits.rotation_window_secs,
        );
        let mut buckets = self.rate_buckets.lock().unwrap_or_else(|e| e.into_inner());
        if buckets.len() >= RATE_BUCKET_EVICT_THRESHOLD {
            evict_idle_rate_buckets(&mut buckets, now);
        }
        charge(buckets.entry(access.rate_key.clone()).or_insert_with(|| {
            FamilyBuckets::new(requests_per_min, bytes_per_min, presence, rotation, now)
        }))
    }

    /// Charge one `POST /family/rotate` attempt against the rotation bucket
    /// and the global backstop, and nothing else.
    ///
    /// The family's request and byte buckets are deliberately untouched. A
    /// rotation is what a family does *because* a device it no longer trusts
    /// holds its member token, and that device can spend the shared request
    /// allowance as fast as the network lets it. If the remedy were charged
    /// to the bucket the attacker controls, the attacker could hold the
    /// family's own eviction call at 429 indefinitely — the rate limiter
    /// would be enforcing the lockout.
    ///
    /// The global backstop is still charged, for the server's sake rather
    /// than the family's: at four per hour per family a rotation flood cannot
    /// make a dent in it, but nothing authenticated should be entirely
    /// invisible to the server-wide cap.
    ///
    /// Only ever call this after the caller's token has authorized, for the
    /// reason spelled out on `check_rate_limit`.
    fn check_rotation_rate_limit(&self, access: &FamilyAccess) -> Result<(), ApiError> {
        let now = Instant::now();
        let charged =
            self.charge_family_buckets(access, now, |buckets| buckets.try_take_rotation(now));
        if let Err((scope, retry_after_secs)) = charged {
            return Err(reject_rate_limited(&access.token, scope, retry_after_secs));
        }
        let global_retry_after = {
            let mut global = self.rate_global.lock().unwrap_or_else(|e| e.into_inner());
            if global.try_take(1.0, now) {
                None
            } else {
                Some(global.retry_after_secs(1.0))
            }
        };
        if let Some(retry_after_secs) = global_retry_after {
            return Err(reject_rate_limited(
                &access.token,
                RateLimitScope::GlobalRequests,
                retry_after_secs,
            ));
        }
        Ok(())
    }

    /// Charge one *cross-family* presence query: the presence bucket, and
    /// then the global backstop.
    ///
    /// The queried family's request and byte buckets are never touched
    /// (`PRESENCE-01`). Only the server-wide backstop is charged beyond the
    /// presence bucket, and it is charged for the server's sake rather than
    /// the family's: at four queries per credential per fifteen minutes, a
    /// caller would need thousands of distinct friend cards to make a dent in
    /// it, and holding thousands of friend cards is a different problem.
    ///
    /// Only ever call this after the caller's token has authorized, for the
    /// reason spelled out on `check_rate_limit`.
    fn check_presence_rate_limit(&self, access: &FamilyAccess) -> Result<(), ApiError> {
        let now = Instant::now();
        let charged =
            self.charge_family_buckets(access, now, |buckets| buckets.try_take_presence(now));
        if let Err((scope, retry_after_secs)) = charged {
            return Err(reject_rate_limited(&access.token, scope, retry_after_secs));
        }
        let global_retry_after = {
            let mut global = self.rate_global.lock().unwrap_or_else(|e| e.into_inner());
            if global.try_take(1.0, now) {
                None
            } else {
                Some(global.retry_after_secs(1.0))
            }
        };
        if let Some(retry_after_secs) = global_retry_after {
            return Err(reject_rate_limited(
                &access.token,
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

/// Which deposit-class ceiling an admission ran into.
///
/// The distinction is the whole point of reporting it: "your own share is
/// full" is a condition the depositor caused and can fix (stop posting, or
/// wait for its own rows to age out), while "the family's deposit half is
/// full" is a condition other depositors caused, which this one can do
/// nothing about but wait through. Both are strictly different from the
/// family mailbox genuinely being full, which is what
/// [`QuotaInsertResult::QuotaExceeded`] means and is the only one of the
/// three the family's own devices can ever see.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepositShareScope {
    /// This depositor alone is at [`DEPOSIT_QUOTA_PER_DEPOSITOR_PERCENT`].
    Depositor,
    /// Deposit-class rows together are at [`DEPOSIT_QUOTA_TOTAL_PERCENT`].
    AllDepositors,
}

impl DepositShareScope {
    /// Stable wire discriminant for the structured API error, so a client can
    /// tell the two apart without parsing prose.
    fn code(self) -> &'static str {
        match self {
            DepositShareScope::Depositor => "depositor_share_exceeded",
            DepositShareScope::AllDepositors => "deposit_share_exceeded",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuotaInsertResult {
    Stored {
        id: i64,
    },
    QuotaExceeded {
        usage_bytes: u64,
    },
    /// A deposit-class post that the family mailbox has room for, but that
    /// its depositor's share (or the deposit-class share as a whole) does
    /// not. Deliberately distinct from [`QuotaInsertResult::QuotaExceeded`]:
    /// the mailbox is not full, so telling the depositor it is would be a
    /// lie, and would send the family looking for a backlog to drain that
    /// draining would not fix. Member-class posts can never produce this.
    DepositShareExceeded {
        scope: DepositShareScope,
        /// Bytes already stored under the ceiling that rejected this post.
        usage_bytes: u64,
        /// The ceiling itself.
        share_bytes: u64,
    },
    /// A row with this `(family_token, msg_id)` already exists carrying
    /// *different* sealed bytes. The stored (first) row is authoritative and
    /// is left untouched; this post was NOT stored. See [`InsertOutcome`] for
    /// why a differing-content re-post is not treated as a dedupe success.
    MsgIdConflict,
}

/// Outcome of a plain (non-quota) envelope insert.
///
/// `msg_id` is a random public id an author generates and mesh headers carry
/// in the clear, so any party can observe one in flight. The relay is a dumb
/// content-agnostic mailbox and cannot tell which of two posts claiming one
/// `msg_id` is authentic. It therefore keeps whichever content it stored
/// first and refuses to overwrite it: a re-post carrying *identical* sealed
/// bytes is a genuine idempotent dedupe (receipt retries, envelope
/// re-uploads) and returns [`InsertOutcome::Stored`] naming the existing row;
/// a re-post carrying *different* bytes under the same id is a distinct
/// [`InsertOutcome::MsgIdConflict`] outcome, so the caller learns its content
/// was not stored rather than mistaking the first row for its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InsertOutcome {
    Stored {
        id: i64,
    },
    /// The id already holds different content; the stored row is unchanged and
    /// this post was not stored.
    MsgIdConflict,
}

impl InsertOutcome {
    /// The stored row id, or panic. Test convenience for the many call sites
    /// that expect a fresh insert to have stored a row.
    #[cfg(test)]
    fn stored_id(&self) -> i64 {
        match self {
            InsertOutcome::Stored { id } => *id,
            InsertOutcome::MsgIdConflict => panic!("expected a stored row, got a msg_id conflict"),
        }
    }
}

/// A provisioned (hosted / Shore Pass) family, stored in the `families`
/// table. Static env-var tokens (`CRUISEMESH_RELAY_TOKENS`) never appear
/// here — they behave as implicit always-active families.
#[derive(Clone, Debug, PartialEq)]
pub struct FamilyRow {
    pub token: String,
    /// Opaque, stable handle for this family, minted at provisioning and
    /// never changed afterwards (`FAMILY_ID_PREFIX`). The token is not a
    /// stable identity — `POST /family/rotate` replaces it — so anything that
    /// must survive a rotation keys on this instead: the rate-limit buckets,
    /// the per-family WebSocket semaphore, and operator addressing of
    /// `/admin/families/{id}`.
    pub family_id: String,
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
    /// WP5 rotation authority: the Ed25519 public key, 32 raw bytes, that is
    /// allowed to sign this family's token rotations. `None` until the first
    /// rotation registers one (trust on first rotation) — see
    /// `rotate_family_token` for what that costs and where it is bounded.
    pub rotation_pk: Option<Vec<u8>>,
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

/// What `RelayStore::rotate_family_token` did (`specs/multi-device-v1.md`
/// §10 step 2).
#[derive(Clone, Debug, PartialEq)]
pub enum FamilyRotation {
    /// The family and every row it owned now answer to the new token.
    Rotated {
        family: FamilyRow,
        /// Envelopes carried across — the rotate-then-drain figure. Nothing a
        /// sibling had not fetched was dropped to make the rotation happen,
        /// and this is the count that says so out loud.
        envelopes_moved: u64,
    },
    /// The new token is already this family's, so a previous call succeeded
    /// and its answer was lost. Reported as success: a client retrying after a
    /// dropped response must converge, not be told it is too late.
    AlreadyRotated { family: FamilyRow },
    /// Neither credential names a provisioned family. Reached when a
    /// static-allowlist deployment tries to rotate (there is no row to
    /// re-key), or when a rotation is attempted twice from a stale token.
    UnknownFamily,
    /// Somebody else already holds the proposed token, as their member or
    /// their deposit credential.
    TokenTaken,
    /// The request did not carry this family's rotation authority: either the
    /// family already registered a key and this is not it (or not a valid
    /// signature by it), or no key is registered yet and the presented
    /// signature does not verify under the presented key. Holding the member
    /// token is not enough, which is the entire point — the revoked device
    /// holds it too.
    Unauthorized,
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
        // WP5: rotation authority + the stable family id. Additive columns
        // plus a backfill of the id, so an already-deployed database gains
        // both on the next restart with no operator step and no downtime.
        migrate_families_rotation_authority(&conn)?;
        // Per-depositor quota accounting: one additive `envelopes` column
        // whose constant default reads every pre-existing row as member
        // class, so a deployed database gains the column on the next restart
        // without a backfill, without a row rewrite, and with no envelope at
        // risk.
        migrate_envelopes_depositor(&conn)?;
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

    // These are the independently validated persisted-envelope columns. A
    // request struct would obscure ownership at the HTTP/store boundary while
    // providing no shared invariant beyond the checks immediately below.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_envelope(
        &self,
        family_token: &str,
        msg_id: Vec<u8>,
        hop_ttl: u8,
        recipient_hint: Vec<u8>,
        sealed: Vec<u8>,
        expiry_ms: i64,
        created_at_ms: i64,
    ) -> Result<InsertOutcome, String> {
        let expiry_ms = Self::effective_expiry(created_at_ms, expiry_ms);
        let mut conn = self.conn.lock().expect("relay store mutex poisoned");
        let tx = conn.transaction().map_err(|e| e.to_string())?;

        // A conflict on `(family_token, msg_id)` is resolved by *content*, not
        // by presence. The stored row is the first writer and the relay cannot
        // know which post is authentic, so it never overwrites sealed bytes.
        // An identical re-post is a dedupe: keep the row, take the longer hop
        // budget / later expiry, return its id. A re-post carrying different
        // sealed bytes is a genuine conflict: leave the stored row *entirely*
        // unchanged (not even the hop/expiry bump) and report it, so the
        // caller does not mistake someone else's row for its own delivered one.
        let existing: Option<(i64, Vec<u8>)> = tx
            .query_row(
                "SELECT id, sealed FROM envelopes WHERE family_token = ?1 AND msg_id = ?2 LIMIT 1",
                params![family_token, msg_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;

        if let Some((id, stored_sealed)) = existing {
            if stored_sealed != sealed {
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(InsertOutcome::MsgIdConflict);
            }
            tx.execute(
                "UPDATE envelopes SET
                    hop_ttl = MAX(hop_ttl, ?3),
                    expiry_ms = MAX(expiry_ms, ?4)
                 WHERE family_token = ?1 AND msg_id = ?2",
                params![family_token, msg_id, hop_ttl as i64, expiry_ms],
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(InsertOutcome::Stored { id });
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
        Ok(InsertOutcome::Stored { id })
    }

    /// Atomically admit a new row under the per-family sealed-byte quota and,
    /// for a deposit-class post, its depositor's fair share of that quota.
    /// The dedupe check, usage calculation, optional expiry pruning, and insert
    /// all run while holding one store lock and one SQLite transaction.
    ///
    /// `depositor` is the accounting identity the row is charged to: `None`
    /// for a member-class post (the family's own devices, which keep the full
    /// quota and are the only class that can be told the mailbox is full),
    /// `Some(key)` for a deposit-class one, where `key` is the deposit
    /// credential that authorized it. The stored column is the *first*
    /// writer's: a dedupe re-post never re-attributes an existing row, for
    /// the same first-writer reason its sealed bytes are never rewritten.
    // Kept parallel to `insert_envelope`: quota admission must receive the
    // same individual stored columns plus its family-level quota.
    #[allow(clippy::too_many_arguments)]
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
        depositor: Option<&str>,
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

        // Resolve a same-id re-post by content, exactly as `insert_envelope`
        // does: an identical re-post dedupes (and is exempt from the quota
        // check below — it stores no new bytes), while a re-post carrying
        // different sealed bytes is a conflict that leaves the stored row
        // untouched and is reported rather than silently swallowed as success.
        let existing: Option<(i64, Vec<u8>)> = tx
            .query_row(
                "SELECT id, sealed FROM envelopes WHERE family_token = ?1 AND msg_id = ?2 LIMIT 1",
                params![family_token, msg_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some((id, stored_sealed)) = existing {
            if stored_sealed != sealed {
                tx.commit().map_err(|e| e.to_string())?;
                return Ok(QuotaInsertResult::MsgIdConflict);
            }
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
        // Prune-then-recheck, exactly as before, but now driven by whichever
        // of the ceilings actually rejected: expired rows free space under
        // every one of them, and a deposit share that is only full of rows
        // past their (much shorter, MAX_DEPOSIT_RETENTION_MS) life must not
        // reject a post the prune would have made room for.
        let mut usage = quota_usage_on(&tx, family_token, depositor)?;
        let mut rejection = quota_rejection(&usage, candidate_bytes, family_quota_bytes, depositor);
        if rejection.is_some() {
            prune_expired_on(&tx, created_at_ms)?;
            usage = quota_usage_on(&tx, family_token, depositor)?;
            rejection = quota_rejection(&usage, candidate_bytes, family_quota_bytes, depositor);
        }
        if let Some(rejection) = rejection {
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(rejection);
        }

        let id = tx
            .query_row(
                "INSERT INTO envelopes
                    (family_token, msg_id, hop_ttl, recipient_hint, sealed, expiry_ms,
                     created_at_ms, depositor)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 RETURNING id",
                params![
                    family_token,
                    msg_id,
                    hop_ttl as i64,
                    recipient_hint,
                    sealed,
                    expiry_ms,
                    created_at_ms,
                    depositor.unwrap_or(MEMBER_DEPOSITOR),
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
    ///
    /// A same-`(family_token, msg_id)` re-post is resolved by *content*, the
    /// same rule `insert_envelope` and `insert_envelope_with_quota` enforce
    /// (contract invariant DEDUP-01): an identical re-post dedupes (longer hop
    /// budget / later expiry win), while a re-post carrying different sealed
    /// bytes leaves the stored first-writer row entirely untouched. This path
    /// has no per-row outcome to return, so a differing-content row is a no-op
    /// rather than a signalled conflict — but it likewise never overwrites the
    /// stored bytes, so no ingest path can silently replace one msg_id's
    /// content with another's.
    pub fn insert_envelopes_batch(&self, rows: &[EnvelopeBatchRow]) -> Result<(), String> {
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
            let mut select = tx
                .prepare(
                    "SELECT sealed FROM envelopes
                     WHERE family_token = ?1 AND msg_id = ?2 LIMIT 1",
                )
                .map_err(|e| e.to_string())?;
            let mut insert = tx
                .prepare(
                    "INSERT INTO envelopes
                        (family_token, msg_id, hop_ttl, recipient_hint, sealed, expiry_ms, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(|e| e.to_string())?;
            let mut bump = tx
                .prepare(
                    "UPDATE envelopes SET
                        hop_ttl = MAX(hop_ttl, ?3),
                        expiry_ms = MAX(expiry_ms, ?4)
                     WHERE family_token = ?1 AND msg_id = ?2",
                )
                .map_err(|e| e.to_string())?;
            for (family, msg_id, hop_ttl, hint, sealed, expiry_ms, created_at_ms) in rows {
                let expiry_ms = Self::effective_expiry(*created_at_ms, *expiry_ms);
                let stored_sealed: Option<Vec<u8>> = select
                    .query_row(params![family, msg_id], |row| row.get(0))
                    .optional()
                    .map_err(|e| e.to_string())?;
                match stored_sealed {
                    // Differing content under an existing id: leave the stored
                    // first-writer row untouched, never overwrite (DEDUP-01).
                    Some(stored) if stored != *sealed => {}
                    // Identical re-post: idempotent dedupe, take the longer
                    // hop budget / later expiry.
                    Some(_) => {
                        bump.execute(params![family, msg_id, *hop_ttl as i64, expiry_ms])
                            .map_err(|e| e.to_string())?;
                    }
                    // New id: store it.
                    None => {
                        insert
                            .execute(params![
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
        let placeholders = std::iter::repeat_n("?", ids.len())
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

    pub fn replace_push_registration(
        &self,
        family_token: &str,
        device_token: &str,
        hints: &[Vec<u8>],
        updated_ms: i64,
    ) -> Result<(), String> {
        if hints.is_empty() || hints.len() > MAX_PUSH_HINTS {
            return Err(format!(
                "push registration requires 1..={MAX_PUSH_HINTS} hints"
            ));
        }
        let mut conn = self.conn.lock().expect("relay store mutex poisoned");
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO push_registrations (family_token, device_token, updated_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(family_token, device_token) DO UPDATE SET
                updated_ms = excluded.updated_ms",
            params![family_token, device_token, updated_ms],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM push_registration_hints
             WHERE family_token = ?1 AND device_token = ?2",
            params![family_token, device_token],
        )
        .map_err(|e| e.to_string())?;
        {
            let mut insert = tx
                .prepare(
                    "INSERT OR IGNORE INTO push_registration_hints
                        (family_token, device_token, hint)
                     VALUES (?1, ?2, ?3)",
                )
                .map_err(|e| e.to_string())?;
            for hint in hints {
                if hint.len() != RECIPIENT_HINT_LEN {
                    return Err(format!("push hint must be {RECIPIENT_HINT_LEN} bytes"));
                }
                insert
                    .execute(params![family_token, device_token, hint])
                    .map_err(|e| e.to_string())?;
            }
        }
        tx.commit().map_err(|e| e.to_string())
    }

    pub fn push_device_tokens_for_hint(
        &self,
        family_token: &str,
        hint: &[u8],
        now_ms: i64,
    ) -> Result<Vec<String>, String> {
        let fresh_after = now_ms.saturating_sub(PUSH_REGISTRATION_RETENTION_MS);
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let mut statement = conn
            .prepare(
                "SELECT DISTINCT r.device_token
                 FROM push_registrations r
                 JOIN push_registration_hints h
                   ON h.family_token = r.family_token
                  AND h.device_token = r.device_token
                 WHERE r.family_token = ?1 AND h.hint = ?2 AND r.updated_ms >= ?3
                 ORDER BY r.device_token",
            )
            .map_err(|e| e.to_string())?;
        let rows = statement
            .query_map(params![family_token, hint, fresh_after], |row| row.get(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<String>, _>>()
            .map_err(|e| e.to_string())
    }

    pub fn remove_push_device_token(&self, device_token: &str) -> Result<u64, String> {
        let mut conn = self.conn.lock().expect("relay store mutex poisoned");
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM push_registration_hints WHERE device_token = ?1",
            params![device_token],
        )
        .map_err(|e| e.to_string())?;
        let deleted = tx
            .execute(
                "DELETE FROM push_registrations WHERE device_token = ?1",
                params![device_token],
            )
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
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
        // `family_id` and `rotation_pk` are deliberately absent from the
        // conflict clause. Re-provisioning is a webhook retry or a renewal of
        // a family that already exists, and neither may disturb its identity:
        // a fresh id would orphan the rate buckets and the operator's saved
        // handle, and clearing the rotation key would hand a revoked device a
        // second shot at trust-on-first-rotation.
        conn.execute(
            "INSERT INTO families
                (token, status, plan, quota_bytes, created_ms, expires_ms, note,
                 deposit_token, family_id)
             VALUES (?1, 'active', ?2, ?3, ?4, ?5, ?6, ?7, ?8)
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
                mint_family_id(),
            ],
        )
        .map_err(|e| e.to_string())?;
        get_family_on(&conn, token)?.ok_or_else(|| "family vanished after upsert".to_string())
    }

    pub fn get_family(&self, token: &str) -> Result<Option<FamilyRow>, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        get_family_on(&conn, token)
    }

    /// Resolve an operator-supplied handle — stable `family_id` or current
    /// member token — to its family row. See `resolve_family_on` for the
    /// resolution order and why both spellings are supported.
    pub fn resolve_family(&self, id_or_token: &str) -> Result<Option<FamilyRow>, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        resolve_family_on(&conn, id_or_token)
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
            .prepare(&format!(
                "SELECT {FAMILY_COLUMNS},
                        COALESCE(SUM(LENGTH(e.sealed)), 0), COUNT(e.id)
                 FROM families f
                 LEFT JOIN envelopes e ON e.family_token = f.token
                 WHERE (?1 IS NULL OR f.status = ?1)
                 GROUP BY f.token
                 ORDER BY f.created_ms ASC, f.token ASC
                 LIMIT ?2 OFFSET ?3"
            ))
            .map_err(|e| e.to_string())?;
        let families = stmt
            .query_map(params![status_filter, limit as i64, offset as i64], |row| {
                Ok(FamilyUsage {
                    family: family_row_from(row)?,
                    usage_bytes: row.get::<_, i64>(10)? as u64,
                    envelope_count: row.get::<_, i64>(11)? as u64,
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

    /// Revoke a family and purge everything it stored (envelopes, presence,
    /// and APNs registrations). Returns `false` if no such family existed.
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
        tx.execute(
            "DELETE FROM push_registration_hints WHERE family_token = ?1",
            params![token],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "DELETE FROM push_registrations WHERE family_token = ?1",
            params![token],
        )
        .map_err(|e| e.to_string())?;
        let deleted = tx
            .execute("DELETE FROM families WHERE token = ?1", params![token])
            .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(deleted > 0)
    }

    /// **Re-key a family in place** (`specs/multi-device-v1.md` §10 step 2).
    ///
    /// Every table in this schema is scoped by `family_token`, which is
    /// exactly the hole §10.2 names: a revoked device holding the old token
    /// can fetch — and ack, which deletes — its siblings' rows indefinitely,
    /// because the token is the whole of the authorization. Rotating the token
    /// is what withdraws that, and it is why this is the one family credential
    /// operation a *client* may perform on itself rather than an operator.
    ///
    /// **Rotate, then drain.** Every row moves with the family in one
    /// transaction: envelopes keep their `id`, their `recipient_hint` and their
    /// place in the fetch order, so a sibling that was offline for the whole
    /// ceremony fetches exactly what it would have fetched, from exactly the
    /// cursor it held, the moment it learns the new token. A rotation that
    /// dropped un-fetched mail to cut off the thief would be the rotation
    /// losing mail, which is the one thing it may not do — so nothing here
    /// deletes.
    ///
    /// The old credentials — member *and* its derived deposit token — stop
    /// resolving the instant this commits. That strands, until the rotation
    /// gossips to them, every contact still holding a friend card minted from
    /// the old member token. That cost is accepted deliberately and is not
    /// widened here: §10 says the propagation window is bounded and the
    /// old-card repair path is the fallback, and a grace window that kept the
    /// old credential alive as deposit-class would hand the revoked device a
    /// capability §10 does not grant it.
    ///
    /// **Authority is a signature, not the bearer token.** Possession of the
    /// member token cannot be what authorizes a rotation, because the device
    /// this ceremony exists to evict is holding that exact token — authorizing
    /// on possession would let the thief re-key the family out from under its
    /// owner, and do it first. So the caller must also sign
    /// `rotation_signed_bytes(current_token, new_token)` under the family's
    /// registered rotation key, and that check happens *here*, inside the
    /// transaction: reading the stored key and writing the re-keyed row in one
    /// atomic step is what stops two concurrent rotations from both passing a
    /// check made against a row that then changed underneath them.
    ///
    /// Registration is trust on first rotation, and **only a call that
    /// actually re-keys may register**. A family with no key yet registers
    /// whichever key signs its first *rotation* validly; from then on that key
    /// is the only authority, and a presented key that differs is refused
    /// without its signature being examined. The consequences of that are
    /// honest and documented on the `rotate_family` handler.
    ///
    /// The "actually re-keys" half is load-bearing. The `already_rotated`
    /// branches below change nothing, so a caller that reaches one of them and
    /// were allowed to register would claim the authority for free: present
    /// the family's own live token as *both* credentials, sign that, and walk
    /// away as the family's permanent rotation key having rotated nothing.
    /// Anyone holding the member token could do it — including the device this
    /// ceremony exists to evict, which is holding it by definition — and every
    /// genuine rotation afterwards would be [`FamilyRotation::Unauthorized`],
    /// leaving §10 step 2 permanently dead for that family. So a `Register`
    /// verdict on an `already_rotated` call is refused. Nothing legitimate is
    /// lost: the only honest way to reach these branches is a retry after a
    /// rotation that landed, and that rotation is exactly what registered the
    /// key, so the retry arrives on the `Accepted` path.
    ///
    /// Idempotent by construction, because a client that loses the response
    /// must be able to ask again: presenting the *new* token (which is what a
    /// retry after a lost response can do, and all it can do) reports
    /// [`FamilyRotation::AlreadyRotated`] rather than re-rotating or failing.
    /// That path is gated by the same signature check — a retry that anyone
    /// could make by replaying the new token would be a way to learn a
    /// rotated family's current credentials from the outside.
    ///
    /// **Push registrations are purged rather than carried.** A push
    /// registration is a per-device wake channel: carrying one across would
    /// leave the revoked device's APNs token still being woken for the
    /// rotated family's mail, which is a small but real leak of the family's
    /// activity to a device that was just cut off — and the relay cannot tell
    /// the revoked device's registration from a sibling's. Siblings re-register
    /// on their next round, so the cost is one round of notification latency.
    /// Envelopes and presence still *move*: rotate-then-drain may not lose
    /// un-fetched mail, and presence is the family's own announcement about
    /// itself, not a per-device channel.
    ///
    /// A WebSocket the revoked device already had open is not torn down, and
    /// does not need to be: every query that socket makes is scoped by the
    /// token it authorized under, which now names zero rows, and every
    /// broadcast is published under the new token. It goes inert rather than
    /// being disconnected — the same outcome, without a second mechanism to
    /// keep correct.
    pub fn rotate_family_token(
        &self,
        current_token: &str,
        new_token: &str,
        authority: &RotationAuthority,
    ) -> Result<FamilyRotation, String> {
        let mut conn = self.conn.lock().expect("relay store mutex poisoned");
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        // Which row this call is about, and whether the work is already done.
        // A caller presenting the new token is either retrying a rotation
        // whose answer it lost or asking to rotate a token to itself; both are
        // the same "already there" answer.
        let (row, already_rotated) = match get_family_on(&tx, current_token)? {
            Some(row) if current_token != new_token => (row, false),
            Some(row) => (row, true),
            // Not the current token, so either the caller already rotated and
            // lost the answer, or this family is not in the table at all (a
            // static env-allowlist deployment). The handler tells those two
            // apart for the caller. No row means no authority to check
            // against and nothing to leak by saying so.
            None => match get_family_on(&tx, new_token)? {
                Some(row) => (row, true),
                None => return Ok(FamilyRotation::UnknownFamily),
            },
        };

        // Authorization before validation: a caller without the family's
        // rotation authority learns nothing about whether its proposed token
        // was available, only that it may not rotate.
        let message = rotation_signed_bytes(current_token, new_token);
        match check_rotation_authority(row.rotation_pk.as_deref(), &message, authority) {
            RotationAuthorityCheck::Refused => return Ok(FamilyRotation::Unauthorized),
            RotationAuthorityCheck::Accepted => {}
            // A call that re-keys nothing may not claim the authority. See the
            // "actually re-keys" paragraph above: this is the difference
            // between trust-on-first-*rotation* and trust-on-first-*ask*, and
            // the second one hands the family's rotation key to whoever holds
            // the member token — the revoked device included.
            RotationAuthorityCheck::Register if already_rotated => {
                return Ok(FamilyRotation::Unauthorized);
            }
            RotationAuthorityCheck::Register => {
                // Trust on first rotation. Written inside the transaction, so
                // it lands only if this call goes on to commit — a rotation
                // refused below for a taken token leaves the family exactly as
                // unregistered as it was, rather than burning its one
                // first-rotation slot on an attempt that did nothing.
                tx.execute(
                    "UPDATE families SET rotation_pk = ?2 WHERE token = ?1",
                    params![row.token, authority.public_key.as_slice()],
                )
                .map_err(|e| e.to_string())?;
                info!(
                    family = %token_prefix(&row.token),
                    "rotation authority registered on first rotation"
                );
            }
        }

        if already_rotated {
            let family = get_family_on(&tx, &row.token)?
                .ok_or_else(|| "family vanished during rotation".to_string())?;
            // Committed rather than rolled back because a first-rotation
            // registration on this path is a real write that must stick.
            tx.commit().map_err(|e| e.to_string())?;
            return Ok(FamilyRotation::AlreadyRotated { family });
        }

        let new_deposit = deposit_token_for(new_token);
        // Both halves of the pair must be free. A member token that collided
        // with another family's deposit token — or vice versa — would make the
        // `WHERE token = ? OR deposit_token = ?` auth lookup ambiguous, which
        // is the one thing CP4's class discipline may never become.
        if get_family_by_credential_on(&tx, new_token)?.is_some()
            || get_family_by_credential_on(&tx, &new_deposit)?.is_some()
        {
            return Ok(FamilyRotation::TokenTaken);
        }
        let envelopes_moved = tx
            .execute(
                "UPDATE envelopes SET family_token = ?2 WHERE family_token = ?1",
                params![current_token, new_token],
            )
            .map_err(|e| e.to_string())? as u64;
        // Presence moves: it is the family's own announcement about itself,
        // and a sibling that slept through the ceremony should not look
        // offline to its contacts afterwards.
        tx.execute(
            "UPDATE presence SET family_token = ?2 WHERE family_token = ?1",
            params![current_token, new_token],
        )
        .map_err(|e| e.to_string())?;
        // Push registrations do not. See the "purged rather than carried"
        // paragraph above: each row is one device's wake channel, the revoked
        // device's is indistinguishable from a sibling's, and a sibling
        // re-registers on its next round.
        for table in ["push_registration_hints", "push_registrations"] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE family_token = ?1"),
                params![current_token],
            )
            .map_err(|e| e.to_string())?;
        }
        tx.execute(
            "UPDATE families SET token = ?2, deposit_token = ?3 WHERE token = ?1",
            params![current_token, new_token, new_deposit],
        )
        .map_err(|e| e.to_string())?;
        let family = get_family_on(&tx, new_token)?
            .ok_or_else(|| "family vanished during rotation".to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
        Ok(FamilyRotation::Rotated {
            family,
            envelopes_moved,
        })
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
    // Also backfilled at startup (`migrate_families_rotation_authority`).
    // Unlike the deposit token this one cannot be re-derived — it is random —
    // so a torn mid-migration read yields an empty id, and every consumer
    // treats empty as "no stable id yet" and falls back to the token. That is
    // exactly the static-allowlist behavior, which is already correct.
    let family_id = row.get::<_, Option<String>>(8)?.unwrap_or_default();
    Ok(FamilyRow {
        token,
        family_id,
        status: row.get(1)?,
        plan: row.get(2)?,
        quota_bytes: row.get::<_, Option<i64>>(3)?.map(|q| q as u64),
        created_ms: row.get(4)?,
        expires_ms: row.get(5)?,
        note: row.get(6)?,
        deposit_token,
        rotation_pk: row.get::<_, Option<Vec<u8>>>(9)?,
    })
}

/// The column list every `families` read shares, in the exact order
/// `family_row_from` indexes. One constant so a new column can never be added
/// to one query and forgotten in another, which would silently shift every
/// index after it.
const FAMILY_COLUMNS: &str = "token, status, plan, quota_bytes, created_ms, expires_ms, note, \
                              deposit_token, family_id, rotation_pk";

fn get_family_on(conn: &Connection, token: &str) -> Result<Option<FamilyRow>, String> {
    conn.query_row(
        &format!("SELECT {FAMILY_COLUMNS} FROM families WHERE token = ?1"),
        params![token],
        family_row_from,
    )
    .optional()
    .map_err(|e| e.to_string())
}

/// Resolve an `/admin/families/{id}` path segment, which may be *either* the
/// family's stable `family_id` or its current member token.
///
/// **Resolution order is family id first, then token.** Both namespaces are
/// prefixed (`FAMILY_ID_PREFIX` vs `cmfam1-` / `DEPOSIT_TOKEN_PREFIX`) and
/// provisioning refuses a token wearing the family-id prefix, so no string can
/// legitimately be both and the order is a tie-break that never fires. It is
/// still written down rather than left to chance: if a pre-prefix legacy token
/// somehow matched an id, the id wins, because the id is the identifier this
/// server minted and the token is one a caller chose.
///
/// Both spellings are supported because both have a real caller. The
/// pass-issuing web flow stored a token when it provisioned, and that token is
/// still how it addresses the family, so `{token}` may not break. But a token
/// stops resolving the moment the family rotates, and an operator who has to
/// find that family afterwards has nothing else to go on — which is what the
/// stable id is for.
fn resolve_family_on(conn: &Connection, id_or_token: &str) -> Result<Option<FamilyRow>, String> {
    let by_id = conn
        .query_row(
            &format!("SELECT {FAMILY_COLUMNS} FROM families WHERE family_id = ?1"),
            params![id_or_token],
            family_row_from,
        )
        .optional()
        .map_err(|e| e.to_string())?;
    match by_id {
        Some(row) => Ok(Some(row)),
        None => get_family_on(conn, id_or_token),
    }
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
            &format!(
                "SELECT {FAMILY_COLUMNS} FROM families WHERE token = ?1 OR deposit_token = ?1
                 ORDER BY (token = ?1) DESC LIMIT 1"
            ),
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

/// The `envelopes.depositor` value that means "posted by the family itself".
///
/// The empty string rather than SQL NULL, deliberately: it is a valid
/// `DEFAULT` for `ALTER TABLE ... ADD COLUMN ... NOT NULL`, which is what
/// lets the migration land on a live database without rewriting a single
/// existing row, and it keeps `depositor <> ''` a plain indexable predicate
/// instead of three-valued-logic. A deposit credential can never collide
/// with it — every one of them carries [`DEPOSIT_TOKEN_PREFIX`].
const MEMBER_DEPOSITOR: &str = "";

/// The three sealed-byte figures an admission decision needs.
///
/// `deposit_bytes` and `depositor_bytes` are only populated for a
/// deposit-class post; a member-class one is checked against the family
/// total alone, so the two extra `SUM()`s are not run at all for it and the
/// hot path for a family's own devices costs exactly what it did before.
struct QuotaUsage {
    family_bytes: u64,
    deposit_bytes: u64,
    depositor_bytes: u64,
}

fn sealed_bytes_sum_on(
    conn: &Connection,
    sql: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<u64, String> {
    let total: Option<Option<i64>> = conn
        .query_row(sql, args, |row| row.get(0))
        .optional()
        .map_err(|e| e.to_string())?;
    Ok(total.flatten().unwrap_or(0) as u64)
}

fn quota_usage_on(
    conn: &Connection,
    family_token: &str,
    depositor: Option<&str>,
) -> Result<QuotaUsage, String> {
    let family_bytes = family_sealed_bytes_on(conn, family_token)?;
    let Some(depositor) = depositor else {
        return Ok(QuotaUsage {
            family_bytes,
            deposit_bytes: 0,
            depositor_bytes: 0,
        });
    };
    let deposit_bytes = sealed_bytes_sum_on(
        conn,
        "SELECT SUM(LENGTH(sealed)) FROM envelopes
         WHERE family_token = ?1 AND depositor <> ''",
        &[&family_token],
    )?;
    let depositor_bytes = sealed_bytes_sum_on(
        conn,
        "SELECT SUM(LENGTH(sealed)) FROM envelopes
         WHERE family_token = ?1 AND depositor = ?2",
        &[&family_token, &depositor],
    )?;
    Ok(QuotaUsage {
        family_bytes,
        deposit_bytes,
        depositor_bytes,
    })
}

/// Decide whether a candidate row is admissible, and if not, which ceiling
/// said no.
///
/// Order is the reporting order, not just an evaluation order. The family
/// quota is checked first because when the mailbox really is full that is
/// the true and actionable answer for anyone — a share rejection would send
/// the depositor away waiting for space that draining the family's backlog
/// is what actually frees. Within the deposit ceilings the depositor's own
/// share comes first: it is the tighter bound and the more specific
/// diagnosis, so a depositor is only ever told "the deposit share is full"
/// when other depositors are genuinely the reason.
fn quota_rejection(
    usage: &QuotaUsage,
    candidate_bytes: u64,
    family_quota_bytes: u64,
    depositor: Option<&str>,
) -> Option<QuotaInsertResult> {
    if usage.family_bytes.saturating_add(candidate_bytes) > family_quota_bytes {
        return Some(QuotaInsertResult::QuotaExceeded {
            usage_bytes: usage.family_bytes,
        });
    }
    depositor?;
    let per_depositor = deposit_per_depositor_share_bytes(family_quota_bytes);
    if usage.depositor_bytes.saturating_add(candidate_bytes) > per_depositor {
        return Some(QuotaInsertResult::DepositShareExceeded {
            scope: DepositShareScope::Depositor,
            usage_bytes: usage.depositor_bytes,
            share_bytes: per_depositor,
        });
    }
    let total_share = deposit_total_share_bytes(family_quota_bytes);
    if usage.deposit_bytes.saturating_add(candidate_bytes) > total_share {
        return Some(QuotaInsertResult::DepositShareExceeded {
            scope: DepositShareScope::AllDepositors,
            usage_bytes: usage.deposit_bytes,
            share_bytes: total_share,
        });
    }
    None
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
    let has_column = table_has_column(conn, "families", "deposit_token")?;
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

/// Whether a table already has a given column. Pulled out because every
/// additive migration here starts with the same question, and `PRAGMA
/// table_info` returning the name in column 1 is exactly the kind of detail
/// that gets copied wrong the third time.
///
/// `table` is interpolated into the pragma because SQLite does not bind
/// identifiers; every caller passes a literal from this file, never anything
/// request-derived.
fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?;
    let present = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .any(|name| name == column);
    Ok(present)
}

/// WP5 startup migration, following `migrate_families_deposit_token`'s
/// pattern exactly (idempotent, self-applying, safe on a deployed database,
/// no operator step):
///
/// 1. `ALTER TABLE families ADD COLUMN family_id` / `rotation_pk` when
///    missing. `SCHEMA`'s `CREATE TABLE IF NOT EXISTS` is a no-op on an
///    existing database and cannot add columns, so this is the only path by
///    which a live relay grows them.
/// 2. Backfill `family_id` for every row that has none, one freshly minted id
///    per family. Existing families therefore gain a stable handle
///    immediately, which is what lets the rate limiter and the WS semaphore
///    key on it for *all* families rather than only ones provisioned from
///    here on.
/// 3. A UNIQUE index on `family_id`, created here rather than in `SCHEMA`
///    because on a pre-WP5 database `SCHEMA` runs before the column exists.
///
/// `rotation_pk` is deliberately **not** backfilled: NULL is its meaningful
/// value, and it means "no rotation authority registered yet". Every family
/// that predates this change is in that state, and the first rotation each
/// one performs is what registers its key.
fn migrate_families_rotation_authority(conn: &Connection) -> Result<(), String> {
    if !table_has_column(conn, "families", "family_id")? {
        conn.execute("ALTER TABLE families ADD COLUMN family_id TEXT", [])
            .map_err(|e| e.to_string())?;
    }
    if !table_has_column(conn, "families", "rotation_pk")? {
        conn.execute("ALTER TABLE families ADD COLUMN rotation_pk BLOB", [])
            .map_err(|e| e.to_string())?;
    }
    let missing: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT token FROM families WHERE family_id IS NULL OR family_id = ''")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    for token in &missing {
        conn.execute(
            "UPDATE families SET family_id = ?2 WHERE token = ?1",
            params![token, mint_family_id()],
        )
        .map_err(|e| e.to_string())?;
    }
    if !missing.is_empty() {
        info!(
            families = missing.len(),
            "WP5 migration: minted stable family ids for existing families"
        );
    }
    conn.execute_batch(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_families_family_id
             ON families(family_id);",
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Additive migration for per-depositor quota accounting, following the same
/// pattern as every schema change before it (idempotent, self-applying, safe
/// on a deployed database, no operator step):
///
/// 1. `ALTER TABLE envelopes ADD COLUMN depositor TEXT NOT NULL DEFAULT ''`
///    when the column is missing. `SCHEMA`'s `CREATE TABLE IF NOT EXISTS` is
///    a no-op on an existing database and cannot add columns, so this is the
///    only path by which a live relay grows it. A constant `DEFAULT` is what
///    makes `NOT NULL` legal in an `ADD COLUMN` at all, and it means SQLite
///    records the new column in the schema and synthesizes the default on
///    read: **no existing row is rewritten, moved, or deleted**, so no
///    envelope can be lost by applying this, and nothing needs backfilling
///    afterwards.
/// 2. An index on `(family_token, depositor)` so the two extra `SUM()`s a
///    deposit-class admission runs can seek their rows instead of walking
///    every row the family owns. Created here rather than in `SCHEMA`
///    because on a pre-migration database `SCHEMA` runs before the column
///    exists.
///
/// The migration default is deliberately **member class**
/// ([`MEMBER_DEPOSITOR`]) for every row that predates it, which is the only
/// safe reading of rows posted before the relay recorded who deposited them.
/// Guessing "deposit" instead would retroactively charge a family's existing
/// friend mail against a share that did not exist when it was posted, and
/// could put that family's friends over their ceiling the moment the process
/// restarts — rejecting posts on account of history rather than behavior.
/// Reading them as member class costs nothing: the family quota still bounds
/// the total exactly as it does today, and the misattribution ages out on
/// its own within [`MAX_DEPOSIT_RETENTION_MS`], the longest any such row can
/// still be alive.
fn migrate_envelopes_depositor(conn: &Connection) -> Result<(), String> {
    if !table_has_column(conn, "envelopes", "depositor")? {
        conn.execute(
            "ALTER TABLE envelopes ADD COLUMN depositor TEXT NOT NULL DEFAULT ''",
            [],
        )
        .map_err(|e| e.to_string())?;
        info!("migration: envelopes.depositor added; existing rows read as member class");
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_envelopes_family_depositor
             ON envelopes(family_token, depositor);",
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
    let push_floor = now_ms.saturating_sub(PUSH_REGISTRATION_RETENTION_MS);
    let deleted_push_hints = conn
        .execute(
            "DELETE FROM push_registration_hints
             WHERE EXISTS (
                 SELECT 1 FROM push_registrations r
                 WHERE r.family_token = push_registration_hints.family_token
                   AND r.device_token = push_registration_hints.device_token
                   AND r.updated_ms < ?1
             )",
            params![push_floor],
        )
        .map_err(|e| e.to_string())?;
    let deleted_push_registrations = conn
        .execute(
            "DELETE FROM push_registrations WHERE updated_ms < ?1",
            params![push_floor],
        )
        .map_err(|e| e.to_string())?;
    Ok((deleted + deleted_presence + deleted_push_hints + deleted_push_registrations) as u64)
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
        .route("/push/registrations", put(put_push_registration))
        // §10 step 2. Under `/family/` rather than `/admin/families/` on
        // purpose: this is the family acting on itself with its own
        // credential, not an operator acting on a family with theirs.
        .route("/family/rotate", post(rotate_family))
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

/// Parse `CRUISEMESH_RELAY_DEPOSIT_PRESENCE_WINDOW_SECS` (see `DEPLOY.md`
/// §10). `0` is rejected: a zero-length window is a division the bucket
/// cannot make, and an operator who wants presence off should set the query
/// allowance, not the window.
pub fn parse_presence_window_secs(raw: &str) -> Result<u64, String> {
    let value: u64 = raw
        .parse()
        .map_err(|_| format!("not a valid window in seconds: {raw:?}"))?;
    if value == 0 {
        return Err("presence window must be greater than 0 seconds".to_string());
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
    // A deposit credential's rows live at most MAX_DEPOSIT_RETENTION_MS: the
    // honest client always asks for 7 days, so only an abuser loses anything,
    // and what they lose is the ability to occupy family quota for the full
    // member-class 30-day ceiling. Clamped here rather than in the store so
    // the store keeps one retention rule and the class stays an HTTP-layer
    // concern.
    let expiry_ms_req = match access.class {
        TokenClass::Deposit => request
            .expiry_ms
            .min(now.saturating_add(MAX_DEPOSIT_RETENTION_MS)),
        TokenClass::Member => request.expiry_ms,
    };
    // Per-family quota override (hosted families) falls back to the server
    // default inside `authorize_family`; FR8 keeps the write off the reactor.
    let family_quota_bytes = access.quota_bytes;
    // The storage-accounting identity: None for the family's own devices
    // (full quota), the presented deposit credential for a friend-card post
    // (its share of that quota). See `FamilyAccess::depositor`.
    let insert_depositor = access.depositor.clone();
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
                insert_depositor.as_deref(),
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
        QuotaInsertResult::DepositShareExceeded {
            scope,
            usage_bytes,
            share_bytes,
        } => {
            // Logged at warn like the other rejections, but never with the
            // depositing credential itself (FR2): a deposit token is
            // semi-public and belongs in `families`, not in a log line. The
            // scope is what an operator actually needs — it says whether one
            // card is running hot or the family's friends collectively are.
            warn!(
                family = %token_prefix(&family_token),
                scope = ?scope,
                usage_bytes,
                share_bytes,
                quota_bytes = access.quota_bytes,
                "envelope rejected: deposit-class share exceeded (507)"
            );
            return Err(ApiError::deposit_share_exceeded(
                scope,
                usage_bytes,
                share_bytes,
                access.quota_bytes,
            ));
        }
        QuotaInsertResult::MsgIdConflict => {
            // Never log the msg_id or sealed bytes (FR2) — only the family
            // prefix, so an operator can correlate without the semi-public id
            // reaching the log.
            warn!(
                family = %token_prefix(&family_token),
                "envelope rejected: msg_id already holds different content (409)"
            );
            return Err(ApiError::msg_id_conflict());
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
        expiry_ms: RelayStore::effective_expiry(now, expiry_ms_req),
        created_at_ms: now,
    };
    // Fan-out for live WS subscribers. Lagging peers are dropped (module docs).
    let _ = state.tx.send(std::sync::Arc::new(BroadcastEnvelope {
        family_token: family_token.clone(),
        recipient_hint: encode_base64_field(&recipient_hint),
        envelope,
    }));

    // Apple suspends ordinary app WebSockets. If this relay has an APNs
    // worker, match the same opaque salted hint and enqueue a content-
    // available doorbell. The worker sees only device tokens; it never sees
    // or interprets sealed message content.
    if let Some(push_tx) = state.push_wake_tx.clone() {
        let store = state.store.clone();
        let push_family = family_token;
        let push_hint = recipient_hint;
        tokio::spawn(async move {
            let tokens = store
                .run_blocking(move |store| {
                    store.push_device_tokens_for_hint(&push_family, &push_hint, now_ms())
                })
                .await;
            match tokens {
                Ok(device_tokens) if !device_tokens.is_empty() => {
                    if let Err(error) = push_tx.try_send(PushWake { device_tokens }) {
                        warn!(%error, "APNs wake queue unavailable; relay delivery remains available by poll")
                    }
                }
                Ok(_) => {}
                Err(detail) => warn!(%detail, "APNs registration lookup failed"),
            }
        });
    }

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
struct PutPushRegistrationRequest {
    device_token: String,
    hints: Vec<String>,
}

#[derive(Serialize)]
struct PutPushRegistrationResponse {
    registered_hints: usize,
}

async fn put_push_registration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PutPushRegistrationRequest>,
) -> Result<Json<PutPushRegistrationResponse>, ApiError> {
    // Deposit credentials appear in friend cards and can only post sealed
    // mail. They must never be able to bind an APNs token to a family.
    let access = authorize_bearer(&state, &headers, FamilyOp::Read).await?;
    state.check_rate_limit(&access, 1.0, 0.0)?;

    let device_token = request.device_token.trim().to_ascii_lowercase();
    let valid_token = (32..=200).contains(&device_token.len())
        && device_token.len() % 2 == 0
        && device_token.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !valid_token {
        return Err(ApiError::bad_request(
            "device_token must be an even-length hexadecimal APNs token".to_string(),
        ));
    }
    if request.hints.is_empty() || request.hints.len() > MAX_PUSH_HINTS {
        return Err(ApiError::bad_request(format!(
            "hints must contain between 1 and {MAX_PUSH_HINTS} entries"
        )));
    }
    let mut hints = Vec::with_capacity(request.hints.len());
    for encoded in request.hints {
        let hint = decode_base64_field(&encoded, "hint")?;
        if hint.len() != RECIPIENT_HINT_LEN {
            return Err(ApiError::bad_request(format!(
                "each hint must decode to {RECIPIENT_HINT_LEN} bytes"
            )));
        }
        if !hints.contains(&hint) {
            hints.push(hint);
        }
    }

    let family_token = access.token;
    let hint_count = hints.len();
    state
        .store
        .run_blocking(move |store| {
            store.replace_push_registration(&family_token, &device_token, &hints, now_ms())
        })
        .await
        .map_err(ApiError::internal)?;
    Ok(Json(PutPushRegistrationResponse {
        registered_hints: hint_count,
    }))
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
    /// Cross-family answers only: which coarse bucket `last_seen_ms` was
    /// rounded into. Omitted entirely for a same-family (member-class)
    /// answer, whose `last_seen_ms` is exact — so a client that ignores this
    /// field reads both answers the way it always has, and a client that
    /// reads it knows which kind it is holding.
    #[serde(skip_serializing_if = "Option::is_none")]
    recency: Option<&'static str>,
}

#[derive(Serialize)]
struct PresenceResponse {
    now_ms: i64,
    presence: Vec<PresenceItem>,
}

/// Coarsen one last-seen stamp for a cross-family caller.
///
/// Returns the bucket name and the stamp to report: the oldest instant still
/// inside that bucket, one millisecond in, so a reader comparing the age with
/// `<` and a reader comparing it with `<=` land in the same tier. The answer
/// is therefore never *newer* than the truth — a contact who synced a second
/// ago is reported as up to two and a half minutes ago, which is the point.
/// Watching this endpoint tells a holder that someone's phone is broadly
/// alive; it cannot tell them when it woke up, when it went quiet, or that
/// anything happened between two samples.
fn coarse_presence(age_ms: i64, now_ms: i64) -> (&'static str, i64) {
    let (bucket, ceiling) = if age_ms <= PRESENCE_BUCKET_ACTIVE_MS {
        ("active", PRESENCE_BUCKET_ACTIVE_MS)
    } else if age_ms <= PRESENCE_BUCKET_RECENT_MS {
        ("recent", PRESENCE_BUCKET_RECENT_MS)
    } else if age_ms <= PRESENCE_BUCKET_DAY_MS {
        ("day", PRESENCE_BUCKET_DAY_MS)
    } else {
        ("older", PRESENCE_RETENTION_MS)
    };
    (bucket, now_ms.saturating_sub(ceiling.saturating_sub(1)))
}

async fn sync_presence(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PresenceRequest>,
) -> Result<Json<PresenceResponse>, ApiError> {
    let access = authorize_bearer(&state, &headers, FamilyOp::Presence).await?;
    let cross_family = access.class == TokenClass::Deposit;
    // Enforced only once the token has authorized (see `check_rate_limit`).
    //
    // The two classes are charged to different dimensions on purpose. A
    // member asking about their own family is an ordinary read and costs an
    // ordinary request. A friend-card holder asking about a contact is
    // charged to the presence bucket alone, so however hard they ask, the
    // family whose relay is answering keeps every request and every byte of
    // its own allowance (`PRESENCE-01`).
    if cross_family {
        state.check_presence_rate_limit(&access)?;
    } else {
        state.check_rate_limit(&access, 1.0, 0.0)?;
    }
    let family_token = access.token;
    // A cross-family caller may ask, and may not tell. Announcing this
    // device's hints into another family's mailbox is the half of the
    // original per-config presence sync that carried a real privacy cost —
    // it tells a family we exist — and reinstating the query does not
    // reinstate it. Refused rather than silently dropped: a client that
    // thinks it is announcing should find out.
    if cross_family && !request.announce.is_empty() {
        return Err(ApiError::presence_query_only());
    }
    let query_cap = if cross_family {
        MAX_DEPOSIT_PRESENCE_QUERY
    } else {
        MAX_PRESENCE_QUERY
    };
    if request.announce.len() > MAX_PRESENCE_ANNOUNCE {
        return Err(ApiError::bad_request(format!(
            "announce must contain at most {MAX_PRESENCE_ANNOUNCE} hints"
        )));
    }
    if request.query.len() > query_cap {
        return Err(ApiError::bad_request(format!(
            "query must contain at most {query_cap} hints"
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
            .map(|row| {
                if cross_family {
                    let age = now.saturating_sub(row.last_seen_ms).max(0);
                    let (recency, last_seen_ms) = coarse_presence(age, now);
                    PresenceItem {
                        hint: encode_base64_field(&row.hint),
                        last_seen_ms,
                        recency: Some(recency),
                    }
                } else {
                    PresenceItem {
                        hint: encode_base64_field(&row.hint),
                        last_seen_ms: row.last_seen_ms,
                        recency: None,
                    }
                }
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
// Admin API — hosted-relay ("Shore Pass") provisioning. Every route requires
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
    /// The family's stable handle. Unlike `token` it never changes, so it is
    /// what an operator should record: `GET /admin/families/{family_id}`
    /// still finds this family after a `POST /family/rotate` has replaced
    /// every credential in this response.
    family_id: String,
    /// CP4: the family's post-only credential, minted alongside `token` at
    /// provisioning. The purchase flow puts `token` on the Shore Pass setup
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
        family_id: row.family_id,
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
    // Same discipline for the family-id namespace: `/admin/families/{id}`
    // resolves one path segment as either an id or a token, and that is only
    // unambiguous while no token can wear the id prefix.
    if is_family_id(token) {
        return Err(ApiError::bad_request(format!(
            "token must not start with the family-id prefix {FAMILY_ID_PREFIX:?}"
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

/// The `{id}` path segment of every by-family admin route is a family id
/// *or* a current member token (`resolve_family_on`).
async fn admin_get_family(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id_or_token): Path<String>,
) -> Result<Json<FamilyResponse>, ApiError> {
    authorize_admin(&state, &headers)?;
    let row = state
        .store
        .resolve_family(&id_or_token)
        .map_err(ApiError::internal)?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(family_response(&state, row)?))
}

async fn admin_patch_family(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id_or_token): Path<String>,
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
    // Resolve the handle to the family's *current* token first; the update
    // itself still keys on the token, which is the `families` primary key.
    let token = state
        .store
        .resolve_family(&id_or_token)
        .map_err(ApiError::internal)?
        .ok_or_else(ApiError::not_found)?
        .token;
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
    Path(id_or_token): Path<String>,
) -> Result<Json<DeleteFamilyResponse>, ApiError> {
    authorize_admin(&state, &headers)?;
    let token = state
        .store
        .resolve_family(&id_or_token)
        .map_err(ApiError::internal)?
        .ok_or_else(ApiError::not_found)?
        .token;
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

/// Both signature fields are `Option` and checked by hand rather than being
/// required by the deserializer. A missing field would otherwise be axum's
/// 422 with a serde message, and this is a 400 with an error a client can act
/// on — the same reason `ListFamiliesQuery` parses its bounds by hand.
#[derive(Deserialize)]
struct RotateFamilyRequest {
    new_token: String,
    /// Ed25519 public key of the rotation authority, 32 bytes, base64url
    /// without padding.
    rotation_pk: Option<String>,
    /// Ed25519 signature over `rotation_signed_bytes(current, new)`, 64
    /// bytes, base64url without padding.
    rotation_sig: Option<String>,
}

#[derive(Serialize)]
struct RotateFamilyResponse {
    /// The family's member token from here on. Echoed rather than assumed so a
    /// client that lost a previous response learns the truth from the server.
    family_token: String,
    /// Its CP4 attenuation, the credential friend cards carry. Derivable
    /// offline by the client; returned so the two sides can be compared.
    deposit_token: String,
    /// Rows carried across (`RelayStore::rotate_family_token`). Zero on a
    /// retry that found the work already done.
    envelopes_moved: u64,
    /// `false` when this call found the rotation already committed.
    rotated: bool,
}

/// **`POST /family/rotate` — a family re-keys itself**
/// (`specs/multi-device-v1.md` §10 step 2).
///
/// The only route where a family credential authorizes changing that
/// credential, and it exists because §10.2's hole cannot be closed anywhere
/// else: relayd scopes fetch and ack by `family_token` alone, so a device that
/// has been revoked from a person's roster keeps full read/delete access to
/// the family mailbox until the token itself changes. Waiting for an operator
/// to re-provision would make "remove this stolen phone" a support ticket.
///
/// Authorization is the current **member** token *and a signature*. A deposit
/// credential is refused by `FamilyOp::Rotate`'s class rule before this body
/// runs — friend cards carry deposit tokens, so a rotatable deposit credential
/// would let anyone a family ever waved a QR code at lock them out of their
/// own mailbox. But the member token alone is not enough either, and that is
/// the point of the second factor: the revoked device *holds* the member
/// token. If possession authorized rotation, the device this ceremony exists
/// to evict could run the ceremony first and lock the owner out.
///
/// So the caller also sends `rotation_pk` (32-byte Ed25519 public key) and
/// `rotation_sig` (64-byte signature), both base64url without padding, over
/// `rotation_signed_bytes(current_token, new_token)` — the presented bearer
/// token and the trimmed replacement, each length-prefixed behind a versioned
/// domain separator. `RelayStore::rotate_family_token` checks it inside the
/// same transaction that performs the re-key.
///
/// **Two consequences, stated plainly rather than buried.**
///
/// *After the first rotation, exactly one key can ever rotate this family.*
/// The relay registers the first key that signs a valid rotation and refuses
/// every other key from then on; there is no recovery path here for a family
/// that loses it, short of an operator re-provisioning. On a **shared Shore
/// Pass** that means only the organizer's person root can rotate, because
/// only one person holds that key — which matches the organizer reality
/// (they bought the pass, they hand out the cards, they are who a household
/// asks when a phone is stolen) rather than fighting it.
///
/// *A thief can race trust-on-first-rotation, once — and only by really
/// rotating.* On the very first rotation a family ever performs, `rotation_pk`
/// is still NULL and there is nothing to check the presented key against, so a
/// revoked device holding the member token could register a key of its own
/// before the owner does and take the authority permanently. That window is
/// real and is accepted deliberately, bounded four ways: it exists only for
/// families provisioned before this shipped, only until their first rotation,
/// it requires the hostile device to already hold a live member token, and the
/// only call that can register is one that *moves the family to a new token* —
/// which is loud, because it locks the owner's other devices out of the
/// mailbox immediately and they notice. A registration that changed nothing
/// would be silent, so `rotate_family_token` refuses to register on any
/// already-rotated call. Families provisioned from here on will register on
/// their first rotation from a device that was never revoked. The alternative
/// — refusing every legacy family's first rotation — would leave exactly the
/// families most likely to need a revocation unable to perform one.
///
/// The replacement is chosen by the client, which is what makes the ceremony
/// survivable. A server-minted token would exist only in the response, so a
/// dropped response would strand the family on a credential that no longer
/// authorizes anything — permanent brickage from a network blip. Instead the
/// client writes its candidate down first (`MessageStore::begin_relay_rotation`)
/// and can always ask again; presenting the new token reports `rotated: false`
/// and the same values, so a retry converges. `MIN_ROTATION_TOKEN_LEN` is the
/// entropy floor that letting the client choose costs.
async fn rotate_family(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RotateFamilyRequest>,
) -> Result<Json<RotateFamilyResponse>, ApiError> {
    let presented = raw_bearer_token(&headers)?;
    let access = authorize_family(&state, &presented, FamilyOp::Rotate, now_ms()).await?;
    // Its own small bucket, never the family's shared request allowance —
    // see `AppState::check_rotation_rate_limit`.
    state.check_rotation_rate_limit(&access)?;

    let new_token = request.new_token.trim().to_string();
    if new_token.len() < MIN_ROTATION_TOKEN_LEN || new_token.len() > MAX_FAMILY_TOKEN_LEN {
        return Err(ApiError::bad_request(format!(
            "new_token must be {MIN_ROTATION_TOKEN_LEN}..={MAX_FAMILY_TOKEN_LEN} characters"
        )));
    }
    if new_token
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(ApiError::bad_request(
            "new_token must not contain whitespace or control characters".to_string(),
        ));
    }
    // CP4's class prefix is what makes a presented credential's class
    // unambiguous; a member token wearing it would break the auth lookup.
    if is_deposit_token(&new_token) {
        return Err(ApiError::bad_request(format!(
            "new_token must not start with the deposit-token prefix {DEPOSIT_TOKEN_PREFIX:?}"
        )));
    }
    // And not the family-id prefix either, for the reason `resolve_family_on`
    // spells out: `/admin/families/{id}` reads one path segment as either
    // namespace, so a token that could pass for an id would make operator
    // addressing ambiguous.
    if is_family_id(&new_token) {
        return Err(ApiError::bad_request(format!(
            "new_token must not start with the family-id prefix {FAMILY_ID_PREFIX:?}"
        )));
    }
    // The same overlap `admin_provision_family` forbids: a family token that
    // shadowed an operator or static credential would cross a trust boundary.
    if state.admin_token.as_deref() == Some(new_token.as_str())
        || state.auth_tokens.contains(&new_token)
        || state.static_deposit_tokens.contains_key(&new_token)
    {
        return Err(ApiError::rotation_token_taken());
    }

    let authority = RotationAuthority {
        public_key: decode_fixed_base64_field(request.rotation_pk.as_ref(), "rotation_pk")?,
        signature: decode_fixed_base64_field(request.rotation_sig.as_ref(), "rotation_sig")?,
    };

    // The signed message binds the credential as *presented and trimmed*
    // (`access.token` is the canonical member token the bearer resolved to,
    // which for `FamilyOp::Rotate` is the bearer itself — deposit credentials
    // never reach here), so a signature captured for one family's rotation
    // cannot be replayed for another's.
    let current = access.token.clone();
    let requested = new_token.clone();
    let outcome = state
        .store
        .run_blocking(move |store| store.rotate_family_token(&current, &requested, &authority))
        .await
        .map_err(ApiError::internal)?;
    match outcome {
        FamilyRotation::Rotated {
            family,
            envelopes_moved,
        } => {
            info!(
                family = %token_prefix(&family.token),
                superseded = %token_prefix(&access.token),
                envelopes_moved,
                "family token rotated"
            );
            Ok(Json(RotateFamilyResponse {
                family_token: family.token,
                deposit_token: family.deposit_token,
                envelopes_moved,
                rotated: true,
            }))
        }
        FamilyRotation::AlreadyRotated { family } => Ok(Json(RotateFamilyResponse {
            family_token: family.token,
            deposit_token: family.deposit_token,
            envelopes_moved: 0,
            rotated: false,
        })),
        FamilyRotation::UnknownFamily => Err(ApiError::rotation_unsupported(&access.token)),
        FamilyRotation::TokenTaken => Err(ApiError::rotation_token_taken()),
        FamilyRotation::Unauthorized => Err(ApiError::rotation_unauthorized(&access.token)),
    }
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
    let family_key = access.family_key;
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
    //
    // Keyed by the family's stable key, not by its token: a rotation replaces
    // the token, and a token-keyed map would strand the old entry — its
    // permits still held by whatever sockets are open under it — while the
    // family silently got a second, full set of connection slots under the
    // new name. The cap is meant to bound one family's live sockets, and only
    // a key that survives the rotation can do that.
    let per_token_semaphore = {
        let mut per_token = state.ws_per_token.lock().unwrap_or_else(|e| e.into_inner());
        per_token
            .entry(family_key)
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
                WsSession {
                    state,
                    family_token: token,
                    hints,
                    hints_base64,
                    after,
                    global_permit,
                    per_token_permit,
                },
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

struct WsSession {
    state: AppState,
    family_token: String,
    hints: Vec<Vec<u8>>,
    hints_base64: HashSet<String>,
    after: i64,
    // FR6: RAII connection-cap permits -- held for the socket's whole
    // lifetime; dropped (and the slot freed) whenever this function
    // returns, on any disconnect path.
    global_permit: OwnedSemaphorePermit,
    per_token_permit: OwnedSemaphorePermit,
}

async fn handle_ws(mut socket: WebSocket, session: WsSession) {
    let WsSession {
        state,
        family_token,
        hints,
        hints_base64,
        mut after,
        global_permit: _global_permit,
        per_token_permit: _per_token_permit,
    } = session;
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

/// A required base64url field that must decode to exactly `N` bytes.
///
/// Absence, bad base64 and a wrong length are all 400s rather than
/// authorization failures: each one means the caller's encoder is wrong, and
/// a 403 would send it looking for a different key instead.
fn decode_fixed_base64_field<const N: usize>(
    value: Option<&String>,
    field: &str,
) -> Result<[u8; N], ApiError> {
    let value = value.ok_or_else(|| ApiError::bad_request(format!("{field} is required")))?;
    let bytes = decode_base64_field(value, field)?;
    bytes
        .try_into()
        .map_err(|_| ApiError::bad_request(format!("{field} must decode to exactly {N} bytes")))
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
    /// Fetch/ack/WS — draining the mailbox.
    Read,
    /// `POST /presence`. Its own op rather than a `Read` because a deposit
    /// credential may perform it and may not perform a `Read`: presence is
    /// the one answer a friend card is *supposed* to be able to ask for, and
    /// it discloses no mail. What it may ask, and what it gets back, is
    /// narrowed in `sync_presence` — announce refused, query capped, answer
    /// coarsened, own rate bucket.
    Presence,
    /// `POST /family/rotate` (`specs/multi-device-v1.md` §10 step 2). Its own
    /// op because it is the only route where holding the family's member
    /// credential authorizes changing that credential: a deposit token — which
    /// rides friend cards, i.e. is held by people outside the family — must
    /// never be able to lock a family out of its own mailbox.
    Rotate,
}

impl FamilyOp {
    /// Whether a deposit-class credential authorizes this op at all.
    fn allowed_for_deposit(self) -> bool {
        matches!(self, FamilyOp::Post | FamilyOp::Presence)
    }
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
    /// The rate-limit bucket key: the family's stable id, prefixed by the
    /// presented credential's class.
    ///
    /// It used to be the presented credential itself, which was wrong in one
    /// specific way that only WP5 made reachable: `POST /family/rotate`
    /// replaces the token, so a family that rotated arrived at a bucket key
    /// nobody had ever used and got a full fresh allowance — the rate limiter
    /// silently reset by a call any member device can make. Keying on the
    /// stable id fixes that while keeping CP4's separation, because the class
    /// prefix still puts member and deposit traffic in different buckets, so
    /// a friend-card flood cannot eat the family's own allowance.
    rate_key: String,
    /// The per-family key for anything that must survive a rotation but is
    /// *not* per-class — today, the WebSocket connection semaphore. The
    /// stable family id where one exists; see `family_limit_key` for the
    /// static-allowlist fallback.
    family_key: String,
    quota_bytes: u64,
    /// The storage-accounting identity a post by this caller is charged to:
    /// `None` for member class (the family itself, which keeps the full
    /// quota), `Some(credential)` for deposit class.
    ///
    /// It is the *presented* credential rather than the resolved member
    /// token, because the resolved token is the same for every friend and
    /// would collapse every depositor into one. Today one deposit credential
    /// per family means that collapse happens anyway — a family stamps one
    /// derived deposit token onto every card it hands out, so the relay
    /// genuinely cannot tell one friend from another, and the honest
    /// enforceable unit of fairness is the credential, not the person
    /// holding it. Keying the accounting on the credential is what makes
    /// that a property of the *credential model* rather than of this code:
    /// the day friend cards carry per-friend deposit credentials, each one
    /// gets its own share here with no further change.
    ///
    /// `POST /family/rotate` retires every outstanding friend card, so the
    /// keys of rows deposited before a rotation name credentials that no
    /// longer authenticate. Their bytes keep counting against the family
    /// quota (nothing is lost or hidden) but stop counting against any live
    /// depositor's share, which is the right answer: those depositors cannot
    /// post again, and the family's remedy was the rotation.
    depositor: Option<String>,
}

/// The stable per-family key the rate buckets and the WS semaphore use.
///
/// Provisioned families have a `family_id` that outlives their token, and
/// that is the whole reason this exists. Static env-allowlist families
/// (`CRUISEMESH_RELAY_TOKENS`) have no `families` row at all, so there is no
/// id to use — they fall back to the token string, which is correct for them
/// rather than merely tolerable: an operator-configured token only changes
/// when the operator edits the config and restarts, so there is nothing for a
/// stable id to protect against. The same fallback covers the torn
/// mid-migration read `family_row_from` describes.
fn family_limit_key(family_id: &str, token: &str) -> String {
    if family_id.is_empty() {
        token.to_string()
    } else {
        family_id.to_string()
    }
}

/// The rate-limit bucket key: the stable family key, namespaced by credential
/// class so member and deposit traffic never share a bucket (CP4).
fn family_rate_key(class: TokenClass, family_key: &str) -> String {
    let class_prefix = match class {
        TokenClass::Member => "member",
        TokenClass::Deposit => "deposit",
    };
    format!("{class_prefix}:{family_key}")
}

/// Resolve a family credential against the static env allowlist first
/// (implicit always-active families, the self-hosted path — zero behavior
/// change), then the static tokens' derived deposit counterparts, then the
/// provisioned `families` table (member or deposit column; status + expiry +
/// per-family quota).
///
/// CP4 enforcement lives HERE, not in handlers: every authenticated route
/// funnels through this function with its `FamilyOp`, and a deposit-class
/// credential authorizes `FamilyOp::Post` and `FamilyOp::Presence` and
/// nothing else — fetch/ack/WS all pass `FamilyOp::Read` and get a structured
/// 403 `deposit_only` before any handler code runs, so no individual handler
/// can forget the check.
///
/// Presence joined that list deliberately, and it is the one op where "may"
/// is not the whole answer: `sync_presence` narrows what a deposit credential
/// may ask and what it is told back. Suspension and expiry are checked for it
/// exactly as for every other op — a lapsed or suspended family answers
/// nothing, to anyone.
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
            rate_key: family_rate_key(TokenClass::Member, token),
            family_key: token.to_string(),
            quota_bytes: state.family_quota_bytes,
            depositor: None,
        });
    }
    if let Some(member) = state.static_deposit_tokens.get(token) {
        if !op.allowed_for_deposit() {
            return Err(ApiError::deposit_only(token));
        }
        return Ok(FamilyAccess {
            rate_key: family_rate_key(TokenClass::Deposit, member),
            family_key: member.clone(),
            token: member.clone(),
            class: TokenClass::Deposit,
            quota_bytes: state.family_quota_bytes,
            depositor: Some(token.to_string()),
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
    if class == TokenClass::Deposit && !op.allowed_for_deposit() {
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
    let family_key = family_limit_key(&family.family_id, &family.token);
    Ok(FamilyAccess {
        quota_bytes: family.quota_bytes.unwrap_or(state.family_quota_bytes),
        token: family.token,
        class,
        rate_key: family_rate_key(class, &family_key),
        family_key,
        depositor: match class {
            TokenClass::Member => None,
            TokenClass::Deposit => Some(token.to_string()),
        },
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
    /// card into the Shore Pass slot" from a revoked or mistyped token.
    fn deposit_only(token: &str) -> Self {
        warn!(
            family = %token_prefix(token),
            "family reject: deposit token used for a member-only operation (403 deposit_only)"
        );
        Self {
            status: StatusCode::FORBIDDEN,
            message: "deposit tokens can only post envelopes and query presence; fetch, \
                      ack, and websocket access require the family's member token"
                .to_string(),
            code: Some("deposit_only"),
            retry_after_secs: None,
        }
    }

    /// A deposit-class credential tried to *announce* presence rather than
    /// only query it. 403 (not 400): the request is well-formed and the
    /// credential is real — announcing into a family you are not a member of
    /// is simply outside the class. The stable `presence_query_only` code
    /// lets a client tell this apart from a malformed hint.
    fn presence_query_only() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: "deposit tokens may query presence but not announce it; announcing \
                      requires the family's member token"
                .to_string(),
            code: Some("presence_query_only"),
            retry_after_secs: None,
        }
    }

    /// `specs/multi-device-v1.md` §10 step 2: this deployment has no family
    /// row to re-key, because the credential comes from the static
    /// `CRUISEMESH_RELAY_TOKENS` allowlist. 409 (not 404): the family is real
    /// and authorized — its token simply lives in the operator's config, and
    /// only the operator can change it. The stable `rotation_unsupported` code
    /// is what lets a revoking device say "your relay was set up by hand, so
    /// finish the rotation with a new setup card" instead of retrying forever.
    fn rotation_unsupported(token: &str) -> Self {
        warn!(
            family = %token_prefix(token),
            "family reject: token rotation on a statically configured family (409 rotation_unsupported)"
        );
        Self {
            status: StatusCode::CONFLICT,
            message: "this family's token is configured on the server and cannot be \
                      rotated from a device; provision a new token instead"
                .to_string(),
            code: Some("rotation_unsupported"),
            retry_after_secs: None,
        }
    }

    /// §10 step 2: the proposed replacement credential already belongs to
    /// somebody. 409 Conflict, and the caller's remedy is to mint another and
    /// try again — which is why this is a distinct code from a malformed
    /// token, whose remedy is to stop sending that shape.
    fn rotation_token_taken() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: "the proposed family token is already in use".to_string(),
            code: Some("rotation_token_taken"),
            retry_after_secs: None,
        }
    }

    /// §10 step 2: the request carried the family's member token but not its
    /// rotation authority. 403 rather than 401, and the distinction matters:
    /// the credential authenticated fine — this is a real member token — and
    /// what failed is the separate signature that says *this* holder may
    /// re-key the family. Telling those apart is what stops a client from
    /// "fixing" the failure by re-fetching its token.
    ///
    /// Logged, like every other family rejection, because a legitimate
    /// rotation refused here means a family cannot evict a device it no
    /// longer trusts, and that is something an operator should be able to see
    /// from the server side rather than only from a support ticket.
    fn rotation_unauthorized(token: &str) -> Self {
        warn!(
            family = %token_prefix(token),
            "family reject: rotation not signed by the family's rotation key (403 rotation_unauthorized)"
        );
        Self {
            status: StatusCode::FORBIDDEN,
            message: "this rotation is not signed by the family's registered rotation key"
                .to_string(),
            code: Some("rotation_unauthorized"),
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

    /// A deposit-class post the family mailbox has room for, but that its
    /// depositor's fair share of that mailbox does not.
    ///
    /// The status stays 507 on purpose. It is the honest status — the server
    /// understood the request and will not store the result — and it is what
    /// keeps this safe for clients that predate it: `core`'s classifier falls
    /// back to the status when it does not recognize a `code`, so an older
    /// app reads this exactly as it reads a full mailbox today (a persistent
    /// storage condition, envelope stays queued for retry), which is the
    /// correct degrade rather than a guess.
    ///
    /// What must never be reused is the *code*. `family_quota_exceeded` means
    /// "this mailbox is full", a claim about the family that a depositor
    /// hitting its own share would be making falsely: the family's own
    /// devices can still post, and no amount of draining by the family
    /// changes this depositor's answer. Two distinct codes, one per
    /// [`DepositShareScope`], let a client say which of the three actually
    /// happened, and let an operator reading logs tell "the family filled its
    /// mailbox" from "one friend card is running hot" without inference.
    fn deposit_share_exceeded(
        scope: DepositShareScope,
        usage_bytes: u64,
        share_bytes: u64,
        quota_bytes: u64,
    ) -> Self {
        let subject = match scope {
            DepositShareScope::Depositor => "this deposit credential's share of",
            DepositShareScope::AllDepositors => "the deposit-class share of",
        };
        Self {
            status: StatusCode::INSUFFICIENT_STORAGE,
            message: format!(
                "{subject} the family mailbox is full: {usage_bytes} bytes used of a \
                 {share_bytes}-byte share ({quota_bytes}-byte family quota, expired rows \
                 already pruned). The family mailbox itself is not full."
            ),
            code: Some(scope.code()),
            retry_after_secs: None,
        }
    }

    /// A row with this `(family_token, msg_id)` already holds *different*
    /// sealed content. The stored row is the first writer and is authoritative;
    /// the relay is content-agnostic and cannot know which post is genuine, so
    /// it keeps the stored row unchanged and reports that this post was not
    /// stored. 409 Conflict is the standard status for "the request conflicts
    /// with the current state of the resource", and it is deliberately distinct
    /// from a dedupe success: a caller must not retire its send state on this,
    /// because its own content never reached the mailbox. The stable
    /// `msg_id_conflict` code lets a client act on it exactly; an older client
    /// that does not recognize the code still sees a non-2xx and treats the
    /// post as not delivered, which is the safe degrade.
    fn msg_id_conflict() -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: "msg_id already holds different content; this post was not stored".to_string(),
            code: Some("msg_id_conflict"),
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
    depositor      TEXT NOT NULL DEFAULT '',
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
CREATE TABLE IF NOT EXISTS push_registrations (
    family_token TEXT NOT NULL,
    device_token TEXT NOT NULL,
    updated_ms   INTEGER NOT NULL,
    PRIMARY KEY(family_token, device_token)
);
CREATE TABLE IF NOT EXISTS push_registration_hints (
    family_token TEXT NOT NULL,
    device_token TEXT NOT NULL,
    hint         BLOB NOT NULL,
    PRIMARY KEY(family_token, device_token, hint)
);
CREATE INDEX IF NOT EXISTS idx_push_registration_hints_lookup
    ON push_registration_hints(family_token, hint);
CREATE INDEX IF NOT EXISTS idx_push_registrations_updated
    ON push_registrations(updated_ms);
CREATE TABLE IF NOT EXISTS families (
    token         TEXT PRIMARY KEY,
    status        TEXT NOT NULL DEFAULT 'active',
    plan          TEXT,
    quota_bytes   INTEGER,
    created_ms    INTEGER NOT NULL,
    expires_ms    INTEGER,
    note          TEXT,
    deposit_token TEXT,
    family_id     TEXT,
    rotation_pk   BLOB
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

    fn push_registration_request(token: &str, device_token: &str, hint: u8) -> Request<Body> {
        Request::builder()
            .method("PUT")
            .uri("/push/registrations")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "device_token": device_token,
                    "hints": [encode_base64_field(&sample_hint(hint))],
                })
                .to_string(),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn member_registration_enqueues_matching_apns_wake() {
        let store = RelayStore::open(":memory:").unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
        let app = app(
            AppState::new(store, HashSet::from(["family-a".to_string()]))
                .with_push_wake_sender(sender),
        );
        let token = "ab".repeat(32);
        let response = app
            .clone()
            .oneshot(push_registration_request("family-a", &token, 1))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["registered_hints"], 1);

        let response = app
            .oneshot(envelope_request("family-a", 9, 48))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let wake = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(wake.device_tokens, vec![token]);
    }

    #[tokio::test]
    async fn deposit_credentials_cannot_register_push_tokens() {
        let app = test_app();
        let response = app
            .oneshot(push_registration_request(
                &deposit_token_for("family-a"),
                &"ab".repeat(32),
                1,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "deposit_only");
    }

    #[test]
    fn stale_push_registrations_do_not_match() {
        let (_db, store) = test_store();
        let token = "ab".repeat(32);
        let now = PUSH_REGISTRATION_RETENTION_MS + 10;
        store
            .replace_push_registration("family-a", &token, &[sample_hint(1)], 1)
            .unwrap();
        assert!(store
            .push_device_tokens_for_hint("family-a", &sample_hint(1), now)
            .unwrap()
            .is_empty());
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
        // token: fetch, ack. (WS is covered in e2e_ws.rs — the upgrade needs
        // a real socket.)
        for (request, what) in [
            (fetch_request(&deposit), "fetch"),
            (ack_request(&deposit, &[relay_id]), "ack"),
        ] {
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{what}");
            assert_eq!(body_json(response).await["code"], "deposit_only", "{what}");
        }
        // Presence is the one exception, and it is an exception to what may
        // be *asked*: `presence_request` announces, which a deposit
        // credential may never do (`PRESENCE-01`), so it is refused with its
        // own code rather than the class one. The query half, and everything
        // the answer is narrowed to, lives in `tests/e2e_presence.rs`.
        let response = app
            .clone()
            .oneshot(presence_request(&deposit))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "presence_query_only");

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

    fn envelope_request_with_expiry(token: &str, msg_byte: u8, expiry_ms: i64) -> Request<Body> {
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
                    "sealed": encode_base64_field(&[7u8; 48]),
                    "expiry_ms": expiry_ms,
                })
                .to_string(),
            ))
            .unwrap()
    }

    #[tokio::test]
    async fn deposit_post_expiry_is_clamped_to_the_deposit_ceiling() {
        let app = test_app();
        let deposit = deposit_token_for("family-a");
        let far_future = now_ms() + 2 * MAX_RETENTION_MS;

        // A deposit post asking for the far future is stored, but lives at
        // most MAX_DEPOSIT_RETENTION_MS — a leaked friend card cannot park
        // quota-filling rows for the member-class 30-day ceiling.
        assert_eq!(
            app.clone()
                .oneshot(envelope_request_with_expiry(&deposit, 1, far_future))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        // The same request over the member token keeps the 30-day ceiling.
        assert_eq!(
            app.clone()
                .oneshot(envelope_request_with_expiry("family-a", 2, far_future))
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
        let page = body_json(response).await;
        let envelopes = page["envelopes"].as_array().unwrap();
        assert_eq!(envelopes.len(), 2);
        // Re-read the clock after the posts so the bound is conservative:
        // each row's expiry was clamped against a `now` at or before this one.
        let latest_now = now_ms();
        for envelope in envelopes {
            let expiry = envelope["expiry_ms"].as_i64().unwrap();
            let msg_id = envelope["msg_id"].as_str().unwrap().to_string();
            if msg_id == encode_base64_field(&sample_msg_id(1)) {
                assert!(
                    expiry <= latest_now + MAX_DEPOSIT_RETENTION_MS,
                    "deposit-posted row outlives the deposit ceiling: {expiry}"
                );
            } else {
                assert!(
                    expiry > latest_now + MAX_DEPOSIT_RETENTION_MS,
                    "member-posted row was wrongly clamped to the deposit ceiling: {expiry}"
                );
            }
        }
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

    /// WP5's additive migration on a database that predates it: the family
    /// gains a stable id it did not have, keeps everything it did have, and
    /// stays *unregistered* for rotation — NULL is the meaningful value of
    /// `rotation_pk`, and backfilling it would mean inventing an authority
    /// nobody holds.
    #[test]
    fn migration_mints_stable_family_ids_and_leaves_rotation_unregistered() {
        let db = NamedTempFile::new().unwrap();
        let path = db.path().to_str().unwrap().to_string();
        {
            // A database exactly as a pre-WP5 (post-CP4) relayd left it.
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE families (
                    token         TEXT PRIMARY KEY,
                    status        TEXT NOT NULL DEFAULT 'active',
                    plan          TEXT,
                    quota_bytes   INTEGER,
                    created_ms    INTEGER NOT NULL,
                    expires_ms    INTEGER,
                    note          TEXT,
                    deposit_token TEXT
                );
                INSERT INTO families (token, status, created_ms, deposit_token)
                    VALUES ('fam-legacy', 'active', 123, 'cmdep1-legacy');",
            )
            .unwrap();
        }

        let store = RelayStore::open(&path).unwrap();
        let row = store.get_family("fam-legacy").unwrap().unwrap();
        assert!(row.family_id.starts_with(FAMILY_ID_PREFIX));
        assert_eq!(row.deposit_token, "cmdep1-legacy");
        assert_eq!(row.rotation_pk, None);

        // The id addresses the family, and it is stable across restarts —
        // re-minting it on every open would orphan whatever an operator
        // wrote down.
        assert_eq!(
            store.resolve_family(&row.family_id).unwrap().unwrap().token,
            "fam-legacy"
        );
        drop(store);
        let store = RelayStore::open(&path).unwrap();
        assert_eq!(
            store.get_family("fam-legacy").unwrap().unwrap().family_id,
            row.family_id
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

    // -----------------------------------------------------------------------
    // §10 step 2: a family re-keys itself
    // -----------------------------------------------------------------------

    /// A member token of the shape `core_mint_relay_member_token` produces:
    /// class-prefixed, and long past `MIN_ROTATION_TOKEN_LEN`.
    fn minted_token(tag: &str) -> String {
        format!("cmfam1-{tag}{}", "0".repeat(32))
    }

    /// The rotation authority the §10 tests sign with. A fixed seed rather
    /// than a fresh keypair per call because trust-on-first-rotation is
    /// stateful: a test that rotates and then retries has to present the same
    /// key both times, exactly as a real client does.
    fn test_rotation_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[7u8; ROTATION_PK_LEN])
    }

    /// A second, unrelated keypair — the revoked device's, in the tests that
    /// model one.
    fn other_rotation_key() -> ed25519_dalek::SigningKey {
        ed25519_dalek::SigningKey::from_bytes(&[9u8; ROTATION_PK_LEN])
    }

    fn rotation_authority(
        key: &ed25519_dalek::SigningKey,
        current_token: &str,
        new_token: &str,
    ) -> RotationAuthority {
        use ed25519_dalek::Signer;
        RotationAuthority {
            public_key: key.verifying_key().to_bytes(),
            signature: key
                .sign(&rotation_signed_bytes(current_token, new_token))
                .to_bytes(),
        }
    }

    /// A well-formed rotation, signed by `key` over the pair the server will
    /// reconstruct: the presented bearer token and the *trimmed* replacement.
    fn rotate_request_signed_by(
        bearer: &str,
        new_token: &str,
        key: &ed25519_dalek::SigningKey,
    ) -> Request<Body> {
        let authority = rotation_authority(key, bearer, new_token.trim());
        rotate_request_with(
            bearer,
            serde_json::json!({
                "new_token": new_token,
                "rotation_pk": encode_base64_field(&authority.public_key),
                "rotation_sig": encode_base64_field(&authority.signature),
            }),
        )
    }

    /// A rotation with a hand-built body, for the malformed and unsigned
    /// cases that cannot be expressed as "signed by some key".
    fn rotate_request_with(bearer: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/family/rotate")
            .header("authorization", format!("Bearer {bearer}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn rotate_request(bearer: &str, new_token: &str) -> Request<Body> {
        rotate_request_signed_by(bearer, new_token, &test_rotation_key())
    }

    /// The gate in one test (`specs/multi-device-v1.md` §13, WP5): **a revoked
    /// device demonstrably loses relay fetch after rotation**, and **rotate,
    /// then drain** — no sibling loses an un-fetched row to make that happen.
    ///
    /// The revoked device here is modelled exactly as the threat model
    /// demands: it kept the old member token and everything derived from it,
    /// and it replays them.
    #[tokio::test]
    async fn rotation_cuts_the_old_credential_off_without_losing_a_single_row() {
        let app = admin_app();
        let old = "fam-before-the-revocation-token";
        let new = minted_token("after");
        provision(&app, old, serde_json::json!({})).await;

        // Two envelopes a sibling has not fetched yet.
        for byte in [1u8, 2u8] {
            assert_eq!(
                app.clone()
                    .oneshot(envelope_request(old, byte, 48))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
        // And presence the family announced under the old token.
        assert_eq!(
            app.clone()
                .oneshot(presence_request(old))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        let response = app
            .clone()
            .oneshot(rotate_request(old, &new))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let rotated = body_json(response).await;
        assert_eq!(rotated["family_token"], new);
        assert_eq!(rotated["deposit_token"], deposit_token_for(&new));
        assert_eq!(rotated["envelopes_moved"], 2);
        assert_eq!(rotated["rotated"], true);

        // The revoked device replays every credential it ever held. Fetch and
        // ack are the two §10.2 names, and ack is the dangerous one -- it
        // DELETES a sibling's row.
        for (request, what) in [
            (fetch_request(old), "fetch"),
            (ack_request(old, &[1, 2]), "ack"),
            (presence_request(old), "presence"),
            (envelope_request(old, 3, 48), "post"),
        ] {
            assert_eq!(
                app.clone().oneshot(request).await.unwrap().status(),
                StatusCode::UNAUTHORIZED,
                "the retired member token must not authorize {what}"
            );
        }
        // Including the deposit attenuation of the retired token, which is
        // what its friend cards carry.
        assert_eq!(
            app.clone()
                .oneshot(envelope_request(&deposit_token_for(old), 4, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED,
            "the retired deposit token must not authorize a post either"
        );

        // Rotate, then drain: the sibling that slept through all of this
        // fetches both rows, with their ids and hints untouched.
        let response = app.clone().oneshot(fetch_request(&new)).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let page = body_json(response).await;
        let envelopes = page["envelopes"].as_array().unwrap();
        assert_eq!(envelopes.len(), 2, "no un-fetched row was dropped");
        assert_eq!(envelopes[0]["id"], 1);
        assert_eq!(envelopes[1]["id"], 2);
        assert_eq!(
            envelopes[0]["recipient_hint"],
            encode_base64_field(&sample_hint(1))
        );

        // And the new deposit token deposits, so a contact who received the
        // kind-9 notice reaches the family again.
        assert_eq!(
            app.clone()
                .oneshot(envelope_request(&deposit_token_for(&new), 5, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    /// What moves across a rotation and what does not.
    ///
    /// Presence moves: it is the family's own announcement about itself, and
    /// a sibling that slept through the ceremony must not look offline
    /// afterwards. Push registrations are *purged*, because each one is a
    /// single device's wake channel and the relay cannot tell the revoked
    /// device's from a sibling's — carrying them would leave the evicted
    /// device still being woken for the family's mail. Siblings re-register
    /// on their next round, which is the cost this deliberately accepts.
    #[test]
    fn rotation_carries_presence_but_purges_push_registrations() {
        let (_db, store) = test_store();
        let old = "fam-before";
        let new = minted_token("after");
        store.upsert_family(old, None, None, None, None, 1).unwrap();
        store
            .sync_presence(old, &[sample_hint(1)], &[sample_hint(1)], 1_000)
            .unwrap();
        store
            .replace_push_registration(old, "apns-device", &[sample_hint(1)], 1_000)
            .unwrap();

        let authority = rotation_authority(&test_rotation_key(), old, &new);
        match store.rotate_family_token(old, &new, &authority).unwrap() {
            FamilyRotation::Rotated { family, .. } => {
                assert_eq!(family.token, new);
                assert_eq!(family.deposit_token, deposit_token_for(&new));
            }
            other => panic!("expected a rotation, got {other:?}"),
        }
        assert!(
            store
                .push_device_tokens_for_hint(&new, &sample_hint(1), 0)
                .unwrap()
                .is_empty(),
            "the revoked device's wake channel must not follow the family"
        );
        assert!(store
            .push_device_tokens_for_hint(old, &sample_hint(1), 0)
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .sync_presence(&new, &[], &[sample_hint(1)], 2_000)
                .unwrap()
                .len(),
            1,
            "the family's announced presence survived the rotation"
        );
    }

    /// The crash-safety contract the client depends on: a rotation whose
    /// response was lost is discoverable by asking again with the credential
    /// the client wrote down first.
    #[tokio::test]
    async fn a_rotation_whose_answer_was_lost_is_recovered_by_retrying() {
        let app = admin_app();
        let old = "fam-before-the-revocation-token";
        let new = minted_token("after");
        provision(&app, old, serde_json::json!({})).await;
        assert_eq!(
            app.clone()
                .oneshot(rotate_request(old, &new))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // The client never saw that answer. It retries with the token it
        // minted -- the only credential it can still present.
        let response = app
            .clone()
            .oneshot(rotate_request(&new, &new))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let retried = body_json(response).await;
        assert_eq!(retried["family_token"], new);
        assert_eq!(retried["rotated"], false);
        assert_eq!(retried["envelopes_moved"], 0);

        // Retrying with the retired credential is an ordinary auth failure --
        // there is nothing to tell a stranger holding a dead token.
        assert_eq!(
            app.clone()
                .oneshot(rotate_request(old, &minted_token("third")))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn only_a_member_credential_of_a_provisioned_family_may_rotate() {
        let app = admin_app();
        let old = "fam-before-the-revocation-token";
        provision(&app, old, serde_json::json!({})).await;

        // A deposit credential rides friend cards, so anyone the family ever
        // waved a QR code at holds one. It must never be able to lock them
        // out of their own mailbox.
        let response = app
            .clone()
            .oneshot(rotate_request(
                &deposit_token_for(old),
                &minted_token("byadeposit"),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "deposit_only");

        // A statically configured family has no row to re-key, and says so
        // with a code a client can act on rather than retry against.
        let response = app
            .clone()
            .oneshot(rotate_request("family-a", &minted_token("static")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(response).await["code"], "rotation_unsupported");
    }

    #[tokio::test]
    async fn rotation_refuses_a_replacement_that_is_weak_malformed_or_taken() {
        let app = admin_app();
        let mine = "fam-mine-before-the-revocation";
        let theirs = "fam-someone-elses-token-entirely";
        provision(&app, mine, serde_json::json!({})).await;
        provision(&app, theirs, serde_json::json!({})).await;

        for (candidate, status, why) in [
            ("short", StatusCode::BAD_REQUEST, "below the entropy floor"),
            (
                "cmdep1-aaaaaaaaaaaaaaaaaaaaaaaaaaa",
                StatusCode::BAD_REQUEST,
                "wears the deposit class prefix",
            ),
            (
                "has a space in it and is long enough",
                StatusCode::BAD_REQUEST,
                "whitespace",
            ),
            (
                theirs,
                StatusCode::CONFLICT,
                "another family's member token",
            ),
            (
                ADMIN_TOKEN,
                StatusCode::BAD_REQUEST,
                "an operator credential, refused by the length floor before \
                 the collision check even runs",
            ),
            (
                &deposit_token_for(theirs),
                StatusCode::BAD_REQUEST,
                "another family's deposit token, refused by class rather than \
                 by collision -- an ambiguous `token OR deposit_token` lookup \
                 is the one thing CP4's discipline may never become",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(rotate_request(mine, candidate))
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{why}");
        }

        // The store refuses the deposit collision on its own too, which is
        // what keeps the class prefix from being the only thing standing
        // between two families and an ambiguous credential.
        let (_db, store) = test_store();
        store
            .upsert_family("fam-one", None, None, None, None, 1)
            .unwrap();
        store
            .upsert_family("fam-two", None, None, None, None, 1)
            .unwrap();
        let key = test_rotation_key();
        let taken_deposit = deposit_token_for("fam-two");
        assert_eq!(
            store
                .rotate_family_token(
                    "fam-one",
                    &taken_deposit,
                    &rotation_authority(&key, "fam-one", &taken_deposit)
                )
                .unwrap(),
            FamilyRotation::TokenTaken
        );
        assert_eq!(
            store
                .rotate_family_token(
                    "fam-one",
                    "fam-two",
                    &rotation_authority(&key, "fam-one", "fam-two")
                )
                .unwrap(),
            FamilyRotation::TokenTaken
        );
        // A refused rotation leaves the family as unregistered as it found
        // it: trust-on-first-rotation is spent by a rotation that commits,
        // not by one that was turned away.
        assert_eq!(
            store.get_family("fam-one").unwrap().unwrap().rotation_pk,
            None
        );
        // A family that is not in the table has nothing to re-key.
        let nope = minted_token("nope");
        assert_eq!(
            store
                .rotate_family_token(
                    "fam-static",
                    &nope,
                    &rotation_authority(&key, "fam-static", &nope)
                )
                .unwrap(),
            FamilyRotation::UnknownFamily
        );

        // And the family still works on the credential it started with.
        assert_eq!(
            app.clone()
                .oneshot(envelope_request(mine, 1, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    /// Read one family through the admin API by whichever handle is given.
    async fn admin_family(app: &Router, id_or_token: &str) -> Response {
        app.clone()
            .oneshot(admin_bare("GET", &format!("/admin/families/{id_or_token}")))
            .await
            .unwrap()
    }

    /// Trust on first rotation, the registering half: a family that has never
    /// rotated has no key to check against, so the first valid signature both
    /// authorizes the rotation and becomes the family's rotation authority
    /// from then on.
    #[tokio::test]
    async fn a_first_rotation_registers_the_key_that_signed_it() {
        let (_db, store) = test_store();
        let old = "fam-never-rotated-before";
        let new = minted_token("first");
        store.upsert_family(old, None, None, None, None, 1).unwrap();
        assert_eq!(
            store.get_family(old).unwrap().unwrap().rotation_pk,
            None,
            "a freshly provisioned family carries no rotation authority yet"
        );

        let key = test_rotation_key();
        match store
            .rotate_family_token(old, &new, &rotation_authority(&key, old, &new))
            .unwrap()
        {
            FamilyRotation::Rotated { family, .. } => assert_eq!(family.token, new),
            other => panic!("expected a rotation, got {other:?}"),
        }
        assert_eq!(
            store.get_family(&new).unwrap().unwrap().rotation_pk,
            Some(key.verifying_key().to_bytes().to_vec()),
            "the row now names the key that signed, and only that key"
        );
    }

    /// **The two halves of the contract, joined.**
    ///
    /// Every other rotation test here signs with this file's own
    /// `rotation_signed_bytes`, which proves the server is self-consistent and
    /// nothing more. The client is a different crate with its own copy of the
    /// domain string, the field names, the base64 alphabet and the framing, so
    /// "both sides agree" is exactly the claim those tests cannot make — and it
    /// is the claim that decides whether a real phone can rotate at all. A
    /// silent disagreement does not fail loudly in development; it ships, and
    /// then every revocation in the field ends in a 403 that looks like an
    /// attack.
    ///
    /// So this drives the real client encoder,
    /// `cruisemesh_core::relay_encode_rotate_request`, straight into the real
    /// route. Nothing is reconstructed by hand on either side.
    #[tokio::test]
    async fn the_core_clients_own_encoder_produces_a_request_this_route_accepts() {
        let app = admin_app();
        let old = "fam-before-the-core-client-rotates";
        let new = cruisemesh_core::core_mint_relay_member_token();
        provision(&app, old, serde_json::json!({})).await;

        // The person root, which is what §14.2 makes the rotation authority:
        // stable across every change of approving device, and the one key a
        // stolen phone never holds.
        let person = cruisemesh_core::generate_identity();
        let rotate = |bearer: String, current: String, next: String, sign_sk: Vec<u8>| {
            Request::builder()
                .method("POST")
                .uri("/family/rotate")
                .header("authorization", format!("Bearer {bearer}"))
                .header("content-type", "application/json")
                .body(Body::from(
                    cruisemesh_core::relay_encode_rotate_request(current, next, sign_sk)
                        .expect("the core client encodes a rotation"),
                ))
                .unwrap()
        };

        let response = app
            .clone()
            .oneshot(rotate(
                old.to_string(),
                old.to_string(),
                new.clone(),
                person.sign_sk.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        // The client's own decoder accepts what came back, so the round trip
        // is pinned end to end rather than only on the way in.
        let parsed = cruisemesh_core::relay_decode_rotate_response(
            serde_json::to_vec(&body_json(response).await).unwrap(),
            new.clone(),
        )
        .expect("the core client decodes the answer");
        assert_eq!(parsed.family_token, new);
        assert!(parsed.rotated);

        // The key the client presented is the one that got registered, proved
        // the only way that matters: a stranger's root is refused, and this
        // person's root still works.
        let stranger = cruisemesh_core::generate_identity();
        let response = app
            .clone()
            .oneshot(rotate(
                new.clone(),
                new.clone(),
                cruisemesh_core::core_mint_relay_member_token(),
                stranger.sign_sk,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "rotation_unauthorized");

        let third = cruisemesh_core::core_mint_relay_member_token();
        assert_eq!(
            app.oneshot(rotate(new.clone(), new, third, person.sign_sk.clone()))
                .await
                .unwrap()
                .status(),
            StatusCode::OK,
            "the person who registered the authority keeps it"
        );
    }

    /// Trust on first rotation, the closing half: once a key is registered a
    /// different one is refused outright, and the refusal changes nothing —
    /// the family still answers to the token it had.
    #[tokio::test]
    async fn a_registered_family_refuses_a_rotation_signed_by_another_key() {
        let app = admin_app();
        let first = "fam-that-has-rotated-once-already";
        let second = minted_token("second");
        provision(&app, first, serde_json::json!({})).await;
        assert_eq!(
            app.clone()
                .oneshot(rotate_request(first, &second))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // A second key, signing a perfectly well-formed rotation. This is the
        // revoked device: it holds the family's current member token, so
        // every check that rests on possession alone would pass.
        let response = app
            .clone()
            .oneshot(rotate_request_signed_by(
                second.as_str(),
                &minted_token("stolen"),
                &other_rotation_key(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "rotation_unauthorized");

        // Nothing moved: the family is still reachable on the credential it
        // held before the attempt, and the thief's candidate names nobody.
        assert_eq!(
            app.clone()
                .oneshot(envelope_request(&second, 1, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::OK,
            "the refused rotation left the family on its current token"
        );
        assert_eq!(
            app.clone()
                .oneshot(fetch_request(&minted_token("stolen")))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
    }

    /// **Trust on first *rotation*, not on first ask.**
    ///
    /// The cheapest possible attack on §10.2, and the one that costs the thief
    /// nothing: a revoked device holds the family's live member token, so it
    /// can present that token as *both* credentials, sign that pair with a key
    /// of its own, and — if the server registered on any valid signature —
    /// become the family's permanent rotation authority having rotated
    /// nothing at all. No race, no rotation, and nobody notices, because
    /// nothing about the family changed. The owner's genuine removal later
    /// gets `rotation_unauthorized`, the client gives up, and the removed
    /// phone keeps the mailbox forever.
    ///
    /// So a no-op call may not register, and the family stays open to the
    /// rotation its owner is about to make.
    #[tokio::test]
    async fn a_rotation_that_re_keys_nothing_cannot_claim_the_authority() {
        let app = admin_app();
        let held = "fam-never-rotated-by-anyone-yet";
        provision(&app, held, serde_json::json!({})).await;

        // The revoked device, asking to rotate the live token to itself.
        let response = app
            .clone()
            .oneshot(rotate_request_signed_by(held, held, &other_rotation_key()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(response).await["code"], "rotation_unauthorized");

        // And the owner's first real rotation still registers, which is the
        // half that proves the refusal above cost the family nothing.
        let new = minted_token("bytheowner");
        assert_eq!(
            app.clone()
                .oneshot(rotate_request(held, &new))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.clone()
                .oneshot(rotate_request_signed_by(
                    &new,
                    &minted_token("stolenafter"),
                    &other_rotation_key()
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN,
            "the owner's key is the registered one"
        );
        // The legitimate lost-answer retry is untouched: presenting the new
        // token as both credentials is what a client does when it never heard
        // the answer, and it is still answered idempotently.
        let response = app
            .clone()
            .oneshot(rotate_request_signed_by(&new, &new, &test_rotation_key()))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["family_token"], new);
        assert_eq!(body["rotated"], false);
    }

    /// The threat model in one test: the revoked device replays the member
    /// token it kept, with no rotation authority to offer. Every shape that
    /// replay can take is refused, and the family is untouched afterwards.
    #[tokio::test]
    async fn replaying_the_member_token_without_a_signature_cannot_rotate() {
        let app = admin_app();
        let held = "fam-the-revoked-device-still-holds";
        provision(&app, held, serde_json::json!({})).await;
        // Register an authority first, so the device is up against a real
        // stored key rather than an empty column.
        let current = minted_token("rotatedonce");
        assert_eq!(
            app.clone()
                .oneshot(rotate_request(held, &current))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        let candidate = minted_token("bythethief");
        let garbage_pk = encode_base64_field(&[0xAAu8; ROTATION_PK_LEN]);
        let garbage_sig = encode_base64_field(&[0x5Au8; ROTATION_SIG_LEN]);
        for (body, status, why) in [
            (
                serde_json::json!({ "new_token": candidate }),
                StatusCode::BAD_REQUEST,
                "no signature fields at all — a malformed request, not a \
                 failed authorization, so the client is told to fix its \
                 encoder rather than to go find a key",
            ),
            (
                serde_json::json!({
                    "new_token": candidate,
                    "rotation_pk": "not base64!!",
                    "rotation_sig": garbage_sig,
                }),
                StatusCode::BAD_REQUEST,
                "unparseable base64",
            ),
            (
                serde_json::json!({
                    "new_token": candidate,
                    "rotation_pk": encode_base64_field(&[0xAAu8; 16]),
                    "rotation_sig": garbage_sig,
                }),
                StatusCode::BAD_REQUEST,
                "a key of the wrong length",
            ),
            (
                serde_json::json!({
                    "new_token": candidate,
                    "rotation_pk": garbage_pk,
                    "rotation_sig": garbage_sig,
                }),
                StatusCode::FORBIDDEN,
                "well-formed bytes that are not this family's key and not a \
                 signature at all — the one case that is an authorization \
                 failure",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(rotate_request_with(&current, body))
                .await
                .unwrap();
            assert_eq!(response.status(), status, "{why}");
        }

        // And the family is exactly where it was.
        assert_eq!(
            app.clone()
                .oneshot(envelope_request(&current, 2, 48))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    /// The operator's side of a rotation. A family's token is not a stable
    /// name for it — the whole point of §10 step 2 is that the token changes
    /// — so the admin API answers to a stable id as well, and an operator who
    /// recorded that id can still find the family afterwards. Recording only
    /// the token, which is what the pass-issuing flow does today, keeps
    /// working for as long as that token is current.
    #[tokio::test]
    async fn a_rotated_family_is_still_reachable_by_its_stable_id() {
        let app = admin_app();
        let old = "fam-before-the-operator-looks";
        let new = minted_token("afterward");
        provision(&app, old, serde_json::json!({})).await;

        let provisioned = body_json(admin_family(&app, old).await).await;
        let family_id = provisioned["family_id"].as_str().unwrap().to_string();
        assert!(
            family_id.starts_with(FAMILY_ID_PREFIX),
            "the stable id is prefixed so it can never be mistaken for a token"
        );

        assert_eq!(
            app.clone()
                .oneshot(rotate_request(old, &new))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // The id survived the rotation and still names this family.
        let response = admin_family(&app, &family_id).await;
        assert_eq!(response.status(), StatusCode::OK);
        let by_id = body_json(response).await;
        assert_eq!(by_id["token"], new);
        assert_eq!(by_id["family_id"], family_id);

        // So does the current token, which is what the pass-issuing flow
        // holds.
        let response = admin_family(&app, &new).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["family_id"], family_id);

        // The retired token names nobody, which is the point of rotating it.
        assert_eq!(
            admin_family(&app, old).await.status(),
            StatusCode::NOT_FOUND
        );
    }

    /// The rotation bucket is not the family's request bucket, and that
    /// separation is what keeps the remedy reachable: the device being
    /// revoked can spend the family's shared request allowance at will, so a
    /// rotation charged to that bucket would be a rotation the thief can
    /// block.
    #[tokio::test]
    async fn a_family_out_of_request_allowance_can_still_rotate() {
        let store = RelayStore::open(":memory:").unwrap();
        let app = app(AppState::with_rate_limits(
            store,
            HashSet::new(),
            RateLimitConfig {
                // One request a minute, spent below by a single fetch.
                requests_per_min: 1,
                ..RateLimitConfig::default()
            },
        )
        .with_admin_token(Some(ADMIN_TOKEN.to_string())));
        let old = "fam-whose-allowance-is-gone";
        let new = minted_token("despiteit");
        provision(&app, old, serde_json::json!({})).await;

        assert_eq!(
            app.clone()
                .oneshot(fetch_request(old))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            app.clone()
                .oneshot(fetch_request(old))
                .await
                .unwrap()
                .status(),
            StatusCode::TOO_MANY_REQUESTS,
            "the family's shared request allowance is exhausted"
        );

        // The rotation goes through anyway.
        let response = app
            .clone()
            .oneshot(rotate_request(old, &new))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["rotated"], true);
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

    /// Shares are a fixed percentage of whatever quota a family has, so a
    /// per-family override scales both of them and the two always sit in the
    /// documented order.
    #[test]
    fn deposit_shares_are_fractions_of_the_family_quota() {
        for quota in [400u64, 1_000, DEFAULT_FAMILY_QUOTA_BYTES, u64::MAX] {
            let per_depositor = deposit_per_depositor_share_bytes(quota);
            let total = deposit_total_share_bytes(quota);
            assert!(per_depositor <= total, "quota {quota}");
            assert!(total < quota, "quota {quota}");
        }
        assert_eq!(deposit_per_depositor_share_bytes(400), 100);
        assert_eq!(deposit_total_share_bytes(400), 200);
        // Wide quotas must not overflow the percentage arithmetic.
        assert_eq!(deposit_total_share_bytes(u64::MAX), u64::MAX / 2);
    }

    /// The availability property this exists for: a friend card holding a
    /// family's deposit credential cannot fill that family's mailbox and
    /// leave the family's own phones unable to post.
    #[tokio::test]
    async fn one_depositor_cannot_spend_the_familys_whole_quota() {
        let app = admin_app();
        // 400-byte quota => 100-byte per-depositor share, 200-byte deposit
        // share in total.
        let response = app
            .clone()
            .oneshot(admin_json(
                "POST",
                "/admin/families",
                serde_json::json!({"token": "fam-share", "quota_bytes": 400}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let deposit = deposit_token_for("fam-share");

        // First friend-card post is comfortably inside the share.
        assert_eq!(
            app.clone()
                .oneshot(envelope_request(&deposit, 1, 80))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // The second would put this depositor at 160 of its 100-byte share.
        // It is refused with its own code -- the mailbox is 320 bytes short
        // of full, so calling this `family_quota_exceeded` would be false.
        let response = app
            .clone()
            .oneshot(envelope_request(&deposit, 2, 80))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INSUFFICIENT_STORAGE);
        let body = body_json(response).await;
        assert_eq!(body["code"], "depositor_share_exceeded");

        // ...and the family itself still reaches its full quota, which is the
        // whole point: 80 deposited + 300 of its own = 380 of 400.
        assert_eq!(
            app.clone()
                .oneshot(envelope_request("fam-share", 3, 300))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );

        // Only when the mailbox is genuinely full does anyone hear that.
        let response = app
            .oneshot(envelope_request("fam-share", 4, 40))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INSUFFICIENT_STORAGE);
        assert_eq!(body_json(response).await["code"], "family_quota_exceeded");
    }

    /// Several depositors against one family: each gets its own share, one
    /// running out does not touch another's, and the shares together stop at
    /// the deposit ceiling rather than growing into the family's half.
    ///
    /// Driven at the store, which is where the policy lives. Over HTTP today
    /// every friend card for a family carries the same derived deposit
    /// credential, so the *credential* is the finest depositor the relay can
    /// honestly distinguish; keying the accounting here means per-friend
    /// credentials, if they ever ship, get per-friend shares for free.
    #[test]
    fn distinct_depositors_get_independent_shares_under_a_shared_ceiling() {
        let (_db, store) = test_store();
        // 1,000-byte quota => 250 per depositor, 500 for all of them.
        let quota = 1_000u64;
        let post = |depositor: Option<&str>, msg_byte: u8, len: usize| {
            store
                .insert_envelope_with_quota(
                    "family-a",
                    sample_msg_id(msg_byte),
                    3,
                    sample_hint(1),
                    vec![msg_byte; len],
                    5_000,
                    1_000,
                    quota,
                    depositor,
                )
                .unwrap()
        };

        assert!(matches!(
            post(Some("cmdep1-one"), 1, 240),
            QuotaInsertResult::Stored { .. }
        ));
        // The same depositor again: its own share, and nothing else, is full.
        assert_eq!(
            post(Some("cmdep1-one"), 2, 240),
            QuotaInsertResult::DepositShareExceeded {
                scope: DepositShareScope::Depositor,
                usage_bytes: 240,
                share_bytes: 250,
            }
        );
        // A second depositor is entirely unaffected by the first.
        assert!(matches!(
            post(Some("cmdep1-two"), 3, 240),
            QuotaInsertResult::Stored { .. }
        ));
        // A third is inside its own share but hits the shared ceiling, and is
        // told so specifically -- this one is not its own doing.
        assert_eq!(
            post(Some("cmdep1-three"), 4, 240),
            QuotaInsertResult::DepositShareExceeded {
                scope: DepositShareScope::AllDepositors,
                usage_bytes: 480,
                share_bytes: 500,
            }
        );
        // However many depositors turn up, the family's own devices still
        // reach the full quota: 480 deposited + 500 of the family's own.
        assert!(matches!(
            post(None, 5, 500),
            QuotaInsertResult::Stored { .. }
        ));
        assert_eq!(store.family_sealed_bytes("family-a").unwrap(), 980);
    }

    /// The additive `envelopes.depositor` migration against a database seeded
    /// exactly as a relay running today would have left it: the rows survive
    /// untouched, they read as member class, and the family's depositors
    /// start from a clean share rather than being charged for history the
    /// relay never recorded.
    #[test]
    fn migration_adds_depositor_and_reads_existing_rows_as_member_class() {
        let db = NamedTempFile::new().unwrap();
        let path = db.path().to_str().unwrap().to_string();
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE envelopes (
                    id             INTEGER PRIMARY KEY AUTOINCREMENT,
                    family_token   TEXT NOT NULL,
                    msg_id         BLOB NOT NULL,
                    hop_ttl        INTEGER NOT NULL,
                    recipient_hint BLOB NOT NULL,
                    sealed         BLOB NOT NULL,
                    expiry_ms      INTEGER NOT NULL,
                    created_at_ms  INTEGER NOT NULL,
                    UNIQUE(family_token, msg_id)
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO envelopes
                    (family_token, msg_id, hop_ttl, recipient_hint, sealed,
                     expiry_ms, created_at_ms)
                 VALUES ('family-a', ?1, 3, ?2, ?3, 5000, 100)",
                params![sample_msg_id(1), sample_hint(1), vec![7u8; 150]],
            )
            .unwrap();
        }

        let store = RelayStore::open(&path).unwrap();
        // Nothing was rewritten or dropped by the migration.
        let rows = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], 0, 10, 1_000)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sealed, vec![7u8; 150]);
        assert_eq!(store.family_sealed_bytes("family-a").unwrap(), 150);

        // The pre-migration row reads as member class. Had it been guessed to
        // be a deposit, this 60-byte post would have been over the 100-byte
        // per-depositor share of a 400-byte quota before it started.
        let admitted = store
            .insert_envelope_with_quota(
                "family-a",
                sample_msg_id(2),
                3,
                sample_hint(1),
                vec![8u8; 60],
                5_000,
                1_000,
                400,
                Some("cmdep1-friend"),
            )
            .unwrap();
        assert!(matches!(admitted, QuotaInsertResult::Stored { .. }));

        // Reopening applies nothing further (idempotent), and the row the
        // migration defaulted still reads as member class.
        drop(store);
        let store = RelayStore::open(&path).unwrap();
        assert_eq!(store.family_sealed_bytes("family-a").unwrap(), 210);
        let depositors: Vec<String> = {
            let conn = store.conn.lock().unwrap();
            let mut stmt = conn
                .prepare("SELECT depositor FROM envelopes ORDER BY id")
                .unwrap();
            let rows = stmt
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            rows
        };
        assert_eq!(
            depositors,
            vec![MEMBER_DEPOSITOR.to_string(), "cmdep1-friend".to_string()]
        );
    }

    #[tokio::test]
    async fn fetch_and_ack_cardinality_caps_fail_before_dynamic_sql() {
        let app = test_app();
        let hint = encode_base64_field(&sample_hint(1));
        let hints = std::iter::repeat_n(hint, MAX_FETCH_HINTS + 1)
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
                            None,
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
    async fn identical_repost_by_msg_id_dedupes_to_the_same_row() {
        // The load-bearing legit case: a receipt retry or an envelope
        // re-upload posts the SAME sealed bytes under a stable msg_id. It must
        // stay a pure idempotent dedupe — one row, same id, and the longer hop
        // budget / later expiry win.
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
            .unwrap()
            .stored_id();
        let second = store
            .insert_envelope(
                "family-a",
                sample_msg_id(9),
                7,
                sample_hint(1),
                sample_sealed(2), // identical sealed — genuine re-upload
                9_000,
                2_000,
            )
            .unwrap();

        assert_eq!(second, InsertOutcome::Stored { id: first_id });
        assert_eq!(store.count_for_family("family-a").unwrap(), 1);

        let rows = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], 0, 10, 2_000)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hop_ttl, 7);
        assert_eq!(rows[0].expiry_ms, 9_000);
        assert_eq!(rows[0].sealed, sample_sealed(2));
    }

    #[tokio::test]
    async fn conflicting_content_under_one_msg_id_is_rejected_not_deduped() {
        // The dedup-poisoning guard: a second post reuses a msg_id already in
        // flight but carries DIFFERENT sealed bytes. The relay cannot know
        // which is authentic, so it keeps the first row byte-for-byte and
        // reports the conflict rather than returning a dedupe success. The
        // second poster must not be able to believe its content was stored,
        // and the first poster's content must survive untouched.
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
            .unwrap()
            .stored_id();
        let conflict = store
            .insert_envelope(
                "family-a",
                sample_msg_id(9),
                7,
                sample_hint(1),
                sample_sealed(99), // different sealed under the same id
                9_000,
                2_000,
            )
            .unwrap();

        assert_eq!(conflict, InsertOutcome::MsgIdConflict);
        assert_eq!(store.count_for_family("family-a").unwrap(), 1);

        // The stored row is entirely unchanged — not even the hop/expiry bump.
        let rows = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], 0, 10, 2_000)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, first_id);
        assert_eq!(rows[0].hop_ttl, 4);
        assert_eq!(rows[0].expiry_ms, 5_000);
        assert_eq!(rows[0].sealed, sample_sealed(2));
    }

    #[tokio::test]
    async fn quota_path_rejects_conflicting_content_and_dedupes_identical() {
        // The quota insert path is a separate code path from `insert_envelope`
        // and must enforce the same content rule.
        let (_db, store) = test_store();
        let quota = 1_000_000u64;
        let first = store
            .insert_envelope_with_quota(
                "family-a",
                sample_msg_id(9),
                4,
                sample_hint(1),
                sample_sealed(2),
                5_000,
                1_000,
                quota,
                None,
            )
            .unwrap();
        let first_id = match first {
            QuotaInsertResult::Stored { id } => id,
            other => panic!("expected stored, got {other:?}"),
        };

        // Identical re-post still dedupes to the same row.
        let dedupe = store
            .insert_envelope_with_quota(
                "family-a",
                sample_msg_id(9),
                7,
                sample_hint(1),
                sample_sealed(2),
                9_000,
                2_000,
                quota,
                None,
            )
            .unwrap();
        assert_eq!(dedupe, QuotaInsertResult::Stored { id: first_id });

        // Different sealed under the same id is a conflict, and the stored row
        // stays exactly as the first writer left it.
        let conflict = store
            .insert_envelope_with_quota(
                "family-a",
                sample_msg_id(9),
                7,
                sample_hint(1),
                sample_sealed(99),
                9_000,
                2_000,
                quota,
                None,
            )
            .unwrap();
        assert_eq!(conflict, QuotaInsertResult::MsgIdConflict);

        let rows = store
            .fetch_envelopes("family-a", vec![sample_hint(1)], 0, 10, 2_000)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sealed, sample_sealed(2));
        // The identical re-post's hop/expiry bump is kept; the conflict's is not.
        assert_eq!(rows[0].hop_ttl, 7);
        assert_eq!(rows[0].expiry_ms, 9_000);
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

    #[test]
    fn batch_insert_resolves_same_msg_id_by_content() {
        // DEDUP-01 for the bulk ingest path: an identical re-post dedupes and
        // takes the longer hop budget / later expiry, while a re-post carrying
        // different sealed bytes leaves the stored first-writer row untouched
        // and never overwrites it.
        let (_db, store) = test_store();
        let now = 1_700_000_000_000i64;
        let msg_id = sample_msg_id(9);
        let hint = sample_hint(1);
        let first = sample_sealed(1);
        let garbage = sample_sealed(2);

        // First write establishes the row with a modest hop budget/expiry.
        store
            .insert_envelopes_batch(&[(
                "family-a".to_string(),
                msg_id.clone(),
                3u8,
                hint.clone(),
                first.clone(),
                now + 10_000,
                now,
            )])
            .unwrap();

        // Identical re-post with a longer budget dedupes onto the same row;
        // a differing-content re-post under the same id is a no-op that does
        // NOT overwrite the stored bytes.
        store
            .insert_envelopes_batch(&[
                (
                    "family-a".to_string(),
                    msg_id.clone(),
                    7u8,
                    hint.clone(),
                    first.clone(),
                    now + 60_000,
                    now,
                ),
                (
                    "family-a".to_string(),
                    msg_id.clone(),
                    9u8,
                    hint.clone(),
                    garbage.clone(),
                    now + 999_000,
                    now,
                ),
            ])
            .unwrap();

        let stored = store
            .fetch_envelopes("family-a", vec![hint.clone()], 0, 100, now)
            .unwrap();
        assert_eq!(
            stored.len(),
            1,
            "one row survives, no second content admitted"
        );
        assert_eq!(
            stored[0].sealed, first,
            "first-writer content is never overwritten by a conflicting re-post"
        );
        assert_eq!(
            stored[0].hop_ttl, 7,
            "identical re-post takes the longer hop budget"
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

    /// The page budget also has a *floor*, set by the clients already in the
    /// field rather than by anything in this file.
    ///
    /// The builds shipped before the walk learned to continue past a short
    /// page ask for 16 rows and stop the moment a page comes back shorter than
    /// that — a short page is how they recognize the end of the mailbox.
    /// Truncating one of their asks would therefore not merely slow their walk
    /// down, it would silence it: everything newer than the truncation point
    /// goes unseen, with no error raised anywhere, until those rows expire.
    ///
    /// The budget is what keeps that from happening. At 16 times the largest
    /// envelope this server will accept, a 16-row ask cannot be cut by bytes
    /// no matter how large those 16 rows are, so those clients always get the
    /// whole window they asked for. Lowering either constant would break that
    /// silently, in the field, on phones nobody can update — hence a test
    /// rather than a comment.
    ///
    /// Retire this once the oldest build still in use ends a mailbox walk only
    /// on a genuinely empty page. Until then the budget may rise, never fall.
    #[test]
    fn the_page_budget_can_never_truncate_a_sixteen_row_ask() {
        const DEPLOYED_CLIENT_FETCH_LIMIT: usize = 16;
        const {
            assert!(
                MAX_FETCH_PAGE_SEALED_BYTES
                    >= DEPLOYED_CLIENT_FETCH_LIMIT * MAX_ENVELOPE_SEALED_BYTES
            )
        };
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

    /// The other direction of the same discipline: core mirrors this cap as
    /// `RELAY_MAX_FETCH_HINTS` because it is core that *builds* the hint set
    /// (this device's own §7 namespace included) and has to argue that a
    /// worst-case family fits inside it. Raising the server cap without the
    /// mirror — or the mirror without the server — fails here.
    #[test]
    fn fetch_hint_cap_matches_the_core() {
        assert_eq!(MAX_FETCH_HINTS, cruisemesh_core::RELAY_MAX_FETCH_HINTS);
    }

    /// The same discipline for the three budget numbers a per-DEVICE fan-out
    /// spends (`specs/multi-device-v1.md` §7): core plans one relay row per
    /// recipient device, each carrying the whole sealed body, so one message
    /// costs `fleet_size × sealed_len` against these server limits. Core
    /// mirrors them and argues the max-cap worst case against them
    /// (`a_max_cap_fanout_fits_the_family_relay_budget`); loosening one here
    /// without the mirror — or tightening one without re-running that argument
    /// — fails at this line instead of in a family's mailbox.
    #[test]
    fn family_budget_constants_match_the_core() {
        assert_eq!(
            MAX_ENVELOPE_SEALED_BYTES as u64,
            cruisemesh_core::RELAY_MAX_ENVELOPE_SEALED_BYTES
        );
        assert_eq!(
            DEFAULT_FAMILY_QUOTA_BYTES,
            cruisemesh_core::RELAY_FAMILY_QUOTA_BYTES
        );
        assert_eq!(
            DEFAULT_RATE_BYTES_PER_MIN,
            cruisemesh_core::RELAY_RATE_BYTES_PER_MIN
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
                    .unwrap()
                    .stored_id(),
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
            .unwrap()
            .stored_id();
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
            .unwrap()
            .stored_id();

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
            let mut family = FamilyBuckets::new(60, 60, (4, 900), (4, 3600), start);
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
