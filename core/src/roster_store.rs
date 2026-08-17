//! Persistence for the device rosters this device holds about its contacts
//! (`specs/multi-device-v1.md` §4).
//!
//! `device_roster.rs` decides what a roster *means* — DL-1 ordering, DL-2 fork
//! quarantine, DL-4 tombstones — and holds no state at all. This module is the
//! other half: it writes that verdict down and reads it back, and it holds no
//! policy. The split is deliberate; the decision rules are the part that must
//! stay testable without a database.
//!
//! Two tables, joined by the contact's `user_id` (their `person_id`, §3):
//!
//! * `contact_rosters` — one row per contact: the roster document's scalars,
//!   its head hash, and the DL-2 quarantine bit. Quarantine lives beside the
//!   roster it protects because it is *sticky*: once set it survives every
//!   later, perfectly valid roster that person gossips, and only a person can
//!   clear a fork.
//! * `contact_devices` — one row per device certificate, plus one row per
//!   tombstone (DL-4: a revoked device id is kept forever, so the roster that
//!   drops it can be recognized as not-a-later-version). `ordinal` preserves
//!   document order, which matters more than it looks: the roster signature and
//!   the head hash are computed over the document as it was signed, so a
//!   round-trip that reordered the devices would recompute a different head and
//!   the next gossip copy would read as a DL-2 fork.
//!
//! DL-5 is inherited rather than restated: a [`Roster`] has no field that can
//! carry an endpoint, so neither does a row here. No relay URL, no host, no
//! discovered address — keys, ids, and counters only. These rows ride a
//! `.cmbak` with the contacts they belong to and need no restore-time
//! sanitization for the same reason: nothing in them describes a network.

use rusqlite::{params, Connection, OptionalExtension};

use crate::device_roster::{roster_head_hash, DeviceCert, DeviceTombstone, Roster};
use crate::store::store_err;
use crate::CoreError;

pub(crate) const CONTACT_ROSTER_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS contact_rosters (
    person_user_id       BLOB PRIMARY KEY,
    recovery_epoch       INTEGER NOT NULL,
    seq                  INTEGER NOT NULL,
    approving_device_id  BLOB NOT NULL,
    inbox_key_generation INTEGER NOT NULL,
    signer_sign_pk       BLOB NOT NULL,
    signature            BLOB NOT NULL,
    head_hash            BLOB NOT NULL,
    quarantined          INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS contact_devices (
    person_user_id  BLOB NOT NULL,
    device_id       BLOB NOT NULL,
    ordinal         INTEGER NOT NULL,
    device_sign_pk  BLOB,
    device_agree_pk BLOB,
    added_epoch     INTEGER,
    flags           INTEGER,
    signer_sign_pk  BLOB,
    signature       BLOB,
    -- NULL for an active certificate; the revoking `seq` for a tombstone
    -- (DL-4). The certificate columns are NULL on a tombstone row because a
    -- tombstone only has to name what may never come back, never its keys.
    revoked_at_seq  INTEGER,
    PRIMARY KEY (person_user_id, device_id)
);
CREATE INDEX IF NOT EXISTS idx_contact_devices_person_ordinal
    ON contact_devices(person_user_id, ordinal);
";

/// What this device holds about one contact's roster.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ContactRosterState {
    /// The last accepted roster, or `None` for a contact who has never
    /// gossiped one — which is every v1 peer, and is exactly the synthetic
    /// one-device person of §5.
    pub roster: Option<Roster>,
    /// DL-2: this person's roster updates are quarantined. Sticky, and never
    /// resolved by arithmetic.
    pub quarantined: bool,
}

/// Whether a contact's roster vouches for one of their devices.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContactDeviceState {
    /// No roster names this device — including every device of a contact who
    /// has never sent a roster at all.
    Unknown,
    Active,
    /// DL-4: tombstoned, forever. Re-linking the same hardware mints a fresh
    /// key and therefore a different device id.
    Revoked,
}

/// The scalar half of a stored roster: everything that is not a device row.
struct StoredRosterHeader {
    recovery_epoch: u64,
    seq: u64,
    approving_device_id: Vec<u8>,
    inbox_key_generation: u64,
    signer_sign_pk: Vec<u8>,
    signature: Vec<u8>,
    head_hash: Vec<u8>,
    quarantined: bool,
}

