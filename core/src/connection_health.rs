//! One answer to "can this phone connect right now?", and one answer to
//! "where does each friend belong on the page?".
//!
//! Both shells already hold every fact needed to answer those questions --
//! mesh runtime state, live direct paths, our own Shore Pass health, relay
//! presence, per-contact card rejections -- and both shells have historically
//! turned those facts into an interpretation *twice*, in Kotlin and in Swift,
//! with the two drifting apart. The in-chat status pill and the connection
//! details page have disagreed in the field for exactly that reason.
//!
//! So the interpretation lives here. The shells hand over the facts, get back
//! a state plus a structured evidence descriptor, and render. Nothing in this
//! module produces user-facing text: copy belongs in `strings.xml` and
//! `Localizable.xcstrings`, where the localization gate can see it. The
//! records below carry counts, path facts, reasons, and actions as enums --
//! never sentences.
//!
//! Two rules shape the whole module:
//!
//! * **A friend's problem is never this device's problem.** The health
//!   classification takes no per-person input at all, which is a structural
//!   guarantee that one friend's rejected card can never turn the overall card
//!   red. That inversion is what made the previous page contradict itself.
//! * **No connection at all is not a fault.** A phone with Bluetooth
//!   listening and nobody nearby is working exactly as designed; a phone with
//!   no Shore Pass is on the free default. Neither is dressed up as a problem
//!   here, because a warning that fires during normal use teaches people to
//!   ignore warnings.
//!
//! See `specs/connection-details-page.md` for the surface these feed.

use crate::contact_relay_health::{
    core_contact_relay_endpoint_usable, core_contact_relay_is_stale,
    core_contact_relay_unreachable_endpoint_usable, core_contact_relay_unreachable_is_stale,
};

/// How long an unresolved check may hold the card in
/// [`CoreConnectionHealth::Checking`] before it must resolve to the
/// best-supported real state.
///
/// Checking is a transition, never a resting state. Ten seconds is long
/// enough that an ordinary relay round trip or a radio coming up finishes
/// inside it, and short enough that a person who opened the page *because*
/// something felt wrong is not left watching a spinner instead of being told
/// what is actually known.
pub const CONNECTION_CHECKING_TIMEOUT_MS: i64 = 10_000;

/// How recently a friend's relay presence must have been seen for them to
/// count as reachable over Shore Pass right now.
///
/// 2.5x the 60 s relay poll cadence: "their phone is actively syncing", with
/// enough slack that one missed poll does not blink them offline. Both shells
/// already carry this number (`ContactReachability.PRESENCE_ONLINE_WINDOW_MS`
/// / `ContactReachability.presenceOnlineWindowMs`); it is stated here so the
/// grouping on this page and the reachability tiers elsewhere can converge on
/// a single definition rather than three copies of one constant.
pub const CONNECTION_PRESENCE_ONLINE_WINDOW_MS: i64 = 150_000;

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// Whether the mesh service itself is running.
///
/// Mirrors Android's `MeshRuntimeState` and the iOS equivalent, minus their
/// display labels -- the labels are shell resources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreMeshRuntime {
    /// Not running: nothing can be sent or received.
    Stopped,
    /// Coming up. No verdict on anything yet.
    Starting,
    /// Running with its radios in hand.
    Active,
    /// Running, but Bluetooth is off, so its BLE roles carry nothing. Treated
    /// exactly like [`CoreDirectPathState::Off`] on the Bluetooth path.
    BluetoothOff,
}

/// Availability of one of *this phone's* direct radio paths.
///
/// "Available" is about the radio, not about company: Bluetooth with nobody
/// in range is available and listening, which is the normal state of a phone
/// in a cabin at night.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreDirectPathState {
    /// Radio off, or the permission to use it was refused.
    Off,
    /// Coming up; no verdict yet.
    Starting,
    /// Up and able to carry a message the moment someone is in range.
    Available,
}

/// State of *this phone's* Shore Pass path.
///
/// One variant per row the Paths section can show. `Message too large` is
/// deliberately absent: an oversized envelope is a fact about one message and
/// one recipient, not about whether this phone can reach the service, and
/// putting it here is how the old page told people their pass was broken when
/// it was fine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreRelayPathState {
    /// No pass saved. The free default -- nearby delivery still works, and
    /// this is never a fault.
    NotSetUp,
    /// A pass is saved and the first check has not answered yet. Shells must
    /// only report this when no settled verdict exists; a routine background
    /// sync pass must keep reporting the last verdict, or the card flickers
    /// through Checking every minute.
    Checking,
    /// The service answered and the pass is good.
    Connected,
    /// The pass is fine; this phone has no validated internet right now.
    /// Expected at sea, and never an error.
    WaitingForInternet,
    /// We have internet and the service did not answer. Transient.
    Unreachable,
    /// Our pass has lapsed.
    PassExpired,
    /// Our pass was turned off by the operator.
    PassSuspended,
    /// Our own saved setup was rejected (HTTP 401/403 on our own token).
    SetupRejected,
    /// Our family's hosted storage is full.
    StorageFull,
    /// The shared family limit slowed syncing (HTTP 429). Recovers on its own
    /// and must never be presented as something to act on.
    SyncingSlowed,
}

/// Everything the overall health classification consumes.
///
/// Deliberately contains no per-person data. See the module note: the overall
/// card describes *this device's* ability to take part, so one friend's
/// broken card cannot reach it even by mistake.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreConnectionHealthInput {
    pub runtime: CoreMeshRuntime,
    pub bluetooth: CoreDirectPathState,
    /// Live direct Bluetooth links right now.
    pub bluetooth_links: u32,
    pub local_wifi: CoreDirectPathState,
    /// Live direct local Wi-Fi links right now.
    pub local_wifi_links: u32,
    pub relay: CoreRelayPathState,
    /// A network with validated internet is available for relay traffic.
    pub validated_internet: bool,
    /// Friends (not strangers) reachable over a live direct link right now.
    /// Zero is an ordinary, healthy number.
    pub nearby_friend_count: u32,
    /// When the current unresolved check started, epoch ms; `0` when nothing
    /// is pending. Used only to bound [`CoreConnectionHealth::Checking`].
    pub checking_since_ms: i64,
    pub now_ms: i64,
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

/// The overall interpretation shown by the connection health card -- and, once
/// the shells are pointed at it, by the in-chat status pill, so the two can
/// never disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreConnectionHealth {
    /// Startup or an active check, with no verdict yet. Bounded by
    /// [`CONNECTION_CHECKING_TIMEOUT_MS`].
    Checking,
    /// Running with at least one path able to carry a message, and nothing
    /// degraded.
    Ready,
    /// Running with at least one useful path, but another expected path is
    /// unavailable or temporarily degraded.
    Limited,
    /// A person needs to do something, or nothing can carry a message at all.
    NeedsAttention,
}

/// Why the state is not [`CoreConnectionHealth::Ready`].
///
/// One reason, not a list: the card shows a single evidence line, and ranking
/// the reasons here is what keeps both shells picking the same one. Raw
/// per-fault detail stays in diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreHealthReason {
    /// The mesh service is not running.
    MeshStopped,
    /// Bluetooth is off, so this phone cannot meet nearby phones.
    BluetoothOff,
    /// Our pass was turned off.
    PassSuspended,
    /// Our pass has lapsed.
    PassExpired,
    /// Our own saved setup was rejected.
    OwnSetupRejected,
    /// Our family's hosted storage is full.
    StorageFull,
    /// We have internet and the service did not answer.
    ShorePassUnreachable,
    /// No validated internet, so the Shore Pass path is resting.
    WaitingForInternet,
    /// The shared family limit slowed syncing; recovers by itself.
    ShorePassSlowed,
    /// Nothing can carry a message right now, and no more specific reason
    /// applies (for example, a radio stuck coming up).
    NoPathAvailable,
}

/// The single action the card may offer. `None` means the app has nothing
/// honest to offer and should say only what is true.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreHealthAction {
    StartMesh,
    TurnOnBluetooth,
    ManageShorePass,
    HowToFix,
}

/// The facts behind the state, for the shell to render into its own copy.
///
/// Counts and enums only. Path fields carry the *normalized* states -- for
/// instance a relay reporting `Connected` with no validated internet comes
/// back as [`CoreRelayPathState::WaitingForInternet`] -- so the Paths rows and
/// the health card are rendered from one consistent set of facts and cannot
/// contradict each other on screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreConnectionEvidence {
    pub nearby_friend_count: u32,
    pub bluetooth: CoreDirectPathState,
    pub bluetooth_links: u32,
    pub local_wifi: CoreDirectPathState,
    pub local_wifi_links: u32,
    pub relay: CoreRelayPathState,
    /// At least one path could carry a message right now.
    pub any_path_usable: bool,
    /// This phone's own Shore Pass path is usable for delivery right now.
    /// Feeds the relay half of the reachable-now test (see
    /// [`core_person_reach`]).
    pub own_relay_usable: bool,
}

/// The whole answer: one state, its evidence, and at most one action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreConnectionHealthReport {
    pub state: CoreConnectionHealth,
    pub evidence: CoreConnectionEvidence,
    pub reason: Option<CoreHealthReason>,
    pub action: Option<CoreHealthAction>,
}

// ---------------------------------------------------------------------------
// Health classification
// ---------------------------------------------------------------------------

/// Has an unresolved check outlived [`CONNECTION_CHECKING_TIMEOUT_MS`]?
///
/// `checking_since_ms <= 0` (nothing recorded) and a mark in the future (the
/// clock moved backwards under us) both answer `true`. Both directions
/// deliberately resolve rather than wait: the fallback is showing the
/// best-supported real state, which is never worse than an endless spinner,
/// whereas the other default can pin the card in Checking forever.
#[uniffi::export]
pub fn core_connection_checking_expired(checking_since_ms: i64, now_ms: i64) -> bool {
    if checking_since_ms <= 0 || now_ms < checking_since_ms {
        return true;
    }
    now_ms - checking_since_ms >= CONNECTION_CHECKING_TIMEOUT_MS
}

/// The relay state after reconciling it with what the network layer says.
///
/// A saved pass that last synced fine still cannot deliver anything while the
/// phone has no validated internet, and reporting `Connected` there is how the
/// old page managed to claim Shore Pass was connected on a phone that had been
/// offline for an hour. A `Checking` that has run out its bound resolves to
/// the honest fallback rather than staying unresolved.
fn normalized_relay(
    input: &CoreConnectionHealthInput,
    checking_expired: bool,
) -> CoreRelayPathState {
    match input.relay {
        CoreRelayPathState::NotSetUp => CoreRelayPathState::NotSetUp,
        CoreRelayPathState::Checking => {
            if !checking_expired {
                CoreRelayPathState::Checking
            } else if !input.validated_internet {
                CoreRelayPathState::WaitingForInternet
            } else {
                CoreRelayPathState::Unreachable
            }
        }
        CoreRelayPathState::Connected | CoreRelayPathState::SyncingSlowed
            if !input.validated_internet =>
        {
            CoreRelayPathState::WaitingForInternet
        }
        other => other,
    }
}

/// Is this relay state one a person has to act on?
fn relay_is_actionable(relay: CoreRelayPathState) -> bool {
    matches!(
        relay,
        CoreRelayPathState::PassExpired
            | CoreRelayPathState::PassSuspended
            | CoreRelayPathState::SetupRejected
            | CoreRelayPathState::StorageFull
    )
}

/// Is this relay state working, if imperfectly, right now?
///
/// `SyncingSlowed` is included on purpose: a 429 means messages are still
/// moving, just more slowly, and calling that "no path" would push the card to
/// NeedsAttention for a condition that clears itself.
fn relay_can_deliver(relay: CoreRelayPathState) -> bool {
    matches!(
        relay,
        CoreRelayPathState::Connected | CoreRelayPathState::SyncingSlowed
    )
}

/// Is this relay state a degradation worth reporting?
///
/// `NotSetUp` is not: no pass is the free default. `Checking` is not: it is
/// the absence of an answer, not an answer.
fn relay_is_degraded(relay: CoreRelayPathState) -> bool {
    match relay {
        CoreRelayPathState::NotSetUp | CoreRelayPathState::Checking => false,
        CoreRelayPathState::Connected => false,
        CoreRelayPathState::WaitingForInternet
        | CoreRelayPathState::Unreachable
        | CoreRelayPathState::SyncingSlowed => true,
        CoreRelayPathState::PassExpired
        | CoreRelayPathState::PassSuspended
        | CoreRelayPathState::SetupRejected
        | CoreRelayPathState::StorageFull => true,
    }
}

