//! Ship Wi-Fi compatibility field reports: evidence, verdict, and the closed
//! JSON artifact.
//!
//! `specs/ship-wifi-field-reports.md` is the contract. This module is the
//! Phase 0 core half of it: the shells will later translate live LAN events
//! into the enums below, and a later service will ingest the JSON this
//! module emits. Nothing here talks to a network, opens a file, or remembers
//! a previous process. That is the no-automatic-upload invariant made
//! structural rather than promised: there is no function in this module that
//! could upload even if a caller wanted it to.
//!
//! # Why the reducer lives here
//!
//! The same event sequence must produce the same candidate report on Android
//! and iOS. Putting the decision in either shell is how the connection-health
//! card and the in-chat pill drifted apart; the field-report verdict is worse
//! to get wrong, because a published `likely_isolated` is a claim about a
//! named ship. So the shells only observe, and this module only decides.
//!
//! # What the reducer is allowed to remember
//!
//! An observation session begins on [`ShipWifiObservationEvent::NetworkJoined`]
//! and ends on [`ShipWifiObservationEvent::NetworkLost`], a replacement join,
//! or process restart ([`ShipWifiObservation::new`]). It stores only the
//! coarse facts the report is allowed to contain: permission, VPN readiness,
//! whether an accepted peer was evidenced, how a successful LAN path was
//! first obtained, probe directions, sweep class, and the guided-test
//! confirmations. It never receives an endpoint, a name, a fingerprint, a
//! timestamp, or a contact identifier. Guided-test direction slots are
//! counters, not peer ids, and they are wiped when the test ends.
//!
//! # Why strength is derived and not serialized
//!
//! The public service is specified to recompute evidence strength from the
//! submitted fields and to ignore a client-supplied label. Serializing one
//! would invite the shells to display a value the directory will not honor,
//! and it would be one more field to keep honest. [`core_ship_wifi_evidence_strength`]
//! exists for local preview; the artifact does not carry it.
//!
//! # Closed schema is the real redaction
//!
//! The forbidden-key list is defense in depth. The primary protection is that
//! the serializer only knows the v1 keys, import rejects any other key, and
//! the reducer has no string-typed event fields that could carry an SSID,
//! address, or contact name in the first place. String values that still
//! reach the artifact (catalog "other" text, optional device model, version
//! strings) are scanned for obvious IP and MAC forms; a match fails the
//! export instead of redacting, so a bug cannot hide inside a rewritten
//! field.

use std::sync::{Mutex, MutexGuard};

use data_encoding::BASE64URL_NOPAD;
use rand_core::{OsRng, RngCore};
use serde::Serialize;
use serde_json::{Map, Value};

use crate::identity::CoreError;

/// Schema version this module produces and the only version it will import.
pub const SHIP_WIFI_REPORT_SCHEMA_VERSION: u32 = 1;

/// Consent policy version this module produces. Import accepts only this
/// value: a later policy is a new conversation with the user, not a silent
/// rewrite.
pub const SHIP_WIFI_CONSENT_POLICY_VERSION: u32 = 1;

/// Standalone artifact size cap from the spec. Enforced on both serialize
/// and import so a draft cannot grow a forbidden field by overflowing a
/// parser.
pub const SHIP_WIFI_REPORT_MAX_BYTES: usize = 8 * 1024;

/// Share-sheet file name for a v1 artifact. Kept here so both shells and the
/// golden fixtures agree on one identifier.
pub const SHIP_WIFI_REPORT_FILE_NAME: &str = "cruisemesh-ship-wifi-report-v1.json";

/// 128 random bits, encoded as unpadded base64url (22 characters).
pub const SHIP_WIFI_NONCE_BYTES: usize = 16;

const SHIP_WIFI_NONCE_CHARS: usize = 22;
const MAX_CATALOG_ID_CHARS: usize = 64;
const MAX_OTHER_NAME_CHARS: usize = 128;
const MAX_DEVICE_MODEL_CHARS: usize = 64;
const MAX_OS_MAJOR_CHARS: usize = 16;
const MAX_APP_VERSION_CHARS: usize = 32;
const MAX_JSON_DEPTH: usize = 8;
const QUALIFYING_TIMEOUTS_PER_DIRECTION: u32 = 2;

/// Keys the v1 artifact is forbidden to carry, as named by the spec.
///
/// Matching is case-insensitive and treats an `_`-separated segment equal to
/// one of these names as the same key (`peer_ip` is `ip`). That is stricter
/// than exact equality and is the choice that makes the denylist easiest to
/// prove: a future field cannot hide a network identifier behind a prefix.
pub const SHIP_WIFI_FORBIDDEN_KEYS: &[&str] = &[
    "ssid",
    "bssid",
    "mac",
    "ip",
    "address",
    "endpoint",
    "port",
    "subnet",
    "gateway",
    "dns",
    "user_id",
    "contact",
    "friend",
    "group",
    "chat",
    "message",
    "relay",
    "cabin",
    "deck",
    "venue",
    "itinerary",
    "latitude",
    "longitude",
    "exact_time",
    "installation_id",
    "advertising_id",
    "vendor_id",
];

const TOP_LEVEL_KEYS: &[&str] = &[
    "schema_version",
    "report_nonce",
    "ship",
    "period",
    "network_context",
    "result",
    "reporting_client",
    "consent",
];
const SHIP_KEYS: &[&str] = &["line_id", "ship_id", "line_other", "ship_other"];
const PERIOD_KEYS: &[&str] = &["value", "precision"];
const NETWORK_CONTEXT_KEYS: &[&str] = &["authorization", "separation"];
const RESULT_KEYS: &[&str] = &[
    "verdict",
    "origin",
    "discovery_source",
    "authenticated_lan",
    "encrypted_round_trip",
    "directions_attempted",
    "completed_sweep",
    "local_permission",
    "vpn_readiness",
];
const REPORTING_CLIENT_KEYS: &[&str] = &["platform", "os_major", "app_version", "device_model"];
const CONSENT_KEYS: &[&str] = &["policy_version"];

fn lock_state(state: &Mutex<SessionState>) -> MutexGuard<'_, SessionState> {
    // Same recovery as the store and the transport trackers: a poisoned
    // mutex here guards only in-memory coarse facts, so the alternative to
    // taking the inner value is a process-wide crash the next time a shell
    // feeds an event.
    state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------------------
// Closed vocabularies
// ---------------------------------------------------------------------------

/// How a successful LAN endpoint was first obtained.
///
/// `authenticated_endpoint` collapses BLE, LAN, and the pairwise-encrypted
/// relay hint on purpose: which existing transport supplied the hint is not
/// a compatibility fact, and publishing it would reveal whether a family
/// bought an internet package.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiDiscoverySource {
    Mdns,
    AuthenticatedEndpoint,
    CachedEndpoint,
    BoundedSweep,
    Manual,
    #[default]
    Unknown,
}

impl ShipWifiDiscoverySource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mdns => "mdns",
            Self::AuthenticatedEndpoint => "authenticated_endpoint",
            Self::CachedEndpoint => "cached_endpoint",
            Self::BoundedSweep => "bounded_sweep",
            Self::Manual => "manual",
            Self::Unknown => "unknown",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "mdns" => Self::Mdns,
            "authenticated_endpoint" => Self::AuthenticatedEndpoint,
            "cached_endpoint" => Self::CachedEndpoint,
            "bounded_sweep" => Self::BoundedSweep,
            "manual" => Self::Manual,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }

    fn is_non_mdns_success(self) -> bool {
        matches!(
            self,
            Self::AuthenticatedEndpoint | Self::CachedEndpoint | Self::BoundedSweep | Self::Manual
        )
    }
}

/// Which existing transport handed this device a peer LAN endpoint.
///
/// The report never names the source. The reducer keeps only "was it a
/// non-LAN authenticated link?", which is a qualifying-negative precondition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiEndpointSource {
    Ble,
    Relay,
    Lan,
}

impl ShipWifiEndpointSource {
    fn is_non_lan(self) -> bool {
        matches!(self, Self::Ble | Self::Relay)
    }
}

/// Platform sweep classification, as already computed by the shells.
///
/// Distinct from [`ShipWifiCompletedSweep`]: the live sweep still says
/// `isolation_suspected`, and the report field records that as `all_silent`
/// so the published artifact never uses the stronger word.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiSweepVerdict {
    FoundPeer,
    HealthyButEmpty,
    IsolationSuspected,
    BlockedByPolicy,
    Inconclusive,
}

/// `completed_sweep` in the v1 artifact. Counts of addresses are not a
/// field and must not become one: they fingerprint a particular network
/// more than they help aggregation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiCompletedSweep {
    #[default]
    NotRun,
    FoundPeer,
    HealthyButEmpty,
    AllSilent,
    BlockedByPolicy,
    Inconclusive,
}

impl ShipWifiCompletedSweep {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRun => "not_run",
            Self::FoundPeer => "found_peer",
            Self::HealthyButEmpty => "healthy_but_empty",
            Self::AllSilent => "all_silent",
            Self::BlockedByPolicy => "blocked_by_policy",
            Self::Inconclusive => "inconclusive",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "not_run" => Self::NotRun,
            "found_peer" => Self::FoundPeer,
            "healthy_but_empty" => Self::HealthyButEmpty,
            "all_silent" => Self::AllSilent,
            "blocked_by_policy" => Self::BlockedByPolicy,
            "inconclusive" => Self::Inconclusive,
            _ => return None,
        })
    }

    fn from_sweep(verdict: ShipWifiSweepVerdict) -> Self {
        match verdict {
            ShipWifiSweepVerdict::FoundPeer => Self::FoundPeer,
            ShipWifiSweepVerdict::HealthyButEmpty => Self::HealthyButEmpty,
            ShipWifiSweepVerdict::IsolationSuspected => Self::AllSilent,
            ShipWifiSweepVerdict::BlockedByPolicy => Self::BlockedByPolicy,
            ShipWifiSweepVerdict::Inconclusive => Self::Inconclusive,
        }
    }
}

/// Direction of one direct probe. Never serialized as a peer identifier;
/// the artifact only records how many distinct directions were attempted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiProbeDirection {
    Outbound,
    Inbound,
}

/// Coarse probe RTT. Accepted so a shell does not have to invent a unit,
/// then discarded: the artifact has no timing field, and a millisecond
/// value would be a surprising way to fingerprint a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiLatencyBucket {
    Under100ms,
    Under500ms,
    Under2s,
    Over2s,
    Unmeasured,
}

/// Reduced direct-connection failure class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiFailureClass {
    TimedOut,
    Refused,
    PolicyDenied,
    NetworkLost,
    HandshakeUnknownPeer,
    Other,
}

/// Serialized compatibility verdict.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiVerdict {
    DirectConfirmed,
    DiscoveryFilteredDirectWorked,
    LikelyIsolated,
    OsOrVpnInterference,
    NoPeerEvidence,
    #[default]
    Inconclusive,
}

