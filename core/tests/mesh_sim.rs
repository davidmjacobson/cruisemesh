//! Headless multi-node mesh simulation (DESIGN.md §5.3 gossip, §10/§11 M2).
//!
//! The gossip relay and carry queue can't be exercised with the two physical
//! phones on hand -- relaying and muling only mean anything with three or more
//! nodes and intermittent contact. This simulation fills that gap: it stands
//! up many nodes with real identities and real crypto, connects and churns
//! them over discrete rounds, and checks that a sealed message reaches its
//! recipient across hops and time gaps it could never reach directly.
//!
//! It drives the **real** core primitives -- `seal_message`/`open_message`,
//! the §6.4 frame codec, [`SeenIds`], and the [`MessageStore`] carry queue --
//! composed by the **same algorithm** `android/.../mesh/MeshService.kt` runs on
//! device:
//!   * receive: dedupe on `msg_id` -> drop if past `expiry` -> try to open;
//!     opening means we're the recipient (deliver, don't re-flood), else it's
//!     foreign traffic to relay (flood with `hop_ttl - 1`, if hops remain) and
//!     carry (enqueue for later).
//!   * meeting (HELLO): first drain carried envelopes whose `recipient_hint`
//!     matches the peer, handing each over and dropping it once delivered;
//!     then spray the remaining carried envelopes onward to a non-recipient
//!     mule, excluding any `msg_id`s that mule's digest says it already knows.
//!
//! Because that orchestration currently lives in Kotlin, this file
//! re-implements it in `SimNode::receive` / `Network::meet`. That validates
//! the protocol, the primitives, and the *design*, and guards against
//! regressions in the shared core -- but it is NOT a test of the Kotlin code
//! itself. The durable fix is to hoist the algorithm into the core so both the
//! shell and this sim call one implementation (noted in HANDOFF). Keep the two
//! in sync until then.

use std::collections::VecDeque;

use cruisemesh_core::{
    compute_recipient_hint, default_expiry, encode_envelope_frame, generate_identity,
    generate_msg_id, open_group_message, open_message, parse_frame, seal_group_message,
    seal_message, CarriedEnvelope, Frame, Group, Identity, MessageStore, SeenIds, DEFAULT_HOP_TTL,
};

const MS_PER_DAY: i64 = 24 * 60 * 60 * 1000;
const BASE_NOW: i64 = 1_700_000_000_000;
const FOREIGN_BUDGET: i64 = 5 * 1024 * 1024;
const DIGEST_CARRIED_MSG_IDS_LIMIT: u64 = 512;

/// One mesh participant: identity + crypto, its persistent carry queue, its
/// flood-dedupe set, and an inbox of payloads it successfully opened.
struct SimNode {
    identity: Identity,
    store: MessageStore,
    seen: SeenIds,
    /// UserIDs this node treats as contacts, for the family/foreign carry
    /// classification (mules generally have none of the recipient's).
    contacts: Vec<Vec<u8>>,
    /// Plaintext of every message this node was the recipient of and opened.
    inbox: Vec<Vec<u8>>,
}

impl SimNode {
    fn new() -> Self {
        SimNode {
            identity: generate_identity(),
            store: MessageStore::open(":memory:".to_string()).expect("open in-memory store"),
            seen: SeenIds::new(),
            contacts: Vec::new(),
            inbox: Vec::new(),
        }
    }

    fn user_id(&self) -> Vec<u8> {
        self.identity.user_id.clone()
    }

    fn import_group(&self, group: Group) {
        self.store.upsert_group(group).expect("import group");
    }

