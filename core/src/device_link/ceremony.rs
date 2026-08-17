//! The link ceremony, as two explicit state machines
//! (`specs/multi-device-v1.md` §9.1–§9.2).
//!
//! This module is shaped after `core/src/session/relay_pass.rs` and
//! `core/src/media/lan_pull.rs`, and for the same reasons: typed actions out,
//! typed resumes in, an explicit `now_ms`, declared budgets, one outstanding
//! action at a time, and no operating system anywhere. There is no socket here,
//! no camera, no timer, no thread. The shell carries bytes over LAN, BLE, or a
//! relay rendezvous and hands back exactly what arrived.
//!
//! ```text
//! new device (§9.1 shows the QR)            approving device (§9.2 scans it)
//! ------------------------------            --------------------------------
//! start(now)     -> ShowQr                  scan(qr, device_count)
//!                                           start(now)     -> SendBytes(msg 1)
//! resume_peer_bytes(msg 1)
//!                -> SendBytes(msg 2)
//!                                           resume_peer_bytes(msg 2)
//!                                                          -> SendBytes(msg 3)
//! resume_peer_bytes(msg 3)
//!                -> ShowSas(confirm_here=false)
//!                                           resume_sent()  -> ShowSas(confirm_here=true)
//!                                           confirm(now)   -> SendBytes(confirm)
//! resume_peer_bytes(confirm)
//!                -> Finished(ChannelReady)  resume_sent()  -> Finished(ChannelReady)
//! ```
//!
//! # Which device confirms
//!
//! Both ends derive the same short authentication string and both ends show it,
//! but only [`CoreLinkApprovingDevice`] has a [`confirm`](CoreLinkApprovingDevice::confirm)
//! method at all. §9.2 puts the explicit tap on the device that is already part
//! of the person, because that is the device whose judgement is worth anything:
//! a brand-new phone confirming its own link would be confirming whatever it is
//! talking to. The new device's [`CoreLinkActionKind::ShowSas`] carries
//! `confirm_here: false` and it waits for the approving device's sealed confirm
//! frame; there is no local override, in this module or in a shell.
//!
//! # What the channel is bound to
//!
//! The handshake is `Noise_XX_25519_ChaChaPoly_BLAKE2s` with a prologue that
//! commits to the scanned offer's ephemeral key and expiry, and **both statics
//! are ephemeral**: the new device's is the key printed in the QR, and the
//! approving device mints a fresh one per ceremony rather than reusing its
//! long-term device key. No identity key, on either side, is ever spent on this
//! transport — identity arrives inside the channel, in §9.3's signed bootstrap.
//!
//! The approving device additionally refuses any peer whose Noise static is not
//! the key it scanned, so the channel is provably the QR's channel and not some
//! other device that reached the rendezvous first. The new device cannot make
//! the mirror-image check — it has never seen the approver's key — which is
//! precisely the gap the short authentication string and the human tap fill.
//!
//! The rendezvous hints deliberately stay *outside* the prologue. They carry no
//! authority: rewriting one can only send the scanner to an endpoint whose
//! occupant cannot complete the handshake. Binding them would have traded that
//! non-property for a forward-compatibility break the first time a later build
//! adds a hint kind this one skips.
//!
//! # Budgets and endings
//!
//! Every ending is named. A ceremony finishes as
//! [`CoreLinkOutcome::ChannelReady`], or it says why it did not: the person
//! declined, someone cancelled, the deadline passed, the offer had expired, the
//! person is at §14.3's hard device cap, the handshake failed closed, or the
//! peer sent something the ceremony has no state for. There is no silent
//! failure and no retry loop: a security handshake that went wrong ends, and
//! the person starts a fresh one with a fresh QR.

use std::sync::{Mutex, MutexGuard};

use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use snow::{Builder, HandshakeState, TransportState};

use super::qr::{
    core_build_link_qr, core_link_rendezvous_id, core_parse_link_qr, LinkRendezvous,
    LINK_QR_DEFAULT_LIFETIME_MS,
};
use crate::device_roster::{core_device_add_outcome, DeviceAddOutcome};
use crate::CoreError;

const LINK_NOISE_PARAMS: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";
/// Distinct from `lan_session.rs`'s same-LAN prologue and from every signing
/// domain in the crate: a handshake minted for one purpose can never be
/// replayed into another.
const LINK_NOISE_PROLOGUE_DOMAIN: &[u8] = b"CruiseMesh device link ceremony v1\0";
/// Hashing domain for the short authentication string. Not a signing domain.
const LINK_SAS_DOMAIN: &[u8] = b"CruiseMesh device link SAS v1\0";

const NOISE_MAX_MESSAGE_SIZE: usize = 65_535;
const NOISE_TAG_SIZE: usize = 16;
/// Bound on one ceremony message the shell may hand back. Handshake messages
/// are under a hundred bytes and the control frames are 33; this leaves room
/// without letting a peer's first move be a large allocation.
const LINK_MAX_CEREMONY_MESSAGE_BYTES: usize = 4 * 1024;
/// Bound on one sealed frame over the ready channel — §9.3's bootstrap chunks
/// its export rather than sending it whole.
pub const LINK_CHANNEL_MAX_PLAINTEXT_BYTES: usize = 60 * 1024;

/// Control frames over the ready channel. `tag(1) || channel_binding(32)` for a
/// confirm; the tag alone for a decline.
const CTRL_CONFIRM: u8 = 0x01;
const CTRL_DECLINE: u8 = 0x02;
const CHANNEL_BINDING_LEN: usize = 32;

/// Digits in the short authentication string. Six, grouped `NNN NNN`: the
/// person is comparing two screens held side by side, and the attacker this
/// number exists to stop gets exactly one online guess before the ceremony ends
/// — the same trade Bluetooth numeric comparison makes. A mismatch is not a
/// retry prompt, it is a declined ceremony.
pub const LINK_SAS_DIGITS: u32 = 6;

/// Declared budgets, in the style `PullBudgets` established.
#[derive(uniffi::Record, Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkBudgets {
    /// Wall-clock bound on one ceremony, from `start` to the confirmed channel.
    /// Generous, because a human tap is inside it.
    pub deadline_ms: i64,
    /// What an [`CoreLinkActionKind::AwaitPeer`] action suggests as a poll
    /// interval — advice for a relay rendezvous, ignored by a socket that can
    /// simply block.
    pub poll_interval_ms: i64,
    /// How long a freshly built offer stands.
    pub qr_lifetime_ms: i64,
}

impl Default for LinkBudgets {
    fn default() -> Self {
        LinkBudgets {
            deadline_ms: 180_000,
            poll_interval_ms: 1_000,
            qr_lifetime_ms: LINK_QR_DEFAULT_LIFETIME_MS,
        }
    }
}

#[uniffi::export]
pub fn core_link_default_budgets() -> LinkBudgets {
    LinkBudgets::default()
}

/// Which half of the ceremony an object is.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreLinkRole {
    /// Shows the QR and is the Noise responder (§9.1).
    NewDevice,
    /// Scans the QR, is the Noise initiator, and holds the confirm (§9.2).
    ApprovingDevice,
}

/// Where a ceremony is. Additive by design: §9.3's bootstrap streaming and
/// §9.4's roster acknowledgement extend this after [`CoreLinkPhase::ChannelReady`].
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreLinkPhase {
    NotStarted,
    /// The offer is on screen and nobody has knocked (§9.1).
    ShowingQr,
    /// Noise messages are in flight (§9.2).
    Handshaking,
    /// Both ends hold the digits; the existing device owes an explicit tap.
    AwaitingConfirm,
    /// Confirmed channel. §9.3 continues from here.
    ChannelReady,
    Finished,
}