impl ShipWifiVerdict {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DirectConfirmed => "direct_confirmed",
            Self::DiscoveryFilteredDirectWorked => "discovery_filtered_direct_worked",
            Self::LikelyIsolated => "likely_isolated",
            Self::OsOrVpnInterference => "os_or_vpn_interference",
            Self::NoPeerEvidence => "no_peer_evidence",
            Self::Inconclusive => "inconclusive",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "direct_confirmed" => Self::DirectConfirmed,
            "discovery_filtered_direct_worked" => Self::DiscoveryFilteredDirectWorked,
            "likely_isolated" => Self::LikelyIsolated,
            "os_or_vpn_interference" => Self::OsOrVpnInterference,
            "no_peer_evidence" => Self::NoPeerEvidence,
            "inconclusive" => Self::Inconclusive,
            _ => return None,
        })
    }
}

/// Transparent evidence strength. Derived, never written into the artifact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiEvidenceStrength {
    StrongPositive,
    Positive,
    QualifyingNegative,
    #[default]
    NonQualifying,
}

impl ShipWifiEvidenceStrength {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StrongPositive => "strong_positive",
            Self::Positive => "positive",
            Self::QualifyingNegative => "qualifying_negative",
            Self::NonQualifying => "non_qualifying",
        }
    }
}

/// Which contribution path produced the result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiOrigin {
    #[default]
    ObservedSession,
    GuidedTest,
}

impl ShipWifiOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ObservedSession => "observed_session",
            Self::GuidedTest => "guided_test",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "observed_session" => Self::ObservedSession,
            "guided_test" => Self::GuidedTest,
            _ => return None,
        })
    }
}

/// How many distinct probe directions ran in this session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiDirectionsAttempted {
    #[default]
    None,
    One,
    Both,
}

impl ShipWifiDirectionsAttempted {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::One => "one",
            Self::Both => "both",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "none" => Self::None,
            "one" => Self::One,
            "both" => Self::Both,
            _ => return None,
        })
    }
}

/// Local-network permission as the reducer knows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiLocalPermission {
    Ready,
    Denied,
    #[default]
    Unknown,
}

impl ShipWifiLocalPermission {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Denied => "denied",
            Self::Unknown => "unknown",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "ready" => Self::Ready,
            "denied" => Self::Denied,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// VPN / Private Relay readiness.
///
/// The spec's event list names only [`ShipWifiObservationEvent::VpnInterferenceSuspected`].
/// The artifact also has `user_confirmed_clear`, which cannot be reached from
/// that event alone, so this module accepts the sibling
/// [`ShipWifiObservationEvent::VpnConfirmedClear`]. See the module report
/// for the spec gap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiVpnReadiness {
    UserConfirmedClear,
    InterferenceSuspected,
    #[default]
    Unknown,
}

impl ShipWifiVpnReadiness {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UserConfirmedClear => "user_confirmed_clear",
            Self::InterferenceSuspected => "interference_suspected",
            Self::Unknown => "unknown",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "user_confirmed_clear" => Self::UserConfirmedClear,
            "interference_suspected" => Self::InterferenceSuspected,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// Wi-Fi authorization state for the phones in the observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiAuthorization {
    BothOnboardOnly,
    BothPaid,
    Mixed,
    Unknown,
}

impl ShipWifiAuthorization {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BothOnboardOnly => "both_onboard_only",
            Self::BothPaid => "both_paid",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "both_onboard_only" => Self::BothOnboardOnly,
            "both_paid" => Self::BothPaid,
            "mixed" => Self::Mixed,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// Approximate phone separation. CruiseMesh must not infer this from radio
/// or topology; only the user may set it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiSeparation {
    SameArea,
    DifferentShipAreas,
    Unknown,
}

impl ShipWifiSeparation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SameArea => "same_area",
            Self::DifferentShipAreas => "different_ship_areas",
            Self::Unknown => "unknown",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "same_area" => Self::SameArea,
            "different_ship_areas" => Self::DifferentShipAreas,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// Calendar precision of the observation period.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiPeriodPrecision {
    Month,
    Year,
}

impl ShipWifiPeriodPrecision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "month" => Self::Month,
            "year" => Self::Year,
            _ => return None,
        })
    }
}

/// Reporting client platform. Desktop exists in this repo but is not a v1
/// field-report path; unknown platforms are rejected rather than stored as
/// free text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiPlatform {
    Android,
    Ios,
}

impl ShipWifiPlatform {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Android => "android",
            Self::Ios => "ios",
        }
    }

    fn from_token(token: &str) -> Option<Self> {
        Some(match token {
            "android" => Self::Android,
            "ios" => Self::Ios,
            _ => return None,
        })
    }
}

/// Normalized observation event.
///
/// Payload-bearing variants carry only other closed enums. There is no
/// `String` and no `Vec<u8>` a caller can pass through from the wire. A leak
/// of an address or a contact name would require adding a field.
///
/// `VpnConfirmedClear` is the one addition to the spec's event list. The
/// list names `VpnInterferenceSuspected` but the artifact's `vpn_readiness`
/// field also has `user_confirmed_clear`, and a current-network report (not
/// only a guided test) can carry that value. See the rollout report.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum ShipWifiObservationEvent {
    NetworkJoined,
    NetworkLost,
    LocalPermissionReady,
    LocalPermissionDenied,
    VpnInterferenceSuspected,
    VpnConfirmedClear,
    MdnsBrowseReady,
    MdnsPeerResolved,
    PeerEndpointReceived {
        source: ShipWifiEndpointSource,
    },
    SweepCompleted {
        verdict: ShipWifiSweepVerdict,
    },
    LanAuthenticated {
        discovery_source: ShipWifiDiscoverySource,
    },
    LanProbeSucceeded {
        direction: ShipWifiProbeDirection,
        latency_bucket: ShipWifiLatencyBucket,
    },
    LanProbeFailed {
        direction: ShipWifiProbeDirection,
        failure_class: ShipWifiFailureClass,
    },
    GuidedTestStarted,
    GuidedPeerConfirmedSameShipWifi,
    GuidedTestCompleted,
}

// ---------------------------------------------------------------------------
// Reducer
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct SessionState {
    session_active: bool,
    local_permission: ShipWifiLocalPermission,
    vpn_readiness: ShipWifiVpnReadiness,
    mdns_browse_ready: bool,
    mdns_peer_resolved: bool,
    peer_evidence: bool,
    peer_endpoint_non_lan: bool,
    authenticated_lan: bool,
    discovery_source: ShipWifiDiscoverySource,
    discovery_source_set: bool,
    encrypted_round_trip: bool,
    completed_sweep: ShipWifiCompletedSweep,
    searched: bool,
    session_outbound: bool,
    session_inbound: bool,
    any_policy_denied: bool,
    any_network_lost_failure: bool,
    guided_started: bool,
    guided_same_ship: bool,
    guided_completed: bool,
    guided_outbound_attempts: u32,
    guided_inbound_attempts: u32,
    guided_outbound_timeouts: u32,
    guided_inbound_timeouts: u32,
    guided_any_refused: bool,
}

impl SessionState {
    fn apply(&mut self, event: &ShipWifiObservationEvent) {
        match event {
            ShipWifiObservationEvent::NetworkJoined => {
                *self = Self {
                    session_active: true,
                    ..Self::default()
                };
            }
            ShipWifiObservationEvent::NetworkLost => {
                *self = Self::default();
            }
            _ if !self.session_active => {}
            ShipWifiObservationEvent::LocalPermissionReady => {
                self.local_permission = ShipWifiLocalPermission::Ready;
            }
            ShipWifiObservationEvent::LocalPermissionDenied => {
                self.local_permission = ShipWifiLocalPermission::Denied;
            }
            ShipWifiObservationEvent::VpnInterferenceSuspected => {
                self.vpn_readiness = ShipWifiVpnReadiness::InterferenceSuspected;
            }
            ShipWifiObservationEvent::VpnConfirmedClear => {
                self.vpn_readiness = ShipWifiVpnReadiness::UserConfirmedClear;
            }
            ShipWifiObservationEvent::MdnsBrowseReady => {
                self.mdns_browse_ready = true;
                self.searched = true;
            }
            ShipWifiObservationEvent::MdnsPeerResolved => {
                if self.mdns_browse_ready {
                    self.mdns_peer_resolved = true;
                }
            }
            ShipWifiObservationEvent::PeerEndpointReceived { source } => {
                self.peer_evidence = true;
                if source.is_non_lan() {
                    self.peer_endpoint_non_lan = true;
                }
            }
            ShipWifiObservationEvent::SweepCompleted { verdict } => {
                self.completed_sweep = ShipWifiCompletedSweep::from_sweep(*verdict);
                self.searched = true;
            }
            ShipWifiObservationEvent::LanAuthenticated { discovery_source } => {
                self.authenticated_lan = true;
                self.peer_evidence = true;
                if !self.discovery_source_set {
                    self.discovery_source = *discovery_source;
                    self.discovery_source_set = true;
                }
            }
            ShipWifiObservationEvent::LanProbeSucceeded { direction, .. } => {
                self.encrypted_round_trip = true;
                self.note_direction(*direction);
                self.note_guided_attempt(*direction, None);
            }
            ShipWifiObservationEvent::LanProbeFailed {
                direction,
                failure_class,
            } => {
                if *failure_class == ShipWifiFailureClass::HandshakeUnknownPeer {
                    // Not the accepted contact. Local diagnostics only.
                    return;
                }
                self.searched = true;
                self.note_direction(*direction);
                match failure_class {
                    ShipWifiFailureClass::PolicyDenied => self.any_policy_denied = true,
                    ShipWifiFailureClass::NetworkLost => self.any_network_lost_failure = true,
                    _ => {}
                }
                self.note_guided_attempt(*direction, Some(*failure_class));
            }
            ShipWifiObservationEvent::GuidedTestStarted => {
                self.guided_started = true;
                self.guided_same_ship = false;
                self.guided_completed = false;
                self.guided_outbound_attempts = 0;
                self.guided_inbound_attempts = 0;
                self.guided_outbound_timeouts = 0;
                self.guided_inbound_timeouts = 0;
                self.guided_any_refused = false;
            }
            ShipWifiObservationEvent::GuidedPeerConfirmedSameShipWifi => {
                if self.guided_started {
                    self.guided_same_ship = true;
                }
            }
            ShipWifiObservationEvent::GuidedTestCompleted => {
                if self.guided_started {
                    self.guided_completed = true;
                    // Ephemeral direction slots are the counters above. They
                    // stay as coarse totals for the verdict and are never a
                    // peer identifier. A later GuidedTestStarted wipes them.
                }
            }
        }
    }

    fn note_direction(&mut self, direction: ShipWifiProbeDirection) {
        match direction {
            ShipWifiProbeDirection::Outbound => self.session_outbound = true,
            ShipWifiProbeDirection::Inbound => self.session_inbound = true,
        }
    }

    fn note_guided_attempt(
        &mut self,
        direction: ShipWifiProbeDirection,
        failure: Option<ShipWifiFailureClass>,
    ) {
        if !self.guided_started || self.guided_completed {
            return;
        }
        let (attempts, timeouts) = match direction {
            ShipWifiProbeDirection::Outbound => (
                &mut self.guided_outbound_attempts,
                &mut self.guided_outbound_timeouts,
            ),
            ShipWifiProbeDirection::Inbound => (
                &mut self.guided_inbound_attempts,
                &mut self.guided_inbound_timeouts,
            ),
        };
        *attempts = attempts.saturating_add(1);
        if failure == Some(ShipWifiFailureClass::TimedOut) {
            *timeouts = timeouts.saturating_add(1);
        }
        if failure == Some(ShipWifiFailureClass::Refused) {
            self.guided_any_refused = true;
        }
    }

