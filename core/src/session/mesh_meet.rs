//! The encounter sequence, owned once, in core.
//!
//! Every time two identified mesh nodes meet — a BLE HELLO, an authenticated
//! LAN session, a failover resume — the *same* ordered work runs here:
//!
//! 1. digest-confirm any 1:1 carry the peer's advertised `msg_id` set proves
//!    they already hold ([`MessageStore::core_confirm_carried_deliveries`]);
//! 2. targeted drain of remaining carried envelopes whose `recipient_hint`
//!    matches the peer (their own recent-day hints, plus every imported group
//!    they belong to);
//! 3. budgeted spray-on-connect of the rest to a non-recipient mule, excluding
//!    every `msg_id` the peer's digest already named.
//!
//! This is the single authority `core/tests/mesh_sim.rs` calls in place of the
//! third copy it used to keep (`Network::meet` composing store primitives to
//! mirror `MeshService.drainCarriedEnvelopesTo` plus `sprayCarriedEnvelopesTo`).
//! It performs no I/O: the caller sends the returned frames. Store mutations
//! that are not I/O — expired-carry prune, digest-confirmed 1:1 removal, the
//! router walk cursors, the spray-policy cadence/burst charge — commit before
//! return, so a lost send afterward can never ack a relay row, delete a
//! carried row that was not digest-proven, or rewind a cursor.
//!
//! **Preserved invariants:**
//!
//! - `CARRY-01` / DTN D2 — a carried 1:1 envelope is removed only on
//!   digest-proof of receipt, never merely because it was offered. Dispatch
//!   of a targeted drain frame is not proof the peer stored it.
//! - `CARRY-02` — that proof is honoured for removal only when
//!   [`CoreMeetRequest::peer_authenticated`] is true. An unauthenticated
//!   digest (a bare BLE HELLO/DIGEST) still *excludes* the advertised ids
//!   from this encounter's offer; it never deletes the durable copy.
//! - `ACK-01` — nothing here acks a relay copy. Offering and even
//!   digest-confirming a local carry are not endpoint consumption.
//! - Group-addressed carries keep their mule-until-opened lifecycle: confirm
//!   is scoped to the peer's own 1:1 hints, so a group row survives one
//!   member advertising it.
//! - `SPRAY-01` — the foreign-carry spray is cadence-gated and byte-budgeted
//!   by [`crate::CoreSprayPolicy`]; the targeted drain is the HELLO path and
//!   is not cadence-gated, but it is still paged and charged against the
//!   link's burst allowance.
//!
//! The shells keep the radios, the HELLO/DIGEST codecs, and the send. They
//! stop keeping the arithmetic.

use std::collections::HashSet;

use crate::{
    encode_envelope_frame, CarriedEnvelope, CoreError, CoreMeshRouterState, CoreSprayPolicy,
    CoreSprayTrigger, MessageStore, CARRIED_SPRAY_BUDGET_BYTES,
};

/// Inputs for one encounter. Every clock is an explicit `now_ms`; every peer
/// claim is an argument, never ambient process state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreMeetRequest {
    /// This device's user id, used to scope the spray plan's own-outbound
    /// lane (empty in the sim, which has no contacts).
    pub own_user_id: Vec<u8>,
    /// The authenticated-or-claimed peer identity the HELLO named.
    pub peer_user_id: Vec<u8>,
    /// Transport address the router and spray policy key this link by.
    pub peer_address: String,
    /// `recent_msg_id`s off the peer's DIGEST (or the sim's stand-in:
    /// [`MessageStore::core_digest_advertised_msg_ids`]). Exclusion and
    /// confirm both read this set; neither invents a known-set of its own.
    pub peer_known_msg_ids: Vec<Vec<u8>>,
    /// `true` only when `peer_user_id` / `peer_known_msg_ids` came from a
    /// cryptographically bound source (Noise-authenticated LAN, a signed
    /// receipt). `false` for a bare BLE HELLO/DIGEST. See `CARRY-02`.
    pub peer_authenticated: bool,
    pub now_ms: i64,
}

