//! The inbound transaction, owned once, in core (Plan §3.2, package D0).
//!
//! Every `0x02` envelope that arrives — over a live BLE/LAN link, or fetched
//! FROM the relay mailbox — runs the *same* disposition here:
//!
//! 1. parse the §6.4 public header and enforce the frame/envelope limits;
//! 2. dedupe on `msg_id` and gate on expiry (never past-expiry, never a hop or
//!    expiry a local client would refuse — [`crate::core_inbound_gate`]);
//! 3. open for self ([`crate::open_message`]) or for an eligible group with the
//!    signer∈members membership guard ([`crate::open_group_message`]); anything
//!    else is foreign traffic to flood and carry;
//! 4. persist carry rows and the consumed-hidden-kind ack evidence through the
//!    store's own short transactions, which have committed before this returns;
//!    and
//! 5. return a bounded [`CoreInboundOutcome`] — at most one delivered payload,
//!    at most one re-flood frame, the disposition the relay ack rule consults,
//!    and work counts — never an unbounded object list.
//!
//! This is the single authority the shells' `InboundEnvelopeProcessor.kt` and
//! `MeshController` will call in D1, and the one `core/tests/mesh_sim.rs` calls
//! today in place of the third copy it used to keep. It performs no I/O: the
//! caller executes the returned re-flood frame and applies the delivered
//! payload (its chat insert, receipts, notifications — the kind-specific
//! delivery that stays native presentation, D1). The carry, hidden-kind ack
//! evidence for a self-consumed *drop*, and receipt rows written here are
//! committed through the store's short transactions before return, so a lost
//! re-flood send afterward can never ack a relay row, delete a carried row, or
//! advance a frontier.
//!
//! The one thing deliberately *not* committed before return is the DTN D4
//! bookkeeping for a payload handed back to deliver: the flood-dedupe `seen`
//! record and the ACK-01 consumed-hidden evidence for a *delivered* pairwise
//! message. Because the durable delivery of that payload is the native
//! caller's (D1's) job and can fail, recording `seen` here would poison the
//! dedupe set on a failed persist (the msg_id would dedupe away forever) and
//! returning an ackable `Consumed` would delete the relay copy that the retry
//! needs. Instead the delivered path returns a [`CoreInboundCommit`] the caller
//! applies only once its delivery succeeds (via
//! [`MessageStore::core_commit_inbound_delivery`]); on a delivery failure the
//! caller drops the token, leaves the msg_id unrecorded, and reports
//! [`CoreInboundDisposition::Failed`] — exactly the production
//! `deliver → record-seen` order (T4-06).
//!
//! **Preserved invariants**, each with an owner in `core/tests/mesh_sim.rs` or
//! this module's tests:
//!
//! - `ACK-01` — only a pairwise-consumed envelope becomes ackable; a carried or
//!   foreign copy never does. The consumed-hidden evidence written here is the
//!   one licence [`crate::MessageStore::core_relay_ack_ids_with_consumed`]
//!   consults for a hidden kind, and it is written only from the pairwise-open
//!   path, exactly as its contract demands.
//! - `ACK-02` — an envelope past its expiry is dropped, never acked; expiry is
//!   the client clock and cannot authorize a delete.
//! - `CARRY-01` / `CARRY-02` — carrying or re-flooding a foreign envelope does
//!   not remove it; removal stays the digest-confirm path's job.
//! - group membership — a group body is delivered only when the signer and this
//!   device are both members; a spoofed-signer group envelope is muled but never
//!   delivered.
//! - blocked sender — an opened pairwise envelope from a blocked identity is
//!   consumed (so its relay copy acks away) but never delivered.
//! - dedupe / expiry / reject gate, and the consumed-hidden set recording.

use std::sync::Arc;

use crate::{
    core_inbound_gate, core_is_own_fanout_hint, decode_extended_message_body,
    encode_envelope_frame, open_group_message, open_message, parse_frame, CarriedEnvelope,
    CoreError, CoreInboundDisposition, CoreInboundGate, Frame, Identity, MessageStore, SeenIds,
};

/// The shared per-envelope foreign carry budget, byte-identical to the shells'
/// `FOREIGN_CARRY_BUDGET_BYTES` (Android `InboundEnvelopeProcessor`, iOS
/// `MeshDefaults`). Family-addressed carry is governed by the store's total
/// budget instead; this bounds only the pure-mule share.
const FOREIGN_CARRY_BUDGET_BYTES: i64 = 5 * 1024 * 1024;