    /// Mirror of `MeshService.handleEnvelope`'s per-frame handling. Returns the
    /// frame to flood onward (already `hop_ttl`-decremented) when this node is
    /// a relay with budget left, or `None` when the frame is a duplicate,
    /// expired, delivered to us, or out of hops.
    fn receive(&mut self, frame_bytes: &[u8], now: i64) -> Option<Vec<u8>> {
        let Ok(Frame::Envelope {
            msg_id,
            hop_ttl,
            expiry,
            recipient_hint,
            sealed,
        }) = parse_frame(frame_bytes.to_vec())
        else {
            return None;
        };

        // 1. Dedupe: handle each msg_id once.
        if !self.seen.check_and_record(msg_id.clone()) {
            return None;
        }
        // 2. Expiry: carriers drop past-expiry envelopes.
        if expiry <= now {
            return None;
        }
        // 3. Open vs relay.
        if let Ok(opened) = open_message(self.identity.clone(), sealed.clone()) {
            // Addressed to us directly: deliver, do not re-flood.
            self.inbox.push(opened.payload);
            return None;
        }

        if let Some(opened) = self.try_open_group_message(&recipient_hint, &sealed, now) {
            // Group member delivery still keeps relaying/carrying for absent members.
            self.inbox.push(opened.payload);
            self.store
                .enqueue_carried_envelope(
                    CarriedEnvelope {
                        msg_id: msg_id.clone(),
                        hop_ttl,
                        expiry,
                        recipient_hint: recipient_hint.clone(),
                        sealed: sealed.clone(),
                    },
                    true,
                    now,
                    FOREIGN_BUDGET,
                )
                .expect("enqueue carried group envelope");
            if hop_ttl > 1 {
                return Some(encode_envelope_frame(
                    msg_id,
                    hop_ttl - 1,
                    expiry,
                    recipient_hint,
                    sealed,
                ));
            }
            return None;
        }

        // Foreign: carry it for later, and relay it now if hops remain.
        let is_family = self.hint_matches_known_target(&recipient_hint, now);
        self.store
            .enqueue_carried_envelope(
                CarriedEnvelope {
                    msg_id: msg_id.clone(),
                    hop_ttl,
                    expiry,
                    recipient_hint: recipient_hint.clone(),
                    sealed: sealed.clone(),
                },
                is_family,
                now,
                FOREIGN_BUDGET,
            )
            .expect("enqueue carried");
        if hop_ttl > 1 {
            Some(encode_envelope_frame(
                msg_id,
                hop_ttl - 1,
                expiry,
                recipient_hint,
                sealed,
            ))
        } else {
            None
        }
    }

    fn try_open_group_message(
        &self,
        recipient_hint: &[u8],
        sealed: &[u8],
        now: i64,
    ) -> Option<cruisemesh_core::OpenedMessage> {
        self.store
            .list_groups()
            .expect("list groups")
            .into_iter()
            .filter(|group| {
                recent_hints(&group.id, now)
                    .iter()
                    .any(|hint| hint == recipient_hint)
            })
            .find_map(|group| open_group_message(group, sealed.to_vec()).ok())
    }

    fn hint_matches_known_target(&self, hint: &[u8], now: i64) -> bool {
        if self
            .contacts
            .iter()
            .any(|target| recent_hints(target, now).iter().any(|h| h == hint))
        {
            return true;
        }
        self.store
            .list_groups()
            .expect("list groups")
            .into_iter()
            .any(|group| recent_hints(&group.id, now).iter().any(|h| h == hint))
    }

    fn recent_delivery_hints(&self, now: i64) -> Vec<Vec<u8>> {
        let mut hints = recent_hints(&self.user_id(), now);
        for group in self.store.list_groups().expect("list groups") {
            hints.extend(recent_hints(&group.id, now));
        }
        hints
    }

    fn recognizes_group_hint(&self, recipient_hint: &[u8], now: i64) -> bool {
        self.store
            .list_groups()
            .expect("list groups")
            .into_iter()
            .any(|group| {
                recent_hints(&group.id, now)
                    .iter()
                    .any(|hint| hint == recipient_hint)
            })
    }
}