/// Bounded work counts for one encounter, so a caller can fold progress into
/// a protocol event without this path writing the ring itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CoreMeetWork {
    /// Targeted-drain frames handed back to send.
    pub targeted_sent: u32,
    /// Spray frames (foreign carry, plus any own-outbound / own-receipt the
    /// plan admitted) handed back to send.
    pub sprayed: u32,
    /// Carried 1:1 rows removed because the peer's digest proved they hold
    /// them. Zero when the peer is unauthenticated (`CARRY-02`) or advertised
    /// nothing we were holding for them.
    pub confirmed_removed: u32,
    /// Drain candidates skipped because the peer's digest already named them.
    pub skipped_known: u32,
}

/// The result of planning one encounter through [`MessageStore::plan_mesh_meet`].
///
/// Every list here is the page this encounter is allowed to offer: the
/// targeted drain is one budgeted walk page, the spray is one
/// [`crate::CoreDigestSprayPlan`] after the spray-policy gate. Nothing
/// unbounded crosses the boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CoreMeetOutcome {
    /// Hint-matched carry frames for the true recipient (or a group member).
    /// Send these first: they are the HELLO drain, not the mule spray.
    pub targeted_frames: Vec<Vec<u8>>,
    /// Foreign-carry (and any admitted own-outbound / own-receipt) frames
    /// for a non-recipient mule. Empty when the spray gate refuses or the
    /// plan selected nothing new.
    pub spray_frames: Vec<Vec<u8>>,
    pub work: CoreMeetWork,
}

impl CoreMeetOutcome {
    /// Drain then spray, the production send order.
    pub fn frames(&self) -> impl Iterator<Item = &Vec<u8>> {
        self.targeted_frames.iter().chain(self.spray_frames.iter())
    }
}