/// Where an inbound envelope came from — the source discriminant relay
/// proxy-polling needs, mirroring the shells' `sourceAddress == null` test.
///
/// [`CoreInboundSource::Mesh`] is a live BLE or authenticated same-LAN frame;
/// [`CoreInboundSource::Relay`] is a row fetched from the relay mailbox, which
/// is already durable server-side, so a carried copy of it is never re-uploaded
/// and a per-member fan-out copy addressed to our own hint is neither carried
/// nor re-flooded (`specs/group-relay-durability.md` §4.3 no-reinjection).
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreInboundSource {
    Mesh,
    Relay,
}

/// Bounded work counts for one processed frame, so a caller can fold
/// receive-path progress into an encounter- or page-granularity protocol event
/// without this hot per-envelope path writing the ring itself (contract §7.1
/// forbids per-envelope ring writes).
#[derive(uniffi::Record, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreInboundWork {
    pub delivered: u32,
    pub carried: u32,
    pub reflooded: u32,
    /// Opened and consumed by this endpoint, but deliberately not delivered
    /// (blocked sender, or a group body whose signer is not a member).
    pub dropped: u32,
    pub deduped: u32,
    pub expired: u32,
    pub rejected: u32,
}

/// The DTN D4 bookkeeping a caller must commit *after* it has durably applied
/// the delivered payload this outcome hands back — the flood-dedupe `seen`
/// record and the ACK-01 consumed-hidden evidence, folded across the FFI
/// boundary so neither is written until the native delivery succeeds.
///
/// It is present in [`CoreInboundOutcome::commit`] exactly when a payload was
/// handed back to deliver (a 1:1 message we opened, or a group message for a
/// member). The caller runs the production `deliver → commit` order:
/// deliver the payload durably, then call
/// [`MessageStore::core_commit_inbound_delivery`] with this token; if that
/// delivery instead fails, drop this token unused and report
/// [`CoreInboundDisposition::Failed`] — the `msg_id` then stays unrecorded and
/// re-presentable, and the relay copy is never acked (T4-06 / DTN D4). This is
/// why the delivered path never records `seen` itself: only the caller knows
/// whether the durable delivery it owns actually landed.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreInboundCommit {
    /// The envelope id to mark seen once delivery succeeds.
    pub msg_id: Vec<u8>,
    /// The opened body's kind, when it decoded, so the commit can record the
    /// ACK-01 consumed-hidden evidence for a hidden kind. `None` for group
    /// deliveries (recording hidden evidence is a pairwise-only licence) and
    /// for bodies that do not decode.
    pub hidden_kind: Option<u8>,
    /// The envelope fields [`MessageStore::core_record_consumed_hidden_msg_id`]
    /// re-checks before it will write evidence. Carried verbatim so the commit
    /// stays a single call with no ambient state.
    pub recipient_hint: Vec<u8>,
    pub expiry: i64,
    pub own_user_id: Vec<u8>,
    pub now_ms: i64,
}

/// The result of running one inbound envelope through [`MessageStore::process_inbound_frame`].
///
/// Every list here is bounded: at most one delivered payload (a frame is
/// addressed to at most one 1:1 recipient or one group we belong to) and at
/// most one re-flood frame. Nothing unbounded crosses the FFI boundary.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreInboundOutcome {
    /// The relay ack rule's input: see [`crate::core_should_ack_inbound`] and
    /// [`crate::MessageStore::core_relay_ack_ids_with_consumed`].
    pub disposition: CoreInboundDisposition,
    /// The opened plaintext to deliver — one entry when this device is the 1:1
    /// recipient or a group member and the sender is neither blocked nor a
    /// non-member, empty otherwise. The caller decodes and applies it.
    pub delivered_payloads: Vec<Vec<u8>>,
    /// The verified sender of the delivered payload, for the caller's kind
    /// dispatch and notifications. `None` when nothing was delivered.
    pub delivered_sender: Option<Vec<u8>>,
    /// The §6.4 frame to flood onward, hop-decremented, or `None` when no hops
    /// remain or the frame is home / an own fan-out copy. The caller sends it;
    /// a failed send cannot undo any store mutation above.
    pub relay_frame: Option<Vec<u8>>,
    /// Whether a carried row was newly enqueued this call.
    pub carried: bool,
    /// Whether an opened pairwise envelope was dropped because its sender is
    /// blocked — consumed for ack purposes, never delivered.
    pub dropped_blocked: bool,
    /// Present exactly when [`Self::delivered_payloads`] is non-empty: the DTN
    /// D4 bookkeeping the caller commits after it durably delivers the payload.
    /// See [`CoreInboundCommit`]. `None` for every path that has no native
    /// delivery to wait on (the store mutations of those paths — carry, hidden
    /// evidence for a self-consumed drop — are already committed before return).
    pub commit: Option<CoreInboundCommit>,
    pub work: CoreInboundWork,
}