    fn directions_attempted(&self) -> ShipWifiDirectionsAttempted {
        match (self.session_outbound, self.session_inbound) {
            (false, false) => ShipWifiDirectionsAttempted::None,
            (true, true) => ShipWifiDirectionsAttempted::Both,
            _ => ShipWifiDirectionsAttempted::One,
        }
    }

    fn origin(&self) -> ShipWifiOrigin {
        if self.guided_started {
            ShipWifiOrigin::GuidedTest
        } else {
            ShipWifiOrigin::ObservedSession
        }
    }

    fn discovery_filtered(&self) -> bool {
        self.authenticated_lan
            && self.mdns_browse_ready
            && !self.mdns_peer_resolved
            && self.discovery_source.is_non_mdns_success()
    }

    fn qualifying_negative(&self) -> bool {
        self.guided_started
            && self.guided_completed
            && self.guided_same_ship
            && self.local_permission == ShipWifiLocalPermission::Ready
            && self.vpn_readiness != ShipWifiVpnReadiness::InterferenceSuspected
            && self.peer_endpoint_non_lan
            && !self.authenticated_lan
            && !self.guided_any_refused
            && !self.any_policy_denied
            && !self.any_network_lost_failure
            && self.guided_outbound_timeouts >= QUALIFYING_TIMEOUTS_PER_DIRECTION
            && self.guided_inbound_timeouts >= QUALIFYING_TIMEOUTS_PER_DIRECTION
    }

    fn verdict(&self) -> ShipWifiVerdict {
        if !self.session_active {
            return ShipWifiVerdict::Inconclusive;
        }
        // An authenticated Noise handshake already proves direct client
        // reachability. That beats a later permission or VPN complaint: the
        // path worked. It also beats a failed mDNS browse.
        if self.authenticated_lan {
            if self.discovery_filtered() {
                return ShipWifiVerdict::DiscoveryFilteredDirectWorked;
            }
            return ShipWifiVerdict::DirectConfirmed;
        }
        if self.local_permission == ShipWifiLocalPermission::Denied
            || self.vpn_readiness == ShipWifiVpnReadiness::InterferenceSuspected
            || self.any_policy_denied
            || self.completed_sweep == ShipWifiCompletedSweep::BlockedByPolicy
        {
            return ShipWifiVerdict::OsOrVpnInterference;
        }
        if self.any_network_lost_failure {
            return ShipWifiVerdict::Inconclusive;
        }
        if self.qualifying_negative() {
            return ShipWifiVerdict::LikelyIsolated;
        }
        if self.searched && !self.peer_evidence {
            return ShipWifiVerdict::NoPeerEvidence;
        }
        ShipWifiVerdict::Inconclusive
    }

    fn snapshot(&self) -> ShipWifiEvidenceSnapshot {
        let discovery_source = if self.authenticated_lan {
            self.discovery_source
        } else {
            ShipWifiDiscoverySource::Unknown
        };
        ShipWifiEvidenceSnapshot {
            session_active: self.session_active,
            verdict: self.verdict(),
            origin: self.origin(),
            discovery_source,
            authenticated_lan: self.authenticated_lan,
            encrypted_round_trip: self.encrypted_round_trip,
            directions_attempted: self.directions_attempted(),
            completed_sweep: self.completed_sweep,
            local_permission: self.local_permission,
            vpn_readiness: self.vpn_readiness,
            has_peer_evidence: self.peer_evidence,
            guided_test_completed: self.guided_completed,
        }
    }
}

/// Coarse facts the reducer is willing to admit, plus the derived verdict.
///
/// No timestamps, no endpoints, no identifiers. Strength is not stored here
/// because it depends on the user-supplied separation, which is not an
/// observation.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShipWifiEvidenceSnapshot {
    pub session_active: bool,
    pub verdict: ShipWifiVerdict,
    pub origin: ShipWifiOrigin,
    pub discovery_source: ShipWifiDiscoverySource,
    pub authenticated_lan: bool,
    pub encrypted_round_trip: bool,
    pub directions_attempted: ShipWifiDirectionsAttempted,
    pub completed_sweep: ShipWifiCompletedSweep,
    pub local_permission: ShipWifiLocalPermission,
    pub vpn_readiness: ShipWifiVpnReadiness,
    pub has_peer_evidence: bool,
    pub guided_test_completed: bool,
}

/// In-memory observation session. Process restart is [`Self::new`].
#[derive(uniffi::Object)]
pub struct ShipWifiObservation {
    state: Mutex<SessionState>,
}

#[uniffi::export]
impl ShipWifiObservation {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(SessionState::default()),
        }
    }

    /// Feed one normalized event. Events that arrive before
    /// [`ShipWifiObservationEvent::NetworkJoined`] are ignored so a stale
    /// permission bit from a previous network cannot attach to the next one.
    pub fn observe(&self, event: ShipWifiObservationEvent) {
        lock_state(&self.state).apply(&event);
    }

    /// Drop every coarse fact. Same effect as process restart.
    pub fn reset(&self) {
        *lock_state(&self.state) = SessionState::default();
    }

    pub fn snapshot(&self) -> ShipWifiEvidenceSnapshot {
        lock_state(&self.state).snapshot()
    }

    /// Build a schema-versioned report from the current session and the
    /// user-approved attribution. Does not send anything.
    pub fn build_report(
        &self,
        attribution: ShipWifiReportAttribution,
    ) -> Result<ShipWifiReport, ShipWifiReportError> {
        core_ship_wifi_build_report(self.snapshot(), attribution)
    }
}

impl Default for ShipWifiObservation {
    fn default() -> Self {
        Self::new()
    }
}

/// Fold a complete event sequence into a snapshot. Golden fixtures and both
/// shells should go through this so an Android UniFFI caller and an iOS
/// UniFFI caller cannot disagree.
#[uniffi::export]
pub fn core_ship_wifi_reduce(events: Vec<ShipWifiObservationEvent>) -> ShipWifiEvidenceSnapshot {
    let mut state = SessionState::default();
    for event in &events {
        state.apply(event);
    }
    state.snapshot()
}

/// Evidence strength from the submitted fields plus user-approved separation.
///
/// The service is specified to recompute this and ignore a client label, so
/// this function is the local preview and the test oracle, not a field in
/// the artifact.
#[uniffi::export]
pub fn core_ship_wifi_evidence_strength(
    snapshot: ShipWifiEvidenceSnapshot,
    separation: ShipWifiSeparation,
) -> ShipWifiEvidenceStrength {
    if snapshot.authenticated_lan
        && (snapshot.encrypted_round_trip || separation == ShipWifiSeparation::DifferentShipAreas)
    {
        ShipWifiEvidenceStrength::StrongPositive
    } else if snapshot.authenticated_lan {
        ShipWifiEvidenceStrength::Positive
    } else if snapshot.verdict == ShipWifiVerdict::LikelyIsolated {
        ShipWifiEvidenceStrength::QualifyingNegative
    } else {
        ShipWifiEvidenceStrength::NonQualifying
    }
}

// ---------------------------------------------------------------------------
// Report value types
// ---------------------------------------------------------------------------

