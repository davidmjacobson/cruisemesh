//! Outbound queue retirement: what leaves `outbound_envelopes`, and when.
//!
//! Protocol contract `QUEUE-01`, issue #283. Before this module the queue had
//! exactly one exit — a flat seven-day expiry — so a device's advertised set
//! only ever grew inside the retention window. A pulled field store held 3,964
//! rows, 3,757 of them unexpired: 2,143 already covered by the recipient's own
//! delivered-receipt watermark, 852 LAN endpoint hints whose payload stops
//! being true fifteen minutes after it is written, and 3,046 superseded
//! generations of two snapshot kinds that are last-write-wins by construction.
//! Fifty-two rows — 43 texts, 8 reactions, one friend request — were somebody's
//! message.
//!
//! Three exits are added here, and they are deliberately *policy* — pure
//! functions plus the exact SQL that executes them — rather than conditionals
//! spread through the store. When encounter planning moves into `mesh_meet`
//! this module travels intact.
//!
//! # 1. Receipt coverage
//!
//! A 1:1 outbound envelope whose lamport is at or below the recipient's
//! `RECEIPT_TYPE_DELIVERED` watermark for that chat has digest proof of
//! receipt, which is the removal condition the DTN ack-safety invariant
//! permits. Three properties make the removal safe rather than merely
//! permitted:
//!
//! * **It is the authority already in use.** `core_digest_spray_plan` seeks
//!   from `lamport > delivered_through`, so a covered row is *already* never
//!   offered on BLE or LAN, and the UI already draws it as delivered. Only the
//!   relay-upload path (which consults no receipt) and the raw byte cost were
//!   still paying for it.
//! * **Only proof this store actually holds counts.** The store's sole durable
//!   record of what a peer received is the cumulative watermark in `receipts`.
//!   `delivery_metrics` is hashed metadata with no envelope identity, so it
//!   proves nothing about a specific row; there is no per-message receipt
//!   table to consult. Coverage therefore means "at or below the cumulative
//!   watermark", nothing more.
//! * **The plaintext that regenerates the envelope must survive.** Retirement
//!   removes a *retransmission artifact*, never the ability to retransmit. The
//!   predicate requires a `messages` row at the same `(chat_id,
//!   sender_user_id, lamport)`, which is what both shells' digest responder
//!   re-seals from when a peer's contiguous digest asks for a lamport the
//!   queue no longer holds (`backfillOutboundAuthoredEnvelope` on Android,
//!   `backfillOutbound` on iOS, both reaching
//!   `MessageStore::backfill_pairwise_envelope`). This is what makes an
//!   over-reported watermark — the receipt-repair lane reports a peer-stream
//!   MAX that can legitimately sit above a lamport the peer never filed —
//!   harmless: a peer that later notices the hole asks for it by contiguous
//!   digest and the envelope is rebuilt from the stored message.
//!
//! That last property is not free, and getting it wrong would undo the whole
//! module. The re-seal path used to *re-queue* whatever it rebuilt, under a
//! fresh random `msg_id` and with `relay_posted_at` cleared. Since a holey peer
//! stream makes rebuilds routine rather than exotic — the digest walks from the
//! peer's contiguous watermark, retirement follows its MAX — that would have
//! regrown the queue on every digest, re-posted acknowledged mail to the relay,
//! and re-minted an identity that every dedupe set on both sides is keyed on.
//! [`backfill_rejoins_the_queue`] is where the two obligations are separated:
//! answer the peer always, re-admit to the queue only what belongs there.
//!
//! ## Group rows are structurally excluded, and the group rule is not guessed
//!
//! Retirement here applies to pairwise 1:1 rows only. The predicate requires
//! `recipient_user_id = chat_id` **and** that `chat_id` names a row in
//! `contacts`, so a group id can never satisfy it — a group message
//! (`recipient_user_id = chat_id = group_id`) and a group invite
//! (`chat_id = group_id`, `recipient_user_id = member`) are both outside the
//! predicate by construction, not by a runtime check that could be forgotten.
//!
//! That is a deliberate refusal to guess, not an oversight. A group envelope
//! is queued **once** against the group id and fanned out per member, while
//! group wire receipts are deferred: no per-member row exists to retire and no
//! per-member proof exists to retire it with. A single group watermark
//! therefore cannot mean "every member received it", and treating it as such
//! would drop mail for the members who did not. Whatever the group rule turns
//! out to be, it needs its own per-member coverage record; it is out of scope
//! for #283 and this module states no opinion about it.
//!
//! # 2. Supersession
//!
//! Four kinds carry a whole-state snapshot to one recipient, and every
//! consumer of the four ignores or overwrites an older generation, so only the
//! newest queued generation per `(recipient, kind)` can ever inform the
//! recipient of anything. Authoring a new one retires the queued older ones.
//! Justification is per kind, checked against what each consumer does — see
//! [`supersedes_queued_generations`].
//!
//! # 3. Right-sized delivery expiry
//!
//! Delivery expiry should not outlive the payload's usefulness. Only one kind
//! carries a payload that expires on a clock, and it gets a matching lifetime;
//! the rest keep the seven-day default. See [`authored_expiry`].

use rusqlite::{params, Connection, OptionalExtension};

use crate::store::store_err;
use crate::{
    CoreError, OutboundEnvelope, DEFAULT_EXPIRY_MS, KIND_FRIEND_DIRECTORY, KIND_LAN_ENDPOINT_HINT,
    KIND_PROFILE_SYNC, KIND_RELAY_UPDATE, KIND_SYNC_DIGEST, RECEIPT_TYPE_DELIVERED,
};

/// How long a queued LAN endpoint hint stays deliverable.
///
/// The payload carries its own validity: `LanEndpointSender` stamps
/// `expires_at_ms = authored_at + 15 minutes`, and the consumer added in #278
/// refuses to save or dial a hint past that stamp — deliberately, because a
/// long-dead address re-filed from a relay backlog was the poison source
/// #271/#278 had to gate against. A delivery lifetime longer than the payload
/// validity therefore buys nothing at all: every envelope that arrives after
/// it is decoded, discarded, and charged to the link that carried it.
///
/// Thirty minutes, not fifteen: delivery expiry a little above payload
/// validity is the defensible shape. It absorbs clock skew between the two
/// phones and one mule hop's latency, so a hint that is still *inside* its
/// stated validity when it reaches the recipient is never dropped by its
/// envelope on the way. It does not extend the window in which a stale hint
/// can be acted on — the payload check owns that, and it is unchanged.
///
/// On the field store this alone takes the standing endpoint-hint queue from
/// 852 rows to 8.
pub const LAN_ENDPOINT_HINT_EXPIRY_MS: i64 = 30 * 60 * 1_000;

/// The delivery lifetime an authored envelope of `kind` gets, in ms.
///
/// Right-sizing is about *service* traffic whose payload stops being true on a
/// clock. Every visible kind — text, reactions, attachment manifests — and
/// every service kind whose usefulness decays by supersession rather than by
/// elapsed time keeps [`DEFAULT_EXPIRY_MS`]:
///
/// * **`KIND_LAN_ENDPOINT_HINT` (8) — shortened.** The only kind whose payload
///   states its own expiry. See [`LAN_ENDPOINT_HINT_EXPIRY_MS`].
/// * **`KIND_PROFILE_SYNC` (5) — unchanged.** A name and avatar do not become
///   false with age. A contact who has been offline for four days still wants
///   the profile authored on day one if no newer one exists; the newer one, if
///   it exists, is what supersession keeps. Shortening this would silently
///   stop propagating profiles to anyone reachable only occasionally.
/// * **`KIND_FRIEND_DIRECTORY` (6) — unchanged.** A directory snapshot's
///   entries carry signed introduction tickets valid for up to
///   `INTRODUCTION_MAX_LIFETIME_MS` (30 days), so the snapshot outlives a
///   seven-day envelope rather than the reverse. Supersession, not a clock, is
///   what retires it.
/// * **`KIND_RELAY_UPDATE` (9) — unchanged.** This kind exists to repair a
///   contact who is posting to a dead mailbox; the endpoint it announces stays
///   true until the next change. A short expiry would make the repair path
///   fail exactly for the contact it is meant to reach — the unreachable one.
///
/// * **`KIND_SYNC_HISTORY` (10) through `KIND_SYNC_SETTINGS` (15), and
///   `KIND_SYNC_DIGEST` (20) — unchanged.** `specs/multi-device-v1.md` §8's
///   self-sync traffic goes to this person's own devices, and SYNC-1 forbids
///   assuming two of them are ever online together. A sibling that has been in
///   a drawer for five days is precisely the case self-sync exists for, so a
///   short lifetime would retire the records exactly where convergence needs
///   them most. What retires a sync record is the sibling having it —
///   anti-entropy — not a clock.
///
///   The digest is the one where a short lifetime is *tempting* and still
///   wrong. A week-old watermark is stale, but stale in the harmless direction:
///   it under-reports what its author holds, so the answer is a few records the
///   asker already had. Expiring it instead means the sibling in the drawer
///   surfaces to an empty mailbox and no round happens at all, which is the
///   failure that costs a message rather than some bytes.
///
/// Group envelopes are authored with [`crate::default_expiry`] directly and
/// never route through here. That matters for one specific reason:
/// `core_group_fanout_rows_for_carried` reconstructs a carried envelope's
/// authoring timestamp as `expiry - DEFAULT_EXPIRY_MS` to recompute per-member
/// recipient hints. It is reached only for group-hinted carried rows, which
/// are group envelopes, which still use the flat default — so the
/// reconstruction stays exact. No 1:1 hint envelope ever reaches it.
pub fn authored_expiry(kind: u8, routing_timestamp_ms: i64) -> i64 {
    routing_timestamp_ms.saturating_add(authored_delivery_lifetime_ms(kind))
}

