//! Bounded, restart-safe bookkeeping for partial blob transfers.
//!
//! # Files or blobs?
//!
//! The spec is explicit about where a *finished* blob goes ("received blobs
//! live in the media store like any received photo/video — the user's own
//! space") and explicit that partial chunk sets have "a bounded budget,
//! garbage-collected oldest-first". It says nothing about the mechanism, so
//! this module chooses, and the choice is: **ciphertext lives in files; only
//! metadata lives in SQLite.**
//!
//! A 128 MB blob written into the message database would put a video inside
//! every backup, every `VACUUM`, and every restore-sanitization pass — three
//! places that exist to move small, sensitive records around and would each
//! grow by two orders of magnitude for a feature the message plane is
//! deliberately not carrying. Files also let the chunk writer append without
//! a transaction, which keeps `TXN-01` trivially true of the bulk path.
//!
//! Core never opens a file. It records the chunk file's *name*, decides when
//! a file should stop existing, and hands that decision to a driver as a
//! typed plan ([`EvictionPlan`]). Byte-moving is platform work; deciding is
//! not.
//!
//! # The budget, and what it will not evict
//!
//! [`super::MEDIA_PARTIAL_BUDGET_BYTES`] bounds the ciphertext held for blobs
//! that are **not yet verified**. When the budget is exceeded, rows are
//! evicted least-recently-used first — and two classes are never evicted, at
//! any pressure:
//!
//! * a blob whose transfer is **active**. Evicting under a live transfer
//!   would delete the file being appended to and turn progress into a loop.
//! * a blob whose manifest message is still **unread**. The person has not
//!   opened the conversation yet; throwing away the download they are about
//!   to look at is the one eviction that is guaranteed to be wrong.
//!
//! A verified blob is not charged to the budget at all: it is finished, and
//! it belongs to the platform media store from that moment. Integration calls
//! [`BlobStore::forget`] once the handoff is done. Until then it is neither
//! counted nor evictable, because deleting a complete download to make room
//! for an incomplete one is never the better trade.
//!
//! If protected rows alone exceed the budget, eviction evicts everything it
//! legitimately can and reports the overshoot rather than breaking a rule to
//! meet a number.
//!
//! # Where these tables live
//!
//! [`MEDIA_SCHEMA_SQL`] is `CREATE TABLE IF NOT EXISTS`, matching the rest of
//! the store's migration style, and every function here takes a borrowed
//! `Connection`. The integration phase applies it to `MessageStore`'s own
//! connection so this metadata sits in the one database the app already
//! backs up; this module does not edit `store.rs` while it is dark.

use rusqlite::{params, Connection, OptionalExtension};

use super::bitmap::ChunkBitmap;
use super::blob::{BlobGeometry, BlobId};
use super::{MediaError, MEDIA_PARTIAL_BUDGET_BYTES};

pub const MEDIA_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS media_blobs (
    blob_id BLOB PRIMARY KEY,
    plaintext_bytes INTEGER NOT NULL,
    ciphertext_bytes INTEGER NOT NULL,
    chunk_count INTEGER NOT NULL,
    bitmap BLOB NOT NULL,
    bytes_present INTEGER NOT NULL,
    verified INTEGER NOT NULL DEFAULT 0,
    transfer_active INTEGER NOT NULL DEFAULT 0,
    manifest_unread INTEGER NOT NULL DEFAULT 1,
    chunk_file TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    last_used_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_media_blobs_lru
    ON media_blobs(verified, transfer_active, manifest_unread, last_used_at_ms);
";

/// One tracked blob.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobRecord {
    pub blob_id: BlobId,
    pub geometry: BlobGeometry,
    pub bytes_present: u64,
    pub chunks_present: u32,
    pub complete: bool,
    pub verified: bool,
    pub transfer_active: bool,
    pub manifest_unread: bool,
    pub chunk_file: String,
    pub created_at_ms: i64,
    pub last_used_at_ms: i64,
}

