//! Persistence for §8's sync streams: what this device has authored, what it
//! has taken from a sibling, and where each stream's gap-free prefix ends.
//!
//! `sync_record.rs` decides what a record *is*; `sync_stream.rs` decides what
//! to ask for and what to send. This module is the durable middle: it writes
//! stream positions down so a digest can be honest across a reboot, keeps the
//! sealed copies of this device's own records so a sibling can be backfilled
//! weeks later, and applies an admitted record's payload into the store the
//! rest of the app reads.
//!
//! ## Two properties everything here is built around
//!
//! **Idempotent.** A record is identified by its stream slot
//! `(author_device_id, kind, stream_seq)`, never by its bytes, so the same
//! record arriving over BLE, over a mule and off a relay row is one row and one
//! application. SYNC-1's exchange is expected to re-offer records — that is how
//! it recovers from a link that dropped mid-round — so re-arrival is the normal
//! case, not the exceptional one.
//!
//! **Out-of-order safe.** Every payload merge below is order-independent:
//! history rows are inserted under their own `(chat, person, device, lamport)`
//! stream key, watermarks take a maximum, settings take the newest epoch,
//! contacts and rosters go through the shipped DL-1-ordered import. So a record
//! is applied the moment it arrives, whatever its `stream_seq`, and the
//! *cursor* — the contiguous watermark a digest advertises — advances
//! separately, only across a gap-free prefix. Record 7 arriving before 3 is
//! therefore useful immediately and still causes 3..=6 to be requested again,
//! which is exactly the behaviour
//! [`crate::MessageStore::highest_contiguous_lamport`] has always had for chat
//! messages.
//!
//! Neither property needs the sibling to be reachable, which is SYNC-1's
//! standing constraint restated as a storage rule.
//!
//! ## What is deliberately *not* stored here
//!
//! **Inbox key secrets.** [`crate::InboxKey`] carries secret material, and §6
//! keeps it in the same place [`crate::Identity`]'s secrets live: the shell's
//! platform-protected storage, never this database. So an applied
//! [`crate::SyncRecordKind::OwnRoster`] record hands its payload back to the
//! caller through [`SyncApplyResult::own_roster`] instead of writing it — a
//! `.cmbak` of this database therefore cannot leak a fleet's inbox key, and the
//! link ceremony that owns key custody (§9, WP3) stays the only writer.
//!
//! **The own roster document and the own device fleet.** Same reason from the
//! other side: declaring which devices this person has is a ceremony's verdict
//! (§9's activation, §10's revocation), and
//! [`crate::MessageStore::set_own_device_fleet`] is deliberately a
//! monotone, whole-record writer that refuses to go backwards. Applying a
//! gossiped record straight into it would let anti-entropy re-widen a fleet a
//! revocation had just narrowed. The record is admitted, its position is
//! recorded, and the payload is returned for the ceremony layer to act on.

use rusqlite::{params, Connection, OptionalExtension};

use crate::device_roster::{RosterVersion, DEVICE_ID_LEN};
use crate::identity::derive_user_id;
use crate::store::store_err;
use crate::sync_record::{
    core_decode_sync_contacts, core_decode_sync_groups, core_decode_sync_history,
    core_decode_sync_own_roster, core_decode_sync_settings, core_decode_sync_watermarks,
    core_sync_kind_is_stream, core_sync_record_kind_of, core_sync_record_kind_wire,
    SealedSyncRecord, SyncContactEntry, SyncContactsPayload, SyncGroupsPayload,
    SyncHistoryDirection, SyncHistoryEntry, SyncHistoryPayload, SyncOwnRosterPayload, SyncRecord,
    SyncRecordKind, SyncSettingEntry, SyncSettingsPayload, SyncWatermarkEntry,
    SyncWatermarkPayload,
};
use crate::sync_stream::{
    core_decode_sync_digest, SyncBackfillOffer, SyncDigest, SyncGap, SyncStreamDigest,
};
use crate::{CoreError, MessageStore};

/// The reserved shared-settings key the person's block list rides under (§8).
///
/// Blocking is a person-level decision, not a device-level one: a family member
/// who blocks somebody on the phone has blocked them, and the tablet still
/// showing that person's mail is the bug. It is carried as a shared setting
/// rather than as a record kind of its own because it is exactly what the
/// Settings stream is for — small, replaceable, newest-wins state — and because
/// a stream of its own would need its own merge rule for a fact that already
/// has one.
///
/// It never leaves the person boundary: the Settings stream is sealed to the
/// inbox key like every other sync record, so a blocked person cannot learn
/// they were blocked, on any device.
pub const SYNC_BLOCKED_SETTING_KEY: &str = "privacy.blocked";

pub(crate) const SYNC_STREAM_SCHEMA_SQL: &str = "
-- One row per sync record this device holds a *position* for
-- (`specs/multi-device-v1.md` §8). The primary key is the stream slot, which
-- is what makes re-arrival free: SYNC-1 re-offers records whenever a round is
-- cut short, and an id derived from the bytes would make a re-seal (SYNC-3)
-- look like a different record.
--
-- The sealed columns are populated only for streams this device *authors*.
-- A sibling's record is applied and its position kept; its bytes are dropped,
-- because only its author can re-seal it after a roster change, so holding a
-- copy this device could never refresh would be storage spent on something it
-- must never forward anyway.
CREATE TABLE IF NOT EXISTS sync_stream_records (
    author_device_id      BLOB NOT NULL,
    kind                  INTEGER NOT NULL,
    stream_seq            INTEGER NOT NULL,
    person_id             BLOB NOT NULL,
    sealed                BLOB,
    sealed_recovery_epoch INTEGER,
    sealed_seq            INTEGER,
    inbox_key_generation  INTEGER,
    created_at_ms         INTEGER NOT NULL,
    PRIMARY KEY (author_device_id, kind, stream_seq)
);

-- SYNC-1's per-stream watermark: the end of the gap-free prefix, and the only
-- number a digest ever advertises. Kept as its own row rather than recomputed
-- because a digest is built on every encounter and a stream is append-only --
-- the walk that advances it visits each seq once, ever.
CREATE TABLE IF NOT EXISTS sync_stream_cursors (
    author_device_id BLOB NOT NULL,
    kind             INTEGER NOT NULL,
    through_seq      INTEGER NOT NULL,
    PRIMARY KEY (author_device_id, kind)
);

-- The settings §8 calls 'shared'. Resolved by the total order
-- `(epoch, author_device_id, value)` (see `SyncSettingEntry`), which converges
-- without either device having to be online AND without leaving two devices
-- permanently forked when they happen to write the same key at the same epoch
-- -- which they will, because a shell stamps `epoch` from the wall clock.
CREATE TABLE IF NOT EXISTS sync_settings (
    key              TEXT PRIMARY KEY,
    value            BLOB NOT NULL,
    epoch            INTEGER NOT NULL,
    author_device_id BLOB NOT NULL
);

-- The own roster and inbox key generation the inbound transaction admits sync
-- records against (§4, §6). Written by the ceremony layer (§9's activation,
-- §10's revocation) through `core_set_own_sync_context` and by nothing else --
-- in particular never by anti-entropy, for the reason the module docs give.
--
-- One row or none. `None` is the ordinary state of an install that has never
-- linked a device, and it is what makes the inbound sync dispatch inert there
-- rather than guessing: no roster, no admission, no application.
CREATE TABLE IF NOT EXISTS own_sync_context (
    id                   INTEGER PRIMARY KEY CHECK (id = 0),
    roster               BLOB NOT NULL,
    recovery_epoch       INTEGER NOT NULL,
    seq                  INTEGER NOT NULL,
    inbox_key_generation INTEGER NOT NULL
);
";

/// What happened to one applied record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum SyncApplyOutcome {
    /// The payload was merged and the stream slot recorded.
    Applied,
    /// This slot was already held. SYNC-1 re-offers records routinely, so this
    /// is an ordinary outcome and not a warning.
    AlreadyHeld,
    /// A [`SyncRecordKind::Digest`] record: read, handed back through
    /// [`SyncApplyResult::peer_digest`], and deliberately given no stream slot.
    /// Always this outcome, never `AlreadyHeld` — two digests from one device
    /// are two different claims about a moving watermark, and deduping the
    /// second would freeze the exchange on the first.
    Read,
}

/// The result of applying one sync record.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct SyncApplyResult {
    pub outcome: SyncApplyOutcome,
    /// Entries the payload carried. Reported rather than summed into a
    /// success/failure bit so a shell can show honest progress on a long
    /// backfill.
    pub applied_entries: u32,
    /// This stream's contiguous watermark *after* the apply — what the next
    /// digest will advertise for it. Unchanged from before when the record
    /// landed above a hole, which is the case worth being able to observe.
    pub through_seq: u64,
    /// The own-roster payload, for [`SyncRecordKind::OwnRoster`] records only.
    /// See the module docs: inbox key secrets and fleet membership are the
    /// link ceremony's to write, never anti-entropy's.
    pub own_roster: Option<SyncOwnRosterPayload>,
    /// The sibling's SYNC-1 watermarks, for [`SyncRecordKind::Digest`] records
    /// only.
    ///
    /// This is what makes the exchange *driven by the exchange* rather than by
    /// a driver that already knew both sides' state: a device learns what a
    /// sibling holds by opening a sealed digest that sibling sent it, computes
    /// what it owes with [`crate::core_sync_digest_gaps`], and answers. Handed
    /// back rather than stored for the same reason a watermark is not
    /// backfilled — it is true for exactly as long as it takes to act on.
    pub peer_digest: Option<SyncDigest>,
}

// ---------------------------------------------------------------------------
// Stream positions
// ---------------------------------------------------------------------------

/// The next free `stream_seq` on `(author_device_id, kind)` — the position the
/// device's next record of that kind takes. 1 for a stream that has never been
/// written, so a stream always starts at 1 and 0 always means "nothing".
pub(crate) fn next_stream_seq(
    conn: &Connection,
    author_device_id: &[u8],
    kind: SyncRecordKind,
) -> Result<u64, CoreError> {
    let highest: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(stream_seq), 0) FROM sync_stream_records
             WHERE author_device_id = ?1 AND kind = ?2",
            params![
                author_device_id,
                i64::from(core_sync_record_kind_wire(kind))
            ],
            |row| row.get(0),
        )
        .map_err(store_err)?;
    Ok(highest as u64 + 1)
}