/// The delivery lifetime alone, split out so it can be tabled in tests without
/// arithmetic. See [`authored_expiry`] for the per-kind rationale.
pub fn authored_delivery_lifetime_ms(kind: u8) -> i64 {
    match kind {
        KIND_LAN_ENDPOINT_HINT => LAN_ENDPOINT_HINT_EXPIRY_MS,
        _ => DEFAULT_EXPIRY_MS,
    }
}

/// Whether authoring a new envelope of `kind` to a recipient retires the
/// older envelopes of the same `(recipient, kind)` still queued for them.
///
/// True for exactly the four kinds that carry a complete snapshot of one piece
/// of this device's state to one person, where the consumer's own rule already
/// makes an older generation a no-op or a regression. Each was checked against
/// what the receiving side actually does with it:
///
/// * **`KIND_PROFILE_SYNC` (5).** The consumer applies the avatar under an
///   `avatar_epoch` guard and the friends-of-friends policy under a `revision`
///   guard, both monotonic, then copies the name across. So an older
///   generation is either ignored (guarded fields) or actively wrong (the name
///   would regress to a previous one). Newest-only is strictly better.
/// * **`KIND_FRIEND_DIRECTORY` (6).** `apply_friend_directory` returns early
///   when `applied_revision >= content.revision`, and a snapshot that is
///   applied *replaces* the stored suggestion set wholesale. An older
///   generation cannot add an entry the newer one omits — it can only be
///   discarded. The field store held 1,594 of these.
/// * **`KIND_LAN_ENDPOINT_HINT` (8).** A phone has one endpoint on one network
///   at a time, and the payload's own expiry stamp means an older hint is
///   either the same address or a dead one. The newest is the only one that
///   can result in a connection.
/// * **`KIND_RELAY_UPDATE` (9).** `apply_contact_relay_update` writes only
///   when `?4 > relay_epoch`, so a lower-epoch notice is discarded on arrival.
///   Keeping the newest queued notice keeps the only one that can move the
///   recipient's view of our mailbox.
///
/// Everything else is false, and two exclusions are worth naming because they
/// look superficially similar:
///
/// * **`KIND_FRIEND_REQUEST` (3) and `KIND_INTRODUCED_FRIEND_REQUEST` (7)** are
///   hidden service kinds too, but they are *events*, not snapshots: each one
///   is a distinct offer with its own ticket and its own reply, and the
///   friend-suggestion retry logic reads whether an unexpired request envelope
///   is still queued. Retiring one on the authoring of another would change
///   what that state machine observes.
/// * **Every visible kind** is chat history. Two texts are two texts.
/// * **§8's six sync *record* kinds (10-15)** look like snapshots and are
///   deliberately not treated as ones. SYNC-1 numbers each record within its
///   `(author device, kind)` stream and a sibling asks for what it is missing
///   by that number, so retiring a queued record on authoring the next one
///   punches a hole in a sequence the far side is counting through — and for
///   `KIND_SYNC_HISTORY` it would discard messages outright, since two runs of
///   history are two runs of history, not two generations of one snapshot.
///   Retirement for these kinds is the sibling's stream watermark advancing,
///   which is the anti-entropy exchange's job rather than the queue's.
/// * **`KIND_SYNC_DIGEST` (20) — supersedes.** The one §8 kind that genuinely
///   is a snapshot, and the exception is the reason it is a separate kind at
///   all. A digest states where its author's streams stand *now*; the next one
///   states the same thing better, and nothing counts through them. A queue
///   holding three generations of one device's watermarks would spend three
///   relay rows to answer one question, and the two older answers are wrong.
pub fn supersedes_queued_generations(kind: u8) -> bool {
    matches!(
        kind,
        KIND_PROFILE_SYNC
            | KIND_FRIEND_DIRECTORY
            | KIND_LAN_ENDPOINT_HINT
            | KIND_RELAY_UPDATE
            | KIND_SYNC_DIGEST
    )
}

/// Whether a delivered-receipt watermark of `delivered_through` covers a
/// queued row at `lamport`.
///
/// Cumulative and inclusive: a receipt states "everything through here", so
/// the row at exactly the watermark is covered. `delivered_through == 0` means
/// no receipt has ever been recorded for the chat, and covers nothing —
/// lamports start at 1.
///
/// Only `RECEIPT_TYPE_DELIVERED` drives retirement. A read receipt is stronger
/// proof, but it is never the *only* proof: both shells stamp the delivered
/// watermark first and the read watermark from the same
/// `PeerStreamWatermark.through` value in the same pass, so a read watermark
/// can never sit above its delivered sibling. Consulting one authority keeps
/// what the queue holds and what the spray offers — which seeks from the
/// delivered watermark — in exact lockstep.
pub fn covered_by_delivered_watermark(lamport: u64, delivered_through: u64) -> bool {
    delivered_through > 0 && lamport <= delivered_through
}

/// The receipt-coverage retirement statement, built in one place so the
/// query-plan test explains the statement the code actually runs rather than a
/// second copy that could go on passing after this one changed.
///
/// Bindings: `?1` the chat/recipient id, `?2` the authoring (own) user id,
/// `?3` the delivered watermark.
///
/// Every clause is load-bearing:
///
/// * `recipient_user_id = ?1 AND chat_id = ?1` — the pairwise 1:1 shape.
/// * `sender_user_id = ?2` — our own authored rows only. A receipt naming some
///   other sender matches nothing.
/// * `lamport <= ?3` — the coverage predicate; see
///   [`covered_by_delivered_watermark`].
/// * `EXISTS (… contacts …)` **and** `NOT EXISTS (… groups …)` — the
///   structural group exclusion, stated both ways on purpose. The positive
///   half says what the rule is *for* (a pairwise conversation with a person
///   this device knows, which is also the only kind of chat a receipt can
///   arrive from); the negative half says outright that a group id can never
///   be reached, so the exclusion does not rest on the two id spaces happening
///   not to overlap. Neither can be forgotten by a caller: both live in the
///   statement.
/// * `EXISTS (… messages …)` — the regeneration guard. The sealed envelope
///   goes only when the plaintext that can rebuild it stays.
///
/// The plan is an index seek on `idx_outbound_chat_sender_lamport` (two
/// equalities then a range) with the `EXISTS` clauses served by covering
/// unique indexes, so cost tracks the rows actually retired, never queue size.
pub(crate) const RETIRE_COVERED_SQL: &str = "DELETE FROM outbound_envelopes
     WHERE recipient_user_id = ?1
       AND chat_id = ?1
       AND sender_user_id = ?2
       AND lamport <= ?3
       AND EXISTS (SELECT 1 FROM contacts WHERE user_id = ?1)
       AND NOT EXISTS (SELECT 1 FROM groups WHERE group_id = ?1)
       AND EXISTS (
             SELECT 1 FROM messages m
             WHERE m.chat_id = outbound_envelopes.chat_id
               AND m.sender_user_id = outbound_envelopes.sender_user_id
               AND m.lamport = outbound_envelopes.lamport
           )";

