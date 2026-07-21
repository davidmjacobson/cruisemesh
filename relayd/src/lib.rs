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

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const RECIPIENT_HINT_LEN: usize = 8;
const MSG_ID_LEN: usize = 16;
const DEFAULT_FETCH_LIMIT: usize = 100;
const MAX_FETCH_LIMIT: usize = 500;
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

/// DESIGN.md §9: hard upper bound on how long a row may live on the relay.
/// Client-supplied `expiry_ms` (typically 7 days via core's
/// `DEFAULT_EXPIRY_MS`) is honored when tighter; this caps the rest.
pub const MAX_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

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

/// FR4: build-time version identifiers, embedded via Cargo (`VERSION`) and
/// `build.rs` (`GIT_SHA`) so `/healthz` and the startup log always reflect
/// the exact commit running -- there was previously no way to ask a
/// deployed relay which of master's several relayd-affecting changes
/// (`/presence`, D7 quotas, T4-09 limits, ...) it was actually running.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GIT_SHA: &str = env!("CRUISEMESH_GIT_SHA");

#[derive(Clone)]
pub struct AppState {
    store: RelayStore,
    auth_tokens: HashSet<String>,
    tx: tokio::sync::broadcast::Sender<std::sync::Arc<BroadcastEnvelope>>,
    family_quota_bytes: u64,
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

    pub fn with_config(
        store: RelayStore,
        auth_tokens: HashSet<String>,
        hub_capacity: usize,
        family_quota_bytes: u64,
    ) -> Self {
        let (tx, _) = tokio::sync::broadcast::channel(hub_capacity.max(1));
        Self {
            store,
            auth_tokens,
            tx,
            family_quota_bytes,
            admin_token: None,
        }
    }

    /// Builder: enable the `/admin/families` API with this bearer token.
    pub fn with_admin_token(mut self, admin_token: Option<String>) -> Self {
        self.admin_token = admin_token;
        self
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
}

impl RelayStore {
    pub fn open(path: &str) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(|e| e.to_string())?;
        conn.execute_batch(SCHEMA).map_err(|e| e.to_string())?;
        Ok(Self {
            conn: std::sync::Arc::new(Mutex::new(conn)),
        })
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
        self.prune_expired(now_ms)?;
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let hint_placeholders = (0..hints.len())
            .map(|index| format!("?{}", index + 3))
            .collect::<Vec<_>>()
            .join(",");
        let limit_placeholder = hints.len() + 3;
        // Content-agnostic: sealed is returned as-is; no kind/type filter.
        let sql = format!(
            "SELECT id, msg_id, hop_ttl, recipient_hint, sealed, expiry_ms, created_at_ms
             FROM envelopes
             WHERE family_token = ?1 AND id > ?2 AND recipient_hint IN ({hint_placeholders})
             ORDER BY id ASC
             LIMIT ?{limit_placeholder}"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let fetch_limit = limit.min(MAX_FETCH_LIMIT) as i64;
        let mut bindings: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(hints.len() + 3);
        bindings.push(&family_token);
        bindings.push(&after_id);
        for hint in &hints {
            bindings.push(hint);
        }
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
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())
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
    pub fn prune_expired(&self, now_ms: i64) -> Result<u64, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let pruned = prune_expired_on(&conn, now_ms)?;
        // FR2: only log when something actually happened -- this runs on
        // every fetch/presence-sync call, so a zero-count line would be the
        // dominant log entry and drown out everything else.
        if pruned > 0 {
            info!(pruned, "pruned expired envelope/presence rows");
        }
        Ok(pruned)
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
        conn.execute(
            "INSERT INTO families (token, status, plan, quota_bytes, created_ms, expires_ms, note)
             VALUES (?1, 'active', ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(token) DO UPDATE SET
                status = 'active',
                plan = excluded.plan,
                quota_bytes = excluded.quota_bytes,
                expires_ms = excluded.expires_ms,
                note = excluded.note",
            params![
                token,
                plan,
                quota_bytes.map(|q| q as i64),
                now_ms,
                expires_ms,
                note,
            ],
        )
        .map_err(|e| e.to_string())?;
        get_family_on(&conn, token)?.ok_or_else(|| "family vanished after upsert".to_string())
    }

