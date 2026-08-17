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
    core_decode_sync_record, core_inbound_gate, core_is_own_fanout_hint,
    core_pairwise_sender_authorized, core_sync_record_admit, core_sync_record_kind_wire,
    decode_extended_message_body, decode_group_invite_content, decode_lan_endpoint_content,
    decode_profile_sync_content, decode_receipt_content, decode_relay_update_content,
    encode_envelope_frame, friend_card_user_id, open_group_message, open_message, parse_frame,
    parse_friend_request_content, verify_shared_friend_card, CarriedEnvelope, Contact,
    ContactDiscoveryPolicy, CoreError, CoreInboundDisposition, CoreInboundGate,
    ExtendedMessageBody, Frame, FriendCard, Identity, IncomingMessageInsertOutcome,
    LanEndpointContent, MessageArrival, MessageStore, PendingSharedRequest, SeenIds,
    SharedFriendCard, StoredMessage, SyncDigest, DEVICE_HARD_CAP, KIND_FRIEND_REQUEST,
    KIND_GROUP_INVITE, KIND_LAN_ENDPOINT_HINT, KIND_PROFILE_SYNC, KIND_RECEIPT, KIND_RELAY_UPDATE,
    LEGACY_DEVICE_ID, RECEIPT_TYPE_DELIVERED,
};

/// What the pairwise-open path found when it asked whether an opened payload
/// was one of §8's sync records.
///
/// Deliberately not exported: a shell never sees this distinction, because
/// every branch of it is finished inside core by the time
/// [`MessageStore::process_inbound_frame`] returns.
enum SyncConsumption {
    /// An ordinary message body (or a payload that is not a content frame at
    /// all). Falls through to the delivery path unchanged.
    NotASyncRecord,
    /// A sync record this person's roster refuses — a revoked author, a
    /// superseded inbox generation, a foreign person, a bad signature.
    /// Consumed by deliberate discard, never applied, and never vouched for.
    Refused,
    /// Admitted and applied, with the sealed-body kind for the ACK-MD-1
    /// evidence and, for a digest, the sibling's watermarks to answer.
    Applied {
        kind: u8,
        peer_digest: Option<SyncDigest>,
    },
}

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
    /// The group this envelope was opened for, when it was a group delivery,
    /// and `None` for a pairwise one. It is both the pairwise discriminant the
    /// delivery fold uses (never the sender-chosen `chat_id`) and the id a
    /// group body's `chat_id` must equal under `DELIVER-01`.
    pub group_id: Option<Vec<u8>>,
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
    /// Which lane the delivered payload belongs to: `Some(group_id)` when it
    /// was opened with a group key and both membership checks passed, `None`
    /// for a pairwise delivery (and when nothing was delivered). Stated here
    /// rather than left for a shell to re-derive from the body's `chat_id`,
    /// because picking the delivery lane is a disposition decision and a
    /// driver that guessed it could route a body to the wrong handler.
    pub delivered_group_id: Option<Vec<u8>>,
    /// The §6.4 frame to flood onward, hop-decremented, or `None` when no hops
    /// remain or the frame is home / an own fan-out copy. The caller sends it;
    /// a failed send cannot undo any store mutation above.
    pub relay_frame: Option<Vec<u8>>,
    /// Whether a carried row was newly enqueued this call.
    pub carried: bool,
    /// Whether that carried row was classified *family* — addressed to a
    /// recipient this device knows — which is the one carry class a shell may
    /// act on beyond storing it, by nudging a relay upload so an internet
    /// phone can proxy it onward. Always false for a relay-sourced row (it is
    /// already on the relay and is never re-uploaded) and for every path that
    /// carried nothing. The classification itself stays here: a driver that
    /// re-derived it from the recipient hint would be inferring carry policy.
    pub carried_family: bool,
    /// Whether an opened pairwise envelope was dropped because its sender is
    /// blocked — consumed for ack purposes, never delivered.
    pub dropped_blocked: bool,
    /// A sibling's SYNC-1 watermarks, present exactly when this envelope
    /// carried a [`crate::SyncRecordKind::Digest`] record that admitted (§8).
    ///
    /// The rest of a sync record's handling finishes inside core — the payload
    /// is applied to the store before this returns, and nothing is handed back
    /// to deliver. A digest is the exception because it is not state to apply
    /// but a *question*: it says what a sibling holds, and the answer is a
    /// backfill round only the driver can send. So the one sync outcome a shell
    /// acts on is this one, and it acts on it by planning a round
    /// ([`crate::core_sync_digest_gaps`] →
    /// [`MessageStore::core_sync_backfill_records`] →
    /// [`crate::core_plan_sync_backfill`]), never by writing anything.
    pub sync_peer_digest: Option<SyncDigest>,
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
            delivered_group_id: None,
            relay_frame: None,
            carried: false,
            carried_family: false,
            dropped_blocked: false,
            sync_peer_digest: None,
            commit: None,
            work,
        }
    }
}

/// The one piece of delivery input that is genuinely the shell's: this
/// device's own friends-of-friends discovery switch, which lives in platform
/// settings rather than the message store. Passed in explicitly so the fold
/// below reads no ambient state (determinism, plan §3.1).
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreDiscoveryPolicyState {
    pub enabled: bool,
    pub revision: u64,
}

/// What the delivery fold did with one opened body.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreDeliveryVerdict {
    /// The body validated and every store effect it implies is committed.
    Applied,
    /// `DELIVER-01`: a pairwise body whose `chat_id` names a thread other than
    /// its verified sender's own. Terminal — consumed, never applied.
    DroppedForeignChat,
    /// The verified pairwise sender is not authorized to dispatch this kind
    /// ([`crate::core_pairwise_sender_authorized`]). Terminal, not a failure:
    /// retrying cannot make an already-authored envelope more authorized.
    DroppedUnauthorizedSender,
    /// The body — or the per-kind content inside it — could not be decoded, or
    /// failed a body-shape check the sender controls (a receipt that does not
    /// acknowledge this device, a group invite whose membership omits its own
    /// sender). Terminal, not a failure: the bytes are fixed and signed, so the
    /// same decode fails identically forever. Delivering it again could only
    /// re-fail, which is why it is consumed rather than left to refetch.
    ///
    /// This matches what the shipping Android delivery path already did with an
    /// undecodable body: log it and report the envelope consumed. Only the
    /// *hidden-kind ack evidence* is withheld when the top-level body would not
    /// decode, and that falls out on its own — core fills
    /// [`CoreInboundCommit::hidden_kind`] from a successful decode and leaves it
    /// `None` otherwise, so an unreadable body can never vouch for a hidden kind.
    DroppedMalformed,
}

/// How a failure inside the delivery fold should be reported.
///
/// The distinction is the whole point: a *deterministic* failure is a property
/// of bytes that are already signed and will never change, so retrying it is a
/// livelock — a relay copy that can never ack away refetches and re-fails
/// forever. A *durability* failure is a property of this device right now (disk
/// full, a quarantined stream conflict), so the envelope must stay
/// re-presentable and unacked (DTN D4 / T4-06).
enum DeliveryFailure {
    /// Report as a terminal [`CoreDeliveryVerdict::DroppedMalformed`] on the
    /// named kind (`None` when the body would not decode far enough to know).
    Malformed(Option<u8>),
    /// Report as `Err` — the caller must not commit.
    Durability(CoreError),
}

/// Store and crypto errors reach the fold through `?` and are durability
/// failures by default; a deterministic body failure is opted in explicitly
/// with [`DeliveryFailure::Malformed`], never by accident.
impl From<CoreError> for DeliveryFailure {
    fn from(err: CoreError) -> Self {
        DeliveryFailure::Durability(err)
    }
}

/// Split a kind handler's error where the two classes are genuinely mixed:
/// friend-request onboarding both parses sender-supplied bytes and writes a
/// contact row. Everything that describes the *bytes* is terminal; only a store
/// failure is worth retrying.
fn classify_body_error(err: CoreError, kind: u8) -> DeliveryFailure {
    match err {
        CoreError::Store(_) => DeliveryFailure::Durability(err),
        _ => DeliveryFailure::Malformed(Some(kind)),
    }
}