impl CoreInboundOutcome {
    fn terminal(disposition: CoreInboundDisposition, work: CoreInboundWork) -> Self {
        CoreInboundOutcome {
            disposition,
            delivered_payloads: Vec::new(),
            delivered_sender: None,
            relay_frame: None,
            carried: false,
            dropped_blocked: false,
            commit: None,
            work,
        }
    }
}

#[uniffi::export]
impl MessageStore {
    /// Run one inbound `0x02` envelope through the production disposition and
    /// return the bounded work to execute. See the module docs for the ordered
    /// steps and the invariants preserved.
    ///
    /// `seen` is the process-wide flood-dedupe set (DESIGN.md §5.3). It is read
    /// with the non-mutating [`SeenIds::contains`] and recorded only once the
    /// envelope reaches a terminal handled state (DTN D4): an envelope whose
    /// durable carry failed stays re-presentable, one that was handled — even
    /// by deliberate drop — is deduped. The one exception is a payload handed
    /// back to *deliver*: its `seen` record is deferred to the caller's
    /// post-delivery [`Self::core_commit_inbound_delivery`], because only the
    /// caller knows whether its durable delivery landed (see the module docs).
    pub fn process_inbound_frame(
        &self,
        identity: Identity,
        seen: Arc<SeenIds>,
        source: CoreInboundSource,
        frame: Vec<u8>,
        now_ms: i64,
    ) -> Result<CoreInboundOutcome, CoreError> {
        let mut work = CoreInboundWork::default();

        // 1. Parse + frame/envelope limits. A non-envelope frame (HELLO,
        // digest, LAN endpoint, probe) is routed elsewhere by the shells and
        // never reaches this path; treat an unexpected one as a terminal
        // non-ackable reject with no side effect and, having no msg_id, no
        // seen record.
        let Ok(Frame::Envelope {
            msg_id,
            hop_ttl,
            expiry,
            recipient_hint,
            sealed,
        }) = parse_frame(frame)
        else {
            work.rejected = 1;
            return Ok(CoreInboundOutcome::terminal(
                CoreInboundDisposition::Rejected,
                work,
            ));
        };

        // 2. Dedupe + expiry + public-header resource gate.
        match core_inbound_gate(!seen.contains(msg_id.clone()), hop_ttl, expiry, now_ms) {
            CoreInboundGate::Seen => {
                // Already handled by a prior copy; nothing to record.
                work.deduped = 1;
                return Ok(CoreInboundOutcome::terminal(
                    CoreInboundDisposition::Seen,
                    work,
                ));
            }
            CoreInboundGate::Expired => {
                seen.record(msg_id);
                work.expired = 1;
                return Ok(CoreInboundOutcome::terminal(
                    CoreInboundDisposition::Expired,
                    work,
                ));
            }
            CoreInboundGate::Rejected => {
                seen.record(msg_id);
                work.rejected = 1;
                return Ok(CoreInboundOutcome::terminal(
                    CoreInboundDisposition::Rejected,
                    work,
                ));
            }
            CoreInboundGate::Dispatch => {}
        }

        // 3a. Pairwise open: a sealed box opens only for its one true X25519
        // recipient (§6.3), so a successful open means this device is the
        // endpoint. Deliver locally and never re-flood — it is home.
        if let Ok(opened) = open_message(identity.clone(), sealed.clone()) {
            // Blocked-sender inbound gate: a blocked identity is dropped before
            // delivery — no chat row, no receipt — but the envelope is still
            // consumed (we are the sole endpoint; a deliberate discard is
            // consumption), so its relay copy acks away instead of refetching
            // forever. A drop has no native delivery to wait on, so it is
            // terminal here: record `seen` now and carry no commit.
            if self.is_user_blocked(opened.sender_user_id.clone())? {
                seen.record(msg_id);
                work.dropped = 1;
                return Ok(CoreInboundOutcome {
                    disposition: CoreInboundDisposition::Consumed,
                    delivered_payloads: Vec::new(),
                    delivered_sender: None,
                    relay_frame: None,
                    carried: false,
                    dropped_blocked: true,
                    commit: None,
                    work,
                });
            }

            // A deliverable 1:1 message. Hand the payload back for the caller's
            // native delivery and defer the DTN D4 bookkeeping to its
            // post-delivery commit: `seen` (so a failed persist does not poison
            // the dedupe set) and the ACK-01 hidden-kind evidence (the one
            // licence a later relay copy of a hidden kind needs to ack away —
            // valid only from this pairwise-open path, and only once the kind
            // was actually consumed). The kind is captured for the commit;
            // sim/raw payloads that are not content frames simply do not decode,
            // so no evidence is ever recorded for them. `Consumed` is the
            // disposition the caller reports *iff* its delivery succeeds; on a
            // failure it drops the commit and reports `Failed` instead.
            let hidden_kind = decode_extended_message_body(opened.payload.clone())
                .ok()
                .map(|body| body.kind);
            work.delivered = 1;
            return Ok(CoreInboundOutcome {
                disposition: CoreInboundDisposition::Consumed,
                delivered_payloads: vec![opened.payload],
                delivered_sender: Some(opened.sender_user_id),
                relay_frame: None,
                carried: false,
                dropped_blocked: false,
                commit: Some(CoreInboundCommit {
                    msg_id,
                    hidden_kind,
                    recipient_hint,
                    expiry,
                    own_user_id: identity.user_id.clone(),
                    now_ms,
                }),
                work,
            });
        }

        // 3b. Group open: a pairwise open failed, so try any imported group
        // whose recent-day hint matches — plus every group when the hint is our
        // own, which is how a per-member relay fan-out copy is recognised. The
        // group key opens membership-agnostically; the membership guard lives
        // here at delivery so a spoofed-signer envelope is not delivered, yet is
        // still muled like the foreign traffic it effectively is (keeping the
        // reject out of the crypto layer, which would otherwise re-flood it as
        // an unrecognised foreign frame anyway).
        let candidates =
            self.group_open_candidates(recipient_hint.clone(), identity.user_id.clone(), now_ms)?;
        for group in candidates {
            let Ok(opened) = open_group_message(group.clone(), sealed.clone()) else {
                continue;
            };

            let signer_is_member = group
                .member_user_ids
                .iter()
                .any(|member| member == &opened.sender_user_id);
            let we_are_member = group
                .member_user_ids
                .iter()
                .any(|member| member == &identity.user_id);
            let deliver = signer_is_member && we_are_member;

            // Carry + reflood run whatever the membership verdict — a member's
            // body is muled on for absent members, a spoofed-signer body is
            // muled like the foreign traffic it effectively is. These are
            // core's durable store mutations and commit before return.
            //
            // A relay-fetched fan-out copy addressed to our own hint is already
            // durable for every member on the relay; re-flooding or carrying it
            // would give the same content a second flood identity under the
            // fan-out msg_id (§4.3). Legacy group-hint rows and every mesh frame
            // keep the flood + carry behaviour.
            let own_fanout = source == CoreInboundSource::Relay
                && core_is_own_fanout_hint(
                    recipient_hint.clone(),
                    identity.user_id.clone(),
                    now_ms,
                );

            let (relay_frame, carried) = if own_fanout {
                (None, false)
            } else {
                let relay_frame = reflood_frame(&msg_id, hop_ttl, expiry, &recipient_hint, &sealed);
                if relay_frame.is_some() {
                    work.reflooded = 1;
                }
                let carried = self.carry(
                    source,
                    &msg_id,
                    hop_ttl,
                    expiry,
                    &recipient_hint,
                    &sealed,
                    now_ms,
                )?;
                if carried {
                    work.carried = 1;
                }
                (relay_frame, carried)
            };

            if deliver {
                // A deliverable group message: hand the payload back and defer
                // the `seen` record to the caller's post-delivery commit, the
                // same DTN D4 order as the pairwise path (group deliveries
                // record no hidden evidence — that is a pairwise-only licence).
                work.delivered = 1;
                return Ok(CoreInboundOutcome {
                    disposition: CoreInboundDisposition::Consumed,
                    delivered_payloads: vec![opened.payload],
                    delivered_sender: Some(opened.sender_user_id),
                    relay_frame,
                    carried,
                    dropped_blocked: false,
                    commit: Some(CoreInboundCommit {
                        msg_id: msg_id.clone(),
                        hidden_kind: None,
                        recipient_hint: recipient_hint.clone(),
                        expiry,
                        own_user_id: identity.user_id.clone(),
                        now_ms,
                    }),
                    work,
                });
            }

            // Spoofed-signer / non-member: no native delivery to wait on. The
            // envelope is consumed by deliberate drop and muled on, so it is
            // terminal here — record `seen` now (matching production's
            // finishAdmission(CONSUMED, terminal=true) for this case).
            seen.record(msg_id.clone());
            work.dropped = 1;
            return Ok(CoreInboundOutcome {
                disposition: CoreInboundDisposition::Consumed,
                delivered_payloads: Vec::new(),
                delivered_sender: None,
                relay_frame,
                carried,
                dropped_blocked: false,
                commit: None,
                work,
            });
        }

        // 3c. Foreign traffic: not ours to open. Flood it onward while hops
        // remain and carry it so we can hand it to its recipient the next time
        // we meet them. Record the msg_id as seen only once the durable carry
        // actually succeeded (DTN D4) — a failed carry leaves it re-presentable
        // on the next copy instead of poisoning the seen set.
        let relay_frame = reflood_frame(&msg_id, hop_ttl, expiry, &recipient_hint, &sealed);
        if relay_frame.is_some() {
            work.reflooded = 1;
        }
        let carried = self.carry(
            source,
            &msg_id,
            hop_ttl,
            expiry,
            &recipient_hint,
            &sealed,
            now_ms,
        )?;
        if carried {
            work.carried = 1;
        }
        // Carried is a terminal handled state; a store failure returns Err
        // above (leaving the id unrecorded), so reaching here means the carry
        // ran and the id may be recorded.
        seen.record(msg_id);
        Ok(CoreInboundOutcome {
            disposition: CoreInboundDisposition::Carried,
            delivered_payloads: Vec::new(),
            delivered_sender: None,
            relay_frame,
            carried,
            dropped_blocked: false,
            commit: None,
            work,
        })
    }