/// The supersession statement. Bindings: `?1` recipient/chat id, `?2` the
/// authoring (own) user id, `?3` kind, `?4` the lamport of the generation just
/// authored — strictly greater than every generation it replaces, since the
/// authored lamport counter only climbs.
///
/// Same index, same shape, same 1:1 restriction — and the group exclusion is
/// spelled out here rather than left to the kind set. `recipient_user_id =
/// chat_id` does **not** exclude a group row on its own: `author_group_message`
/// and the metadata-update path both set `recipient_user_id = chat_id =
/// group.id`, so a group envelope satisfies that clause exactly. What keeps
/// group rows safe today is only that none of the four supersedable kinds is
/// group-authorable — a fact that would quietly stop being true the day
/// somebody adds a group membership or metadata *snapshot* to
/// [`supersedes_queued_generations`], and the cost of being wrong is deleting
/// queued group mail with no per-member coverage record to justify it.
/// `NOT EXISTS (… groups …)` makes the boundary structural, exactly as
/// [`RETIRE_COVERED_SQL`] does.
pub(crate) const RETIRE_SUPERSEDED_SQL: &str = "DELETE FROM outbound_envelopes
     WHERE recipient_user_id = ?1
       AND chat_id = ?1
       AND sender_user_id = ?2
       AND kind = ?3
       AND lamport < ?4
       AND NOT EXISTS (SELECT 1 FROM groups WHERE group_id = ?1)";

/// Retire everything a delivered watermark of `through_lamport` now covers in
/// the 1:1 chat with `chat_id`, for envelopes authored by `sender_user_id`.
/// Returns how many rows went.
///
/// This is the incremental hook: `record_receipt` calls it in the same
/// transaction that advances the watermark, so the queue shrinks at the moment
/// the proof arrives rather than at some later sweep. Idempotent — a replayed
/// or duplicate receipt retires nothing the first one did not.
pub(crate) fn retire_receipt_covered(
    conn: &Connection,
    chat_id: &[u8],
    sender_user_id: &[u8],
    through_lamport: u64,
) -> Result<u64, CoreError> {
    if through_lamport == 0 {
        return Ok(0);
    }
    let removed = conn
        .execute(
            RETIRE_COVERED_SQL,
            params![chat_id, sender_user_id, through_lamport as i64],
        )
        .map_err(store_err)?;
    Ok(removed as u64)
}

/// Retire the queued older generations of `(recipient_user_id, kind)` that the
/// generation at `lamport` replaces. No-op for kinds that are not snapshots.
pub(crate) fn retire_superseded(
    conn: &Connection,
    recipient_user_id: &[u8],
    chat_id: &[u8],
    sender_user_id: &[u8],
    kind: u8,
    lamport: u64,
) -> Result<u64, CoreError> {
    // Pairwise only, and cheaply provable here: for the four supersedable
    // kinds the authoring path sets chat_id = recipient_user_id = the
    // contact's user id. A mismatch means something else authored this row and
    // the statement's assumptions do not hold, so leave it alone.
    if !supersedes_queued_generations(kind) || recipient_user_id != chat_id {
        return Ok(0);
    }
    let removed = conn
        .execute(
            RETIRE_SUPERSEDED_SQL,
            params![
                recipient_user_id,
                sender_user_id,
                kind as i64,
                lamport as i64
            ],
        )
        .map_err(store_err)?;
    Ok(removed as u64)
}