/// User-approved identification and client description. The only place a
/// `String` enters the report, and every string is length-limited, scanned
/// for IP/MAC forms, and shown in the preview.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShipWifiReportAttribution {
    pub line_id: Option<String>,
    pub ship_id: Option<String>,
    pub line_other: Option<String>,
    pub ship_other: Option<String>,
    pub period_value: String,
    pub period_precision: ShipWifiPeriodPrecision,
    pub authorization: ShipWifiAuthorization,
    pub separation: ShipWifiSeparation,
    pub platform: ShipWifiPlatform,
    pub os_major: String,
    pub app_version: String,
    pub device_model: Option<String>,
    pub consent_policy_version: u32,
    /// When `None`, a fresh 128-bit nonce is generated. Tests and replayed
    /// drafts pass an explicit value so the artifact is deterministic.
    pub report_nonce: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShipWifiShip {
    pub line_id: Option<String>,
    pub ship_id: Option<String>,
    pub line_other: Option<String>,
    pub ship_other: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShipWifiPeriod {
    pub value: String,
    pub precision: ShipWifiPeriodPrecision,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShipWifiNetworkContext {
    pub authorization: ShipWifiAuthorization,
    pub separation: ShipWifiSeparation,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShipWifiResult {
    pub verdict: ShipWifiVerdict,
    pub origin: ShipWifiOrigin,
    pub discovery_source: ShipWifiDiscoverySource,
    pub authenticated_lan: bool,
    pub encrypted_round_trip: bool,
    pub directions_attempted: ShipWifiDirectionsAttempted,
    pub completed_sweep: ShipWifiCompletedSweep,
    pub local_permission: ShipWifiLocalPermission,
    pub vpn_readiness: ShipWifiVpnReadiness,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShipWifiReportingClient {
    pub platform: ShipWifiPlatform,
    pub os_major: String,
    pub app_version: String,
    pub device_model: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShipWifiConsent {
    pub policy_version: u32,
}

/// Schema-versioned report. This is the object the preview must render; the
/// JSON is a serialization of it, not a parallel summary.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ShipWifiReport {
    pub schema_version: u32,
    pub report_nonce: String,
    pub ship: ShipWifiShip,
    pub period: ShipWifiPeriod,
    pub network_context: ShipWifiNetworkContext,
    pub result: ShipWifiResult,
    pub reporting_client: ShipWifiReportingClient,
    pub consent: ShipWifiConsent,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum ShipWifiReportError {
    #[error("report JSON exceeds 8 KiB")]
    TooLarge,
    #[error("unknown report field '{key}'")]
    UnknownField { key: String },
    #[error("forbidden report field '{key}'")]
    ForbiddenField { key: String },
    #[error("report value looks like a network identifier")]
    NetworkIdentifierInValue,
    #[error("invalid ship Wi-Fi report: {reason}")]
    Invalid { reason: String },
}

impl From<ShipWifiReportError> for CoreError {
    fn from(error: ShipWifiReportError) -> Self {
        CoreError::Malformed(error.to_string())
    }
}

// ---------------------------------------------------------------------------
// Build / serialize / parse
// ---------------------------------------------------------------------------

/// Assemble a report from reducer output and user-approved attribution.
#[uniffi::export]
pub fn core_ship_wifi_build_report(
    snapshot: ShipWifiEvidenceSnapshot,
    attribution: ShipWifiReportAttribution,
) -> Result<ShipWifiReport, ShipWifiReportError> {
    let report_nonce = match attribution.report_nonce {
        Some(nonce) => normalize_nonce(&nonce)?,
        None => core_ship_wifi_generate_nonce(),
    };
    if attribution.consent_policy_version != SHIP_WIFI_CONSENT_POLICY_VERSION {
        return Err(invalid(format!(
            "consent policy_version must be {SHIP_WIFI_CONSENT_POLICY_VERSION}"
        )));
    }
    let report = ShipWifiReport {
        schema_version: SHIP_WIFI_REPORT_SCHEMA_VERSION,
        report_nonce,
        ship: ShipWifiShip {
            line_id: normalize_catalog_id(attribution.line_id, "line_id")?,
            ship_id: normalize_catalog_id(attribution.ship_id, "ship_id")?,
            line_other: normalize_other_name(attribution.line_other, "line_other")?,
            ship_other: normalize_other_name(attribution.ship_other, "ship_other")?,
        },
        period: ShipWifiPeriod {
            value: normalize_period(&attribution.period_value, attribution.period_precision)?,
            precision: attribution.period_precision,
        },
        network_context: ShipWifiNetworkContext {
            authorization: attribution.authorization,
            separation: attribution.separation,
        },
        result: ShipWifiResult {
            verdict: snapshot.verdict,
            origin: snapshot.origin,
            discovery_source: snapshot.discovery_source,
            authenticated_lan: snapshot.authenticated_lan,
            encrypted_round_trip: snapshot.encrypted_round_trip,
            directions_attempted: snapshot.directions_attempted,
            completed_sweep: snapshot.completed_sweep,
            local_permission: snapshot.local_permission,
            vpn_readiness: snapshot.vpn_readiness,
        },
        reporting_client: ShipWifiReportingClient {
            platform: attribution.platform,
            os_major: normalize_short_token(attribution.os_major, "os_major", MAX_OS_MAJOR_CHARS)?,
            app_version: normalize_short_token(
                attribution.app_version,
                "app_version",
                MAX_APP_VERSION_CHARS,
            )?,
            device_model: normalize_device_model(attribution.device_model)?,
        },
        consent: ShipWifiConsent {
            policy_version: attribution.consent_policy_version,
        },
    };
    validate_ship_identification(&report.ship)?;
    // Fail closed on any string that looks like an address, including the
    // ones the user typed into Other / device model.
    scan_report_strings(&report)?;
    Ok(report)
}

/// Canonical pretty-printed JSON, at most 8 KiB, with a trailing newline.
/// `device_model` is omitted when unset, per the spec's production rule.
#[uniffi::export]
pub fn core_ship_wifi_serialize_report(
    report: ShipWifiReport,
) -> Result<String, ShipWifiReportError> {
    scan_report_strings(&report)?;
    let json = canonical_json(&report)?;
    if json.len() > SHIP_WIFI_REPORT_MAX_BYTES {
        return Err(ShipWifiReportError::TooLarge);
    }
    if let Some(key) = first_forbidden_key_in_json(&json) {
        return Err(ShipWifiReportError::ForbiddenField { key });
    }
    if json_strings_look_like_network_ids(&json) {
        return Err(ShipWifiReportError::NetworkIdentifierInValue);
    }
    Ok(json)
}

/// Import a draft. Unknown keys, forbidden keys, and IP/MAC-shaped values
/// are rejected. Accepted drafts are not rewritten: the returned value is
/// what was in the file, and a later serialize only canonicalizes
/// whitespace and key order.
#[uniffi::export]
pub fn core_ship_wifi_parse_report(json: String) -> Result<ShipWifiReport, ShipWifiReportError> {
    if json.len() > SHIP_WIFI_REPORT_MAX_BYTES {
        return Err(ShipWifiReportError::TooLarge);
    }
    let value: Value = serde_json::from_str(&json).map_err(|error| invalid(error.to_string()))?;
    let object = value
        .as_object()
        .ok_or_else(|| invalid("report must be a JSON object"))?;
    reject_closed_schema(object, TOP_LEVEL_KEYS, 0)?;
    if json_strings_look_like_network_ids(&json) {
        return Err(ShipWifiReportError::NetworkIdentifierInValue);
    }
    let schema_version = req_u32(object, "schema_version")?;
    if schema_version != SHIP_WIFI_REPORT_SCHEMA_VERSION {
        return Err(invalid(format!(
            "unsupported schema_version {schema_version}"
        )));
    }
    let report_nonce = normalize_nonce(req_str(object, "report_nonce")?)?;
    let ship = parse_ship(req_object(object, "ship")?)?;
    let period = parse_period(req_object(object, "period")?)?;
    let network_context = parse_network_context(req_object(object, "network_context")?)?;
    let result = parse_result(req_object(object, "result")?)?;
    let reporting_client = parse_reporting_client(req_object(object, "reporting_client")?)?;
    let consent = parse_consent(req_object(object, "consent")?)?;
    let report = ShipWifiReport {
        schema_version,
        report_nonce,
        ship,
        period,
        network_context,
        result,
        reporting_client,
        consent,
    };
    scan_report_strings(&report)?;
    Ok(report)
}

#[uniffi::export]
pub fn core_ship_wifi_generate_nonce() -> String {
    let mut bytes = [0u8; SHIP_WIFI_NONCE_BYTES];
    OsRng.fill_bytes(&mut bytes);
    BASE64URL_NOPAD.encode(&bytes)
}

#[uniffi::export]
pub fn core_ship_wifi_report_max_bytes() -> u32 {
    SHIP_WIFI_REPORT_MAX_BYTES as u32
}

#[uniffi::export]
pub fn core_ship_wifi_report_file_name() -> String {
    SHIP_WIFI_REPORT_FILE_NAME.to_string()
}

#[uniffi::export]
pub fn core_ship_wifi_schema_version() -> u32 {
    SHIP_WIFI_REPORT_SCHEMA_VERSION
}

#[uniffi::export]
pub fn core_ship_wifi_forbidden_keys() -> Vec<String> {
    SHIP_WIFI_FORBIDDEN_KEYS
        .iter()
        .map(|key| (*key).to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Canonical JSON
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct WireReport<'a> {
    schema_version: u32,
    report_nonce: &'a str,
    ship: WireShip<'a>,
    period: WirePeriod<'a>,
    network_context: WireNetworkContext<'a>,
    result: WireResult<'a>,
    reporting_client: WireClient<'a>,
    consent: WireConsent,
}

#[derive(Serialize)]
struct WireShip<'a> {
    line_id: Option<&'a str>,
    ship_id: Option<&'a str>,
    line_other: Option<&'a str>,
    ship_other: Option<&'a str>,
}

#[derive(Serialize)]
struct WirePeriod<'a> {
    value: &'a str,
    precision: &'a str,
}

#[derive(Serialize)]
struct WireNetworkContext<'a> {
    authorization: &'a str,
    separation: &'a str,
}

#[derive(Serialize)]
struct WireResult<'a> {
    verdict: &'a str,
    origin: &'a str,
    discovery_source: &'a str,
    authenticated_lan: bool,
    encrypted_round_trip: bool,
    directions_attempted: &'a str,
    completed_sweep: &'a str,
    local_permission: &'a str,
    vpn_readiness: &'a str,
}

#[derive(Serialize)]
struct WireClient<'a> {
    platform: &'a str,
    os_major: &'a str,
    app_version: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    device_model: Option<&'a str>,
}

#[derive(Serialize)]
struct WireConsent {
    policy_version: u32,
}

fn canonical_json(report: &ShipWifiReport) -> Result<String, ShipWifiReportError> {
    let wire = WireReport {
        schema_version: report.schema_version,
        report_nonce: &report.report_nonce,
        ship: WireShip {
            line_id: report.ship.line_id.as_deref(),
            ship_id: report.ship.ship_id.as_deref(),
            line_other: report.ship.line_other.as_deref(),
            ship_other: report.ship.ship_other.as_deref(),
        },
        period: WirePeriod {
            value: &report.period.value,
            precision: report.period.precision.as_str(),
        },
        network_context: WireNetworkContext {
            authorization: report.network_context.authorization.as_str(),
            separation: report.network_context.separation.as_str(),
        },
        result: WireResult {
            verdict: report.result.verdict.as_str(),
            origin: report.result.origin.as_str(),
            discovery_source: report.result.discovery_source.as_str(),
            authenticated_lan: report.result.authenticated_lan,
            encrypted_round_trip: report.result.encrypted_round_trip,
            directions_attempted: report.result.directions_attempted.as_str(),
            completed_sweep: report.result.completed_sweep.as_str(),
            local_permission: report.result.local_permission.as_str(),
            vpn_readiness: report.result.vpn_readiness.as_str(),
        },
        reporting_client: WireClient {
            platform: report.reporting_client.platform.as_str(),
            os_major: &report.reporting_client.os_major,
            app_version: &report.reporting_client.app_version,
            device_model: report.reporting_client.device_model.as_deref(),
        },
        consent: WireConsent {
            policy_version: report.consent.policy_version,
        },
    };
    let mut json = serde_json::to_string_pretty(&wire)
        .map_err(|error| invalid(format!("serialize: {error}")))?;
    json.push('\n');
    Ok(json)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn invalid(reason: impl Into<String>) -> ShipWifiReportError {
    ShipWifiReportError::Invalid {
        reason: reason.into(),
    }
}

fn validate_ship_identification(ship: &ShipWifiShip) -> Result<(), ShipWifiReportError> {
    let has_line = ship.line_id.is_some() || ship.line_other.is_some();
    let has_ship = ship.ship_id.is_some() || ship.ship_other.is_some();
    if !has_line || !has_ship {
        return Err(invalid(
            "ship identification needs a line and a ship (catalog id or other)",
        ));
    }
    Ok(())
}

fn normalize_nonce(nonce: &str) -> Result<String, ShipWifiReportError> {
    if nonce.len() != SHIP_WIFI_NONCE_CHARS
        || !nonce
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(invalid("report_nonce must be 22-character base64url"));
    }
    let decoded = BASE64URL_NOPAD
        .decode(nonce.as_bytes())
        .map_err(|_| invalid("report_nonce is not valid base64url"))?;
    if decoded.len() != SHIP_WIFI_NONCE_BYTES {
        return Err(invalid("report_nonce must decode to 128 bits"));
    }
    Ok(nonce.to_string())
}

fn normalize_catalog_id(
    value: Option<String>,
    field: &str,
) -> Result<Option<String>, ShipWifiReportError> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let normalized = trimmed.to_ascii_lowercase();
    if normalized.len() > MAX_CATALOG_ID_CHARS {
        return Err(invalid(format!(
            "{field} is longer than {MAX_CATALOG_ID_CHARS} characters"
        )));
    }
    if !is_catalog_slug(&normalized) {
        return Err(invalid(format!("{field} must be a lowercase catalog slug")));
    }
    reject_network_shaped(&normalized)?;
    Ok(Some(normalized))
}

fn is_catalog_slug(value: &str) -> bool {
    let mut start = true;
    for byte in value.bytes() {
        if start {
            if !byte.is_ascii_lowercase() && !byte.is_ascii_digit() {
                return false;
            }
            start = false;
            continue;
        }
        if byte == b'-' {
            start = true;
            continue;
        }
        if !byte.is_ascii_lowercase() && !byte.is_ascii_digit() {
            return false;
        }
    }
    !start && !value.is_empty()
}