impl MessageStore {
    /// Plan one mesh encounter and return the bounded frames to send.
    ///
    /// `router` and `spray` are the process-wide objects the shells already
    /// hold; this method records walk-cursor and cadence/burst progress on
    /// them and does not replace them. See the module docs for the ordered
    /// steps and the invariants preserved.
    pub fn plan_mesh_meet(
        &self,
        router: &CoreMeshRouterState,
        spray: &CoreSprayPolicy,
        request: CoreMeetRequest,
    ) -> Result<CoreMeetOutcome, CoreError> {
        let CoreMeetRequest {
            own_user_id,
            peer_user_id,
            peer_address,
            peer_known_msg_ids,
            peer_authenticated,
            now_ms,
        } = request;

        self.prune_expired_carried(now_ms)?;

        // 1. Digest-confirm first so a peer that already has a 1:1 we are
        // still carrying does not get it offered again this encounter, and
        // so the durable copy is retired only on proof — never on dispatch.
        let confirmed_removed = self.core_confirm_carried_deliveries(
            peer_user_id.clone(),
            peer_known_msg_ids.clone(),
            peer_authenticated,
            now_ms,
        )?;
        let peer_key = peer_key(&peer_user_id);
        if confirmed_removed > 0 {
            spray.note_receipt_progress(peer_key.clone(), now_ms);
        }

        let peer_hints = self.delivery_hints_for_peer(peer_user_id.clone(), now_ms)?;
        let known: HashSet<Vec<u8>> = peer_known_msg_ids.iter().cloned().collect();

        // 2. Targeted HELLO drain. Not cadence-gated (two phones meeting and
        // handing over mail addressed to one of them is the product) but
        // still paged on the disjoint targeted cursor and charged against
        // the link burst so a reconnect storm cannot drain unbounded.
        let mut targeted_frames = Vec::new();
        let mut skipped_known = 0_u32;
        let targeted_lane = router.targeted_carried_lane_for(peer_address.clone(), now_ms);
        if !targeted_lane.skip {
            let page = self.carried_envelopes_for_hints_page(
                peer_hints.clone(),
                now_ms,
                CARRIED_SPRAY_BUDGET_BYTES,
                targeted_lane.after,
            )?;
            let mut offered_sealed = 0_u64;
            for envelope in &page.rows {
                if known.contains(&envelope.msg_id) {
                    skipped_known = skipped_known.saturating_add(1);
                    continue;
                }
                offered_sealed = offered_sealed.saturating_add(envelope.sealed.len() as u64);
                targeted_frames.push(frame_carried(envelope));
            }
            router.record_targeted_carried_progress(
                peer_address.clone(),
                page.next,
                page.exhausted,
                now_ms,
            );
            spray.note_bytes_queued(peer_address.clone(), offered_sealed, now_ms);
        }

        // 3. Budgeted spray-on-connect to a non-recipient mule. Cadence,
        // identical-set suppression and the per-encounter byte budgets all
        // live in spray_policy; the plan itself never removes a row.
        let mut spray_frames = Vec::new();
        let gate = spray.may_spray(
            peer_key.clone(),
            peer_address.clone(),
            CoreSprayTrigger::FirstContact,
            now_ms,
        );
        if gate.allow {
            let lane = router.carried_lane_for(peer_address.clone(), now_ms);
            let plan = self.core_digest_spray_plan(
                own_user_id,
                peer_user_id,
                peer_hints,
                peer_known_msg_ids,
                now_ms,
                if lane.skip {
                    0
                } else {
                    gate.carried_budget_bytes
                },
                gate.own_outbound_budget_bytes,
                gate.own_receipt_budget_bytes,
                RECEIPT_QUERY_LIMIT,
                router.peer_acks_hidden_kinds(peer_address.clone()),
                router.hidden_offered_for(peer_address.clone()),
                lane.after,
            )?;
            let admission = spray.admit_plan(peer_key, peer_address.clone(), plan.lanes, now_ms);
            if admission.send_carried {
                spray_frames.extend(plan.carried_frames);
                if !lane.skip {
                    router.record_carried_progress(
                        peer_address.clone(),
                        plan.next_carried_cursor,
                        plan.carried_exhausted,
                        now_ms,
                    );
                }
            }
            if admission.send_own_outbound {
                spray_frames.extend(plan.own_outbound_frames);
                router.record_hidden_offered(peer_address, plan.offered_hidden_msg_ids);
            }
            if admission.send_own_receipts {
                spray_frames.extend(plan.own_receipt_frames);
            }
        }

        let work = CoreMeetWork {
            targeted_sent: u32::try_from(targeted_frames.len()).unwrap_or(u32::MAX),
            sprayed: u32::try_from(spray_frames.len()).unwrap_or(u32::MAX),
            confirmed_removed: u32::try_from(confirmed_removed).unwrap_or(u32::MAX),
            skipped_known,
        };
        Ok(CoreMeetOutcome {
            targeted_frames,
            spray_frames,
            work,
        })
    }
}

/// Desktop's digest-response receipt query bound, used here so the own-receipt
/// spray lane agrees with the production caller.
const RECEIPT_QUERY_LIMIT: u64 = 256;

fn frame_carried(envelope: &CarriedEnvelope) -> Vec<u8> {
    encode_envelope_frame(
        envelope.msg_id.clone(),
        envelope.hop_ttl,
        envelope.expiry,
        envelope.recipient_hint.clone(),
        envelope.sealed.clone(),
    )
}