/// What recording one chunk changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChunkOutcome {
    /// False for a duplicate. Progress is monotone, so a duplicate is not
    /// progress and does not move the byte counter.
    pub newly_present: bool,
    pub chunks_present: u32,
    pub bytes_present: u64,
    pub complete: bool,
}

/// One blob an eviction would drop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvictedBlob {
    pub blob_id: BlobId,
    /// The file the driver deletes. Core never touches it.
    pub chunk_file: String,
    pub bytes_reclaimed: u64,
    pub last_used_at_ms: i64,
}

/// A deterministic eviction decision under an explicit clock.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvictionPlan {
    pub evicted: Vec<EvictedBlob>,
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// Charged bytes that no rule allowed evicting. Non-zero here with
    /// `bytes_after` still over budget is the honest report of a budget that
    /// could not be met without breaking a protection.
    pub protected_bytes: u64,
}

impl EvictionPlan {
    pub fn is_empty(&self) -> bool {
        self.evicted.is_empty()
    }
}

/// Metadata for partial blob transfers, over a borrowed connection.
pub struct BlobStore<'a> {
    conn: &'a Connection,
}

impl<'a> BlobStore<'a> {
    /// Apply the schema and return a handle. Idempotent.
    pub fn open(conn: &'a Connection) -> Result<Self, MediaError> {
        conn.execute_batch(MEDIA_SCHEMA_SQL).map_err(store_err)?;
        Ok(BlobStore { conn })
    }

    /// Start (or re-attach to) a transfer. Idempotent: a second call for a
    /// blob already tracked returns the existing row untouched, so a restart
    /// mid-transfer resumes rather than resetting a bitmap.
    pub fn begin(
        &self,
        blob_id: &BlobId,
        geometry: &BlobGeometry,
        chunk_file: &str,
        now_ms: i64,
    ) -> Result<BlobRecord, MediaError> {
        if let Some(existing) = self.record(blob_id)? {
            if existing.geometry != *geometry {
                return Err(MediaError::Malformed(
                    "a tracked blob cannot change geometry".into(),
                ));
            }
            return Ok(existing);
        }
        let bitmap = ChunkBitmap::empty(geometry.chunk_count)?;
        self.conn
            .execute(
                "INSERT INTO media_blobs (blob_id, plaintext_bytes, ciphertext_bytes, chunk_count, \
                 bitmap, bytes_present, verified, transfer_active, manifest_unread, chunk_file, \
                 created_at_ms, last_used_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, 0, 0, 1, ?6, ?7, ?7)",
                params![
                    blob_id.as_bytes().to_vec(),
                    geometry.plaintext_bytes as i64,
                    geometry.ciphertext_bytes as i64,
                    geometry.chunk_count as i64,
                    bitmap.as_bytes().to_vec(),
                    chunk_file,
                    now_ms,
                ],
            )
            .map_err(store_err)?;
        self.record(blob_id)?
            .ok_or_else(|| MediaError::Store("inserted row vanished".into()))
    }

