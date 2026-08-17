//! The §9 linking ceremony, driven end to end through the exported surface.
//!
//! `specs/multi-device-v1.md` §13's WP3 gate is "link two dev builds end to end
//! on LAN and on relay-only". The two phones are David's half. This file is the
//! half that must be true before the phones are worth picking up: the same two
//! state machines, driven by a shell-shaped loop over the same public API the
//! Kotlin and Swift bindings see, across the two transports whose *shape*
//! differs — a live link where bytes arrive the moment they are sent, and a
//! store-and-forward mailbox where each side polls, waits, and sees nothing for
//! a while.
//!
//! Neither driver here knows anything about Noise, the offer, the digits, or
//! the confirm. It moves bytes between two mailboxes and answers the actions it
//! is handed, which is exactly the contract the shells will implement.

use std::collections::VecDeque;

use cruisemesh_core::{
    core_link_rendezvous_id, CoreLinkAction, CoreLinkActionKind, CoreLinkApprovingDevice,
    CoreLinkNewDevice, CoreLinkOutcome, CoreLinkSummary, LinkBudgets,
};

const NOW: i64 = 1_755_000_000_000;

/// How the two devices are connected. The ceremony cannot tell the difference,
/// which is the property under test.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Wire {
    /// A LAN socket or a BLE link: bytes are there when the peer looks.
    Live,
    /// A relay rendezvous: bytes sit in a mailbox, each side polls on the
    /// interval core suggested, and the clock moves while nothing happens.
    Mailbox,
}

struct Sim {
    newcomer: CoreLinkNewDevice,
    approver: CoreLinkApprovingDevice,
    to_newcomer: VecDeque<Vec<u8>>,
    to_approver: VecDeque<Vec<u8>>,
    now_ms: i64,
    wire: Wire,
    newcomer_sas: Option<String>,
    approver_sas: Option<String>,
}

impl Sim {
    fn new(wire: Wire, active_device_count: u32) -> Sim {
        let newcomer = CoreLinkNewDevice::new(
            vec!["192.168.1.24:45892".to_string()],
            vec!["https://relay.example".to_string()],
            NOW,
            None,
        )
        .expect("an offer is buildable");
        let approver = CoreLinkApprovingDevice::scan(newcomer.qr_text(), active_device_count, None)
            .expect("the offer scans");
        Sim {
            newcomer,
            approver,
            to_newcomer: VecDeque::new(),
            to_approver: VecDeque::new(),
            now_ms: NOW,
            wire,
            newcomer_sas: None,
            approver_sas: None,
        }
    }

    /// One step of the new device's shell.
    fn step_newcomer(&mut self, action: CoreLinkAction) -> CoreLinkAction {
        match action.kind {
            CoreLinkActionKind::SendBytes { bytes } => {
                self.to_approver.push_back(bytes);
                self.newcomer.resume_sent(self.now_ms)
            }
            // The new device shows the digits and then waits: §9.2 puts the tap
            // on the other phone, so this side has nothing to answer with.
            CoreLinkActionKind::ShowSas {
                sas, confirm_here, ..
            } => {
                assert!(!confirm_here, "the new device must never hold the confirm");
                self.newcomer_sas = Some(sas);
                self.receive_newcomer()
            }
            CoreLinkActionKind::ShowQr { .. } | CoreLinkActionKind::AwaitPeer { .. } => {
                self.receive_newcomer()
            }
            CoreLinkActionKind::Finished { summary } => CoreLinkAction {
                kind: CoreLinkActionKind::Finished { summary },
                ..action
            },
        }
    }

    fn receive_newcomer(&mut self) -> CoreLinkAction {
        match self.to_newcomer.pop_front() {
            Some(bytes) => self.newcomer.resume_peer_bytes(self.now_ms, bytes),
            None => {
                self.idle();
                self.newcomer.tick(self.now_ms)
            }
        }
    }

    /// One step of the approving device's shell.
    fn step_approver(&mut self, action: CoreLinkAction) -> CoreLinkAction {
        match action.kind {
            CoreLinkActionKind::SendBytes { bytes } => {
                self.to_newcomer.push_back(bytes);
                self.approver.resume_sent(self.now_ms)
            }
            // The one place a human is required. The digits are compared here.
            CoreLinkActionKind::ShowSas {
                sas, confirm_here, ..
            } => {
                assert!(confirm_here, "the existing device holds the confirm (§9.2)");
                self.approver_sas = Some(sas);
                self.approver.confirm(self.now_ms)
            }
            CoreLinkActionKind::AwaitPeer { .. } | CoreLinkActionKind::ShowQr { .. } => {
                match self.to_approver.pop_front() {
                    Some(bytes) => self.approver.resume_peer_bytes(self.now_ms, bytes),
                    None => {
                        self.idle();
                        self.approver.tick(self.now_ms)
                    }
                }
            }
            CoreLinkActionKind::Finished { summary } => CoreLinkAction {
                kind: CoreLinkActionKind::Finished { summary },
                ..action
            },
        }
    }

    /// Nothing arrived. On a live link that is a moment; on a mailbox it is a
    /// poll interval, and the clock really moves.
    fn idle(&mut self) {
        if self.wire == Wire::Mailbox {
            self.now_ms += LinkBudgets::default().poll_interval_ms;
        }
    }