/// Record a slot and advance its stream's contiguous cursor. Returns false if
/// the slot was already held.
fn hold_slot(
    conn: &Connection,
    person_id: &[u8],
    author_device_id: &[u8],
    kind: SyncRecordKind,
    stream_seq: u64,
    sealed: Option<&SealedSyncRecord>,
    now_ms: i64,
) -> Result<bool, CoreError> {
    let wire_kind = i64::from(core_sync_record_kind_wire(kind));
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO sync_stream_records
                (author_device_id, kind, stream_seq, person_id, sealed,
                 sealed_recovery_epoch, sealed_seq, inbox_key_generation, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                author_device_id,
                wire_kind,
                stream_seq as i64,
                person_id,
                sealed.map(|s| s.sealed.clone()),
                sealed.map(|s| s.sealed_for.recovery_epoch as i64),
                sealed.map(|s| s.sealed_for.seq as i64),
                sealed.map(|s| s.inbox_key_generation as i64),
                now_ms,
            ],
        )
        .map_err(store_err)?
        > 0;
    if inserted {
        advance_cursor(conn, author_device_id, kind)?;
    }
    Ok(inserted)
}

/// Walk the stream forward from its stored cursor for as long as the next
/// `stream_seq` is present, and write the new watermark.
///
/// The walk is over the seqs *above* the cursor only, so across the life of a
/// stream every record is visited exactly once by it. This is SYNC-1's
/// contiguity rule and the reason a digest can be built cheaply on every
/// encounter.
fn advance_cursor(
    conn: &Connection,
    author_device_id: &[u8],
    kind: SyncRecordKind,
) -> Result<u64, CoreError> {
    let wire_kind = i64::from(core_sync_record_kind_wire(kind));
    let mut through = stream_cursor(conn, author_device_id, kind)?;
    let mut stmt = conn
        .prepare(
            "SELECT stream_seq FROM sync_stream_records
             WHERE author_device_id = ?1 AND kind = ?2 AND stream_seq > ?3
             ORDER BY stream_seq ASC",
        )
        .map_err(store_err)?;
    let rows = stmt
        .query_map(
            params![author_device_id, wire_kind, through as i64],
            |row| row.get::<_, i64>(0),
        )
        .map_err(store_err)?;
    for seq in rows {
        let seq = seq.map_err(store_err)? as u64;
        if seq != through + 1 {
            break;
        }
        through = seq;
    }
    conn.execute(
        "INSERT INTO sync_stream_cursors (author_device_id, kind, through_seq)
            VALUES (?1, ?2, ?3)
         ON CONFLICT(author_device_id, kind) DO UPDATE SET
            through_seq = MAX(through_seq, excluded.through_seq)",
        params![author_device_id, wire_kind, through as i64],
    )
    .map_err(store_err)?;
    Ok(through)
}

pub(crate) fn stream_cursor(
    conn: &Connection,
    author_device_id: &[u8],
    kind: SyncRecordKind,
) -> Result<u64, CoreError> {
    stream_cursor_by_wire(conn, author_device_id, core_sync_record_kind_wire(kind))
}

/// [`stream_cursor`] addressed by the raw wire byte, so a stream this build
/// cannot name still has a readable watermark to advertise.
fn stream_cursor_by_wire(
    conn: &Connection,
    author_device_id: &[u8],
    wire_kind: u8,
) -> Result<u64, CoreError> {
    let through: Option<i64> = conn
        .query_row(
            "SELECT through_seq FROM sync_stream_cursors
             WHERE author_device_id = ?1 AND kind = ?2",
            params![author_device_id, i64::from(wire_kind)],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_err)?;
    Ok(through.unwrap_or(0) as u64)
}

/// This device's whole SYNC-1 digest: every stream it holds anything of, at its
/// contiguous watermark.
///
/// Own-authored streams and streams taken from siblings are one list on
/// purpose. Anti-entropy is symmetric, and a device that restored a `.cmbak`
/// legitimately learns its *own* older stream back from a sibling that kept it.
/// Two facts travel per stream, and they are not the same fact — see
/// [`SyncStreamDigest::can_serve`]. `through_seq` is what this device *holds*;
/// `can_serve` is whether it holds the sealed bytes, which only a stream's
/// author does.
///
/// A kind this build cannot name is advertised at its cursor rather than
/// omitted. A row can only carry one if a newer build wrote this database — the
/// person's own other install, after a downgrade or a restore — and omitting it
/// would tell that install "I have none of this stream", every round, forever.
/// Advertising it as positions-only is the honest claim and the terminating
/// one: the newer build stops re-sending, and [`crate::core_sync_digest_gaps`]
/// declines to request records this build could not decode anyway.
pub(crate) fn digest(conn: &Connection, person_id: &[u8]) -> Result<SyncDigest, CoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT author_device_id, kind,
                    MAX(CASE WHEN sealed IS NOT NULL THEN 1 ELSE 0 END)
             FROM sync_stream_records
             WHERE person_id = ?1
             GROUP BY author_device_id, kind
             ORDER BY author_device_id ASC, kind ASC",
        )
        .map_err(store_err)?;
    let rows = stmt
        .query_map(params![person_id], |row| {
            Ok((
                row.get::<_, Vec<u8>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)? != 0,
            ))
        })
        .map_err(store_err)?;
    let mut streams = Vec::new();
    for row in rows {
        let (author_device_id, wire_kind, can_serve) = row.map_err(store_err)?;
        let Ok(kind) = u8::try_from(wire_kind) else {
            continue;
        };
        streams.push(SyncStreamDigest {
            through_seq: stream_cursor_by_wire(conn, &author_device_id, kind)?,
            author_device_id,
            kind,
            can_serve,
        });
    }
    Ok(SyncDigest {
        person_id: person_id.to_vec(),
        streams,
    })
}

/// The sealed records this device could send to answer `gaps`, oldest first.
///
/// Only rows with sealed bytes come back — that is, only streams this device
/// authored. A sibling's record is not this device's to forward: it could not
/// re-seal it after a roster change (SYNC-3), and the sibling that authored it
/// answers for it directly on its own next encounter.
pub(crate) fn backfill_offers(
    conn: &Connection,
    gaps: &[SyncGap],
    limit: u32,
) -> Result<Vec<StoredSyncRecord>, CoreError> {
    let mut out: Vec<StoredSyncRecord> = Vec::new();
    for gap in gaps {
        if out.len() >= limit as usize {
            break;
        }
        // A gap naming a kind this build cannot decode cannot name a record
        // this build authored, so there is nothing to look up. Guarded rather
        // than left to return no rows, so the enum below has one source.
        let Some(kind) = core_sync_record_kind_of(gap.kind) else {
            continue;
        };
        let remaining = limit as usize - out.len();
        let mut stmt = conn
            .prepare(
                "SELECT stream_seq, sealed, sealed_recovery_epoch, sealed_seq,
                        inbox_key_generation
                 FROM sync_stream_records
                 WHERE author_device_id = ?1 AND kind = ?2
                   AND stream_seq > ?3 AND stream_seq <= ?4 AND sealed IS NOT NULL
                 ORDER BY stream_seq ASC
                 LIMIT ?5",
            )
            .map_err(store_err)?;
        let author_device_id = gap.author_device_id.clone();
        let rows = stmt
            .query_map(
                params![
                    author_device_id,
                    i64::from(gap.kind),
                    gap.after_seq as i64,
                    gap.through_seq as i64,
                    remaining as i64,
                ],
                |row| {
                    Ok(StoredSyncRecord {
                        author_device_id: author_device_id.clone(),
                        kind,
                        stream_seq: row.get::<_, i64>(0)? as u64,
                        sealed: row.get(1)?,
                        sealed_for: RosterVersion {
                            recovery_epoch: row.get::<_, i64>(2)? as u64,
                            seq: row.get::<_, i64>(3)? as u64,
                        },
                        inbox_key_generation: row.get::<_, i64>(4)? as u64,
                    })
                },
            )
            .map_err(store_err)?;
        for row in rows {
            out.push(row.map_err(store_err)?);
        }
    }
    Ok(out)
}

/// One of this device's own sealed records, ready to hand to a transport.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct StoredSyncRecord {
    pub author_device_id: Vec<u8>,
    pub kind: SyncRecordKind,
    pub stream_seq: u64,
    pub sealed: Vec<u8>,
    pub sealed_for: RosterVersion,
    pub inbox_key_generation: u64,
}