    pub fn record(&self, blob_id: &BlobId) -> Result<Option<BlobRecord>, MediaError> {
        self.conn
            .query_row(
                "SELECT plaintext_bytes, bitmap, bytes_present, verified, transfer_active, \
                 manifest_unread, chunk_file, created_at_ms, last_used_at_ms \
                 FROM media_blobs WHERE blob_id = ?1",
                params![blob_id.as_bytes().to_vec()],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                        row.get::<_, i64>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                    ))
                },
            )
            .optional()
            .map_err(store_err)?
            .map(|row| {
                let geometry = BlobGeometry::for_plaintext_len(row.0 as u64)?;
                let bitmap = ChunkBitmap::from_bytes(geometry.chunk_count, &row.1)?;
                Ok(BlobRecord {
                    blob_id: *blob_id,
                    geometry,
                    bytes_present: row.2 as u64,
                    chunks_present: bitmap.present_count(),
                    complete: bitmap.is_complete(),
                    verified: row.3 != 0,
                    transfer_active: row.4 != 0,
                    manifest_unread: row.5 != 0,
                    chunk_file: row.6,
                    created_at_ms: row.7,
                    last_used_at_ms: row.8,
                })
            })
            .transpose()
    }

    pub fn bitmap(&self, blob_id: &BlobId) -> Result<Option<ChunkBitmap>, MediaError> {
        let Some(record) = self.record(blob_id)? else {
            return Ok(None);
        };
        let bytes: Vec<u8> = self
            .conn
            .query_row(
                "SELECT bitmap FROM media_blobs WHERE blob_id = ?1",
                params![blob_id.as_bytes().to_vec()],
                |row| row.get(0),
            )
            .map_err(store_err)?;
        Ok(Some(ChunkBitmap::from_bytes(
            record.geometry.chunk_count,
            &bytes,
        )?))
    }

    /// Record an **authenticated** chunk. Callers must have opened the chunk
    /// with its blob key first: this function is bookkeeping, and a chunk
    /// that failed authentication must never reach it.
    pub fn record_chunk(
        &self,
        blob_id: &BlobId,
        index: u32,
        now_ms: i64,
    ) -> Result<ChunkOutcome, MediaError> {
        let record = self.require(blob_id)?;
        let mut bitmap = self
            .bitmap(blob_id)?
            .ok_or_else(|| MediaError::Store("bitmap vanished".into()))?;
        let chunk_bytes = u64::from(
            record
                .geometry
                .chunk_ciphertext_len(index)
                .ok_or_else(|| MediaError::Malformed(format!("chunk {index} is past the end")))?,
        );
        let newly_present = bitmap.set(index);
        let bytes_present = if newly_present {
            record.bytes_present + chunk_bytes
        } else {
            record.bytes_present
        };
        self.write_bitmap(blob_id, &bitmap, bytes_present, now_ms)?;
        Ok(ChunkOutcome {
            newly_present,
            chunks_present: bitmap.present_count(),
            bytes_present,
            complete: bitmap.is_complete(),
        })
    }

    /// The corrupted-chunk recovery path: re-mark a chunk missing and clear
    /// any verification. The blob plane never keeps a chunk that failed a
    /// check, and it never advances progress by keeping it.
    pub fn record_corrupt_chunk(
        &self,
        blob_id: &BlobId,
        index: u32,
        now_ms: i64,
    ) -> Result<ChunkOutcome, MediaError> {
        let record = self.require(blob_id)?;
        let mut bitmap = self
            .bitmap(blob_id)?
            .ok_or_else(|| MediaError::Store("bitmap vanished".into()))?;
        let chunk_bytes = u64::from(
            record
                .geometry
                .chunk_ciphertext_len(index)
                .ok_or_else(|| MediaError::Malformed(format!("chunk {index} is past the end")))?,
        );
        let cleared = bitmap.clear(index);
        let bytes_present = if cleared {
            record.bytes_present.saturating_sub(chunk_bytes)
        } else {
            record.bytes_present
        };
        self.write_bitmap(blob_id, &bitmap, bytes_present, now_ms)?;
        self.conn
            .execute(
                "UPDATE media_blobs SET verified = 0 WHERE blob_id = ?1",
                params![blob_id.as_bytes().to_vec()],
            )
            .map_err(store_err)?;
        Ok(ChunkOutcome {
            newly_present: false,
            chunks_present: bitmap.present_count(),
            bytes_present,
            complete: bitmap.is_complete(),
        })
    }

    /// A whole-blob digest failure. Every chunk goes back to missing: the
    /// assembled bytes did not match the manifest, and there is no way to
    /// tell from the digest alone which chunk lied.
    pub fn record_failed_verification(
        &self,
        blob_id: &BlobId,
        now_ms: i64,
    ) -> Result<ChunkOutcome, MediaError> {
        let record = self.require(blob_id)?;
        let bitmap = ChunkBitmap::empty(record.geometry.chunk_count)?;
        self.write_bitmap(blob_id, &bitmap, 0, now_ms)?;
        self.conn
            .execute(
                "UPDATE media_blobs SET verified = 0 WHERE blob_id = ?1",
                params![blob_id.as_bytes().to_vec()],
            )
            .map_err(store_err)?;
        Ok(ChunkOutcome {
            newly_present: false,
            chunks_present: 0,
            bytes_present: 0,
            complete: false,
        })
    }

    /// Mark a blob verified against its manifest digest. Refuses while chunks
    /// are missing, so `BLOB-05` cannot be satisfied by an optimistic caller.
    pub fn mark_verified(&self, blob_id: &BlobId, now_ms: i64) -> Result<(), MediaError> {
        let record = self.require(blob_id)?;
        if !record.complete {
            return Err(MediaError::Malformed(
                "an incomplete blob cannot be verified".into(),
            ));
        }
        self.set_flag(blob_id, "verified", true, now_ms)
    }

    pub fn set_transfer_active(
        &self,
        blob_id: &BlobId,
        active: bool,
        now_ms: i64,
    ) -> Result<(), MediaError> {
        self.set_flag(blob_id, "transfer_active", active, now_ms)
    }

    pub fn set_manifest_unread(
        &self,
        blob_id: &BlobId,
        unread: bool,
        now_ms: i64,
    ) -> Result<(), MediaError> {
        self.set_flag(blob_id, "manifest_unread", unread, now_ms)
    }

    /// Move a blob to the front of the LRU without changing anything else.
    pub fn touch(&self, blob_id: &BlobId, now_ms: i64) -> Result<(), MediaError> {
        self.require(blob_id)?;
        self.conn
            .execute(
                "UPDATE media_blobs SET last_used_at_ms = ?2 WHERE blob_id = ?1",
                params![blob_id.as_bytes().to_vec(), now_ms],
            )
            .map_err(store_err)?;
        Ok(())
    }

    /// Drop a row the caller has finished with — after handing a verified
    /// blob to the platform media store, or after applying an eviction.
    pub fn forget(&self, blob_id: &BlobId) -> Result<bool, MediaError> {
        let removed = self
            .conn
            .execute(
                "DELETE FROM media_blobs WHERE blob_id = ?1",
                params![blob_id.as_bytes().to_vec()],
            )
            .map_err(store_err)?;
        Ok(removed > 0)
    }

    /// Ciphertext bytes held for unverified blobs — what the budget counts.
    pub fn charged_bytes(&self) -> Result<u64, MediaError> {
        let total: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(bytes_present), 0) FROM media_blobs WHERE verified = 0",
                [],
                |row| row.get(0),
            )
            .map_err(store_err)?;
        Ok(total.max(0) as u64)
    }

    /// Decide what to evict. Pure decision: no file is touched, no row is
    /// deleted. Deterministic under a fixed clock — least-recently-used
    /// first, blob id breaking a tie — so the same store always produces the
    /// same plan.
    pub fn plan_eviction(&self, budget_bytes: u64) -> Result<EvictionPlan, MediaError> {
        let bytes_before = self.charged_bytes()?;
        let mut plan = EvictionPlan {
            bytes_before,
            bytes_after: bytes_before,
            ..EvictionPlan::default()
        };
        plan.protected_bytes = self.protected_bytes()?;
        if bytes_before <= budget_bytes {
            return Ok(plan);
        }

        let mut statement = self
            .conn
            .prepare(
                "SELECT blob_id, chunk_file, bytes_present, last_used_at_ms FROM media_blobs \
                 WHERE verified = 0 AND transfer_active = 0 AND manifest_unread = 0 \
                 ORDER BY last_used_at_ms ASC, blob_id ASC",
            )
            .map_err(store_err)?;
        let candidates = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })
            .map_err(store_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(store_err)?;

        for (id_bytes, chunk_file, bytes, last_used_at_ms) in candidates {
            if plan.bytes_after <= budget_bytes {
                break;
            }
            let bytes = bytes.max(0) as u64;
            plan.bytes_after = plan.bytes_after.saturating_sub(bytes);
            plan.evicted.push(EvictedBlob {
                blob_id: BlobId::from_slice(&id_bytes)?,
                chunk_file,
                bytes_reclaimed: bytes,
                last_used_at_ms,
            });
        }
        Ok(plan)
    }

    /// Plan against the standard device budget.
    pub fn plan_default_eviction(&self) -> Result<EvictionPlan, MediaError> {
        self.plan_eviction(MEDIA_PARTIAL_BUDGET_BYTES)
    }

    /// Apply a plan's row deletions. The files named in the plan are the
    /// driver's to delete; core never opens one.
    pub fn apply_eviction(&self, plan: &EvictionPlan) -> Result<u32, MediaError> {
        let mut removed = 0;
        for evicted in &plan.evicted {
            if self.forget(&evicted.blob_id)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn protected_bytes(&self) -> Result<u64, MediaError> {
        let total: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(SUM(bytes_present), 0) FROM media_blobs \
                 WHERE verified = 0 AND (transfer_active != 0 OR manifest_unread != 0)",
                [],
                |row| row.get(0),
            )
            .map_err(store_err)?;
        Ok(total.max(0) as u64)
    }

    fn require(&self, blob_id: &BlobId) -> Result<BlobRecord, MediaError> {
        self.record(blob_id)?
            .ok_or_else(|| MediaError::Store(format!("no blob {}", blob_id.short())))
    }

    fn write_bitmap(
        &self,
        blob_id: &BlobId,
        bitmap: &ChunkBitmap,
        bytes_present: u64,
        now_ms: i64,
    ) -> Result<(), MediaError> {
        self.conn
            .execute(
                "UPDATE media_blobs SET bitmap = ?2, bytes_present = ?3, last_used_at_ms = ?4 \
                 WHERE blob_id = ?1",
                params![
                    blob_id.as_bytes().to_vec(),
                    bitmap.as_bytes().to_vec(),
                    bytes_present as i64,
                    now_ms,
                ],
            )
            .map_err(store_err)?;
        Ok(())
    }

    fn set_flag(
        &self,
        blob_id: &BlobId,
        column: &str,
        value: bool,
        now_ms: i64,
    ) -> Result<(), MediaError> {
        self.require(blob_id)?;
        // `column` is never caller-supplied: every call site passes one of
        // three literals below, so no user data reaches this string.
        debug_assert!(matches!(
            column,
            "verified" | "transfer_active" | "manifest_unread"
        ));
        let sql = format!(
            "UPDATE media_blobs SET {column} = ?2, last_used_at_ms = ?3 WHERE blob_id = ?1"
        );
        self.conn
            .execute(
                &sql,
                params![blob_id.as_bytes().to_vec(), i64::from(value), now_ms],
            )
            .map_err(store_err)?;
        Ok(())
    }
}