/// The `recipient_hint`s a peer with `user_id` could match for a still-live
/// envelope: their UserID hashed against each day-number in the expiry window
/// (mirror of `MeshService.recentHintsFor`).
fn recent_hints(user_id: &[u8], now: i64) -> Vec<Vec<u8>> {
    (0..=7)
        .map(|days_ago| compute_recipient_hint(user_id.to_vec(), now - days_ago * MS_PER_DAY))
        .collect()
}

/// A collection of nodes plus the current round's adjacency (which nodes are
/// in radio contact). Adjacency is symmetric and reset each round to model
/// mobility/churn.
struct Network {
    nodes: Vec<SimNode>,
    adjacency: Vec<Vec<usize>>,
    /// Every frame handed onto a link this run -- the flooding-cost metric.
    transmissions: usize,
}

impl Network {
    fn new(n: usize) -> Self {
        Network {
            nodes: (0..n).map(|_| SimNode::new()).collect(),
            adjacency: vec![Vec::new(); n],
            transmissions: 0,
        }
    }

    fn set_edges(&mut self, edges: &[(usize, usize)]) {
        for adj in &mut self.adjacency {
            adj.clear();
        }
        for &(a, b) in edges {
            self.adjacency[a].push(b);
            self.adjacency[b].push(a);
        }
    }

    /// Seal `payload` from `from` to recipient `to`, wrap it in a fresh §6.4
    /// header, mark our own msg_id seen (a sealed box can't be opened by its
    /// sender), and flood it to `from`'s current neighbors. Mirrors the send
    /// path plus the initial flood.
    fn author_and_flood(&mut self, from: usize, to: usize, payload: &[u8], hop_ttl: u8, now: i64) {
        let recipient_agree_pk = self.nodes[to].identity.agree_pk.clone();
        let recipient_user_id = self.nodes[to].user_id();
        let sealed = seal_message(
            self.nodes[from].identity.clone(),
            recipient_agree_pk,
            payload.to_vec(),
        )
        .expect("seal");
        let msg_id = generate_msg_id();
        self.nodes[from].seen.record(msg_id.clone());
        let frame = encode_envelope_frame(
            msg_id,
            hop_ttl,
            default_expiry(now),
            compute_recipient_hint(recipient_user_id, now),
            sealed,
        );
        self.flood_from(from, frame, now);
    }

    fn author_group_and_flood(
        &mut self,
        from: usize,
        group: Group,
        payload: &[u8],
        hop_ttl: u8,
        now: i64,
    ) {
        let sealed = seal_group_message(
            self.nodes[from].identity.clone(),
            group.clone(),
            payload.to_vec(),
        )
        .expect("group seal");
        let msg_id = generate_msg_id();
        self.nodes[from].seen.record(msg_id.clone());
        let frame = encode_envelope_frame(
            msg_id,
            hop_ttl,
            default_expiry(now),
            compute_recipient_hint(group.id, now),
            sealed,
        );
        self.flood_from(from, frame, now);
    }

    /// Deliver `frame` to every neighbor of `origin`, cascading relays through
    /// the round's connected component until quiescent. Per-node dedupe
    /// guarantees termination.
    fn flood_from(&mut self, origin: usize, frame: Vec<u8>, now: i64) {
        let mut queue: VecDeque<(usize, Vec<u8>, usize)> = VecDeque::new();
        for &nb in &self.adjacency[origin] {
            queue.push_back((nb, frame.clone(), origin));
        }
        while let Some((target, frame, from)) = queue.pop_front() {
            self.transmissions += 1;
            let relay = self.nodes[target].receive(&frame, now);
            if let Some(relayed) = relay {
                let neighbors = self.adjacency[target].clone();
                for nb in neighbors {
                    if nb != from {
                        queue.push_back((nb, relayed.clone(), target));
                    }
                }
            }
        }
    }