impl StoredSyncRecord {
    /// The planner's view of this row (see [`crate::core_plan_sync_backfill`]).
    pub fn offer(&self) -> SyncBackfillOffer {
        SyncBackfillOffer {
            author_device_id: self.author_device_id.clone(),
            kind: core_sync_record_kind_wire(self.kind),
            stream_seq: self.stream_seq,
            sealed_for: self.sealed_for,
            inbox_key_generation: self.inbox_key_generation,
            byte_len: self.sealed.len() as u64,
        }
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Merge one shared setting under [`SyncSettingEntry`]'s total order
/// `(epoch, author_device_id, value)`. A losing or identical write is a no-op
/// rather than an error: two devices writing the same setting offline is the
/// ordinary case §8 asks this merge to survive, and the loser losing quietly is
/// what "converges without either device being online" means here.
///
/// The comparison is done in Rust rather than in the `ON CONFLICT` clause on
/// purpose. A three-field lexicographic order over one integer and two blobs is
/// a paragraph of SQL that reads like a puzzle and a line of Rust that reads
/// like the rule, and this is the one function in the file whose exact
/// behaviour decides whether a fleet converges or sits forked.
pub(crate) fn put_setting(conn: &Connection, entry: &SyncSettingEntry) -> Result<bool, CoreError> {
    if entry.author_device_id.len() != DEVICE_ID_LEN {
        return Err(CoreError::Malformed(
            "a shared setting must name the device that wrote it".to_string(),
        ));
    }
    if let Some(stored) = setting(conn, &entry.key)? {
        if !entry.supersedes(&stored) {
            return Ok(false);
        }
    }
    conn.execute(
        "INSERT INTO sync_settings (key, value, epoch, author_device_id)
            VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(key) DO UPDATE SET
            value = excluded.value,
            epoch = excluded.epoch,
            author_device_id = excluded.author_device_id",
        params![
            entry.key,
            entry.value,
            entry.epoch as i64,
            entry.author_device_id
        ],
    )
    .map_err(store_err)?;
    Ok(true)
}

/// One setting by key, straight off the primary key rather than by walking the
/// whole stream. SYNC-2's draft lookups run on every composer keystroke a shell
/// decides to persist, which is the one caller here that is not a page.
pub(crate) fn setting(conn: &Connection, key: &str) -> Result<Option<SyncSettingEntry>, CoreError> {
    conn.query_row(
        "SELECT key, value, epoch, author_device_id FROM sync_settings WHERE key = ?1",
        params![key],
        |row| {
            Ok(SyncSettingEntry {
                key: row.get(0)?,
                value: row.get(1)?,
                epoch: row.get::<_, i64>(2)? as u64,
                author_device_id: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(store_err)
}

pub(crate) fn list_settings(conn: &Connection) -> Result<Vec<SyncSettingEntry>, CoreError> {
    let mut stmt = conn
        .prepare("SELECT key, value, epoch, author_device_id FROM sync_settings ORDER BY key ASC")
        .map_err(store_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(SyncSettingEntry {
                key: row.get(0)?,
                value: row.get(1)?,
                epoch: row.get::<_, i64>(2)? as u64,
                author_device_id: row.get(3)?,
            })
        })
        .map_err(store_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
}

// ---------------------------------------------------------------------------
// The block list, as a shared setting (§8)
// ---------------------------------------------------------------------------

/// Encode a block list for [`SYNC_BLOCKED_SETTING_KEY`]: `count(u16)` then one
/// `len(u16) ‖ person_id` per entry, sorted and deduplicated.
///
/// Sorted because the value is compared byte-for-byte by the settings merge's
/// last tiebreak, and two devices holding the same block set in different
/// insertion orders would otherwise look like two different values and trade
/// records forever.
fn encode_block_list(mut ids: Vec<Vec<u8>>) -> Vec<u8> {
    ids.sort();
    ids.dedup();
    let mut out = Vec::with_capacity(2 + ids.len() * 18);
    out.extend_from_slice(&(ids.len().min(u16::MAX as usize) as u16).to_be_bytes());
    for id in ids.iter().take(u16::MAX as usize) {
        out.extend_from_slice(&(id.len() as u16).to_be_bytes());
        out.extend_from_slice(id);
    }
    out
}

/// Decode a block list. A malformed value yields an empty list rather than an
/// error: this is replaceable state whose worst failure is that somebody stays
/// unblocked on one device, and refusing the whole Settings record over it
/// would strand every other setting in the same payload.
fn decode_block_list(bytes: &[u8]) -> Vec<Vec<u8>> {
    if bytes.len() < 2 {
        return Vec::new();
    }
    let count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let mut out = Vec::with_capacity(count.min(1024));
    let mut offset = 2usize;
    for _ in 0..count {
        if offset + 2 > bytes.len() {
            return out;
        }
        let len = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        offset += 2;
        if offset + len > bytes.len() {
            return out;
        }
        out.push(bytes[offset..offset + len].to_vec());
        offset += len;
    }
    out
}

// ---------------------------------------------------------------------------
// Applying a record
// ---------------------------------------------------------------------------

/// Apply one admitted sync record into the store (SYNC-1).
///
/// **This function presumes admission.** [`crate::core_open_sync_record`] is
/// what proves the record is this person's, sealed under the current inbox key,
/// authored by a device the own roster still vouches for, and correctly signed;
/// nothing below re-derives any of that, exactly as `apply_contact_roster`
/// presumes `core_roster_accept`'s verdict. A caller that reaches this with an
/// unadmitted record has already lost the person boundary somewhere upstream.
///
/// Ordering inside is deliberate: **the payload is merged first and the stream
/// slot is recorded last.** A crash between the two re-applies the record on
/// the next round, which every merge below tolerates; recording the slot first
/// would make a crash silently skip it forever.
pub(crate) fn apply(
    store: &MessageStore,
    record: SyncRecord,
    now_ms: i64,
) -> Result<SyncApplyResult, CoreError> {
    let author_device_id = record.author_device_id.clone();
    let kind = record.kind;

    // A digest is read, never filed. See [`SyncRecordKind::Digest`]: it is the
    // message that *asks*, so giving it a stream slot would make a device
    // request last week's watermarks, apply them, advertise them, and be
    // offered them again. Handled before the held-slot lookup so no digest ever
    // touches the slot tables at all.
    if !core_sync_kind_is_stream(kind) {
        let peer_digest = core_decode_sync_digest(record.payload.clone())?;
        if peer_digest.person_id != record.person_id {
            return Err(CoreError::Malformed(
                "a sync digest names a different person than the record carrying it".to_string(),
            ));
        }
        let streams = peer_digest.streams.len() as u32;
        return Ok(SyncApplyResult {
            outcome: SyncApplyOutcome::Read,
            applied_entries: streams,
            through_seq: 0,
            own_roster: None,
            peer_digest: Some(peer_digest),
        });
    }

    let held = {
        let conn = store.locked_conn();
        conn.query_row(
            "SELECT 1 FROM sync_stream_records
             WHERE author_device_id = ?1 AND kind = ?2 AND stream_seq = ?3",
            params![
                author_device_id,
                i64::from(core_sync_record_kind_wire(kind)),
                record.stream_seq as i64
            ],
            |_| Ok(()),
        )
        .optional()
        .map_err(store_err)?
        .is_some()
    };
    if held {
        let conn = store.locked_conn();
        return Ok(SyncApplyResult {
            outcome: SyncApplyOutcome::AlreadyHeld,
            applied_entries: 0,
            through_seq: stream_cursor(&conn, &author_device_id, kind)?,
            own_roster: None,
            peer_digest: None,
        });
    }

    let mut own_roster = None;
    let applied_entries = match kind {
        SyncRecordKind::History => apply_history(store, &record)?,
        SyncRecordKind::Watermarks => apply_watermarks(store, &record)?,
        SyncRecordKind::Contacts => apply_contacts(store, &record)?,
        SyncRecordKind::Groups => apply_groups(store, &record)?,
        SyncRecordKind::Settings => apply_settings(store, &record, now_ms)?,
        SyncRecordKind::OwnRoster => {
            let payload = core_decode_sync_own_roster(record.payload.clone())?;
            let count = payload.inbox_keys.len() as u32;
            own_roster = Some(payload);
            count
        }
        SyncRecordKind::Digest => unreachable!("handled above, before the slot tables"),
    };

    let conn = store.locked_conn();
    hold_slot(
        &conn,
        &record.person_id,
        &author_device_id,
        kind,
        record.stream_seq,
        None,
        now_ms,
    )?;
    Ok(SyncApplyResult {
        outcome: SyncApplyOutcome::Applied,
        applied_entries,
        through_seq: stream_cursor(&conn, &author_device_id, kind)?,
        own_roster,
        peer_digest: None,
    })
}

/// History: each entry becomes an ordinary `messages` row on §5's stream key.
///
/// The insert is the shipped device-aware one, so a synced row is subject to
/// exactly the same duplicate detection and fork quarantine as a row that
/// arrived over a radio — a message this device already consumed over BLE is
/// recognized by its own stream position, not merely by `origin_msg_id`.
fn apply_history(store: &MessageStore, record: &SyncRecord) -> Result<u32, CoreError> {
    let payload = core_decode_sync_history(record.payload.clone())?;
    let mut applied = 0;
    for entry in payload.entries {
        let body = crate::decode_extended_message_body(entry.body)?;
        let message = crate::StoredMessage {
            chat_id: body.chat_id,
            sender_user_id: entry.sender_person_id,
            lamport: body.lamport,
            timestamp: body.timestamp,
            kind: body.kind,
            payload: body.content,
            sender_device_id: entry.sender_device_id.clone(),
        };
        store.insert_incoming_message_from_device(
            message,
            Some(entry.sender_device_id),
            entry.origin_msg_id,
            body.reply_to_msg_id,
            // No arrival evidence: this row reached the person over some
            // transport, but not over one *this* device observed, and inventing
            // a route would corrupt the delivery metrics it feeds.
            None,
        )?;
        applied += 1;
    }
    Ok(applied)
}

/// Watermarks: max-merged into the same `outgoing_receipts` table this device's
/// own reading writes, so a chat read on a phone is read on the tablet.
///
/// A zero watermark is skipped rather than written: it carries no information
/// (the table already reads 0 for an absent row) and writing it would only
/// churn.
fn apply_watermarks(store: &MessageStore, record: &SyncRecord) -> Result<u32, CoreError> {
    let payload = core_decode_sync_watermarks(record.payload.clone())?;
    let mut applied = 0;
    for entry in payload.entries {
        if entry.delivered_through_lamport > 0 {
            store.record_outgoing_receipt(
                entry.chat_id.clone(),
                entry.subject_person_id.clone(),
                crate::RECEIPT_TYPE_DELIVERED,
                entry.delivered_through_lamport,
            )?;
        }
        if entry.read_through_lamport > 0 {
            store.record_outgoing_receipt(
                entry.chat_id,
                entry.subject_person_id,
                crate::RECEIPT_TYPE_READ,
                entry.read_through_lamport,
            )?;
        }
        applied += 1;
    }
    Ok(applied)
}

/// Contacts: a card this person already holds, landed through the shipped
/// import path and merged by a rule that converges.
///
/// The card's `sign_pk` must derive the entry's `person_id` (§3: the person
/// root *is* the identity key). A record that paired a name with somebody
/// else's keys would otherwise be a way to rewrite a contact from inside the
/// person boundary.
///
/// **What happens when both devices already know the contact** is the part
/// worth reading, because the obvious answer is wrong twice over. Leaving the
/// local row alone is *not* convergent: a person who re-adds a contact from a
/// fresh card on the tablet — a new relay endpoint, a corrected name — leaves
/// the phone permanently on the old card, each device refusing the other's
/// view, and the fleet forked on a difference neither surface can show. Taking
/// the incoming card unconditionally is not convergent either: it makes the
/// winner whoever spoke last, so the two devices trade the two cards back and
/// forth on every round.
///
/// So the merge is a **total order on the card itself**, and both devices
/// compute the same winner from the same two documents whatever order they meet
/// in. A friend card carries no timestamp — nothing in the format has ever
/// needed one — so the order is over the card's canonical bytes
/// ([`contact_card_bytes`]): arbitrary as a statement about which card is
/// *newer*, exact as a statement about which card both devices will keep. What
/// matters here is agreement, and an arbitrary rule that agrees beats a
/// plausible rule that does not.
///
/// A `nickname` is untouched by any of this: it is local, never rides a wire
/// format, and a synced card has no opinion about it.
///
/// The contact's *roster* applies either way, because `apply_contact_roster` is
/// DL-1 ordered and cannot go backwards.
fn apply_contacts(store: &MessageStore, record: &SyncRecord) -> Result<u32, CoreError> {
    let payload = core_decode_sync_contacts(record.payload.clone())?;
    let mut applied = 0;
    for entry in payload.entries {
        let card = crate::parse_friend_card(entry.card_json)?;
        if derive_user_id(&card.sign_pk)[..] != entry.person_id[..] {
            return Err(CoreError::Malformed(
                "synced contact card does not belong to the person it is filed under".to_string(),
            ));
        }
        let incoming = crate::Contact {
            user_id: entry.person_id.clone(),
            name: card.name,
            sign_pk: card.sign_pk,
            agree_pk: card.agree_pk,
            relay_url: card.relay_url,
            relay_token: card.relay_token,
            nickname: None,
        };
        match store.get_contact(entry.person_id.clone())? {
            None => store.upsert_contact(incoming)?,
            Some(existing) => {
                if contact_card_bytes(&incoming) > contact_card_bytes(&existing) {
                    // `upsert_contact` writes no nickname column, so the local
                    // one survives this by construction — the same reason
                    // re-importing a friend card has always preserved it.
                    store.upsert_contact(incoming)?;
                }
            }
        }
        if let Some(roster) = entry.roster {
            store.apply_contact_roster(roster)?;
        }
        applied += 1;
    }
    Ok(applied)
}

/// A contact's card as comparable bytes: the fields a card actually carries, in
/// the fixed order [`contact_card`] writes them, with the local-only nickname
/// and the per-device roster head left out.
///
/// Built by hand rather than by serializing [`crate::FriendCard`], because a
/// merge rule that depended on `serde_json`'s field order would be a merge rule
/// that a dependency bump could change under a fleet mid-conversation.
fn contact_card_bytes(contact: &crate::Contact) -> Vec<u8> {
    let mut out = Vec::new();
    let mut push = |bytes: &[u8]| {
        out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        out.extend_from_slice(bytes);
    };
    push(contact.name.as_bytes());
    push(&contact.sign_pk);
    push(&contact.agree_pk);
    push(contact.relay_url.as_deref().unwrap_or("").as_bytes());
    push(contact.relay_token.as_deref().unwrap_or("").as_bytes());
    out
}

/// Groups: §11 leaves group crypto alone, so a synced group is byte-identical
/// to an invited one and lands through the same upsert.
fn apply_groups(store: &MessageStore, record: &SyncRecord) -> Result<u32, CoreError> {
    let payload = core_decode_sync_groups(record.payload.clone())?;
    let mut applied = 0;
    for group in payload.groups {
        store.upsert_group(group)?;
        applied += 1;
    }
    Ok(applied)
}

/// Settings: merged under the total order, with one entry that is not merely
/// state.
///
/// [`SYNC_BLOCKED_SETTING_KEY`] is the person's block list, and a converged
/// *row* is not the point — a converged **refusal** is. So when the block entry
/// wins the merge, the ids it names are pushed into the shipped
/// `blocked_identities` table, which is what the inbound gate actually reads.
/// A person who blocks somebody on the phone has blocked them, and the tablet
/// still delivering that person's mail is the bug this closes.
///
/// The application is additive: an id in the list is blocked, and one absent
/// from it is left alone rather than unblocked. Unblocking is a deliberate act
/// on a surface, and a fleet where a stale sibling's list could *un*block
/// somebody would be a fleet where the safety-relevant direction is the one
/// that loses a race.
fn apply_settings(
    store: &MessageStore,
    record: &SyncRecord,
    now_ms: i64,
) -> Result<u32, CoreError> {
    let payload = core_decode_sync_settings(record.payload.clone())?;
    let mut applied = 0;
    let mut newly_blocked: Vec<Vec<u8>> = Vec::new();
    {
        let conn = store.locked_conn();
        for entry in payload.entries {
            let took = put_setting(&conn, &entry)?;
            if took && entry.key == SYNC_BLOCKED_SETTING_KEY {
                newly_blocked = decode_block_list(&entry.value);
            }
            applied += 1;
        }
    }
    for user_id in newly_blocked {
        store.block_user(user_id, now_ms)?;
    }
    Ok(applied)
}

// ---------------------------------------------------------------------------
// Retaining this device's own records
// ---------------------------------------------------------------------------

/// Keep a record this device authored, together with the bytes a sibling will
/// eventually be sent.
///
/// Returns false when the slot was already retained — re-sealing the same slot
/// after a roster change is an *update*, handled by [`reseal`], not by a second
/// row.
pub(crate) fn retain(
    conn: &Connection,
    record: &SyncRecord,
    sealed: &SealedSyncRecord,
    now_ms: i64,
) -> Result<bool, CoreError> {
    if !core_sync_kind_is_stream(record.kind) {
        // A digest is sent, not kept. Retaining one would put yesterday's
        // watermarks in the backfill pool, where a sibling would ask for them,
        // apply them, and advertise them — see [`SyncRecordKind::Digest`].
        return Err(CoreError::Malformed(
            "a sync digest is not a retained stream record".to_string(),
        ));
    }
    hold_slot(
        conn,
        &record.person_id,
        &record.author_device_id,
        record.kind,
        record.stream_seq,
        Some(sealed),
        now_ms,
    )
}

/// Replace a retained record's sealed bytes after a roster change (SYNC-3).
///
/// The slot, and therefore [`crate::core_sync_record_id`], is unchanged — which
/// is the whole reason that id names the slot and not the bytes: a fleet that
/// re-sealed its backlog after every link would otherwise spend a fresh relay
/// row per record per roster version.
pub(crate) fn reseal(
    conn: &Connection,
    record: &SyncRecord,
    sealed: &SealedSyncRecord,
) -> Result<bool, CoreError> {
    let changed = conn
        .execute(
            "UPDATE sync_stream_records
                SET sealed = ?4, sealed_recovery_epoch = ?5, sealed_seq = ?6,
                    inbox_key_generation = ?7
             WHERE author_device_id = ?1 AND kind = ?2 AND stream_seq = ?3",
            params![
                record.author_device_id,
                i64::from(core_sync_record_kind_wire(record.kind)),
                record.stream_seq as i64,
                sealed.sealed,
                sealed.sealed_for.recovery_epoch as i64,
                sealed.sealed_for.seq as i64,
                sealed.inbox_key_generation as i64,
            ],
        )
        .map_err(store_err)?;
    Ok(changed > 0)
}

// ---------------------------------------------------------------------------
// Harvest: what goes *into* a record
// ---------------------------------------------------------------------------

fn history_page(
    conn: &Connection,
    own_person_id: &[u8],
    chat_id: &[u8],
    sender_person_id: &[u8],
    after_lamport: u64,
    limit: u32,
) -> Result<SyncHistoryPayload, CoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT lamport, timestamp, kind, payload, sender_device_id, msg_id, reply_to_msg_id
             FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport > ?3 AND msg_id IS NOT NULL
             ORDER BY lamport ASC
             LIMIT ?4",
        )
        .map_err(store_err)?;
    let rows = stmt
        .query_map(
            params![
                chat_id,
                sender_person_id,
                after_lamport as i64,
                limit as i64
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)? as u8,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, Option<Vec<u8>>>(6)?,
                ))
            },
        )
        .map_err(store_err)?;
    let mut entries = Vec::new();
    for row in rows {
        let (lamport, timestamp, kind, content, sender_device_id, msg_id, reply_to_msg_id) =
            row.map_err(store_err)?;
        // The body is re-encoded in the shipped extended form rather than
        // copied out of the original envelope, because the original's bytes are
        // not kept -- and re-encoding is what lets §5's device dimension ride
        // along even for a row that arrived on a legacy envelope.
        let body = crate::encode_message_body_extended(
            crate::MessageBody {
                kind,
                chat_id: chat_id.to_vec(),
                lamport,
                timestamp,
                content,
            },
            reply_to_msg_id,
            Some(sender_device_id.clone()),
            None,
        )?;
        entries.push(SyncHistoryEntry {
            origin_msg_id: msg_id,
            direction: if sender_person_id == own_person_id {
                SyncHistoryDirection::Authored
            } else {
                SyncHistoryDirection::Received
            },
            sender_person_id: sender_person_id.to_vec(),
            sender_device_id,
            body,
        });
    }
    Ok(SyncHistoryPayload { entries })
}

fn watermark_page(conn: &Connection, limit: u32) -> Result<SyncWatermarkPayload, CoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT chat_id, sender_user_id,
                    COALESCE(MAX(CASE WHEN receipt_type = ?1 THEN through_lamport END), 0),
                    COALESCE(MAX(CASE WHEN receipt_type = ?2 THEN through_lamport END), 0)
             FROM outgoing_receipts
             GROUP BY chat_id, sender_user_id
             ORDER BY chat_id ASC, sender_user_id ASC
             LIMIT ?3",
        )
        .map_err(store_err)?;
    let rows = stmt
        .query_map(
            params![
                i64::from(crate::RECEIPT_TYPE_DELIVERED),
                i64::from(crate::RECEIPT_TYPE_READ),
                limit as i64
            ],
            |row| {
                Ok(SyncWatermarkEntry {
                    chat_id: row.get(0)?,
                    subject_person_id: row.get(1)?,
                    delivered_through_lamport: row.get::<_, i64>(2)? as u64,
                    read_through_lamport: row.get::<_, i64>(3)? as u64,
                })
            },
        )
        .map_err(store_err)?;
    Ok(SyncWatermarkPayload {
        entries: rows.collect::<Result<Vec<_>, _>>().map_err(store_err)?,
    })
}