    pub fn get_family(&self, token: &str) -> Result<Option<FamilyRow>, String> {
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        get_family_on(&conn, token)
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
    /// family+hint+id index is used instead of a table scan.
    pub fn explain_fetch_plan(
        &self,
        family_token: &str,
        hints: &[Vec<u8>],
        after_id: i64,
        limit: usize,
    ) -> Result<String, String> {
        if hints.is_empty() {
            return Ok(String::new());
        }
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let hint_placeholders = (0..hints.len())
            .map(|index| format!("?{}", index + 3))
            .collect::<Vec<_>>()
            .join(",");
        let limit_placeholder = hints.len() + 3;
        let sql = format!(
            "EXPLAIN QUERY PLAN
             SELECT id, msg_id, hop_ttl, recipient_hint, sealed, expiry_ms, created_at_ms
             FROM envelopes
             WHERE family_token = ?1 AND id > ?2 AND recipient_hint IN ({hint_placeholders})
             ORDER BY id ASC
             LIMIT ?{limit_placeholder}"
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let fetch_limit = limit.min(MAX_FETCH_LIMIT) as i64;
        let mut bindings: Vec<&dyn rusqlite::ToSql> = Vec::with_capacity(hints.len() + 3);
        bindings.push(&family_token);
        bindings.push(&after_id);
        for hint in hints {
            bindings.push(hint);
        }
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

fn get_family_on(conn: &Connection, token: &str) -> Result<Option<FamilyRow>, String> {
    conn.query_row(
        "SELECT token, status, plan, quota_bytes, created_ms, expires_ms, note
         FROM families WHERE token = ?1",
        params![token],
        |row| {
            Ok(FamilyRow {
                token: row.get(0)?,
                status: row.get(1)?,
                plan: row.get(2)?,
                quota_bytes: row.get::<_, Option<i64>>(3)?.map(|q| q as u64),
                created_ms: row.get(4)?,
                expires_ms: row.get(5)?,
                note: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
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

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/ws", get(ws_handler))
        .route("/envelopes", post(post_envelope).get(get_envelopes))
        .route("/envelopes/ack", post(ack_envelopes))
        .route("/presence", post(sync_presence))
        .route("/admin/families", post(admin_provision_family))
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
    let access = authorize_bearer(&state, &headers, FamilyOp::Post)?;
    let family_token = access.token.clone();
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
    let now = now_ms();

    // Dedupe, quota accounting, expiry pruning, and insertion are one store
    // transaction so concurrent posts cannot all pass the same usage check.
    let result = state
        .store
        .insert_envelope_with_quota(
            &family_token,
            msg_id.clone(),
            request.hop_ttl,
            recipient_hint.clone(),
            sealed.clone(),
            request.expiry_ms,
            now,
            access.quota_bytes,
        )
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
    let family_token = authorize_bearer(&state, &headers, FamilyOp::Read)?.token;
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
    let rows = state
        .store
        .fetch_envelopes(&family_token, hints, after, limit, now_ms())
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
    let family_token = authorize_bearer(&state, &headers, FamilyOp::Read)?.token;
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
        .ack_envelopes(&family_token, ids)
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
    let family_token = authorize_bearer(&state, &headers, FamilyOp::Read)?.token;
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
        .sync_presence(&family_token, &announce, &query, now)
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

fn family_response(state: &AppState, row: FamilyRow) -> Result<FamilyResponse, ApiError> {
    let usage_bytes = state
        .store
        .family_sealed_bytes(&row.token)
        .map_err(ApiError::internal)?;
    let envelope_count = state
        .store
        .count_for_family(&row.token)
        .map_err(ApiError::internal)?;
    Ok(FamilyResponse {
        effective_quota_bytes: row.quota_bytes.unwrap_or(state.family_quota_bytes),
        token: row.token,
        status: row.status,
        plan: row.plan,
        quota_bytes: row.quota_bytes,
        created_ms: row.created_ms,
        expires_ms: row.expires_ms,
        note: row.note,
        usage_bytes,
        envelope_count,
    })
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
    // delivery-only, so it counts as a Read op for expiry-grace purposes.
    let token = if let Ok(access) = authorize_bearer(&state, &headers, FamilyOp::Read) {
        access.token
    } else {
        let Some(t) = query.token.as_deref().filter(|t| !t.is_empty()) else {
            return Err(ApiError::unauthorized(
                "missing family token (Authorization: Bearer or ?token=)".to_string(),
            ));
        };
        authorize_family(&state, t, FamilyOp::Read, now_ms())?.token
    };
    let (hints, hints_base64) = decode_fetch_hints(&query.hints)?;
    let after = query.after.unwrap_or(0);
    if after < 0 {
        return Err(ApiError::bad_request(
            "after must be non-negative".to_string(),
        ));
    }

    Ok(ws
        .max_message_size(WS_MAX_INBOUND_MESSAGE_BYTES)
        .max_frame_size(WS_MAX_INBOUND_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_ws(socket, state, token, hints, hints_base64, after))
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

async fn handle_ws(
    mut socket: WebSocket,
    state: AppState,
    family_token: String,
    hints: Vec<Vec<u8>>,
    hints_base64: HashSet<String>,
    mut after: i64,
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
        let rows = match state.store.fetch_envelopes(
            &family_token,
            hints.clone(),
            after,
            DEFAULT_FETCH_LIMIT,
            now_ms(),
        ) {
            Ok(r) => r,
            Err(_) => return,
        };
        if rows.is_empty() {
            break;
        }
        let n = rows.len();
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
        if n < DEFAULT_FETCH_LIMIT {
            break;
        }
    }

    // --- Live push ---
    loop {
        tokio::select! {
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
                    // Client→server traffic ignored (acks are REST-only).
                    Some(Ok(_)) => {}
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

/// The result of authenticating a family bearer token: the token itself
/// plus the quota that applies to it (per-family override or server default).
struct FamilyAccess {
    token: String,
    quota_bytes: u64,
}

/// Resolve a family token against the static env allowlist first (implicit
/// always-active families, the self-hosted path — zero behavior change), then
/// the provisioned `families` table (status + expiry + per-family quota).
fn authorize_family(
    state: &AppState,
    token: &str,
    op: FamilyOp,
    now_ms: i64,
) -> Result<FamilyAccess, ApiError> {
    if state.auth_tokens.contains(token) {
        return Ok(FamilyAccess {
            token: token.to_string(),
            quota_bytes: state.family_quota_bytes,
        });
    }
    let Some(family) = state.store.get_family(token).map_err(ApiError::internal)? else {
        return Err(ApiError::unauthorized("unknown family token".to_string()));
    };
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
        token: family.token,
        quota_bytes: family.quota_bytes.unwrap_or(state.family_quota_bytes),
    })
}

fn authorize_bearer(
    state: &AppState,
    headers: &HeaderMap,
    op: FamilyOp,
) -> Result<FamilyAccess, ApiError> {
    let token = raw_bearer_token(headers)?;
    authorize_family(state, &token, op, now_ms())
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
}

impl ApiError {
    fn bad_request(message: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
            code: None,
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
        }
    }

    fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: "not found".to_string(),
            code: None,
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
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = match self.code {
            Some(code) => serde_json::json!({ "error": self.message, "code": code }),
            None => serde_json::json!({ "error": self.message }),
        };
        (self.status, Json(body)).into_response()
    }
}

const SCHEMA: &str = "
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
    token        TEXT PRIMARY KEY,
    status       TEXT NOT NULL DEFAULT 'active',
    plan         TEXT,
    quota_bytes  INTEGER,
    created_ms   INTEGER NOT NULL,
    expires_ms   INTEGER,
    note         TEXT
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

    #[tokio::test]
    async fn expired_rows_are_pruned_before_fetch() {
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
        assert_eq!(store.count_for_family("family-a").unwrap(), 1);
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
            .explain_fetch_plan("family-a", &[sample_hint(1)], 0, 50)
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
            .explain_fetch_plan("family-a", &[sample_hint(3)], 100, 100)
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
}