    /// Every in-contact pair does the HELLO sync: first targeted carried
    /// delivery to the true recipient, then spray-on-connect to a non-recipient
    /// mule excluding any `msg_id`s the peer already knows. Mirrors
    /// `MeshService.drainCarriedEnvelopesTo` plus `sprayCarriedEnvelopesTo`,
    /// run in both directions for each edge.
    fn meet(&mut self, now: i64) {
        let n = self.nodes.len();
        for a in 0..n {
            let neighbors = self.adjacency[a].clone();
            for b in neighbors {
                let peer_known_msg_ids = self.nodes[b]
                    .store
                    .carried_msg_ids(DIGEST_CARRIED_MSG_IDS_LIMIT)
                    .expect("peer carried msg ids");
                let hints = self.nodes[b].recent_delivery_hints(now);
                let drained = self.nodes[a]
                    .store
                    .carried_envelopes_for_hints(hints, now)
                    .expect("query carried");
                for env in drained {
                    let sender_knows_group =
                        self.nodes[a].recognizes_group_hint(&env.recipient_hint, now);
                    if sender_knows_group
                        && peer_known_msg_ids
                            .iter()
                            .any(|known_msg_id| known_msg_id == &env.msg_id)
                    {
                        continue;
                    }
                    if !sender_knows_group {
                        self.nodes[a]
                            .store
                            .remove_carried_envelope(env.msg_id.clone())
                            .expect("remove carried");
                    }
                    let frame = encode_envelope_frame(
                        env.msg_id.clone(),
                        env.hop_ttl,
                        env.expiry,
                        env.recipient_hint,
                        env.sealed,
                    );
                    self.transmissions += 1;
                    // The peer opens it if it's theirs; if not (only on a hint
                    // collision, vanishingly rare) they'd re-carry -- ignore the
                    // relay return here, meetings don't cascade floods.
                    let _ = self.nodes[b].receive(&frame, now);
                }

                let spray = self.nodes[a]
                    .store
                    .carried_envelopes_for_peer_sync(
                        self.nodes[b].recent_delivery_hints(now),
                        peer_known_msg_ids,
                        now,
                    )
                    .expect("query spray candidates");
                for env in spray {
                    let frame = encode_envelope_frame(
                        env.msg_id,
                        env.hop_ttl,
                        env.expiry,
                        env.recipient_hint,
                        env.sealed,
                    );
                    self.transmissions += 1;
                    let _ = self.nodes[b].receive(&frame, now);
                }
            }
        }
    }

    fn inbox_len(&self, node: usize) -> usize {
        self.nodes[node].inbox.len()
    }

    fn openers_of(&self, payload: &[u8]) -> Vec<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| n.inbox.iter().any(|m| m == payload))
            .map(|(i, _)| i)
            .collect()
    }
}

#[test]
fn flood_cascades_across_a_multihop_chain_and_only_the_recipient_opens() {
    // 0 - 1 - 2 - 3, all connected this round. 0 sends to 3, two relays away.
    let mut net = Network::new(4);
    net.set_edges(&[(0, 1), (1, 2), (2, 3)]);
    let msg = b"three hops to dinner";

    net.author_and_flood(0, 3, msg, DEFAULT_HOP_TTL, BASE_NOW);

    assert_eq!(
        net.openers_of(msg),
        vec![3],
        "only the intended recipient opens it"
    );
    assert_eq!(net.inbox_len(3), 1);
}

#[test]
fn hop_ttl_bounds_the_flood() {
    let msg = b"barely made it";

    // Recipient two hops away (0 - 1 - 2). hop_ttl = 1 means "neighbors only":
    // node 1 receives it but has no hops left to forward, so 2 never sees it.
    let mut short = Network::new(3);
    short.set_edges(&[(0, 1), (1, 2)]);
    short.author_and_flood(0, 2, msg, 1, BASE_NOW);
    assert!(
        short.openers_of(msg).is_empty(),
        "hop_ttl=1 can't reach a 2-hop recipient"
    );

    // hop_ttl = 2 gives node 1 exactly one forward, reaching node 2.
    let mut ok = Network::new(3);
    ok.set_edges(&[(0, 1), (1, 2)]);
    ok.author_and_flood(0, 2, msg, 2, BASE_NOW);
    assert_eq!(
        ok.openers_of(msg),
        vec![2],
        "hop_ttl=2 reaches the 2-hop recipient"
    );
}

