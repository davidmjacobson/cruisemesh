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

    let pending = input.runtime == CoreMeshRuntime::Starting
        || bluetooth == CoreDirectPathState::Starting
        || input.local_wifi == CoreDirectPathState::Starting
        || relay == CoreRelayPathState::Checking;

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
/// Phase 1 shells supply only `None` or [`CorePersonAttention::SetupRejected`]
/// -- the one per-person fault the app already tracks. The remaining variants
/// exist so the per-recipient delivery read model can fill them in later
/// without reshaping this API, which is also why the grouping call already
/// takes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, uniffi::Enum)]
pub enum CorePersonAttention {
    /// A usable route exists but nothing has progressed for the delayed
    /// window. The mildest reason: it often clears itself.
    Delayed,
    /// A queued message exceeds the size cap and can never post as-is.
    MessageTooLarge,
    /// Our own pass cannot post on their behalf (expired, suspended, or the
    /// family's storage is full).
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