/// The one reason to show, most actionable first, and the action that goes
/// with it.
///
/// Bluetooth outranks every pass fault because turning the radio back on is a
/// one-tap fix that restores the path this product is built on, while a pass
/// problem usually needs a website and a card. The pass faults themselves are
/// ranked by how completely they stop delivery.
fn reason_and_action(
    runtime: CoreMeshRuntime,
    bluetooth_off: bool,
    relay: CoreRelayPathState,
    any_path_usable: bool,
) -> Option<(CoreHealthReason, Option<CoreHealthAction>)> {
    if runtime == CoreMeshRuntime::Stopped {
        return Some((
            CoreHealthReason::MeshStopped,
            Some(CoreHealthAction::StartMesh),
        ));
    }
    if bluetooth_off {
        return Some((
            CoreHealthReason::BluetoothOff,
            Some(CoreHealthAction::TurnOnBluetooth),
        ));
    }
    let relay_reason = match relay {
        CoreRelayPathState::PassSuspended => Some((
            CoreHealthReason::PassSuspended,
            Some(CoreHealthAction::ManageShorePass),
        )),
        CoreRelayPathState::PassExpired => Some((
            CoreHealthReason::PassExpired,
            Some(CoreHealthAction::ManageShorePass),
        )),
        CoreRelayPathState::SetupRejected => Some((
            CoreHealthReason::OwnSetupRejected,
            Some(CoreHealthAction::HowToFix),
        )),
        CoreRelayPathState::StorageFull => Some((
            CoreHealthReason::StorageFull,
            Some(CoreHealthAction::HowToFix),
        )),
        CoreRelayPathState::Unreachable => Some((CoreHealthReason::ShorePassUnreachable, None)),
        CoreRelayPathState::WaitingForInternet => {
            Some((CoreHealthReason::WaitingForInternet, None))
        }
        CoreRelayPathState::SyncingSlowed => Some((CoreHealthReason::ShorePassSlowed, None)),
        CoreRelayPathState::NotSetUp
        | CoreRelayPathState::Checking
        | CoreRelayPathState::Connected => None,
    };
    if relay_reason.is_some() {
        return relay_reason;
    }
    if !any_path_usable {
        return Some((CoreHealthReason::NoPathAvailable, None));
    }
    None
}

/// Is some path still coming up, with no verdict on it yet?
///
/// Exported because both shells have to answer the same question *before* they
/// call [`core_classify_connection_health`] -- they own the clock that records
/// when the wait began, and the classification only bounds a wait it is told
/// about. Answering it in Kotlin and again in Swift is how iOS came to omit
/// the two radio cases and show `Needs attention` while its Bluetooth stack
/// was still answering. One definition, used by the classifier itself below,
/// makes that class of drift impossible.
#[uniffi::export]
pub fn core_connection_check_pending(
    runtime: CoreMeshRuntime,
    bluetooth: CoreDirectPathState,
    local_wifi: CoreDirectPathState,
    relay: CoreRelayPathState,
) -> bool {
    runtime == CoreMeshRuntime::Starting
        || bluetooth == CoreDirectPathState::Starting
        || local_wifi == CoreDirectPathState::Starting
        || relay == CoreRelayPathState::Checking
}

/// Classify this device's overall connection health.
///
/// The order of the decision matters and is the specification's, not an
/// implementation detail:
///
/// 1. A stopped service is the one condition that beats everything, because
///    nothing else is even being attempted.
/// 2. Startup, and any state where nothing is usable *yet* while some path is
///    still coming up, reports Checking -- but only inside
///    [`CONNECTION_CHECKING_TIMEOUT_MS`]. A failure is never displayed before
///    the check that would prove it has finished or run out.
/// 3. With nothing able to carry a message, the state is NeedsAttention.
/// 4. With something able to carry a message but an expected path missing or
///    degraded, the state is Limited.
/// 5. Otherwise Ready -- including with no friends nearby and no pass saved,
///    which are ordinary conditions and not faults.
#[uniffi::export]
pub fn core_classify_connection_health(
    input: CoreConnectionHealthInput,
) -> CoreConnectionHealthReport {
    let checking_expired = core_connection_checking_expired(input.checking_since_ms, input.now_ms);
    let relay = normalized_relay(&input, checking_expired);
    let bluetooth_off = input.bluetooth == CoreDirectPathState::Off
        || input.runtime == CoreMeshRuntime::BluetoothOff;
    let bluetooth = if bluetooth_off {
        CoreDirectPathState::Off
    } else {
        input.bluetooth
    };
    let stopped = input.runtime == CoreMeshRuntime::Stopped;

    // A stopped service owns no radios, so nothing below it is true.
    let bluetooth_usable = !stopped && bluetooth == CoreDirectPathState::Available;
    let local_wifi_usable = !stopped && input.local_wifi == CoreDirectPathState::Available;
    let own_relay_usable = !stopped && relay_can_deliver(relay);
    let any_path_usable = bluetooth_usable || local_wifi_usable || own_relay_usable;

    let evidence = CoreConnectionEvidence {
        nearby_friend_count: input.nearby_friend_count,
        bluetooth,
        bluetooth_links: input.bluetooth_links,
        local_wifi: input.local_wifi,
        local_wifi_links: input.local_wifi_links,
        relay,
        any_path_usable,
        own_relay_usable,
    };

    let pending = core_connection_check_pending(input.runtime, bluetooth, input.local_wifi, relay);

    if !stopped
        && !checking_expired
        && (input.runtime == CoreMeshRuntime::Starting || (!any_path_usable && pending))
    {
        return CoreConnectionHealthReport {
            state: CoreConnectionHealth::Checking,
            evidence,
            reason: None,
            action: None,
        };
    }

    let (reason, action) =
        match reason_and_action(input.runtime, bluetooth_off, relay, any_path_usable) {
            Some((reason, action)) => (Some(reason), action),
            None => (None, None),
        };

    let degraded = bluetooth_off
        || relay_is_degraded(relay)
        || relay_is_actionable(relay)
        || reason == Some(CoreHealthReason::NoPathAvailable);

    let state = if stopped || !any_path_usable {
        CoreConnectionHealth::NeedsAttention
    } else if degraded {
        CoreConnectionHealth::Limited
    } else {
        CoreConnectionHealth::Ready
    };

    // Ready is Ready: no leftover reason, no leftover action.
    let (reason, action) = if state == CoreConnectionHealth::Ready {
        (None, None)
    } else {
        (reason, action)
    };

    CoreConnectionHealthReport {
        state,
        evidence,
        reason,
        action,
    }
}

// ---------------------------------------------------------------------------
// People
// ---------------------------------------------------------------------------

/// Which direct radio a live link to a person uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreDirectLink {
    Bluetooth,
    LocalWifi,
}

/// Why a person needs the user's attention.
///
/// Produced by [`core_classify_recipient_delivery`] from the per-recipient
/// read model, so a person's placement in the Needs attention group and the
/// delivery line in their row are always the same verdict rather than two
/// judgements that can disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CorePersonAttention {
    /// A usable route exists but nothing has progressed for the delayed
    /// window. The mildest reason: it often clears itself.
    Delayed,
    /// A queued message exceeds the size cap and can never post as-is.
    MessageTooLarge,
    /// Our own pass cannot post on their behalf: expired, suspended, our own
    /// saved setup rejected, or the family's storage is full.
    PassBlocked,
    /// Their saved Shore Pass setup was rejected -- their friend card points
    /// at somewhere that will not serve them. The most severe, because it
    /// needs the *friend* to act first and nothing on this phone can fix it.
    SetupRejected,
}

/// How a person can be reached at this moment.
///
/// This is a statement about now, not a prediction. A person with none of
/// these is not broken -- store-and-forward delivery is what the product does,
/// and waiting is the expected case.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CorePersonReach {
    /// Live direct link over Bluetooth.
    DirectBluetooth,
    /// Live direct link over local Wi-Fi.
    DirectLocalWifi,
    /// No live link, but their relay presence is fresh and our own Shore Pass
    /// path works.
    RelayPresence,
    /// Not reachable right now.
    None,
}

/// The three groups the People section renders, in page order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CorePersonGroup {
    NeedsAttention,
    ReachableNow,
    OtherPeople,
}

/// One person's facts, as the shell already holds them.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CorePersonHealthInput {
    pub user_id: Vec<u8>,
    /// The name the shell will render. Used only as a sort key; it is never
    /// echoed back.
    pub display_name: String,
    /// This identity carries a block tombstone (`MessageStore::is_user_blocked`
    /// / the cached blocked set). A blocked person is dropped by
    /// [`core_group_people`] before any grouping happens.
    pub blocked: bool,
    /// A live direct link right now, and which radio it uses.
    pub direct_link: Option<CoreDirectLink>,
    /// Relay presence last-seen, epoch ms; `0` when never seen.
    pub presence_last_seen_ms: i64,
    /// Freshest evidence of any kind that their device was alive (HELLO,
    /// message, receipt, presence), epoch ms; `0` when there is no history.
    pub last_seen_ms: i64,
    /// Why they need attention, when they do.
    pub attention: Option<CorePersonAttention>,
    /// Timestamp of the oldest affected user-visible message, epoch ms; `0`
    /// when unknown. Orders the Needs attention group after severity.
    pub attention_since_ms: i64,
}

/// Where one person lands, and what the row should say about reachability.
///
/// Carries the user id rather than the name: the shell already has the name,
/// and this record travels through logs and tests where a name should not.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CorePersonPlacement {
    pub user_id: Vec<u8>,
    pub group: CorePersonGroup,
    pub reach: CorePersonReach,
    pub attention: Option<CorePersonAttention>,
}

/// The People section, grouped and ordered. Blocked identities appear in
/// none of these lists.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CorePeopleGroups {
    pub needs_attention: Vec<CorePersonPlacement>,
    pub reachable_now: Vec<CorePersonPlacement>,
    pub other_people: Vec<CorePersonPlacement>,
}

/// Severity order inside the Needs attention group; higher shows first.
#[uniffi::export]
pub fn core_person_attention_rank(attention: CorePersonAttention) -> u8 {
    match attention {
        CorePersonAttention::SetupRejected => 4,
        CorePersonAttention::PassBlocked => 3,
        CorePersonAttention::MessageTooLarge => 2,
        CorePersonAttention::Delayed => 1,
    }
}

/// How a person is reachable right now.
///
/// A live direct link is reachability by observation. Relay presence is
/// reachability by inference, and the inference only holds while *our* own
/// Shore Pass path can actually deliver -- knowing their phone synced two
/// minutes ago is useless if this phone cannot post anything. That
/// conjunction is the whole point: it is what stops the page promising
/// delivery over a path this device does not have.
///
/// `own_relay_usable` comes from
/// [`CoreConnectionEvidence::own_relay_usable`], so the People section and
/// the health card cannot disagree about whether Shore Pass works.
#[uniffi::export]
pub fn core_person_reach(
    direct_link: Option<CoreDirectLink>,
    presence_last_seen_ms: i64,
    own_relay_usable: bool,
    now_ms: i64,
) -> CorePersonReach {
    match direct_link {
        Some(CoreDirectLink::Bluetooth) => return CorePersonReach::DirectBluetooth,
        Some(CoreDirectLink::LocalWifi) => return CorePersonReach::DirectLocalWifi,
        None => {}
    }
    if own_relay_usable
        && presence_last_seen_ms > 0
        && now_ms >= presence_last_seen_ms
        && now_ms - presence_last_seen_ms <= CONNECTION_PRESENCE_ONLINE_WINDOW_MS
    {
        return CorePersonReach::RelayPresence;
    }
    // A presence stamp in the future is a clock artifact, not evidence. It
    // reads as "seen 0 minutes ago" forever, which would pin someone in
    // Reachable now long after they left.
    CorePersonReach::None
}

/// Is this person reachable right now, by the page's definition?
///
/// Exported separately because the answer is useful on its own (a chat header,
/// a badge) and must not be re-derived from [`CorePersonReach`] by hand in two
/// languages.
#[uniffi::export]
pub fn core_person_is_reachable_now(reach: CorePersonReach) -> bool {
    reach != CorePersonReach::None
}

/// The freshest useful evidence for a person, for Other people's ordering.
fn newest_evidence_ms(person: &CorePersonHealthInput) -> i64 {
    person.last_seen_ms.max(person.presence_last_seen_ms).max(0)
}

/// Group and order every person for the People section.
///
/// Blocked identities are removed first, before anything else looks at them.
/// That is structural on purpose: a block is a tombstone, and a UI-side filter
/// applied after grouping is one forgotten call site away from putting a
/// blocked person back on the screen.
///
/// Ordering, per group:
///
/// * **Needs attention**: severity ([`core_person_attention_rank`]), then the
///   oldest affected message first, then name.
/// * **Reachable now**: name.
/// * **Other people**: freshest evidence first, with people who have no
///   history at all last, then name.
///
/// Names sort case-insensitively; ties break on user id so the order is
/// stable across reloads rather than shuffling under the reader.
#[uniffi::export]
pub fn core_group_people(
    people: Vec<CorePersonHealthInput>,
    own_relay_usable: bool,
    now_ms: i64,
) -> CorePeopleGroups {
    let mut needs_attention: Vec<(&CorePersonHealthInput, CorePersonPlacement)> = Vec::new();
    let mut reachable_now: Vec<(&CorePersonHealthInput, CorePersonPlacement)> = Vec::new();
    let mut other_people: Vec<(&CorePersonHealthInput, CorePersonPlacement)> = Vec::new();

    for person in people.iter().filter(|person| !person.blocked) {
        let reach = core_person_reach(
            person.direct_link,
            person.presence_last_seen_ms,
            own_relay_usable,
            now_ms,
        );
        let group = if person.attention.is_some() {
            CorePersonGroup::NeedsAttention
        } else if core_person_is_reachable_now(reach) {
            CorePersonGroup::ReachableNow
        } else {
            CorePersonGroup::OtherPeople
        };
        let placement = CorePersonPlacement {
            user_id: person.user_id.clone(),
            group,
            reach,
            attention: person.attention,
        };
        match group {
            CorePersonGroup::NeedsAttention => needs_attention.push((person, placement)),
            CorePersonGroup::ReachableNow => reachable_now.push((person, placement)),
            CorePersonGroup::OtherPeople => other_people.push((person, placement)),
        }
    }

    needs_attention.sort_by(|(left, left_place), (right, right_place)| {
        let left_rank = left.attention.map(core_person_attention_rank).unwrap_or(0);
        let right_rank = right.attention.map(core_person_attention_rank).unwrap_or(0);
        right_rank
            .cmp(&left_rank)
            // Unknown ages (0) sort last, so a row with a real age always wins.
            .then_with(|| {
                let left_age = if left.attention_since_ms > 0 {
                    left.attention_since_ms
                } else {
                    i64::MAX
                };
                let right_age = if right.attention_since_ms > 0 {
                    right.attention_since_ms
                } else {
                    i64::MAX
                };
                left_age.cmp(&right_age)
            })
            .then_with(|| name_order(left, right))
            .then_with(|| left_place.user_id.cmp(&right_place.user_id))
    });

    reachable_now.sort_by(|(left, left_place), (right, right_place)| {
        name_order(left, right).then_with(|| left_place.user_id.cmp(&right_place.user_id))
    });

    other_people.sort_by(|(left, left_place), (right, right_place)| {
        newest_evidence_ms(right)
            .cmp(&newest_evidence_ms(left))
            .then_with(|| name_order(left, right))
            .then_with(|| left_place.user_id.cmp(&right_place.user_id))
    });

    CorePeopleGroups {
        needs_attention: needs_attention.into_iter().map(|(_, p)| p).collect(),
        reachable_now: reachable_now.into_iter().map(|(_, p)| p).collect(),
        other_people: other_people.into_iter().map(|(_, p)| p).collect(),
    }
}