/// Read back what was stored for one person.
///
/// The head hash is re-derived from the reconstructed document and checked
/// against the one that was written. A mismatch means the round-trip lost or
/// reordered something, and that is not a survivable bug to paper over: the
/// stored roster is what DL-2 compares an incoming copy against, so a document
/// that came back subtly different would make the next honest gossip copy look
/// like a fork and quarantine a person for good.
///
/// So it self-heals rather than bricking. Returning an error here would fail
/// every read for that contact forever — `apply_contact_roster` could not even
/// accept the corrected document, because it must load the stored one first —
/// which is a permanent outage caused by local damage rather than by anything
/// the person did. Instead the damaged rows are deleted and the contact reads
/// as "no roster yet", which is the ordinary pre-gossip state of every v1 peer
/// (§5's synthetic one-device person). The next honest gossip re-establishes
/// it as a [`crate::RosterUpdateReason::FirstRoster`].
///
/// The one thing that survives the delete is the DL-2 quarantine bit. A fork
/// is resolved by a person, never by arithmetic and certainly not by a
/// corrupted row: dropping the quarantine here would let local damage clear a
/// safety state a human was supposed to clear.
pub(crate) fn load_state(
    conn: &Connection,
    person_user_id: &[u8],
) -> Result<ContactRosterState, CoreError> {
    let header = conn
        .query_row(
            "SELECT recovery_epoch, seq, approving_device_id, inbox_key_generation,
                    signer_sign_pk, signature, head_hash, quarantined
             FROM contact_rosters WHERE person_user_id = ?1",
            params![person_user_id],
            |row| {
                Ok(StoredRosterHeader {
                    recovery_epoch: row.get::<_, i64>(0)? as u64,
                    seq: row.get::<_, i64>(1)? as u64,
                    approving_device_id: row.get(2)?,
                    inbox_key_generation: row.get::<_, i64>(3)? as u64,
                    signer_sign_pk: row.get(4)?,
                    signature: row.get(5)?,
                    head_hash: row.get(6)?,
                    quarantined: row.get::<_, i64>(7)? != 0,
                })
            },
        )
        .optional()
        .map_err(store_err)?;

    let Some(header) = header else {
        return Ok(ContactRosterState {
            roster: None,
            quarantined: false,
        });
    };
    // The quarantine-only row: no document, only the sticky DL-2 bit. It is
    // what self-healing leaves behind, and an empty head hash is a value no
    // real roster can have (`roster_head_hash` is always 32 bytes).
    if header.head_hash.is_empty() {
        return Ok(ContactRosterState {
            roster: None,
            quarantined: header.quarantined,
        });
    }

    let mut devices = Vec::new();
    let mut tombstones = Vec::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT device_id, device_sign_pk, device_agree_pk, added_epoch, flags,
                        signer_sign_pk, signature, revoked_at_seq
                 FROM contact_devices
                 WHERE person_user_id = ?1
                 ORDER BY revoked_at_seq IS NOT NULL ASC, ordinal ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![person_user_id], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                    row.get::<_, Option<Vec<u8>>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            })
            .map_err(store_err)?;
        for row in rows {
            let (
                device_id,
                device_sign_pk,
                device_agree_pk,
                added_epoch,
                flags,
                cert_signer_sign_pk,
                cert_signature,
                revoked_at_seq,
            ) = row.map_err(store_err)?;
            match revoked_at_seq {
                Some(revoked_at_seq) => tombstones.push(DeviceTombstone {
                    device_id,
                    revoked_at_seq: revoked_at_seq as u64,
                }),
                None => devices.push(DeviceCert {
                    person_id: person_user_id.to_vec(),
                    device_sign_pk: cert_column(device_sign_pk, "device_sign_pk")?,
                    device_agree_pk: cert_column(device_agree_pk, "device_agree_pk")?,
                    added_epoch: added_epoch.unwrap_or_default() as u64,
                    flags: flags.unwrap_or_default() as u32,
                    signer_sign_pk: cert_column(cert_signer_sign_pk, "signer_sign_pk")?,
                    signature: cert_column(cert_signature, "signature")?,
                }),
            }
        }
    }

    let roster = Roster {
        person_id: person_user_id.to_vec(),
        recovery_epoch: header.recovery_epoch,
        seq: header.seq,
        devices,
        tombstones,
        approving_device_id: header.approving_device_id,
        inbox_key_generation: header.inbox_key_generation,
        signer_sign_pk: header.signer_sign_pk,
        signature: header.signature,
    };
    if roster_head_hash(&roster) != header.head_hash {
        self_heal_damaged_roster(conn, person_user_id, header.quarantined)?;
        return Ok(ContactRosterState {
            roster: None,
            quarantined: header.quarantined,
        });
    }
    Ok(ContactRosterState {
        roster: Some(roster),
        quarantined: header.quarantined,
    })
}

