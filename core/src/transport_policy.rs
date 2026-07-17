use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::DigestEntry;

pub const SMALL_FRAME_RACE_MAX_BYTES: u32 = 8 * 1024;
pub const DEFAULT_INITIAL_BACKOFF_MS: i64 = 5_000;
pub const DEFAULT_MAX_BACKOFF_MS: i64 = 5 * 60_000;
pub const DEFAULT_MAX_CONSECUTIVE_FAILURES: u32 = 6;
pub const DEFAULT_LAN_HEALTH_TIMEOUT_MS: i64 = 20_000;
pub const DEFAULT_LAN_HEALTH_MAX_TIMEOUTS: u32 = 3;

#[uniffi::export]
pub fn digest_is_expected_chat_id(digest_chat_id: Vec<u8>, hello_user_id: Option<Vec<u8>>) -> bool {
    hello_user_id.is_some_and(|id| id == digest_chat_id)
}

#[uniffi::export]
pub fn digest_through_lamport_for_sender(
    entries: Vec<DigestEntry>,
    sender_user_id: Vec<u8>,
) -> u64 {
    entries
        .into_iter()
        .find(|entry| entry.sender_user_id == sender_user_id)
        .map_or(0, |entry| entry.through_lamport)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreTransport {
    Central,
    Peripheral,
    Lan,
}

impl CoreTransport {
    fn priority(self) -> u8 {
        match self {
            Self::Lan => 10,
            Self::Central | Self::Peripheral => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreTransportRoute {
    pub transport: CoreTransport,
    pub address: String,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreIdentifiedRoute {
    pub transport: CoreTransport,
    pub address: String,
    pub user_id: Vec<u8>,
}

#[derive(Clone)]
struct Peer {
    transport: CoreTransport,
    user_id: Option<Vec<u8>>,
}

#[derive(uniffi::Object)]
pub struct CoreMeshRouterState {
    peers: Mutex<HashMap<String, Peer>>,
}

#[uniffi::export]
impl CoreMeshRouterState {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self {
            peers: Mutex::new(HashMap::new()),
        }
    }

    pub fn on_connected(&self, address: String, transport: CoreTransport) {
        self.peers.lock().unwrap().insert(
            address,
            Peer {
                transport,
                user_id: None,
            },
        );
    }

    pub fn on_disconnected(&self, address: String) {
        self.peers.lock().unwrap().remove(&address);
    }

    pub fn on_hello(&self, address: String, user_id: Vec<u8>) -> bool {
        let mut peers = self.peers.lock().unwrap();
        let Some(peer) = peers.get_mut(&address) else {
            return false;
        };
        if peer.user_id.as_ref().is_some_and(|known| *known != user_id) {
            return false;
        }
        peer.user_id = Some(user_id);
        true
    }

    pub fn user_id_for(&self, address: String) -> Option<Vec<u8>> {
        self.peers
            .lock()
            .unwrap()
            .get(&address)
            .and_then(|peer| peer.user_id.clone())
    }

    pub fn transport_for(&self, address: String) -> Option<CoreTransport> {
        self.peers
            .lock()
            .unwrap()
            .get(&address)
            .map(|peer| peer.transport)
    }

    pub fn connected_routes(&self) -> Vec<CoreTransportRoute> {
        self.peers
            .lock()
            .unwrap()
            .iter()
            .map(|(address, peer)| CoreTransportRoute {
                transport: peer.transport,
                address: address.clone(),
            })
            .collect()
    }

    pub fn identified_routes(&self) -> Vec<CoreIdentifiedRoute> {
        self.peers
            .lock()
            .unwrap()
            .iter()
            .filter_map(|(address, peer)| {
                peer.user_id.as_ref().map(|user_id| CoreIdentifiedRoute {
                    transport: peer.transport,
                    address: address.clone(),
                    user_id: user_id.clone(),
                })
            })
            .collect()
    }

    pub fn connected_user_count(&self) -> u32 {
        self.peers
            .lock()
            .unwrap()
            .values()
            .filter_map(|peer| peer.user_id.clone())
            .collect::<HashSet<_>>()
            .len() as u32
    }

    pub fn route_for(&self, user_id: Vec<u8>) -> Option<CoreTransportRoute> {
        self.routes_for(user_id).into_iter().next()
    }

    pub fn routes_for(&self, user_id: Vec<u8>) -> Vec<CoreTransportRoute> {
        let mut routes: Vec<_> = self
            .peers
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, peer)| peer.user_id.as_ref() == Some(&user_id))
            .map(|(address, peer)| CoreTransportRoute {
                transport: peer.transport,
                address: address.clone(),
            })
            .collect();
        routes.sort_by_key(|route| std::cmp::Reverse(route.transport.priority()));
        routes
    }

    pub fn helloed_user_ids(&self) -> Vec<Vec<u8>> {
        self.peers
            .lock()
            .unwrap()
            .values()
            .filter_map(|peer| peer.user_id.clone())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn clear_transports(&self, transports: Vec<CoreTransport>) {
        self.peers
            .lock()
            .unwrap()
            .retain(|_, peer| !transports.contains(&peer.transport));
    }

    pub fn clear(&self) {
        self.peers.lock().unwrap().clear();
    }
}

#[uniffi::export]
pub fn core_transport_send_plan(
    routes: Vec<CoreTransportRoute>,
    frame_size: u32,
) -> Vec<CoreTransportRoute> {
    let Some(lan) = routes
        .iter()
        .find(|route| route.transport == CoreTransport::Lan)
        .cloned()
    else {
        return routes.into_iter().take(1).collect();
    };
    if frame_size > SMALL_FRAME_RACE_MAX_BYTES {
        return vec![lan];
    }
    let Some(ble) = routes
        .into_iter()
        .find(|route| route.transport != CoreTransport::Lan)
    else {
        return vec![lan];
    };
    vec![lan, ble]
}

#[derive(Clone, Copy)]
struct BackoffState {
    consecutive_failures: u32,
    next_eligible_at_ms: i64,
}

#[derive(uniffi::Object)]
pub struct CoreReconnectBackoffTracker {
    initial_backoff_ms: i64,
    max_backoff_ms: i64,
    max_consecutive_failures: u32,
    state: Mutex<HashMap<String, BackoffState>>,
}

#[uniffi::export]
impl CoreReconnectBackoffTracker {
    #[uniffi::constructor]
    pub fn new(initial_backoff_ms: i64, max_backoff_ms: i64, max_failures: u32) -> Self {
        Self {
            initial_backoff_ms: initial_backoff_ms.max(0),
            max_backoff_ms: max_backoff_ms.max(0),
            max_consecutive_failures: max_failures,
            state: Mutex::new(HashMap::new()),
        }
    }

    pub fn can_attempt(&self, address: String, now_ms: i64) -> bool {
        self.state
            .lock()
            .unwrap()
            .get(&address)
            .map_or(true, |state| {
                state.consecutive_failures < self.max_consecutive_failures
                    && now_ms >= state.next_eligible_at_ms
            })
    }

    pub fn is_given_up(&self, address: String) -> bool {
        self.failure_count(address) >= self.max_consecutive_failures
    }

    pub fn failure_count(&self, address: String) -> u32 {
        self.state
            .lock()
            .unwrap()
            .get(&address)
            .map_or(0, |state| state.consecutive_failures)
    }

    pub fn retry_delay_ms(&self, address: String, now_ms: i64) -> Option<i64> {
        self.state.lock().unwrap().get(&address).and_then(|state| {
            (state.consecutive_failures < self.max_consecutive_failures)
                .then(|| state.next_eligible_at_ms.saturating_sub(now_ms).max(0))
        })
    }

    pub fn record_failure(&self, address: String, now_ms: i64) -> u32 {
        let mut states = self.state.lock().unwrap();
        let failures = states
            .get(&address)
            .map_or(1, |state| state.consecutive_failures.saturating_add(1));
        let multiplier = 1_i64
            .checked_shl(failures.saturating_sub(1).min(20))
            .unwrap_or(i64::MAX);
        let backoff = self
            .initial_backoff_ms
            .saturating_mul(multiplier)
            .min(self.max_backoff_ms);
        states.insert(
            address,
            BackoffState {
                consecutive_failures: failures,
                next_eligible_at_ms: now_ms.saturating_add(backoff),
            },
        );
        failures
    }

    pub fn record_success(&self, address: String) {
        self.state.lock().unwrap().remove(&address);
    }
    pub fn clear(&self) {
        self.state.lock().unwrap().clear();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreLanHealthAction {
    Send,
    Wait,
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreLanHealthDecision {
    pub action: CoreLanHealthAction,
    pub nonce: Option<u64>,
}

#[derive(Clone, Copy)]
struct LanLinkState {
    pending_nonce: Option<u64>,
    sent_at_ms: i64,
    consecutive_timeouts: u32,
}

#[derive(uniffi::Object)]
pub struct CoreLanHealthTracker {
    timeout_ms: i64,
    max_consecutive_timeouts: u32,
    links: Mutex<HashMap<String, LanLinkState>>,
}

#[uniffi::export]
impl CoreLanHealthTracker {
    #[uniffi::constructor]
    pub fn new(timeout_ms: i64, max_timeouts: u32) -> Self {
        Self {
            timeout_ms: timeout_ms.max(0),
            max_consecutive_timeouts: max_timeouts,
            links: Mutex::new(HashMap::new()),
        }
    }

    pub fn next(&self, address: String, now_ms: i64, nonce: u64) -> CoreLanHealthDecision {
        let mut links = self.links.lock().unwrap();
        let Some(current) = links.get(&address).copied() else {
            links.insert(
                address,
                LanLinkState {
                    pending_nonce: Some(nonce),
                    sent_at_ms: now_ms,
                    consecutive_timeouts: 0,
                },
            );
            return health_decision(CoreLanHealthAction::Send, Some(nonce));
        };
        if current.pending_nonce.is_none() {
            links.insert(
                address,
                LanLinkState {
                    pending_nonce: Some(nonce),
                    sent_at_ms: now_ms,
                    ..current
                },
            );
            return health_decision(CoreLanHealthAction::Send, Some(nonce));
        }
        if now_ms.saturating_sub(current.sent_at_ms) < self.timeout_ms {
            return health_decision(CoreLanHealthAction::Wait, None);
        }
        let failures = current.consecutive_timeouts.saturating_add(1);
        if failures >= self.max_consecutive_timeouts {
            links.remove(&address);
            return health_decision(CoreLanHealthAction::Close, None);
        }
        links.insert(
            address,
            LanLinkState {
                pending_nonce: Some(nonce),
                sent_at_ms: now_ms,
                consecutive_timeouts: failures,
            },
        );
        health_decision(CoreLanHealthAction::Send, Some(nonce))
    }

    pub fn response(&self, address: String, nonce: u64, now_ms: i64) -> Option<i64> {
        let mut links = self.links.lock().unwrap();
        let current = links.get(&address).copied()?;
        if current.pending_nonce != Some(nonce) {
            return None;
        }
        links.insert(
            address,
            LanLinkState {
                pending_nonce: None,
                sent_at_ms: 0,
                consecutive_timeouts: 0,
            },
        );
        Some(now_ms.saturating_sub(current.sent_at_ms).max(0))
    }

    pub fn remove(&self, address: String) {
        self.links.lock().unwrap().remove(&address);
    }
    pub fn clear(&self) {
        self.links.lock().unwrap().clear();
    }
}

fn health_decision(action: CoreLanHealthAction, nonce: Option<u64>) -> CoreLanHealthDecision {
    CoreLanHealthDecision { action, nonce }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_rejects_identity_changes_and_prefers_lan() {
        let router = CoreMeshRouterState::new();
        router.on_connected("ble".into(), CoreTransport::Central);
        router.on_connected("lan".into(), CoreTransport::Lan);
        assert!(router.on_hello("ble".into(), vec![1]));
        assert!(router.on_hello("lan".into(), vec![1]));
        assert!(!router.on_hello("lan".into(), vec![2]));
        assert_eq!(router.route_for(vec![1]).unwrap().address, "lan");
        assert_eq!(router.connected_user_count(), 1);
    }

    #[test]
    fn send_plan_races_small_frames_and_uses_lan_for_large_frames() {
        let routes = vec![
            CoreTransportRoute {
                transport: CoreTransport::Lan,
                address: "lan".into(),
            },
            CoreTransportRoute {
                transport: CoreTransport::Central,
                address: "ble".into(),
            },
        ];
        assert_eq!(core_transport_send_plan(routes.clone(), 100).len(), 2);
        assert_eq!(core_transport_send_plan(routes, 9_000).len(), 1);
    }

    #[test]
    fn reconnect_backoff_doubles_then_gives_up() {
        let tracker = CoreReconnectBackoffTracker::new(10, 40, 3);
        assert!(tracker.can_attempt("peer".into(), 0));
        assert_eq!(tracker.record_failure("peer".into(), 0), 1);
        assert_eq!(tracker.retry_delay_ms("peer".into(), 3), Some(7));
        assert_eq!(tracker.record_failure("peer".into(), 10), 2);
        assert_eq!(tracker.record_failure("peer".into(), 30), 3);
        assert!(tracker.is_given_up("peer".into()));
    }

    #[test]
    fn lan_health_tracks_matching_responses_and_closes() {
        let tracker = CoreLanHealthTracker::new(10, 2);
        assert_eq!(
            tracker.next("a".into(), 0, 1).action,
            CoreLanHealthAction::Send
        );
        assert_eq!(
            tracker.next("a".into(), 5, 2).action,
            CoreLanHealthAction::Wait
        );
        assert_eq!(tracker.response("a".into(), 1, 7), Some(7));
        assert_eq!(
            tracker.next("a".into(), 8, 3).action,
            CoreLanHealthAction::Send
        );
        assert_eq!(
            tracker.next("a".into(), 18, 4).action,
            CoreLanHealthAction::Send
        );
        assert_eq!(
            tracker.next("a".into(), 28, 5).action,
            CoreLanHealthAction::Close
        );
    }
}