fn name_order(left: &CorePersonHealthInput, right: &CorePersonHealthInput) -> std::cmp::Ordering {
    left.display_name
        .to_lowercase()
        .cmp(&right.display_name.to_lowercase())
}

// ---------------------------------------------------------------------------
// Delivery language
// ---------------------------------------------------------------------------

/// How long a usable route may carry no progress before the waiting work is
/// called out as delayed.
///
/// Ten minutes, and it is a single named constant here precisely so field
/// evidence can move it without either shell being touched. The number only
/// has meaning while a route is usable: a friend who is simply elsewhere may
/// stay queued for days without anything being wrong, and no threshold applies
/// to them at any age.
///
/// Ten is chosen against the relay's own rhythm -- a sync pass runs about once
/// a minute -- so reaching it means roughly ten consecutive passes moved
/// nothing. Shorter would fire during an ordinary lift ride; much longer and a
/// genuinely stuck conversation stays silent past the point a person has
/// already noticed and started doubting the app.
pub const RELAY_DELIVERY_DELAYED_THRESHOLD_MS: i64 = 10 * 60 * 1000;

/// The user-visible *movement* meaning of messages still waiting for one
/// person: what happens to them next, on the evidence available.
///
/// Deliberately has no error member, and does not gain one in Phase 2. This
/// answers "where is this work going", and the answer for a friend who is
/// merely elsewhere is "it travels at the next encounter" whatever else is
/// wrong -- store-and-forward through encounters is what the product does.
/// Faults are carried alongside it as overlays on [`CoreDeliveryLine`], so a
/// fault can explain why the *internet* route is stopped without ever turning
/// the promise underneath into a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreDeliveryState {
    /// A route to this person is usable now.
    Sending,
    /// No route right now; the work travels at the next encounter.
    WillDeliverWhenReconnected,
    /// Shore Pass is the only known route and this phone has no internet.
    WaitingForInternet,
}

/// A terminal or configuration fault stopping the internet route to one
/// person, and therefore the reason an error row can offer `How to fix`.
///
/// Each variant exists because it maps to different, concrete instructions
/// (see the specification's "How to fix" section). A fault with no distinct
/// remedy would not earn a variant -- it would just be a longer sentence
/// describing the same button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CoreDeliveryBlockedReason {
    /// *Their* saved Shore Pass setup will not serve us: it authoritatively
    /// rejected our credential, or the host in their friend card has gone
    /// unanswered long enough to be actionable rather than transient.
    ///
    /// Both land here because the repair is identical and order-sensitive: the
    /// friend fixes their own pass first, *then* shares a fresh card, which is
    /// then rescanned. A card re-shared before the pass is fixed reproduces
    /// the problem exactly.
    ContactSetupRejected,
    /// Our own pass has lapsed. Repaired under Manage Shore Pass.
    PassExpired,
    /// Our own pass was turned off by the operator.
    PassSuspended,
    /// Our family's hosted storage is full. It frees itself as friends collect
    /// their messages, which is the first thing the instructions must say.
    StorageFull,
    /// Our own saved setup was rejected. Same affordance as an expired pass,
    /// separate variant because the explanation differs.
    OwnSetupRejected,
    /// A waiting message is larger than any transport will carry. The sealed
    /// ceiling is enforced identically by the relay and by peer framing, so
    /// this is terminal on every path, not merely on the internet one -- and
    /// no amount of reconnecting will change it.
    MessageTooLarge,
}

/// Everything the page says about one person's waiting mail: where it is
/// going, whether it has stalled, what is stopping it, and where that puts
/// them in the People grouping.
///
/// The three verdict fields are layered, not alternatives, and render in this
/// precedence: `blocked_reason`, then `delayed`, then `state`. Keeping `state`
/// truthful underneath a fault is the point -- an expired pass stops the
/// internet route, but the messages really will still go the moment the friend
/// is nearby, and a page that replaced that promise with "can't be sent" would
/// be lying about the one behaviour this product exists for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreDeliveryLine {
    /// User-visible messages the line is about. Never zero: no waiting work
    /// means no line at all.
    pub count: u32,
    /// Where this work is going.
    pub state: CoreDeliveryState,
    /// A usable route exists, this device still has work it has not handed
    /// over, and nothing has progressed for
    /// [`RELAY_DELIVERY_DELAYED_THRESHOLD_MS`].
    ///
    /// All three, not just the last two: mail already accepted for a friend
    /// who has not collected it is the product working, at ten minutes and at
    /// ten days.
    pub delayed: bool,
    /// A terminal or configuration fault stops the internet route.
    pub blocked_reason: Option<CoreDeliveryBlockedReason>,
    /// Where this person belongs in the People grouping; `None` leaves them
    /// wherever their reachability puts them.
    pub attention: Option<CorePersonAttention>,
    /// When the oldest affected message started waiting, epoch ms; `0` when
    /// unknown. Dates the delayed line and orders Needs attention.
    ///
    /// This device's queue time, passed through from
    /// [`crate::CoreRecipientDeliveryStatus::oldest_waiting_ms`]. Deliberately
    /// not the message's displayed timestamp: causal ordering floors an
    /// authored timestamp above the whole chat, so a peer with a fast clock
    /// could drag ours past `now` and suppress the line entirely.
    pub oldest_waiting_ms: i64,
}

/// Everything the per-person delivery line consumes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreDeliveryLineInput {
    /// Rows still awaiting *relay upload* for this recipient, straight from
    /// the diagnostic relay-depth query. See
    /// [`core_relay_queue_reflects_delivery`] for why this number is only
    /// sometimes evidence about delivery.
    pub queued: u32,
    /// This phone's own Shore Pass path, normalized
    /// ([`CoreConnectionEvidence::relay`]).
    pub relay: CoreRelayPathState,
    /// This phone's own Shore Pass path can deliver right now
    /// ([`CoreConnectionEvidence::own_relay_usable`]).
    pub own_relay_usable: bool,
    /// Their friend card carries an internet-delivery endpoint at all.
    /// Without one, no amount of internet on this phone reaches them.
    pub contact_has_relay_endpoint: bool,
    /// Their endpoint has been written off after authoritatively rejecting us
    /// (`core_contact_relay_endpoint_usable` said no), so it is not a route
    /// today.
    pub contact_relay_stale: bool,
    /// A live direct link to this person exists right now.
    pub direct_link: bool,
    /// The freshest thing recorded about this person is a delivery receipt --
    /// the page's own row says they received a message from us.
    pub delivery_receipt_is_newest_evidence: bool,
}

/// Is there a route to *this person* right now, by the specification's
/// definition?
///
/// A live direct link, or our own working Shore Pass path plus an endpoint of
/// theirs that is not resting after rejecting us. Exported rather than spelled
/// out in each shell so a later change to what counts as usable cannot land on
/// one platform only.
#[uniffi::export]
pub fn core_contact_route_usable(
    direct_link: bool,
    own_relay_usable: bool,
    contact_has_relay_endpoint: bool,
    contact_relay_stale: bool,
) -> bool {
    direct_link || (own_relay_usable && contact_has_relay_endpoint && !contact_relay_stale)
}

/// Is this person's saved endpoint resting -- not a route to try right now?
///
/// Both halves of the persisted endpoint health, asked as one question.
/// Written off after authoritatively rejecting us, or quiet long enough that
/// spending further requests on it is waste: either way there is no internet
/// route to this person at this moment.
///
/// Exported because two callers need the same answer -- the delivery
/// classification below and [`core_person_best_route`] -- and because the
/// alternative is each shell re-deriving it from four persisted numbers and
/// two rest windows it does not own. `crate::contact_relay_health` remains the
/// only place those thresholds live; this is a name for the conjunction, not a
/// second copy of the rules.
///
/// Deliberately *not* the same question as "should a person be told about
/// this": a rested endpoint becomes probe-eligible again on a timer, so this
/// answer blinks off and on for as long as the fault lasts. What a person is
/// told is driven by the streak having reached the stale threshold, which only
/// a success clears.
#[uniffi::export]
pub fn core_contact_endpoint_resting(
    relay_reject_streak: i64,
    relay_rejected_at_ms: i64,
    relay_unreachable_streak: i64,
    relay_unreachable_at_ms: i64,
    now_ms: i64,
) -> bool {
    !core_contact_relay_endpoint_usable(relay_reject_streak, relay_rejected_at_ms, now_ms)
        || !core_contact_relay_unreachable_endpoint_usable(
            relay_unreachable_streak,
            relay_unreachable_at_ms,
            now_ms,
        )
}

/// How a message to this person would travel if one were sent right now.
///
/// The person detail expansion's "best known route now", as an enum. There is
/// no `Relay`/`ShorePass` case that means "probably": either the internet
/// route is available to this person by [`core_contact_route_usable`]'s
/// definition, or the honest answer is that the work travels at the next
/// encounter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CorePersonRoute {
    /// A live Bluetooth link to them right now.
    DirectBluetooth,
    /// A live local Wi-Fi link to them right now.
    DirectLocalWifi,
    /// No live link, but our Shore Pass path and their endpoint can carry it.
    ShorePass,
    /// No route at this moment. Not a fault: the work travels when the phones
    /// next meet, which is what this product is for.
    NoneNow,
}

/// The core's routing answer for one person, for the shells to restate.
///
/// The page must never re-derive this. Some friend endpoints are post-only by
/// design (newer friend cards carry no address this phone may poll), and a
/// shell that answered "can I reach it myself" would report those friends
/// broken when delivery to them works perfectly. Asking the core the same
/// question the router would ask is what stops that.
#[uniffi::export]
pub fn core_person_best_route(
    direct_link: Option<CoreDirectLink>,
    own_relay_usable: bool,
    contact_has_relay_endpoint: bool,
    contact_endpoint_resting: bool,
) -> CorePersonRoute {
    match direct_link {
        Some(CoreDirectLink::Bluetooth) => return CorePersonRoute::DirectBluetooth,
        Some(CoreDirectLink::LocalWifi) => return CorePersonRoute::DirectLocalWifi,
        None => {}
    }
    if core_contact_route_usable(
        false,
        own_relay_usable,
        contact_has_relay_endpoint,
        contact_endpoint_resting,
    ) {
        CorePersonRoute::ShorePass
    } else {
        CorePersonRoute::NoneNow
    }
}

/// Does the relay-upload backlog for this recipient say anything about
/// *delivery*?
///
/// Only when relay upload is the thing that would drain it. The backlog counts
/// outbound rows whose upload timestamp is still unset, and that timestamp is
/// set by one event only: a successful upload. Delivery receipts do not clear
/// it, and neither does handing the message straight to the person over
/// Bluetooth -- durable copies are left in place on purpose.
///
/// So on a phone with no pass saved, or for a friend whose card carries no
/// endpoint, or one whose endpoint has been written off, the number is not a
/// backlog at all: it is every message written to that person inside the
/// retention window, and it never goes down. Reading it as delivery state
/// there produces exactly the contradiction this page exists to remove --
/// `Received your message 12 min ago` with `Sending 12 messages…` underneath
/// it, for a week.
///
/// This is a Phase 1 honesty gate over an existing diagnostic query, not the
/// per-recipient delivery read model. That model (Phase 2) replaces the whole
/// question with a receipt-aware count and makes this function unnecessary.
#[uniffi::export]
pub fn core_relay_queue_reflects_delivery(
    relay: CoreRelayPathState,
    contact_has_relay_endpoint: bool,
    contact_relay_stale: bool,
) -> bool {
    relay != CoreRelayPathState::NotSetUp && contact_has_relay_endpoint && !contact_relay_stale
}