/// Drop a person's damaged roster rows, keeping only the DL-2 quarantine bit
/// if it was set. The bit is carried by a row with an empty head hash, which
/// [`load_state`] reads as "no document, quarantine only" — no real roster can
/// take that shape, so the sentinel can never be mistaken for one.
fn self_heal_damaged_roster(
    conn: &Connection,
    person_user_id: &[u8],
    quarantined: bool,
) -> Result<(), CoreError> {
    delete_person(conn, person_user_id)?;
    if quarantined {
        conn.execute(
            "INSERT INTO contact_rosters
                (person_user_id, recovery_epoch, seq, approving_device_id,
                 inbox_key_generation, signer_sign_pk, signature, head_hash, quarantined)
             VALUES (?1, 0, 0, X'', 0, X'', X'', X'', 1)",
            params![person_user_id],
        )
        .map_err(store_err)?;
    }
    Ok(())
}

/// Delete roster rows for anyone who is no longer a contact. Called once from
/// `MessageStore::open`.
///
/// `delete_contact` already removes a departing contact's roster, so this is
/// the sweep for the paths that do not go through it: a restored `.cmbak`
/// written by a build whose delete path differed, or rows left by an
/// interrupted write. It matters because a roster is authority data — "is this
/// device one of theirs" — and a stale one belonging to a person the user
/// removed and later re-added must never answer that question.
pub(crate) fn sweep_orphaned_persons(conn: &Connection) -> Result<(), CoreError> {
    conn.execute(
        "DELETE FROM contact_rosters
         WHERE person_user_id NOT IN (SELECT user_id FROM contacts)",
        [],
    )
    .map_err(store_err)?;
    conn.execute(
        "DELETE FROM contact_devices
         WHERE person_user_id NOT IN (SELECT user_id FROM contacts)",
        [],
    )
    .map_err(store_err)?;
    Ok(())
}

/// Replace what is stored for this person with `roster`, carrying the
/// quarantine bit the caller was handed by `core_roster_accept`.
///
/// Only a roster that already validated against the contact's person root
/// should reach here, so `person_id` agreement between the document and its
/// certificates is an invariant rather than a check — except that storing a
/// certificate for the wrong person would silently corrupt the round-trip, so
/// it is checked anyway.
pub(crate) fn save_roster(
    conn: &Connection,
    roster: &Roster,
    quarantined: bool,
) -> Result<(), CoreError> {
    for cert in &roster.devices {
        if cert.person_id != roster.person_id {
            return Err(CoreError::Store(
                "device certificate names a different person than its roster".into(),
            ));
        }
    }
    conn.execute(
        "INSERT INTO contact_rosters
            (person_user_id, recovery_epoch, seq, approving_device_id,
             inbox_key_generation, signer_sign_pk, signature, head_hash, quarantined)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(person_user_id) DO UPDATE SET
             recovery_epoch = excluded.recovery_epoch,
             seq = excluded.seq,
             approving_device_id = excluded.approving_device_id,
             inbox_key_generation = excluded.inbox_key_generation,
             signer_sign_pk = excluded.signer_sign_pk,
             signature = excluded.signature,
             head_hash = excluded.head_hash,
             quarantined = excluded.quarantined",
        params![
            roster.person_id,
            roster.recovery_epoch as i64,
            roster.seq as i64,
            roster.approving_device_id,
            roster.inbox_key_generation as i64,
            roster.signer_sign_pk,
            roster.signature,
            roster_head_hash(roster),
            i64::from(quarantined),
        ],
    )
    .map_err(store_err)?;
    // The device rows are a projection of the document, so they are rewritten
    // wholesale rather than merged: a merge could leave behind a device the new
    // roster dropped, and "still listed here" is what an authority check reads.
    conn.execute(
        "DELETE FROM contact_devices WHERE person_user_id = ?1",
        params![roster.person_id],
    )
    .map_err(store_err)?;
    for (ordinal, cert) in roster.devices.iter().enumerate() {
        conn.execute(
            "INSERT INTO contact_devices
                (person_user_id, device_id, ordinal, device_sign_pk, device_agree_pk,
                 added_epoch, flags, signer_sign_pk, signature, revoked_at_seq)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL)",
            params![
                roster.person_id,
                cert.device_id(),
                ordinal as i64,
                cert.device_sign_pk,
                cert.device_agree_pk,
                cert.added_epoch as i64,
                cert.flags as i64,
                cert.signer_sign_pk,
                cert.signature,
            ],
        )
        .map_err(store_err)?;
    }
    for (ordinal, tombstone) in roster.tombstones.iter().enumerate() {
        conn.execute(
            "INSERT INTO contact_devices
                (person_user_id, device_id, ordinal, revoked_at_seq)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                roster.person_id,
                tombstone.device_id,
                ordinal as i64,
                tombstone.revoked_at_seq as i64,
            ],
        )
        .map_err(store_err)?;
    }
    Ok(())
}