/// How a ceremony ended. Every ending is named; none is silent.
#[derive(uniffi::Enum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreLinkOutcome {
    /// §9.2 complete: a confirmed Noise channel, ready for §9.3's bootstrap.
    ChannelReady,
    /// The person said the digits did not match, or the other end did.
    Declined,
    /// Someone put the phone down: [`CoreLinkNewDevice::cancel`] or
    /// [`CoreLinkApprovingDevice::cancel`].
    Cancelled,
    /// The declared deadline passed with the ceremony unfinished.
    TimedOut,
    /// The offer had already expired when the scanner arrived.
    QrExpired,
    /// §14.3: this person already holds the hard cap of devices, so the add is
    /// refused before a single byte moves.
    DeviceCapReached,
    /// A Noise message failed to open, or the peer's static key was not the one
    /// in the scanned QR.
    HandshakeFailed,
    /// A well-formed channel carried something this ceremony has no state for.
    ProtocolError,
}

/// What an action asks of the shell.
#[derive(uniffi::Enum, Clone, Debug, PartialEq, Eq)]
pub enum CoreLinkActionKind {
    /// Put this on screen and listen on the rendezvous it names. Resumed by
    /// [`CoreLinkNewDevice::resume_peer_bytes`] when someone knocks.
    ShowQr { qr_text: String },
    /// Hand these bytes to the peer, then call `resume_sent`. A peer reply that
    /// arrives first is itself proof of delivery and is accepted in place of
    /// the `resume_sent` that would have preceded it.
    SendBytes { bytes: Vec<u8> },
    /// Nothing to send. Wait for peer bytes, polling a relay rendezvous no
    /// faster than `wait_ms`.
    AwaitPeer { wait_ms: i64 },
    /// Show these digits. `confirm_here` is true on the approving device only
    /// (§9.2) — it is the one screen with a button. `warn_soft_cap` carries
    /// §14.3's soft-cap warning to that same screen, where the person is
    /// already being asked to decide.
    ShowSas {
        sas: String,
        confirm_here: bool,
        warn_soft_cap: bool,
    },
    /// Nothing more. The summary carries the ending.
    Finished { summary: CoreLinkSummary },
}

/// One step of a ceremony.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreLinkAction {
    pub role: CoreLinkRole,
    /// Strictly increasing across the actions one ceremony emits. A restated
    /// action — what a stale resume gets back — keeps the id it already had,
    /// because nothing new was emitted.
    pub action_id: u64,
    pub phase: CoreLinkPhase,
    pub kind: CoreLinkActionKind,
}

/// How a ceremony ended, and what it leaves behind for §9.3.
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct CoreLinkSummary {
    pub role: CoreLinkRole,
    pub outcome: CoreLinkOutcome,
    /// The digits both ends showed, once they existed.
    pub sas: Option<String>,
    /// The Noise handshake hash: 32 bytes committing to the prologue, both
    /// statics, and every handshake message. §9.3's bootstrap and §9.4's roster
    /// acknowledgement bind to this, so a bootstrap can never be replayed into
    /// a different ceremony.
    pub channel_binding: Option<Vec<u8>>,
    /// The peer's Noise static key. On the approving device this is provably
    /// the key printed in the QR.
    pub peer_static_pk: Option<Vec<u8>>,
    /// §14.3: this add takes the person past the soft cap and the shell should
    /// say so.
    pub soft_cap_warning: bool,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub messages_sent: u32,
    pub messages_received: u32,
    /// Resumes that did not match the outstanding action and were ignored.
    pub stale_resumes_ignored: u32,
}

/// The short authentication string both ends derive from the channel binding
/// (§9.2). Pure, and frozen by a golden vector: two builds that disagree here
/// would show a person two different numbers for one channel and teach them to
/// tap through a mismatch.
#[uniffi::export]
pub fn core_link_sas(channel_binding: Vec<u8>) -> Result<String, CoreError> {
    link_sas(&channel_binding)
}

fn link_sas(channel_binding: &[u8]) -> Result<String, CoreError> {
    if channel_binding.len() != CHANNEL_BINDING_LEN {
        return Err(CoreError::InvalidKeyLength {
            expected: CHANNEL_BINDING_LEN as u32,
            actual: channel_binding.len() as u32,
        });
    }
    let mut hasher = Blake2bVar::new(8).expect("valid blake2b output length");
    hasher.update(LINK_SAS_DOMAIN);
    hasher.update(channel_binding);
    let mut digest = [0u8; 8];
    hasher
        .finalize_variable(&mut digest)
        .expect("output buffer matches configured length");
    // A 64-bit reduction into a million: the modulo bias is under 2^-44 and the
    // whole number is one online guess wide anyway.
    let value = u64::from_be_bytes(digest) % 1_000_000;
    let digits = format!("{value:06}");
    Ok(format!("{} {}", &digits[..3], &digits[3..]))
}

// ---------------------------------------------------------------------------
// Shared machinery
// ---------------------------------------------------------------------------

enum Channel {
    Handshake(Box<HandshakeState>),
    Transport(Box<TransportState>),
    /// Momentarily between the two, or torn down.
    Spent,
}

struct Ceremony {
    role: CoreLinkRole,
    budgets: LinkBudgets,
    rendezvous: LinkRendezvous,
    /// The new device's own QR text; empty on the approving device, which
    /// scanned one rather than minting it.
    qr_text: String,
    channel: Channel,
    channel_binding: Option<Vec<u8>>,
    sas: Option<String>,
    peer_static_pk: Option<Vec<u8>>,
    soft_cap_warning: bool,
    /// §14.3 refusal, decided at scan time and applied at `start`.
    cap_refused: bool,
    /// Set when the outstanding send is the last thing this side does.
    pending_terminal: Option<CoreLinkOutcome>,
    started: bool,
    started_at_ms: i64,
    /// When the ceremony deadline started running, or `None` while an offer is
    /// merely on screen. A QR left up on a kitchen table is bounded by its own
    /// expiry, not by the ceremony clock — the ceremony has not begun until
    /// someone knocks, and starting the clock at `start` would have made every
    /// unattended offer end as [`CoreLinkOutcome::TimedOut`] rather than as the
    /// expired offer it is.
    deadline_from_ms: Option<i64>,
    now_ms: i64,
    next_action_id: u64,
    outstanding: Option<CoreLinkAction>,
    finished: Option<CoreLinkSummary>,
    messages_sent: u32,
    messages_received: u32,
    stale_resumes_ignored: u32,
}

impl Ceremony {
    fn new(
        role: CoreLinkRole,
        budgets: LinkBudgets,
        rendezvous: LinkRendezvous,
        qr_text: String,
        handshake: HandshakeState,
    ) -> Self {
        Ceremony {
            role,
            budgets,
            rendezvous,
            qr_text,
            channel: Channel::Handshake(Box::new(handshake)),
            channel_binding: None,
            sas: None,
            peer_static_pk: None,
            soft_cap_warning: false,
            cap_refused: false,
            pending_terminal: None,
            started: false,
            started_at_ms: 0,
            deadline_from_ms: None,
            now_ms: 0,
            next_action_id: 1,
            outstanding: None,
            finished: None,
            messages_sent: 0,
            messages_received: 0,
            stale_resumes_ignored: 0,
        }
    }

    // -- action plumbing ---------------------------------------------------

    fn emit(&mut self, phase: CoreLinkPhase, kind: CoreLinkActionKind) -> CoreLinkAction {
        if let CoreLinkActionKind::SendBytes { .. } = kind {
            self.messages_sent = self.messages_sent.saturating_add(1);
        }
        let action = CoreLinkAction {
            role: self.role,
            action_id: self.next_action_id,
            phase,
            kind,
        };
        self.next_action_id = self.next_action_id.saturating_add(1);
        self.outstanding = Some(action.clone());
        action
    }

    fn finish(&mut self, outcome: CoreLinkOutcome, now_ms: i64) -> CoreLinkAction {
        if self.finished.is_none() {
            self.finished = Some(CoreLinkSummary {
                role: self.role,
                outcome,
                sas: self.sas.clone(),
                channel_binding: self.channel_binding.clone(),
                peer_static_pk: self.peer_static_pk.clone(),
                soft_cap_warning: self.soft_cap_warning,
                started_at_ms: self.started_at_ms,
                finished_at_ms: now_ms,
                messages_sent: self.messages_sent,
                messages_received: self.messages_received,
                stale_resumes_ignored: self.stale_resumes_ignored,
            });
            // A ceremony that did not reach a confirmed channel keeps no
            // channel: there is nothing a declined, timed-out or failed
            // handshake could legitimately seal afterwards.
            if outcome != CoreLinkOutcome::ChannelReady {
                self.channel = Channel::Spent;
            }
        }
        self.outstanding = None;
        self.finished_action()
    }

