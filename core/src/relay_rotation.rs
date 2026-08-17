//! Relay credential rotation: withdrawing a revoked device's mailbox access
//! (`specs/multi-device-v1.md` §10 step 2).
//!
//! §10 step 1 rotates the *inbox key*, which stops a revoked device reading
//! anything sealed after its revocation. It does nothing at all about the
//! relay, and the spec names the hole exactly: **relayd scopes fetch and ack
//! by `family_token` alone**, so a device that was cut from the roster keeps
//! full read access to the family mailbox — and ack *deletes* — for as long as
//! it holds the old token. A thief who cannot read a word of the mail can
//! still empty the mailbox before anybody else fetches it. That is why the
//! token has to move too, and why this module exists.
//!
//! # The three legs, and which of them is new
//!
//! 1. **The server.** relayd re-keys the family in place
//!    (`RelayStore::rotate_family_token`, `POST /family/rotate`). Every
//!    envelope, presence row and push registration moves with the family in
//!    one transaction, ids and hints untouched: **rotate, then drain**. A
//!    sibling that slept through the whole ceremony fetches exactly what it
//!    would have fetched, from exactly the cursor it held, once it learns the
//!    new token. Nothing is deleted to make a rotation happen.
//! 2. **Own devices** get the new *member* token — the fetch/ack credential —
//!    through §8's Settings stream, under
//!    [`SYNC_RELAY_CREDENTIAL_SETTING_KEY`]. That stream is sealed to the
//!    inbox key, which §10.1 has *already* rotated by the time this runs, so
//!    the announcement is structurally unreadable to the device being cut off.
//!    [`MessageStore::commit_relay_rotation`] refuses to publish before that
//!    ordering holds, because publishing under the superseded generation would
//!    hand the thief its replacement credential in the same breath as taking
//!    the old one away.
//! 3. **Contacts** get the new *deposit* token through the shipped
//!    `CAP_RELAY_UPDATE` kind-9 notice, at a bumped `relay_epoch`. Nothing
//!    here is new: `encode_relay_update_content` already attenuates whatever
//!    it is handed to deposit class, `apply_contact_relay_update` already
//!    enforces self-scoping and monotonic epochs, and the shells already
//!    broadcast on an epoch bump. §10.2 says "push it via the existing
//!    relay-update machinery", so this leg produces the epoch and the
//!    recipient list and reuses that machinery verbatim.
//!
//! # What the cut-off costs, stated rather than hidden
//!
//! The old credential dies at once — member token and its derived deposit
//! token together. Every friend card minted from the old token stops
//! depositing that instant, including cards printed on paper that no gossip
//! can ever reach. A contact who has been offline for months therefore posts
//! into a 401 until either the kind-9 notice reaches them or a person re-shares
//! a card.
//!
//! That is the accepted §10 window and it is deliberately not widened. Keeping
//! the old token alive as a deposit-class credential for a grace period was
//! considered and rejected: it would buy availability for stale cards by
//! handing the revoked device a capability — depositing into its former
//! family's mailbox, on that family's quota — that §10 does not grant it. The
//! spec's answer to the stranded contact is the repair path this codebase
//! already runs: two authoritative `TokenRejected` answers mark the endpoint
//! stale ([`crate::core_contact_relay_is_stale`]), delivery falls back to our
//! own endpoint rather than looping on a dead one
//! ([`crate::resolved_contact_delivery_relay`]), the state is surfaced, and a
//! re-shared `CMRELAY1` / friend card repairs it. Propagation-bounded, never
//! permanent brickage.
//!
//! ## The people on the same Shore Pass
//!
//! One `family_token` can serve several *people*, not just one person's
//! devices — that is what a shared pass is. Those people are contacts, not own
//! devices, so neither leg above reaches them with a **member** credential:
//! the Settings stream is sealed inside one person's boundary, and a kind-9
//! notice can only ever carry a deposit token (`encode_relay_update_content`
//! attenuates unconditionally, and `decode_relay_update_content` refuses a
//! member-class one even if it somehow arrived). So a rotation performed by
//! one person on the pass locks the others out of the shared mailbox until
//! they are re-provisioned.
//!
//! That is not a gap this slice papers over, because the alternative is worse:
//! a channel that could hand a member token to a contact is exactly the
//! capability CP4 removed, and the revoked device is a contact of everybody it
//! ever met. The repair is the one the spec already names — a re-shared
//! `CMRELAY1` setup card, which is the only artefact in the system that
//! carries a member credential on purpose (`crate::make_relay_setup_card`).
//! Until it is scanned, sends toward a pass-sharer whose card credential has
//! been written off resolve to *nothing* rather than to our rotated mailbox:
//! posting there would look like delivery to a mailbox they can no longer
//! drain. Both halves are pinned by tests below.
//!
//! # Crash safety, and why the *client* picks the new token
//!
//! A rotation that the server committed and the client did not hear about
//! would be a family locked out of its own mailbox by a dropped TCP
//! connection. So the replacement credential is minted here
//! ([`core_mint_relay_member_token`]), written down durably
//! ([`MessageStore::begin_relay_rotation`]) **before** the call, and the
//! server's rotate is idempotent in it: a retry presenting the new token is
//! answered `rotated: false` with the same values. The recovery rule for a
//! device that finds a pending rotation is therefore "try the new token; if it
//! authorizes, the rotation happened".
//!
//! Nothing in this module acks, deletes or dispatches an envelope, so the DTN
//! ack-safety invariant is untouched. Nothing here puts an endpoint in a
//! roster or forwards a third party's, so the endpoint-privacy invariant is
//! untouched. The envelope public header is not involved at all.

use data_encoding::BASE64URL_NOPAD;
use rand_core::{OsRng, RngCore};
use rusqlite::{params, OptionalExtension};

use crate::device_roster::DEVICE_ID_LEN;
use crate::relay_wire::{relay_deposit_token_for, relay_token_is_deposit, RelayEndpoint};
use crate::revocation::RevocationCommit;
use crate::store::store_err;
use crate::sync_record::SyncSettingEntry;
use crate::{CoreError, MessageStore};

/// Class prefix on a member token this core minted.
///
/// Purely descriptive — relayd resolves credentials by table lookup, and the
/// only prefix that carries *meaning* is CP4's `cmdep1-`, which marks the
/// deposit class. This one exists so an operator reading a support report can
/// tell a rotated token from a hand-provisioned one at a glance, and it is
/// deliberately not something any check depends on: tokens provisioned before
/// this existed are ordinary member tokens forever.
pub const RELAY_MEMBER_TOKEN_PREFIX: &str = "cmfam1-";

/// Bytes of randomness behind a minted member token. The token is a bearer
/// credential for a whole family's mailbox and it is semi-public by design
/// (its attenuation rides QR cards), so it is sized like a key rather than
/// like an id.
const RELAY_MEMBER_TOKEN_ENTROPY_BYTES: usize = 32;

/// The reserved shared-settings key the family's relay credential rides under
/// (§8, §10.2).
///
/// A sibling that was asleep through a rotation has to learn the new member
/// token from somewhere, and §8's Settings stream is what that somewhere is:
/// small, replaceable, newest-wins state, sealed to the person's inbox key, so
/// it never crosses the person boundary and — after §10.1 — is unreadable to
/// the device the rotation is cutting off.
///
/// It is a credential rather than content, which is worth naming because
/// `sync_store`'s module doc is careful that a `.cmbak` of this database
/// cannot leak the fleet's *inbox key*. The relay member token is a different
/// thing: `CoreBackupPayload` has carried `relay_url`/`relay_token` since long
/// before multi-device, so a backup already holds it, and a restored backup
/// that could not reach the family mailbox would be a worse failure than the
/// one this avoids.
pub const SYNC_RELAY_CREDENTIAL_SETTING_KEY: &str = "relay.credential";