#[test]
fn a_single_mule_carries_a_message_across_a_time_gap() {
    // The canonical DTN win: sender and recipient are never in contact, but a
    // mule meets each in turn. 0 = sender, 1 = mule, 2 = recipient.
    let mut net = Network::new(3);
    let msg = b"see you at the far end of the ship";

    // Round 1: only 0 and 1 are in range. 0 sends to 2 (not present). 1 can't
    // open it -> relays (no other neighbors) and carries it.
    net.set_edges(&[(0, 1)]);
    net.author_and_flood(0, 2, msg, DEFAULT_HOP_TTL, BASE_NOW);
    assert!(
        net.openers_of(msg).is_empty(),
        "recipient wasn't in range yet"
    );
    assert_eq!(
        net.nodes[1].store.carried_len().unwrap(),
        1,
        "mule is carrying it"
    );

    // Round 2: the mule has moved and now meets the recipient; the sender is
    // gone. The carry-drain hands it over on HELLO.
    let later = BASE_NOW + 30_000;
    net.set_edges(&[(1, 2)]);
    net.meet(later);

    assert_eq!(
        net.openers_of(msg),
        vec![2],
        "the mule delivered it to the recipient"
    );
    assert_eq!(
        net.nodes[1].store.carried_len().unwrap(),
        0,
        "mule dropped it once delivered"
    );
}

#[test]
fn an_expired_envelope_is_never_delivered() {
    // 0 - 1 - 2; 0 sends to 2, but the envelope's expiry is already in the past.
    let mut net = Network::new(3);
    net.set_edges(&[(0, 1), (1, 2)]);
    let recipient_agree_pk = net.nodes[2].identity.agree_pk.clone();
    let recipient_user_id = net.nodes[2].user_id();
    let sealed = seal_message(
        net.nodes[0].identity.clone(),
        recipient_agree_pk,
        b"too late".to_vec(),
    )
    .expect("seal");
    let msg_id = generate_msg_id();
    let already_expired = BASE_NOW - 1;
    let frame = encode_envelope_frame(
        msg_id,
        DEFAULT_HOP_TTL,
        already_expired,
        compute_recipient_hint(recipient_user_id, BASE_NOW),
        sealed,
    );

    net.flood_from(0, frame, BASE_NOW);

    assert!(
        net.openers_of(b"too late").is_empty(),
        "expired envelope is dropped, not delivered"
    );
    assert_eq!(
        net.nodes[1].store.carried_len().unwrap(),
        0,
        "and not carried"
    );
}

#[test]
fn fifty_node_dense_flood_delivers_once_and_dedupe_bounds_the_cost() {
    // 50 nodes, all mutually in range this round -- the worst case for flood
    // amplification. The message must reach its recipient, exactly one node
    // (the recipient) may open it, and dedupe must keep total transmissions
    // bounded rather than exploding combinatorially.
    let n = 50;
    let mut net = Network::new(n);
    let edges: Vec<(usize, usize)> = (0..n)
        .flat_map(|a| (a + 1..n).map(move |b| (a, b)))
        .collect();
    net.set_edges(&edges);
    let msg = b"all hands: lifeboat drill at 1400";

    net.author_and_flood(0, 37, msg, DEFAULT_HOP_TTL, BASE_NOW);

    assert_eq!(
        net.openers_of(msg),
        vec![37],
        "exactly the recipient opens it, nobody else"
    );
    // Each node relays a given msg_id at most once (dedupe), to at most n-1
    // neighbors, so transmissions can't exceed n*(n-1). A blow-up would mean
    // the seen-set isn't cutting the flood.
    assert!(
        net.transmissions <= n * (n - 1),
        "flood cost {} exceeded the dedupe bound {}",
        net.transmissions,
        n * (n - 1),
    );
}