    /// Commit the DTN D4 bookkeeping for a payload the caller has now durably
    /// delivered: record the ACK-01 consumed-hidden evidence (best-effort — the
    /// store re-checks every safety condition and declines otherwise) and mark
    /// the msg_id seen so the next copy dedupes. MUST be called only after the
    /// native delivery of [`CoreInboundOutcome::delivered_payloads`] succeeded;
    /// on a delivery failure the caller leaves this uncalled, so the msg_id
    /// stays re-presentable and the disposition it reports is
    /// [`CoreInboundDisposition::Failed`] (see the module docs). Takes only the
    /// bounded [`CoreInboundCommit`] the outcome handed back — no ambient state.
    pub fn core_commit_inbound_delivery(&self, seen: Arc<SeenIds>, commit: CoreInboundCommit) {
        if let Some(kind) = commit.hidden_kind {
            // Best-effort, exactly as the pairwise path recorded it before: a
            // missing record costs one relay re-fetch, never a message.
            let _ = self.core_record_consumed_hidden_msg_id(
                commit.msg_id.clone(),
                kind,
                commit.recipient_hint,
                commit.expiry,
                commit.own_user_id,
                commit.now_ms,
            );
        }
        seen.record(commit.msg_id);
    }
}

/// Store mutations kept deliberately OUT of the exported FFI surface: internal
/// helpers the inbound transaction composes. Leaving them off the
/// `#[uniffi::export]` block above keeps D1's frozen binding baseline from
/// gaining a raw `carry` method a shell could call to enqueue a carried row
/// outside the single-authority disposition (A0 clean-surface discipline).
impl MessageStore {
    /// Enqueue a foreign/group envelope into the carry queue for later delivery.
    ///
    /// The stored `hop_ttl` is one less than the header's: carrying is itself a
    /// hop, so a mule delivery must count it exactly as the flood path counts
    /// its own re-relays. A relay-sourced row is stored `from_relay` so it is
    /// never re-uploaded (it is already on the relay); a mesh-sourced row is
    /// force-family here (the group/fan-out path) or classified by the store.
    #[allow(clippy::too_many_arguments)]
    fn carry(
        &self,
        source: CoreInboundSource,
        msg_id: &[u8],
        hop_ttl: u8,
        expiry: i64,
        recipient_hint: &[u8],
        sealed: &[u8],
        now_ms: i64,
    ) -> Result<bool, CoreError> {
        let carried = CarriedEnvelope {
            msg_id: msg_id.to_vec(),
            hop_ttl: hop_ttl.saturating_sub(1),
            expiry,
            recipient_hint: recipient_hint.to_vec(),
            sealed: sealed.to_vec(),
        };
        match source {
            CoreInboundSource::Relay => self.enqueue_relay_carried_envelope(carried, now_ms),
            CoreInboundSource::Mesh => {
                let is_family = self.hint_matches_known_target(recipient_hint.to_vec(), now_ms)?;
                self.enqueue_carried_envelope(
                    carried,
                    is_family,
                    now_ms,
                    FOREIGN_CARRY_BUDGET_BYTES,
                )
            }
        }
    }
}