// ---------------------------------------------------------------------------
// The store surface
// ---------------------------------------------------------------------------

/// SYNC-1's face for the two shells and for the simulator.
///
/// Deliberately thin: every method here delegates to a free function above or
/// to a shipped store primitive. The anti-entropy *arithmetic* lives in
/// `sync_stream.rs` where it is testable without a database, and the
/// person-boundary crypto lives in `sync_record.rs` where it is testable
/// without either — the same three-way split `device_roster.rs`,
/// `roster_store.rs` and `store.rs` already keep for rosters.
#[uniffi::export]
impl MessageStore {
    /// This device's SYNC-1 digest for `person_id`: every sync stream it holds
    /// anything of, at its contiguous watermark.
    ///
    /// Cheap enough to rebuild on every encounter, which is the point. A digest
    /// a device only refreshed occasionally would answer a sibling with a
    /// watermark it had already passed, and the sibling would spend the
    /// encounter re-sending what had already landed.
    pub fn core_sync_digest(&self, person_id: Vec<u8>) -> Result<SyncDigest, CoreError> {
        let conn = self.locked_conn();
        digest(&conn, &person_id)
    }

    /// The next `stream_seq` for one of this device's own streams (§8): where
    /// its next record of `kind` goes.
    ///
    /// Gap-free by construction, because SYNC-1's contiguity rule means a hole
    /// in a stream stops a sibling's watermark at that hole forever. A caller
    /// that mints a seq and then fails to retain the record must reuse the seq,
    /// never skip past it.
    pub fn core_sync_next_stream_seq(
        &self,
        author_device_id: Vec<u8>,
        kind: SyncRecordKind,
    ) -> Result<u64, CoreError> {
        let conn = self.locked_conn();
        next_stream_seq(&conn, &author_device_id, kind)
    }

    /// Keep a record this device authored, with the sealed bytes a sibling will
    /// be sent whenever it next surfaces. `false` means the slot was already
    /// retained.
    ///
    /// Retention is what makes SYNC-1's "no operation may assume both devices
    /// are ever concurrently online" true rather than aspirational: the sealed
    /// copy outlives the encounter it was made for, so a sibling that has been
    /// dark for a fortnight is answered out of storage rather than out of a
    /// conversation nobody was present for.
    pub fn core_sync_retain_record(
        &self,
        record: SyncRecord,
        sealed: SealedSyncRecord,
        now_ms: i64,
    ) -> Result<bool, CoreError> {
        let conn = self.locked_conn();
        retain(&conn, &record, &sealed, now_ms)
    }