#[test]
fn multi_mule_carry_chain_delivers_once_spray_on_connect_exists() {
    // 0 sends to 3. A chain of mules meets pairwise over time -- 0&1, then
    // 1&2, then 2&3 -- but 0 and 3 never share a mule that also meets the
    // other. With spray-on-connect paired to the digest's exact carried
    // `msg_id` set, the envelope should now cross 1->2 and then reach 3.
    let mut net = Network::new(4);
    let msg = b"lost in the relay chain";

    net.set_edges(&[(0, 1)]);
    net.author_and_flood(0, 3, msg, DEFAULT_HOP_TTL, BASE_NOW);
    assert_eq!(
        net.nodes[1].store.carried_len().unwrap(),
        1,
        "mule 1 picked it up"
    );

    net.set_edges(&[(1, 2)]);
    net.meet(BASE_NOW + 10_000);
    assert_eq!(
        net.nodes[2].store.carried_len().unwrap(),
        1,
        "mule 1 sprays it onward to mule 2"
    );

    net.set_edges(&[(2, 3)]);
    net.meet(BASE_NOW + 20_000);

    assert_eq!(
        net.openers_of(msg),
        vec![3],
        "multi-mule carry chain reaches the recipient"
    );
}

#[test]
fn repeated_mule_meetings_do_not_resend_known_carried_envelopes() {
    let mut net = Network::new(4);
    let msg = b"don't keep re-spraying me";

    net.set_edges(&[(0, 1)]);
    net.author_and_flood(0, 3, msg, DEFAULT_HOP_TTL, BASE_NOW);

    net.set_edges(&[(1, 2)]);
    net.meet(BASE_NOW + 10_000);
    let after_first_meet = net.transmissions;

    // Same two mules meet again before the recipient leg. Mule 2 already
    // carries this msg_id, so the exact digest set should suppress a resend.
    net.meet(BASE_NOW + 20_000);
    assert_eq!(
        net.transmissions, after_first_meet,
        "second meeting was fully suppressed"
    );
}

#[test]
fn group_message_floods_and_mules_to_every_other_member_with_dedupe() {
    let mut net = Network::new(4);
    let group = Group {
        id: vec![0x44; 16],
        name: "Deck Crew".to_string(),
        member_user_ids: net.nodes.iter().map(|node| node.user_id()).collect(),
        key: vec![0x55; 32],
    };
    for node in &net.nodes {
        node.import_group(group.clone());
    }
    let msg = b"all hands to station bravo";

    net.set_edges(&[(0, 1)]);
    net.author_group_and_flood(0, group, msg, DEFAULT_HOP_TTL, BASE_NOW);
    assert_eq!(
        net.openers_of(msg),
        vec![1],
        "first in-range member opens it"
    );
    assert_eq!(
        net.nodes[1].store.carried_len().unwrap(),
        1,
        "group member keeps carrying after opening"
    );

    net.set_edges(&[(1, 2)]);
    net.meet(BASE_NOW + 10_000);
    assert_eq!(
        net.openers_of(msg),
        vec![1, 2],
        "second member gets it via mule delivery"
    );
    let after_first_meet = net.transmissions;

    net.meet(BASE_NOW + 20_000);
    assert_eq!(
        net.transmissions, after_first_meet,
        "repeat meeting was dedupe-suppressed"
    );

    net.set_edges(&[(2, 3)]);
    net.meet(BASE_NOW + 30_000);
    assert_eq!(
        net.openers_of(msg),
        vec![1, 2, 3],
        "all other group members opened it once"
    );
}