    fn finished_action(&self) -> CoreLinkAction {
        let summary = self
            .finished
            .clone()
            .expect("finished_action is only reached once a summary exists");
        CoreLinkAction {
            role: self.role,
            action_id: 0,
            phase: CoreLinkPhase::Finished,
            kind: CoreLinkActionKind::Finished { summary },
        }
    }

    fn restate(&mut self) -> CoreLinkAction {
        if self.finished.is_some() {
            return self.finished_action();
        }
        match &self.outstanding {
            Some(action) => action.clone(),
            None => CoreLinkAction {
                role: self.role,
                action_id: 0,
                phase: CoreLinkPhase::NotStarted,
                kind: CoreLinkActionKind::AwaitPeer {
                    wait_ms: self.budgets.poll_interval_ms,
                },
            },
        }
    }

    fn stale(&mut self) -> CoreLinkAction {
        self.stale_resumes_ignored = self.stale_resumes_ignored.saturating_add(1);
        if let Some(summary) = self.finished.as_mut() {
            summary.stale_resumes_ignored = self.stale_resumes_ignored;
        }
        self.restate()
    }

    /// This ceremony's clock, clamped non-decreasing.
    ///
    /// `now_ms` is a wall clock, and a wall clock can go backwards: an NTP
    /// correction, a manual date change, a timezone-confused shell. A ceremony
    /// that accepted a rewound clock would have its deadline pushed back by
    /// exactly the amount of the rewind, and a peer that could induce one could
    /// hold a confirm screen open indefinitely. Time only ever moves forward in
    /// here, so the deadline is a bound rather than a suggestion.
    ///
    /// Clamping is deliberately one-directional: a clock that jumps *forward*
    /// is honoured, because a ceremony ending early is the safe direction and a
    /// person can simply start another one.
    fn clock(&mut self, now_ms: i64) -> i64 {
        if now_ms > self.now_ms {
            self.now_ms = now_ms;
        }
        self.now_ms
    }

    /// The one place every entry point passes through: a finished ceremony
    /// restates its summary, and a live one that ran past its deadline ends
    /// here rather than a step later.
    fn guard(&mut self, now_ms: i64) -> Option<CoreLinkAction> {
        if self.finished.is_some() {
            return Some(self.finished_action());
        }
        let now_ms = self.clock(now_ms);
        if let Some(from_ms) = self.deadline_from_ms {
            if now_ms.saturating_sub(from_ms) > self.budgets.deadline_ms {
                return Some(self.finish(CoreLinkOutcome::TimedOut, now_ms));
            }
        }
        // An offer that expired while it sat on screen stops being an offer.
        // Once the handshake is under way the deadline governs instead: the
        // ceremony is no longer an open invitation, it is a conversation.
        if matches!(self.outstanding, Some(CoreLinkAction { ref kind, .. }) if matches!(kind, CoreLinkActionKind::ShowQr { .. }))
            && now_ms > self.rendezvous.expires_at_ms
        {
            return Some(self.finish(CoreLinkOutcome::QrExpired, now_ms));
        }
        None
    }

    // -- Noise -------------------------------------------------------------

    fn handshake_mut(&mut self) -> Option<&mut HandshakeState> {
        match &mut self.channel {
            Channel::Handshake(handshake) => Some(handshake.as_mut()),
            _ => None,
        }
    }

    fn write_handshake_message(&mut self) -> Option<Vec<u8>> {
        let mut buffer = vec![0u8; NOISE_MAX_MESSAGE_SIZE];
        let handshake = self.handshake_mut()?;
        let len = handshake.write_message(&[], &mut buffer).ok()?;
        buffer.truncate(len);
        Some(buffer)
    }

    fn read_handshake_message(&mut self, message: &[u8]) -> Option<()> {
        let mut buffer = vec![0u8; NOISE_MAX_MESSAGE_SIZE];
        let handshake = self.handshake_mut()?;
        handshake.read_message(message, &mut buffer).ok()?;
        Some(())
    }

    fn handshake_finished(&self) -> bool {
        match &self.channel {
            Channel::Handshake(handshake) => handshake.is_handshake_finished(),
            Channel::Transport(_) => true,
            Channel::Spent => false,
        }
    }

    /// Capture the binding, derive the digits, and move into transport mode.
    /// The handshake hash has to be read before the state is consumed, which is
    /// why this is one step rather than two.
    fn finalize_channel(&mut self) -> bool {
        let handshake = match std::mem::replace(&mut self.channel, Channel::Spent) {
            Channel::Handshake(handshake) => handshake,
            other => {
                self.channel = other;
                return matches!(self.channel, Channel::Transport(_));
            }
        };
        let binding = handshake.get_handshake_hash().to_vec();
        let peer_static = handshake.get_remote_static().map(<[u8]>::to_vec);
        let Ok(sas) = link_sas(&binding) else {
            return false;
        };
        let Ok(transport) = handshake.into_transport_mode() else {
            return false;
        };
        self.channel = Channel::Transport(Box::new(transport));
        self.channel_binding = Some(binding);
        self.sas = Some(sas);
        self.peer_static_pk = peer_static;
        true
    }

    fn seal(&mut self, plaintext: &[u8]) -> Option<Vec<u8>> {
        if plaintext.len() > LINK_CHANNEL_MAX_PLAINTEXT_BYTES {
            return None;
        }
        let Channel::Transport(transport) = &mut self.channel else {
            return None;
        };
        let mut buffer = vec![0u8; plaintext.len() + NOISE_TAG_SIZE];
        let len = transport.write_message(plaintext, &mut buffer).ok()?;
        buffer.truncate(len);
        Some(buffer)
    }

    fn open(&mut self, frame: &[u8]) -> Option<Vec<u8>> {
        if frame.len() > LINK_CHANNEL_MAX_PLAINTEXT_BYTES + NOISE_TAG_SIZE {
            return None;
        }
        let Channel::Transport(transport) = &mut self.channel else {
            return None;
        };
        let mut buffer = vec![0u8; frame.len()];
        let len = transport.read_message(frame, &mut buffer).ok()?;
        buffer.truncate(len);
        Some(buffer)
    }

    fn control_frame(&mut self, tag: u8) -> Option<Vec<u8>> {
        let mut plaintext = vec![tag];
        if tag == CTRL_CONFIRM {
            plaintext.extend_from_slice(self.channel_binding.as_deref()?);
        }
        self.seal(&plaintext)
    }

    // -- shared steps ------------------------------------------------------

    /// What the outstanding action is expecting, with one substitution: a peer
    /// message that arrives while a send is outstanding is itself proof the
    /// send left, so it stands in for the `resume_sent` that never came.
    /// Without that, a shell reporting delivery late would deadlock a ceremony
    /// whose next message is already in hand.
    ///
    /// The exception is the last send of all — the confirm or decline frame,
    /// which has `pending_terminal` set. Nothing the peer says after that is
    /// answered, because there is nothing left to answer with.
    fn expecting(&mut self) -> Option<CoreLinkActionKind> {
        let kind = self
            .outstanding
            .as_ref()
            .map(|action| action.kind.clone())?;
        if matches!(kind, CoreLinkActionKind::SendBytes { .. }) {
            if self.pending_terminal.is_some() {
                return Some(kind);
            }
            self.outstanding = None;
            return Some(CoreLinkActionKind::AwaitPeer {
                wait_ms: self.budgets.poll_interval_ms,
            });
        }
        Some(kind)
    }