    /// Replace a retained record's sealed bytes after a roster change (SYNC-3).
    /// The stream slot — and therefore [`crate::core_sync_record_id`] — is
    /// unchanged, so the re-seal costs no new relay row.
    pub fn core_sync_reseal_record(
        &self,
        record: SyncRecord,
        sealed: SealedSyncRecord,
    ) -> Result<bool, CoreError> {
        let conn = self.locked_conn();
        reseal(&conn, &record, &sealed)
    }

    /// The sealed records this device can send to answer `gaps`, oldest first
    /// and capped at `limit` rows.
    ///
    /// Feed the result through [`MessageStore::core_sync_backfill_offers`] into
    /// [`crate::core_plan_sync_backfill`] with the same `gaps`: this method
    /// decides what exists, the planner decides what fits.
    pub fn core_sync_backfill_records(
        &self,
        gaps: Vec<SyncGap>,
        limit: u32,
    ) -> Result<Vec<StoredSyncRecord>, CoreError> {
        let conn = self.locked_conn();
        backfill_offers(&conn, &gaps, limit)
    }

    /// The planner's view of a list of stored records, in the same order.
    ///
    /// Exported because a shell cannot call a plain Rust method on a
    /// `uniffi::Record`, and re-deriving `byte_len` and `sealed_for` on each
    /// platform is exactly the duplicated arithmetic core-first exists to stop.
    pub fn core_sync_backfill_offers(
        &self,
        records: Vec<StoredSyncRecord>,
    ) -> Vec<SyncBackfillOffer> {
        records.iter().map(StoredSyncRecord::offer).collect()
    }

    /// Apply one admitted sync record (SYNC-1). Idempotent per stream slot and
    /// safe to call with records in any order — see the module docs for why
    /// both hold, and for why the caller must have opened the record through
    /// [`crate::core_open_sync_record`] first.
    pub fn core_apply_sync_record(
        &self,
        record: SyncRecord,
        now_ms: i64,
    ) -> Result<SyncApplyResult, CoreError> {
        apply(self, record, now_ms)
    }

    /// A page of message history for a sync record, oldest first: everything in
    /// `chat_id` from `sender_person_id` above `after_lamport`.
    ///
    /// `own_person_id` decides each entry's [`crate::SyncHistoryDirection`]
    /// rather than leaving the receiver to infer it, so a sibling reading the
    /// record knows which entries are the person's own outbound before it has
    /// looked anything up — the fact SYNC-2's dedup turns on.
    ///
    /// Rows with no stored `msg_id` are skipped. A history entry names the
    /// original envelope so a sibling that already consumed the message
    /// recognizes it instead of storing it twice, and a row that predates the
    /// id column cannot offer that.
    pub fn core_sync_history_page(
        &self,
        own_person_id: Vec<u8>,
        chat_id: Vec<u8>,
        sender_person_id: Vec<u8>,
        after_lamport: u64,
        limit: u32,
    ) -> Result<SyncHistoryPayload, CoreError> {
        let conn = self.locked_conn();
        history_page(
            &conn,
            &own_person_id,
            &chat_id,
            &sender_person_id,
            after_lamport,
            limit,
        )
    }

    /// Every delivered/read watermark this device holds, for a
    /// [`SyncRecordKind::Watermarks`] record.
    ///
    /// Cumulative and therefore whole: watermarks merge by maximum, so sending
    /// the current set is always correct and a truncated page is never wrong,
    /// only incomplete.
    pub fn core_sync_watermark_page(&self, limit: u32) -> Result<SyncWatermarkPayload, CoreError> {
        let conn = self.locked_conn();
        watermark_page(&conn, limit)
    }

    /// This device's contacts, with their rosters, for a
    /// [`SyncRecordKind::Contacts`] record.
    ///
    /// The card is rebuilt from the stored contact rather than kept verbatim,
    /// and is therefore unsigned — which [`crate::parse_friend_card`] accepts,
    /// and which is honest about what vouches for it: the sibling's device
    /// signature on the record carrying it, inside a seal no third party can
    /// open. A nickname is never included, exactly as it is never written to
    /// any other wire format.
    ///
    /// **A blocked person is not offered.** The block list converges through
    /// the Settings stream ([`SYNC_BLOCKED_SETTING_KEY`]), and the two facts
    /// have to agree or the fleet oscillates: a device that both blocks Ash and
    /// keeps publishing Ash's card would re-seed Ash onto a sibling that had
    /// just dropped them, round after round. Blocking is the stronger
    /// statement, so it wins the page.
    pub fn core_sync_contacts_page(&self, limit: u32) -> Result<SyncContactsPayload, CoreError> {
        let mut entries = Vec::new();
        let blocked = self.list_blocked_users()?;
        for contact in self.list_contacts()? {
            if entries.len() >= limit as usize {
                break;
            }
            if blocked.contains(&contact.user_id) {
                continue;
            }
            let card = crate::FriendCard {
                name: contact.name.clone(),
                sign_pk: contact.sign_pk.clone(),
                agree_pk: contact.agree_pk.clone(),
                relay_url: contact.relay_url.clone(),
                relay_token: contact.relay_token.clone(),
                signature: None,
                roster_head_hash: None,
            };
            let card_json = serde_json::to_string(&card)
                .map_err(|e| CoreError::InvalidFriendCard(e.to_string()))?;
            entries.push(SyncContactEntry {
                roster: self.contact_roster_state(contact.user_id.clone())?.roster,
                person_id: contact.user_id,
                card_json,
            });
        }
        Ok(SyncContactsPayload { entries })
    }

    /// This device's groups, for a [`SyncRecordKind::Groups`] record.
    pub fn core_sync_groups_page(&self, limit: u32) -> Result<SyncGroupsPayload, CoreError> {
        let mut groups = self.list_groups()?;
        groups.truncate(limit as usize);
        Ok(SyncGroupsPayload { groups })
    }

    /// The shared settings this device holds, for a
    /// [`SyncRecordKind::Settings`] record.
    pub fn core_sync_settings_page(&self, limit: u32) -> Result<SyncSettingsPayload, CoreError> {
        let conn = self.locked_conn();
        let mut entries = list_settings(&conn)?;
        entries.truncate(limit as usize);
        Ok(SyncSettingsPayload { entries })
    }

    /// Write a shared setting locally. `false` means a value that wins the
    /// merge order was already stored, so nothing changed.
    ///
    /// The entry's `author_device_id` is **overwritten** with this device's own
    /// authoring id, whatever the caller passed. A local write is authored
    /// here by definition, and letting a shell name somebody else as the author
    /// would hand it the tiebreak — the one field in the order that exists
    /// precisely because no writer may choose its own position in it.
    pub fn core_sync_put_setting(&self, entry: SyncSettingEntry) -> Result<bool, CoreError> {
        let conn = self.locked_conn();
        let author_device_id = crate::store::own_authoring_device_id(&conn)?;
        put_setting(
            &conn,
            &SyncSettingEntry {
                author_device_id,
                ..entry
            },
        )
    }

    /// One shared setting, or `None` if it has never been written.
    pub fn core_sync_get_setting(
        &self,
        key: String,
    ) -> Result<Option<SyncSettingEntry>, CoreError> {
        let conn = self.locked_conn();
        setting(&conn, &key)
    }

    /// Publish this device's block list into the shared Settings stream (§8),
    /// so blocking on one device is blocking on all of them.
    ///
    /// Called by whichever surface performs the block, in the same breath as
    /// [`MessageStore::block_user`] — the local table is what refuses mail here
    /// and now, and this is what carries the decision to a sibling that may not
    /// be reachable for a week. `epoch` is milliseconds, like every other
    /// setting.
    pub fn core_sync_publish_block_list(&self, epoch: u64) -> Result<bool, CoreError> {
        let blocked = self.list_blocked_users()?;
        let conn = self.locked_conn();
        let author_device_id = crate::store::own_authoring_device_id(&conn)?;
        put_setting(
            &conn,
            &SyncSettingEntry {
                key: SYNC_BLOCKED_SETTING_KEY.to_string(),
                value: encode_block_list(blocked),
                epoch,
                author_device_id,
            },
        )
    }