/// How far into the future a wall-clock relay epoch may sit before this device
/// refuses to believe it (§10.2, T23).
///
/// Every epoch in the relay story is a millisecond wall-clock stamp compared
/// with `>`, which is exactly what a hostile writer poisons: one entry at
/// `i64::MAX` — or one settings write at `u64::MAX` — and no honest device can
/// ever announce a credential again, because nothing it can stamp will be
/// strictly newer. The revoked device is assumed to hold the old inbox key and
/// the old member token, so it *can* author one of each, which makes this a
/// live denial of service against the very remedy §10 exists to run.
///
/// The bound is what turns "newest wins" back into something an honest clock
/// can win. Three days is generous against the real cause of skew — a phone
/// whose clock is wrong, or one that spent a fortnight in a drawer and applies
/// a record authored while it slept — and is orders of magnitude short of the
/// climbs a poisoner needs. A value beyond it is not a clock that is a little
/// off; it is a claim about a time that has not happened.
pub const RELAY_EPOCH_MAX_SKEW_MS: i64 = 3 * 24 * 60 * 60 * 1000;

pub(crate) const RELAY_ROTATION_SCHEMA_SQL: &str = "
-- §10 step 2's crash-safety journal. At most one row, ever: a device performs
-- one rotation at a time, and a second `begin` replaces the first rather than
-- queueing behind it (a rotation that never reached the server is worth
-- nothing, and the token it proposed is worth nothing either).
--
-- The row is written BEFORE the network call and kept after it, so a device
-- that dies mid-ceremony wakes up knowing which credential to try. See
-- `MessageStore::begin_relay_rotation`.
CREATE TABLE IF NOT EXISTS relay_rotation (
    id                   INTEGER PRIMARY KEY CHECK (id = 1),
    relay_url            TEXT NOT NULL,
    superseded_token     TEXT NOT NULL,
    new_token            TEXT NOT NULL,
    relay_epoch          INTEGER NOT NULL,
    inbox_key_generation INTEGER NOT NULL,
    revoked_device_ids   BLOB NOT NULL,
    started_at_ms        INTEGER NOT NULL,
    committed_at_ms      INTEGER
);
";

/// Mint a fresh member-class relay credential (§10.2).
///
/// Minted locally rather than issued by the server on purpose: the client has
/// to be able to name the credential it is asking for, or a lost response
/// leaves it holding a token that no longer authorizes anything and no way to
/// discover the one that does. See this module's crash-safety note.
#[uniffi::export]
pub fn core_mint_relay_member_token() -> String {
    let mut bytes = [0u8; RELAY_MEMBER_TOKEN_ENTROPY_BYTES];
    OsRng.fill_bytes(&mut bytes);
    format!(
        "{RELAY_MEMBER_TOKEN_PREFIX}{}",
        BASE64URL_NOPAD.encode(&bytes)
    )
}

/// One planned relay rotation: the credential to move to, and the two epochs
/// that decide whether it is safe to announce.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct RelayRotationPlan {
    pub relay_url: String,
    /// The credential being retired. Kept so a device recovering a
    /// half-finished rotation knows which of the two tokens it was holding.
    pub superseded_token: String,
    /// The credential to move to. Already minted, and durable before the call
    /// (see [`MessageStore::begin_relay_rotation`]).
    pub new_token: String,
    /// [`crate::relay_deposit_token_for`] of `new_token`: what kind-9 notices
    /// and friend cards will carry. Derived here so both legs agree.
    pub new_deposit_token: String,
    /// The T23 epoch the contact leg announces at — strictly above whatever
    /// this device announced last, so a replayed older notice cannot pull a
    /// contact back to the retired credential.
    pub relay_epoch: i64,
    /// The inbox key generation §10.1 rotated to. The sibling leg refuses to
    /// publish until this device is actually there, because the Settings
    /// stream is sealed to it.
    pub inbox_key_generation: u64,
    /// What made this rotation necessary — §10.1's buried devices, carried
    /// through so the shell can say why the relay credential changed.
    pub revoked_device_ids: Vec<Vec<u8>>,
}

/// What a committed rotation left behind, and what still has to go out.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct RelayRotationCommit {
    /// This device's relay configuration from here on: the shell writes it to
    /// platform storage, exactly as it would after scanning a setup card.
    pub endpoint: RelayEndpoint,
    /// The deposit attenuation kind-9 will carry.
    pub deposit_token: String,
    /// The epoch to stamp those notices with.
    pub relay_epoch: i64,
    /// The credential that just died. Useful for a shell that wants to stop
    /// retrying it, and for the operator reading a support report.
    pub superseded_token: String,
    /// Every contact that must be told (§10.2's contact leg). The notices
    /// themselves ride the shipped `CAP_RELAY_UPDATE` path.
    pub contact_user_ids: Vec<Vec<u8>>,
    /// The epoch the sibling leg actually published at. Usually
    /// [`Self::relay_epoch`], and strictly above it when a stored entry had
    /// already claimed that stamp — see
    /// [`MessageStore::commit_relay_rotation`].
    pub published_epoch: u64,
}

/// **Plan §10.2, from §10.1's result.**
///
/// Takes the [`RevocationCommit`] rather than a bare device list because the
/// trigger is structural: a relay rotation is something a revocation causes,
/// and a caller that has not committed a revocation has nothing to rotate
/// *for*. It also carries the two facts the legs need — which devices were
/// buried, and which inbox generation the announcement must be sealed under.
///
/// `None` means there is nothing to rotate: a person with no Shore Pass has no
/// family token, and their revocation is complete without this step.
///
/// A deposit-class credential is an error rather than a `None`. A device whose
/// own relay config holds a deposit token cannot fetch its own mail either, so
/// it is misconfigured; rotating is not the repair and silently skipping would
/// hide it.
#[uniffi::export]
pub fn core_plan_relay_rotation(
    revocation: RevocationCommit,
    relay_url: String,
    current_token: String,
    previous_relay_epoch: i64,
    now_ms: i64,
) -> Result<Option<RelayRotationPlan>, CoreError> {
    let relay_url = relay_url.trim().to_string();
    let current_token = current_token.trim().to_string();
    if relay_url.is_empty() || current_token.is_empty() {
        return Ok(None);
    }
    if relay_token_is_deposit(current_token.clone()) {
        return Err(CoreError::Malformed(
            "this device holds a deposit-class relay credential, which cannot be rotated"
                .to_string(),
        ));
    }
    let new_token = core_mint_relay_member_token();
    Ok(Some(RelayRotationPlan {
        relay_url,
        superseded_token: current_token,
        new_deposit_token: relay_deposit_token_for(new_token.clone()),
        new_token,
        // T23's monotonicity, matching what the shells already do on an
        // endpoint change: wall clock, but never equal to or below the last
        // epoch we announced, so two rotations inside one millisecond still
        // order.
        relay_epoch: now_ms.max(previous_relay_epoch.saturating_add(1)),
        inbox_key_generation: revocation.inbox_key_generation,
        revoked_device_ids: revocation.revoked_device_ids,
    }))
}