    fn advance_handshake(&mut self, message: &[u8], now_ms: i64) -> CoreLinkAction {
        if message.is_empty() || message.len() > LINK_MAX_CEREMONY_MESSAGE_BYTES {
            return self.finish(CoreLinkOutcome::HandshakeFailed, now_ms);
        }
        self.messages_received = self.messages_received.saturating_add(1);
        if self.read_handshake_message(message).is_none() {
            return self.finish(CoreLinkOutcome::HandshakeFailed, now_ms);
        }
        // The approving device learns the peer's static from Noise XX message 2
        // and refuses anyone who is not the QR it scanned — before it writes
        // message 3, so a stranger is answered with nothing at all.
        //
        // An ABSENT static is a failure too, not a skip. This branch runs once,
        // on the message that carries it (XX's second), and a handshake that
        // reached this point without one is a pattern this build does not
        // understand — reading it as "nothing to check" would let exactly the
        // peer this check exists to refuse through the one door it guards.
        if self.role == CoreLinkRole::ApprovingDevice {
            let expected = self.rendezvous.link_pk.clone();
            let remote = self
                .handshake_mut()
                .and_then(|handshake| handshake.get_remote_static().map(<[u8]>::to_vec));
            match remote {
                Some(remote) if remote == expected => {}
                _ => return self.finish(CoreLinkOutcome::HandshakeFailed, now_ms),
            }
        }
        if !self.handshake_finished() {
            let Some(reply) = self.write_handshake_message() else {
                return self.finish(CoreLinkOutcome::HandshakeFailed, now_ms);
            };
            // Writing the last XX message finishes the initiator's handshake,
            // so the binding exists before the bytes are even handed over.
            if self.handshake_finished() && !self.finalize_channel() {
                return self.finish(CoreLinkOutcome::HandshakeFailed, now_ms);
            }
            return self.emit(
                CoreLinkPhase::Handshaking,
                CoreLinkActionKind::SendBytes { bytes: reply },
            );
        }
        if !self.finalize_channel() {
            return self.finish(CoreLinkOutcome::HandshakeFailed, now_ms);
        }
        self.show_sas()
    }

    fn show_sas(&mut self) -> CoreLinkAction {
        let sas = self.sas.clone().unwrap_or_default();
        self.emit(
            CoreLinkPhase::AwaitingConfirm,
            CoreLinkActionKind::ShowSas {
                sas,
                confirm_here: self.role == CoreLinkRole::ApprovingDevice,
                warn_soft_cap: self.soft_cap_warning,
            },
        )
    }

    fn resume_sent(&mut self, now_ms: i64) -> CoreLinkAction {
        if let Some(action) = self.guard(now_ms) {
            return action;
        }
        let now_ms = self.now_ms;
        if !matches!(
            self.outstanding,
            Some(CoreLinkAction {
                kind: CoreLinkActionKind::SendBytes { .. },
                ..
            })
        ) {
            return self.stale();
        }
        self.outstanding = None;
        if let Some(outcome) = self.pending_terminal.take() {
            return self.finish(outcome, now_ms);
        }
        if self.handshake_finished() {
            return self.show_sas();
        }
        self.emit(
            CoreLinkPhase::Handshaking,
            CoreLinkActionKind::AwaitPeer {
                wait_ms: self.budgets.poll_interval_ms,
            },
        )
    }

    fn cancel(&mut self, now_ms: i64) -> CoreLinkSummary {
        if self.finished.is_none() {
            let now_ms = self.clock(now_ms);
            self.finish(CoreLinkOutcome::Cancelled, now_ms);
        }
        self.finished
            .clone()
            .expect("cancel leaves a summary behind")
    }

    fn channel_ready(&self) -> bool {
        matches!(
            self.finished,
            Some(CoreLinkSummary {
                outcome: CoreLinkOutcome::ChannelReady,
                ..
            })
        )
    }
}

fn noise_error(error: snow::Error) -> CoreError {
    CoreError::Crypto(format!("device link handshake: {error}"))
}

/// `domain || link_pk || expires_at_ms`. See the module docs for why the
/// rendezvous hints are deliberately not in here.
fn link_prologue(rendezvous: &LinkRendezvous) -> Vec<u8> {
    let mut out = LINK_NOISE_PROLOGUE_DOMAIN.to_vec();
    out.extend_from_slice(&rendezvous.link_pk);
    out.extend_from_slice(&rendezvous.expires_at_ms.to_be_bytes());
    out
}

fn build_handshake(
    initiator: bool,
    rendezvous: &LinkRendezvous,
    local_private_key: &[u8],
) -> Result<HandshakeState, CoreError> {
    let params = LINK_NOISE_PARAMS
        .parse()
        .map_err(|error| CoreError::Crypto(format!("invalid link Noise parameters: {error}")))?;
    let prologue = link_prologue(rendezvous);
    let builder = Builder::new(params)
        .prologue(&prologue)
        .map_err(noise_error)?
        .local_private_key(local_private_key)
        .map_err(noise_error)?;
    if initiator {
        builder.build_initiator()
    } else {
        builder.build_responder()
    }
    .map_err(noise_error)
}

/// A fresh X25519 keypair for one ceremony, generated by the same resolver that
/// will use it — so the public key printed in the QR is exactly the static key
/// the handshake presents, with no clamping disagreement possible.
fn generate_link_keypair() -> Result<(Vec<u8>, Vec<u8>), CoreError> {
    let params = LINK_NOISE_PARAMS
        .parse()
        .map_err(|error| CoreError::Crypto(format!("invalid link Noise parameters: {error}")))?;
    let keypair = Builder::new(params)
        .generate_keypair()
        .map_err(noise_error)?;
    Ok((keypair.public, keypair.private))
}

// ---------------------------------------------------------------------------
// The new device (§9.1)
// ---------------------------------------------------------------------------

/// The half that shows the QR and waits to be adopted.
///
/// It holds no identity: a device running this has not been linked, has no
/// roster, no [`OwnDeviceFleet`](crate::OwnDeviceFleet), and nothing to
/// advertise, author, or ack with. §9.4's pre-activation invisibility starts as
/// a fact about this object rather than as a rule someone has to enforce.
#[derive(uniffi::Object)]
pub struct CoreLinkNewDevice {
    state: Mutex<Ceremony>,
    link_pk: Vec<u8>,
}

#[uniffi::export]
impl CoreLinkNewDevice {
    /// Mint an ephemeral link keypair and build the offer (§9.1).
    ///
    /// `lan_endpoints` and `relay_base_urls` are this device's OWN endpoints —
    /// nothing discovered, nothing third-party. The secret half of the link key
    /// stays inside this object and crosses no binding.
    #[uniffi::constructor]
    pub fn new(
        lan_endpoints: Vec<String>,
        relay_base_urls: Vec<String>,
        now_ms: i64,
        budgets: Option<LinkBudgets>,
    ) -> Result<Self, CoreError> {
        let budgets = budgets.unwrap_or_default();
        let (link_pk, link_sk) = generate_link_keypair()?;
        let rendezvous = LinkRendezvous {
            link_pk: link_pk.clone(),
            expires_at_ms: now_ms.saturating_add(budgets.qr_lifetime_ms),
            lan_endpoints,
            relay_base_urls,
        };
        // Built here rather than on demand so an offer that cannot be encoded
        // fails at construction, not at the moment a person is holding up a
        // phone.
        let qr_text = core_build_link_qr(rendezvous.clone())?;
        let handshake = build_handshake(false, &rendezvous, &link_sk)?;
        Ok(CoreLinkNewDevice {
            state: Mutex::new(Ceremony::new(
                CoreLinkRole::NewDevice,
                budgets,
                rendezvous,
                qr_text,
                handshake,
            )),
            link_pk,
        })
    }

    /// The `CMLINK1:` text to render as a QR.
    pub fn qr_text(&self) -> String {
        self.lock().qr_text.clone()
    }

    /// The ephemeral public key this offer publishes.
    pub fn link_pk(&self) -> Vec<u8> {
        self.link_pk.clone()
    }

    /// The mailbox namespace to listen on for a relay rendezvous.
    pub fn rendezvous_id(&self) -> Result<Vec<u8>, CoreError> {
        core_link_rendezvous_id(self.link_pk.clone())
    }

    pub fn rendezvous(&self) -> LinkRendezvous {
        self.lock().rendezvous.clone()
    }