    /// Record the own roster and inbox key generation the inbound transaction
    /// admits sync records against (§4, §6).
    ///
    /// This is the **ceremony's** write — §9's activation and §10's revocation
    /// — and deliberately not anti-entropy's: an applied
    /// [`SyncRecordKind::OwnRoster`] record hands its payload back through
    /// [`SyncApplyResult::own_roster`] for a ceremony to act on rather than
    /// writing here, or a gossiped record could re-widen a fleet a revocation
    /// had just narrowed.
    ///
    /// Monotone in `(recovery_epoch, seq)`, for the reason
    /// [`MessageStore::set_own_device_fleet`] is: a roster that went backwards
    /// would re-admit a device the current one has already tombstoned. A
    /// non-superseding write returns `false` and changes nothing.
    pub fn core_set_own_sync_context(
        &self,
        roster: crate::Roster,
        inbox_key_generation: u64,
    ) -> Result<bool, CoreError> {
        let conn = self.locked_conn();
        let current: Option<(i64, i64)> = conn
            .query_row(
                "SELECT recovery_epoch, seq FROM own_sync_context WHERE id = 0",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(store_err)?;
        let incoming = (roster.recovery_epoch, roster.seq);
        if let Some((epoch, seq)) = current {
            if incoming <= (epoch as u64, seq as u64) {
                return Ok(false);
            }
        }
        let encoded = crate::core_encode_roster(roster.clone())?;
        conn.execute(
            "INSERT INTO own_sync_context (id, roster, recovery_epoch, seq, inbox_key_generation)
                VALUES (0, ?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                roster = excluded.roster,
                recovery_epoch = excluded.recovery_epoch,
                seq = excluded.seq,
                inbox_key_generation = excluded.inbox_key_generation",
            params![
                encoded,
                roster.recovery_epoch as i64,
                roster.seq as i64,
                inbox_key_generation as i64
            ],
        )
        .map_err(store_err)?;
        Ok(true)
    }

    /// The stored own sync context, or `None` on an install that has never
    /// linked a device — which is the ordinary state of a v1 phone and is what
    /// makes the inbound sync dispatch inert there rather than guessing.
    pub fn core_own_sync_context(&self) -> Result<Option<OwnSyncContext>, CoreError> {
        let conn = self.locked_conn();
        own_sync_context(&conn)
    }
}

/// The own roster and inbox key generation §8's inbound dispatch admits
/// against.
#[derive(Clone, Debug, PartialEq, uniffi::Record)]
pub struct OwnSyncContext {
    pub roster: crate::Roster,
    pub inbox_key_generation: u64,
}

pub(crate) fn own_sync_context(conn: &Connection) -> Result<Option<OwnSyncContext>, CoreError> {
    let row: Option<(Vec<u8>, i64)> = conn
        .query_row(
            "SELECT roster, inbox_key_generation FROM own_sync_context WHERE id = 0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(store_err)?;
    let Some((encoded, generation)) = row else {
        return Ok(None);
    };
    Ok(Some(OwnSyncContext {
        roster: crate::core_decode_roster(encoded)?,
        inbox_key_generation: generation as u64,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync_record::{
        core_encode_sync_history, core_encode_sync_own_roster, core_encode_sync_settings,
        core_encode_sync_watermarks, core_mint_inbox_key, core_sign_sync_record,
    };
    use crate::sync_stream::core_sync_digest_gaps;
    use crate::{
        core_sign_device_cert, core_sign_roster, encode_message_body_extended,
        generate_device_keypair, generate_identity, make_friend_card, DeviceCert, DeviceKeypair,
        Group, Identity, MessageBody, StoredMessage, DEVICE_CERT_FLAG_ROSTER_SIGNING, KIND_TEXT,
        RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
    };

    const NOW: i64 = 1_700_000_000_000;

    struct Fixture {
        store: MessageStore,
        person: Identity,
        device: DeviceKeypair,
    }

    fn fixture() -> Fixture {
        Fixture {
            store: MessageStore::open(":memory:".to_string()).expect("open"),
            person: generate_identity(),
            device: generate_device_keypair(),
        }
    }

    impl Fixture {
        /// A signed record on this device's stream at `stream_seq`. Signing is
        /// real — the whole point of the apply path is that it only ever sees
        /// records something already admitted.
        fn record(&self, kind: SyncRecordKind, stream_seq: u64, payload: Vec<u8>) -> SyncRecord {
            core_sign_sync_record(
                SyncRecord {
                    kind,
                    person_id: self.person.user_id.clone(),
                    author_device_id: Vec::new(),
                    roster_version: RosterVersion {
                        recovery_epoch: 0,
                        seq: 1,
                    },
                    inbox_key_generation: 0,
                    stream_seq,
                    timestamp_ms: NOW,
                    payload,
                    signature: Vec::new(),
                },
                self.device.sign_sk.clone(),
            )
            .expect("record signs")
        }

        fn settings(&self, stream_seq: u64, key: &str, value: &[u8], epoch: u64) -> SyncRecord {
            self.record(
                SyncRecordKind::Settings,
                stream_seq,
                core_encode_sync_settings(SyncSettingsPayload {
                    entries: vec![SyncSettingEntry {
                        key: key.to_string(),
                        value: value.to_vec(),
                        epoch,
                        author_device_id: self.device.device_id.clone(),
                    }],
                })
                .expect("encode"),
            )
        }

        fn apply(&self, record: SyncRecord) -> SyncApplyResult {
            self.store
                .core_apply_sync_record(record, NOW)
                .expect("apply")
        }

        fn cursor(&self, kind: SyncRecordKind) -> u64 {
            let conn = self.store.locked_conn();
            stream_cursor(&conn, &self.device.device_id, kind).expect("cursor")
        }
    }

    /// A history payload carrying one message from `sender` in their chat.
    fn history_of(sender: &[u8], sender_device: &[u8], lamport: u64, text: &str) -> Vec<u8> {
        let body = encode_message_body_extended(
            MessageBody {
                kind: KIND_TEXT,
                chat_id: sender.to_vec(),
                lamport,
                timestamp: NOW,
                content: text.as_bytes().to_vec(),
            },
            None,
            Some(sender_device.to_vec()),
            None,
        )
        .expect("encode body");
        core_encode_sync_history(SyncHistoryPayload {
            entries: vec![SyncHistoryEntry {
                origin_msg_id: vec![lamport as u8; 16],
                direction: SyncHistoryDirection::Received,
                sender_person_id: sender.to_vec(),
                sender_device_id: sender_device.to_vec(),
                body,
            }],
        })
        .expect("encode history")
    }

    #[test]
    fn a_cursor_only_advances_across_a_gap_free_prefix() {
        let f = fixture();
        assert_eq!(f.apply(f.settings(1, "a", b"1", 1)).through_seq, 1);
        // Record 3 lands above a hole: SYNC-1 says apply it anyway, and
        // advertise 1, so the sibling re-sends 2.
        assert_eq!(f.apply(f.settings(3, "c", b"3", 1)).through_seq, 1);
        // 2 closes the hole and the watermark jumps past 3 in one step.
        assert_eq!(f.apply(f.settings(2, "b", b"2", 1)).through_seq, 3);
        assert_eq!(f.cursor(SyncRecordKind::Settings), 3);
    }

    #[test]
    fn a_record_that_landed_above_a_hole_is_still_applied_immediately() {
        let f = fixture();
        let contact = generate_identity();
        f.apply(f.record(
            SyncRecordKind::History,
            2,
            history_of(
                &contact.user_id,
                &crate::LEGACY_DEVICE_ID,
                4,
                "we docked early",
            ),
        ));
        assert_eq!(
            f.store
                .messages_for_chat(contact.user_id.clone())
                .expect("read back")
                .len(),
            1,
            "a hole in the sync stream must not hold a delivered message hostage"
        );
        assert_eq!(f.cursor(SyncRecordKind::History), 0);
    }

    #[test]
    fn re_offering_a_record_is_free_and_changes_nothing() {
        let f = fixture();
        let record = f.settings(1, "theme", b"dark", 5);
        assert_eq!(f.apply(record.clone()).outcome, SyncApplyOutcome::Applied);
        let again = f.apply(record);
        assert_eq!(
            again.outcome,
            SyncApplyOutcome::AlreadyHeld,
            "SYNC-1 re-offers records whenever a round is cut short; that is \
             ordinary, not an error"
        );
        assert_eq!(again.applied_entries, 0);
        assert_eq!(again.through_seq, 1);
    }

    #[test]
    fn watermarks_merge_by_maximum_whichever_order_they_arrive_in() {
        let f = fixture();
        let contact = generate_identity();
        let watermark = |seq: u64, delivered: u64, read: u64| {
            f.record(
                SyncRecordKind::Watermarks,
                seq,
                core_encode_sync_watermarks(SyncWatermarkPayload {
                    entries: vec![SyncWatermarkEntry {
                        chat_id: contact.user_id.clone(),
                        subject_person_id: contact.user_id.clone(),
                        delivered_through_lamport: delivered,
                        read_through_lamport: read,
                    }],
                })
                .expect("encode"),
            )
        };
        // Newest first, then the stale one behind it -- the order a DTN hop
        // legitimately produces.
        f.apply(watermark(2, 9, 7));
        f.apply(watermark(1, 4, 2));
        assert_eq!(
            f.store
                .outgoing_receipt_through(
                    contact.user_id.clone(),
                    contact.user_id.clone(),
                    RECEIPT_TYPE_DELIVERED
                )
                .expect("delivered"),
            9
        );
        assert_eq!(
            f.store
                .outgoing_receipt_through(
                    contact.user_id.clone(),
                    contact.user_id,
                    RECEIPT_TYPE_READ
                )
                .expect("read"),
            7,
            "an older watermark arriving late must never walk a chat back to \
             unread"
        );
    }

    #[test]
    fn settings_take_the_newest_epoch_and_ignore_a_late_older_write() {
        let f = fixture();
        f.apply(f.settings(1, "theme", b"dark", 9));
        f.apply(f.settings(2, "theme", b"light", 4));
        assert_eq!(
            f.store
                .core_sync_get_setting("theme".to_string())
                .expect("get")
                .expect("stored"),
            SyncSettingEntry {
                key: "theme".to_string(),
                value: b"dark".to_vec(),
                epoch: 9,
                author_device_id: f.device.device_id.clone(),
            }
        );
    }

    #[test]
    fn an_own_roster_record_hands_its_inbox_keys_back_instead_of_storing_them() {
        let f = fixture();
        let roster = own_roster(&f.person, &f.device);
        let key = core_mint_inbox_key(0);
        let result = f.apply(
            f.record(
                SyncRecordKind::OwnRoster,
                1,
                core_encode_sync_own_roster(SyncOwnRosterPayload {
                    roster,
                    inbox_keys: vec![key.clone()],
                })
                .expect("encode"),
            ),
        );
        assert_eq!(
            result.own_roster.expect("payload").inbox_keys,
            vec![key.clone()],
            "§6 keeps inbox key custody with the shell, so the record is \
             admitted, positioned, and handed straight back"
        );
        let conn = f.store.locked_conn();
        let holds_secret: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sync_stream_records WHERE sealed IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(
            holds_secret, 0,
            "nothing this record carried was written to the database"
        );
    }

    #[test]
    fn a_contact_card_may_not_be_filed_under_another_persons_id() {
        let f = fixture();
        let contact = generate_identity();
        let impostor = generate_identity();
        let card = make_friend_card("Ash".to_string(), contact.clone(), None, None).expect("card");
        let err = f
            .store
            .core_apply_sync_record(
                f.record(
                    SyncRecordKind::Contacts,
                    1,
                    crate::core_encode_sync_contacts(SyncContactsPayload {
                        entries: vec![SyncContactEntry {
                            person_id: impostor.user_id,
                            card_json: card,
                            roster: None,
                        }],
                    })
                    .expect("encode"),
                ),
                NOW,
            )
            .expect_err("a card and the id it is filed under must agree");
        assert!(matches!(err, CoreError::Malformed(_)));
    }

    #[test]
    fn a_contact_this_device_already_knows_is_not_overwritten() {
        let f = fixture();
        let contact = generate_identity();
        f.store
            .upsert_contact(crate::Contact {
                user_id: contact.user_id.clone(),
                name: "Ash on this phone".to_string(),
                sign_pk: contact.sign_pk.clone(),
                agree_pk: contact.agree_pk.clone(),
                relay_url: None,
                relay_token: None,
                nickname: None,
            })
            .expect("local contact");
        let card = make_friend_card("Ash elsewhere".to_string(), contact.clone(), None, None)
            .expect("card");
        f.apply(
            f.record(
                SyncRecordKind::Contacts,
                1,
                crate::core_encode_sync_contacts(SyncContactsPayload {
                    entries: vec![SyncContactEntry {
                        person_id: contact.user_id.clone(),
                        card_json: card,
                        roster: None,
                    }],
                })
                .expect("encode"),
            ),
        );
        assert_eq!(
            f.store
                .get_contact(contact.user_id)
                .expect("contact")
                .expect("still there")
                .name,
            "Ash on this phone",
            "a sibling's older view of a contact must not revert what this \
             device learned first"
        );
    }

    #[test]
    fn groups_arrive_whole_so_a_new_device_needs_no_re_invite() {
        let f = fixture();
        let group = Group {
            id: vec![7u8; 16],
            name: "Deck 9".to_string(),
            member_user_ids: vec![f.person.user_id.clone()],
            key: vec![3u8; 32],
            metadata_revision: 1,
            metadata_changed_by: f.person.user_id.clone(),
        };
        f.apply(
            f.record(
                SyncRecordKind::Groups,
                1,
                crate::core_encode_sync_groups(SyncGroupsPayload {
                    groups: vec![group.clone()],
                })
                .expect("encode"),
            ),
        );
        let stored = f.store.list_groups().expect("groups");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, group.id);
        assert_eq!(stored[0].name, group.name);
        assert_eq!(stored[0].member_user_ids, group.member_user_ids);
        assert_eq!(
            stored[0].key, group.key,
            "§11 leaves group crypto alone: the key travels, so the new device \
             needs no re-invite from every member"
        );
        // The metadata pair travels with the group. It has to: the store's
        // group upsert refuses anything older than what it holds by exactly
        // this pair, so a record that arrived at revision 0 would be correctly
        // ignored by every device that had ever renamed the group — a rename
        // that converges once and then never again.
        assert_eq!(stored[0].metadata_revision, group.metadata_revision);
        assert_eq!(stored[0].metadata_changed_by, group.metadata_changed_by);
    }

    #[test]
    fn a_restore_that_excludes_history_keeps_stream_positions_and_drops_the_content() {
        let dir = std::env::temp_dir().join(format!("cm-sync-restore-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("restored.sqlite");
        let path_str = path.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&path);

        let person = generate_identity();
        let device = generate_device_keypair();
        {
            let store = MessageStore::open(path_str.clone()).expect("open");
            let record = core_sign_sync_record(
                SyncRecord {
                    kind: SyncRecordKind::Settings,
                    person_id: person.user_id.clone(),
                    author_device_id: Vec::new(),
                    roster_version: RosterVersion {
                        recovery_epoch: 0,
                        seq: 1,
                    },
                    inbox_key_generation: 0,
                    stream_seq: 4,
                    timestamp_ms: NOW,
                    payload: core_encode_sync_settings(SyncSettingsPayload { entries: vec![] })
                        .expect("encode"),
                    signature: Vec::new(),
                },
                device.sign_sk.clone(),
            )
            .expect("signs");
            let sealed = crate::core_seal_sync_record(
                record.clone(),
                person.clone(),
                core_mint_inbox_key(0),
            )
            .expect("seal");
            store
                .core_sync_retain_record(record, sealed, NOW)
                .expect("retain");
        }

        crate::sanitize_restored_message_store_with_options(
            path_str.clone(),
            crate::BackupContentOptions {
                include_message_history: false,
                include_pending_deliveries_for_others: false,
            },
            NOW,
        )
        .expect("sanitize");

        let store = MessageStore::open(path_str).expect("reopen");
        assert!(
            store
                .core_sync_backfill_records(
                    vec![SyncGap {
                        author_device_id: device.device_id.clone(),
                        kind: core_sync_record_kind_wire(SyncRecordKind::Settings),
                        after_seq: 0,
                        through_seq: 99,
                    }],
                    32,
                )
                .expect("offers")
                .is_empty(),
            "excluded content must not come back through a sibling's next digest"
        );
        assert_eq!(
            store
                .core_sync_next_stream_seq(device.device_id, SyncRecordKind::Settings)
                .expect("next"),
            5,
            "the position survives, or a restored device would silently reuse a \
             slot its siblings already hold"
        );
        let _ = std::fs::remove_file(dir.join("restored.sqlite"));
    }

    #[test]
    fn backfill_offers_only_ever_return_this_devices_own_sealed_records() {
        let f = fixture();
        let sibling = generate_device_keypair();
        let key = core_mint_inbox_key(0);
        let mine = f.settings(1, "theme", b"dark", 1);
        let sealed =
            crate::core_seal_sync_record(mine.clone(), f.person.clone(), key).expect("seal");
        assert!(f
            .store
            .core_sync_retain_record(mine, sealed, NOW)
            .expect("retain"));

        // A sibling's record: applied, positioned, but never re-forwardable.
        let theirs = core_sign_sync_record(
            SyncRecord {
                stream_seq: 1,
                ..f.settings(1, "sibling", b"x", 1)
            },
            sibling.sign_sk.clone(),
        )
        .expect("sibling signs");
        f.apply(theirs);

        let digest = f
            .store
            .core_sync_digest(f.person.user_id.clone())
            .expect("digest");
        assert_eq!(digest.streams.len(), 2, "both streams are advertised");

        let gaps = core_sync_digest_gaps(
            SyncDigest {
                person_id: f.person.user_id.clone(),
                streams: Vec::new(),
            },
            digest,
        )
        .expect("gaps");
        let offers = f
            .store
            .core_sync_backfill_records(gaps, 32)
            .expect("offers");
        assert_eq!(offers.len(), 1);
        assert_eq!(
            offers[0].author_device_id, f.device.device_id,
            "only the author can re-seal a record after a roster change, so \
             only the author answers for it"
        );
    }

    #[test]
    fn a_reseal_replaces_the_bytes_without_moving_the_slot() {
        let f = fixture();
        let record = f.settings(1, "theme", b"dark", 1);
        let first =
            crate::core_seal_sync_record(record.clone(), f.person.clone(), core_mint_inbox_key(0))
                .expect("seal");
        f.store
            .core_sync_retain_record(record.clone(), first.clone(), NOW)
            .expect("retain");
        let second =
            crate::core_seal_sync_record(record.clone(), f.person.clone(), core_mint_inbox_key(0))
                .expect("re-seal");
        assert!(f
            .store
            .core_sync_reseal_record(record, second.clone())
            .expect("reseal"));

        let gaps = vec![SyncGap {
            author_device_id: f.device.device_id.clone(),
            kind: core_sync_record_kind_wire(SyncRecordKind::Settings),
            after_seq: 0,
            through_seq: 1,
        }];
        let offers = f
            .store
            .core_sync_backfill_records(gaps, 32)
            .expect("offers");
        assert_eq!(offers.len(), 1, "still exactly one row, not two");
        assert_eq!(offers[0].sealed, second.sealed);
        assert_ne!(offers[0].sealed, first.sealed);
    }

    #[test]
    fn a_history_page_labels_the_persons_own_rows_authored() {
        let f = fixture();
        let contact = generate_identity();
        for (sender, lamport, text) in [
            (contact.user_id.clone(), 1u64, "are you up"),
            (f.person.user_id.clone(), 2u64, "just about"),
        ] {
            f.store
                .insert_incoming_message(
                    StoredMessage {
                        chat_id: contact.user_id.clone(),
                        sender_user_id: sender,
                        lamport,
                        timestamp: NOW,
                        kind: KIND_TEXT,
                        payload: text.as_bytes().to_vec(),
                        sender_device_id: crate::LEGACY_DEVICE_ID.to_vec(),
                    },
                    vec![lamport as u8; 16],
                    None,
                )
                .expect("insert");
        }
        let received = f
            .store
            .core_sync_history_page(
                f.person.user_id.clone(),
                contact.user_id.clone(),
                contact.user_id.clone(),
                0,
                32,
            )
            .expect("page");
        assert_eq!(received.entries.len(), 1);
        assert_eq!(
            received.entries[0].direction,
            SyncHistoryDirection::Received
        );

        let authored = f
            .store
            .core_sync_history_page(
                f.person.user_id.clone(),
                contact.user_id,
                f.person.user_id.clone(),
                0,
                32,
            )
            .expect("page");
        assert_eq!(authored.entries.len(), 1);
        assert_eq!(
            authored.entries[0].direction,
            SyncHistoryDirection::Authored,
            "SYNC-2's dedup reads this field, so the authoring side names it \
             rather than leaving the sibling to infer it"
        );
    }

    /// The tie a wall-clock epoch produces in the field, and the reason a
    /// setting names its author. Two devices write one key in one millisecond;
    /// with epoch alone each keeps its own value forever, because neither
    /// incoming record is strictly newer, and no surface anywhere shows the
    /// split.
    #[test]
    fn an_equal_epoch_is_broken_by_the_author_and_then_by_the_value() {
        let f = fixture();
        let low = vec![0x11; 16];
        let high = vec![0x22; 16];
        let put = |author: &Vec<u8>, value: &[u8], epoch: u64| {
            let conn = f.store.locked_conn();
            put_setting(
                &conn,
                &SyncSettingEntry {
                    key: "theme".to_string(),
                    value: value.to_vec(),
                    epoch,
                    author_device_id: author.clone(),
                },
            )
            .expect("put")
        };
        let stored = || {
            f.store
                .core_sync_get_setting("theme".to_string())
                .expect("get")
                .expect("stored")
        };

        assert!(put(&low, b"warm", 5));
        assert!(
            put(&high, b"cool", 5),
            "the higher author id takes an otherwise tied write"
        );
        assert_eq!(stored().value, b"cool".to_vec());
        assert!(
            !put(&low, b"warm", 5),
            "and the lower one loses however many times it is offered"
        );
        assert_eq!(stored().value, b"cool".to_vec());

        // The order is total, so one author re-offering a different value at
        // one epoch still has a decided winner rather than a coin flip.
        assert!(put(&high, b"warmer", 5));
        assert_eq!(
            stored().value,
            b"warmer".to_vec(),
            "value breaks the last tie: 'warmer' sorts above 'cool'"
        );

        // And an epoch still beats everything.
        assert!(put(&low, b"warm", 6));
        assert_eq!(stored().value, b"warm".to_vec());
    }

    /// Re-applying an identical entry changes nothing and says so, because
    /// SYNC-1 re-offers records as its ordinary way of recovering a cut round.
    #[test]
    fn re_applying_the_identical_setting_is_free() {
        let f = fixture();
        let entry = SyncSettingEntry {
            key: "theme".to_string(),
            value: b"dark".to_vec(),
            epoch: 3,
            author_device_id: vec![0x11; 16],
        };
        let conn = f.store.locked_conn();
        assert!(put_setting(&conn, &entry).expect("put"));
        assert!(!put_setting(&conn, &entry).expect("put again"));
    }

    /// A digest states two different things per stream, and collapsing them
    /// breaks anti-entropy in one direction or the other.
    ///
    /// The watermark says what this device holds — dropping a stream it cannot
    /// serve would tell that stream's author "I have none of it" and be sent the
    /// whole thing again on every encounter. The serve flag says whether it can
    /// hand any of it over — claiming it could would make every sibling plan a
    /// gap-fill against it that comes back empty, round after round.
    #[test]
    fn a_digest_advertises_a_foreign_stream_at_its_cursor_and_cannot_serve_it() {
        let f = fixture();
        let sibling = generate_device_keypair();
        let key = core_mint_inbox_key(0);

        let mine = f.settings(1, "theme", b"dark", 1);
        let sealed =
            crate::core_seal_sync_record(mine.clone(), f.person.clone(), key).expect("seal");
        f.store
            .core_sync_retain_record(mine, sealed, NOW)
            .expect("retain");

        let theirs = core_sign_sync_record(
            SyncRecord {
                stream_seq: 1,
                ..f.settings(1, "sibling", b"x", 1)
            },
            sibling.sign_sk.clone(),
        )
        .expect("sibling signs");
        f.apply(theirs);

        let digest = f
            .store
            .core_sync_digest(f.person.user_id.clone())
            .expect("digest");
        let own = digest
            .streams
            .iter()
            .find(|stream| stream.author_device_id == f.device.device_id)
            .expect("this device's own stream");
        let foreign = digest
            .streams
            .iter()
            .find(|stream| stream.author_device_id == sibling.device_id)
            .expect("the sibling's stream, held but not servable");
        assert!(own.can_serve);
        assert_eq!(own.through_seq, 1);
        assert!(
            !foreign.can_serve,
            "only the author can re-seal its own records after a roster change, \
             so only the author answers for them"
        );
        assert_eq!(
            foreign.through_seq, 1,
            "and the watermark is still advertised, or the sibling would re-send \
             the whole stream on every encounter forever"
        );
    }

    /// A stream a *newer* build wrote into this database is advertised at its
    /// cursor rather than omitted. Omitting it reads, to that newer build, as
    /// "send me all of it", every round, for as long as both installs exist.
    #[test]
    fn a_kind_this_build_cannot_name_is_still_advertised() {
        let f = fixture();
        f.apply(f.settings(1, "theme", b"dark", 1));
        {
            let conn = f.store.locked_conn();
            conn.execute(
                "INSERT INTO sync_stream_records
                    (author_device_id, kind, stream_seq, person_id, created_at_ms)
                 VALUES (?1, 250, 1, ?2, ?3)",
                params![f.device.device_id, f.person.user_id, NOW],
            )
            .expect("a newer build's row");
            conn.execute(
                "INSERT INTO sync_stream_cursors (author_device_id, kind, through_seq)
                 VALUES (?1, 250, 1)",
                params![f.device.device_id],
            )
            .expect("its cursor");
        }
        let digest = f
            .store
            .core_sync_digest(f.person.user_id.clone())
            .expect("digest");
        let unknown = digest
            .streams
            .iter()
            .find(|stream| stream.kind == 250)
            .expect("the unnameable stream is still named");
        assert_eq!(unknown.through_seq, 1);
        assert!(!unknown.can_serve);
    }

    /// Blocking is a person-level decision, so the block list travels as a
    /// shared setting and a blocked person stops being offered as a contact.
    /// Publishing them anyway would re-seed somebody the fleet had just dropped.
    #[test]
    fn a_blocked_person_leaves_the_block_list_and_the_contacts_page() {
        let f = fixture();
        let contact = generate_identity();
        f.store
            .upsert_contact(crate::Contact {
                user_id: contact.user_id.clone(),
                name: "Ash".to_string(),
                sign_pk: contact.sign_pk.clone(),
                agree_pk: contact.agree_pk.clone(),
                relay_url: None,
                relay_token: None,
                nickname: None,
            })
            .expect("contact");
        assert_eq!(
            f.store
                .core_sync_contacts_page(32)
                .expect("page")
                .entries
                .len(),
            1
        );

        f.store
            .block_user(contact.user_id.clone(), NOW)
            .expect("block");
        assert!(f
            .store
            .core_sync_publish_block_list(NOW as u64)
            .expect("publish"));
        assert!(
            f.store
                .core_sync_contacts_page(32)
                .expect("page")
                .entries
                .is_empty(),
            "a device that both blocks somebody and keeps publishing their card \
             would re-seed them onto a sibling that had just dropped them"
        );
        let entry = f
            .store
            .core_sync_get_setting(SYNC_BLOCKED_SETTING_KEY.to_string())
            .expect("setting")
            .expect("published");
        assert_eq!(decode_block_list(&entry.value), vec![contact.user_id]);
    }

    /// Applying a block entry does not merely store a row: it writes the block
    /// into the table the inbound gate actually reads. A converged row is not
    /// the property; a converged refusal is.
    #[test]
    fn applying_a_block_entry_actually_blocks() {
        let f = fixture();
        let blocked = generate_identity();
        f.apply(
            f.record(
                SyncRecordKind::Settings,
                1,
                core_encode_sync_settings(SyncSettingsPayload {
                    entries: vec![SyncSettingEntry {
                        key: SYNC_BLOCKED_SETTING_KEY.to_string(),
                        value: encode_block_list(vec![blocked.user_id.clone()]),
                        epoch: 5,
                        author_device_id: f.device.device_id.clone(),
                    }],
                })
                .expect("encode"),
            ),
        );
        assert!(f.store.is_user_blocked(blocked.user_id).expect("blocked"));
    }

    /// The merge that lets a re-learned contact converge. "Leave the local row
    /// alone" forks the fleet the first time somebody re-scans a friend card
    /// after their relay moves; "take the incoming card" oscillates, because the
    /// winner becomes whoever spoke last.
    #[test]
    fn a_re_learned_contact_is_merged_by_a_total_order_on_the_card() {
        let f = fixture();
        let contact = generate_identity();
        let card = |name: &str, relay: Option<&str>| {
            crate::core_encode_sync_contacts(SyncContactsPayload {
                entries: vec![SyncContactEntry {
                    person_id: contact.user_id.clone(),
                    card_json: make_friend_card(
                        name.to_string(),
                        contact.clone(),
                        relay.map(|r| r.to_string()),
                        None,
                    )
                    .expect("card"),
                    roster: None,
                }],
            })
            .expect("encode")
        };

        f.apply(f.record(SyncRecordKind::Contacts, 1, card("Ash", None)));
        // A fresher card whose bytes sort above the stored one's.
        f.apply(f.record(
            SyncRecordKind::Contacts,
            2,
            card("Ash", Some("https://relay.example")),
        ));
        let stored = f
            .store
            .get_contact(contact.user_id.clone())
            .expect("contact")
            .expect("still there");
        assert_eq!(
            stored.relay_url.as_deref(),
            Some("https://relay.example"),
            "an endpoint that converged on one device and not another is a fleet \
             where one phone can reach somebody and the other cannot"
        );

        // And the older card, offered again, loses — deterministically, so the
        // two devices do not trade it back and forth forever.
        f.apply(f.record(SyncRecordKind::Contacts, 3, card("Ash", None)));
        assert_eq!(
            f.store
                .get_contact(contact.user_id)
                .expect("contact")
                .expect("still there")
                .relay_url
                .as_deref(),
            Some("https://relay.example")
        );
    }

    /// A local nickname is not the sibling's to erase. It never rides a wire
    /// format, so a merged card has no opinion about it and must leave it
    /// standing.
    #[test]
    fn a_merge_keeps_the_local_nickname() {
        let f = fixture();
        let contact = generate_identity();
        f.store
            .upsert_contact(crate::Contact {
                user_id: contact.user_id.clone(),
                name: "Ash".to_string(),
                sign_pk: contact.sign_pk.clone(),
                agree_pk: contact.agree_pk.clone(),
                relay_url: None,
                relay_token: None,
                nickname: None,
            })
            .expect("contact");
        f.store
            .set_contact_nickname(contact.user_id.clone(), Some("Mum".to_string()))
            .expect("nickname");
        f.apply(
            f.record(
                SyncRecordKind::Contacts,
                1,
                crate::core_encode_sync_contacts(SyncContactsPayload {
                    entries: vec![SyncContactEntry {
                        person_id: contact.user_id.clone(),
                        card_json: make_friend_card(
                            "Ash".to_string(),
                            contact.clone(),
                            Some("https://relay.example".to_string()),
                            None,
                        )
                        .expect("card"),
                        roster: None,
                    }],
                })
                .expect("encode"),
            ),
        );
        let stored = f
            .store
            .get_contact(contact.user_id)
            .expect("contact")
            .expect("still there");
        assert_eq!(stored.relay_url.as_deref(), Some("https://relay.example"));
        assert_eq!(stored.nickname.as_deref(), Some("Mum"));
    }

    /// A digest is read and handed back, never filed. Filing one would put
    /// yesterday's watermarks in the backfill pool, where a sibling would ask
    /// for them, apply them, advertise them, and be offered them again.
    #[test]
    fn a_digest_record_is_read_and_never_given_a_stream_slot() {
        let f = fixture();
        let payload = crate::core_encode_sync_digest(SyncDigest {
            person_id: f.person.user_id.clone(),
            streams: vec![SyncStreamDigest {
                author_device_id: f.device.device_id.clone(),
                kind: core_sync_record_kind_wire(SyncRecordKind::Settings),
                through_seq: 7,
                can_serve: true,
            }],
        })
        .expect("encode");
        let record = f.record(SyncRecordKind::Digest, 1, payload);

        let first = f.apply(record.clone());
        assert_eq!(first.outcome, SyncApplyOutcome::Read);
        assert_eq!(
            first.peer_digest.expect("handed back").streams[0].through_seq,
            7
        );
        // Twice, because two digests from one device are two different claims
        // about a moving watermark and deduping the second would freeze the
        // exchange on the first.
        assert_eq!(f.apply(record).outcome, SyncApplyOutcome::Read);
        assert_eq!(
            f.store
                .core_sync_digest(f.person.user_id.clone())
                .expect("digest")
                .streams
                .len(),
            0,
            "nothing was filed, so nothing is advertised"
        );
        assert!(matches!(
            {
                let conn = f.store.locked_conn();
                let sealed = crate::core_seal_sync_record(
                    f.record(SyncRecordKind::Digest, 2, Vec::new()),
                    f.person.clone(),
                    core_mint_inbox_key(0),
                );
                sealed.map(|sealed| {
                    retain(
                        &conn,
                        &f.record(SyncRecordKind::Digest, 2, Vec::new()),
                        &sealed,
                        NOW,
                    )
                })
            },
            Ok(Err(CoreError::Malformed(_)))
        ));
    }

    fn own_roster(person: &Identity, device: &DeviceKeypair) -> crate::Roster {
        let cert = core_sign_device_cert(
            DeviceCert {
                person_id: person.user_id.clone(),
                device_sign_pk: device.sign_pk.clone(),
                device_agree_pk: device.agree_pk.clone(),
                added_epoch: 0,
                flags: DEVICE_CERT_FLAG_ROSTER_SIGNING,
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
            },
            person.sign_sk.clone(),
        )
        .expect("cert signs");
        core_sign_roster(
            crate::Roster {
                person_id: person.user_id.clone(),
                recovery_epoch: 0,
                seq: 0,
                devices: vec![cert],
                tombstones: Vec::new(),
                approving_device_id: device.device_id.clone(),
                inbox_key_generation: 0,
                signer_sign_pk: Vec::new(),
                signature: Vec::new(),
            },
            person.sign_sk.clone(),
        )
        .expect("roster signs")
    }
}