fn normalize_other_name(
    value: Option<String>,
    field: &str,
) -> Result<Option<String>, ShipWifiReportError> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_OTHER_NAME_CHARS {
        return Err(invalid(format!(
            "{field} is longer than {MAX_OTHER_NAME_CHARS} characters"
        )));
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        return Err(invalid(format!("{field} contains a control character")));
    }
    reject_network_shaped(trimmed)?;
    Ok(Some(trimmed.to_string()))
}

fn normalize_device_model(value: Option<String>) -> Result<Option<String>, ShipWifiReportError> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().count() > MAX_DEVICE_MODEL_CHARS {
        return Err(invalid(format!(
            "device_model is longer than {MAX_DEVICE_MODEL_CHARS} characters"
        )));
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        return Err(invalid("device_model contains a control character"));
    }
    reject_network_shaped(trimmed)?;
    Ok(Some(trimmed.to_string()))
}

fn normalize_short_token(
    value: String,
    field: &str,
    max_chars: usize,
) -> Result<String, ShipWifiReportError> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(invalid(format!("{field} is required")));
    }
    if trimmed.chars().count() > max_chars {
        return Err(invalid(format!(
            "{field} is longer than {max_chars} characters"
        )));
    }
    if trimmed.chars().any(|ch| ch.is_control()) {
        return Err(invalid(format!("{field} contains a control character")));
    }
    reject_network_shaped(trimmed)?;
    Ok(trimmed.to_string())
}

fn normalize_period(
    value: &str,
    precision: ShipWifiPeriodPrecision,
) -> Result<String, ShipWifiReportError> {
    let trimmed = value.trim();
    let ok = match precision {
        ShipWifiPeriodPrecision::Month => is_year_month(trimmed),
        ShipWifiPeriodPrecision::Year => is_year(trimmed),
    };
    if !ok {
        return Err(invalid(match precision {
            ShipWifiPeriodPrecision::Month => "period value must be YYYY-MM",
            ShipWifiPeriodPrecision::Year => "period value must be YYYY",
        }));
    }
    Ok(trimmed.to_string())
}

fn is_year(value: &str) -> bool {
    value.len() == 4 && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_year_month(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 7 || bytes[4] != b'-' {
        return false;
    }
    if !bytes[..4].iter().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    if !bytes[5].is_ascii_digit() || !bytes[6].is_ascii_digit() {
        return false;
    }
    let month = (bytes[5] - b'0') * 10 + (bytes[6] - b'0');
    (1..=12).contains(&month)
}

fn reject_closed_schema(
    object: &Map<String, Value>,
    allowed: &[&str],
    depth: usize,
) -> Result<(), ShipWifiReportError> {
    if depth > MAX_JSON_DEPTH {
        return Err(invalid("JSON nesting exceeds the closed-schema depth"));
    }
    for (key, value) in object {
        if key_is_forbidden(key) {
            return Err(ShipWifiReportError::ForbiddenField { key: key.clone() });
        }
        if !allowed.contains(&key.as_str()) {
            return Err(ShipWifiReportError::UnknownField { key: key.clone() });
        }
        match value {
            Value::Object(child) => {
                let child_allowed = match key.as_str() {
                    "ship" => SHIP_KEYS,
                    "period" => PERIOD_KEYS,
                    "network_context" => NETWORK_CONTEXT_KEYS,
                    "result" => RESULT_KEYS,
                    "reporting_client" => REPORTING_CLIENT_KEYS,
                    "consent" => CONSENT_KEYS,
                    _ => &[] as &[&str],
                };
                if child_allowed.is_empty() {
                    return Err(invalid(format!("{key} must not be an object")));
                }
                reject_closed_schema(child, child_allowed, depth + 1)?;
            }
            Value::Array(_) => return Err(invalid(format!("{key} must not be an array"))),
            _ => {}
        }
    }
    Ok(())
}

fn key_is_forbidden(key: &str) -> bool {
    key.split(['_', '-', '.']).any(|segment| {
        SHIP_WIFI_FORBIDDEN_KEYS
            .iter()
            .any(|forbidden| segment.eq_ignore_ascii_case(forbidden))
    }) || SHIP_WIFI_FORBIDDEN_KEYS
        .iter()
        .any(|forbidden| key.eq_ignore_ascii_case(forbidden))
}

fn first_forbidden_key_in_json(json: &str) -> Option<String> {
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(json) else {
        return None;
    };
    first_forbidden_key_in_value(&Value::Object(object))
}

fn first_forbidden_key_in_value(value: &Value) -> Option<String> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key_is_forbidden(key) {
                    return Some(key.clone());
                }
                if let Some(found) = first_forbidden_key_in_value(child) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(first_forbidden_key_in_value),
        _ => None,
    }
}

fn scan_report_strings(report: &ShipWifiReport) -> Result<(), ShipWifiReportError> {
    let mut strings = vec![
        report.report_nonce.as_str(),
        report.period.value.as_str(),
        report.reporting_client.os_major.as_str(),
        report.reporting_client.app_version.as_str(),
    ];
    if let Some(value) = report.ship.line_id.as_deref() {
        strings.push(value);
    }
    if let Some(value) = report.ship.ship_id.as_deref() {
        strings.push(value);
    }
    if let Some(value) = report.ship.line_other.as_deref() {
        strings.push(value);
    }
    if let Some(value) = report.ship.ship_other.as_deref() {
        strings.push(value);
    }
    if let Some(value) = report.reporting_client.device_model.as_deref() {
        strings.push(value);
    }
    for value in strings {
        reject_network_shaped(value)?;
    }
    Ok(())
}

fn json_strings_look_like_network_ids(json: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return looks_like_network_identifier(json);
    };
    walk_strings(&value, &mut |text| looks_like_network_identifier(text))
}

fn walk_strings(value: &Value, visit: &mut dyn FnMut(&str) -> bool) -> bool {
    match value {
        Value::String(text) => visit(text),
        Value::Object(object) => object.values().any(|child| walk_strings(child, visit)),
        Value::Array(items) => items.iter().any(|child| walk_strings(child, visit)),
        _ => false,
    }
}

fn reject_network_shaped(value: &str) -> Result<(), ShipWifiReportError> {
    if looks_like_network_identifier(value) {
        Err(ShipWifiReportError::NetworkIdentifierInValue)
    } else {
        Ok(())
    }
}

/// Obvious IPv4, IPv6, and MAC forms. Deliberately ignores dotted triples
/// such as `1.2.0` so an app version cannot trip the gate.
fn looks_like_network_identifier(value: &str) -> bool {
    contains_ipv4(value) || contains_mac(value) || contains_ipv6(value)
}

fn contains_ipv4(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() {
            if let Some(end) = match_ipv4_at(bytes, index) {
                let bounded_left = index == 0 || !bytes[index - 1].is_ascii_digit();
                let bounded_right = end == bytes.len() || !bytes[end].is_ascii_digit();
                if bounded_left && bounded_right {
                    return true;
                }
                index = end;
                continue;
            }
        }
        index += 1;
    }
    false
}

fn match_ipv4_at(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start;
    for part in 0..4 {
        if part > 0 {
            if cursor >= bytes.len() || bytes[cursor] != b'.' {
                return None;
            }
            cursor += 1;
        }
        let octet_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
        let digits = &bytes[octet_start..cursor];
        if digits.is_empty() || digits.len() > 3 {
            return None;
        }
        let value = digits
            .iter()
            .fold(0u16, |acc, digit| acc * 10 + u16::from(digit - b'0'));
        if value > 255 {
            return None;
        }
    }
    Some(cursor)
}

fn contains_mac(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    while index + 16 < bytes.len() {
        let sep = bytes[index + 2];
        if sep != b':' && sep != b'-' {
            index += 1;
            continue;
        }
        if is_mac_at(bytes, index, sep) {
            return true;
        }
        index += 1;
    }
    false
}

fn is_mac_at(bytes: &[u8], start: usize, sep: u8) -> bool {
    let mut cursor = start;
    for part in 0..6 {
        if part > 0 {
            if cursor >= bytes.len() || bytes[cursor] != sep {
                return false;
            }
            cursor += 1;
        }
        if cursor + 1 >= bytes.len() {
            return false;
        }
        if !bytes[cursor].is_ascii_hexdigit() || !bytes[cursor + 1].is_ascii_hexdigit() {
            return false;
        }
        cursor += 2;
    }
    true
}

fn contains_ipv6(value: &str) -> bool {
    for token in value.split(|ch: char| {
        ch.is_ascii_whitespace() || matches!(ch, ',' | ';' | '/' | '(' | ')' | '[' | ']')
    }) {
        if token_looks_like_ipv6(token) {
            return true;
        }
    }
    false
}

fn token_looks_like_ipv6(token: &str) -> bool {
    if !token.contains(':') {
        return false;
    }
    if !token.chars().all(|ch| ch.is_ascii_hexdigit() || ch == ':') {
        return false;
    }
    let colons = token.chars().filter(|ch| *ch == ':').count();
    colons >= 2 && token.chars().any(|ch| ch.is_ascii_hexdigit())
}

fn req_object<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<&'a Map<String, Value>, ShipWifiReportError> {
    match object.get(key) {
        Some(Value::Object(child)) => Ok(child),
        Some(_) => Err(invalid(format!("{key} must be an object"))),
        None => Err(invalid(format!("missing {key}"))),
    }
}

fn req_str<'a>(object: &'a Map<String, Value>, key: &str) -> Result<&'a str, ShipWifiReportError> {
    match object.get(key) {
        Some(Value::String(value)) => Ok(value),
        Some(_) => Err(invalid(format!("{key} must be a string"))),
        None => Err(invalid(format!("missing {key}"))),
    }
}

fn opt_str<'a>(
    object: &'a Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, ShipWifiReportError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.as_str())),
        Some(_) => Err(invalid(format!("{key} must be a string or null"))),
    }
}

fn req_bool(object: &Map<String, Value>, key: &str) -> Result<bool, ShipWifiReportError> {
    match object.get(key) {
        Some(Value::Bool(value)) => Ok(*value),
        Some(_) => Err(invalid(format!("{key} must be a boolean"))),
        None => Err(invalid(format!("missing {key}"))),
    }
}

fn req_u32(object: &Map<String, Value>, key: &str) -> Result<u32, ShipWifiReportError> {
    match object.get(key) {
        Some(Value::Number(number)) => number
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| invalid(format!("{key} is out of range"))),
        Some(_) => Err(invalid(format!("{key} must be a number"))),
        None => Err(invalid(format!("missing {key}"))),
    }
}

fn parse_enum<T>(
    object: &Map<String, Value>,
    key: &str,
    parse: fn(&str) -> Option<T>,
) -> Result<T, ShipWifiReportError> {
    let token = req_str(object, key)?;
    parse(token).ok_or_else(|| invalid(format!("unknown {key} value")))
}