/// Everything the per-recipient delivery classification consumes.
///
/// The first block comes verbatim from
/// [`crate::CoreRecipientDeliveryStatus`] -- the store's answer to "what is
/// still outstanding for this person" -- and the second from this device's own
/// path state, which the shell already holds. Nothing here is a verdict; the
/// verdicts are all below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CoreRecipientDeliveryInput {
    /// User-visible messages their delivery receipt does not cover.
    pub waiting_count: u32,
    /// How many of those this device has not yet handed to Shore Pass
    /// ([`crate::CoreRecipientDeliveryStatus::unposted_waiting_count`]).
    ///
    /// Zero, with messages still waiting, means this phone has done everything
    /// it can and the other phone has not collected -- the ordinary
    /// store-and-forward case, and never a stall.
    pub unposted_waiting_count: u32,
    /// When the oldest of those started waiting, epoch ms; `0` when unknown.
    /// This device's queue time, not the message's displayed timestamp -- see
    /// [`crate::CoreRecipientDeliveryStatus::oldest_waiting_ms`] for why the
    /// two differ and why using the displayed one would be wrong.
    pub oldest_waiting_ms: i64,
    /// Newest evidence that their mail moved, epoch ms; `0` when none.
    pub last_progress_ms: i64,
    /// A waiting envelope exceeds what any transport will carry.
    pub oversized_waiting: bool,
    /// Persisted contact-endpoint health, interpreted here through
    /// `crate::contact_relay_health` rather than re-derived.
    pub relay_reject_streak: i64,
    pub relay_rejected_at_ms: i64,
    pub relay_unreachable_streak: i64,
    pub relay_unreachable_at_ms: i64,
    /// This phone's own Shore Pass path, normalized
    /// ([`CoreConnectionEvidence::relay`]).
    pub relay: CoreRelayPathState,
    /// This phone's own Shore Pass path can deliver right now
    /// ([`CoreConnectionEvidence::own_relay_usable`]).
    pub own_relay_usable: bool,
    /// Their friend card carries an internet-delivery endpoint at all.
    /// Without one, nothing about our pass or their pass is relevant to them.
    pub contact_has_relay_endpoint: bool,
    /// A live direct link to this person exists right now.
    pub direct_link: bool,
    pub now_ms: i64,
}

/// The facts the one classifier works from, after each entry point has
/// resolved its own inputs into them.
///
/// Exists so the Phase 1 entry point and the Phase 2 one cannot drift: there
/// is a single decision procedure, and the two public functions differ only in
/// how much evidence they are able to supply it.
struct DeliveryFacts {
    waiting_count: u32,
    /// How much of the waiting work this device has not handed over yet. The
    /// gate on `delayed`: see [`delivery_progress_possible`].
    unposted_waiting_count: u32,
    oldest_waiting_ms: i64,
    last_progress_ms: i64,
    oversized_waiting: bool,
    /// Their endpoint is resting -- written off after rejecting us, or quiet
    /// long enough to stop spending requests on. Not a route today.
    contact_endpoint_resting: bool,
    /// Their endpoint has failed long enough that a person should be told,
    /// rather than merely long enough to stop retrying.
    contact_setup_actionable: bool,
    relay: CoreRelayPathState,
    own_relay_usable: bool,
    contact_has_relay_endpoint: bool,
    direct_link: bool,
    now_ms: i64,
}

/// Has a usable route carried nothing for longer than the delayed window?
///
/// Measured from the later of the last progress and the oldest waiting
/// message. Both halves matter: with no progress ever recorded the wait is
/// dated from the message itself, and a message queued a minute ago is not
/// delayed merely because the previous upload was hours earlier. Unknown
/// timestamps and a clock that has moved backwards both answer `false` --
/// inventing a delay from a missing number would put a red row under a friend
/// on nothing but arithmetic.
/// Is there anything left for *this device* to make progress on?
///
/// Only while some of the waiting work has not been handed over yet. A
/// successful upload is terminal progress on this side: once every waiting
/// envelope has been accepted, nothing further happens here until their receipt
/// comes back or the two phones meet, and no amount of time passing changes
/// that.
///
/// This is the difference between a stall and the product working. Without it,
/// "a usable route and no progress for ten minutes" describes every message
/// sent to a phone that is asleep -- the upload succeeds in seconds, the
/// receipt cannot arrive until they wake, and the row would sit in Needs
/// attention reading `1 message delayed · 9 hours` by morning, with nothing
/// wrong and nothing to do. It is also permanent in the field failure where a
/// peer's contiguous receipt watermark stalls behind a gap: the waiting count
/// never returns to zero, so an age-only rule would never let go.
///
/// A phone with no pass, or a friend whose card carries no endpoint, never
/// posts anything, so everything waiting is un-posted. That is correct: the
/// delayed window is only consulted while a route is usable, and without an
/// endpoint the only usable route is a live link -- where work genuinely should
/// be moving.
fn delivery_progress_possible(facts: &DeliveryFacts) -> bool {
    facts.unposted_waiting_count > 0
}

fn delivery_progress_stalled(last_progress_ms: i64, oldest_waiting_ms: i64, now_ms: i64) -> bool {
    let since = last_progress_ms.max(oldest_waiting_ms);
    if since <= 0 || now_ms < since {
        return false;
    }
    now_ms - since >= RELAY_DELIVERY_DELAYED_THRESHOLD_MS
}

/// The one place a fault becomes an error row.
///
/// Order is the specification's severity order, and every arm before the pass
/// faults exists to stop a device-wide problem being re-announced under every
/// friend:
///
/// * an oversized message is terminal on *every* path, so it outranks
///   everything and applies even while the friend is standing right here;
/// * a live direct link means the work is moving now, so nothing is blocking
///   it whatever the internet path is doing;
/// * a friend whose card carries no endpoint is untouched by any pass fault --
///   the internet was never their route, and telling their reader otherwise
///   would be the "red under every friend" failure this page replaces.
fn delivery_blocked_reason(facts: &DeliveryFacts) -> Option<CoreDeliveryBlockedReason> {
    if facts.oversized_waiting {
        return Some(CoreDeliveryBlockedReason::MessageTooLarge);
    }
    if facts.direct_link || !facts.contact_has_relay_endpoint {
        return None;
    }
    if facts.contact_setup_actionable {
        return Some(CoreDeliveryBlockedReason::ContactSetupRejected);
    }
    match facts.relay {
        CoreRelayPathState::PassSuspended => Some(CoreDeliveryBlockedReason::PassSuspended),
        CoreRelayPathState::PassExpired => Some(CoreDeliveryBlockedReason::PassExpired),
        CoreRelayPathState::StorageFull => Some(CoreDeliveryBlockedReason::StorageFull),
        CoreRelayPathState::SetupRejected => Some(CoreDeliveryBlockedReason::OwnSetupRejected),
        // Everything else is a service having a moment, or no pass at all.
        // Neither is a fault, and neither stops the next encounter.
        CoreRelayPathState::NotSetUp
        | CoreRelayPathState::Checking
        | CoreRelayPathState::Connected
        | CoreRelayPathState::WaitingForInternet
        | CoreRelayPathState::Unreachable
        | CoreRelayPathState::SyncingSlowed => None,
    }
}

/// Which Needs attention bucket a verdict puts the person in.
fn delivery_attention(
    blocked_reason: Option<CoreDeliveryBlockedReason>,
    delayed: bool,
) -> Option<CorePersonAttention> {
    match blocked_reason {
        Some(CoreDeliveryBlockedReason::ContactSetupRejected) => {
            Some(CorePersonAttention::SetupRejected)
        }
        Some(CoreDeliveryBlockedReason::MessageTooLarge) => {
            Some(CorePersonAttention::MessageTooLarge)
        }
        Some(CoreDeliveryBlockedReason::PassExpired)
        | Some(CoreDeliveryBlockedReason::PassSuspended)
        | Some(CoreDeliveryBlockedReason::StorageFull)
        | Some(CoreDeliveryBlockedReason::OwnSetupRejected) => {
            Some(CorePersonAttention::PassBlocked)
        }
        None if delayed => Some(CorePersonAttention::Delayed),
        None => None,
    }
}

/// The single delivery decision procedure.
fn classify_delivery(facts: DeliveryFacts) -> Option<CoreDeliveryLine> {
    if facts.waiting_count == 0 {
        return None;
    }
    let route_usable = core_contact_route_usable(
        facts.direct_link,
        facts.own_relay_usable,
        facts.contact_has_relay_endpoint,
        facts.contact_endpoint_resting,
    );
    let state = if route_usable {
        CoreDeliveryState::Sending
    } else if facts.relay == CoreRelayPathState::WaitingForInternet
        && facts.contact_has_relay_endpoint
        && !facts.contact_endpoint_resting
    {
        // Only say "waiting for internet" when internet is genuinely what is
        // missing. A friend whose card carries no endpoint, or one whose
        // endpoint is resting, would not be reached by this phone coming
        // online, and telling their reader to find Wi-Fi would waste their
        // afternoon.
        CoreDeliveryState::WaitingForInternet
    } else {
        // Deliberately a promise, not a failure: store-and-forward through
        // encounters is the product's core behavior, and a friend who is
        // simply ashore may stay here indefinitely without anything being
        // wrong.
        CoreDeliveryState::WillDeliverWhenReconnected
    };
    // Three conditions, and all three are needed for the word "delayed" to be
    // true: a route this device could use, work it has not managed to hand
    // over, and enough time on both for that to be surprising.
    let delayed = route_usable
        && delivery_progress_possible(&facts)
        && delivery_progress_stalled(
            facts.last_progress_ms,
            facts.oldest_waiting_ms,
            facts.now_ms,
        );
    let blocked_reason = delivery_blocked_reason(&facts);
    Some(CoreDeliveryLine {
        count: facts.waiting_count,
        state,
        delayed,
        blocked_reason,
        attention: delivery_attention(blocked_reason, delayed),
        oldest_waiting_ms: facts.oldest_waiting_ms,
    })
}

/// The whole per-recipient delivery verdict, from the read model plus this
/// device's path state.
///
/// This is the specification's derived-state table, evaluated once, in one
/// language. The two rules it exists to guarantee:
///
/// * **A receipt silences the line rather than contradicting it.** The count
///   arriving here is already receipt-aware, so "they received your message"
///   and a waiting line cannot appear together -- not because a special case
///   suppresses the second, but because there is nothing left to count.
/// * **Age alone is never a fault.** The delayed window is only consulted
///   while a route is usable *and* this device still has work it has not
///   handed over, and the movement state under any fault stays a promise. A
///   friend who is offline stays neutral at ten minutes, at ten hours, and at
///   ten days -- including the common case where our own pass is working
///   perfectly, accepted every message, and their phone simply has not
///   collected them.
#[uniffi::export]
pub fn core_classify_recipient_delivery(
    input: CoreRecipientDeliveryInput,
) -> Option<CoreDeliveryLine> {
    // Both halves of the persisted endpoint health are consulted through
    // `contact_relay_health`, never re-derived: that module owns the streak
    // thresholds, the rest windows, and the backwards-clock rule, and a second
    // copy of any of them here is how the two shells drifted in the first
    // place. `core_contact_endpoint_resting` is the exported name for the
    // conjunction, so the person detail's route answer and this verdict cannot
    // disagree about whether the internet route exists.
    let endpoint_resting = core_contact_endpoint_resting(
        input.relay_reject_streak,
        input.relay_rejected_at_ms,
        input.relay_unreachable_streak,
        input.relay_unreachable_at_ms,
        input.now_ms,
    );
    classify_delivery(DeliveryFacts {
        waiting_count: input.waiting_count,
        unposted_waiting_count: input.unposted_waiting_count,
        oldest_waiting_ms: input.oldest_waiting_ms,
        last_progress_ms: input.last_progress_ms,
        oversized_waiting: input.oversized_waiting,
        contact_endpoint_resting: endpoint_resting,
        // Resting and actionable are deliberately different questions. A
        // written-off card becomes probe-eligible again every six hours, and
        // an unanswered host every half hour, so "resting" blinks off and on
        // for as long as the fault lasts -- fine for spending requests, and
        // useless as a thing to tell a person. The verdict a person sees is
        // driven by the streak having reached the stale threshold, which only
        // a success clears.
        contact_setup_actionable: core_contact_relay_is_stale(input.relay_reject_streak)
            || core_contact_relay_unreachable_is_stale(input.relay_unreachable_streak),
        relay: input.relay,
        own_relay_usable: input.own_relay_usable,
        contact_has_relay_endpoint: input.contact_has_relay_endpoint,
        direct_link: input.direct_link,
        now_ms: input.now_ms,
    })
}