/// DL-2: mark this person's roster updates quarantined. A no-op when nothing
/// is stored for them, which cannot arise from a real fork — a fork is two
/// documents at one version, so one of them is already stored.
pub(crate) fn set_quarantined(
    conn: &Connection,
    person_user_id: &[u8],
    quarantined: bool,
) -> Result<(), CoreError> {
    conn.execute(
        "UPDATE contact_rosters SET quarantined = ?2 WHERE person_user_id = ?1",
        params![person_user_id, i64::from(quarantined)],
    )
    .map_err(store_err)?;
    Ok(())
}

/// DL-2 for a person this device may hold no *stored* roster row for.
///
/// [`set_quarantined`] is an update and deliberately so: a fork between two
/// gossiped contact rosters is two documents at one version, so one of them is
/// already in `contact_rosters` and there is always a row to flag. §10.1's
/// handoff path broke that assumption — this person's own roster lives in
/// `own_roster`, not here, so a fork discovered while adopting a sibling's
/// rotation announcement has no row to update and the sticky bit would be
/// dropped on the floor.
///
/// So this inserts the quarantine-only shape [`load_state`] already knows how
/// to read: an empty `head_hash`, which is a value no real roster can have,
/// and the bit. Nothing else is written, because nothing else is known — the
/// document itself is not this table's to keep.
pub(crate) fn mark_quarantined(conn: &Connection, person_user_id: &[u8]) -> Result<(), CoreError> {
    conn.execute(
        "INSERT INTO contact_rosters
            (person_user_id, recovery_epoch, seq, approving_device_id,
             inbox_key_generation, signer_sign_pk, signature, head_hash, quarantined)
         VALUES (?1, 0, 0, X'', 0, X'', X'', X'', 1)
         ON CONFLICT(person_user_id) DO UPDATE SET quarantined = 1",
        params![person_user_id],
    )
    .map_err(store_err)?;
    Ok(())
}

/// Whether this person's roster vouches for `device_id`.
pub(crate) fn device_state(
    conn: &Connection,
    person_user_id: &[u8],
    device_id: &[u8],
) -> Result<ContactDeviceState, CoreError> {
    let revoked: Option<Option<i64>> = conn
        .query_row(
            "SELECT revoked_at_seq FROM contact_devices
             WHERE person_user_id = ?1 AND device_id = ?2",
            params![person_user_id, device_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_err)?;
    Ok(match revoked {
        None => ContactDeviceState::Unknown,
        Some(None) => ContactDeviceState::Active,
        Some(Some(_)) => ContactDeviceState::Revoked,
    })
}

/// The active device ids of one contact, in roster document order.
pub(crate) fn active_device_ids(
    conn: &Connection,
    person_user_id: &[u8],
) -> Result<Vec<Vec<u8>>, CoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT device_id FROM contact_devices
             WHERE person_user_id = ?1 AND revoked_at_seq IS NULL
             ORDER BY ordinal ASC",
        )
        .map_err(store_err)?;
    let rows = stmt
        .query_map(params![person_user_id], |row| row.get(0))
        .map_err(store_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
}

/// Forget everything held about one person's devices. Called from
/// `delete_contact`: the roster describes a contact, so it goes when they do.
pub(crate) fn delete_person(conn: &Connection, person_user_id: &[u8]) -> Result<(), CoreError> {
    conn.execute(
        "DELETE FROM contact_rosters WHERE person_user_id = ?1",
        params![person_user_id],
    )
    .map_err(store_err)?;
    conn.execute(
        "DELETE FROM contact_devices WHERE person_user_id = ?1",
        params![person_user_id],
    )
    .map_err(store_err)?;
    Ok(())
}

fn cert_column(value: Option<Vec<u8>>, field: &str) -> Result<Vec<u8>, CoreError> {
    value.ok_or_else(|| CoreError::Store(format!("stored device certificate is missing {field}")))
}