    pub fn phase(&self) -> CoreLinkPhase {
        let state = self.lock();
        phase_of(&state)
    }

    /// Show the offer and start listening. Calling it twice restates rather
    /// than restarting: a ceremony is a one-shot object and a second QR is a
    /// second object.
    pub fn start(&self, now_ms: i64) -> CoreLinkAction {
        let mut state = self.lock();
        if state.finished.is_some() || state.started {
            return state.restate();
        }
        let now_ms = state.clock(now_ms);
        state.started = true;
        state.started_at_ms = now_ms;
        if now_ms > state.rendezvous.expires_at_ms {
            return state.finish(CoreLinkOutcome::QrExpired, now_ms);
        }
        let qr_text = state.qr_text.clone();
        state.emit(
            CoreLinkPhase::ShowingQr,
            CoreLinkActionKind::ShowQr { qr_text },
        )
    }

    /// The outstanding [`CoreLinkActionKind::SendBytes`] went out.
    pub fn resume_sent(&self, now_ms: i64) -> CoreLinkAction {
        self.lock().resume_sent(now_ms)
    }

    /// Bytes arrived from the peer.
    pub fn resume_peer_bytes(&self, now_ms: i64, bytes: Vec<u8>) -> CoreLinkAction {
        let mut state = self.lock();
        if let Some(action) = state.guard(now_ms) {
            return action;
        }
        let now_ms = state.now_ms;
        match state.expecting() {
            // Someone knocked, or the last handshake message landed.
            Some(CoreLinkActionKind::ShowQr { .. })
            | Some(CoreLinkActionKind::AwaitPeer { .. }) => {
                // §9.1's offer is over the moment someone knocks: from here the
                // ceremony deadline governs, not the QR's lifetime.
                state.deadline_from_ms.get_or_insert(now_ms);
                state.advance_handshake(&bytes, now_ms)
            }
            // The digits are up and the existing device has decided (§9.2).
            Some(CoreLinkActionKind::ShowSas { .. }) => {
                state.messages_received = state.messages_received.saturating_add(1);
                let Some(plaintext) = state.open(&bytes) else {
                    return state.finish(CoreLinkOutcome::HandshakeFailed, now_ms);
                };
                match plaintext.first().copied() {
                    Some(CTRL_CONFIRM)
                        if plaintext.len() == 1 + CHANNEL_BINDING_LEN
                            && Some(&plaintext[1..]) == state.channel_binding.as_deref() =>
                    {
                        state.outstanding = None;
                        state.finish(CoreLinkOutcome::ChannelReady, now_ms)
                    }
                    Some(CTRL_DECLINE) if plaintext.len() == 1 => {
                        state.finish(CoreLinkOutcome::Declined, now_ms)
                    }
                    _ => state.finish(CoreLinkOutcome::ProtocolError, now_ms),
                }
            }
            // Nothing was outstanding: bytes from nowhere change nothing.
            _ => state.stale(),
        }
    }

    /// No progress; re-state the outstanding action, or end on the deadline.
    pub fn tick(&self, now_ms: i64) -> CoreLinkAction {
        let mut state = self.lock();
        if let Some(action) = state.guard(now_ms) {
            return action;
        }
        state.restate()
    }

    pub fn cancel(&self, now_ms: i64) -> CoreLinkSummary {
        self.lock().cancel(now_ms)
    }

    pub fn summary(&self) -> Option<CoreLinkSummary> {
        self.lock().finished.clone()
    }

    /// §9.3's seam: once the channel is confirmed, the bootstrap rides it.
    /// Refused before then, and after any ending that is not a ready channel.
    pub fn seal_channel_frame(&self, plaintext: Vec<u8>) -> Result<Vec<u8>, CoreError> {
        seal_on(&mut self.lock(), &plaintext)
    }

    pub fn open_channel_frame(&self, frame: Vec<u8>) -> Result<Vec<u8>, CoreError> {
        open_on(&mut self.lock(), &frame)
    }
}

impl CoreLinkNewDevice {
    fn lock(&self) -> MutexGuard<'_, Ceremony> {
        self.state.lock().unwrap_or_else(|poisoned| {
            self.state.clear_poison();
            poisoned.into_inner()
        })
    }
}

// ---------------------------------------------------------------------------
// The approving device (§9.2)
// ---------------------------------------------------------------------------

/// The half that scans the QR, holds the confirm, and will sign the roster.
#[derive(uniffi::Object)]
pub struct CoreLinkApprovingDevice {
    state: Mutex<Ceremony>,
}

#[uniffi::export]
impl CoreLinkApprovingDevice {
    /// Scan an offer (§9.2).
    ///
    /// `active_device_count` is how many devices this person's roster holds
    /// right now. §14.3's boundary is applied to the count *after* the add
    /// through the one function that owns it, so a refusal here and a refusal
    /// in [`core_roster_validate`](crate::core_roster_validate) can never
    /// disagree: past the hard cap the ceremony ends at `start` without a byte
    /// moving; past the soft cap it runs and the confirm screen carries the
    /// warning.
    ///
    /// An unparseable or newer-than-this-build code is an error rather than an
    /// outcome — there is no ceremony to summarise, and `UnsupportedLink` is
    /// the shells' existing "update the app" copy.
    #[uniffi::constructor]
    pub fn scan(
        qr_text: String,
        active_device_count: u32,
        budgets: Option<LinkBudgets>,
    ) -> Result<Self, CoreError> {
        let budgets = budgets.unwrap_or_default();
        let rendezvous = core_parse_link_qr(qr_text)?;
        let (_, ephemeral_sk) = generate_link_keypair()?;
        let handshake = build_handshake(true, &rendezvous, &ephemeral_sk)?;
        let mut ceremony = Ceremony::new(
            CoreLinkRole::ApprovingDevice,
            budgets,
            rendezvous,
            String::new(),
            handshake,
        );
        match core_device_add_outcome(active_device_count.saturating_add(1)) {
            DeviceAddOutcome::Added => {}
            DeviceAddOutcome::AddedWithWarning => ceremony.soft_cap_warning = true,
            DeviceAddOutcome::Refused => ceremony.cap_refused = true,
        }
        Ok(CoreLinkApprovingDevice {
            state: Mutex::new(ceremony),
        })
    }

    pub fn rendezvous(&self) -> LinkRendezvous {
        self.lock().rendezvous.clone()
    }

    /// The mailbox namespace to meet the new device on, for a relay rendezvous.
    pub fn rendezvous_id(&self) -> Result<Vec<u8>, CoreError> {
        core_link_rendezvous_id(self.lock().rendezvous.link_pk.clone())
    }

    /// §14.3's answer for this add, decided at scan time.
    pub fn add_outcome(&self) -> DeviceAddOutcome {
        let state = self.lock();
        if state.cap_refused {
            DeviceAddOutcome::Refused
        } else if state.soft_cap_warning {
            DeviceAddOutcome::AddedWithWarning
        } else {
            DeviceAddOutcome::Added
        }
    }

    pub fn phase(&self) -> CoreLinkPhase {
        let state = self.lock();
        phase_of(&state)
    }

    /// Open the channel: the first Noise message (§9.2).
    pub fn start(&self, now_ms: i64) -> CoreLinkAction {
        let mut state = self.lock();
        if state.finished.is_some() || state.started {
            return state.restate();
        }
        let now_ms = state.clock(now_ms);
        state.started = true;
        state.started_at_ms = now_ms;
        // The scanner opens the channel immediately, so its clock starts here.
        state.deadline_from_ms = Some(now_ms);
        // §14.3 first: a person at the hard cap is told before their other
        // phone is contacted at all.
        if state.cap_refused {
            return state.finish(CoreLinkOutcome::DeviceCapReached, now_ms);
        }
        if now_ms > state.rendezvous.expires_at_ms {
            return state.finish(CoreLinkOutcome::QrExpired, now_ms);
        }
        let Some(message) = state.write_handshake_message() else {
            return state.finish(CoreLinkOutcome::HandshakeFailed, now_ms);
        };
        state.emit(
            CoreLinkPhase::Handshaking,
            CoreLinkActionKind::SendBytes { bytes: message },
        )
    }

