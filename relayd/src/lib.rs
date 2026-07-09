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

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const RECIPIENT_HINT_LEN: usize = 8;
const MSG_ID_LEN: usize = 16;
const DEFAULT_FETCH_LIMIT: usize = 100;
const MAX_FETCH_LIMIT: usize = 500;

/// DESIGN.md §9: hard upper bound on how long a row may live on the relay.
/// Client-supplied `expiry_ms` (typically 7 days via core's
/// `DEFAULT_EXPIRY_MS`) is honored when tighter; this caps the rest.
pub const MAX_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Clone)]
pub struct AppState {
    store: RelayStore,
    auth_tokens: HashSet<String>,
}

impl AppState {
    pub fn new(store: RelayStore, auth_tokens: HashSet<String>) -> Self {
        Self { store, auth_tokens }
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

    /// Bulk insert inside a single transaction (index/plan benchmarks).
    pub fn insert_envelopes_batch(
        &self,
        rows: &[(String, Vec<u8>, u8, Vec<u8>, Vec<u8>, i64, i64)],
    ) -> Result<(), String> {
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
        let retention_floor = now_ms.saturating_sub(MAX_RETENTION_MS);
        let conn = self.conn.lock().expect("relay store mutex poisoned");
        let deleted = conn
            .execute(
                "DELETE FROM envelopes
                 WHERE expiry_ms <= ?1 OR created_at_ms <= ?2",
                params![now_ms, retention_floor],
            )
            .map_err(|e| e.to_string())?;
        Ok(deleted as u64)
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

pub fn app(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/envelopes", post(post_envelope).get(get_envelopes))
        .route("/envelopes/ack", post(ack_envelopes))
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

#[derive(Serialize)]
struct HealthzResponse {
    status: &'static str,
}

async fn healthz() -> Json<HealthzResponse> {
    Json(HealthzResponse { status: "ok" })
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
    let family_token = bearer_token(&headers, &state.auth_tokens)?;
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
        return Err(ApiError::bad_request("sealed must not be empty".to_string()));
    }
    let id = state
        .store
        .insert_envelope(
            &family_token,
            msg_id,
            request.hop_ttl,
            recipient_hint,
            sealed,
            request.expiry_ms,
            now_ms(),
        )
        .map_err(ApiError::internal)?;
    Ok(Json(PostEnvelopeResponse { id }))
}

#[derive(Deserialize)]
struct GetEnvelopesQuery {
    hints: String,
    after: Option<i64>,
    limit: Option<usize>,
}

#[derive(Serialize)]
struct EnvelopeResponse {
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
    let family_token = bearer_token(&headers, &state.auth_tokens)?;
    let hints = query
        .hints
        .split(',')
        .filter(|h| !h.is_empty())
        .map(|hint| decode_base64_field(hint, "hints"))
        .collect::<Result<Vec<_>, _>>()?;
    if hints.is_empty() {
        return Err(ApiError::bad_request("at least one hint is required".to_string()));
    }
    for hint in &hints {
        if hint.len() != RECIPIENT_HINT_LEN {
            return Err(ApiError::bad_request(format!(
                "each hint must be {RECIPIENT_HINT_LEN} bytes after base64url decoding"
            )));
        }
    }
    let after = query.after.unwrap_or(0);
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
    let family_token = bearer_token(&headers, &state.auth_tokens)?;
    let deleted = state
        .store
        .ack_envelopes(&family_token, request.ids)
        .map_err(ApiError::internal)?;
    Ok(Json(AckResponse { deleted }))
}

fn decode_base64_field(value: &str, field: &str) -> Result<Vec<u8>, ApiError> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ApiError::bad_request(format!("{field} must be base64url without padding")))
}

fn encode_base64_field(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn bearer_token(headers: &HeaderMap, allowed_tokens: &HashSet<String>) -> Result<String, ApiError> {
    let auth = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::unauthorized("missing Authorization header".to_string()))?;
    let token = auth
        .strip_prefix("Bearer ")
        .ok_or_else(|| ApiError::unauthorized("Authorization must be Bearer <token>".to_string()))?;
    if !allowed_tokens.contains(token) {
        return Err(ApiError::unauthorized("unknown family token".to_string()));
    }
    Ok(token.to_string())
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
}

impl ApiError {
    fn bad_request(message: String) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message,
        }
    }

    fn unauthorized(message: String) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message,
        }
    }

    fn internal(message: String) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
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
        let db = NamedTempFile::new().unwrap();
        let store = RelayStore::open(db.path().to_str().unwrap()).unwrap();
        app(AppState::new(
            store,
            HashSet::from(["family-a".to_string(), "family-b".to_string()]),
        ))
    }

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
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
        let deleted = store
            .ack_envelopes("family-a", vec![first[0].id])
            .unwrap();
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
        assert_eq!(RelayStore::effective_expiry(created, seven_days), seven_days);
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
}