#[uniffi::export]
impl MessageStore {
    /// **Write the rotation down before performing it** (§10.2, crash safety).
    ///
    /// Must be called before `POST /family/rotate`, and the ordering is the
    /// whole point: the only way a device can recover from a lost response is
    /// to already know which credential to try. A second `begin` replaces a
    /// pending first — a proposal that never reached the server is worth
    /// nothing, and neither is the token it named.
    pub fn begin_relay_rotation(
        &self,
        plan: RelayRotationPlan,
        now_ms: i64,
    ) -> Result<(), CoreError> {
        if plan.new_token.trim().is_empty() || relay_token_is_deposit(plan.new_token.clone()) {
            return Err(CoreError::Malformed(
                "a rotation must name a member-class replacement credential".to_string(),
            ));
        }
        if plan.new_token == plan.superseded_token {
            return Err(CoreError::Malformed(
                "a rotation must move to a different credential".to_string(),
            ));
        }
        let conn = self.locked_conn();
        conn.execute(
            "INSERT INTO relay_rotation
                (id, relay_url, superseded_token, new_token, relay_epoch,
                 inbox_key_generation, revoked_device_ids, started_at_ms, committed_at_ms)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)
             ON CONFLICT(id) DO UPDATE SET
                relay_url = excluded.relay_url,
                superseded_token = excluded.superseded_token,
                new_token = excluded.new_token,
                relay_epoch = excluded.relay_epoch,
                inbox_key_generation = excluded.inbox_key_generation,
                revoked_device_ids = excluded.revoked_device_ids,
                started_at_ms = excluded.started_at_ms,
                committed_at_ms = NULL",
            params![
                plan.relay_url,
                plan.superseded_token,
                plan.new_token,
                plan.relay_epoch,
                plan.inbox_key_generation as i64,
                pack_device_ids(&plan.revoked_device_ids),
                now_ms,
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// The rotation this device started and has not committed, if any.
    ///
    /// A shell finding one on launch should try the plan's `new_token` against
    /// the relay: if it authorizes, the server rotated and only the answer was
    /// lost, so [`Self::commit_relay_rotation`] finishes the job. If the *old*
    /// token still authorizes, the call never landed and the rotation can be
    /// retried unchanged.
    pub fn pending_relay_rotation(&self) -> Result<Option<RelayRotationPlan>, CoreError> {
        let conn = self.locked_conn();
        conn.query_row(
            "SELECT relay_url, superseded_token, new_token, relay_epoch,
                    inbox_key_generation, revoked_device_ids
               FROM relay_rotation
              WHERE id = 1 AND committed_at_ms IS NULL",
            [],
            |row| {
                let new_token: String = row.get(2)?;
                Ok(RelayRotationPlan {
                    relay_url: row.get(0)?,
                    superseded_token: row.get(1)?,
                    new_deposit_token: relay_deposit_token_for(new_token.clone()),
                    new_token,
                    relay_epoch: row.get(3)?,
                    inbox_key_generation: row.get::<_, i64>(4)? as u64,
                    revoked_device_ids: unpack_device_ids(&row.get::<_, Vec<u8>>(5)?),
                })
            },
        )
        .optional()
        .map_err(store_err)
    }

    /// Give up on a rotation that cannot be completed, leaving this device on
    /// the credential it already has.
    ///
    /// Safe precisely because the server side is idempotent and all-or-nothing:
    /// either the rotation landed — in which case the old token stops
    /// authorizing and the shell learns that from the next 401, not from this
    /// row — or it did not, and the old token is still the family's. Returns
    /// whether anything was pending.
    pub fn abandon_relay_rotation(&self) -> Result<bool, CoreError> {
        let conn = self.locked_conn();
        let cleared = conn
            .execute(
                "DELETE FROM relay_rotation WHERE id = 1 AND committed_at_ms IS NULL",
                [],
            )
            .map_err(store_err)?;
        Ok(cleared > 0)
    }

    /// **Commit §10.2 once the relay has confirmed the rotation.**
    ///
    /// Order, and why each step is where it is:
    ///
    /// 1. **Refuse to publish under a superseded inbox generation.** §10.1
    ///    rotates the inbox key; the Settings stream is sealed to it. Running
    ///    this before that landed would seal the family's new fetch credential
    ///    under the generation the revoked device still holds — handing the
    ///    thief its replacement in the same breath as taking the old one away.
    ///    The check is against this device's own sync context, which
    ///    `commit_own_revocation` moves as its third step.
    /// 2. **Publish to siblings**, into [`SYNC_RELAY_CREDENTIAL_SETTING_KEY`]
    ///    at the rotation's epoch. WP4's carrier takes it from there over any
    ///    of the four transports — which matters, because a sibling that
    ///    missed the rotation cannot reach the relay to be told over the relay.
    /// 3. **Mark the journal committed**, so a relaunch does not re-run a
    ///    rotation that has already happened.
    ///
    /// The contact leg is returned rather than performed: the notices are
    /// ordinary kind-9 traffic on the shipped `CAP_RELAY_UPDATE` path, and
    /// this returns the epoch to stamp them with and the list to send them to.
    ///
    /// # A rotation always wins its own publish
    ///
    /// The sibling leg used to write at the plan's epoch and report a lost
    /// merge as an ordinary outcome. That was wrong in exactly the case this
    /// module exists for. The Settings merge is "highest `(epoch,
    /// author_device_id, value)` wins", the revoked device held the inbox key
    /// up to the moment §10.1 rotated it, and it could therefore have authored
    /// a `relay.credential` entry at any epoch it liked before being cut off.
    /// A rotation that shrugged at losing to that entry would leave the whole
    /// fleet converged on a credential the thief chose, announced under the
    /// person's own name, with nothing in the system ever able to displace it.
    ///
    /// So this publishes at `max(plan.relay_epoch, stored_epoch + 1)` — a
    /// stamp that is by construction strictly above whatever is there — and a
    /// write that still does not take is an **error**, not a result. There is
    /// no useful state to return from a rotation whose replacement credential
    /// no sibling will ever learn; the caller's remedy is to retry, and the
    /// journal row [`Self::begin_relay_rotation`] wrote is what lets it.
    ///
    /// The epoch that entry can climb to is bounded on the way *in*, by
    /// [`RELAY_EPOCH_MAX_SKEW_MS`] in the settings apply, which is what keeps
    /// `stored_epoch + 1` a number an honest clock can still stamp.
    ///
    /// Two of the person's own devices revoking at once are the case this
    /// deliberately no longer treats as a loss: both rotate, both publish, the
    /// later wall clock wins, and the loser's own relay call has already been
    /// superseded on the server too — its token no longer authorizes, so it
    /// discovers the winner's credential by the same route a months-asleep
    /// sibling does.
    pub fn commit_relay_rotation(
        &self,
        plan: RelayRotationPlan,
        now_ms: i64,
    ) -> Result<RelayRotationCommit, CoreError> {
        let generation = self
            .core_own_sync_context()?
            .map(|context| context.inbox_key_generation)
            .unwrap_or_default();
        if generation < plan.inbox_key_generation {
            return Err(CoreError::Store(format!(
                "this device is still at inbox key generation {generation}, so it cannot \
                 announce a relay credential for generation {}",
                plan.inbox_key_generation
            )));
        }
        let stored_epoch = self
            .core_sync_get_setting(SYNC_RELAY_CREDENTIAL_SETTING_KEY.to_string())?
            .map(|entry| entry.epoch)
            .unwrap_or(0);
        // The relay epoch is already a monotonic wall-clock stamp for exactly
        // this credential, so reusing it keeps the sibling leg and the contact
        // leg ordered by the same number rather than by two clocks that can
        // disagree — but never below what is already stored, or the rotation
        // announces into a slot it cannot win.
        let published_epoch = (plan.relay_epoch.max(0) as u64).max(stored_epoch.saturating_add(1));
        let published_to_siblings = self.core_sync_put_setting(SyncSettingEntry {
            key: SYNC_RELAY_CREDENTIAL_SETTING_KEY.to_string(),
            value: encode_relay_credential(&plan.relay_url, &plan.new_token),
            epoch: published_epoch,
            author_device_id: Vec::new(),
        })?;
        if !published_to_siblings {
            return Err(CoreError::Store(format!(
                "the rotated relay credential could not be published to this person's other \
                 devices: a stored entry at epoch {stored_epoch} still wins the settings merge, \
                 so no sibling would ever learn the replacement"
            )));
        }
        let contact_user_ids = self
            .list_contacts()?
            .into_iter()
            .map(|contact| contact.user_id)
            .collect();
        {
            let conn = self.locked_conn();
            conn.execute(
                "UPDATE relay_rotation SET committed_at_ms = ?1
                  WHERE id = 1 AND new_token = ?2",
                params![now_ms, plan.new_token],
            )
            .map_err(store_err)?;
        }
        Ok(RelayRotationCommit {
            endpoint: RelayEndpoint {
                url: plan.relay_url,
                token: plan.new_token,
            },
            deposit_token: plan.new_deposit_token,
            relay_epoch: plan.relay_epoch,
            superseded_token: plan.superseded_token,
            contact_user_ids,
            published_epoch,
        })
    }

    /// The family relay credential a sibling published (§10.2's own-device
    /// leg, receiving side).
    ///
    /// A device that adopted a rotation announcement reads its new
    /// configuration here and writes it to platform storage. `None` means no
    /// sibling has ever published one, which is every fleet that has not
    /// rotated — the shell keeps whatever it was configured with.
    pub fn relay_credential_setting(&self) -> Result<Option<RelayEndpoint>, CoreError> {
        let Some(entry) =
            self.core_sync_get_setting(SYNC_RELAY_CREDENTIAL_SETTING_KEY.to_string())?
        else {
            return Ok(None);
        };
        Ok(decode_relay_credential(&entry.value))
    }
}

// ---------------------------------------------------------------------------
// Codecs
// ---------------------------------------------------------------------------

/// `len(u16) ‖ url ‖ len(u16) ‖ token`.
///
/// Length-prefixed rather than delimited because a relay URL is
/// caller-supplied text and a token may hold any non-whitespace byte; the
/// settings merge compares values byte-for-byte, so an encoding where two
/// different pairs could collide would let two devices sit forked forever.
fn encode_relay_credential(url: &str, token: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + url.len() + token.len());
    for field in [url, token] {
        out.extend_from_slice(&(field.len().min(u16::MAX as usize) as u16).to_be_bytes());
        out.extend_from_slice(&field.as_bytes()[..field.len().min(u16::MAX as usize)]);
    }
    out
}

/// The inverse, tolerant in the same way `decode_block_list` is: a value this
/// build cannot read yields `None` rather than an error, so one unreadable
/// setting cannot strand every other setting in the same record.
pub(crate) fn decode_relay_credential(bytes: &[u8]) -> Option<RelayEndpoint> {
    let mut offset = 0usize;
    let mut fields = Vec::with_capacity(2);
    for _ in 0..2 {
        if offset + 2 > bytes.len() {
            return None;
        }
        let len = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        offset += 2;
        if offset + len > bytes.len() {
            return None;
        }
        fields.push(String::from_utf8(bytes[offset..offset + len].to_vec()).ok()?);
        offset += len;
    }
    let token = fields.pop()?;
    let url = fields.pop()?;
    if url.is_empty() || token.is_empty() {
        return None;
    }
    Some(RelayEndpoint { url, token })
}

/// Device ids are fixed-width (DL-1), so the journal stores them concatenated
/// rather than framed.
pub(crate) fn pack_device_ids(ids: &[Vec<u8>]) -> Vec<u8> {
    ids.iter()
        .filter(|id| id.len() == DEVICE_ID_LEN)
        .flat_map(|id| id.iter().copied())
        .collect()
}

pub(crate) fn unpack_device_ids(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .chunks_exact(DEVICE_ID_LEN)
        .map(<[u8]>::to_vec)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contact_relay_health::core_contact_relay_endpoint_usable;
    use crate::device_link::activation::{
        core_link_genesis_roster, core_link_sign_new_device_roster,
    };
    use crate::device_roster::{generate_device_keypair, DeviceKeypair, Roster};
    use crate::identity::generate_identity;
    use crate::relay_wire::{
        relay_decode_rotate_response, relay_encode_rotate_request, relay_rotate_path,
        resolved_contact_delivery_relay,
    };
    use crate::revocation::core_revoke_devices_roster;
    use crate::sync_record::{core_mint_inbox_key, InboxKey};
    use crate::{Contact, Identity};

    const NOW: i64 = 1_755_000_000_000;
    const RELAY_URL: &str = "https://relay.example.com";
    const OLD_TOKEN: &str = "family-token-before-the-revocation";

    struct Fleet {
        person: Identity,
        approver: DeviceKeypair,
        sibling: DeviceKeypair,
        roster: Roster,
        inbox_key: InboxKey,
    }

    fn fleet() -> Fleet {
        let person = generate_identity();
        let approver = generate_device_keypair();
        let sibling = generate_device_keypair();
        let genesis = core_link_genesis_roster(
            person.sign_sk.clone(),
            approver.sign_pk.clone(),
            approver.agree_pk.clone(),
        )
        .expect("genesis");
        let roster = core_link_sign_new_device_roster(
            genesis,
            person.sign_pk.clone(),
            approver.sign_sk.clone(),
            sibling.sign_pk.clone(),
            sibling.agree_pk.clone(),
        )
        .expect("link")
        .roster;
        let inbox_key = core_mint_inbox_key(roster.inbox_key_generation);
        Fleet {
            person,
            approver,
            sibling,
            roster,
            inbox_key,
        }
    }

    fn a_contact(name: &str) -> Contact {
        let other = generate_identity();
        Contact {
            user_id: other.user_id,
            name: name.to_string(),
            sign_pk: other.sign_pk,
            agree_pk: other.agree_pk,
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    /// A store that has committed §10.1's revocation, plus the plan §10.2
    /// makes from it.
    fn revoked(fleet: &Fleet) -> (MessageStore, RelayRotationPlan) {
        let store = MessageStore::open(":memory:".to_string()).expect("open");
        store
            .adopt_own_roster(
                fleet.roster.clone(),
                fleet.person.sign_pk.clone(),
                fleet.approver.device_id.clone(),
            )
            .expect("adopt");
        store
            .core_set_own_sync_context(fleet.roster.clone(), fleet.roster.inbox_key_generation)
            .expect("sync context");
        let update = core_revoke_devices_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            vec![fleet.sibling.device_id.clone()],
            fleet.inbox_key.clone(),
        )
        .expect("revocation");
        // §10.1's two-call ceremony, with the step a shell does between them --
        // putting the rotated key in platform storage -- standing in as an
        // assertion. See `MessageStore::commit_own_revocation`.
        let key = store
            .begin_own_revocation(
                update.clone(),
                fleet.person.sign_pk.clone(),
                fleet.approver.clone(),
                NOW,
            )
            .expect("begin");
        assert_eq!(key, update.inbox_key);
        let commit = store
            .commit_own_revocation(
                update,
                fleet.person.sign_pk.clone(),
                fleet.approver.clone(),
                Some(fleet.inbox_key.clone()),
                NOW,
            )
            .expect("commit");
        let plan =
            core_plan_relay_rotation(commit, RELAY_URL.to_string(), OLD_TOKEN.to_string(), 0, NOW)
                .expect("plan")
                .expect("a family with a pass rotates");
        (store, plan)
    }

    #[test]
    fn a_minted_token_is_member_class_high_entropy_and_never_repeats() {
        let token = core_mint_relay_member_token();
        assert!(token.starts_with(RELAY_MEMBER_TOKEN_PREFIX));
        assert!(!relay_token_is_deposit(token.clone()));
        // Above relayd's `MIN_ROTATION_TOKEN_LEN` floor with room to spare,
        // and below the friend-card cap.
        assert!(token.len() >= 24 && token.len() <= 1024);
        assert_ne!(token, core_mint_relay_member_token());
    }

    #[test]
    fn a_revocation_plans_a_rotation_the_revoked_device_cannot_predict() {
        let fleet = fleet();
        let (_store, plan) = revoked(&fleet);

        assert_eq!(plan.superseded_token, OLD_TOKEN);
        assert_ne!(plan.new_token, OLD_TOKEN);
        // The revoked device holds the old token and every byte it ever saw.
        // The replacement must not be derivable from any of it.
        assert!(!plan.new_token.contains(OLD_TOKEN));
        assert_eq!(
            plan.new_deposit_token,
            relay_deposit_token_for(plan.new_token.clone())
        );
        assert_eq!(plan.revoked_device_ids, vec![fleet.sibling.device_id]);
        // §10.1's generation, which the sibling leg is gated on.
        assert_eq!(
            plan.inbox_key_generation,
            fleet.roster.inbox_key_generation + 1
        );
        assert_eq!(plan.relay_epoch, NOW);
    }

    #[test]
    fn a_person_with_no_pass_has_nothing_to_rotate() {
        let fleet = fleet();
        let commit = revocation_commit_stub(&fleet);
        assert!(
            core_plan_relay_rotation(commit.clone(), String::new(), String::new(), 0, NOW)
                .expect("plan")
                .is_none()
        );
        // Whitespace is not a credential either.
        assert!(core_plan_relay_rotation(
            commit.clone(),
            "  ".to_string(),
            "  ".to_string(),
            0,
            NOW
        )
        .expect("plan")
        .is_none());
        // A deposit credential is a misconfiguration, not a no-op: a device
        // holding one cannot fetch its own mail either.
        assert!(core_plan_relay_rotation(
            commit,
            RELAY_URL.to_string(),
            relay_deposit_token_for(OLD_TOKEN.to_string()),
            0,
            NOW,
        )
        .is_err());
    }

    #[test]
    fn the_epoch_always_climbs_so_a_replayed_notice_cannot_restore_the_dead_token() {
        let fleet = fleet();
        let (store, first) = revoked(&fleet);
        let commit = store
            .commit_relay_rotation(first.clone(), NOW)
            .expect("commit");
        let later = core_plan_relay_rotation(
            revocation_commit_stub_at(&fleet, first.inbox_key_generation),
            RELAY_URL.to_string(),
            commit.endpoint.token.clone(),
            commit.relay_epoch,
            // Same millisecond: the clock did not move, so the epoch has to.
            // Otherwise a second rotation inside one millisecond would
            // announce at an epoch `apply_contact_relay_update` refuses as not
            // strictly newer, and every contact would keep the dead token.
            NOW,
        )
        .expect("plan")
        .expect("plan");
        assert!(later.relay_epoch > commit.relay_epoch);
        assert_ne!(later.new_token, commit.endpoint.token);
    }

    /// A `RevocationCommit` shaped for a plan-only test. Every field the plan
    /// reads is set; the rest exist only to satisfy the record.
    fn revocation_commit_stub(fleet: &Fleet) -> RevocationCommit {
        revocation_commit_stub_at(fleet, fleet.roster.inbox_key_generation)
    }

    fn revocation_commit_stub_at(fleet: &Fleet, inbox_key_generation: u64) -> RevocationCommit {
        RevocationCommit {
            roster: fleet.roster.clone(),
            roster_head: Vec::new(),
            inbox_key_generation,
            revoked_device_ids: vec![fleet.sibling.device_id.clone()],
            stream_seq: 1,
            handoffs: Vec::new(),
            contact_user_ids: Vec::new(),
            roster_document: Vec::new(),
            resealed_records: 0,
            unresealable_records: 0,
        }
    }

    #[test]
    fn the_journal_survives_a_crash_between_minting_and_confirming() {
        let fleet = fleet();
        let (store, plan) = revoked(&fleet);
        assert!(store.pending_relay_rotation().expect("pending").is_none());

        store
            .begin_relay_rotation(plan.clone(), NOW)
            .expect("begin");
        // This is the state a device wakes up in when the response was lost:
        // it knows both credentials, so it can ask the server which one won.
        let recovered = store
            .pending_relay_rotation()
            .expect("pending")
            .expect("a rotation is in flight");
        assert_eq!(recovered, plan);

        store
            .commit_relay_rotation(plan.clone(), NOW)
            .expect("commit");
        assert!(
            store.pending_relay_rotation().expect("pending").is_none(),
            "a committed rotation is not re-run on the next launch"
        );

        // Abandoning after the fact changes nothing, and abandoning a fresh
        // one leaves the device on the credential it already had.
        assert!(!store.abandon_relay_rotation().expect("abandon"));
        store.begin_relay_rotation(plan, NOW).expect("begin");
        assert!(store.abandon_relay_rotation().expect("abandon"));
        assert!(store.pending_relay_rotation().expect("pending").is_none());
    }

    #[test]
    fn a_rotation_must_move_to_a_different_member_credential() {
        let fleet = fleet();
        let (store, plan) = revoked(&fleet);
        assert!(store
            .begin_relay_rotation(
                RelayRotationPlan {
                    new_token: plan.superseded_token.clone(),
                    ..plan.clone()
                },
                NOW
            )
            .is_err());
        assert!(store
            .begin_relay_rotation(
                RelayRotationPlan {
                    new_token: relay_deposit_token_for(plan.new_token.clone()),
                    ..plan
                },
                NOW
            )
            .is_err());
    }

    #[test]
    fn committing_hands_the_shell_both_legs_and_the_new_configuration() {
        let fleet = fleet();
        let (store, plan) = revoked(&fleet);
        let first = a_contact("Ada");
        let second = a_contact("Bo");
        store.upsert_contact(first.clone()).expect("contact");
        store.upsert_contact(second.clone()).expect("contact");
        store
            .begin_relay_rotation(plan.clone(), NOW)
            .expect("begin");

        let commit = store
            .commit_relay_rotation(plan.clone(), NOW)
            .expect("commit");
        assert_eq!(commit.endpoint.url, RELAY_URL);
        assert_eq!(commit.endpoint.token, plan.new_token);
        assert_eq!(commit.superseded_token, OLD_TOKEN);
        assert_eq!(commit.relay_epoch, plan.relay_epoch);
        assert_eq!(commit.published_epoch, plan.relay_epoch as u64);
        // The contact leg is everybody, because a contact that misses the
        // notice posts into a dead credential until somebody repairs the card.
        let mut told = commit.contact_user_ids.clone();
        told.sort();
        let mut expected = vec![first.user_id, second.user_id];
        expected.sort();
        assert_eq!(told, expected);
        // What kind-9 will actually carry is the attenuation, never the member
        // token -- `encode_relay_update_content` enforces that independently,
        // and this is the value it will produce.
        assert_eq!(
            commit.deposit_token,
            relay_deposit_token_for(plan.new_token.clone())
        );
        assert!(relay_token_is_deposit(commit.deposit_token));
    }

    #[test]
    fn a_sibling_reads_the_new_credential_out_of_the_settings_stream() {
        let fleet = fleet();
        let (store, plan) = revoked(&fleet);
        assert!(store.relay_credential_setting().expect("setting").is_none());

        store
            .commit_relay_rotation(plan.clone(), NOW)
            .expect("commit");
        assert_eq!(
            store.relay_credential_setting().expect("setting"),
            Some(RelayEndpoint {
                url: RELAY_URL.to_string(),
                token: plan.new_token.clone(),
            })
        );

        // The setting rides §8's Settings stream, so this is what a sibling
        // that was asleep through the ceremony receives and writes to its own
        // relay configuration.
        let page = store.core_sync_settings_page(16).expect("page");
        let entry = page
            .entries
            .iter()
            .find(|entry| entry.key == SYNC_RELAY_CREDENTIAL_SETTING_KEY)
            .expect("the credential is in the shared settings");
        assert_eq!(entry.epoch, plan.relay_epoch as u64);
        assert_eq!(
            decode_relay_credential(&entry.value),
            Some(RelayEndpoint {
                url: RELAY_URL.to_string(),
                token: plan.new_token,
            })
        );
    }

    /// §10.2's own-device leg, end to end over a real WP4 sync record rather
    /// than by reading the settings table on the same store: the revoking
    /// device authors a Settings record, seals it to the ROTATED inbox key,
    /// and the sibling opens it, applies it, and is told the credential
    /// changed.
    ///
    /// The last part matters as much as the delivery. A sibling that had to
    /// poll for a rotation would keep hammering a dead token in the meantime,
    /// so the fact is reported out of the apply, exactly as an own-roster
    /// record's inbox keys are.
    #[test]
    fn a_sibling_receives_the_credential_through_a_real_sealed_sync_record() {
        let fleet = fleet();
        // Three devices, so one survives the revocation to be told.
        let survivor = generate_device_keypair();
        let roster = core_link_sign_new_device_roster(
            fleet.roster.clone(),
            fleet.person.sign_pk.clone(),
            fleet.approver.sign_sk.clone(),
            survivor.sign_pk.clone(),
            survivor.agree_pk.clone(),
        )
        .expect("link")
        .roster;
        let fleet = Fleet { roster, ..fleet };

        let (store, plan) = revoked(&fleet);
        let rotated_key = core_mint_inbox_key(plan.inbox_key_generation);
        let post_revocation = store.own_roster().expect("roster").expect("adopted");
        store
            .commit_relay_rotation(plan.clone(), NOW)
            .expect("commit");

        // The Settings record WP4 would author on the next round.
        let stream_seq = store
            .core_sync_next_stream_seq(
                fleet.approver.device_id.clone(),
                crate::SyncRecordKind::Settings,
            )
            .expect("seq");
        let record = crate::core_sign_sync_record(
            crate::SyncRecord {
                kind: crate::SyncRecordKind::Settings,
                person_id: fleet.person.user_id.clone(),
                author_device_id: Vec::new(),
                roster_version: post_revocation.version(),
                inbox_key_generation: plan.inbox_key_generation,
                stream_seq,
                timestamp_ms: NOW,
                payload: crate::core_encode_sync_settings(
                    store.core_sync_settings_page(16).expect("page"),
                )
                .expect("payload"),
                signature: Vec::new(),
            },
            fleet.approver.sign_sk.clone(),
        )
        .expect("signs");
        let sealed = crate::core_seal_sync_record(
            record,
            crate::core_device_sync_identity(fleet.approver.clone()),
            rotated_key.clone(),
        )
        .expect("seals");

        // The revoked device kept the OLD inbox key, and the record carrying
        // the replacement credential is exactly as unreadable to it as the
        // rest of the fleet's mail.
        assert!(crate::core_open_sync_record(
            sealed.sealed.clone(),
            fleet.inbox_key.clone(),
            post_revocation.clone(),
        )
        .is_err());

        // The surviving sibling opens it and is told, in the apply result,
        // that its relay configuration has to change.
        let sibling_store = MessageStore::open(":memory:".to_string()).expect("open");
        sibling_store
            .adopt_own_roster(
                post_revocation.clone(),
                fleet.person.sign_pk.clone(),
                survivor.device_id.clone(),
            )
            .expect("adopt");
        sibling_store
            .core_set_own_sync_context(post_revocation.clone(), plan.inbox_key_generation)
            .expect("sync context");
        let opened = crate::core_open_sync_record(sealed.sealed, rotated_key, post_revocation)
            .expect("opens");
        let applied = sibling_store
            .core_apply_sync_record(opened, NOW)
            .expect("applies");
        assert_eq!(
            applied.relay_credential,
            Some(RelayEndpoint {
                url: RELAY_URL.to_string(),
                token: plan.new_token.clone(),
            })
        );
        // And it is readable afterwards, for a shell that restarted between
        // the sync round and writing it down.
        assert_eq!(
            sibling_store.relay_credential_setting().expect("setting"),
            Some(RelayEndpoint {
                url: RELAY_URL.to_string(),
                token: plan.new_token,
            })
        );
    }

    #[test]
    fn the_new_credential_is_never_announced_under_the_superseded_inbox_generation() {
        // The whole reason the sibling leg is gated: the Settings stream is
        // sealed to the inbox key, so publishing before §10.1's rotation
        // landed would seal the family's replacement fetch credential under
        // the generation the revoked device still holds.
        let fleet = fleet();
        let store = MessageStore::open(":memory:".to_string()).expect("open");
        store
            .adopt_own_roster(
                fleet.roster.clone(),
                fleet.person.sign_pk.clone(),
                fleet.approver.device_id.clone(),
            )
            .expect("adopt");
        store
            .core_set_own_sync_context(fleet.roster.clone(), fleet.roster.inbox_key_generation)
            .expect("sync context");

        let plan = RelayRotationPlan {
            relay_url: RELAY_URL.to_string(),
            superseded_token: OLD_TOKEN.to_string(),
            new_token: core_mint_relay_member_token(),
            new_deposit_token: String::new(),
            relay_epoch: NOW,
            // One generation ahead of where this device actually is.
            inbox_key_generation: fleet.roster.inbox_key_generation + 1,
            revoked_device_ids: vec![fleet.sibling.device_id.clone()],
        };
        assert!(store.commit_relay_rotation(plan, NOW).is_err());
        assert!(
            store.relay_credential_setting().expect("setting").is_none(),
            "nothing was published"
        );
    }

    /// The credential-publish race, in the shape the threat model gives it:
    /// the revoked device held the inbox key up to the moment §10.1 rotated
    /// it, so it could author a `relay.credential` entry at any epoch it
    /// liked. Two independent things have to hold, and this pins both.
    #[test]
    fn a_thiefs_pre_published_credential_cannot_outlive_the_rotation() {
        let fleet = fleet();
        let (store, plan) = revoked(&fleet);
        let held = store.own_roster().expect("roster");
        let thief_device = generate_device_keypair();

        // (1) On the way in: an entry at an epoch no clock could produce is
        // refused rather than stored, so it never becomes the thing an honest
        // rotation has to climb over.
        let poisoned = SyncSettingEntry {
            key: SYNC_RELAY_CREDENTIAL_SETTING_KEY.to_string(),
            value: encode_relay_credential("https://relay.thief.example", "cmfam1-thief"),
            epoch: u64::MAX,
            author_device_id: thief_device.device_id.clone(),
        };
        assert!(!crate::sync_store::relay_credential_entry_is_admissible(
            &poisoned,
            held.as_ref(),
            NOW,
        ));
        // And so is one authored by a device this roster has buried, at any
        // epoch at all. `core_sync_record_admit` refuses a record AUTHORED by
        // a tombstoned device, but a settings page carries one author per
        // entry, so the rule has to hold one level in as well.
        assert!(!crate::sync_store::relay_credential_entry_is_admissible(
            &SyncSettingEntry {
                epoch: NOW as u64,
                author_device_id: fleet.sibling.device_id.clone(),
                ..poisoned.clone()
            },
            held.as_ref(),
            NOW,
        ));
        // A believable entry from a device still in the fleet is admitted:
        // the clamp refuses poison, not participation.
        assert!(crate::sync_store::relay_credential_entry_is_admissible(
            &SyncSettingEntry {
                epoch: NOW as u64,
                author_device_id: fleet.approver.device_id.clone(),
                ..poisoned
            },
            held.as_ref(),
            NOW,
        ));

        // (2) On the way out: whatever is stored, the rotation's own publish
        // supersedes it. The thief's entry is planted directly here, as though
        // it had landed before the clamp existed, and the rotation still wins.
        store
            .core_sync_put_setting(SyncSettingEntry {
                key: SYNC_RELAY_CREDENTIAL_SETTING_KEY.to_string(),
                value: encode_relay_credential("https://relay.thief.example", "cmfam1-thief"),
                epoch: plan.relay_epoch as u64 + 5_000,
                author_device_id: Vec::new(),
            })
            .expect("planted");
        let commit = store
            .commit_relay_rotation(plan.clone(), NOW)
            .expect("commit");
        assert!(commit.published_epoch > plan.relay_epoch as u64 + 5_000);
        assert_eq!(
            store.relay_credential_setting().expect("setting"),
            Some(RelayEndpoint {
                url: RELAY_URL.to_string(),
                token: plan.new_token,
            }),
            "the rotation, not the planted entry, is what a sibling reads"
        );
    }

    /// The other half of "a rotation always wins its own publish": a publish
    /// that somehow still loses is an error, because there is no useful state
    /// to return from a rotation whose replacement credential no sibling will
    /// ever learn.
    #[test]
    fn a_publish_that_cannot_win_is_a_failed_rotation_not_a_quiet_outcome() {
        let fleet = fleet();
        let (store, plan) = revoked(&fleet);
        // `u64::MAX` is the one stored epoch `stored + 1` cannot climb over.
        // It can no longer arrive over the wire (the clamp above), but a
        // database written before the clamp existed could still hold it, and
        // the honest answer is a refusal the caller can retry rather than a
        // commit that announced nothing.
        store
            .core_sync_put_setting(SyncSettingEntry {
                key: SYNC_RELAY_CREDENTIAL_SETTING_KEY.to_string(),
                value: encode_relay_credential("https://relay.thief.example", "cmfam1-thief"),
                epoch: u64::MAX,
                author_device_id: Vec::new(),
            })
            .expect("planted");
        assert!(store.commit_relay_rotation(plan, NOW).is_err());
    }

    #[test]
    fn the_credential_codec_round_trips_and_refuses_what_it_cannot_read() {
        let round_tripped =
            decode_relay_credential(&encode_relay_credential(RELAY_URL, "cmfam1-abc"));
        assert_eq!(
            round_tripped,
            Some(RelayEndpoint {
                url: RELAY_URL.to_string(),
                token: "cmfam1-abc".to_string(),
            })
        );
        assert_eq!(decode_relay_credential(&[]), None);
        assert_eq!(decode_relay_credential(&[0, 9, 1, 2]), None);
        // A half-written pair is not a credential.
        assert_eq!(
            decode_relay_credential(&encode_relay_credential(RELAY_URL, "")),
            None
        );
    }

    /// §10.2's accepted window, driven from the side that actually pays it:
    /// the months-offline contact's own phone.
    ///
    /// They scanned our friend card long before the revocation, so what they
    /// hold for us is the deposit attenuation of a member token that no longer
    /// exists. They are on their own relay, so nothing else about their setup
    /// is affected — the only thing broken is the one credential our rotation
    /// retired, which is exactly the shape §10 says must be bounded.
    #[test]
    fn a_months_offline_contact_is_stranded_bounded_and_repaired_by_the_shipped_path() {
        let fleet = fleet();
        let (revoking_store, plan) = revoked(&fleet);
        let commit = revoking_store
            .commit_relay_rotation(plan, NOW)
            .expect("commit");

        // Their phone, their pass, and their stale card for us.
        let their_store = MessageStore::open(":memory:".to_string()).expect("open");
        let stale_card_token = relay_deposit_token_for(OLD_TOKEN.to_string());
        let us = Contact {
            user_id: fleet.person.user_id.clone(),
            sign_pk: fleet.person.sign_pk.clone(),
            agree_pk: fleet.person.agree_pk.clone(),
            relay_url: Some(RELAY_URL.to_string()),
            relay_token: Some(stale_card_token.clone()),
            ..a_contact("The person who revoked")
        };
        their_store.upsert_contact(us.clone()).expect("contact");
        let their_url = Some("https://relay.theirs.example".to_string());
        let their_token = Some("their-own-family-token-entirely".to_string());

        // Before any evidence their sends still go to our card's endpoint: an
        // endpoint is only written off by observing it fail, never by
        // assumption. This is the propagation window, and it is real work
        // going nowhere.
        assert!(core_contact_relay_endpoint_usable(0, 0, NOW));
        assert_eq!(
            resolved_contact_delivery_relay(
                us.relay_url.clone(),
                us.relay_token.clone(),
                their_url.clone(),
                their_token.clone(),
                true,
            )
            .expect("routes somewhere")
            .token,
            stale_card_token,
        );

        // Our relay answers 401 to the retired credential, which
        // `relay_classify_http_error` calls authoritative. Two of those write
        // the card off — the same two the rebuilt-relay-box incident put here,
        // unchanged by this work.
        let fault = crate::relay_status::relay_classify_http_error(401, None);
        assert!(crate::contact_relay_health::contact_relay_fault_is_authoritative(fault));
        assert_eq!(crate::core_contact_relay_streak_delta(fault), 1);
        let mut streak = 0;
        for _ in 0..crate::CONTACT_RELAY_STALE_STREAK {
            streak = their_store
                .note_contact_relay_rejected(us.user_id.clone(), NOW)
                .expect("rejection recorded");
        }
        assert!(crate::core_contact_relay_is_stale(streak));
        assert!(!core_contact_relay_endpoint_usable(streak, NOW, NOW));

        // Written off, their queue stops hammering a dead credential and falls
        // back to their own endpoint rather than stalling. It delivers nothing
        // to us — we are not on their relay — but the state is surfaced, which
        // is what makes "re-share your card" something a person can be asked
        // to do instead of a silent black hole.
        assert!(resolved_contact_delivery_relay(
            us.relay_url.clone(),
            us.relay_token.clone(),
            their_url.clone(),
            their_token.clone(),
            false,
        )
        .is_some());
        // Bounded, not permanent: the six-hour backstop re-probes even a
        // written-off endpoint, so the moment the credential is repaired by
        // any route, sending resumes with nobody restarting anything.
        assert!(core_contact_relay_endpoint_usable(
            streak,
            NOW,
            NOW + crate::CONTACT_RELAY_RECHECK_MS,
        ));

        // The repair itself is the shipped kind-9 path, unmodified. The notice
        // carries the attenuation of the NEW member token — never the member
        // token, which `encode_relay_update_content` enforces on its own — at
        // the rotation's epoch.
        let notice = crate::decode_relay_update_content(
            crate::encode_relay_update_content(crate::RelayUpdateContent {
                subject_user_id: fleet.person.user_id.clone(),
                relay_epoch: commit.relay_epoch,
                relay_url: commit.endpoint.url.clone(),
                relay_token: commit.endpoint.token.clone(),
            })
            .expect("encodes"),
        )
        .expect("decodes");
        assert_eq!(notice.relay_token, commit.deposit_token);
        assert!(relay_token_is_deposit(notice.relay_token.clone()));
        assert_ne!(notice.relay_token, stale_card_token);

        assert!(their_store
            .apply_contact_relay_update(fleet.person.user_id.clone(), notice.clone(), NOW)
            .expect("applies"));
        let repaired = their_store
            .get_contact(fleet.person.user_id.clone())
            .expect("contact")
            .expect("still a contact");
        assert_eq!(repaired.relay_token, Some(commit.deposit_token.clone()));
        // Repair also clears the write-off, so the very next pass posts again
        // instead of waiting out a six-hour probe window.
        assert!(their_store
            .list_contact_relay_rejections()
            .expect("rejections")
            .iter()
            .all(|row| !crate::core_contact_relay_is_stale(row.reject_streak)));

        // And the revoked device replaying the pre-rotation notice it saw
        // cannot drag them back onto the dead credential: T23's epoch is
        // strictly monotonic.
        let replayed = crate::decode_relay_update_content(
            crate::encode_relay_update_content(crate::RelayUpdateContent {
                subject_user_id: fleet.person.user_id.clone(),
                relay_epoch: commit.relay_epoch - 1,
                relay_url: RELAY_URL.to_string(),
                relay_token: OLD_TOKEN.to_string(),
            })
            .expect("encodes"),
        )
        .expect("decodes");
        assert!(!their_store
            .apply_contact_relay_update(fleet.person.user_id.clone(), replayed, NOW)
            .expect("applies"));
        assert_eq!(
            their_store
                .get_contact(fleet.person.user_id)
                .expect("contact")
                .expect("still a contact")
                .relay_token,
            Some(commit.deposit_token)
        );
    }

    /// The consequence the module doc names, pinned so a future change to it
    /// is deliberate: a contact who was given our *member* token — everyone
    /// sharing one Shore Pass — is not repaired by the kind-9 notice, because
    /// that notice can only ever carry a deposit credential. Their repair is
    /// a re-shared `CMRELAY1` setup card, and until then the honest answer for
    /// a send is "nowhere", not "post it into a mailbox they can no longer
    /// read".
    #[test]
    fn a_pass_sharer_is_not_repaired_by_the_notice_and_the_router_says_so() {
        let fleet = fleet();
        let (store, plan) = revoked(&fleet);
        let commit = store.commit_relay_rotation(plan, NOW).expect("commit");

        let their_card_token = relay_deposit_token_for(OLD_TOKEN.to_string());
        let own_url = Some(commit.endpoint.url.clone());
        let own_token = Some(commit.endpoint.token.clone());
        // Same host, retired credential, and our own credential is the only
        // live one on it. Written off, this resolves to nothing rather than to
        // a mailbox the recipient's dead member token can no longer drain.
        assert_eq!(
            resolved_contact_delivery_relay(
                Some(RELAY_URL.to_string()),
                Some(their_card_token),
                own_url,
                own_token,
                false,
            ),
            None
        );
        // The setup card is the repair, and it carries the member credential
        // the notice never can.
        let card = crate::make_relay_setup_card(
            commit.endpoint.url.clone(),
            commit.endpoint.token.clone(),
        )
        .expect("setup card");
        let parsed = crate::parse_relay_setup_text(card).expect("parses");
        assert_eq!(parsed.relay_token, commit.endpoint.token);
        assert!(!relay_token_is_deposit(parsed.relay_token));
    }

    #[test]
    fn the_rotate_call_is_encoded_and_verified_by_the_core_not_the_shell() {
        assert_eq!(relay_rotate_path(), "/family/rotate");
        let token = core_mint_relay_member_token();
        let signer = generate_identity();
        let body = relay_encode_rotate_request(
            OLD_TOKEN.to_string(),
            token.clone(),
            signer.sign_sk.clone(),
        )
        .expect("encodes");
        let sent: serde_json::Value = serde_json::from_slice(&body).expect("json");
        assert_eq!(sent["new_token"], token);
        // A deposit credential can never be a family's member token.
        // The signature is over BOTH credentials, so it is a statement about
        // this rotation rather than a bearer token of its own, and the
        // registered key is the person root -- stable across every change of
        // approving device, and the one key a stolen phone never holds.
        assert_eq!(sent["rotation_pk"], BASE64URL_NOPAD.encode(&signer.sign_pk));
        let signature = ed25519_dalek::Signature::from_slice(
            &BASE64URL_NOPAD
                .decode(sent["rotation_sig"].as_str().unwrap().as_bytes())
                .expect("base64"),
        )
        .expect("64 bytes");
        let mut signed = b"CruiseMesh family token rotation v1\0".to_vec();
        for field in [OLD_TOKEN, token.as_str()] {
            signed.extend_from_slice(&(field.len() as u16).to_be_bytes());
            signed.extend_from_slice(field.as_bytes());
        }
        use ed25519_dalek::Verifier;
        assert!(crate::crypto::verifying_key_from_bytes(&signer.sign_pk)
            .expect("key")
            .verify(&signed, &signature)
            .is_ok());

        assert!(relay_encode_rotate_request(
            OLD_TOKEN.to_string(),
            relay_deposit_token_for(token.clone()),
            signer.sign_sk,
        )
        .is_err());

        let answer = serde_json::json!({
            "family_token": token,
            "deposit_token": relay_deposit_token_for(token.clone()),
            "envelopes_moved": 7,
            "rotated": true,
        });
        let decoded =
            relay_decode_rotate_response(serde_json::to_vec(&answer).unwrap(), token.clone())
                .expect("decodes");
        assert_eq!(decoded.family_token, token);
        assert_eq!(decoded.envelopes_moved, 7);
        assert!(decoded.rotated);

        // An answer about a different family is refused rather than adopted:
        // this result is about to be written to platform storage and gossiped
        // to every contact.
        assert!(relay_decode_rotate_response(
            serde_json::to_vec(&answer).unwrap(),
            core_mint_relay_member_token(),
        )
        .is_err());
        // And the deposit half is re-derived, never trusted -- it is what
        // friend cards will carry.
        let forged = serde_json::json!({
            "family_token": token,
            "deposit_token": "cmdep1-not-derived-from-anything",
            "envelopes_moved": 0,
            "rotated": true,
        });
        assert!(relay_decode_rotate_response(serde_json::to_vec(&forged).unwrap(), token).is_err());
    }
}