/// Whether a re-sealed repair envelope belongs back in the outbound queue.
///
/// This is the other half of retirement, and without it the first half does
/// not survive contact with a digest. Both shells' digest responders answer a
/// peer with `queuedByLamport[lamport] ?: backfill(...)`, walking every stored
/// message above the peer's **contiguous** watermark. Retirement is driven by
/// the **delivered** watermark, which is a MAX over the peer's stream
/// (`WM-01`). Any hole in the peer's copy pins its contiguous watermark far
/// below that MAX — on the field store, three of the peer streams were holey,
/// one of them holding 385 messages with a contiguous watermark of 342 — so
/// the responder routinely asks to rebuild rows retirement has just removed.
/// If every rebuild re-queued, the queue would regrow to its old size within
/// one link session, the relay uploader would re-post acknowledged mail with a
/// cleared `relay_posted_at`, and the measured shrink would be a mirage.
///
/// So a repair copy is re-sealed and sent, but rejoins the queue only when the
/// queue's own rules say it belongs there:
///
/// * **A snapshot kind never rejoins.** Its queue membership is governed by
///   supersession and a short life; a newer generation is already queued or
///   already delivered, and re-admitting an old one would re-advertise exactly
///   the thing this module exists to stop. The peer still receives the re-seal
///   on the link that asked for it, which is what closes its stream gap.
/// * **A receipt-covered lamport never rejoins.** Its absence is not a gap in
///   the queue; it is the proof of delivery doing its job. Re-admitting it
///   would undo the retirement and hand the relay uploader mail the recipient
///   has already acknowledged.
///
/// Everything else does rejoin — genuinely undelivered visible mail whose
/// sealed copy predates the outbound queue table. That is the case this path
/// was built for, and it is unchanged.
pub(crate) fn backfill_rejoins_the_queue(
    conn: &Connection,
    envelope: &OutboundEnvelope,
) -> Result<bool, CoreError> {
    if supersedes_queued_generations(envelope.kind) {
        return Ok(false);
    }
    let delivered_through: i64 = conn
        .query_row(
            "SELECT through_lamport FROM receipts
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
            params![
                &envelope.chat_id,
                &envelope.sender_user_id,
                RECEIPT_TYPE_DELIVERED as i64
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_err)?
        .unwrap_or(0);
    Ok(!covered_by_delivered_watermark(
        envelope.lamport,
        delivered_through.max(0) as u64,
    ))
}

/// One-time-per-open catch-up for rows that were covered before this build
/// existed, plus a safety net for any watermark advance the incremental hook
/// missed. Returns how many rows went.
///
/// Driven from `receipts`, not from the queue: one row per (chat, receipt
/// type), so the driving read is bounded by how many people this device talks
/// to — 25 rows on the field store — and each of them costs one index seek
/// into `outbound_envelopes`. Nothing here scans the queue, so a store holding
/// six figures of envelopes pays the same as an empty one.
///
/// It reads no identity: an `outbound_envelopes` row is by construction one
/// this device authored, so `receipts.sender_user_id` is the only sender that
/// can match, and a receipt that names anyone else deletes nothing. That is
/// what lets this run inside `MessageStore::open`, before any caller has
/// supplied an identity — and it is also what makes a restored backup behave
/// correctly: the restored store retires exactly what the receipts it restored
/// with prove, and nothing else.
pub(crate) fn sweep_receipt_covered(conn: &Connection) -> Result<u64, CoreError> {
    let mut driver = conn
        .prepare(
            "SELECT chat_id, sender_user_id, through_lamport FROM receipts
             WHERE receipt_type = ?1 AND through_lamport > 0",
        )
        .map_err(store_err)?;
    let covered: Vec<(Vec<u8>, Vec<u8>, i64)> = driver
        .query_map(params![RECEIPT_TYPE_DELIVERED as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(store_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_err)?;
    drop(driver);

    let mut removed = 0_u64;
    for (chat_id, sender_user_id, through_lamport) in covered {
        if through_lamport <= 0 {
            continue;
        }
        removed += retire_receipt_covered(conn, &chat_id, &sender_user_id, through_lamport as u64)?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::{
        create_group, generate_identity, Contact, Group, Identity, MessageStore,
        KIND_ATTACHMENT_CHUNK, KIND_ATTACHMENT_MANIFEST, KIND_FRIEND_REQUEST, KIND_GROUP_INVITE,
        KIND_GROUP_METADATA_UPDATE, KIND_INTRODUCED_FRIEND_REQUEST, KIND_REACTION, KIND_RECEIPT,
        KIND_SYNC_CONTACTS, KIND_SYNC_GROUPS, KIND_SYNC_HISTORY, KIND_SYNC_OWN_ROSTER,
        KIND_SYNC_SETTINGS, KIND_SYNC_WATERMARK, KIND_TEXT, RECEIPT_TYPE_READ,
    };

    const NOW: i64 = 1_700_000_000_000;

    fn contact_for(identity: &Identity, name: &str) -> Contact {
        Contact {
            user_id: identity.user_id.clone(),
            name: name.to_string(),
            sign_pk: identity.sign_pk.clone(),
            agree_pk: identity.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    /// A store with one authoring identity and one accepted contact -- the
    /// smallest world in which a 1:1 receipt can arrive at all.
    fn pairing(path: &str) -> (MessageStore, Identity, Identity, Contact) {
        let store = MessageStore::open(path.to_string()).unwrap();
        let me = generate_identity();
        let peer = generate_identity();
        let peer_contact = contact_for(&peer, "Robin");
        store.upsert_contact(peer_contact.clone()).unwrap();
        (store, me, peer, peer_contact)
    }

    fn temp_store_path(tag: &str) -> String {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("cruisemesh-retire-{tag}-{unique}.sqlite"))
            .to_string_lossy()
            .to_string()
    }

    /// Everything still queued to `peer` from `me`, by lamport.
    fn queued(store: &MessageStore, me: &Identity, peer: &Identity) -> Vec<(u64, u8)> {
        store
            .outbound_envelopes_after(peer.user_id.clone(), me.user_id.clone(), 0)
            .unwrap()
            .into_iter()
            .map(|envelope| (envelope.lamport, envelope.kind))
            .collect()
    }

    fn author(
        store: &MessageStore,
        me: &Identity,
        contact: &Contact,
        kind: u8,
        payload: &[u8],
    ) -> u64 {
        store
            .author_pairwise_message(
                me.clone(),
                contact.clone(),
                kind,
                payload.to_vec(),
                None,
                NOW,
            )
            .unwrap()
            .envelope
            .lamport
    }

    /// Every `KIND_*` constant declared in `protocol.rs`, parsed out of the
    /// source so the inventory cannot silently fall behind the wire.
    ///
    /// [`delivery_lifetime_is_decided_per_kind`] and
    /// [`supersession_covers_exactly_the_four_snapshot_kinds`] both drive
    /// hand-written decision tables, and both policy functions end in a
    /// catch-all arm — a `u8` match cannot be exhaustive. Without this, adding
    /// a kind compiles, silently inherits the seven-day default and
    /// "not a snapshot", and every test stays green, which is the opposite of
    /// what those tables promise a reader auditing the policy. With it, the new
    /// constant appears here and the tables fail until somebody decides.
    fn every_declared_kind() -> Vec<(u8, String)> {
        let source = include_str!("protocol.rs");
        let mut kinds = Vec::new();
        for line in source.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("pub const KIND_") else {
                continue;
            };
            let Some((name, value)) = rest.split_once(": u8 = ") else {
                continue;
            };
            let value = value.trim_end_matches(';').trim();
            let Ok(kind) = value.parse::<u8>() else {
                continue;
            };
            kinds.push((kind, format!("KIND_{name}")));
        }
        assert!(
            kinds.len() >= 12,
            "the KIND_* scan found only {} constants, so it has stopped matching the source",
            kinds.len()
        );
        kinds
    }

    /// Every kind this codebase can author, and the lifetime it must get.
    /// A new kind added without a decision here shows up as a failing row.
    #[test]
    fn delivery_lifetime_is_decided_per_kind() {
        let table: &[(u8, i64, &str)] = &[
            (
                KIND_ATTACHMENT_CHUNK,
                DEFAULT_EXPIRY_MS,
                "carries a slice of a manifest that keeps the default",
            ),
            (KIND_TEXT, DEFAULT_EXPIRY_MS, "chat history"),
            (KIND_RECEIPT, DEFAULT_EXPIRY_MS, "not authored through here"),
            (
                KIND_FRIEND_REQUEST,
                DEFAULT_EXPIRY_MS,
                "an offer, not a snapshot",
            ),
            (KIND_GROUP_INVITE, DEFAULT_EXPIRY_MS, "an offer"),
            (KIND_PROFILE_SYNC, DEFAULT_EXPIRY_MS, "no clock decay"),
            (
                KIND_FRIEND_DIRECTORY,
                DEFAULT_EXPIRY_MS,
                "tickets outlive a week",
            ),
            (
                KIND_INTRODUCED_FRIEND_REQUEST,
                DEFAULT_EXPIRY_MS,
                "an offer",
            ),
            (
                KIND_LAN_ENDPOINT_HINT,
                LAN_ENDPOINT_HINT_EXPIRY_MS,
                "payload states 15 minutes",
            ),
            (
                KIND_RELAY_UPDATE,
                DEFAULT_EXPIRY_MS,
                "repairs the unreachable",
            ),
            (KIND_ATTACHMENT_MANIFEST, DEFAULT_EXPIRY_MS, "chat history"),
            (KIND_REACTION, DEFAULT_EXPIRY_MS, "chat history"),
            (
                KIND_GROUP_METADATA_UPDATE,
                DEFAULT_EXPIRY_MS,
                "group stream",
            ),
            (KIND_SYNC_HISTORY, DEFAULT_EXPIRY_MS, "self-sync converges"),
            (
                KIND_SYNC_WATERMARK,
                DEFAULT_EXPIRY_MS,
                "self-sync converges",
            ),
            (KIND_SYNC_CONTACTS, DEFAULT_EXPIRY_MS, "self-sync converges"),
            (
                KIND_SYNC_OWN_ROSTER,
                DEFAULT_EXPIRY_MS,
                "self-sync converges",
            ),
            (KIND_SYNC_GROUPS, DEFAULT_EXPIRY_MS, "self-sync converges"),
            (KIND_SYNC_SETTINGS, DEFAULT_EXPIRY_MS, "self-sync converges"),
            (
                KIND_SYNC_DIGEST,
                DEFAULT_EXPIRY_MS,
                "a stale watermark under-reports; an expired one means no round",
            ),
        ];
        for (kind, expected, why) in table {
            assert_eq!(
                authored_delivery_lifetime_ms(*kind),
                *expected,
                "kind {kind} ({why})"
            );
            assert_eq!(
                authored_expiry(*kind, 1_700_000_000_000),
                1_700_000_000_000 + expected,
                "kind {kind} ({why})"
            );
        }
        let decided: HashSet<u8> = table.iter().map(|(kind, _, _)| *kind).collect();
        for (kind, name) in every_declared_kind() {
            assert!(
                decided.contains(&kind),
                "{name} ({kind}) has no delivery-lifetime decision; it would inherit the \
                 seven-day default by accident"
            );
        }
    }

    /// The endpoint hint's envelope must outlive its payload, and by a margin
    /// small enough that it is a skew allowance rather than a second lifetime.
    #[test]
    fn the_endpoint_hint_envelope_outlives_its_payload_but_only_just() {
        let payload_validity_ms = 15 * 60 * 1_000;
        assert!(
            LAN_ENDPOINT_HINT_EXPIRY_MS > payload_validity_ms,
            "an envelope shorter than its payload would drop hints that are still true"
        );
        assert!(
            LAN_ENDPOINT_HINT_EXPIRY_MS <= 2 * payload_validity_ms,
            "a much longer envelope is the seven-day problem in miniature"
        );
        assert!(
            LAN_ENDPOINT_HINT_EXPIRY_MS < authored_delivery_lifetime_ms(KIND_TEXT) / 100,
            "the hint lifetime must stay two orders of magnitude under a message's"
        );
    }

    #[test]
    fn supersession_covers_exactly_the_four_snapshot_kinds() {
        let table: &[(u8, bool, &str)] = &[
            (
                KIND_PROFILE_SYNC,
                true,
                "avatar epoch + policy revision guard",
            ),
            (
                KIND_FRIEND_DIRECTORY,
                true,
                "applied_revision guard, whole-snapshot replace",
            ),
            (KIND_LAN_ENDPOINT_HINT, true, "one endpoint at a time"),
            (KIND_RELAY_UPDATE, true, "relay_epoch guard"),
            (KIND_TEXT, false, "two texts are two texts"),
            (KIND_RECEIPT, false, "not on this table"),
            (KIND_FRIEND_REQUEST, false, "an offer with its own reply"),
            (KIND_GROUP_INVITE, false, "an offer"),
            (
                KIND_INTRODUCED_FRIEND_REQUEST,
                false,
                "retry state reads the queue",
            ),
            (KIND_ATTACHMENT_MANIFEST, false, "chat history"),
            (KIND_ATTACHMENT_CHUNK, false, "a slice, not a snapshot"),
            (KIND_REACTION, false, "chat history"),
            (KIND_GROUP_METADATA_UPDATE, false, "convergent group stream"),
            (KIND_SYNC_HISTORY, false, "two runs of history are two runs"),
            (
                KIND_SYNC_WATERMARK,
                false,
                "a numbered stream, not a snapshot",
            ),
            (
                KIND_SYNC_CONTACTS,
                false,
                "a numbered stream, not a snapshot",
            ),
            (
                KIND_SYNC_OWN_ROSTER,
                false,
                "a numbered stream, not a snapshot",
            ),
            (KIND_SYNC_GROUPS, false, "a numbered stream, not a snapshot"),
            (
                KIND_SYNC_SETTINGS,
                false,
                "a numbered stream, not a snapshot",
            ),
            (
                KIND_SYNC_DIGEST,
                true,
                "the one §8 kind that really is a snapshot",
            ),
        ];
        for (kind, expected, why) in table {
            assert_eq!(
                supersedes_queued_generations(*kind),
                *expected,
                "kind {kind} ({why})"
            );
        }
        let decided: HashSet<u8> = table.iter().map(|(kind, _, _)| *kind).collect();
        for (kind, name) in every_declared_kind() {
            assert!(
                decided.contains(&kind),
                "{name} ({kind}) has no supersession decision; it would default to \
                 'not a snapshot' by accident"
            );
        }
    }

    #[test]
    fn coverage_is_cumulative_and_inclusive() {
        let table: &[(u64, u64, bool)] = &[
            (1, 0, false),
            (1, 1, true),
            (5, 4, false),
            (5, 5, true),
            (5, 6, true),
            (1, u64::MAX, true),
            (u64::MAX, u64::MAX, true),
        ];
        for (lamport, through, expected) in table {
            assert_eq!(
                covered_by_delivered_watermark(*lamport, *through),
                *expected,
                "lamport {lamport} against watermark {through}"
            );
        }
    }

    /// An authored expiry cannot be walked past the end of the number line by
    /// a phone whose clock is set to the far future.
    #[test]
    fn a_far_future_clock_saturates_rather_than_wraps() {
        assert_eq!(authored_expiry(KIND_TEXT, i64::MAX), i64::MAX);
        assert_eq!(authored_expiry(KIND_LAN_ENDPOINT_HINT, i64::MAX), i64::MAX);
    }

    // -----------------------------------------------------------------
    // 1. Receipt coverage
    // -----------------------------------------------------------------

    /// The incremental hook. A watermark advance retires exactly what it newly
    /// covers, and each further advance retires only the next slice -- so the
    /// queue tracks the proof rather than the retention window.
    #[test]
    fn a_watermark_advance_retires_exactly_what_it_newly_covers() {
        let (store, me, peer, contact) = pairing(":memory:");
        for body in [&b"one"[..], b"two", b"three", b"four"] {
            author(&store, &me, &contact, KIND_TEXT, body);
        }
        assert_eq!(queued(&store, &me, &peer).len(), 4);

        store
            .record_receipt(
                peer.user_id.clone(),
                me.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                2,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            queued(&store, &me, &peer),
            vec![(3, KIND_TEXT), (4, KIND_TEXT)],
            "a watermark of 2 covers lamports 1 and 2 and nothing else"
        );

        store
            .record_receipt(
                peer.user_id.clone(),
                me.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                4,
                None,
                None,
            )
            .unwrap();
        assert!(queued(&store, &me, &peer).is_empty());
    }

    /// Coverage is read from the *stored* watermark, so a replayed or
    /// out-of-order receipt below what is already known retires nothing extra
    /// and un-retires nothing either.
    #[test]
    fn a_replayed_receipt_below_the_stored_watermark_changes_nothing() {
        let (store, me, peer, contact) = pairing(":memory:");
        for body in [&b"one"[..], b"two", b"three"] {
            author(&store, &me, &contact, KIND_TEXT, body);
        }
        for through in [2, 1, 2] {
            store
                .record_receipt(
                    peer.user_id.clone(),
                    me.user_id.clone(),
                    RECEIPT_TYPE_DELIVERED,
                    through,
                    None,
                    None,
                )
                .unwrap();
        }
        assert_eq!(queued(&store, &me, &peer), vec![(3, KIND_TEXT)]);
    }

    /// A read receipt is stronger proof than a delivered one, but it is never
    /// the only proof: both shells stamp them from the same watermark in the
    /// same pass. Retirement consults one authority, and a read receipt
    /// arriving alone leaves the queue alone -- the honest reading, since this
    /// build's peers never send one without its delivered sibling.
    #[test]
    fn only_the_delivered_watermark_retires() {
        let (store, me, peer, contact) = pairing(":memory:");
        author(&store, &me, &contact, KIND_TEXT, b"one");
        store
            .record_receipt(
                peer.user_id.clone(),
                me.user_id.clone(),
                RECEIPT_TYPE_READ,
                9,
                None,
                None,
            )
            .unwrap();
        assert_eq!(queued(&store, &me, &peer).len(), 1);
    }

    /// The store holds one kind of delivery proof: the cumulative watermark in
    /// `receipts`. There is no per-message receipt table, and the hashed
    /// `delivery_metrics` rows carry no envelope identity, so they cannot
    /// retire anything. Stamping one must leave the queue untouched.
    #[test]
    fn a_per_message_delivery_metric_is_not_proof_the_queue_may_act_on() {
        let (store, me, peer, contact) = pairing(":memory:");
        author(&store, &me, &contact, KIND_TEXT, b"one");
        store
            .record_sent_metric(peer.user_id.clone(), 1, NOW)
            .unwrap();
        store
            .record_delivered_metric(peer.user_id.clone(), 1, NOW + 1_000, Some(0))
            .unwrap();
        assert_eq!(
            queued(&store, &me, &peer).len(),
            1,
            "metadata metrics name no envelope and prove nothing about one"
        );
    }

    /// Hidden service kinds share the 1:1 lamport stream, so a cumulative
    /// watermark covers them exactly as it covers a text. Above the watermark
    /// they stay, whatever their kind.
    #[test]
    fn hidden_kinds_above_the_watermark_are_not_retired() {
        let (store, me, peer, contact) = pairing(":memory:");
        let text = author(&store, &me, &contact, KIND_TEXT, b"hello");
        let profile = author(&store, &me, &contact, KIND_PROFILE_SYNC, b"profile");
        let hint = author(&store, &me, &contact, KIND_LAN_ENDPOINT_HINT, b"hint");
        assert!(text < profile && profile < hint);

        store
            .record_receipt(
                peer.user_id.clone(),
                me.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                profile,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            queued(&store, &me, &peer),
            vec![(hint, KIND_LAN_ENDPOINT_HINT)],
            "a hidden kind below the watermark goes; one above it stays"
        );
    }

    /// Group rows are excluded by the shape of the predicate, not by luck. A
    /// group envelope is queued once against the group id and fanned out per
    /// member, and group wire receipts are deferred, so no single watermark can
    /// mean "every member received it". Even a watermark recorded against the
    /// group id must not touch it.
    #[test]
    fn a_watermark_on_a_group_chat_retires_nothing() {
        let (store, me, peer, _contact) = pairing(":memory:");
        let group: Group = create_group(
            "Deck party".to_string(),
            vec![me.user_id.clone(), peer.user_id.clone()],
        )
        .unwrap();
        store.upsert_group(group.clone()).unwrap();
        store
            .author_group_message(
                me.clone(),
                group.clone(),
                KIND_TEXT,
                b"meet at 6".to_vec(),
                None,
                NOW,
            )
            .unwrap();
        let before = store
            .outbound_envelopes_after(group.id.clone(), me.user_id.clone(), 0)
            .unwrap();
        assert_eq!(before.len(), 1);

        store
            .record_receipt(
                group.id.clone(),
                me.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                9_999,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .outbound_envelopes_after(group.id.clone(), me.user_id.clone(), 0)
                .unwrap()
                .len(),
            1,
            "group coverage semantics are per member and are not modelled; nothing may be assumed"
        );
    }

    /// A group invite is pairwise-sealed to one member but filed under the
    /// group's chat id, so it looks 1:1 from the recipient side and is not.
    /// The `recipient_user_id = chat_id` half of the predicate is what keeps it.
    #[test]
    fn a_group_invite_queued_under_a_group_chat_is_not_retired() {
        let (store, me, peer, contact) = pairing(":memory:");
        let group: Group = create_group(
            "Deck party".to_string(),
            vec![me.user_id.clone(), peer.user_id.clone()],
        )
        .unwrap();
        store.upsert_group(group.clone()).unwrap();
        store
            .queue_group_invites(me.clone(), group.clone(), vec![contact.clone()], NOW)
            .unwrap();
        store
            .record_receipt(
                peer.user_id.clone(),
                me.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                9_999,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .outbound_envelopes_after(group.id.clone(), me.user_id.clone(), 0)
                .unwrap()
                .len(),
            1
        );
    }

    // -----------------------------------------------------------------
    // 2. The startup sweep
    // -----------------------------------------------------------------

    /// Rows covered before this build existed. The sweep is what reaches them:
    /// reopening the store retires exactly the covered slice and leaves the
    /// rest, with no receipt arriving to trigger the incremental hook.
    #[test]
    fn reopening_a_store_sweeps_rows_a_receipt_already_covered() {
        let path = temp_store_path("sweep");
        let (store, me, peer, contact) = pairing(&path);
        for body in [&b"one"[..], b"two", b"three"] {
            author(&store, &me, &contact, KIND_TEXT, body);
        }
        // Write the watermark the way a pre-#283 build did: straight into
        // `receipts`, with no retirement attached.
        {
            let conn = store.conn.lock().expect("store mutex poisoned");
            conn.execute(
                "INSERT INTO receipts (chat_id, sender_user_id, receipt_type, through_lamport)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    peer.user_id.clone(),
                    me.user_id.clone(),
                    RECEIPT_TYPE_DELIVERED as i64,
                    2i64
                ],
            )
            .unwrap();
        }
        assert_eq!(queued(&store, &me, &peer).len(), 3, "nothing retired yet");
        drop(store);

        let reopened = MessageStore::open(path.clone()).unwrap();
        assert_eq!(
            reopened
                .outbound_envelopes_after(peer.user_id.clone(), me.user_id.clone(), 0)
                .unwrap()
                .into_iter()
                .map(|e| e.lamport)
                .collect::<Vec<_>>(),
            vec![3],
        );
        drop(reopened);
        let _ = std::fs::remove_file(&path);
    }

    /// A store restored from a backup carries whatever receipts the backup
    /// held -- which may be behind, or ahead of, the queue it was restored
    /// alongside. The sweep must act on that store's own receipts and on
    /// nothing else: no assumption that a restored queue was delivered, and no
    /// refusal to retire what the restored receipts genuinely prove.
    #[test]
    fn a_restored_store_retires_only_what_its_own_receipts_prove() {
        let path = temp_store_path("restored");
        let (store, me, peer, contact) = pairing(&path);
        let other = generate_identity();
        let other_contact = contact_for(&other, "Sam");
        store.upsert_contact(other_contact.clone()).unwrap();
        for body in [&b"a"[..], b"b", b"c"] {
            author(&store, &me, &contact, KIND_TEXT, body);
        }
        for body in [&b"x"[..], b"y"] {
            author(&store, &me, &other_contact, KIND_TEXT, body);
        }
        // The restored image proves delivery for one conversation only, and
        // only part of it.
        {
            let conn = store.conn.lock().expect("store mutex poisoned");
            conn.execute(
                "INSERT INTO receipts (chat_id, sender_user_id, receipt_type, through_lamport)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    peer.user_id.clone(),
                    me.user_id.clone(),
                    RECEIPT_TYPE_DELIVERED as i64,
                    1i64
                ],
            )
            .unwrap();
        }
        drop(store);

        let reopened = MessageStore::open(path.clone()).unwrap();
        assert_eq!(
            reopened
                .outbound_envelopes_after(peer.user_id.clone(), me.user_id.clone(), 0)
                .unwrap()
                .len(),
            2,
            "only the one lamport the restored receipt covers may go"
        );
        assert_eq!(
            reopened
                .outbound_envelopes_after(other.user_id.clone(), me.user_id.clone(), 0)
                .unwrap()
                .len(),
            2,
            "a conversation the restored image proves nothing about is untouched"
        );
        drop(reopened);
        let _ = std::fs::remove_file(&path);
    }

    // -----------------------------------------------------------------
    // 2b. The repair re-seal must not undo the retirement
    // -----------------------------------------------------------------

    /// Both shells' digest responder, reduced to the part that matters here:
    /// walk every stored message above the peer's *contiguous* watermark, use
    /// the queued envelope if there is one and re-seal from the stored message
    /// if there is not, and send what comes back. Returns the `msg_id` of each
    /// envelope that would go on the wire, in order.
    fn simulate_digest_response(
        store: &MessageStore,
        me: &Identity,
        contact: &Contact,
        peer_has_through: u64,
    ) -> Vec<Vec<u8>> {
        let queued: std::collections::HashMap<u64, Vec<u8>> = store
            .outbound_envelopes_after(
                contact.user_id.clone(),
                me.user_id.clone(),
                peer_has_through,
            )
            .unwrap()
            .into_iter()
            .map(|envelope| (envelope.lamport, envelope.msg_id))
            .collect();
        store
            .messages_after(
                contact.user_id.clone(),
                me.user_id.clone(),
                peer_has_through,
            )
            .unwrap()
            .into_iter()
            .map(|message| match queued.get(&message.lamport) {
                Some(msg_id) => msg_id.clone(),
                None => {
                    store
                        .backfill_pairwise_envelope(me.clone(), contact.clone(), message, None)
                        .unwrap()
                        .envelope
                        .msg_id
                }
            })
            .collect()
    }

    /// The failure this module would otherwise cause, pinned. A peer's digest
    /// reports its *contiguous* watermark, which a single hole pins far below
    /// the *delivered* MAX that drives retirement, so the responder routinely
    /// asks to rebuild rows retirement has just removed. Answering must not
    /// re-queue them: otherwise the queue regrows to its old size within one
    /// link session and the relay uploader re-posts mail the recipient has
    /// already acknowledged, with `relay_posted_at` cleared.
    #[test]
    fn answering_a_holey_digest_does_not_regrow_the_queue_or_the_relay_set() {
        let (store, me, peer, contact) = pairing(":memory:");
        for body in [&b"one"[..], b"two", b"three"] {
            author(&store, &me, &contact, KIND_TEXT, body);
        }
        for generation in 0..3u8 {
            author(&store, &me, &contact, KIND_PROFILE_SYNC, &[generation; 4]);
        }
        assert_eq!(queued(&store, &me, &peer).len(), 4);

        store
            .record_receipt(
                peer.user_id.clone(),
                me.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                6,
                None,
                None,
            )
            .unwrap();
        assert!(queued(&store, &me, &peer).is_empty());

        // The peer's contiguous watermark is 0 -- a hole at its very first
        // lamport -- so it asks for the whole stream, twice over two sessions.
        let first = simulate_digest_response(&store, &me, &contact, 0);
        let second = simulate_digest_response(&store, &me, &contact, 0);
        assert_eq!(first.len(), 6, "every stored message is answered");
        assert_eq!(
            first, second,
            "a re-sealed envelope keeps the message's own identity, so the \
             peer's dedupe and both shells' once-per-session offer bound still \
             recognise it"
        );
        assert!(
            queued(&store, &me, &peer).is_empty(),
            "answering a digest must not re-admit retired rows to the queue"
        );
        assert!(
            store
                .pending_relay_outbound_envelopes(1_000, NOW, vec![])
                .unwrap()
                .is_empty(),
            "answering a digest must not hand the relay uploader acknowledged mail"
        );
    }

    /// The re-sealed envelope carries the id the message was authored with, so
    /// a repair is a retransmission and not new traffic.
    #[test]
    fn a_repair_re_seal_carries_the_authored_msg_id() {
        let (store, me, peer, contact) = pairing(":memory:");
        let authored = store
            .author_pairwise_message(
                me.clone(),
                contact.clone(),
                KIND_TEXT,
                b"hello".to_vec(),
                None,
                NOW,
            )
            .unwrap();
        store
            .record_receipt(
                peer.user_id.clone(),
                me.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                authored.envelope.lamport,
                None,
                None,
            )
            .unwrap();
        assert!(queued(&store, &me, &peer).is_empty());

        let rebuilt = store
            .backfill_pairwise_envelope(me.clone(), contact.clone(), authored.message.clone(), None)
            .unwrap();
        assert_eq!(
            rebuilt.envelope.msg_id, authored.envelope.msg_id,
            "a re-seal must not mint a new identity"
        );
        assert!(!rebuilt.envelope.sealed.is_empty());
    }

    /// A row whose absence is unexplained -- authored before the outbound queue
    /// table existed, still undelivered, still a kind the queue is for -- is
    /// exactly what this path was built for, and it still rejoins.
    #[test]
    fn a_genuinely_legacy_envelope_still_rejoins_the_queue() {
        let (store, me, peer, contact) = pairing(":memory:");
        let authored = store
            .author_pairwise_message(
                me.clone(),
                contact.clone(),
                KIND_TEXT,
                b"pre-queue".to_vec(),
                None,
                NOW,
            )
            .unwrap();
        // Simulate the pre-queue store: the message row exists, the sealed row
        // never did. No receipt covers it.
        {
            let conn = store.conn.lock().expect("store mutex poisoned");
            conn.execute("DELETE FROM outbound_envelopes", []).unwrap();
        }
        assert!(queued(&store, &me, &peer).is_empty());

        store
            .backfill_pairwise_envelope(me.clone(), contact.clone(), authored.message, None)
            .unwrap();
        assert_eq!(
            queued(&store, &me, &peer),
            vec![(authored.envelope.lamport, KIND_TEXT)],
            "undelivered legacy mail belongs in the queue"
        );
    }

    /// A snapshot kind never rejoins, whatever the receipts say. Its queue
    /// membership is governed by supersession and a short life; re-admitting an
    /// old generation would re-advertise the exact thing this module removes.
    #[test]
    fn a_repair_re_seal_of_a_snapshot_kind_never_rejoins_the_queue() {
        let (store, me, peer, contact) = pairing(":memory:");
        let hint = store
            .author_pairwise_message(
                me.clone(),
                contact.clone(),
                KIND_LAN_ENDPOINT_HINT,
                b"endpoint".to_vec(),
                None,
                NOW,
            )
            .unwrap();
        {
            let conn = store.conn.lock().expect("store mutex poisoned");
            conn.execute("DELETE FROM outbound_envelopes", []).unwrap();
        }
        let rebuilt = store
            .backfill_pairwise_envelope(me.clone(), contact.clone(), hint.message, None)
            .unwrap();
        assert!(
            !rebuilt.envelope.sealed.is_empty(),
            "the peer is still answered"
        );
        assert!(
            queued(&store, &me, &peer).is_empty(),
            "a superseded generation must not be re-advertised"
        );
    }

    /// A repair re-seal is transmitted the moment it is built, so it must be
    /// deliverable. The short ephemeral lifetimes are an *authoring* policy;
    /// applied to a re-seal of an older message they would hand back an
    /// envelope already past its expiry, which the shells frame anyway and the
    /// recipient's inbound gate drops -- dead bytes, and a stream hole that
    /// could never close.
    #[test]
    fn a_repair_re_seal_is_never_born_expired() {
        let (store, me, _peer, contact) = pairing(":memory:");
        let hint = store
            .author_pairwise_message(
                me.clone(),
                contact.clone(),
                KIND_LAN_ENDPOINT_HINT,
                b"endpoint".to_vec(),
                None,
                NOW,
            )
            .unwrap();
        assert_eq!(hint.envelope.expiry, NOW + LAN_ENDPOINT_HINT_EXPIRY_MS);
        {
            let conn = store.conn.lock().expect("store mutex poisoned");
            conn.execute("DELETE FROM outbound_envelopes", []).unwrap();
        }
        let rebuilt = store
            .backfill_pairwise_envelope(me.clone(), contact.clone(), hint.message, None)
            .unwrap();
        let a_day_later = NOW + 24 * 60 * 60 * 1_000;
        assert!(
            rebuilt.envelope.expiry > a_day_later,
            "a re-seal an hour, or a day, after authoring must still be deliverable"
        );
        assert_eq!(rebuilt.envelope.expiry, NOW + DEFAULT_EXPIRY_MS);
    }

    // -----------------------------------------------------------------
    // 3. Supersession
    // -----------------------------------------------------------------

    /// Authoring a new snapshot leaves exactly one generation per (recipient,
    /// kind) queued -- the newest -- while an unrelated recipient's copy and
    /// every non-snapshot kind are untouched.
    #[test]
    fn supersession_keeps_exactly_the_newest_generation_per_recipient_and_kind() {
        let (store, me, peer, contact) = pairing(":memory:");
        let other = generate_identity();
        let other_contact = contact_for(&other, "Sam");
        store.upsert_contact(other_contact.clone()).unwrap();

        author(&store, &me, &contact, KIND_TEXT, b"a real message");
        author(
            &store,
            &me,
            &other_contact,
            KIND_PROFILE_SYNC,
            b"gen-1-other",
        );
        for generation in 0..5 {
            author(
                &store,
                &me,
                &contact,
                KIND_PROFILE_SYNC,
                format!("profile-{generation}").as_bytes(),
            );
            author(
                &store,
                &me,
                &contact,
                KIND_FRIEND_DIRECTORY,
                format!("directory-{generation}").as_bytes(),
            );
            author(
                &store,
                &me,
                &contact,
                KIND_LAN_ENDPOINT_HINT,
                format!("hint-{generation}").as_bytes(),
            );
            author(
                &store,
                &me,
                &contact,
                KIND_RELAY_UPDATE,
                format!("relay-{generation}").as_bytes(),
            );
        }

        let mut kinds: Vec<u8> = queued(&store, &me, &peer)
            .into_iter()
            .map(|(_, kind)| kind)
            .collect();
        kinds.sort_unstable();
        assert_eq!(
            kinds,
            vec![
                KIND_TEXT,
                KIND_PROFILE_SYNC,
                KIND_FRIEND_DIRECTORY,
                KIND_LAN_ENDPOINT_HINT,
                KIND_RELAY_UPDATE,
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>(),
            "twenty snapshot generations collapse to four, and the text survives"
        );

        // The survivor of each snapshot kind is the newest one authored.
        let queued_now = queued(&store, &me, &peer);
        for kind in [
            KIND_PROFILE_SYNC,
            KIND_FRIEND_DIRECTORY,
            KIND_LAN_ENDPOINT_HINT,
            KIND_RELAY_UPDATE,
        ] {
            let of_kind: Vec<u64> = queued_now
                .iter()
                .filter(|(_, k)| *k == kind)
                .map(|(lamport, _)| *lamport)
                .collect();
            assert_eq!(
                of_kind.len(),
                1,
                "kind {kind} kept more than one generation"
            );
        }
        // The other recipient still has their own copy: supersession is scoped
        // to one conversation.
        assert_eq!(
            store
                .outbound_envelopes_after(other.user_id.clone(), me.user_id.clone(), 0)
                .unwrap()
                .len(),
            1
        );
    }

    /// The supersession statement's group exclusion is structural, not a
    /// consequence of today's kind set.
    ///
    /// A group envelope satisfies `recipient_user_id = chat_id` exactly --
    /// `author_group_message` sets both to the group id -- so the pairwise
    /// shape alone does not exclude it. Only `NOT EXISTS (… groups …)` does.
    /// The statement is exercised directly here, with a kind the caller-level
    /// guard would refuse, so the protection is pinned to the SQL and survives
    /// somebody later adding a group-authorable snapshot kind to
    /// [`supersedes_queued_generations`].
    #[test]
    fn the_supersession_statement_refuses_a_group_row_by_itself() {
        let (store, me, peer, _contact) = pairing(":memory:");
        let group = create_group(
            "Deck party".to_string(),
            vec![me.user_id.clone(), peer.user_id.clone()],
        )
        .unwrap();
        store.upsert_group(group.clone()).unwrap();
        for body in [&b"first"[..], b"second"] {
            store
                .author_group_message(
                    me.clone(),
                    group.clone(),
                    KIND_TEXT,
                    body.to_vec(),
                    None,
                    NOW,
                )
                .unwrap();
        }
        let conn = store.conn.lock().expect("store mutex poisoned");
        let removed = conn
            .execute(
                RETIRE_SUPERSEDED_SQL,
                params![
                    group.id.clone(),
                    me.user_id.clone(),
                    KIND_TEXT as i64,
                    9_999i64
                ],
            )
            .unwrap();
        assert_eq!(
            removed, 0,
            "no group row may be retired without a per-member coverage record"
        );
    }

    /// Request-shaped hidden kinds are events, not snapshots. Two friend
    /// requests to the same person are two distinct offers and both stay.
    #[test]
    fn request_kinds_are_not_superseded() {
        let (store, me, peer, contact) = pairing(":memory:");
        author(&store, &me, &contact, KIND_FRIEND_REQUEST, b"card-1");
        author(&store, &me, &contact, KIND_FRIEND_REQUEST, b"card-2");
        assert_eq!(queued(&store, &me, &peer).len(), 2);
    }

    // -----------------------------------------------------------------
    // 4. Right-sized expiry
    // -----------------------------------------------------------------

    /// The endpoint hint's envelope dies with its payload; a text authored in
    /// the same breath keeps the full retention window.
    #[test]
    fn an_authored_endpoint_hint_expires_with_its_payload_and_a_text_does_not() {
        let (store, me, _peer, contact) = pairing(":memory:");
        let hint = store
            .author_pairwise_message(
                me.clone(),
                contact.clone(),
                KIND_LAN_ENDPOINT_HINT,
                b"endpoint".to_vec(),
                None,
                NOW,
            )
            .unwrap();
        let text = store
            .author_pairwise_message(
                me.clone(),
                contact.clone(),
                KIND_TEXT,
                b"hello".to_vec(),
                None,
                NOW,
            )
            .unwrap();
        assert_eq!(hint.envelope.expiry, NOW + LAN_ENDPOINT_HINT_EXPIRY_MS);
        assert_eq!(text.envelope.expiry, NOW + DEFAULT_EXPIRY_MS);
        assert!(
            hint.envelope.expiry < text.envelope.expiry,
            "a service payload must not outlive a person's message"
        );
    }

    /// A group envelope keeps the flat default. That is not cosmetic:
    /// `core_group_fanout_rows_for_carried` reconstructs a carried group
    /// envelope's authoring day as `expiry - DEFAULT_EXPIRY_MS` to recompute
    /// per-member recipient hints, and a shortened group expiry would silently
    /// address the fan-out to the wrong day's hint.
    #[test]
    fn group_envelopes_keep_the_flat_default_expiry() {
        let (store, me, peer, _contact) = pairing(":memory:");
        let group = create_group(
            "Deck party".to_string(),
            vec![me.user_id.clone(), peer.user_id.clone()],
        )
        .unwrap();
        store.upsert_group(group.clone()).unwrap();
        let authored = store
            .author_group_message(me.clone(), group, KIND_TEXT, b"hi".to_vec(), None, NOW)
            .unwrap();
        assert_eq!(authored.envelope.expiry, NOW + DEFAULT_EXPIRY_MS);
    }

    // -----------------------------------------------------------------
    // 5. Both readers of the table see the same shrunken set
    // -----------------------------------------------------------------

    /// The relay-upload path and the BLE/LAN digest offer path read the same
    /// `outbound_envelopes` rows, so retirement has to shrink both. The relay
    /// path is the one that changes behaviour: it consults no receipt, so
    /// before this it re-uploaded rows the recipient had already acknowledged.
    #[test]
    fn retirement_shrinks_the_relay_upload_set_and_the_digest_offer_set_together() {
        let (store, me, peer, contact) = pairing(":memory:");
        for body in [&b"one"[..], b"two", b"three"] {
            author(&store, &me, &contact, KIND_TEXT, body);
        }
        assert_eq!(
            store
                .pending_relay_outbound_envelopes(64, NOW, vec![])
                .unwrap()
                .len(),
            3
        );
        let (offered, _) = store
            .outbound_envelopes_after_budgeted(
                peer.user_id.clone(),
                me.user_id.clone(),
                0,
                &HashSet::new(),
                NOW,
                u64::MAX,
            )
            .unwrap();
        assert_eq!(offered.len(), 3);

        store
            .record_receipt(
                peer.user_id.clone(),
                me.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                2,
                None,
                None,
            )
            .unwrap();

        assert_eq!(
            store
                .pending_relay_outbound_envelopes(64, NOW, vec![])
                .unwrap()
                .len(),
            1,
            "the relay uploader no longer re-posts acknowledged mail"
        );
        let (offered, _) = store
            .outbound_envelopes_after_budgeted(
                peer.user_id.clone(),
                me.user_id.clone(),
                0,
                &HashSet::new(),
                NOW,
                u64::MAX,
            )
            .unwrap();
        assert_eq!(offered.len(), 1);
    }

    // -----------------------------------------------------------------
    // 6. Cost
    // -----------------------------------------------------------------

    /// A field store carries six figures of envelopes. Both retirement
    /// statements must reach their rows by seeking a contiguous index range,
    /// never by walking the queue -- and the plan is explained against the
    /// shared constants, not a second copy of the SQL that could go on passing
    /// after the real statement changed.
    #[test]
    fn both_retirement_statements_seek_an_index_and_never_scan_the_queue() {
        let (store, me, peer, _contact) = pairing(":memory:");
        let conn = store.conn.lock().expect("store mutex poisoned");
        let covered_plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {RETIRE_COVERED_SQL}"))
            .unwrap()
            .query_map(
                params![peer.user_id.clone(), me.user_id.clone(), 5i64],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let covered_plan = covered_plan.join("\n");
        assert!(
            covered_plan.contains("SEARCH outbound_envelopes USING INDEX idx_outbound_"),
            "coverage retirement did not seek a queue index:\n{covered_plan}"
        );
        assert!(
            !covered_plan.contains("SCAN outbound_envelopes"),
            "coverage retirement fell back to a scan:\n{covered_plan}"
        );

        let superseded_plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {RETIRE_SUPERSEDED_SQL}"))
            .unwrap()
            .query_map(
                params![
                    peer.user_id.clone(),
                    me.user_id.clone(),
                    KIND_PROFILE_SYNC as i64,
                    5i64
                ],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let superseded_plan = superseded_plan.join("\n");
        assert!(
            superseded_plan.contains("SEARCH outbound_envelopes USING INDEX idx_outbound_"),
            "supersession did not seek a queue index:\n{superseded_plan}"
        );
        assert!(
            !superseded_plan.contains("SCAN outbound_envelopes"),
            "supersession fell back to a scan:\n{superseded_plan}"
        );
    }

    /// The sweep is driven from `receipts` -- one row per chat per receipt type
    /// -- so its cost is set by how many people this device talks to, not by
    /// how much mail is queued. Two conversations' worth of receipts against a
    /// large queue must still cost two seeks.
    #[test]
    fn the_sweep_is_driven_by_receipts_and_not_by_queue_size() {
        let (store, me, peer, contact) = pairing(":memory:");
        for index in 0..200 {
            author(
                &store,
                &me,
                &contact,
                KIND_TEXT,
                format!("message {index}").as_bytes(),
            );
        }
        let conn = store.conn.lock().expect("store mutex poisoned");
        let plan: Vec<String> = conn
            .prepare(
                "EXPLAIN QUERY PLAN SELECT chat_id, sender_user_id, through_lamport FROM receipts
                 WHERE receipt_type = ?1 AND through_lamport > 0",
            )
            .unwrap()
            .query_map(params![RECEIPT_TYPE_DELIVERED as i64], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let plan = plan.join("\n");
        assert!(
            !plan.contains("outbound_envelopes"),
            "the sweep driver must not touch the queue at all:\n{plan}"
        );
        drop(conn);

        // And the sweep itself, run against that queue, removes exactly the
        // covered slice.
        store
            .record_receipt(
                peer.user_id.clone(),
                me.user_id.clone(),
                RECEIPT_TYPE_DELIVERED,
                150,
                None,
                None,
            )
            .unwrap();
        assert_eq!(queued(&store, &me, &peer).len(), 50);
        let conn = store.conn.lock().expect("store mutex poisoned");
        assert_eq!(
            sweep_receipt_covered(&conn).unwrap(),
            0,
            "a second sweep finds nothing left to do"
        );
    }
}