    pub fn resume_sent(&self, now_ms: i64) -> CoreLinkAction {
        self.lock().resume_sent(now_ms)
    }

    pub fn resume_peer_bytes(&self, now_ms: i64, bytes: Vec<u8>) -> CoreLinkAction {
        let mut state = self.lock();
        if let Some(action) = state.guard(now_ms) {
            return action;
        }
        let now_ms = state.now_ms;
        match state.expecting() {
            Some(CoreLinkActionKind::AwaitPeer { .. }) => state.advance_handshake(&bytes, now_ms),
            // The confirm screen answers to a person, not to a peer. Bytes that
            // arrive here are ignored rather than obeyed, so nothing a remote
            // end sends can dismiss the one decision §9.2 puts in human hands.
            _ => state.stale(),
        }
    }

    /// **§9.2's explicit action, on the existing device.** The person compared
    /// the digits on both screens and said they match.
    pub fn confirm(&self, now_ms: i64) -> CoreLinkAction {
        self.decide(now_ms, CTRL_CONFIRM, CoreLinkOutcome::ChannelReady)
    }

    /// The digits did not match. The channel is told and then dropped; the
    /// person starts over with a fresh QR.
    pub fn decline(&self, now_ms: i64) -> CoreLinkAction {
        self.decide(now_ms, CTRL_DECLINE, CoreLinkOutcome::Declined)
    }

    pub fn tick(&self, now_ms: i64) -> CoreLinkAction {
        let mut state = self.lock();
        if let Some(action) = state.guard(now_ms) {
            return action;
        }
        state.restate()
    }

    pub fn cancel(&self, now_ms: i64) -> CoreLinkSummary {
        self.lock().cancel(now_ms)
    }

    pub fn summary(&self) -> Option<CoreLinkSummary> {
        self.lock().finished.clone()
    }

    /// §9.3's seam: the bootstrap the approving device streams rides this.
    pub fn seal_channel_frame(&self, plaintext: Vec<u8>) -> Result<Vec<u8>, CoreError> {
        seal_on(&mut self.lock(), &plaintext)
    }

    pub fn open_channel_frame(&self, frame: Vec<u8>) -> Result<Vec<u8>, CoreError> {
        open_on(&mut self.lock(), &frame)
    }
}

impl CoreLinkApprovingDevice {
    fn lock(&self) -> MutexGuard<'_, Ceremony> {
        self.state.lock().unwrap_or_else(|poisoned| {
            self.state.clear_poison();
            poisoned.into_inner()
        })
    }

    fn decide(&self, now_ms: i64, tag: u8, outcome: CoreLinkOutcome) -> CoreLinkAction {
        let mut state = self.lock();
        if let Some(action) = state.guard(now_ms) {
            return action;
        }
        let now_ms = state.now_ms;
        if !matches!(
            state.outstanding,
            Some(CoreLinkAction {
                kind: CoreLinkActionKind::ShowSas { .. },
                ..
            })
        ) {
            // Nothing has been shown to compare, so there is nothing to agree
            // with. A confirm from an unexpected screen changes nothing.
            return state.stale();
        }
        let Some(frame) = state.control_frame(tag) else {
            return state.finish(CoreLinkOutcome::HandshakeFailed, now_ms);
        };
        state.pending_terminal = Some(outcome);
        state.emit(
            CoreLinkPhase::ChannelReady,
            CoreLinkActionKind::SendBytes { bytes: frame },
        )
    }
}

// ---------------------------------------------------------------------------
// Shared accessors
// ---------------------------------------------------------------------------

fn phase_of(state: &Ceremony) -> CoreLinkPhase {
    if state.finished.is_some() {
        return CoreLinkPhase::Finished;
    }
    match state.outstanding.as_ref().map(|action| action.phase) {
        Some(phase) => phase,
        None if state.started => CoreLinkPhase::Handshaking,
        None => CoreLinkPhase::NotStarted,
    }
}

fn seal_on(state: &mut Ceremony, plaintext: &[u8]) -> Result<Vec<u8>, CoreError> {
    if !state.channel_ready() {
        return Err(CoreError::Crypto(
            "device link channel is not confirmed".to_string(),
        ));
    }
    state
        .seal(plaintext)
        .ok_or_else(|| CoreError::Crypto("device link frame could not be sealed".to_string()))
}

