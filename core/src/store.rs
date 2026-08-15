//! Message + contact store: SQLite-backed persistence (DESIGN.md §7.1,
//! §10). `insert_message` is idempotent on (chat_id, sender_user_id,
//! lamport): re-delivering the same envelope (expected under DTN) is a
//! no-op. A conflict whose (timestamp, kind, payload) *don't* match is
//! ambiguous and is ignored: the current wire format has no authenticated
//! stream generation with which to prove that either branch supersedes the
//! other, so an incoming copy must never erase visible history -- see
//! [MessageStore::insert_message]'s doc comment. Per-chat lamport counters
//! are maintained independently by each sender (DESIGN.md §7.1), so gap
//! detection in [MessageStore::highest_contiguous_lamport] is keyed on
//! (chat_id, sender_user_id), not chat_id alone.
//!
//! Contacts (DESIGN.md §6.2) live in the same store/connection rather than a
//! separate file: they're the other half of "who can I seal a message to,"
//! which is exactly the data a message store needs alongside messages
//! themselves.
//!
//! Groups (DESIGN.md §6.5) live here too: one `groups` row for the stable
//! id/name/key tuple, plus `group_members` rows for the current membership.
//! Group chat history reuses the existing `messages` table with `chat_id =
//! group_id`; the existing `(chat_id, sender_user_id, lamport)` streams were
//! already designed for multi-sender chats.
//!
//! ## Receipts (DESIGN.md §7.2)
//!
//! The `receipts` table records what *this device's peer* has acknowledged
//! about messages *this device sent*: "the peer has delivered/read messages
//! authored by `sender_user_id` (this device's own UserID, from the peer's
//! point of view) in `chat_id`, through `through_lamport`." Per §7.2 a
//! receipt is cumulative, and receipts "are tiny, idempotent, and re-sent
//! opportunistically on every peer sync, so a lost receipt heals itself" --
//! which means the same or a stale receipt can arrive more than once, and an
//! older cumulative value can arrive *after* a newer one (DTN reordering).
//! [`MessageStore::record_receipt`] is therefore a monotonic upsert: it only
//! ever raises `through_lamport`, never lowers it.
//!
//! ## Outbound envelope queue (DESIGN.md §4, §9)
//!
//! Locally authored traffic should be sealed once, persisted, and then handed
//! to whatever transports are up. The `outbound_envelopes` table is that
//! transport-agnostic queue for authored chat messages: it stores the exact
//! §6.4 public header plus sealed bytes, keyed by the logical message
//! identity `(chat_id, sender_user_id, kind, lamport, recipient_user_id)` so
//! reconnect retries reuse the same `msg_id` and ciphertext instead of
//! re-sealing a fresh envelope every time. The recipient now participates in
//! the dedupe key because `kind=4` group invites are one logical chat event
//! fanned out as several pairwise-sealed envelopes, one per member.
//! `insert_outgoing_message` writes the plaintext message row and one queued
//! envelope in one transaction so local history and sync state never diverge
//! on a crash boundary.
//!
//! ## Outgoing receipts (DESIGN.md §7.2, §7.3)
//!
//! The `outgoing_receipts` table is the mirror image of `receipts`: it tracks
//! what *this device* has locally observed and should tell its peer about
//! messages the peer authored: "I have delivered/read your messages in
//! `chat_id` through `through_lamport`." Keeping that cumulative watermark in
//! the store lets a lost standalone receipt heal itself on the next digest
//! sync -- §7.3's "receipts first" rule. Like incoming receipts, outgoing
//! ones are cumulative and monotonic, so stale retries must never lower the
//! stored watermark.
//!
//! ## Outgoing receipt envelope queue (DESIGN.md §7.2, §7.3, §9)
//!
//! Relay upload needs the same "seal once, retry many times" property as
//! authored text, but receipts are not chat-stream messages: they never live
//! in `messages`, carry no lamport sequence of their own, and must never be
//! acked in return. The `outgoing_receipt_envelopes` table is therefore a
//! separate queue keyed by the logical receipt watermark
//! `(chat_id, sender_user_id, receipt_type)`. Each row stores the exact
//! sealed envelope for the *latest* cumulative watermark owed on that key.
//! Re-queueing the same watermark is a no-op that preserves the existing
//! `msg_id`; advancing the watermark replaces the row with a newly sealed
//! envelope and clears its relay-posted marker so the higher cumulative
//! receipt uploads on the next relay sync.
//!
//! ## Sync digests (DESIGN.md §7.3)
//!
//! On peer connect, each side summarizes what it already has per chat so the
//! other side can send just what's missing. §7.3 describes that summary as
//! "(chat id, highest-contiguous lamport, recent msg_id bloom filter)".
//! [`MessageStore::chat_digest`] implements the contiguous-lamport half of
//! that -- one [`DigestEntry`] per sender who has posted in the chat,
//! reusing the same gap-aware [`MessageStore::highest_contiguous_lamport`]
//! logic already needed for per-sender ordering. The msg-id half currently
//! ships as an **exact** list of carried-envelope `msg_id`s
//! ([`MessageStore::carried_msg_ids`]) rather than a bloom filter: family
//! scale keeps that list small enough, exactness avoids false positives, and
//! it is sufficient to unlock spray-on-connect for mule chains. A true bloom
//! filter remains a possible future compression step, especially once
//! out-of-order/non-contiguous delivered message ids also need to participate.
//! [`MessageStore::messages_after`] answers the other half: given what a
//! peer's digest says they already have, which of *our* messages from a
//! given sender are they missing.
//!
//! The advertised msg-id list is also how a mule learns it can safely drop a
//! carried 1:1 envelope (DTN_TODOS.md §3.2, D2 mule-drain-confirm): the true
//! recipient doesn't carry a message it opens, it *consumes* it, so
//! [`MessageStore::recent_consumed_msg_ids`] feeds the same advertised list
//! alongside [`MessageStore::carried_msg_ids`] (see
//! `engine.rs::core_digest_advertised_msg_ids`) -- otherwise a message we
//! successfully received would never show up in what we tell a mule we
//! already have, and the mule would keep it until expiry. The other half of
//! D2 -- actually removing a carried envelope once a peer's digest proves
//! they have it -- is `engine.rs::core_confirm_carried_deliveries`.

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Transaction};
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

use crate::groups::{canonicalize_members, validate_group};
use crate::limits::MAX_ENVELOPE_SEALED_BYTES;
use crate::{
    core_is_visible_chat_kind, verify_introduction_ticket, CoreError, CoreInboundDisposition,
    CoreRelayEnvelopeDisposition, CoreRelayFetchedEnvelope, CoreRelayShadowReport,
    FriendDirectoryContent, Group, IntroductionTicket, RelayUpdateContent, SuggestedFriendCard,
    KIND_INTRODUCED_FRIEND_REQUEST, MS_PER_DAY, RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
};

type CarriedHintRecipient = (Vec<u8>, Vec<u8>);

/// FC6: recover from mutex poisoning instead of propagating it as a panic.
/// `Mutex::lock` returns `Err` if a prior locker panicked while holding the
/// lock; the stdlib's default guidance is that the protected data might be
/// left in an inconsistent state. But every store write here runs inside a
/// rusqlite transaction (`Connection::transaction`/`tx.commit()`), so a
/// panic mid-write simply means the transaction was never committed --
/// SQLite's own atomicity already leaves the connection in a state that's
/// safe to keep using; there is nothing about a poisoned `Mutex<Connection>`
/// that makes the connection itself unsafe. Without this, the first panic
/// under the lock (a bug, an unexpected SQLite error path, an OOM) would
/// poison the mutex permanently: every later store call from the UniFFI
/// boundary would itself panic natively, turning one bug into a
/// process-wide crash loop until the app restarts.
fn lock_conn(conn: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
    conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

const MESSAGE_ID_LEN: usize = 16;
const CARRIED_CONTENT_DIGEST_LEN: usize = 32;
const DEFAULT_TOTAL_CARRY_BUDGET_BYTES: i64 = 64 * 1024 * 1024;

/// One stored message body (DESIGN.md §7.1). `timestamp` is milliseconds
/// since the Unix epoch; `kind` matches the DESIGN.md §7.1 `kind` byte
/// (text=1, receipt=2, friend-request=3, group-invite=4, ...).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct StoredMessage {
    pub chat_id: Vec<u8>,
    pub sender_user_id: Vec<u8>,
    pub lamport: u64,
    pub timestamp: i64,
    pub kind: u8,
    pub payload: Vec<u8>,
}

/// Local-only diagnostics for how an incoming message reached this device.
/// `transport`: 0 = BLE direct, 1 = BLE through another device, 2 = relay,
/// 3 = same-LAN direct, 4 = same-LAN through another device.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct MessageArrival {
    pub transport: u8,
    pub hops_taken: u8,
    pub received_at: i64,
}

/// Redacted, metadata-only view of a quarantined stream conflict. The full
/// incoming branch remains private inside the SQLite store for a future
/// explicit recovery rule; diagnostics receive only stable hashes and counts.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct MessageConflictSummary {
    pub chat_hash: String,
    pub sender_hash: String,
    pub lamport: u64,
    pub existing_fingerprint: String,
    pub incoming_fingerprint: String,
    pub arrival_transport: Option<u8>,
    pub first_seen_at_ms: i64,
    pub last_seen_at_ms: i64,
    pub seen_count: u64,
}

/// Result of an arrival-aware incoming insert. Legacy callers still receive a
/// boolean; transport shells use this richer result so a quarantined conflict
/// is logged distinctly from an ordinary duplicate.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncomingMessageInsertOutcome {
    Inserted,
    Duplicate,
    QuarantinedConflict,
}

/// When one message in a chat first reached this device, keyed by the
/// (sender, lamport) pair that identifies it within that chat. Returned in
/// bulk by [`MessageStore::chat_received_times`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CoreMessageReceivedAt {
    pub sender_user_id: Vec<u8>,
    pub lamport: u64,
    pub received_at_ms: i64,
}

/// A privacy-preserving path by which the device has reached a friend.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerConnectionTransport {
    Bluetooth,
    LocalWifi,
    ShorePass,
    /// Another device carried this the last hop, so no path to the friend was
    /// observed at all.
    ///
    /// Not a fourth way of reaching someone — the absence of one. A muled
    /// message says a phone in the middle had Bluetooth to us; it says nothing
    /// about whether the *sender* was ever nearby, and for group chat muling is
    /// the ordinary case rather than the exception. Folding these into
    /// Bluetooth or local Wi-Fi is how connection history ends up telling
    /// someone their friend was in Bluetooth range of them when that friend was
    /// on the other side of the ship.
    ///
    /// Surfaces have to render this as no claim, not as a path name. See
    /// [`core_peer_transport_is_observed`].
    Carried,
}

/// Did we actually observe the path this evidence arrived on?
///
/// False only for [`PeerConnectionTransport::Carried`]. A surface that names a
/// path must ask this first and drop the "via ..." clause when the answer is
/// no; saying less is the only honest option, because the hop we saw belongs to
/// whichever phone relayed it and not to the friend the line is about.
#[uniffi::export]
pub fn core_peer_transport_is_observed(transport: PeerConnectionTransport) -> bool {
    !matches!(transport, PeerConnectionTransport::Carried)
}

/// A metadata-only connection event. No addresses, network names, tokens, or
/// message content are retained.
///
/// The two message kinds are opposite directions and must not be confused --
/// getting them the wrong way round is a user-visible lie, since the
/// Connection details screen names them:
/// - [`PeerConnectionEventKind::MessageDelivered`]: a message *we sent* reached
///   *them*. Recorded in [`MessageStore::record_receipt`], and only when their
///   delivery receipt newly covers a genuinely visible message we authored --
///   never for a receipt that merely acks profile sync, the friend directory,
///   an endpoint hint or a relay-change notice, and never twice for the same
///   proof. The peer named on the event is the one who received our message.
/// - [`PeerConnectionEventKind::MessageReceived`]: a message *they sent* reached
///   *us*. Recorded where a genuinely visible inbound chat message is stored,
///   never for receipts, profile sync, relay updates or any other hidden kind.
///
/// Both directions ask the same question of a kind --
/// [`crate::core_is_visible_chat_kind`] -- and each has exactly one recording
/// site. Adding a second is how one of these starts lying.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerConnectionEventKind {
    Connected,
    Disconnected,
    PresenceSeen,
    MessageDelivered,
    MessageReceived,
}

#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PeerConnectionEvent {
    pub user_id: Vec<u8>,
    pub transport: PeerConnectionTransport,
    pub kind: PeerConnectionEventKind,
    pub occurred_at_ms: i64,
}

/// The newest moment each kind of evidence was recorded for one peer on one
/// path. `last_delivered_at_ms` is OUR message reaching THEM (their receipt
/// came back); `last_received_at_ms` is THEIR visible chat message reaching
/// US. Both are `None` until the corresponding event has actually happened.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct PeerConnectionSummary {
    pub user_id: Vec<u8>,
    pub transport: PeerConnectionTransport,
    pub last_connected_at_ms: Option<i64>,
    pub last_disconnected_at_ms: Option<i64>,
    pub last_seen_at_ms: Option<i64>,
    pub last_delivered_at_ms: Option<i64>,
    pub last_received_at_ms: Option<i64>,
}

/// Maps the [`MessageArrival::transport`] encoding (0/1 BLE direct/muled,
/// 2 relay, 3/4 LAN direct/muled) onto the coarse, privacy-preserving path
/// shown in connection history. Lives in core so both shells label an arrival
/// identically -- the mapping used to be copy-pasted per platform.
///
/// The muled encodings (1, 4) map to [`PeerConnectionTransport::Carried`]
/// rather than to the radio the last hop happened to use. That hop was between
/// us and the phone in the middle; the friend whose line this becomes may never
/// have been in range of us at all. The message-info sheet already draws this
/// distinction ("another device over BLE"), and connection history contradicting
/// it is exactly the kind of confident wrong answer this screen exists to stop
/// giving.
#[uniffi::export]
pub fn core_peer_transport_for_arrival(transport: u8) -> PeerConnectionTransport {
    match transport {
        0 => PeerConnectionTransport::Bluetooth,
        3 => PeerConnectionTransport::LocalWifi,
        1 | 4 => PeerConnectionTransport::Carried,
        _ => PeerConnectionTransport::ShorePass,
    }
}

/// Stable envelope identity for a stored message and, for replies, the
/// encrypted id of the quoted message. Legacy rows may have no reference.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct MessageReference {
    pub msg_id: Vec<u8>,
    pub reply_to_msg_id: Option<Vec<u8>>,
}

/// Where a stored message row lives (`chat_id`) and who authored it
/// (`sender_user_id`), keyed by stable envelope `msg_id` -- see
/// [`MessageStore::message_origin_by_msg_id`]. Both fields are needed by the
/// relay ack-decision path: the local storage convention makes their
/// comparison meaningful (a 1:1 incoming row has `chat_id ==
/// sender_user_id`; a group row has `chat_id = group id`, which never equals
/// a member's user id).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct MessageOrigin {
    pub chat_id: Vec<u8>,
    pub sender_user_id: Vec<u8>,
}

/// An accepted friend (DESIGN.md §6.2): the public half of someone else's
/// identity, imported from a scanned/pasted `FriendCard`. `user_id` is
/// derived the same way as one's own (`friend_card_user_id`), so it's a
/// stable primary key even though a display name can be edited later.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct Contact {
    pub user_id: Vec<u8>,
    pub name: String,
    pub sign_pk: Vec<u8>,
    pub agree_pk: Vec<u8>,
    pub relay_url: Option<String>,
    pub relay_token: Option<String>,
    /// A local-only nickname the user set for this contact (T16). Presentation
    /// only: it is NEVER written to a `FriendCard`, digest, or any wire format,
    /// and importing a friend card never overwrites it. `None`/blank means fall
    /// back to `name`. Defaulted so existing constructors need not pass it.
    #[uniffi(default = None)]
    pub nickname: Option<String>,
}

/// One contact's recorded rejection streak against their card's relay
/// endpoint (see `crate::contact_relay_health`).
///
/// Deliberately NOT a field on [`Contact`]: this is observed local health,
/// not part of the identity a friend card carries, and folding it into the
/// record every call site constructs would invite it into a wire format it
/// has no business in.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ContactRelayRejection {
    pub user_id: Vec<u8>,
    pub reject_streak: i64,
    pub rejected_at_ms: i64,
}

/// One contact endpoint's persisted transport-level failure streak.
///
/// Kept separate from [`ContactRelayRejection`] because silence proves that an
/// address is not answering, not that its credential was rejected. The shell
/// therefore rests this endpoint without falling back to another mailbox, but
/// can still surface a prolonged failure and resume the same rest after an app
/// restart. `endpoint_key` is a hash of URL plus token, never the credential.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ContactRelayUnreachable {
    pub user_id: Vec<u8>,
    pub endpoint_key: String,
    pub unreachable_streak: i64,
    pub unreachable_at_ms: i64,
}

/// One pairwise-stream lamport consumed by this device without a durable
/// `messages` row in that chat.
///
/// Receipts, profile updates, LAN endpoint hints, and other control envelopes
/// share the sender's chat lamport counter. Keeping their exact positions lets
/// the visible-gap scan distinguish a missing chat message from an intentional
/// control-message hole without inventing a broad high-water mark that could
/// hide a real loss.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct ConsumedHiddenLamport {
    pub sender_user_id: Vec<u8>,
    pub lamport: u64,
}

/// How far a relay mailbox has been walked, how far the sweep now under way
/// has got, and when it was last walked in full. See [`crate::relay_cursor`]
/// for what the three numbers mean and the rules that move them.
///
/// An unknown mailbox reads as all zeroes — walk everything, no sweep under
/// way, and a sweep is due. That is the correct answer for a first run, for a
/// rotated credential, and for a restored backup alike, so no caller needs to
/// special-case any of them.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct RelayFetchCursor {
    /// The highest relay row id whose page was fully processed. A normal
    /// pass resumes its `after=` here.
    pub after_id: i64,
    /// When a walk from 0 last completed for this mailbox, or 0 if never.
    pub last_sweep_at_ms: i64,
    /// How far the sweep currently in progress has walked, or 0 when no sweep
    /// is part-way through.
    ///
    /// A sweep is bounded ([`crate::relay_mailbox_walk_action`]) and so
    /// usually spans several passes; this is the only thing that lets it
    /// resume rather than restart. It cannot be folded into `after_id`: the
    /// frontier never moves backwards, so on a mailbox already walked to the
    /// top it says nothing about where the sweep is.
    ///
    /// Non-zero also *means* a sweep is under way — [`crate::relay_sweep_due`]
    /// reads it that way — so it is cleared exactly when a sweep stops being
    /// under way: on the empty page that completes it, and on a hint-set
    /// change that invalidates the coverage it claims.
    pub sweep_after_id: i64,
    /// When the sweep now under way first got somewhere, or 0 when no sweep
    /// is part-way through (or when one is, but has not yet fully processed a
    /// page).
    ///
    /// A resume cursor is only as good as the id space it points into. A relay
    /// rebuilt from scratch restarts its row ids at 1, and a cursor remembered
    /// from the old id space then points past the end of the new mailbox: the
    /// resumed walk fetches one empty page, reads it as end-of-mailbox, and
    /// records a sweep that covered nothing. This timestamp is how
    /// [`crate::relay_sweep_restart_from_zero`] tells a sweep that yielded a
    /// second ago — whose empty page is simply the end of the mailbox — from
    /// one that has been stalled across days offline, which is the case in
    /// which the mailbox underneath it can have been replaced.
    pub sweep_started_at_ms: i64,
}

/// The name to show for a contact: the local nickname when the user has set a
/// non-blank one (T16), otherwise the card `name`. Kept in core so both shells
/// resolve identically everywhere a contact name is displayed.
#[uniffi::export]
pub fn core_contact_display_name(contact: Contact) -> String {
    contact_display_name(&contact)
}

/// Borrowing form of [`core_contact_display_name`], for core-internal callers
/// that would otherwise clone a whole contact per comparison.
pub(crate) fn contact_display_name(contact: &Contact) -> String {
    match contact.nickname.as_deref().map(str::trim) {
        Some(nickname) if !nickname.is_empty() => nickname.to_string(),
        _ => contact.name.clone(),
    }
}

/// The last authenticated friends-of-friends policy advertised by a contact.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ContactDiscoveryPolicy {
    pub user_id: Vec<u8>,
    pub protocol_version: u8,
    pub enabled: bool,
    pub revision: u64,
}

/// One candidate/source pair. Callers group rows with the same candidate
/// UserID to present all known mutual friends.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct FriendSuggestion {
    pub candidate: SuggestedFriendCard,
    pub introducer_user_id: Vec<u8>,
    pub ticket: IntroductionTicket,
    /// 0 = available, 1 = requested, 2 = hidden.
    pub state: u8,
}

/// How an accepted contact first entered the local trust graph.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ContactProvenance {
    pub user_id: Vec<u8>,
    /// 0 = direct QR/link, 1 = introduced by another accepted contact,
    /// 2 = added from a shared contact card (specs/share-contact.md).
    pub source: u8,
    pub introducer_user_id: Option<Vec<u8>>,
    pub introduced_at_ms: i64,
    /// Were we standing next to this person when we accepted them? True for a
    /// camera QR scan (co-presence by construction) and for any add where the
    /// peer was in the live nearby set at the time.
    ///
    /// Deliberately not inferred from [`ContactProvenance::source`]: `source =
    /// 0` conflates an in-person scan with a card pasted from an aeroplane,
    /// and those two carry opposite expectations about whether internet
    /// delivery was ever part of the deal. Stores written before this field
    /// existed read as `false` -- unknown, so say the true thing rather than
    /// assume an in-person encounter we have no record of.
    pub added_nearby: bool,
}

/// An inbound friend request that originated from a shared contact card and
/// is waiting for this user's explicit decision (specs/share-contact.md).
/// Everything needed to build the Contact on accept, held outside `contacts`
/// until then.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct PendingSharedRequest {
    pub requester_user_id: Vec<u8>,
    pub name: String,
    pub sign_pk: Vec<u8>,
    pub agree_pk: Vec<u8>,
    pub relay_url: Option<String>,
    pub relay_token: Option<String>,
    pub sharer_user_id: Vec<u8>,
    pub expires_at_ms: i64,
    pub first_seen_ms: i64,
    /// When this request last raised a prompt; 0 = never. Gates the
    /// one-prompt-per-requester-per-day rule.
    pub last_prompted_ms: i64,
}

/// Dismissal state for one requester's shared-card prompts. Survives the
/// pending row it came from.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct SharedRequestDismissal {
    pub requester_user_id: Vec<u8>,
    pub count: u32,
    /// Once true ("Don't ask again"), matching requests are dropped before
    /// any prompt. Cleared only by directly scanning that person's own code.
    pub suppressed: bool,
}

/// The requester's record of a shared-card request it sent, so the UI can
/// honestly distinguish "waiting" from "didn't respond" (past expiry).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct OutgoingSharedRequest {
    pub candidate_user_id: Vec<u8>,
    pub expires_at_ms: i64,
    pub sent_at_ms: i64,
}

/// One entry of a per-chat sync digest (DESIGN.md §7.3): "I have `sender_user_id`'s
/// messages in this chat contiguously through `through_lamport`."
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct DigestEntry {
    pub sender_user_id: Vec<u8>,
    pub through_lamport: u64,
}

/// One member's delivered/read watermarks for messages authored by a given
/// sender in a group (D9). `added_at_ms` is 0 for founding members (or
/// members imported before this column existed); a later joiner has the
/// wall-clock of the upsert that first listed them.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct GroupMemberReceipt {
    pub member_user_id: Vec<u8>,
    pub delivered_through: u64,
    pub read_through: u64,
    pub delivered_via_transport: Option<u8>,
    pub added_at_ms: i64,
}

/// Per-member group receipt snapshot used to derive the aggregate tick
/// (`✓✓` = every eligible current member is at or above the message lamport).
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct GroupReceiptState {
    pub members: Vec<GroupMemberReceipt>,
}

/// A sealed envelope this node is muling for someone else (DESIGN.md §5.3
/// carry queue): a foreign envelope we couldn't open (so it isn't for us) but
/// hold on to, to hand to its recipient when we next meet them. These are the
/// §6.4 public-header fields plus the opaque sealed blob -- everything needed
/// to reconstruct the exact `0x02` frame for onward delivery (see
/// [`crate::encode_envelope_frame`]). The internal eviction bookkeeping
/// (is-family, received-at, size) is deliberately *not* on this record: it's
/// an implementation detail of the store's budget enforcement, not something
/// the transport layer needs when it pulls an envelope back out to send.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CarriedEnvelope {
    pub msg_id: Vec<u8>,
    pub hop_ttl: u8,
    pub expiry: i64,
    pub recipient_hint: Vec<u8>,
    pub sealed: Vec<u8>,
}

/// A resume point in the carry queue's `(received_at, msg_id)` order --
/// "everything at or before this row has already been offered to this peer
/// during this link session".
///
/// Both fields together, because `received_at` alone is not unique: two
/// envelopes accepted in the same millisecond would otherwise let a cursor
/// either skip one or re-offer one forever. `msg_id` is the table's primary
/// key, so the pair is a total order over the queue and matches the
/// `ORDER BY received_at ASC, msg_id ASC` every carried query already uses.
///
/// This is offering bookkeeping only. It never removes anything: a carried
/// copy is still dropped only on digest-proof of receipt
/// ([`MessageStore::core_confirm_carried_deliveries`]), eviction, or expiry.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreCarriedCursor {
    pub received_at: i64,
    pub msg_id: Vec<u8>,
}

/// One page of [`MessageStore::carried_envelopes_for_peer_sync`].
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CoreCarriedSyncPage {
    pub rows: Vec<CarriedEnvelope>,
    /// Resume point for the next page: the last row on this one, or `None`
    /// if the page is empty (nothing to resume past).
    pub next: Option<CoreCarriedCursor>,
    /// Whether the scan reached the tail of the queue rather than stopping on
    /// the byte budget or the row ceiling. `true` means the walk is complete:
    /// everything this peer is eligible to be offered has now been offered.
    pub exhausted: bool,
}

/// Envelope-count ceiling for one round of carried paging, alongside the byte
/// budget.
///
/// The byte budget alone bounds the *volume* of a round but not its *frame
/// count*, and on the transports this traffic actually rides the frame count is
/// what hurts. A typing indicator or a receipt seals to a few dozen bytes, so a
/// courier holding hundreds of them clears a 256 KiB budget without ever
/// approaching it, and every one of those envelopes is a separate fragmented
/// write into a Bluetooth link's single FIFO. That queue is shared with live
/// mail to real contacts, and the far side must process each frame on its
/// receive path before the next one lands.
///
/// 64 is chosen to sit under the point where a round monopolizes a link:
/// at BLE's practical throughput a 64-frame round drains in a couple of
/// seconds, which fits comfortably inside the 3-5 minute re-digest interval
/// that offers the next page. It is also well under the number of ids a peer
/// can advertise back in one DIGEST, so a full page is still confirmable in a
/// single exchange rather than trickling proof across several.
///
/// Like the byte budget, this bounds only what is OFFERED: the carry queue is
/// untouched, the cursor advances past what was offered, and the next round
/// resumes behind it, so a backlog is paced rather than dropped.
pub const DEFAULT_CARRIED_PAGE_MAX_ROWS: u32 = 64;

/// The row ceiling the shells pass to the carried paging calls. Exported as a
/// function because UniFFI has no constants, so neither shell can drift from
/// [`DEFAULT_CARRIED_PAGE_MAX_ROWS`].
#[uniffi::export]
pub fn core_carried_page_max_rows() -> u32 {
    DEFAULT_CARRIED_PAGE_MAX_ROWS
}

/// Home chat-list preview for one chat (G1): last visible message, unread, and
/// own receipt watermarks — without marshaling the full message history.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CoreChatPreview {
    pub chat_id: Vec<u8>,
    pub last_message: Option<StoredMessage>,
    pub unread_count: u32,
    pub own_delivered_through: u64,
    pub own_read_through: u64,
    pub avatar_bytes: Option<Vec<u8>>,
}

/// One locally authored sealed envelope persisted for resend over BLE and
/// relay. This is the exact §6.4 public header plus sealed bytes, alongside
/// the local message metadata needed to query the queue by chat/sender/lamport
/// and to stage relay uploads later.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct OutboundEnvelope {
    pub msg_id: Vec<u8>,
    pub recipient_user_id: Vec<u8>,
    pub chat_id: Vec<u8>,
    pub sender_user_id: Vec<u8>,
    pub kind: u8,
    pub lamport: u64,
    pub timestamp: i64,
    pub hop_ttl: u8,
    pub expiry: i64,
    pub recipient_hint: Vec<u8>,
    pub sealed: Vec<u8>,
}

/// How many relay uploads are still queued for one recipient. Reported by
/// [`MessageStore::pending_relay_outbound_depth_by_recipient`] for diagnostics
/// exports, where a lopsided backlog is the signature of one unreachable
/// contact holding up the queue.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct RelayQueueDepth {
    pub recipient_user_id: Vec<u8>,
    pub queued: u64,
}

/// What one friend's outgoing mail actually looks like right now, in the terms
/// the connection details page needs to say something true about it.
///
/// Deliberately a separate record from [`RelayQueueDepth`] rather than more
/// fields on it. That record answers a diagnostic question -- how many rows are
/// still waiting for relay upload -- and the page's whole problem was that the
/// answer looks like a delivery failure when it is not: a phone with no Shore
/// Pass never stamps `relay_posted_at`, so its "backlog" is every message
/// written inside the retention window, forever, underneath a row that already
/// says the friend received one. Growing that record until it could serve both
/// purposes would have made every field mean "it depends".
///
/// Everything here is a fact, never a verdict. The classification lives in
/// `crate::connection_health`, which turns these numbers plus this device's own
/// path state into the line a person reads.
///
/// Scope, and why: **the pairwise conversation with this person only.** A
/// group message is queued once against the group id, not once per member, so
/// there is no per-member row to count in the first place -- and even if there
/// were, there would be nothing to clear it with: group wire receipts are
/// deferred, so no group message's delivery is ever confirmed per member.
/// Attributing group mail to members here would report every group message as
/// waiting for everyone until its envelope expired a week later, which is
/// precisely the permanent false warning this page exists to remove. What
/// cannot be proven is left out.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreRecipientDeliveryStatus {
    pub recipient_user_id: Vec<u8>,
    /// User-visible messages we authored to this person that their delivery
    /// receipt does not yet cover, and whose envelopes have not expired.
    /// Hidden control kinds (endpoint hints, profile sync, relay-change
    /// notices, reactions) share the same lamport stream and are excluded:
    /// they produce no chat row, so counting them would inflate the number a
    /// person reads against messages they can actually see.
    pub waiting_count: u64,
    /// When the oldest of those started waiting, epoch ms; `0` when none.
    /// Orders the Needs attention group and dates the delayed line.
    ///
    /// This device's queue time, deliberately, not the message's displayed
    /// timestamp: causal ordering floors an authored timestamp above
    /// everything already in the chat, so a peer with a fast clock can push
    /// ours forward and make a message that has been stuck for an hour read as
    /// newer than it is.
    pub oldest_waiting_ms: i64,
    /// The newest evidence that this person's mail is *moving*, epoch ms; `0`
    /// when nothing has ever moved.
    ///
    /// Defined as the later of the two things this store actually records as
    /// progress toward one recipient:
    ///
    /// * the newest successful relay upload for them
    ///   (`outbound_envelopes.relay_posted_at`, stamped only by an accepted
    ///   POST), and
    /// * the newest delivery confirmation from them
    ///   (`peer_connection_summary.last_delivered_at_ms`, stamped when their
    ///   receipt comes back, on whichever transport carried it -- which is
    ///   also the only direct-link delivery evidence that exists, since a
    ///   message handed over Bluetooth leaves no other durable mark).
    ///
    /// Queueing is deliberately not progress: a person typing four more
    /// messages into a stuck conversation must not reset the delay clock.
    /// Nor is a receipt *watermark* on its own -- it carries no timestamp, so
    /// the confirmation event above is what dates it.
    pub last_progress_ms: i64,
    /// How many of [`waiting_count`](Self::waiting_count) this device has not
    /// yet handed to Shore Pass (`relay_posted_at IS NULL`).
    ///
    /// The difference between "we still have work to do" and "we have done
    /// everything we can and the other phone has not collected yet". A
    /// successful upload is *terminal* progress for this device: nothing
    /// further happens on this side until either their receipt comes back or
    /// the two phones meet. So a count of zero here, with messages still
    /// waiting, is the ordinary store-and-forward case -- a friend who is
    /// asleep, ashore, or simply not syncing -- and must never be reported as
    /// a stall. A count above zero is the case where this phone's own queue is
    /// genuinely not moving.
    ///
    /// On a phone with no pass, or for a friend whose card carries no
    /// endpoint, nothing is ever posted, so this equals `waiting_count`. That
    /// is correct and harmless: the delayed window is only consulted while a
    /// route is usable, and without an endpoint the only usable route is a
    /// live link -- where work really should be moving.
    pub unposted_waiting_count: u64,
    /// A waiting envelope is larger than any transport will carry (the sealed
    /// ceiling is enforced identically by the relay and by peer framing), so
    /// retrying can never deliver it.
    pub oversized_waiting: bool,
    /// Persisted contact-endpoint health, straight from `contacts` (see
    /// `crate::contact_relay_health` for what the numbers mean). Passed
    /// through rather than interpreted here so one module owns the thresholds.
    pub relay_reject_streak: i64,
    pub relay_rejected_at_ms: i64,
    pub relay_unreachable_streak: i64,
    pub relay_unreachable_at_ms: i64,
}

/// User-visible choices for the content portion of an encrypted account
/// backup. Identity, contacts, groups, cryptographic continuity and authored
/// Lamport high-water marks are always included by the platform payload/store
/// snapshot and cannot be disabled.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackupContentOptions {
    /// Visible conversations plus their receipt and pending-send state.
    pub include_message_history: bool,
    /// Encrypted courier cargo held temporarily for other people. This is
    /// deliberately off by default: it can be large and a restored copy has
    /// weaker delivery-progress evidence than the live carrier did.
    pub include_pending_deliveries_for_others: bool,
}

impl Default for BackupContentOptions {
    fn default() -> Self {
        Self {
            include_message_history: true,
            include_pending_deliveries_for_others: false,
        }
    }
}

/// Redacted inventory shown before exporting or installing a backup. Byte
/// counts cover encrypted/message payload bytes rather than SQLite overhead,
/// so they are useful, stable estimates rather than promises about final file
/// size after encryption.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupInventory {
    pub contact_count: u64,
    pub group_count: u64,
    pub message_count: u64,
    pub message_bytes: u64,
    pub pending_own_delivery_count: u64,
    pub pending_own_delivery_bytes: u64,
    pub pending_courier_delivery_count: u64,
    pub pending_courier_delivery_bytes: u64,
}

/// What the core removed while preparing a snapshot or making a legacy
/// full-database restore safe. Counts are intentionally content-free so the
/// report is safe to log and include in diagnostics.
#[derive(uniffi::Record, Clone, Debug, Default, PartialEq, Eq)]
pub struct BackupSanitizationReport {
    pub removed_message_count: u64,
    pub removed_pending_own_delivery_count: u64,
    pub removed_courier_delivery_count: u64,
    pub removed_expired_delivery_count: u64,
    pub removed_connection_event_count: u64,
}

/// One persisted outgoing receipt envelope for relay upload and retry.
/// Unlike [`OutboundEnvelope`], this queue is keyed by the cumulative receipt
/// watermark rather than the chat lamport stream, so `through_lamport` is the
/// semantic identity that advances while `msg_id` stays stable for a given
/// watermark.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct OutgoingReceiptEnvelope {
    pub msg_id: Vec<u8>,
    pub recipient_user_id: Vec<u8>,
    pub chat_id: Vec<u8>,
    pub sender_user_id: Vec<u8>,
    pub receipt_type: u8,
    pub through_lamport: u64,
    pub timestamp: i64,
    pub hop_ttl: u8,
    pub expiry: i64,
    pub recipient_hint: Vec<u8>,
    pub sealed: Vec<u8>,
}

#[derive(uniffi::Object)]
pub struct MessageStore {
    pub(crate) conn: Mutex<Connection>,
    /// FC2 test-only instrumentation: counts how many `sealed` envelope
    /// blobs the digest-spray queries
    /// ([`MessageStore::carried_envelopes_for_peer_sync`],
    /// [`MessageStore::outbound_envelopes_after_budgeted`]) actually
    /// materialize into Rust memory. Those queries push `known`/`expiry`
    /// exclusion (and, for the outbound spray, the shared byte budget) into
    /// the SQL `WHERE` clause so a row that's already known to the peer, or
    /// that would overflow the budget, is never decoded -- this counter is
    /// how the regression tests prove that rather than just asserting on
    /// the (identical either way) final selection. Not exported via UniFFI;
    /// compiled out of non-test builds.
    #[cfg(test)]
    pub(crate) sealed_reads: std::sync::atomic::AtomicU64,
}

/// Transaction-scoped body of [`MessageStore::record_peer_connection_event`], so a
/// store write that already holds a transaction can record its evidence in
/// the same atomic step instead of reaching back through `&self` for a
/// second lock on the same connection (which would deadlock).
fn record_peer_connection_event_tx(
    tx: &Transaction<'_>,
    user_id: &[u8],
    transport: PeerConnectionTransport,
    kind: PeerConnectionEventKind,
    occurred_at_ms: i64,
) -> Result<(), CoreError> {
    let transport_value = peer_transport_value(transport);
    let kind_value = peer_event_kind_value(kind);
    tx.execute(
        "INSERT INTO peer_connection_events
            (user_id, transport, kind, occurred_at_ms)
         SELECT ?1, ?2, ?3, ?4
         WHERE NOT EXISTS (
            SELECT 1 FROM peer_connection_events
            WHERE user_id = ?1 AND transport = ?2 AND kind = ?3
              AND occurred_at_ms >= ?4 - 30000
         )",
        params![&user_id, transport_value, kind_value, occurred_at_ms],
    )
    .map_err(store_err)?;
    tx.execute(
        "INSERT INTO peer_connection_summary
            (user_id, transport, last_connected_at_ms,
             last_disconnected_at_ms, last_seen_at_ms, last_delivered_at_ms,
             last_received_at_ms)
         VALUES (
            ?1, ?2,
            CASE WHEN ?3 = 0 THEN ?4 END,
            CASE WHEN ?3 = 1 THEN ?4 END,
            CASE WHEN ?3 = 2 THEN ?4 END,
            CASE WHEN ?3 = 3 THEN ?4 END,
            CASE WHEN ?3 = 4 THEN ?4 END
         )
         ON CONFLICT(user_id, transport) DO UPDATE SET
            last_connected_at_ms = COALESCE(
                MAX(last_connected_at_ms, excluded.last_connected_at_ms),
                last_connected_at_ms, excluded.last_connected_at_ms),
            last_disconnected_at_ms = COALESCE(
                MAX(last_disconnected_at_ms, excluded.last_disconnected_at_ms),
                last_disconnected_at_ms, excluded.last_disconnected_at_ms),
            last_seen_at_ms = COALESCE(
                MAX(last_seen_at_ms, excluded.last_seen_at_ms),
                last_seen_at_ms, excluded.last_seen_at_ms),
            last_delivered_at_ms = COALESCE(
                MAX(last_delivered_at_ms, excluded.last_delivered_at_ms),
                last_delivered_at_ms, excluded.last_delivered_at_ms),
            last_received_at_ms = COALESCE(
                MAX(last_received_at_ms, excluded.last_received_at_ms),
                last_received_at_ms, excluded.last_received_at_ms)",
        params![&user_id, transport_value, kind_value, occurred_at_ms],
    )
    .map_err(store_err)?;
    tx.execute(
        "DELETE FROM peer_connection_events WHERE occurred_at_ms < ?1",
        params![occurred_at_ms.saturating_sub(30 * 24 * 60 * 60 * 1000)],
    )
    .map_err(store_err)?;
    tx.execute(
        "DELETE FROM peer_connection_events
         WHERE id NOT IN (
            SELECT id FROM peer_connection_events
            ORDER BY occurred_at_ms DESC, id DESC LIMIT 1000
         )",
        [],
    )
    .map_err(store_err)?;
    Ok(())
}

/// Does a delivered watermark moving from `previous_through` to
/// `effective_through` newly prove that a message *a person can see*
/// reached the peer?
///
/// Three conditions, all necessary:
/// - the watermark **strictly advanced**. A duplicate or replayed receipt
///   re-covers lamports already proved delivered and proves nothing new; so
///   does a restored backup replaying receipts against a watermark the
///   restored `receipts` row already holds.
/// - the newly covered range contains a row in this chat **authored by
///   us** (`sender_user_id` is our own id on the receipt path -- a receipt
///   only ever acks messages we wrote).
/// - at least one such row is a **visible** kind, by
///   [`crate::core_is_visible_chat_kind`] -- the same single predicate the
///   inbound direction and both chat screens use. Hidden service kinds
///   (profile sync, friend directory, LAN endpoint hints, relay-change
///   notices) get `messages` rows and advance the peer's watermark exactly
///   like real messages do, so without this the screen reports a delivery
///   for traffic no human ever authored.
///
/// The scan is bounded by the newly covered range and rides the
/// `UNIQUE(chat_id, sender_user_id, lamport)` index; it stops at the first
/// visible row.
fn receipt_newly_covers_visible_authored_message(
    tx: &Transaction<'_>,
    chat_id: &[u8],
    sender_user_id: &[u8],
    previous_through: u64,
    effective_through: u64,
) -> Result<bool, CoreError> {
    if effective_through <= previous_through {
        return Ok(false);
    }
    let mut stmt = tx
        .prepare(
            "SELECT kind FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2
               AND lamport > ?3 AND lamport <= ?4
             ORDER BY lamport ASC",
        )
        .map_err(store_err)?;
    let mut rows = stmt
        .query(params![
            chat_id,
            sender_user_id,
            previous_through as i64,
            effective_through as i64
        ])
        .map_err(store_err)?;
    while let Some(row) = rows.next().map_err(store_err)? {
        let kind: i64 = row.get(0).map_err(store_err)?;
        if u8::try_from(kind).is_ok_and(crate::core_is_visible_chat_kind) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Record the outbound half of the Connection details evidence -- "they
/// received your message" -- for a delivered receipt that
/// [`receipt_newly_covers_visible_authored_message`] accepts.
///
/// The peer named is `chat_id`, which on the 1:1 receipt path is the friend
/// who sent the receipt. A chat id that is not an accepted contact (a
/// group, or someone since removed) records nothing: the screen only ever
/// lists friends, so an event for anyone else could never be shown against
/// a name -- the same skip the inbound direction makes.
///
/// An unknown return route claims no path rather than guessing one.
/// [`PeerConnectionTransport::Carried`] is how this store already spells
/// "evidence, but no path we observed", and surfaces render it without a
/// "via ..." clause.
fn record_delivered_evidence(
    tx: &Transaction<'_>,
    chat_id: &[u8],
    sender_user_id: &[u8],
    previous_through: u64,
    effective_through: u64,
    via_transport: Option<u8>,
    received_at_ms: i64,
) -> Result<(), CoreError> {
    if received_at_ms < 0 {
        return Ok(());
    }
    if !receipt_newly_covers_visible_authored_message(
        tx,
        chat_id,
        sender_user_id,
        previous_through,
        effective_through,
    )? {
        return Ok(());
    }
    let is_contact: bool = tx
        .query_row(
            "SELECT 1 FROM contacts WHERE user_id = ?1",
            params![chat_id],
            |_| Ok(true),
        )
        .optional()
        .map_err(store_err)?
        .unwrap_or(false);
    if !is_contact {
        return Ok(());
    }
    let transport = match via_transport {
        Some(value) => core_peer_transport_for_arrival(value),
        None => PeerConnectionTransport::Carried,
    };
    record_peer_connection_event_tx(
        tx,
        chat_id,
        transport,
        PeerConnectionEventKind::MessageDelivered,
        received_at_ms,
    )
}

#[uniffi::export]
impl MessageStore {
    /// Open (creating if needed) the message store at `path`. Pass
    /// `":memory:"` for an ephemeral in-process store.
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Self, CoreError> {
        let mut conn = Connection::open(&path).map_err(store_err)?;
        // FC10: no pragmas were set before this, so every store opened with
        // SQLite's rollback-journal default -- every write fsyncs the
        // journal, and `backup_to`'s `VACUUM INTO` (which runs under the
        // same `Mutex<Connection>` as every other store call) blocked the
        // whole store for as long as the backup took. WAL lets readers
        // proceed against the last-committed snapshot while a writer is
        // mid-transaction and persists on the database file itself (so it
        // survives reopen); `synchronous=NORMAL` is the mode SQLite's own
        // docs call safe under WAL (fsync only at checkpoint, not every
        // commit); `busy_timeout` gives a brief retry window instead of an
        // immediate `SQLITE_BUSY` in the rare case something outside this
        // `Mutex` (a manual DB inspection, a crashed prior process holding
        // the lock a moment longer) contends for the same file. None of
        // this applies to `:memory:` stores -- SQLite doesn't support WAL
        // there and silently keeps the in-memory journal mode instead.
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(store_err)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(store_err)?;
        conn.pragma_update(None, "busy_timeout", 5_000)
            .map_err(store_err)?;
        conn.execute_batch(SCHEMA).map_err(store_err)?;
        conn.execute_batch(crate::protocol_event::PROTOCOL_EVENT_SCHEMA_SQL)
            .map_err(store_err)?;
        migrate_delivery_metrics_schema(&conn)?;
        ensure_contact_column(&conn, "relay_token", "TEXT")?;
        ensure_contact_column(&conn, "avatar", "BLOB")?;
        ensure_contact_column(&conn, "avatar_epoch", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_contact_column(&conn, "nickname", "TEXT")?;
        // T23 relay-change propagation: the monotonic epoch of the newest
        // relay endpoint applied for this contact. Older stores predate it,
        // and 0 is the right default -- any notice a contact sends carries a
        // wall-clock-seeded epoch well above it, so the first notice after an
        // upgrade always applies.
        ensure_contact_column(&conn, "relay_epoch", "INTEGER NOT NULL DEFAULT 0")?;
        // Consecutive authoritative rejections from this contact's card
        // endpoint, and when the streak last advanced (see
        // `crate::contact_relay_health`). 0 is the right default for an
        // existing store: an endpoint is only written off by observing it
        // fail, never by assumption, so every contact starts trusted and the
        // very next sync pass re-establishes the truth.
        ensure_contact_column(&conn, "relay_reject_streak", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_contact_column(&conn, "relay_rejected_at", "INTEGER NOT NULL DEFAULT 0")?;
        // Transport-level failures (DNS, refused connection, TLS) need their
        // own persisted streak: they rest rather than fall back, and survive a
        // restart without being mistaken for an authoritative rejection.
        ensure_contact_column(&conn, "relay_unreachable_endpoint_key", "TEXT")?;
        ensure_contact_column(
            &conn,
            "relay_unreachable_streak",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_contact_column(&conn, "relay_unreachable_at", "INTEGER NOT NULL DEFAULT 0")?;
        // Relay proxy-polling (see enqueue_relay_carried_envelope): marks a
        // carried envelope as one we pulled FROM the relay rather than one we
        // received over BLE, so the relay-upload query can skip re-uploading
        // it. Older on-disk stores predate the column.
        ensure_column(
            &conn,
            "carried_envelopes",
            "from_relay",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "carried_envelopes", "content_digest", "BLOB")?;
        // Upload-side twin of the relay fetch cursor: the relay URL this
        // carried envelope was last confirmed present on -- stamped by a
        // successful upload or by fetching the same `msg_id` back off a
        // relay; NULL = never confirmed anywhere. Consulted ONLY by
        // `family_carried_envelopes` (the relay-upload query); no removal
        // path reads it -- a carried envelope is still dropped only on
        // digest-proof of receipt, EVICT-01 foreign-pressure eviction, or
        // expiry. Without it every
        // sync pass re-posted the same head-of-queue envelopes for their
        // whole seven-day life, burning the family's shared relay rate
        // budget and starving rows behind the batch limit of their first
        // upload.
        ensure_column(&conn, "carried_envelopes", "relay_uploaded_to", "TEXT")?;
        migrate_carried_content_digests(&mut conn)?;
        conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_carried_content_digest
             ON carried_envelopes(content_digest)",
            [],
        )
        .map_err(store_err)?;
        // FC7: supports the relay-upload query in `family_carried_envelopes`
        // (`WHERE is_family = 1 AND from_relay = 0 AND relay_uploaded_to IS
        // NULL AND expiry > ?1 ORDER BY received_at ASC, msg_id ASC`).
        // Created here (after `from_relay` is
        // ensured above) rather than in SCHEMA, since an older on-disk store
        // won't have that column yet when SCHEMA's CREATE TABLE IF NOT EXISTS
        // runs. `expiry` (a range predicate) and `relay_uploaded_to` (added
        // later; almost every candidate row is NULL anyway) are applied as
        // cheap residual filters per row rather than indexed:
        // for `expiry`, SQLite can use a leading index column for
        // either a range filter or to satisfy ORDER BY, not both at once --
        // putting it before `received_at` still leaves the ORDER BY needing
        // a temp b-tree sort (verified empirically). With only the two
        // equality columns leading, followed by both ORDER BY columns in
        // their query order, the index hands back rows already fully sorted
        // and `expiry > ?1` is applied as a cheap residual filter per row.
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_carried_family_upload
             ON carried_envelopes(is_family, from_relay, received_at, msg_id)",
            [],
        )
        .map_err(store_err)?;
        // Whether the peer was standing next to us when we accepted them (see
        // `ContactProvenance::added_nearby`). Existing rows predate the field
        // and default to 0, which reads as "no record of an in-person
        // encounter" -- the honest default, since it only ever suppresses a
        // warning we would otherwise be right to show.
        ensure_column(
            &conn,
            "contact_provenance",
            "added_nearby",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        // Inbound-arrival rollup. Stores written before this column existed
        // simply have no inbound evidence to show, which is the honest
        // reading: nothing recorded it back then.
        ensure_column(
            &conn,
            "peer_connection_summary",
            "last_received_at_ms",
            "INTEGER",
        )?;
        // Sweep resume cursor. Stores written before this column existed have
        // no sweep in progress to resume, and 0 is exactly that reading -- so
        // an upgrade starts its next sweep at the beginning, once, and every
        // sweep after that resumes properly.
        ensure_column(
            &conn,
            "relay_fetch_cursors",
            "sweep_after_id",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        // When the sweep now under way first got somewhere. 0 on an older
        // store reads as "no sweep under way that I can date", which costs
        // nothing: the first page of the next sweep stamps it.
        ensure_column(
            &conn,
            "relay_fetch_cursors",
            "sweep_started_at",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(&conn, "messages", "arrival_transport", "INTEGER")?;
        ensure_column(&conn, "receipts", "via_transport", "INTEGER")?;
        ensure_column(&conn, "messages", "hops_taken", "INTEGER")?;
        ensure_column(&conn, "messages", "received_at", "INTEGER")?;
        ensure_column(&conn, "messages", "msg_id", "BLOB")?;
        ensure_column(&conn, "messages", "reply_to_msg_id", "BLOB")?;
        ensure_column(&conn, "messages", "outbound_expiry", "INTEGER")?;
        ensure_column(
            &conn,
            "groups",
            "metadata_revision",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "groups",
            "metadata_changed_by",
            "BLOB NOT NULL DEFAULT X''",
        )?;
        ensure_column(
            &conn,
            "group_members",
            "added_at_ms",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        // Older stores already have stable ids for locally authored rows in
        // the outbound queue. Backfill those so they can be quoted after an
        // upgrade; received legacy rows cannot be recovered retroactively.
        conn.execute(
            "UPDATE messages
             SET msg_id = (
                 SELECT outbound_envelopes.msg_id
                 FROM outbound_envelopes
                 WHERE outbound_envelopes.chat_id = messages.chat_id
                   AND outbound_envelopes.sender_user_id = messages.sender_user_id
                   AND outbound_envelopes.lamport = messages.lamport
                 ORDER BY outbound_envelopes.queued_at ASC
                 LIMIT 1
             )
             WHERE msg_id IS NULL
               AND EXISTS (
                   SELECT 1 FROM outbound_envelopes
                   WHERE outbound_envelopes.chat_id = messages.chat_id
                     AND outbound_envelopes.sender_user_id = messages.sender_user_id
                     AND outbound_envelopes.lamport = messages.lamport
               )",
            [],
        )
        .map_err(store_err)?;
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_chat_msg_id
             ON messages(chat_id, msg_id)",
            [],
        )
        .map_err(store_err)?;
        // Unlike the composite index above, `message_origin_by_msg_id` looks
        // a `msg_id` up with no `chat_id` in hand (a relay-fetched envelope
        // knows only its own `msg_id`), so it needs `msg_id` leading an index
        // on its own to avoid a full table scan.
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_msg_id ON messages(msg_id)",
            [],
        )
        .map_err(store_err)?;
        // Receipt-coverage catch-up (#283, contract `QUEUE-01`). Every store
        // written before receipt-coverage retirement existed carries rows the
        // recipient acknowledged long ago -- 2,143 of 3,964 on the field store
        // that prompted the issue -- and the incremental hook in
        // `record_receipt` can only ever act on receipts that arrive from now
        // on. This is the sweep that reaches the rest.
        //
        // It sits in `open` rather than behind a one-shot migration flag on
        // purpose. It is driven from `receipts`, which holds one row per chat
        // per receipt type, so its cost is set by how many people this device
        // talks to and not at all by queue size; after the first run it finds
        // nothing and costs a handful of index seeks. Running it every open
        // therefore buys a standing safety net -- a watermark advanced by a
        // path that somehow bypassed the hook, or a database restored from a
        // backup taken before this build -- for a price that does not grow.
        // Forward-only, and no schema change: no column and no index is added
        // or repurposed, so an older build reading this store afterwards sees
        // a smaller queue and nothing else.
        let retired = crate::outbound_retirement::sweep_receipt_covered(&conn)?;
        // Only when it found something. The catch-up sweep runs on every open
        // and, after the first one, retires nothing for the rest of the
        // device's life -- so recording the empty case would put one record per
        // launch into the ring, and would leave a brand-new install with
        // "diagnostics captured" showing on a screen where nothing had
        // happened yet.
        if retired > 0 {
            crate::protocol_event::note(
                &conn,
                &[crate::protocol_event::ProtocolEventDraft::new(
                    crate::protocol_event::ProtocolEventCode::OutboundQueueScanned,
                    0,
                    "receipt_covered_rows_retired",
                )
                .invariants(&["QUEUE-01"])
                .count("rows_retired", i64::try_from(retired).unwrap_or(i64::MAX))],
            );
        }
        Ok(Self {
            conn: Mutex::new(conn),
            #[cfg(test)]
            sealed_reads: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// Record a bounded, metadata-only connection event for an accepted peer.
    /// Identical high-frequency signals are coalesced for 30 seconds; detailed
    /// events are retained for 30 days and capped at 1,000 rows.
    pub fn record_peer_connection_event(
        &self,
        user_id: Vec<u8>,
        transport: PeerConnectionTransport,
        kind: PeerConnectionEventKind,
        occurred_at_ms: i64,
    ) -> Result<(), CoreError> {
        if user_id.is_empty() || user_id.len() > 128 {
            return Err(CoreError::Malformed("invalid peer user id".into()));
        }
        if occurred_at_ms < 0 {
            return Err(CoreError::Malformed(
                "connection event time cannot be negative".into(),
            ));
        }
        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        record_peer_connection_event_tx(&tx, &user_id, transport, kind, occurred_at_ms)?;
        tx.commit().map_err(store_err)
    }

    pub fn peer_connection_events(
        &self,
        user_id: Option<Vec<u8>>,
        limit: u32,
    ) -> Result<Vec<PeerConnectionEvent>, CoreError> {
        let limit = i64::from(limit.clamp(1, 500));
        let conn = lock_conn(&self.conn);
        let mut rows = Vec::new();
        if let Some(user_id) = user_id {
            let mut stmt = conn
                .prepare(
                    "SELECT user_id, transport, kind, occurred_at_ms
                     FROM peer_connection_events WHERE user_id = ?1
                     ORDER BY occurred_at_ms DESC, id DESC LIMIT ?2",
                )
                .map_err(store_err)?;
            let mapped = stmt
                .query_map(params![user_id, limit], row_to_peer_connection_event)
                .map_err(store_err)?;
            for row in mapped {
                rows.push(row.map_err(store_err)?);
            }
        } else {
            let mut stmt = conn
                .prepare(
                    "SELECT user_id, transport, kind, occurred_at_ms
                     FROM peer_connection_events
                     ORDER BY occurred_at_ms DESC, id DESC LIMIT ?1",
                )
                .map_err(store_err)?;
            let mapped = stmt
                .query_map(params![limit], row_to_peer_connection_event)
                .map_err(store_err)?;
            for row in mapped {
                rows.push(row.map_err(store_err)?);
            }
        }
        Ok(rows)
    }

    pub fn peer_connection_summaries(&self) -> Result<Vec<PeerConnectionSummary>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT user_id, transport, last_connected_at_ms,
                        last_disconnected_at_ms, last_seen_at_ms, last_delivered_at_ms,
                        last_received_at_ms
                 FROM peer_connection_summary
                 ORDER BY MAX(
                     COALESCE(last_received_at_ms, 0),
                     COALESCE(last_delivered_at_ms, 0),
                     COALESCE(last_seen_at_ms, 0),
                     COALESCE(last_connected_at_ms, 0),
                     COALESCE(last_disconnected_at_ms, 0)
                 ) DESC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(PeerConnectionSummary {
                    user_id: row.get(0)?,
                    transport: peer_transport_from_value(row.get(1)?)?,
                    last_connected_at_ms: row.get(2)?,
                    last_disconnected_at_ms: row.get(3)?,
                    last_seen_at_ms: row.get(4)?,
                    last_delivered_at_ms: row.get(5)?,
                    last_received_at_ms: row.get(6)?,
                })
            })
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    pub fn clear_peer_connection_history(&self) -> Result<(), CoreError> {
        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        tx.execute("DELETE FROM peer_connection_events", [])
            .map_err(store_err)?;
        tx.execute("DELETE FROM peer_connection_summary", [])
            .map_err(store_err)?;
        tx.commit().map_err(store_err)
    }

    /// Writes a transactionally consistent standalone SQLite snapshot.
    /// The destination must not already exist; callers should use a unique
    /// temporary path and remove it after reading the backup bytes.
    ///
    /// ## Courier state does not ride the backup
    ///
    /// User-owned messages, contacts, authored Lamport watermarks and receipts
    /// are history and come back exactly as they were. Courier ciphertext in
    /// `carried_envelopes` is different: it belongs to other recipients and a
    /// restored copy has no delivery-progress evidence. Restoring hundreds of
    /// those rows used to offer the whole stale backlog again on every new BLE
    /// link, immediately multiplying traffic across rotating peer addresses.
    ///
    /// Relay fetch cursors deliberately *do* ride the backup. Dropping a
    /// cursor forces an immediate walk from row zero; on a shared family
    /// mailbox that re-downloads the stale proxy mail we just discarded and
    /// can recreate the restore storm before the UI opens. The six-hour
    /// periodic sweep already repairs a stale frontier or rebuilt relay, so
    /// preserving it trades at most bounded delivery delay for avoiding an
    /// unbounded restore-time replay.
    pub fn backup_to(&self, destination: String) -> Result<(), CoreError> {
        self.backup_to_with_options(
            destination,
            BackupContentOptions::default(),
            current_unix_time_ms()?,
        )?;
        Ok(())
    }

    /// Return the redacted content inventory used by both mobile backup UIs.
    /// Expired queue rows are excluded because snapshot sanitation removes
    /// them regardless of which options the user selects.
    pub fn backup_inventory(&self, now_ms: i64) -> Result<BackupInventory, CoreError> {
        validate_backup_now(now_ms)?;
        backup_inventory_from_conn(&lock_conn(&self.conn), now_ms)
    }

    /// Write a transactionally consistent snapshot and apply the Rust-owned
    /// content policy to the copy. This is the canonical export path; mobile
    /// shells only choose a destination and collect preferences that live
    /// outside SQLite.
    pub fn backup_to_with_options(
        &self,
        destination: String,
        options: BackupContentOptions,
        now_ms: i64,
    ) -> Result<BackupSanitizationReport, CoreError> {
        validate_backup_now(now_ms)?;
        let destination = std::path::Path::new(destination.trim());
        if !destination.is_absolute() {
            return Err(CoreError::Store(
                "backup destination must be an absolute path".into(),
            ));
        }
        match std::fs::symlink_metadata(destination) {
            Ok(_) => return Err(CoreError::Store("backup destination already exists".into())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CoreError::Store(format!(
                    "cannot inspect backup destination: {error}"
                )))
            }
        }
        if !destination.parent().is_some_and(std::path::Path::is_dir) {
            return Err(CoreError::Store(
                "backup destination parent is not a directory".into(),
            ));
        }
        let conn = lock_conn(&self.conn);
        conn.execute("VACUUM INTO ?1", params![destination.to_string_lossy()])
            .map_err(store_err)?;
        // See the doc comment: scrub the copy, never the live store. Restore
        // repeats this operation so legacy full-database `.cmbak` files made
        // before this policy are safe too.
        let mut snapshot = Connection::open(destination).map_err(store_err)?;
        let report = sanitize_restore_contents(&mut snapshot, options, now_ms)?;
        Ok(report)
    }

    /// Insert a message from a remote sender's stream, merging metadata from a
    /// true duplicate while failing closed when two different authenticated
    /// messages claim the same stream position.
    ///
    /// A conflict on `(chat_id, sender_user_id, lamport)` is ambiguous on its
    /// own: it could be a digest resend or relay copy of a message we already
    /// have (same sealed content, arriving twice -- expected under DTN and
    /// harmless to ignore), or it could be a sender who reset their stream
    /// (e.g. deleted the chat and re-added the contact, per DESIGN.md §7.1's
    /// "own outgoing stream has no gaps" -- deleting local history restarts
    /// their lamport counter at 1) and is now re-using a lamport number we
    /// already hold from their *old* stream for a genuinely new message. This
    /// method tells the two apart by comparing the existing row's
    /// `(timestamp, kind, payload)` against the incoming one:
    ///
    /// - **Identical** on all three -> true duplicate. No-op, returns `Ok(false)`,
    ///   same behavior as the old plain `INSERT OR IGNORE`.
    /// - **Different** -> ambiguous conflict. Keep the existing branch and all
    ///   receipt state, quarantine the private incoming content, and return
    ///   `Ok(false)`. Callers that must distinguish this from a duplicate
    ///   should use one of the classified incoming methods instead.
    ///   Timestamps cannot break the tie: they are sender wall-clock display
    ///   hints and may be wrong. A delayed courier or restored backup can
    ///   legitimately replay an older authenticated branch, while a confused
    ///   clock can make that stale branch appear newer. Automatic fork
    ///   recovery therefore requires a future protocol revision carrying an
    ///   explicit authenticated stream generation/epoch.
    pub fn insert_message(&self, message: StoredMessage) -> Result<bool, CoreError> {
        Ok(matches!(
            incoming_message_reference::insert(self, message, None, None, None)?,
            IncomingMessageInsertOutcome::Inserted
        ))
    }

    /// Insert an opened incoming message together with the envelope id used
    /// for quoting it and an optional encrypted reply target. `false` means
    /// either a true duplicate or a quarantined conflict; callers that log or
    /// otherwise act differently on those outcomes should use
    /// [`Self::insert_incoming_message_classified`].
    pub fn insert_incoming_message(
        &self,
        message: StoredMessage,
        msg_id: Vec<u8>,
        reply_to_msg_id: Option<Vec<u8>>,
    ) -> Result<bool, CoreError> {
        Ok(matches!(
            self.insert_incoming_message_classified(message, msg_id, reply_to_msg_id)?,
            IncomingMessageInsertOutcome::Inserted
        ))
    }

    /// Insert an opened incoming message without arrival-route evidence while
    /// preserving the full duplicate-versus-quarantine result. This covers
    /// local carry-queue drains where no live transport can truthfully be
    /// attributed to the original arrival.
    pub fn insert_incoming_message_classified(
        &self,
        message: StoredMessage,
        msg_id: Vec<u8>,
        reply_to_msg_id: Option<Vec<u8>>,
    ) -> Result<IncomingMessageInsertOutcome, CoreError> {
        validate_msg_id("msg_id", &msg_id)?;
        if let Some(reply_to_msg_id) = reply_to_msg_id.as_deref() {
            validate_msg_id("reply_to_msg_id", reply_to_msg_id)?;
        }
        incoming_message_reference::insert(self, message, Some(msg_id), reply_to_msg_id, None)
    }

    /// Insert an opened incoming message and atomically retain first-arrival
    /// transport evidence. If the stream position conflicts, the existing
    /// visible branch wins and the incoming branch plus its source is placed
    /// in the bounded conflict quarantine instead of being silently dropped.
    pub fn insert_incoming_message_with_arrival(
        &self,
        message: StoredMessage,
        msg_id: Vec<u8>,
        reply_to_msg_id: Option<Vec<u8>>,
        arrival: MessageArrival,
    ) -> Result<IncomingMessageInsertOutcome, CoreError> {
        validate_msg_id("msg_id", &msg_id)?;
        if let Some(reply_to_msg_id) = reply_to_msg_id.as_deref() {
            validate_msg_id("reply_to_msg_id", reply_to_msg_id)?;
        }
        validate_message_arrival(&arrival)?;
        incoming_message_reference::insert(
            self,
            message,
            Some(msg_id),
            reply_to_msg_id,
            Some(arrival),
        )
    }

    /// Newest quarantined stream conflicts, with identifiers and message
    /// bodies replaced by stable pseudonymous hashes.
    pub fn message_conflict_summaries(
        &self,
        limit: u32,
    ) -> Result<Vec<MessageConflictSummary>, CoreError> {
        let limit = i64::from(limit.clamp(1, MESSAGE_CONFLICT_QUARANTINE_LIMIT as u32));
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT chat_id, sender_user_id, lamport,
                        existing_fingerprint, incoming_fingerprint,
                        arrival_transport, first_seen_at, last_seen_at, seen_count
                 FROM message_conflicts
                 ORDER BY last_seen_at DESC, id DESC
                 LIMIT ?1",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![limit], |row| {
                let chat_id: Vec<u8> = row.get(0)?;
                let sender_user_id: Vec<u8> = row.get(1)?;
                let existing_fingerprint: Vec<u8> = row.get(3)?;
                let incoming_fingerprint: Vec<u8> = row.get(4)?;
                Ok(MessageConflictSummary {
                    chat_hash: hex_lower(&metric_chat_hash(&chat_id)),
                    sender_hash: hex_lower(&metric_sender_hash(&sender_user_id)),
                    lamport: row.get::<_, i64>(2)? as u64,
                    existing_fingerprint: hex_lower(&existing_fingerprint),
                    incoming_fingerprint: hex_lower(&incoming_fingerprint),
                    arrival_transport: row.get::<_, Option<i64>>(5)?.map(|value| value as u8),
                    first_seen_at_ms: row.get(6)?,
                    last_seen_at_ms: row.get(7)?,
                    seen_count: row.get::<_, i64>(8)? as u64,
                })
            })
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Quarantined stream-conflict metadata as a diagnostics-safe CSV.
    /// Message bodies, raw chat ids, raw sender ids, and message ids never
    /// leave the core. The quarantine is globally bounded, so exporting all
    /// retained rows is also bounded.
    pub fn export_message_conflicts_csv(&self) -> Result<String, CoreError> {
        let summaries =
            self.message_conflict_summaries(MESSAGE_CONFLICT_QUARANTINE_LIMIT as u32)?;
        let mut out = String::from(
            "chat,sender,lamport,existing_fingerprint,incoming_fingerprint,arrival_transport,first_seen_at_ms,last_seen_at_ms,seen_count\n",
        );
        for summary in summaries {
            let arrival_transport = summary
                .arrival_transport
                .map(|value| value.to_string())
                .unwrap_or_default();
            out.push_str(&format!(
                "{},{},{},{},{},{},{},{},{}\n",
                summary.chat_hash,
                summary.sender_hash,
                summary.lamport,
                summary.existing_fingerprint,
                summary.incoming_fingerprint,
                arrival_transport,
                summary.first_seen_at_ms,
                summary.last_seen_at_ms,
                summary.seen_count,
            ));
        }
        Ok(out)
    }

    /// Record a durable clone warning for `user_id`. Callers must have
    /// authenticated proof (a Noise static key equal to this identity's
    /// agreement key). Do not persist this from an unauthenticated HELLO —
    /// that frame is spoofable — and do not persist it from a stream
    /// conflict: a replacement phone that reused lamports after a restore
    /// is not two live copies.
    pub fn record_identity_clone_warning(
        &self,
        user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<(), CoreError> {
        if user_id.is_empty() {
            return Err(CoreError::Malformed(
                "identity clone warning needs a user id".into(),
            ));
        }
        let conn = lock_conn(&self.conn);
        upsert_identity_clone_warning(&conn, &user_id, now_ms)
    }

    /// Whether this identity has been seen live on a second device.
    /// Only an authenticated sighting writes this table — a stream conflict
    /// is not enough (a replacement phone that reused lamports after a
    /// restore is not two live copies).
    pub fn has_identity_clone_warning(&self, user_id: Vec<u8>) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM identity_clone_warnings WHERE user_id = ?1)",
            params![user_id],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| value != 0)
        .map_err(store_err)
    }

    /// Clear this device's clone warning after the person has confirmed that
    /// no other phone is using their backup. A later authenticated sighting
    /// records a fresh row and surfaces the warning again.
    pub fn clear_identity_clone_warning(&self, user_id: Vec<u8>) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "DELETE FROM identity_clone_warnings WHERE user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Whether the bounded conflict quarantine contains any rows. This is the
    /// cheap predicate used by diagnostics screens; unlike CSV export it does
    /// not materialise the retained summaries or touch the filesystem.
    pub fn has_message_conflicts(&self) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM message_conflicts)",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map(|value| value != 0)
        .map_err(store_err)
    }

    /// Erase quarantined conflict branches and their metadata. Diagnostic
    /// export deliberately exposes only redacted summaries, but the retained
    /// rows contain message bodies for a future recovery rule. The user-facing
    /// "Delete captured diagnostics" action therefore needs an explicit way
    /// to remove both the summary and its private backing evidence.
    pub fn clear_message_conflicts(&self) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute("DELETE FROM message_conflicts", [])
            .map_err(store_err)?;
        conn.execute("DELETE FROM identity_clone_warnings", [])
            .map_err(store_err)?;
        Ok(())
    }
}

/// Make an installed legacy full-database backup safe before any transport
/// opens it. The path is opened through [`MessageStore::open`] first so old
/// schemas receive the normal forward migrations before the cleanup runs.
///
/// Returns the number of carried envelopes discarded. User-owned history,
/// contacts, authored Lamport watermarks, unexpired outbound authored work,
/// receipts and the relay frontier are deliberately preserved. New callers should use
/// [`sanitize_restored_message_store_with_options`] to make the choice
/// explicit and receive the full redacted report.
#[uniffi::export]
pub fn sanitize_restored_message_store(path: String) -> Result<u64, CoreError> {
    let report = sanitize_restored_message_store_with_options(
        path,
        BackupContentOptions::default(),
        current_unix_time_ms()?,
    )?;
    Ok(report.removed_courier_delivery_count)
}

/// Inspect an untrusted/decrypted backup database after applying the ordinary
/// forward schema migrations. The caller must use a private temporary copy:
/// opening a legacy SQLite file can create journal siblings and migrate it.
#[uniffi::export]
pub fn inspect_restored_message_store(
    path: String,
    now_ms: i64,
) -> Result<BackupInventory, CoreError> {
    validate_backup_now(now_ms)?;
    let store = MessageStore::open(path)?;
    store.backup_inventory(now_ms)
}

/// Apply the same Rust-owned classification policy to legacy and current
/// backups immediately before installation. This second pass is deliberate:
/// it protects old `.cmbak` files created before selectable/sanitized exports
/// existed and prevents a modified platform shell from bypassing the policy.
#[uniffi::export]
pub fn sanitize_restored_message_store_with_options(
    path: String,
    options: BackupContentOptions,
    now_ms: i64,
) -> Result<BackupSanitizationReport, CoreError> {
    validate_backup_now(now_ms)?;
    let store = MessageStore::open(path)?;
    let mut conn = lock_conn(&store.conn);
    sanitize_restore_contents(&mut conn, options, now_ms)
}

fn backup_inventory_from_conn(
    conn: &Connection,
    now_ms: i64,
) -> Result<BackupInventory, CoreError> {
    let contact_count = conn
        .query_row("SELECT COUNT(*) FROM contacts", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(store_err)?;
    let group_count = conn
        .query_row("SELECT COUNT(*) FROM groups", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(store_err)?;
    let (message_count, message_bytes) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(payload)), 0) FROM messages",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(store_err)?;
    let (pending_own_delivery_count, pending_own_delivery_bytes) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM (
                 SELECT LENGTH(sealed) AS bytes FROM outbound_envelopes WHERE expiry > ?1
                 UNION ALL
                 SELECT LENGTH(sealed) AS bytes FROM outgoing_receipt_envelopes WHERE expiry > ?1
             )",
            params![now_ms],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(store_err)?;
    let (pending_courier_delivery_count, pending_courier_delivery_bytes) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(size_bytes), 0)
             FROM carried_envelopes WHERE expiry > ?1",
            params![now_ms],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
        )
        .map_err(store_err)?;
    let to_u64 = |label: &str, value: i64| {
        u64::try_from(value)
            .map_err(|_| CoreError::Store(format!("negative {label} while inventorying backup")))
    };
    Ok(BackupInventory {
        contact_count: to_u64("contact count", contact_count)?,
        group_count: to_u64("group count", group_count)?,
        message_count: to_u64("message count", message_count)?,
        message_bytes: to_u64("message bytes", message_bytes)?,
        pending_own_delivery_count: to_u64(
            "pending own-delivery count",
            pending_own_delivery_count,
        )?,
        pending_own_delivery_bytes: to_u64(
            "pending own-delivery bytes",
            pending_own_delivery_bytes,
        )?,
        pending_courier_delivery_count: to_u64(
            "pending courier-delivery count",
            pending_courier_delivery_count,
        )?,
        pending_courier_delivery_bytes: to_u64(
            "pending courier-delivery bytes",
            pending_courier_delivery_bytes,
        )?,
    })
}

fn sanitize_restore_contents(
    conn: &mut Connection,
    options: BackupContentOptions,
    now_ms: i64,
) -> Result<BackupSanitizationReport, CoreError> {
    let tx = conn.transaction().map_err(store_err)?;

    // Connection history and reachability streaks describe the phone and
    // network that created the backup, not the restored device. They must not
    // make the new installation claim a stale peer is online or unreachable.
    let removed_connection_event_count = (tx
        .execute("DELETE FROM peer_connection_events", [])
        .map_err(store_err)?
        + tx.execute("DELETE FROM peer_connection_summary", [])
            .map_err(store_err)?) as u64;
    let mut report = BackupSanitizationReport {
        removed_connection_event_count,
        ..BackupSanitizationReport::default()
    };
    tx.execute(
        "UPDATE contacts SET
             relay_reject_streak = 0,
             relay_rejected_at = 0,
             relay_unreachable_endpoint_key = NULL,
             relay_unreachable_streak = 0,
             relay_unreachable_at = 0",
        [],
    )
    .map_err(store_err)?;

    if options.include_message_history {
        report.removed_expired_delivery_count += tx
            .execute(
                "DELETE FROM outbound_envelopes WHERE expiry <= ?1",
                params![now_ms],
            )
            .map_err(store_err)? as u64;
        report.removed_expired_delivery_count += tx
            .execute(
                "DELETE FROM outgoing_receipt_envelopes WHERE expiry <= ?1",
                params![now_ms],
            )
            .map_err(store_err)? as u64;
        report.removed_expired_delivery_count += tx
            .execute(
                "DELETE FROM consumed_hidden_msg_ids WHERE expiry_ms <= ?1",
                params![now_ms],
            )
            .map_err(store_err)? as u64;
    } else {
        report.removed_message_count =
            tx.execute("DELETE FROM messages", []).map_err(store_err)? as u64;
        // Quarantined alternatives are private backing evidence, not visible
        // message-history rows counted by the inventory/report.
        tx.execute("DELETE FROM message_conflicts", [])
            .map_err(store_err)?;
        report.removed_pending_own_delivery_count = (tx
            .execute("DELETE FROM outbound_envelopes", [])
            .map_err(store_err)?
            + tx.execute("DELETE FROM outgoing_receipt_envelopes", [])
                .map_err(store_err)?) as u64;
        for table in [
            "receipts",
            "outgoing_receipts",
            "delivery_metrics",
            "consumed_hidden_msg_ids",
            "consumed_hidden_lamports",
        ] {
            tx.execute(&format!("DELETE FROM {table}"), [])
                .map_err(store_err)?;
        }
        // authored_lamport_watermarks intentionally survives: it contains no
        // content and prevents a restored identity from reusing old stream
        // positions after the user chooses not to restore conversations.
    }

    if options.include_pending_deliveries_for_others {
        report.removed_expired_delivery_count += tx
            .execute(
                "DELETE FROM carried_envelopes WHERE expiry <= ?1",
                params![now_ms],
            )
            .map_err(store_err)? as u64;
    } else {
        report.removed_courier_delivery_count = tx
            .execute("DELETE FROM carried_envelopes", [])
            .map_err(store_err)? as u64;
    }

    tx.commit().map_err(store_err)?;
    // DELETE alone leaves content in SQLite freelist pages. Compact the
    // private snapshot so an excluded conversation/courier ciphertext is not
    // merely unreachable through SQL but still recoverable from the `.cmbak`
    // plaintext after decryption. Then force all WAL pages into the main file,
    // which is the only file the platform seals or stages.
    conn.execute_batch("VACUUM; PRAGMA wal_checkpoint(TRUNCATE)")
        .map_err(store_err)?;
    Ok(report)
}

fn validate_backup_now(now_ms: i64) -> Result<(), CoreError> {
    if now_ms < 0 {
        return Err(CoreError::Malformed(
            "backup inventory time cannot be negative".into(),
        ));
    }
    Ok(())
}

fn current_unix_time_ms() -> Result<i64, CoreError> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| CoreError::Store(format!("system clock precedes Unix epoch: {error}")))?;
    i64::try_from(duration.as_millis())
        .map_err(|_| CoreError::Store("system clock does not fit backup timestamp".into()))
}

const MESSAGE_CONFLICT_QUARANTINE_LIMIT: i64 = 64;
const MESSAGE_CONFLICT_FINGERPRINT_LEN: usize = 16;

fn validate_message_arrival(arrival: &MessageArrival) -> Result<(), CoreError> {
    if arrival.transport > 4 {
        return Err(CoreError::Malformed(
            "invalid message arrival transport".to_string(),
        ));
    }
    Ok(())
}

fn message_conflict_fingerprint(
    chat_id: &[u8],
    sender_user_id: &[u8],
    timestamp: i64,
    kind: u8,
    payload: &[u8],
) -> Vec<u8> {
    let mut hasher =
        Blake2bVar::new(MESSAGE_CONFLICT_FINGERPRINT_LEN).expect("valid BLAKE2b digest length");
    // Bind the fingerprint to the random conversation and sender ids. Besides
    // preventing cross-chat correlation, this makes guessing short/common
    // plaintext from an exported fingerprint impractical without the raw ids,
    // neither of which crosses the diagnostics boundary.
    hasher.update(&(chat_id.len() as u64).to_be_bytes());
    hasher.update(chat_id);
    hasher.update(&(sender_user_id.len() as u64).to_be_bytes());
    hasher.update(sender_user_id);
    hasher.update(&timestamp.to_be_bytes());
    hasher.update(&[kind]);
    hasher.update(payload);
    let mut digest = vec![0; MESSAGE_CONFLICT_FINGERPRINT_LEN];
    hasher
        .finalize_variable(&mut digest)
        .expect("digest output has configured length");
    digest
}

fn upsert_identity_clone_warning(
    conn: &Connection,
    user_id: &[u8],
    now_ms: i64,
) -> Result<(), CoreError> {
    conn.execute(
        "INSERT INTO identity_clone_warnings (user_id, first_seen_at, last_seen_at)
         VALUES (?1, ?2, ?2)
         ON CONFLICT(user_id) DO UPDATE SET
             last_seen_at = MAX(identity_clone_warnings.last_seen_at, excluded.last_seen_at)",
        params![user_id, now_ms],
    )
    .map_err(store_err)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn quarantine_message_conflict(
    tx: &Transaction<'_>,
    existing_timestamp: i64,
    existing_kind: i64,
    existing_payload: &[u8],
    incoming: &StoredMessage,
    incoming_msg_id: Option<&[u8]>,
    incoming_reply_to_msg_id: Option<&[u8]>,
    arrival: Option<&MessageArrival>,
) -> Result<(), CoreError> {
    let existing_kind = u8::try_from(existing_kind)
        .map_err(|_| CoreError::Store("stored message kind is outside u8 range".into()))?;
    let existing_fingerprint = message_conflict_fingerprint(
        &incoming.chat_id,
        &incoming.sender_user_id,
        existing_timestamp,
        existing_kind,
        existing_payload,
    );
    let incoming_fingerprint = message_conflict_fingerprint(
        &incoming.chat_id,
        &incoming.sender_user_id,
        incoming.timestamp,
        incoming.kind,
        &incoming.payload,
    );
    let observed_at = match arrival {
        Some(value) => value.received_at,
        None => current_unix_time_ms()?,
    };
    tx.execute(
        "INSERT INTO message_conflicts
            (chat_id, sender_user_id, lamport, existing_fingerprint,
             incoming_fingerprint, incoming_timestamp, incoming_kind,
             incoming_payload, incoming_msg_id, incoming_reply_to_msg_id,
             arrival_transport, hops_taken, first_seen_at, last_seen_at, seen_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13, 1)
         ON CONFLICT(chat_id, sender_user_id, lamport, incoming_fingerprint)
         DO UPDATE SET
             existing_fingerprint = excluded.existing_fingerprint,
             last_seen_at = MAX(message_conflicts.last_seen_at, excluded.last_seen_at),
             seen_count = message_conflicts.seen_count + 1,
             incoming_msg_id = COALESCE(message_conflicts.incoming_msg_id, excluded.incoming_msg_id),
             incoming_reply_to_msg_id = COALESCE(message_conflicts.incoming_reply_to_msg_id, excluded.incoming_reply_to_msg_id),
             arrival_transport = COALESCE(message_conflicts.arrival_transport, excluded.arrival_transport),
             hops_taken = COALESCE(message_conflicts.hops_taken, excluded.hops_taken)",
        params![
            incoming.chat_id,
            incoming.sender_user_id,
            incoming.lamport as i64,
            existing_fingerprint,
            incoming_fingerprint,
            incoming.timestamp,
            incoming.kind as i64,
            incoming.payload,
            incoming_msg_id,
            incoming_reply_to_msg_id,
            arrival.map(|value| value.transport as i64),
            arrival.map(|value| value.hops_taken as i64),
            observed_at,
        ],
    )
    .map_err(store_err)?;
    tx.execute(
        "DELETE FROM message_conflicts
         WHERE id IN (
             SELECT id FROM message_conflicts
             -- Envelope-backed chat paths leave a durable msg_id and are the
             -- branch evidence most likely to explain missing user content.
             -- Hidden/legacy collisions remain useful, but may not age those
             -- rows out of the one global bounded quarantine.
             ORDER BY (incoming_msg_id IS NOT NULL) DESC,
                      last_seen_at DESC, id DESC
             LIMIT -1 OFFSET ?1
         )",
        params![MESSAGE_CONFLICT_QUARANTINE_LIMIT],
    )
    .map_err(store_err)?;
    Ok(())
}

/// Ring plumbing for core policy objects that hold no connection of their own
/// (today: `CoreSprayPolicy`). Not exported: no shell composes an event.
impl MessageStore {
    pub(crate) fn record_protocol_events(
        &self,
        drafts: &[crate::protocol_event::ProtocolEventDraft],
    ) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        crate::protocol_event::append(&conn, drafts)
    }

    pub(crate) fn protocol_event_pseudonym(
        &self,
        kind: &'static str,
        raw: &[u8],
    ) -> Result<String, CoreError> {
        let conn = lock_conn(&self.conn);
        crate::protocol_event::actor_pseudonym(&conn, kind, raw)
    }
}

/// Internal-only helpers, never exported over UniFFI: not wrapped in
/// `#[uniffi::export]` because these are implementation details of the
/// digest spray plan (FC2) rather than API the platform shells call
/// directly.
impl MessageStore {
    /// `(hint, recipient_user_id)` for every hint a carried envelope could
    /// currently be addressed by, resolved exactly the way
    /// [`MessageStore::contact_matching_hint`] resolves one: a contact's own
    /// recent-day hints first, then a group's hints attributed to the first
    /// member who is a contact (a group carry uploads via any member's relay
    /// config). Earlier entries win on collision, matching that function's
    /// first-match-wins iteration.
    fn carried_hint_recipients(&self, now_ms: i64) -> Result<Vec<CarriedHintRecipient>, CoreError> {
        let contacts = self.list_contacts()?;
        let mut map: Vec<CarriedHintRecipient> = Vec::new();
        for contact in &contacts {
            for hint in crate::recipient_hints::recent_hints_for(contact.user_id.clone(), now_ms) {
                map.push((hint, contact.user_id.clone()));
            }
        }
        for group in self.list_groups()? {
            let Some(member) = group
                .member_user_ids
                .iter()
                .find(|id| contacts.iter().any(|c| c.user_id == **id))
            else {
                continue;
            };
            for hint in crate::recipient_hints::recent_hints_for(group.id.clone(), now_ms) {
                map.push((hint, member.clone()));
            }
        }
        Ok(map)
    }

    /// FC2 test-only accessor for [`MessageStore::sealed_reads`].
    #[cfg(test)]
    pub(crate) fn test_sealed_reads(&self) -> u64 {
        self.sealed_reads.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Up to `limit` carried-envelope `msg_id`s, **newest** first — the
    /// digest-advertisement counterpart to
    /// [`MessageStore::carried_msg_ids`]'s oldest-first order.
    ///
    /// At courier scale the two orders behave very differently. A phone
    /// carrying more rows than the advertisement holds would, oldest-first,
    /// advertise a frozen window over its *oldest* rows — exactly the rows
    /// its own oldest-first carry eviction deletes next — so the
    /// advertisement describes envelopes it no longer has while saying
    /// nothing about the ones it just accepted. A courier peer then re-offers
    /// everything newer on every reconnect, the loaded phone accepts and
    /// evicts, and the pair live-locks on offer/accept/evict. Newest-first
    /// advertises the half of the carry queue where suppression actually
    /// matters, and moves with the queue instead of pinning to its tail.
    ///
    /// [`MessageStore::carried_msg_ids`] keeps its oldest-first order for its
    /// other callers (seen-id seeding on the shells, the mesh simulator),
    /// where the traversal order is the point.
    pub(crate) fn carried_msg_ids_desc(&self, limit: u64) -> Result<Vec<Vec<u8>>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT msg_id FROM carried_envelopes
                 ORDER BY received_at DESC, msg_id DESC
                 LIMIT ?1",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![limit as i64], |row| row.get::<_, Vec<u8>>(0))
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }
}

mod incoming_message_reference {
    use super::*;

    pub(super) fn insert(
        store: &MessageStore,
        message: StoredMessage,
        msg_id: Option<Vec<u8>>,
        reply_to_msg_id: Option<Vec<u8>>,
        arrival: Option<MessageArrival>,
    ) -> Result<IncomingMessageInsertOutcome, CoreError> {
        validate_stored_message(&message)?;
        if let Some(arrival) = arrival.as_ref() {
            validate_message_arrival(arrival)?;
        }
        let mut conn = lock_conn(&store.conn);
        let tx = conn.transaction().map_err(store_err)?;

        let inserted = tx
            .execute(
                "INSERT OR IGNORE INTO messages
                    (chat_id, sender_user_id, lamport, timestamp, kind, payload,
                     msg_id, reply_to_msg_id, arrival_transport, hops_taken, received_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    message.chat_id,
                    message.sender_user_id,
                    message.lamport as i64,
                    message.timestamp,
                    message.kind as i64,
                    message.payload,
                    msg_id,
                    reply_to_msg_id,
                    arrival.as_ref().map(|value| value.transport as i64),
                    arrival.as_ref().map(|value| value.hops_taken as i64),
                    arrival.as_ref().map(|value| value.received_at),
                ],
            )
            .map_err(store_err)?
            > 0;

        if inserted {
            if let Some(arrival) = arrival.as_ref() {
                // Preserve the field metric previously written by the shells'
                // follow-up `record_message_arrival` call. Keeping it in this
                // transaction also prevents a crash gap between content and
                // first-arrival diagnostics.
                let _ = tx.execute(
                    "INSERT OR IGNORE INTO delivery_metrics
                        (chat_hash, lamport, direction, sender_hash, at_ms,
                         arrival_transport, hop_count)
                     VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6)",
                    params![
                        metric_chat_hash(&message.chat_id),
                        message.lamport as i64,
                        metric_sender_hash(&message.sender_user_id),
                        arrival.received_at,
                        arrival.transport as i64,
                        arrival.hops_taken as i64,
                    ],
                );
            }
            tx.commit().map_err(store_err)?;
            return Ok(IncomingMessageInsertOutcome::Inserted);
        }

        // Conflict: a row already exists at this (chat_id, sender_user_id,
        // lamport). Figure out whether it's the same message or a fork.
        //
        // FC9: `reply_to_msg_id` is deliberately NOT part of this
        // comparison. It used to be, but the plain `insert_message` path
        // (used for kinds that don't carry an envelope id/reply target)
        // always passes `None` here -- so the same logical message arriving
        // once through that path and once through
        // `insert_incoming_message` with a reply target set would differ
        // only in `reply_to_msg_id` and get misclassified as a fork,
        // deleting the tail and wiping `outgoing_receipts` for no reason.
        // `reply_to_msg_id` is reconciled via `COALESCE` below instead, the
        // same way `msg_id` already is.
        let existing: Option<(i64, i64, Vec<u8>)> = tx
            .query_row(
                "SELECT timestamp, kind, payload FROM messages
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3",
                params![
                    message.chat_id,
                    message.sender_user_id,
                    message.lamport as i64
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(store_err)?;

        let is_true_duplicate = match &existing {
            Some((timestamp, kind, payload)) => {
                *timestamp == message.timestamp
                    && *kind == message.kind as i64
                    && *payload == message.payload
            }
            // Shouldn't happen (we just failed to insert on a conflict), but
            // if the row vanished under us, treat it as nothing to recover.
            None => {
                tx.commit().map_err(store_err)?;
                return Ok(IncomingMessageInsertOutcome::Duplicate);
            }
        };

        if is_true_duplicate {
            if msg_id.is_some() || reply_to_msg_id.is_some() {
                tx.execute(
                    "UPDATE messages
                     SET msg_id = COALESCE(msg_id, ?4),
                         reply_to_msg_id = COALESCE(reply_to_msg_id, ?5)
                     WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3",
                    params![
                        message.chat_id,
                        message.sender_user_id,
                        message.lamport as i64,
                        msg_id,
                        reply_to_msg_id,
                    ],
                )
                .map_err(store_err)?;
            }
            tx.commit().map_err(store_err)?;
            return Ok(IncomingMessageInsertOutcome::Duplicate);
        }

        // A conflict proves only that two authenticated branches claim the
        // same stream position, not which one is authoritative. Preserve the
        // branch already rendered to the user. In particular, never use the
        // sender's wall clock as a generation signal: causal_order explicitly
        // bounds its influence because phone clocks can be wrong. Keep the
        // incoming branch in a bounded quarantine so diagnostics and a future
        // authenticated recovery rule have evidence instead of a silent drop.
        let (existing_timestamp, existing_kind, existing_payload) =
            existing.expect("conflicting row was selected above");
        quarantine_message_conflict(
            &tx,
            existing_timestamp,
            existing_kind,
            &existing_payload,
            &message,
            msg_id.as_deref(),
            reply_to_msg_id.as_deref(),
            arrival.as_ref(),
        )?;
        tx.commit().map_err(store_err)?;
        Ok(IncomingMessageInsertOutcome::QuarantinedConflict)
    }
}

#[uniffi::export]
impl MessageStore {
    /// Atomically persist one locally authored message and the exact sealed
    /// envelope that should be retried for it over BLE and relay. The message
    /// row stays idempotent on `(chat_id, sender_user_id, lamport)`; the
    /// outbound queue uses the same logical identity as its dedupe key, so
    /// re-queuing the same authored message is a no-op instead of creating a
    /// second `msg_id`.
    pub fn insert_outgoing_message(
        &self,
        message: StoredMessage,
        envelope: OutboundEnvelope,
        queued_at_ms: i64,
    ) -> Result<bool, CoreError> {
        outgoing_message_reference::insert(self, message, envelope, None, queued_at_ms)
    }

    /// Atomically persist a locally authored reply and its stable sealed
    /// envelope. The target id remains local metadata as well as encrypted
    /// body metadata, so rendering never needs to reopen ciphertext.
    pub fn insert_outgoing_reply(
        &self,
        message: StoredMessage,
        envelope: OutboundEnvelope,
        reply_to_msg_id: Vec<u8>,
        queued_at_ms: i64,
    ) -> Result<bool, CoreError> {
        validate_msg_id("msg_id", &envelope.msg_id)?;
        validate_msg_id("reply_to_msg_id", &reply_to_msg_id)?;
        outgoing_message_reference::insert(
            self,
            message,
            envelope,
            Some(reply_to_msg_id),
            queued_at_ms,
        )
    }
}

mod outgoing_message_reference {
    use super::*;

    pub(super) fn insert(
        store: &MessageStore,
        message: StoredMessage,
        envelope: OutboundEnvelope,
        reply_to_msg_id: Option<Vec<u8>>,
        queued_at_ms: i64,
    ) -> Result<bool, CoreError> {
        validate_stored_message(&message)?;
        validate_sqlite_u64("envelope lamport", envelope.lamport)?;
        let mut conn = lock_conn(&store.conn);
        let tx = conn.transaction().map_err(store_err)?;
        tx.execute(
            "INSERT OR IGNORE INTO messages
                (chat_id, sender_user_id, lamport, timestamp, kind, payload,
                 msg_id, reply_to_msg_id, outbound_expiry)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                message.chat_id,
                message.sender_user_id,
                message.lamport as i64,
                message.timestamp,
                message.kind as i64,
                message.payload,
                envelope.msg_id,
                reply_to_msg_id,
                envelope.expiry,
            ],
        )
        .map_err(store_err)?;
        tx.execute(
            "UPDATE messages
             SET msg_id = COALESCE(msg_id, ?4),
                 reply_to_msg_id = COALESCE(reply_to_msg_id, ?5),
                 outbound_expiry = COALESCE(outbound_expiry, ?6)
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3",
            params![
                message.chat_id,
                message.sender_user_id,
                message.lamport as i64,
                envelope.msg_id,
                reply_to_msg_id,
                envelope.expiry,
            ],
        )
        .map_err(store_err)?;
        let changed = tx
            .execute(
                "INSERT OR IGNORE INTO outbound_envelopes
                    (dedupe_key, msg_id, recipient_user_id, chat_id, sender_user_id, kind,
                     lamport, timestamp, hop_ttl, expiry, recipient_hint, sealed, queued_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    outbound_message_dedupe_key(
                        &envelope.chat_id,
                        &envelope.sender_user_id,
                        envelope.kind,
                        envelope.lamport,
                        &envelope.recipient_user_id,
                    ),
                    envelope.msg_id,
                    envelope.recipient_user_id,
                    envelope.chat_id,
                    envelope.sender_user_id,
                    envelope.kind as i64,
                    envelope.lamport as i64,
                    envelope.timestamp,
                    envelope.hop_ttl as i64,
                    envelope.expiry,
                    envelope.recipient_hint,
                    envelope.sealed,
                    queued_at_ms,
                ],
            )
            .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(changed > 0)
    }
}

#[uniffi::export]
impl MessageStore {
    /// All messages in a chat, oldest first by author timestamp.
    ///
    /// `lamport` is only comparable within one sender's stream
    /// (`chat_id`,`sender_user_id`), not across both participants in a 1:1
    /// chat. So conversation display order must not be `ORDER BY lamport`
    /// across the whole chat or a later message from Alice can render before
    /// an earlier-timestamped message from Bob simply because Alice's local
    /// counter is smaller. `id` is only a stable tie-breaker for equal
    /// timestamps.
    pub fn messages_for_chat(&self, chat_id: Vec<u8>) -> Result<Vec<StoredMessage>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload
                 FROM messages WHERE chat_id = ?1 ORDER BY timestamp ASC, id ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![chat_id], row_to_message)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Home-list row data for one chat without marshaling the full history.
    ///
    /// G1 (PRIVATE-TODO §0b): the Android home list used to call
    /// [`Self::messages_for_chat`] then `core_last_visible_message` per chat on
    /// the main thread, which under a mesh storm ANR'd. One lock, one last
    /// visible row, SQL unread, and receipt watermarks — never the whole chat.
    pub fn chat_preview(
        &self,
        chat_id: Vec<u8>,
        own_user_id: Vec<u8>,
    ) -> Result<CoreChatPreview, CoreError> {
        let conn = lock_conn(&self.conn);
        let last_message = conn
            .query_row(
                "SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload
                 FROM messages
                 WHERE chat_id = ?1 AND kind IN (?2, ?3, ?4)
                 ORDER BY timestamp DESC, id DESC
                 LIMIT 1",
                params![
                    chat_id,
                    crate::KIND_TEXT as i64,
                    crate::KIND_ATTACHMENT_MANIFEST as i64,
                    crate::KIND_GROUP_INVITE as i64
                ],
                row_to_message,
            )
            .optional()
            .map_err(store_err)?;
        let unread_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM messages m
                 WHERE m.chat_id = ?1 AND m.sender_user_id != ?2 AND m.kind IN (?3, ?4, ?5)
                   AND m.lamport > COALESCE((SELECT through_lamport FROM outgoing_receipts r
                       WHERE r.chat_id = m.chat_id AND r.sender_user_id = m.sender_user_id
                         AND r.receipt_type = ?6), 0)",
                params![
                    chat_id,
                    own_user_id,
                    crate::KIND_TEXT as i64,
                    crate::KIND_ATTACHMENT_MANIFEST as i64,
                    crate::KIND_GROUP_INVITE as i64,
                    crate::RECEIPT_TYPE_READ as i64
                ],
                |row| row.get(0),
            )
            .map_err(store_err)?;
        let own_delivered_through: i64 = conn
            .query_row(
                "SELECT through_lamport FROM receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![chat_id, own_user_id, crate::RECEIPT_TYPE_DELIVERED as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?
            .unwrap_or(0);
        let own_read_through: i64 = conn
            .query_row(
                "SELECT through_lamport FROM receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![chat_id, own_user_id, crate::RECEIPT_TYPE_READ as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?
            .unwrap_or(0);
        let last_message_timestamp = last_message.as_ref().map(|m| m.timestamp).unwrap_or(0);
        let (own_delivered_through, own_read_through) = group_preview_watermarks(
            &conn,
            &chat_id,
            &own_user_id,
            last_message_timestamp,
            own_delivered_through,
            own_read_through,
        )?;
        let avatar_bytes: Option<Vec<u8>> = conn
            .query_row(
                "SELECT avatar FROM contacts WHERE user_id = ?1",
                params![chat_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?
            .flatten();
        Ok(CoreChatPreview {
            chat_id,
            last_message,
            unread_count: unread_count as u32,
            own_delivered_through: own_delivered_through as u64,
            own_read_through: own_read_through as u64,
            avatar_bytes,
        })
    }

    /// Record an exact lamport this device consumed from a pairwise stream
    /// even though that envelope leaves no durable `msg_id`-bearing message
    /// row in this chat.
    ///
    /// Shells call this only after authenticated delivery finishes. Core still
    /// refuses kinds whose ordinary incoming path already persists the exact
    /// envelope as a message row; those rows are already gap evidence and a
    /// second copy here would obscure the ownership boundary.
    pub fn record_consumed_hidden_lamport(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        lamport: u64,
        kind: u8,
    ) -> Result<bool, CoreError> {
        if crate::core_kind_persists_msg_id_row(kind) {
            return Ok(false);
        }
        // Durable gap evidence is only meaningful for an accepted 1:1 chat.
        // The two onboarding kinds may be sent by strangers; refusing them
        // until their handler has actually created a contact prevents an
        // unauthenticated sender from growing a history-lifetime table.
        if chat_id != sender_user_id {
            return Ok(false);
        }
        validate_sqlite_u64("consumed hidden lamport", lamport)?;
        let conn = lock_conn(&self.conn);
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO consumed_hidden_lamports
                    (chat_id, sender_user_id, lamport)
                 SELECT ?1, ?2, ?3
                 WHERE EXISTS (SELECT 1 FROM contacts WHERE user_id = ?2)
                   AND NOT EXISTS (
                       SELECT 1 FROM messages
                       WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3
                   )",
                params![chat_id, sender_user_id, lamport as i64],
            )
            .map_err(store_err)?;
        Ok(inserted > 0)
    }

    /// Exact consumed control-message positions for one chat, grouped by the
    /// sender stream the visible-gap policy compares independently.
    pub fn consumed_hidden_lamports(
        &self,
        chat_id: Vec<u8>,
    ) -> Result<Vec<ConsumedHiddenLamport>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT sender_user_id, lamport
                 FROM consumed_hidden_lamports
                 WHERE chat_id = ?1
                 ORDER BY sender_user_id ASC, lamport ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![chat_id], |row| {
                Ok(ConsumedHiddenLamport {
                    sender_user_id: row.get(0)?,
                    lamport: row.get::<_, i64>(1)? as u64,
                })
            })
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Stable id and optional reply target for one stored message. Returns
    /// `None` for legacy rows whose inbound envelope id was never recorded.
    pub fn message_reference(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        lamport: u64,
    ) -> Result<Option<MessageReference>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT msg_id, reply_to_msg_id
             FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3
               AND msg_id IS NOT NULL",
            params![chat_id, sender_user_id, lamport as i64],
            |row| {
                Ok(MessageReference {
                    msg_id: row.get(0)?,
                    reply_to_msg_id: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(store_err)
    }

    /// Expiry of a locally-authored message's durable outbound envelope.
    /// This remains available after the retry queue prunes expired ciphertext.
    pub fn outbound_message_expiry(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        lamport: u64,
    ) -> Result<Option<i64>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT outbound_expiry FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3",
            params![chat_id, sender_user_id, lamport as i64],
            |row| row.get::<_, Option<i64>>(0),
        )
        .optional()
        .map_err(store_err)
        .map(|value| value.flatten())
    }

    /// Resolve a quoted message by stable envelope id within one chat.
    /// Missing history is expected and returns `None`.
    pub fn message_by_msg_id(
        &self,
        chat_id: Vec<u8>,
        msg_id: Vec<u8>,
    ) -> Result<Option<StoredMessage>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload
             FROM messages
             WHERE chat_id = ?1 AND msg_id = ?2
             ORDER BY id ASC
             LIMIT 1",
            params![chat_id, msg_id],
            row_to_message,
        )
        .optional()
        .map_err(store_err)
    }

    /// Chat and sender of a durably consumed message keyed by its stable
    /// envelope `msg_id` alone, searched across accepted messages and the
    /// bounded conflict quarantine -- unlike
    /// [`Self::message_by_msg_id`], which needs `chat_id` up front and is
    /// useless here because a relay-fetched envelope only carries its own
    /// `msg_id`.
    ///
    /// This backs the consumed-SEEN relay ack rule in `engine.rs`
    /// (`MessageStore::core_relay_ack_ids_with_consumed`): a relay-fetched
    /// copy that dedupes as `Seen` (already handled via some other path) is
    /// only safe to ack if THIS device actually consumed and durably retained
    /// it as a real message, not merely muled it. An accepted message row or a
    /// quarantined alternative can provide that evidence for kinds carrying a
    /// durable `msg_id` -- 1:1/group text, attachment manifests, reactions,
    /// and group metadata updates. Hidden kinds -- receipts, profile sync,
    /// friend requests/directory, group invites, LAN endpoint hints -- use the
    /// plain `insert_message` path with no id and therefore never match here.
    ///
    /// Returns `None` for an unknown `msg_id` (never stored, or hidden-kind
    /// with no durable id). The store deliberately returns the raw
    /// [`MessageOrigin`] instead of a verdict: it is the caller's job to
    /// exclude own-authored rows (`sender_user_id == own user id` -- the
    /// relay copy is there for the recipient) and group rows (`chat_id !=
    /// sender_user_id` -- other members of the shared family mailbox still
    /// need the relay copy).
    pub fn message_origin_by_msg_id(
        &self,
        msg_id: Vec<u8>,
    ) -> Result<Option<MessageOrigin>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT chat_id, sender_user_id
             FROM (
                 SELECT chat_id, sender_user_id, id, 0 AS source
                 FROM messages WHERE msg_id = ?1
                 UNION ALL
                 SELECT chat_id, sender_user_id, id, 1 AS source
                 FROM message_conflicts WHERE incoming_msg_id = ?1
             )
             ORDER BY source ASC, id ASC
             LIMIT 1",
            params![msg_id],
            |row| {
                Ok(MessageOrigin {
                    chat_id: row.get(0)?,
                    sender_user_id: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(store_err)
    }

    /// Attach first-arrival diagnostics to an already inserted incoming
    /// message. A redundant mesh/relay copy never overwrites the original
    /// route, hop count, or receive time.
    pub fn record_message_arrival(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        lamport: u64,
        arrival: MessageArrival,
    ) -> Result<bool, CoreError> {
        validate_message_arrival(&arrival)?;
        let conn = lock_conn(&self.conn);
        let changed = conn
            .execute(
                "UPDATE messages
                 SET arrival_transport = ?4, hops_taken = ?5, received_at = ?6
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3
                   AND arrival_transport IS NULL",
                params![
                    chat_id,
                    sender_user_id,
                    lamport as i64,
                    arrival.transport as i64,
                    arrival.hops_taken as i64,
                    arrival.received_at,
                ],
            )
            .map_err(store_err)?;
        if changed > 0 {
            // V2 field metric: log the inbound arrival alongside the diagnostic
            // update, on the message's first arrival only. Best-effort and
            // metadata-only; a metrics failure must not fail delivery.
            let _ = conn.execute(
                "INSERT OR IGNORE INTO delivery_metrics
                    (chat_hash, lamport, direction, sender_hash, at_ms, arrival_transport, hop_count)
                 VALUES (?1, ?2, 1, ?3, ?4, ?5, ?6)",
                params![
                    metric_chat_hash(&chat_id),
                    lamport as i64,
                    metric_sender_hash(&sender_user_id),
                    arrival.received_at,
                    arrival.transport as i64,
                    arrival.hops_taken as i64,
                ],
            );
        }
        Ok(changed > 0)
    }

    /// V2 field metric: record that this device authored an outbound message
    /// at `lamport` in `chat_id` at `sent_at_ms`, so the cruise-test export can
    /// later measure delivery latency and the route a receipt returned on.
    /// Idempotent per (chat, lamport); metadata only -- the chat is stored as
    /// an 8-byte hash and no content is kept. See [`delivery_metrics`].
    pub fn record_sent_metric(
        &self,
        chat_id: Vec<u8>,
        lamport: u64,
        sent_at_ms: i64,
    ) -> Result<(), CoreError> {
        validate_sqlite_u64("metric lamport", lamport)?;
        let conn = lock_conn(&self.conn);
        conn.execute(
            "INSERT OR IGNORE INTO delivery_metrics
                (chat_hash, lamport, direction, sender_hash, at_ms)
             VALUES (?1, ?2, 0, ?3, ?4)",
            params![
                metric_chat_hash(&chat_id),
                lamport as i64,
                metric_sender_self(),
                sent_at_ms,
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// V2 field metric: stamp the delivery time and return route (T6
    /// `via_transport`) onto every outbound metric row in `chat_id` at or below
    /// the confirmed `through_lamport` that isn't already marked delivered.
    /// Cumulative receipts confirm a run of messages at once, so this covers
    /// them all; the first confirmation wins (a later, higher watermark still
    /// stamps the messages it newly covers). Metadata only.
    pub fn record_delivered_metric(
        &self,
        chat_id: Vec<u8>,
        through_lamport: u64,
        delivered_at_ms: i64,
        via_transport: Option<u8>,
    ) -> Result<(), CoreError> {
        validate_sqlite_u64("metric lamport", through_lamport)?;
        let conn = lock_conn(&self.conn);
        conn.execute(
            "UPDATE delivery_metrics
             SET delivered_at_ms = ?3, via_transport = ?4
             WHERE chat_hash = ?1 AND direction = 0
               AND lamport <= ?2 AND delivered_at_ms IS NULL",
            params![
                metric_chat_hash(&chat_id),
                through_lamport as i64,
                delivered_at_ms,
                via_transport.map(|t| t as i64),
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// V2 field metrics as CSV for the cruise-test export (metadata only). One
    /// row per sent/received message; `latency_ms` is the send->delivered gap
    /// for confirmed outbound messages. Transports use the
    /// [`MessageArrival::transport`] encoding (0/1 BLE direct/muled, 2 relay,
    /// 3/4 LAN direct/muled). Empty cells are unknown/not-applicable.
    pub fn export_delivery_metrics_csv(&self) -> Result<String, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT chat_hash, lamport, direction, sender_hash, at_ms, delivered_at_ms,
                        via_transport, arrival_transport, hop_count
                 FROM delivery_metrics
                 ORDER BY direction, chat_hash, lamport, sender_hash",
            )
            .map_err(store_err)?;
        let mut out = String::from(
            "direction,chat,lamport,sender,at_ms,delivered_at_ms,latency_ms,via_transport,arrival_transport,hop_count\n",
        );
        let rows = stmt
            .query_map([], |row| {
                let chat_hash: Vec<u8> = row.get(0)?;
                let lamport: i64 = row.get(1)?;
                let direction: i64 = row.get(2)?;
                let sender_hash: Vec<u8> = row.get(3)?;
                let at_ms: i64 = row.get(4)?;
                let delivered_at_ms: Option<i64> = row.get(5)?;
                let via_transport: Option<i64> = row.get(6)?;
                let arrival_transport: Option<i64> = row.get(7)?;
                let hop_count: Option<i64> = row.get(8)?;
                let latency_ms = match (direction, delivered_at_ms) {
                    (0, Some(d)) => Some(d - at_ms),
                    _ => None,
                };
                let dir = if direction == 0 { "sent" } else { "received" };
                // "sent" rows always carry the fixed self sentinel (this
                // device is the sole author of its own stream) -- leave the
                // sender cell blank rather than print a meaningless constant.
                let sender_cell = if direction == 1 {
                    hex_lower(&sender_hash)
                } else {
                    String::new()
                };
                let cell = |v: Option<i64>| v.map(|n| n.to_string()).unwrap_or_default();
                Ok(format!(
                    "{dir},{},{lamport},{sender_cell},{at_ms},{},{},{},{},{}\n",
                    hex_lower(&chat_hash),
                    cell(delivered_at_ms),
                    cell(latency_ms),
                    cell(via_transport),
                    cell(arrival_transport),
                    cell(hop_count),
                ))
            })
            .map_err(store_err)?;
        for row in rows {
            out.push_str(&row.map_err(store_err)?);
        }
        Ok(out)
    }

    /// Erases every V2 field-metrics row.
    ///
    /// The counterpart to [`Self::export_delivery_metrics_csv`]. These rows
    /// used to leave the device only when someone deliberately tapped a
    /// separate "Export field metrics" button, so having no way to erase them
    /// was defensible. Now that they ride along with every "Share
    /// diagnostics", the tester-facing delete has to reach them too --
    /// otherwise "delete captured diagnostics" leaves behind the one captured
    /// thing it did not name.
    ///
    /// Deliberately does not touch `messages`: the `arrival_transport` and
    /// `hops_taken` columns there are per-message delivery facts the chat UI
    /// renders, not captured diagnostics, and clearing them would silently
    /// change what the app says about existing conversations.
    pub fn clear_delivery_metrics(&self) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute("DELETE FROM delivery_metrics", [])
            .map_err(store_err)?;
        Ok(())
    }

    /// Whether any field-metrics rows exist.
    ///
    /// The cheap question the UI actually wants when deciding whether the
    /// delete button has anything to act on. Asking
    /// [`Self::export_delivery_metrics_csv`] instead means serialising every
    /// row -- thousands of them after a week aboard -- and, on Android, the
    /// caller then had to write that CSV to disk just to count its lines,
    /// during Compose composition. `EXISTS` stops at the first row and touches
    /// no files.
    pub fn has_delivery_metrics(&self) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row("SELECT EXISTS(SELECT 1 FROM delivery_metrics)", [], |row| {
            row.get::<_, i64>(0)
        })
        .map(|n| n != 0)
        .map_err(store_err)
    }

    /// First-arrival diagnostics for one message, or `None` for locally
    /// authored/legacy rows that predate diagnostics.
    pub fn message_arrival(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        lamport: u64,
    ) -> Result<Option<MessageArrival>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT arrival_transport, hops_taken, received_at
             FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport = ?3
               AND arrival_transport IS NOT NULL",
            params![chat_id, sender_user_id, lamport as i64],
            |row| {
                Ok(MessageArrival {
                    transport: row.get::<_, i64>(0)? as u8,
                    hops_taken: row.get::<_, i64>(1)? as u8,
                    received_at: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(store_err)
    }

    /// Arrival times for every message in `chat_id` that has one, for
    /// [`crate::late_arrival`]'s displacement test.
    ///
    /// One query rather than a [`Self::message_arrival`] call per row: the
    /// shells recompute this on every conversation reload, and a per-bubble
    /// round trip across the FFI would put a store read back on the render
    /// path that FA4 moved off it.
    ///
    /// Rows we authored locally and rows stored before arrival diagnostics
    /// existed have no `received_at` and are simply absent -- callers treat a
    /// missing entry as "no recorded arrival", which is what
    /// [`crate::late_arrival::LateArrivalInput::arrival_ts_ms`] wants.
    pub fn chat_received_times(
        &self,
        chat_id: Vec<u8>,
    ) -> Result<Vec<CoreMessageReceivedAt>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT sender_user_id, lamport, received_at
                 FROM messages
                 WHERE chat_id = ?1 AND received_at IS NOT NULL",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![chat_id], |row| {
                Ok(CoreMessageReceivedAt {
                    sender_user_id: row.get(0)?,
                    lamport: row.get::<_, i64>(1)? as u64,
                    received_at_ms: row.get(2)?,
                })
            })
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// The highest lamport value N such that every message `1..=N` from this
    /// sender in this chat is present -- the point up to which there's no
    /// gap (DESIGN.md §7.3: "message 12 arrived, 11 hasn't -- keep
    /// waiting"). Returns 0 if message 1 itself hasn't arrived yet.
    pub fn highest_contiguous_lamport(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
    ) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        highest_contiguous_lamport_locked(&conn, &chat_id, &sender_user_id)
    }

    /// The highest lamport value actually held from this sender in this
    /// chat -- a plain MAX, with no contiguity requirement. Returns 0 when
    /// nothing has been received yet.
    ///
    /// This is deliberately a *different* primitive from
    /// [`MessageStore::highest_contiguous_lamport`], and the split matters:
    /// the transactional authoring Lamport ratchet
    /// lets a sender's stream legitimately start above 1 after a chat
    /// history wipe, because lamports below the new base never existed for
    /// anyone -- there is nothing to be "contiguous from 1" with. A
    /// receiver holding only e.g. {3, 4} from that sender has a perfectly
    /// complete view of everything the sender ever sent, but
    /// `highest_contiguous_lamport` reports 0 for it (it stops at the first
    /// missing lamport, and 1 and 2 are permanently missing). Basing
    /// delivered/read receipts and the local read/unread badge on that 0
    /// makes them stall forever: `handle_chat_viewed`-style callers would
    /// record read-through 0 and the unread count would never clear.
    ///
    /// `highest_lamport` fixes that by answering "what's the highest
    /// message I actually hold from this sender" instead -- which is the
    /// right basis for a receipt/badge watermark. It is *not* a safe
    /// substitute for [`MessageStore::highest_contiguous_lamport`] in
    /// digest sync ([`MessageStore::chat_digest`]): that path genuinely
    /// needs the gap-aware contiguous count so it can detect a hole and
    /// re-request the missing early messages (DESIGN.md §7.3). Reporting a
    /// bare MAX there would let a real front-gap (message 1 lost in
    /// transit, not wiped) go undetected forever, since the peer would
    /// believe we already have everything up to the max we've seen.
    ///
    /// Moving receipts/badges to MAX is safe from a message-loss standpoint,
    /// though the reason changed with #283 and is worth restating exactly.
    /// `record_receipt` now *does* retire the `outbound_envelopes` rows a
    /// delivered watermark covers, so an overstated watermark reaching the
    /// sender does remove sealed rows for messages that peer never filed. What
    /// makes that harmless is that retirement removes only the sealed
    /// retransmission artifact and only where the `messages` row that
    /// regenerates it survives (see `crate::outbound_retirement`): the sender
    /// still holds the message, the peer's *digest* still reports the gap-aware
    /// contiguous watermark, and the digest responder re-seals the envelope
    /// from the stored message. Nothing a sender still owes becomes
    /// unsendable. Carried rows -- other people's mail, which this device is
    /// the only copy of -- are untouched by all of this and still leave only
    /// on their own digest proof.
    pub fn highest_lamport(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
    ) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT COALESCE(MAX(lamport), 0) FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2",
            params![chat_id, sender_user_id],
            |row| row.get::<_, i64>(0),
        )
        .map_err(store_err)
        .map(|n| n as u64)
    }

    /// A sync digest for `chat_id` (DESIGN.md §7.3): one [`DigestEntry`] per
    /// distinct sender who has ever posted in this chat, each with their
    /// [`MessageStore::highest_contiguous_lamport`]. Ordered by
    /// `sender_user_id` for a deterministic wire encoding. See the module
    /// docs for why this covers only the contiguous-lamport half of §7.3's
    /// digest (the recent-msg_id bloom filter is deferred).
    pub fn chat_digest(&self, chat_id: Vec<u8>) -> Result<Vec<DigestEntry>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT sender_user_id FROM messages
                 WHERE chat_id = ?1 ORDER BY sender_user_id ASC",
            )
            .map_err(store_err)?;
        let senders = stmt
            .query_map(params![chat_id], |row| row.get::<_, Vec<u8>>(0))
            .map_err(store_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(store_err)?;

        let mut entries = Vec::with_capacity(senders.len());
        for sender_user_id in senders {
            let through_lamport =
                highest_contiguous_lamport_locked(&conn, &chat_id, &sender_user_id)?;
            entries.push(DigestEntry {
                sender_user_id,
                through_lamport,
            });
        }
        Ok(entries)
    }

    /// Messages from `sender_user_id` in `chat_id` with `lamport >
    /// after_lamport`, oldest first -- what a peer whose digest reported
    /// `after_lamport` for this sender is missing (DESIGN.md §7.3).
    pub fn messages_after(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        after_lamport: u64,
    ) -> Result<Vec<StoredMessage>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload
                 FROM messages
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport > ?3
                 ORDER BY lamport ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(
                params![chat_id, sender_user_id, after_lamport as i64],
                row_to_message,
            )
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Exact sealed envelopes for this device's authored messages in
    /// `chat_id` whose lamport is above `after_lamport`, oldest first. This
    /// is the transport-level counterpart to [`MessageStore::messages_after`]:
    /// same logical retry set, but with the stable persisted `msg_id` and
    /// ciphertext needed for dedupe-safe resend across any authored
    /// message-kind that participates in the chat lamport stream.
    pub fn outbound_envelopes_after(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        after_lamport: u64,
    ) -> Result<Vec<OutboundEnvelope>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, kind, lamport,
                        timestamp, hop_ttl, expiry, recipient_hint, sealed
                 FROM outbound_envelopes
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport > ?3
                 ORDER BY lamport ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(
                params![chat_id, sender_user_id, after_lamport as i64],
                row_to_outbound,
            )
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Relay-upload candidates: locally authored envelopes not yet marked as
    /// posted to a relay, unexpired as of `now_ms`.
    ///
    /// Rows are drawn **round-robin across recipients** rather than in flat
    /// queue order: each recipient's oldest envelope is offered before any
    /// recipient's second, and so on. A flat `ORDER BY queued_at LIMIT n` lets
    /// one recipient own the entire window, and because a failed upload never
    /// sets `relay_posted_at`, those same rows refill it on every pass --
    /// forever. One contact with an unreachable relay then starves every other
    /// conversation on the device indefinitely. Ranking first by position
    /// *within* a recipient makes that impossible while costing nothing when
    /// only one recipient has traffic: with a single recipient the ranks are
    /// already `1, 2, 3, ...` in queue order, so the batch is byte-identical
    /// to the flat query. Fairness binds only under contention.
    ///
    /// `skip_recipient_user_ids` drops recipients the caller already knows it
    /// cannot post to on this pass (no resolvable relay config -- resting,
    /// unconfigured, or written off). The exclusion has to happen *here*
    /// rather than in the caller's loop: a row the caller fetches and then
    /// skips has still consumed one of `limit` slots, so filtering downstream
    /// leaves the starvation fully intact. Skipped rows keep their queued
    /// state untouched and are offered again on a later pass, and to the
    /// BLE/LAN paths meanwhile, exactly as before.
    ///
    /// Group-addressed envelopes carry `recipient_user_id = group_id`, so a
    /// group ranks as its own recipient -- which is what fan-out wants.
    pub fn pending_relay_outbound_envelopes(
        &self,
        limit: u64,
        now_ms: i64,
        skip_recipient_user_ids: Vec<Vec<u8>>,
    ) -> Result<Vec<OutboundEnvelope>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut args: Vec<Value> = vec![Value::Integer(now_ms)];
        let skip_clause = if skip_recipient_user_ids.is_empty() {
            String::new()
        } else {
            let placeholders = vec!["?"; skip_recipient_user_ids.len()].join(", ");
            args.extend(skip_recipient_user_ids.into_iter().map(Value::Blob));
            format!(" AND recipient_user_id NOT IN ({placeholders})")
        };
        args.push(Value::Integer(limit as i64));
        let sql = format!(
            "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, kind, lamport,
                    timestamp, hop_ttl, expiry, recipient_hint, sealed
             FROM (
                 SELECT *, ROW_NUMBER() OVER (
                            PARTITION BY recipient_user_id
                            ORDER BY queued_at ASC, msg_id ASC
                        ) AS recipient_rank
                 FROM outbound_envelopes
                 WHERE relay_posted_at IS NULL AND expiry > ?{skip_clause}
             )
             ORDER BY recipient_rank ASC, queued_at ASC, msg_id ASC
             LIMIT ?"
        );
        let mut stmt = conn.prepare(&sql).map_err(store_err)?;
        let rows = stmt
            .query_map(params_from_iter(args.iter()), row_to_outbound)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// How many unposted, unexpired relay-upload candidates are queued per
    /// recipient as of `now_ms`, largest backlog first. Diagnostics only: a
    /// stranded outbound queue was previously invisible in a support export,
    /// which made "nothing is being delivered" indistinguishable from
    /// "nothing was sent" without a debugger.
    pub fn pending_relay_outbound_depth_by_recipient(
        &self,
        now_ms: i64,
    ) -> Result<Vec<RelayQueueDepth>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT recipient_user_id, COUNT(*) AS queued
                 FROM outbound_envelopes
                 WHERE relay_posted_at IS NULL AND expiry > ?1
                 GROUP BY recipient_user_id
                 ORDER BY queued DESC, recipient_user_id ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![now_ms], |row| {
                Ok(RelayQueueDepth {
                    recipient_user_id: row.get(0)?,
                    queued: row.get::<_, i64>(1)? as u64,
                })
            })
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Per-recipient delivery state for the connection details page: how much
    /// of what we said to each person is still unaccounted for, how old it is,
    /// when anything last moved, and what their endpoint's persisted health
    /// says.
    ///
    /// Driven from the *recipient* side, one index seek per person, never a
    /// walk of message history. A field store carries six figures of envelopes
    /// and a hundred megabytes of database; a query whose cost tracked total
    /// history would have to be run off the main thread and would still get
    /// slower every week the phone was used. Each recipient costs:
    ///
    /// * one primary-key probe of `blocked_identities`;
    /// * one primary-key read of `contacts`;
    /// * one primary-key read of `receipts` for their delivery watermark;
    /// * one primary-key read of `peer_connection_summary`; and
    /// * one range seek on `idx_outbound_recipient_chat_lamport` covering
    ///   exactly the envelopes above that watermark -- the unacknowledged
    ///   ones, which is the set the page is about. Everything already
    ///   confirmed is skipped by the seek rather than filtered afterwards.
    ///
    /// See [`recipient_waiting_sql`] for the statement itself and
    /// `recipient_delivery_query_seeks_the_recipient_index` for the plan test
    /// that keeps it honest.
    ///
    /// Blocked identities are dropped rather than reported: a block is a
    /// tombstone and this page must not surface the person in any form. That
    /// filter is here, in the query, so a caller cannot forget it.
    pub fn recipient_delivery_status(
        &self,
        own_user_id: Vec<u8>,
        recipient_user_ids: Vec<Vec<u8>>,
        now_ms: i64,
    ) -> Result<Vec<CoreRecipientDeliveryStatus>, CoreError> {
        if own_user_id.is_empty() {
            return Err(CoreError::Malformed("own user id must not be empty".into()));
        }
        let conn = lock_conn(&self.conn);
        let waiting_sql = recipient_waiting_sql();
        let mut blocked_stmt = conn
            .prepare("SELECT 1 FROM blocked_identities WHERE user_id = ?1")
            .map_err(store_err)?;
        let mut contact_stmt = conn
            .prepare(
                "SELECT relay_reject_streak, relay_rejected_at,
                        relay_unreachable_streak, relay_unreachable_at
                 FROM contacts WHERE user_id = ?1",
            )
            .map_err(store_err)?;
        let mut receipt_stmt = conn
            .prepare(
                "SELECT through_lamport FROM receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
            )
            .map_err(store_err)?;
        let mut delivered_stmt = conn
            .prepare(
                "SELECT COALESCE(MAX(last_delivered_at_ms), 0)
                 FROM peer_connection_summary WHERE user_id = ?1",
            )
            .map_err(store_err)?;
        let mut waiting_stmt = conn.prepare(&waiting_sql).map_err(store_err)?;

        let mut out = Vec::with_capacity(recipient_user_ids.len());
        for recipient in recipient_user_ids {
            // Skipped, not fatal. One degenerate id -- an old import, a
            // half-written restore -- must not empty the whole page: both
            // shells treat an error from this call as "no delivery state at
            // all", so failing the batch would silently blank every friend's
            // line rather than the one row that cannot be read. Dropping the
            // offending recipient degrades to exactly one missing line, the
            // same way a blocked identity does.
            if recipient.is_empty() || recipient.len() > 128 {
                continue;
            }
            let blocked = blocked_stmt
                .query_row(params![&recipient], |_| Ok(()))
                .optional()
                .map_err(store_err)?
                .is_some();
            if blocked {
                continue;
            }
            // A receipt covers everything at or below its watermark, so the
            // seek starts strictly above it. An over-reported watermark (the
            // repair lane reports a peer-stream MAX that can sit above
            // anything we hold) simply empties the range, which reads as
            // "nothing outstanding" -- the honest answer.
            let through_lamport: i64 = receipt_stmt
                .query_row(
                    params![&recipient, &own_user_id, RECEIPT_TYPE_DELIVERED as i64],
                    |row| row.get(0),
                )
                .optional()
                .map_err(store_err)?
                .unwrap_or(0);
            let (
                waiting_count,
                oldest_waiting_ms,
                last_upload_ms,
                oversized_waiting,
                unposted_waiting_count,
            ) = waiting_stmt
                .query_row(
                    params![
                        &recipient,
                        through_lamport,
                        now_ms,
                        MAX_ENVELOPE_SEALED_BYTES as i64
                    ],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)? != 0,
                            row.get::<_, i64>(4)?,
                        ))
                    },
                )
                .map_err(store_err)?;
            let last_delivered_ms: i64 = delivered_stmt
                .query_row(params![&recipient], |row| row.get(0))
                .map_err(store_err)?;
            let (reject_streak, rejected_at, unreachable_streak, unreachable_at) = contact_stmt
                .query_row(params![&recipient], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                })
                .optional()
                .map_err(store_err)?
                .unwrap_or((0, 0, 0, 0));
            out.push(CoreRecipientDeliveryStatus {
                recipient_user_id: recipient,
                waiting_count: waiting_count.max(0) as u64,
                oldest_waiting_ms: oldest_waiting_ms.max(0),
                last_progress_ms: last_upload_ms.max(last_delivered_ms).max(0),
                unposted_waiting_count: unposted_waiting_count.max(0) as u64,
                oversized_waiting,
                relay_reject_streak: reject_streak,
                relay_rejected_at_ms: rejected_at,
                relay_unreachable_streak: unreachable_streak,
                relay_unreachable_at_ms: unreachable_at,
            });
        }
        Ok(out)
    }

    /// Mark one outbound envelope as successfully posted to a relay. Returns
    /// `true` if a queued row was updated.
    pub fn mark_outbound_envelope_relay_posted(
        &self,
        msg_id: Vec<u8>,
        posted_at_ms: i64,
    ) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let changed = conn
            .execute(
                "UPDATE outbound_envelopes SET relay_posted_at = ?2 WHERE msg_id = ?1",
                params![msg_id, posted_at_ms],
            )
            .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// Delete expired outbound envelopes as of `now_ms`. The plaintext
    /// message history stays intact; this only prunes retry state whose public
    /// expiry window has elapsed.
    pub fn prune_expired_outbound_envelopes(&self, now_ms: i64) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let pruned = conn
            .execute(
                "DELETE FROM outbound_envelopes WHERE expiry <= ?1",
                params![now_ms],
            )
            .map_err(store_err)?;
        Ok(pruned as u64)
    }

    /// Which members of a group-addressed envelope's per-member relay fan-out
    /// have already landed durably on `relay_url`.
    ///
    /// A group envelope becomes one row per member on the wire, and the
    /// envelope's own `relay_posted_at` may only be stamped once every one of
    /// them landed. Without a per-member record the only safe answer to a
    /// partial failure is to re-post the whole set next pass, which is what
    /// the legacy engine does and what makes a twelve-member group cost
    /// twelve posts every pass while one member's row keeps failing. These
    /// markers gate **re-posting** only; like the carried-row marker they
    /// never feed a removal or an ack decision.
    ///
    /// Scoped to a mailbox for the same reason the carried marker is:
    /// "already posted to the old relay" says nothing about a new one.
    pub fn relay_fanout_posted_members(
        &self,
        msg_id: Vec<u8>,
        relay_url: String,
    ) -> Result<Vec<Vec<u8>>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT member_user_id FROM outbound_fanout_posted
                 WHERE msg_id = ?1 AND relay_url = ?2",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![msg_id, relay_url], |row| row.get::<_, Vec<u8>>(0))
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Record that one member's fan-out row landed on `relay_url`. Returns
    /// whether this call was the one that recorded it.
    pub fn mark_relay_fanout_row_posted(
        &self,
        msg_id: Vec<u8>,
        member_user_id: Vec<u8>,
        relay_url: String,
        now_ms: i64,
    ) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let changed = conn
            .execute(
                "INSERT OR IGNORE INTO outbound_fanout_posted
                     (msg_id, member_user_id, relay_url, posted_at)
                 VALUES (?1, ?2, ?3, ?4)",
                params![msg_id, member_user_id, relay_url, now_ms],
            )
            .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// Drop fan-out markers whose envelope is no longer an upload candidate --
    /// it was posted in full, expired, or was pruned. Housekeeping only: a
    /// stale marker could only ever suppress a re-post of a row whose envelope
    /// no longer exists, which costs nothing, but the table would otherwise
    /// grow without bound.
    pub fn prune_relay_fanout_markers(&self) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let pruned = conn
            .execute(
                "DELETE FROM outbound_fanout_posted
                 WHERE msg_id NOT IN (
                     SELECT msg_id FROM outbound_envelopes WHERE relay_posted_at IS NULL
                 )",
                [],
            )
            .map_err(store_err)?;
        Ok(pruned as u64)
    }

    /// Forget every fan-out marker, for the same reason
    /// [`MessageStore::clear_carried_relay_upload_markers`] exists: a changed
    /// endpoint makes every "already posted there" answer irrelevant.
    pub fn clear_relay_fanout_markers(&self) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let cleared = conn
            .execute("DELETE FROM outbound_fanout_posted", [])
            .map_err(store_err)?;
        Ok(cleared as u64)
    }

    /// The latest relay-uploadable receipt envelope persisted for this
    /// cumulative outgoing receipt watermark, if any.
    pub fn outgoing_receipt_envelope(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
    ) -> Result<Option<OutgoingReceiptEnvelope>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, receipt_type,
                    through_lamport, timestamp, hop_ttl, expiry, recipient_hint, sealed
             FROM outgoing_receipt_envelopes
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
            params![chat_id, sender_user_id, receipt_type as i64],
            row_to_outgoing_receipt,
        )
        .optional()
        .map_err(store_err)
    }

    /// Persist or advance the exact sealed receipt envelope to relay-upload
    /// for one logical outgoing receipt watermark. Same watermark -> no-op,
    /// preserving the existing `msg_id`; higher watermark -> replace the row
    /// and clear `relay_posted_at`; lower watermark -> ignored as stale.
    pub fn upsert_outgoing_receipt_envelope(
        &self,
        envelope: OutgoingReceiptEnvelope,
        queued_at_ms: i64,
    ) -> Result<bool, CoreError> {
        validate_receipt_watermark(envelope.receipt_type, envelope.through_lamport)?;
        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        let existing: Option<i64> = tx
            .query_row(
                "SELECT through_lamport FROM outgoing_receipt_envelopes
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![
                    &envelope.chat_id,
                    &envelope.sender_user_id,
                    envelope.receipt_type as i64,
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let changed = match existing {
            Some(current) if current >= envelope.through_lamport as i64 => false,
            Some(_) => {
                tx.execute(
                    "UPDATE outgoing_receipt_envelopes
                     SET msg_id = ?4,
                         recipient_user_id = ?5,
                         through_lamport = ?6,
                         timestamp = ?7,
                         hop_ttl = ?8,
                         expiry = ?9,
                         recipient_hint = ?10,
                         sealed = ?11,
                         queued_at = ?12,
                         relay_posted_at = NULL
                     WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                    params![
                        &envelope.chat_id,
                        &envelope.sender_user_id,
                        envelope.receipt_type as i64,
                        &envelope.msg_id,
                        &envelope.recipient_user_id,
                        envelope.through_lamport as i64,
                        envelope.timestamp,
                        envelope.hop_ttl as i64,
                        envelope.expiry,
                        &envelope.recipient_hint,
                        &envelope.sealed,
                        queued_at_ms,
                    ],
                )
                .map_err(store_err)?;
                true
            }
            None => {
                tx.execute(
                    "INSERT INTO outgoing_receipt_envelopes
                        (chat_id, sender_user_id, receipt_type, through_lamport, msg_id,
                         recipient_user_id, timestamp, hop_ttl, expiry, recipient_hint,
                         sealed, queued_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                    params![
                        &envelope.chat_id,
                        &envelope.sender_user_id,
                        envelope.receipt_type as i64,
                        envelope.through_lamport as i64,
                        &envelope.msg_id,
                        &envelope.recipient_user_id,
                        envelope.timestamp,
                        envelope.hop_ttl as i64,
                        envelope.expiry,
                        &envelope.recipient_hint,
                        &envelope.sealed,
                        queued_at_ms,
                    ],
                )
                .map_err(store_err)?;
                true
            }
        };
        tx.commit().map_err(store_err)?;
        Ok(changed)
    }

    /// Relay-upload candidates: persisted receipt envelopes not yet marked as
    /// posted to a relay, unexpired as of `now_ms`, oldest first.
    /// Receipts are drawn round-robin across recipients and honour the same
    /// skip set as [`MessageStore::pending_relay_outbound_envelopes`], for the
    /// same reason and with the same guarantees -- see that function's doc
    /// comment for why flat queue order starves.
    ///
    /// This queue is not a lesser case of the problem: in the field capture it
    /// was the one visibly failing (`Failed to upload receipt envelope` against
    /// an unreachable host, over and over). Receipts are also the queue most
    /// likely to be jammed by one bad contact, because every message received
    /// from anyone generates one and they are re-queued until they post.
    pub fn pending_relay_outgoing_receipt_envelopes(
        &self,
        limit: u64,
        now_ms: i64,
        skip_recipient_user_ids: Vec<Vec<u8>>,
    ) -> Result<Vec<OutgoingReceiptEnvelope>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut args: Vec<Value> = vec![Value::Integer(now_ms)];
        let skip_clause = if skip_recipient_user_ids.is_empty() {
            String::new()
        } else {
            let placeholders = vec!["?"; skip_recipient_user_ids.len()].join(", ");
            args.extend(skip_recipient_user_ids.into_iter().map(Value::Blob));
            format!(" AND recipient_user_id NOT IN ({placeholders})")
        };
        args.push(Value::Integer(limit as i64));
        let sql = format!(
            "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, receipt_type,
                    through_lamport, timestamp, hop_ttl, expiry, recipient_hint, sealed
             FROM (
                 SELECT *, ROW_NUMBER() OVER (
                            PARTITION BY recipient_user_id
                            ORDER BY queued_at ASC, msg_id ASC
                        ) AS recipient_rank
                 FROM outgoing_receipt_envelopes
                 WHERE relay_posted_at IS NULL AND expiry > ?{skip_clause}
             )
             ORDER BY recipient_rank ASC, queued_at ASC, msg_id ASC
             LIMIT ?"
        );
        let mut stmt = conn.prepare(&sql).map_err(store_err)?;
        let rows = stmt
            .query_map(params_from_iter(args.iter()), row_to_outgoing_receipt)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Mark one outgoing receipt envelope as successfully posted to a relay.
    /// Returns `true` if a queued row was updated.
    pub fn mark_outgoing_receipt_envelope_relay_posted(
        &self,
        msg_id: Vec<u8>,
        posted_at_ms: i64,
    ) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let changed = conn
            .execute(
                "UPDATE outgoing_receipt_envelopes SET relay_posted_at = ?2 WHERE msg_id = ?1",
                params![msg_id, posted_at_ms],
            )
            .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// Delete expired outgoing receipt envelopes as of `now_ms`. The
    /// underlying outgoing receipt watermark remains in `outgoing_receipts`;
    /// this only prunes the persisted sealed retry artifact.
    pub fn prune_expired_outgoing_receipt_envelopes(&self, now_ms: i64) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let pruned = conn
            .execute(
                "DELETE FROM outgoing_receipt_envelopes WHERE expiry <= ?1",
                params![now_ms],
            )
            .map_err(store_err)?;
        Ok(pruned as u64)
    }

    /// Record that a peer has delivered/read messages authored by
    /// `sender_user_id` in `chat_id` through `through_lamport` (DESIGN.md
    /// §7.2). Monotonic: if a receipt for the same (chat_id,
    /// sender_user_id, receipt_type) is already recorded with a
    /// `through_lamport` at or above this one, it's left unchanged --
    /// receipts can arrive out of order or be replayed under DTN, and a
    /// stale/duplicate receipt must never regress what's already known.
    ///
    /// `via_transport` (T6) is the transport the receipt itself returned on
    /// (the [`MessageArrival::transport`] encoding), recorded so a message's
    /// Info pane can prove *how* delivery was confirmed. It is overwritten
    /// when the watermark actually advances and the new receipt carries a
    /// known transport, and (FC4) also filled in when the watermark merely
    /// *matches* the stored one but the stored route is still unknown --
    /// otherwise a first confirmation whose return route we couldn't
    /// determine would permanently hide a later, more informative receipt at
    /// the same watermark. A stale/replayed receipt (lower watermark) never
    /// touches it, and a receipt with an unknown route (`via_transport =
    /// None`) never clears an already-known one. Pass `None` when the return
    /// route isn't known.
    ///
    /// **A delivered receipt also retires what it newly covers** (#283,
    /// contract `QUEUE-01`). Proof of delivery for a 1:1 outbound envelope is
    /// the removal condition the DTN ack-safety invariant permits, and this is
    /// the natural, incremental place to act on it: the queue shrinks in the
    /// same transaction that records the proof, rather than growing until a
    /// flat seven-day expiry. Nothing here touches group rows or carried rows
    /// -- `CARRY-01` is untouched, a carried copy still leaves only on its own
    /// digest proof -- and the retirement is deliberately narrower than the
    /// watermark: it removes a sealed *retransmission artifact* only where the
    /// `messages` row that regenerates it survives, so a peer that later
    /// notices a hole still gets the envelope rebuilt by the digest responder.
    /// See `crate::outbound_retirement` for the full predicate and its
    /// reasoning.
    ///
    /// **A delivered receipt is also the sole author of
    /// [`PeerConnectionEventKind::MessageDelivered`]**, and only when it newly
    /// covers a message a person can actually see. Both shells used to record
    /// that event unconditionally on every delivered receipt, which made the
    /// Connection details screen say "Received your message yesterday" about
    /// contacts nobody had written to in days: the app authors hidden service
    /// messages (profile sync, the friend directory, LAN endpoint hints,
    /// relay-change notices) into the same lamport stream, and a cumulative
    /// receipt covers those too. The inbound direction has always been narrow
    /// this way; this is its twin. See
    /// [`receipt_newly_covers_visible_authored_message`].
    ///
    /// `received_at_ms` is when this receipt reached this device -- the moment
    /// the event is dated with. `None` means the caller has no arrival to date
    /// the evidence by, and then no connection event is recorded at all: a
    /// wrong timestamp on a screen whose entire content is timestamps is worse
    /// than a missing line.
    pub fn record_receipt(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
        through_lamport: u64,
        via_transport: Option<u8>,
        received_at_ms: Option<i64>,
    ) -> Result<(), CoreError> {
        validate_receipt_watermark(receipt_type, through_lamport)?;
        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        let previous_through: i64 = tx
            .query_row(
                "SELECT through_lamport FROM receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![&chat_id, &sender_user_id, receipt_type as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?
            .unwrap_or(0);
        tx.execute(
            "INSERT INTO receipts (chat_id, sender_user_id, receipt_type, through_lamport, via_transport)
                VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                via_transport = CASE
                    WHEN excluded.via_transport IS NOT NULL
                         AND (
                             excluded.through_lamport > through_lamport
                             OR (excluded.through_lamport = through_lamport
                                 AND via_transport IS NULL)
                         )
                    THEN excluded.via_transport
                    ELSE via_transport END,
                through_lamport = MAX(through_lamport, excluded.through_lamport)",
            params![
                chat_id,
                sender_user_id,
                receipt_type as i64,
                through_lamport as i64,
                via_transport.map(|t| t as i64)
            ],
        )
        .map_err(store_err)?;
        if receipt_type == RECEIPT_TYPE_DELIVERED {
            // Against the *stored* watermark, not the one that just arrived: a
            // replayed or out-of-order receipt may be below what is already
            // known, and the queue must reflect the best proof held, not the
            // last message seen.
            let effective: i64 = tx
                .query_row(
                    "SELECT through_lamport FROM receipts
                     WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                    params![&chat_id, &sender_user_id, receipt_type as i64],
                    |row| row.get(0),
                )
                .map_err(store_err)?;
            let retired = crate::outbound_retirement::retire_receipt_covered(
                &tx,
                &chat_id,
                &sender_user_id,
                effective.max(0) as u64,
            )?;
            if retired > 0 {
                // Per receipt, not per row: a delivered watermark that covers
                // 200 envelopes is one decision, and recording it 200 times
                // would put a hot loop in the ring.
                crate::protocol_event::note_for(&tx, "peer", &chat_id, |peer| {
                    vec![crate::protocol_event::ProtocolEventDraft::new(
                        crate::protocol_event::ProtocolEventCode::OutboundRowRetired,
                        0,
                        "delivered_watermark_covered_them",
                    )
                    .actor(peer)
                    .invariants(&["QUEUE-01", "CARRY-01"])
                    .count("rows_retired", i64::try_from(retired).unwrap_or(i64::MAX))
                    .count("through_lamport", effective.max(0))]
                });
            }
            if let Some(received_at_ms) = received_at_ms {
                record_delivered_evidence(
                    &tx,
                    &chat_id,
                    &sender_user_id,
                    previous_through.max(0) as u64,
                    effective.max(0) as u64,
                    via_transport,
                    received_at_ms,
                )?;
            }
        }
        tx.commit().map_err(store_err)?;
        Ok(())
    }

    /// The cumulative lamport a receipt of `receipt_type` covers for
    /// `sender_user_id`'s messages in `chat_id` (DESIGN.md §7.2). Returns 0
    /// if no such receipt has been recorded.
    pub fn receipt_through(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
    ) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let through: Option<i64> = conn
            .query_row(
                "SELECT through_lamport FROM receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![chat_id, sender_user_id, receipt_type as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(through.unwrap_or(0) as u64)
    }

    /// The transport a peer's `receipt_type` receipt returned on for the
    /// highest watermark recorded so far (T6) -- the [`MessageArrival::transport`]
    /// encoding. `None` if no such receipt exists yet or its return route was
    /// unknown. Any message whose lamport is at or below
    /// [`MessageStore::receipt_through`] for the same key was confirmed by this
    /// route, so the Info pane can show it against every acknowledged message,
    /// not just the one at the exact watermark.
    pub fn receipt_via_transport(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
    ) -> Result<Option<u8>, CoreError> {
        let conn = lock_conn(&self.conn);
        let via: Option<Option<i64>> = conn
            .query_row(
                "SELECT via_transport FROM receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![chat_id, sender_user_id, receipt_type as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(via.flatten().map(|t| t as u8))
    }

    /// Record that *this device* has delivered/read messages authored by
    /// `sender_user_id` in `chat_id` through `through_lamport` -- the
    /// cumulative receipt watermark to send back on the next peer sync
    /// (DESIGN.md §7.2, §7.3). Monotonic for the same reason as
    /// [`MessageStore::record_receipt`]: once a receipt watermark advances,
    /// stale retries must never regress it.
    pub fn record_outgoing_receipt(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
        through_lamport: u64,
    ) -> Result<(), CoreError> {
        validate_receipt_watermark(receipt_type, through_lamport)?;
        let conn = lock_conn(&self.conn);
        conn.execute(
            "INSERT INTO outgoing_receipts (chat_id, sender_user_id, receipt_type, through_lamport)
                VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(chat_id, sender_user_id, receipt_type) DO UPDATE SET
                through_lamport = MAX(through_lamport, excluded.through_lamport)",
            params![
                chat_id,
                sender_user_id,
                receipt_type as i64,
                through_lamport as i64
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// The cumulative lamport this device should report back in an outgoing
    /// receipt of `receipt_type` for `sender_user_id`'s messages in `chat_id`
    /// (DESIGN.md §7.2, §7.3). Returns 0 if no such local receipt state has
    /// been recorded yet.
    pub fn outgoing_receipt_through(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        receipt_type: u8,
    ) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let through: Option<i64> = conn
            .query_row(
                "SELECT through_lamport FROM outgoing_receipts
                 WHERE chat_id = ?1 AND sender_user_id = ?2 AND receipt_type = ?3",
                params![chat_id, sender_user_id, receipt_type as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(through.unwrap_or(0) as u64)
    }

    /// Record that `member_user_id` has delivered/read messages authored by
    /// `author_user_id` in `group_id` through `through_lamport`. Monotonic
    /// and isolated from the 1:1 `receipts` table so a group watermark can
    /// never paint ticks on a pairwise chat.
    pub fn record_group_receipt(
        &self,
        group_id: Vec<u8>,
        author_user_id: Vec<u8>,
        member_user_id: Vec<u8>,
        receipt_type: u8,
        through_lamport: u64,
        via_transport: Option<u8>,
    ) -> Result<(), CoreError> {
        validate_receipt_watermark(receipt_type, through_lamport)?;
        if group_id.len() != crate::GROUP_ID_LEN {
            return Err(CoreError::Malformed(format!(
                "group receipt id must be exactly {} bytes",
                crate::GROUP_ID_LEN
            )));
        }
        let conn = lock_conn(&self.conn);
        conn.execute(
            "INSERT INTO group_receipts
                (group_id, author_user_id, member_user_id, receipt_type, through_lamport, via_transport)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(group_id, author_user_id, member_user_id, receipt_type) DO UPDATE SET
                via_transport = CASE
                    WHEN excluded.via_transport IS NOT NULL
                         AND (
                             excluded.through_lamport > through_lamport
                             OR (excluded.through_lamport = through_lamport
                                 AND via_transport IS NULL)
                         )
                    THEN excluded.via_transport
                    ELSE via_transport END,
                through_lamport = MAX(through_lamport, excluded.through_lamport)",
            params![
                group_id,
                author_user_id,
                member_user_id,
                receipt_type as i64,
                through_lamport as i64,
                via_transport.map(|t| t as i64)
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Cumulative group watermark `member` has reported for `author`'s
    /// stream in `group_id`. 0 if none has been recorded.
    pub fn group_receipt_through(
        &self,
        group_id: Vec<u8>,
        author_user_id: Vec<u8>,
        member_user_id: Vec<u8>,
        receipt_type: u8,
    ) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let through: Option<i64> = conn
            .query_row(
                "SELECT through_lamport FROM group_receipts
                 WHERE group_id = ?1 AND author_user_id = ?2
                   AND member_user_id = ?3 AND receipt_type = ?4",
                params![
                    group_id,
                    author_user_id,
                    member_user_id,
                    receipt_type as i64
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(through.unwrap_or(0) as u64)
    }

    /// T6 route the member's `receipt_type` watermark last advanced on, if any.
    pub fn group_receipt_via_transport(
        &self,
        group_id: Vec<u8>,
        author_user_id: Vec<u8>,
        member_user_id: Vec<u8>,
        receipt_type: u8,
    ) -> Result<Option<u8>, CoreError> {
        let conn = lock_conn(&self.conn);
        let via: Option<Option<i64>> = conn
            .query_row(
                "SELECT via_transport FROM group_receipts
                 WHERE group_id = ?1 AND author_user_id = ?2
                   AND member_user_id = ?3 AND receipt_type = ?4",
                params![
                    group_id,
                    author_user_id,
                    member_user_id,
                    receipt_type as i64
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(via.flatten().map(|t| t as u8))
    }

    /// Per-member delivered/read snapshot for `author_user_id`'s stream in
    /// this group. `member_user_ids` is the current roster the caller wants
    /// considered (typically `group.member_user_ids`); members not in that
    /// list are omitted so a departed member cannot hold the aggregate tick.
    pub fn group_receipt_state(
        &self,
        group_id: Vec<u8>,
        author_user_id: Vec<u8>,
        member_user_ids: Vec<Vec<u8>>,
    ) -> Result<GroupReceiptState, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut added_at: HashMap<Vec<u8>, i64> = HashMap::new();
        {
            let mut stmt = conn
                .prepare("SELECT user_id, added_at_ms FROM group_members WHERE group_id = ?1")
                .map_err(store_err)?;
            let rows = stmt
                .query_map(params![&group_id], |row| {
                    Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(store_err)?;
            for row in rows {
                let (user_id, at) = row.map_err(store_err)?;
                added_at.insert(user_id, at);
            }
        }
        let mut watermarks: HashMap<(Vec<u8>, u8), (u64, Option<u8>)> = HashMap::new();
        {
            let mut stmt = conn
                .prepare(
                    "SELECT member_user_id, receipt_type, through_lamport, via_transport
                     FROM group_receipts
                     WHERE group_id = ?1 AND author_user_id = ?2",
                )
                .map_err(store_err)?;
            let rows = stmt
                .query_map(params![&group_id, &author_user_id], |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, i64>(1)? as u8,
                        row.get::<_, i64>(2)? as u64,
                        row.get::<_, Option<i64>>(3)?,
                    ))
                })
                .map_err(store_err)?;
            for row in rows {
                let (member, receipt_type, through, via) = row.map_err(store_err)?;
                watermarks.insert((member, receipt_type), (through, via.map(|v| v as u8)));
            }
        }
        let mut members = Vec::with_capacity(member_user_ids.len());
        for member_user_id in member_user_ids {
            let delivered = watermarks
                .get(&(member_user_id.clone(), RECEIPT_TYPE_DELIVERED))
                .cloned()
                .unwrap_or((0, None));
            let read = watermarks
                .get(&(member_user_id.clone(), RECEIPT_TYPE_READ))
                .map(|(through, _)| *through)
                .unwrap_or(0);
            members.push(GroupMemberReceipt {
                added_at_ms: added_at.get(&member_user_id).copied().unwrap_or(0),
                member_user_id,
                delivered_through: delivered.0,
                read_through: read,
                delivered_via_transport: delivered.1,
            });
        }
        Ok(GroupReceiptState { members })
    }

    /// Add or update a contact, keyed on `user_id` -- re-scanning the same
    /// FriendCard (e.g. after they update their display name) replaces the
    /// row rather than erroring or duplicating.
    pub fn upsert_contact(&self, contact: Contact) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "INSERT INTO contacts (user_id, name, sign_pk, agree_pk, relay_url, relay_token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(user_id) DO UPDATE SET
                name = excluded.name,
                sign_pk = excluded.sign_pk,
                agree_pk = excluded.agree_pk,
                relay_url = excluded.relay_url,
                relay_token = excluded.relay_token,
                -- A card that moves the endpoint earns a clean slate; one
                -- that re-states the same endpoint keeps its streak, so
                -- re-importing an unchanged stale card cannot launder it
                -- back to healthy. `IS NOT` is the null-safe comparison:
                -- plain `<>` is NULL for a card with no relay fields, which
                -- would silently take the ELSE branch.
                relay_reject_streak = CASE
                    WHEN excluded.relay_url IS NOT contacts.relay_url
                      OR excluded.relay_token IS NOT contacts.relay_token
                    THEN 0 ELSE contacts.relay_reject_streak END,
                relay_rejected_at = CASE
                    WHEN excluded.relay_url IS NOT contacts.relay_url
                      OR excluded.relay_token IS NOT contacts.relay_token
                    THEN 0 ELSE contacts.relay_rejected_at END,
                relay_unreachable_endpoint_key = CASE
                    WHEN excluded.relay_url IS NOT contacts.relay_url
                      OR excluded.relay_token IS NOT contacts.relay_token
                    THEN NULL ELSE contacts.relay_unreachable_endpoint_key END,
                relay_unreachable_streak = CASE
                    WHEN excluded.relay_url IS NOT contacts.relay_url
                      OR excluded.relay_token IS NOT contacts.relay_token
                    THEN 0 ELSE contacts.relay_unreachable_streak END,
                relay_unreachable_at = CASE
                    WHEN excluded.relay_url IS NOT contacts.relay_url
                      OR excluded.relay_token IS NOT contacts.relay_token
                    THEN 0 ELSE contacts.relay_unreachable_at END",
            params![
                contact.user_id,
                contact.name,
                contact.sign_pk,
                contact.agree_pk,
                contact.relay_url,
                contact.relay_token,
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Import a friend card without allowing an older/blank card to erase a
    /// complete relay configuration already known for that contact.
    pub fn upsert_imported_contact(&self, mut contact: Contact) -> Result<Contact, CoreError> {
        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        let incoming_has_relay = contact
            .relay_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && contact
                .relay_token
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty());
        if !incoming_has_relay {
            let existing: Option<(Option<String>, Option<String>)> = tx
                .query_row(
                    "SELECT relay_url, relay_token FROM contacts WHERE user_id = ?1",
                    params![contact.user_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(store_err)?;
            if let Some((url, token)) = existing {
                let existing_has_relay =
                    url.as_deref().is_some_and(|value| !value.trim().is_empty())
                        && token
                            .as_deref()
                            .is_some_and(|value| !value.trim().is_empty());
                if existing_has_relay {
                    contact.relay_url = url;
                    contact.relay_token = token;
                }
            }
        }
        tx.execute(
            "INSERT INTO contacts (user_id, name, sign_pk, agree_pk, relay_url, relay_token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(user_id) DO UPDATE SET
                name = excluded.name,
                sign_pk = excluded.sign_pk,
                agree_pk = excluded.agree_pk,
                relay_url = excluded.relay_url,
                relay_token = excluded.relay_token,
                -- A card that moves the endpoint earns a clean slate; one
                -- that re-states the same endpoint keeps its streak, so
                -- re-importing an unchanged stale card cannot launder it
                -- back to healthy. `IS NOT` is the null-safe comparison:
                -- plain `<>` is NULL for a card with no relay fields, which
                -- would silently take the ELSE branch.
                relay_reject_streak = CASE
                    WHEN excluded.relay_url IS NOT contacts.relay_url
                      OR excluded.relay_token IS NOT contacts.relay_token
                    THEN 0 ELSE contacts.relay_reject_streak END,
                relay_rejected_at = CASE
                    WHEN excluded.relay_url IS NOT contacts.relay_url
                      OR excluded.relay_token IS NOT contacts.relay_token
                    THEN 0 ELSE contacts.relay_rejected_at END,
                relay_unreachable_endpoint_key = CASE
                    WHEN excluded.relay_url IS NOT contacts.relay_url
                      OR excluded.relay_token IS NOT contacts.relay_token
                    THEN NULL ELSE contacts.relay_unreachable_endpoint_key END,
                relay_unreachable_streak = CASE
                    WHEN excluded.relay_url IS NOT contacts.relay_url
                      OR excluded.relay_token IS NOT contacts.relay_token
                    THEN 0 ELSE contacts.relay_unreachable_streak END,
                relay_unreachable_at = CASE
                    WHEN excluded.relay_url IS NOT contacts.relay_url
                      OR excluded.relay_token IS NOT contacts.relay_token
                    THEN 0 ELSE contacts.relay_unreachable_at END",
            params![
                contact.user_id,
                contact.name,
                contact.sign_pk,
                contact.agree_pk,
                contact.relay_url,
                contact.relay_token,
            ],
        )
        .map_err(store_err)?;
        // A deliberate direct import (QR scan / pasted card) is the one act
        // that clears a block tombstone (specs/friends-of-friends.md §"delete
        // + tombstone") — remote-initiated writes never reach here.
        tx.execute(
            "DELETE FROM blocked_identities WHERE user_id = ?1",
            params![contact.user_id],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(contact)
    }

    /// Apply a contact avatar update only if `epoch` is newer than the stored
    /// avatar epoch. `None` or an empty blob clears the avatar but still
    /// records the newer epoch.
    pub fn set_contact_avatar(
        &self,
        user_id: Vec<u8>,
        avatar: Option<Vec<u8>>,
        epoch: i64,
    ) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let avatar = avatar.filter(|bytes| !bytes.is_empty());
        let changed = conn
            .execute(
                "UPDATE contacts
                 SET avatar = ?2, avatar_epoch = ?3
                 WHERE user_id = ?1 AND ?3 > avatar_epoch",
                params![user_id, avatar, epoch],
            )
            .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// T23: apply a contact's relay-change notice to their stored endpoint.
    ///
    /// Three rules, all enforced here rather than in either shell, because
    /// getting any of them wrong in one platform's copy is exactly the class
    /// of bug `core-first` exists to prevent:
    ///
    /// 1. **Sender scoping.** `sender_user_id` is the identity that *sealed*
    ///    the envelope, as verified by [`crate::open_message`]. A notice may
    ///    only move its own sender's endpoint; one claiming a different
    ///    `subject_user_id` is rejected outright. Pairwise sealing already
    ///    makes forging someone else's notice hard, but "hard" is not the
    ///    invariant CLAUDE.md states — a device never publishes, forwards, or
    ///    accepts a *third party's* endpoint, full stop.
    /// 2. **Deposit-class only** (CP4), re-checked independently of the
    ///    decoder so a future caller cannot route around it.
    /// 3. **Monotonic epochs**, the same "apply only if newer" discipline as
    ///    [`MessageStore::set_contact_avatar`]. Sideband traffic reorders
    ///    freely over DTN and replays cheaply off a relay, so a notice that
    ///    is not strictly newer than what is stored must never regress an
    ///    endpoint that already got repaired.
    ///
    /// Returns whether the contact's endpoint actually moved. `false` covers
    /// both "not a contact" and "we already hold this or newer" — neither is
    /// an error, both are ordinary outcomes of a spray/replay.
    pub fn apply_contact_relay_update(
        &self,
        sender_user_id: Vec<u8>,
        content: RelayUpdateContent,
    ) -> Result<bool, CoreError> {
        if content.subject_user_id != sender_user_id {
            return Err(CoreError::Malformed(
                "relay update may only change the sending contact's own endpoint".into(),
            ));
        }
        crate::protocol::validate_relay_update_credential(
            &content.relay_url,
            &content.relay_token,
        )?;
        let url = (!content.relay_url.is_empty()).then_some(content.relay_url);
        let token = (!content.relay_token.is_empty()).then_some(content.relay_token);
        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        let changed = tx
            .execute(
                // A newly announced endpoint has never been tried, so it
                // starts trusted: carrying the old endpoint's rejection
                // streak forward would write off the very notice that
                // repairs the contact.
                "UPDATE contacts
                 SET relay_url = ?2, relay_token = ?3, relay_epoch = ?4,
                      relay_reject_streak = 0, relay_rejected_at = 0,
                      relay_unreachable_endpoint_key = NULL,
                      relay_unreachable_streak = 0, relay_unreachable_at = 0
                 WHERE user_id = ?1 AND ?4 > relay_epoch",
                params![sender_user_id, url, token, content.relay_epoch],
            )
            .map_err(store_err)?;
        if changed > 0 {
            // The contact's mail now targets a different mailbox, so
            // "already uploaded" no longer holds -- re-offer the carry
            // queue once (see clear_carried_relay_upload_markers's doc for
            // why the clear is wholesale rather than per-contact).
            tx.execute(
                "UPDATE carried_envelopes SET relay_uploaded_to = NULL
                 WHERE relay_uploaded_to IS NOT NULL",
                [],
            )
            .map_err(store_err)?;
        }
        tx.commit().map_err(store_err)?;
        Ok(changed > 0)
    }

    /// The newest relay-change epoch applied for a contact (T23). 0 means no
    /// notice has ever been applied — their endpoint is still whatever their
    /// friend card carried.
    pub fn contact_relay_epoch(&self, user_id: Vec<u8>) -> Result<i64, CoreError> {
        let conn = lock_conn(&self.conn);
        let epoch: Option<i64> = conn
            .query_row(
                "SELECT relay_epoch FROM contacts WHERE user_id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(epoch.unwrap_or(0))
    }

    /// Record one authoritative rejection from a contact's card endpoint and
    /// return the resulting streak (see `crate::contact_relay_health`).
    ///
    /// Advancing the streak also re-stamps `relay_rejected_at`, so the
    /// six-hour re-probe window is measured from the most recent evidence
    /// rather than from the first failure — a card that has been dead for a
    /// week is probed once every six hours, not continuously.
    pub fn note_contact_relay_rejected(
        &self,
        user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<i64, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "UPDATE contacts
             SET relay_reject_streak = relay_reject_streak + 1, relay_rejected_at = ?2
             WHERE user_id = ?1",
            params![user_id, now_ms],
        )
        .map_err(store_err)?;
        let streak: Option<i64> = conn
            .query_row(
                "SELECT relay_reject_streak FROM contacts WHERE user_id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let streak = streak.unwrap_or(0);
        crate::protocol_event::note_for(&conn, "peer", &user_id, |peer| {
            vec![crate::protocol_event::ProtocolEventDraft::new(
                crate::protocol_event::ProtocolEventCode::RequestRejected,
                now_ms,
                "contact_endpoint_rejected_us_authoritatively",
            )
            .actor(peer)
            .invariants(&["SILENCE-01"])
            .count("reject_streak", streak)]
        });
        Ok(streak)
    }

    /// Forget any recorded rejection for a contact — called on every
    /// successful post to their endpoint.
    ///
    /// Success is the only thing that clears a streak. In particular a
    /// *transient* fault must not, or a dead endpoint that also rate-limits
    /// us could launder itself back to healthy on the strength of the 429
    /// and resume hammering forever.
    pub fn clear_contact_relay_rejection(&self, user_id: Vec<u8>) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "UPDATE contacts
             SET relay_reject_streak = 0, relay_rejected_at = 0
             WHERE user_id = ?1 AND relay_reject_streak <> 0",
            params![user_id],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Every contact whose card endpoint currently carries a rejection
    /// streak. Read once per sync pass and consulted per contact, rather
    /// than a query per contact per pass.
    pub fn list_contact_relay_rejections(&self) -> Result<Vec<ContactRelayRejection>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT user_id, relay_reject_streak, relay_rejected_at
                 FROM contacts WHERE relay_reject_streak > 0",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ContactRelayRejection {
                    user_id: row.get(0)?,
                    reject_streak: row.get(1)?,
                    rejected_at_ms: row.get(2)?,
                })
            })
            .map_err(store_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(store_err)?;
        Ok(rows)
    }

    /// Record one sync pass in which this contact's endpoint gave no HTTP
    /// answer even though another relay proved this device was online.
    ///
    /// The endpoint hash is part of the state: a changed friend card starts at
    /// one rather than inheriting a retired host's streak. The shell decides,
    /// through [`crate::core_contact_relay_unreachable_delta`], whether the
    /// observation is strong enough to call this method.
    pub fn note_contact_relay_unreachable(
        &self,
        user_id: Vec<u8>,
        endpoint_key: String,
        now_ms: i64,
    ) -> Result<i64, CoreError> {
        if endpoint_key.is_empty() {
            return Err(CoreError::Malformed(
                "contact relay endpoint key must not be empty".into(),
            ));
        }
        let conn = lock_conn(&self.conn);
        conn.execute(
            "UPDATE contacts
             SET relay_unreachable_streak = CASE
                     WHEN relay_unreachable_endpoint_key = ?2
                     THEN relay_unreachable_streak + 1
                     ELSE 1
                 END,
                 relay_unreachable_endpoint_key = ?2,
                 relay_unreachable_at = ?3
             WHERE user_id = ?1",
            params![user_id, endpoint_key, now_ms],
        )
        .map_err(store_err)?;
        let streak: Option<i64> = conn
            .query_row(
                "SELECT relay_unreachable_streak FROM contacts WHERE user_id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let streak = streak.unwrap_or(0);
        if streak == crate::CONTACT_RELAY_UNREACHABLE_STREAK {
            // Only on the crossing, not on every failure after it. A dead
            // endpoint that is retried once every rest window would otherwise
            // fill the ring with the same record and evict the evidence of how
            // it got there.
            crate::protocol_event::note_for(&conn, "peer", &user_id, |peer| {
                vec![crate::protocol_event::ProtocolEventDraft::new(
                    crate::protocol_event::ProtocolEventCode::EndpointRested,
                    now_ms,
                    "no_answer_streak_reached_the_rest_threshold",
                )
                .actor(peer)
                .invariants(&["SILENCE-01"])
                .count("unreachable_streak", streak)]
            });
        }
        Ok(streak)
    }

    /// Clear the transport-level streak when the endpoint gives any HTTP
    /// answer. A 401 may advance the separate rejection streak, but it proves
    /// the host is reachable and must settle the silence verdict.
    pub fn clear_contact_relay_unreachable(&self, user_id: Vec<u8>) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        let recovered = conn
            .execute(
                "UPDATE contacts
                 SET relay_unreachable_endpoint_key = NULL,
                     relay_unreachable_streak = 0,
                     relay_unreachable_at = 0
                 WHERE user_id = ?1 AND relay_unreachable_streak <> 0",
                params![user_id],
            )
            .map_err(store_err)?;
        if recovered > 0 {
            crate::protocol_event::note_for(&conn, "peer", &user_id, |peer| {
                vec![crate::protocol_event::ProtocolEventDraft::new(
                    crate::protocol_event::ProtocolEventCode::EndpointRecovered,
                    0,
                    "endpoint_answered_again",
                )
                .actor(peer)
                .invariants(&["SILENCE-01"])]
            });
        }
        Ok(())
    }

    /// Every contact endpoint carrying a persisted no-answer streak. Read once
    /// per sync pass so rest windows and the stale-contact UI survive process
    /// restarts without a query per contact.
    pub fn list_contact_relay_unreachable(
        &self,
    ) -> Result<Vec<ContactRelayUnreachable>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT user_id, relay_unreachable_endpoint_key,
                        relay_unreachable_streak, relay_unreachable_at
                 FROM contacts
                 WHERE relay_unreachable_streak > 0
                   AND relay_unreachable_endpoint_key IS NOT NULL",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ContactRelayUnreachable {
                    user_id: row.get(0)?,
                    endpoint_key: row.get(1)?,
                    unreachable_streak: row.get(2)?,
                    unreachable_at_ms: row.get(3)?,
                })
            })
            .map_err(store_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(store_err)?;
        Ok(rows)
    }

    /// Where the walk of one relay mailbox got to (see
    /// [`crate::relay_cursor`]). An unknown or empty `config_key` reads as
    /// "nothing remembered": start at 0, sweep is due.
    pub fn relay_fetch_cursor(&self, config_key: String) -> Result<RelayFetchCursor, CoreError> {
        if config_key.is_empty() {
            return Ok(RelayFetchCursor {
                after_id: 0,
                last_sweep_at_ms: 0,
                sweep_after_id: 0,
                sweep_started_at_ms: 0,
            });
        }
        let conn = lock_conn(&self.conn);
        let row: Option<(i64, i64, i64, i64)> = conn
            .query_row(
                "SELECT after_id, last_sweep_at, sweep_after_id, sweep_started_at
                 FROM relay_fetch_cursors WHERE config_key = ?1",
                params![config_key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(store_err)?;
        let (after_id, last_sweep_at_ms, sweep_after_id, sweep_started_at_ms) =
            row.unwrap_or((0, 0, 0, 0));
        Ok(RelayFetchCursor {
            after_id,
            last_sweep_at_ms,
            sweep_after_id,
            sweep_started_at_ms,
        })
    }

    /// Persist the frontier after one fetch page, and return what is now
    /// remembered.
    ///
    /// The decision itself is [`crate::relay_cursor_advance`] — the store only
    /// reads, applies it, and writes — so the safety rule (never move past a
    /// page that did not reach a terminal disposition for every envelope, and
    /// never move backwards) is stated once, in policy, and tested there
    /// rather than through SQL.
    ///
    /// A blank `config_key` (an endpoint with no URL or no token) persists
    /// nothing: such a config always walks from 0 rather than sharing one row
    /// with every other incomplete config.
    pub fn advance_relay_fetch_cursor(
        &self,
        config_key: String,
        page_next_cursor: i64,
        page_fully_processed: bool,
    ) -> Result<i64, CoreError> {
        if config_key.is_empty() {
            return Ok(0);
        }
        let conn = lock_conn(&self.conn);
        let persisted: i64 = conn
            .query_row(
                "SELECT after_id FROM relay_fetch_cursors WHERE config_key = ?1",
                params![config_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?
            .unwrap_or(0);
        let advanced =
            crate::relay_cursor_advance(persisted, page_next_cursor, page_fully_processed);
        if advanced == persisted {
            // A held frontier is the interesting half. `sweep-livelock` is
            // exactly this record repeating with the same numbers, and until
            // now the only way to see it was to read a device's prose log.
            crate::protocol_event::note_for(&conn, "mailbox", config_key.as_bytes(), |mailbox| {
                vec![crate::protocol_event::ProtocolEventDraft::new(
                    crate::protocol_event::ProtocolEventCode::FrontierHeld,
                    // This method has no clock argument, and adding one would
                    // change an exported signature both shells call. The ring
                    // clamps forward instead and marks the record `inferred_at`,
                    // so it reads as "no earlier than the one before it" --
                    // which is exactly what is known here, and a reader is not
                    // invited to treat a borrowed timestamp as a measured one.
                    // C4 gives the walk a real clock.
                    0,
                    if page_fully_processed {
                        "page_did_not_advance_the_cursor"
                    } else {
                        "page_not_fully_processed"
                    },
                )
                .actor(mailbox)
                .invariants(&["CURSOR-01", "PAGE-01"])
                .count("frontier", persisted.max(0))
                .count("page_next_cursor", page_next_cursor.max(0))]
            });
            return Ok(persisted);
        }
        conn.execute(
            "INSERT INTO relay_fetch_cursors (config_key, after_id, last_sweep_at)
             VALUES (?1, ?2, 0)
             ON CONFLICT(config_key) DO UPDATE SET after_id = ?2",
            params![config_key, advanced],
        )
        .map_err(store_err)?;
        crate::protocol_event::note_for(&conn, "mailbox", config_key.as_bytes(), |mailbox| {
            vec![crate::protocol_event::ProtocolEventDraft::new(
                crate::protocol_event::ProtocolEventCode::FrontierAdvanced,
                0,
                "page_fully_processed",
            )
            .actor(mailbox)
            .invariants(&["CURSOR-01"])
            .count("frontier_before", persisted.max(0))
            .count("frontier_after", advanced.max(0))]
        });
        Ok(advanced)
    }

    /// Persist how far the sweep now under way has walked, and return what is
    /// now remembered.
    ///
    /// The frontier's twin, and deliberately a separate column rather than a
    /// second use of `after_id`: [`crate::relay_cursor_advance`] never lets the
    /// frontier move backwards, so on a mailbox already walked to the top the
    /// frontier cannot say where a sweep has got to. Without a cursor of its
    /// own a sweep restarted at 0 on every yield and, on any mailbox holding
    /// more rows than one bounded pass can take, never reached the empty page
    /// that completes it — a permanent re-download loop.
    ///
    /// It obeys the same rule the frontier does, decided by the same function:
    /// it moves only for a page that reached a terminal disposition for every
    /// envelope *and* landed its acks, and it never moves backwards. Call it
    /// only while actually sweeping — an ordinary pass writing its page
    /// cursors here would leave behind progress claiming coverage of rows the
    /// sweep never looked at, and `sweep_after_id` is also what
    /// [`crate::relay_sweep_due`] reads as "a sweep is under way".
    ///
    /// The first page that actually moves a sweep off 0 also dates the sweep,
    /// in `sweep_started_at`. Nothing else writes it, so it is the age of the
    /// walk rather than of the last page — which is the question
    /// [`crate::relay_sweep_restart_from_zero`] has to answer, and the reason
    /// `now_ms` is a parameter of an otherwise timeless function.
    pub fn advance_relay_sweep_cursor(
        &self,
        config_key: String,
        page_next_cursor: i64,
        page_fully_processed: bool,
        now_ms: i64,
    ) -> Result<i64, CoreError> {
        if config_key.is_empty() {
            return Ok(0);
        }
        let conn = lock_conn(&self.conn);
        let persisted: i64 = conn
            .query_row(
                "SELECT sweep_after_id FROM relay_fetch_cursors WHERE config_key = ?1",
                params![config_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?
            .unwrap_or(0);
        let advanced =
            crate::relay_cursor_advance(persisted, page_next_cursor, page_fully_processed);
        if advanced == persisted {
            crate::protocol_event::note_for(&conn, "mailbox", config_key.as_bytes(), |mailbox| {
                vec![crate::protocol_event::ProtocolEventDraft::new(
                    crate::protocol_event::ProtocolEventCode::FrontierHeld,
                    now_ms,
                    "sweep_cursor_held",
                )
                .actor(mailbox)
                .invariants(&["CURSOR-01", "PROGRESS-01"])
                .count("sweep_cursor", persisted.max(0))
                .count("page_next_cursor", page_next_cursor.max(0))]
            });
            return Ok(persisted);
        }
        crate::protocol_event::note_for(&conn, "mailbox", config_key.as_bytes(), |mailbox| {
            vec![crate::protocol_event::ProtocolEventDraft::new(
                if persisted <= 0 {
                    crate::protocol_event::ProtocolEventCode::SweepStarted
                } else {
                    crate::protocol_event::ProtocolEventCode::SweepResumed
                },
                now_ms,
                "page_fully_processed",
            )
            .actor(mailbox)
            .invariants(&["CURSOR-01", "PROGRESS-01"])
            .count("sweep_cursor_before", persisted.max(0))
            .count("sweep_cursor_after", advanced.max(0))]
        });
        if persisted <= 0 {
            // This sweep's first page: date it.
            conn.execute(
                "INSERT INTO relay_fetch_cursors
                     (config_key, after_id, last_sweep_at, sweep_after_id, sweep_started_at)
                 VALUES (?1, 0, 0, ?2, ?3)
                 ON CONFLICT(config_key)
                 DO UPDATE SET sweep_after_id = ?2, sweep_started_at = ?3",
                params![config_key, advanced, now_ms],
            )
            .map_err(store_err)?;
        } else {
            conn.execute(
                "INSERT INTO relay_fetch_cursors
                     (config_key, after_id, last_sweep_at, sweep_after_id)
                 VALUES (?1, 0, 0, ?2)
                 ON CONFLICT(config_key) DO UPDATE SET sweep_after_id = ?2",
                params![config_key, advanced],
            )
            .map_err(store_err)?;
        }
        Ok(advanced)
    }

    /// Forget how far the sweep now under way has walked, so the next walk of
    /// this mailbox starts at 0 again, and date the restart.
    ///
    /// Two callers, both of them cases where the remembered progress has
    /// stopped meaning what it says:
    ///
    /// - a sweep whose resume cursor is stale enough to be pointing into an id
    ///   space that no longer exists ([`crate::relay_sweep_restart_from_zero`]
    ///   decides; the shell calls this and then walks from 0 in the same
    ///   pass);
    /// - a walk abandoned because the relay returned rows without advancing
    ///   its cursor. That mailbox is answering incoherently, and leaving
    ///   non-zero progress behind would hold it in "a sweep is under way"
    ///   ([`crate::relay_sweep_due`]) on *every* pass from then on — which
    ///   also means never running an ordinary frontier pass against it again,
    ///   so new mail at the top of that mailbox would stop arriving
    ///   altogether. Clearing it hands the mailbox back to the schedule.
    ///
    /// Stamping `sweep_started_at` matters for the first caller and is
    /// harmless for the second: it is what stops the walk this call is about
    /// to start from being judged stale on the very next pass and restarted
    /// again, which would be the same re-download loop in a new costume.
    /// Writes nothing for a mailbox with no cursor row — there is no progress
    /// to forget, and inventing a row would only claim a sweep that is not
    /// happening.
    pub fn reset_relay_sweep_progress(
        &self,
        config_key: String,
        now_ms: i64,
    ) -> Result<(), CoreError> {
        if config_key.is_empty() {
            return Ok(());
        }
        let conn = lock_conn(&self.conn);
        let forgotten = conn
            .execute(
                "UPDATE relay_fetch_cursors SET sweep_after_id = 0, sweep_started_at = ?2
                 WHERE config_key = ?1",
                params![config_key, now_ms],
            )
            .map_err(store_err)?;
        if forgotten > 0 {
            crate::protocol_event::note_for(&conn, "mailbox", config_key.as_bytes(), |mailbox| {
                vec![crate::protocol_event::ProtocolEventDraft::new(
                    crate::protocol_event::ProtocolEventCode::SweepRestarted,
                    now_ms,
                    "resume_cursor_no_longer_meant_what_it_said",
                )
                .actor(mailbox)
                .invariants(&["PROGRESS-01"])]
            });
        }
        Ok(())
    }

    /// Record that a walk from 0 completed for this mailbox, restarting its
    /// sweep interval, clearing the sweep's resume cursor, and lowering the
    /// frontier if the walk proved it sits above the top of the mailbox.
    /// Reports whether the frontier was lowered.
    ///
    /// Called only when the walk actually reached the end of the mailbox. A
    /// sweep cut short — the service stopped, internet went away, the relay
    /// errored, or the walk simply ran out of its per-pass budget —
    /// deliberately leaves the timestamp alone, so the next pass finishes the
    /// sweep instead of believing a partial re-walk was a full one. The
    /// resume cursor is what lets "the next pass finishes it" be true rather
    /// than aspirational, and clearing it here is the single act that turns a
    /// sweep from in-progress back into scheduled.
    ///
    /// `swept_through_id` is the `after=` the walk was holding when the empty
    /// page arrived — the highest id of a hint-matching row the sweep proved
    /// exists. [`crate::relay_frontier_after_completed_sweep`] decides what to
    /// do with it, and that doc comment carries the reasoning: it is the one
    /// place the frontier moves *down*, it only does so on a mailbox that had
    /// rows in it, and it never moves the frontier up. Restricting the whole
    /// question to this method is what keeps "only a completed sweep is
    /// evidence" true by construction — nothing else in the store can reach
    /// the lowering, and both shells call this only from the empty page.
    ///
    /// The read and the write share one held connection lock, so a page cursor
    /// landing between them cannot make the repair decide against a frontier
    /// that no longer exists.
    ///
    /// Reporting the lowering is not bookkeeping: relayd's live push gates on
    /// the `after=` the client subscribed with, and that gate only ever moves
    /// *up* for the life of the socket (`handle_ws` keeps a local `after` and
    /// runs `after = after.max(id)` on every row it replays or pushes). So a
    /// socket opened against the old frontier can never deliver a row at or
    /// below it, and stays deaf to a rebuilt mailbox until it reconnects. The
    /// shells use this to reopen it (`RelayPushClient.resubscribe`).
    ///
    /// A `true` here is not by itself a rebuilt relay, and the shells' logs say
    /// so: relayd deletes a row when it is acked, so a healthy mailbox whose
    /// newest rows were consumed by this device routinely reports a top below
    /// the frontier. See [`crate::relay_frontier_after_completed_sweep`] for why
    /// lowering there is correct and free. The reopen is unconditional on the
    /// lowering because the client cannot tell the two apart, and a threshold
    /// that guessed would suppress the reopen in exactly the rebuild whose
    /// frontier was low to begin with; the cost is one socket reopen per
    /// completed sweep of the one mailbox the socket watches, at most once per
    /// `RELAY_SWEEP_INTERVAL_MS`.
    pub fn note_relay_sweep_completed(
        &self,
        config_key: String,
        now_ms: i64,
        swept_through_id: i64,
    ) -> Result<bool, CoreError> {
        if config_key.is_empty() {
            return Ok(false);
        }
        let conn = lock_conn(&self.conn);
        let persisted: i64 = conn
            .query_row(
                "SELECT after_id FROM relay_fetch_cursors WHERE config_key = ?1",
                params![config_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?
            .unwrap_or(0);
        let repaired = crate::relay_frontier_after_completed_sweep(persisted, swept_through_id);
        conn.execute(
            "INSERT INTO relay_fetch_cursors
                 (config_key, after_id, last_sweep_at, sweep_after_id, sweep_started_at)
             VALUES (?1, ?3, ?2, 0, 0)
             ON CONFLICT(config_key)
             DO UPDATE SET last_sweep_at = ?2, after_id = ?3, sweep_after_id = 0,
                           sweep_started_at = 0",
            params![config_key, now_ms, repaired],
        )
        .map_err(store_err)?;
        crate::protocol_event::note_for(&conn, "mailbox", config_key.as_bytes(), |mailbox| {
            let mut events = vec![crate::protocol_event::ProtocolEventDraft::new(
                crate::protocol_event::ProtocolEventCode::SweepCompleted,
                now_ms,
                "empty_page_reached",
            )
            .actor(mailbox.clone())
            .invariants(&["CURSOR-01", "PROGRESS-01"])
            .count("swept_through_id", swept_through_id.max(0))
            .count("frontier", repaired.max(0))];
            if repaired < persisted {
                // The one place the frontier moves down, so the one record that
                // has to say so plainly: a reader who sees a frontier fall
                // without this beside it is looking at a bug rather than a
                // repair.
                events.push(
                    crate::protocol_event::ProtocolEventDraft::new(
                        crate::protocol_event::ProtocolEventCode::FrontierLowered,
                        now_ms,
                        "completed_sweep_found_a_lower_top",
                    )
                    .actor(mailbox)
                    .invariants(&["CURSOR-01"])
                    .count("frontier_before", persisted.max(0))
                    .count("frontier_after", repaired.max(0)),
                );
            }
            events
        });
        Ok(repaired < persisted)
    }

    // -----------------------------------------------------------------
    // Protocol-event ring (see `crate::protocol_event`)
    // -----------------------------------------------------------------

    /// The whole ring as a JSONL archive, ready to drop into the diagnostics
    /// zip the Advanced screen already shares.
    ///
    /// Nothing uploads it. Nothing schedules it. It is produced when a person
    /// taps share and not otherwise, which is why it can afford to be a full
    /// serialization rather than a sampled one.
    pub fn export_protocol_events_jsonl(&self) -> Result<String, CoreError> {
        let conn = lock_conn(&self.conn);
        crate::protocol_event::export_jsonl(&conn)
    }

    /// Whether the ring holds anything, for gating the share and delete
    /// buttons. Stops at the first row rather than serializing the archive to
    /// count it -- the same reason `has_delivery_metrics` exists.
    pub fn has_protocol_events(&self) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let (records, _) = crate::protocol_event::ring_size(&conn)?;
        Ok(records > 0)
    }

    /// Erase the ring. Part of "delete captured diagnostics": an archive the
    /// person believes they deleted must not be reconstructible from the
    /// store it came out of.
    pub fn clear_protocol_events(&self) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        crate::protocol_event::clear(&conn)
    }

    /// Record that a family rate limit ended a pass's remaining network work.
    ///
    /// The 429 decision itself still lives in the shells today; this is the
    /// one call each of them needs to make the abort visible in an archive,
    /// and it is deliberately typed rather than a free-form journal entry --
    /// counts and a fixed outcome, so no caller can put a token or a URL in
    /// it. When the relay policy hoist gives core the decision, the emit moves
    /// inside and this method goes with the shell code that called it.
    pub fn note_relay_rate_limit_abort(
        &self,
        mailbox_key: String,
        retry_after_ms: i64,
        requests_made: i64,
        envelopes_remaining: i64,
        now_ms: i64,
    ) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        let mailbox =
            crate::protocol_event::actor_pseudonym(&conn, "mailbox", mailbox_key.as_bytes())?;
        crate::protocol_event::append(
            &conn,
            &[crate::protocol_event::ProtocolEventDraft::new(
                crate::protocol_event::ProtocolEventCode::RateLimitAbort,
                now_ms,
                "remaining_network_stages_skipped",
            )
            .actor(mailbox)
            .invariants(&["RATE-01", "LIVE-01"])
            .count("retry_after_ms", retry_after_ms.max(0))
            .count("requests", requests_made.max(0))
            .count("envelopes_remaining", envelopes_remaining.max(0))],
        )
    }

    /// The generic violation hook: record that a named Contract v1 invariant
    /// did not hold here.
    ///
    /// `invariant_id` must be one the contract knows, and `outcome` must be a
    /// short stable token; anything else is refused rather than written,
    /// because a violation record that carried prose would be the easiest
    /// place in the whole system to leak a message body.
    pub fn note_invariant_violation(
        &self,
        invariant_id: String,
        outcome: String,
        now_ms: i64,
    ) -> Result<(), CoreError> {
        if !crate::protocol_event::is_known_invariant(&invariant_id) {
            return Err(CoreError::Malformed(format!(
                "{invariant_id} is not a Contract v1 invariant"
            )));
        }
        if !crate::protocol_event::is_stable_token(&outcome) {
            return Err(CoreError::Malformed(
                "an invariant-violation outcome must be a short stable token".into(),
            ));
        }
        let conn = lock_conn(&self.conn);
        // The id is resolved back to the contract's own `'static` entry rather
        // than kept as the caller's string, so the stored id is by
        // construction one the contract knows.
        let id: &'static str = crate::PROTOCOL_INVARIANT_IDS
            .iter()
            .copied()
            .find(|known| *known == invariant_id)
            .expect("checked above");
        let draft = crate::protocol_event::ProtocolEventDraft::with_checked_outcome(
            crate::protocol_event::ProtocolEventCode::InvariantViolation,
            now_ms,
            &outcome,
        )
        .expect("checked above")
        .invariants(&[id]);
        crate::protocol_event::append(&conn, &[draft])
    }

    /// Forget every remembered frontier, so the next pass re-walks each
    /// mailbox from the beginning.
    ///
    /// This is an explicit administrative reset, not part of backup/restore.
    /// Restore preserves the frontier because clearing it immediately walks
    /// an entire shared mailbox and can recreate discarded courier backlog;
    /// scheduled sweeps provide the bounded stale-frontier repair path.
    pub fn clear_relay_fetch_cursors(&self) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute("DELETE FROM relay_fetch_cursors", [])
            .map_err(store_err)?;
        Ok(())
    }

    /// Notice that the set of ids our relay fetch hints derive from has
    /// changed, and invalidate every remembered frontier if it has. Returns
    /// whether this pass did so.
    ///
    /// Call once at the start of a sync pass, before computing hints. See
    /// [`crate::relay_hint_source_digest`] for why the frontier — not the sweep
    /// schedule — is the thing that has to give here: relayd's `next_cursor` is
    /// the id of the last row matching the hints *you sent*, so rows belonging
    /// to a hint gained later are already behind the frontier, and no sweep
    /// interval, however short, changes that. Only re-walking from 0 finds
    /// them.
    ///
    /// Zeroing the frontier is the whole mechanism, and it is deliberately not
    /// "force a sweep": `after_id = 0` is what a pass reads whether or not it
    /// is flagged as sweeping, so the re-walk does not depend on any
    /// per-process sweep bookkeeping the shells keep. Re-walking is cheap and
    /// self-correcting — everything already delivered is deduped on the way
    /// back in by the seen-id gossip filter.
    ///
    /// It resets `sweep_after_id` alongside `after_id`, and for the same
    /// reason. A sweep part-way up the mailbox has covered the rows below its
    /// resume cursor *against the old hint set*; the rows a widened hint set
    /// makes visible are exactly the ones that were invisible on the way past.
    /// Resuming from that cursor would carry the gap forward into the
    /// completed sweep and then close the schedule behind it. Zeroing progress
    /// is safe here precisely because it does not force a sweep — 0 reads as
    /// "no sweep under way" in [`crate::relay_sweep_due`], and the frontier
    /// reset above is what actually re-walks the mailbox.
    ///
    /// It resets those two columns and nothing else. Deleting the rows outright would
    /// take `last_sweep_at` with it, and that timestamp is the *only* record of
    /// when each mailbox was last walked end to end. Losing it has two bad
    /// consequences and no good one: every mailbox reads as never-swept and so
    /// spends a full flagged sweep on the next cold start, on top of the
    /// re-walk this invalidation already schedules; and, worse, within the
    /// running process [`crate::relay_sweep_due`] answers `!swept_this_session`
    /// for a zeroed timestamp, so a process that had already swept would see
    /// "not due" from then until it restarted — a membership change would
    /// quietly switch the six-hour sweep off for the lifetime of the service.
    /// Keeping the timestamp keeps the cadence honest: the re-walk happens now,
    /// and the next scheduled sweep still lands when it was always going to.
    ///
    /// The re-walk is deliberately not credited as a sweep either. It is not
    /// flagged `sweeping`, so nothing writes `last_sweep_at`, and a walk that
    /// dies half way therefore cannot leave behind a timestamp claiming the
    /// mailbox was covered.
    ///
    /// A mailbox with no row yet is untouched by the `UPDATE` and keeps
    /// reading as `{ after_id: 0, last_sweep_at: 0 }` — already the "walk from
    /// the beginning, sweep on the first pass" state, which is exactly right
    /// for one this device has never fetched from.
    ///
    /// The digest write and the frontier reset share one transaction. Written
    /// separately, a failure or a kill between them would leave the digest
    /// reading as current with the frontiers never reset, and the invalidation
    /// would be lost for good — the newly-visible mail would stay hidden until
    /// the next scheduled sweep or the next membership change.
    ///
    /// The first call on a database with no row stores the digest and reports
    /// `false`. An install has nothing behind a frontier to miss, and reporting
    /// `true` would spend a re-walk of every mailbox on the one case that
    /// already starts from 0.
    pub fn note_relay_hint_sources(&self, own_user_id: Vec<u8>) -> Result<bool, CoreError> {
        let digest = crate::relay_hint_source_digest(self.relay_hint_source_ids(&own_user_id)?);

        let mut conn = lock_conn(&self.conn);
        let stored: Option<String> = conn
            .query_row(
                "SELECT digest FROM relay_hint_source_state WHERE id = 0",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        if stored.as_deref() == Some(digest.as_str()) {
            return Ok(false);
        }
        let invalidates = stored.is_some();
        let tx = conn.transaction().map_err(store_err)?;
        if invalidates {
            tx.execute(
                "UPDATE relay_fetch_cursors
                 SET after_id = 0, sweep_after_id = 0, sweep_started_at = 0",
                [],
            )
            .map_err(store_err)?;
        }
        tx.execute(
            "INSERT INTO relay_hint_source_state (id, digest) VALUES (0, ?1)
             ON CONFLICT(id) DO UPDATE SET digest = ?1",
            params![digest],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(invalidates)
    }

    /// Set (or clear) the local nickname for a contact (T16). A `None` or
    /// blank/whitespace value clears it, falling display back to the card
    /// `name`. Returns whether a row was updated (false = unknown contact).
    /// This never touches any wire-visible field; the nickname stays local.
    pub fn set_contact_nickname(
        &self,
        user_id: Vec<u8>,
        nickname: Option<String>,
    ) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let nickname = nickname
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let changed = conn
            .execute(
                "UPDATE contacts SET nickname = ?2 WHERE user_id = ?1",
                params![user_id, nickname],
            )
            .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// The canonical JPEG avatar bytes for a contact, if one has been synced.
    pub fn contact_avatar(&self, user_id: Vec<u8>) -> Result<Option<Vec<u8>>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT avatar FROM contacts WHERE user_id = ?1",
            params![user_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_err)
        .map(|row| row.flatten())
    }

    /// The newest avatar/display-name profile-sync epoch applied for a contact.
    pub fn contact_avatar_epoch(&self, user_id: Vec<u8>) -> Result<i64, CoreError> {
        let conn = lock_conn(&self.conn);
        let epoch: Option<i64> = conn
            .query_row(
                "SELECT avatar_epoch FROM contacts WHERE user_id = ?1",
                params![user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(epoch.unwrap_or(0))
    }

    /// Delete a contact and, with it, the entire 1:1 chat: the contact row,
    /// every message whose `chat_id` is their UserID (DESIGN.md §7.1: a 1:1
    /// chat's id *is* the peer's UserID), that chat's incoming/outgoing
    /// receipt rows, and every queued retry artifact keyed to this chat
    /// (`outgoing_receipt_envelopes`, `outbound_envelopes`).
    ///
    /// Messages are deleted rather than retained, deliberately: the driving
    /// use case is pruning a dead contact whose identity changed (e.g. a
    /// reinstall regenerated their keys), where the old chat can never
    /// receive again -- and this app's privacy posture (DESIGN.md §6.4
    /// hides even receipt metadata from the wire) argues against quietly
    /// hoarding plaintext history for a peer the user chose to remove.
    /// Group messages from this sender are untouched (they live under the
    /// group's chat_id and belong to the group, not the contact).
    ///
    /// The two queued-envelope tables matter as much as `messages` here: a
    /// deleted chat that left `outbound_envelopes` behind re-arms the
    /// reset-stream trap: stale queued envelopes can resend frames from the
    /// deleted history to a peer whose lamport stream has since moved on.
    /// The peer now preserves its visible branch on such a conflict because
    /// the protocol has no authenticated stream generation; preventing the
    /// conflict here is still necessary so newly authored messages are not
    /// ignored as ambiguous. And a leftover `receipts` row is exactly the
    /// overstated ratchet that painted false read-ticks before that fix: a
    /// delete must yield a genuinely blank slate, not a chat that looks
    /// empty locally but still remembers watermarks against history the
    /// user asked to erase.
    ///
    /// **One thing must survive: `authored_lamport_watermarks`.** This used to
    /// say that nothing did, and that was wrong in a way that cost a peer
    /// their history. Deletion is one-sided by design -- we drop our copy, the
    /// peer keeps theirs -- but the lamport counter was derived purely from
    /// rows this function deletes, so it restarted at 1 while the peer still
    /// held our stream up into the hundreds. Re-authoring over lamports they
    /// already have does not look like a restart to them; it looks like an
    /// ambiguous fork. Older builds could respond by deleting their copy of
    /// the conversation from that lamport up, while current builds preserve
    /// their history but cannot accept our genuinely new colliding message.
    /// Keeping the watermark means our numbering continues where it left off
    /// and no fork is ever detected. It stores no content -- only how far the
    /// counter got -- but note it is keyed by the peer's UserID, so a bare
    /// counter does outlive a delete; that is the deliberate cost of not
    /// erasing someone else's chat from their phone.
    ///
    /// Atomic (single transaction) and idempotent: deleting an unknown
    /// contact is a no-op. Returns `true` if a contact row was removed.
    pub fn delete_contact(&self, user_id: Vec<u8>) -> Result<bool, CoreError> {
        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        let removed = tx
            .execute("DELETE FROM contacts WHERE user_id = ?1", params![user_id])
            .map_err(store_err)?;
        tx.execute("DELETE FROM messages WHERE chat_id = ?1", params![user_id])
            .map_err(store_err)?;
        tx.execute(
            "DELETE FROM message_conflicts WHERE chat_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM consumed_hidden_lamports WHERE chat_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute("DELETE FROM receipts WHERE chat_id = ?1", params![user_id])
            .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outgoing_receipts WHERE chat_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outgoing_receipt_envelopes WHERE chat_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outbound_envelopes WHERE chat_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM contact_discovery_policy WHERE user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM contact_provenance WHERE user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM friend_suggestions
             WHERE candidate_user_id = ?1 OR introducer_user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM friend_suggestion_state WHERE candidate_user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM friend_directory_state WHERE introducer_user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM identity_clone_warnings WHERE user_id = ?1",
            params![user_id],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(removed > 0)
    }

    /// Look up a single contact by UserID, or `None` if not a contact.
    pub fn get_contact(&self, user_id: Vec<u8>) -> Result<Option<Contact>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT user_id, name, sign_pk, agree_pk, relay_url, relay_token, nickname FROM contacts WHERE user_id = ?1",
            params![user_id],
            row_to_contact,
        )
        .optional()
        .map_err(store_err)
    }

    /// All contacts, alphabetical by name.
    pub fn list_contacts(&self) -> Result<Vec<Contact>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare("SELECT user_id, name, sign_pk, agree_pk, relay_url, relay_token, nickname FROM contacts ORDER BY name ASC")
            .map_err(store_err)?;
        let rows = stmt.query_map([], row_to_contact).map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Apply an authenticated contact's discovery policy if it is newer.
    pub fn upsert_contact_discovery_policy(
        &self,
        policy: ContactDiscoveryPolicy,
    ) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let changed = conn
            .execute(
                "INSERT INTO contact_discovery_policy
                    (user_id, protocol_version, enabled, revision)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(user_id) DO UPDATE SET
                    protocol_version = excluded.protocol_version,
                    enabled = excluded.enabled,
                    revision = excluded.revision
                 WHERE excluded.revision > contact_discovery_policy.revision",
                params![
                    policy.user_id,
                    policy.protocol_version as i64,
                    i64::from(policy.enabled),
                    policy.revision as i64,
                ],
            )
            .map_err(store_err)?
            > 0;
        Ok(changed)
    }

    pub fn get_contact_discovery_policy(
        &self,
        user_id: Vec<u8>,
    ) -> Result<Option<ContactDiscoveryPolicy>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT user_id, protocol_version, enabled, revision
             FROM contact_discovery_policy WHERE user_id = ?1",
            params![user_id],
            |row| {
                Ok(ContactDiscoveryPolicy {
                    user_id: row.get(0)?,
                    protocol_version: row.get::<_, i64>(1)? as u8,
                    enabled: row.get::<_, i64>(2)? != 0,
                    revision: row.get::<_, i64>(3)? as u64,
                })
            },
        )
        .optional()
        .map_err(store_err)
    }

    /// Atomically replace all suggestions supplied by one introducer. The
    /// directory's tickets are checked here so both mobile shells share the
    /// same fail-closed behavior.
    pub fn apply_friend_directory(
        &self,
        introducer_user_id: Vec<u8>,
        recipient_user_id: Vec<u8>,
        content: FriendDirectoryContent,
        now_ms: i64,
    ) -> Result<bool, CoreError> {
        if content.version != 1 || content.entries.len() > 64 {
            return Err(CoreError::Malformed("invalid friend directory".to_string()));
        }
        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        let introducer_sign_pk: Option<Vec<u8>> = tx
            .query_row(
                "SELECT sign_pk FROM contacts WHERE user_id = ?1",
                params![introducer_user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        let Some(introducer_sign_pk) = introducer_sign_pk else {
            return Err(CoreError::Malformed(
                "friend directory sender is not a contact".to_string(),
            ));
        };
        let applied: Option<i64> = tx
            .query_row(
                "SELECT applied_revision FROM friend_directory_state
                 WHERE introducer_user_id = ?1",
                params![introducer_user_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        if applied.is_some_and(|revision| revision as u64 >= content.revision) {
            tx.commit().map_err(store_err)?;
            return Ok(false);
        }

        for entry in &content.entries {
            if entry.ticket.introducer_user_id != introducer_user_id
                || !verify_introduction_ticket(
                    entry.ticket.clone(),
                    introducer_sign_pk.clone(),
                    entry.candidate.user_id.clone(),
                    recipient_user_id.clone(),
                    entry.candidate_policy_revision,
                    now_ms,
                )?
            {
                return Err(CoreError::Malformed(
                    "friend directory contains an invalid introduction ticket".to_string(),
                ));
            }

            // A requested suggestion becomes retryable once there is no
            // longer an unexpired introduced-request envelope queued for it.
            // Hidden suggestions remain suppressed across snapshots.
            let request_still_pending: bool = tx
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM outbound_envelopes
                         WHERE recipient_user_id = ?1 AND kind = ?2 AND expiry > ?3
                     )",
                    params![
                        &entry.candidate.user_id,
                        KIND_INTRODUCED_FRIEND_REQUEST as i64,
                        now_ms,
                    ],
                    |row| row.get(0),
                )
                .map_err(store_err)?;
            if !request_still_pending {
                tx.execute(
                    "DELETE FROM friend_suggestion_state
                     WHERE candidate_user_id = ?1 AND state = 1",
                    params![&entry.candidate.user_id],
                )
                .map_err(store_err)?;
            }
        }

        tx.execute(
            "INSERT INTO friend_directory_state (introducer_user_id, applied_revision)
             VALUES (?1, ?2)
             ON CONFLICT(introducer_user_id) DO UPDATE SET
                applied_revision = excluded.applied_revision",
            params![introducer_user_id, content.revision as i64],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM friend_suggestions WHERE introducer_user_id = ?1",
            params![introducer_user_id],
        )
        .map_err(store_err)?;
        for entry in content.entries {
            let ticket =
                serde_json::to_vec(&entry.ticket).map_err(|e| CoreError::Store(e.to_string()))?;
            tx.execute(
                "INSERT INTO friend_suggestions
                    (candidate_user_id, introducer_user_id, name, sign_pk, agree_pk,
                     candidate_policy_revision, ticket, expires_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    entry.candidate.user_id,
                    introducer_user_id,
                    entry.candidate.name,
                    entry.candidate.sign_pk,
                    entry.candidate.agree_pk,
                    entry.candidate_policy_revision as i64,
                    ticket,
                    entry.ticket.expires_at_ms,
                ],
            )
            .map_err(store_err)?;
        }
        tx.commit().map_err(store_err)?;
        Ok(true)
    }

    pub fn list_friend_suggestions(&self, now_ms: i64) -> Result<Vec<FriendSuggestion>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT s.candidate_user_id, s.name, s.sign_pk, s.agree_pk,
                        s.introducer_user_id, s.ticket,
                        COALESCE(x.state, 0)
                 FROM friend_suggestions s
                 LEFT JOIN friend_suggestion_state x
                    ON x.candidate_user_id = s.candidate_user_id
                 LEFT JOIN contacts c ON c.user_id = s.candidate_user_id
                 LEFT JOIN blocked_identities b ON b.user_id = s.candidate_user_id
                 WHERE c.user_id IS NULL AND b.user_id IS NULL
                       AND s.expires_at_ms >= ?1
                       AND COALESCE(x.state, 0) != 2
                 ORDER BY lower(s.name), s.introducer_user_id",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![now_ms], |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, i64>(6)? as u8,
                ))
            })
            .map_err(store_err)?;
        let mut suggestions = Vec::new();
        for row in rows {
            let (user_id, name, sign_pk, agree_pk, introducer_user_id, ticket_json, state) =
                row.map_err(store_err)?;
            let ticket: IntroductionTicket = serde_json::from_slice(&ticket_json)
                .map_err(|e| CoreError::Store(e.to_string()))?;
            suggestions.push(FriendSuggestion {
                candidate: SuggestedFriendCard {
                    name,
                    user_id,
                    sign_pk,
                    agree_pk,
                },
                introducer_user_id,
                ticket,
                state,
            });
        }
        Ok(suggestions)
    }

    /// Block an identity (specs/friends-of-friends.md "dismissal-block
    /// tombstone"): inbound envelopes from it are dropped by both shells, it
    /// never appears as a friend suggestion, and a replayed friend request
    /// cannot re-create the contact. Silent — the blocked party is never
    /// notified. The contact row and chat history are kept; only a deliberate
    /// re-import of their card ([MessageStore::upsert_imported_contact])
    /// clears the block.
    pub fn block_user(&self, user_id: Vec<u8>, now_ms: i64) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "INSERT INTO blocked_identities (user_id, blocked_at_ms) VALUES (?1, ?2)
             ON CONFLICT(user_id) DO NOTHING",
            params![user_id, now_ms],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn unblock_user(&self, user_id: Vec<u8>) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let deleted = conn
            .execute(
                "DELETE FROM blocked_identities WHERE user_id = ?1",
                params![user_id],
            )
            .map_err(store_err)?;
        Ok(deleted > 0)
    }

    pub fn is_user_blocked(&self, user_id: Vec<u8>) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM blocked_identities WHERE user_id = ?1)",
            params![user_id],
            |row| row.get(0),
        )
        .map_err(store_err)
    }

    pub fn list_blocked_users(&self) -> Result<Vec<Vec<u8>>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare("SELECT user_id FROM blocked_identities ORDER BY blocked_at_ms")
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(store_err)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row.map_err(store_err)?);
        }
        Ok(out)
    }

    /// State values: 0 available, 1 requested, 2 hidden.
    pub fn set_friend_suggestion_state(
        &self,
        candidate_user_id: Vec<u8>,
        state: u8,
    ) -> Result<(), CoreError> {
        if state > 2 {
            return Err(CoreError::Malformed("invalid suggestion state".to_string()));
        }
        let conn = lock_conn(&self.conn);
        conn.execute(
            "INSERT INTO friend_suggestion_state (candidate_user_id, state)
             VALUES (?1, ?2)
             ON CONFLICT(candidate_user_id) DO UPDATE SET state = excluded.state",
            params![candidate_user_id, state as i64],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn remove_friend_suggestion(&self, candidate_user_id: Vec<u8>) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "DELETE FROM friend_suggestions WHERE candidate_user_id = ?1",
            params![candidate_user_id],
        )
        .map_err(store_err)?;
        conn.execute(
            "DELETE FROM friend_suggestion_state WHERE candidate_user_id = ?1",
            params![candidate_user_id],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn clear_friend_suggestions(&self) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute_batch(
            "DELETE FROM friend_suggestions;
             DELETE FROM friend_directory_state;",
        )
        .map_err(store_err)
    }

    pub fn upsert_contact_provenance(
        &self,
        provenance: ContactProvenance,
    ) -> Result<(), CoreError> {
        if provenance.source > 2 {
            return Err(CoreError::Malformed(
                "invalid contact provenance".to_string(),
            ));
        }
        let conn = lock_conn(&self.conn);
        conn.execute(
            "INSERT INTO contact_provenance
                (user_id, source, introducer_user_id, introduced_at_ms, added_nearby)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(user_id) DO UPDATE SET
                source = CASE WHEN contact_provenance.source = 0 THEN 0 ELSE excluded.source END,
                introducer_user_id = CASE WHEN contact_provenance.source = 0
                    THEN contact_provenance.introducer_user_id ELSE excluded.introducer_user_id END,
                introduced_at_ms = CASE WHEN contact_provenance.source = 0
                    THEN contact_provenance.introduced_at_ms ELSE excluded.introduced_at_ms END,
                -- Meeting in person is a fact about an encounter that happened;
                -- a later remote re-add does not unmake it, so this one is
                -- sticky rather than last-write-wins.
                added_nearby = CASE WHEN contact_provenance.added_nearby = 1 OR excluded.added_nearby = 1
                    THEN 1 ELSE 0 END",
            params![
                provenance.user_id,
                provenance.source as i64,
                provenance.introducer_user_id,
                provenance.introduced_at_ms,
                provenance.added_nearby as i64,
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn get_contact_provenance(
        &self,
        user_id: Vec<u8>,
    ) -> Result<Option<ContactProvenance>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT user_id, source, introducer_user_id, introduced_at_ms, added_nearby
             FROM contact_provenance WHERE user_id = ?1",
            params![user_id],
            |row| {
                Ok(ContactProvenance {
                    user_id: row.get(0)?,
                    source: row.get::<_, i64>(1)? as u8,
                    introducer_user_id: row.get(2)?,
                    introduced_at_ms: row.get(3)?,
                    added_nearby: row.get::<_, i64>(4)? != 0,
                })
            },
        )
        .optional()
        .map_err(store_err)
    }

    /// Record or refresh an inbound shared-card request. A duplicate delivery
    /// updates the row rather than stacking prompts: `first_seen_ms` and
    /// `last_prompted_ms` are preserved so redelivery neither resets the
    /// prompt-rate clock nor re-raises the sheet.
    pub fn upsert_pending_shared_request(
        &self,
        request: PendingSharedRequest,
    ) -> Result<(), CoreError> {
        conn_execute_pending_shared_upsert(&lock_conn(&self.conn), &request)
    }

    /// All pending shared-card requests, oldest first. Rows past expiry are
    /// swept here rather than by a background job — read is the only moment
    /// staleness matters.
    pub fn list_pending_shared_requests(
        &self,
        now_ms: i64,
    ) -> Result<Vec<PendingSharedRequest>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "DELETE FROM pending_shared_requests WHERE expires_at_ms < ?1",
            params![now_ms],
        )
        .map_err(store_err)?;
        let mut stmt = conn
            .prepare(
                "SELECT requester_user_id, name, sign_pk, agree_pk, relay_url, relay_token,
                        sharer_user_id, expires_at_ms, first_seen_ms, last_prompted_ms
                 FROM pending_shared_requests ORDER BY first_seen_ms ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], row_to_pending_shared_request)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    pub fn get_pending_shared_request(
        &self,
        requester_user_id: Vec<u8>,
    ) -> Result<Option<PendingSharedRequest>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT requester_user_id, name, sign_pk, agree_pk, relay_url, relay_token,
                    sharer_user_id, expires_at_ms, first_seen_ms, last_prompted_ms
             FROM pending_shared_requests WHERE requester_user_id = ?1",
            params![requester_user_id],
            row_to_pending_shared_request,
        )
        .optional()
        .map_err(store_err)
    }

    pub fn delete_pending_shared_request(
        &self,
        requester_user_id: Vec<u8>,
    ) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "DELETE FROM pending_shared_requests WHERE requester_user_id = ?1",
            params![requester_user_id],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Should this requester's pending request raise a prompt right now, and
    /// if so, stamp it as prompted. One atomic decision so at most one prompt
    /// per requester per day survives concurrent deliveries: `false` for a
    /// suppressed requester, a missing row, or a prompt within the last day.
    pub fn note_shared_request_prompt(
        &self,
        requester_user_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let suppressed: bool = conn
            .query_row(
                "SELECT suppressed FROM shared_request_dismissals WHERE requester_user_id = ?1",
                params![requester_user_id],
                |row| row.get::<_, i64>(0).map(|v| v != 0),
            )
            .optional()
            .map_err(store_err)?
            .unwrap_or(false);
        if suppressed {
            return Ok(false);
        }
        let changed = conn
            .execute(
                "UPDATE pending_shared_requests SET last_prompted_ms = ?2
                 WHERE requester_user_id = ?1 AND last_prompted_ms <= ?2 - ?3",
                params![requester_user_id, now_ms, MS_PER_DAY],
            )
            .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// Record a **Not now** and return the new dismissal count, so the shell
    /// knows when to start offering "Don't ask again" (from the second one).
    pub fn record_shared_request_dismissal(
        &self,
        requester_user_id: Vec<u8>,
    ) -> Result<u32, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "INSERT INTO shared_request_dismissals (requester_user_id, count, suppressed)
             VALUES (?1, 1, 0)
             ON CONFLICT(requester_user_id) DO UPDATE SET
                count = shared_request_dismissals.count + 1",
            params![requester_user_id],
        )
        .map_err(store_err)?;
        conn.query_row(
            "SELECT count FROM shared_request_dismissals WHERE requester_user_id = ?1",
            params![requester_user_id],
            |row| row.get::<_, i64>(0).map(|v| v as u32),
        )
        .map_err(store_err)
    }

    /// "Don't ask again": a quiet local tombstone, no notification to anyone.
    pub fn suppress_shared_requests(&self, requester_user_id: Vec<u8>) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "INSERT INTO shared_request_dismissals (requester_user_id, count, suppressed)
             VALUES (?1, 0, 1)
             ON CONFLICT(requester_user_id) DO UPDATE SET suppressed = 1",
            params![requester_user_id],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn get_shared_request_dismissal(
        &self,
        requester_user_id: Vec<u8>,
    ) -> Result<Option<SharedRequestDismissal>, CoreError> {
        let conn = lock_conn(&self.conn);
        conn.query_row(
            "SELECT requester_user_id, count, suppressed
             FROM shared_request_dismissals WHERE requester_user_id = ?1",
            params![requester_user_id],
            |row| {
                Ok(SharedRequestDismissal {
                    requester_user_id: row.get(0)?,
                    count: row.get::<_, i64>(1)? as u32,
                    suppressed: row.get::<_, i64>(2)? != 0,
                })
            },
        )
        .optional()
        .map_err(store_err)
    }

    /// Directly scanning the person's own QR code is the escape hatch that
    /// clears both a suppression and any dismissal history.
    pub fn clear_shared_request_dismissal(
        &self,
        requester_user_id: Vec<u8>,
    ) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "DELETE FROM shared_request_dismissals WHERE requester_user_id = ?1",
            params![requester_user_id],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Record (or refresh, on a re-send) the requester-side "waiting" state
    /// for one shared-card connection.
    pub fn upsert_outgoing_shared_request(
        &self,
        request: OutgoingSharedRequest,
    ) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "INSERT INTO outgoing_shared_requests (candidate_user_id, expires_at_ms, sent_at_ms)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(candidate_user_id) DO UPDATE SET
                expires_at_ms = excluded.expires_at_ms,
                sent_at_ms = excluded.sent_at_ms",
            params![
                request.candidate_user_id,
                request.expires_at_ms,
                request.sent_at_ms,
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// All outgoing shared-card requests, including expired ones — expiry is
    /// exactly the state the UI must surface as "didn't respond", so the rows
    /// outlive it until the connection completes or the user clears them.
    pub fn list_outgoing_shared_requests(&self) -> Result<Vec<OutgoingSharedRequest>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT candidate_user_id, expires_at_ms, sent_at_ms
                 FROM outgoing_shared_requests ORDER BY sent_at_ms ASC",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok(OutgoingSharedRequest {
                    candidate_user_id: row.get(0)?,
                    expires_at_ms: row.get(1)?,
                    sent_at_ms: row.get(2)?,
                })
            })
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    pub fn delete_outgoing_shared_request(
        &self,
        candidate_user_id: Vec<u8>,
    ) -> Result<(), CoreError> {
        let conn = lock_conn(&self.conn);
        conn.execute(
            "DELETE FROM outgoing_shared_requests WHERE candidate_user_id = ?1",
            params![candidate_user_id],
        )
        .map_err(store_err)?;
        Ok(())
    }

    /// Add or replace a group definition and its full membership. Updating an
    /// existing group id replaces the stored key/member list atomically,
    /// which is the v1 rotation path for membership changes.
    pub fn upsert_group(&self, group: Group) -> Result<(), CoreError> {
        validate_group(&group)?;
        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        upsert_group_tx(&tx, &group)?;
        tx.commit().map_err(store_err)?;
        Ok(())
    }

    /// Look up one imported group by id, including its current member list.
    pub fn get_group(&self, group_id: Vec<u8>) -> Result<Option<Group>, CoreError> {
        let conn = lock_conn(&self.conn);
        let row: Option<GroupRow> = conn
            .query_row(
                "SELECT group_id, name, group_key, metadata_revision, metadata_changed_by
                 FROM groups WHERE group_id = ?1",
                params![&group_id],
                row_to_group_row,
            )
            .optional()
            .map_err(store_err)?;
        row.map(|row| hydrate_group(&conn, row)).transpose()
    }

    /// All imported groups, alphabetical by name then id for stable ordering.
    pub fn list_groups(&self) -> Result<Vec<Group>, CoreError> {
        let conn = lock_conn(&self.conn);
        let raw = {
            let mut stmt = conn
                .prepare(
                    "SELECT group_id, name, group_key, metadata_revision, metadata_changed_by
                     FROM groups ORDER BY name ASC, group_id ASC",
                )
                .map_err(store_err)?;
            let rows = stmt.query_map([], row_to_group_row).map_err(store_err)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(store_err)?
        };
        raw.into_iter()
            .map(|row| hydrate_group(&conn, row))
            .collect()
    }

    /// Delete a group definition, its membership rows, and every row of
    /// local chat history / retry state keyed by that group id: `messages`,
    /// `receipts`, `outgoing_receipts`, `outgoing_receipt_envelopes`, and
    /// `outbound_envelopes` -- the same "genuinely blank slate" purge as
    /// [`MessageStore::delete_contact`] (see that method's doc comment for
    /// why leftover queued envelopes or receipt watermarks are a bug, not
    /// just clutter: they re-arm the reset-stream trap fixed in fc6b9f9 and
    /// paint false read-ticks). Atomic and idempotent.
    pub fn delete_group(&self, group_id: Vec<u8>) -> Result<bool, CoreError> {
        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        tx.execute(
            "DELETE FROM group_members WHERE group_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        let removed = tx
            .execute("DELETE FROM groups WHERE group_id = ?1", params![&group_id])
            .map_err(store_err)?;
        tx.execute(
            "DELETE FROM messages WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM message_conflicts WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM consumed_hidden_lamports WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM receipts WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outgoing_receipts WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outgoing_receipt_envelopes WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM group_receipts WHERE group_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.execute(
            "DELETE FROM outbound_envelopes WHERE chat_id = ?1",
            params![&group_id],
        )
        .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(removed > 0)
    }

    // --- carry queue (DESIGN.md §5.3) --------------------------------------

    /// Store a foreign envelope for later store-and-forward delivery
    /// (DESIGN.md §5.3 carry queue). Keyed on `msg_id`, so re-enqueuing an
    /// envelope we're already carrying is a no-op (returns `false`); a fresh
    /// insert returns `true`. A digest over `recipient_hint || sealed` also
    /// collapses a ciphertext rewrapped under a new attacker-selected public
    /// `msg_id`, while preserving group fan-out copies with different hints.
    ///
    /// `is_family` marks whether this envelope is addressed to someone this
    /// node knows (its `recipient_hint` matched a contact -- the caller
    /// decides, since it holds the contacts and the hint derivation). Family
    /// envelopes are never pressure-evicted. Foreign rows additionally share
    /// `foreign_budget_bytes`, and the entire queue has a 64 MiB sealed-byte
    /// admission ceiling so a forged family hint cannot grow it indefinitely.
    /// Pressure may evict older foreign rows; if the new row still cannot fit,
    /// admission fails without marking it seen or acking it, so a later copy
    /// can retry. Every pressure loss/rejection attempts a redacted protocol
    /// event; diagnostics failure never fails admission work. All of this
    /// happens in one transaction.
    pub fn enqueue_carried_envelope(
        &self,
        envelope: CarriedEnvelope,
        is_family: bool,
        received_at_ms: i64,
        foreign_budget_bytes: i64,
    ) -> Result<bool, CoreError> {
        enqueue_carried_envelope_with_budgets(
            self,
            envelope,
            is_family,
            received_at_ms,
            foreign_budget_bytes,
            DEFAULT_TOTAL_CARRY_BUDGET_BYTES,
        )
    }

    /// Store an envelope pulled FROM the relay that we're proxying for its
    /// real recipient (relay proxy-polling: an internet phone fetches a
    /// contact's `recipient_hint`s alongside its own so a 1:1 message can
    /// bridge across BLE clusters -- see `MeshService.pollRelayMailbox` /
    /// `relayProxyHints` on the Kotlin side). This is the relay-sourced
    /// twin of [`MessageStore::enqueue_carried_envelope`]: always
    /// `is_family = 1` (the relay hint match already proved it's addressed
    /// to someone we know, so it gets family-first eviction priority) and
    /// `from_relay = 1`, which excludes it from
    /// [`MessageStore::family_carried_envelopes`] -- the relay-upload query
    /// -- because it is *already on the relay*; re-uploading it would just
    /// churn traffic and could resurrect a copy the real recipient already
    /// acked. It still shows up in [`MessageStore::carried_envelopes_for_hints`]
    /// / [`MessageStore::carried_envelopes_for_peer_sync`] so we can hand it
    /// to the real recipient over BLE. `INSERT OR IGNORE` keyed on `msg_id`,
    /// so re-fetching the same still-unacked proxy envelope on a later poll
    /// pass is a no-op. Returns whether a new row was admitted. A capacity
    /// rejection returns an error so a carry-only inbound path leaves the
    /// relay row unacked and can present it again after space becomes
    /// available.
    pub fn enqueue_relay_carried_envelope(
        &self,
        envelope: CarriedEnvelope,
        now_ms: i64,
    ) -> Result<bool, CoreError> {
        enqueue_relay_carried_envelope_with_budget(
            self,
            envelope,
            now_ms,
            DEFAULT_TOTAL_CARRY_BUDGET_BYTES,
        )
    }

    /// Carried envelopes whose `recipient_hint` matches any of `hints` and
    /// that haven't expired as of `now_ms`, oldest first (DESIGN.md §5.3).
    /// The caller passes the set of hints a just-met peer could match --
    /// `recipient_hint` rotates daily (§6.4), so that's the peer's UserID
    /// hashed against each recent day. A match means "this envelope is for
    /// that peer," so the caller can hand it over and then
    /// [`MessageStore::remove_carried_envelope`] it.
    pub fn carried_envelopes_for_hints(
        &self,
        hints: Vec<Vec<u8>>,
        now_ms: i64,
    ) -> Result<Vec<CarriedEnvelope>, CoreError> {
        // Unlimited page for callers that still want the full matching set
        // (tests, offline tooling). HELLO drain uses the budgeted page API.
        Ok(self
            .carried_envelopes_for_hints_page(hints, now_ms, u64::MAX, u32::MAX, None)?
            .rows)
    }

    /// Budgeted, cursor-resumable page of carried envelopes matching `hints`
    /// (G2: HELLO `drainCarriedEnvelopesTo`). Same DTN rules as the unbudgeted
    /// form: only *offers*; never removes. `budget_bytes == 0` is the off
    /// switch, and so is `max_rows == 0`. Head-of-line oversized exception and
    /// the row ceiling both match [`Self::carried_envelopes_for_peer_sync`];
    /// see [`DEFAULT_CARRIED_PAGE_MAX_ROWS`] for why a byte budget alone is not
    /// enough.
    pub fn carried_envelopes_for_hints_page(
        &self,
        hints: Vec<Vec<u8>>,
        now_ms: i64,
        budget_bytes: u64,
        max_rows: u32,
        after: Option<CoreCarriedCursor>,
    ) -> Result<CoreCarriedSyncPage, CoreError> {
        if hints.is_empty() || budget_bytes == 0 || max_rows == 0 {
            return Ok(CoreCarriedSyncPage {
                rows: Vec::new(),
                next: None,
                exhausted: hints.is_empty(),
            });
        }
        let conn = lock_conn(&self.conn);
        let placeholders = std::iter::repeat_n("?", hints.len())
            .collect::<Vec<_>>()
            .join(",");
        let mut sql = format!(
            "SELECT msg_id, hop_ttl, expiry, recipient_hint, sealed, received_at
             FROM carried_envelopes
             WHERE expiry > ?1 AND recipient_hint IN ({placeholders})"
        );
        let mut bind: Vec<Value> = vec![Value::Integer(now_ms)];
        for hint in &hints {
            bind.push(Value::Blob(hint.clone()));
        }
        if let Some(cursor) = &after {
            sql.push_str(" AND (received_at > ?");
            bind.push(Value::Integer(cursor.received_at));
            let received_at_param = bind.len();
            sql.push_str(&received_at_param.to_string());
            sql.push_str(" OR (received_at = ?");
            sql.push_str(&received_at_param.to_string());
            sql.push_str(" AND msg_id > ?");
            bind.push(Value::Blob(cursor.msg_id.clone()));
            sql.push_str(&bind.len().to_string());
            sql.push_str("))");
        }
        sql.push_str(" ORDER BY received_at ASC, msg_id ASC");
        let mut stmt = conn.prepare(&sql).map_err(store_err)?;
        let rows = stmt
            .query_map(params_from_iter(bind.iter()), |row| {
                Ok((row_to_carried(row)?, row.get::<_, i64>(5)?))
            })
            .map_err(store_err)?;
        let mut selected: Vec<CarriedEnvelope> = Vec::new();
        let mut next: Option<CoreCarriedCursor> = None;
        let mut used = 0_u64;
        let mut exhausted = true;
        for row in rows {
            let (envelope, received_at) = row.map_err(store_err)?;
            if selected.len() as u64 >= u64::from(max_rows) {
                // The row ceiling, checked before the byte budget: a page of
                // tiny envelopes never comes near the budget, and the frame
                // count is what floods the link. `exhausted` stays false, so
                // the lane keeps its cursor and resumes here next round.
                exhausted = false;
                break;
            }
            let size = envelope.sealed.len() as u64;
            if used > 0 && used.saturating_add(size) > budget_bytes {
                exhausted = false;
                break;
            }
            if used == 0 && size > budget_bytes {
                // Head-of-line liveness: offer one oversized row, then stop.
                selected.push(envelope.clone());
                next = Some(CoreCarriedCursor {
                    received_at,
                    msg_id: envelope.msg_id,
                });
                exhausted = false;
                break;
            }
            used = used.saturating_add(size);
            next = Some(CoreCarriedCursor {
                received_at,
                msg_id: envelope.msg_id.clone(),
            });
            selected.push(envelope);
        }
        Ok(CoreCarriedSyncPage {
            rows: selected,
            next,
            exhausted,
        })
    }

    /// Up to `limit` carried-envelope `msg_id`s, oldest first. This is the
    /// exact-set stand-in for §7.3's "recent msg_id bloom filter": enough for
    /// a peer to say "I already carry these" so another mule doesn't blindly
    /// resend them on every reconnect.
    pub fn carried_msg_ids(&self, limit: u64) -> Result<Vec<Vec<u8>>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT msg_id FROM carried_envelopes
                 ORDER BY received_at ASC, msg_id ASC
                 LIMIT ?1",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![limit as i64], |row| row.get::<_, Vec<u8>>(0))
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Up to `limit` recent message-stream `msg_id`s this device holds,
    /// newest first: every `messages` row with a recorded envelope id, which
    /// covers both *consumed* incoming messages (opened-and-stored via
    /// `insert_incoming_message`) and our own *authored* ones
    /// (`insert_outgoing_message` writes the envelope's `msg_id` into the
    /// message row too, and `open` backfills it for older rows). This is the
    /// counterpart to [`MessageStore::carried_msg_ids`] for the D2
    /// mule-drain-confirm fix (DTN_TODOS.md §3.2): a recipient does not carry
    /// a message it has decrypted and stored -- it consumes it, so
    /// `carried_msg_ids` alone never advertises "I got it" for our own
    /// incoming mail. Merging this list into what we advertise in our own
    /// outgoing DIGEST (`recent_msg_id`s, see `protocol.rs`'s DIGEST frame
    /// docs, and `engine.rs::core_digest_advertised_msg_ids`) is what lets a
    /// mule that's still holding our envelope in its carry queue notice, on
    /// its next digest exchange with us, that we already have it and drop
    /// its copy -- without any wire-format change, since the DIGEST frame
    /// already carries an arbitrary `recent_msg_id` list.
    ///
    /// Including our own authored ids alongside the consumed ones is harmless
    /// and actively useful: a mule's Hook-B spray can hand us back an
    /// envelope we ourselves authored, and advertising its `msg_id` here
    /// suppresses that resend at the source -- the same rationale as the
    /// Kotlin side's `seedSeenIdsFromOwnHistory`.
    ///
    /// Newest first (unlike `carried_msg_ids`'s oldest-first) because the
    /// caller bounds the merged advertised list to a fixed count
    /// (`engine.rs::DIGEST_ADVERTISED_MSG_IDS_LIMIT`): the most recently
    /// landed messages are the ones most likely to still be sitting in some
    /// mule's carry queue, so they're the ones worth prioritizing when the
    /// list must be truncated.
    ///
    /// Only rows with a non-`NULL` `msg_id` participate -- legacy rows that
    /// predate envelope-id recording don't have one and are silently skipped
    /// by the `WHERE` clause.
    pub fn recent_consumed_msg_ids(&self, limit: u64) -> Result<Vec<Vec<u8>>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT msg_id FROM messages
                 WHERE msg_id IS NOT NULL
                 ORDER BY id DESC
                 LIMIT ?1",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![limit as i64], |row| row.get::<_, Vec<u8>>(0))
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Carried envelopes suitable to spray to a non-recipient mule on peer
    /// sync: unexpired as of `now_ms`, not already known to the peer
    /// (`peer_known_msg_ids` from its digest), and not actually addressed to
    /// that peer (`peer_hints`, which the targeted-delivery path handles
    /// separately). Ordered oldest first.
    ///
    /// FC2: `peer_known_msg_ids`/`peer_hints` exclusion is pushed into the
    /// SQL `WHERE` clause (as `NOT IN`) rather than fetched-then-filtered in
    /// Rust, so a row the peer already has never has its `sealed` ciphertext
    /// decoded at all -- with D8's periodic re-digest, this query now runs
    /// every few minutes on every long-lived link, and the old
    /// fetch-everything-then-filter shape meant every one of those ticks
    /// paid to materialize up to the full 64 MiB carry budget regardless of
    /// how little of it was actually new to the peer.
    ///
    /// `after` resumes the walk: rows are restricted to those strictly after
    /// that `(received_at, msg_id)` point. Without it a courier whose backlog
    /// exceeds one round's budget re-read from the oldest row on every
    /// re-digest and re-offered the same head forever, so a peer whose digest
    /// cannot advertise the whole store (the advertised-id list is capped)
    /// never saw the young tail at all. The caller (the per-link-session
    /// policy in `transport_policy.rs`) hands back [`CoreCarriedSyncPage::next`]
    /// on the following round, so successive rounds walk the queue instead of
    /// re-treading its head. `None` starts a fresh full pass.
    ///
    /// `budget_bytes` bounds one encounter's worth of foreign-carry offering
    /// by summed sealed-byte size: rows are taken oldest first until the next
    /// one would not fit, and then iteration stops (so the rest never has its
    /// ciphertext decoded either). Frames are all-or-nothing on the wire, so a
    /// row is never truncated to fit. The one exception is the head of the
    /// list: if the oldest eligible envelope is by itself bigger than the whole
    /// budget, it is offered anyway and the round stops there. Skipping it
    /// instead would block the lane forever -- selection is always oldest
    /// first, so that same row would be reconsidered and rejected on every
    /// future round until it expired, and nothing behind it would ever be
    /// offered. One oversized frame per encounter keeps the lane live while
    /// still bounding the round to about a single envelope. Any *later* row
    /// that does not fit the remaining budget still just ends the round, and
    /// a `budget_bytes` of zero still offers nothing at all -- that is the
    /// lane's off switch rather than a small allowance.
    /// Nothing is dropped by this cut: the carry
    /// queue is untouched, and D8's periodic re-digest re-offers whatever did
    /// not fit on the next round, so a big backlog is *paced* across rounds
    /// instead of monopolizing a slow link's single FIFO in one burst.
    ///
    /// `max_rows` is the same cut expressed in envelopes rather than bytes, and
    /// it exists because the byte budget alone does not bound a round's frame
    /// count: hundreds of receipt-sized envelopes clear a 256 KiB budget
    /// untouched while still queueing hundreds of separate writes into one
    /// link's FIFO ahead of live mail. Rows are taken oldest first until either
    /// cut binds, whichever comes first. Zero is an off switch exactly as a
    /// zero byte budget is. See [`DEFAULT_CARRIED_PAGE_MAX_ROWS`].
    pub fn carried_envelopes_for_peer_sync(
        &self,
        peer_hints: Vec<Vec<u8>>,
        peer_known_msg_ids: Vec<Vec<u8>>,
        now_ms: i64,
        budget_bytes: u64,
        max_rows: u32,
        after: Option<CoreCarriedCursor>,
    ) -> Result<CoreCarriedSyncPage, CoreError> {
        if budget_bytes == 0 || max_rows == 0 {
            // The lane's off switch. Returning here rather than letting the
            // loop below break on its first row keeps the query -- and one
            // row's ciphertext decode -- off a link that is parked. Not
            // `exhausted`: nothing was examined, so nothing was ruled out.
            return Ok(CoreCarriedSyncPage {
                rows: Vec::new(),
                next: None,
                exhausted: false,
            });
        }
        let conn = lock_conn(&self.conn);
        let mut sql = String::from(
            "SELECT msg_id, hop_ttl, expiry, recipient_hint, sealed, received_at
             FROM carried_envelopes
             WHERE expiry > ?1",
        );
        let mut bind: Vec<Value> = vec![Value::Integer(now_ms)];
        push_not_in(&mut sql, &mut bind, "msg_id", &peer_known_msg_ids);
        push_not_in(&mut sql, &mut bind, "recipient_hint", &peer_hints);
        // Keyset, not OFFSET: the queue is written to while a walk is in
        // progress, so a row count would skip rows that shifted under it. The
        // predicate is expressed in exactly the ORDER BY's terms, so
        // `idx_carried_received_at` can seek straight to the resume point.
        //
        // Numbered placeholders, because `received_at` is compared twice and
        // must not consume two binds. They are appended after `push_not_in`'s
        // anonymous `?`s, which SQLite numbers sequentially from the largest
        // index used so far -- so `bind.len()` right after a push is exactly
        // that parameter's index.
        if let Some(cursor) = &after {
            sql.push_str(" AND (received_at > ?");
            bind.push(Value::Integer(cursor.received_at));
            let received_at_param = bind.len();
            sql.push_str(&received_at_param.to_string());
            sql.push_str(" OR (received_at = ?");
            sql.push_str(&received_at_param.to_string());
            sql.push_str(" AND msg_id > ?");
            bind.push(Value::Blob(cursor.msg_id.clone()));
            sql.push_str(&bind.len().to_string());
            sql.push_str("))");
        }
        sql.push_str(" ORDER BY received_at ASC, msg_id ASC");
        let mut stmt = conn.prepare(&sql).map_err(store_err)?;
        let rows = stmt
            .query_map(params_from_iter(bind.iter()), |row| {
                Ok((row_to_carried(row)?, row.get::<_, i64>(5)?))
            })
            .map_err(store_err)?;
        let mut selected: Vec<CarriedEnvelope> = Vec::new();
        let mut next: Option<CoreCarriedCursor> = None;
        let mut used = 0_u64;
        // Only a `break` below leaves this false: falling off the end of the
        // result set means the walk reached the tail of the queue.
        let mut exhausted = true;
        for row in rows {
            let (envelope, received_at) = row.map_err(store_err)?;
            // Counted here, not over `selected`: this row's ciphertext has now
            // been decoded whether or not the budget lets us keep it, and the
            // FC2 counter measures exactly that cost.
            #[cfg(test)]
            self.sealed_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if selected.len() as u64 >= u64::from(max_rows) {
                // The row ceiling. Checked ahead of the byte budget because a
                // page of tiny envelopes never reaches the budget at all, and
                // the frame count is what floods a slow link. No head-of-line
                // exception is needed here: unlike an oversized envelope, a row
                // stopped by the count is not stopped again next round -- the
                // cursor has already advanced past everything that was taken,
                // so this row is the head of the next page.
                exhausted = false;
                break;
            }
            let size = envelope.sealed.len() as u64;
            let cursor = CoreCarriedCursor {
                received_at,
                msg_id: envelope.msg_id.clone(),
            };
            if used.saturating_add(size) > budget_bytes {
                // Head-of-line liveness: an oldest row that alone exceeds the
                // budget is taken anyway, then the round stops. Rejecting it
                // would wedge the lane -- oldest-first means it would be the
                // first row considered every round until expiry. A budget of
                // zero is the one exception: that is the explicit off switch
                // for the lane, not a small allowance. Taking it still
                // advances the cursor past it, so the next round resumes
                // behind it rather than re-deciding the same row.
                if selected.is_empty() && budget_bytes > 0 {
                    selected.push(envelope);
                    next = Some(cursor);
                }
                exhausted = false;
                break;
            }
            used += size;
            selected.push(envelope);
            next = Some(cursor);
        }
        Ok(CoreCarriedSyncPage {
            rows: selected,
            next,
            exhausted,
        })
    }

    /// Drop a carried envelope by `msg_id` -- called once it's been handed to
    /// its recipient (DESIGN.md §5.3: a mule's job is done on delivery).
    /// Returns `true` if a row was removed.
    pub fn remove_carried_envelope(&self, msg_id: Vec<u8>) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let removed = conn
            .execute(
                "DELETE FROM carried_envelopes WHERE msg_id = ?1",
                params![msg_id],
            )
            .map_err(store_err)?;
        Ok(removed > 0)
    }

    /// Delete every carried envelope whose `expiry` is at or before `now_ms`
    /// (DESIGN.md §5.3: "carriers drop the envelope past this time"). Returns
    /// how many were pruned.
    ///
    /// Pruning never invalidates an offering cursor: a [`CoreCarriedCursor`]
    /// is a `(received_at, msg_id)` *value* compared by the keyset predicate,
    /// not a position or a row id, so a walk resuming behind a row this
    /// deleted simply finds the next surviving row. That is the whole reason
    /// the resume point is a keyset rather than an offset -- and the same
    /// reason a confirmed delivery
    /// ([`Self::core_confirm_carried_deliveries`]) needs no cursor fix-up
    /// either.
    pub fn prune_expired_carried(&self, now_ms: i64) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let pruned = conn
            .execute(
                "DELETE FROM carried_envelopes WHERE expiry <= ?1",
                params![now_ms],
            )
            .map_err(store_err)?;
        Ok(pruned as u64)
    }

    /// Whether `msg_id` is in the consumed-hidden-kind set -- i.e. this
    /// device already recorded, via
    /// [`Self::core_record_consumed_hidden_msg_id`], that it consumed that
    /// exact envelope as its sole true endpoint consumer.
    ///
    /// A row that is present but past its `expiry_ms` reads as absent even
    /// before [`Self::prune_expired_consumed_hidden_msg_ids`] gets to it: the
    /// answer must not depend on when the prune last ran, and the safe
    /// direction for an aged-out row is "no evidence" (leave the relay copy
    /// alone).
    pub fn consumed_hidden_msg_id_recorded(
        &self,
        msg_id: Vec<u8>,
        now_ms: i64,
    ) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM consumed_hidden_msg_ids
                 WHERE msg_id = ?1 AND expiry_ms > ?2",
                params![msg_id, now_ms],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        Ok(found.is_some())
    }

    /// Drop every consumed-hidden-kind record whose envelope expiry has
    /// passed. Member of the `prune_expired_*` family and called from the same
    /// places: once an envelope is expired its relay copy is ackable on the
    /// `Expired` disposition alone, so the record has nothing left to prove.
    /// Returns how many rows were pruned.
    pub fn prune_expired_consumed_hidden_msg_ids(&self, now_ms: i64) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let pruned = conn
            .execute(
                "DELETE FROM consumed_hidden_msg_ids WHERE expiry_ms <= ?1",
                params![now_ms],
            )
            .map_err(store_err)?;
        Ok(pruned as u64)
    }

    /// Rows currently in the consumed-hidden-kind set, expired ones included
    /// (diagnostics/tests).
    pub fn consumed_hidden_msg_id_count(&self) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM consumed_hidden_msg_ids", [], |row| {
                row.get(0)
            })
            .map_err(store_err)?;
        Ok(count as u64)
    }

    /// Number of envelopes currently in the carry queue (diagnostics/tests).
    pub fn carried_len(&self) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM carried_envelopes", [], |row| {
                row.get(0)
            })
            .map_err(store_err)?;
        Ok(count as u64)
    }

    /// Unexpired carried envelopes that were classified as family traffic
    /// when received, oldest first. Used by relay upload so one phone with
    /// internet can uplink ciphertext it is muling for known contacts.
    /// Excludes `from_relay = 1` rows: those were pulled FROM the relay by
    /// proxy-polling in the first place (see
    /// [`MessageStore::enqueue_relay_carried_envelope`]), so re-uploading them
    /// here would be pointless churn (and could resurrect an envelope the
    /// real recipient already acked). Also excludes rows whose
    /// `relay_uploaded_to` marker is set (see
    /// [`MessageStore::mark_carried_envelope_relay_uploaded`]): a relay that
    /// already holds an envelope dedupes a re-post but still charges the
    /// family's shared request budget for it, so before the marker existed a
    /// phone with a deep carry queue re-posted its oldest `limit` rows on
    /// every single sync pass -- rate-limiting the whole family -- while
    /// rows behind the batch head never got their first upload at all.
    /// ## Fairness across recipients
    ///
    /// Rows are drawn round-robin across the recipient each envelope is bound
    /// for, and recipients in `skip_recipient_user_ids` are excluded outright
    /// -- the same policy, for the same reason, as
    /// [`MessageStore::pending_relay_outbound_envelopes`]. A failed upload
    /// leaves `relay_uploaded_to` unset, so under flat `received_at` order one
    /// unreachable destination refills the whole window on every pass and
    /// nothing else in the carry queue is ever offered. In the field capture
    /// that accounted for 236 of 758 upload failures, alongside the outbound
    /// and receipt queues doing the same thing.
    ///
    /// This queue could not reuse that fix directly: `carried_envelopes` is
    /// other people's mail being muled, addressed by a day-bucketed
    /// `recipient_hint` that rotates, so there is no recipient column to
    /// partition on and "skip these ids" cannot be expressed against the
    /// stored rows. Resolving the hint is what makes it possible -- and it
    /// belongs here rather than in the shells, because a row the caller
    /// fetches and then skips has still consumed one of `limit` slots.
    ///
    /// The resolution is small and bounded (contacts and groups, each over a
    /// [`CARRY_HINT_DAY_WINDOW_DAYS`] window), so it is materialised into a
    /// temp table and joined, rather than scanning a capped page of rows in
    /// Rust -- a capped scan would quietly reintroduce the starvation it is
    /// meant to remove as soon as one recipient's backlog exceeded the cap.
    ///
    /// Rows whose hint resolves to nothing are still returned, in a partition
    /// of their own. Dropping them was the first instinct -- an unresolvable
    /// hint is one the caller skips anyway -- but it silently changes what
    /// this function promises, and it would strand a legitimate case: a group
    /// carry none of whose members is a contact yet resolves to no recipient
    /// here, while the caller can still upload it via the group path. Giving
    /// them one shared bucket bounds how much of a batch they can take without
    /// removing anything that used to be offered.
    pub fn family_carried_envelopes(
        &self,
        limit: u64,
        now_ms: i64,
        skip_recipient_user_ids: Vec<Vec<u8>>,
    ) -> Result<Vec<CarriedEnvelope>, CoreError> {
        let hint_map = self.carried_hint_recipients(now_ms)?;
        let conn = lock_conn(&self.conn);
        conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS carried_hint_map (
                 hint              BLOB PRIMARY KEY,
                 recipient_user_id BLOB NOT NULL
             );
             DELETE FROM carried_hint_map;",
        )
        .map_err(store_err)?;
        {
            let mut insert = conn
                .prepare(
                    "INSERT OR IGNORE INTO carried_hint_map (hint, recipient_user_id)
                     VALUES (?1, ?2)",
                )
                .map_err(store_err)?;
            for (hint, recipient) in &hint_map {
                insert
                    .execute(params![hint, recipient])
                    .map_err(store_err)?;
            }
        }

        let mut args: Vec<Value> = vec![Value::Integer(now_ms)];
        let skip_clause = if skip_recipient_user_ids.is_empty() {
            String::new()
        } else {
            let placeholders = vec!["?"; skip_recipient_user_ids.len()].join(", ");
            args.extend(skip_recipient_user_ids.into_iter().map(Value::Blob));
            // An unresolved hint has a NULL recipient, and `NULL NOT IN (...)`
            // is NULL rather than true, so those rows need saying explicitly
            // or the skip set would silently drop every one of them.
            format!(
                " AND (m.recipient_user_id IS NULL
                       OR m.recipient_user_id NOT IN ({placeholders}))"
            )
        };
        args.push(Value::Integer(limit as i64));
        let sql = family_carried_upload_sql(&skip_clause);
        let mut stmt = conn.prepare(&sql).map_err(store_err)?;
        let rows = stmt
            .query_map(params_from_iter(args.iter()), row_to_carried)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Record that `relay_url` is confirmed to hold this carried envelope --
    /// either because this device just uploaded it there (2xx) or because it
    /// just fetched the same `msg_id` off that relay's mailbox. From then on
    /// [`MessageStore::family_carried_envelopes`] stops offering the row for
    /// upload, which is the whole fix for the re-post-every-pass storm: an
    /// upload becomes once per envelope per mailbox instead of once per
    /// envelope per pass. First writer wins (`relay_uploaded_to IS NULL`
    /// guard), so a marker never silently moves between relays -- an
    /// endpoint change instead clears markers wholesale
    /// ([`MessageStore::clear_carried_relay_upload_markers`]) and the next
    /// pass re-posts once to the new mailbox.
    ///
    /// This gates re-upload ONLY. It must never feed a removal decision: a
    /// carried envelope still leaves the queue only on digest-proof of
    /// receipt, eviction, or expiry (DTN ack-safety rule in CLAUDE.md).
    /// Returns whether a row was newly marked.
    pub fn mark_carried_envelope_relay_uploaded(
        &self,
        msg_id: Vec<u8>,
        relay_url: String,
    ) -> Result<bool, CoreError> {
        let conn = lock_conn(&self.conn);
        let changed = conn
            .execute(
                "UPDATE carried_envelopes SET relay_uploaded_to = ?2
                 WHERE msg_id = ?1 AND relay_uploaded_to IS NULL",
                params![msg_id, relay_url],
            )
            .map_err(store_err)?;
        Ok(changed > 0)
    }

    /// Forget every carried-upload marker, so the next sync pass offers the
    /// whole (family, non-relay-sourced) carry queue for upload once more.
    /// Called when a relay endpoint changes -- ours (a new Shore Pass, a
    /// manual edit, a restore onto a different config) or a contact's (a
    /// T23 relay-change notice, applied inside
    /// [`MessageStore::apply_contact_relay_update`]) -- because "already on
    /// the old mailbox" says nothing about the new one. Endpoint changes are
    /// rare and the relay dedupes re-posts, so one wholesale re-offer is the
    /// simple safe answer; scoping the clear to one contact's envelopes is
    /// not possible anyway (recipient hints rotate daily and are not
    /// reversible to a contact). Returns how many rows were cleared.
    pub fn clear_carried_relay_upload_markers(&self) -> Result<u64, CoreError> {
        let conn = lock_conn(&self.conn);
        let changed = conn
            .execute(
                "UPDATE carried_envelopes SET relay_uploaded_to = NULL
                 WHERE relay_uploaded_to IS NOT NULL",
                [],
            )
            .map_err(store_err)?;
        Ok(changed as u64)
    }

    /// Durably ingest one fetched relay page inside **one** transaction, and
    /// report what each row's disposition was.
    ///
    /// This is `TXN-01`'s first half as a store primitive. The transaction
    /// opened here commits before this call returns, and therefore *before*
    /// any ack request is constructed, let alone sent. The second short
    /// transaction is the frontier advance
    /// ([`MessageStore::advance_relay_fetch_cursor`]), which the caller
    /// performs only after the ack succeeded. A crash anywhere between the two
    /// leaves a consumed-but-unacked page that the relay re-presents and this
    /// method re-ingests as nothing new, with the frontier still where it was.
    ///
    /// ## What "ingest" means at this revision, stated plainly
    ///
    /// Opening a sealed payload needs the device identity and the crypto path,
    /// and moving that decision into core is package D0 (`mesh_receive`). So
    /// the dispositions this method can honestly derive are the ones that need
    /// no key:
    ///
    /// * `Expired` — the envelope's own public expiry has passed. Nothing is
    ///   left to preserve, so it is ack-eligible.
    /// * `Rejected` — the public header failed local validation (an
    ///   impossible `hop_ttl`, an expiry outside the accepted window). Never
    ///   acked: a header this device cannot accept is not proof it was the
    ///   payload's endpoint consumer, so the server's copy survives for
    ///   another client or another build. It is still a *terminal*
    ///   disposition, and deliberately so — holding the frontier on one would
    ///   strand every row above it on every ordinary pass, forever.
    ///
    ///   What makes that survivable is the periodic sweep, which walks the
    ///   mailbox from zero rather than from the frontier: a row this build
    ///   refused and a later build accepts is re-presented there, and the
    ///   server's copy was never acked away, so nothing was lost. Note the
    ///   shape of what is being said, though — "a newer sender's header can
    ///   cost this device a delay until it sweeps or updates" is a behaviour
    ///   statement neither shipped shell was confirmed to make. Package C4
    ///   pins it against `relayd` end-to-end before any shell migrates onto
    ///   this primitive.
    /// * `Seen` — this device already has the envelope. Three routes reach
    ///   it: a `messages` row, the consumed-hidden set, or the carry queue
    ///   naming the same `msg_id`; and a fourth, narrower one — the carry
    ///   queue's unique `content_digest` index refusing a row whose
    ///   `(recipient_hint, sealed)` pair it already holds under a different
    ///   `msg_id`, which is the same envelope arriving twice with two ids.
    ///   None of those routes acks anything by itself: ack eligibility for a
    ///   `Seen` row is [`MessageStore::core_relay_ack_ids_with_consumed`]'s
    ///   question alone, and it asks for evidence naming *that* `msg_id`, so
    ///   the digest-collision route cannot produce an ack.
    /// * `Carried` — newly persisted as a relay-sourced carried row, so it is
    ///   delivered over the mesh and never re-uploaded. Never acked
    ///   (`ACK-01`): muling is not consuming.
    /// * `Failed` — a valid new row could not fit its applicable foreign or
    ///   total budget. The candidate is removed again in the same transaction,
    ///   remains unacked, and holds the frontier so it can retry.
    ///
    /// `Consumed` is deliberately absent. Until D0 lands, a row this device is
    /// the true endpoint for is carried rather than opened, which costs a
    /// re-fetch and never a deletion — the safe direction.
    pub fn ingest_relay_page(
        &self,
        envelopes: Vec<CoreRelayFetchedEnvelope>,
        now_ms: i64,
        pass_id: Option<String>,
        action_id: i64,
    ) -> Result<CoreRelayPageIngest, CoreError> {
        let mut ingest = CoreRelayPageIngest {
            rows: Vec::with_capacity(envelopes.len()),
            rows_returned: envelopes.len() as u32,
            rows_ingested: 0,
            rows_already_known: 0,
            rows_expired: 0,
            rows_rejected: 0,
            fully_processed: true,
        };
        if envelopes.is_empty() {
            return Ok(ingest);
        }

        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        tx.execute(
            "DELETE FROM carried_envelopes WHERE expiry <= ?1",
            params![now_ms],
        )
        .map_err(store_err)?;
        for envelope in envelopes {
            let carried = CarriedEnvelope {
                msg_id: envelope.msg_id.clone(),
                hop_ttl: envelope.hop_ttl,
                expiry: envelope.expiry_ms,
                recipient_hint: envelope.recipient_hint.clone(),
                sealed: envelope.sealed.clone(),
            };
            let disposition = if envelope.expiry_ms <= now_ms {
                ingest.rows_expired += 1;
                CoreInboundDisposition::Expired
            } else if validate_carried_envelope(&carried, now_ms).is_err() {
                // A header this device cannot accept locally. Not stored and
                // never acked, so the server's copy outlives us -- but still
                // terminal, so the frontier moves past it. See the method
                // doc: a header that is malformed now will be malformed on
                // every future pass, and a frontier held on one would strand
                // the whole mailbox above it.
                ingest.rows_rejected += 1;
                CoreInboundDisposition::Rejected
            } else if relay_row_already_known(&tx, &envelope.msg_id)? {
                ingest.rows_already_known += 1;
                CoreInboundDisposition::Seen
            } else {
                let size = carried.sealed.len() as i64;
                let digest = carried_content_digest(&carried.recipient_hint, &carried.sealed);
                // The stored `hop_ttl` is one less than the header's, exactly
                // as `carriedHopTtl` on Android and `carriedHopTtl` on iOS
                // apply it to a relay-fetched row. Fetching from the relay
                // and handing the envelope on is a hop, so a row stored
                // verbatim would report a single-mule delivery as zero hops
                // taken and would re-flood with one hop of the sender's
                // budget this device never paid for.
                let carried_hop_ttl = carried.hop_ttl.saturating_sub(1);
                let inserted = tx
                    .execute(
                        "INSERT OR IGNORE INTO carried_envelopes
                            (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family,
                             received_at, size_bytes, from_relay, content_digest)
                         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, 1, ?8)",
                        params![
                            carried.msg_id,
                            carried_hop_ttl as i64,
                            carried.expiry,
                            carried.recipient_hint,
                            carried.sealed,
                            now_ms,
                            size,
                            digest,
                        ],
                    )
                    .map_err(store_err)?;
                if inserted > 0 {
                    if finalize_carried_admission(
                        &tx,
                        &envelope.msg_id,
                        true,
                        size,
                        i64::MAX,
                        DEFAULT_TOTAL_CARRY_BUDGET_BYTES,
                        now_ms,
                    )? {
                        ingest.rows_ingested += 1;
                        CoreInboundDisposition::Carried
                    } else {
                        CoreInboundDisposition::Failed
                    }
                } else {
                    ingest.rows_already_known += 1;
                    CoreInboundDisposition::Seen
                }
            };
            ingest.rows.push(CoreRelayEnvelopeDisposition {
                relay_id: envelope.id,
                msg_id: envelope.msg_id,
                disposition,
                recipient_hint: envelope.recipient_hint,
            });
        }
        tx.commit().map_err(store_err)?;

        // Every row above reached a terminal disposition, so the page is fully
        // processed; the flag exists because a future ingest (D0) can fail to
        // store a row it opened, and `CURSOR-01` then has to hold the frontier.
        ingest.fully_processed = ingest
            .rows
            .iter()
            .all(|row| row.disposition != CoreInboundDisposition::Failed);

        {
            let conn: &Connection = &conn;
            // Named by pass and action like every other record the pass
            // emits. Without them the one record that says TXN-01's first
            // transaction happened is the one a reader cannot join to the
            // fetch above it or the ack below it.
            let mut draft = crate::protocol_event::ProtocolEventDraft::new(
                crate::protocol_event::ProtocolEventCode::PageIngested,
                now_ms,
                if ingest.rows_ingested > 0 {
                    "page_consumed_before_any_ack"
                } else {
                    "replay_applied_nothing"
                },
            )
            .invariants(&["PAGE-01", "TXN-01", "IDEMP-01"])
            .count("rows_returned", i64::from(ingest.rows_returned))
            .count("rows_ingested", i64::from(ingest.rows_ingested))
            .count("rows_already_known", i64::from(ingest.rows_already_known))
            .count("rows_expired", i64::from(ingest.rows_expired))
            .count("transactions", 1);
            if let Some(pass_id) = pass_id {
                draft = draft.pass(pass_id);
            }
            if action_id > 0 {
                draft = draft.action(action_id);
            }
            crate::protocol_event::note(conn, &[draft]);
        }
        Ok(ingest)
    }
}

/// Does a durable local record already name this envelope?
///
/// Three places count, and they are the three places a `msg_id` can be
/// remembered: a stored message, the consumed-hidden set for kinds that leave
/// no message row, and the carry queue itself. Anything found here is `Seen` —
/// which says only "I recognise this", never "I consumed it". Whether a `Seen`
/// row may be acked is [`MessageStore::core_relay_ack_ids_with_consumed`]'s
/// question alone.
fn relay_row_already_known(tx: &Transaction<'_>, msg_id: &[u8]) -> Result<bool, CoreError> {
    let known: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM messages WHERE msg_id = ?1
             UNION ALL SELECT 1 FROM consumed_hidden_msg_ids WHERE msg_id = ?1
             UNION ALL SELECT 1 FROM carried_envelopes WHERE msg_id = ?1
             LIMIT 1",
            params![msg_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_err)?;
    Ok(known.is_some())
}

impl MessageStore {
    /// Emit protocol events from a core decision point that holds no
    /// connection of its own — today that is `session::relay_pass`.
    ///
    /// Best-effort by construction, like every other operational emit: a
    /// diagnostics record is never allowed to be the reason a relay pass
    /// fails. See `crate::protocol_event::note`.
    pub(crate) fn note_protocol_events(
        &self,
        drafts: &[crate::protocol_event::ProtocolEventDraft],
    ) {
        let conn = lock_conn(&self.conn);
        crate::protocol_event::note(&conn, drafts);
    }

    /// The archive-local pseudonym for one raw identifier, or `None` if the
    /// ring could not allocate one. Callers that cannot name an actor emit
    /// without one rather than failing.
    pub(crate) fn protocol_pseudonym(&self, kind: &'static str, raw: &[u8]) -> Option<String> {
        let conn = lock_conn(&self.conn);
        crate::protocol_event::actor_pseudonym(&conn, kind, raw).ok()
    }
}

/// The migration canary's one write.
#[uniffi::export]
impl MessageStore {
    /// Record what a shadow comparison found, and nothing else.
    ///
    /// This is the only store call the canary is permitted to make, and it
    /// touches exactly one table: the bounded diagnostics ring. No message,
    /// no cursor, no marker, no health row. It is deliberately a method
    /// narrow enough that a shell can hand the canary *this call alone* —
    /// Android passes it as a one-method sink rather than passing the store —
    /// so "production store writes come from one engine per pass" is a
    /// property of what the canary can reach rather than a rule someone has
    /// to remember. The report it takes cannot be turned back into a row.
    ///
    /// Every sampled comparison records one summary line whether or not it
    /// found anything, because "the canary ran and agreed" and "the canary
    /// never ran" are the two readings a release archive most needs to tell
    /// apart. Each *kind* of disagreement then gets one record carrying how
    /// many rows showed it.
    ///
    /// One record per kind rather than per row is what keeps this affordable.
    /// The ring holds a couple of thousand events and every append evicts the
    /// oldest, so an emitter whose volume scales with a device's rows can
    /// quietly become the only thing in a support archive — and a diverging
    /// device diverges *systematically*, so the hundredth copy of a finding
    /// carries nothing the first did not. That caps one sampled pass at seven
    /// records however badly the two engines disagree.
    ///
    /// `SECRET-01` is structural here: a [`CoreRelayShadowReport`] has no
    /// field that can hold a token, an endpoint or a payload.
    pub fn note_relay_shadow_report(&self, report: CoreRelayShadowReport, now_ms: i64) {
        let mut drafts = Vec::with_capacity(report.mismatches.len() + 1);
        drafts.push(
            crate::protocol_event::ProtocolEventDraft::new(
                crate::protocol_event::ProtocolEventCode::ShadowMismatch,
                now_ms,
                if report.mismatches.is_empty() {
                    "shadow_agreed"
                } else {
                    "shadow_diverged"
                },
            )
            .count("steps_compared", i64::from(report.steps_compared))
            .count("skips_compared", i64::from(report.skips_compared))
            .count("rows_unshadowed", i64::from(report.rows_unshadowed))
            .count("rows_truncated", i64::from(report.rows_truncated))
            .count("mismatch_kinds", report.mismatches.len() as i64),
        );
        for mismatch in &report.mismatches {
            drafts.push(
                crate::protocol_event::ProtocolEventDraft::new(
                    crate::protocol_event::ProtocolEventCode::ShadowMismatch,
                    now_ms,
                    mismatch.kind.as_token(),
                )
                .count("first_index", i64::from(mismatch.first_index))
                .count("rows", i64::from(mismatch.rows)),
            );
        }
        let conn = lock_conn(&self.conn);
        crate::protocol_event::note(&conn, &drafts);
    }
}

/// One ingested relay page: what happened to each row, and the counts a
/// transcript and a summary report.
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct CoreRelayPageIngest {
    pub rows: Vec<CoreRelayEnvelopeDisposition>,
    pub rows_returned: u32,
    /// Rows newly persisted by this call. Zero on a replay of a page already
    /// ingested, which is what makes the ingest idempotent (`IDEMP-01`).
    pub rows_ingested: u32,
    pub rows_already_known: u32,
    pub rows_expired: u32,
    pub rows_rejected: u32,
    /// Whether every row reached a terminal disposition. `CURSOR-01` forbids
    /// advancing a frontier across a page where this is false.
    pub fully_processed: bool,
}

/// Hard ceiling on `consumed_hidden_msg_ids` rows, as a backstop under the
/// expiry bound rather than instead of it.
///
/// Ordinary traffic is nowhere near this: every row costs one envelope this
/// device opened with its own key and consumed, and the whole set ages out
/// within the envelope expiry (7 days by default). The cap exists for the one
/// shape an *unknown* sender can drive -- [`crate::KIND_FRIEND_REQUEST`] and
/// [`crate::KIND_INTRODUCED_FRIEND_REQUEST`] are deliberately accepted from
/// strangers (see `core_pairwise_sender_authorized`), so without a ceiling a
/// stranger could grow this table without an accepted contact relationship.
/// Eviction drops the soonest-to-expire rows first, which is the safe
/// direction: a dropped row can only cost a relay re-fetch, never a deletion.
const CONSUMED_HIDDEN_MSG_ID_LIMIT: i64 = 20_000;

/// Internal-only helpers, never exported over UniFFI.
impl MessageStore {
    /// Raw insert behind [`MessageStore::core_record_consumed_hidden_msg_id`],
    /// which owns every safety condition. Deliberately `pub(crate)`: nothing
    /// outside core may write this table, because a row here is a licence to
    /// DELETE a relay copy and the licence is only valid under the sole-true-
    /// endpoint-consumer rule that lives on the caller.
    ///
    /// Idempotent on `msg_id`, keeping the later expiry if the same envelope
    /// is consumed twice with different clamps. Returns `true` when the set
    /// now vouches for `msg_id`.
    pub(crate) fn insert_consumed_hidden_msg_id(
        &self,
        msg_id: Vec<u8>,
        expiry_ms: i64,
    ) -> Result<bool, CoreError> {
        self.insert_consumed_hidden_msg_id_capped(msg_id, expiry_ms, CONSUMED_HIDDEN_MSG_ID_LIMIT)
    }

    /// [`Self::insert_consumed_hidden_msg_id`] with the row cap injected, so
    /// the eviction rule can be tested at a handful of rows instead of
    /// [`CONSUMED_HIDDEN_MSG_ID_LIMIT`] of them.
    pub(crate) fn insert_consumed_hidden_msg_id_capped(
        &self,
        msg_id: Vec<u8>,
        expiry_ms: i64,
        limit: i64,
    ) -> Result<bool, CoreError> {
        let mut conn = lock_conn(&self.conn);
        let tx = conn.transaction().map_err(store_err)?;
        tx.execute(
            "INSERT INTO consumed_hidden_msg_ids (msg_id, expiry_ms)
             VALUES (?1, ?2)
             ON CONFLICT(msg_id) DO UPDATE SET
                 expiry_ms = MAX(expiry_ms, excluded.expiry_ms)",
            params![&msg_id, expiry_ms],
        )
        .map_err(store_err)?;
        // Only walk the eviction query when the table is actually over the
        // cap. The count is an index-only scan of a table that ordinarily
        // holds a few hundred rows; the eviction below is the expensive part
        // and, outside an abuse case, never runs at all.
        let rows: i64 = tx
            .query_row("SELECT COUNT(*) FROM consumed_hidden_msg_ids", [], |row| {
                row.get(0)
            })
            .map_err(store_err)?;
        if rows > limit {
            tx.execute(
                "DELETE FROM consumed_hidden_msg_ids
                 WHERE msg_id NOT IN (
                     SELECT msg_id FROM consumed_hidden_msg_ids
                     ORDER BY expiry_ms DESC, msg_id ASC
                     LIMIT ?1
                 )",
                params![limit],
            )
            .map_err(store_err)?;
        }
        let kept: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM consumed_hidden_msg_ids WHERE msg_id = ?1",
                params![&msg_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?;
        tx.commit().map_err(store_err)?;
        Ok(kept.is_some())
    }
}

/// Internal-only helper, never exported over UniFFI: implementation detail
/// of `engine.rs::core_digest_spray_plan`'s per-contact outbound fan-in
/// (FC2), not API either platform shell calls directly.
impl MessageStore {
    /// Budget-aware, exclusion-pushed-down variant of
    /// [`MessageStore::outbound_envelopes_after`] used only by the digest
    /// spray plan. Unlike the public method, this pushes both `expiry` and
    /// `known_msg_ids` exclusion into the SQL `WHERE` clause -- so ciphertext
    /// for an expired or already-peer-known row is never decoded -- and
    /// stops stepping the query as soon as `budget_bytes` (this contact's
    /// share of what's left of the shared spray budget) would be exceeded,
    /// so a contact's entire remaining backlog is never pulled into memory
    /// once the budget for this spray is already spent.
    ///
    /// Returns the selected envelopes (oldest first, already filtered and
    /// budget-bounded) plus whether the budget ran out partway through this
    /// contact's stream. The exact greedy selection order this preserves --
    /// contacts in caller-supplied order, envelopes within a contact oldest
    /// first, stop at the very first envelope that would overflow the
    /// *shared* budget -- is [`crate::engine::select_own_outbound`]'s
    /// algorithm; the caller (`core_digest_spray_plan`) still runs that
    /// function over the assembled, now much smaller, result as the single
    /// source of truth for the final selection, so this method only needs
    /// to reproduce its budget arithmetic closely enough to avoid fetching
    /// ciphertext that pass would discard anyway -- it does not need to be
    /// the final word on what's included.
    pub(crate) fn outbound_envelopes_after_budgeted(
        &self,
        chat_id: Vec<u8>,
        sender_user_id: Vec<u8>,
        after_lamport: u64,
        known_msg_ids: &HashSet<Vec<u8>>,
        now_ms: i64,
        budget_bytes: u64,
    ) -> Result<(Vec<OutboundEnvelope>, bool), CoreError> {
        let conn = lock_conn(&self.conn);
        let mut sql = String::from(
            "SELECT msg_id, recipient_user_id, chat_id, sender_user_id, kind, lamport,
                    timestamp, hop_ttl, expiry, recipient_hint, sealed
             FROM outbound_envelopes
             WHERE chat_id = ?1 AND sender_user_id = ?2 AND lamport > ?3 AND expiry > ?4",
        );
        let mut bind: Vec<Value> = vec![
            Value::Blob(chat_id),
            Value::Blob(sender_user_id),
            Value::Integer(after_lamport as i64),
            Value::Integer(now_ms),
        ];
        let known: Vec<Vec<u8>> = known_msg_ids.iter().cloned().collect();
        push_not_in(&mut sql, &mut bind, "msg_id", &known);
        sql.push_str(" ORDER BY lamport ASC");
        let mut stmt = conn.prepare(&sql).map_err(store_err)?;
        let mut rows = stmt
            .query_map(params_from_iter(bind.iter()), row_to_outbound)
            .map_err(store_err)?;
        let mut selected = Vec::new();
        let mut used = 0_u64;
        let mut exhausted = false;
        for row in &mut rows {
            let envelope = row.map_err(store_err)?;
            // Every iteration here is a `sealed` blob actually decoded --
            // known/expired rows never reach this loop at all (excluded by
            // the `WHERE` clause above), so counting per-decode (rather
            // than per-selected) also captures the one row that trips the
            // budget and gets decoded just to learn its size before being
            // rejected.
            #[cfg(test)]
            self.sealed_reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let size = envelope.sealed.len() as u64;
            if used.saturating_add(size) > budget_bytes {
                exhausted = true;
                break;
            }
            used += size;
            selected.push(envelope);
        }
        Ok((selected, exhausted))
    }
}

/// Appends ` AND {column} NOT IN (?,?,...)` to `sql` and pushes the
/// corresponding blob values onto `bind`, in order -- shared by the FC2
/// digest-spray queries that need to exclude a caller-supplied set of
/// msg_ids/hints in SQL rather than fetch-then-filter in Rust. A `NOT IN
/// ()` with zero elements is invalid SQLite syntax, so an empty `values`
/// leaves `sql`/`bind` untouched (no exclusion predicate at all, which is
/// the correct behavior: nothing to exclude).
fn push_not_in(sql: &mut String, bind: &mut Vec<Value>, column: &str, values: &[Vec<u8>]) {
    if values.is_empty() {
        return;
    }
    sql.push_str(" AND ");
    sql.push_str(column);
    sql.push_str(" NOT IN (");
    for (i, value) in values.iter().enumerate() {
        if i > 0 {
            sql.push(',');
        }
        sql.push('?');
        bind.push(Value::Blob(value.clone()));
    }
    sql.push(')');
}

fn validate_carried_envelope(envelope: &CarriedEnvelope, now_ms: i64) -> Result<(), CoreError> {
    if envelope.hop_ttl > crate::DEFAULT_HOP_TTL {
        return Err(CoreError::Malformed(format!(
            "carried envelope hop_ttl exceeds {}",
            crate::DEFAULT_HOP_TTL
        )));
    }
    if envelope.expiry <= now_ms
        || envelope.expiry > now_ms.saturating_add(crate::MAX_CARRY_FUTURE_MS)
    {
        return Err(CoreError::Malformed(
            "carried envelope expiry is outside the accepted window".to_string(),
        ));
    }
    Ok(())
}

/// Length of the metadata-only chat/sender hashes used by
/// [`delivery_metrics`]. Eight bytes is enough to keep distinct chats (or
/// senders) apart in an export without storing (or being reversible to) the
/// raw contact/group/user id.
const METRIC_CHAT_HASH_LEN: usize = 8;

/// Lowercase hex, for rendering the metric chat/sender hash in the CSV
/// export.
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Shared hash used for both the chat and sender tags in [`delivery_metrics`]
/// rows: a short, non-reversible tag, never a raw user/group id.
fn metric_hash8(id: &[u8]) -> Vec<u8> {
    let mut hasher = Blake2bVar::new(METRIC_CHAT_HASH_LEN).expect("valid BLAKE2b digest length");
    hasher.update(id);
    let mut digest = vec![0; METRIC_CHAT_HASH_LEN];
    hasher
        .finalize_variable(&mut digest)
        .expect("digest output has configured length");
    digest
}

/// A short, non-reversible tag for a chat id, used only to group field-metric
/// rows in the export. Never a raw user/group id -- see [`delivery_metrics`].
fn metric_chat_hash(chat_id: &[u8]) -> Vec<u8> {
    metric_hash8(chat_id)
}

/// FC1: a short, non-reversible tag for a sender's user id, added to the
/// [`delivery_metrics`] primary key so that two group members who happen to
/// share a lamport value (each member has an independent lamport stream) no
/// longer collide and silently drop one arrival.
fn metric_sender_hash(sender_user_id: &[u8]) -> Vec<u8> {
    metric_hash8(sender_user_id)
}

/// Sentinel sender hash for locally authored ("sent") [`delivery_metrics`]
/// rows. `record_sent_metric` has no sender argument -- this device is
/// always the sole author of its own outbound stream, so there is no
/// collision to resolve and a fixed placeholder is enough to satisfy the
/// primary key.
fn metric_sender_self() -> Vec<u8> {
    vec![0u8; METRIC_CHAT_HASH_LEN]
}

/// FC1 migration: pre-existing on-disk stores have `delivery_metrics` keyed
/// on `(chat_hash, lamport, direction)` only, which silently drops a group
/// arrival whenever two senders share a lamport value at the same watermark
/// (routine -- every group member has an independent lamport stream). This
/// table is local, best-effort, pre-cruise diagnostics data (see the schema
/// doc comment) with no cross-device meaning, so a row-preserving migration
/// isn't worth it: drop and let it be recreated with the current schema. New
/// arrivals repopulate it going forward.
fn migrate_delivery_metrics_schema(conn: &Connection) -> Result<(), CoreError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(delivery_metrics)")
        .map_err(store_err)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(store_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_err)?;
    drop(stmt);
    if names.is_empty() || names.iter().any(|c| c == "sender_hash") {
        // Fresh store (SCHEMA already created the current shape) or already
        // migrated on a previous open.
        return Ok(());
    }
    conn.execute("DROP TABLE delivery_metrics", [])
        .map_err(store_err)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS delivery_metrics (
            chat_hash         BLOB NOT NULL,
            lamport           INTEGER NOT NULL,
            direction         INTEGER NOT NULL,
            sender_hash       BLOB NOT NULL,
            at_ms             INTEGER NOT NULL,
            delivered_at_ms   INTEGER,
            via_transport     INTEGER,
            arrival_transport INTEGER,
            hop_count         INTEGER,
            PRIMARY KEY(chat_hash, lamport, direction, sender_hash)
        );",
    )
    .map_err(store_err)?;
    Ok(())
}

fn carried_content_digest(recipient_hint: &[u8], sealed: &[u8]) -> Vec<u8> {
    let mut hasher =
        Blake2bVar::new(CARRIED_CONTENT_DIGEST_LEN).expect("valid BLAKE2b digest length");
    hasher.update(recipient_hint);
    hasher.update(sealed);
    let mut digest = vec![0; CARRIED_CONTENT_DIGEST_LEN];
    hasher
        .finalize_variable(&mut digest)
        .expect("digest output has configured length");
    digest
}

const CARRY_ADMISSION_CAPACITY_ERROR: &str =
    "carry admission rejected: applicable byte budget cannot fit row";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CarriedBudgetEnforcement {
    evicted_rows: i64,
    evicted_bytes: i64,
    total_bytes: i64,
    within_budget: bool,
}

fn note_carried_row_eviction(
    tx: &Transaction<'_>,
    pressure: CarriedBudgetEnforcement,
    now_ms: i64,
) {
    if pressure.evicted_rows == 0 {
        return;
    }
    crate::protocol_event::note(
        tx,
        &[crate::protocol_event::ProtocolEventDraft::new(
            crate::protocol_event::ProtocolEventCode::CarriedRowEvicted,
            now_ms,
            "foreign_rows_evicted",
        )
        .invariants(&["EVICT-01", "CARRY-01"])
        .count("rows_evicted", pressure.evicted_rows)
        .count("bytes_evicted", pressure.evicted_bytes)
        .count("queue_bytes", pressure.total_bytes.max(0))],
    );
}

fn note_carry_admission_rejected(
    tx: &Transaction<'_>,
    is_family: bool,
    incoming_bytes: i64,
    foreign_budget_bytes: i64,
    total_budget_bytes: i64,
    now_ms: i64,
) {
    let mut draft = crate::protocol_event::ProtocolEventDraft::new(
        crate::protocol_event::ProtocolEventCode::CarryAdmissionRejected,
        now_ms,
        if is_family {
            "family_admission_rejected"
        } else {
            "foreign_admission_rejected"
        },
    )
    .invariants(&["EVICT-01", "CARRY-01"])
    .count("incoming_bytes", incoming_bytes.max(0))
    .count("total_budget_bytes", total_budget_bytes.max(0));
    if !is_family {
        draft = draft.count("foreign_budget_bytes", foreign_budget_bytes.max(0));
    }
    crate::protocol_event::note(tx, &[draft]);
}

/// Whether the just-inserted candidate can fit after every *other* foreign
/// row has been evicted. Checking this before enforcement is what makes a
/// failed admission atomic: an impossible candidate cannot destroy older
/// foreign rows on its way to being rejected.
fn carried_admission_can_fit(
    tx: &Transaction<'_>,
    is_family: bool,
    incoming_bytes: i64,
    foreign_budget_bytes: i64,
    total_budget_bytes: i64,
) -> Result<bool, CoreError> {
    let family_bytes: i64 = tx
        .query_row(
            "SELECT COALESCE(SUM(size_bytes), 0)
             FROM carried_envelopes WHERE is_family = 1",
            [],
            |row| row.get(0),
        )
        .map_err(store_err)?;
    let total_budget_bytes = total_budget_bytes.max(0);
    if is_family {
        Ok(family_bytes <= total_budget_bytes)
    } else {
        Ok(incoming_bytes <= foreign_budget_bytes.max(0)
            && family_bytes.saturating_add(incoming_bytes) <= total_budget_bytes)
    }
}

/// Apply EVICT-01 to one row that was inserted in the caller's transaction.
/// Returns `false` only after removing that same candidate and recording a
/// redacted rejection. Existing family rows are never deletion candidates.
fn finalize_carried_admission(
    tx: &Transaction<'_>,
    msg_id: &[u8],
    is_family: bool,
    incoming_bytes: i64,
    foreign_budget_bytes: i64,
    total_budget_bytes: i64,
    now_ms: i64,
) -> Result<bool, CoreError> {
    if !carried_admission_can_fit(
        tx,
        is_family,
        incoming_bytes,
        foreign_budget_bytes,
        total_budget_bytes,
    )? {
        tx.execute(
            "DELETE FROM carried_envelopes WHERE msg_id = ?1",
            params![msg_id],
        )
        .map_err(store_err)?;
        note_carry_admission_rejected(
            tx,
            is_family,
            incoming_bytes,
            foreign_budget_bytes,
            total_budget_bytes,
            now_ms,
        );
        return Ok(false);
    }

    let pressure = enforce_carried_budgets_protecting(
        tx,
        foreign_budget_bytes,
        total_budget_bytes,
        Some(msg_id),
    )?;
    if !pressure.within_budget {
        // The feasibility check above and this enforcement run under the same
        // SQLite transaction, so this would mean the two rules drifted. Fail
        // closed rather than silently accepting an over-budget row.
        return Err(CoreError::Store(
            "carry admission feasibility/enforcement mismatch".into(),
        ));
    }
    note_carried_row_eviction(tx, pressure, now_ms);
    Ok(true)
}

fn enqueue_carried_envelope_with_budgets(
    store: &MessageStore,
    envelope: CarriedEnvelope,
    is_family: bool,
    received_at_ms: i64,
    foreign_budget_bytes: i64,
    total_budget_bytes: i64,
) -> Result<bool, CoreError> {
    validate_carried_envelope(&envelope, received_at_ms)?;
    let content_digest = carried_content_digest(&envelope.recipient_hint, &envelope.sealed);
    let msg_id = envelope.msg_id.clone();
    let mut conn = lock_conn(&store.conn);
    let tx = conn.transaction().map_err(store_err)?;
    let size = envelope.sealed.len() as i64;
    let changed = tx
        .execute(
            "INSERT OR IGNORE INTO carried_envelopes
                (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family,
                 received_at, size_bytes, content_digest)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                envelope.msg_id,
                envelope.hop_ttl as i64,
                envelope.expiry,
                envelope.recipient_hint,
                envelope.sealed,
                is_family as i64,
                received_at_ms,
                size,
                content_digest,
            ],
        )
        .map_err(store_err)?;

    if changed == 0 {
        // Already carrying this msg_id or content: nothing inserted, so the
        // budget cannot have grown.
        tx.commit().map_err(store_err)?;
        return Ok(false);
    }

    tx.execute(
        "DELETE FROM carried_envelopes WHERE expiry <= ?1",
        params![received_at_ms],
    )
    .map_err(store_err)?;
    let admitted = finalize_carried_admission(
        &tx,
        &msg_id,
        is_family,
        size,
        foreign_budget_bytes,
        total_budget_bytes,
        received_at_ms,
    )?;
    tx.commit().map_err(store_err)?;
    if admitted {
        Ok(true)
    } else {
        Err(CoreError::Store(CARRY_ADMISSION_CAPACITY_ERROR.into()))
    }
}

fn enqueue_relay_carried_envelope_with_budget(
    store: &MessageStore,
    envelope: CarriedEnvelope,
    now_ms: i64,
    total_budget_bytes: i64,
) -> Result<bool, CoreError> {
    validate_carried_envelope(&envelope, now_ms)?;
    let content_digest = carried_content_digest(&envelope.recipient_hint, &envelope.sealed);
    let msg_id = envelope.msg_id.clone();
    let mut conn = lock_conn(&store.conn);
    let tx = conn.transaction().map_err(store_err)?;
    let size = envelope.sealed.len() as i64;
    let changed = tx
        .execute(
            "INSERT OR IGNORE INTO carried_envelopes
                (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family,
                 received_at, size_bytes, from_relay, content_digest)
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, 1, ?8)",
            params![
                envelope.msg_id,
                envelope.hop_ttl as i64,
                envelope.expiry,
                envelope.recipient_hint,
                envelope.sealed,
                now_ms,
                size,
                content_digest,
            ],
        )
        .map_err(store_err)?;
    if changed == 0 {
        tx.commit().map_err(store_err)?;
        return Ok(false);
    }

    tx.execute(
        "DELETE FROM carried_envelopes WHERE expiry <= ?1",
        params![now_ms],
    )
    .map_err(store_err)?;
    let admitted = finalize_carried_admission(
        &tx,
        &msg_id,
        true,
        size,
        i64::MAX,
        total_budget_bytes,
        now_ms,
    )?;
    tx.commit().map_err(store_err)?;
    if admitted {
        Ok(true)
    } else {
        Err(CoreError::Store(CARRY_ADMISSION_CAPACITY_ERROR.into()))
    }
}

/// Backfill the content-level dedupe key for existing stores and collapse
/// pre-migration duplicates deterministically, keeping the oldest row. No
/// deletion here is a delivery signal; this is local queue compaction only.
///
/// FC3: this used to be a `SELECT ... LIMIT 1` + single-row `UPDATE`/`DELETE`
/// pair per legacy (NULL-digest) row, run synchronously in
/// [`MessageStore::open`] -- for K legacy rows that's O(K) round trips, each
/// re-scanning the shrinking `WHERE content_digest IS NULL` set from
/// scratch. Instead: one bulk `SELECT` reads every legacy row up front
/// (oldest first, matching the original loop's iteration order so the
/// "collision keeps the oldest row" tie-break is unchanged, including
/// against rows a prior migration already assigned a digest to -- those are
/// seeded into `seen` first, exactly as before), the BLAKE2b digest is
/// still computed per row in Rust (SQLite has no built-in BLAKE2b), and then
/// the writes are batched into at most one `UPDATE` (via a `VALUES`
/// pseudo-table joined by `rowid`) and one `DELETE ... WHERE rowid IN
/// (...)` -- a constant number of statements regardless of K, instead of
/// 2*K+1.
fn migrate_carried_content_digests(conn: &mut Connection) -> Result<(), CoreError> {
    let tx = conn.transaction().map_err(store_err)?;
    let mut seen: HashSet<Vec<u8>> = {
        let mut stmt = tx
            .prepare(
                "SELECT content_digest FROM carried_envelopes
                 WHERE content_digest IS NOT NULL",
            )
            .map_err(store_err)?;
        let collected = stmt
            .query_map([], |row| row.get(0))
            .map_err(store_err)?
            .collect::<Result<HashSet<_>, _>>()
            .map_err(store_err)?;
        collected
    };

    let legacy: Vec<(i64, Vec<u8>, Vec<u8>)> = {
        let mut stmt = tx
            .prepare(
                "SELECT rowid, recipient_hint, sealed
                 FROM carried_envelopes
                 WHERE content_digest IS NULL
                 ORDER BY received_at ASC, msg_id ASC",
            )
            .map_err(store_err)?;
        let collected = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(store_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(store_err)?;
        collected
    };

    let mut update_pairs: Vec<Value> = Vec::with_capacity(legacy.len() * 2);
    let mut update_rowids: Vec<Value> = Vec::with_capacity(legacy.len());
    let mut delete_rowids: Vec<Value> = Vec::new();
    for (rowid, recipient_hint, sealed) in legacy {
        let digest = carried_content_digest(&recipient_hint, &sealed);
        if seen.insert(digest.clone()) {
            update_pairs.push(Value::Integer(rowid));
            update_pairs.push(Value::Blob(digest));
            update_rowids.push(Value::Integer(rowid));
        } else {
            delete_rowids.push(Value::Integer(rowid));
        }
    }

    if !update_rowids.is_empty() {
        let values_clause = std::iter::repeat_n("(?,?)", update_rowids.len())
            .collect::<Vec<_>>()
            .join(",");
        let in_clause = std::iter::repeat_n("?", update_rowids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            // The bundled SQLite rejects `AS v(rid, digest)`-style derived
            // table column renaming here, so this relies on the default
            // `column1`/`column2` names SQLite assigns to a bare `VALUES`
            // row constructor.
            "UPDATE carried_envelopes
             SET content_digest = (
                 SELECT v.column2 FROM (VALUES {values_clause}) AS v
                 WHERE v.column1 = carried_envelopes.rowid
             )
             WHERE rowid IN ({in_clause})"
        );
        let mut bind = update_pairs;
        bind.extend(update_rowids);
        tx.execute(&sql, params_from_iter(bind.iter()))
            .map_err(store_err)?;
    }

    if !delete_rowids.is_empty() {
        let in_clause = std::iter::repeat_n("?", delete_rowids.len())
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("DELETE FROM carried_envelopes WHERE rowid IN ({in_clause})");
        tx.execute(&sql, params_from_iter(delete_rowids.iter()))
            .map_err(store_err)?;
    }

    let pressure =
        enforce_carried_budgets_protecting(&tx, i64::MAX, DEFAULT_TOTAL_CARRY_BUDGET_BYTES, None)?;
    note_carried_row_eviction(&tx, pressure, 0);
    tx.commit().map_err(store_err)
}

#[cfg(test)]
fn enforce_carried_budgets(
    tx: &Transaction<'_>,
    foreign_budget_bytes: i64,
    total_budget_bytes: i64,
) -> Result<CarriedBudgetEnforcement, CoreError> {
    enforce_carried_budgets_protecting(tx, foreign_budget_bytes, total_budget_bytes, None)
}

fn enforce_carried_budgets_protecting(
    tx: &Transaction<'_>,
    foreign_budget_bytes: i64,
    total_budget_bytes: i64,
    protected_msg_id: Option<&[u8]>,
) -> Result<CarriedBudgetEnforcement, CoreError> {
    let (mut foreign_total, mut total): (i64, i64) = tx
        .query_row(
            "SELECT COALESCE(SUM(CASE WHEN is_family = 0 THEN size_bytes ELSE 0 END), 0),
                    COALESCE(SUM(size_bytes), 0)
             FROM carried_envelopes",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(store_err)?;
    let foreign_budget_bytes = foreign_budget_bytes.max(0);
    let total_budget_bytes = total_budget_bytes.max(0);
    let mut pressure = CarriedBudgetEnforcement::default();
    while foreign_total > foreign_budget_bytes {
        let oldest: Option<(Vec<u8>, i64)> = tx
            .query_row(
                "SELECT msg_id, size_bytes FROM carried_envelopes
                 WHERE is_family = 0 AND (?1 IS NULL OR msg_id != ?1)
                 ORDER BY received_at ASC, msg_id ASC LIMIT 1",
                params![protected_msg_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(store_err)?;
        let Some((msg_id, size)) = oldest else {
            break;
        };
        tx.execute(
            "DELETE FROM carried_envelopes WHERE msg_id = ?1",
            params![msg_id],
        )
        .map_err(store_err)?;
        foreign_total = foreign_total.saturating_sub(size);
        total = total.saturating_sub(size);
        pressure.evicted_rows = pressure.evicted_rows.saturating_add(1);
        pressure.evicted_bytes = pressure.evicted_bytes.saturating_add(size.max(0));
    }

    while total > total_budget_bytes {
        let oldest: Option<(Vec<u8>, i64)> = tx
            .query_row(
                "SELECT msg_id, size_bytes FROM carried_envelopes
                 WHERE is_family = 0 AND (?1 IS NULL OR msg_id != ?1)
                 ORDER BY received_at ASC, msg_id ASC LIMIT 1",
                params![protected_msg_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(store_err)?;
        let Some((msg_id, size)) = oldest else {
            break;
        };
        tx.execute(
            "DELETE FROM carried_envelopes WHERE msg_id = ?1",
            params![msg_id],
        )
        .map_err(store_err)?;
        foreign_total = foreign_total.saturating_sub(size);
        total = total.saturating_sub(size);
        pressure.evicted_rows = pressure.evicted_rows.saturating_add(1);
        pressure.evicted_bytes = pressure.evicted_bytes.saturating_add(size.max(0));
    }
    pressure.total_bytes = total.max(0);
    pressure.within_budget = foreign_total <= foreign_budget_bytes && total <= total_budget_bytes;
    Ok(pressure)
}

// Crate-linked node shells can finish the relay/inbound migration without
// widening the UniFFI surface. CoreRelayPass intentionally persists fetched
// rows first; these bounded queries let the same process subsequently present
// only relay-sourced rows to `process_inbound_frame`. They expose no mutation:
// consumption still goes through the shared inbound authority and an explicit
// `remove_carried_envelope` only after delivery succeeds.
impl MessageStore {
    /// Bounded recent read-model rows for crate-linked presentation shells.
    ///
    /// Kept outside the UniFFI export impl because mobile already owns its
    /// paging surface. Visible rows and reactions have independent limits so
    /// hidden protocol traffic cannot crowd chat history out of the page and
    /// reactions cannot crowd out the messages they annotate. SQL selects
    /// newest-first under each limit, then restores oldest-first order.
    pub fn recent_presentation_messages_for_chat(
        &self,
        chat_id: Vec<u8>,
        visible_limit: u64,
        reaction_limit: u64,
    ) -> Result<Vec<StoredMessage>, CoreError> {
        let conn = lock_conn(&self.conn);
        let visible = visible_chat_kind_sql_list();
        let mut stmt = conn
            .prepare(&format!(
                "WITH recent_visible AS (
                    SELECT id, chat_id, sender_user_id, lamport, timestamp, kind, payload
                    FROM messages
                    WHERE chat_id = ?1 AND kind IN ({visible})
                    ORDER BY timestamp DESC, id DESC
                    LIMIT ?2
                 ), recent_reactions AS (
                    SELECT id, chat_id, sender_user_id, lamport, timestamp, kind, payload
                    FROM messages
                    WHERE chat_id = ?1 AND kind = ?3
                    ORDER BY timestamp DESC, id DESC
                    LIMIT ?4
                 )
                 SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload
                 FROM (
                    SELECT * FROM recent_visible
                    UNION ALL
                    SELECT * FROM recent_reactions
                 )
                 ORDER BY timestamp ASC, id ASC",
            ))
            .map_err(store_err)?;
        let rows = stmt
            .query_map(
                params![
                    chat_id,
                    visible_limit.min(i64::MAX as u64) as i64,
                    crate::KIND_REACTION as i64,
                    reaction_limit.min(i64::MAX as u64) as i64,
                ],
                row_to_message,
            )
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    /// Same presentation page as [`Self::recent_presentation_messages_for_chat`],
    /// optionally older than `before_timestamp_ms`. Desktop uses this to page
    /// without pulling the whole thread; mobile keeps its own surface.
    pub fn presentation_messages_before(
        &self,
        chat_id: Vec<u8>,
        before_timestamp_ms: Option<i64>,
        visible_limit: u64,
        reaction_limit: u64,
    ) -> Result<Vec<StoredMessage>, CoreError> {
        let conn = lock_conn(&self.conn);
        let visible = visible_chat_kind_sql_list();
        let mut stmt = conn
            .prepare(&format!(
                "WITH recent_visible AS (
                    SELECT id, chat_id, sender_user_id, lamport, timestamp, kind, payload
                    FROM messages
                    WHERE chat_id = ?1 AND kind IN ({visible})
                      AND (?5 IS NULL OR timestamp < ?5)
                    ORDER BY timestamp DESC, id DESC
                    LIMIT ?2
                 ), recent_reactions AS (
                    SELECT id, chat_id, sender_user_id, lamport, timestamp, kind, payload
                    FROM messages
                    WHERE chat_id = ?1 AND kind = ?3
                      AND (?5 IS NULL OR timestamp < ?5)
                    ORDER BY timestamp DESC, id DESC
                    LIMIT ?4
                 )
                 SELECT chat_id, sender_user_id, lamport, timestamp, kind, payload
                 FROM (
                    SELECT * FROM recent_visible
                    UNION ALL
                    SELECT * FROM recent_reactions
                 )
                 ORDER BY timestamp ASC, id ASC",
            ))
            .map_err(store_err)?;
        let rows = stmt
            .query_map(
                params![
                    chat_id,
                    visible_limit.min(i64::MAX as u64) as i64,
                    crate::KIND_REACTION as i64,
                    reaction_limit.min(i64::MAX as u64) as i64,
                    before_timestamp_ms,
                ],
                row_to_message,
            )
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    pub fn relay_sourced_carried_envelopes(
        &self,
        limit: u64,
        now_ms: i64,
    ) -> Result<Vec<CarriedEnvelope>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT msg_id, hop_ttl, expiry, recipient_hint, sealed
                 FROM carried_envelopes
                 WHERE from_relay = 1 AND expiry > ?1
                 ORDER BY received_at ASC, msg_id ASC
                 LIMIT ?2",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![now_ms, limit as i64], row_to_carried)
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }

    pub fn non_relay_carried_msg_ids(&self, limit: u64) -> Result<Vec<Vec<u8>>, CoreError> {
        let conn = lock_conn(&self.conn);
        let mut stmt = conn
            .prepare(
                "SELECT msg_id FROM carried_envelopes
                 WHERE from_relay = 0
                 ORDER BY received_at ASC, msg_id ASC
                 LIMIT ?1",
            )
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![limit as i64], |row| row.get(0))
            .map_err(store_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
    }
}

/// Shared by [`MessageStore::highest_contiguous_lamport`] and
/// [`MessageStore::chat_digest`] (which needs it once per sender, under a
/// single lock acquisition -- `Connection`'s `Mutex` isn't reentrant, so
/// `chat_digest` can't just call the `&self` method above for each sender).
fn highest_contiguous_lamport_locked(
    conn: &Connection,
    chat_id: &[u8],
    sender_user_id: &[u8],
) -> Result<u64, CoreError> {
    let mut stmt = conn
        .prepare(
            "SELECT lamport FROM messages
             WHERE chat_id = ?1 AND sender_user_id = ?2
             ORDER BY lamport ASC",
        )
        .map_err(store_err)?;
    let lamports = stmt
        .query_map(params![chat_id, sender_user_id], |row| row.get::<_, i64>(0))
        .map_err(store_err)?;

    let mut expected: u64 = 1;
    for lamport in lamports {
        let lamport = lamport.map_err(store_err)? as u64;
        if lamport != expected {
            break;
        }
        expected += 1;
    }
    Ok(expected - 1)
}

fn row_to_message(row: &rusqlite::Row) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        chat_id: row.get(0)?,
        sender_user_id: row.get(1)?,
        lamport: row.get::<_, i64>(2)? as u64,
        timestamp: row.get(3)?,
        kind: row.get::<_, i64>(4)? as u8,
        payload: row.get(5)?,
    })
}

fn row_to_carried(row: &rusqlite::Row) -> rusqlite::Result<CarriedEnvelope> {
    Ok(CarriedEnvelope {
        msg_id: row.get(0)?,
        hop_ttl: row.get::<_, i64>(1)? as u8,
        expiry: row.get(2)?,
        recipient_hint: row.get(3)?,
        sealed: row.get(4)?,
    })
}

pub(crate) fn row_to_outbound(row: &rusqlite::Row) -> rusqlite::Result<OutboundEnvelope> {
    Ok(OutboundEnvelope {
        msg_id: row.get(0)?,
        recipient_user_id: row.get(1)?,
        chat_id: row.get(2)?,
        sender_user_id: row.get(3)?,
        kind: row.get::<_, i64>(4)? as u8,
        lamport: row.get::<_, i64>(5)? as u64,
        timestamp: row.get(6)?,
        hop_ttl: row.get::<_, i64>(7)? as u8,
        expiry: row.get(8)?,
        recipient_hint: row.get(9)?,
        sealed: row.get(10)?,
    })
}

pub(crate) fn row_to_outgoing_receipt(
    row: &rusqlite::Row,
) -> rusqlite::Result<OutgoingReceiptEnvelope> {
    Ok(OutgoingReceiptEnvelope {
        msg_id: row.get(0)?,
        recipient_user_id: row.get(1)?,
        chat_id: row.get(2)?,
        sender_user_id: row.get(3)?,
        receipt_type: row.get::<_, i64>(4)? as u8,
        through_lamport: row.get::<_, i64>(5)? as u64,
        timestamp: row.get(6)?,
        hop_ttl: row.get::<_, i64>(7)? as u8,
        expiry: row.get(8)?,
        recipient_hint: row.get(9)?,
        sealed: row.get(10)?,
    })
}

fn row_to_contact(row: &rusqlite::Row) -> rusqlite::Result<Contact> {
    Ok(Contact {
        user_id: row.get(0)?,
        name: row.get(1)?,
        sign_pk: row.get(2)?,
        agree_pk: row.get(3)?,
        relay_url: row.get(4)?,
        relay_token: row.get(5)?,
        nickname: row.get(6)?,
    })
}

fn row_to_pending_shared_request(row: &rusqlite::Row) -> rusqlite::Result<PendingSharedRequest> {
    Ok(PendingSharedRequest {
        requester_user_id: row.get(0)?,
        name: row.get(1)?,
        sign_pk: row.get(2)?,
        agree_pk: row.get(3)?,
        relay_url: row.get(4)?,
        relay_token: row.get(5)?,
        sharer_user_id: row.get(6)?,
        expires_at_ms: row.get(7)?,
        first_seen_ms: row.get(8)?,
        last_prompted_ms: row.get(9)?,
    })
}

/// Upsert that keeps redelivery quiet: everything the card carries refreshes,
/// but `first_seen_ms`/`last_prompted_ms` stay put so a duplicate cannot
/// reset the prompt-rate clock.
fn conn_execute_pending_shared_upsert(
    conn: &Connection,
    request: &PendingSharedRequest,
) -> Result<(), CoreError> {
    conn.execute(
        "INSERT INTO pending_shared_requests
            (requester_user_id, name, sign_pk, agree_pk, relay_url, relay_token,
             sharer_user_id, expires_at_ms, first_seen_ms, last_prompted_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0)
         ON CONFLICT(requester_user_id) DO UPDATE SET
            name = excluded.name,
            sign_pk = excluded.sign_pk,
            agree_pk = excluded.agree_pk,
            relay_url = excluded.relay_url,
            relay_token = excluded.relay_token,
            sharer_user_id = excluded.sharer_user_id,
            expires_at_ms = excluded.expires_at_ms",
        params![
            request.requester_user_id,
            request.name,
            request.sign_pk,
            request.agree_pk,
            request.relay_url,
            request.relay_token,
            request.sharer_user_id,
            request.expires_at_ms,
            request.first_seen_ms,
        ],
    )
    .map_err(store_err)?;
    Ok(())
}

// Stored on disk, so these numbers are frozen: only ever append.
fn peer_transport_value(transport: PeerConnectionTransport) -> i64 {
    match transport {
        PeerConnectionTransport::Bluetooth => 0,
        PeerConnectionTransport::LocalWifi => 1,
        PeerConnectionTransport::ShorePass => 2,
        PeerConnectionTransport::Carried => 3,
    }
}

fn peer_transport_from_value(value: i64) -> rusqlite::Result<PeerConnectionTransport> {
    match value {
        0 => Ok(PeerConnectionTransport::Bluetooth),
        1 => Ok(PeerConnectionTransport::LocalWifi),
        2 => Ok(PeerConnectionTransport::ShorePass),
        3 => Ok(PeerConnectionTransport::Carried),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(1, value)),
    }
}

// Stored on disk, so these numbers are frozen: only ever append.
// `record_peer_connection_event`'s summary upsert switches on the same values.
fn peer_event_kind_value(kind: PeerConnectionEventKind) -> i64 {
    match kind {
        PeerConnectionEventKind::Connected => 0,
        PeerConnectionEventKind::Disconnected => 1,
        PeerConnectionEventKind::PresenceSeen => 2,
        PeerConnectionEventKind::MessageDelivered => 3,
        PeerConnectionEventKind::MessageReceived => 4,
    }
}

fn peer_event_kind_from_value(value: i64) -> rusqlite::Result<PeerConnectionEventKind> {
    match value {
        0 => Ok(PeerConnectionEventKind::Connected),
        1 => Ok(PeerConnectionEventKind::Disconnected),
        2 => Ok(PeerConnectionEventKind::PresenceSeen),
        3 => Ok(PeerConnectionEventKind::MessageDelivered),
        4 => Ok(PeerConnectionEventKind::MessageReceived),
        _ => Err(rusqlite::Error::IntegralValueOutOfRange(2, value)),
    }
}

fn row_to_peer_connection_event(row: &rusqlite::Row) -> rusqlite::Result<PeerConnectionEvent> {
    Ok(PeerConnectionEvent {
        user_id: row.get(0)?,
        transport: peer_transport_from_value(row.get(1)?)?,
        kind: peer_event_kind_from_value(row.get(2)?)?,
        occurred_at_ms: row.get(3)?,
    })
}

struct GroupRow {
    id: Vec<u8>,
    name: String,
    key: Vec<u8>,
    metadata_revision: u64,
    metadata_changed_by: Vec<u8>,
}

fn row_to_group_row(row: &rusqlite::Row) -> rusqlite::Result<GroupRow> {
    Ok(GroupRow {
        id: row.get(0)?,
        name: row.get(1)?,
        key: row.get(2)?,
        metadata_revision: row.get(3)?,
        metadata_changed_by: row.get(4)?,
    })
}

fn hydrate_group(conn: &Connection, row: GroupRow) -> Result<Group, CoreError> {
    Ok(Group {
        member_user_ids: load_group_members(conn, &row.id)?,
        id: row.id,
        name: row.name,
        key: row.key,
        metadata_revision: row.metadata_revision,
        metadata_changed_by: row.metadata_changed_by,
    })
}

/// Persist a group and its canonical member set inside an existing transaction.
/// A stale invite (revision zero) must not roll back newer metadata.
pub(crate) fn upsert_group_tx(tx: &Transaction<'_>, group: &Group) -> Result<(), CoreError> {
    validate_group(group)?;
    let current: Option<(u64, Vec<u8>)> = tx
        .query_row(
            "SELECT metadata_revision, metadata_changed_by FROM groups WHERE group_id = ?1",
            params![&group.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(store_err)?;
    if current.as_ref().is_some_and(|(revision, changed_by)| {
        (*revision, changed_by.as_slice())
            > (
                group.metadata_revision,
                group.metadata_changed_by.as_slice(),
            )
    }) {
        return Ok(());
    }

    tx.execute(
        "INSERT INTO groups
            (group_id, name, group_key, metadata_revision, metadata_changed_by)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(group_id) DO UPDATE SET
            name = excluded.name,
            group_key = excluded.group_key,
            metadata_revision = excluded.metadata_revision,
            metadata_changed_by = excluded.metadata_changed_by",
        params![
            &group.id,
            &group.name,
            &group.key,
            group.metadata_revision,
            &group.metadata_changed_by,
        ],
    )
    .map_err(store_err)?;
    let mut previous_added: HashMap<Vec<u8>, i64> = HashMap::new();
    {
        let mut stmt = tx
            .prepare("SELECT user_id, added_at_ms FROM group_members WHERE group_id = ?1")
            .map_err(store_err)?;
        let rows = stmt
            .query_map(params![&group.id], |row| {
                Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(store_err)?;
        for row in rows {
            let (user_id, added_at_ms) = row.map_err(store_err)?;
            previous_added.insert(user_id, added_at_ms);
        }
    }
    tx.execute(
        "DELETE FROM group_members WHERE group_id = ?1",
        params![&group.id],
    )
    .map_err(store_err)?;
    let now_ms = unix_now_ms();
    let founding = previous_added.is_empty();
    for member_user_id in canonicalize_members(group.member_user_ids.clone()) {
        let added_at_ms = previous_added
            .get(&member_user_id)
            .copied()
            .unwrap_or(if founding { 0 } else { now_ms });
        tx.execute(
            "INSERT INTO group_members (group_id, user_id, added_at_ms) VALUES (?1, ?2, ?3)",
            params![&group.id, member_user_id, added_at_ms],
        )
        .map_err(store_err)?;
    }
    Ok(())
}

fn unix_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .unwrap_or(0)
}

/// If `chat_id` is a group, the list-row ticks are the min watermark every
/// other member who already belonged when the last message was sent has
/// reported. Late joiners are left out, matching `core_group_tick_status_for`.
/// 1:1 chats keep the `receipts` values.
fn group_preview_watermarks(
    conn: &Connection,
    chat_id: &[u8],
    own_user_id: &[u8],
    last_message_timestamp: i64,
    pairwise_delivered: i64,
    pairwise_read: i64,
) -> Result<(i64, i64), CoreError> {
    let is_group: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM groups WHERE group_id = ?1",
            params![chat_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_err)?;
    if is_group.is_none() {
        return Ok((pairwise_delivered, pairwise_read));
    }
    let mut stmt = conn
        .prepare("SELECT user_id, added_at_ms FROM group_members WHERE group_id = ?1")
        .map_err(store_err)?;
    let rows = stmt
        .query_map(params![chat_id], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(store_err)?;
    let mut others: Vec<Vec<u8>> = Vec::new();
    for row in rows {
        let (member, added_at_ms) = row.map_err(store_err)?;
        if member.as_slice() == own_user_id {
            continue;
        }
        if added_at_ms > 0 && added_at_ms > last_message_timestamp {
            continue;
        }
        others.push(member);
    }
    if others.is_empty() {
        return Ok((i64::MAX, i64::MAX));
    }
    let mut delivered = i64::MAX;
    let mut read = i64::MAX;
    for member in others {
        let d: i64 = conn
            .query_row(
                "SELECT through_lamport FROM group_receipts
                 WHERE group_id = ?1 AND author_user_id = ?2
                   AND member_user_id = ?3 AND receipt_type = ?4",
                params![chat_id, own_user_id, member, RECEIPT_TYPE_DELIVERED as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?
            .unwrap_or(0);
        let r: i64 = conn
            .query_row(
                "SELECT through_lamport FROM group_receipts
                 WHERE group_id = ?1 AND author_user_id = ?2
                   AND member_user_id = ?3 AND receipt_type = ?4",
                params![chat_id, own_user_id, member, RECEIPT_TYPE_READ as i64],
                |row| row.get(0),
            )
            .optional()
            .map_err(store_err)?
            .unwrap_or(0);
        delivered = delivered.min(d);
        read = read.min(r);
    }
    Ok((delivered, read))
}

fn load_group_members(conn: &Connection, group_id: &[u8]) -> Result<Vec<Vec<u8>>, CoreError> {
    let mut stmt = conn
        .prepare("SELECT user_id FROM group_members WHERE group_id = ?1 ORDER BY user_id ASC")
        .map_err(store_err)?;
    let rows = stmt
        .query_map(params![group_id], |row| row.get::<_, Vec<u8>>(0))
        .map_err(store_err)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(store_err)
}

pub(crate) fn store_err(e: rusqlite::Error) -> CoreError {
    CoreError::Store(e.to_string())
}

fn validate_msg_id(field: &str, msg_id: &[u8]) -> Result<(), CoreError> {
    if msg_id.len() != MESSAGE_ID_LEN {
        return Err(CoreError::Malformed(format!(
            "{field} must be exactly {MESSAGE_ID_LEN} bytes"
        )));
    }
    Ok(())
}

fn validate_stored_message(message: &StoredMessage) -> Result<(), CoreError> {
    validate_sqlite_u64("message lamport", message.lamport)
}

fn validate_receipt_watermark(receipt_type: u8, through_lamport: u64) -> Result<(), CoreError> {
    if receipt_type != crate::RECEIPT_TYPE_DELIVERED && receipt_type != crate::RECEIPT_TYPE_READ {
        return Err(CoreError::Malformed("invalid receipt type".into()));
    }
    validate_sqlite_u64("receipt watermark", through_lamport)
}

fn validate_sqlite_u64(field: &str, value: u64) -> Result<(), CoreError> {
    if value > i64::MAX as u64 {
        return Err(CoreError::Malformed(format!(
            "{field} exceeds the supported range"
        )));
    }
    Ok(())
}

pub(crate) fn outbound_message_dedupe_key(
    chat_id: &[u8],
    sender_user_id: &[u8],
    kind: u8,
    lamport: u64,
    recipient_user_id: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        1 + 2 + chat_id.len() + 2 + sender_user_id.len() + 1 + 8 + 2 + recipient_user_id.len(),
    );
    out.push(1);
    write_bytes16_local(&mut out, chat_id);
    write_bytes16_local(&mut out, sender_user_id);
    out.push(kind);
    out.extend_from_slice(&lamport.to_be_bytes());
    write_bytes16_local(&mut out, recipient_user_id);
    out
}

fn write_bytes16_local(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

fn ensure_contact_column(conn: &Connection, name: &str, column_def: &str) -> Result<(), CoreError> {
    let mut stmt = conn
        .prepare("PRAGMA table_info(contacts)")
        .map_err(store_err)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(store_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_err)?;
    if names.iter().any(|existing| existing == name) {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE contacts ADD COLUMN {name} {column_def}"),
        [],
    )
    .map_err(store_err)?;
    Ok(())
}

/// Generic version of [`ensure_contact_column`] for any table: adds `name`
/// (with `column_def`) to `table` if an older on-disk schema doesn't already
/// have it. Idempotent -- a no-op once the column exists.
fn ensure_column(
    conn: &Connection,
    table: &str,
    name: &str,
    column_def: &str,
) -> Result<(), CoreError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(store_err)?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(store_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_err)?;
    if names.iter().any(|existing| existing == name) {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {name} {column_def}"),
        [],
    )
    .map_err(store_err)?;
    Ok(())
}

/// The chat kinds that produce a row a person can actually see, as a SQL
/// literal list.
///
/// Generated from [`core_is_visible_chat_kind`] rather than written out, so
/// the SQL cannot drift from the predicate the chat screens filter with. That
/// matters more here than it looks: hidden control kinds -- endpoint hints,
/// profile sync, relay-change notices, friend directories, reactions -- share
/// the *same lamport stream* as visible messages and ride the same outbound
/// queue. A count that included them would tell a person "6 messages waiting"
/// for two typed sentences plus four pieces of bookkeeping, and would do it
/// worst on exactly the busiest, most-synced conversations.
fn visible_chat_kind_sql_list() -> String {
    (u8::MIN..=u8::MAX)
        .filter(|kind| core_is_visible_chat_kind(*kind))
        .map(|kind| kind.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The per-recipient waiting-work query behind
/// [`MessageStore::recipient_delivery_status`], built in one place so the
/// query-plan test explains the statement the code actually runs (the lesson
/// of the carried-upload plan test, which once passed against SQL that no
/// longer existed).
///
/// Parameters: `?1` recipient user id, `?2` their delivery receipt watermark,
/// `?3` now, `?4` the sealed-envelope ceiling.
///
/// The `WHERE` clause is the whole performance story. `recipient_user_id = ?1
/// AND chat_id = ?1` is the pairwise conversation with that person (a 1:1 chat
/// is keyed by the other party's user id), and `lamport > ?2` starts the seek
/// immediately above what their receipt already covers. Together they name a
/// contiguous slice of `idx_outbound_recipient_chat_lamport` containing
/// exactly the unacknowledged envelopes -- no history walk, and no growth as
/// the conversation gets longer, since everything confirmed falls below the
/// seek.
///
/// Expiry and kind are applied as residual filters inside the aggregate rather
/// than as index columns: a leading range column would cost the seek, and the
/// rows they filter are already only the unacknowledged ones.
///
/// `MAX(relay_posted_at)` deliberately spans the whole slice including hidden
/// kinds -- posting any envelope for this person is progress on their queue,
/// whether or not it shows up in the chat.
///
/// The un-posted count is the same visible set as the waiting count, narrowed
/// to rows this device has not managed to hand over yet (`relay_posted_at IS
/// NULL`). It is the only way to tell "our queue is stuck" from "we have done
/// our part and they have not collected", and the two must not be confused: a
/// completed upload is the last progress this side can ever record, so
/// measuring a stall against it would put a permanent warning under every
/// friend whose phone is switched off.
///
/// The age comes from `queued_at`, not from the message's own `timestamp`.
/// They differ: an authored message's display timestamp is floored above
/// everything already in the chat, so a peer whose clock runs fast drags our
/// next few timestamps forward with it, and a message could read as newer than
/// the moment it was written. `queued_at` is this device's own record of when
/// the wait began, which is the number "waiting 14 min" is actually claiming.
fn recipient_waiting_sql() -> String {
    let visible = visible_chat_kind_sql_list();
    format!(
        "SELECT
             COALESCE(SUM(CASE WHEN expiry > ?3 AND kind IN ({visible})
                               THEN 1 ELSE 0 END), 0),
             COALESCE(MIN(CASE WHEN expiry > ?3 AND kind IN ({visible})
                               THEN queued_at END), 0),
             COALESCE(MAX(relay_posted_at), 0),
             COALESCE(MAX(CASE WHEN expiry > ?3 AND kind IN ({visible})
                                    AND LENGTH(sealed) > ?4
                               THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN expiry > ?3 AND kind IN ({visible})
                                    AND relay_posted_at IS NULL
                               THEN 1 ELSE 0 END), 0)
         FROM outbound_envelopes
         WHERE recipient_user_id = ?1 AND chat_id = ?1 AND lamport > ?2"
    )
}

/// The relay-upload query for [`MessageStore::family_carried_envelopes`],
/// built in one place so the query-plan test can explain the query the code
/// actually runs. It used to keep its own copy of this SQL, which meant it
/// went on passing against a query that no longer existed.
fn family_carried_upload_sql(skip_clause: &str) -> String {
    format!(
        "SELECT msg_id, hop_ttl, expiry, recipient_hint, sealed
         FROM (
             SELECT c.msg_id, c.hop_ttl, c.expiry, c.recipient_hint, c.sealed,
                    c.received_at,
                    ROW_NUMBER() OVER (
                        PARTITION BY COALESCE(m.recipient_user_id, x'')
                        ORDER BY c.received_at ASC, c.msg_id ASC
                    ) AS recipient_rank
             FROM carried_envelopes c
             LEFT JOIN carried_hint_map m ON m.hint = c.recipient_hint
             WHERE c.is_family = 1 AND c.from_relay = 0
               AND c.relay_uploaded_to IS NULL AND c.expiry > ?{skip_clause}
         )
         ORDER BY recipient_rank ASC, received_at ASC, msg_id ASC
         LIMIT ?"
    )
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS messages (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id        BLOB NOT NULL,
    sender_user_id BLOB NOT NULL,
    lamport        INTEGER NOT NULL,
    timestamp      INTEGER NOT NULL,
    kind           INTEGER NOT NULL,
    payload        BLOB NOT NULL,
    arrival_transport INTEGER,
    hops_taken     INTEGER,
    received_at    INTEGER,
    msg_id         BLOB,
    reply_to_msg_id BLOB,
    outbound_expiry INTEGER,
    UNIQUE(chat_id, sender_user_id, lamport)
);
CREATE INDEX IF NOT EXISTS idx_messages_chat_lamport ON messages(chat_id, lamport);
CREATE INDEX IF NOT EXISTS idx_messages_chat_timestamp_id ON messages(chat_id, timestamp, id);

-- Conflicting authenticated branches cannot be resolved safely without a
-- wire-level stream generation. Keep the visible branch in `messages` and
-- retain each distinct incoming branch here for bounded recovery/diagnostics.
CREATE TABLE IF NOT EXISTS message_conflicts (
    id                       INTEGER PRIMARY KEY AUTOINCREMENT,
    chat_id                  BLOB NOT NULL,
    sender_user_id           BLOB NOT NULL,
    lamport                  INTEGER NOT NULL,
    existing_fingerprint     BLOB NOT NULL,
    incoming_fingerprint     BLOB NOT NULL,
    incoming_timestamp       INTEGER NOT NULL,
    incoming_kind            INTEGER NOT NULL,
    incoming_payload         BLOB NOT NULL,
    incoming_msg_id          BLOB,
    incoming_reply_to_msg_id BLOB,
    arrival_transport        INTEGER,
    hops_taken               INTEGER,
    first_seen_at            INTEGER NOT NULL,
    last_seen_at             INTEGER NOT NULL,
    seen_count               INTEGER NOT NULL DEFAULT 1,
    UNIQUE(chat_id, sender_user_id, lamport, incoming_fingerprint)
);
CREATE INDEX IF NOT EXISTS idx_message_conflicts_recent
    ON message_conflicts(last_seen_at DESC, id DESC);

-- WPT clone guard (`specs/multi-device-v1.md` §13): a second live device
-- presenting this identity, proven by an authenticated Noise static key,
-- is stored here so the shells can surface a safety warning. Stream
-- conflicts are diagnostic only and do not write this table.
CREATE TABLE IF NOT EXISTS identity_clone_warnings (
    user_id       BLOB PRIMARY KEY,
    first_seen_at INTEGER NOT NULL,
    last_seen_at  INTEGER NOT NULL
);

-- The highest lamport this device has ever authored into a chat, kept
-- separately from `messages` so it SURVIVES delete_contact. Deleting a
-- contact clears our copy of a chat, but the peer keeps theirs; if our
-- counter restarted from 1 we would author lamports the peer already holds,
-- and their store would read that as us having forked our stream and delete
-- their history to recover. This table is the one thing that must outlive a
-- delete, and it holds no content -- only how far the counter got.
CREATE TABLE IF NOT EXISTS authored_lamport_watermarks (
    chat_id        BLOB NOT NULL,
    sender_user_id BLOB NOT NULL,
    high_lamport   INTEGER NOT NULL,
    PRIMARY KEY(chat_id, sender_user_id)
);

CREATE TABLE IF NOT EXISTS contacts (
    user_id   BLOB PRIMARY KEY,
    name      TEXT NOT NULL,
    sign_pk   BLOB NOT NULL,
    agree_pk  BLOB NOT NULL,
    relay_url TEXT,
    relay_token TEXT,
    avatar BLOB,
    avatar_epoch INTEGER NOT NULL DEFAULT 0,
    nickname TEXT,
    relay_epoch INTEGER NOT NULL DEFAULT 0,
    relay_reject_streak INTEGER NOT NULL DEFAULT 0,
    relay_rejected_at INTEGER NOT NULL DEFAULT 0,
    relay_unreachable_endpoint_key TEXT,
    relay_unreachable_streak INTEGER NOT NULL DEFAULT 0,
    relay_unreachable_at INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS contact_discovery_policy (
    user_id BLOB PRIMARY KEY,
    protocol_version INTEGER NOT NULL,
    enabled INTEGER NOT NULL,
    revision INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS friend_directory_state (
    introducer_user_id BLOB PRIMARY KEY,
    applied_revision INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS friend_suggestions (
    candidate_user_id BLOB NOT NULL,
    introducer_user_id BLOB NOT NULL,
    name TEXT NOT NULL,
    sign_pk BLOB NOT NULL,
    agree_pk BLOB NOT NULL,
    candidate_policy_revision INTEGER NOT NULL,
    ticket BLOB NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    PRIMARY KEY(candidate_user_id, introducer_user_id)
);
CREATE INDEX IF NOT EXISTS idx_friend_suggestions_introducer
    ON friend_suggestions(introducer_user_id);

CREATE TABLE IF NOT EXISTS friend_suggestion_state (
    candidate_user_id BLOB PRIMARY KEY,
    state INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS blocked_identities (
    user_id BLOB PRIMARY KEY,
    blocked_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS contact_provenance (
    user_id BLOB PRIMARY KEY,
    source INTEGER NOT NULL,
    introducer_user_id BLOB,
    introduced_at_ms INTEGER NOT NULL,
    added_nearby INTEGER NOT NULL DEFAULT 0
);

-- Inbound shared-card friend requests waiting for the user's decision
-- (specs/share-contact.md decision 5): nothing touches contacts until the
-- confirmation is accepted, so the request needs its own place to wait.
CREATE TABLE IF NOT EXISTS pending_shared_requests (
    requester_user_id BLOB PRIMARY KEY,
    name TEXT NOT NULL,
    sign_pk BLOB NOT NULL,
    agree_pk BLOB NOT NULL,
    relay_url TEXT,
    relay_token TEXT,
    sharer_user_id BLOB NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    first_seen_ms INTEGER NOT NULL,
    last_prompted_ms INTEGER NOT NULL DEFAULT 0
);

-- Dismissal bookkeeping for shared-card prompts, kept apart from the pending
-- row so it survives it: Not now deletes the request but the count must
-- persist to offer Do-not-ask-again on the second dismissal.
CREATE TABLE IF NOT EXISTS shared_request_dismissals (
    requester_user_id BLOB PRIMARY KEY,
    count INTEGER NOT NULL DEFAULT 0,
    suppressed INTEGER NOT NULL DEFAULT 0
);

-- The requester's side of a shared-card connection, so the waiting-to-accept
-- copy has a machine behind it. Rows are kept past expiry deliberately:
-- every rejection path drops silently by design, so expiry is the moment the
-- UI switches from waiting to did-not-respond.
CREATE TABLE IF NOT EXISTS outgoing_shared_requests (
    candidate_user_id BLOB PRIMARY KEY,
    expires_at_ms INTEGER NOT NULL,
    sent_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS groups (
    group_id             BLOB PRIMARY KEY,
    name                 TEXT NOT NULL,
    group_key            BLOB NOT NULL,
    metadata_revision    INTEGER NOT NULL DEFAULT 0,
    metadata_changed_by  BLOB NOT NULL DEFAULT X''
);

CREATE TABLE IF NOT EXISTS group_members (
    group_id BLOB NOT NULL,
    user_id  BLOB NOT NULL,
    added_at_ms INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(group_id, user_id)
);
CREATE INDEX IF NOT EXISTS idx_group_members_user_id ON group_members(user_id);

-- D9: per-member delivered/read watermarks for a group's authored streams.
-- Distinct from `receipts` so a group receipt can never land in a 1:1 chat.
CREATE TABLE IF NOT EXISTS group_receipts (
    group_id         BLOB NOT NULL,
    author_user_id   BLOB NOT NULL,
    member_user_id   BLOB NOT NULL,
    receipt_type     INTEGER NOT NULL,
    through_lamport  INTEGER NOT NULL,
    via_transport    INTEGER,
    PRIMARY KEY(group_id, author_user_id, member_user_id, receipt_type)
);

CREATE TABLE IF NOT EXISTS receipts (
    chat_id         BLOB NOT NULL,
    sender_user_id  BLOB NOT NULL,
    receipt_type    INTEGER NOT NULL,
    through_lamport INTEGER NOT NULL,
    via_transport   INTEGER,
    PRIMARY KEY(chat_id, sender_user_id, receipt_type)
);

CREATE TABLE IF NOT EXISTS outgoing_receipts (
    chat_id         BLOB NOT NULL,
    sender_user_id  BLOB NOT NULL,
    receipt_type    INTEGER NOT NULL,
    through_lamport INTEGER NOT NULL,
    PRIMARY KEY(chat_id, sender_user_id, receipt_type)
);

-- V2 field metrics: a local, metadata-only ledger for the cruise test.
-- One row per message we sent or received, keyed by an 8-byte hash of the
-- chat id (never the raw id), an 8-byte hash of the sender (FC1: in a group,
-- every member has an independent lamport stream, so two senders routinely
-- share a lamport value in the same chat -- the sender hash keeps their
-- arrivals from colliding on the primary key), and our/their lamport. No
-- message content is ever stored here. `direction` 0 = we sent it, 1 = we
-- received it.
CREATE TABLE IF NOT EXISTS delivery_metrics (
    chat_hash         BLOB NOT NULL,
    lamport           INTEGER NOT NULL,
    direction         INTEGER NOT NULL,
    sender_hash       BLOB NOT NULL,
    at_ms             INTEGER NOT NULL,
    delivered_at_ms   INTEGER,
    via_transport     INTEGER,
    arrival_transport INTEGER,
    hop_count         INTEGER,
    PRIMARY KEY(chat_hash, lamport, direction, sender_hash)
);

CREATE TABLE IF NOT EXISTS outgoing_receipt_envelopes (
    chat_id           BLOB NOT NULL,
    sender_user_id    BLOB NOT NULL,
    receipt_type      INTEGER NOT NULL,
    through_lamport   INTEGER NOT NULL,
    msg_id            BLOB NOT NULL UNIQUE,
    recipient_user_id BLOB NOT NULL,
    timestamp         INTEGER NOT NULL,
    hop_ttl           INTEGER NOT NULL,
    expiry            INTEGER NOT NULL,
    recipient_hint    BLOB NOT NULL,
    sealed            BLOB NOT NULL,
    queued_at         INTEGER NOT NULL,
    relay_posted_at   INTEGER,
    PRIMARY KEY(chat_id, sender_user_id, receipt_type)
);
CREATE INDEX IF NOT EXISTS idx_outgoing_receipt_envelopes_relay_posted_queued
    ON outgoing_receipt_envelopes(relay_posted_at, queued_at);
CREATE INDEX IF NOT EXISTS idx_outgoing_receipt_envelopes_expiry
    ON outgoing_receipt_envelopes(expiry);

CREATE TABLE IF NOT EXISTS outbound_envelopes (
    dedupe_key        BLOB NOT NULL UNIQUE,
    msg_id            BLOB PRIMARY KEY,
    recipient_user_id BLOB NOT NULL,
    chat_id           BLOB NOT NULL,
    sender_user_id    BLOB NOT NULL,
    kind              INTEGER NOT NULL,
    lamport           INTEGER NOT NULL,
    timestamp         INTEGER NOT NULL,
    hop_ttl           INTEGER NOT NULL,
    expiry            INTEGER NOT NULL,
    recipient_hint    BLOB NOT NULL,
    sealed            BLOB NOT NULL,
    queued_at         INTEGER NOT NULL,
    relay_posted_at   INTEGER
);
CREATE INDEX IF NOT EXISTS idx_outbound_chat_sender_lamport
    ON outbound_envelopes(chat_id, sender_user_id, lamport);
CREATE INDEX IF NOT EXISTS idx_outbound_relay_posted_queued
    ON outbound_envelopes(relay_posted_at, queued_at);
CREATE INDEX IF NOT EXISTS idx_outbound_expiry ON outbound_envelopes(expiry);
-- Supports `recipient_waiting_sql`: the connection details page asks, per
-- friend, what is still outstanding for them, and must answer from a seek
-- rather than a scan. The two equality columns lead so the pairwise
-- conversation with one person is a single index range; `lamport` follows so
-- the range can start immediately above their delivery receipt watermark and
-- skip everything already confirmed. Every column has been in the table since
-- its first version, so this belongs in SCHEMA (replayed on every open) rather
-- than beside the later migrations in `open`.
CREATE INDEX IF NOT EXISTS idx_outbound_recipient_chat_lamport
    ON outbound_envelopes(recipient_user_id, chat_id, lamport);

-- Per-member progress for a group-addressed envelope's relay fan-out. One
-- group envelope is one row per member on the wire, and `relay_posted_at` may
-- only be stamped once all of them land, so a partial failure needs somewhere
-- durable to say which members are already done. Keyed by mailbox as well as
-- by member: already posted there says nothing about a different relay.
-- Gates re-posting only -- never removal, never an ack.
CREATE TABLE IF NOT EXISTS outbound_fanout_posted (
    msg_id         BLOB NOT NULL,
    member_user_id BLOB NOT NULL,
    relay_url      TEXT NOT NULL,
    posted_at      INTEGER NOT NULL,
    PRIMARY KEY(msg_id, member_user_id, relay_url)
);

CREATE TABLE IF NOT EXISTS carried_envelopes (
    msg_id         BLOB PRIMARY KEY,
    hop_ttl        INTEGER NOT NULL,
    expiry         INTEGER NOT NULL,
    recipient_hint BLOB NOT NULL,
    sealed         BLOB NOT NULL,
    is_family      INTEGER NOT NULL,
    received_at    INTEGER NOT NULL,
    size_bytes     INTEGER NOT NULL,
    from_relay     INTEGER NOT NULL DEFAULT 0,
    content_digest BLOB,
    relay_uploaded_to TEXT
);
CREATE INDEX IF NOT EXISTS idx_carried_hint ON carried_envelopes(recipient_hint);
CREATE INDEX IF NOT EXISTS idx_carried_expiry ON carried_envelopes(expiry);
-- Covers both the ORDER BY and the keyset resume predicate of
-- `carried_envelopes_for_peer_sync`: the per-link-session cursor seeks
-- straight to `(received_at, msg_id) > (?, ?)` instead of re-walking the head
-- of a courier's whole backlog on every re-digest. Both columns have been in
-- the table since its first version, so unlike `idx_carried_family_upload`
-- (which needs the later-added `from_relay`) this can live in SCHEMA, which
-- `open` replays on every store, new or existing.
CREATE INDEX IF NOT EXISTS idx_carried_received_at
    ON carried_envelopes(received_at, msg_id);

CREATE TABLE IF NOT EXISTS peer_connection_events (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id        BLOB NOT NULL,
    transport      INTEGER NOT NULL,
    kind           INTEGER NOT NULL,
    occurred_at_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_peer_connection_events_recent
    ON peer_connection_events(occurred_at_ms DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_peer_connection_events_user_recent
    ON peer_connection_events(user_id, occurred_at_ms DESC, id DESC);

-- `last_delivered_at_ms` is OUR message reaching THEM (their receipt came
-- back); `last_received_at_ms` is THEIR visible chat message reaching US.
CREATE TABLE IF NOT EXISTS peer_connection_summary (
    user_id                 BLOB NOT NULL,
    transport               INTEGER NOT NULL,
    last_connected_at_ms    INTEGER,
    last_disconnected_at_ms INTEGER,
    last_seen_at_ms         INTEGER,
    last_delivered_at_ms    INTEGER,
    last_received_at_ms     INTEGER,
    PRIMARY KEY(user_id, transport)
);

-- How far the relay-mailbox walk has got, per mailbox. `config_key` is
-- `relay_cursor_key(url, token)` -- a hash, so no relay credential is stored
-- here and a rotated token simply has no row (which reads as cursor 0). See
-- `crate::relay_cursor` for the policy. These rows ride a `.cmbak`: clearing
-- them makes restore immediately re-walk a shared mailbox and recreate the
-- stale courier backlog restore intentionally discarded. Scheduled sweeps
-- bound the repair delay if a restored frontier is stale.
-- `sweep_after_id` is how far the sweep currently in progress has walked, 0
-- when none is. A walk is bounded per pass, so a sweep of a deep mailbox spans
-- several passes and needs somewhere to resume from; `after_id` cannot serve,
-- because the frontier never moves backwards and so carries no information
-- about a sweep's position. Cleared by completion and by a hint-set change.
-- `sweep_started_at` dates that sweep, from its first fully-processed page. A
-- resume cursor is only meaningful in the id space it was recorded in, and a
-- relay rebuilt from scratch restarts its ids at 1; a sweep stalled across
-- days offline therefore re-walks from 0 rather than trusting the empty page a
-- stale cursor produces. Cleared alongside `sweep_after_id`.
-- `after_id` moves down exactly once in its life, and only through
-- `note_relay_sweep_completed`: a sweep that walked to the natural empty page
-- and found its highest matching row below the remembered frontier has proved
-- that frontier belongs to an id space the relay no longer has. See
-- `crate::relay_frontier_after_completed_sweep`.
CREATE TABLE IF NOT EXISTS relay_fetch_cursors (
    config_key       TEXT PRIMARY KEY,
    after_id         INTEGER NOT NULL DEFAULT 0,
    last_sweep_at    INTEGER NOT NULL DEFAULT 0,
    sweep_after_id   INTEGER NOT NULL DEFAULT 0,
    sweep_started_at INTEGER NOT NULL DEFAULT 0
);

-- One row. The digest of the id set our relay fetch hints derive from (own
-- user id + member groups + contacts), as `relay_hint_source_digest` computes
-- it. Compared once per sync pass so that gaining a contact or a group
-- invalidates the frontiers above, which is the only thing that reaches mail
-- already sitting under a hint we did not have yet. A digest rather than the
-- ids themselves, so this is not a second copy of the contact list.
CREATE TABLE IF NOT EXISTS relay_hint_source_state (
    id     INTEGER PRIMARY KEY CHECK (id = 0),
    digest TEXT NOT NULL
);

-- `msg_id`s this device consumed as the envelope's SOLE true endpoint
-- consumer, for kinds that leave no `messages` row to prove it with (see
-- `core_kind_persists_msg_id_row`). Written only by
-- `MessageStore::core_record_consumed_hidden_msg_id`, which owns the safety
-- rule; read only by `core_relay_ack_ids_with_consumed`, which uses it to ack
-- an already-consumed relay copy instead of re-fetching it until expiry.
--
-- Bounded by construction: `expiry_ms` is the envelope's own expiry (7 days
-- by default, clamped to relayd's 30-day ceiling), and rows are dropped by
-- `prune_expired_consumed_hidden_msg_ids` on the same schedule as every other
-- expiry prune. A hard row cap backstops the one kind an unknown sender can
-- inject (friend requests).
CREATE TABLE IF NOT EXISTS consumed_hidden_msg_ids (
    msg_id    BLOB PRIMARY KEY,
    expiry_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_consumed_hidden_expiry
    ON consumed_hidden_msg_ids(expiry_ms);

-- Exact positions in a pairwise sender stream that this device consumed but
-- did not retain as message rows in that chat. Unlike the relay-ack evidence
-- above, these rows follow chat-history lifetime rather than envelope expiry:
-- a visible message can sit on either side of a control-message lamport for as
-- long as the user keeps the conversation, and forgetting the middle later
-- would resurrect a false messages-still-arriving gap.
CREATE TABLE IF NOT EXISTS consumed_hidden_lamports (
    chat_id       BLOB    NOT NULL,
    sender_user_id BLOB   NOT NULL,
    lamport       INTEGER NOT NULL,
    PRIMARY KEY (chat_id, sender_user_id, lamport)
);
";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        create_introduction_ticket, generate_identity, FriendDirectoryEntry, Group,
        KIND_GROUP_INVITE, RECEIPT_TYPE_DELIVERED, RECEIPT_TYPE_READ,
    };
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    const DEFAULT_HOP_TTL: u8 = 7;

    fn msg(chat: &[u8], sender: &[u8], lamport: u64, text: &str) -> StoredMessage {
        StoredMessage {
            chat_id: chat.to_vec(),
            sender_user_id: sender.to_vec(),
            lamport,
            timestamp: 1_700_000_000_000,
            kind: 1,
            payload: text.as_bytes().to_vec(),
        }
    }

    fn outbound_for(
        message: &StoredMessage,
        recipient_user_id: &[u8],
        msg_id: &[u8],
    ) -> OutboundEnvelope {
        OutboundEnvelope {
            msg_id: msg_id.to_vec(),
            recipient_user_id: recipient_user_id.to_vec(),
            chat_id: message.chat_id.clone(),
            sender_user_id: message.sender_user_id.clone(),
            kind: message.kind,
            lamport: message.lamport,
            timestamp: message.timestamp,
            hop_ttl: DEFAULT_HOP_TTL,
            expiry: message.timestamp + 60_000,
            recipient_hint: b"hint-123".to_vec(),
            sealed: format!("sealed-{}", message.lamport).into_bytes(),
        }
    }

    fn outgoing_receipt_for(
        chat_id: &[u8],
        sender_user_id: &[u8],
        recipient_user_id: &[u8],
        receipt_type: u8,
        through_lamport: u64,
        msg_id: &[u8],
    ) -> OutgoingReceiptEnvelope {
        OutgoingReceiptEnvelope {
            msg_id: msg_id.to_vec(),
            recipient_user_id: recipient_user_id.to_vec(),
            chat_id: chat_id.to_vec(),
            sender_user_id: sender_user_id.to_vec(),
            receipt_type,
            through_lamport,
            timestamp: 1_700_000_000_000,
            hop_ttl: DEFAULT_HOP_TTL,
            expiry: 1_700_000_060_000,
            recipient_hint: b"hint-456".to_vec(),
            sealed: format!("receipt-{receipt_type}-{through_lamport}").into_bytes(),
        }
    }

    fn group(id_byte: u8, name: &str, key_byte: u8, members: &[&[u8]]) -> Group {
        Group {
            id: vec![id_byte; 16],
            name: name.to_string(),
            member_user_ids: members.iter().map(|member| test_user_id(member)).collect(),
            key: vec![key_byte; 32],
            metadata_revision: 0,
            metadata_changed_by: Vec::new(),
        }
    }

    fn test_user_id(label: &[u8]) -> Vec<u8> {
        let mut user_id = vec![0; 16];
        let count = label.len().min(user_id.len());
        user_id[..count].copy_from_slice(&label[..count]);
        user_id
    }

    #[test]
    fn insert_then_fetch_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "there"))
            .unwrap();

        let messages = store.messages_for_chat(b"chat-a".to_vec()).unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].payload, b"hi");
        assert_eq!(messages[1].payload, b"there");
    }

    #[test]
    fn connection_history_coalesces_bounds_and_clears() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let alice = test_user_id(b"alice");
        store
            .record_peer_connection_event(
                alice.clone(),
                PeerConnectionTransport::Bluetooth,
                PeerConnectionEventKind::Connected,
                1_700_000_000_000,
            )
            .unwrap();
        store
            .record_peer_connection_event(
                alice.clone(),
                PeerConnectionTransport::Bluetooth,
                PeerConnectionEventKind::Connected,
                1_700_000_001_000,
            )
            .unwrap();
        store
            .record_peer_connection_event(
                alice.clone(),
                PeerConnectionTransport::ShorePass,
                PeerConnectionEventKind::PresenceSeen,
                1_700_000_002_000,
            )
            .unwrap();

        let events = store.peer_connection_events(None, 50).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, PeerConnectionEventKind::PresenceSeen);
        let summaries = store.peer_connection_summaries().unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(
            summaries
                .iter()
                .find(|summary| summary.transport == PeerConnectionTransport::Bluetooth)
                .unwrap()
                .last_connected_at_ms,
            Some(1_700_000_001_000)
        );

        store.clear_peer_connection_history().unwrap();
        assert!(store.peer_connection_events(None, 50).unwrap().is_empty());
        assert!(store.peer_connection_summaries().unwrap().is_empty());
    }

    /// The two message directions roll up into separate columns. Conflating
    /// them is what made the Connection details screen claim a friend had
    /// messaged us when in truth our own message had merely got through.
    #[test]
    fn connection_summary_keeps_the_two_message_directions_apart() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let alice = test_user_id(b"alice");
        store
            .record_peer_connection_event(
                alice.clone(),
                PeerConnectionTransport::Bluetooth,
                PeerConnectionEventKind::MessageDelivered,
                1_700_000_000_000,
            )
            .unwrap();
        store
            .record_peer_connection_event(
                alice.clone(),
                PeerConnectionTransport::Bluetooth,
                PeerConnectionEventKind::MessageReceived,
                1_700_000_060_000,
            )
            .unwrap();

        let events = store
            .peer_connection_events(Some(alice.clone()), 50)
            .unwrap();
        assert_eq!(events.len(), 2);
        // Both survive the 30s coalescing window: different kinds never merge.
        assert_eq!(events[0].kind, PeerConnectionEventKind::MessageReceived);
        assert_eq!(events[1].kind, PeerConnectionEventKind::MessageDelivered);

        let summaries = store.peer_connection_summaries().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].last_delivered_at_ms, Some(1_700_000_000_000));
        assert_eq!(summaries[0].last_received_at_ms, Some(1_700_000_060_000));
        assert_eq!(summaries[0].last_seen_at_ms, None);
        assert_eq!(summaries[0].last_connected_at_ms, None);
    }

    /// A 1:1 chat with one accepted contact and this device as the author,
    /// laid out the way the receipt path sees it: `chat_id` is the friend,
    /// `sender_user_id` on every row is us.
    fn chat_with_authored_kinds(kinds: &[(u64, u8)]) -> (MessageStore, Vec<u8>, Vec<u8>) {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let friend = test_user_id(b"friend");
        let me = test_user_id(b"me");
        store.upsert_contact(contact(&friend, "Friend")).unwrap();
        for (lamport, kind) in kinds {
            let mut row = msg(&friend, &me, *lamport, "payload");
            row.kind = *kind;
            store.insert_message(row).unwrap();
        }
        (store, friend, me)
    }

    fn delivered_events(store: &MessageStore, friend: &[u8]) -> Vec<PeerConnectionEvent> {
        store
            .peer_connection_events(Some(friend.to_vec()), 50)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == PeerConnectionEventKind::MessageDelivered)
            .collect()
    }

    /// The bug this gate exists for. The app writes profile sync, the friend
    /// directory, LAN endpoint hints and relay-change notices into the same
    /// lamport stream as real messages, and a cumulative delivery receipt
    /// covers them all. Before the gate, a contact whose phone merely acked a
    /// friend-directory blob read as "Received your message yesterday" on the
    /// Connection details screen -- about a conversation nobody had touched in
    /// days.
    #[test]
    fn a_delivered_receipt_covering_only_service_traffic_records_no_delivery() {
        let (store, friend, me) = chat_with_authored_kinds(&[
            (1, crate::KIND_PROFILE_SYNC),
            (2, crate::KIND_FRIEND_DIRECTORY),
            (3, crate::KIND_LAN_ENDPOINT_HINT),
            (4, crate::KIND_RELAY_UPDATE),
            (5, crate::KIND_REACTION),
        ]);

        store
            .record_receipt(
                friend.clone(),
                me,
                RECEIPT_TYPE_DELIVERED,
                5,
                Some(3),
                Some(1_700_000_000_000),
            )
            .unwrap();

        assert!(delivered_events(&store, &friend).is_empty());
        assert!(store.peer_connection_summaries().unwrap().is_empty());
    }

    /// The honest case: a receipt that newly covers something a person wrote
    /// and can see is exactly what "Received your message" means. The event
    /// carries the route the receipt came back on and the moment it arrived.
    #[test]
    fn a_delivered_receipt_covering_a_visible_message_records_the_delivery() {
        let (store, friend, me) =
            chat_with_authored_kinds(&[(1, crate::KIND_PROFILE_SYNC), (2, crate::KIND_TEXT)]);

        store
            .record_receipt(
                friend.clone(),
                me,
                RECEIPT_TYPE_DELIVERED,
                2,
                Some(3), // local Wi-Fi
                Some(1_700_000_000_000),
            )
            .unwrap();

        let events = delivered_events(&store, &friend);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].transport, PeerConnectionTransport::LocalWifi);
        assert_eq!(events[0].occurred_at_ms, 1_700_000_000_000);
        let summaries = store.peer_connection_summaries().unwrap();
        assert_eq!(summaries[0].last_delivered_at_ms, Some(1_700_000_000_000));
    }

    /// Every visible kind counts, and only the visible kinds do -- the same
    /// single predicate the chat screens and the inbound direction use.
    #[test]
    fn the_visible_set_is_the_one_shared_predicate() {
        for kind in 0u8..=32 {
            let (store, friend, me) = chat_with_authored_kinds(&[(1, kind)]);
            store
                .record_receipt(
                    friend.clone(),
                    me,
                    RECEIPT_TYPE_DELIVERED,
                    1,
                    Some(3),
                    Some(1_700_000_000_000),
                )
                .unwrap();
            assert_eq!(
                !delivered_events(&store, &friend).is_empty(),
                crate::core_is_visible_chat_kind(kind),
                "kind {kind} recorded a delivery it should not have, or missed one it should"
            );
        }
    }

    /// A receipt that re-covers lamports already proved delivered proves
    /// nothing new. Under DTN the same receipt is replayed routinely -- over
    /// BLE, off a relay, out of a mule's carry queue -- and each replay used
    /// to stamp a fresh "just now" onto the screen.
    #[test]
    fn a_replayed_delivered_receipt_records_no_second_delivery() {
        let (store, friend, me) = chat_with_authored_kinds(&[(1, crate::KIND_TEXT)]);
        let first = 1_700_000_000_000;
        // Far past the 30s coalescing window, so a second event would show.
        let much_later = first + 3 * 24 * 60 * 60 * 1000;

        for at in [first, much_later] {
            store
                .record_receipt(
                    friend.clone(),
                    me.clone(),
                    RECEIPT_TYPE_DELIVERED,
                    1,
                    Some(3),
                    Some(at),
                )
                .unwrap();
        }

        let events = delivered_events(&store, &friend);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].occurred_at_ms, first);
    }

    /// Restoring a backup replays receipts the restored `receipts` rows
    /// already cover. The watermark cannot advance, so no evidence is
    /// fabricated and the screen keeps saying what it said before the restore.
    #[test]
    fn a_restored_store_replaying_old_receipts_records_no_fresh_delivery() {
        let (store, friend, me) = chat_with_authored_kinds(&[(1, crate::KIND_TEXT)]);
        let long_ago = 1_700_000_000_000;
        store
            .record_receipt(
                friend.clone(),
                me.clone(),
                RECEIPT_TYPE_DELIVERED,
                1,
                Some(3),
                Some(long_ago),
            )
            .unwrap();

        // The restore hands the same watermark back, dated "now".
        let now = long_ago + 30 * 24 * 60 * 60 * 1000;
        store
            .record_receipt(
                friend.clone(),
                me,
                RECEIPT_TYPE_DELIVERED,
                1,
                Some(0),
                Some(now),
            )
            .unwrap();

        let events = delivered_events(&store, &friend);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].occurred_at_ms, long_ago);
        let summaries = store.peer_connection_summaries().unwrap();
        assert_eq!(summaries[0].last_delivered_at_ms, Some(long_ago));
    }

    /// One receipt is one decision, however many messages it covers. A
    /// watermark that jumps over a run of service traffic and two real
    /// messages records a single delivery, not one per row.
    #[test]
    fn a_watermark_advancing_over_a_mix_records_one_delivery() {
        let (store, friend, me) = chat_with_authored_kinds(&[
            (1, crate::KIND_PROFILE_SYNC),
            (2, crate::KIND_TEXT),
            (3, crate::KIND_RELAY_UPDATE),
            (4, crate::KIND_ATTACHMENT_MANIFEST),
        ]);

        store
            .record_receipt(
                friend.clone(),
                me,
                RECEIPT_TYPE_DELIVERED,
                4,
                Some(3),
                Some(1_700_000_000_000),
            )
            .unwrap();

        assert_eq!(delivered_events(&store, &friend).len(), 1);
    }

    /// Only the newly covered span is examined. A visible message already
    /// proved delivered cannot make a later service-only advance claim a
    /// second delivery.
    #[test]
    fn an_advance_over_service_traffic_alone_adds_nothing_after_a_real_delivery() {
        let (store, friend, me) =
            chat_with_authored_kinds(&[(1, crate::KIND_TEXT), (2, crate::KIND_FRIEND_DIRECTORY)]);
        let first = 1_700_000_000_000;
        store
            .record_receipt(
                friend.clone(),
                me.clone(),
                RECEIPT_TYPE_DELIVERED,
                1,
                Some(3),
                Some(first),
            )
            .unwrap();
        store
            .record_receipt(
                friend.clone(),
                me,
                RECEIPT_TYPE_DELIVERED,
                2,
                Some(3),
                Some(first + 3 * 24 * 60 * 60 * 1000),
            )
            .unwrap();

        let events = delivered_events(&store, &friend);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].occurred_at_ms, first);
    }

    /// A read receipt has never recorded connection evidence and still
    /// doesn't; the delivered watermark is the one this screen measures.
    #[test]
    fn a_read_receipt_records_no_delivery_evidence() {
        let (store, friend, me) = chat_with_authored_kinds(&[(1, crate::KIND_TEXT)]);
        store
            .record_receipt(
                friend.clone(),
                me,
                RECEIPT_TYPE_READ,
                1,
                Some(3),
                Some(1_700_000_000_000),
            )
            .unwrap();
        assert!(store
            .peer_connection_events(Some(friend), 50)
            .unwrap()
            .is_empty());
    }

    /// An unknown return route names no path. `Carried` is how this store
    /// spells "evidence, but nothing we observed", and the surfaces drop the
    /// "via ..." clause for it rather than guessing Bluetooth or Wi-Fi.
    #[test]
    fn a_delivered_receipt_with_an_unknown_route_claims_no_path() {
        let (store, friend, me) = chat_with_authored_kinds(&[(1, crate::KIND_TEXT)]);
        store
            .record_receipt(
                friend.clone(),
                me,
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
                Some(1_700_000_000_000),
            )
            .unwrap();

        let events = delivered_events(&store, &friend);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].transport, PeerConnectionTransport::Carried);
        assert!(!crate::core_peer_transport_is_observed(events[0].transport));
    }

    /// No arrival to date the evidence by, no evidence. Every line on the
    /// Connection details screen is a timestamp; a guessed one is worse than
    /// a missing row.
    #[test]
    fn a_delivered_receipt_with_no_arrival_time_records_nothing() {
        let (store, friend, me) = chat_with_authored_kinds(&[(1, crate::KIND_TEXT)]);
        store
            .record_receipt(friend.clone(), me, RECEIPT_TYPE_DELIVERED, 1, Some(3), None)
            .unwrap();
        assert!(delivered_events(&store, &friend).is_empty());
    }

    /// The screen only ever lists friends, so a chat that is not an accepted
    /// contact -- a group id, or someone since removed -- could never show the
    /// row anyway. Same skip the inbound direction makes.
    #[test]
    fn a_delivered_receipt_for_a_chat_that_is_not_a_contact_records_nothing() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let group_id = test_user_id(b"group");
        let me = test_user_id(b"me");
        let mut row = msg(&group_id, &me, 1, "hello everyone");
        row.kind = crate::KIND_TEXT;
        store.insert_message(row).unwrap();

        store
            .record_receipt(
                group_id.clone(),
                me,
                RECEIPT_TYPE_DELIVERED,
                1,
                Some(3),
                Some(1_700_000_000_000),
            )
            .unwrap();

        assert!(delivered_events(&store, &group_id).is_empty());
    }

    /// A receipt only ever acks messages *we* wrote. Their own inbound
    /// messages sit in the same chat under their own sender id and must not
    /// satisfy the gate.
    #[test]
    fn a_receipt_is_not_satisfied_by_the_friends_own_messages() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let friend = test_user_id(b"friend");
        let me = test_user_id(b"me");
        store.upsert_contact(contact(&friend, "Friend")).unwrap();
        store
            .insert_message(msg(&friend, &friend, 1, "hi"))
            .unwrap();

        store
            .record_receipt(
                friend.clone(),
                me,
                RECEIPT_TYPE_DELIVERED,
                1,
                Some(3),
                Some(1_700_000_000_000),
            )
            .unwrap();

        assert!(delivered_events(&store, &friend).is_empty());
    }

    /// Summaries are ordered by the newest evidence of ANY kind. The old
    /// COALESCE-based ordering picked the first non-null column instead, so a
    /// row with stale delivery evidence outranked a row seen seconds ago.
    #[test]
    fn connection_summaries_order_by_newest_evidence_of_any_kind() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let stale = test_user_id(b"stale");
        let fresh = test_user_id(b"fresh");
        store
            .record_peer_connection_event(
                stale.clone(),
                PeerConnectionTransport::Bluetooth,
                PeerConnectionEventKind::MessageDelivered,
                1_700_000_000_000,
            )
            .unwrap();
        store
            .record_peer_connection_event(
                fresh.clone(),
                PeerConnectionTransport::Bluetooth,
                PeerConnectionEventKind::MessageReceived,
                1_700_000_500_000,
            )
            .unwrap();

        let summaries = store.peer_connection_summaries().unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].user_id, fresh);
        assert_eq!(summaries[1].user_id, stale);
    }

    /// A store created before `last_received_at_ms` existed must gain the
    /// column on reopen rather than failing every summary read.
    #[test]
    fn open_migrates_an_old_peer_connection_summary_to_add_last_received() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cruisemesh-peer-summary-{unique}.sqlite"));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE peer_connection_summary (
                user_id                 BLOB NOT NULL,
                transport               INTEGER NOT NULL,
                last_connected_at_ms    INTEGER,
                last_disconnected_at_ms INTEGER,
                last_seen_at_ms         INTEGER,
                last_delivered_at_ms    INTEGER,
                PRIMARY KEY(user_id, transport)
            );
            INSERT INTO peer_connection_summary
                (user_id, transport, last_delivered_at_ms)
                VALUES (X'0102', 0, 1700000000000);
            ",
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str.clone()).unwrap();
        let summaries = store.peer_connection_summaries().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].last_delivered_at_ms, Some(1_700_000_000_000));
        assert_eq!(summaries[0].last_received_at_ms, None);

        store
            .record_peer_connection_event(
                vec![1, 2],
                PeerConnectionTransport::Bluetooth,
                PeerConnectionEventKind::MessageReceived,
                1_700_000_090_000,
            )
            .unwrap();
        let summaries = store.peer_connection_summaries().unwrap();
        assert_eq!(summaries[0].last_received_at_ms, Some(1_700_000_090_000));
        assert_eq!(summaries[0].last_delivered_at_ms, Some(1_700_000_000_000));

        drop(store);
        let _ = fs::remove_file(&path_str);
    }

    #[test]
    fn arrival_transport_maps_onto_the_coarse_connection_path() {
        assert_eq!(
            core_peer_transport_for_arrival(0),
            PeerConnectionTransport::Bluetooth
        );
        assert_eq!(
            core_peer_transport_for_arrival(2),
            PeerConnectionTransport::ShorePass
        );
        assert_eq!(
            core_peer_transport_for_arrival(3),
            PeerConnectionTransport::LocalWifi
        );
    }

    #[test]
    fn a_muled_arrival_claims_no_path() {
        // The hop we saw was between us and the phone in the middle. Reporting
        // it as Bluetooth would tell someone their friend was in range when
        // that friend may be nowhere near -- and for group chat, muling is the
        // ordinary case, not the exception.
        assert_eq!(
            core_peer_transport_for_arrival(1),
            PeerConnectionTransport::Carried,
            "BLE through another device is not Bluetooth to this friend"
        );
        assert_eq!(
            core_peer_transport_for_arrival(4),
            PeerConnectionTransport::Carried,
            "LAN through another device is not local Wi-Fi to this friend"
        );
        assert!(!core_peer_transport_is_observed(
            PeerConnectionTransport::Carried
        ));
        for observed in [
            PeerConnectionTransport::Bluetooth,
            PeerConnectionTransport::LocalWifi,
            PeerConnectionTransport::ShorePass,
        ] {
            assert!(core_peer_transport_is_observed(observed));
        }
    }

    #[test]
    fn a_carried_path_survives_a_round_trip_through_the_database() {
        // The on-disk numbering is append-only; Carried is 3. A row written as
        // Carried must not read back as one of the real paths.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_peer_connection_event(
                vec![7; 16],
                PeerConnectionTransport::Carried,
                PeerConnectionEventKind::MessageReceived,
                1_700_000_000_000,
            )
            .unwrap();
        let summaries = store.peer_connection_summaries().unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].transport, PeerConnectionTransport::Carried);
        assert_eq!(summaries[0].last_received_at_ms, Some(1_700_000_000_000));

        // And it is a distinct row from a genuinely observed Bluetooth hop with
        // the same peer, so one cannot overwrite the other's evidence.
        store
            .record_peer_connection_event(
                vec![7; 16],
                PeerConnectionTransport::Bluetooth,
                PeerConnectionEventKind::MessageReceived,
                1_700_000_050_000,
            )
            .unwrap();
        let summaries = store.peer_connection_summaries().unwrap();
        assert_eq!(summaries.len(), 2);
    }

    #[test]
    fn only_kinds_a_person_sees_in_a_conversation_can_report_an_arrival() {
        // The safety-relevant half of the arrival event: a receipt, a profile
        // sync or a relay update landing must never become "sent you a
        // message". This pins the gate's contract at the kind level -- the
        // shells consult exactly this function before recording.
        // Two judgment calls worth naming rather than discovering later. A
        // group invite counts: it is a line the person sees appear in the
        // conversation, even though nobody typed it. A reaction does not: it
        // renders as a chip on an existing bubble, so reporting it as a message
        // arriving would overstate what happened.
        for visible in [
            crate::KIND_TEXT,
            crate::KIND_ATTACHMENT_MANIFEST,
            crate::KIND_GROUP_INVITE,
        ] {
            assert!(
                crate::core_is_visible_chat_kind(visible),
                "kind {visible} is shown in a conversation and should report an arrival"
            );
        }
        for quiet in [
            crate::KIND_RECEIPT,
            crate::KIND_PROFILE_SYNC,
            crate::KIND_RELAY_UPDATE,
            crate::KIND_FRIEND_DIRECTORY,
            crate::KIND_LAN_ENDPOINT_HINT,
            crate::KIND_REACTION,
        ] {
            assert!(
                !crate::core_is_visible_chat_kind(quiet),
                "kind {quiet} must never say a friend sent a message"
            );
        }
    }

    #[test]
    fn incoming_message_reference_round_trips_and_resolves_quote_target() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let original = msg(b"chat-a", b"alice", 1, "first");
        let original_id = vec![1; MESSAGE_ID_LEN];
        assert!(store
            .insert_incoming_message(original.clone(), original_id.clone(), None)
            .unwrap());

        let reply = msg(b"chat-a", b"alice", 2, "second");
        let reply_id = vec![2; MESSAGE_ID_LEN];
        assert!(store
            .insert_incoming_message(reply.clone(), reply_id.clone(), Some(original_id.clone()),)
            .unwrap());
        assert_eq!(
            store
                .message_reference(
                    reply.chat_id.clone(),
                    reply.sender_user_id.clone(),
                    reply.lamport,
                )
                .unwrap(),
            Some(MessageReference {
                msg_id: reply_id,
                reply_to_msg_id: Some(original_id.clone()),
            }),
        );
        assert_eq!(
            store
                .message_by_msg_id(reply.chat_id.clone(), original_id)
                .unwrap(),
            Some(original),
        );

        // A redundant copy cannot replace the first stable envelope id.
        assert!(!store
            .insert_incoming_message(
                reply.clone(),
                vec![3; MESSAGE_ID_LEN],
                Some(vec![1; MESSAGE_ID_LEN]),
            )
            .unwrap());
        assert_eq!(
            store
                .message_reference(reply.chat_id, reply.sender_user_id, reply.lamport)
                .unwrap()
                .unwrap()
                .msg_id,
            vec![2; MESSAGE_ID_LEN],
        );
    }

    #[test]
    fn message_origin_by_msg_id_finds_a_one_to_one_row_by_msg_id_alone() {
        // Unlike `message_by_msg_id`, no `chat_id` is supplied -- this is the
        // relay ack-decision path, which only ever has the envelope's
        // `msg_id` in hand. A 1:1 incoming row follows the local convention
        // "chat keyed by the other party": chat_id == sender_user_id, which
        // is exactly what the ack-decision helper keys off.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let incoming = msg(b"alice", b"alice", 1, "hi");
        let incoming_id = vec![7; MESSAGE_ID_LEN];
        store
            .insert_incoming_message(incoming.clone(), incoming_id.clone(), None)
            .unwrap();

        assert_eq!(
            store.message_origin_by_msg_id(incoming_id).unwrap(),
            Some(MessageOrigin {
                chat_id: b"alice".to_vec(),
                sender_user_id: b"alice".to_vec(),
            }),
        );
    }

    #[test]
    fn message_origin_by_msg_id_reports_a_group_row_with_its_group_chat_id() {
        // A consumed group message is stored under chat_id = group id with
        // sender_user_id = the authoring member, so chat_id !=
        // sender_user_id. The origin must surface both fields untouched: the
        // ack-decision helper relies on that inequality to refuse to ack a
        // group envelope off the shared family relay mailbox (other members
        // still need the relay copy).
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let group_message = msg(b"group-1", b"alice", 1, "hi group");
        let group_msg_id = vec![10; MESSAGE_ID_LEN];
        store
            .insert_incoming_message(group_message, group_msg_id.clone(), None)
            .unwrap();

        assert_eq!(
            store.message_origin_by_msg_id(group_msg_id).unwrap(),
            Some(MessageOrigin {
                chat_id: b"group-1".to_vec(),
                sender_user_id: b"alice".to_vec(),
            }),
        );
    }

    #[test]
    fn message_origin_by_msg_id_returns_none_for_an_unknown_msg_id() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(
            store
                .message_origin_by_msg_id(vec![0xEE; MESSAGE_ID_LEN])
                .unwrap(),
            None,
        );
    }

    #[test]
    fn message_origin_by_msg_id_returns_our_own_id_for_authored_outbound_messages() {
        // Our own outbound envelope also has a `messages` row (via
        // `insert_outgoing_message`), with `sender_user_id == us`. The store
        // deliberately does NOT filter this out -- it's the caller's job
        // (`engine::consumed_seen_is_ackable`) to compare the returned
        // sender against its own identity and refuse to ack an own-authored
        // envelope, since that relay copy exists for the recipient, not us.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"self", 1, "hello");
        let envelope = outbound_for(&message, b"recipient", &[8; MESSAGE_ID_LEN]);
        store
            .insert_outgoing_message(message, envelope, 1_700_000_000_000)
            .unwrap();

        assert_eq!(
            store
                .message_origin_by_msg_id(vec![8; MESSAGE_ID_LEN])
                .unwrap(),
            Some(MessageOrigin {
                chat_id: b"chat-a".to_vec(),
                sender_user_id: b"self".to_vec(),
            }),
        );
    }

    #[test]
    fn message_origin_by_msg_id_does_not_match_hidden_kind_rows_with_no_msg_id() {
        // Hidden kinds (receipts, profile sync, friend requests/directory,
        // group invites, LAN endpoint hints) are stored via plain
        // `insert_message`, which never records a `msg_id` -- so they must
        // never spuriously match a real envelope's `msg_id` here.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "hidden-kind-payload"))
            .unwrap();

        assert_eq!(
            store
                .message_origin_by_msg_id(vec![9; MESSAGE_ID_LEN])
                .unwrap(),
            None,
        );
    }

    #[test]
    fn outgoing_reply_persists_reference_atomically() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"self", 1, "reply");
        let envelope = outbound_for(&message, b"recipient", &[4; MESSAGE_ID_LEN]);
        let reply_to = vec![5; MESSAGE_ID_LEN];
        assert!(store
            .insert_outgoing_reply(message.clone(), envelope, reply_to.clone(), 1_000)
            .unwrap());
        assert_eq!(
            store
                .message_reference(message.chat_id, message.sender_user_id, message.lamport)
                .unwrap(),
            Some(MessageReference {
                msg_id: vec![4; MESSAGE_ID_LEN],
                reply_to_msg_id: Some(reply_to),
            }),
        );
    }

    #[test]
    fn open_backfills_authored_message_ids_from_the_outbound_queue() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cruisemesh-store-message-reference-backfill-{unique}.sqlite"
        ));
        let path_str = path.to_string_lossy().to_string();
        let message = msg(b"chat-a", b"self", 1, "sent before upgrade");
        let msg_id = vec![6; MESSAGE_ID_LEN];

        let store = MessageStore::open(path_str.clone()).unwrap();
        store
            .insert_outgoing_message(
                message.clone(),
                outbound_for(&message, b"recipient", &msg_id),
                1_000,
            )
            .unwrap();
        drop(store);

        // Model the old schema's message row after the columns are added but
        // before open() performs its outbound-queue backfill.
        let conn = Connection::open(&path_str).unwrap();
        conn.execute("UPDATE messages SET msg_id = NULL", [])
            .unwrap();
        drop(conn);

        let reopened = MessageStore::open(path_str).unwrap();
        assert_eq!(
            reopened
                .message_reference(message.chat_id, message.sender_user_id, message.lamport)
                .unwrap(),
            Some(MessageReference {
                msg_id,
                reply_to_msg_id: None,
            }),
        );

        drop(reopened);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn incoming_references_require_fixed_width_ids() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(matches!(
            store.insert_incoming_message(
                msg(b"chat-a", b"alice", 1, "hi"),
                vec![1; MESSAGE_ID_LEN - 1],
                None,
            ),
            Err(CoreError::Malformed(_))
        ));
    }

    #[test]
    fn message_arrival_records_first_route_without_changing_message_shape() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        store.insert_message(message.clone()).unwrap();
        let first = MessageArrival {
            transport: 1,
            hops_taken: 2,
            received_at: 1_700_000_000_500,
        };
        assert!(store
            .record_message_arrival(
                message.chat_id.clone(),
                message.sender_user_id.clone(),
                message.lamport,
                first.clone(),
            )
            .unwrap());
        assert_eq!(
            store
                .message_arrival(
                    message.chat_id.clone(),
                    message.sender_user_id.clone(),
                    message.lamport,
                )
                .unwrap(),
            Some(first),
        );

        assert!(!store
            .record_message_arrival(
                message.chat_id.clone(),
                message.sender_user_id.clone(),
                message.lamport,
                MessageArrival {
                    transport: 2,
                    hops_taken: 7,
                    received_at: 1_700_000_999_999,
                },
            )
            .unwrap());
        assert_eq!(
            store.messages_for_chat(message.chat_id.clone()).unwrap(),
            vec![message],
        );
    }

    #[test]
    fn chat_preview_returns_last_visible_without_full_history_marshal() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let own = b"me".to_vec();
        let peer = b"alice-id".to_vec();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        store
            .set_contact_avatar(peer.clone(), Some(vec![9, 9, 9]), 1)
            .unwrap();
        // Friend-request noise must not win the preview.
        store
            .insert_message(StoredMessage {
                chat_id: peer.clone(),
                sender_user_id: peer.clone(),
                lamport: 1,
                timestamp: 100,
                kind: crate::KIND_FRIEND_REQUEST,
                payload: b"noise".to_vec(),
            })
            .unwrap();
        store
            .insert_message(StoredMessage {
                chat_id: peer.clone(),
                sender_user_id: peer.clone(),
                lamport: 2,
                timestamp: 200,
                kind: crate::KIND_TEXT,
                payload: b"hello".to_vec(),
            })
            .unwrap();
        store
            .insert_message(StoredMessage {
                chat_id: peer.clone(),
                sender_user_id: peer.clone(),
                lamport: 3,
                timestamp: 300,
                kind: crate::KIND_TEXT,
                payload: b"world".to_vec(),
            })
            .unwrap();
        let preview = store.chat_preview(peer.clone(), own).unwrap();
        assert_eq!(preview.chat_id, peer);
        assert_eq!(preview.last_message.as_ref().map(|m| m.lamport), Some(3));
        assert_eq!(
            preview.last_message.as_ref().map(|m| m.payload.as_slice()),
            Some(b"world".as_slice())
        );
        assert_eq!(preview.unread_count, 2);
        assert_eq!(preview.avatar_bytes, Some(vec![9, 9, 9]));
        assert_eq!(preview.own_delivered_through, 0);
        assert_eq!(preview.own_read_through, 0);
        assert_eq!(store.messages_for_chat(peer).unwrap().len(), 3);
    }

    #[test]
    fn carried_envelopes_for_hints_page_respects_budget_and_does_not_remove_rows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let hint = crate::compute_recipient_hint(b"peer".to_vec(), 0);
        let now = 1_000_i64;
        let foreign_budget = i64::MAX;
        for i in 0..4u8 {
            store
                .enqueue_carried_envelope(
                    CarriedEnvelope {
                        msg_id: vec![i; 16],
                        hop_ttl: 5,
                        expiry: now + 60_000,
                        recipient_hint: hint.clone(),
                        sealed: vec![i; 100],
                    },
                    false,
                    now + i as i64,
                    foreign_budget,
                )
                .unwrap();
        }
        // 250 bytes fits two 100-byte sealed bodies; third would exceed.
        let page = store
            .carried_envelopes_for_hints_page(vec![hint.clone()], now, 250, u32::MAX, None)
            .unwrap();
        assert_eq!(page.rows.len(), 2);
        assert!(!page.exhausted);
        assert!(page.next.is_some());
        // Rows remain in the store (DTN: offer only).
        assert_eq!(
            store
                .carried_envelopes_for_hints(vec![hint.clone()], now)
                .unwrap()
                .len(),
            4
        );
        let page2 = store
            .carried_envelopes_for_hints_page(vec![hint], now, 250, u32::MAX, page.next)
            .unwrap();
        assert_eq!(page2.rows.len(), 2);
        assert!(page2.exhausted);
    }

    #[test]
    fn chat_received_times_covers_arrived_rows_and_skips_legacy_and_own_rows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let arrived = msg(b"chat-a", b"alice", 1, "carried");
        // No arrival ever recorded: a locally authored row, or one stored
        // before diagnostics existed. Both must stay absent rather than
        // reporting a made-up arrival time.
        let legacy = msg(b"chat-a", b"alice", 2, "legacy");
        let own = msg(b"chat-a", b"me", 1, "mine");
        store.insert_message(arrived.clone()).unwrap();
        store.insert_message(legacy).unwrap();
        store.insert_message(own).unwrap();
        store
            .record_message_arrival(
                arrived.chat_id.clone(),
                arrived.sender_user_id.clone(),
                arrived.lamport,
                MessageArrival {
                    transport: 2,
                    hops_taken: 1,
                    received_at: 1_700_000_600_000,
                },
            )
            .unwrap();

        assert_eq!(
            store.chat_received_times(arrived.chat_id.clone()).unwrap(),
            vec![CoreMessageReceivedAt {
                sender_user_id: arrived.sender_user_id.clone(),
                lamport: arrived.lamport,
                received_at_ms: 1_700_000_600_000,
            }],
        );
        // Another chat's arrivals never leak into this one.
        assert!(store
            .chat_received_times(b"chat-b".to_vec())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn message_arrival_accepts_lan_routes_and_rejects_unknown_routes() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        store.insert_message(message.clone()).unwrap();

        assert!(store
            .record_message_arrival(
                message.chat_id.clone(),
                message.sender_user_id.clone(),
                message.lamport,
                MessageArrival {
                    transport: 4,
                    hops_taken: 1,
                    received_at: 1_700_000_000_500,
                },
            )
            .unwrap());

        assert!(matches!(
            store.record_message_arrival(
                message.chat_id,
                message.sender_user_id,
                message.lamport,
                MessageArrival {
                    transport: 5,
                    hops_taken: 0,
                    received_at: 1_700_000_000_600,
                },
            ),
            Err(CoreError::Malformed(_))
        ));
    }

    #[test]
    fn message_arrival_is_absent_for_legacy_or_outgoing_rows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        store.insert_message(message.clone()).unwrap();
        assert_eq!(
            store
                .message_arrival(
                    message.chat_id.clone(),
                    message.sender_user_id.clone(),
                    message.lamport,
                )
                .unwrap(),
            None,
        );
        assert_eq!(
            store
                .message_reference(message.chat_id, message.sender_user_id, message.lamport)
                .unwrap(),
            None,
        );
    }

    #[test]
    fn open_migrates_old_messages_table_to_add_arrival_and_reference_columns() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cruisemesh-store-migration-message-arrival-{unique}.sqlite"
        ));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE messages (
                id             INTEGER PRIMARY KEY AUTOINCREMENT,
                chat_id        BLOB NOT NULL,
                sender_user_id BLOB NOT NULL,
                lamport        INTEGER NOT NULL,
                timestamp      INTEGER NOT NULL,
                kind           INTEGER NOT NULL,
                payload        BLOB NOT NULL,
                UNIQUE(chat_id, sender_user_id, lamport)
            );
            INSERT INTO messages
                (chat_id, sender_user_id, lamport, timestamp, kind, payload)
            VALUES
                (x'636861742D61', x'616C696365', 1, 1700000000000, 0, x'6869');
            ",
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str).unwrap();
        let arrival = MessageArrival {
            transport: 2,
            hops_taken: 3,
            received_at: 1_700_000_000_500,
        };
        assert!(store
            .record_message_arrival(b"chat-a".to_vec(), b"alice".to_vec(), 1, arrival.clone(),)
            .unwrap());
        assert_eq!(
            store
                .message_arrival(b"chat-a".to_vec(), b"alice".to_vec(), 1)
                .unwrap(),
            Some(arrival),
        );

        let legacy = StoredMessage {
            chat_id: b"chat-a".to_vec(),
            sender_user_id: b"alice".to_vec(),
            lamport: 1,
            timestamp: 1_700_000_000_000,
            kind: 0,
            payload: b"hi".to_vec(),
        };
        let legacy_id = vec![9; MESSAGE_ID_LEN];
        assert!(!store
            .insert_incoming_message(legacy, legacy_id.clone(), None)
            .unwrap());
        assert_eq!(
            store
                .message_reference(b"chat-a".to_vec(), b"alice".to_vec(), 1)
                .unwrap(),
            Some(MessageReference {
                msg_id: legacy_id,
                reply_to_msg_id: None,
            }),
        );

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn messages_for_chat_orders_mixed_senders_by_timestamp_not_lamport() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();

        let mut later_lamport_earlier_time = msg(b"chat-a", b"bob", 5, "seems to work");
        later_lamport_earlier_time.timestamp = 100;
        store.insert_message(later_lamport_earlier_time).unwrap();

        let mut later_lamport_later_time = msg(b"chat-a", b"bob", 6, "what about this?");
        later_lamport_later_time.timestamp = 200;
        store.insert_message(later_lamport_later_time).unwrap();

        let mut lower_lamport_latest_time = msg(b"chat-a", b"alice", 4, "rr-test-1");
        lower_lamport_latest_time.timestamp = 300;
        store.insert_message(lower_lamport_latest_time).unwrap();

        let payloads: Vec<Vec<u8>> = store
            .messages_for_chat(b"chat-a".to_vec())
            .unwrap()
            .into_iter()
            .map(|m| m.payload)
            .collect();

        assert_eq!(
            payloads,
            vec![
                b"seems to work".to_vec(),
                b"what about this?".to_vec(),
                b"rr-test-1".to_vec(),
            ]
        );
    }

    #[test]
    fn insert_is_idempotent_on_dedupe_key() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap());
        // Re-delivery of the same envelope (expected under DTN): no-op, not an error.
        assert!(!store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap());
        assert_eq!(
            store.messages_for_chat(b"chat-a".to_vec()).unwrap().len(),
            1
        );
    }

    #[test]
    fn writes_reject_values_that_sqlite_cannot_represent() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(store
            .insert_message(msg(b"chat-a", b"alice", i64::MAX as u64 + 1, "bad"))
            .is_err());
        assert!(store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                i64::MAX as u64 + 1,
                None,
                None
            )
            .is_err());
        assert!(store
            .record_receipt(b"chat-a".to_vec(), b"alice".to_vec(), 0xff, 1, None, None)
            .is_err());
        assert!(store
            .upsert_outgoing_receipt_envelope(
                outgoing_receipt_for(
                    b"chat-a",
                    b"alice",
                    b"bob",
                    RECEIPT_TYPE_DELIVERED,
                    i64::MAX as u64 + 1,
                    &[9; 16],
                ),
                1,
            )
            .is_err());
        assert!(store
            .messages_for_chat(b"chat-a".to_vec())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn insert_message_a_reply_target_mismatch_alone_is_a_merge_not_a_fork() {
        // FC9: the fork discriminator used to include `reply_to_msg_id`,
        // but the plain `insert_message` path (used here) always passes
        // `None` for it. Before the fix, the same logical message arriving
        // once with a reply target set (via `insert_incoming_message`) and
        // once without (via `insert_message`) differed only in
        // `reply_to_msg_id` and got misclassified as a fork -- deleting the
        // tail at and above this lamport and wiping `outgoing_receipts`.
        let store = MessageStore::open(":memory:".to_string()).unwrap();

        // A watermark that the old destructive conflict recovery incorrectly
        // wiped.
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap();
        store
            .record_outgoing_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
                1,
            )
            .unwrap();

        let quoted_id = vec![9; MESSAGE_ID_LEN];
        let same_message = msg(b"chat-a", b"alice", 2, "same content");

        // First arrival: known reply target, via insert_incoming_message.
        let own_id = vec![2; MESSAGE_ID_LEN];
        assert!(store
            .insert_incoming_message(
                same_message.clone(),
                own_id.clone(),
                Some(quoted_id.clone()),
            )
            .unwrap());

        // Second arrival: identical (chat, sender, lamport, timestamp,
        // kind, payload), but through the plain path that always passes
        // `reply_to_msg_id = None`. Must be recognized as the same message
        // (a merge/no-op), not a fork.
        assert!(!store.insert_message(same_message.clone()).unwrap());

        // The row survived (wasn't deleted as a "stale tail"), still has
        // its lamport-1 predecessor, and its reply target/msg_id were
        // reconciled via COALESCE rather than cleared.
        let remaining = store.messages_for_chat(b"chat-a".to_vec()).unwrap();
        assert_eq!(remaining.len(), 2, "no tail deletion should have occurred");
        assert_eq!(
            store
                .message_reference(
                    same_message.chat_id.clone(),
                    same_message.sender_user_id.clone(),
                    same_message.lamport,
                )
                .unwrap(),
            Some(MessageReference {
                msg_id: own_id,
                reply_to_msg_id: Some(quoted_id),
            }),
            "reply target and msg_id must be preserved, not wiped"
        );

        // The unrelated watermark from before must survive too.
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_READ,
                )
                .unwrap(),
            1,
            "a reply-target-only difference must not wipe receipt state"
        );
    }

    #[test]
    fn insert_message_true_duplicate_is_ignored() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(store
            .insert_message(msg(b"chat-a", b"alice", 3, "hi"))
            .unwrap());
        // Same (chat_id, sender_user_id, lamport) *and* same (timestamp,
        // kind, payload) -- a true duplicate (digest resend / relay replay
        // of the identical sealed message), not a fork. No-op, row count
        // unchanged.
        assert!(!store
            .insert_message(msg(b"chat-a", b"alice", 3, "hi"))
            .unwrap());
        assert_eq!(
            store.messages_for_chat(b"chat-a".to_vec()).unwrap().len(),
            1
        );
    }

    #[test]
    fn stale_conflict_cannot_delete_a_newer_delivered_tail() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();

        // Katie's current visible David stream from the field report. Lamport
        // 695 was a hidden/control event, so the visible rows are sparse.
        for (lamport, timestamp, payload) in [
            (694, 10_000, "david-current-694"),
            (696, 30_000, "david-current-696"),
            (697, 40_000, "david-current-697"),
        ] {
            let mut current = msg(b"katie", b"david", lamport, payload);
            current.timestamp = timestamp;
            assert!(store.insert_message(current).unwrap());
        }
        store
            .record_outgoing_receipt(
                b"katie".to_vec(),
                b"david".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                697,
            )
            .unwrap();

        // Restored couriers replay different authenticated David 694s. Test
        // both ordinary stale time and a confused/future clock: neither is a
        // trustworthy branch-generation signal.
        for (timestamp, payload) in [
            (5_000, "older-stale-restored-694"),
            (90_000, "future-clock-stale-restored-694"),
        ] {
            let mut restored = msg(b"katie", b"david", 694, payload);
            restored.timestamp = timestamp;
            assert!(!store.insert_message(restored).unwrap());
        }

        let remaining = store.messages_for_chat(b"katie".to_vec()).unwrap();
        assert_eq!(
            remaining
                .iter()
                .map(|message| (message.lamport, message.payload.as_slice()))
                .collect::<Vec<_>>(),
            vec![
                (694, b"david-current-694".as_slice()),
                (696, b"david-current-696".as_slice()),
                (697, b"david-current-697".as_slice()),
            ],
            "a delayed conflicting envelope must not replace its collision or erase the visible tail"
        );
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"katie".to_vec(),
                    b"david".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED,
                )
                .unwrap(),
            697,
            "ignoring stale evidence must preserve the receipt derived from the retained branch"
        );
    }

    #[test]
    fn arrival_aware_insert_atomically_records_first_route_and_classifies_replay() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat", b"sender", 9, "aboard");
        let first_arrival = MessageArrival {
            transport: 4,
            hops_taken: 3,
            received_at: 1_700_000_000_123,
        };
        assert_eq!(
            store
                .insert_incoming_message_with_arrival(
                    message.clone(),
                    vec![0x11; MESSAGE_ID_LEN],
                    None,
                    first_arrival.clone(),
                )
                .unwrap(),
            IncomingMessageInsertOutcome::Inserted
        );
        assert_eq!(
            store
                .message_arrival(b"chat".to_vec(), b"sender".to_vec(), 9)
                .unwrap(),
            Some(first_arrival.clone())
        );
        let csv = store.export_delivery_metrics_csv().unwrap();
        assert_eq!(csv.lines().count(), 2);
        assert!(csv.contains(",1700000000123,,,,4,3"));

        assert_eq!(
            store
                .insert_incoming_message_with_arrival(
                    message,
                    vec![0x22; MESSAGE_ID_LEN],
                    None,
                    MessageArrival {
                        transport: 0,
                        hops_taken: 1,
                        received_at: first_arrival.received_at + 1_000,
                    },
                )
                .unwrap(),
            IncomingMessageInsertOutcome::Duplicate
        );
        assert_eq!(
            store
                .message_arrival(b"chat".to_vec(), b"sender".to_vec(), 9)
                .unwrap(),
            Some(first_arrival),
            "a replay must not replace first-arrival evidence"
        );
        assert_eq!(
            store.export_delivery_metrics_csv().unwrap().lines().count(),
            2
        );
    }

    #[test]
    fn stream_conflict_is_quarantined_with_redacted_source_diagnostics() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut current = msg(b"katie", b"david", 694, "current-visible");
        current.timestamp = 10_000;
        assert!(store.insert_message(current).unwrap());

        let mut stale = msg(b"katie", b"david", 694, "stale-restored");
        stale.timestamp = 5_000;
        let arrival = MessageArrival {
            transport: 1,
            hops_taken: 2,
            received_at: 50_000,
        };
        assert_eq!(
            store
                .insert_incoming_message_with_arrival(
                    stale.clone(),
                    vec![0xAB; MESSAGE_ID_LEN],
                    None,
                    arrival.clone(),
                )
                .unwrap(),
            IncomingMessageInsertOutcome::QuarantinedConflict
        );
        // The same conflicting branch is one quarantine row with a replay
        // count, not an attacker-controlled unbounded append.
        assert_eq!(
            store
                .insert_incoming_message_with_arrival(
                    stale,
                    vec![0xCD; MESSAGE_ID_LEN],
                    None,
                    MessageArrival {
                        received_at: 60_000,
                        ..arrival
                    },
                )
                .unwrap(),
            IncomingMessageInsertOutcome::QuarantinedConflict
        );

        let conflicts = store.message_conflict_summaries(10).unwrap();
        assert_eq!(conflicts.len(), 1);
        let conflict = &conflicts[0];
        assert_eq!(conflict.lamport, 694);
        assert_eq!(conflict.arrival_transport, Some(1));
        assert_eq!(conflict.first_seen_at_ms, 50_000);
        assert_eq!(conflict.last_seen_at_ms, 60_000);
        assert_eq!(conflict.seen_count, 2);
        assert_eq!(conflict.chat_hash.len(), METRIC_CHAT_HASH_LEN * 2);
        assert_eq!(conflict.sender_hash.len(), METRIC_CHAT_HASH_LEN * 2);
        assert_eq!(
            conflict.existing_fingerprint.len(),
            MESSAGE_CONFLICT_FINGERPRINT_LEN * 2
        );

        let conn = lock_conn(&store.conn);
        let quarantined_payload: Vec<u8> = conn
            .query_row(
                "SELECT incoming_payload FROM message_conflicts",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(quarantined_payload, b"stale-restored");
        drop(conn);
        assert_eq!(
            store.messages_for_chat(b"katie".to_vec()).unwrap()[0].payload,
            b"current-visible"
        );

        let csv = store.export_message_conflicts_csv().unwrap();
        assert_eq!(csv.lines().count(), 2);
        assert!(csv.contains(&conflict.chat_hash));
        assert!(csv.contains(&conflict.sender_hash));
        assert!(csv.contains(",694,"));
        assert!(csv.contains(",1,50000,60000,2"));
        assert!(!csv.contains("katie"));
        assert!(!csv.contains("david"));
        assert!(!csv.contains("current-visible"));
        assert!(!csv.contains("stale-restored"));
        assert!(store.has_message_conflicts().unwrap());
        // A stream conflict is not a clone: a replacement phone that reused
        // lamports after a restore produces the same quarantine.
        assert!(!store.has_identity_clone_warning(b"david".to_vec()).unwrap());
        store.clear_message_conflicts().unwrap();
        assert!(!store.has_message_conflicts().unwrap());
        assert_eq!(
            store
                .export_message_conflicts_csv()
                .unwrap()
                .lines()
                .count(),
            1
        );
        assert_eq!(
            store.messages_for_chat(b"katie".to_vec()).unwrap()[0].payload,
            b"current-visible",
            "clearing diagnostics must not touch the accepted visible branch"
        );
    }

    /// Two live devices restored from the same `.cmbak` author colliding
    /// lamports (`specs/multi-device-v1.md` §1 / WPT). The visible branch is
    /// kept and the other is quarantined; that is not by itself a clone
    /// warning (a replacement phone does the same thing).
    #[test]
    fn two_live_clones_keep_the_visible_branch_instead_of_silent_delete() {
        let bob = MessageStore::open(":memory:".to_string()).unwrap();
        let alice = b"alice-clone";
        let chat = alice; // 1:1 chat id is the sender's user id
        assert!(bob
            .insert_message(msg(chat, alice, 1, "from phone 1"))
            .unwrap());
        let from_phone_2 = msg(chat, alice, 1, "from phone 2");
        assert_eq!(
            bob.insert_incoming_message_classified(from_phone_2, vec![0x11; MESSAGE_ID_LEN], None,)
                .unwrap(),
            IncomingMessageInsertOutcome::QuarantinedConflict
        );
        assert!(!bob.has_identity_clone_warning(alice.to_vec()).unwrap());
        assert_eq!(
            bob.messages_for_chat(chat.to_vec()).unwrap()[0].payload,
            b"from phone 1"
        );

        // Authenticated callers persist a warning without a conflict row.
        // Unauthenticated HELLO must not.
        let alice_phone = MessageStore::open(":memory:".to_string()).unwrap();
        alice_phone
            .record_identity_clone_warning(alice.to_vec(), 50_000)
            .unwrap();
        alice_phone
            .record_identity_clone_warning(alice.to_vec(), 60_000)
            .unwrap();
        assert!(alice_phone
            .has_identity_clone_warning(alice.to_vec())
            .unwrap());
        assert!(!alice_phone
            .has_identity_clone_warning(b"bob".to_vec())
            .unwrap());

        alice_phone
            .clear_identity_clone_warning(alice.to_vec())
            .unwrap();
        assert!(
            !alice_phone
                .has_identity_clone_warning(alice.to_vec())
                .unwrap(),
            "a person who confirms this is their only phone can clear the banner"
        );
        alice_phone
            .record_identity_clone_warning(alice.to_vec(), 65_000)
            .unwrap();
        assert!(
            alice_phone
                .has_identity_clone_warning(alice.to_vec())
                .unwrap(),
            "a later authenticated sighting must surface the warning again"
        );

        alice_phone.upsert_contact(contact(alice, "Alice")).unwrap();
        assert!(alice_phone.delete_contact(alice.to_vec()).unwrap());
        assert!(
            !alice_phone
                .has_identity_clone_warning(alice.to_vec())
                .unwrap(),
            "deleting the contact must clear its clone warning"
        );

        alice_phone
            .record_identity_clone_warning(alice.to_vec(), 70_000)
            .unwrap();
        alice_phone.clear_message_conflicts().unwrap();
        assert!(
            !alice_phone
                .has_identity_clone_warning(alice.to_vec())
                .unwrap(),
            "delete-captured-diagnostics must clear clone warnings"
        );
    }

    #[test]
    fn stream_conflict_quarantine_is_globally_bounded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(store
            .insert_message(msg(b"chat", b"sender", 1, "current"))
            .unwrap());
        for index in 0..(MESSAGE_CONFLICT_QUARANTINE_LIMIT + 5) {
            let mut conflict = msg(
                b"chat",
                b"sender",
                1,
                &format!("conflicting-branch-{index}"),
            );
            conflict.timestamp += index;
            assert!(!store.insert_message(conflict).unwrap());
        }
        assert_eq!(
            store.message_conflict_summaries(1_000).unwrap().len(),
            MESSAGE_CONFLICT_QUARANTINE_LIMIT as usize
        );
        assert!(
            !store
                .has_identity_clone_warning(b"sender".to_vec())
                .unwrap(),
            "a bounded conflict quarantine is not a clone warning"
        );
    }

    #[test]
    fn hidden_conflicts_cannot_evict_envelope_backed_chat_conflicts() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(store
            .insert_message(msg(b"chat", b"sender", 1, "current"))
            .unwrap());

        for index in 0..MESSAGE_CONFLICT_QUARANTINE_LIMIT {
            let mut conflict = msg(
                b"chat",
                b"sender",
                1,
                &format!("chat-conflicting-branch-{index}"),
            );
            conflict.timestamp += index;
            assert_eq!(
                store
                    .insert_incoming_message_classified(
                        conflict,
                        vec![index as u8; MESSAGE_ID_LEN],
                        None,
                    )
                    .unwrap(),
                IncomingMessageInsertOutcome::QuarantinedConflict
            );
        }

        // These plain inserts model hidden/legacy paths with no durable
        // envelope id. The quarantine remains globally capped, but none may
        // displace the chat-path recovery evidence already retained.
        for index in 0..5 {
            let mut hidden = msg(
                b"chat",
                b"sender",
                1,
                &format!("hidden-conflicting-branch-{index}"),
            );
            hidden.timestamp += 10_000 + index;
            assert!(!store.insert_message(hidden).unwrap());
        }

        let conn = lock_conn(&store.conn);
        let (total, envelope_backed): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COUNT(incoming_msg_id) FROM message_conflicts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(total, MESSAGE_CONFLICT_QUARANTINE_LIMIT);
        assert_eq!(envelope_backed, MESSAGE_CONFLICT_QUARANTINE_LIMIT);
    }

    #[test]
    fn insert_message_conflict_preserves_tail_and_outgoing_receipts() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice", "Alice")).unwrap();
        // Alice's original stream: message rows at 1, 2, 3, and 5, plus a
        // consumed control envelope at 4. We'd told her (via
        // outgoing_receipts) that we'd read through the full stream at 5.
        for lamport in [1, 2, 3, 5] {
            store
                .insert_message(msg(b"alice", b"alice", lamport, "old"))
                .unwrap();
        }
        store
            .record_consumed_hidden_lamport(
                b"alice".to_vec(),
                b"alice".to_vec(),
                4,
                crate::KIND_RECEIPT,
            )
            .unwrap();
        store
            .record_outgoing_receipt(
                b"alice".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
                5,
            )
            .unwrap();
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"alice".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_READ,
                )
                .unwrap(),
            5
        );

        // Alice may have reset her stream, or an old restored courier may be
        // replaying another branch. Even a much newer timestamp cannot prove
        // which: phone clocks are not authenticated stream generations.
        let mut conflict = msg(b"alice", b"alice", 3, "new-after-reset");
        conflict.timestamp = 1_700_000_500_000;
        assert!(!store.insert_message(conflict).unwrap());

        // The already-visible branch and exact hidden evidence stay intact.
        let remaining = store.messages_for_chat(b"alice".to_vec()).unwrap();
        assert_eq!(remaining.len(), 4); // 1, 2, 3, and 5
        let three = remaining.iter().find(|m| m.lamport == 3).unwrap();
        assert_eq!(three.payload, b"old");
        assert_eq!(three.timestamp, 1_700_000_000_000);
        assert!(remaining.iter().any(|m| m.lamport == 5));
        assert_eq!(
            store
                .consumed_hidden_lamports(b"alice".to_vec())
                .unwrap()
                .iter()
                .map(|entry| entry.lamport)
                .collect::<Vec<_>>(),
            vec![4],
            "ambiguous conflict must not erase accepted hidden evidence"
        );

        // The messages-only contiguous view stays where it was: row 4 is a
        // separately retained hidden/control event, so this helper stops at 3.
        assert_eq!(
            store
                .highest_contiguous_lamport(b"alice".to_vec(), b"alice".to_vec())
                .unwrap(),
            3
        );

        // A receipt derived from the retained branch must not disappear.
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"alice".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_READ,
                )
                .unwrap(),
            5
        );
    }

    #[test]
    fn insert_message_conflict_does_not_touch_receipts_table() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        for lamport in 1..=5u64 {
            store
                .insert_message(msg(b"chat-a", b"alice", lamport, "old"))
                .unwrap();
        }
        // `receipts` in this chat is the peer's ack of *our own* outgoing
        // stream ("self") -- unrelated to alice's conflicting stream -- and
        // must survive untouched.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"self".to_vec(),
                crate::RECEIPT_TYPE_READ,
                2,
                None,
                None,
            )
            .unwrap();

        let mut conflict = msg(b"chat-a", b"alice", 3, "new-after-reset");
        conflict.timestamp = 1_700_000_500_000;
        assert!(!store.insert_message(conflict).unwrap());

        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"self".to_vec(),
                    crate::RECEIPT_TYPE_READ
                )
                .unwrap(),
            2
        );
    }

    #[test]
    fn messages_are_isolated_per_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"chat-b", b"alice", 1, "yo"))
            .unwrap();

        assert_eq!(
            store.messages_for_chat(b"chat-a".to_vec()).unwrap().len(),
            1
        );
        assert_eq!(
            store.messages_for_chat(b"chat-b".to_vec()).unwrap().len(),
            1
        );
    }

    #[test]
    fn highest_contiguous_lamport_is_zero_with_no_messages() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let n = store
            .highest_contiguous_lamport(b"chat-a".to_vec(), b"alice".to_vec())
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn highest_contiguous_lamport_stops_at_a_gap() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "one"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "two"))
            .unwrap();
        // lamport 3 is missing -- message 4 arrived out of order (DTN reality).
        store
            .insert_message(msg(b"chat-a", b"alice", 4, "four"))
            .unwrap();

        let n = store
            .highest_contiguous_lamport(b"chat-a".to_vec(), b"alice".to_vec())
            .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn highest_contiguous_lamport_is_per_sender_not_per_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "there"))
            .unwrap();
        // Bob's own counter in the same chat starts independently at 1.
        store
            .insert_message(msg(b"chat-a", b"bob", 1, "hey"))
            .unwrap();

        assert_eq!(
            store
                .highest_contiguous_lamport(b"chat-a".to_vec(), b"alice".to_vec())
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .highest_contiguous_lamport(b"chat-a".to_vec(), b"bob".to_vec())
                .unwrap(),
            1
        );
    }

    #[test]
    fn highest_lamport_is_zero_with_no_messages() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let n = store
            .highest_lamport(b"chat-a".to_vec(), b"alice".to_vec())
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn highest_lamport_is_max_across_a_front_gap() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // Lamports 1 and 2 never existed for alice -- e.g. her stream base
        // was ratcheted forward after a chat history wipe -- so her stream
        // legitimately starts at 3. `highest_contiguous_lamport` would
        // report 0 here (nothing contiguous from 1); `highest_lamport`
        // reports what she's actually sent.
        store
            .insert_message(msg(b"chat-a", b"alice", 3, "three"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 4, "four"))
            .unwrap();

        let n = store
            .highest_lamport(b"chat-a".to_vec(), b"alice".to_vec())
            .unwrap();
        assert_eq!(n, 4);
    }

    #[test]
    fn highest_lamport_is_max_across_an_internal_gap() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "one"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "two"))
            .unwrap();
        // lamport 3 is missing -- message 4 arrived out of order (DTN
        // reality) -- but we still hold the max the sender has reached.
        store
            .insert_message(msg(b"chat-a", b"alice", 4, "four"))
            .unwrap();

        let n = store
            .highest_lamport(b"chat-a".to_vec(), b"alice".to_vec())
            .unwrap();
        assert_eq!(n, 4);
    }

    #[test]
    fn highest_lamport_is_per_sender_not_per_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "there"))
            .unwrap();
        // Bob's own counter in the same chat starts independently at 1.
        store
            .insert_message(msg(b"chat-a", b"bob", 1, "hey"))
            .unwrap();

        assert_eq!(
            store
                .highest_lamport(b"chat-a".to_vec(), b"alice".to_vec())
                .unwrap(),
            2
        );
        assert_eq!(
            store
                .highest_lamport(b"chat-a".to_vec(), b"bob".to_vec())
                .unwrap(),
            1
        );
    }

    #[test]
    fn insert_outgoing_message_persists_message_and_queue_entry() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        let outbound = outbound_for(&message, b"bob", b"msg-000000000001");

        assert!(store
            .insert_outgoing_message(message.clone(), outbound.clone(), 1_700_000_000_100)
            .unwrap());

        assert_eq!(
            store.messages_for_chat(b"chat-a".to_vec()).unwrap(),
            vec![message]
        );
        assert_eq!(
            store
                .outbound_envelopes_after(b"chat-a".to_vec(), b"alice".to_vec(), 0)
                .unwrap(),
            vec![outbound],
        );
    }

    #[test]
    fn insert_outgoing_message_is_idempotent_on_logical_message_identity() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        let first = outbound_for(&message, b"bob", b"msg-000000000001");
        let second = outbound_for(&message, b"bob", b"msg-000000000002");

        assert!(store
            .insert_outgoing_message(message.clone(), first.clone(), 1_700_000_000_100)
            .unwrap());
        assert!(!store
            .insert_outgoing_message(message, second, 1_700_000_000_200)
            .unwrap());

        assert_eq!(
            store
                .outbound_envelopes_after(b"chat-a".to_vec(), b"alice".to_vec(), 0)
                .unwrap(),
            vec![first],
        );
    }

    #[test]
    fn insert_outgoing_message_can_backfill_queue_for_existing_message() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = msg(b"chat-a", b"alice", 1, "hi");
        store.insert_message(message.clone()).unwrap();

        assert!(store
            .insert_outgoing_message(
                message.clone(),
                outbound_for(&message, b"bob", b"msg-000000000001"),
                1_700_000_000_100,
            )
            .unwrap());
        assert_eq!(
            store.messages_for_chat(b"chat-a".to_vec()).unwrap(),
            vec![message]
        );
        assert_eq!(
            store
                .outbound_envelopes_after(b"chat-a".to_vec(), b"alice".to_vec(), 0)
                .unwrap()
                .len(),
            1,
        );
    }

    #[test]
    fn outbound_envelopes_after_includes_non_text_authored_kinds() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut friend_request = msg(b"chat-a", b"alice", 1, "{\"name\":\"Alice\"}");
        friend_request.kind = 3;
        let outbound = outbound_for(&friend_request, b"bob", b"msg-000000000003");

        assert!(store
            .insert_outgoing_message(friend_request.clone(), outbound.clone(), 1_700_000_000_100)
            .unwrap());

        assert_eq!(
            store
                .outbound_envelopes_after(b"chat-a".to_vec(), b"alice".to_vec(), 0)
                .unwrap(),
            vec![outbound],
        );
    }

    #[test]
    fn group_invites_can_queue_one_pairwise_envelope_per_recipient() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut invite = msg(&[0x11; 16], b"alice", 7, "group invite");
        invite.kind = KIND_GROUP_INVITE;
        let first = outbound_for(&invite, b"bob", b"msg-000000000011");
        let second = outbound_for(&invite, b"carol", b"msg-000000000012");

        assert!(store
            .insert_outgoing_message(invite.clone(), first.clone(), 1_700_000_000_100)
            .unwrap());
        assert!(store
            .insert_outgoing_message(invite.clone(), second.clone(), 1_700_000_000_200)
            .unwrap());

        assert_eq!(
            store.messages_for_chat(invite.chat_id.clone()).unwrap(),
            vec![invite]
        );
        let mut recipients: Vec<Vec<u8>> = store
            .outbound_envelopes_after(vec![0x11; 16], b"alice".to_vec(), 0)
            .unwrap()
            .into_iter()
            .map(|envelope| envelope.recipient_user_id)
            .collect();
        recipients.sort();
        assert_eq!(recipients, vec![b"bob".to_vec(), b"carol".to_vec()]);
    }

    /// The peer's side of the field failure: a sender whose lamport counter
    /// restarted re-authors over lamports the peer already holds. Without an
    /// authenticated stream generation there is no safe way to distinguish
    /// that reset from a stale restored-courier replay, so the peer preserves
    /// the branch it has already rendered.
    #[test]
    fn a_sender_conflict_never_destroys_the_peers_history() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        for i in 1..=5u64 {
            let mut old = msg(b"david", b"david", i, "old");
            old.payload = format!("old-{i}").into_bytes();
            assert!(store.insert_message(old).unwrap());
        }
        assert_eq!(store.messages_for_chat(b"david".to_vec()).unwrap().len(), 5);

        // Same lamport, different content, and even a later timestamp remain
        // ambiguous rather than authorizing destructive recovery.
        let mut restarted = msg(b"david", b"david", 1, "after a delete");
        restarted.timestamp += 999;
        assert!(!store.insert_message(restarted).unwrap());

        let rows = store.messages_for_chat(b"david".to_vec()).unwrap();
        assert_eq!(rows.len(), 5, "the peer's visible history must survive");
        assert_eq!(rows[0].payload, b"old-1".to_vec());
        assert_eq!(rows[4].payload, b"old-5".to_vec());
    }

    #[test]
    fn deleting_a_contact_does_not_rewind_the_authored_lamport_counter() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let identity = crate::generate_identity();
        let peer = crate::generate_identity();
        let contact = Contact {
            user_id: peer.user_id.clone(),
            name: "Peer".to_string(),
            sign_pk: peer.sign_pk.clone(),
            agree_pk: peer.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        };

        store.upsert_contact(contact.clone()).unwrap();
        let mut last = 0;
        for i in 0..3 {
            let authored = store
                .author_pairwise_message(
                    identity.clone(),
                    contact.clone(),
                    1,
                    format!("hello {i}").into_bytes(),
                    None,
                    1_700_000_000_000 + i,
                )
                .unwrap();
            last = authored.envelope.lamport;
        }
        assert_eq!(last, 3);

        assert!(store.delete_contact(contact.user_id.clone()).unwrap());
        assert!(store
            .messages_for_chat(contact.user_id.clone())
            .unwrap()
            .is_empty());

        // Re-add and send again. The counter must continue past the peer's
        // retained high-water mark, not restart at 1 -- restarting is what
        // makes the peer delete their own history to "recover" from the
        // apparent fork.
        store.upsert_contact(contact.clone()).unwrap();
        let after = store
            .author_pairwise_message(
                identity,
                contact,
                1,
                b"after the delete".to_vec(),
                None,
                1_700_000_100_000,
            )
            .unwrap();
        assert_eq!(
            after.envelope.lamport, 4,
            "the counter must not be reused against a peer that still holds the old stream",
        );
    }

    #[test]
    fn pending_relay_outbound_envelopes_skip_posted_and_expired_rows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let live = msg(b"chat-a", b"alice", 1, "live");
        let stale = msg(b"chat-a", b"alice", 2, "stale");
        let posted = msg(b"chat-a", b"alice", 3, "posted");

        let mut live_env = outbound_for(&live, b"bob", b"msg-000000000001");
        live_env.expiry = 10_000;
        let mut stale_env = outbound_for(&stale, b"bob", b"msg-000000000002");
        stale_env.expiry = 1_999;
        let mut posted_env = outbound_for(&posted, b"bob", b"msg-000000000003");
        posted_env.expiry = 10_000;

        store
            .insert_outgoing_message(live, live_env.clone(), 1_000)
            .unwrap();
        store
            .insert_outgoing_message(stale, stale_env, 1_100)
            .unwrap();
        store
            .insert_outgoing_message(posted, posted_env.clone(), 1_200)
            .unwrap();
        assert!(store
            .mark_outbound_envelope_relay_posted(posted_env.msg_id.clone(), 1_500)
            .unwrap());

        assert_eq!(
            store
                .pending_relay_outbound_envelopes(10, 2_000, vec![])
                .unwrap(),
            vec![live_env],
        );
    }

    /// Queue `count` envelopes to `recipient`, all unexpired, queued in the
    /// given `queued_at` order so the flat-order behaviour is unambiguous.
    fn queue_outbound_for(
        store: &MessageStore,
        recipient: &[u8],
        chat: &[u8],
        count: u64,
        first_queued_at: i64,
    ) -> Vec<Vec<u8>> {
        let mut ids = Vec::new();
        for i in 0..count {
            let message = msg(chat, b"alice", i + 1, "body");
            let msg_id = format!("msg-{}-{:08}", String::from_utf8_lossy(recipient), i);
            let mut envelope = outbound_for(&message, recipient, msg_id.as_bytes());
            envelope.expiry = 10_000_000;
            ids.push(envelope.msg_id.clone());
            store
                .insert_outgoing_message(message, envelope, first_queued_at + i as i64)
                .unwrap();
        }
        ids
    }

    #[test]
    fn one_recipients_backlog_cannot_consume_the_whole_relay_batch() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // Bob's relay is unreachable, so his rows never clear and they were
        // queued first -- exactly the field case where every other
        // conversation went dark for three days.
        queue_outbound_for(&store, b"bob", b"chat-bob", 200, 1_000);
        queue_outbound_for(&store, b"carol", b"chat-carol", 3, 900_000);

        let batch = store
            .pending_relay_outbound_envelopes(128, 2_000, vec![])
            .unwrap();
        assert_eq!(batch.len(), 128);
        let carol_rows = batch
            .iter()
            .filter(|e| e.recipient_user_id == b"carol".to_vec())
            .count();
        // Flat queue order would have given Carol zero of the 128 slots even
        // though her messages are newer: Bob's backlog fills the window.
        assert_eq!(carol_rows, 3, "every newer recipient's row must get a slot");
    }

    #[test]
    fn a_single_recipient_still_gets_the_whole_batch_in_queue_order() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let ids = queue_outbound_for(&store, b"bob", b"chat-bob", 200, 1_000);

        let batch = store
            .pending_relay_outbound_envelopes(128, 2_000, vec![])
            .unwrap();
        // Fairness must bind only under contention: uncontended throughput and
        // ordering are unchanged from the flat query.
        assert_eq!(batch.len(), 128);
        assert_eq!(
            batch.iter().map(|e| e.msg_id.clone()).collect::<Vec<_>>(),
            ids[..128].to_vec(),
        );
    }

    #[test]
    fn skipped_recipients_do_not_consume_relay_batch_slots() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        queue_outbound_for(&store, b"bob", b"chat-bob", 200, 1_000);
        queue_outbound_for(&store, b"carol", b"chat-carol", 40, 900_000);

        let batch = store
            .pending_relay_outbound_envelopes(128, 2_000, vec![b"bob".to_vec()])
            .unwrap();
        // Bob is known-unpostable this pass, so none of his rows are fetched
        // at all -- Carol's whole backlog gets the window.
        assert_eq!(batch.len(), 40);
        assert!(batch
            .iter()
            .all(|e| e.recipient_user_id == b"carol".to_vec()));
    }

    #[test]
    fn skipping_a_recipient_leaves_its_queue_intact_for_a_later_pass() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let bob_ids = queue_outbound_for(&store, b"bob", b"chat-bob", 5, 1_000);

        assert!(store
            .pending_relay_outbound_envelopes(128, 2_000, vec![b"bob".to_vec()])
            .unwrap()
            .is_empty());
        // Skipping is not a terminal state: once his relay resolves again the
        // same rows are offered, unchanged.
        assert_eq!(
            store
                .pending_relay_outbound_envelopes(128, 2_000, vec![])
                .unwrap()
                .iter()
                .map(|e| e.msg_id.clone())
                .collect::<Vec<_>>(),
            bob_ids,
        );
    }

    /// The receipt queue starves the same way the outbound one does, and in
    /// the field capture it was the queue visibly failing.
    ///
    /// A receipt row is a watermark per (chat, sender, type), so a single 1:1
    /// contact only ever holds a couple of rows -- one recipient builds a deep
    /// backlog by being a member of many group chats, which is what this
    /// queues.
    #[test]
    fn one_recipients_receipt_backlog_cannot_consume_the_whole_relay_batch() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        for i in 0..20u8 {
            let envelope = outgoing_receipt_for(
                &[0xB0 + i],
                b"sender",
                b"bob",
                crate::RECEIPT_TYPE_DELIVERED,
                i as u64 + 1,
                format!("receipt-bob-{i:08}").as_bytes(),
            );
            store
                .upsert_outgoing_receipt_envelope(envelope, 1_000 + i as i64)
                .unwrap();
        }
        let carol = outgoing_receipt_for(
            b"chat-carol",
            b"sender",
            b"carol",
            crate::RECEIPT_TYPE_DELIVERED,
            1,
            b"receipt-carol-01",
        );
        store
            .upsert_outgoing_receipt_envelope(carol.clone(), 900_000)
            .unwrap();

        // Flat queue order gives Carol's newer receipt none of the first four
        // slots; Bob queued first and holds twenty.
        let batch = store
            .pending_relay_outgoing_receipt_envelopes(4, 2_000, vec![])
            .unwrap();
        assert_eq!(batch.len(), 4);
        assert_eq!(
            batch
                .iter()
                .filter(|e| e.recipient_user_id == b"carol".to_vec())
                .count(),
            1,
            "every newer recipient's receipt must get a slot",
        );

        // And a known-unpostable recipient consumes no slots at all.
        let skipped = store
            .pending_relay_outgoing_receipt_envelopes(128, 2_000, vec![b"bob".to_vec()])
            .unwrap();
        assert_eq!(skipped, vec![carol]);
    }

    #[test]
    fn relay_queue_depth_reports_the_backlog_per_recipient() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        queue_outbound_for(&store, b"bob", b"chat-bob", 7, 1_000);
        queue_outbound_for(&store, b"carol", b"chat-carol", 2, 900_000);

        assert_eq!(
            store
                .pending_relay_outbound_depth_by_recipient(2_000)
                .unwrap(),
            vec![
                RelayQueueDepth {
                    recipient_user_id: b"bob".to_vec(),
                    queued: 7,
                },
                RelayQueueDepth {
                    recipient_user_id: b"carol".to_vec(),
                    queued: 2,
                },
            ],
        );
    }

    // --- per-recipient delivery read model ---------------------------------

    const ALICE: &[u8] = b"alice";
    const BOB: &[u8] = b"bob";
    const CAROL: &[u8] = b"carol";
    const DELIVERY_NOW: i64 = 2_000_000;

    /// Queue one outbound envelope in the pairwise conversation with
    /// `recipient` (1:1 chats are keyed by the other party's user id).
    fn queue_pairwise(
        store: &MessageStore,
        recipient: &[u8],
        lamport: u64,
        kind: u8,
        timestamp: i64,
        expiry: i64,
        sealed_len: usize,
    ) -> Vec<u8> {
        let message = StoredMessage {
            chat_id: recipient.to_vec(),
            sender_user_id: ALICE.to_vec(),
            lamport,
            timestamp,
            kind,
            payload: b"body".to_vec(),
        };
        let msg_id = format!("out-{}-{lamport}", String::from_utf8_lossy(recipient)).into_bytes();
        let mut envelope = outbound_for(&message, recipient, &msg_id);
        envelope.expiry = expiry;
        envelope.sealed = vec![7u8; sealed_len];
        store
            .insert_outgoing_message(message, envelope, timestamp)
            .unwrap();
        msg_id
    }

    #[test]
    fn the_waiting_age_is_when_we_queued_it_not_when_it_claims_to_be_from() {
        // Causal ordering floors an authored timestamp above everything
        // already in the chat, so a peer with a fast clock drags ours forward
        // with it. Dating the wait from the displayed timestamp would let a
        // message stuck for an hour read as newer than the moment it was
        // written -- and, at a display timestamp beyond `now`, would suppress
        // the delayed line entirely.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let message = StoredMessage {
            chat_id: BOB.to_vec(),
            sender_user_id: ALICE.to_vec(),
            lamport: 1,
            timestamp: DELIVERY_NOW + 3_600_000,
            kind: crate::KIND_TEXT,
            payload: b"body".to_vec(),
        };
        let mut envelope = outbound_for(&message, BOB, b"out-bob-skewed");
        envelope.expiry = DELIVERY_NOW + 60_000;
        store
            .insert_outgoing_message(message, envelope, 1_000_000)
            .unwrap();

        let bob = delivery_status(&store, BOB);
        assert_eq!(bob.waiting_count, 1);
        assert_eq!(bob.oldest_waiting_ms, 1_000_000);
    }

    fn delivery_status(store: &MessageStore, recipient: &[u8]) -> CoreRecipientDeliveryStatus {
        store
            .recipient_delivery_status(ALICE.to_vec(), vec![recipient.to_vec()], DELIVERY_NOW)
            .unwrap()
            .pop()
            .expect("a status row per unblocked recipient")
    }

    #[test]
    fn recipient_delivery_counts_only_unacknowledged_visible_pairwise_messages() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let live = DELIVERY_NOW + 60_000;
        queue_pairwise(&store, BOB, 1, crate::KIND_TEXT, 1_000_000, live, 32);
        // A hidden control kind sharing the same lamport stream. Counting it
        // would tell a person two sentences were four messages.
        queue_pairwise(
            &store,
            BOB,
            2,
            crate::KIND_PROFILE_SYNC,
            1_050_000,
            live,
            32,
        );
        queue_pairwise(&store, BOB, 3, crate::KIND_TEXT, 1_100_000, live, 32);
        // Past its expiry: no path will carry it again, so it is not waiting
        // work any more.
        queue_pairwise(
            &store,
            BOB,
            4,
            crate::KIND_TEXT,
            1_150_000,
            DELIVERY_NOW - 1,
            32,
        );
        // Another conversation entirely.
        queue_pairwise(&store, CAROL, 1, crate::KIND_TEXT, 1_200_000, live, 32);

        let bob = delivery_status(&store, BOB);
        assert_eq!(bob.waiting_count, 2);
        assert_eq!(bob.oldest_waiting_ms, 1_000_000);
        assert_eq!(delivery_status(&store, CAROL).waiting_count, 1);

        // Their receipt covers the first message: it stops being counted, and
        // the age moves to what is genuinely still outstanding.
        store
            .record_receipt(
                BOB.to_vec(),
                ALICE.to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                1,
                None,
                None,
            )
            .unwrap();
        let bob = delivery_status(&store, BOB);
        assert_eq!(bob.waiting_count, 1);
        assert_eq!(bob.oldest_waiting_ms, 1_100_000);

        // An over-reported watermark (the receipt-repair lane reports a peer
        // stream MAX) empties the range rather than going negative.
        store
            .record_receipt(
                BOB.to_vec(),
                ALICE.to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9_999,
                None,
                None,
            )
            .unwrap();
        let bob = delivery_status(&store, BOB);
        assert_eq!(bob.waiting_count, 0);
        assert_eq!(bob.oldest_waiting_ms, 0);
    }

    #[test]
    fn hidden_kinds_alone_are_never_waiting_messages() {
        // Endpoint hints and relay-change notices fly between two phones that
        // have said nothing to each other in weeks. That must read as an empty
        // conversation, not as a backlog.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let live = DELIVERY_NOW + 60_000;
        for (i, kind) in [
            crate::KIND_LAN_ENDPOINT_HINT,
            crate::KIND_PROFILE_SYNC,
            crate::KIND_RELAY_UPDATE,
            crate::KIND_REACTION,
            crate::KIND_FRIEND_DIRECTORY,
        ]
        .into_iter()
        .enumerate()
        {
            queue_pairwise(&store, BOB, i as u64 + 1, kind, 1_000_000, live, 32);
        }
        let bob = delivery_status(&store, BOB);
        assert_eq!(bob.waiting_count, 0);
        assert_eq!(bob.oldest_waiting_ms, 0);
    }

    #[test]
    fn group_mail_is_never_attributed_to_a_member() {
        // A group envelope is queued once against the group id, and no group
        // receipt exists to clear it. Counting it under each member would put
        // a permanent waiting line beneath everyone in the chat.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let group = b"group-vacation";
        let message = StoredMessage {
            chat_id: group.to_vec(),
            sender_user_id: ALICE.to_vec(),
            lamport: 1,
            timestamp: 1_000_000,
            kind: crate::KIND_TEXT,
            payload: b"body".to_vec(),
        };
        let mut envelope = outbound_for(&message, group, b"out-group-1");
        envelope.expiry = DELIVERY_NOW + 60_000;
        store
            .insert_outgoing_message(message, envelope, 1_000_000)
            .unwrap();

        assert_eq!(delivery_status(&store, BOB).waiting_count, 0);
    }

    #[test]
    fn blocked_identities_get_no_delivery_row_at_all() {
        // A block is a tombstone, and the filter lives in the query so no
        // caller can forget it.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        queue_pairwise(
            &store,
            BOB,
            1,
            crate::KIND_TEXT,
            1_000_000,
            DELIVERY_NOW + 60_000,
            32,
        );
        store.block_user(BOB.to_vec(), 1_500_000).unwrap();

        let rows = store
            .recipient_delivery_status(
                ALICE.to_vec(),
                vec![BOB.to_vec(), CAROL.to_vec()],
                DELIVERY_NOW,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].recipient_user_id, CAROL.to_vec());
    }

    #[test]
    fn last_progress_takes_the_newer_of_an_upload_and_a_confirmation() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let live = DELIVERY_NOW + 60_000;
        let first = queue_pairwise(&store, BOB, 1, crate::KIND_TEXT, 1_000_000, live, 32);
        queue_pairwise(&store, BOB, 2, crate::KIND_TEXT, 1_100_000, live, 32);

        // Nothing has moved yet.
        assert_eq!(delivery_status(&store, BOB).last_progress_ms, 0);

        // An accepted upload is progress.
        store
            .mark_outbound_envelope_relay_posted(first, 1_400_000)
            .unwrap();
        assert_eq!(delivery_status(&store, BOB).last_progress_ms, 1_400_000);

        // So is a confirmation coming back, on whichever transport carried it
        // -- which is the only durable mark a Bluetooth hand-off leaves.
        store
            .record_peer_connection_event(
                BOB.to_vec(),
                PeerConnectionTransport::Bluetooth,
                PeerConnectionEventKind::MessageDelivered,
                1_700_000,
            )
            .unwrap();
        assert_eq!(delivery_status(&store, BOB).last_progress_ms, 1_700_000);

        // An older confirmation on another path never drags it backwards.
        store
            .record_peer_connection_event(
                BOB.to_vec(),
                PeerConnectionTransport::ShorePass,
                PeerConnectionEventKind::MessageDelivered,
                1_200_000,
            )
            .unwrap();
        assert_eq!(delivery_status(&store, BOB).last_progress_ms, 1_700_000);

        // And merely queueing more work is not progress: a person typing into
        // a stuck conversation must not reset the delay clock.
        queue_pairwise(&store, BOB, 3, crate::KIND_TEXT, 1_900_000, live, 32);
        assert_eq!(delivery_status(&store, BOB).last_progress_ms, 1_700_000);
    }

    #[test]
    fn work_this_device_has_already_handed_over_stops_counting_as_ours() {
        // The difference between "our queue is stuck" and "we have done our
        // part and they have not collected yet". Both leave `waiting_count`
        // where it is -- only their receipt clears that -- so without this
        // column there is no way to tell a stalled upload from a friend whose
        // phone is switched off, and the page would warn about both.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let live = DELIVERY_NOW + 60_000;
        let first = queue_pairwise(&store, BOB, 1, crate::KIND_TEXT, 1_000_000, live, 32);
        let second = queue_pairwise(&store, BOB, 2, crate::KIND_TEXT, 1_100_000, live, 32);
        assert_eq!(delivery_status(&store, BOB).unposted_waiting_count, 2);

        store
            .mark_outbound_envelope_relay_posted(first, 1_400_000)
            .unwrap();
        assert_eq!(delivery_status(&store, BOB).unposted_waiting_count, 1);

        store
            .mark_outbound_envelope_relay_posted(second, 1_450_000)
            .unwrap();
        let bob = delivery_status(&store, BOB);
        // Everything uploaded, nothing confirmed: still waiting on them, but
        // nothing left for this phone to do about it.
        assert_eq!(bob.waiting_count, 2);
        assert_eq!(bob.unposted_waiting_count, 0);
    }

    #[test]
    fn one_unreadable_recipient_id_costs_one_row_not_the_page() {
        // Both shells read an error from this call as "no delivery state at
        // all", so failing the batch over a single degenerate id -- an old
        // import, a half-written restore -- would blank every friend's line
        // instead of one.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        queue_pairwise(
            &store,
            BOB,
            1,
            crate::KIND_TEXT,
            1_000_000,
            DELIVERY_NOW + 60_000,
            32,
        );

        let rows = store
            .recipient_delivery_status(
                ALICE.to_vec(),
                vec![Vec::new(), vec![9u8; 129], BOB.to_vec()],
                DELIVERY_NOW,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].recipient_user_id, BOB.to_vec());
        assert_eq!(rows[0].waiting_count, 1);
    }

    #[test]
    fn an_envelope_no_transport_will_carry_is_reported_as_oversized() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let live = DELIVERY_NOW + 60_000;
        queue_pairwise(
            &store,
            BOB,
            1,
            crate::KIND_TEXT,
            1_000_000,
            live,
            MAX_ENVELOPE_SEALED_BYTES,
        );
        assert!(!delivery_status(&store, BOB).oversized_waiting);

        queue_pairwise(
            &store,
            BOB,
            2,
            crate::KIND_TEXT,
            1_100_000,
            live,
            MAX_ENVELOPE_SEALED_BYTES + 1,
        );
        assert!(delivery_status(&store, BOB).oversized_waiting);

        // Once their receipt covers it, it is no longer waiting work and no
        // longer a reason to warn anyone.
        store
            .record_receipt(
                BOB.to_vec(),
                ALICE.to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                2,
                None,
                None,
            )
            .unwrap();
        assert!(!delivery_status(&store, BOB).oversized_waiting);
    }

    #[test]
    fn a_broken_event_ring_never_fails_the_work_it_was_recording() {
        // The rule for the whole diagnostics subsystem, asserted where it
        // costs the most. The ring is destroyed under a live store — standing
        // in for a full disk, a locked table, or any other transient fault —
        // and then every operational path that emits into it is driven. Each
        // one must carry on exactly as if the ring had never existed: an
        // archive nobody asked for is not worth a frontier that stops
        // advancing, a receipt that is not recorded, or a message that cannot
        // be authored.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(BOB, "Bo")).unwrap();
        {
            let conn = lock_conn(&store.conn);
            conn.execute_batch("DROP TABLE protocol_events").unwrap();
        }

        let key = "https://relay.example.invalid/|token".to_string();
        assert_eq!(
            store
                .advance_relay_fetch_cursor(key.clone(), 40, true)
                .unwrap(),
            40,
            "the mailbox walk must still advance its frontier"
        );
        assert_eq!(
            store
                .advance_relay_fetch_cursor(key.clone(), 90, true)
                .unwrap(),
            90,
            "and must still advance it on the call after"
        );
        assert_eq!(
            store
                .advance_relay_sweep_cursor(key.clone(), 20, true, 1_700_000_000_000)
                .unwrap(),
            20
        );
        store
            .note_relay_sweep_completed(key.clone(), 1_700_000_001_000, 90)
            .unwrap();
        store
            .note_contact_relay_rejected(BOB.to_vec(), 1_700_000_002_000)
            .unwrap();
        store.clear_contact_relay_rejection(BOB.to_vec()).unwrap();

        // Authoring queues a row, and a covering receipt retires it. Both run
        // inside transactions that a propagated ring error would have rolled
        // back.
        queue_pairwise(
            &store,
            BOB,
            2,
            crate::KIND_TEXT,
            1_100_000,
            1_700_000_000_000,
            64,
        );
        store
            .record_receipt(
                BOB.to_vec(),
                ALICE.to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                2,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_through(BOB.to_vec(), ALICE.to_vec(), crate::RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            2,
            "the receipt was recorded, not lost with the ring"
        );

        // EVICT-01 has two new ring call sites inside carry transactions.
        // Neither a successful foreign eviction nor a capacity rejection may
        // turn an optional diagnostics failure into operational data loss.
        assert!(enqueue_carried_envelope_with_budgets(
            &store,
            carried(b"pressure-old", b"h1", 9_000, 60),
            false,
            1_000,
            60,
            60,
        )
        .unwrap());
        assert!(enqueue_carried_envelope_with_budgets(
            &store,
            carried(b"pressure-family", b"h2", 9_000, 60),
            true,
            2_000,
            60,
            60,
        )
        .unwrap());
        assert_eq!(
            store.carried_msg_ids(10).unwrap(),
            vec![b"pressure-family".to_vec()]
        );
        assert!(enqueue_carried_envelope_with_budgets(
            &store,
            carried(b"pressure-reject", b"h3", 9_000, 1),
            true,
            3_000,
            60,
            60,
        )
        .is_err());
        assert_eq!(
            store.carried_msg_ids(10).unwrap(),
            vec![b"pressure-family".to_vec()],
            "the rejected candidate is removed even when its evidence cannot be written"
        );
    }

    #[test]
    fn persisted_contact_endpoint_health_rides_along_uninterpreted() {
        // The thresholds belong to `contact_relay_health`; this query only
        // carries the numbers so the classifier can apply them once.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(BOB, "Bo")).unwrap();
        store
            .note_contact_relay_rejected(BOB.to_vec(), 1_234_000)
            .unwrap();
        store
            .note_contact_relay_unreachable(BOB.to_vec(), "endpoint".to_string(), 1_235_000)
            .unwrap();

        let bob = delivery_status(&store, BOB);
        assert_eq!(bob.relay_reject_streak, 1);
        assert_eq!(bob.relay_rejected_at_ms, 1_234_000);
        assert_eq!(bob.relay_unreachable_streak, 1);
        assert_eq!(bob.relay_unreachable_at_ms, 1_235_000);

        // A recipient with no contact row at all reads as healthy rather than
        // erroring: an unknown endpoint has never failed.
        let carol = delivery_status(&store, CAROL);
        assert_eq!(carol.relay_reject_streak, 0);
        assert_eq!(carol.relay_unreachable_streak, 0);
    }

    #[test]
    fn visible_chat_kind_sql_list_is_generated_from_the_predicate() {
        // Written out, this list would drift from the chat screens' filter the
        // first time a kind was added. Generated, it cannot.
        let list = visible_chat_kind_sql_list();
        let kinds: Vec<u8> = list
            .split(", ")
            .map(|kind| kind.parse::<u8>().unwrap())
            .collect();
        assert!(!kinds.is_empty());
        for kind in u8::MIN..=u8::MAX {
            assert_eq!(
                kinds.contains(&kind),
                core_is_visible_chat_kind(kind),
                "kind {kind}"
            );
        }
    }

    #[test]
    fn recipient_delivery_query_seeks_the_recipient_index() {
        // A field store carries six figures of envelopes. This query must
        // reach one person's outstanding mail by seeking a contiguous index
        // range, never by walking the queue -- and the plan is explained
        // against the shared builder, not a second copy of the SQL that could
        // go on passing after the real statement changed.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let conn = lock_conn(&store.conn);
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {}", recipient_waiting_sql()))
            .unwrap()
            .query_map(
                params![
                    BOB.to_vec(),
                    0i64,
                    DELIVERY_NOW,
                    MAX_ENVELOPE_SEALED_BYTES as i64
                ],
                |row| row.get::<_, String>(3),
            )
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let plan_text = plan.join("\n");
        assert!(
            plan_text.contains("idx_outbound_recipient_chat_lamport"),
            "plan did not use the recipient index:\n{plan_text}"
        );
        assert!(
            !plan_text.contains("SCAN outbound_envelopes"),
            "plan fell back to a scan:\n{plan_text}"
        );
    }

    #[test]
    fn outgoing_receipt_envelope_round_trips_and_is_queryable_by_watermark_key() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let envelope = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            5,
            b"receipt-00000001",
        );

        assert!(store
            .upsert_outgoing_receipt_envelope(envelope.clone(), 1_000)
            .unwrap());
        assert_eq!(
            store
                .outgoing_receipt_envelope(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED,
                )
                .unwrap(),
            Some(envelope),
        );
    }

    #[test]
    fn outgoing_receipt_envelope_same_watermark_preserves_stable_msg_id() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let first = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            5,
            b"receipt-00000001",
        );
        let second = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            5,
            b"receipt-00000002",
        );

        assert!(store
            .upsert_outgoing_receipt_envelope(first.clone(), 1_000)
            .unwrap());
        assert!(!store
            .upsert_outgoing_receipt_envelope(second, 2_000)
            .unwrap());
        assert_eq!(
            store
                .outgoing_receipt_envelope(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED,
                )
                .unwrap(),
            Some(first),
        );
    }

    #[test]
    fn outgoing_receipt_envelope_higher_watermark_replaces_row_and_requeues_for_relay() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let first = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            5,
            b"receipt-00000001",
        );
        let second = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            7,
            b"receipt-00000002",
        );

        assert!(store
            .upsert_outgoing_receipt_envelope(first.clone(), 1_000)
            .unwrap());
        assert!(store
            .mark_outgoing_receipt_envelope_relay_posted(first.msg_id.clone(), 1_500)
            .unwrap());
        assert!(store
            .upsert_outgoing_receipt_envelope(second.clone(), 2_000)
            .unwrap());

        assert_eq!(
            store
                .pending_relay_outgoing_receipt_envelopes(10, 3_000, vec![])
                .unwrap(),
            vec![second.clone()],
        );
        assert_eq!(
            store
                .outgoing_receipt_envelope(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED,
                )
                .unwrap(),
            Some(second),
        );
    }

    #[test]
    fn pending_relay_outgoing_receipt_envelopes_skip_posted_and_expired_rows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let live = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_DELIVERED,
            5,
            b"receipt-00000001",
        );
        let mut expired = outgoing_receipt_for(
            b"chat-a",
            b"alice",
            b"alice",
            crate::RECEIPT_TYPE_READ,
            4,
            b"receipt-00000002",
        );
        expired.expiry = 1_999;
        let posted = outgoing_receipt_for(
            b"chat-b",
            b"bob",
            b"bob",
            crate::RECEIPT_TYPE_DELIVERED,
            9,
            b"receipt-00000003",
        );

        store
            .upsert_outgoing_receipt_envelope(live.clone(), 1_000)
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(expired, 1_100)
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(posted.clone(), 1_200)
            .unwrap();
        assert!(store
            .mark_outgoing_receipt_envelope_relay_posted(posted.msg_id.clone(), 1_500)
            .unwrap());

        assert_eq!(
            store
                .pending_relay_outgoing_receipt_envelopes(10, 2_000, vec![])
                .unwrap(),
            vec![live],
        );
    }

    #[test]
    fn family_carried_envelopes_return_only_unexpired_family_rows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(carried(b"fam", b"h1", 9_000, 10), true, 1_000, BIG_BUDGET)
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"foreign", b"h2", 9_000, 10),
                false,
                2_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"expired", b"h3", 1_500, 10),
                true,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();

        let rows = store.family_carried_envelopes(10, 2_000, vec![]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].msg_id, b"fam".to_vec());
    }

    /// The mule queue starves like the outbound and receipt queues did: one
    /// unreachable destination's rows never clear, so under flat `received_at`
    /// order they refill the batch every pass. 236 of the 758 upload failures
    /// in the field capture came from this queue.
    #[test]
    fn one_destinations_carry_backlog_cannot_consume_the_whole_upload_batch() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let now = 2_000i64;
        let stuck = contact(b"stuck-contact-id", "Stuck");
        let healthy = contact(b"healthy-contact", "Healthy");
        store.upsert_contact(stuck.clone()).unwrap();
        store.upsert_contact(healthy.clone()).unwrap();
        let stuck_hint = crate::recipient_hints::recent_hints_for(stuck.user_id.clone(), now)
            .into_iter()
            .next()
            .unwrap();
        let healthy_hint = crate::recipient_hints::recent_hints_for(healthy.user_id.clone(), now)
            .into_iter()
            .next()
            .unwrap();

        // The jammed destination queued first and holds a deep backlog.
        for i in 0..40u8 {
            store
                .enqueue_carried_envelope(
                    carried(&[b'S', i], &stuck_hint, 900_000, 10),
                    true,
                    1_000 + i as i64,
                    BIG_BUDGET,
                )
                .unwrap();
        }
        for i in 0..3u8 {
            store
                .enqueue_carried_envelope(
                    carried(&[b'H', i], &healthy_hint, 900_000, 10),
                    true,
                    800_000 + i as i64,
                    BIG_BUDGET,
                )
                .unwrap();
        }

        let batch = store.family_carried_envelopes(8, now, vec![]).unwrap();
        assert_eq!(batch.len(), 8);
        assert_eq!(
            batch
                .iter()
                .filter(|e| e.recipient_hint == healthy_hint)
                .count(),
            3,
            "the reachable destination's carry must still be offered",
        );

        // And a destination known to be unpostable consumes no slots at all.
        let skipped = store
            .family_carried_envelopes(128, now, vec![stuck.user_id.clone()])
            .unwrap();
        assert_eq!(skipped.len(), 3);
        assert!(skipped.iter().all(|e| e.recipient_hint == healthy_hint));
    }

    /// A carry whose hint resolves to nobody must still be offered: a group
    /// carry with no contact member among its recipients resolves to no
    /// recipient here, but the caller can still upload it via the group path.
    #[test]
    fn carried_rows_with_an_unresolvable_hint_are_still_offered() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"orphan", b"no-such-hint", 900_000, 10),
                true,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        let rows = store.family_carried_envelopes(10, 2_000, vec![]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].msg_id, b"orphan".to_vec());
    }

    #[test]
    fn family_carried_envelopes_query_still_uses_the_supporting_index() {
        // FC7: the relay-upload query filters on (is_family, from_relay,
        // expiry); without a supporting index SQLite falls back to a full
        // scan. `idx_carried_family_upload` covers that filter.
        //
        // This explains the query the code actually runs. It previously kept
        // its own copy of the SQL, so it went on passing against a query that
        // no longer existed -- which is why the builder is shared now.
        //
        // The original also asserted no temp b-tree. That no longer holds and
        // should not: ordering round-robin across recipients cannot be served
        // by an index whose order is `received_at`, so the fairness sort is a
        // real and accepted cost. It sorts only rows the WHERE clause already
        // selected -- family mail, not yet uploaded, unexpired -- and the
        // alternative is the starvation this ordering exists to prevent.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let conn = lock_conn(&store.conn);
        conn.execute_batch(
            "CREATE TEMP TABLE IF NOT EXISTS carried_hint_map (
                 hint              BLOB PRIMARY KEY,
                 recipient_user_id BLOB NOT NULL
             );",
        )
        .unwrap();
        let plan: Vec<String> = conn
            .prepare(&format!(
                "EXPLAIN QUERY PLAN {}",
                family_carried_upload_sql("")
            ))
            .unwrap()
            .query_map(params![2_000i64, 10i64], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let plan_text = plan.join(
            "
",
        );
        assert!(
            plan_text.contains("idx_carried_family_upload"),
            "plan did not use the index:
{plan_text}"
        );
    }

    fn contact(user_id: &[u8], name: &str) -> Contact {
        Contact {
            user_id: user_id.to_vec(),
            name: name.to_string(),
            sign_pk: vec![1u8; 32],
            agree_pk: vec![2u8; 32],
            relay_url: None,
            relay_token: None,
            nickname: None,
        }
    }

    #[test]
    fn contact_discovery_policy_only_moves_forward() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let id = vec![1; 16];
        assert!(store
            .upsert_contact_discovery_policy(ContactDiscoveryPolicy {
                user_id: id.clone(),
                protocol_version: 1,
                enabled: true,
                revision: 5,
            })
            .unwrap());
        assert!(!store
            .upsert_contact_discovery_policy(ContactDiscoveryPolicy {
                user_id: id.clone(),
                protocol_version: 1,
                enabled: false,
                revision: 4,
            })
            .unwrap());
        let policy = store.get_contact_discovery_policy(id).unwrap().unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.revision, 5);
    }

    #[test]
    fn block_unblock_round_trip_and_listing() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mallory = test_user_id(b"mallory");
        assert!(!store.is_user_blocked(mallory.clone()).unwrap());
        store.block_user(mallory.clone(), 1_000).unwrap();
        store.block_user(mallory.clone(), 2_000).unwrap(); // idempotent
        assert!(store.is_user_blocked(mallory.clone()).unwrap());
        assert_eq!(store.list_blocked_users().unwrap(), vec![mallory.clone()]);
        assert!(store.unblock_user(mallory.clone()).unwrap());
        assert!(!store.unblock_user(mallory.clone()).unwrap());
        assert!(!store.is_user_blocked(mallory).unwrap());
    }

    #[test]
    fn blocked_identity_is_excluded_from_suggestions() {
        let alice = generate_identity();
        let bob = generate_identity();
        let carol = generate_identity();
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .upsert_contact(Contact {
                user_id: alice.user_id.clone(),
                name: "Alice".to_string(),
                sign_pk: alice.sign_pk.clone(),
                agree_pk: alice.agree_pk.clone(),
                relay_url: None,
                relay_token: None,
                nickname: None,
            })
            .unwrap();
        let ticket = create_introduction_ticket(
            alice.clone(),
            carol.user_id.clone(),
            bob.user_id.clone(),
            3,
            1_000,
            100_000,
            vec![9; 16],
        )
        .unwrap();
        store
            .apply_friend_directory(
                alice.user_id.clone(),
                bob.user_id.clone(),
                FriendDirectoryContent {
                    version: 1,
                    revision: 1,
                    entries: vec![FriendDirectoryEntry {
                        candidate: SuggestedFriendCard {
                            name: "Carol".to_string(),
                            user_id: carol.user_id.clone(),
                            sign_pk: carol.sign_pk.clone(),
                            agree_pk: carol.agree_pk.clone(),
                        },
                        candidate_policy_revision: 3,
                        ticket,
                    }],
                },
                2_000,
            )
            .unwrap();
        assert_eq!(store.list_friend_suggestions(2_000).unwrap().len(), 1);

        store.block_user(carol.user_id.clone(), 2_500).unwrap();
        assert!(store.list_friend_suggestions(2_000).unwrap().is_empty());
        store.unblock_user(carol.user_id).unwrap();
        assert_eq!(store.list_friend_suggestions(2_000).unwrap().len(), 1);
    }

    #[test]
    fn deliberate_reimport_clears_block() {
        let carol = generate_identity();
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.block_user(carol.user_id.clone(), 1_000).unwrap();
        assert!(store.is_user_blocked(carol.user_id.clone()).unwrap());
        store
            .upsert_imported_contact(Contact {
                user_id: carol.user_id.clone(),
                name: "Carol".to_string(),
                sign_pk: carol.sign_pk.clone(),
                agree_pk: carol.agree_pk.clone(),
                relay_url: None,
                relay_token: None,
                nickname: None,
            })
            .unwrap();
        assert!(!store.is_user_blocked(carol.user_id).unwrap());
    }

    #[test]
    fn friend_directory_replaces_one_source_and_honors_suppression() {
        let alice = generate_identity();
        let bob = generate_identity();
        let carol = generate_identity();
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .upsert_contact(Contact {
                user_id: alice.user_id.clone(),
                name: "Alice".to_string(),
                sign_pk: alice.sign_pk.clone(),
                agree_pk: alice.agree_pk.clone(),
                relay_url: None,
                relay_token: None,
                nickname: None,
            })
            .unwrap();
        let ticket = create_introduction_ticket(
            alice.clone(),
            carol.user_id.clone(),
            bob.user_id.clone(),
            3,
            1_000,
            100_000,
            vec![9; 16],
        )
        .unwrap();
        let directory = FriendDirectoryContent {
            version: 1,
            revision: 1,
            entries: vec![FriendDirectoryEntry {
                candidate: SuggestedFriendCard {
                    name: "Carol".to_string(),
                    user_id: carol.user_id.clone(),
                    sign_pk: carol.sign_pk.clone(),
                    agree_pk: carol.agree_pk.clone(),
                },
                candidate_policy_revision: 3,
                ticket,
            }],
        };
        assert!(store
            .apply_friend_directory(
                alice.user_id.clone(),
                bob.user_id.clone(),
                directory.clone(),
                2_000,
            )
            .unwrap());
        assert!(!store
            .apply_friend_directory(alice.user_id.clone(), vec![2; 16], directory, 2_000,)
            .unwrap());
        let suggestions = store.list_friend_suggestions(2_000).unwrap();
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].candidate.name, "Carol");

        store
            .set_friend_suggestion_state(suggestions[0].candidate.user_id.clone(), 1)
            .unwrap();
        let newer_ticket = create_introduction_ticket(
            alice.clone(),
            carol.user_id.clone(),
            bob.user_id.clone(),
            3,
            2_000,
            101_000,
            vec![10; 16],
        )
        .unwrap();
        assert!(store
            .apply_friend_directory(
                alice.user_id.clone(),
                bob.user_id,
                FriendDirectoryContent {
                    version: 1,
                    revision: 2,
                    entries: vec![FriendDirectoryEntry {
                        candidate: SuggestedFriendCard {
                            name: "Carol".to_string(),
                            user_id: carol.user_id,
                            sign_pk: carol.sign_pk,
                            agree_pk: carol.agree_pk,
                        },
                        candidate_policy_revision: 3,
                        ticket: newer_ticket,
                    }],
                },
                2_000,
            )
            .unwrap());
        let suggestions = store.list_friend_suggestions(2_000).unwrap();
        assert_eq!(suggestions[0].state, 0);

        store
            .set_friend_suggestion_state(suggestions[0].candidate.user_id.clone(), 2)
            .unwrap();
        assert!(store.list_friend_suggestions(2_000).unwrap().is_empty());

        assert!(store
            .apply_friend_directory(
                alice.user_id,
                vec![2; 16],
                FriendDirectoryContent {
                    version: 1,
                    revision: 3,
                    entries: Vec::new(),
                },
                2_000,
            )
            .unwrap());
    }

    #[test]
    fn direct_provenance_cannot_be_downgraded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let user_id = vec![3; 16];
        store
            .upsert_contact_provenance(ContactProvenance {
                user_id: user_id.clone(),
                source: 0,
                introducer_user_id: None,
                introduced_at_ms: 10,
                added_nearby: true,
            })
            .unwrap();
        store
            .upsert_contact_provenance(ContactProvenance {
                user_id: user_id.clone(),
                source: 1,
                introducer_user_id: Some(vec![4; 16]),
                introduced_at_ms: 20,
                added_nearby: false,
            })
            .unwrap();
        let provenance = store.get_contact_provenance(user_id).unwrap().unwrap();
        assert_eq!(provenance.source, 0);
        assert!(provenance.introducer_user_id.is_none());
        assert_eq!(provenance.introduced_at_ms, 10);
    }

    #[test]
    fn shared_provenance_persists_and_never_downgrades_direct() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();

        // source = 2 is now valid and round-trips.
        let shared_id = vec![9; 16];
        store
            .upsert_contact_provenance(ContactProvenance {
                user_id: shared_id.clone(),
                source: 2,
                introducer_user_id: Some(vec![4; 16]),
                introduced_at_ms: 10,
                added_nearby: false,
            })
            .unwrap();
        let provenance = store.get_contact_provenance(shared_id).unwrap().unwrap();
        assert_eq!(provenance.source, 2);
        assert_eq!(provenance.introducer_user_id, Some(vec![4; 16]));

        // The guard widened by exactly one value.
        assert!(store
            .upsert_contact_provenance(ContactProvenance {
                user_id: vec![10; 16],
                source: 3,
                introducer_user_id: None,
                introduced_at_ms: 10,
                added_nearby: false,
            })
            .is_err());

        // A later shared import cannot overwrite direct.
        let direct_id = vec![11; 16];
        for source in [0u8, 2u8] {
            store
                .upsert_contact_provenance(ContactProvenance {
                    user_id: direct_id.clone(),
                    source,
                    introducer_user_id: (source == 2).then(|| vec![4; 16]),
                    introduced_at_ms: 10 + source as i64,
                    added_nearby: false,
                })
                .unwrap();
        }
        assert_eq!(
            store
                .get_contact_provenance(direct_id)
                .unwrap()
                .unwrap()
                .source,
            0
        );
    }

    fn pending_request(requester: u8, expires_at_ms: i64) -> PendingSharedRequest {
        PendingSharedRequest {
            requester_user_id: vec![requester; 16],
            name: "Riley".to_string(),
            sign_pk: vec![1; 32],
            agree_pk: vec![2; 32],
            relay_url: None,
            relay_token: None,
            sharer_user_id: vec![5; 16],
            expires_at_ms,
            first_seen_ms: 100,
            last_prompted_ms: 0,
        }
    }

    #[test]
    fn pending_shared_requests_dedupe_and_sweep_on_read() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .upsert_pending_shared_request(pending_request(1, 10_000))
            .unwrap();

        // Redelivery updates the row instead of stacking, and keeps
        // first_seen_ms.
        let mut redelivered = pending_request(1, 20_000);
        redelivered.first_seen_ms = 999;
        redelivered.name = "Riley S".to_string();
        store.upsert_pending_shared_request(redelivered).unwrap();
        let rows = store.list_pending_shared_requests(0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Riley S");
        assert_eq!(rows[0].first_seen_ms, 100);
        assert_eq!(rows[0].expires_at_ms, 20_000);

        // Expired rows vanish on read.
        store
            .upsert_pending_shared_request(pending_request(2, 5_000))
            .unwrap();
        let rows = store.list_pending_shared_requests(6_000).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].requester_user_id, vec![1; 16]);
        assert!(store
            .get_pending_shared_request(vec![2; 16])
            .unwrap()
            .is_none());
    }

    #[test]
    fn shared_request_prompts_are_rate_limited_and_suppressible() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let requester = vec![1; 16];
        store
            .upsert_pending_shared_request(pending_request(1, i64::MAX))
            .unwrap();

        // First prompt fires; a second within the same day does not; a day
        // later it may fire again.
        assert!(store
            .note_shared_request_prompt(requester.clone(), MS_PER_DAY)
            .unwrap());
        assert!(!store
            .note_shared_request_prompt(requester.clone(), MS_PER_DAY + 1000)
            .unwrap());
        assert!(store
            .note_shared_request_prompt(requester.clone(), 2 * MS_PER_DAY + 1000)
            .unwrap());

        // Not now: count climbs across the row's deletion.
        assert_eq!(
            store
                .record_shared_request_dismissal(requester.clone())
                .unwrap(),
            1
        );
        store
            .delete_pending_shared_request(requester.clone())
            .unwrap();
        assert_eq!(
            store
                .record_shared_request_dismissal(requester.clone())
                .unwrap(),
            2
        );

        // Don't ask again: no prompt ever, even for a fresh pending row.
        store.suppress_shared_requests(requester.clone()).unwrap();
        store
            .upsert_pending_shared_request(pending_request(1, i64::MAX))
            .unwrap();
        assert!(!store
            .note_shared_request_prompt(requester.clone(), 10 * MS_PER_DAY)
            .unwrap());
        assert!(
            store
                .get_shared_request_dismissal(requester.clone())
                .unwrap()
                .unwrap()
                .suppressed
        );

        // A direct scan clears the tombstone and the history with it.
        store
            .clear_shared_request_dismissal(requester.clone())
            .unwrap();
        assert!(store
            .get_shared_request_dismissal(requester.clone())
            .unwrap()
            .is_none());
        assert!(store
            .note_shared_request_prompt(requester, 11 * MS_PER_DAY)
            .unwrap());
    }

    #[test]
    fn outgoing_shared_requests_outlive_expiry_until_deleted() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .upsert_outgoing_shared_request(OutgoingSharedRequest {
                candidate_user_id: vec![1; 16],
                expires_at_ms: 1_000,
                sent_at_ms: 500,
            })
            .unwrap();
        // Long past expiry the row is still there: expiry is a UI state
        // ("didn't respond"), not a deletion trigger.
        let rows = store.list_outgoing_shared_requests().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].expires_at_ms, 1_000);
        store.delete_outgoing_shared_request(vec![1; 16]).unwrap();
        assert!(store.list_outgoing_shared_requests().unwrap().is_empty());
    }

    #[test]
    fn meeting_in_person_survives_a_later_remote_re_add() {
        // A card pasted later (or an introduction arriving from a friend)
        // must not erase the fact that we once stood next to this person.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let user_id = vec![7; 16];
        store
            .upsert_contact_provenance(ContactProvenance {
                user_id: user_id.clone(),
                source: 0,
                introducer_user_id: None,
                introduced_at_ms: 10,
                added_nearby: true,
            })
            .unwrap();
        store
            .upsert_contact_provenance(ContactProvenance {
                user_id: user_id.clone(),
                source: 0,
                introducer_user_id: None,
                introduced_at_ms: 20,
                added_nearby: false,
            })
            .unwrap();
        assert!(
            store
                .get_contact_provenance(user_id)
                .unwrap()
                .unwrap()
                .added_nearby
        );
    }

    #[test]
    fn a_remote_add_can_be_upgraded_once_we_actually_meet() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let user_id = vec![8; 16];
        store
            .upsert_contact_provenance(ContactProvenance {
                user_id: user_id.clone(),
                source: 0,
                introducer_user_id: None,
                introduced_at_ms: 10,
                added_nearby: false,
            })
            .unwrap();
        assert!(
            !store
                .get_contact_provenance(user_id.clone())
                .unwrap()
                .unwrap()
                .added_nearby
        );
        store
            .upsert_contact_provenance(ContactProvenance {
                user_id: user_id.clone(),
                source: 0,
                introducer_user_id: None,
                introduced_at_ms: 20,
                added_nearby: true,
            })
            .unwrap();
        assert!(
            store
                .get_contact_provenance(user_id)
                .unwrap()
                .unwrap()
                .added_nearby
        );
    }

    #[test]
    fn open_migrates_an_old_contacts_table_to_add_relay_token() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cruisemesh-store-migration-{unique}.sqlite"));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE contacts (
                user_id   BLOB PRIMARY KEY,
                name      TEXT NOT NULL,
                sign_pk   BLOB NOT NULL,
                agree_pk  BLOB NOT NULL,
                relay_url TEXT
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO contacts (user_id, name, sign_pk, agree_pk, relay_url)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                b"alice-id".to_vec(),
                "Alice",
                vec![1u8; 32],
                vec![2u8; 32],
                "https://relay.example"
            ],
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str.clone()).unwrap();
        let migrated = store.get_contact(b"alice-id".to_vec()).unwrap().unwrap();
        assert_eq!(
            migrated.relay_url,
            Some("https://relay.example".to_string())
        );
        assert_eq!(migrated.relay_token, None);

        let mut updated = migrated.clone();
        updated.relay_token = Some("family-token".to_string());
        store.upsert_contact(updated.clone()).unwrap();
        assert_eq!(
            store.get_contact(b"alice-id".to_vec()).unwrap(),
            Some(updated)
        );

        drop(store);
        fs::remove_file(path).unwrap();
    }

    /// A store written before the sweep resume cursor existed keeps its
    /// frontier and its sweep timestamp, and reads as "no sweep under way" --
    /// which is true, because the sweep that was interrupted by the upgrade
    /// had nowhere to record itself. It starts its next sweep at the
    /// beginning, once, and resumes properly from then on.
    #[test]
    fn open_migrates_a_relay_cursor_table_without_the_sweep_resume_column() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cruisemesh-sweep-migration-{unique}.sqlite"));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE relay_fetch_cursors (
                config_key    TEXT PRIMARY KEY,
                after_id      INTEGER NOT NULL DEFAULT 0,
                last_sweep_at INTEGER NOT NULL DEFAULT 0
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO relay_fetch_cursors (config_key, after_id, last_sweep_at)
             VALUES (?1, 29000, 1000000)",
            params![cursor_key()],
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str).unwrap();
        let cursor = store.relay_fetch_cursor(cursor_key()).unwrap();
        assert_eq!(cursor.after_id, 29_000);
        assert_eq!(cursor.last_sweep_at_ms, 1_000_000);
        assert_eq!(cursor.sweep_after_id, 0);
        assert_eq!(cursor.sweep_started_at_ms, 0);
        // The new columns are writable, so the very next sweep resumes
        // normally -- and dates itself, so it is never mistaken for a sweep
        // stalled since before the relay was last rebuilt.
        assert_eq!(
            store
                .advance_relay_sweep_cursor(cursor_key(), 512, true, 2_000_000)
                .unwrap(),
            512
        );
        assert_eq!(
            store
                .relay_fetch_cursor(cursor_key())
                .unwrap()
                .sweep_started_at_ms,
            2_000_000
        );

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn set_contact_nickname_round_trips_and_blank_clears() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        assert!(store
            .set_contact_nickname(b"alice-id".to_vec(), Some("  Mom  ".to_string()))
            .unwrap());
        // Whitespace is trimmed on the way in.
        assert_eq!(
            store
                .get_contact(b"alice-id".to_vec())
                .unwrap()
                .unwrap()
                .nickname,
            Some("Mom".to_string())
        );

        // A blank value clears the nickname.
        assert!(store
            .set_contact_nickname(b"alice-id".to_vec(), Some("   ".to_string()))
            .unwrap());
        assert_eq!(
            store
                .get_contact(b"alice-id".to_vec())
                .unwrap()
                .unwrap()
                .nickname,
            None
        );

        // Unknown contact reports no change.
        assert!(!store
            .set_contact_nickname(b"nobody".to_vec(), Some("X".to_string()))
            .unwrap());
    }

    #[test]
    fn reimporting_a_friend_card_preserves_the_local_nickname() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        store
            .set_contact_nickname(b"alice-id".to_vec(), Some("Mom".to_string()))
            .unwrap();

        // Re-importing the card (e.g. a re-scan) carries no nickname and must
        // not erase the local one.
        let mut card = contact(b"alice-id", "Alice iPhone");
        card.relay_url = Some("https://relay.example".to_string());
        card.relay_token = Some("family".to_string());
        store.upsert_imported_contact(card).unwrap();

        let after = store.get_contact(b"alice-id".to_vec()).unwrap().unwrap();
        assert_eq!(after.nickname, Some("Mom".to_string()));
        // The card name still updated; only the nickname is sticky.
        assert_eq!(after.name, "Alice iPhone");
    }

    #[test]
    fn contact_display_name_prefers_a_nonblank_nickname() {
        let mut c = contact(b"alice-id", "Alice");
        assert_eq!(core_contact_display_name(c.clone()), "Alice");

        c.nickname = Some("Mom".to_string());
        assert_eq!(core_contact_display_name(c.clone()), "Mom");

        // A blank nickname falls back to the card name.
        c.nickname = Some("   ".to_string());
        assert_eq!(core_contact_display_name(c), "Alice");
    }

    #[test]
    fn open_migrates_contacts_table_to_add_avatar_columns() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("cruisemesh-store-avatar-migration-{unique}.sqlite"));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE contacts (
                user_id   BLOB PRIMARY KEY,
                name      TEXT NOT NULL,
                sign_pk   BLOB NOT NULL,
                agree_pk  BLOB NOT NULL,
                relay_url TEXT,
                relay_token TEXT
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO contacts (user_id, name, sign_pk, agree_pk, relay_url, relay_token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                b"alice-id".to_vec(),
                "Alice",
                vec![1u8; 32],
                vec![2u8; 32],
                "https://relay.example",
                "token"
            ],
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str.clone()).unwrap();
        assert_eq!(store.contact_avatar(b"alice-id".to_vec()).unwrap(), None);
        assert_eq!(store.contact_avatar_epoch(b"alice-id".to_vec()).unwrap(), 0);
        assert!(store
            .set_contact_avatar(b"alice-id".to_vec(), Some(vec![1, 2, 3]), 10)
            .unwrap());
        assert_eq!(
            store.contact_avatar(b"alice-id".to_vec()).unwrap(),
            Some(vec![1, 2, 3])
        );

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn upsert_then_get_contact_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        let found = store
            .get_contact(b"alice-id".to_vec())
            .unwrap()
            .expect("contact exists");
        assert_eq!(found.name, "Alice");
        assert_eq!(found.sign_pk, vec![1u8; 32]);
    }

    #[test]
    fn get_contact_returns_none_when_absent() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(store.get_contact(b"nobody".to_vec()).unwrap(), None);
    }

    #[test]
    fn upsert_replaces_rather_than_duplicates() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        store
            .upsert_contact(contact(b"alice-id", "Alice R."))
            .unwrap();

        let contacts = store.list_contacts().unwrap();
        assert_eq!(contacts.len(), 1);
        assert_eq!(contacts[0].name, "Alice R.");
    }

    // -- T23 relay-change propagation -----------------------------------

    fn relay_notice(subject: &[u8], epoch: i64, url: &str) -> RelayUpdateContent {
        RelayUpdateContent {
            subject_user_id: subject.to_vec(),
            relay_epoch: epoch,
            relay_url: url.to_string(),
            relay_token: crate::relay_deposit_token_for("their-member-token".to_string()),
        }
    }

    #[test]
    fn relay_update_repairs_a_stale_endpoint() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut alice = contact(b"alice-id", "Alice");
        alice.relay_url = Some("https://retired.relay.example".to_string());
        alice.relay_token = Some("cmdep1-stale".to_string());
        store.upsert_contact(alice).unwrap();
        assert_eq!(store.contact_relay_epoch(b"alice-id".to_vec()).unwrap(), 0);

        let notice = relay_notice(b"alice-id", 100, "https://new.relay.example");
        assert!(store
            .apply_contact_relay_update(b"alice-id".to_vec(), notice.clone())
            .unwrap());

        let stored = store.get_contact(b"alice-id".to_vec()).unwrap().unwrap();
        assert_eq!(
            stored.relay_url.as_deref(),
            Some("https://new.relay.example")
        );
        assert_eq!(stored.relay_token, Some(notice.relay_token));
        assert_eq!(
            store.contact_relay_epoch(b"alice-id".to_vec()).unwrap(),
            100
        );
    }

    /// Sideband traffic reorders freely over DTN and replays cheaply off a
    /// relay. A notice that is not strictly newer must never walk a repaired
    /// endpoint back to a dead one.
    #[test]
    fn relay_update_epochs_are_monotonic() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        assert!(store
            .apply_contact_relay_update(
                b"alice-id".to_vec(),
                relay_notice(b"alice-id", 200, "https://current.relay.example")
            )
            .unwrap());

        for stale_epoch in [199, 200, 0, -5] {
            assert!(
                !store
                    .apply_contact_relay_update(
                        b"alice-id".to_vec(),
                        relay_notice(b"alice-id", stale_epoch, "https://retired.relay.example")
                    )
                    .unwrap(),
                "epoch {stale_epoch} was applied over 200"
            );
            let stored = store.get_contact(b"alice-id".to_vec()).unwrap().unwrap();
            assert_eq!(
                stored.relay_url.as_deref(),
                Some("https://current.relay.example")
            );
            assert_eq!(
                store.contact_relay_epoch(b"alice-id".to_vec()).unwrap(),
                200
            );
        }

        // Strictly newer still applies.
        assert!(store
            .apply_contact_relay_update(
                b"alice-id".to_vec(),
                relay_notice(b"alice-id", 201, "https://newer.relay.example")
            )
            .unwrap());
    }

    /// Endpoint privacy (CLAUDE.md): a device never accepts a *third party's*
    /// endpoint from anyone. Sealing makes forging Bob's notice hard, but the
    /// invariant is enforced, not assumed -- and this also catches a shell
    /// that passes the wrong user id into the applier.
    #[test]
    fn relay_update_cannot_change_a_third_party() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        let mut bob = contact(b"bob-id", "Bob");
        bob.relay_url = Some("https://bob.relay.example".to_string());
        bob.relay_token = Some("cmdep1-bob".to_string());
        store.upsert_contact(bob).unwrap();

        // Alice seals a notice that claims to move Bob's endpoint.
        let err = store
            .apply_contact_relay_update(
                b"alice-id".to_vec(),
                relay_notice(b"bob-id", 500, "https://attacker.relay.example"),
            )
            .unwrap_err();
        assert!(matches!(err, CoreError::Malformed(_)));

        for user in [&b"bob-id"[..], &b"alice-id"[..]] {
            let stored = store.get_contact(user.to_vec()).unwrap().unwrap();
            assert_ne!(
                stored.relay_url.as_deref(),
                Some("https://attacker.relay.example")
            );
            assert_eq!(store.contact_relay_epoch(user.to_vec()).unwrap(), 0);
        }
    }

    /// CP4, re-checked in the store so a caller that skipped the decoder
    /// cannot install a fetch/ack-capable credential for a contact.
    #[test]
    fn relay_update_refuses_a_member_class_credential() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        let mut notice = relay_notice(b"alice-id", 10, "https://new.relay.example");
        notice.relay_token = "their-member-token".to_string();
        assert!(store
            .apply_contact_relay_update(b"alice-id".to_vec(), notice)
            .is_err());

        // A half-configured endpoint is refused for the same reason: it is
        // neither a usable endpoint nor an honest "no internet delivery".
        let mut partial = relay_notice(b"alice-id", 10, "https://new.relay.example");
        partial.relay_token = String::new();
        assert!(store
            .apply_contact_relay_update(b"alice-id".to_vec(), partial)
            .is_err());
    }

    #[test]
    fn relay_update_can_clear_an_endpoint_and_skips_unknown_contacts() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut alice = contact(b"alice-id", "Alice");
        alice.relay_url = Some("https://old.relay.example".to_string());
        alice.relay_token = Some("cmdep1-old".to_string());
        store.upsert_contact(alice).unwrap();

        // "My pass lapsed": propagating the clear is what stops contacts
        // posting into a mailbox that no longer accepts their mail.
        let cleared = RelayUpdateContent {
            subject_user_id: b"alice-id".to_vec(),
            relay_epoch: 7,
            relay_url: String::new(),
            relay_token: String::new(),
        };
        assert!(store
            .apply_contact_relay_update(b"alice-id".to_vec(), cleared)
            .unwrap());
        let stored = store.get_contact(b"alice-id".to_vec()).unwrap().unwrap();
        assert_eq!(stored.relay_url, None);
        assert_eq!(stored.relay_token, None);

        // A notice from somebody who is not a contact is an ordinary no-op,
        // not an error: sprays and relay replays produce these routinely.
        assert!(!store
            .apply_contact_relay_update(
                b"stranger-id".to_vec(),
                relay_notice(b"stranger-id", 9, "https://stranger.relay.example")
            )
            .unwrap());
        assert_eq!(
            store.contact_relay_epoch(b"stranger-id".to_vec()).unwrap(),
            0
        );
    }

    #[test]
    fn upsert_contact_preserves_avatar_and_epoch() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        assert!(store
            .set_contact_avatar(b"alice-id".to_vec(), Some(vec![9, 8, 7]), 123)
            .unwrap());

        let mut updated = contact(b"alice-id", "Alice R.");
        updated.relay_url = Some("https://relay.example".to_string());
        store.upsert_contact(updated).unwrap();

        assert_eq!(
            store.contact_avatar(b"alice-id".to_vec()).unwrap(),
            Some(vec![9, 8, 7])
        );
        assert_eq!(
            store.contact_avatar_epoch(b"alice-id".to_vec()).unwrap(),
            123
        );
    }

    // -- relay fetch cursors (the frontier that stops re-walking the
    //    whole mailbox on every pass) -----------------------------------

    fn cursor_key() -> String {
        crate::relay_cursor_key("https://relay.example".into(), "member-token".into())
    }

    #[test]
    fn an_unknown_mailbox_starts_at_zero_with_a_sweep_due() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let cursor = store.relay_fetch_cursor(cursor_key()).unwrap();
        assert_eq!(cursor.after_id, 0);
        assert_eq!(cursor.last_sweep_at_ms, 0);
        // A mailbox this device has never swept walks from the beginning on
        // its first pass -- which is what a fresh install, a rotated token,
        // or a moved host looks like from here.
        assert!(crate::relay_sweep_due(
            false,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            10_000
        ));
    }

    #[test]
    fn a_fully_processed_page_advances_the_persisted_frontier() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        assert_eq!(
            store
                .advance_relay_fetch_cursor(key.clone(), 256, true)
                .unwrap(),
            256
        );
        assert_eq!(store.relay_fetch_cursor(key.clone()).unwrap().after_id, 256);
        assert_eq!(
            store
                .advance_relay_fetch_cursor(key.clone(), 512, true)
                .unwrap(),
            512
        );
        assert_eq!(store.relay_fetch_cursor(key).unwrap().after_id, 512);
    }

    #[test]
    fn a_page_that_failed_mid_way_never_advances_the_persisted_frontier() {
        // The safety rule: an envelope that did not reach a terminal
        // disposition must be presented again next pass, which can only
        // happen if nothing was persisted past it.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        store
            .advance_relay_fetch_cursor(key.clone(), 256, true)
            .unwrap();
        assert_eq!(
            store
                .advance_relay_fetch_cursor(key.clone(), 512, false)
                .unwrap(),
            256
        );
        assert_eq!(store.relay_fetch_cursor(key).unwrap().after_id, 256);
    }

    #[test]
    fn a_sweep_re_reading_old_pages_does_not_rewind_the_frontier() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        store
            .advance_relay_fetch_cursor(key.clone(), 9_000, true)
            .unwrap();
        // A sweep walks from 0 again; its early pages report low cursors.
        for page_cursor in [256, 512, 8_000] {
            store
                .advance_relay_fetch_cursor(key.clone(), page_cursor, true)
                .unwrap();
        }
        assert_eq!(
            store.relay_fetch_cursor(key.clone()).unwrap().after_id,
            9_000
        );
        // ...and it still moves once the sweep passes the old frontier.
        store
            .advance_relay_fetch_cursor(key.clone(), 9_500, true)
            .unwrap();
        assert_eq!(store.relay_fetch_cursor(key).unwrap().after_id, 9_500);
    }

    #[test]
    fn a_rotated_token_or_a_moved_host_reads_as_a_fresh_mailbox() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .advance_relay_fetch_cursor(cursor_key(), 9_000, true)
            .unwrap();
        let rotated = crate::relay_cursor_key("https://relay.example".into(), "new-token".into());
        let moved = crate::relay_cursor_key("https://other.example".into(), "member-token".into());
        for key in [rotated, moved] {
            let cursor = store.relay_fetch_cursor(key).unwrap();
            assert_eq!(cursor.after_id, 0, "an unknown key must start over");
            assert_eq!(cursor.last_sweep_at_ms, 0);
        }
        // The original mailbox is untouched by either.
        assert_eq!(
            store.relay_fetch_cursor(cursor_key()).unwrap().after_id,
            9_000
        );
    }

    #[test]
    fn an_endpoint_with_no_key_persists_nothing() {
        // A config missing a URL or a token has no stable identity; it must
        // not share one row with every other incomplete config.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(
            store
                .advance_relay_fetch_cursor(String::new(), 9_000, true)
                .unwrap(),
            0
        );
        store
            .note_relay_sweep_completed(String::new(), 5_000, 0)
            .unwrap();
        let cursor = store.relay_fetch_cursor(String::new()).unwrap();
        assert_eq!(cursor.after_id, 0);
        assert_eq!(cursor.last_sweep_at_ms, 0);
    }

    #[test]
    fn a_completed_sweep_restarts_the_sweep_interval_without_touching_the_frontier() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        store
            .advance_relay_fetch_cursor(key.clone(), 9_000, true)
            .unwrap();
        store
            .note_relay_sweep_completed(key.clone(), 1_000_000, 9_000)
            .unwrap();
        let cursor = store.relay_fetch_cursor(key.clone()).unwrap();
        assert_eq!(cursor.after_id, 9_000, "a sweep must not cost the frontier");
        assert_eq!(cursor.last_sweep_at_ms, 1_000_000);
        assert!(!crate::relay_sweep_due(
            true,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            1_000_001
        ));
        assert!(crate::relay_sweep_due(
            true,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            1_000_000 + crate::RELAY_SWEEP_INTERVAL_MS
        ));
        // Recording a sweep for a mailbox with no frontier yet is fine too.
        let fresh = crate::relay_cursor_key("https://fresh.example".into(), "tok".into());
        store
            .note_relay_sweep_completed(fresh.clone(), 7, 0)
            .unwrap();
        assert_eq!(store.relay_fetch_cursor(fresh).unwrap().after_id, 0);
    }

    /// The livelock, at the store. A sweep bounded by
    /// `relay_mailbox_walk_action` yields part-way up the mailbox; the pass a
    /// second later must resume where it stopped. Reading 0 here is what made
    /// a mailbox of 512-plus hint-matching rows re-download its first pages
    /// every few seconds and never finish.
    #[test]
    fn a_yielded_sweep_resumes_from_its_progress_instead_of_zero() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        // Frontier already at the top of a long-lived mailbox, and a sweep now
        // due on the six-hour schedule.
        store
            .advance_relay_fetch_cursor(key.clone(), 29_000, true)
            .unwrap();
        store
            .note_relay_sweep_completed(key.clone(), 1_000_000, 29_000)
            .unwrap();
        let now = 1_000_000 + crate::RELAY_SWEEP_INTERVAL_MS;

        let cursor = store.relay_fetch_cursor(key.clone()).unwrap();
        assert!(crate::relay_sweep_due(
            true,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            now
        ));
        assert_eq!(
            crate::relay_pass_start_cursor(true, cursor.after_id, cursor.sweep_after_id),
            0,
            "a sweep with no progress yet starts at the beginning"
        );
        // Four pages, then the budget runs out and the pass yields.
        for page_cursor in [128, 256, 384, 512] {
            store
                .advance_relay_sweep_cursor(key.clone(), page_cursor, true, now)
                .unwrap();
        }

        let cursor = store.relay_fetch_cursor(key.clone()).unwrap();
        assert_eq!(cursor.sweep_after_id, 512);
        assert_eq!(
            cursor.after_id, 29_000,
            "a sweep must still never cost the frontier its position"
        );
        assert!(
            crate::relay_sweep_due(true, cursor.last_sweep_at_ms, cursor.sweep_after_id, now),
            "an unfinished sweep stays due however recent the timestamp"
        );
        assert_eq!(
            crate::relay_pass_start_cursor(true, cursor.after_id, cursor.sweep_after_id),
            512,
            "the continuation resumes; restarting at 0 is the livelock"
        );
        // ...and an ordinary pass in between is unaffected: it reads the
        // frontier, never the sweep's progress.
        assert_eq!(
            crate::relay_pass_start_cursor(false, cursor.after_id, cursor.sweep_after_id),
            29_000
        );
    }

    /// The DTN-mirror rule, applied to the sweep cursor: a page that did not
    /// reach a terminal disposition for every envelope (or failed to land its
    /// acks) must be presented again, so nothing may be persisted past it.
    #[test]
    fn sweep_progress_only_moves_for_a_fully_processed_page() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        assert_eq!(
            store
                .advance_relay_sweep_cursor(key.clone(), 256, true, 1_000)
                .unwrap(),
            256
        );
        assert_eq!(
            store
                .advance_relay_sweep_cursor(key.clone(), 512, false, 1_000)
                .unwrap(),
            256
        );
        assert_eq!(
            store
                .relay_fetch_cursor(key.clone())
                .unwrap()
                .sweep_after_id,
            256
        );
        // And it never moves backwards, so a re-presented page cannot undo
        // ground the sweep has already covered.
        assert_eq!(
            store
                .advance_relay_sweep_cursor(key.clone(), 128, true, 1_000)
                .unwrap(),
            256
        );
        // A config with no usable endpoint persists nothing at all.
        assert_eq!(
            store
                .advance_relay_sweep_cursor(String::new(), 9, true, 1_000)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .relay_fetch_cursor(String::new())
                .unwrap()
                .sweep_after_id,
            0
        );
    }

    /// The empty page ends the walk: the timestamp restarts and the resume
    /// cursor is cleared, which is the single act that turns the sweep from
    /// in-progress back into scheduled.
    #[test]
    fn completing_a_sweep_clears_its_resume_cursor() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        store
            .advance_relay_sweep_cursor(key.clone(), 512, true, 1_000)
            .unwrap();
        store
            .note_relay_sweep_completed(key.clone(), 2_000_000, 512)
            .unwrap();
        let cursor = store.relay_fetch_cursor(key).unwrap();
        assert_eq!(cursor.sweep_after_id, 0);
        assert_eq!(cursor.last_sweep_at_ms, 2_000_000);
        assert!(
            !crate::relay_sweep_due(
                true,
                cursor.last_sweep_at_ms,
                cursor.sweep_after_id,
                2_000_001
            ),
            "a finished sweep goes back on the schedule"
        );
    }

    /// A sweep is dated by its first page and by nothing else, so the date is
    /// the age of the *walk* rather than of the last page it took.
    #[test]
    fn sweep_progress_is_dated_by_the_page_that_starts_the_walk() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        // A page that fails to fully process moves nothing, so it dates
        // nothing either -- there is no walk to date yet.
        store
            .advance_relay_sweep_cursor(key.clone(), 256, false, 4_000)
            .unwrap();
        assert_eq!(
            store
                .relay_fetch_cursor(key.clone())
                .unwrap()
                .sweep_started_at_ms,
            0
        );
        store
            .advance_relay_sweep_cursor(key.clone(), 256, true, 5_000)
            .unwrap();
        store
            .advance_relay_sweep_cursor(key.clone(), 512, true, 9_000)
            .unwrap();
        let cursor = store.relay_fetch_cursor(key).unwrap();
        assert_eq!(cursor.sweep_after_id, 512);
        assert_eq!(
            cursor.sweep_started_at_ms, 5_000,
            "later pages must not make a stalled sweep look young"
        );
    }

    /// The rebuilt-relay case, end to end at the store. A phone goes offline
    /// mid-sweep for days; while it is away the relay is rebuilt from a fresh
    /// volume and its row ids restart at 1. The remembered resume cursor now
    /// points past the end of the mailbox, so resuming from it would fetch one
    /// empty page, record a sweep that covered nothing, and put the mailbox
    /// back to sleep for another six hours with real mail sitting below a
    /// frontier no ordinary pass will ever go under. The walk restarts at 0
    /// instead, and heals on the first pass back.
    #[test]
    fn a_sweep_stalled_across_days_offline_restarts_from_zero() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        let last_sweep = 1_000_000i64;
        store
            .advance_relay_fetch_cursor(key.clone(), 29_000, true)
            .unwrap();
        store
            .note_relay_sweep_completed(key.clone(), last_sweep, 29_000)
            .unwrap();
        // The next sweep starts on schedule and yields part-way up.
        let sweep_started = last_sweep + crate::RELAY_SWEEP_INTERVAL_MS;
        store
            .advance_relay_sweep_cursor(key.clone(), 20_000, true, sweep_started)
            .unwrap();

        let back_online = sweep_started + 3 * 24 * 60 * 60 * 1000;
        let cursor = store.relay_fetch_cursor(key.clone()).unwrap();
        assert!(crate::relay_sweep_due(
            false,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            back_online
        ));
        assert!(crate::relay_sweep_restart_from_zero(
            cursor.sweep_after_id,
            cursor.sweep_started_at_ms,
            back_online
        ));
        store
            .reset_relay_sweep_progress(key.clone(), back_online)
            .unwrap();

        let cursor = store.relay_fetch_cursor(key.clone()).unwrap();
        assert_eq!(
            crate::relay_pass_start_cursor(true, cursor.after_id, cursor.sweep_after_id),
            0,
            "the walk must start at the beginning of the mailbox that exists now"
        );
        assert_eq!(
            cursor.after_id, 29_000,
            "and the frontier is not what proved wrong, so it is left alone"
        );

        // That walk yields in its turn -- and the pass a second later resumes
        // it rather than restarting it, or the repair would be its own loop.
        store
            .advance_relay_sweep_cursor(key.clone(), 512, true, back_online)
            .unwrap();
        let cursor = store.relay_fetch_cursor(key).unwrap();
        assert!(!crate::relay_sweep_restart_from_zero(
            cursor.sweep_after_id,
            cursor.sweep_started_at_ms,
            back_online + crate::relay_mailbox_continuation_delay_ms()
        ));
        assert_eq!(
            crate::relay_pass_start_cursor(true, cursor.after_id, cursor.sweep_after_id),
            512
        );
    }

    /// The other half of the rebuilt-relay story, and the one that was still
    /// open: the *frontier*. A phone whose relay was rebuilt underneath it
    /// remembers a frontier from an id space that no longer exists, so every
    /// ordinary pass asks above the top of the new mailbox and sees nothing --
    /// and relayd's live push gates on the same value, so the socket is blind
    /// too. Only the six-hourly sweep, which starts at 0, ever reached that
    /// mail. Here the sweep that walks the new mailbox end to end is what
    /// hands the mailbox back to ordinary delivery.
    #[test]
    fn a_completed_sweep_over_a_rebuilt_mailbox_lowers_the_frontier() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        let last_sweep = 1_000_000i64;
        store
            .advance_relay_fetch_cursor(key.clone(), 29_000, true)
            .unwrap();
        store
            .note_relay_sweep_completed(key.clone(), last_sweep, 29_000)
            .unwrap();

        // The relay is rebuilt from a fresh volume: row ids restart at 1, and
        // the mailbox now holds two pages ending at id 40. An ordinary pass
        // cannot see any of it.
        let now = last_sweep + crate::RELAY_SWEEP_INTERVAL_MS;
        let cursor = store.relay_fetch_cursor(key.clone()).unwrap();
        assert_eq!(
            crate::relay_pass_start_cursor(false, cursor.after_id, cursor.sweep_after_id),
            29_000,
            "the blindness this repairs: asking above the top of the mailbox"
        );
        assert!(crate::relay_sweep_due(
            true,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            now
        ));

        // The sweep walks from 0. Its pages are far below the frontier, so
        // they cannot move it -- the never-backwards rule still holds per page.
        for page_cursor in [16i64, 40i64] {
            store
                .advance_relay_sweep_cursor(key.clone(), page_cursor, true, now)
                .unwrap();
            store
                .advance_relay_fetch_cursor(key.clone(), page_cursor, true)
                .unwrap();
        }
        assert_eq!(
            store.relay_fetch_cursor(key.clone()).unwrap().after_id,
            29_000
        );

        // The empty page ends the walk at after=40, and that is the evidence.
        assert!(store
            .note_relay_sweep_completed(key.clone(), now, 40)
            .unwrap());
        let cursor = store.relay_fetch_cursor(key.clone()).unwrap();
        assert_eq!(
            cursor.after_id, 40,
            "the frontier now names the mailbox that exists"
        );
        assert_eq!(cursor.sweep_after_id, 0);
        assert_eq!(cursor.last_sweep_at_ms, now);

        // Ordinary delivery is restored: the next pass asks just above the new
        // top and picks up the mail that lands there, without waiting for
        // another sweep.
        assert_eq!(
            crate::relay_pass_start_cursor(false, cursor.after_id, cursor.sweep_after_id),
            40
        );
        assert_eq!(
            store
                .advance_relay_fetch_cursor(key.clone(), 41, true)
                .unwrap(),
            41
        );

        // And the repair does not repeat. The next completed sweep finds the
        // same top of the same mailbox and writes nothing back.
        let later = now + crate::RELAY_SWEEP_INTERVAL_MS;
        assert!(!store
            .note_relay_sweep_completed(key.clone(), later, 41)
            .unwrap());
        assert_eq!(store.relay_fetch_cursor(key).unwrap().after_id, 41);
    }

    /// The hazard the rule turns on: a drained mailbox and a rebuilt one look
    /// almost the same from the client. A completed sweep that met the empty
    /// page immediately has no evidence either way, so it leaves the frontier
    /// alone -- and loses nothing by doing so, because mail arriving on a
    /// relay that was *not* rebuilt lands above that frontier where an
    /// ordinary pass finds it.
    #[test]
    fn a_completed_sweep_over_a_quiet_mailbox_leaves_the_frontier_alone() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        store
            .advance_relay_fetch_cursor(key.clone(), 29_000, true)
            .unwrap();
        // Everything this device ever fetched has since been acked away, so
        // the sweep walks from 0 straight into the empty page.
        assert!(!store
            .note_relay_sweep_completed(key.clone(), 2_000_000, 0)
            .unwrap());
        let cursor = store.relay_fetch_cursor(key.clone()).unwrap();
        assert_eq!(cursor.after_id, 29_000);
        assert_eq!(cursor.last_sweep_at_ms, 2_000_000);
        // New mail on the same relay is above the frontier and arrives
        // normally.
        assert_eq!(
            store.advance_relay_fetch_cursor(key, 29_001, true).unwrap(),
            29_001
        );
    }

    /// A sweep that yielded, was abandoned, or simply died with the process
    /// has walked a *prefix* of the mailbox and knows nothing about the top of
    /// it. Lowering the frontier to a prefix's last id would strand every row
    /// above it behind a re-walk on every ordinary pass -- the exact
    /// thousands-of-round-trips behaviour the frontier exists to remove. Only
    /// the empty page at the end of a walk reaches the repair at all.
    #[test]
    fn an_interrupted_sweep_never_lowers_the_frontier() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        store
            .advance_relay_fetch_cursor(key.clone(), 29_000, true)
            .unwrap();
        // Four pages, then the per-pass budget runs out. Nothing calls
        // `note_relay_sweep_completed`, so nothing can lower anything.
        for page_cursor in [128i64, 256, 384, 512] {
            store
                .advance_relay_sweep_cursor(key.clone(), page_cursor, true, 1_000)
                .unwrap();
        }
        assert_eq!(
            store.relay_fetch_cursor(key.clone()).unwrap().after_id,
            29_000
        );
        // Abandoning the walk outright does not lower it either.
        store
            .reset_relay_sweep_progress(key.clone(), 2_000)
            .unwrap();
        assert_eq!(
            store.relay_fetch_cursor(key).unwrap().after_id,
            29_000,
            "a walk that never reached the end of the mailbox is not evidence"
        );
    }

    /// The frontier's own safety rule survives the exception. A page whose
    /// envelopes could not all be processed freezes the frontier while the
    /// walk carries on above it, so a completed sweep routinely ends *above*
    /// the frontier; treating that as license to raise it would skip exactly
    /// the envelope the freeze exists to re-present.
    #[test]
    fn a_completed_sweep_never_raises_the_frontier() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        store
            .advance_relay_fetch_cursor(key.clone(), 5_900, true)
            .unwrap();
        // The page at 6_000 threw, so the frontier stops there while the walk
        // continues to the top of the mailbox at 29_000.
        assert_eq!(
            store
                .advance_relay_fetch_cursor(key.clone(), 6_000, false)
                .unwrap(),
            5_900
        );
        assert!(!store
            .note_relay_sweep_completed(key.clone(), 1_000_000, 29_000)
            .unwrap());
        assert_eq!(
            store.relay_fetch_cursor(key).unwrap().after_id,
            5_900,
            "the unprocessed envelope must still be re-presented"
        );
    }

    /// A relay that answers incoherently -- rows returned, cursor standing
    /// still -- ends the walk without completing the sweep. The progress it
    /// leaves behind has to go, or a mailbox that has never completed a sweep
    /// reads as "a sweep is under way" on every pass from then on: it would
    /// never run an ordinary frontier pass again, and new mail at the top of
    /// that mailbox would simply stop arriving.
    #[test]
    fn abandoning_a_walk_hands_the_mailbox_back_to_the_schedule() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        store
            .advance_relay_fetch_cursor(key.clone(), 29_000, true)
            .unwrap();
        store
            .advance_relay_sweep_cursor(key.clone(), 512, true, 1_000)
            .unwrap();

        let cursor = store.relay_fetch_cursor(key.clone()).unwrap();
        assert!(
            crate::relay_sweep_due(true, cursor.last_sweep_at_ms, cursor.sweep_after_id, 2_000),
            "progress alone keeps a sweep due -- which is the point, until it is abandoned"
        );
        store
            .reset_relay_sweep_progress(key.clone(), 2_000)
            .unwrap();

        let cursor = store.relay_fetch_cursor(key).unwrap();
        assert_eq!(cursor.sweep_after_id, 0);
        assert!(
            !crate::relay_sweep_due(true, cursor.last_sweep_at_ms, cursor.sweep_after_id, 2_000),
            "an abandoned walk must not pin the mailbox into sweeping forever"
        );
        assert_eq!(
            crate::relay_pass_start_cursor(false, cursor.after_id, cursor.sweep_after_id),
            29_000,
            "ordinary passes resume, so new mail keeps arriving"
        );
    }

    /// A sweep is a walk across process lifetimes: the phone is killed and
    /// restarted all day, and the resume cursor is the only reason that costs
    /// nothing.
    #[test]
    fn a_sweep_interrupted_by_a_restart_resumes_where_it_stopped() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cruisemesh-sweep-restart-{unique}.sqlite"));
        let path = path.to_string_lossy().to_string();
        let key = cursor_key();
        {
            let store = MessageStore::open(path.clone()).unwrap();
            store
                .advance_relay_sweep_cursor(key.clone(), 512, true, 1_000)
                .unwrap();
        }
        let restarted = MessageStore::open(path.clone()).unwrap();
        let cursor = restarted.relay_fetch_cursor(key).unwrap();
        assert_eq!(cursor.sweep_after_id, 512);
        // `swept_this_session` is false again after a restart, but that guard
        // is not what keeps this sweep alive -- the persisted progress is.
        assert!(crate::relay_sweep_due(
            false,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            9_999
        ));
        assert_eq!(
            crate::relay_pass_start_cursor(true, cursor.after_id, cursor.sweep_after_id),
            512
        );
        drop(restarted);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn clearing_cursors_makes_the_next_pass_re_walk_every_mailbox() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let other = crate::relay_cursor_key("https://other.example".into(), "tok".into());
        store
            .advance_relay_fetch_cursor(cursor_key(), 9_000, true)
            .unwrap();
        store
            .advance_relay_fetch_cursor(other.clone(), 4_000, true)
            .unwrap();
        store.clear_relay_fetch_cursors().unwrap();
        assert_eq!(store.relay_fetch_cursor(cursor_key()).unwrap().after_id, 0);
        assert_eq!(store.relay_fetch_cursor(other).unwrap().after_id, 0);
    }

    #[test]
    fn a_stored_sweep_timestamp_survives_a_cold_start() {
        // The regression the cold-start rule exists to prevent, exercised
        // through the store rather than as pure arithmetic: write a real
        // timestamp the way a completed walk does, then ask as a *cold start*
        // (swept_this_session = false, which is what every fresh process
        // passes). It must not re-walk. If per-process forcing is ever
        // reintroduced at the store or shell layer, this is the assertion that
        // fails.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let key = cursor_key();
        store
            .advance_relay_fetch_cursor(key.clone(), 29_000, true)
            .unwrap();
        store
            .note_relay_sweep_completed(key.clone(), 1_000_000, 29_000)
            .unwrap();

        let cursor = store.relay_fetch_cursor(key.clone()).unwrap();
        assert!(
            !crate::relay_sweep_due(
                false,
                cursor.last_sweep_at_ms,
                cursor.sweep_after_id,
                1_000_000 + 60_000
            ),
            "a restart minutes after a sweep must not re-walk the mailbox"
        );
        assert_eq!(
            crate::relay_pass_start_cursor(false, cursor.after_id, cursor.sweep_after_id),
            29_000,
            "and the pass must resume from the frontier, not from 0"
        );
        // The interval still governs once it has actually elapsed.
        assert!(crate::relay_sweep_due(
            false,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            1_000_000 + crate::RELAY_SWEEP_INTERVAL_MS
        ));
    }

    #[test]
    fn gaining_a_hint_source_invalidates_every_frontier() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let own = vec![9u8; 16];
        let other = crate::relay_cursor_key("https://other.example".into(), "tok".into());

        // First call on a fresh database records the set and forces nothing:
        // an install has nothing sitting behind a frontier to miss.
        assert!(!store.note_relay_hint_sources(own.clone()).unwrap());
        assert!(!store.note_relay_hint_sources(own.clone()).unwrap());

        store
            .advance_relay_fetch_cursor(cursor_key(), 29_000, true)
            .unwrap();
        store
            .advance_relay_fetch_cursor(other.clone(), 4_000, true)
            .unwrap();
        store
            .note_relay_sweep_completed(cursor_key(), 1_000_000, 29_000)
            .unwrap();
        // ...and a sweep of the second mailbox is part-way up it.
        store
            .advance_relay_sweep_cursor(other.clone(), 6_000, true, 1_000)
            .unwrap();

        // Importing a contact widens the proxy-poll hints. Mail already in the
        // mailbox under that contact's hints is *below* the frontier, so the
        // frontier -- not the sweep schedule -- is what has to give.
        store
            .upsert_imported_contact(contact(&[7u8; 16], "Newcomer"))
            .unwrap();
        assert!(store.note_relay_hint_sources(own.clone()).unwrap());
        assert_eq!(store.relay_fetch_cursor(cursor_key()).unwrap().after_id, 0);
        assert_eq!(store.relay_fetch_cursor(other.clone()).unwrap().after_id, 0);
        // The half-finished sweep's progress goes with it. Everything below
        // 6_000 was walked while the widened hints were still invisible, so
        // resuming from there would carry that gap into a sweep that then
        // reports itself complete.
        assert_eq!(
            store
                .relay_fetch_cursor(other.clone())
                .unwrap()
                .sweep_after_id,
            0,
            "a widened hint set invalidates a partial sweep's coverage"
        );
        assert_eq!(
            store
                .relay_fetch_cursor(other.clone())
                .unwrap()
                .sweep_started_at_ms,
            0,
            "and the sweep it dated is no longer under way"
        );
        // ...and *only* the frontier gives. The sweep timestamp is the only
        // record of when this mailbox was last walked end to end; dropping it
        // here would both spend a full sweep on the next cold start and, worse,
        // read as never-swept inside a process that had already swept -- which
        // `relay_sweep_due` answers as "not due", switching the schedule off
        // until the service restarted.
        assert_eq!(
            store
                .relay_fetch_cursor(cursor_key())
                .unwrap()
                .last_sweep_at_ms,
            1_000_000,
            "invalidating a frontier must not forget when the mailbox was swept"
        );

        // Steady state again: no further re-walks while the set holds still.
        store
            .advance_relay_fetch_cursor(cursor_key(), 30_000, true)
            .unwrap();
        assert!(!store.note_relay_hint_sources(own.clone()).unwrap());
        assert_eq!(
            store.relay_fetch_cursor(cursor_key()).unwrap().after_id,
            30_000
        );

        // Joining a group widens the self hints the same way.
        store
            .upsert_group(Group {
                id: vec![5u8; 16],
                name: "Deck 9".into(),
                member_user_ids: vec![own.clone(), vec![7u8; 16]],
                key: vec![3u8; 32],
                metadata_revision: 1,
                metadata_changed_by: own.clone(),
            })
            .unwrap();
        assert!(store.note_relay_hint_sources(own.clone()).unwrap());
        assert_eq!(store.relay_fetch_cursor(cursor_key()).unwrap().after_id, 0);
    }

    #[test]
    fn the_sweep_is_still_due_on_schedule_after_an_invalidation() {
        // The regression: a running process sweeps once (so its shell now
        // passes swept_this_session = true), then the user joins a group. If
        // the invalidation forgot `last_sweep_at` along with the frontier, the
        // mailbox would read as never-swept, and `relay_sweep_due(true, 0, _)`
        // is false forever -- no six-hourly sweep would fire again until the
        // service restarted. The re-walk would still happen, but nothing would
        // record it as a sweep, so the clock would never restart either.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let own = vec![9u8; 16];
        let key = cursor_key();
        assert!(!store.note_relay_hint_sources(own.clone()).unwrap());

        let swept_at = 1_000_000i64;
        store
            .advance_relay_fetch_cursor(key.clone(), 29_000, true)
            .unwrap();
        store
            .note_relay_sweep_completed(key.clone(), swept_at, 29_000)
            .unwrap();

        store
            .upsert_imported_contact(contact(&[7u8; 16], "Newcomer"))
            .unwrap();
        assert!(store.note_relay_hint_sources(own).unwrap());

        let cursor = store.relay_fetch_cursor(key).unwrap();
        // The re-walk happens now, sweep flag or not.
        assert_eq!(
            crate::relay_pass_start_cursor(false, cursor.after_id, cursor.sweep_after_id),
            0
        );
        // And the six-hour cadence is untouched: not due a minute later, due
        // once the interval measured from the *real* last sweep has elapsed --
        // asked with swept_this_session = true, the value that made the bug
        // permanent.
        assert!(!crate::relay_sweep_due(
            true,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            swept_at + 60_000
        ));
        assert!(
            crate::relay_sweep_due(
                true,
                cursor.last_sweep_at_ms,
                cursor.sweep_after_id,
                swept_at + crate::RELAY_SWEEP_INTERVAL_MS
            ),
            "a membership change must not disable the periodic sweep"
        );
    }

    #[test]
    fn an_unswept_mailbox_still_sweeps_on_its_first_pass_after_an_invalidation() {
        // The other half: a mailbox with no cursor row is not matched by the
        // reset at all, and must keep reading as "walk from 0, sweep on the
        // first pass" -- what a fresh install, a restore, a rotated token and a
        // moved host all look like.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let own = vec![9u8; 16];
        assert!(!store.note_relay_hint_sources(own.clone()).unwrap());
        store
            .upsert_imported_contact(contact(&[7u8; 16], "Newcomer"))
            .unwrap();
        assert!(store.note_relay_hint_sources(own).unwrap());

        let cursor = store.relay_fetch_cursor(cursor_key()).unwrap();
        assert_eq!(cursor.after_id, 0);
        assert_eq!(cursor.last_sweep_at_ms, 0);
        assert!(crate::relay_sweep_due(
            false,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            10_000
        ));
    }

    #[test]
    fn a_digest_change_and_a_frontier_reset_land_together() {
        // The two writes are one transaction, so the observable states are
        // "old digest with the old frontier" and "new digest with a zeroed
        // frontier" -- never the pairing that loses the invalidation for good
        // (digest current, frontier never reset, the newly-visible mail hidden
        // until the next scheduled sweep).
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let own = vec![9u8; 16];
        assert!(!store.note_relay_hint_sources(own.clone()).unwrap());
        store
            .advance_relay_fetch_cursor(cursor_key(), 29_000, true)
            .unwrap();
        store
            .upsert_imported_contact(contact(&[7u8; 16], "Newcomer"))
            .unwrap();

        assert!(store.note_relay_hint_sources(own.clone()).unwrap());
        assert_eq!(store.relay_fetch_cursor(cursor_key()).unwrap().after_id, 0);
        // The digest committed with it: the same set does not re-walk again.
        assert!(!store.note_relay_hint_sources(own).unwrap());
    }

    #[test]
    fn a_group_we_are_not_in_is_not_one_of_our_hint_sources() {
        // `relay_self_hints` only contributes groups we are a member of, so a
        // group imported without us in it must not spend a re-walk.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let own = vec![9u8; 16];
        assert!(!store.note_relay_hint_sources(own.clone()).unwrap());
        store
            .advance_relay_fetch_cursor(cursor_key(), 29_000, true)
            .unwrap();
        store
            .upsert_group(Group {
                id: vec![6u8; 16],
                name: "Someone else's group".into(),
                member_user_ids: vec![vec![7u8; 16], vec![8u8; 16]],
                key: vec![3u8; 32],
                metadata_revision: 1,
                metadata_changed_by: vec![7u8; 16],
            })
            .unwrap();
        assert!(!store.note_relay_hint_sources(own).unwrap());
        assert_eq!(
            store.relay_fetch_cursor(cursor_key()).unwrap().after_id,
            29_000
        );
    }

    #[test]
    fn a_backup_snapshot_drops_courier_rows_but_keeps_the_relay_frontier() {
        // Dropping the frontier used to force an immediate walk from zero,
        // which re-downloaded proxy mail into the carry queue the backup had
        // just discarded. Preserve it; scheduled sweeps repair a stale one.
        let dir = std::env::temp_dir().join(format!(
            "cruisemesh-cursor-backup-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("snapshot.sqlite");
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat", b"alice", 1, "hello"))
            .unwrap();
        store
            .advance_relay_fetch_cursor(cursor_key(), 9_000, true)
            .unwrap();
        store
            .note_relay_sweep_completed(cursor_key(), 1_000_000, 9_000)
            .unwrap();
        store
            .enqueue_relay_carried_envelope(
                CarriedEnvelope {
                    msg_id: b"carried-envelope".to_vec(),
                    hop_ttl: 7,
                    expiry: 2_000_000,
                    recipient_hint: b"hint".to_vec(),
                    sealed: b"sealed".to_vec(),
                },
                900_000,
            )
            .unwrap();

        store.backup_to(path.to_string_lossy().to_string()).unwrap();
        let restored = MessageStore::open(path.to_string_lossy().to_string()).unwrap();
        // History survives...
        assert_eq!(
            restored.messages_for_chat(b"chat".to_vec()).unwrap().len(),
            1
        );
        assert_eq!(restored.carried_len().unwrap(), 0);
        // ...and a recent frontier prevents an immediate mailbox replay.
        let cursor = restored.relay_fetch_cursor(cursor_key()).unwrap();
        assert_eq!(cursor.after_id, 9_000);
        assert_eq!(cursor.last_sweep_at_ms, 1_000_000);
        assert!(!crate::relay_sweep_due(
            false,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            1_000_001
        ));
        // And taking the backup did not cost the live store its frontier.
        assert_eq!(
            store.relay_fetch_cursor(cursor_key()).unwrap().after_id,
            9_000
        );
        drop(restored);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn selectable_backup_keeps_continuity_without_history_and_prunes_opted_in_cargo() {
        let dir = std::env::temp_dir().join(format!(
            "cruisemesh-selectable-backup-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("snapshot.sqlite");
        let now_ms = 1_700_000_000_500;
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut alice = contact(b"alice-id", "Alice");
        alice.relay_url = Some("https://relay.example".into());
        store.upsert_contact(alice).unwrap();
        let authored = msg(b"alice-id", b"me", 777, "private history");
        let outbound = outbound_for(&authored, b"alice-id", b"msg-000000000777");
        store
            .insert_outgoing_message(authored, outbound, now_ms - 100)
            .unwrap();
        store
            .record_peer_connection_event(
                b"alice-id".to_vec(),
                PeerConnectionTransport::Bluetooth,
                PeerConnectionEventKind::Connected,
                now_ms - 50,
            )
            .unwrap();
        {
            let conn = lock_conn(&store.conn);
            conn.execute(
                "INSERT INTO authored_lamport_watermarks
                    (chat_id, sender_user_id, high_lamport) VALUES (?1, ?2, 777)",
                params![b"alice-id".as_slice(), b"me".as_slice()],
            )
            .unwrap();
            conn.execute(
                "UPDATE contacts SET relay_reject_streak = 4, relay_rejected_at = ?1,
                     relay_unreachable_endpoint_key = 'stale', relay_unreachable_streak = 3,
                     relay_unreachable_at = ?1 WHERE user_id = ?2",
                params![now_ms - 1, b"alice-id".as_slice()],
            )
            .unwrap();
            for (id, expiry, bytes) in [
                (b"active-courier-1".as_slice(), now_ms + 10_000, 111_i64),
                (b"expired-courier".as_slice(), now_ms, 222_i64),
            ] {
                conn.execute(
                    "INSERT INTO carried_envelopes
                        (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family,
                         received_at, size_bytes, from_relay, content_digest)
                     VALUES (?1, 7, ?2, X'01', zeroblob(?3), 1, ?4, ?3, 0, ?1)",
                    params![id, expiry, bytes, now_ms - 200],
                )
                .unwrap();
            }
        }

        assert_eq!(
            store.backup_inventory(now_ms).unwrap(),
            BackupInventory {
                contact_count: 1,
                message_count: 1,
                message_bytes: "private history".len() as u64,
                pending_own_delivery_count: 1,
                pending_own_delivery_bytes: "sealed-777".len() as u64,
                pending_courier_delivery_count: 1,
                pending_courier_delivery_bytes: 111,
                ..BackupInventory::default()
            }
        );

        let report = store
            .backup_to_with_options(
                path.to_string_lossy().to_string(),
                BackupContentOptions {
                    include_message_history: false,
                    include_pending_deliveries_for_others: true,
                },
                now_ms,
            )
            .unwrap();
        assert_eq!(report.removed_message_count, 1);
        assert_eq!(report.removed_pending_own_delivery_count, 1);
        assert_eq!(report.removed_courier_delivery_count, 0);
        assert_eq!(report.removed_expired_delivery_count, 1);
        assert_eq!(report.removed_connection_event_count, 2);

        let restored = MessageStore::open(path.to_string_lossy().to_string()).unwrap();
        assert!(restored
            .messages_for_chat(b"alice-id".to_vec())
            .unwrap()
            .is_empty());
        assert!(restored
            .outbound_envelopes_after(b"alice-id".to_vec(), b"me".to_vec(), 0)
            .unwrap()
            .is_empty());
        assert_eq!(restored.carried_len().unwrap(), 1);
        assert_eq!(restored.list_contacts().unwrap().len(), 1);
        let conn = lock_conn(&restored.conn);
        let authored_high: i64 = conn
            .query_row(
                "SELECT high_lamport FROM authored_lamport_watermarks
                 WHERE chat_id = ?1 AND sender_user_id = ?2",
                params![b"alice-id".as_slice(), b"me".as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(authored_high, 777);
        let transient: (i64, i64, Option<String>) = conn
            .query_row(
                "SELECT relay_reject_streak, relay_unreachable_streak,
                        relay_unreachable_endpoint_key
                 FROM contacts WHERE user_id = ?1",
                params![b"alice-id".as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(transient, (0, 0, None));
        drop(conn);
        drop(restored);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn backup_content_options_cover_all_four_combinations() {
        let dir = std::env::temp_dir().join(format!(
            "cruisemesh-backup-options-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let now_ms = 1_700_000_000_500;

        for (index, include_history, include_courier) in [
            (0, false, false),
            (1, false, true),
            (2, true, false),
            (3, true, true),
        ] {
            let path = dir.join(format!("snapshot-{index}.sqlite"));
            let store = MessageStore::open(":memory:".to_string()).unwrap();
            let history = msg(b"alice-id", b"me", 8, "history");
            store.insert_message(history.clone()).unwrap();
            let mut conflict = history;
            conflict.payload = b"conflicting restored history".to_vec();
            assert!(!store.insert_message(conflict).unwrap());
            {
                let conn = lock_conn(&store.conn);
                conn.execute(
                    "INSERT INTO authored_lamport_watermarks
                        (chat_id, sender_user_id, high_lamport) VALUES (?1, ?2, 8)",
                    params![b"alice-id".as_slice(), b"me".as_slice()],
                )
                .unwrap();
                conn.execute(
                    "INSERT INTO carried_envelopes
                        (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family,
                         received_at, size_bytes, from_relay, content_digest)
                     VALUES (X'01020304', 7, ?1, X'01', X'02', 1, ?2, 1, 0, X'03')",
                    params![now_ms + 10_000, now_ms - 1],
                )
                .unwrap();
            }

            store
                .backup_to_with_options(
                    path.to_string_lossy().to_string(),
                    BackupContentOptions {
                        include_message_history: include_history,
                        include_pending_deliveries_for_others: include_courier,
                    },
                    now_ms,
                )
                .unwrap();
            let restored = MessageStore::open(path.to_string_lossy().to_string()).unwrap();
            assert_eq!(
                !restored
                    .messages_for_chat(b"alice-id".to_vec())
                    .unwrap()
                    .is_empty(),
                include_history
            );
            assert_eq!(restored.carried_len().unwrap() == 1, include_courier);
            assert_eq!(restored.has_message_conflicts().unwrap(), include_history);
            let authored_high: i64 = lock_conn(&restored.conn)
                .query_row(
                    "SELECT high_lamport FROM authored_lamport_watermarks
                     WHERE chat_id = ?1 AND sender_user_id = ?2",
                    params![b"alice-id".as_slice(), b"me".as_slice()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(authored_high, 8);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_full_database_restore_discards_cargo_and_expired_runtime_rows() {
        let dir = std::env::temp_dir().join(format!(
            "cruisemesh-legacy-restore-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("restored.sqlite");
        let path_string = path.to_string_lossy().to_string();
        let store = MessageStore::open(path_string.clone()).unwrap();

        let alice = contact(b"alice-id", "Alice");
        store.upsert_contact(alice.clone()).unwrap();
        let authored = msg(b"alice-id", b"me", 777, "user-owned history");
        let outbound = outbound_for(&authored, b"alice-id", b"msg-000000000777");
        store
            .insert_outgoing_message(authored.clone(), outbound.clone(), 1_700_000_000_100)
            .unwrap();
        store
            .record_receipt(
                b"alice-id".to_vec(),
                b"me".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                777,
                Some(2),
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(
                b"alice-id".to_vec(),
                b"alice-id".to_vec(),
                RECEIPT_TYPE_READ,
                42,
            )
            .unwrap();
        let own_id = vec![9u8; 16];
        assert!(!store.note_relay_hint_sources(own_id.clone()).unwrap());
        store
            .advance_relay_fetch_cursor(cursor_key(), 9_000, true)
            .unwrap();
        store
            .note_relay_sweep_completed(cursor_key(), 1_000_000, 9_000)
            .unwrap();

        {
            let mut conn = lock_conn(&store.conn);
            let tx = conn.transaction().unwrap();
            tx.execute(
                "INSERT INTO authored_lamport_watermarks
                    (chat_id, sender_user_id, high_lamport) VALUES (?1, ?2, 777)",
                params![b"alice-id".as_slice(), b"me".as_slice()],
            )
            .unwrap();
            for i in 0u64..400 {
                let msg_id = [i.to_be_bytes(), i.to_le_bytes()].concat();
                tx.execute(
                    "INSERT INTO carried_envelopes
                        (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family,
                         received_at, size_bytes, from_relay, content_digest)
                     VALUES (?1, 7, 2000000, ?2, ?3, 1, ?4, 32, 1, ?5)",
                    params![
                        msg_id,
                        b"proxy-hint".as_slice(),
                        format!("restored-ciphertext-{i}").into_bytes(),
                        900_000 + i as i64,
                        i.to_be_bytes().to_vec(),
                    ],
                )
                .unwrap();
            }
            tx.commit().unwrap();
        }
        assert_eq!(store.carried_len().unwrap(), 400);
        drop(store);

        assert_eq!(
            sanitize_restored_message_store(path_string.clone()).unwrap(),
            400
        );
        let restored = MessageStore::open(path_string).unwrap();
        assert_eq!(restored.carried_len().unwrap(), 0);
        assert_eq!(
            restored.get_contact(b"alice-id".to_vec()).unwrap(),
            Some(alice)
        );
        assert_eq!(
            restored.messages_for_chat(b"alice-id".to_vec()).unwrap(),
            vec![authored]
        );
        assert!(restored
            .outbound_envelopes_after(b"alice-id".to_vec(), b"me".to_vec(), 0)
            .unwrap()
            .is_empty());
        assert_eq!(
            restored
                .receipt_through(b"alice-id".to_vec(), b"me".to_vec(), RECEIPT_TYPE_DELIVERED,)
                .unwrap(),
            777
        );
        assert_eq!(
            restored
                .outgoing_receipt_through(
                    b"alice-id".to_vec(),
                    b"alice-id".to_vec(),
                    RECEIPT_TYPE_READ,
                )
                .unwrap(),
            42
        );
        let authored_high: i64 = lock_conn(&restored.conn)
            .query_row(
                "SELECT high_lamport FROM authored_lamport_watermarks
                 WHERE chat_id = ?1 AND sender_user_id = ?2",
                params![b"alice-id".as_slice(), b"me".as_slice()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(authored_high, 777);
        let cursor = restored.relay_fetch_cursor(cursor_key()).unwrap();
        assert_eq!(cursor.after_id, 9_000);
        assert_eq!(cursor.last_sweep_at_ms, 1_000_000);
        assert!(!crate::relay_sweep_due(
            false,
            cursor.last_sweep_at_ms,
            cursor.sweep_after_id,
            1_000_001
        ));
        assert!(
            !restored.note_relay_hint_sources(own_id).unwrap(),
            "preserving the hint digest must not invalidate the preserved frontier"
        );

        drop(restored);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn contact_relay_rejections_accumulate_and_clear_on_success() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut alice = contact(b"alice-id", "Alice");
        alice.relay_url = Some("https://dead.example".to_string());
        alice.relay_token = Some("tok".to_string());
        store.upsert_contact(alice).unwrap();

        assert!(store.list_contact_relay_rejections().unwrap().is_empty());
        assert_eq!(
            store
                .note_contact_relay_rejected(b"alice-id".to_vec(), 1_000)
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .note_contact_relay_rejected(b"alice-id".to_vec(), 2_000)
                .unwrap(),
            2
        );

        let rejections = store.list_contact_relay_rejections().unwrap();
        assert_eq!(rejections.len(), 1);
        assert_eq!(rejections[0].reject_streak, 2);
        // Re-stamped by the newest evidence, not pinned to the first failure.
        assert_eq!(rejections[0].rejected_at_ms, 2_000);

        store
            .clear_contact_relay_rejection(b"alice-id".to_vec())
            .unwrap();
        assert!(store.list_contact_relay_rejections().unwrap().is_empty());
    }

    #[test]
    fn contact_relay_silence_survives_reopen_and_tracks_the_endpoint() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("cruisemesh-contact-relay-silence-{unique}.sqlite"));
        let path_str = path.to_string_lossy().to_string();
        let mut alice = contact(b"alice-id", "Alice");
        alice.relay_url = Some("https://dead.example".to_string());
        alice.relay_token = Some("tok".to_string());

        let store = MessageStore::open(path_str.clone()).unwrap();
        store.upsert_imported_contact(alice.clone()).unwrap();
        assert_eq!(
            store
                .note_contact_relay_unreachable(
                    b"alice-id".to_vec(),
                    "dead-endpoint-key".to_string(),
                    1_000,
                )
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .note_contact_relay_unreachable(
                    b"alice-id".to_vec(),
                    "dead-endpoint-key".to_string(),
                    2_000,
                )
                .unwrap(),
            2
        );
        drop(store);

        // A process restart must resume the rest instead of re-arming the dead
        // endpoint from zero. Re-importing the unchanged card also preserves
        // that verdict.
        let store = MessageStore::open(path_str).unwrap();
        assert_eq!(
            store.list_contact_relay_unreachable().unwrap(),
            vec![ContactRelayUnreachable {
                user_id: b"alice-id".to_vec(),
                endpoint_key: "dead-endpoint-key".to_string(),
                unreachable_streak: 2,
                unreachable_at_ms: 2_000,
            }]
        );
        store.upsert_imported_contact(alice).unwrap();
        assert_eq!(
            store.list_contact_relay_unreachable().unwrap()[0].unreachable_streak,
            2
        );

        // A different endpoint has never failed and starts its own streak.
        assert_eq!(
            store
                .note_contact_relay_unreachable(
                    b"alice-id".to_vec(),
                    "new-endpoint-key".to_string(),
                    3_000,
                )
                .unwrap(),
            1
        );
        store
            .clear_contact_relay_unreachable(b"alice-id".to_vec())
            .unwrap();
        assert!(store.list_contact_relay_unreachable().unwrap().is_empty());

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn re_importing_the_same_stale_card_does_not_launder_the_streak() {
        // The field repair is "ask them to share their card again" -- but a
        // card re-shared from a phone whose config never changed carries the
        // SAME dead endpoint. Clearing the streak for it would restart the
        // hammering and make the repair look like it worked.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut alice = contact(b"alice-id", "Alice");
        alice.relay_url = Some("https://dead.example".to_string());
        alice.relay_token = Some("tok".to_string());
        store.upsert_imported_contact(alice.clone()).unwrap();
        store
            .note_contact_relay_rejected(b"alice-id".to_vec(), 1_000)
            .unwrap();
        store
            .note_contact_relay_rejected(b"alice-id".to_vec(), 2_000)
            .unwrap();
        store
            .note_contact_relay_unreachable(
                b"alice-id".to_vec(),
                "dead-endpoint-key".to_string(),
                2_000,
            )
            .unwrap();

        store.upsert_imported_contact(alice).unwrap();
        assert_eq!(
            store.list_contact_relay_rejections().unwrap()[0].reject_streak,
            2
        );
        assert_eq!(
            store.list_contact_relay_unreachable().unwrap()[0].unreachable_streak,
            1
        );

        // A card that actually moves the endpoint has never been tried, so
        // it starts trusted.
        let mut moved = contact(b"alice-id", "Alice");
        moved.relay_url = Some("https://live.example".to_string());
        moved.relay_token = Some("tok".to_string());
        store.upsert_imported_contact(moved).unwrap();
        assert!(store.list_contact_relay_rejections().unwrap().is_empty());
        assert!(store.list_contact_relay_unreachable().unwrap().is_empty());
    }

    #[test]
    fn a_relay_update_notice_clears_the_streak() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut alice = contact(b"alice-id", "Alice");
        alice.relay_url = Some("https://dead.example".to_string());
        alice.relay_token = Some("tok".to_string());
        store.upsert_contact(alice).unwrap();
        store
            .note_contact_relay_rejected(b"alice-id".to_vec(), 1_000)
            .unwrap();
        store
            .note_contact_relay_rejected(b"alice-id".to_vec(), 2_000)
            .unwrap();
        store
            .note_contact_relay_unreachable(
                b"alice-id".to_vec(),
                "old-endpoint-key".to_string(),
                2_000,
            )
            .unwrap();

        assert!(store
            .apply_contact_relay_update(
                b"alice-id".to_vec(),
                relay_notice(b"alice-id", 5, "https://live.example")
            )
            .unwrap());
        assert!(store.list_contact_relay_rejections().unwrap().is_empty());
        assert!(store.list_contact_relay_unreachable().unwrap().is_empty());
    }

    #[test]
    fn imported_blank_card_preserves_existing_relay_details() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut original = contact(b"alice-id", "Alice");
        original.relay_url = Some("https://relay.example".to_string());
        original.relay_token = Some("family-token".to_string());
        store.upsert_contact(original).unwrap();

        let imported = store
            .upsert_imported_contact(contact(b"alice-id", "Alice Renamed"))
            .unwrap();
        assert_eq!(imported.relay_url.as_deref(), Some("https://relay.example"));
        assert_eq!(imported.relay_token.as_deref(), Some("family-token"));
        assert_eq!(
            store.get_contact(b"alice-id".to_vec()).unwrap(),
            Some(imported)
        );
    }

    #[test]
    fn imported_complete_relay_replaces_existing_details() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut original = contact(b"alice-id", "Alice");
        original.relay_url = Some("https://old.example".to_string());
        original.relay_token = Some("old".to_string());
        store.upsert_contact(original).unwrap();

        let mut incoming = contact(b"alice-id", "Alice");
        incoming.relay_url = Some("https://new.example".to_string());
        incoming.relay_token = Some("new".to_string());
        let imported = store.upsert_imported_contact(incoming).unwrap();
        assert_eq!(imported.relay_url.as_deref(), Some("https://new.example"));
        assert_eq!(imported.relay_token.as_deref(), Some("new"));
    }

    #[test]
    fn set_contact_avatar_applies_only_newer_epochs() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        assert!(store
            .set_contact_avatar(b"alice-id".to_vec(), Some(vec![1]), 100)
            .unwrap());
        assert!(!store
            .set_contact_avatar(b"alice-id".to_vec(), Some(vec![2]), 99)
            .unwrap());
        assert!(!store
            .set_contact_avatar(b"alice-id".to_vec(), Some(vec![3]), 100)
            .unwrap());
        assert_eq!(
            store.contact_avatar(b"alice-id".to_vec()).unwrap(),
            Some(vec![1])
        );
        assert_eq!(
            store.contact_avatar_epoch(b"alice-id".to_vec()).unwrap(),
            100
        );

        assert!(store
            .set_contact_avatar(b"alice-id".to_vec(), Some(Vec::new()), 101)
            .unwrap());
        assert_eq!(store.contact_avatar(b"alice-id".to_vec()).unwrap(), None);
        assert_eq!(
            store.contact_avatar_epoch(b"alice-id".to_vec()).unwrap(),
            101
        );
    }

    #[test]
    fn set_contact_avatar_unknown_contact_is_noop() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(!store
            .set_contact_avatar(b"nobody".to_vec(), Some(vec![1]), 1)
            .unwrap());
        assert_eq!(store.contact_avatar_epoch(b"nobody".to_vec()).unwrap(), 0);
        assert_eq!(store.contact_avatar(b"nobody".to_vec()).unwrap(), None);
    }

    #[test]
    fn list_contacts_is_alphabetical() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"bob-id", "Bob")).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        let names: Vec<String> = store
            .list_contacts()
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, vec!["Alice".to_string(), "Bob".to_string()]);
    }

    #[test]
    fn delete_contact_removes_the_contact() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();

        assert!(store.delete_contact(b"alice-id".to_vec()).unwrap());
        assert_eq!(store.get_contact(b"alice-id".to_vec()).unwrap(), None);
        assert!(store.list_contacts().unwrap().is_empty());
    }

    #[test]
    fn delete_contact_is_a_noop_for_unknown_contact() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(!store.delete_contact(b"nobody".to_vec()).unwrap());
        // Deleting twice is idempotent, not an error.
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        assert!(store.delete_contact(b"alice-id".to_vec()).unwrap());
        assert!(!store.delete_contact(b"alice-id".to_vec()).unwrap());
    }

    #[test]
    fn delete_contact_deletes_the_1to1_chat_messages_and_receipts() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        // 1:1 chat_id = the peer's UserID (DESIGN.md §7.1): both directions live under it.
        store
            .insert_message(msg(b"alice-id", b"alice-id", 1, "from alice"))
            .unwrap();
        store
            .insert_message(msg(b"alice-id", b"me", 1, "from me"))
            .unwrap();
        store
            .record_receipt(
                b"alice-id".to_vec(),
                b"me".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                1,
                None,
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(
                b"alice-id".to_vec(),
                b"alice-id".to_vec(),
                crate::RECEIPT_TYPE_READ,
                1,
            )
            .unwrap();
        store
            .record_consumed_hidden_lamport(
                b"alice-id".to_vec(),
                b"alice-id".to_vec(),
                2,
                crate::KIND_RECEIPT,
            )
            .unwrap();

        assert!(store.delete_contact(b"alice-id".to_vec()).unwrap());

        assert!(store
            .messages_for_chat(b"alice-id".to_vec())
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .receipt_through(
                    b"alice-id".to_vec(),
                    b"me".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"alice-id".to_vec(),
                    b"alice-id".to_vec(),
                    crate::RECEIPT_TYPE_READ,
                )
                .unwrap(),
            0
        );
        assert!(store
            .consumed_hidden_lamports(b"alice-id".to_vec())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn delete_contact_leaves_other_contacts_and_chats_alone() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        store.upsert_contact(contact(b"bob-id", "Bob")).unwrap();
        store
            .insert_message(msg(b"alice-id", b"alice-id", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"bob-id", b"bob-id", 1, "yo"))
            .unwrap();
        // A group chat where alice posted: her group messages belong to the
        // group's chat_id, not to her contact, and must survive.
        store
            .insert_message(msg(b"group-1", b"alice-id", 1, "group msg"))
            .unwrap();

        assert!(store.delete_contact(b"alice-id".to_vec()).unwrap());

        assert_eq!(store.list_contacts().unwrap().len(), 1);
        assert_eq!(
            store.messages_for_chat(b"bob-id".to_vec()).unwrap().len(),
            1
        );
        assert_eq!(
            store.messages_for_chat(b"group-1".to_vec()).unwrap().len(),
            1
        );
        assert!(store
            .messages_for_chat(b"alice-id".to_vec())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn delete_contact_purges_all_per_chat_state_and_leaves_other_chat_alone() {
        // Regression test for the silent-blackhole-adjacent bug: a delete
        // that only cleared `messages`/`receipts`/`outgoing_receipts` left
        // `outgoing_receipt_envelopes` and `outbound_envelopes` behind,
        // re-arming the reset-stream trap fixed in fc6b9f9 and leaving a
        // "deleted" chat that could still resend frames from its erased
        // history. Seed every one of the five per-chat tables for two
        // contacts, delete one, and verify a genuinely blank slate for it
        // while the other contact's chat is untouched.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        store.upsert_contact(contact(b"bob-id", "Bob")).unwrap();

        // Alice's chat: one message from her, a receipt she gave us, an
        // outgoing receipt watermark, its queued envelope, and one queued
        // outbound message envelope.
        store
            .insert_message(msg(b"alice-id", b"alice-id", 1, "hi from alice"))
            .unwrap();
        store
            .record_receipt(
                b"alice-id".to_vec(),
                b"me".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(
                b"alice-id".to_vec(),
                b"alice-id".to_vec(),
                RECEIPT_TYPE_READ,
                1,
            )
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(
                outgoing_receipt_for(
                    b"alice-id",
                    b"alice-id",
                    b"alice-id",
                    RECEIPT_TYPE_READ,
                    1,
                    b"rcpt-alice-1",
                ),
                1_700_000_000_100,
            )
            .unwrap();
        let alice_outgoing = msg(b"alice-id", b"me", 1, "reply to alice");
        store
            .insert_outgoing_message(
                alice_outgoing.clone(),
                outbound_for(&alice_outgoing, b"alice-id", b"msg-alice-out-1"),
                1_700_000_000_200,
            )
            .unwrap();

        // Bob's chat: the identical shape of state, to prove it survives.
        store
            .insert_message(msg(b"bob-id", b"bob-id", 1, "hi from bob"))
            .unwrap();
        store
            .record_receipt(
                b"bob-id".to_vec(),
                b"me".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(b"bob-id".to_vec(), b"bob-id".to_vec(), RECEIPT_TYPE_READ, 1)
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(
                outgoing_receipt_for(
                    b"bob-id",
                    b"bob-id",
                    b"bob-id",
                    RECEIPT_TYPE_READ,
                    1,
                    b"rcpt-bob-1",
                ),
                1_700_000_000_100,
            )
            .unwrap();
        let bob_outgoing = msg(b"bob-id", b"me", 1, "reply to bob");
        let bob_outbound = outbound_for(&bob_outgoing, b"bob-id", b"msg-bob-out-1");
        store
            .insert_outgoing_message(
                bob_outgoing.clone(),
                bob_outbound.clone(),
                1_700_000_000_200,
            )
            .unwrap();

        assert!(store.delete_contact(b"alice-id".to_vec()).unwrap());

        // All five per-chat tables are empty for alice's chat_id.
        assert!(store
            .messages_for_chat(b"alice-id".to_vec())
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .receipt_through(b"alice-id".to_vec(), b"me".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"alice-id".to_vec(),
                    b"alice-id".to_vec(),
                    RECEIPT_TYPE_READ
                )
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_envelope(
                    b"alice-id".to_vec(),
                    b"alice-id".to_vec(),
                    RECEIPT_TYPE_READ
                )
                .unwrap(),
            None
        );
        assert!(store
            .outbound_envelopes_after(b"alice-id".to_vec(), b"me".to_vec(), 0)
            .unwrap()
            .is_empty());

        // Bob's chat is untouched in every one of the same five tables
        // (2 messages: bob's incoming one plus our outgoing reply).
        assert_eq!(
            store.messages_for_chat(b"bob-id".to_vec()).unwrap().len(),
            2
        );
        assert_eq!(
            store
                .receipt_through(b"bob-id".to_vec(), b"me".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .outgoing_receipt_through(b"bob-id".to_vec(), b"bob-id".to_vec(), RECEIPT_TYPE_READ)
                .unwrap(),
            1
        );
        assert!(store
            .outgoing_receipt_envelope(b"bob-id".to_vec(), b"bob-id".to_vec(), RECEIPT_TYPE_READ)
            .unwrap()
            .is_some());
        assert_eq!(
            store
                .outbound_envelopes_after(b"bob-id".to_vec(), b"me".to_vec(), 0)
                .unwrap(),
            vec![bob_outbound],
        );
    }

    // --- groups (DESIGN.md §6.5) ------------------------------------------

    #[test]
    fn upsert_then_get_group_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let group = group(0x11, "Family", 0x22, &[b"carol", b"alice", b"alice"]);
        store.upsert_group(group.clone()).unwrap();

        assert_eq!(
            store.get_group(group.id.clone()).unwrap(),
            Some(Group {
                id: group.id,
                name: "Family".to_string(),
                member_user_ids: vec![test_user_id(b"alice"), test_user_id(b"carol")],
                key: vec![0x22; 32],
                metadata_revision: 0,
                metadata_changed_by: Vec::new(),
            })
        );
    }

    #[test]
    fn upsert_group_replaces_key_and_members_for_rotation() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut rotated = group(0x11, "Bridge", 0x22, &[b"alice", b"bob"]);
        store.upsert_group(rotated.clone()).unwrap();
        rotated.key = vec![0x33; 32];
        rotated.member_user_ids = vec![test_user_id(b"alice"), test_user_id(b"dave")];

        store.upsert_group(rotated.clone()).unwrap();

        assert_eq!(store.get_group(rotated.id.clone()).unwrap(), Some(rotated));
    }

    #[test]
    fn stale_group_invite_cannot_roll_back_newer_metadata() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let stale = group(0x11, "Old name", 0x22, &[b"alice", b"bob"]);
        let mut current = stale.clone();
        current.name = "New name".to_string();
        current.member_user_ids.push(test_user_id(b"carol"));
        current.metadata_revision = 4;
        current.metadata_changed_by = test_user_id(b"alice");
        store.upsert_group(current.clone()).unwrap();

        store.upsert_group(stale).unwrap();

        assert_eq!(store.get_group(current.id.clone()).unwrap(), Some(current));
    }

    #[test]
    fn list_groups_orders_by_name() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .upsert_group(group(0x22, "Zulu", 0x33, &[b"alice"]))
            .unwrap();
        store
            .upsert_group(group(0x11, "Alpha", 0x22, &[b"bob"]))
            .unwrap();

        let names: Vec<String> = store
            .list_groups()
            .unwrap()
            .into_iter()
            .map(|group| group.name)
            .collect();
        assert_eq!(names, vec!["Alpha".to_string(), "Zulu".to_string()]);
    }

    #[test]
    fn group_receipts_are_monotonic_and_isolated_from_one_to_one() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let family = group(0x11, "Family", 0x22, &[b"me", b"alice", b"bob"]);
        store.upsert_group(family.clone()).unwrap();
        let me = test_user_id(b"me");
        let alice = test_user_id(b"alice");
        let bob = test_user_id(b"bob");

        store
            .record_group_receipt(
                family.id.clone(),
                me.clone(),
                alice.clone(),
                RECEIPT_TYPE_DELIVERED,
                3,
                Some(0),
            )
            .unwrap();
        store
            .record_group_receipt(
                family.id.clone(),
                me.clone(),
                alice.clone(),
                RECEIPT_TYPE_DELIVERED,
                2,
                Some(3),
            )
            .unwrap();
        assert_eq!(
            store
                .group_receipt_through(
                    family.id.clone(),
                    me.clone(),
                    alice.clone(),
                    RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            3
        );
        assert_eq!(
            store
                .group_receipt_via_transport(
                    family.id.clone(),
                    me.clone(),
                    alice,
                    RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            Some(0)
        );
        assert_eq!(
            store
                .receipt_through(family.id.clone(), me.clone(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            0
        );

        store
            .record_group_receipt(
                family.id.clone(),
                me.clone(),
                bob.clone(),
                RECEIPT_TYPE_DELIVERED,
                3,
                Some(2),
            )
            .unwrap();
        store
            .record_group_receipt(
                family.id.clone(),
                me.clone(),
                bob,
                RECEIPT_TYPE_READ,
                1,
                None,
            )
            .unwrap();
        let state = store
            .group_receipt_state(
                family.id.clone(),
                me.clone(),
                family.member_user_ids.clone(),
            )
            .unwrap();
        assert_eq!(
            crate::core_group_tick_status_for(3, 1_700_000_000_000, me, state),
            crate::CoreTickStatus::Delivered
        );
    }

    #[test]
    fn adding_a_group_member_stamps_added_at_and_keeps_founders() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut family = group(0x11, "Family", 0x22, &[b"me", b"alice"]);
        store.upsert_group(family.clone()).unwrap();
        family.member_user_ids.push(test_user_id(b"carol"));
        family.metadata_revision = 1;
        family.metadata_changed_by = test_user_id(b"me");
        store.upsert_group(family.clone()).unwrap();

        let state = store
            .group_receipt_state(
                family.id.clone(),
                test_user_id(b"me"),
                family.member_user_ids,
            )
            .unwrap();
        let founder = state
            .members
            .iter()
            .find(|m| m.member_user_id == test_user_id(b"alice"))
            .unwrap();
        let joiner = state
            .members
            .iter()
            .find(|m| m.member_user_id == test_user_id(b"carol"))
            .unwrap();
        assert_eq!(founder.added_at_ms, 0);
        assert!(joiner.added_at_ms > 0);
    }

    #[test]
    fn group_chat_preview_excludes_late_joiners_from_watermarks() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut family = group(0x11, "Family", 0x22, &[b"me", b"alice"]);
        store.upsert_group(family.clone()).unwrap();
        let me = test_user_id(b"me");
        let alice = test_user_id(b"alice");
        store
            .insert_message(StoredMessage {
                chat_id: family.id.clone(),
                sender_user_id: me.clone(),
                lamport: 3,
                timestamp: 1_700_000_000_000,
                kind: crate::KIND_TEXT,
                payload: b"already read".to_vec(),
            })
            .unwrap();
        store
            .record_group_receipt(
                family.id.clone(),
                me.clone(),
                alice,
                RECEIPT_TYPE_DELIVERED,
                3,
                None,
            )
            .unwrap();
        store
            .record_group_receipt(
                family.id.clone(),
                me.clone(),
                test_user_id(b"alice"),
                RECEIPT_TYPE_READ,
                3,
                None,
            )
            .unwrap();
        let before = store.chat_preview(family.id.clone(), me.clone()).unwrap();
        assert_eq!(before.own_delivered_through, 3);
        assert_eq!(before.own_read_through, 3);

        family.member_user_ids.push(test_user_id(b"carol"));
        family.metadata_revision = 1;
        family.metadata_changed_by = me.clone();
        store.upsert_group(family.clone()).unwrap();
        let after = store.chat_preview(family.id, me).unwrap();
        assert_eq!(after.own_delivered_through, 3);
        assert_eq!(after.own_read_through, 3);
    }

    #[test]
    fn delete_group_removes_group_and_group_chat_state() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let group = group(0x11, "Family", 0x22, &[b"alice", b"bob"]);
        store.upsert_group(group.clone()).unwrap();
        store
            .insert_message(StoredMessage {
                chat_id: group.id.clone(),
                sender_user_id: b"alice".to_vec(),
                lamport: 1,
                timestamp: 1_700_000_000_000,
                kind: 1,
                payload: b"group hi".to_vec(),
            })
            .unwrap();
        store
            .record_receipt(
                group.id.clone(),
                b"alice".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(group.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_READ, 1)
            .unwrap();
        store
            .record_group_receipt(
                group.id.clone(),
                test_user_id(b"me"),
                test_user_id(b"alice"),
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
            )
            .unwrap();

        assert!(store.delete_group(group.id.clone()).unwrap());
        assert_eq!(store.get_group(group.id.clone()).unwrap(), None);
        assert!(store
            .messages_for_chat(group.id.clone())
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .receipt_through(group.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_through(group.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_READ)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .group_receipt_through(
                    group.id.clone(),
                    test_user_id(b"me"),
                    test_user_id(b"alice"),
                    RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn delete_group_purges_all_per_chat_state_and_leaves_other_group_alone() {
        // Mirrors delete_contact_purges_all_per_chat_state_and_leaves_other_chat_alone:
        // seed all five per-chat tables for two groups, delete one, verify a
        // blank slate for it and the other group's state untouched.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let group_a = group(0x11, "Family", 0x22, &[b"alice", b"bob"]);
        let group_b = group(0x33, "Crew", 0x44, &[b"carol", b"dave"]);
        store.upsert_group(group_a.clone()).unwrap();
        store.upsert_group(group_b.clone()).unwrap();

        store
            .insert_message(msg(&group_a.id, b"alice", 1, "hi group a"))
            .unwrap();
        store
            .record_receipt(
                group_a.id.clone(),
                b"alice".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(group_a.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_READ, 1)
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(
                outgoing_receipt_for(
                    &group_a.id,
                    b"alice",
                    b"alice",
                    RECEIPT_TYPE_READ,
                    1,
                    b"rcpt-a-1",
                ),
                1_700_000_000_100,
            )
            .unwrap();
        let a_outgoing = msg(&group_a.id, b"me", 1, "reply in group a");
        store
            .insert_outgoing_message(
                a_outgoing.clone(),
                outbound_for(&a_outgoing, b"alice", b"msg-a-out-1"),
                1_700_000_000_200,
            )
            .unwrap();

        store
            .insert_message(msg(&group_b.id, b"carol", 1, "hi group b"))
            .unwrap();
        store
            .record_receipt(
                group_b.id.clone(),
                b"carol".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                1,
                None,
                None,
            )
            .unwrap();
        store
            .record_outgoing_receipt(group_b.id.clone(), b"carol".to_vec(), RECEIPT_TYPE_READ, 1)
            .unwrap();
        store
            .upsert_outgoing_receipt_envelope(
                outgoing_receipt_for(
                    &group_b.id,
                    b"carol",
                    b"carol",
                    RECEIPT_TYPE_READ,
                    1,
                    b"rcpt-b-1",
                ),
                1_700_000_000_100,
            )
            .unwrap();
        let b_outgoing = msg(&group_b.id, b"me", 1, "reply in group b");
        let b_outbound = outbound_for(&b_outgoing, b"carol", b"msg-b-out-1");
        store
            .insert_outgoing_message(b_outgoing.clone(), b_outbound.clone(), 1_700_000_000_200)
            .unwrap();

        assert!(store.delete_group(group_a.id.clone()).unwrap());

        // All five per-chat tables are empty for group_a's chat_id.
        assert!(store
            .messages_for_chat(group_a.id.clone())
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .receipt_through(
                    group_a.id.clone(),
                    b"alice".to_vec(),
                    RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_through(group_a.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_READ)
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .outgoing_receipt_envelope(group_a.id.clone(), b"alice".to_vec(), RECEIPT_TYPE_READ)
                .unwrap(),
            None
        );
        assert!(store
            .outbound_envelopes_after(group_a.id.clone(), b"me".to_vec(), 0)
            .unwrap()
            .is_empty());

        // group_b's state is untouched in every one of the same five tables
        // (2 messages: carol's incoming one plus our outgoing reply).
        assert_eq!(
            store.messages_for_chat(group_b.id.clone()).unwrap().len(),
            2
        );
        assert_eq!(
            store
                .receipt_through(
                    group_b.id.clone(),
                    b"carol".to_vec(),
                    RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .outgoing_receipt_through(group_b.id.clone(), b"carol".to_vec(), RECEIPT_TYPE_READ)
                .unwrap(),
            1
        );
        assert!(store
            .outgoing_receipt_envelope(group_b.id.clone(), b"carol".to_vec(), RECEIPT_TYPE_READ)
            .unwrap()
            .is_some());
        assert_eq!(
            store
                .outbound_envelopes_after(group_b.id.clone(), b"me".to_vec(), 0)
                .unwrap(),
            vec![b_outbound],
        );
    }

    // --- receipts (DESIGN.md §7.2) -----------------------------------------

    #[test]
    fn receipt_through_is_zero_when_none_recorded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let through = store
            .receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
            )
            .unwrap();
        assert_eq!(through, 0);
    }

    #[test]
    fn record_receipt_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                5,
                None,
                None,
            )
            .unwrap();

        let through = store
            .receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
            )
            .unwrap();
        assert_eq!(through, 5);
    }

    #[test]
    fn record_receipt_is_monotonic_upward() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                5,
                None,
                None,
            )
            .unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
                None,
            )
            .unwrap();

        let through = store
            .receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
            )
            .unwrap();
        assert_eq!(through, 9);
    }

    #[test]
    fn record_receipt_never_regresses_on_a_lower_or_replayed_value() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
                None,
            )
            .unwrap();
        // A stale/replayed receipt (lower, or the same, value) must not undo progress.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                3,
                None,
                None,
            )
            .unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
                None,
            )
            .unwrap();

        let through = store
            .receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
            )
            .unwrap();
        assert_eq!(through, 9);
    }

    /// The receipt-repair lane reports an *uncapped* peer-stream watermark:
    /// MAX semantics, so it can legitimately sit above the highest lamport the
    /// acknowledging side holds a row for (a front gap from the lamport
    /// ratchet, or a kind -- like a group invite -- filed under another chat).
    /// The receiving side must absorb that harmlessly: monotonic as always,
    /// and never leaving anything it still owes unsendable.
    ///
    /// Since #283 "harmlessly" no longer means "changes nothing". The covered
    /// envelope is retired, because the watermark is the same proof the spray
    /// planner already seeks past and the same proof the UI already draws two
    /// ticks from -- keeping a row nobody will ever offer again was the defect.
    /// What must survive is the *ability to send it again*, and that is what
    /// this pins: the `messages` row stays, so the digest responder's backfill
    /// can re-seal the envelope for a peer whose contiguous digest later
    /// reports the hole.
    #[test]
    fn an_over_reported_watermark_retires_the_envelope_but_never_the_ability_to_resend() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let alice = crate::generate_identity();
        let bob = crate::generate_identity();
        let bob_contact = Contact {
            user_id: bob.user_id.clone(),
            name: "Bob".to_string(),
            sign_pk: bob.sign_pk.clone(),
            agree_pk: bob.agree_pk.clone(),
            relay_url: None,
            relay_token: None,
            nickname: None,
        };
        store.upsert_contact(bob_contact.clone()).unwrap();
        let authored = store
            .author_pairwise_message(
                alice.clone(),
                bob_contact.clone(),
                crate::KIND_TEXT,
                b"hello".to_vec(),
                None,
                1_700_000_000_000,
            )
            .unwrap();
        assert_eq!(
            store
                .outbound_envelopes_after(bob.user_id.clone(), alice.user_id.clone(), 0)
                .unwrap()
                .len(),
            1
        );

        // Bob acks far beyond anything Alice authored.
        store
            .record_receipt(
                bob.user_id.clone(),
                alice.user_id.clone(),
                crate::RECEIPT_TYPE_DELIVERED,
                9_999,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_through(
                    bob.user_id.clone(),
                    alice.user_id.clone(),
                    crate::RECEIPT_TYPE_DELIVERED,
                )
                .unwrap(),
            9_999
        );
        // The covered envelope is retired...
        assert!(store
            .outbound_envelopes_after(bob.user_id.clone(), alice.user_id.clone(), 0)
            .unwrap()
            .is_empty());
        // ...and the message it sealed is still here, so the digest responder
        // can rebuild the envelope on demand.
        let stored = store
            .messages_after(bob.user_id.clone(), alice.user_id.clone(), 0)
            .unwrap();
        assert_eq!(stored.len(), 1);
        let rebuilt = store
            .backfill_pairwise_envelope(alice.clone(), bob_contact.clone(), stored[0].clone(), None)
            .unwrap();
        assert_eq!(rebuilt.envelope.lamport, authored.envelope.lamport);
        assert_eq!(rebuilt.envelope.kind, crate::KIND_TEXT);
        assert!(!rebuilt.envelope.sealed.is_empty());

        // And an ordinary, correctly-sized receipt arriving afterwards still
        // cannot regress the watermark.
        store
            .record_receipt(
                bob.user_id.clone(),
                alice.user_id.clone(),
                crate::RECEIPT_TYPE_DELIVERED,
                1,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_through(
                    bob.user_id.clone(),
                    alice.user_id.clone(),
                    crate::RECEIPT_TYPE_DELIVERED,
                )
                .unwrap(),
            9_999
        );
    }

    #[test]
    fn record_receipt_records_and_advances_via_transport() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // First confirmation returned over relay (transport 2).
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                5,
                Some(2),
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            Some(2)
        );

        // A later confirmation that advances the watermark over BLE direct
        // (transport 0) updates the recorded route.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                Some(0),
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            Some(0)
        );
    }

    #[test]
    fn via_transport_is_kept_when_the_watermark_does_not_advance_or_route_is_unknown() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                Some(3), // local Wi-Fi confirmed the watermark first
                None,
            )
            .unwrap();
        // A re-sent receipt for the same watermark on a different link must not
        // overwrite the transport that first confirmed it.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                Some(2),
                None,
            )
            .unwrap();
        // A watermark-advancing receipt whose return route is unknown keeps the
        // last known route rather than clearing it.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                12,
                None,
                None,
            )
            .unwrap();

        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            12
        );
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            Some(3)
        );
    }

    #[test]
    fn via_transport_backfills_from_null_at_the_same_watermark_but_never_clears_afterward() {
        // FC4: the first receipt at a watermark can arrive with an unknown
        // route (via_transport = None). A later receipt confirming the *same*
        // watermark with a known route must fill the gap instead of being
        // permanently ignored -- but once a route is known, a later
        // unknown-route receipt (even a resend of the same watermark) must
        // never clear it back to unknown.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            None
        );

        // Same watermark, now with a known route (BLE direct, transport 0):
        // fills the previously-unknown route.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                Some(0),
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            9
        );
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            Some(0)
        );

        // A later receipt at the same watermark with an unknown route must
        // never clear the route that's now known.
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            Some(0)
        );
    }

    #[test]
    fn receipt_via_transport_is_none_when_unrecorded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(
            store
                .receipt_via_transport(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            None
        );
    }

    #[test]
    fn open_migrates_an_old_receipts_table_to_add_via_transport() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("cruisemesh-receipts-migration-{unique}.sqlite"));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE receipts (
                chat_id         BLOB NOT NULL,
                sender_user_id  BLOB NOT NULL,
                receipt_type    INTEGER NOT NULL,
                through_lamport INTEGER NOT NULL,
                PRIMARY KEY(chat_id, sender_user_id, receipt_type)
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO receipts (chat_id, sender_user_id, receipt_type, through_lamport)
             VALUES (?1, ?2, ?3, ?4)",
            params![b"alice-id".to_vec(), b"me".to_vec(), 1i64, 3i64],
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str.clone()).unwrap();
        // The pre-existing watermark survives the migration; its route is
        // unknown (the column was just added, defaulting to NULL).
        assert_eq!(
            store
                .receipt_through(b"alice-id".to_vec(), b"me".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            3
        );
        assert_eq!(
            store
                .receipt_via_transport(b"alice-id".to_vec(), b"me".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            None
        );
        // A newer confirmation that advances the watermark now records its route.
        store
            .record_receipt(
                b"alice-id".to_vec(),
                b"me".to_vec(),
                RECEIPT_TYPE_DELIVERED,
                5,
                Some(0),
                None,
            )
            .unwrap();
        assert_eq!(
            store
                .receipt_via_transport(b"alice-id".to_vec(), b"me".to_vec(), RECEIPT_TYPE_DELIVERED)
                .unwrap(),
            Some(0)
        );

        drop(store);
        let _ = std::fs::remove_file(&path_str);
    }

    #[test]
    fn receipt_types_are_independent() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
                None,
            )
            .unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
                4,
                None,
                None,
            )
            .unwrap();

        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            9
        );
        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_READ
                )
                .unwrap(),
            4
        );
    }

    #[test]
    fn receipts_are_independent_per_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
                None,
                None,
            )
            .unwrap();
        store
            .record_receipt(
                b"chat-b".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                2,
                None,
                None,
            )
            .unwrap();

        assert_eq!(
            store
                .receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            9
        );
        assert_eq!(
            store
                .receipt_through(
                    b"chat-b".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED
                )
                .unwrap(),
            2
        );
    }

    // --- delivery metrics / field export (V2) -----------------------------

    #[test]
    fn sent_then_delivered_metric_stamps_latency_and_route_for_the_covered_run() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        for (lamport, at) in [(1u64, 1_000i64), (2, 1_100), (3, 1_200)] {
            store
                .record_sent_metric(b"alice".to_vec(), lamport, at)
                .unwrap();
        }
        // A cumulative delivered receipt through lamport 2, returned over BLE
        // direct (transport 0), stamps messages 1 and 2 -- not 3.
        store
            .record_delivered_metric(b"alice".to_vec(), 2, 1_500, Some(0))
            .unwrap();

        let csv = store.export_delivery_metrics_csv().unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(
            lines[0],
            "direction,chat,lamport,sender,at_ms,delivered_at_ms,latency_ms,via_transport,arrival_transport,hop_count"
        );
        // Rows are ordered by direction then chat then lamport, so 1..=3
        // follow. "sent" rows carry an empty sender cell (see
        // `metric_sender_self`).
        assert!(lines[1].starts_with("sent,"));
        assert!(lines[1].ends_with(",1,,1000,1500,500,0,,"));
        assert!(lines[2].ends_with(",2,,1100,1500,400,0,,"));
        // Message 3 is beyond the watermark: no delivery yet.
        assert!(lines[3].ends_with(",3,,1200,,,,,"));
    }

    #[test]
    fn delivered_metric_keeps_the_first_confirmation() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_sent_metric(b"alice".to_vec(), 1, 1_000)
            .unwrap();
        store
            .record_delivered_metric(b"alice".to_vec(), 1, 1_500, Some(3))
            .unwrap();
        // A later receipt for the same message must not overwrite the first
        // confirmation's time or route.
        store
            .record_delivered_metric(b"alice".to_vec(), 1, 1_900, Some(0))
            .unwrap();

        let csv = store.export_delivery_metrics_csv().unwrap();
        let row = csv.lines().nth(1).unwrap();
        assert!(row.ends_with(",1,,1000,1500,500,3,,"), "row was: {row}");
    }

    #[test]
    fn record_message_arrival_logs_an_inbound_metric_once() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.insert_message(msg(b"bob", b"bob", 5, "hi")).unwrap();
        let first = MessageArrival {
            transport: 2,
            hops_taken: 3,
            received_at: 1_700_000_000_777,
        };
        assert!(store
            .record_message_arrival(b"bob".to_vec(), b"bob".to_vec(), 5, first)
            .unwrap());
        // A redundant later copy neither overwrites the arrival nor adds a row.
        let redundant = MessageArrival {
            transport: 0,
            hops_taken: 1,
            received_at: 1_700_000_009_999,
        };
        assert!(!store
            .record_message_arrival(b"bob".to_vec(), b"bob".to_vec(), 5, redundant)
            .unwrap());

        let csv = store.export_delivery_metrics_csv().unwrap();
        let received: Vec<&str> = csv.lines().filter(|l| l.starts_with("received,")).collect();
        assert_eq!(received.len(), 1);
        let bob_sender = hex_lower(&metric_sender_hash(b"bob"));
        assert!(
            received[0].ends_with(&format!(",5,{bob_sender},1700000000777,,,,2,3")),
            "row: {}",
            received[0]
        );
    }

    #[test]
    fn record_message_arrival_keys_on_sender_so_group_lamport_collisions_dont_drop_rows() {
        // FC1: in a group, every member has an independent lamport stream,
        // so two different senders routinely share `lamport = 1` in the same
        // chat. Before FC1 the primary key omitted the sender and the
        // second arrival silently vanished under INSERT OR IGNORE.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"group-1", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"group-1", b"carol", 1, "hey"))
            .unwrap();

        let alice_arrival = MessageArrival {
            transport: 0,
            hops_taken: 1,
            received_at: 1_700_000_001_000,
        };
        let carol_arrival = MessageArrival {
            transport: 3,
            hops_taken: 2,
            received_at: 1_700_000_002_000,
        };
        assert!(store
            .record_message_arrival(b"group-1".to_vec(), b"alice".to_vec(), 1, alice_arrival)
            .unwrap());
        assert!(store
            .record_message_arrival(b"group-1".to_vec(), b"carol".to_vec(), 1, carol_arrival)
            .unwrap());

        let csv = store.export_delivery_metrics_csv().unwrap();
        let received: Vec<&str> = csv.lines().filter(|l| l.starts_with("received,")).collect();
        // Both arrivals landed as distinct rows -- neither dropped the other.
        assert_eq!(received.len(), 2, "csv was:\n{csv}");
        let alice_sender = hex_lower(&metric_sender_hash(b"alice"));
        let carol_sender = hex_lower(&metric_sender_hash(b"carol"));
        assert_ne!(alice_sender, carol_sender);
        assert!(received
            .iter()
            .any(|r| r.contains(&format!(",1,{alice_sender},"))));
        assert!(received
            .iter()
            .any(|r| r.contains(&format!(",1,{carol_sender},"))));
    }

    #[test]
    fn metrics_export_hashes_the_chat_and_never_leaks_the_raw_id() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let chat = b"super-secret-contact-id".to_vec();
        store.record_sent_metric(chat.clone(), 1, 1_000).unwrap();
        let csv = store.export_delivery_metrics_csv().unwrap();
        assert!(!csv.contains("super-secret-contact-id"));
        // Same chat hashes stably to the same tag across calls.
        store.record_sent_metric(chat.clone(), 2, 1_100).unwrap();
        let tag1 = csv
            .lines()
            .nth(1)
            .unwrap()
            .split(',')
            .nth(1)
            .unwrap()
            .to_string();
        let csv2 = store.export_delivery_metrics_csv().unwrap();
        let tag2 = csv2.lines().nth(1).unwrap().split(',').nth(1).unwrap();
        assert_eq!(tag1, tag2);
    }

    #[test]
    fn empty_metrics_export_is_just_the_header() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let csv = store.export_delivery_metrics_csv().unwrap();
        assert_eq!(csv.lines().count(), 1);
        assert!(csv.starts_with("direction,chat,lamport,"));
    }

    #[test]
    fn clearing_metrics_leaves_the_export_empty() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_sent_metric(b"chat".to_vec(), 1, 1_000)
            .unwrap();
        store
            .record_sent_metric(b"chat".to_vec(), 2, 1_100)
            .unwrap();
        assert!(store.export_delivery_metrics_csv().unwrap().lines().count() > 1);

        store.clear_delivery_metrics().unwrap();

        let csv = store.export_delivery_metrics_csv().unwrap();
        assert_eq!(csv.lines().count(), 1, "only the header should remain");
        assert!(csv.starts_with("direction,chat,lamport,"));
    }

    #[test]
    fn has_metrics_answers_without_exporting() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(!store.has_delivery_metrics().unwrap());
        store
            .record_sent_metric(b"chat".to_vec(), 1, 1_000)
            .unwrap();
        assert!(store.has_delivery_metrics().unwrap());
        store.clear_delivery_metrics().unwrap();
        assert!(!store.has_delivery_metrics().unwrap());
    }

    #[test]
    fn clearing_metrics_is_safe_when_nothing_was_captured() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.clear_delivery_metrics().unwrap();
        assert_eq!(
            store.export_delivery_metrics_csv().unwrap().lines().count(),
            1
        );
    }

    #[test]
    fn clearing_metrics_keeps_recording_afterwards() {
        // The delete must not leave the table unusable: a tester who erases
        // diagnostics mid-cruise still needs the next messages measured.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_sent_metric(b"chat".to_vec(), 1, 1_000)
            .unwrap();
        store.clear_delivery_metrics().unwrap();
        store
            .record_sent_metric(b"chat".to_vec(), 2, 1_200)
            .unwrap();
        assert_eq!(
            store.export_delivery_metrics_csv().unwrap().lines().count(),
            2,
            "the post-clear row should be the only one"
        );
    }

    #[test]
    fn open_migrates_an_old_delivery_metrics_table_to_add_the_sender_key() {
        // FC1: pre-existing stores have `delivery_metrics` keyed on
        // (chat_hash, lamport, direction) only. Opening such a store must
        // not fail or panic; the disposable diagnostics table is dropped and
        // recreated with the sender-aware primary key (see
        // `migrate_delivery_metrics_schema`), and new arrivals -- including
        // the group lamport-collision case the old key couldn't hold -- work
        // afterward.
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cruisemesh-delivery-metrics-migration-{unique}.sqlite"
        ));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE delivery_metrics (
                chat_hash         BLOB NOT NULL,
                lamport           INTEGER NOT NULL,
                direction         INTEGER NOT NULL,
                at_ms             INTEGER NOT NULL,
                delivered_at_ms   INTEGER,
                via_transport     INTEGER,
                arrival_transport INTEGER,
                hop_count         INTEGER,
                PRIMARY KEY(chat_hash, lamport, direction)
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO delivery_metrics (chat_hash, lamport, direction, at_ms)
             VALUES (?1, ?2, 0, ?3)",
            params![vec![1u8; 8], 1i64, 1_000i64],
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str.clone()).unwrap();
        // Old-shape data is local, best-effort diagnostics: it's fine for the
        // pre-migration row to be gone, as long as the store opens cleanly
        // and the export still works.
        let csv = store.export_delivery_metrics_csv().unwrap();
        assert_eq!(csv.lines().count(), 1, "csv was:\n{csv}");

        // The migrated schema now tolerates a group lamport collision.
        store
            .insert_message(msg(b"group-1", b"alice", 1, "hi"))
            .unwrap();
        store
            .insert_message(msg(b"group-1", b"carol", 1, "hey"))
            .unwrap();
        assert!(store
            .record_message_arrival(
                b"group-1".to_vec(),
                b"alice".to_vec(),
                1,
                MessageArrival {
                    transport: 0,
                    hops_taken: 1,
                    received_at: 1_700_000_001_000,
                },
            )
            .unwrap());
        assert!(store
            .record_message_arrival(
                b"group-1".to_vec(),
                b"carol".to_vec(),
                1,
                MessageArrival {
                    transport: 3,
                    hops_taken: 2,
                    received_at: 1_700_000_002_000,
                },
            )
            .unwrap());
        let csv = store.export_delivery_metrics_csv().unwrap();
        let received = csv.lines().filter(|l| l.starts_with("received,")).count();
        assert_eq!(received, 2, "csv was:\n{csv}");

        let _ = std::fs::remove_file(&path);
    }

    // --- outgoing receipts (DESIGN.md §7.2, §7.3) -------------------------

    #[test]
    fn outgoing_receipt_through_is_zero_when_none_recorded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let through = store
            .outgoing_receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
            )
            .unwrap();
        assert_eq!(through, 0);
    }

    #[test]
    fn record_outgoing_receipt_round_trips_and_never_regresses() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_outgoing_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
                5,
            )
            .unwrap();
        store
            .record_outgoing_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
                3,
            )
            .unwrap();

        let through = store
            .outgoing_receipt_through(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
            )
            .unwrap();
        assert_eq!(through, 5);
    }

    #[test]
    fn outgoing_receipt_types_are_independent() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .record_outgoing_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_DELIVERED,
                9,
            )
            .unwrap();
        store
            .record_outgoing_receipt(
                b"chat-a".to_vec(),
                b"alice".to_vec(),
                crate::RECEIPT_TYPE_READ,
                4,
            )
            .unwrap();

        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_DELIVERED,
                )
                .unwrap(),
            9
        );
        assert_eq!(
            store
                .outgoing_receipt_through(
                    b"chat-a".to_vec(),
                    b"alice".to_vec(),
                    crate::RECEIPT_TYPE_READ,
                )
                .unwrap(),
            4
        );
    }

    // --- sync digests (DESIGN.md §7.3) -------------------------------------

    #[test]
    fn chat_digest_is_empty_for_unknown_chat() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert_eq!(store.chat_digest(b"chat-a".to_vec()).unwrap(), Vec::new());
    }

    #[test]
    fn chat_digest_has_one_entry_per_sender_with_their_contiguous_lamport() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "one"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "two"))
            .unwrap();
        // A gap: lamport 3 missing for alice.
        store
            .insert_message(msg(b"chat-a", b"alice", 4, "four"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"bob", 1, "hey"))
            .unwrap();

        let digest = store.chat_digest(b"chat-a".to_vec()).unwrap();
        assert_eq!(
            digest,
            vec![
                DigestEntry {
                    sender_user_id: b"alice".to_vec(),
                    through_lamport: 2
                },
                DigestEntry {
                    sender_user_id: b"bob".to_vec(),
                    through_lamport: 1
                },
            ]
        );
    }

    #[test]
    fn messages_after_returns_only_newer_messages_ascending() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "one"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "two"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 3, "three"))
            .unwrap();

        let missing = store
            .messages_after(b"chat-a".to_vec(), b"alice".to_vec(), 1)
            .unwrap();
        let payloads: Vec<Vec<u8>> = missing.into_iter().map(|m| m.payload).collect();
        assert_eq!(payloads, vec![b"two".to_vec(), b"three".to_vec()]);
    }

    #[test]
    fn messages_after_zero_returns_everything() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "one"))
            .unwrap();
        store
            .insert_message(msg(b"chat-a", b"alice", 2, "two"))
            .unwrap();

        let missing = store
            .messages_after(b"chat-a".to_vec(), b"alice".to_vec(), 0)
            .unwrap();
        assert_eq!(missing.len(), 2);
    }

    /// The composition §7.3 sync relies on: A has messages B lacks; feeding
    /// B's digest into A's `messages_after` per sender yields exactly what B
    /// is missing, no more and no less.
    #[test]
    fn chat_digest_and_messages_after_compose_to_find_exactly_the_gap() {
        let store_a = MessageStore::open(":memory:".to_string()).unwrap();
        let store_b = MessageStore::open(":memory:".to_string()).unwrap();

        for lamport in 1..=5u64 {
            let m = msg(b"chat-a", b"alice", lamport, &format!("msg-{lamport}"));
            store_a.insert_message(m).unwrap();
        }
        // B only has the first two.
        store_b
            .insert_message(msg(b"chat-a", b"alice", 1, "msg-1"))
            .unwrap();
        store_b
            .insert_message(msg(b"chat-a", b"alice", 2, "msg-2"))
            .unwrap();

        let b_digest = store_b.chat_digest(b"chat-a".to_vec()).unwrap();
        assert_eq!(
            b_digest,
            vec![DigestEntry {
                sender_user_id: b"alice".to_vec(),
                through_lamport: 2
            }]
        );

        let mut all_missing = Vec::new();
        for entry in &b_digest {
            let missing = store_a
                .messages_after(
                    b"chat-a".to_vec(),
                    entry.sender_user_id.clone(),
                    entry.through_lamport,
                )
                .unwrap();
            all_missing.extend(missing);
        }

        let payloads: Vec<Vec<u8>> = all_missing.into_iter().map(|m| m.payload).collect();
        assert_eq!(
            payloads,
            vec![b"msg-3".to_vec(), b"msg-4".to_vec(), b"msg-5".to_vec()]
        );
    }

    // --- carry queue (DESIGN.md §5.3) --------------------------------------

    const BIG_BUDGET: i64 = 5 * 1024 * 1024;

    fn carried(msg_id: &[u8], hint: &[u8], expiry: i64, sealed_len: usize) -> CarriedEnvelope {
        let fill = msg_id
            .iter()
            .fold(0xAB_u8, |acc, byte| acc.wrapping_add(*byte));
        CarriedEnvelope {
            msg_id: msg_id.to_vec(),
            hop_ttl: 7,
            expiry,
            recipient_hint: hint.to_vec(),
            sealed: vec![fill; sealed_len],
        }
    }

    #[test]
    fn enqueue_then_fetch_by_hint_round_trips() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let env = carried(b"m1", b"hint-a", 2_000, 100);
        assert!(store
            .enqueue_carried_envelope(env.clone(), false, 1_000, BIG_BUDGET)
            .unwrap());

        let found = store
            .carried_envelopes_for_hints(vec![b"hint-a".to_vec()], 1_500)
            .unwrap();
        assert_eq!(found, vec![env]);
    }

    #[test]
    fn enqueue_is_idempotent_on_msg_id() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(store
            .enqueue_carried_envelope(carried(b"m1", b"h", 2_000, 100), false, 1_000, BIG_BUDGET)
            .unwrap());
        // Same msg_id, re-received under DTN: no-op, not a duplicate row.
        assert!(!store
            .enqueue_carried_envelope(carried(b"m1", b"h", 2_000, 100), false, 1_050, BIG_BUDGET)
            .unwrap());
        assert_eq!(store.carried_len().unwrap(), 1);
    }

    #[test]
    fn enqueue_dedupes_rewrapped_ciphertext_but_preserves_distinct_hints() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let original = carried(b"original", b"hint-a", 9_000, 100);
        assert!(store
            .enqueue_carried_envelope(original.clone(), true, 1_000, BIG_BUDGET)
            .unwrap());

        let mut rewrapped = original.clone();
        rewrapped.msg_id = b"attacker-new-id".to_vec();
        rewrapped.expiry = 10_000;
        assert!(!store
            .enqueue_carried_envelope(rewrapped, true, 2_000, BIG_BUDGET)
            .unwrap());

        let mut group_fanout = original;
        group_fanout.msg_id = b"member-copy".to_vec();
        group_fanout.recipient_hint = b"hint-b".to_vec();
        assert!(store
            .enqueue_carried_envelope(group_fanout, true, 3_000, BIG_BUDGET)
            .unwrap());
        assert_eq!(store.carried_len().unwrap(), 2);
    }

    #[test]
    fn carry_ingest_rejects_amplified_hop_and_expiry_fields() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let mut bad_hop = carried(b"hop", b"hint", 9_000, 10);
        bad_hop.hop_ttl = crate::DEFAULT_HOP_TTL + 1;
        assert!(store
            .enqueue_carried_envelope(bad_hop, true, 1_000, BIG_BUDGET)
            .is_err());

        let too_far = carried(
            b"expiry",
            b"hint",
            1_000 + crate::MAX_CARRY_FUTURE_MS + 1,
            10,
        );
        assert!(store
            .enqueue_relay_carried_envelope(too_far, 1_000)
            .is_err());
        assert_eq!(store.carried_len().unwrap(), 0);
    }

    #[test]
    fn fetch_by_hint_ignores_nonmatching_and_expired() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m1", b"hint-a", 2_000, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m2", b"hint-b", 2_000, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m3", b"hint-a", 1_200, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();

        // now_ms = 1_500: m3 (expiry 1_200) is expired; m2 has the wrong hint.
        let found = store
            .carried_envelopes_for_hints(vec![b"hint-a".to_vec()], 1_500)
            .unwrap();
        let ids: Vec<Vec<u8>> = found.into_iter().map(|e| e.msg_id).collect();
        assert_eq!(ids, vec![b"m1".to_vec()]);
    }

    #[test]
    fn fetch_matches_any_of_several_hints() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m1", b"day-a", 9_000, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m2", b"day-b", 9_000, 10),
                false,
                1_100,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"m3", b"day-c", 9_000, 10),
                false,
                1_200,
                BIG_BUDGET,
            )
            .unwrap();

        // A peer's recent-day hints cover day-a and day-c but not day-b.
        let found = store
            .carried_envelopes_for_hints(vec![b"day-a".to_vec(), b"day-c".to_vec()], 5_000)
            .unwrap();
        let ids: Vec<Vec<u8>> = found.into_iter().map(|e| e.msg_id).collect();
        assert_eq!(ids, vec![b"m1".to_vec(), b"m3".to_vec()]); // oldest received_at first
    }

    #[test]
    fn carried_msg_ids_are_returned_oldest_first_and_limited() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(carried(b"m1", b"h", 9_000, 10), false, 1_000, BIG_BUDGET)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"m2", b"h", 9_000, 10), false, 2_000, BIG_BUDGET)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"m3", b"h", 9_000, 10), false, 3_000, BIG_BUDGET)
            .unwrap();

        let ids = store.carried_msg_ids(2).unwrap();
        assert_eq!(ids, vec![b"m1".to_vec(), b"m2".to_vec()]);
    }

    #[test]
    fn carried_msg_ids_desc_are_returned_newest_first_and_limited() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(carried(b"m1", b"h", 9_000, 10), false, 1_000, BIG_BUDGET)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"m2", b"h", 9_000, 10), false, 2_000, BIG_BUDGET)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"m3", b"h", 9_000, 10), false, 3_000, BIG_BUDGET)
            .unwrap();

        let ids = store.carried_msg_ids_desc(2).unwrap();
        assert_eq!(ids, vec![b"m3".to_vec(), b"m2".to_vec()]);
    }

    #[test]
    fn recent_consumed_msg_ids_are_returned_newest_first_and_limited() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_incoming_message(
                msg(b"chat-a", b"alice", 1, "one"),
                vec![1; MESSAGE_ID_LEN],
                None,
            )
            .unwrap();
        store
            .insert_incoming_message(
                msg(b"chat-a", b"alice", 2, "two"),
                vec![2; MESSAGE_ID_LEN],
                None,
            )
            .unwrap();
        store
            .insert_incoming_message(
                msg(b"chat-a", b"alice", 3, "three"),
                vec![3; MESSAGE_ID_LEN],
                None,
            )
            .unwrap();

        let ids = store.recent_consumed_msg_ids(2).unwrap();
        assert_eq!(ids, vec![vec![3; MESSAGE_ID_LEN], vec![2; MESSAGE_ID_LEN]]);
    }

    #[test]
    fn recent_consumed_msg_ids_skips_rows_without_a_msg_id() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // Legacy rows (inserted before envelope-id recording existed) carry
        // no msg_id; `insert_message` reproduces that shape.
        store
            .insert_message(msg(b"chat-a", b"alice", 1, "legacy"))
            .unwrap();
        store
            .insert_incoming_message(
                msg(b"chat-a", b"alice", 2, "two"),
                vec![2; MESSAGE_ID_LEN],
                None,
            )
            .unwrap();

        let ids = store.recent_consumed_msg_ids(10).unwrap();
        assert_eq!(ids, vec![vec![2; MESSAGE_ID_LEN]]);
    }

    /// specs/group-relay-durability.md §4.3 / §6 scenario (2): the same
    /// logical group message can arrive under two envelope identities -- the
    /// ORIGINAL msg_id over BLE and a per-member fan-out msg_id from the
    /// relay. The `UNIQUE(chat_id, sender_user_id, lamport)` dedup renders
    /// it once regardless; the second insert is a silent no-op.
    #[test]
    fn same_message_under_two_envelope_ids_renders_once() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let ble_first = store
            .insert_incoming_message(
                msg(b"group-g", b"alice", 5, "meet at the buffet"),
                vec![0x11; MESSAGE_ID_LEN], // original envelope id (BLE flood)
                None,
            )
            .unwrap();
        let relay_second = store
            .insert_incoming_message(
                msg(b"group-g", b"alice", 5, "meet at the buffet"),
                vec![0x22; MESSAGE_ID_LEN], // fan-out id (relay fetch)
                None,
            )
            .unwrap();
        assert!(ble_first);
        assert!(!relay_second, "duplicate must be a silent no-op");
        assert_eq!(
            store.messages_for_chat(b"group-g".to_vec()).unwrap().len(),
            1,
            "one rendered row despite two envelope identities"
        );
    }

    #[test]
    fn recent_consumed_msg_ids_includes_own_authored_messages() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_incoming_message(
                msg(b"chat-a", b"alice", 1, "theirs"),
                vec![1; MESSAGE_ID_LEN],
                None,
            )
            .unwrap();
        // Our own authored message: `insert_outgoing_message` records the
        // envelope's msg_id on the message row too, so it must be advertised
        // alongside consumed incoming ids -- that's what suppresses a mule's
        // Hook-B spray from handing us back an envelope we authored.
        let authored = msg(b"chat-a", b"self", 1, "mine");
        let envelope = outbound_for(&authored, b"alice", &[2; MESSAGE_ID_LEN]);
        store
            .insert_outgoing_message(authored, envelope, 1_000)
            .unwrap();

        let ids = store.recent_consumed_msg_ids(10).unwrap();
        assert_eq!(ids, vec![vec![2; MESSAGE_ID_LEN], vec![1; MESSAGE_ID_LEN]]);
    }

    #[test]
    fn peer_sync_candidates_exclude_the_peers_known_ids_and_targeted_delivery() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"known", b"day-a", 9_000, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"for-peer", b"day-b", 9_000, 10),
                false,
                2_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"spray", b"day-c", 9_000, 10),
                false,
                3_000,
                BIG_BUDGET,
            )
            .unwrap();

        let found = store
            .carried_envelopes_for_peer_sync(
                vec![b"day-b".to_vec()],
                vec![b"known".to_vec()],
                5_000,
                u64::MAX,
                u32::MAX,
                None,
            )
            .unwrap();
        let ids: Vec<Vec<u8>> = found.rows.into_iter().map(|e| e.msg_id).collect();
        assert_eq!(ids, vec![b"spray".to_vec()]);
    }

    // --- FC2: digest spray pushes exclusion/budget into SQL ----------------

    #[test]
    fn peer_sync_never_decodes_sealed_ciphertext_for_rows_the_peer_already_knows() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        for (id, hint) in [
            (b"k1" as &[u8], b"hint-1" as &[u8]),
            (b"k2", b"hint-2"),
            (b"k3", b"hint-3"),
        ] {
            store
                .enqueue_carried_envelope(carried(id, hint, 9_000, 4_096), false, 1_000, BIG_BUDGET)
                .unwrap();
        }

        let known_ids = vec![b"k1".to_vec(), b"k2".to_vec(), b"k3".to_vec()];
        let found = store
            .carried_envelopes_for_peer_sync(vec![], known_ids, 5_000, u64::MAX, u32::MAX, None)
            .unwrap();

        assert!(found.rows.is_empty());
        assert!(
            found.exhausted,
            "a scan that reaches the tail is exhausted even when it selects nothing"
        );
        assert_eq!(
            store.test_sealed_reads(),
            0,
            "a row already known to the peer must never have its sealed ciphertext decoded"
        );
    }

    #[test]
    fn peer_sync_offers_an_oversized_oldest_envelope_alone_rather_than_wedging_the_lane() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"huge", b"day-a", 9_000, 300),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"small-1", b"day-b", 9_000, 100),
                false,
                2_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"small-2", b"day-c", 9_000, 100),
                false,
                3_000,
                BIG_BUDGET,
            )
            .unwrap();

        // The oldest row alone busts the 250-byte budget. Skipping it would
        // park it at the head of every future round until it expired, and
        // nothing behind it would ever be offered, so it goes out by itself.
        let round_one = store
            .carried_envelopes_for_peer_sync(vec![], vec![], 5_000, 250, u32::MAX, None)
            .unwrap();
        let ids: Vec<Vec<u8>> = round_one.rows.into_iter().map(|e| e.msg_id).collect();
        assert_eq!(ids, vec![b"huge".to_vec()]);

        // Once the peer advertises it in a digest, the lane moves on and the
        // two small ones fit the same budget together.
        let round_two = store
            .carried_envelopes_for_peer_sync(
                vec![],
                vec![b"huge".to_vec()],
                5_000,
                250,
                u32::MAX,
                None,
            )
            .unwrap();
        let ids: Vec<Vec<u8>> = round_two.rows.into_iter().map(|e| e.msg_id).collect();
        assert_eq!(ids, vec![b"small-1".to_vec(), b"small-2".to_vec()]);
    }

    // --- per-link-session carried cursor -----------------------------------

    #[test]
    fn a_zero_budget_reads_nothing_at_all() {
        // The lane's off switch, used by the shells while a completed walk is
        // parked. It must not cost a query or a ciphertext decode per
        // re-digest, and it must not claim the queue was exhausted.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"one", b"hint", 9_000, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();

        let page = store
            .carried_envelopes_for_peer_sync(vec![], vec![], 5_000, 0, u32::MAX, None)
            .unwrap();
        assert!(page.rows.is_empty());
        assert!(page.next.is_none());
        assert!(
            !page.exhausted,
            "nothing was examined, so nothing was ruled out"
        );
        assert_eq!(store.test_sealed_reads(), 0);
    }

    #[test]
    fn peer_sync_pages_forward_from_the_cursor_and_is_exhausted_only_at_the_tail() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // Two rows share a millisecond, so the walk also has to order on
        // msg_id -- a received_at-only cursor would skip one of them.
        for (id, received_at) in [
            (b"e1" as &[u8], 1_000_i64),
            (b"e2", 1_000),
            (b"e3", 2_000),
            (b"e4", 3_000),
        ] {
            store
                .enqueue_carried_envelope(
                    carried(id, b"hint", 9_000, 100),
                    false,
                    received_at,
                    BIG_BUDGET,
                )
                .unwrap();
        }

        // 250 bytes fits two 100-byte rows, not three.
        let page_one = store
            .carried_envelopes_for_peer_sync(vec![], vec![], 5_000, 250, u32::MAX, None)
            .unwrap();
        let ids: Vec<Vec<u8>> = page_one.rows.iter().map(|e| e.msg_id.clone()).collect();
        assert_eq!(ids, vec![b"e1".to_vec(), b"e2".to_vec()]);
        assert!(!page_one.exhausted, "two of four rows is not the tail");
        assert_eq!(
            page_one.next,
            Some(CoreCarriedCursor {
                received_at: 1_000,
                msg_id: b"e2".to_vec(),
            })
        );

        let page_two = store
            .carried_envelopes_for_peer_sync(
                vec![],
                vec![],
                5_000,
                250,
                u32::MAX,
                page_one.next.clone(),
            )
            .unwrap();
        let ids: Vec<Vec<u8>> = page_two.rows.iter().map(|e| e.msg_id.clone()).collect();
        assert_eq!(
            ids,
            vec![b"e3".to_vec(), b"e4".to_vec()],
            "the second page starts strictly after the cursor"
        );
        assert!(
            page_two.exhausted,
            "the second page consumed the rest of the queue"
        );

        let page_three = store
            .carried_envelopes_for_peer_sync(vec![], vec![], 5_000, 250, u32::MAX, page_two.next)
            .unwrap();
        assert!(page_three.rows.is_empty());
        assert!(page_three.exhausted);
        assert_eq!(page_three.next, None);
    }

    #[test]
    fn a_row_that_arrives_after_the_cursor_was_set_is_still_offered() {
        // The young tail must never starve: a cursor is a resume point in the
        // queue's order, not a snapshot of what existed when it was taken.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"old", b"hint", 9_000, 10),
                false,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();

        let first = store
            .carried_envelopes_for_peer_sync(vec![], vec![], 5_000, 10, u32::MAX, None)
            .unwrap();
        assert_eq!(first.rows.len(), 1);

        store
            .enqueue_carried_envelope(
                carried(b"young", b"hint", 9_000, 10),
                false,
                2_000,
                BIG_BUDGET,
            )
            .unwrap();

        let second = store
            .carried_envelopes_for_peer_sync(vec![], vec![], 5_000, 10, u32::MAX, first.next)
            .unwrap();
        let ids: Vec<Vec<u8>> = second.rows.into_iter().map(|e| e.msg_id).collect();
        assert_eq!(ids, vec![b"young".to_vec()]);
    }

    #[test]
    fn peer_sync_never_decodes_sealed_ciphertext_for_rows_behind_the_cursor() {
        // The whole point of a keyset cursor over a re-read from the top: the
        // rows a previous round already offered are excluded by the index
        // seek, so their ciphertext is never materialized again.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        for (id, received_at) in [
            (b"c1" as &[u8], 1_000_i64),
            (b"c2", 2_000),
            (b"c3", 3_000),
            (b"c4", 4_000),
        ] {
            store
                .enqueue_carried_envelope(
                    carried(id, b"hint", 9_000, 4_096),
                    false,
                    received_at,
                    BIG_BUDGET,
                )
                .unwrap();
        }

        let after = Some(CoreCarriedCursor {
            received_at: 3_000,
            msg_id: b"c3".to_vec(),
        });
        let page = store
            .carried_envelopes_for_peer_sync(vec![], vec![], 5_000, u64::MAX, u32::MAX, after)
            .unwrap();

        let ids: Vec<Vec<u8>> = page.rows.into_iter().map(|e| e.msg_id).collect();
        assert_eq!(ids, vec![b"c4".to_vec()]);
        assert_eq!(
            store.test_sealed_reads(),
            1,
            "only the one row past the cursor may have its sealed ciphertext \
             decoded; the three behind it must never be read"
        );
    }

    // --- envelope-count ceiling --------------------------------------------

    /// `count` tiny carried rows, one per millisecond from `first_received_at`,
    /// named `n-0`, `n-1`, ... in queue order.
    fn seed_tiny_carried(store: &MessageStore, count: usize, first_received_at: i64) {
        for index in 0..count {
            // Sealed bytes must differ per row: the queue dedupes on a digest
            // of (hint, sealed), so a shared filler would silently collapse the
            // page these tests are trying to fill.
            let envelope = CarriedEnvelope {
                msg_id: format!("n-{index}").into_bytes(),
                hop_ttl: 7,
                expiry: 9_000,
                recipient_hint: b"hint".to_vec(),
                sealed: (index as u64).to_be_bytes().to_vec(),
            };
            store
                .enqueue_carried_envelope(
                    envelope,
                    false,
                    first_received_at + index as i64,
                    BIG_BUDGET,
                )
                .unwrap();
        }
    }

    #[test]
    fn peer_sync_stops_at_the_row_ceiling_even_when_the_byte_budget_is_untouched() {
        // 200 receipt-sized envelopes are ~1.6 KiB in total: the byte budget
        // never binds, but 200 frames into one link's FIFO is exactly what the
        // ceiling exists to prevent.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        seed_tiny_carried(&store, 200, 1_000);

        let page = store
            .carried_envelopes_for_peer_sync(vec![], vec![], 5_000, u64::MAX, 64, None)
            .unwrap();
        assert_eq!(page.rows.len(), 64);
        assert!(!page.exhausted, "163 rows are still behind the cursor");
        assert_eq!(page.rows[0].msg_id, b"n-0".to_vec());
        assert_eq!(page.rows[63].msg_id, b"n-63".to_vec());
        assert_eq!(
            page.next,
            Some(CoreCarriedCursor {
                received_at: 1_063,
                msg_id: b"n-63".to_vec(),
            })
        );
    }

    #[test]
    fn a_row_capped_page_resumes_where_it_stopped_and_reaches_the_tail() {
        // The row ceiling must page the queue, not re-tread its head: three
        // rounds of five over eleven rows, with no row offered twice and none
        // skipped, and only the last round claiming the tail.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        seed_tiny_carried(&store, 11, 1_000);

        let mut seen: Vec<Vec<u8>> = Vec::new();
        let mut cursor = None;
        let mut rounds = 0;
        loop {
            let page = store
                .carried_envelopes_for_peer_sync(vec![], vec![], 5_000, u64::MAX, 5, cursor)
                .unwrap();
            rounds += 1;
            seen.extend(page.rows.iter().map(|e| e.msg_id.clone()));
            cursor = page.next;
            if page.exhausted {
                break;
            }
            assert!(rounds < 10, "the walk must terminate");
        }
        assert_eq!(rounds, 3);
        let expected: Vec<Vec<u8>> = (0..11).map(|i| format!("n-{i}").into_bytes()).collect();
        assert_eq!(seen, expected);
    }

    #[test]
    fn the_byte_budget_still_binds_first_for_large_envelopes() {
        // The ceiling is an addition, not a replacement: with rows this big the
        // budget is still what stops the round well short of `max_rows`.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        for (index, id) in [b"b1" as &[u8], b"b2", b"b3", b"b4"].iter().enumerate() {
            store
                .enqueue_carried_envelope(
                    carried(id, b"hint", 9_000, 4_096),
                    false,
                    1_000 + index as i64,
                    BIG_BUDGET,
                )
                .unwrap();
        }

        let page = store
            .carried_envelopes_for_peer_sync(vec![], vec![], 5_000, 9_000, 64, None)
            .unwrap();
        let ids: Vec<Vec<u8>> = page.rows.iter().map(|e| e.msg_id.clone()).collect();
        assert_eq!(ids, vec![b"b1".to_vec(), b"b2".to_vec()]);
        assert!(!page.exhausted);
    }

    #[test]
    fn a_zero_row_ceiling_is_an_off_switch_like_a_zero_budget() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        seed_tiny_carried(&store, 3, 1_000);

        let page = store
            .carried_envelopes_for_peer_sync(vec![], vec![], 5_000, u64::MAX, 0, None)
            .unwrap();
        assert!(page.rows.is_empty());
        assert!(page.next.is_none());
        assert!(
            !page.exhausted,
            "nothing was examined, so nothing was ruled out"
        );
        assert_eq!(store.test_sealed_reads(), 0);
    }

    #[test]
    fn hints_page_stops_at_the_row_ceiling_and_resumes_across_pages() {
        // The HELLO drain's lane gets the same ceiling, and the same
        // offer-only guarantee: every row is still in the queue afterwards.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        seed_tiny_carried(&store, 7, 1_000);
        let hint = b"hint".to_vec();

        let page_one = store
            .carried_envelopes_for_hints_page(vec![hint.clone()], 5_000, u64::MAX, 3, None)
            .unwrap();
        let ids: Vec<Vec<u8>> = page_one.rows.iter().map(|e| e.msg_id.clone()).collect();
        assert_eq!(ids, vec![b"n-0".to_vec(), b"n-1".to_vec(), b"n-2".to_vec()]);
        assert!(!page_one.exhausted);

        let page_two = store
            .carried_envelopes_for_hints_page(
                vec![hint.clone()],
                5_000,
                u64::MAX,
                3,
                page_one.next.clone(),
            )
            .unwrap();
        let ids: Vec<Vec<u8>> = page_two.rows.iter().map(|e| e.msg_id.clone()).collect();
        assert_eq!(ids, vec![b"n-3".to_vec(), b"n-4".to_vec(), b"n-5".to_vec()]);

        // Nothing was removed by any of this: paging only ever offers.
        assert_eq!(
            store
                .carried_envelopes_for_hints(vec![hint], 5_000)
                .unwrap()
                .len(),
            7
        );
    }

    #[test]
    fn a_zero_row_ceiling_offers_nothing_from_the_hints_page() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        seed_tiny_carried(&store, 3, 1_000);

        let page = store
            .carried_envelopes_for_hints_page(vec![b"hint".to_vec()], 5_000, u64::MAX, 0, None)
            .unwrap();
        assert!(page.rows.is_empty());
        assert!(page.next.is_none());
        assert!(!page.exhausted);
    }

    #[test]
    fn the_digest_spray_plan_applies_the_default_row_ceiling() {
        // The shells do not pass the ceiling to the plan; core applies its own.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        seed_tiny_carried(&store, DEFAULT_CARRIED_PAGE_MAX_ROWS as usize + 40, 1_000);

        let plan = store
            .core_digest_spray_plan(
                b"me".to_vec(),
                b"peer".to_vec(),
                vec![],
                vec![],
                5_000,
                u64::MAX,
                0,
                0,
                16,
                true,
                vec![],
                None,
            )
            .unwrap();
        assert_eq!(
            plan.carried_frames.len(),
            DEFAULT_CARRIED_PAGE_MAX_ROWS as usize
        );
        assert!(!plan.carried_exhausted);
    }

    #[test]
    fn outbound_budgeted_excludes_known_ids_in_sql_and_stops_at_the_shared_budget() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();

        let msg1 = msg(b"bob", b"alice", 1, "one");
        let mut env1 = outbound_for(&msg1, b"bob", b"msg-000000000001");
        env1.sealed = vec![1u8; 10];
        env1.expiry = 1_700_000_100_000;
        store
            .insert_outgoing_message(msg1, env1.clone(), 1_700_000_000_100)
            .unwrap();

        let msg2 = msg(b"bob", b"alice", 2, "two");
        let mut env2 = outbound_for(&msg2, b"bob", b"msg-000000000002");
        env2.sealed = vec![2u8; 10];
        env2.expiry = 1_700_000_100_000;
        store
            .insert_outgoing_message(msg2, env2.clone(), 1_700_000_000_200)
            .unwrap();

        let msg3 = msg(b"bob", b"alice", 3, "three");
        let mut env3 = outbound_for(&msg3, b"bob", b"msg-000000000003");
        env3.sealed = vec![3u8; 10];
        env3.expiry = 1_700_000_100_000;
        store
            .insert_outgoing_message(msg3, env3.clone(), 1_700_000_000_300)
            .unwrap();

        // env1 is already known to the peer -- excluded in SQL, never
        // decoded. Budget (15 bytes) fits env2 (10 bytes) alone but not
        // env2+env3 (20 bytes), so env3 is the one that trips the budget.
        let known: HashSet<Vec<u8>> = [env1.msg_id.clone()].into_iter().collect();
        let (selected, exhausted) = store
            .outbound_envelopes_after_budgeted(
                b"bob".to_vec(),
                b"alice".to_vec(),
                0,
                &known,
                1_700_000_000_000,
                15,
            )
            .unwrap();

        assert_eq!(selected, vec![env2]);
        assert!(exhausted, "the third envelope should overflow the budget");
        assert_eq!(
            store.test_sealed_reads(),
            2,
            "known env1 must never be decoded; env2 (selected) and env3 (the \
             one that overflows the budget, decoded only to learn its size) \
             account for the other two"
        );
    }

    #[test]
    fn consumed_hidden_msg_ids_evict_the_soonest_to_expire_when_over_the_cap() {
        // The cap is a backstop under the expiry bound (see
        // CONSUMED_HIDDEN_MSG_ID_LIMIT). Eviction must drop the rows with the
        // least life left, because a dropped row costs only a relay re-fetch.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        for (id, expiry) in [(1_u8, 5_000_i64), (2, 1_000), (3, 9_000), (4, 3_000)] {
            assert!(store
                .insert_consumed_hidden_msg_id_capped(vec![id; 16], expiry, 3)
                .unwrap());
        }
        assert_eq!(store.consumed_hidden_msg_id_count().unwrap(), 3);
        // The 1_000-expiry row is the one that went.
        assert!(!store
            .consumed_hidden_msg_id_recorded(vec![2; 16], 0)
            .unwrap());
        for id in [1_u8, 3, 4] {
            assert!(store
                .consumed_hidden_msg_id_recorded(vec![id; 16], 0)
                .unwrap());
        }
        // An insert that is itself the soonest to expire reports that it was
        // NOT kept, so no caller ever believes in evidence that isn't there.
        assert!(!store
            .insert_consumed_hidden_msg_id_capped(vec![5; 16], 1, 3)
            .unwrap());
        assert_eq!(store.consumed_hidden_msg_id_count().unwrap(), 3);
    }

    #[test]
    fn consumed_hidden_msg_ids_keep_the_later_expiry_on_reinsert() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .insert_consumed_hidden_msg_id(vec![1; 16], 9_000)
            .unwrap();
        store
            .insert_consumed_hidden_msg_id(vec![1; 16], 2_000)
            .unwrap();
        assert_eq!(store.consumed_hidden_msg_id_count().unwrap(), 1);
        assert!(store
            .consumed_hidden_msg_id_recorded(vec![1; 16], 8_999)
            .unwrap());
    }

    #[test]
    fn consumed_hidden_lamports_are_exact_accepted_pairwise_evidence() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice", "Alice")).unwrap();
        store.upsert_contact(contact(b"bob", "Bob")).unwrap();
        assert!(store
            .record_consumed_hidden_lamport(
                b"alice".to_vec(),
                b"alice".to_vec(),
                2,
                crate::KIND_RECEIPT,
            )
            .unwrap());
        assert!(!store
            .record_consumed_hidden_lamport(
                b"alice".to_vec(),
                b"alice".to_vec(),
                2,
                crate::KIND_RECEIPT,
            )
            .unwrap());
        assert!(store
            .record_consumed_hidden_lamport(
                b"bob".to_vec(),
                b"bob".to_vec(),
                3,
                crate::KIND_PROFILE_SYNC,
            )
            .unwrap());
        assert!(!store
            .record_consumed_hidden_lamport(
                b"not-a-pairwise-chat".to_vec(),
                b"alice".to_vec(),
                4,
                crate::KIND_RECEIPT,
            )
            .unwrap());
        assert!(!store
            .record_consumed_hidden_lamport(
                b"alice".to_vec(),
                b"alice".to_vec(),
                5,
                crate::KIND_TEXT,
            )
            .unwrap());
        assert!(!store
            .record_consumed_hidden_lamport(
                b"stranger".to_vec(),
                b"stranger".to_vec(),
                6,
                crate::KIND_FRIEND_REQUEST,
            )
            .unwrap());
        let mut stored_control = msg(b"alice", b"alice", 7, "stored control");
        stored_control.kind = crate::KIND_LAN_ENDPOINT_HINT;
        store.insert_message(stored_control).unwrap();
        assert!(!store
            .record_consumed_hidden_lamport(
                b"alice".to_vec(),
                b"alice".to_vec(),
                7,
                crate::KIND_LAN_ENDPOINT_HINT,
            )
            .unwrap());

        assert_eq!(
            store.consumed_hidden_lamports(b"alice".to_vec()).unwrap(),
            vec![ConsumedHiddenLamport {
                sender_user_id: b"alice".to_vec(),
                lamport: 2,
            }]
        );
        assert_eq!(
            store.consumed_hidden_lamports(b"bob".to_vec()).unwrap(),
            vec![ConsumedHiddenLamport {
                sender_user_id: b"bob".to_vec(),
                lamport: 3,
            }]
        );
    }

    #[test]
    fn open_migrates_a_store_that_predates_the_consumed_hidden_msg_id_table() {
        // A populated store written before this feature has no
        // `consumed_hidden_msg_ids` table at all. Opening it must add the
        // table without disturbing existing rows, and the new path must work
        // against the migrated schema immediately.
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cruisemesh-store-migration-consumed-hidden-{unique}.sqlite"
        ));
        let path_str = path.to_string_lossy().to_string();
        {
            let store = MessageStore::open(path_str.clone()).unwrap();
            store
                .insert_message(msg(b"bob", b"alice", 1, "before the upgrade"))
                .unwrap();
            store
                .insert_consumed_hidden_msg_id(vec![1; 16], 9_000)
                .unwrap();
        }
        // Simulate the older schema by removing the table (and its index)
        // from the on-disk file, leaving everything else populated.
        {
            let conn = Connection::open(&path_str).unwrap();
            conn.execute_batch(
                "DROP INDEX IF EXISTS idx_consumed_hidden_expiry;
                 DROP TABLE IF EXISTS consumed_hidden_msg_ids;",
            )
            .unwrap();
            let missing: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master
                     WHERE type = 'table' AND name = 'consumed_hidden_msg_ids'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(missing, 0, "fixture must actually predate the table");
        }

        let store = MessageStore::open(path_str).unwrap();
        assert_eq!(
            store.messages_for_chat(b"bob".to_vec()).unwrap().len(),
            1,
            "migration must not disturb existing rows",
        );
        assert_eq!(store.consumed_hidden_msg_id_count().unwrap(), 0);
        assert!(store
            .insert_consumed_hidden_msg_id(vec![7; 16], 9_000)
            .unwrap());
        assert!(store
            .consumed_hidden_msg_id_recorded(vec![7; 16], 1_000)
            .unwrap());
        assert_eq!(
            store.prune_expired_consumed_hidden_msg_ids(9_000).unwrap(),
            1
        );

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn open_migrates_a_store_that_predates_consumed_hidden_lamports() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cruisemesh-store-migration-consumed-lamports-{unique}.sqlite"
        ));
        let path_str = path.to_string_lossy().to_string();
        {
            let store = MessageStore::open(path_str.clone()).unwrap();
            store.upsert_contact(contact(b"alice", "Alice")).unwrap();
            store
                .insert_message(msg(b"alice", b"alice", 1, "before the upgrade"))
                .unwrap();
        }
        {
            let conn = Connection::open(&path_str).unwrap();
            conn.execute_batch("DROP TABLE IF EXISTS consumed_hidden_lamports;")
                .unwrap();
        }

        let store = MessageStore::open(path_str).unwrap();
        assert_eq!(
            store.messages_for_chat(b"alice".to_vec()).unwrap().len(),
            1,
            "migration must not disturb existing rows",
        );
        assert!(store
            .record_consumed_hidden_lamport(
                b"alice".to_vec(),
                b"alice".to_vec(),
                2,
                crate::KIND_RECEIPT,
            )
            .unwrap());
        assert_eq!(
            store.consumed_hidden_lamports(b"alice".to_vec()).unwrap(),
            vec![ConsumedHiddenLamport {
                sender_user_id: b"alice".to_vec(),
                lamport: 2,
            }]
        );

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn remove_carried_deletes_it() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(carried(b"m1", b"h", 2_000, 10), false, 1_000, BIG_BUDGET)
            .unwrap();
        assert!(store.remove_carried_envelope(b"m1".to_vec()).unwrap());
        assert!(!store.remove_carried_envelope(b"m1".to_vec()).unwrap()); // gone, idempotent
        assert_eq!(store.carried_len().unwrap(), 0);
    }

    #[test]
    fn prune_expired_carried_drops_only_the_expired() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(carried(b"live", b"h", 5_000, 10), false, 1_000, BIG_BUDGET)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"dead", b"h", 1_500, 10), false, 1_000, BIG_BUDGET)
            .unwrap();

        assert_eq!(store.prune_expired_carried(2_000).unwrap(), 1);
        assert_eq!(store.carried_len().unwrap(), 1);
        let found = store
            .carried_envelopes_for_hints(vec![b"h".to_vec()], 2_000)
            .unwrap();
        assert_eq!(found[0].msg_id, b"live");
    }

    #[test]
    fn foreign_budget_evicts_oldest_foreign_first() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // Budget of 250 bytes; three 100-byte foreign envelopes can't all fit.
        store
            .enqueue_carried_envelope(carried(b"f1", b"h", 9_000, 100), false, 1_000, 250)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"f2", b"h", 9_000, 100), false, 2_000, 250)
            .unwrap();
        // Third insert pushes total to 300 > 250, evicting the oldest (f1).
        store
            .enqueue_carried_envelope(carried(b"f3", b"h", 9_000, 100), false, 3_000, 250)
            .unwrap();

        let ids: Vec<Vec<u8>> = store
            .carried_envelopes_for_hints(vec![b"h".to_vec()], 5_000)
            .unwrap()
            .into_iter()
            .map(|e| e.msg_id)
            .collect();
        assert_eq!(ids, vec![b"f2".to_vec(), b"f3".to_vec()]);
    }

    #[test]
    fn family_envelopes_win_foreign_budget_eviction() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        // A family envelope (is_family = true) far exceeding the budget stays.
        store
            .enqueue_carried_envelope(carried(b"fam", b"h", 9_000, 400), true, 1_000, 250)
            .unwrap();
        // Foreign envelopes still get budget-capped independently...
        store
            .enqueue_carried_envelope(carried(b"f1", b"h", 9_000, 100), false, 2_000, 250)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"f2", b"h", 9_000, 100), false, 3_000, 250)
            .unwrap();
        store
            .enqueue_carried_envelope(carried(b"f3", b"h", 9_000, 100), false, 4_000, 250)
            .unwrap();

        let ids: Vec<Vec<u8>> = store
            .carried_envelopes_for_hints(vec![b"h".to_vec()], 5_000)
            .unwrap()
            .into_iter()
            .map(|e| e.msg_id)
            .collect();
        // fam survives despite being 400 bytes (> budget); foreign kept to f2,f3.
        assert_eq!(ids, vec![b"fam".to_vec(), b"f2".to_vec(), b"f3".to_vec()]);
    }

    #[test]
    fn total_budget_evicts_foreign_but_never_family() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"family-old", b"h1", 9_000, 100),
                true,
                1_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"foreign", b"h2", 9_000, 100),
                false,
                2_000,
                BIG_BUDGET,
            )
            .unwrap();
        store
            .enqueue_carried_envelope(
                carried(b"family-new", b"h3", 9_000, 100),
                true,
                3_000,
                BIG_BUDGET,
            )
            .unwrap();

        let mut conn = lock_conn(&store.conn);
        let tx = conn.transaction().unwrap();
        let pressure = enforce_carried_budgets(&tx, BIG_BUDGET, 200).unwrap();
        assert!(pressure.within_budget);
        assert_eq!(pressure.evicted_rows, 1);
        tx.commit().unwrap();
        drop(conn);

        let ids = store.carried_msg_ids(10).unwrap();
        assert_eq!(ids, vec![b"family-old".to_vec(), b"family-new".to_vec()]);

        let mut conn = lock_conn(&store.conn);
        let tx = conn.transaction().unwrap();
        let pressure = enforce_carried_budgets(&tx, BIG_BUDGET, 100).unwrap();
        assert!(
            !pressure.within_budget,
            "family-only overage must remain visible instead of deleting a live row"
        );
        assert_eq!(pressure.evicted_rows, 0);
        tx.commit().unwrap();
        drop(conn);
        assert_eq!(
            store.carried_msg_ids(10).unwrap(),
            vec![b"family-old".to_vec(), b"family-new".to_vec()]
        );
    }

    #[test]
    fn family_admission_rejects_atomically_and_retries_after_expiry_frees_space() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(enqueue_carried_envelope_with_budgets(
            &store,
            carried(b"family-old", b"h1", 1_500, 100),
            true,
            1_000,
            BIG_BUDGET,
            150,
        )
        .unwrap());

        let candidate = carried(b"family-new", b"h2", 9_000, 100);
        let error = enqueue_carried_envelope_with_budgets(
            &store,
            candidate.clone(),
            true,
            1_100,
            BIG_BUDGET,
            150,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CoreError::Store(message) if message == CARRY_ADMISSION_CAPACITY_ERROR
        ));
        assert_eq!(
            store.carried_msg_ids(10).unwrap(),
            vec![b"family-old".to_vec()]
        );

        let evidence = store.export_protocol_events_jsonl().unwrap();
        assert!(evidence.contains(r#""code":"carry_admission_rejected""#));
        assert!(evidence.contains(r#""outcome":"family_admission_rejected""#));
        assert!(evidence.contains("EVICT-01"));
        assert!(
            !evidence.contains("family-new"),
            "pressure evidence must not carry a raw msg_id"
        );

        // The rejected row was not retained as a duplicate/seen record. Once
        // the old row expires, the exact same candidate can be admitted.
        assert!(enqueue_carried_envelope_with_budgets(
            &store, candidate, true, 2_000, BIG_BUDGET, 150,
        )
        .unwrap());
        assert_eq!(
            store.carried_msg_ids(10).unwrap(),
            vec![b"family-new".to_vec()]
        );
    }

    #[test]
    fn a_new_family_row_spends_foreign_capacity_before_admission() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        for (id, is_family, received_at) in [
            (b"foreign".as_slice(), false, 1_000),
            (b"family-old".as_slice(), true, 2_000),
            (b"family-new".as_slice(), true, 3_000),
        ] {
            assert!(enqueue_carried_envelope_with_budgets(
                &store,
                carried(id, id, 9_000, 100),
                is_family,
                received_at,
                BIG_BUDGET,
                200,
            )
            .unwrap());
        }

        assert_eq!(
            store.carried_msg_ids(10).unwrap(),
            vec![b"family-old".to_vec(), b"family-new".to_vec()]
        );
        let evidence = store.export_protocol_events_jsonl().unwrap();
        assert!(evidence.contains(r#""code":"carried_row_evicted""#));
        assert!(evidence.contains(r#""outcome":"foreign_rows_evicted""#));
        assert!(evidence.contains(r#""rows_evicted":1"#));
        assert!(evidence.contains(r#""bytes_evicted":100"#));
    }

    #[test]
    fn foreign_admission_cannot_displace_family_or_poison_a_retry() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(enqueue_carried_envelope_with_budgets(
            &store,
            carried(b"family", b"family-hint", 9_000, 150),
            true,
            1_000,
            BIG_BUDGET,
            150,
        )
        .unwrap());
        let candidate = carried(b"foreign", b"foreign-hint", 9_000, 10);
        assert!(enqueue_carried_envelope_with_budgets(
            &store,
            candidate.clone(),
            false,
            2_000,
            BIG_BUDGET,
            150,
        )
        .is_err());
        assert_eq!(store.carried_msg_ids(10).unwrap(), vec![b"family".to_vec()]);

        assert!(store.remove_carried_envelope(b"family".to_vec()).unwrap());
        assert!(enqueue_carried_envelope_with_budgets(
            &store, candidate, false, 3_000, BIG_BUDGET, 150,
        )
        .unwrap());
    }

    #[test]
    fn oversized_foreign_admission_rejects_before_eviction_and_reports_its_budget() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(enqueue_carried_envelope_with_budgets(
            &store,
            carried(b"foreign-old", b"old-hint", 9_000, 40),
            false,
            1_000,
            100,
            1_000,
        )
        .unwrap());

        let candidate = carried(b"foreign-big", b"big-hint", 9_000, 101);
        let error = enqueue_carried_envelope_with_budgets(
            &store,
            candidate.clone(),
            false,
            2_000,
            100,
            1_000,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CoreError::Store(message) if message == CARRY_ADMISSION_CAPACITY_ERROR
        ));
        assert_eq!(
            store.carried_msg_ids(10).unwrap(),
            vec![b"foreign-old".to_vec()],
            "an impossible candidate must not evict a useful older foreign row on its way out"
        );

        let evidence = store.export_protocol_events_jsonl().unwrap();
        assert!(evidence.contains(r#""outcome":"foreign_admission_rejected""#));
        assert!(evidence.contains(r#""incoming_bytes":101"#));
        assert!(evidence.contains(r#""foreign_budget_bytes":100"#));
        assert!(!evidence.contains("foreign-big"));

        assert!(
            enqueue_carried_envelope_with_budgets(&store, candidate, false, 3_000, 200, 1_000,)
                .unwrap()
        );
        assert_eq!(
            store.carried_msg_ids(10).unwrap(),
            vec![b"foreign-old".to_vec(), b"foreign-big".to_vec()]
        );
    }

    #[test]
    fn relay_proxy_admission_uses_the_same_family_preservation_rule() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        assert!(enqueue_carried_envelope_with_budgets(
            &store,
            carried(b"family", b"h1", 9_000, 100),
            true,
            1_000,
            BIG_BUDGET,
            100,
        )
        .unwrap());

        let error = enqueue_relay_carried_envelope_with_budget(
            &store,
            carried(b"relay-proxy", b"h2", 9_000, 10),
            2_000,
            100,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            CoreError::Store(message) if message == CARRY_ADMISSION_CAPACITY_ERROR
        ));
        assert_eq!(store.carried_msg_ids(10).unwrap(), vec![b"family".to_vec()]);
    }

    #[test]
    fn bulk_relay_page_capacity_failure_is_retryable_and_holds_the_page() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let existing_id = b"family-full-0001";
        {
            let conn = lock_conn(&store.conn);
            conn.execute(
                "INSERT INTO carried_envelopes
                    (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family,
                     received_at, size_bytes, from_relay, content_digest)
                 VALUES (?1, 3, 9000, X'0102030405060708', zeroblob(?2), 1,
                         1000, ?2, 0, X'01')",
                params![existing_id.as_slice(), DEFAULT_TOTAL_CARRY_BUDGET_BYTES],
            )
            .unwrap();
        }

        let candidate_id = b"relay-rejected01".to_vec();
        let ingest = store
            .ingest_relay_page(
                vec![CoreRelayFetchedEnvelope {
                    id: 77,
                    msg_id: candidate_id.clone(),
                    hop_ttl: 3,
                    recipient_hint: b"hint-123".to_vec(),
                    sealed: vec![9; 10],
                    expiry_ms: 9_000,
                }],
                2_000,
                Some("p1-1".to_string()),
                4,
            )
            .unwrap();

        assert_eq!(ingest.rows_ingested, 0);
        assert!(
            !ingest.fully_processed,
            "a failed row must hold the frontier"
        );
        assert_eq!(ingest.rows.len(), 1);
        assert_eq!(ingest.rows[0].disposition, CoreInboundDisposition::Failed);
        assert_eq!(
            store.carried_msg_ids(10).unwrap(),
            vec![existing_id.to_vec()],
            "the admitted family row survives and the rejected candidate leaves no dedupe poison"
        );

        let evidence = store.export_protocol_events_jsonl().unwrap();
        assert!(evidence.contains(r#""code":"carry_admission_rejected""#));
        assert!(!evidence.contains("relay-rejected01"));
        assert!(
            evidence.contains(r#""code":"page_ingested""#),
            "the committed page transaction remains observable even when its frontier must hold"
        );

        // Free capacity and present the exact same relay row again. The first
        // rejection did not create a carried row or any separate seen record.
        assert!(store.remove_carried_envelope(existing_id.to_vec()).unwrap());
        let retry = store
            .ingest_relay_page(
                vec![CoreRelayFetchedEnvelope {
                    id: 77,
                    msg_id: candidate_id,
                    hop_ttl: 3,
                    recipient_hint: b"hint-123".to_vec(),
                    sealed: vec![9; 10],
                    expiry_ms: 9_000,
                }],
                3_000,
                Some("p2-1".to_string()),
                4,
            )
            .unwrap();
        assert!(retry.fully_processed);
        assert_eq!(retry.rows_ingested, 1);
        assert_eq!(retry.rows[0].disposition, CoreInboundDisposition::Carried);
    }

    // --- relay proxy-polling (from_relay) -----------------------------------

    #[test]
    fn relay_carried_envelope_is_deliverable_over_ble_but_never_reuploaded() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let env = carried(b"proxy", b"hint-a", 9_000, 10);
        assert!(store
            .enqueue_relay_carried_envelope(env.clone(), 1_000)
            .unwrap());

        // Deliverable over BLE to the real recipient...
        let found = store
            .carried_envelopes_for_hints(vec![b"hint-a".to_vec()], 2_000)
            .unwrap();
        assert_eq!(found, vec![env]);

        // ...but never re-uploaded to the relay it came from.
        let uploadable = store.family_carried_envelopes(10, 2_000, vec![]).unwrap();
        assert!(uploadable.is_empty());
    }

    #[test]
    fn normal_family_carried_envelope_is_still_reuploaded() {
        // Unchanged behavior: a family envelope received over BLE (not from
        // the relay) still surfaces in the relay-upload query.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let env = carried(b"ble-family", b"hint-a", 9_000, 10);
        assert!(store
            .enqueue_carried_envelope(env.clone(), true, 1_000, BIG_BUDGET)
            .unwrap());

        let uploadable = store.family_carried_envelopes(10, 2_000, vec![]).unwrap();
        assert_eq!(uploadable, vec![env]);
    }

    // --- carried-upload marker (relay_uploaded_to) --------------------------

    #[test]
    fn marked_carried_envelope_is_never_offered_for_upload_again() {
        // The core of the re-post-storm fix: once an upload (or a fetch of
        // the same msg_id) confirms a relay holds the envelope, the upload
        // query stops offering it -- but it stays deliverable over BLE,
        // because the marker gates re-upload only, never removal.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let env = carried(b"ble-family", b"hint-a", 9_000, 10);
        assert!(store
            .enqueue_carried_envelope(env.clone(), true, 1_000, BIG_BUDGET)
            .unwrap());
        assert_eq!(
            store
                .family_carried_envelopes(10, 2_000, vec![])
                .unwrap()
                .len(),
            1
        );

        assert!(store
            .mark_carried_envelope_relay_uploaded(
                b"ble-family".to_vec(),
                "https://relay.example".to_string(),
            )
            .unwrap());
        assert!(store
            .family_carried_envelopes(10, 2_000, vec![])
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .carried_envelopes_for_hints(vec![b"hint-a".to_vec()], 2_000)
                .unwrap(),
            vec![env],
        );
    }

    #[test]
    fn marker_is_first_writer_wins_and_marking_nothing_is_not_an_error() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let env = carried(b"ble-family", b"hint-a", 9_000, 10);
        assert!(store
            .enqueue_carried_envelope(env, true, 1_000, BIG_BUDGET)
            .unwrap());

        assert!(store
            .mark_carried_envelope_relay_uploaded(
                b"ble-family".to_vec(),
                "https://first.example".to_string(),
            )
            .unwrap());
        // Second confirmation (e.g. the same envelope fetched back off the
        // relay a moment after the upload marked it) changes nothing.
        assert!(!store
            .mark_carried_envelope_relay_uploaded(
                b"ble-family".to_vec(),
                "https://second.example".to_string(),
            )
            .unwrap());
        // Marking an unknown msg_id (a fetched envelope we never carried) is
        // an ordinary no-op, not an error.
        assert!(!store
            .mark_carried_envelope_relay_uploaded(
                b"never-carried".to_vec(),
                "https://relay.example".to_string(),
            )
            .unwrap());
    }

    #[test]
    fn clearing_markers_reoffers_the_carry_queue_once() {
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        let env = carried(b"ble-family", b"hint-a", 9_000, 10);
        assert!(store
            .enqueue_carried_envelope(env.clone(), true, 1_000, BIG_BUDGET)
            .unwrap());
        assert!(store
            .mark_carried_envelope_relay_uploaded(
                b"ble-family".to_vec(),
                "https://old.example".to_string(),
            )
            .unwrap());
        assert!(store
            .family_carried_envelopes(10, 2_000, vec![])
            .unwrap()
            .is_empty());

        assert_eq!(store.clear_carried_relay_upload_markers().unwrap(), 1);
        assert_eq!(
            store.family_carried_envelopes(10, 2_000, vec![]).unwrap(),
            vec![env],
        );
        // Idempotent: nothing left to clear.
        assert_eq!(store.clear_carried_relay_upload_markers().unwrap(), 0);
    }

    #[test]
    fn contact_relay_update_clears_upload_markers_so_the_new_mailbox_gets_one_post() {
        // A T23 relay-change notice moves a contact's mailbox; everything
        // "already uploaded" was uploaded to the OLD one, so the applied
        // notice must re-offer the carry queue. A stale (not-newer) notice
        // must not.
        let store = MessageStore::open(":memory:".to_string()).unwrap();
        store.upsert_contact(contact(b"alice-id", "Alice")).unwrap();
        let env = carried(b"ble-family", b"hint-a", 9_000, 10);
        assert!(store
            .enqueue_carried_envelope(env.clone(), true, 1_000, BIG_BUDGET)
            .unwrap());
        assert!(store
            .mark_carried_envelope_relay_uploaded(
                b"ble-family".to_vec(),
                "https://old.example".to_string(),
            )
            .unwrap());

        let notice = relay_notice(b"alice-id", 100, "https://new.relay.example");
        assert!(store
            .apply_contact_relay_update(b"alice-id".to_vec(), notice.clone())
            .unwrap());
        assert_eq!(
            store.family_carried_envelopes(10, 2_000, vec![]).unwrap(),
            vec![env],
        );

        // Re-mark, then replay the same (now stale) notice: no endpoint
        // move, so the marker must survive.
        assert!(store
            .mark_carried_envelope_relay_uploaded(
                b"ble-family".to_vec(),
                "https://new.relay.example".to_string(),
            )
            .unwrap());
        assert!(!store
            .apply_contact_relay_update(b"alice-id".to_vec(), notice)
            .unwrap());
        assert!(store
            .family_carried_envelopes(10, 2_000, vec![])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn open_migrates_an_old_carried_envelopes_table_to_add_from_relay() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cruisemesh-store-migration-carried-{unique}.sqlite"
        ));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE carried_envelopes (
                msg_id         BLOB PRIMARY KEY,
                hop_ttl        INTEGER NOT NULL,
                expiry         INTEGER NOT NULL,
                recipient_hint BLOB NOT NULL,
                sealed         BLOB NOT NULL,
                is_family      INTEGER NOT NULL,
                received_at    INTEGER NOT NULL,
                size_bytes     INTEGER NOT NULL
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO carried_envelopes
                (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family, received_at, size_bytes)
             VALUES (?1, 7, 9000, ?2, ?3, 1, 1000, 4)",
            params![
                b"legacy-one".as_slice(),
                b"same-hint".as_slice(),
                b"same".as_slice()
            ],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO carried_envelopes
                (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family, received_at, size_bytes)
             VALUES (?1, 7, 9000, ?2, ?3, 1, 2000, 4)",
            params![
                b"legacy-two".as_slice(),
                b"same-hint".as_slice(),
                b"same".as_slice()
            ],
        )
        .unwrap();
        drop(conn);

        // Opening an old store (pre-dating from_relay) must migrate the
        // column, not error, and the new relay-sourced path must work
        // against the migrated schema.
        let store = MessageStore::open(path_str.clone()).unwrap();
        assert_eq!(store.carried_len().unwrap(), 1, "migration dedupes content");
        let env = carried(b"proxy", b"hint-a", 9_000, 10);
        assert!(store
            .enqueue_relay_carried_envelope(env.clone(), 1_000)
            .unwrap());
        let found = store
            .carried_envelopes_for_hints(vec![b"hint-a".to_vec()], 2_000)
            .unwrap();
        assert_eq!(found, vec![env]);

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn open_batch_migrates_many_legacy_carried_rows_keeping_the_oldest_per_duplicate_group() {
        // FC3: migrate_carried_content_digests used to be one SELECT-LIMIT-1
        // + single-row UPDATE/DELETE pair per legacy row. This exercises it
        // at a scale (multiple duplicate groups of different sizes, plus a
        // singleton) that would have meant dozens of round trips under the
        // old per-row loop, and checks the batched rewrite keeps the exact
        // same tie-break: within a group of rows whose (recipient_hint,
        // sealed) collide, the one with the earliest `received_at` survives.
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "cruisemesh-store-migration-carried-batch-{unique}.sqlite"
        ));
        let path_str = path.to_string_lossy().to_string();
        let conn = Connection::open(&path_str).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE carried_envelopes (
                msg_id         BLOB PRIMARY KEY,
                hop_ttl        INTEGER NOT NULL,
                expiry         INTEGER NOT NULL,
                recipient_hint BLOB NOT NULL,
                sealed         BLOB NOT NULL,
                is_family      INTEGER NOT NULL,
                received_at    INTEGER NOT NULL,
                size_bytes     INTEGER NOT NULL
            );
            ",
        )
        .unwrap();
        // Group A: 4 rows sharing (hint, sealed) -- a1 (received_at=100) is
        // the oldest and must survive.
        for (msg_id, received_at) in [("a1", 100), ("a2", 200), ("a3", 300), ("a4", 400)] {
            conn.execute(
                "INSERT INTO carried_envelopes
                    (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family, received_at, size_bytes)
                 VALUES (?1, 7, 9000, ?2, ?3, 1, ?4, 4)",
                params![msg_id.as_bytes(), b"hint-a".as_slice(), b"same-a".as_slice(), received_at],
            )
            .unwrap();
        }
        // Group B: 2 rows sharing (hint, sealed) -- b1 (received_at=150) is
        // the oldest.
        for (msg_id, received_at) in [("b1", 150), ("b2", 250)] {
            conn.execute(
                "INSERT INTO carried_envelopes
                    (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family, received_at, size_bytes)
                 VALUES (?1, 7, 9000, ?2, ?3, 1, ?4, 4)",
                params![msg_id.as_bytes(), b"hint-b".as_slice(), b"same-b".as_slice(), received_at],
            )
            .unwrap();
        }
        // Singleton: nothing to dedupe against.
        conn.execute(
            "INSERT INTO carried_envelopes
                (msg_id, hop_ttl, expiry, recipient_hint, sealed, is_family, received_at, size_bytes)
             VALUES (?1, 7, 9000, ?2, ?3, 1, 50, 4)",
            params![b"c1".as_slice(), b"hint-c".as_slice(), b"unique-c".as_slice()],
        )
        .unwrap();
        drop(conn);

        let store = MessageStore::open(path_str.clone()).unwrap();
        assert_eq!(
            store.carried_len().unwrap(),
            3,
            "one survivor per duplicate group plus the singleton"
        );

        let mut surviving_ids: Vec<Vec<u8>> = store
            .carried_envelopes_for_hints(
                vec![b"hint-a".to_vec(), b"hint-b".to_vec(), b"hint-c".to_vec()],
                1_000,
            )
            .unwrap()
            .into_iter()
            .map(|e| e.msg_id)
            .collect();
        surviving_ids.sort();
        assert_eq!(
            surviving_ids,
            vec![b"a1".to_vec(), b"b1".to_vec(), b"c1".to_vec()],
            "the oldest row (by received_at) in each duplicate group must be the one kept"
        );

        drop(store);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn backup_to_writes_a_consistent_reopenable_snapshot() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        store
            .insert_message(msg(b"chat", b"sender", 1, "backed up"))
            .unwrap();
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cruisemesh-backup-{unique}.sqlite"));

        store.backup_to(path.to_string_lossy().to_string()).unwrap();
        let restored = MessageStore::open(path.to_string_lossy().to_string()).unwrap();
        assert_eq!(
            restored.messages_for_chat(b"chat".to_vec()).unwrap().len(),
            1
        );

        drop(restored);
        fs::remove_file(path).unwrap();
    }

    // --- FC10: WAL / busy_timeout -------------------------------------------

    fn journal_mode(store: &MessageStore) -> String {
        store
            .conn
            .lock()
            .expect("store mutex poisoned")
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap()
            .to_lowercase()
    }

    #[test]
    fn open_sets_wal_journal_mode_for_a_new_file_backed_store() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cruisemesh-wal-{unique}.sqlite"));

        let store = MessageStore::open(path.to_string_lossy().to_string()).unwrap();
        assert_eq!(journal_mode(&store), "wal");

        drop(store);
        fs::remove_file(&path).unwrap();
        // Best-effort: WAL's sidecar files are normally removed automatically
        // when the last connection to the database closes, but don't fail
        // the test if that didn't happen.
        let _ = fs::remove_file(format!("{}-wal", path.to_string_lossy()));
        let _ = fs::remove_file(format!("{}-shm", path.to_string_lossy()));
    }

    #[test]
    fn backup_to_still_works_with_data_when_the_source_store_is_under_wal() {
        // FC10: backup_to's VACUUM INTO runs under the same Mutex<Connection>
        // as every other store call. Confirm a WAL-mode, file-backed source
        // store with data can still back itself up, and that the resulting
        // snapshot is itself a normal, reopenable, WAL-mode store.
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let source_path =
            std::env::temp_dir().join(format!("cruisemesh-wal-source-{unique}.sqlite"));
        let dest_path = std::env::temp_dir().join(format!("cruisemesh-wal-backup-{unique}.sqlite"));

        let store = MessageStore::open(source_path.to_string_lossy().to_string()).unwrap();
        assert_eq!(journal_mode(&store), "wal");
        store
            .insert_message(msg(b"chat", b"sender", 1, "backed up under wal"))
            .unwrap();

        store
            .backup_to(dest_path.to_string_lossy().to_string())
            .unwrap();

        let restored = MessageStore::open(dest_path.to_string_lossy().to_string()).unwrap();
        assert_eq!(
            restored.messages_for_chat(b"chat".to_vec()).unwrap().len(),
            1
        );
        assert_eq!(journal_mode(&restored), "wal");

        drop(store);
        drop(restored);
        fs::remove_file(&source_path).unwrap();
        fs::remove_file(&dest_path).unwrap();
    }

    #[test]
    fn backup_to_rejects_relative_and_existing_destinations() {
        let store = MessageStore::open(":memory:".into()).unwrap();
        assert!(store.backup_to("relative.sqlite".into()).is_err());

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("cruisemesh-existing-{unique}.sqlite"));
        fs::write(&path, b"leave intact").unwrap();
        assert!(store.backup_to(path.to_string_lossy().to_string()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"leave intact");
        fs::remove_file(path).unwrap();
    }

    // --- FC6: mutex poison recovery -----------------------------------------

    #[test]
    fn store_recovers_from_a_poisoned_mutex_instead_of_crash_looping() {
        // Before FC6, every store method did `self.conn.lock().expect(...)`:
        // once ANY panic happened while holding the lock (a bug, an
        // unexpected SQLite error path), the stdlib marks the Mutex
        // poisoned forever, and every later `.lock().expect(...)` call
        // would itself panic -- turning one panic into a permanent
        // process-wide store outage across the UniFFI boundary. Simulate
        // that first panic here (inside `catch_unwind` so the test process
        // itself survives) and confirm a later store call still succeeds
        // instead of panicking.
        let store = MessageStore::open(":memory:".to_string()).unwrap();

        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = store.conn.lock().unwrap();
            panic!("FC6 test: simulated panic while holding the store mutex");
        }));
        assert!(panicked.is_err(), "the closure above should have panicked");
        assert!(store.conn.is_poisoned(), "the mutex should now be poisoned");

        // A later call through the normal store API must recover the guard
        // and succeed, not panic.
        assert!(store
            .insert_message(msg(b"chat", b"sender", 1, "still works"))
            .unwrap());
        assert_eq!(store.messages_for_chat(b"chat".to_vec()).unwrap().len(), 1);
    }
}