fn store_err(err: rusqlite::Error) -> MediaError {
    MediaError::Store(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::blob::{seal_blob, test_key, BLOB_ID_LEN};

    fn conn() -> Connection {
        Connection::open_in_memory().expect("in-memory sqlite")
    }

    fn blob_id(seed: u8) -> BlobId {
        BlobId([seed; BLOB_ID_LEN])
    }

    /// A geometry whose ciphertext is `chunks` full chunks.
    fn geometry(chunks: u64) -> BlobGeometry {
        BlobGeometry::for_plaintext_len(
            chunks * u64::from(super::super::MEDIA_CHUNK_PLAINTEXT_BYTES),
        )
        .unwrap()
    }

    #[test]
    fn a_transfer_survives_a_restart_and_resumes_from_its_bitmap() {
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        let id = blob_id(1);
        store
            .begin(&id, &geometry(4), "blob-1.part", 1_000)
            .unwrap();
        store.record_chunk(&id, 0, 1_100).unwrap();
        store.record_chunk(&id, 2, 1_200).unwrap();

        // "Restart": a new handle over the same database.
        let reopened = BlobStore::open(&db).unwrap();
        let record = reopened.record(&id).unwrap().unwrap();
        assert_eq!(record.chunks_present, 2);
        assert!(!record.complete);
        let bitmap = reopened.bitmap(&id).unwrap().unwrap();
        assert!(bitmap.has(0) && bitmap.has(2) && !bitmap.has(1));
        assert_eq!(
            bitmap.missing_ranges(64, 8),
            vec![
                crate::media::bitmap::ChunkRange { start: 1, count: 1 },
                crate::media::bitmap::ChunkRange { start: 3, count: 1 },
            ]
        );

        // And beginning again is a resume, not a reset.
        let again = reopened
            .begin(&id, &geometry(4), "blob-1.part", 2_000)
            .unwrap();
        assert_eq!(again.chunks_present, 2);
    }

    #[test]
    fn progress_is_monotone_and_duplicates_cost_nothing() {
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        let id = blob_id(2);
        let geo = geometry(3);
        store.begin(&id, &geo, "blob-2.part", 10).unwrap();

        let first = store.record_chunk(&id, 1, 20).unwrap();
        assert!(first.newly_present);
        assert_eq!(first.bytes_present, u64::from(geo.chunk_ciphertext_bytes));

        let duplicate = store.record_chunk(&id, 1, 30).unwrap();
        assert!(!duplicate.newly_present);
        assert_eq!(duplicate.bytes_present, first.bytes_present);
        assert_eq!(duplicate.chunks_present, 1);
    }

    #[test]
    fn a_corrupt_chunk_goes_back_to_missing_and_never_counts() {
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        let id = blob_id(3);
        store.begin(&id, &geometry(2), "blob-3.part", 10).unwrap();
        store.record_chunk(&id, 0, 20).unwrap();
        store.record_chunk(&id, 1, 30).unwrap();
        store.mark_verified(&id, 40).unwrap();

        let outcome = store.record_corrupt_chunk(&id, 1, 50).unwrap();
        assert!(!outcome.complete);
        assert_eq!(outcome.chunks_present, 1);
        let record = store.record(&id).unwrap().unwrap();
        assert!(!record.verified, "a corrupt chunk revokes verification");
        assert!(!store.bitmap(&id).unwrap().unwrap().has(1));
    }

    #[test]
    fn a_failed_digest_discards_the_whole_blob() {
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        let id = blob_id(4);
        store.begin(&id, &geometry(3), "blob-4.part", 10).unwrap();
        for index in 0..3 {
            store.record_chunk(&id, index, 20).unwrap();
        }
        let outcome = store.record_failed_verification(&id, 30).unwrap();
        assert_eq!(outcome.chunks_present, 0);
        assert_eq!(outcome.bytes_present, 0);
        assert_eq!(store.charged_bytes().unwrap(), 0);
    }

    #[test]
    fn an_incomplete_blob_cannot_be_marked_verified() {
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        let id = blob_id(5);
        store.begin(&id, &geometry(2), "blob-5.part", 10).unwrap();
        store.record_chunk(&id, 0, 20).unwrap();
        assert!(store.mark_verified(&id, 30).is_err());
    }

    #[test]
    fn a_verified_blob_is_not_charged_to_the_partial_budget() {
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        let id = blob_id(6);
        store.begin(&id, &geometry(1), "blob-6.part", 10).unwrap();
        store.record_chunk(&id, 0, 20).unwrap();
        assert!(store.charged_bytes().unwrap() > 0);
        store.mark_verified(&id, 30).unwrap();
        assert_eq!(
            store.charged_bytes().unwrap(),
            0,
            "a finished download belongs to the media store, not to this budget"
        );
    }

    /// Fill a store with `count` blobs of one chunk each, one per second.
    fn seed(store: &BlobStore, count: u8) {
        for seed in 0..count {
            let id = blob_id(seed);
            store
                .begin(
                    &id,
                    &geometry(1),
                    &format!("blob-{seed}.part"),
                    1_000 + i64::from(seed),
                )
                .unwrap();
            store.record_chunk(&id, 0, 1_000 + i64::from(seed)).unwrap();
            store
                .set_manifest_unread(&id, false, 1_000 + i64::from(seed))
                .unwrap();
        }
    }

    #[test]
    fn eviction_is_least_recently_used_first_and_deterministic() {
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        seed(&store, 5);
        let chunk = geometry(1).ciphertext_bytes;

        // Budget for three of the five.
        let plan = store.plan_eviction(chunk * 3).unwrap();
        assert_eq!(plan.bytes_before, chunk * 5);
        assert_eq!(plan.bytes_after, chunk * 3);
        assert_eq!(
            plan.evicted
                .iter()
                .map(|e| e.chunk_file.clone())
                .collect::<Vec<_>>(),
            vec!["blob-0.part".to_string(), "blob-1.part".to_string()],
            "oldest use goes first"
        );
        assert_eq!(
            plan,
            store.plan_eviction(chunk * 3).unwrap(),
            "planning twice is the same plan"
        );

        // Planning alone changes nothing.
        assert_eq!(store.charged_bytes().unwrap(), chunk * 5);
        assert_eq!(store.apply_eviction(&plan).unwrap(), 2);
        assert_eq!(store.charged_bytes().unwrap(), chunk * 3);
        assert!(store.record(&blob_id(0)).unwrap().is_none());
        assert!(store.record(&blob_id(4)).unwrap().is_some());
    }

    #[test]
    fn touching_a_blob_moves_it_out_of_the_eviction_line() {
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        seed(&store, 4);
        let chunk = geometry(1).ciphertext_bytes;

        store.touch(&blob_id(0), 9_000).unwrap();
        let plan = store.plan_eviction(chunk * 2).unwrap();
        assert_eq!(
            plan.evicted
                .iter()
                .map(|e| e.chunk_file.clone())
                .collect::<Vec<_>>(),
            vec!["blob-1.part".to_string(), "blob-2.part".to_string()]
        );
    }

    #[test]
    fn eviction_never_takes_an_active_transfer_or_an_unread_manifest() {
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        seed(&store, 4);
        let chunk = geometry(1).ciphertext_bytes;

        store.set_transfer_active(&blob_id(0), true, 1_000).unwrap();
        store.set_manifest_unread(&blob_id(1), true, 1_000).unwrap();

        // A budget of zero: eviction takes everything it legitimately can and
        // reports what it could not.
        let plan = store.plan_eviction(0).unwrap();
        assert_eq!(
            plan.evicted
                .iter()
                .map(|e| e.chunk_file.clone())
                .collect::<Vec<_>>(),
            vec!["blob-2.part".to_string(), "blob-3.part".to_string()]
        );
        assert_eq!(plan.protected_bytes, chunk * 2);
        assert_eq!(plan.bytes_after, chunk * 2);
        assert!(
            plan.bytes_after > 0,
            "the budget is missed rather than a protection broken, and the plan says so"
        );
    }

    #[test]
    fn nothing_is_evicted_under_budget() {
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        seed(&store, 3);
        let plan = store.plan_default_eviction().unwrap();
        assert!(plan.is_empty());
        assert_eq!(plan.bytes_before, plan.bytes_after);
    }

    #[test]
    fn a_tracked_blob_cannot_change_geometry_underneath_its_bitmap() {
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        let id = blob_id(7);
        store.begin(&id, &geometry(2), "blob-7.part", 10).unwrap();
        assert!(store.begin(&id, &geometry(3), "blob-7.part", 20).is_err());
    }

    #[test]
    fn the_store_only_ever_holds_ciphertext_geometry_never_plaintext() {
        // BLOB-02, from the store's side: what is tracked is the ciphertext
        // length and the ciphertext's name. Nothing here can hold a key.
        let sealed = seal_blob(&test_key(1), b"a picture of the fjords").unwrap();
        let db = conn();
        let store = BlobStore::open(&db).unwrap();
        store
            .begin(&sealed.id, &sealed.geometry, "fjords.part", 10)
            .unwrap();
        let columns: Vec<String> = db
            .prepare("SELECT * FROM media_blobs")
            .unwrap()
            .column_names()
            .iter()
            .map(|name| name.to_string())
            .collect();
        assert!(
            !columns.iter().any(|name| name.contains("key")),
            "the blob store has no column that could hold key material: {columns:?}"
        );
    }
}