fn peer_key(user_id: &[u8]) -> String {
    user_id.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spray_policy::SprayPolicyConfig;
    use crate::{
        compute_recipient_hint, generate_identity, generate_msg_id, CarriedEnvelope,
        CoreSprayPolicy, CoreTransport, MessageStore, DEFAULT_HOP_TTL, MS_PER_DAY,
    };

    const NOW: i64 = 1_700_000_000_000;
    const FOREIGN_BUDGET: i64 = 5 * 1024 * 1024;
    const ADDRESS: &str = "ble:meet-peer";

    fn store() -> MessageStore {
        MessageStore::open(":memory:".to_string()).expect("open in-memory store")
    }

    fn router_for(peer_user_id: &[u8]) -> CoreMeshRouterState {
        let router = CoreMeshRouterState::new();
        router.on_connected(ADDRESS.to_string(), CoreTransport::Central);
        assert!(router.on_hello(ADDRESS.to_string(), peer_user_id.to_vec()));
        router
    }

    fn request(
        own_user_id: Vec<u8>,
        peer_user_id: Vec<u8>,
        peer_known_msg_ids: Vec<Vec<u8>>,
        peer_authenticated: bool,
    ) -> CoreMeetRequest {
        CoreMeetRequest {
            own_user_id,
            peer_user_id,
            peer_address: ADDRESS.to_string(),
            peer_known_msg_ids,
            peer_authenticated,
            now_ms: NOW,
        }
    }

    fn enqueue(
        store: &MessageStore,
        msg_id: Vec<u8>,
        hint: Vec<u8>,
        sealed: Vec<u8>,
        is_family: bool,
        received_at: i64,
    ) {
        store
            .enqueue_carried_envelope(
                CarriedEnvelope {
                    msg_id,
                    hop_ttl: DEFAULT_HOP_TTL,
                    expiry: NOW + 7 * MS_PER_DAY,
                    recipient_hint: hint,
                    sealed,
                },
                is_family,
                received_at,
                FOREIGN_BUDGET,
            )
            .expect("enqueue carried");
    }

    fn frame_msg_id(frame: &[u8]) -> Vec<u8> {
        frame[1..17].to_vec()
    }

    #[test]
    fn targeted_drain_offers_the_hint_matched_envelope_and_does_not_remove_it() {
        // CARRY-01: handing a 1:1 carry to its recipient is an offer, not a
        // delete. The row stays until a later authenticated digest names it.
        let store = store();
        let me = generate_identity();
        let peer = generate_identity();
        let msg_id = generate_msg_id();
        let hint = compute_recipient_hint(peer.user_id.clone(), NOW);
        enqueue(&store, msg_id.clone(), hint, vec![0xAA; 32], false, NOW);

        let router = router_for(&peer.user_id);
        let spray = CoreSprayPolicy::new();
        let outcome = store
            .plan_mesh_meet(
                &router,
                &spray,
                request(me.user_id, peer.user_id, vec![], true),
            )
            .unwrap();

        assert_eq!(outcome.work.targeted_sent, 1);
        assert_eq!(frame_msg_id(&outcome.targeted_frames[0]), msg_id);
        assert!(
            outcome.spray_frames.is_empty(),
            "hint-matched mail is the drain, not the mule spray"
        );
        assert_eq!(outcome.work.confirmed_removed, 0);
        assert_eq!(
            store.carried_len().unwrap(),
            1,
            "dispatch is not digest-proof; the mule still holds the copy"
        );
    }

    #[test]
    fn digest_exclusion_skips_an_envelope_the_peer_already_advertises() {
        // Unauthenticated so confirm cannot retire the row: the skip is
        // exclusion of this encounter's offer, not a delete.
        let store = store();
        let me = generate_identity();
        let peer = generate_identity();
        let known_id = generate_msg_id();
        let hint = compute_recipient_hint(peer.user_id.clone(), NOW);
        enqueue(&store, known_id.clone(), hint, vec![0xBB; 32], false, NOW);

        let router = router_for(&peer.user_id);
        let spray = CoreSprayPolicy::new();
        let outcome = store
            .plan_mesh_meet(
                &router,
                &spray,
                request(me.user_id, peer.user_id, vec![known_id], false),
            )
            .unwrap();

        assert_eq!(outcome.work.targeted_sent, 0);
        assert_eq!(outcome.work.skipped_known, 1);
        assert!(outcome.targeted_frames.is_empty());
        assert_eq!(outcome.work.confirmed_removed, 0);
        assert_eq!(store.carried_len().unwrap(), 1);
    }

    #[test]
    fn spray_stops_when_the_encounter_budget_is_spent() {
        let store = store();
        let me = generate_identity();
        let peer = generate_identity();
        let stranger = generate_identity();
        let hint = compute_recipient_hint(stranger.user_id, NOW);
        for index in 0..5 {
            let mut msg_id = vec![0xC0; 16];
            msg_id[1] = index;
            // Distinct ciphertext: the carry queue dedupes on (hint, sealed).
            let mut sealed = vec![0xAB; 100];
            sealed[0] = index;
            enqueue(
                &store,
                msg_id,
                hint.clone(),
                sealed,
                false,
                NOW + i64::from(index),
            );
        }

        let spray = CoreSprayPolicy::with_config(SprayPolicyConfig {
            carried_budget_bytes: 250,
            own_outbound_budget_bytes: 0,
            own_receipt_budget_bytes: 0,
            link_burst_bytes: 4_000,
            ..SprayPolicyConfig::default()
        });
        let router = router_for(&peer.user_id);
        let outcome = store
            .plan_mesh_meet(
                &router,
                &spray,
                request(me.user_id, peer.user_id, vec![], true),
            )
            .unwrap();

        assert!(
            outcome.targeted_frames.is_empty(),
            "foreign mail is not hint-matched to this peer"
        );
        assert_eq!(
            outcome.work.sprayed, 2,
            "250 bytes fits two 100-byte envelopes; the third would overflow"
        );
        assert_eq!(
            store.carried_len().unwrap(),
            5,
            "a budget cut offers; it never deletes"
        );
    }

    #[test]
    fn a_repeat_meeting_does_not_resend_a_known_carried_envelope() {
        let store = store();
        let me = generate_identity();
        let peer = generate_identity();
        let stranger = generate_identity();
        let msg_id = generate_msg_id();
        let hint = compute_recipient_hint(stranger.user_id, NOW);
        enqueue(&store, msg_id.clone(), hint, vec![0xCD; 32], false, NOW);

        let first_router = router_for(&peer.user_id);
        let first_spray = CoreSprayPolicy::new();

        let first = store
            .plan_mesh_meet(
                &first_router,
                &first_spray,
                request(me.user_id.clone(), peer.user_id.clone(), vec![], true),
            )
            .unwrap();
        assert_eq!(first.work.sprayed, 1);
        assert_eq!(frame_msg_id(&first.spray_frames[0]), msg_id);

        // A later meeting of the same pair, with a fresh link session, whose
        // digest now names the id the first encounter offered. A new router
        // and spray policy isolate digest exclusion from cadence / cursor
        // parking: the second encounter is allowed to spray, and must still
        // offer nothing. The mule still holds the row.
        let second_router = router_for(&peer.user_id);
        let second_spray = CoreSprayPolicy::new();
        let second = store
            .plan_mesh_meet(
                &second_router,
                &second_spray,
                request(me.user_id, peer.user_id, vec![msg_id], true),
            )
            .unwrap();
        assert_eq!(second.work.sprayed, 0);
        assert!(second.spray_frames.is_empty());
        assert_eq!(
            store.carried_len().unwrap(),
            1,
            "a mule-to-mule offer is never a 1:1 confirm"
        );
    }

    #[test]
    fn an_unauthenticated_digest_excludes_but_never_removes() {
        // CARRY-02: a spoofed BLE DIGEST can decline the mail, not destroy it.
        let store = store();
        let me = generate_identity();
        let peer = generate_identity();
        let msg_id = generate_msg_id();
        let hint = compute_recipient_hint(peer.user_id.clone(), NOW);
        enqueue(&store, msg_id.clone(), hint, vec![0xEE; 32], false, NOW);

        let router = router_for(&peer.user_id);
        let spray = CoreSprayPolicy::new();
        let outcome = store
            .plan_mesh_meet(
                &router,
                &spray,
                request(me.user_id, peer.user_id, vec![msg_id], false),
            )
            .unwrap();

        assert_eq!(outcome.work.confirmed_removed, 0);
        assert_eq!(outcome.work.targeted_sent, 0);
        assert_eq!(outcome.work.skipped_known, 1);
        assert_eq!(store.carried_len().unwrap(), 1);
    }
}