    /// Run both halves until they finish, or give up long before the deadline
    /// could rescue a livelock.
    fn run(&mut self) -> (CoreLinkSummary, CoreLinkSummary) {
        let mut newcomer_action = self.newcomer.start(self.now_ms);
        let mut approver_action = self.approver.start(self.now_ms);
        for _ in 0..64 {
            if let (
                CoreLinkActionKind::Finished { summary: newcomer },
                CoreLinkActionKind::Finished { summary: approver },
            ) = (&newcomer_action.kind, &approver_action.kind)
            {
                return (newcomer.clone(), approver.clone());
            }
            newcomer_action = self.step_newcomer(newcomer_action);
            approver_action = self.step_approver(approver_action);
        }
        panic!("the ceremony never settled");
    }
}

/// The §13 gate's LAN half, proven in a loop before any phone is involved.
#[test]
fn two_devices_link_over_a_live_link() {
    let mut sim = Sim::new(Wire::Live, 1);
    let (newcomer, approver) = sim.run();

    assert_eq!(newcomer.outcome, CoreLinkOutcome::ChannelReady);
    assert_eq!(approver.outcome, CoreLinkOutcome::ChannelReady);
    assert_eq!(newcomer.channel_binding, approver.channel_binding);
    assert_eq!(sim.newcomer_sas, sim.approver_sas);
    assert!(sim.newcomer_sas.is_some());
    assert_eq!(newcomer.sas, sim.newcomer_sas);
    // The scanner met the key it scanned; the newcomer met a key it could not
    // have checked, which is what the digits were for.
    assert_eq!(approver.peer_static_pk, Some(sim.newcomer.link_pk()));

    // §9.3's seam: the bootstrap rides this channel next.
    let sealed = sim
        .approver
        .seal_channel_frame(b"canonical bootstrap".to_vec())
        .expect("a confirmed channel seals");
    assert_eq!(
        sim.newcomer.open_channel_frame(sealed).unwrap(),
        b"canonical bootstrap".to_vec()
    );
}

/// The §13 gate's relay-only half: the same ceremony where nothing is ever in
/// hand when it is wanted, every wait costs real time, and both sides meet on a
/// mailbox namespace each derives for itself.
#[test]
fn two_devices_link_over_a_store_and_forward_rendezvous() {
    let mut sim = Sim::new(Wire::Mailbox, 1);
    let offered = sim.newcomer.rendezvous_id().unwrap();
    let scanned = sim.approver.rendezvous_id().unwrap();
    assert_eq!(
        offered, scanned,
        "both devices derive the same rendezvous mailbox from the offer"
    );
    assert_eq!(
        offered,
        core_link_rendezvous_id(sim.newcomer.link_pk()).unwrap()
    );

    let started_at = sim.now_ms;
    let (newcomer, approver) = sim.run();

    assert_eq!(newcomer.outcome, CoreLinkOutcome::ChannelReady);
    assert_eq!(approver.outcome, CoreLinkOutcome::ChannelReady);
    assert_eq!(newcomer.channel_binding, approver.channel_binding);
    assert_eq!(sim.newcomer_sas, sim.approver_sas);
    assert!(
        sim.now_ms > started_at,
        "a mailbox ceremony spends real waiting time"
    );
    assert!(sim.now_ms - started_at < LinkBudgets::default().deadline_ms);
}

/// A rendezvous nobody ever answers. Both halves end, and each says the true
/// thing about its own side: the scanner ran out of ceremony, the offer ran out
/// of offer.
#[test]
fn a_rendezvous_nobody_answers_ends_on_both_sides() {
    let sim = Sim::new(Wire::Mailbox, 1);
    let budgets = LinkBudgets::default();

    sim.newcomer.start(NOW);
    sim.approver.start(NOW);
    sim.approver.resume_sent(NOW);

    let approver_end = match sim.approver.tick(NOW + budgets.deadline_ms + 1).kind {
        CoreLinkActionKind::Finished { summary } => summary,
        other => panic!("expected the scanner to give up, got {other:?}"),
    };
    assert_eq!(approver_end.outcome, CoreLinkOutcome::TimedOut);

    let newcomer_end = match sim.newcomer.tick(NOW + budgets.qr_lifetime_ms + 1).kind {
        CoreLinkActionKind::Finished { summary } => summary,
        other => panic!("expected the offer to expire, got {other:?}"),
    };
    assert_eq!(newcomer_end.outcome, CoreLinkOutcome::QrExpired);
    assert!(newcomer_end.channel_binding.is_none());
}

/// §14.3's hard cap through the same public surface: the scanner never opens a
/// channel, and the device showing the QR is left waiting rather than told
/// anything about the other person's device count.
#[test]
fn a_person_at_the_hard_cap_links_nothing() {
    let sim = Sim::new(Wire::Live, 16);
    sim.newcomer.start(NOW);

    let summary = match sim.approver.start(NOW).kind {
        CoreLinkActionKind::Finished { summary } => summary,
        other => panic!("expected an immediate refusal, got {other:?}"),
    };
    assert_eq!(summary.outcome, CoreLinkOutcome::DeviceCapReached);
    assert_eq!(summary.messages_sent, 0);
    assert!(sim.newcomer.summary().is_none());
}