/// The single delivery effect core cannot perform itself: recording a
/// contact's advertised LAN endpoint in the shell's own endpoint cache (a
/// file/preferences store outside SQLite). Present only for an applied kind-8
/// hint, and it is *this contact's own* endpoint by construction — the
/// `ENDPOINT-01` privacy rule is unchanged, because the fold hands back a hint
/// keyed to its verified sender and nothing else.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CoreLanEndpointIntent {
    pub peer_user_id: Vec<u8>,
    pub endpoint: LanEndpointContent,
    pub observed_at_ms: i64,
}

/// The bounded typed result of [`MessageStore::core_deliver_inbound`] — the
/// per-kind delivery decisions folded into core, with only the driver work
/// core cannot do handed back.
///
/// A driver executes what is in here and infers nothing: it must not re-read
/// `kind` to decide on further store work, because every store effect for
/// every kind has already been committed when this returns.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CoreInboundDelivery {
    pub verdict: CoreDeliveryVerdict,
    /// The decoded body's kind, for the driver's presentation surface
    /// (notification routing) — never for another policy branch.
    pub kind: u8,
    /// Whether a `messages` row was written, so a shell with a chat surface
    /// knows there is something new to show. Hidden kinds and drops are false.
    pub persisted: bool,
    /// See [`CoreLanEndpointIntent`]. `None` for every other kind.
    pub endpoint_hint: Option<CoreLanEndpointIntent>,
}

impl CoreInboundDelivery {
    fn dropped(verdict: CoreDeliveryVerdict, kind: u8) -> Self {
        CoreInboundDelivery {
            verdict,
            kind,
            persisted: false,
            endpoint_hint: None,
        }
    }
}