/// The hop-decremented §6.4 frame to flood onward, or `None` when the hop
/// budget is exhausted (the local device is the final carrier).
fn reflood_frame(
    msg_id: &[u8],
    hop_ttl: u8,
    expiry: i64,
    recipient_hint: &[u8],
    sealed: &[u8],
) -> Option<Vec<u8>> {
    if hop_ttl > 1 {
        Some(encode_envelope_frame(
            msg_id.to_vec(),
            hop_ttl - 1,
            expiry,
            recipient_hint.to_vec(),
            sealed.to_vec(),
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    //! Executable owners for the inbound invariants the multi-node
    //! `core/tests/mesh_sim.rs` does not reach: the blocked-sender gate, the
    //! group signer∈members guard, hidden-kind ack evidence (`ACK-01`), the
    //! expiry drop (`ACK-02`), foreign carry never being ackable
    //! (`ACK-01`/`CARRY-01`), and duplicate dedupe — all through the one
    //! production entry point.

    use std::sync::Arc;

    use crate::{
        compute_recipient_hint, core_should_ack_inbound, encode_envelope_frame,
        encode_message_body, generate_identity, generate_msg_id, seal_group_message, seal_message,
        CoreInboundDisposition, CoreInboundSource, CoreRelayEnvelopeDisposition, Group,
        MessageBody, MessageStore, SeenIds, DEFAULT_HOP_TTL, KIND_PROFILE_SYNC, MS_PER_DAY,
    };

    const NOW: i64 = 1_700_000_000_000;

    fn store() -> MessageStore {
        MessageStore::open(":memory:".to_string()).expect("open in-memory store")
    }

    fn seen() -> Arc<SeenIds> {
        Arc::new(SeenIds::new())
    }

    fn expiry() -> i64 {
        NOW + 7 * MS_PER_DAY
    }

    #[test]
    fn a_blocked_pairwise_sender_is_consumed_but_never_delivered() {
        // ACK-01 companion: a blocked identity's envelope is opened (we are the
        // sole endpoint), so it acks away as consumed rather than refetching
        // forever — but its body is dropped before any delivery.
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        store.block_user(sender.user_id.clone(), NOW).unwrap();

        let sealed = seal_message(sender, me.agree_pk.clone(), b"let me back in".to_vec()).unwrap();
        let frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            expiry(),
            compute_recipient_hint(me.user_id.clone(), NOW),
            sealed,
        );

        let outcome = store
            .process_inbound_frame(me, seen(), CoreInboundSource::Mesh, frame, NOW)
            .unwrap();

        assert_eq!(outcome.disposition, CoreInboundDisposition::Consumed);
        assert!(outcome.dropped_blocked);
        assert!(outcome.delivered_payloads.is_empty());
        assert!(!outcome.carried);
        assert!(
            core_should_ack_inbound(outcome.disposition),
            "a blocked sender's relay copy still acks away — we were its endpoint"
        );
    }

    #[test]
    fn a_consumed_hidden_kind_records_the_evidence_that_lets_its_relay_copy_ack() {
        // ACK-01: a hidden kind leaves no messages row, so the only thing that
        // can ever ack its relay copy is the consumed-hidden evidence this path
        // writes. Prove a later SEEN presentation of the same id becomes ackable.
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        let own_hint = compute_recipient_hint(me.user_id.clone(), NOW);

        let payload = encode_message_body(MessageBody {
            kind: KIND_PROFILE_SYNC,
            chat_id: sender.user_id.clone(),
            lamport: 1,
            timestamp: NOW,
            content: b"profile".to_vec(),
        })
        .unwrap();
        let sealed = seal_message(sender, me.agree_pk.clone(), payload).unwrap();
        let msg_id = generate_msg_id();
        let frame = encode_envelope_frame(
            msg_id.clone(),
            DEFAULT_HOP_TTL,
            expiry(),
            own_hint.clone(),
            sealed,
        );

        let commit_seen = seen();
        let outcome = store
            .process_inbound_frame(
                me.clone(),
                commit_seen.clone(),
                CoreInboundSource::Relay,
                frame,
                NOW,
            )
            .unwrap();
        assert_eq!(outcome.disposition, CoreInboundDisposition::Consumed);
        assert_eq!(outcome.delivered_payloads.len(), 1);

        // DTN D4: the hidden-kind evidence is deferred to the post-delivery
        // commit, so it is written only once the caller has durably delivered
        // the payload it was handed. Run that commit now.
        let commit = outcome
            .commit
            .expect("a delivered payload carries a commit token");
        store.core_commit_inbound_delivery(commit_seen, commit);

        // Before the record, a SEEN copy of a hidden kind is not ackable; the
        // evidence the commit wrote is what turns this into an ack.
        let ack = store
            .core_relay_ack_ids_with_consumed(
                vec![CoreRelayEnvelopeDisposition {
                    relay_id: 42,
                    msg_id,
                    disposition: CoreInboundDisposition::Seen,
                    recipient_hint: own_hint,
                }],
                me.user_id.clone(),
                NOW,
            )
            .unwrap();
        assert_eq!(
            ack,
            vec![42],
            "the consumed-hidden evidence recorded through the entry point makes the copy ackable"
        );
    }

    #[test]
    fn a_group_body_from_a_non_member_signer_is_muled_but_never_delivered() {
        // Group membership guard: a spoofer holding the group key but absent
        // from the member list must not have its body delivered, though the
        // envelope is still muled like the foreign traffic it is.
        let store = store();
        let me = generate_identity();
        let friend = generate_identity();
        let spoofer = generate_identity();
        let group = Group {
            id: vec![0x33; 16],
            name: "Family".to_string(),
            member_user_ids: vec![me.user_id.clone(), friend.user_id.clone()],
            key: vec![0x44; 32],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        };
        store.upsert_group(group.clone()).unwrap();
        let group_hint = compute_recipient_hint(group.id.clone(), NOW);

        let forged = seal_group_message(spoofer, group.clone(), b"forged".to_vec()).unwrap();
        let forged_frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            expiry(),
            group_hint.clone(),
            forged,
        );
        let outcome = store
            .process_inbound_frame(
                me.clone(),
                seen(),
                CoreInboundSource::Mesh,
                forged_frame,
                NOW,
            )
            .unwrap();
        assert_eq!(outcome.disposition, CoreInboundDisposition::Consumed);
        assert!(
            outcome.delivered_payloads.is_empty(),
            "a spoofed-signer group body is never delivered"
        );
        assert!(outcome.carried, "but the envelope is still muled onward");

        // Positive control: a real member's message is delivered.
        let real = seal_group_message(friend, group.clone(), b"real".to_vec()).unwrap();
        let real_frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            expiry(),
            group_hint,
            real,
        );
        let outcome = store
            .process_inbound_frame(me, seen(), CoreInboundSource::Mesh, real_frame, NOW)
            .unwrap();
        assert_eq!(outcome.delivered_payloads, vec![b"real".to_vec()]);
    }

    #[test]
    fn an_expired_envelope_is_dropped_and_never_ackable() {
        // ACK-02: expiry is the client clock; it drops the frame and never
        // authorizes an ack.
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        let sealed = seal_message(sender, me.agree_pk.clone(), b"too late".to_vec()).unwrap();
        let frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            NOW - 1,
            compute_recipient_hint(me.user_id.clone(), NOW),
            sealed,
        );

        let outcome = store
            .process_inbound_frame(me, seen(), CoreInboundSource::Mesh, frame, NOW)
            .unwrap();

        assert_eq!(outcome.disposition, CoreInboundDisposition::Expired);
        assert!(outcome.delivered_payloads.is_empty());
        assert!(!outcome.carried);
        assert!(!core_should_ack_inbound(outcome.disposition));
    }

    #[test]
    fn a_foreign_relay_row_is_carried_and_never_ackable() {
        // ACK-01 / CARRY-01: a relay-fetched envelope for someone else is
        // carried for its real recipient and must stay unacked in the mailbox.
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        let stranger = generate_identity();
        let sealed =
            seal_message(sender, stranger.agree_pk.clone(), b"not yours".to_vec()).unwrap();
        let frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            expiry(),
            compute_recipient_hint(stranger.user_id.clone(), NOW),
            sealed,
        );

        let outcome = store
            .process_inbound_frame(me, seen(), CoreInboundSource::Relay, frame, NOW)
            .unwrap();

        assert_eq!(outcome.disposition, CoreInboundDisposition::Carried);
        assert!(outcome.carried);
        assert!(outcome.delivered_payloads.is_empty());
        assert!(
            !core_should_ack_inbound(outcome.disposition),
            "carrying a message for its real recipient never acks the relay copy"
        );
    }

    #[test]
    fn a_duplicate_frame_is_deduped_and_delivered_only_once() {
        // The shared seen-set gates the second copy without re-delivering.
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        let sealed = seal_message(sender, me.agree_pk.clone(), b"only once".to_vec()).unwrap();
        let frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            expiry(),
            compute_recipient_hint(me.user_id.clone(), NOW),
            sealed,
        );
        let shared = seen();

        let first = store
            .process_inbound_frame(
                me.clone(),
                shared.clone(),
                CoreInboundSource::Mesh,
                frame.clone(),
                NOW,
            )
            .unwrap();
        assert_eq!(first.disposition, CoreInboundDisposition::Consumed);
        assert_eq!(first.delivered_payloads.len(), 1);
        // DTN D4: the first copy's `seen` record lands on the post-delivery
        // commit, and it is exactly that record that must dedupe the second.
        store.core_commit_inbound_delivery(
            shared.clone(),
            first
                .commit
                .expect("a delivered payload carries a commit token"),
        );

        let second = store
            .process_inbound_frame(me, shared, CoreInboundSource::Mesh, frame, NOW)
            .unwrap();
        assert_eq!(second.disposition, CoreInboundDisposition::Seen);
        assert!(second.delivered_payloads.is_empty());
    }

    #[test]
    fn an_uncommitted_delivery_stays_re_presentable_and_is_never_deduped() {
        // DTN D4 / T4-06: the whole reason the delivered path defers `seen` to
        // the caller's post-delivery commit is that a durable delivery FAILURE
        // must leave the envelope re-presentable — never poisoned into the
        // dedupe set and never acked. Model a failed native delivery by simply
        // NOT calling `core_commit_inbound_delivery`, then prove the very next
        // copy re-dispatches (delivers again) instead of being dropped as Seen.
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        let sealed = seal_message(sender, me.agree_pk.clone(), b"retry me".to_vec()).unwrap();
        let frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            expiry(),
            compute_recipient_hint(me.user_id.clone(), NOW),
            sealed,
        );
        let shared = seen();

        // First copy opens and is handed back to deliver, but the caller's
        // durable delivery "fails", so it drops the commit and reports Failed.
        let first = store
            .process_inbound_frame(
                me.clone(),
                shared.clone(),
                CoreInboundSource::Mesh,
                frame.clone(),
                NOW,
            )
            .unwrap();
        assert_eq!(first.disposition, CoreInboundDisposition::Consumed);
        assert_eq!(first.delivered_payloads.len(), 1);
        assert!(first.commit.is_some());
        // Deliberately do NOT commit — this stands in for a failed persist.

        // The next copy must re-dispatch, not dedupe: the msg_id was never
        // recorded, so it opens and is delivered again for the retry.
        let second = store
            .process_inbound_frame(me, shared, CoreInboundSource::Mesh, frame, NOW)
            .unwrap();
        assert_eq!(
            second.disposition,
            CoreInboundDisposition::Consumed,
            "an uncommitted (failed) delivery must not poison the seen set"
        );
        assert_eq!(second.delivered_payloads.len(), 1);
    }
}