/// The delivery line for one person from Phase 1's inputs, or `None` when
/// there is nothing honest to say.
///
/// A thin front end onto [`core_classify_recipient_delivery`], kept because
/// both shells call it while their per-recipient read model is being wired up.
/// It supplies exactly the evidence Phase 1 has -- a raw relay-upload depth, a
/// receipt as newest-evidence flag, and path state -- and no ages or
/// per-recipient faults, so the classifier cannot return `delayed` or a
/// blocking reason through this door. That is a property of the missing
/// inputs, not of a second decision procedure, which is why there is only one.
///
/// Nothing reachable through here is an error and nothing here is red. The old
/// page's `Pending relay upload` under every friend -- including friends who
/// had already received the message -- is what this replaces.
#[uniffi::export]
pub fn core_classify_delivery_line(input: CoreDeliveryLineInput) -> Option<CoreDeliveryState> {
    if !core_relay_queue_reflects_delivery(
        input.relay,
        input.contact_has_relay_endpoint,
        input.contact_relay_stale,
    ) {
        return None;
    }
    // The page has just told the reader this person received a message from
    // us. Whatever bookkeeping is left, contradicting that sentence one line
    // below it is worse than saying nothing. The Phase 2 door needs no such
    // rule: its count is receipt-aware to begin with.
    if input.delivery_receipt_is_newest_evidence {
        return None;
    }
    classify_delivery(DeliveryFacts {
        waiting_count: input.queued,
        // The Phase 1 depth *is* the un-posted backlog: it counts rows whose
        // upload timestamp is still unset.
        unposted_waiting_count: input.queued,
        oldest_waiting_ms: 0,
        last_progress_ms: 0,
        oversized_waiting: false,
        contact_endpoint_resting: input.contact_relay_stale,
        contact_setup_actionable: false,
        relay: input.relay,
        own_relay_usable: input.own_relay_usable,
        contact_has_relay_endpoint: input.contact_has_relay_endpoint,
        direct_link: input.direct_link,
        now_ms: 0,
    })
    .map(|line| line.state)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    fn healthy() -> CoreConnectionHealthInput {
        CoreConnectionHealthInput {
            runtime: CoreMeshRuntime::Active,
            bluetooth: CoreDirectPathState::Available,
            bluetooth_links: 0,
            local_wifi: CoreDirectPathState::Available,
            local_wifi_links: 0,
            relay: CoreRelayPathState::Connected,
            validated_internet: true,
            nearby_friend_count: 0,
            checking_since_ms: 0,
            now_ms: NOW,
        }
    }

    fn classify(input: CoreConnectionHealthInput) -> CoreConnectionHealth {
        core_classify_connection_health(input).state
    }

    #[test]
    fn nobody_nearby_is_ready_not_limited() {
        // The failure this page exists to stop: a phone doing exactly what it
        // was designed to do, told it has a problem.
        let report = core_classify_connection_health(healthy());
        assert_eq!(report.state, CoreConnectionHealth::Ready);
        assert_eq!(report.evidence.nearby_friend_count, 0);
        assert_eq!(report.reason, None);
        assert_eq!(report.action, None);
    }

    #[test]
    fn no_pass_saved_is_ready() {
        // No Shore Pass is the free default, not a fault.
        let report = core_classify_connection_health(CoreConnectionHealthInput {
            relay: CoreRelayPathState::NotSetUp,
            validated_internet: false,
            ..healthy()
        });
        assert_eq!(report.state, CoreConnectionHealth::Ready);
        assert_eq!(report.reason, None);
    }

    #[test]
    fn state_matrix() {
        // (relay, bluetooth, runtime, validated internet) -> state, reason
        let cases: Vec<(
            CoreRelayPathState,
            CoreDirectPathState,
            CoreMeshRuntime,
            bool,
            CoreConnectionHealth,
            Option<CoreHealthReason>,
        )> = vec![
            // Everything up.
            (
                CoreRelayPathState::Connected,
                CoreDirectPathState::Available,
                CoreMeshRuntime::Active,
                true,
                CoreConnectionHealth::Ready,
                None,
            ),
            // Pass fine, no internet: expected at sea, still Limited because
            // an expected path is resting.
            (
                CoreRelayPathState::WaitingForInternet,
                CoreDirectPathState::Available,
                CoreMeshRuntime::Active,
                false,
                CoreConnectionHealth::Limited,
                Some(CoreHealthReason::WaitingForInternet),
            ),
            // Slowed by the shared family limit: recovers on its own.
            (
                CoreRelayPathState::SyncingSlowed,
                CoreDirectPathState::Available,
                CoreMeshRuntime::Active,
                true,
                CoreConnectionHealth::Limited,
                Some(CoreHealthReason::ShorePassSlowed),
            ),
            // Service did not answer.
            (
                CoreRelayPathState::Unreachable,
                CoreDirectPathState::Available,
                CoreMeshRuntime::Active,
                true,
                CoreConnectionHealth::Limited,
                Some(CoreHealthReason::ShorePassUnreachable),
            ),
            // Bluetooth off, pass connected: one path left.
            (
                CoreRelayPathState::Connected,
                CoreDirectPathState::Off,
                CoreMeshRuntime::Active,
                true,
                CoreConnectionHealth::Limited,
                Some(CoreHealthReason::BluetoothOff),
            ),
            // Bluetooth off and no working pass: nothing left.
            (
                CoreRelayPathState::WaitingForInternet,
                CoreDirectPathState::Off,
                CoreMeshRuntime::Active,
                false,
                CoreConnectionHealth::NeedsAttention,
                Some(CoreHealthReason::BluetoothOff),
            ),
            // Our own pass expired, direct path still there.
            (
                CoreRelayPathState::PassExpired,
                CoreDirectPathState::Available,
                CoreMeshRuntime::Active,
                true,
                CoreConnectionHealth::Limited,
                Some(CoreHealthReason::PassExpired),
            ),
            (
                CoreRelayPathState::PassSuspended,
                CoreDirectPathState::Available,
                CoreMeshRuntime::Active,
                true,
                CoreConnectionHealth::Limited,
                Some(CoreHealthReason::PassSuspended),
            ),
            (
                CoreRelayPathState::SetupRejected,
                CoreDirectPathState::Available,
                CoreMeshRuntime::Active,
                true,
                CoreConnectionHealth::Limited,
                Some(CoreHealthReason::OwnSetupRejected),
            ),
            (
                CoreRelayPathState::StorageFull,
                CoreDirectPathState::Available,
                CoreMeshRuntime::Active,
                true,
                CoreConnectionHealth::Limited,
                Some(CoreHealthReason::StorageFull),
            ),
            // Mesh stopped beats everything.
            (
                CoreRelayPathState::Connected,
                CoreDirectPathState::Available,
                CoreMeshRuntime::Stopped,
                true,
                CoreConnectionHealth::NeedsAttention,
                Some(CoreHealthReason::MeshStopped),
            ),
            // Runtime reports Bluetooth off even when the path field does not.
            (
                CoreRelayPathState::Connected,
                CoreDirectPathState::Available,
                CoreMeshRuntime::BluetoothOff,
                true,
                CoreConnectionHealth::Limited,
                Some(CoreHealthReason::BluetoothOff),
            ),
        ];

        for (relay, bluetooth, runtime, internet, want_state, want_reason) in cases {
            let report = core_classify_connection_health(CoreConnectionHealthInput {
                relay,
                bluetooth,
                runtime,
                validated_internet: internet,
                // Local Wi-Fi off throughout so the matrix isolates the pair
                // being varied; an available LAN would mask Bluetooth being
                // off.
                local_wifi: CoreDirectPathState::Off,
                ..healthy()
            });
            assert_eq!(
                report.state, want_state,
                "{relay:?}/{bluetooth:?}/{runtime:?}/internet={internet}"
            );
            assert_eq!(
                report.reason, want_reason,
                "{relay:?}/{bluetooth:?}/{runtime:?}/internet={internet}"
            );
        }
    }

    #[test]
    fn rate_limited_is_limited_never_needs_attention() {
        // A 429 is the service asking us to slow down. Messages still move,
        // and nobody should be told to contact support.
        for bluetooth in [CoreDirectPathState::Available, CoreDirectPathState::Off] {
            let report = core_classify_connection_health(CoreConnectionHealthInput {
                relay: CoreRelayPathState::SyncingSlowed,
                bluetooth,
                local_wifi: CoreDirectPathState::Off,
                ..healthy()
            });
            assert_ne!(report.state, CoreConnectionHealth::NeedsAttention);
            assert_eq!(report.state, CoreConnectionHealth::Limited);
            assert!(report.evidence.any_path_usable);
        }
    }

    #[test]
    fn connected_without_internet_reports_waiting() {
        // The old page's flagship contradiction: "Shore Pass connected" on a
        // phone that has been offline for an hour.
        let report = core_classify_connection_health(CoreConnectionHealthInput {
            relay: CoreRelayPathState::Connected,
            validated_internet: false,
            ..healthy()
        });
        assert_eq!(
            report.evidence.relay,
            CoreRelayPathState::WaitingForInternet
        );
        assert!(!report.evidence.own_relay_usable);
        assert_eq!(report.state, CoreConnectionHealth::Limited);
    }

    #[test]
    fn stopped_mesh_owns_no_paths() {
        let report = core_classify_connection_health(CoreConnectionHealthInput {
            runtime: CoreMeshRuntime::Stopped,
            ..healthy()
        });
        assert!(!report.evidence.any_path_usable);
        assert!(!report.evidence.own_relay_usable);
        assert_eq!(report.action, Some(CoreHealthAction::StartMesh));
    }

    #[test]
    fn checking_is_bounded_and_every_path_resolves() {
        // Every way of entering Checking must leave it within the bound.
        let pending_inputs = vec![
            CoreConnectionHealthInput {
                runtime: CoreMeshRuntime::Starting,
                checking_since_ms: NOW,
                ..healthy()
            },
            CoreConnectionHealthInput {
                relay: CoreRelayPathState::Checking,
                bluetooth: CoreDirectPathState::Off,
                local_wifi: CoreDirectPathState::Off,
                checking_since_ms: NOW,
                ..healthy()
            },
            CoreConnectionHealthInput {
                relay: CoreRelayPathState::NotSetUp,
                bluetooth: CoreDirectPathState::Starting,
                local_wifi: CoreDirectPathState::Off,
                checking_since_ms: NOW,
                ..healthy()
            },
            CoreConnectionHealthInput {
                relay: CoreRelayPathState::NotSetUp,
                bluetooth: CoreDirectPathState::Off,
                local_wifi: CoreDirectPathState::Starting,
                checking_since_ms: NOW,
                ..healthy()
            },
        ];
        for input in pending_inputs {
            assert_eq!(
                classify(CoreConnectionHealthInput {
                    now_ms: NOW + CONNECTION_CHECKING_TIMEOUT_MS - 1,
                    ..input.clone()
                }),
                CoreConnectionHealth::Checking,
                "{input:?} should still be checking inside the bound"
            );
            let resolved = classify(CoreConnectionHealthInput {
                now_ms: NOW + CONNECTION_CHECKING_TIMEOUT_MS,
                ..input.clone()
            });
            assert_ne!(
                resolved,
                CoreConnectionHealth::Checking,
                "{input:?} must resolve at the bound"
            );
        }
    }

    #[test]
    fn checking_never_hides_a_verdict_that_already_exists() {
        // Bluetooth is up and listening: that is a real answer, so a relay
        // check running in the background must not blank the card.
        let report = core_classify_connection_health(CoreConnectionHealthInput {
            relay: CoreRelayPathState::Checking,
            bluetooth: CoreDirectPathState::Available,
            checking_since_ms: NOW,
            ..healthy()
        });
        assert_eq!(report.state, CoreConnectionHealth::Ready);
        assert_eq!(report.evidence.relay, CoreRelayPathState::Checking);
    }

    #[test]
    fn expired_checking_resolves_to_a_real_relay_state() {
        let offline = core_classify_connection_health(CoreConnectionHealthInput {
            relay: CoreRelayPathState::Checking,
            validated_internet: false,
            checking_since_ms: NOW - CONNECTION_CHECKING_TIMEOUT_MS,
            ..healthy()
        });
        assert_eq!(
            offline.evidence.relay,
            CoreRelayPathState::WaitingForInternet
        );
        let online = core_classify_connection_health(CoreConnectionHealthInput {
            relay: CoreRelayPathState::Checking,
            checking_since_ms: NOW - CONNECTION_CHECKING_TIMEOUT_MS,
            ..healthy()
        });
        assert_eq!(online.evidence.relay, CoreRelayPathState::Unreachable);
    }

    #[test]
    fn checking_bound_handles_missing_and_backwards_clocks() {
        assert!(core_connection_checking_expired(0, NOW));
        assert!(core_connection_checking_expired(-1, NOW));
        // Clock moved backwards under us: resolve rather than spin forever.
        assert!(core_connection_checking_expired(NOW + 60_000, NOW));
        assert!(!core_connection_checking_expired(NOW, NOW));
        assert!(!core_connection_checking_expired(
            NOW,
            NOW + CONNECTION_CHECKING_TIMEOUT_MS - 1
        ));
        assert!(core_connection_checking_expired(
            NOW,
            NOW + CONNECTION_CHECKING_TIMEOUT_MS
        ));
    }

    // -----------------------------------------------------------------------
    // People
    // -----------------------------------------------------------------------

    fn person(name: &str, id: u8) -> CorePersonHealthInput {
        CorePersonHealthInput {
            user_id: vec![id],
            display_name: name.to_string(),
            blocked: false,
            direct_link: None,
            presence_last_seen_ms: 0,
            last_seen_ms: 0,
            attention: None,
            attention_since_ms: 0,
        }
    }

    fn ids(placements: &[CorePersonPlacement]) -> Vec<u8> {
        placements.iter().map(|p| p.user_id[0]).collect()
    }

    #[test]
    fn one_friends_rejected_card_never_drives_the_overall_state() {
        // The overall classification cannot see people at all -- this test
        // pins that the two calls are independent, which is the guarantee.
        let people = vec![CorePersonHealthInput {
            attention: Some(CorePersonAttention::SetupRejected),
            attention_since_ms: NOW - 60_000,
            ..person("Riley's phone", 1)
        }];
        let report = core_classify_connection_health(healthy());
        assert_eq!(report.state, CoreConnectionHealth::Ready);

        let groups = core_group_people(people, report.evidence.own_relay_usable, NOW);
        assert_eq!(ids(&groups.needs_attention), vec![1]);
        assert!(groups.reachable_now.is_empty());
        assert!(groups.other_people.is_empty());
    }

    #[test]
    fn reachable_now_takes_live_links_and_fresh_presence_with_a_working_pass() {
        let people = vec![
            CorePersonHealthInput {
                direct_link: Some(CoreDirectLink::LocalWifi),
                ..person("Riley's phone", 1)
            },
            CorePersonHealthInput {
                direct_link: Some(CoreDirectLink::Bluetooth),
                ..person("Sam", 2)
            },
            CorePersonHealthInput {
                presence_last_seen_ms: NOW - 60_000,
                last_seen_ms: NOW - 60_000,
                ..person("Ash", 3)
            },
            // Presence just outside the window.
            CorePersonHealthInput {
                presence_last_seen_ms: NOW - CONNECTION_PRESENCE_ONLINE_WINDOW_MS - 1,
                last_seen_ms: NOW - CONNECTION_PRESENCE_ONLINE_WINDOW_MS - 1,
                ..person("Dana", 4)
            },
        ];
        let groups = core_group_people(people.clone(), true, NOW);
        // Ordered by name: Ash, Riley's phone, Sam.
        assert_eq!(ids(&groups.reachable_now), vec![3, 1, 2]);
        assert_eq!(ids(&groups.other_people), vec![4]);
        assert_eq!(
            groups.reachable_now[0].reach,
            CorePersonReach::RelayPresence
        );
        assert_eq!(
            groups.reachable_now[1].reach,
            CorePersonReach::DirectLocalWifi
        );
        assert_eq!(
            groups.reachable_now[2].reach,
            CorePersonReach::DirectBluetooth
        );

        // Same facts with our own pass down: presence alone no longer counts,
        // direct links still do.
        let groups = core_group_people(people, false, NOW);
        assert_eq!(ids(&groups.reachable_now), vec![1, 2]);
        assert_eq!(ids(&groups.other_people), vec![3, 4]);
    }

    #[test]
    fn presence_from_the_future_is_not_evidence() {
        let groups = core_group_people(
            vec![CorePersonHealthInput {
                presence_last_seen_ms: NOW + 60_000,
                ..person("Ash", 1)
            }],
            true,
            NOW,
        );
        assert!(groups.reachable_now.is_empty());
        assert_eq!(ids(&groups.other_people), vec![1]);
    }

    #[test]
    fn blocked_people_are_absent_from_every_group() {
        let people = vec![
            CorePersonHealthInput {
                blocked: true,
                direct_link: Some(CoreDirectLink::Bluetooth),
                attention: Some(CorePersonAttention::SetupRejected),
                last_seen_ms: NOW,
                ..person("Blocked", 9)
            },
            CorePersonHealthInput {
                direct_link: Some(CoreDirectLink::Bluetooth),
                ..person("Sam", 2)
            },
        ];
        let groups = core_group_people(people, true, NOW);
        for group in [
            &groups.needs_attention,
            &groups.reachable_now,
            &groups.other_people,
        ] {
            assert!(!ids(group).contains(&9));
        }
        assert_eq!(ids(&groups.reachable_now), vec![2]);
    }

    #[test]
    fn needs_attention_orders_by_severity_then_oldest_affected_message() {
        let people = vec![
            CorePersonHealthInput {
                attention: Some(CorePersonAttention::Delayed),
                attention_since_ms: NOW - 600_000,
                ..person("Ash", 1)
            },
            CorePersonHealthInput {
                attention: Some(CorePersonAttention::SetupRejected),
                attention_since_ms: NOW - 60_000,
                ..person("Bo", 2)
            },
            CorePersonHealthInput {
                attention: Some(CorePersonAttention::SetupRejected),
                attention_since_ms: NOW - 900_000,
                ..person("Cam", 3)
            },
            CorePersonHealthInput {
                attention: Some(CorePersonAttention::SetupRejected),
                // Unknown age sorts behind any known one.
                attention_since_ms: 0,
                ..person("Dana", 4)
            },
        ];
        let groups = core_group_people(people, true, NOW);
        assert_eq!(ids(&groups.needs_attention), vec![3, 2, 4, 1]);
    }

    #[test]
    fn needs_attention_wins_over_being_reachable() {
        // A friend can be standing next to you and still have a broken card.
        let groups = core_group_people(
            vec![CorePersonHealthInput {
                direct_link: Some(CoreDirectLink::Bluetooth),
                attention: Some(CorePersonAttention::SetupRejected),
                ..person("Sam", 1)
            }],
            true,
            NOW,
        );
        assert_eq!(ids(&groups.needs_attention), vec![1]);
        assert_eq!(
            groups.needs_attention[0].reach,
            CorePersonReach::DirectBluetooth
        );
        assert!(groups.reachable_now.is_empty());
    }

    #[test]
    fn other_people_order_by_freshest_evidence_then_name_with_no_history_last() {
        let people = vec![
            CorePersonHealthInput {
                last_seen_ms: NOW - 3_600_000,
                ..person("Ash", 1)
            },
            CorePersonHealthInput {
                last_seen_ms: NOW - 600_000,
                ..person("Bo", 2)
            },
            person("Zoe", 3),
            person("Ada", 4),
            CorePersonHealthInput {
                presence_last_seen_ms: NOW - 300_000,
                ..person("Cam", 5)
            },
        ];
        let groups = core_group_people(people, true, NOW);
        // Cam (5 min, via presence) then Bo (10 min) then Ash (1 hour), then
        // the two with nothing recorded, by name.
        assert_eq!(ids(&groups.other_people), vec![5, 2, 1, 4, 3]);
    }

    #[test]
    fn ordering_is_stable_for_identical_facts() {
        let people = vec![person("Same", 2), person("same", 1)];
        let groups = core_group_people(people, true, NOW);
        assert_eq!(ids(&groups.other_people), vec![1, 2]);
    }

    #[test]
    fn attention_rank_is_strictly_ordered() {
        let ordered = [
            CorePersonAttention::Delayed,
            CorePersonAttention::MessageTooLarge,
            CorePersonAttention::PassBlocked,
            CorePersonAttention::SetupRejected,
        ];
        for pair in ordered.windows(2) {
            assert!(
                core_person_attention_rank(pair[0]) < core_person_attention_rank(pair[1]),
                "{:?} should rank below {:?}",
                pair[0],
                pair[1]
            );
        }
    }

    #[test]
    fn reachability_helper_agrees_with_the_grouping() {
        assert!(core_person_is_reachable_now(
            CorePersonReach::DirectBluetooth
        ));
        assert!(core_person_is_reachable_now(
            CorePersonReach::DirectLocalWifi
        ));
        assert!(core_person_is_reachable_now(CorePersonReach::RelayPresence));
        assert!(!core_person_is_reachable_now(CorePersonReach::None));
    }

    #[test]
    fn check_pending_covers_every_path_that_can_still_be_coming_up() {
        // The shells own the clock that records when a wait began, so they ask
        // this question themselves. iOS asked a narrower one and rendered
        // "Needs attention" while CoreBluetooth was still answering.
        assert!(core_connection_check_pending(
            CoreMeshRuntime::Starting,
            CoreDirectPathState::Available,
            CoreDirectPathState::Available,
            CoreRelayPathState::Connected
        ));
        assert!(core_connection_check_pending(
            CoreMeshRuntime::Active,
            CoreDirectPathState::Starting,
            CoreDirectPathState::Off,
            CoreRelayPathState::NotSetUp
        ));
        assert!(core_connection_check_pending(
            CoreMeshRuntime::Active,
            CoreDirectPathState::Off,
            CoreDirectPathState::Starting,
            CoreRelayPathState::NotSetUp
        ));
        assert!(core_connection_check_pending(
            CoreMeshRuntime::Active,
            CoreDirectPathState::Available,
            CoreDirectPathState::Available,
            CoreRelayPathState::Checking
        ));
        assert!(!core_connection_check_pending(
            CoreMeshRuntime::Active,
            CoreDirectPathState::Available,
            CoreDirectPathState::Off,
            CoreRelayPathState::NotSetUp
        ));
        assert!(!core_connection_check_pending(
            CoreMeshRuntime::Stopped,
            CoreDirectPathState::Off,
            CoreDirectPathState::Off,
            CoreRelayPathState::NotSetUp
        ));
    }

    #[test]
    fn a_radio_still_coming_up_is_checking_not_a_failure() {
        // The exact iOS trace: relaunch with the Bluetooth stack unanswered,
        // no LAN, no pass. Nothing is usable yet, but nothing has failed
        // either.
        let input = CoreConnectionHealthInput {
            runtime: CoreMeshRuntime::Active,
            bluetooth: CoreDirectPathState::Starting,
            local_wifi: CoreDirectPathState::Off,
            relay: CoreRelayPathState::NotSetUp,
            validated_internet: false,
            checking_since_ms: NOW,
            ..healthy()
        };
        assert_eq!(
            classify(CoreConnectionHealthInput {
                now_ms: NOW + 1_000,
                ..input.clone()
            }),
            CoreConnectionHealth::Checking
        );
        // And it still resolves at the bound rather than resting there.
        assert_eq!(
            classify(CoreConnectionHealthInput {
                now_ms: NOW + CONNECTION_CHECKING_TIMEOUT_MS,
                ..input
            }),
            CoreConnectionHealth::NeedsAttention
        );
    }

    // -----------------------------------------------------------------------
    // Delivery language
    // -----------------------------------------------------------------------

    fn queued(count: u32) -> CoreDeliveryLineInput {
        CoreDeliveryLineInput {
            queued: count,
            relay: CoreRelayPathState::Connected,
            own_relay_usable: true,
            contact_has_relay_endpoint: true,
            contact_relay_stale: false,
            direct_link: false,
            delivery_receipt_is_newest_evidence: false,
        }
    }

    #[test]
    fn nothing_waiting_means_no_line() {
        assert_eq!(core_classify_delivery_line(queued(0)), None);
    }

    #[test]
    fn a_backlog_that_relay_upload_cannot_drain_is_not_delivery_evidence() {
        // The failure this gate exists for: no pass saved, so the upload
        // stamp is never set, so the "backlog" is every message written to
        // this person in the retention window -- forever, and beside a row
        // that already says they received one.
        assert_eq!(
            core_classify_delivery_line(CoreDeliveryLineInput {
                relay: CoreRelayPathState::NotSetUp,
                own_relay_usable: false,
                direct_link: true,
                ..queued(12)
            }),
            None
        );
        // Same reasoning for a friend whose card carries no endpoint at all,
        // and for one whose endpoint has been written off.
        assert_eq!(
            core_classify_delivery_line(CoreDeliveryLineInput {
                contact_has_relay_endpoint: false,
                ..queued(12)
            }),
            None
        );
        assert_eq!(
            core_classify_delivery_line(CoreDeliveryLineInput {
                contact_relay_stale: true,
                ..queued(12)
            }),
            None
        );
    }

    #[test]
    fn a_delivery_receipt_silences_the_line_rather_than_contradicting_it() {
        assert_eq!(
            core_classify_delivery_line(CoreDeliveryLineInput {
                delivery_receipt_is_newest_evidence: true,
                ..queued(12)
            }),
            None
        );
    }

    #[test]
    fn delivery_matrix() {
        // (relay, own relay usable, direct link) -> line
        let cases: Vec<(CoreRelayPathState, bool, bool, Option<CoreDeliveryState>)> = vec![
            (
                CoreRelayPathState::Connected,
                true,
                false,
                Some(CoreDeliveryState::Sending),
            ),
            (
                CoreRelayPathState::SyncingSlowed,
                true,
                false,
                Some(CoreDeliveryState::Sending),
            ),
            // No usable pass, but they are standing right here.
            (
                CoreRelayPathState::WaitingForInternet,
                false,
                true,
                Some(CoreDeliveryState::Sending),
            ),
            (
                CoreRelayPathState::WaitingForInternet,
                false,
                false,
                Some(CoreDeliveryState::WaitingForInternet),
            ),
            // Terminal-looking pass faults are still not failures for the
            // person: the work travels at the next encounter.
            (
                CoreRelayPathState::Unreachable,
                false,
                false,
                Some(CoreDeliveryState::WillDeliverWhenReconnected),
            ),
            (
                CoreRelayPathState::PassExpired,
                false,
                false,
                Some(CoreDeliveryState::WillDeliverWhenReconnected),
            ),
            (
                CoreRelayPathState::StorageFull,
                false,
                false,
                Some(CoreDeliveryState::WillDeliverWhenReconnected),
            ),
            (
                CoreRelayPathState::Checking,
                false,
                false,
                Some(CoreDeliveryState::WillDeliverWhenReconnected),
            ),
        ];
        for (relay, own_relay_usable, direct_link, want) in cases {
            assert_eq!(
                core_classify_delivery_line(CoreDeliveryLineInput {
                    relay,
                    own_relay_usable,
                    direct_link,
                    ..queued(2)
                }),
                want,
                "{relay:?}/own={own_relay_usable}/direct={direct_link}"
            );
        }
    }

    #[test]
    fn route_usability_is_one_predicate() {
        assert!(core_contact_route_usable(true, false, false, true));
        assert!(core_contact_route_usable(false, true, true, false));
        assert!(!core_contact_route_usable(false, true, true, true));
        assert!(!core_contact_route_usable(false, true, false, false));
        assert!(!core_contact_route_usable(false, false, true, false));
    }

    // -----------------------------------------------------------------------
    // Per-recipient delivery (Phase 2)
    // -----------------------------------------------------------------------

    use crate::contact_relay_health::{
        CONTACT_RELAY_RECHECK_MS, CONTACT_RELAY_STALE_STREAK,
        CONTACT_RELAY_UNREACHABLE_STALE_STREAK, CONTACT_RELAY_UNREACHABLE_STREAK,
    };

    /// A healthy recipient with `count` messages waiting, none of them handed
    /// over yet, everything working, and progress a minute ago.
    fn waiting(count: u32) -> CoreRecipientDeliveryInput {
        CoreRecipientDeliveryInput {
            waiting_count: count,
            unposted_waiting_count: count,
            oldest_waiting_ms: NOW - 60_000,
            last_progress_ms: NOW - 60_000,
            oversized_waiting: false,
            relay_reject_streak: 0,
            relay_rejected_at_ms: 0,
            relay_unreachable_streak: 0,
            relay_unreachable_at_ms: 0,
            relay: CoreRelayPathState::Connected,
            own_relay_usable: true,
            contact_has_relay_endpoint: true,
            direct_link: false,
            now_ms: NOW,
        }
    }

    /// Nobody reachable, nothing moving: the ordinary offline friend.
    fn offline(count: u32) -> CoreRecipientDeliveryInput {
        CoreRecipientDeliveryInput {
            relay: CoreRelayPathState::WaitingForInternet,
            own_relay_usable: false,
            ..waiting(count)
        }
    }

    /// Our own path is fine and every waiting message was accepted; the friend
    /// simply has not collected them. The commonest state in the product, and
    /// the one an age-only rule would call a fault.
    fn uncollected(count: u32) -> CoreRecipientDeliveryInput {
        CoreRecipientDeliveryInput {
            unposted_waiting_count: 0,
            ..waiting(count)
        }
    }

    #[test]
    fn a_receipt_covered_recipient_gets_no_line_at_all() {
        // Not "the line is suppressed" -- there is nothing left to count,
        // because the store's count is receipt-aware. That is what makes
        // "Received your message" and a waiting warning structurally unable to
        // appear together.
        assert_eq!(core_classify_recipient_delivery(waiting(0)), None);
    }

    #[test]
    fn recipient_delivery_matrix() {
        // The specification's derived-state table, one row at a time.
        // (label, input, expected state, delayed, blocked reason)
        let cases: Vec<(
            &str,
            CoreRecipientDeliveryInput,
            CoreDeliveryState,
            bool,
            Option<CoreDeliveryBlockedReason>,
        )> = vec![
            (
                "usable relay route",
                waiting(2),
                CoreDeliveryState::Sending,
                false,
                None,
            ),
            (
                "standing next to them with no pass at all",
                CoreRecipientDeliveryInput {
                    relay: CoreRelayPathState::NotSetUp,
                    own_relay_usable: false,
                    contact_has_relay_endpoint: false,
                    direct_link: true,
                    ..waiting(2)
                },
                CoreDeliveryState::Sending,
                false,
                None,
            ),
            (
                "no internet, and internet is what is missing",
                offline(2),
                CoreDeliveryState::WaitingForInternet,
                false,
                None,
            ),
            (
                "no internet, but they have no endpoint either",
                CoreRecipientDeliveryInput {
                    contact_has_relay_endpoint: false,
                    ..offline(2)
                },
                CoreDeliveryState::WillDeliverWhenReconnected,
                false,
                None,
            ),
            (
                "the service is not answering",
                CoreRecipientDeliveryInput {
                    relay: CoreRelayPathState::Unreachable,
                    own_relay_usable: false,
                    ..waiting(2)
                },
                CoreDeliveryState::WillDeliverWhenReconnected,
                false,
                None,
            ),
            (
                "slowed by the shared family limit: still moving",
                CoreRecipientDeliveryInput {
                    relay: CoreRelayPathState::SyncingSlowed,
                    ..waiting(2)
                },
                CoreDeliveryState::Sending,
                false,
                None,
            ),
            (
                "usable route, nothing has moved for the window",
                CoreRecipientDeliveryInput {
                    last_progress_ms: NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
                    oldest_waiting_ms: NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
                    ..waiting(2)
                },
                CoreDeliveryState::Sending,
                true,
                None,
            ),
            (
                "their saved setup was rejected",
                CoreRecipientDeliveryInput {
                    relay_reject_streak: CONTACT_RELAY_STALE_STREAK,
                    relay_rejected_at_ms: NOW - 1_000,
                    ..waiting(2)
                },
                CoreDeliveryState::WillDeliverWhenReconnected,
                false,
                Some(CoreDeliveryBlockedReason::ContactSetupRejected),
            ),
            (
                "their host has been silent long enough to say so",
                CoreRecipientDeliveryInput {
                    relay_unreachable_streak: CONTACT_RELAY_UNREACHABLE_STALE_STREAK,
                    relay_unreachable_at_ms: NOW - 1_000,
                    ..waiting(2)
                },
                CoreDeliveryState::WillDeliverWhenReconnected,
                false,
                Some(CoreDeliveryBlockedReason::ContactSetupRejected),
            ),
            (
                "our pass expired",
                CoreRecipientDeliveryInput {
                    relay: CoreRelayPathState::PassExpired,
                    own_relay_usable: false,
                    ..waiting(2)
                },
                CoreDeliveryState::WillDeliverWhenReconnected,
                false,
                Some(CoreDeliveryBlockedReason::PassExpired),
            ),
            (
                "our pass suspended",
                CoreRecipientDeliveryInput {
                    relay: CoreRelayPathState::PassSuspended,
                    own_relay_usable: false,
                    ..waiting(2)
                },
                CoreDeliveryState::WillDeliverWhenReconnected,
                false,
                Some(CoreDeliveryBlockedReason::PassSuspended),
            ),
            (
                "our family storage is full",
                CoreRecipientDeliveryInput {
                    relay: CoreRelayPathState::StorageFull,
                    own_relay_usable: false,
                    ..waiting(2)
                },
                CoreDeliveryState::WillDeliverWhenReconnected,
                false,
                Some(CoreDeliveryBlockedReason::StorageFull),
            ),
            (
                "our own saved setup was rejected",
                CoreRecipientDeliveryInput {
                    relay: CoreRelayPathState::SetupRejected,
                    own_relay_usable: false,
                    ..waiting(2)
                },
                CoreDeliveryState::WillDeliverWhenReconnected,
                false,
                Some(CoreDeliveryBlockedReason::OwnSetupRejected),
            ),
            (
                "a message no transport will carry",
                CoreRecipientDeliveryInput {
                    oversized_waiting: true,
                    ..waiting(1)
                },
                CoreDeliveryState::Sending,
                false,
                Some(CoreDeliveryBlockedReason::MessageTooLarge),
            ),
        ];

        for (label, input, want_state, want_delayed, want_reason) in cases {
            let line = core_classify_recipient_delivery(input)
                .unwrap_or_else(|| panic!("{label}: expected a line"));
            assert_eq!(line.state, want_state, "{label}");
            assert_eq!(line.delayed, want_delayed, "{label}");
            assert_eq!(line.blocked_reason, want_reason, "{label}");
        }
    }

    #[test]
    fn an_offline_friend_is_never_an_error_at_any_age() {
        // The DTN rule, pinned. Waiting is what this product is for; nothing
        // about how long it has been waiting may turn it into a fault.
        let minute = 60_000i64;
        for age in [
            minute,
            10 * minute,
            60 * minute,
            24 * 60 * minute,
            30 * 24 * 60 * minute,
            365 * 24 * 60 * minute,
        ] {
            let line = core_classify_recipient_delivery(CoreRecipientDeliveryInput {
                oldest_waiting_ms: NOW - age,
                last_progress_ms: 0,
                ..offline(3)
            })
            .expect("waiting work still gets a line");
            assert_eq!(
                line.state,
                CoreDeliveryState::WaitingForInternet,
                "age {age}ms"
            );
            assert!(!line.delayed, "age {age}ms");
            assert_eq!(line.blocked_reason, None, "age {age}ms");
            assert_eq!(line.attention, None, "age {age}ms");
        }
    }

    #[test]
    fn a_friend_who_has_not_collected_yet_is_never_an_error_at_any_age() {
        // The same DTN rule for the far commoner shape: our pass is Connected,
        // their endpoint is healthy, every message was accepted -- and their
        // phone is asleep. A successful upload is the last progress this
        // device can ever record, so measuring a stall against it would put
        // "1 message delayed · 9 hours" under every friend messaged overnight,
        // permanently in the field failure where a peer's receipt watermark
        // stalls behind a gap.
        let minute = 60_000i64;
        for age in [
            10 * minute,
            60 * minute,
            24 * 60 * minute,
            30 * 24 * 60 * minute,
        ] {
            let line = core_classify_recipient_delivery(CoreRecipientDeliveryInput {
                oldest_waiting_ms: NOW - age,
                // The upload succeeded when the message was written, and
                // nothing has happened since.
                last_progress_ms: NOW - age,
                ..uncollected(2)
            })
            .expect("waiting work still gets a line");
            // Truthful about where the work is, silent about whose fault it
            // is, and nowhere near Needs attention.
            assert_eq!(line.state, CoreDeliveryState::Sending, "age {age}ms");
            assert!(!line.delayed, "age {age}ms");
            assert_eq!(line.blocked_reason, None, "age {age}ms");
            assert_eq!(line.attention, None, "age {age}ms");
        }

        // One message left un-posted in the same conversation is enough to
        // bring the stall back: now this phone really is stuck.
        let line = core_classify_recipient_delivery(CoreRecipientDeliveryInput {
            unposted_waiting_count: 1,
            oldest_waiting_ms: NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
            last_progress_ms: NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
            ..uncollected(2)
        })
        .unwrap();
        assert!(line.delayed);
        assert_eq!(line.attention, Some(CorePersonAttention::Delayed));
    }

    #[test]
    fn a_fault_never_takes_the_promise_away() {
        // Every blocking reason still leaves a truthful movement state
        // underneath it: the messages really do travel at the next encounter.
        for input in [
            CoreRecipientDeliveryInput {
                relay: CoreRelayPathState::PassExpired,
                own_relay_usable: false,
                ..waiting(2)
            },
            CoreRecipientDeliveryInput {
                relay_reject_streak: CONTACT_RELAY_STALE_STREAK,
                relay_rejected_at_ms: NOW,
                ..waiting(2)
            },
        ] {
            let line = core_classify_recipient_delivery(input).unwrap();
            assert!(line.blocked_reason.is_some());
            assert_eq!(line.state, CoreDeliveryState::WillDeliverWhenReconnected);
        }
    }

    #[test]
    fn a_friend_with_no_endpoint_is_untouched_by_our_pass_faults() {
        // The "red under every friend" failure, structurally prevented: the
        // internet was never this person's route, so our pass expiring says
        // nothing about them.
        for relay in [
            CoreRelayPathState::PassExpired,
            CoreRelayPathState::PassSuspended,
            CoreRelayPathState::StorageFull,
            CoreRelayPathState::SetupRejected,
        ] {
            let line = core_classify_recipient_delivery(CoreRecipientDeliveryInput {
                relay,
                own_relay_usable: false,
                contact_has_relay_endpoint: false,
                ..waiting(2)
            })
            .unwrap();
            assert_eq!(line.blocked_reason, None, "{relay:?}");
            assert_eq!(line.attention, None, "{relay:?}");
            assert_eq!(line.state, CoreDeliveryState::WillDeliverWhenReconnected);
        }
    }

    #[test]
    fn a_live_link_beats_every_pass_fault_but_not_an_oversized_message() {
        // Their phone is right here: the work is moving, whatever the internet
        // path is doing.
        let line = core_classify_recipient_delivery(CoreRecipientDeliveryInput {
            relay: CoreRelayPathState::PassExpired,
            own_relay_usable: false,
            direct_link: true,
            relay_reject_streak: CONTACT_RELAY_STALE_STREAK,
            relay_rejected_at_ms: NOW,
            ..waiting(2)
        })
        .unwrap();
        assert_eq!(line.state, CoreDeliveryState::Sending);
        assert_eq!(line.blocked_reason, None);

        // Except for a message the framing itself will not carry, which no
        // amount of proximity fixes.
        let line = core_classify_recipient_delivery(CoreRecipientDeliveryInput {
            direct_link: true,
            oversized_waiting: true,
            ..waiting(1)
        })
        .unwrap();
        assert_eq!(
            line.blocked_reason,
            Some(CoreDeliveryBlockedReason::MessageTooLarge)
        );
    }

    #[test]
    fn delayed_needs_a_usable_route_and_a_real_age() {
        // Exactly at the threshold, with a route: delayed.
        let stalled = CoreRecipientDeliveryInput {
            oldest_waiting_ms: NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
            last_progress_ms: NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
            ..waiting(2)
        };
        assert!(core_classify_recipient_delivery(stalled).unwrap().delayed);

        // One millisecond short of it: not yet.
        assert!(
            !core_classify_recipient_delivery(CoreRecipientDeliveryInput {
                oldest_waiting_ms: NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS + 1,
                last_progress_ms: NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS + 1,
                ..waiting(2)
            })
            .unwrap()
            .delayed
        );

        // A recent upload resets it even though the oldest message is old:
        // something is moving, which is the whole question.
        assert!(
            !core_classify_recipient_delivery(CoreRecipientDeliveryInput {
                oldest_waiting_ms: NOW - 10 * RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
                last_progress_ms: NOW - 1_000,
                ..waiting(2)
            })
            .unwrap()
            .delayed
        );

        // Nothing has ever moved, so the wait is dated from the message.
        assert!(
            core_classify_recipient_delivery(CoreRecipientDeliveryInput {
                oldest_waiting_ms: NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
                last_progress_ms: 0,
                ..waiting(2)
            })
            .unwrap()
            .delayed
        );

        // No usable route: age means nothing.
        assert!(
            !core_classify_recipient_delivery(CoreRecipientDeliveryInput {
                oldest_waiting_ms: NOW - 100 * RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
                last_progress_ms: 0,
                ..offline(2)
            })
            .unwrap()
            .delayed
        );

        // Nothing left for this device to hand over: age means nothing here
        // either, however usable the route is. Waiting on the other phone is
        // not a stall.
        assert!(
            !core_classify_recipient_delivery(CoreRecipientDeliveryInput {
                oldest_waiting_ms: NOW - 100 * RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
                last_progress_ms: NOW - 100 * RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
                ..uncollected(2)
            })
            .unwrap()
            .delayed
        );
    }

    #[test]
    fn unknown_and_backwards_timestamps_never_invent_a_delay() {
        // Both directions resolve to "not delayed": a red row assembled from a
        // missing number, or from a clock that moved, is worse than silence.
        assert!(!delivery_progress_stalled(0, 0, NOW));
        assert!(!delivery_progress_stalled(-1, -1, NOW));
        assert!(!delivery_progress_stalled(NOW + 60_000, NOW + 60_000, NOW));
        assert!(!delivery_progress_stalled(
            0,
            NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS + 1,
            NOW
        ));
        assert!(delivery_progress_stalled(
            0,
            NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
            NOW
        ));
    }

    #[test]
    fn the_person_verdict_uses_stale_thresholds_not_the_probe_windows() {
        // A written-off card becomes probe-eligible again every six hours.
        // That must not blink the row a person is reading back to normal.
        let rejected_at = NOW - CONTACT_RELAY_RECHECK_MS;
        let line = core_classify_recipient_delivery(CoreRecipientDeliveryInput {
            relay_reject_streak: CONTACT_RELAY_STALE_STREAK,
            relay_rejected_at_ms: rejected_at,
            ..waiting(2)
        })
        .unwrap();
        assert_eq!(
            line.blocked_reason,
            Some(CoreDeliveryBlockedReason::ContactSetupRejected)
        );

        // And a rest that has not yet become actionable stays quiet: two
        // silent passes stop us hammering the host, but are not enough to tell
        // a person their friend's setup is broken.
        let line = core_classify_recipient_delivery(CoreRecipientDeliveryInput {
            relay_unreachable_streak: CONTACT_RELAY_UNREACHABLE_STREAK,
            relay_unreachable_at_ms: NOW,
            ..waiting(2)
        })
        .unwrap();
        assert_eq!(line.blocked_reason, None);
        assert_eq!(line.state, CoreDeliveryState::WillDeliverWhenReconnected);
    }

    #[test]
    fn attention_follows_the_reason_and_orders_by_severity() {
        let cases = [
            (
                CoreRecipientDeliveryInput {
                    relay_reject_streak: CONTACT_RELAY_STALE_STREAK,
                    relay_rejected_at_ms: NOW,
                    ..waiting(2)
                },
                CorePersonAttention::SetupRejected,
            ),
            (
                CoreRecipientDeliveryInput {
                    relay: CoreRelayPathState::PassExpired,
                    own_relay_usable: false,
                    ..waiting(2)
                },
                CorePersonAttention::PassBlocked,
            ),
            (
                CoreRecipientDeliveryInput {
                    oversized_waiting: true,
                    ..waiting(1)
                },
                CorePersonAttention::MessageTooLarge,
            ),
            (
                CoreRecipientDeliveryInput {
                    oldest_waiting_ms: NOW - RELAY_DELIVERY_DELAYED_THRESHOLD_MS,
                    last_progress_ms: 0,
                    ..waiting(2)
                },
                CorePersonAttention::Delayed,
            ),
        ];
        for (input, want) in cases {
            let line = core_classify_recipient_delivery(input).unwrap();
            assert_eq!(line.attention, Some(want));
        }

        // And the verdict feeds the grouping directly, without a second
        // opinion about who needs attention.
        let line = core_classify_recipient_delivery(CoreRecipientDeliveryInput {
            relay_reject_streak: CONTACT_RELAY_STALE_STREAK,
            relay_rejected_at_ms: NOW,
            ..waiting(2)
        })
        .unwrap();
        let groups = core_group_people(
            vec![CorePersonHealthInput {
                direct_link: Some(CoreDirectLink::Bluetooth),
                attention: line.attention,
                attention_since_ms: line.oldest_waiting_ms,
                ..person("Ash", 1)
            }],
            true,
            NOW,
        );
        assert_eq!(ids(&groups.needs_attention), vec![1]);
    }

    #[test]
    fn the_line_carries_the_count_and_age_the_copy_needs() {
        let line = core_classify_recipient_delivery(waiting(7)).unwrap();
        assert_eq!(line.count, 7);
        assert_eq!(line.oldest_waiting_ms, NOW - 60_000);
    }

    #[test]
    fn the_phase_one_door_can_never_reach_delayed_or_blocked() {
        // One decision procedure, two doors. The old entry point cannot
        // produce a fault or a delay because it has no evidence for one --
        // not because a second implementation withholds it.
        for relay in [
            CoreRelayPathState::Connected,
            CoreRelayPathState::SyncingSlowed,
            CoreRelayPathState::WaitingForInternet,
            CoreRelayPathState::Unreachable,
            CoreRelayPathState::PassExpired,
            CoreRelayPathState::PassSuspended,
            CoreRelayPathState::SetupRejected,
            CoreRelayPathState::StorageFull,
            CoreRelayPathState::Checking,
            CoreRelayPathState::NotSetUp,
        ] {
            for own_relay_usable in [true, false] {
                for direct_link in [true, false] {
                    for contact_relay_stale in [true, false] {
                        let state = core_classify_delivery_line(CoreDeliveryLineInput {
                            relay,
                            own_relay_usable,
                            direct_link,
                            contact_relay_stale,
                            ..queued(3)
                        });
                        assert!(matches!(
                            state,
                            None | Some(CoreDeliveryState::Sending)
                                | Some(CoreDeliveryState::WaitingForInternet)
                                | Some(CoreDeliveryState::WillDeliverWhenReconnected)
                        ));
                    }
                }
            }
        }
    }

    #[test]
    fn a_resting_endpoint_is_either_half_of_the_persisted_health() {
        use crate::contact_relay_health::{
            CONTACT_RELAY_RECHECK_MS, CONTACT_RELAY_STALE_STREAK,
            CONTACT_RELAY_UNREACHABLE_REST_MS, CONTACT_RELAY_UNREACHABLE_STREAK,
        };
        // (reject streak, rejected at, unreachable streak, unreachable at,
        //  resting, what it is)
        let cases: [(i64, i64, i64, i64, bool, &str); 6] = [
            (0, 0, 0, 0, false, "a healthy endpoint rests for nothing"),
            (
                CONTACT_RELAY_STALE_STREAK,
                NOW,
                0,
                0,
                true,
                "just written off after rejecting us",
            ),
            (
                CONTACT_RELAY_STALE_STREAK,
                NOW - CONTACT_RELAY_RECHECK_MS,
                0,
                0,
                false,
                "written off, but the re-probe is due again",
            ),
            (
                0,
                0,
                CONTACT_RELAY_UNREACHABLE_STREAK,
                NOW,
                true,
                "gone quiet, still inside its rest window",
            ),
            (
                0,
                0,
                CONTACT_RELAY_UNREACHABLE_STREAK,
                NOW - CONTACT_RELAY_UNREACHABLE_REST_MS,
                false,
                "gone quiet, rested, worth one more request",
            ),
            (
                CONTACT_RELAY_STALE_STREAK,
                NOW - CONTACT_RELAY_RECHECK_MS,
                CONTACT_RELAY_UNREACHABLE_STREAK,
                NOW,
                true,
                "either half resting is enough to rest the endpoint",
            ),
        ];
        for (reject, rejected_at, unreachable, unreachable_at, resting, what) in cases {
            assert_eq!(
                core_contact_endpoint_resting(
                    reject,
                    rejected_at,
                    unreachable,
                    unreachable_at,
                    NOW
                ),
                resting,
                "{what}"
            );
        }
    }

    #[test]
    fn the_best_route_is_the_routers_answer_and_never_the_pages() {
        // (direct link, own relay usable, they have an endpoint, resting,
        //  route, what it is)
        let cases: [(
            Option<CoreDirectLink>,
            bool,
            bool,
            bool,
            CorePersonRoute,
            &str,
        ); 7] = [
            (
                Some(CoreDirectLink::Bluetooth),
                false,
                false,
                true,
                CorePersonRoute::DirectBluetooth,
                "a live radio beats every internet consideration",
            ),
            (
                Some(CoreDirectLink::LocalWifi),
                true,
                true,
                false,
                CorePersonRoute::DirectLocalWifi,
                "the local link is named, not the pass behind it",
            ),
            (
                None,
                true,
                true,
                false,
                CorePersonRoute::ShorePass,
                "our pass works and their endpoint is live",
            ),
            (
                None,
                false,
                true,
                false,
                CorePersonRoute::NoneNow,
                "their endpoint is fine; ours cannot post",
            ),
            (
                None,
                true,
                false,
                false,
                CorePersonRoute::NoneNow,
                "no endpoint on their card: internet reaches nobody",
            ),
            (
                None,
                true,
                true,
                true,
                CorePersonRoute::NoneNow,
                "their endpoint is resting, so it is not a route today",
            ),
            (
                None,
                false,
                false,
                true,
                CorePersonRoute::NoneNow,
                "nothing anywhere, which is still not a fault",
            ),
        ];
        for (direct, own_relay_usable, has_endpoint, resting, route, what) in cases {
            assert_eq!(
                core_person_best_route(direct, own_relay_usable, has_endpoint, resting),
                route,
                "{what}"
            );
        }
    }

    #[test]
    fn the_best_route_agrees_with_the_delivery_verdict() {
        // The property the person detail depends on: if the route answer says
        // there is one, the delivery line says the work is moving. Two
        // sentences in the same expansion disagreeing about whether a friend
        // is reachable is exactly the contradiction this page replaces.
        for own_relay_usable in [true, false] {
            for has_endpoint in [true, false] {
                for direct_link in [true, false] {
                    for reject_streak in [0, CONTACT_RELAY_STALE_STREAK_FOR_TEST] {
                        let resting = core_contact_endpoint_resting(reject_streak, NOW, 0, 0, NOW);
                        let route = core_person_best_route(
                            direct_link.then_some(CoreDirectLink::Bluetooth),
                            own_relay_usable,
                            has_endpoint,
                            resting,
                        );
                        let line = core_classify_recipient_delivery(CoreRecipientDeliveryInput {
                            waiting_count: 2,
                            unposted_waiting_count: 2,
                            oldest_waiting_ms: NOW - 60_000,
                            last_progress_ms: NOW - 60_000,
                            oversized_waiting: false,
                            relay_reject_streak: reject_streak,
                            relay_rejected_at_ms: NOW,
                            relay_unreachable_streak: 0,
                            relay_unreachable_at_ms: 0,
                            relay: CoreRelayPathState::Connected,
                            own_relay_usable,
                            contact_has_relay_endpoint: has_endpoint,
                            direct_link,
                            now_ms: NOW,
                        })
                        .expect("two messages are waiting");
                        assert_eq!(
                            route != CorePersonRoute::NoneNow,
                            line.state == CoreDeliveryState::Sending,
                            "route {route:?} and state {:?} disagree",
                            line.state,
                        );
                    }
                }
            }
        }
    }

    /// Local alias so the loop above reads without an import at the top of a
    /// module that otherwise names no streak thresholds.
    const CONTACT_RELAY_STALE_STREAK_FOR_TEST: i64 =
        crate::contact_relay_health::CONTACT_RELAY_STALE_STREAK;

    #[test]
    fn health_evidence_feeds_the_people_grouping_without_a_second_opinion() {
        // The wiring the spec asks for: one relay verdict, used twice.
        let report = core_classify_connection_health(CoreConnectionHealthInput {
            relay: CoreRelayPathState::Connected,
            validated_internet: false,
            ..healthy()
        });
        let groups = core_group_people(
            vec![CorePersonHealthInput {
                presence_last_seen_ms: NOW - 1_000,
                ..person("Ash", 1)
            }],
            report.evidence.own_relay_usable,
            NOW,
        );
        // No internet here, so "seen online" cannot be claimed.
        assert!(groups.reachable_now.is_empty());
        assert_eq!(ids(&groups.other_people), vec![1]);
    }
}