#[uniffi::export]
impl MessageStore {
    /// Apply one opened, verified inbound body — the per-kind half of the
    /// inbound transaction, owned once, in core.
    ///
    /// [`Self::process_inbound_frame`] decides *whether* an envelope is ours;
    /// this decides *what its body means*. Callers run them in that order and
    /// then, only if this returned `Ok`, run
    /// [`Self::core_commit_inbound_delivery`] with the same commit token —
    /// the production `deliver → commit` order (DTN D4 / T4-06). An `Err` here
    /// is a *durability* failure and only that: the caller drops the token
    /// unused, leaves the `msg_id` re-presentable and reports
    /// [`CoreInboundDisposition::Failed`], because a retry on a healthier disk
    /// can genuinely succeed.
    ///
    /// A rejection the bytes themselves earn is not an error. It comes back as
    /// a terminal [`CoreDeliveryVerdict::DroppedForeignChat`],
    /// [`CoreDeliveryVerdict::DroppedUnauthorizedSender`] or
    /// [`CoreDeliveryVerdict::DroppedMalformed`], which the caller commits like
    /// any other consumption so the relay copy acks away instead of refetching
    /// and re-failing forever. Retrying a signed body that will not decode
    /// cannot make it decode.
    ///
    /// The two gates it applies before any kind runs:
    ///
    /// - `DELIVER-01` — a pairwise body is written only into its verified
    ///   sender's own thread, and a group body only into the group whose key
    ///   opened it. `chat_id` is attacker-chosen data inside a signed body;
    ///   opening the seal proves *who* wrote it, never *where* they may
    ///   write. Without this, any accepted contact or group member could
    ///   author rows into a thread they are not part of.
    /// - [`crate::core_pairwise_sender_authorized`] — the shared
    ///   sender/kind predicate all three shells already call.
    ///
    /// Whether the body is pairwise is read from
    /// [`CoreInboundCommit::group_id`], which core fills in only for a group
    /// delivery. It is deliberately never inferred from `chat_id`, which the
    /// sender controls.
    pub fn core_deliver_inbound(
        &self,
        identity: Identity,
        sender_user_id: Vec<u8>,
        payload: Vec<u8>,
        commit: CoreInboundCommit,
        arrival: MessageArrival,
        discovery: CoreDiscoveryPolicyState,
    ) -> Result<CoreInboundDelivery, CoreError> {
        match self.deliver_inbound_body(
            identity,
            sender_user_id,
            payload,
            commit,
            arrival,
            discovery,
        ) {
            Ok(delivery) => Ok(delivery),
            Err(DeliveryFailure::Malformed(kind)) => Ok(CoreInboundDelivery::dropped(
                CoreDeliveryVerdict::DroppedMalformed,
                kind.unwrap_or(0),
            )),
            Err(DeliveryFailure::Durability(err)) => Err(err),
        }
    }

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
                    delivered_group_id: None,
                    relay_frame: None,
                    carried: false,
                    carried_family: false,
                    dropped_blocked: true,
                    sync_peer_digest: None,
                    commit: None,
                    work,
                });
            }

            // §8 self-sync, before the message-body decode. A pairwise open
            // against this device's own key is exactly the condition §6's
            // person inbox key describes in v1 (every linked device holds the
            // person identity, so the identity secret *is* the inbox secret),
            // so a sibling's sync record surfaces here and nowhere else.
            //
            // It is dispatched in core rather than handed to the shells for the
            // same reason the rest of this transaction is: a sync record is
            // opened, admitted against the own roster and applied to the store
            // in one place, or it is three places that will disagree about
            // SYNC-3. Nothing is handed back to deliver — the store write has
            // already committed when this returns — so this is terminal here,
            // exactly like the blocked-sender drop above.
            match self.consume_sync_record(&opened.sender_user_id, &opened.payload, now_ms)? {
                SyncConsumption::NotASyncRecord => {}
                SyncConsumption::Refused => {
                    // Opened for us and deliberately discarded: consumed, so
                    // the relay copy acks away rather than refetching a record
                    // that will fail the same roster gate forever. No hidden
                    // evidence: nothing of it was applied, and the licence to
                    // delete a relay row is only ever written for what this
                    // device actually took.
                    seen.record(msg_id);
                    work.dropped = 1;
                    return Ok(CoreInboundOutcome {
                        disposition: CoreInboundDisposition::Consumed,
                        delivered_payloads: Vec::new(),
                        delivered_sender: None,
                        delivered_group_id: None,
                        relay_frame: None,
                        carried: false,
                        carried_family: false,
                        dropped_blocked: false,
                        sync_peer_digest: None,
                        commit: None,
                        work,
                    });
                }
                SyncConsumption::Applied { kind, peer_digest } => {
                    // ACK-MD-1's evidence, written here rather than deferred to
                    // a commit token, because there is no native delivery to
                    // wait on: the apply above is the durable delivery, and it
                    // has committed. This is what makes
                    // `core_kind_persists_msg_id_row`'s claim about the sync
                    // kinds true — they leave no `messages` row of their own,
                    // so the consumed-hidden set is the only thing that can
                    // later vouch for having taken this device's fan-out copy.
                    // Best-effort exactly as every other hidden kind's is: the
                    // store re-checks every safety condition and declines
                    // otherwise, and a missing record costs one relay refetch.
                    let _ = self.core_record_consumed_hidden_msg_id(
                        msg_id.clone(),
                        kind,
                        recipient_hint,
                        expiry,
                        identity.user_id.clone(),
                        now_ms,
                    );
                    seen.record(msg_id);
                    work.delivered = 1;
                    return Ok(CoreInboundOutcome {
                        disposition: CoreInboundDisposition::Consumed,
                        delivered_payloads: Vec::new(),
                        delivered_sender: None,
                        delivered_group_id: None,
                        relay_frame: None,
                        carried: false,
                        carried_family: false,
                        dropped_blocked: false,
                        sync_peer_digest: peer_digest,
                        commit: None,
                        work,
                    });
                }
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
                delivered_group_id: None,
                relay_frame: None,
                carried: false,
                carried_family: false,
                dropped_blocked: false,
                sync_peer_digest: None,
                commit: Some(CoreInboundCommit {
                    msg_id,
                    group_id: None,
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

            let (relay_frame, carry) = if own_fanout {
                (None, CarryOutcome::default())
            } else {
                let relay_frame = reflood_frame(&msg_id, hop_ttl, expiry, &recipient_hint, &sealed);
                if relay_frame.is_some() {
                    work.reflooded = 1;
                }
                let carry = self.carry(
                    source,
                    &msg_id,
                    hop_ttl,
                    expiry,
                    &recipient_hint,
                    &sealed,
                    now_ms,
                )?;
                if carry.stored {
                    work.carried = 1;
                }
                (relay_frame, carry)
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
                    delivered_group_id: Some(group.id.clone()),
                    relay_frame,
                    carried: carry.stored,
                    carried_family: carry.family,
                    dropped_blocked: false,
                    sync_peer_digest: None,
                    commit: Some(CoreInboundCommit {
                        msg_id: msg_id.clone(),
                        group_id: Some(group.id.clone()),
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
                delivered_group_id: None,
                relay_frame,
                carried: carry.stored,
                carried_family: carry.family,
                dropped_blocked: false,
                sync_peer_digest: None,
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
        let carry = self.carry(
            source,
            &msg_id,
            hop_ttl,
            expiry,
            &recipient_hint,
            &sealed,
            now_ms,
        )?;
        if carry.stored {
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
            delivered_group_id: None,
            relay_frame,
            carried: carry.stored,
            carried_family: carry.family,
            dropped_blocked: false,
            sync_peer_digest: None,
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
    /// What the pairwise-open path found when it asked "is this a §8 sync
    /// record?".
    ///
    /// `outer_signer` is the verified sender the pairwise open already
    /// established — the id the envelope's own signature derives, which is a
    /// *device* id for a record a linked device sealed and the person id for
    /// one an un-linked install sealed (§14.2 keeps the root off every phone,
    /// so the first case is the normal one). It is passed rather than
    /// re-derived so this function cannot disagree with the open that produced
    /// it.
    fn consume_sync_record(
        &self,
        outer_signer: &[u8],
        payload: &[u8],
        now_ms: i64,
    ) -> Result<SyncConsumption, CoreError> {
        // No own roster, no admission. An install that has never linked a
        // device has nothing to check a record against and no sibling that
        // could have sent one, so the dispatch is inert there rather than
        // guessing — which is also what keeps this path free on the v1 phones
        // that are the overwhelming majority of the fleet.
        let context = {
            let conn = self.locked_conn();
            crate::sync_store::own_sync_context(&conn)?
        };
        let Some(context) = context else {
            return Ok(SyncConsumption::NotASyncRecord);
        };
        // Strict: a version byte, a named sync kind, exact framing and no
        // trailing bytes. An ordinary message body fails all of them, so
        // "decodes" is the discriminant and there is no ambiguous middle.
        let Ok(record) = core_decode_sync_record(payload.to_vec()) else {
            return Ok(SyncConsumption::NotASyncRecord);
        };
        let kind = core_sync_record_kind_wire(record.kind);
        // The two SYNC-3 gates `core_open_sync_record` applies, in the same
        // order, against the roster this device actually holds. The decryption
        // half already happened — `open_message` above is the same open.
        if !crate::sync_record::outer_signer_is_own(outer_signer, &context.roster) {
            return Ok(SyncConsumption::Refused);
        }
        if core_sync_record_admit(
            record.clone(),
            context.inbox_key_generation,
            context.roster.clone(),
        )
        .is_some()
        {
            return Ok(SyncConsumption::Refused);
        }
        let applied = self.core_apply_sync_record(record, now_ms)?;
        Ok(SyncConsumption::Applied {
            kind,
            peer_digest: applied.peer_digest,
        })
    }

    /// The delivery fold itself. See [`Self::core_deliver_inbound`] for the
    /// contract; the only difference is the richer error type, which lets a
    /// deterministic body failure be told apart from a durability one.
    fn deliver_inbound_body(
        &self,
        identity: Identity,
        sender_user_id: Vec<u8>,
        payload: Vec<u8>,
        commit: CoreInboundCommit,
        arrival: MessageArrival,
        discovery: CoreDiscoveryPolicyState,
    ) -> Result<CoreInboundDelivery, DeliveryFailure> {
        let body =
            decode_extended_message_body(payload).map_err(|_| DeliveryFailure::Malformed(None))?;
        let pairwise = commit.group_id.is_none();
        let now_ms = arrival.received_at;

        // DELIVER-01, both halves. A pairwise body may only be written into
        // its verified sender's own thread; a group body may only be written
        // into the group whose key actually opened it. Either way the wire
        // `chat_id` is checked against something core established, never
        // trusted as the destination it names.
        match &commit.group_id {
            Some(group_id) if &body.chat_id != group_id => {
                return Ok(CoreInboundDelivery::dropped(
                    CoreDeliveryVerdict::DroppedForeignChat,
                    body.kind,
                ));
            }
            _ => {}
        }

        if pairwise {
            if body.chat_id != sender_user_id {
                return Ok(CoreInboundDelivery::dropped(
                    CoreDeliveryVerdict::DroppedForeignChat,
                    body.kind,
                ));
            }
            let sender_is_contact = self.get_contact(sender_user_id.clone())?.is_some();
            if !core_pairwise_sender_authorized(
                body.kind,
                sender_is_contact,
                sender_user_id == identity.user_id,
            ) {
                return Ok(CoreInboundDelivery::dropped(
                    CoreDeliveryVerdict::DroppedUnauthorizedSender,
                    body.kind,
                ));
            }
        }

        let mut delivery = CoreInboundDelivery {
            verdict: CoreDeliveryVerdict::Applied,
            kind: body.kind,
            persisted: false,
            endpoint_hint: None,
        };

        match body.kind {
            KIND_FRIEND_REQUEST if pairwise => {
                self.apply_friend_request(
                    &identity,
                    &sender_user_id,
                    &body.content,
                    discovery,
                    now_ms,
                )
                .map_err(|err| classify_body_error(err, body.kind))?;
                self.persist_inbound(&sender_user_id, &body, &commit, arrival)?;
                delivery.persisted = true;
            }
            KIND_RECEIPT if pairwise => {
                let receipt = decode_receipt_content(body.content.clone())
                    .map_err(|_| DeliveryFailure::Malformed(Some(body.kind)))?;
                if receipt.sender_user_id != identity.user_id {
                    // A receipt for someone else's stream: sender-chosen data
                    // that will never become ours. Terminal, not retryable.
                    return Err(DeliveryFailure::Malformed(Some(body.kind)));
                }
                if let Some(group_id) = receipt.group_id {
                    self.record_group_receipt(
                        group_id,
                        identity.user_id.clone(),
                        sender_user_id.clone(),
                        receipt.receipt_type,
                        receipt.lamport,
                        Some(arrival.transport),
                    )?;
                } else {
                    self.record_receipt(
                        sender_user_id.clone(),
                        identity.user_id.clone(),
                        receipt.receipt_type,
                        receipt.lamport,
                        Some(arrival.transport),
                        Some(now_ms),
                    )?;
                }
            }
            KIND_LAN_ENDPOINT_HINT if pairwise => {
                let endpoint = decode_lan_endpoint_content(body.content.clone())
                    .map_err(|_| DeliveryFailure::Malformed(Some(body.kind)))?;
                self.persist_inbound(&sender_user_id, &body, &commit, arrival)?;
                delivery.persisted = true;
                delivery.endpoint_hint = Some(CoreLanEndpointIntent {
                    peer_user_id: sender_user_id.clone(),
                    endpoint,
                    observed_at_ms: now_ms,
                });
            }
            KIND_RELAY_UPDATE if pairwise => {
                let content = decode_relay_update_content(body.content.clone())
                    .map_err(|_| DeliveryFailure::Malformed(Some(body.kind)))?;
                self.persist_inbound(&sender_user_id, &body, &commit, arrival)?;
                delivery.persisted = true;
                // The hidden row stays durable even if the store rejects a
                // mis-scoped or over-privileged credential update.
                let _ = self.apply_contact_relay_update(sender_user_id.clone(), content);
            }
            KIND_PROFILE_SYNC if pairwise => {
                let content = decode_profile_sync_content(body.content.clone())
                    .map_err(|_| DeliveryFailure::Malformed(Some(body.kind)))?;
                self.persist_inbound(&sender_user_id, &body, &commit, arrival)?;
                delivery.persisted = true;
                if let Some(mut contact) = self.get_contact(sender_user_id.clone())? {
                    self.upsert_contact_discovery_policy(ContactDiscoveryPolicy {
                        user_id: sender_user_id.clone(),
                        protocol_version: content.friends_of_friends_version,
                        enabled: content.friends_of_friends_enabled,
                        revision: content.friends_of_friends_revision,
                    })?;
                    self.set_contact_avatar(
                        sender_user_id.clone(),
                        (!content.avatar.is_empty()).then_some(content.avatar),
                        content.avatar_epoch,
                    )?;
                    if contact.name != content.name {
                        contact.name = content.name;
                        self.upsert_contact(contact)?;
                    }
                }
            }
            KIND_GROUP_INVITE if pairwise => {
                let group = decode_group_invite_content(body.content.clone())
                    .map_err(|_| DeliveryFailure::Malformed(Some(body.kind)))?;
                if !group.member_user_ids.contains(&sender_user_id)
                    || !group.member_user_ids.contains(&identity.user_id)
                {
                    // Membership is fixed inside the signed body, so this
                    // invite can never become valid. Terminal, not retryable.
                    return Err(DeliveryFailure::Malformed(Some(body.kind)));
                }
                self.upsert_group(group.clone())?;
                // The invite is filed under the group it creates, not under
                // the 1:1 thread it travelled in. DELIVER-01 has already
                // pinned the wire `chat_id` to the sender, so this rewrite is
                // a core decision over verified data, never sender-chosen.
                let mut stored = body.clone();
                stored.chat_id = group.id;
                self.persist_inbound(&sender_user_id, &stored, &commit, arrival)?;
                delivery.persisted = true;
            }
            _ => {
                self.persist_inbound(&sender_user_id, &body, &commit, arrival)?;
                delivery.persisted = true;
            }
        }

        // Auto-receipt tail: acknowledge a contact's stream up to its highest
        // contiguous point. A receipt never acknowledges a receipt.
        if pairwise && body.kind != KIND_RECEIPT {
            if let Some(contact) = self.get_contact(sender_user_id.clone())? {
                let through = self
                    .highest_contiguous_lamport(sender_user_id.clone(), sender_user_id.clone())?;
                let _ = self.author_receipt(
                    identity,
                    contact,
                    sender_user_id,
                    RECEIPT_TYPE_DELIVERED,
                    through,
                    now_ms,
                )?;
            }
        }

        Ok(delivery)
    }

    /// Write the opened body as a `messages` row, mapping the store's insert
    /// outcome onto the delivery contract: an insert or an idempotent
    /// duplicate is success; a quarantined stream conflict is a durability
    /// failure, so the caller must not commit and the envelope stays
    /// re-presentable.
    ///
    /// §5: the row lands on its author's device stream. The device id comes
    /// from inside the seal, so it is already bound to the verified sender —
    /// and it partitions that sender's own history rather than authorizing
    /// anything, which is why WP1 honours it without a roster lookup. A body
    /// with no device field (every legacy peer, permanently) lands on
    /// `LEGACY_DEVICE_ID`, exactly where every pre-migration row already is.
    /// Refusing a *tombstoned* signer's new events is DL-4's other half and
    /// belongs to WP5, once rosters actually gossip.
    ///
    /// Because the id is honoured without a roster lookup, it is bounded
    /// instead: see [`Self::bounded_device_stream`].
    fn persist_inbound(
        &self,
        sender_user_id: &[u8],
        body: &ExtendedMessageBody,
        commit: &CoreInboundCommit,
        arrival: MessageArrival,
    ) -> Result<(), CoreError> {
        let sender_device_id = self.bounded_device_stream(
            &body.chat_id,
            sender_user_id,
            body.sender_device_id.clone(),
        )?;
        let outcome = self.insert_incoming_message_from_device(
            StoredMessage {
                chat_id: body.chat_id.clone(),
                sender_user_id: sender_user_id.to_vec(),
                lamport: body.lamport,
                timestamp: body.timestamp,
                kind: body.kind,
                payload: body.content.clone(),
                sender_device_id: sender_device_id.clone(),
            },
            Some(sender_device_id),
            commit.msg_id.clone(),
            body.reply_to_msg_id.clone(),
            Some(arrival),
        )?;
        match outcome {
            IncomingMessageInsertOutcome::Inserted | IncomingMessageInsertOutcome::Duplicate => {
                Ok(())
            }
            IncomingMessageInsertOutcome::QuarantinedConflict => Err(CoreError::Store(
                "message stream conflict was quarantined".into(),
            )),
        }
    }

    /// Bound how many device streams one sender can open in one chat (§14.3).
    ///
    /// The sealed-body device id is authenticated as *coming from* the sender,
    /// but nothing yet checks it against that person's roster — WP5 does, once
    /// rosters gossip. Until then a sender who wanted to could stamp a fresh
    /// random id on every message and mint an unbounded number of streams in
    /// this device's store, each one its own conflict namespace and its own
    /// entry in every per-stream read. That is a storage and query cost a peer
    /// should not be able to choose.
    ///
    /// So the count is capped at what §14.3 allows a person to have:
    /// [`DEVICE_HARD_CAP`] device streams, alongside the always-available
    /// legacy stream. A device id that would open the 17th files on
    /// [`LEGACY_DEVICE_ID`] instead — bounded, no message lost, and exactly the
    /// shape everything had before WP1, which is the failure mode already
    /// known to work. An id that already has a stream is always honoured, so a
    /// real 16-device person never degrades.
    fn bounded_device_stream(
        &self,
        chat_id: &[u8],
        sender_user_id: &[u8],
        sender_device_id: Option<Vec<u8>>,
    ) -> Result<Vec<u8>, CoreError> {
        let stream = crate::core_device_stream_id(sender_device_id);
        if stream == LEGACY_DEVICE_ID {
            return Ok(stream);
        }
        let held = self.message_stream_device_ids(chat_id.to_vec(), sender_user_id.to_vec())?;
        if held.contains(&stream) {
            return Ok(stream);
        }
        let device_streams = held
            .iter()
            .filter(|id| id.as_slice() != LEGACY_DEVICE_ID)
            .count();
        if device_streams >= DEVICE_HARD_CAP as usize {
            return Ok(LEGACY_DEVICE_ID.to_vec());
        }
        Ok(stream)
    }

    /// A kind-1 friend request: either a direct card, which onboards its
    /// verified sender, or a friends-of-friends card, which may only ever
    /// raise a prompt.
    fn apply_friend_request(
        &self,
        identity: &Identity,
        sender_user_id: &[u8],
        content: &[u8],
        discovery: CoreDiscoveryPolicyState,
        now_ms: i64,
    ) -> Result<(), CoreError> {
        let json = std::str::from_utf8(content)
            .map_err(|_| CoreError::Malformed("friend request is not UTF-8".into()))?;
        let request = parse_friend_request_content(json.to_string())?;
        if friend_card_user_id(request.card.clone()) != sender_user_id {
            return Err(CoreError::InvalidFriendCard(
                "friend request card does not match its verified sender".into(),
            ));
        }
        if let Some(shared) = request.shared {
            return self.hold_shared_friend_request(
                identity,
                sender_user_id,
                request.card,
                shared,
                discovery,
                now_ms,
            );
        }
        self.upsert_imported_contact(Contact {
            user_id: sender_user_id.to_vec(),
            name: request.card.name,
            sign_pk: request.card.sign_pk,
            agree_pk: request.card.agree_pk,
            relay_url: request.card.relay_url,
            relay_token: request.card.relay_token,
            nickname: None,
        })?;
        Ok(())
    }

    /// An introduced request never creates a contact. Every check that fails
    /// here drops it silently and without a prompt, exactly as
    /// [`crate::verify_shared_friend_card`]'s contract describes.
    fn hold_shared_friend_request(
        &self,
        identity: &Identity,
        sender_user_id: &[u8],
        card: FriendCard,
        shared: SharedFriendCard,
        discovery: CoreDiscoveryPolicyState,
        now_ms: i64,
    ) -> Result<(), CoreError> {
        let Some(sharer) = self.get_contact(shared.sharer_user_id.clone())? else {
            return Ok(());
        };
        if self.is_user_blocked(shared.sharer_user_id.clone())? {
            return Ok(());
        }
        if friend_card_user_id(shared.card.clone()) != identity.user_id {
            return Ok(());
        }
        if !discovery.enabled {
            return Ok(());
        }
        if !verify_shared_friend_card(
            shared.clone(),
            sharer.sign_pk,
            identity.user_id.clone(),
            discovery.revision,
            now_ms,
        )? {
            return Ok(());
        }
        if self
            .get_shared_request_dismissal(sender_user_id.to_vec())?
            .map(|row| row.suppressed)
            .unwrap_or(false)
        {
            return Ok(());
        }
        self.upsert_pending_shared_request(PendingSharedRequest {
            requester_user_id: sender_user_id.to_vec(),
            name: card.name,
            sign_pk: card.sign_pk,
            agree_pk: card.agree_pk,
            relay_url: card.relay_url,
            relay_token: card.relay_token,
            sharer_user_id: shared.sharer_user_id,
            expires_at_ms: shared.expires_at_ms,
            first_seen_ms: now_ms,
            last_prompted_ms: 0,
        })?;
        let _ = self.note_shared_request_prompt(sender_user_id.to_vec(), now_ms);
        Ok(())
    }

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
    ) -> Result<CarryOutcome, CoreError> {
        let carried = CarriedEnvelope {
            msg_id: msg_id.to_vec(),
            hop_ttl: hop_ttl.saturating_sub(1),
            expiry,
            recipient_hint: recipient_hint.to_vec(),
            sealed: sealed.to_vec(),
        };
        match source {
            CoreInboundSource::Relay => Ok(CarryOutcome {
                stored: self.enqueue_relay_carried_envelope(carried, now_ms)?,
                // A relay-sourced row is already durable server-side and is
                // never re-uploaded, so it is never the family class a shell
                // nudges a relay upload for.
                family: false,
            }),
            CoreInboundSource::Mesh => {
                let is_family = self.hint_matches_known_target(recipient_hint.to_vec(), now_ms)?;
                let stored = self.enqueue_carried_envelope(
                    carried,
                    is_family,
                    now_ms,
                    FOREIGN_CARRY_BUDGET_BYTES,
                )?;
                Ok(CarryOutcome {
                    stored,
                    family: stored && is_family,
                })
            }
        }
    }
}

/// What one carry attempt did: whether a row is now enqueued, and whether it
/// was classified family. Internal to this module — the shells see only the
/// two booleans folded into [`CoreInboundOutcome`].
#[derive(Clone, Copy, Debug, Default)]
struct CarryOutcome {
    stored: bool,
    family: bool,
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
        Contact, CoreInboundDisposition, CoreInboundSource, CoreRelayEnvelopeDisposition, Group,
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
        assert!(outcome.delivered_group_id.is_none());

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
        assert_eq!(
            outcome.delivered_group_id,
            Some(group.id),
            "a group delivery names its group so a shell never guesses the lane"
        );
    }

    #[test]
    fn a_mule_copy_for_a_known_contact_is_reported_family() {
        // The one carry class a shell may act on beyond storing it: a family
        // row is offered to the relay so an internet-connected phone can proxy
        // it onward. The classification is stated here rather than re-derived
        // from the recipient hint by each shell.
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        let friend = generate_identity();
        store
            .upsert_imported_contact(Contact {
                user_id: friend.user_id.clone(),
                name: "Friend".to_string(),
                sign_pk: friend.sign_pk.clone(),
                agree_pk: friend.agree_pk.clone(),
                relay_url: None,
                relay_token: None,
                nickname: None,
            })
            .unwrap();

        let sealed = seal_message(
            sender.clone(),
            friend.agree_pk.clone(),
            b"for a friend".to_vec(),
        )
        .unwrap();
        let hint = compute_recipient_hint(friend.user_id.clone(), NOW);
        let frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            expiry(),
            hint.clone(),
            sealed,
        );
        let outcome = store
            .process_inbound_frame(me.clone(), seen(), CoreInboundSource::Mesh, frame, NOW)
            .unwrap();
        assert_eq!(outcome.disposition, CoreInboundDisposition::Carried);
        assert!(outcome.carried);
        assert!(
            outcome.carried_family,
            "it is addressed to a contact we know"
        );

        // The same envelope fetched FROM the relay is already durable there and
        // is never offered back to it.
        let relayed_sealed = seal_message(
            sender,
            friend.agree_pk.clone(),
            b"a second one, from the relay".to_vec(),
        )
        .unwrap();
        let relay_frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            expiry(),
            hint,
            relayed_sealed,
        );
        let outcome = store
            .process_inbound_frame(me, seen(), CoreInboundSource::Relay, relay_frame, NOW)
            .unwrap();
        assert!(outcome.carried);
        assert!(
            !outcome.carried_family,
            "a relay-sourced row is never re-uploaded, so it is never the family class"
        );
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

#[cfg(test)]
mod delivery_tests {
    //! Executable owners for the per-kind delivery fold — the policy that used
    //! to be a third copy in the desktop shell — driven end to end through the
    //! one production pair (`process_inbound_frame` → `core_deliver_inbound`),
    //! never by calling the fold with a hand-built commit token.
    //!
    //! `DELIVER-01` owns the two vectors at the top: a pairwise body may name
    //! only its verified sender's thread, and a group body only the group whose
    //! key opened it.

    use std::sync::Arc;

    use crate::{
        compute_recipient_hint, encode_envelope_frame, encode_group_invite_content,
        encode_lan_endpoint_content, encode_message_body, encode_message_body_extended,
        encode_profile_sync_content, encode_receipt_content, encode_relay_update_content,
        generate_identity, generate_msg_id, make_friend_card, seal_group_message, seal_message,
        Contact, CoreDeliveryVerdict, CoreDiscoveryPolicyState, CoreInboundSource, Group, Identity,
        LanEndpointContent, MessageArrival, MessageBody, MessageStore, ProfileSyncContent,
        ReceiptContent, RelayUpdateContent, SeenIds, DEFAULT_HOP_TTL, DEVICE_ID_LEN,
        KIND_FRIEND_REQUEST, KIND_GROUP_INVITE, KIND_LAN_ENDPOINT_HINT, KIND_PROFILE_SYNC,
        KIND_RECEIPT, KIND_RELAY_UPDATE, KIND_TEXT, LEGACY_DEVICE_ID, MS_PER_DAY,
        RECEIPT_TYPE_DELIVERED,
    };

    use super::CoreInboundDelivery;

    const NOW: i64 = 1_700_000_000_000;

    fn store() -> MessageStore {
        MessageStore::open(":memory:".to_string()).expect("open in-memory store")
    }

    fn contact(identity: &Identity, name: &str) -> Contact {
        Contact {
            user_id: identity.user_id.clone(),
            name: name.into(),
            sign_pk: identity.sign_pk.clone(),
            agree_pk: identity.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    fn arrival() -> MessageArrival {
        MessageArrival {
            transport: 3,
            hops_taken: 0,
            received_at: NOW,
        }
    }

    fn discovery() -> CoreDiscoveryPolicyState {
        CoreDiscoveryPolicyState {
            enabled: true,
            revision: 0,
        }
    }

    fn body(kind: u8, chat_id: Vec<u8>, content: Vec<u8>) -> Vec<u8> {
        encode_message_body(MessageBody {
            kind,
            chat_id,
            lamport: 1,
            timestamp: NOW,
            content,
        })
        .expect("encode body")
    }

    /// Run the production pair for a pairwise envelope from `sender` to `me`.
    fn deliver_pairwise(
        store: &MessageStore,
        me: &Identity,
        sender: &Identity,
        payload: Vec<u8>,
    ) -> CoreInboundDelivery {
        let sealed = seal_message(sender.clone(), me.agree_pk.clone(), payload).expect("seal");
        let frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            NOW + 7 * MS_PER_DAY,
            compute_recipient_hint(me.user_id.clone(), NOW),
            sealed,
        );
        let seen = Arc::new(SeenIds::new());
        let outcome = store
            .process_inbound_frame(me.clone(), seen, CoreInboundSource::Mesh, frame, NOW)
            .expect("inbound");
        let payload = outcome
            .delivered_payloads
            .first()
            .cloned()
            .expect("a pairwise envelope for us is delivered");
        store
            .core_deliver_inbound(
                me.clone(),
                outcome.delivered_sender.expect("verified sender"),
                payload,
                outcome.commit.expect("commit token"),
                arrival(),
                discovery(),
            )
            .expect("delivery")
    }

    /// Run the production pair for a group envelope signed by `sender`.
    fn deliver_group(
        store: &MessageStore,
        me: &Identity,
        sender: &Identity,
        group: &Group,
        payload: Vec<u8>,
    ) -> CoreInboundDelivery {
        let sealed = seal_group_message(sender.clone(), group.clone(), payload).expect("seal");
        let frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            NOW + 7 * MS_PER_DAY,
            compute_recipient_hint(group.id.clone(), NOW),
            sealed,
        );
        let seen = Arc::new(SeenIds::new());
        let outcome = store
            .process_inbound_frame(me.clone(), seen, CoreInboundSource::Mesh, frame, NOW)
            .expect("inbound");
        let payload = outcome
            .delivered_payloads
            .first()
            .cloned()
            .expect("a group envelope for a member is delivered");
        store
            .core_deliver_inbound(
                me.clone(),
                outcome.delivered_sender.expect("verified sender"),
                payload,
                outcome.commit.expect("commit token"),
                arrival(),
                discovery(),
            )
            .expect("delivery")
    }

    fn group_of(me: &Identity, friend: &Identity) -> Group {
        Group {
            id: vec![0x33; 16],
            name: "Deck 9".to_string(),
            member_user_ids: vec![me.user_id.clone(), friend.user_id.clone()],
            key: vec![0x44; 32],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        }
    }

    /// §5, through the one production pair: a person's devices author into
    /// separate streams even at one lamport, and a body with no device field
    /// lands on the reserved legacy stream beside them. The device id has to
    /// survive the whole path — encode, seal, open, decode, insert — to be
    /// worth anything, so nothing here short-circuits it.
    #[test]
    fn a_persons_devices_author_into_separate_streams() {
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        store.upsert_contact(contact(&sender, "Sender")).unwrap();

        let phone = vec![0x51; DEVICE_ID_LEN];
        let tablet = vec![0x52; DEVICE_ID_LEN];
        for authoring in [None, Some(phone.clone()), Some(tablet.clone())] {
            let payload = encode_message_body_extended(
                MessageBody {
                    kind: KIND_TEXT,
                    chat_id: sender.user_id.clone(),
                    lamport: 1,
                    timestamp: NOW,
                    content: b"one person, three envelopes".to_vec(),
                },
                None,
                authoring,
                None,
            )
            .expect("encode body");
            let delivery = deliver_pairwise(&store, &me, &sender, payload);
            assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
            assert!(delivery.persisted);
        }

        // The pre-WP1 stream key would have called the second and third of
        // these a fork of the first.
        assert!(!store.has_message_conflicts().unwrap());
        let mut expected = vec![LEGACY_DEVICE_ID.to_vec(), phone, tablet];
        expected.sort();
        assert_eq!(
            store
                .message_stream_device_ids(sender.user_id.clone(), sender.user_id.clone())
                .unwrap(),
            expected
        );
    }

    /// §5, WP4: the id an activated device authors under reaches the recipient.
    ///
    /// The test above proves the *path* carries a device id; this one proves
    /// the authoring side actually puts one there. Nothing below hands the
    /// sender's store a device id — `author_pairwise_message` reads the fleet
    /// §9's activation left behind — and nothing tells the recipient one is
    /// coming: it is an ordinary unlinked peer running the production inbound
    /// pair, which is exactly what a legacy build is.
    #[test]
    fn an_activated_devices_authored_envelope_arrives_on_its_own_stream() {
        let recipient_store = store();
        let me = generate_identity();
        let sender = generate_identity();
        recipient_store
            .upsert_contact(contact(&sender, "Sender"))
            .unwrap();

        let sender_store = store();
        sender_store.upsert_contact(contact(&me, "Me")).unwrap();
        let phone = crate::generate_device_keypair();
        sender_store
            .set_own_device_fleet(crate::OwnDeviceFleet {
                own_device_id: Some(phone.device_id.clone()),
                device_ids: vec![
                    phone.device_id.clone(),
                    crate::generate_device_keypair().device_id,
                ],
                projected_from: crate::RosterVersion {
                    recovery_epoch: 0,
                    seq: 1,
                },
            })
            .unwrap();

        let authored = sender_store
            .author_pairwise_message(
                sender.clone(),
                contact(&me, "Me"),
                KIND_TEXT,
                b"we docked early".to_vec(),
                None,
                NOW,
            )
            .unwrap();

        let frame = encode_envelope_frame(
            authored.envelope.msg_id.clone(),
            DEFAULT_HOP_TTL,
            NOW + 7 * MS_PER_DAY,
            compute_recipient_hint(me.user_id.clone(), NOW),
            authored.envelope.sealed.clone(),
        );
        let outcome = recipient_store
            .process_inbound_frame(
                me.clone(),
                Arc::new(SeenIds::new()),
                CoreInboundSource::Mesh,
                frame,
                NOW,
            )
            .expect("inbound");
        let delivery = recipient_store
            .core_deliver_inbound(
                me,
                outcome.delivered_sender.expect("verified sender"),
                outcome
                    .delivered_payloads
                    .first()
                    .cloned()
                    .expect("delivered"),
                outcome.commit.expect("commit token"),
                arrival(),
                discovery(),
            )
            .expect("delivery");

        assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
        assert_eq!(
            recipient_store
                .message_stream_device_ids(sender.user_id.clone(), sender.user_id)
                .unwrap(),
            vec![phone.device_id],
            "the recipient files the message on the device that authored it, \
             and never on the legacy stream"
        );
    }

    /// §12's other half: an install that has never linked still emits exactly
    /// today's envelope, and its mail still lands on the legacy stream. This is
    /// every device in the field until WP3's ceremony writes a fleet.
    #[test]
    fn an_unlinked_devices_authored_envelope_stays_on_the_legacy_stream() {
        let recipient_store = store();
        let me = generate_identity();
        let sender = generate_identity();
        recipient_store
            .upsert_contact(contact(&sender, "Sender"))
            .unwrap();

        let sender_store = store();
        let authored = sender_store
            .author_pairwise_message(
                sender.clone(),
                contact(&me, "Me"),
                KIND_TEXT,
                b"we docked early".to_vec(),
                None,
                NOW,
            )
            .unwrap();
        assert_eq!(authored.message.sender_device_id, LEGACY_DEVICE_ID.to_vec());

        let frame = encode_envelope_frame(
            authored.envelope.msg_id.clone(),
            DEFAULT_HOP_TTL,
            NOW + 7 * MS_PER_DAY,
            compute_recipient_hint(me.user_id.clone(), NOW),
            authored.envelope.sealed.clone(),
        );
        let outcome = recipient_store
            .process_inbound_frame(
                me.clone(),
                Arc::new(SeenIds::new()),
                CoreInboundSource::Mesh,
                frame,
                NOW,
            )
            .expect("inbound");
        recipient_store
            .core_deliver_inbound(
                me,
                outcome.delivered_sender.expect("verified sender"),
                outcome
                    .delivered_payloads
                    .first()
                    .cloned()
                    .expect("delivered"),
                outcome.commit.expect("commit token"),
                arrival(),
                discovery(),
            )
            .expect("delivery");

        assert_eq!(
            recipient_store
                .message_stream_device_ids(sender.user_id.clone(), sender.user_id)
                .unwrap(),
            vec![LEGACY_DEVICE_ID.to_vec()]
        );
    }

    /// §14.3: the sealed-body device id is honoured without a roster check
    /// (WP5 owns that), so the number of streams it can open is bounded here.
    /// A sender stamping a fresh id on every message must not be able to mint
    /// unbounded stream namespaces in this device's store; past the hard cap
    /// the row files on the legacy stream, which loses nothing and is exactly
    /// the pre-WP1 shape.
    #[test]
    fn a_sender_cannot_mint_more_device_streams_than_the_hard_cap() {
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        store.upsert_contact(contact(&sender, "Sender")).unwrap();

        let mut lamport = 0;
        let mut send = |device: u8| {
            lamport += 1;
            let payload = encode_message_body_extended(
                MessageBody {
                    kind: KIND_TEXT,
                    chat_id: sender.user_id.clone(),
                    lamport,
                    timestamp: NOW,
                    content: vec![device],
                },
                None,
                Some(vec![device; DEVICE_ID_LEN]),
                None,
            )
            .expect("encode body");
            let delivery = deliver_pairwise(&store, &me, &sender, payload);
            assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
            assert!(delivery.persisted);
        };

        for device in 1..=super::DEVICE_HARD_CAP as u8 {
            send(device);
        }
        let held = store
            .message_stream_device_ids(sender.user_id.clone(), sender.user_id.clone())
            .unwrap();
        assert_eq!(held.len(), super::DEVICE_HARD_CAP as usize);
        assert!(!held.contains(&LEGACY_DEVICE_ID.to_vec()));

        // The 17th distinct device id opens no new stream; its message still
        // arrives, on the legacy one.
        send(super::DEVICE_HARD_CAP as u8 + 1);
        let held = store
            .message_stream_device_ids(sender.user_id.clone(), sender.user_id.clone())
            .unwrap();
        assert_eq!(held.len(), super::DEVICE_HARD_CAP as usize + 1);
        assert!(held.contains(&LEGACY_DEVICE_ID.to_vec()));
        assert!(!held.contains(&vec![super::DEVICE_HARD_CAP as u8 + 1; DEVICE_ID_LEN]));
        assert_eq!(
            store
                .messages_for_chat(sender.user_id.clone())
                .unwrap()
                .len(),
            super::DEVICE_HARD_CAP as usize + 1,
            "nothing is dropped: the capped row files on the legacy stream",
        );

        // A device that already has a stream keeps it -- a real 16-device
        // person never degrades.
        send(1);
        assert_eq!(
            store
                .message_stream_device_ids(sender.user_id.clone(), sender.user_id.clone())
                .unwrap()
                .len(),
            super::DEVICE_HARD_CAP as usize + 1,
        );
    }

    #[test]
    fn deliver_01_a_pairwise_body_may_only_name_its_verified_senders_thread() {
        // The signed body's chat_id is sender-chosen. An accepted contact
        // aiming it at a THIRD party's thread must write nothing at all --
        // not a message row, not a receipt.
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        let victim = generate_identity();
        store.upsert_contact(contact(&sender, "Sender")).unwrap();
        store.upsert_contact(contact(&victim, "Victim")).unwrap();

        let forged = body(KIND_TEXT, victim.user_id.clone(), b"not from me".to_vec());
        let delivery = deliver_pairwise(&store, &me, &sender, forged);

        assert_eq!(delivery.verdict, CoreDeliveryVerdict::DroppedForeignChat);
        assert!(!delivery.persisted);
        assert!(
            store
                .messages_for_chat(victim.user_id.clone())
                .unwrap()
                .is_empty(),
            "a body naming someone else's thread must not land in it"
        );
        assert!(store
            .messages_for_chat(sender.user_id.clone())
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .outgoing_receipt_through(
                    sender.user_id.clone(),
                    sender.user_id,
                    RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            0,
            "a dropped body is never auto-receipted"
        );
    }

    #[test]
    fn deliver_01_a_group_body_may_only_name_the_group_that_opened_it() {
        // Same rule on the group half: a member holding the group key must not
        // be able to file a row into another chat by renaming it.
        let store = store();
        let me = generate_identity();
        let friend = generate_identity();
        let group = group_of(&me, &friend);
        store.upsert_group(group.clone()).unwrap();

        let forged = body(KIND_TEXT, me.user_id.clone(), b"wrong thread".to_vec());
        let delivery = deliver_group(&store, &me, &friend, &group, forged);

        assert_eq!(delivery.verdict, CoreDeliveryVerdict::DroppedForeignChat);
        assert!(!delivery.persisted);
        assert!(store
            .messages_for_chat(me.user_id.clone())
            .unwrap()
            .is_empty());
        assert!(store.messages_for_chat(group.id).unwrap().is_empty());
    }

    #[test]
    fn a_group_body_from_a_member_lands_in_its_group() {
        let store = store();
        let me = generate_identity();
        let friend = generate_identity();
        let group = group_of(&me, &friend);
        store.upsert_group(group.clone()).unwrap();

        let delivery = deliver_group(
            &store,
            &me,
            &friend,
            &group,
            body(KIND_TEXT, group.id.clone(), b"deck party".to_vec()),
        );

        assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
        assert!(delivery.persisted);
        assert!(delivery.endpoint_hint.is_none());
        let rows = store.messages_for_chat(group.id).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].payload, b"deck party".to_vec());
    }

    #[test]
    fn a_pairwise_text_from_a_contact_is_stored_and_auto_receipted() {
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        store.upsert_contact(contact(&sender, "Sender")).unwrap();

        let delivery = deliver_pairwise(
            &store,
            &me,
            &sender,
            body(KIND_TEXT, sender.user_id.clone(), b"on deck 9".to_vec()),
        );

        assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
        assert_eq!(delivery.kind, KIND_TEXT);
        assert!(delivery.persisted);
        let rows = store.messages_for_chat(sender.user_id.clone()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            store
                .outgoing_receipt_through(
                    sender.user_id.clone(),
                    sender.user_id,
                    RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            1,
            "delivery acknowledges the sender's stream up to its contiguous point"
        );
    }

    #[test]
    fn an_unauthorized_pairwise_sender_is_dropped_rather_than_failed() {
        // Not yet a contact, and the kind is not an onboarding kind: a
        // terminal policy verdict, so the envelope is consumed and its relay
        // copy acks away instead of being refetched forever.
        let store = store();
        let me = generate_identity();
        let stranger = generate_identity();

        let delivery = deliver_pairwise(
            &store,
            &me,
            &stranger,
            body(KIND_TEXT, stranger.user_id.clone(), b"hello?".to_vec()),
        );

        assert_eq!(
            delivery.verdict,
            CoreDeliveryVerdict::DroppedUnauthorizedSender
        );
        assert!(!delivery.persisted);
        assert!(store
            .messages_for_chat(stranger.user_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_direct_friend_request_onboards_its_verified_sender() {
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        let card = make_friend_card("Emma".into(), sender.clone(), None, None).unwrap();

        let delivery = deliver_pairwise(
            &store,
            &me,
            &sender,
            body(
                KIND_FRIEND_REQUEST,
                sender.user_id.clone(),
                card.into_bytes(),
            ),
        );

        assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
        assert_eq!(
            store.get_contact(sender.user_id).unwrap().unwrap().name,
            "Emma"
        );
    }

    #[test]
    fn a_receipt_advances_a_watermark_and_never_answers_itself() {
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        store.upsert_contact(contact(&sender, "Sender")).unwrap();

        let content = encode_receipt_content(ReceiptContent {
            chat_id: sender.user_id.clone(),
            sender_user_id: me.user_id.clone(),
            lamport: 7,
            receipt_type: RECEIPT_TYPE_DELIVERED,
            group_id: None,
        })
        .unwrap();
        let delivery = deliver_pairwise(
            &store,
            &me,
            &sender,
            body(KIND_RECEIPT, sender.user_id.clone(), content),
        );

        assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
        assert!(
            !delivery.persisted,
            "a receipt is a hidden kind: it leaves no chat row"
        );
        assert_eq!(
            store
                .receipt_through(
                    sender.user_id.clone(),
                    me.user_id.clone(),
                    RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            7
        );
        assert_eq!(
            store
                .outgoing_receipt_through(
                    sender.user_id.clone(),
                    sender.user_id,
                    RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            0,
            "a receipt must never trigger an auto-receipt back"
        );
    }

    #[test]
    fn a_receipt_for_another_stream_is_dropped_not_silently_written() {
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        let other = generate_identity();
        store.upsert_contact(contact(&sender, "Sender")).unwrap();

        let content = encode_receipt_content(ReceiptContent {
            chat_id: sender.user_id.clone(),
            sender_user_id: other.user_id.clone(),
            lamport: 7,
            receipt_type: RECEIPT_TYPE_DELIVERED,
            group_id: None,
        })
        .unwrap();
        let sealed = seal_message(
            sender.clone(),
            me.agree_pk.clone(),
            body(KIND_RECEIPT, sender.user_id.clone(), content),
        )
        .unwrap();
        let frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            NOW + 7 * MS_PER_DAY,
            compute_recipient_hint(me.user_id.clone(), NOW),
            sealed,
        );
        let outcome = store
            .process_inbound_frame(
                me.clone(),
                Arc::new(SeenIds::new()),
                CoreInboundSource::Mesh,
                frame,
                NOW,
            )
            .unwrap();
        let result = store.core_deliver_inbound(
            me.clone(),
            outcome.delivered_sender.unwrap(),
            outcome.delivered_payloads[0].clone(),
            outcome.commit.unwrap(),
            arrival(),
            discovery(),
        );
        let delivery = result.expect("a body-shape rejection is a verdict, not an error");
        assert_eq!(
            delivery.verdict,
            CoreDeliveryVerdict::DroppedMalformed,
            "a receipt that does not acknowledge our own stream is dropped terminally"
        );
        assert!(!delivery.persisted);
    }

    /// The livelock this guards against: a permanently undecodable body from a
    /// verified sender used to come back as `Err`, which every shell maps to
    /// "delivery failed, do not ack". The relay copy is then refetched and
    /// re-fails forever. Deterministic decode failures are terminal, and the
    /// caller commits them like any other consumption.
    #[test]
    fn an_undecodable_body_is_a_terminal_drop_not_a_retryable_failure() {
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        store.upsert_contact(contact(&sender, "Sender")).unwrap();

        let sealed = seal_message(
            sender.clone(),
            me.agree_pk.clone(),
            // Not a message body at all: this will never decode, on this
            // attempt or any retry.
            vec![0xff; 12],
        )
        .expect("seal");
        let frame = encode_envelope_frame(
            generate_msg_id(),
            DEFAULT_HOP_TTL,
            NOW + 7 * MS_PER_DAY,
            compute_recipient_hint(me.user_id.clone(), NOW),
            sealed,
        );
        let outcome = store
            .process_inbound_frame(
                me.clone(),
                Arc::new(SeenIds::new()),
                CoreInboundSource::Mesh,
                frame,
                NOW,
            )
            .expect("inbound");
        let commit = outcome.commit.expect("commit token");
        assert!(
            commit.hidden_kind.is_none(),
            "an unreadable body must never vouch for a hidden kind's relay ack"
        );

        let delivery = store
            .core_deliver_inbound(
                me.clone(),
                outcome.delivered_sender.expect("verified sender"),
                outcome.delivered_payloads[0].clone(),
                commit,
                arrival(),
                discovery(),
            )
            .expect("an undecodable body is consumed, never a retryable failure");
        assert_eq!(delivery.verdict, CoreDeliveryVerdict::DroppedMalformed);
        assert!(!delivery.persisted);
        assert!(delivery.endpoint_hint.is_none());
    }

    /// The same rule one level in: the body decoded, the per-kind content did
    /// not. Still fixed bytes, still terminal.
    #[test]
    fn undecodable_per_kind_content_is_a_terminal_drop() {
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        store.upsert_contact(contact(&sender, "Sender")).unwrap();

        let delivery = deliver_pairwise(
            &store,
            &me,
            &sender,
            body(
                KIND_LAN_ENDPOINT_HINT,
                sender.user_id.clone(),
                vec![0xff; 9],
            ),
        );
        assert_eq!(delivery.verdict, CoreDeliveryVerdict::DroppedMalformed);
        assert_eq!(delivery.kind, KIND_LAN_ENDPOINT_HINT);
        assert!(delivery.endpoint_hint.is_none());
        assert!(!delivery.persisted);
    }

    #[test]
    fn a_lan_endpoint_hint_returns_an_intent_keyed_to_its_sender() {
        // The only effect the driver executes. ENDPOINT-01: the intent names
        // the verified sender and that sender's own advertised endpoint --
        // nothing discovered from a third party crosses this boundary.
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        store.upsert_contact(contact(&sender, "Sender")).unwrap();

        let endpoint = LanEndpointContent {
            instance_token: vec![7; 8],
            network_id: vec![9; 8],
            host: "192.168.1.42".into(),
            port: 41234,
            expires_at_ms: NOW + 60_000,
        };
        let content = encode_lan_endpoint_content(endpoint.clone()).unwrap();
        let delivery = deliver_pairwise(
            &store,
            &me,
            &sender,
            body(KIND_LAN_ENDPOINT_HINT, sender.user_id.clone(), content),
        );

        assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
        let hint = delivery.endpoint_hint.expect("an endpoint intent");
        assert_eq!(hint.peer_user_id, sender.user_id);
        assert_eq!(hint.endpoint.host, endpoint.host);
        assert_eq!(hint.observed_at_ms, NOW);
    }

    #[test]
    fn a_profile_sync_updates_the_contact_it_came_from() {
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        store.upsert_contact(contact(&sender, "Old name")).unwrap();

        let content = encode_profile_sync_content(ProfileSyncContent {
            avatar_epoch: NOW,
            name: "New name".into(),
            avatar: Vec::new(),
            friends_of_friends_version: 1,
            friends_of_friends_enabled: true,
            friends_of_friends_revision: 3,
        })
        .unwrap();
        let delivery = deliver_pairwise(
            &store,
            &me,
            &sender,
            body(KIND_PROFILE_SYNC, sender.user_id.clone(), content),
        );

        assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
        assert_eq!(
            store
                .get_contact(sender.user_id.clone())
                .unwrap()
                .unwrap()
                .name,
            "New name"
        );
    }

    #[test]
    fn a_relay_update_never_moves_a_third_partys_endpoint() {
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        let other = generate_identity();
        store.upsert_contact(contact(&sender, "Sender")).unwrap();
        store.upsert_contact(contact(&other, "Other")).unwrap();

        // A notice naming somebody else's endpoint is refused by the store
        // rule; the hidden row still lands, and nothing else moves.
        let content = encode_relay_update_content(RelayUpdateContent {
            subject_user_id: other.user_id.clone(),
            relay_url: "https://relay.example/".into(),
            relay_token: "tok-aaaaaaaaaaaaaaaa".into(),
            relay_epoch: NOW,
        })
        .unwrap();
        let delivery = deliver_pairwise(
            &store,
            &me,
            &sender,
            body(KIND_RELAY_UPDATE, sender.user_id.clone(), content),
        );

        assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
        assert!(store
            .get_contact(other.user_id)
            .unwrap()
            .unwrap()
            .relay_url
            .is_none());
    }

    #[test]
    fn a_group_invite_is_filed_under_the_group_it_creates() {
        let store = store();
        let me = generate_identity();
        let sender = generate_identity();
        store.upsert_contact(contact(&sender, "Sender")).unwrap();

        let group = Group {
            id: vec![0x55; 16],
            name: "Muster".into(),
            member_user_ids: vec![me.user_id.clone(), sender.user_id.clone()],
            key: vec![0x66; 32],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        };
        let content = encode_group_invite_content(group.clone()).unwrap();
        let delivery = deliver_pairwise(
            &store,
            &me,
            &sender,
            body(KIND_GROUP_INVITE, sender.user_id.clone(), content),
        );

        assert_eq!(delivery.verdict, CoreDeliveryVerdict::Applied);
        assert!(store.get_group(group.id.clone()).unwrap().is_some());
        assert_eq!(
            store.messages_for_chat(group.id).unwrap().len(),
            1,
            "the invite row belongs to the group, not the 1:1 thread"
        );
        assert!(store.messages_for_chat(sender.user_id).unwrap().is_empty());
    }
}