fn parse_ship(object: &Map<String, Value>) -> Result<ShipWifiShip, ShipWifiReportError> {
    let ship = ShipWifiShip {
        line_id: normalize_catalog_id(opt_str(object, "line_id")?.map(str::to_string), "line_id")?,
        ship_id: normalize_catalog_id(opt_str(object, "ship_id")?.map(str::to_string), "ship_id")?,
        line_other: normalize_other_name(
            opt_str(object, "line_other")?.map(str::to_string),
            "line_other",
        )?,
        ship_other: normalize_other_name(
            opt_str(object, "ship_other")?.map(str::to_string),
            "ship_other",
        )?,
    };
    validate_ship_identification(&ship)?;
    Ok(ship)
}

fn parse_period(object: &Map<String, Value>) -> Result<ShipWifiPeriod, ShipWifiReportError> {
    let precision = parse_enum(object, "precision", ShipWifiPeriodPrecision::from_token)?;
    let value = normalize_period(req_str(object, "value")?, precision)?;
    Ok(ShipWifiPeriod { value, precision })
}

fn parse_network_context(
    object: &Map<String, Value>,
) -> Result<ShipWifiNetworkContext, ShipWifiReportError> {
    Ok(ShipWifiNetworkContext {
        authorization: parse_enum(object, "authorization", ShipWifiAuthorization::from_token)?,
        separation: parse_enum(object, "separation", ShipWifiSeparation::from_token)?,
    })
}

fn parse_result(object: &Map<String, Value>) -> Result<ShipWifiResult, ShipWifiReportError> {
    Ok(ShipWifiResult {
        verdict: parse_enum(object, "verdict", ShipWifiVerdict::from_token)?,
        origin: parse_enum(object, "origin", ShipWifiOrigin::from_token)?,
        discovery_source: parse_enum(
            object,
            "discovery_source",
            ShipWifiDiscoverySource::from_token,
        )?,
        authenticated_lan: req_bool(object, "authenticated_lan")?,
        encrypted_round_trip: req_bool(object, "encrypted_round_trip")?,
        directions_attempted: parse_enum(
            object,
            "directions_attempted",
            ShipWifiDirectionsAttempted::from_token,
        )?,
        completed_sweep: parse_enum(
            object,
            "completed_sweep",
            ShipWifiCompletedSweep::from_token,
        )?,
        local_permission: parse_enum(
            object,
            "local_permission",
            ShipWifiLocalPermission::from_token,
        )?,
        vpn_readiness: parse_enum(object, "vpn_readiness", ShipWifiVpnReadiness::from_token)?,
    })
}

fn parse_reporting_client(
    object: &Map<String, Value>,
) -> Result<ShipWifiReportingClient, ShipWifiReportError> {
    Ok(ShipWifiReportingClient {
        platform: parse_enum(object, "platform", ShipWifiPlatform::from_token)?,
        os_major: normalize_short_token(
            req_str(object, "os_major")?.to_string(),
            "os_major",
            MAX_OS_MAJOR_CHARS,
        )?,
        app_version: normalize_short_token(
            req_str(object, "app_version")?.to_string(),
            "app_version",
            MAX_APP_VERSION_CHARS,
        )?,
        device_model: normalize_device_model(opt_str(object, "device_model")?.map(str::to_string))?,
    })
}