fn open_on(state: &mut Ceremony, frame: &[u8]) -> Result<Vec<u8>, CoreError> {
    if !state.channel_ready() {
        return Err(CoreError::Crypto(
            "device link channel is not confirmed".to_string(),
        ));
    }
    state
        .open(frame)
        .ok_or_else(|| CoreError::Crypto("device link frame could not be opened".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_755_000_000_000;

    fn new_device() -> CoreLinkNewDevice {
        CoreLinkNewDevice::new(
            vec!["192.168.1.24:45892".to_string()],
            vec!["https://relay.example".to_string()],
            NOW,
            None,
        )
        .unwrap()
    }

    fn approving_for(
        offer: &CoreLinkNewDevice,
        active_device_count: u32,
    ) -> CoreLinkApprovingDevice {
        CoreLinkApprovingDevice::scan(offer.qr_text(), active_device_count, None).unwrap()
    }

    fn bytes_of(action: &CoreLinkAction) -> Vec<u8> {
        match &action.kind {
            CoreLinkActionKind::SendBytes { bytes } => bytes.clone(),
            other => panic!("expected bytes to send, got {other:?}"),
        }
    }

    fn summary_of(action: &CoreLinkAction) -> CoreLinkSummary {
        match &action.kind {
            CoreLinkActionKind::Finished { summary } => summary.clone(),
            other => panic!("expected a finished action, got {other:?}"),
        }
    }

    /// Drive both halves against each other up to the confirm screen, returning
    /// the digits each end derived. No sockets: this IS the loopback the §13
    /// gate asks be sim-proven before two phones ever meet.
    fn run_to_confirm(
        newcomer: &CoreLinkNewDevice,
        approver: &CoreLinkApprovingDevice,
    ) -> (String, String) {
        let shown = newcomer.start(NOW);
        assert!(matches!(shown.kind, CoreLinkActionKind::ShowQr { .. }));

        let msg1 = approver.start(NOW);
        let sent = bytes_of(&msg1);
        assert!(matches!(
            approver.resume_sent(NOW).kind,
            CoreLinkActionKind::AwaitPeer { .. }
        ));

        let msg2 = newcomer.resume_peer_bytes(NOW, sent);
        let sent = bytes_of(&msg2);
        assert!(matches!(
            newcomer.resume_sent(NOW).kind,
            CoreLinkActionKind::AwaitPeer { .. }
        ));

        let msg3 = approver.resume_peer_bytes(NOW, sent);
        let sent = bytes_of(&msg3);

        let newcomer_sas = match newcomer.resume_peer_bytes(NOW, sent).kind {
            CoreLinkActionKind::ShowSas {
                sas, confirm_here, ..
            } => {
                assert!(!confirm_here, "the new device never holds the confirm");
                sas
            }
            other => panic!("expected the new device to show digits, got {other:?}"),
        };
        let approver_sas = match approver.resume_sent(NOW).kind {
            CoreLinkActionKind::ShowSas {
                sas, confirm_here, ..
            } => {
                assert!(confirm_here, "the existing device holds the confirm (§9.2)");
                sas
            }
            other => panic!("expected the approving device to show digits, got {other:?}"),
        };
        (newcomer_sas, approver_sas)
    }

    /// The whole §9.1–§9.2 happy path over a loopback channel.
    #[test]
    fn two_halves_reach_a_confirmed_channel() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);

        let (newcomer_sas, approver_sas) = run_to_confirm(&newcomer, &approver);
        assert_eq!(newcomer_sas, approver_sas);
        assert_eq!(newcomer_sas.len(), 7, "six digits and a space");

        let confirm = approver.confirm(NOW);
        let frame = bytes_of(&confirm);
        let approver_done = summary_of(&approver.resume_sent(NOW));
        let newcomer_done = summary_of(&newcomer.resume_peer_bytes(NOW, frame));

        assert_eq!(approver_done.outcome, CoreLinkOutcome::ChannelReady);
        assert_eq!(newcomer_done.outcome, CoreLinkOutcome::ChannelReady);
        assert_eq!(
            approver_done.channel_binding, newcomer_done.channel_binding,
            "both ends bind to the same transcript"
        );
        assert_eq!(approver_done.sas, Some(approver_sas));
        assert!(!approver_done.soft_cap_warning);
        assert_eq!(newcomer.phase(), CoreLinkPhase::Finished);

        // The approving device provably met the key it scanned; the new device
        // met a key it had never seen, which is what the digits were for.
        assert_eq!(approver_done.peer_static_pk, Some(newcomer.link_pk()));
        assert!(newcomer_done.peer_static_pk.is_some());
        assert_ne!(newcomer_done.peer_static_pk, Some(newcomer.link_pk()));

        // §9.3's seam: the confirmed channel carries the bootstrap next.
        let sealed = approver
            .seal_channel_frame(b"bootstrap chunk".to_vec())
            .unwrap();
        assert_eq!(
            newcomer.open_channel_frame(sealed).unwrap(),
            b"bootstrap chunk".to_vec()
        );
        let sealed_back = newcomer.seal_channel_frame(b"roster ack".to_vec()).unwrap();
        assert_eq!(
            approver.open_channel_frame(sealed_back).unwrap(),
            b"roster ack".to_vec()
        );
    }

    /// Two ceremonies are two channels: the digits are not a device property a
    /// watcher could learn once and replay.
    #[test]
    fn every_ceremony_derives_its_own_digits() {
        let first = new_device();
        let first_sas = run_to_confirm(&first, &approving_for(&first, 1)).0;
        let second = new_device();
        let second_sas = run_to_confirm(&second, &approving_for(&second, 1)).0;
        assert_ne!(first_sas, second_sas);
    }

    /// §9.2's rule, structurally: the new device has no confirm at all, and it
    /// sits on the digits until the existing device speaks.
    #[test]
    fn only_the_existing_device_can_confirm() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        run_to_confirm(&newcomer, &approver);

        // Nothing the new device can do advances it: tick and stray bytes both
        // leave the digits on screen.
        assert!(matches!(
            newcomer.tick(NOW + 5_000).kind,
            CoreLinkActionKind::ShowSas { .. }
        ));
        assert_eq!(newcomer.phase(), CoreLinkPhase::AwaitingConfirm);
        assert!(newcomer.summary().is_none());
        // And it cannot seal anything on a channel nobody has confirmed.
        assert!(newcomer.seal_channel_frame(b"bootstrap".to_vec()).is_err());
        assert!(approver.seal_channel_frame(b"bootstrap".to_vec()).is_err());
    }

    #[test]
    fn declining_on_the_existing_device_ends_both_halves() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        run_to_confirm(&newcomer, &approver);

        let frame = bytes_of(&approver.decline(NOW));
        assert_eq!(
            summary_of(&approver.resume_sent(NOW)).outcome,
            CoreLinkOutcome::Declined
        );
        assert_eq!(
            summary_of(&newcomer.resume_peer_bytes(NOW, frame)).outcome,
            CoreLinkOutcome::Declined
        );
        assert!(newcomer.open_channel_frame(vec![0u8; 32]).is_err());
    }

    /// §14.3's hard cap, applied before the ceremony spends anything.
    #[test]
    fn the_hard_cap_refuses_before_a_byte_moves() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 16);
        assert_eq!(approver.add_outcome(), DeviceAddOutcome::Refused);

        let summary = summary_of(&approver.start(NOW));
        assert_eq!(summary.outcome, CoreLinkOutcome::DeviceCapReached);
        assert_eq!(summary.messages_sent, 0);
        assert_eq!(summary.messages_received, 0);
    }

    /// The boundary either side of the soft cap: the 9th device warns, the 8th
    /// does not, and the warning reaches the screen that asks for the tap.
    #[test]
    fn the_soft_cap_warns_on_the_confirm_screen() {
        let quiet = new_device();
        assert_eq!(
            approving_for(&quiet, 7).add_outcome(),
            DeviceAddOutcome::Added
        );

        let newcomer = new_device();
        let approver = approving_for(&newcomer, 8);
        assert_eq!(approver.add_outcome(), DeviceAddOutcome::AddedWithWarning);
        run_to_confirm(&newcomer, &approver);
        match approver.tick(NOW).kind {
            CoreLinkActionKind::ShowSas {
                warn_soft_cap,
                confirm_here,
                ..
            } => {
                assert!(warn_soft_cap);
                assert!(confirm_here);
            }
            other => panic!("expected the digits with a warning, got {other:?}"),
        }
        // The new device is told nothing about how many siblings it is joining.
        match newcomer.tick(NOW).kind {
            CoreLinkActionKind::ShowSas { warn_soft_cap, .. } => assert!(!warn_soft_cap),
            other => panic!("expected the digits, got {other:?}"),
        }
    }

    #[test]
    fn an_expired_offer_never_opens_a_channel() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        let expired_at = newcomer.rendezvous().expires_at_ms + 1;

        assert_eq!(
            summary_of(&approver.start(expired_at)).outcome,
            CoreLinkOutcome::QrExpired
        );
        assert_eq!(
            summary_of(&newcomer.start(expired_at)).outcome,
            CoreLinkOutcome::QrExpired
        );
    }

    /// An offer that expires while it is on screen stops being an offer.
    #[test]
    fn an_offer_expires_under_the_person_who_left_it_up() {
        let newcomer = new_device();
        newcomer.start(NOW);
        let expired_at = newcomer.rendezvous().expires_at_ms + 1;
        assert_eq!(
            summary_of(&newcomer.tick(expired_at)).outcome,
            CoreLinkOutcome::QrExpired
        );
    }

    #[test]
    fn the_deadline_ends_a_ceremony_that_stalled_mid_handshake() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        let budgets = LinkBudgets::default();

        approver.start(NOW);
        approver.resume_sent(NOW);
        let summary = summary_of(&approver.tick(NOW + budgets.deadline_ms + 1));
        assert_eq!(summary.outcome, CoreLinkOutcome::TimedOut);
        // A late peer message after the deadline changes nothing.
        assert_eq!(
            summary_of(&approver.resume_peer_bytes(NOW + budgets.deadline_ms + 2, vec![1, 2, 3]))
                .outcome,
            CoreLinkOutcome::TimedOut
        );

        // The new device's clock starts when someone knocks, not when the QR
        // goes up: an unattended offer ends as an expired offer instead.
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        newcomer.start(NOW);
        let msg1 = bytes_of(&approver.start(NOW));
        newcomer.resume_peer_bytes(NOW + 1_000, msg1);
        assert_eq!(
            summary_of(&newcomer.tick(NOW + 1_000 + budgets.deadline_ms + 1)).outcome,
            CoreLinkOutcome::TimedOut
        );
    }

    /// A wall clock that goes backwards must not buy a ceremony more time. The
    /// deadline is measured against the highest time this ceremony has ever
    /// been told about, so a rewind — an NTP correction, a hand-set date, a
    /// peer stalling until one happens — cannot hold a confirm screen open.
    #[test]
    fn a_rewound_clock_cannot_stall_the_deadline() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        let budgets = LinkBudgets::default();

        approver.start(NOW);
        approver.resume_sent(NOW);
        // Nearly out of time, and then the clock jumps back a day.
        assert!(matches!(
            approver.tick(NOW + budgets.deadline_ms).kind,
            CoreLinkActionKind::AwaitPeer { .. }
        ));
        assert!(matches!(
            approver.tick(NOW - 86_400_000).kind,
            CoreLinkActionKind::AwaitPeer { .. }
        ));
        // One more millisecond of real time ends it, exactly as if the rewind
        // had never been reported.
        assert_eq!(
            summary_of(&approver.tick(NOW + budgets.deadline_ms + 1)).outcome,
            CoreLinkOutcome::TimedOut
        );
        // And the summary is stamped with the clamped clock, never the rewind.
        assert!(approver.summary().unwrap().finished_at_ms >= NOW + budgets.deadline_ms);
    }

    #[test]
    fn cancel_is_terminal_and_later_resumes_are_ignored() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        let msg1 = bytes_of(&approver.start(NOW));
        newcomer.start(NOW);

        let summary = approver.cancel(NOW + 10);
        assert_eq!(summary.outcome, CoreLinkOutcome::Cancelled);
        assert_eq!(
            approver.cancel(NOW + 20).outcome,
            CoreLinkOutcome::Cancelled
        );
        assert_eq!(
            summary_of(&approver.resume_peer_bytes(NOW + 30, msg1)).outcome,
            CoreLinkOutcome::Cancelled
        );
        assert_eq!(
            summary_of(&approver.start(NOW + 40)).outcome,
            CoreLinkOutcome::Cancelled
        );
        assert!(approver.summary().unwrap().stale_resumes_ignored <= 1);
    }

    /// A tampered handshake message fails closed at the first message that
    /// carries a tag to check — Noise XX's second — and there is no retry: a
    /// security handshake that went wrong ends, and the person starts fresh.
    #[test]
    fn a_tampered_handshake_message_fails_closed() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        newcomer.start(NOW);
        let msg1 = bytes_of(&approver.start(NOW));
        approver.resume_sent(NOW);
        let mut msg2 = bytes_of(&newcomer.resume_peer_bytes(NOW, msg1));
        newcomer.resume_sent(NOW);
        msg2[40] ^= 0xff;

        let summary = summary_of(&approver.resume_peer_bytes(NOW, msg2));
        assert_eq!(summary.outcome, CoreLinkOutcome::HandshakeFailed);
        assert!(summary.channel_binding.is_none());
        assert!(summary.sas.is_none());
        // And the same on the other side, for the message the new device reads.
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        newcomer.start(NOW);
        let msg1 = bytes_of(&approver.start(NOW));
        let msg2 = bytes_of(&newcomer.resume_peer_bytes(NOW, msg1));
        let mut msg3 = bytes_of(&approver.resume_peer_bytes(NOW, msg2));
        msg3[40] ^= 0xff;
        assert_eq!(
            summary_of(&newcomer.resume_peer_bytes(NOW, msg3)).outcome,
            CoreLinkOutcome::HandshakeFailed
        );
    }

    #[test]
    fn oversized_and_empty_peer_messages_fail_closed() {
        for message in [Vec::new(), vec![7u8; LINK_MAX_CEREMONY_MESSAGE_BYTES + 1]] {
            let newcomer = new_device();
            newcomer.start(NOW);
            assert_eq!(
                summary_of(&newcomer.resume_peer_bytes(NOW, message)).outcome,
                CoreLinkOutcome::HandshakeFailed
            );
        }
    }

    /// The QR binding, tested directly: a responder that holds a different
    /// static key than the one printed in the scanned QR is refused, even
    /// though it speaks the same prologue and the same pattern.
    #[test]
    fn an_approving_device_refuses_a_peer_that_is_not_the_scanned_key() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        let rendezvous = newcomer.rendezvous();

        // A stranger at the rendezvous: same prologue (it read the QR too), a
        // key of its own.
        let (_, stranger_sk) = generate_link_keypair().unwrap();
        let mut stranger = build_handshake(false, &rendezvous, &stranger_sk).unwrap();

        let msg1 = bytes_of(&approver.start(NOW));
        approver.resume_sent(NOW);
        let mut scratch = vec![0u8; NOISE_MAX_MESSAGE_SIZE];
        stranger.read_message(&msg1, &mut scratch).unwrap();
        let mut msg2 = vec![0u8; NOISE_MAX_MESSAGE_SIZE];
        let len = stranger.write_message(&[], &mut msg2).unwrap();
        msg2.truncate(len);

        let summary = summary_of(&approver.resume_peer_bytes(NOW, msg2));
        assert_eq!(summary.outcome, CoreLinkOutcome::HandshakeFailed);
        assert_eq!(
            summary.messages_sent, 1,
            "the stranger was answered with nothing after message 1"
        );
    }

    /// A ceremony bound to one offer cannot be completed with a device holding
    /// another. Noise XX's first message is unauthenticated by construction, so
    /// the wrong device does answer it — and then the prologue and the static
    /// check both refuse what comes back, which is where it matters.
    #[test]
    fn a_channel_is_bound_to_the_offer_that_was_scanned() {
        let scanned = new_device();
        let other = new_device();
        let approver = approving_for(&scanned, 1);
        other.start(NOW);

        let msg1 = bytes_of(&approver.start(NOW));
        let msg2 = bytes_of(&other.resume_peer_bytes(NOW, msg1));
        assert_eq!(
            summary_of(&approver.resume_peer_bytes(NOW, msg2)).outcome,
            CoreLinkOutcome::HandshakeFailed
        );
        // The device that was never scanned learns nothing either: its own
        // ceremony is left waiting and ends on its deadline.
        assert!(other.summary().is_none());
    }

    /// Resumes that do not match the outstanding action are counted and
    /// ignored, never applied: a driver replaying an old result cannot advance
    /// a ceremony, and a peer cannot answer a question that was asked of a
    /// person.
    #[test]
    fn stale_resumes_are_ignored_rather_than_applied() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        newcomer.start(NOW);

        // Nothing is outstanding to send yet.
        assert!(matches!(
            approver.resume_sent(NOW).kind,
            CoreLinkActionKind::AwaitPeer { .. }
        ));
        // A confirm before there are digits to compare changes nothing.
        assert!(matches!(
            approver.confirm(NOW).kind,
            CoreLinkActionKind::AwaitPeer { .. }
        ));
        assert!(approver.summary().is_none());

        run_to_confirm(&newcomer, &approver);

        // The confirm screen answers to a person: bytes arriving there are
        // counted and ignored, and the digits stay up.
        let restated = approver.resume_peer_bytes(NOW, vec![9, 9, 9]);
        match restated.kind {
            CoreLinkActionKind::ShowSas { confirm_here, .. } => assert!(confirm_here),
            other => panic!("expected the digits to stay up, got {other:?}"),
        }
        let before = restated.action_id;
        assert_eq!(
            approver.tick(NOW).action_id,
            before,
            "a restated action keeps its id"
        );

        // And the ceremony still completes from exactly where it was.
        let frame = bytes_of(&approver.confirm(NOW));
        assert_eq!(
            summary_of(&approver.resume_sent(NOW)).outcome,
            CoreLinkOutcome::ChannelReady
        );
        assert_eq!(
            summary_of(&newcomer.resume_peer_bytes(NOW, frame)).outcome,
            CoreLinkOutcome::ChannelReady
        );
        assert!(approver.summary().unwrap().stale_resumes_ignored >= 3);
    }

    /// A reply is proof the send left: a shell that reports delivery late must
    /// not deadlock a ceremony whose next message is already in hand.
    #[test]
    fn a_reply_stands_in_for_a_late_delivery_report() {
        let newcomer = new_device();
        let approver = approving_for(&newcomer, 1);
        newcomer.start(NOW);

        let msg1 = bytes_of(&approver.start(NOW));
        // No resume_sent on either side, ever.
        let msg2 = bytes_of(&newcomer.resume_peer_bytes(NOW, msg1));
        let msg3 = bytes_of(&approver.resume_peer_bytes(NOW, msg2));
        assert!(matches!(
            newcomer.resume_peer_bytes(NOW, msg3).kind,
            CoreLinkActionKind::ShowSas { .. }
        ));
    }

    /// The digits, frozen. Two builds that disagree here would show a person
    /// two different numbers for one channel.
    #[test]
    fn golden_link_sas() {
        assert_eq!(core_link_sas(vec![0u8; 32]).unwrap(), "216 281");
        assert_eq!(core_link_sas(vec![0xab; 32]).unwrap(), "640 064");
        assert!(core_link_sas(vec![0u8; 31]).is_err());
        assert!(core_link_sas(Vec::new()).is_err());
    }
}