fn parse_consent(object: &Map<String, Value>) -> Result<ShipWifiConsent, ShipWifiReportError> {
    let policy_version = req_u32(object, "policy_version")?;
    if policy_version != SHIP_WIFI_CONSENT_POLICY_VERSION {
        return Err(invalid(format!(
            "consent policy_version must be {SHIP_WIFI_CONSENT_POLICY_VERSION}"
        )));
    }
    Ok(ShipWifiConsent { policy_version })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Map, Value};

    const FIXTURE_NONCE: &str = "AAAAAAAAAAAAAAAAAAAAAA";

    fn attribution() -> ShipWifiReportAttribution {
        ShipWifiReportAttribution {
            line_id: Some("norwegian-cruise-line".into()),
            ship_id: Some("norwegian-jade".into()),
            line_other: None,
            ship_other: None,
            period_value: "2026-05".into(),
            period_precision: ShipWifiPeriodPrecision::Month,
            authorization: ShipWifiAuthorization::BothOnboardOnly,
            separation: ShipWifiSeparation::SameArea,
            platform: ShipWifiPlatform::Android,
            os_major: "16".into(),
            app_version: "1.2.0".into(),
            device_model: None,
            consent_policy_version: SHIP_WIFI_CONSENT_POLICY_VERSION,
            report_nonce: Some(FIXTURE_NONCE.into()),
        }
    }

    fn reduce(events: &[ShipWifiObservationEvent]) -> ShipWifiEvidenceSnapshot {
        core_ship_wifi_reduce(events.to_vec())
    }

    fn strength(
        snapshot: &ShipWifiEvidenceSnapshot,
        separation: ShipWifiSeparation,
    ) -> ShipWifiEvidenceStrength {
        core_ship_wifi_evidence_strength(snapshot.clone(), separation)
    }

    fn timeout(direction: ShipWifiProbeDirection) -> ShipWifiObservationEvent {
        ShipWifiObservationEvent::LanProbeFailed {
            direction,
            failure_class: ShipWifiFailureClass::TimedOut,
        }
    }

    fn probe_ok(direction: ShipWifiProbeDirection) -> ShipWifiObservationEvent {
        ShipWifiObservationEvent::LanProbeSucceeded {
            direction,
            latency_bucket: ShipWifiLatencyBucket::Under100ms,
        }
    }

    /// Permissive guest LAN: mDNS finds the peer and an authenticated
    /// handshake plus encrypted probe both succeed.
    fn permissive_events() -> Vec<ShipWifiObservationEvent> {
        vec![
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
            ShipWifiObservationEvent::VpnConfirmedClear,
            ShipWifiObservationEvent::MdnsBrowseReady,
            ShipWifiObservationEvent::MdnsPeerResolved,
            ShipWifiObservationEvent::LanAuthenticated {
                discovery_source: ShipWifiDiscoverySource::Mdns,
            },
            probe_ok(ShipWifiProbeDirection::Outbound),
        ]
    }

    /// Client-isolated LAN: a guided two-phone test with a known peer
    /// exchanged over BLE, both directions timing out twice.
    fn client_isolated_events() -> Vec<ShipWifiObservationEvent> {
        vec![
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
            ShipWifiObservationEvent::VpnConfirmedClear,
            ShipWifiObservationEvent::PeerEndpointReceived {
                source: ShipWifiEndpointSource::Ble,
            },
            ShipWifiObservationEvent::SweepCompleted {
                verdict: ShipWifiSweepVerdict::IsolationSuspected,
            },
            ShipWifiObservationEvent::GuidedTestStarted,
            ShipWifiObservationEvent::GuidedPeerConfirmedSameShipWifi,
            timeout(ShipWifiProbeDirection::Outbound),
            timeout(ShipWifiProbeDirection::Outbound),
            timeout(ShipWifiProbeDirection::Inbound),
            timeout(ShipWifiProbeDirection::Inbound),
            ShipWifiObservationEvent::GuidedTestCompleted,
        ]
    }

    /// Searched the LAN (browse + all-silent sweep) with no accepted peer.
    fn no_peer_events() -> Vec<ShipWifiObservationEvent> {
        vec![
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
            ShipWifiObservationEvent::VpnConfirmedClear,
            ShipWifiObservationEvent::MdnsBrowseReady,
            ShipWifiObservationEvent::SweepCompleted {
                verdict: ShipWifiSweepVerdict::IsolationSuspected,
            },
        ]
    }

    /// VPN / Private Relay intercepts local traffic before a meaningful test.
    fn vpn_denied_events() -> Vec<ShipWifiObservationEvent> {
        vec![
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
            ShipWifiObservationEvent::VpnInterferenceSuspected,
            ShipWifiObservationEvent::MdnsBrowseReady,
        ]
    }

    /// Multicast/Bonjour filtered: browse never resolves, but an
    /// authenticated non-mDNS endpoint still completes a LAN handshake.
    fn multicast_filtered_events() -> Vec<ShipWifiObservationEvent> {
        vec![
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
            ShipWifiObservationEvent::VpnConfirmedClear,
            ShipWifiObservationEvent::MdnsBrowseReady,
            ShipWifiObservationEvent::PeerEndpointReceived {
                source: ShipWifiEndpointSource::Ble,
            },
            ShipWifiObservationEvent::LanAuthenticated {
                discovery_source: ShipWifiDiscoverySource::AuthenticatedEndpoint,
            },
            probe_ok(ShipWifiProbeDirection::Outbound),
        ]
    }

    fn collect_keys(value: &Value, keys: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    keys.push(key.clone());
                    collect_keys(child, keys);
                }
            }
            Value::Array(items) => {
                for item in items {
                    collect_keys(item, keys);
                }
            }
            _ => {}
        }
    }

    fn key_matches_denylist(key: &str) -> Option<&'static str> {
        SHIP_WIFI_FORBIDDEN_KEYS.iter().copied().find(|forbidden| {
            key.eq_ignore_ascii_case(forbidden)
                || key
                    .split(['_', '-', '.'])
                    .any(|segment| segment.eq_ignore_ascii_case(forbidden))
        })
    }

    fn insert_top_level_key(json: &str, key: &str) -> String {
        let mut root: Map<String, Value> = serde_json::from_str(json).expect("fixture json");
        root.insert(key.to_string(), json!("should-never-export"));
        serde_json::to_string(&Value::Object(root)).expect("rewrite")
    }

    fn serialize_fixture(events: &[ShipWifiObservationEvent]) -> (ShipWifiReport, String) {
        let snapshot = reduce(events);
        let report = core_ship_wifi_build_report(snapshot, attribution()).expect("build");
        let json = core_ship_wifi_serialize_report(report.clone()).expect("serialize");
        (report, json)
    }

    #[test]
    fn a_permissive_network_fixture_is_direct_confirmed_with_strong_positive_evidence() {
        let snapshot = reduce(&permissive_events());
        assert!(snapshot.session_active);
        assert_eq!(snapshot.verdict, ShipWifiVerdict::DirectConfirmed);
        assert_eq!(snapshot.origin, ShipWifiOrigin::ObservedSession);
        assert_eq!(snapshot.discovery_source, ShipWifiDiscoverySource::Mdns);
        assert!(snapshot.authenticated_lan);
        assert!(snapshot.encrypted_round_trip);
        assert_eq!(snapshot.local_permission, ShipWifiLocalPermission::Ready);
        assert_eq!(
            strength(&snapshot, ShipWifiSeparation::SameArea),
            ShipWifiEvidenceStrength::StrongPositive
        );
    }

    #[test]
    fn a_client_isolated_guided_test_fixture_is_likely_isolated_with_qualifying_negative_evidence()
    {
        let snapshot = reduce(&client_isolated_events());
        assert_eq!(snapshot.verdict, ShipWifiVerdict::LikelyIsolated);
        assert_eq!(snapshot.origin, ShipWifiOrigin::GuidedTest);
        assert!(!snapshot.authenticated_lan);
        assert!(!snapshot.encrypted_round_trip);
        assert_eq!(
            snapshot.directions_attempted,
            ShipWifiDirectionsAttempted::Both
        );
        assert_eq!(snapshot.completed_sweep, ShipWifiCompletedSweep::AllSilent);
        assert!(snapshot.has_peer_evidence);
        assert!(snapshot.guided_test_completed);
        assert_eq!(
            strength(&snapshot, ShipWifiSeparation::SameArea),
            ShipWifiEvidenceStrength::QualifyingNegative
        );
    }

    #[test]
    fn a_no_peer_sweep_fixture_is_no_peer_evidence_and_non_qualifying() {
        let snapshot = reduce(&no_peer_events());
        assert_eq!(snapshot.verdict, ShipWifiVerdict::NoPeerEvidence);
        assert!(!snapshot.has_peer_evidence);
        assert_eq!(snapshot.completed_sweep, ShipWifiCompletedSweep::AllSilent);
        assert_eq!(snapshot.discovery_source, ShipWifiDiscoverySource::Unknown);
        assert_eq!(
            strength(&snapshot, ShipWifiSeparation::SameArea),
            ShipWifiEvidenceStrength::NonQualifying
        );
    }

    #[test]
    fn a_vpn_denied_fixture_is_os_or_vpn_interference_and_non_qualifying() {
        let snapshot = reduce(&vpn_denied_events());
        assert_eq!(snapshot.verdict, ShipWifiVerdict::OsOrVpnInterference);
        assert_eq!(
            snapshot.vpn_readiness,
            ShipWifiVpnReadiness::InterferenceSuspected
        );
        assert!(!snapshot.authenticated_lan);
        assert_eq!(
            strength(&snapshot, ShipWifiSeparation::SameArea),
            ShipWifiEvidenceStrength::NonQualifying
        );
    }

    #[test]
    fn a_multicast_filtered_fixture_is_discovery_filtered_direct_worked_with_strong_positive_evidence(
    ) {
        let snapshot = reduce(&multicast_filtered_events());
        assert_eq!(
            snapshot.verdict,
            ShipWifiVerdict::DiscoveryFilteredDirectWorked
        );
        assert_eq!(
            snapshot.discovery_source,
            ShipWifiDiscoverySource::AuthenticatedEndpoint
        );
        assert!(snapshot.authenticated_lan);
        assert!(snapshot.encrypted_round_trip);
        assert_eq!(
            strength(&snapshot, ShipWifiSeparation::SameArea),
            ShipWifiEvidenceStrength::StrongPositive
        );
    }

    #[test]
    fn an_authenticated_lan_link_stays_positive_even_when_mdns_never_resolved() {
        let snapshot = reduce(&[
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
            ShipWifiObservationEvent::MdnsBrowseReady,
            ShipWifiObservationEvent::LanAuthenticated {
                discovery_source: ShipWifiDiscoverySource::BoundedSweep,
            },
        ]);
        assert_eq!(
            snapshot.verdict,
            ShipWifiVerdict::DiscoveryFilteredDirectWorked
        );
        assert_eq!(
            strength(&snapshot, ShipWifiSeparation::SameArea),
            ShipWifiEvidenceStrength::Positive
        );
        assert_eq!(
            strength(&snapshot, ShipWifiSeparation::DifferentShipAreas),
            ShipWifiEvidenceStrength::StrongPositive
        );
    }

    #[test]
    fn policy_denial_is_os_or_vpn_interference_never_likely_isolated() {
        let snapshot = reduce(&[
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
            ShipWifiObservationEvent::PeerEndpointReceived {
                source: ShipWifiEndpointSource::Ble,
            },
            ShipWifiObservationEvent::LanProbeFailed {
                direction: ShipWifiProbeDirection::Outbound,
                failure_class: ShipWifiFailureClass::PolicyDenied,
            },
        ]);
        assert_eq!(snapshot.verdict, ShipWifiVerdict::OsOrVpnInterference);
        assert_ne!(snapshot.verdict, ShipWifiVerdict::LikelyIsolated);
    }

    #[test]
    fn local_permission_denied_is_os_or_vpn_interference() {
        let snapshot = reduce(&[
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionDenied,
            ShipWifiObservationEvent::MdnsBrowseReady,
        ]);
        assert_eq!(snapshot.verdict, ShipWifiVerdict::OsOrVpnInterference);
    }

    #[test]
    fn a_blocked_by_policy_sweep_is_os_or_vpn_interference() {
        let snapshot = reduce(&[
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::SweepCompleted {
                verdict: ShipWifiSweepVerdict::BlockedByPolicy,
            },
        ]);
        assert_eq!(snapshot.verdict, ShipWifiVerdict::OsOrVpnInterference);
        assert_eq!(
            snapshot.completed_sweep,
            ShipWifiCompletedSweep::BlockedByPolicy
        );
    }

    #[test]
    fn refused_ports_alone_never_yield_likely_isolated() {
        let snapshot = reduce(&[
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
            ShipWifiObservationEvent::VpnConfirmedClear,
            ShipWifiObservationEvent::PeerEndpointReceived {
                source: ShipWifiEndpointSource::Ble,
            },
            ShipWifiObservationEvent::GuidedTestStarted,
            ShipWifiObservationEvent::GuidedPeerConfirmedSameShipWifi,
            ShipWifiObservationEvent::LanProbeFailed {
                direction: ShipWifiProbeDirection::Outbound,
                failure_class: ShipWifiFailureClass::Refused,
            },
            ShipWifiObservationEvent::LanProbeFailed {
                direction: ShipWifiProbeDirection::Outbound,
                failure_class: ShipWifiFailureClass::Refused,
            },
            ShipWifiObservationEvent::LanProbeFailed {
                direction: ShipWifiProbeDirection::Inbound,
                failure_class: ShipWifiFailureClass::Refused,
            },
            ShipWifiObservationEvent::LanProbeFailed {
                direction: ShipWifiProbeDirection::Inbound,
                failure_class: ShipWifiFailureClass::Refused,
            },
            ShipWifiObservationEvent::GuidedTestCompleted,
        ]);
        assert_ne!(snapshot.verdict, ShipWifiVerdict::LikelyIsolated);
        assert_eq!(snapshot.verdict, ShipWifiVerdict::Inconclusive);
        assert_eq!(
            strength(&snapshot, ShipWifiSeparation::SameArea),
            ShipWifiEvidenceStrength::NonQualifying
        );
    }

    #[test]
    fn handshake_unknown_peer_never_contributes_to_a_compatibility_verdict() {
        let snapshot = reduce(&[
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
            ShipWifiObservationEvent::LanProbeFailed {
                direction: ShipWifiProbeDirection::Outbound,
                failure_class: ShipWifiFailureClass::HandshakeUnknownPeer,
            },
        ]);
        assert_eq!(snapshot.verdict, ShipWifiVerdict::Inconclusive);
        assert_eq!(
            snapshot.directions_attempted,
            ShipWifiDirectionsAttempted::None
        );
        assert!(!snapshot.has_peer_evidence);
    }

    #[test]
    fn network_loss_clears_generic_observation_evidence() {
        let mut events = permissive_events();
        events.push(ShipWifiObservationEvent::NetworkLost);
        let snapshot = reduce(&events);
        assert!(!snapshot.session_active);
        assert_eq!(snapshot.verdict, ShipWifiVerdict::Inconclusive);
        assert!(!snapshot.authenticated_lan);
        assert!(!snapshot.has_peer_evidence);
    }

    #[test]
    fn events_before_network_joined_do_not_attach_to_the_next_session() {
        let snapshot = reduce(&[
            ShipWifiObservationEvent::LocalPermissionDenied,
            ShipWifiObservationEvent::VpnInterferenceSuspected,
            ShipWifiObservationEvent::LanAuthenticated {
                discovery_source: ShipWifiDiscoverySource::Mdns,
            },
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
        ]);
        assert_eq!(snapshot.verdict, ShipWifiVerdict::Inconclusive);
        assert!(!snapshot.authenticated_lan);
        assert_eq!(snapshot.local_permission, ShipWifiLocalPermission::Ready);
        assert_eq!(snapshot.vpn_readiness, ShipWifiVpnReadiness::Unknown);
    }

    #[test]
    fn an_incomplete_guided_test_is_not_a_qualifying_negative() {
        let snapshot = reduce(&[
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
            ShipWifiObservationEvent::VpnConfirmedClear,
            ShipWifiObservationEvent::PeerEndpointReceived {
                source: ShipWifiEndpointSource::Ble,
            },
            ShipWifiObservationEvent::GuidedTestStarted,
            ShipWifiObservationEvent::GuidedPeerConfirmedSameShipWifi,
            timeout(ShipWifiProbeDirection::Outbound),
            timeout(ShipWifiProbeDirection::Outbound),
            timeout(ShipWifiProbeDirection::Inbound),
            timeout(ShipWifiProbeDirection::Inbound),
            // GuidedTestCompleted omitted on purpose.
        ]);
        assert_ne!(snapshot.verdict, ShipWifiVerdict::LikelyIsolated);
        assert!(!snapshot.guided_test_completed);
        assert_eq!(
            strength(&snapshot, ShipWifiSeparation::SameArea),
            ShipWifiEvidenceStrength::NonQualifying
        );
    }

    #[test]
    fn one_timeout_per_direction_is_not_enough_for_likely_isolated() {
        let snapshot = reduce(&[
            ShipWifiObservationEvent::NetworkJoined,
            ShipWifiObservationEvent::LocalPermissionReady,
            ShipWifiObservationEvent::VpnConfirmedClear,
            ShipWifiObservationEvent::PeerEndpointReceived {
                source: ShipWifiEndpointSource::Ble,
            },
            ShipWifiObservationEvent::GuidedTestStarted,
            ShipWifiObservationEvent::GuidedPeerConfirmedSameShipWifi,
            timeout(ShipWifiProbeDirection::Outbound),
            timeout(ShipWifiProbeDirection::Inbound),
            ShipWifiObservationEvent::GuidedTestCompleted,
        ]);
        assert_eq!(snapshot.verdict, ShipWifiVerdict::Inconclusive);
    }

    #[test]
    fn serialized_golden_reports_contain_none_of_the_modules_forbidden_keys() {
        let fixtures = [
            permissive_events(),
            client_isolated_events(),
            no_peer_events(),
            vpn_denied_events(),
            multicast_filtered_events(),
        ];
        let denylist = core_ship_wifi_forbidden_keys();
        assert_eq!(denylist.len(), SHIP_WIFI_FORBIDDEN_KEYS.len());
        for forbidden in SHIP_WIFI_FORBIDDEN_KEYS {
            assert!(
                denylist.iter().any(|key| key == forbidden),
                "exported denylist drifted from SHIP_WIFI_FORBIDDEN_KEYS: {forbidden}"
            );
        }

        for events in fixtures {
            let (_report, json) = serialize_fixture(&events);
            let value: Value = serde_json::from_str(&json).expect("canonical json");
            let mut keys = Vec::new();
            collect_keys(&value, &mut keys);
            for key in &keys {
                if let Some(forbidden) = key_matches_denylist(key) {
                    panic!("serialized key {key:?} matches forbidden {forbidden:?}");
                }
            }
            for forbidden in SHIP_WIFI_FORBIDDEN_KEYS {
                assert!(
                    !keys.iter().any(|key| key.eq_ignore_ascii_case(forbidden)),
                    "forbidden key {forbidden:?} appeared in serialized output"
                );
            }
        }
    }

    #[test]
    fn closed_schema_validation_rejects_a_draft_containing_any_forbidden_key() {
        let (_report, json) = serialize_fixture(&permissive_events());
        for forbidden in SHIP_WIFI_FORBIDDEN_KEYS {
            let draft = insert_top_level_key(&json, forbidden);
            let error =
                core_ship_wifi_parse_report(draft).expect_err("forbidden key must be rejected");
            match error {
                ShipWifiReportError::ForbiddenField { key } => {
                    assert!(
                        key.eq_ignore_ascii_case(forbidden),
                        "rejected {key:?}, expected {forbidden:?}"
                    );
                }
                other => panic!("expected ForbiddenField for {forbidden:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_compound_key_that_hides_a_forbidden_segment_is_rejected() {
        let (_report, json) = serialize_fixture(&permissive_events());
        let draft = insert_top_level_key(&json, "peer_ip");
        let error = core_ship_wifi_parse_report(draft).expect_err("peer_ip is ip");
        assert!(matches!(
            error,
            ShipWifiReportError::ForbiddenField { ref key } if key == "peer_ip"
        ));
    }

    #[test]
    fn an_ip_or_mac_shaped_string_cannot_enter_serialized_output() {
        let snapshot = reduce(&permissive_events());
        let mut poisoned = attribution();
        poisoned.line_other = Some("gateway 192.168.1.1".into());
        poisoned.line_id = None;
        let error = core_ship_wifi_build_report(snapshot.clone(), poisoned)
            .expect_err("IPv4 in line_other");
        assert!(matches!(
            error,
            ShipWifiReportError::NetworkIdentifierInValue
        ));

        let mut mac = attribution();
        mac.device_model = Some("aa:bb:cc:dd:ee:ff".into());
        let error =
            core_ship_wifi_build_report(snapshot.clone(), mac).expect_err("MAC in device_model");
        assert!(matches!(
            error,
            ShipWifiReportError::NetworkIdentifierInValue
        ));

        let mut ipv6 = attribution();
        ipv6.ship_other = Some("fe80::1".into());
        ipv6.ship_id = None;
        let error = core_ship_wifi_build_report(snapshot, ipv6).expect_err("IPv6 in ship_other");
        assert!(matches!(
            error,
            ShipWifiReportError::NetworkIdentifierInValue
        ));
    }

    #[test]
    fn a_dotted_app_version_is_not_treated_as_a_network_identifier() {
        let snapshot = reduce(&permissive_events());
        let mut attr = attribution();
        attr.app_version = "1.2.0".into();
        let report = core_ship_wifi_build_report(snapshot, attr).expect("app version 1.2.0");
        let json = core_ship_wifi_serialize_report(report).expect("serialize");
        assert!(json.contains("\"app_version\": \"1.2.0\""));
    }

    #[test]
    fn a_serialized_report_round_trips_through_parse() {
        for events in [
            permissive_events(),
            client_isolated_events(),
            no_peer_events(),
            vpn_denied_events(),
            multicast_filtered_events(),
        ] {
            let (report, json) = serialize_fixture(&events);
            let parsed = core_ship_wifi_parse_report(json).expect("parse");
            assert_eq!(parsed, report);
        }
    }

    #[test]
    fn an_optional_device_model_round_trips_and_is_omitted_when_unset() {
        let snapshot = reduce(&permissive_events());
        let without = core_ship_wifi_build_report(snapshot.clone(), attribution()).unwrap();
        let json_without = core_ship_wifi_serialize_report(without.clone()).unwrap();
        assert!(
            !json_without.contains("device_model"),
            "unset device_model must be omitted, not null"
        );
        assert_eq!(core_ship_wifi_parse_report(json_without).unwrap(), without);

        let mut attr = attribution();
        attr.device_model = Some("Pixel 6".into());
        let with = core_ship_wifi_build_report(snapshot, attr).unwrap();
        let json_with = core_ship_wifi_serialize_report(with.clone()).unwrap();
        assert!(json_with.contains("\"device_model\": \"Pixel 6\""));
        assert_eq!(core_ship_wifi_parse_report(json_with).unwrap(), with);
    }

    #[test]
    fn parse_rejects_malformed_json_and_a_non_object_root() {
        for draft in ["", "{", "[]", "\"report\"", "null"] {
            let error = core_ship_wifi_parse_report(draft.into()).expect_err(draft);
            assert!(
                matches!(error, ShipWifiReportError::Invalid { .. }),
                "{draft:?} => {error:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_an_unknown_top_level_key() {
        let (_report, json) = serialize_fixture(&permissive_events());
        let draft = insert_top_level_key(&json, "telemetry");
        let error = core_ship_wifi_parse_report(draft).expect_err("unknown key");
        assert!(matches!(
            error,
            ShipWifiReportError::UnknownField { ref key } if key == "telemetry"
        ));
    }

    #[test]
    fn parse_rejects_an_unknown_nested_key() {
        let (_report, json) = serialize_fixture(&permissive_events());
        let mut root: Map<String, Value> = serde_json::from_str(&json).unwrap();
        root.get_mut("result")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("rtt_ms".into(), json!(12));
        let draft = serde_json::to_string(&Value::Object(root)).unwrap();
        let error = core_ship_wifi_parse_report(draft).expect_err("unknown result key");
        assert!(matches!(
            error,
            ShipWifiReportError::UnknownField { ref key } if key == "rtt_ms"
        ));
    }

    #[test]
    fn parse_rejects_an_unsupported_schema_version_and_unknown_enum() {
        let (_report, json) = serialize_fixture(&permissive_events());
        let mut root: Map<String, Value> = serde_json::from_str(&json).unwrap();
        root.insert("schema_version".into(), json!(2));
        let draft = serde_json::to_string(&Value::Object(root)).unwrap();
        let error = core_ship_wifi_parse_report(draft).expect_err("schema 2");
        assert!(matches!(error, ShipWifiReportError::Invalid { .. }));

        let mut root: Map<String, Value> = serde_json::from_str(&json).unwrap();
        root.get_mut("result")
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("verdict".into(), json!("totally_isolated"));
        let draft = serde_json::to_string(&Value::Object(root)).unwrap();
        let error = core_ship_wifi_parse_report(draft).expect_err("unknown verdict");
        assert!(matches!(error, ShipWifiReportError::Invalid { .. }));
    }

    #[test]
    fn parse_rejects_a_missing_required_object() {
        let (_report, json) = serialize_fixture(&permissive_events());
        let mut root: Map<String, Value> = serde_json::from_str(&json).unwrap();
        root.remove("consent");
        let draft = serde_json::to_string(&Value::Object(root)).unwrap();
        let error = core_ship_wifi_parse_report(draft).expect_err("missing consent");
        assert!(matches!(error, ShipWifiReportError::Invalid { .. }));
    }

    #[test]
    fn a_report_over_eight_kib_is_rejected_on_import() {
        assert_eq!(SHIP_WIFI_REPORT_MAX_BYTES, 8 * 1024);
        assert_eq!(
            core_ship_wifi_report_max_bytes(),
            SHIP_WIFI_REPORT_MAX_BYTES as u32
        );
        let oversized = " ".repeat(SHIP_WIFI_REPORT_MAX_BYTES + 1);
        let error = core_ship_wifi_parse_report(oversized).expect_err("oversize");
        assert!(matches!(error, ShipWifiReportError::TooLarge));

        let at_cap = " ".repeat(SHIP_WIFI_REPORT_MAX_BYTES);
        let error = core_ship_wifi_parse_report(at_cap).expect_err("spaces are not a report");
        assert!(
            !matches!(error, ShipWifiReportError::TooLarge),
            "exactly 8 KiB must pass the size gate and fail as invalid JSON"
        );
    }

    #[test]
    fn a_canonical_report_stays_under_the_standalone_size_cap() {
        let mut attr = attribution();
        attr.line_other = Some("x".repeat(MAX_OTHER_NAME_CHARS));
        attr.ship_other = Some("y".repeat(MAX_OTHER_NAME_CHARS));
        attr.line_id = None;
        attr.ship_id = None;
        attr.device_model = Some("z".repeat(MAX_DEVICE_MODEL_CHARS));
        attr.os_major = "1".repeat(MAX_OS_MAJOR_CHARS);
        attr.app_version = "2".repeat(MAX_APP_VERSION_CHARS);
        let report =
            core_ship_wifi_build_report(reduce(&permissive_events()), attr).expect("max strings");
        let json = core_ship_wifi_serialize_report(report).expect("serialize");
        assert!(
            json.len() <= SHIP_WIFI_REPORT_MAX_BYTES,
            "max-filled report is {} bytes",
            json.len()
        );
    }

    #[test]
    fn period_precision_accepts_month_and_year_forms_and_rejects_the_wrong_grain() {
        let snapshot = reduce(&permissive_events());

        let mut month = attribution();
        month.period_precision = ShipWifiPeriodPrecision::Month;
        month.period_value = "2026-05".into();
        let report = core_ship_wifi_build_report(snapshot.clone(), month).unwrap();
        assert_eq!(report.period.value, "2026-05");
        assert_eq!(report.period.precision, ShipWifiPeriodPrecision::Month);

        let mut year = attribution();
        year.period_precision = ShipWifiPeriodPrecision::Year;
        year.period_value = "2026".into();
        let report = core_ship_wifi_build_report(snapshot.clone(), year).unwrap();
        assert_eq!(report.period.value, "2026");
        assert_eq!(report.period.precision, ShipWifiPeriodPrecision::Year);

        let mut month_as_year = attribution();
        month_as_year.period_precision = ShipWifiPeriodPrecision::Year;
        month_as_year.period_value = "2026-05".into();
        assert!(core_ship_wifi_build_report(snapshot.clone(), month_as_year).is_err());

        let mut year_as_month = attribution();
        year_as_month.period_precision = ShipWifiPeriodPrecision::Month;
        year_as_month.period_value = "2026".into();
        assert!(core_ship_wifi_build_report(snapshot.clone(), year_as_month).is_err());

        for bad in ["2026-00", "2026-13", "2026-5", "26-05", "202605"] {
            let mut attr = attribution();
            attr.period_value = bad.into();
            assert!(
                core_ship_wifi_build_report(snapshot.clone(), attr).is_err(),
                "period {bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn observation_snapshot_agrees_with_the_pure_reducer() {
        let events = multicast_filtered_events();
        let reduced = reduce(&events);
        let session = ShipWifiObservation::new();
        for event in events {
            session.observe(event);
        }
        assert_eq!(session.snapshot(), reduced);
        session.reset();
        assert_eq!(session.snapshot(), ShipWifiObservation::new().snapshot());
        assert!(!session.snapshot().session_active);
    }

    #[test]
    fn share_sheet_file_name_and_schema_constants_match_the_phase_zero_contract() {
        assert_eq!(
            core_ship_wifi_report_file_name(),
            "cruisemesh-ship-wifi-report-v1.json"
        );
        assert_eq!(core_ship_wifi_schema_version(), 1);
        assert_eq!(SHIP_WIFI_CONSENT_POLICY_VERSION, 1);
    }
}
