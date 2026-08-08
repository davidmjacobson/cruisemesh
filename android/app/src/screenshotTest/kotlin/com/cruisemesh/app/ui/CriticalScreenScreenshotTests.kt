package com.cruisemesh.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.chat.MessageComposer
import uniffi.cruisemesh_core.CoreConnectionHealth
import uniffi.cruisemesh_core.CoreDeliveryBlockedReason
import uniffi.cruisemesh_core.CoreDeliveryLine
import uniffi.cruisemesh_core.CoreDeliveryState
import uniffi.cruisemesh_core.CoreDirectPathState
import uniffi.cruisemesh_core.CoreHealthAction
import uniffi.cruisemesh_core.CoreHealthReason
import uniffi.cruisemesh_core.CorePersonAttention
import uniffi.cruisemesh_core.CorePersonRoute
import uniffi.cruisemesh_core.CoreRelayPathState
import uniffi.cruisemesh_core.PeerConnectionTransport

@Preview(name = "terms_compact", widthDp = 360, heightDp = 640, showBackground = true)
@Preview(name = "terms_compact_large_font", widthDp = 360, heightDp = 640, fontScale = 1.3f, showBackground = true)
@Composable
fun TermsScreenshot() {
    CruiseMeshTheme {
        TermsAcceptanceScreen(onAccept = {})
    }
}

@Preview(name = "onboarding_compact", widthDp = 360, heightDp = 640, showBackground = true)
@Preview(name = "onboarding_compact_large_font", widthDp = 360, heightDp = 640, fontScale = 1.3f, showBackground = true)
@Composable
fun OnboardingScreenshot() {
    CruiseMeshTheme {
        OnboardingScreen(
            userId = ByteArray(32) { 1 },
            displayId = "CM-K7QX-9M2P-3F8J-QRTZ-AB",
            displayName = "",
            avatarPath = null,
            meshPermissionsGranted = false,
            batteryExemptionGranted = false,
            onDisplayNameChange = {},
            onTakePhoto = {},
            onChoosePhoto = {},
            onRemovePhoto = {},
            onRequestMeshPermissions = {},
            onRequestBatteryExemption = {},
            onRestore = {},
            onComplete = {},
        )
    }
}

@Preview(name = "composer_empty", widthDp = 360, heightDp = 120, showBackground = true)
@Composable
fun EmptyComposerScreenshot() {
    ComposerFrame(draft = "", hasPendingAttachment = false)
}

@Preview(name = "composer_caption", widthDp = 360, heightDp = 160, fontScale = 1.3f, showBackground = true)
@Composable
fun CaptionComposerScreenshot() {
    ComposerFrame(draft = "Meet by the pool after dinner", hasPendingAttachment = true)
}

@Composable
private fun ComposerFrame(draft: String, hasPendingAttachment: Boolean) {
    CruiseMeshTheme {
        Column(modifier = Modifier.fillMaxSize().padding(horizontal = 12.dp, vertical = 8.dp)) {
            MessageComposer(
                draft = draft,
                onDraftChange = {},
                onSend = {},
                hasPendingAttachment = hasPendingAttachment,
                ownBubbleColor = Color(0xFF236A5B),
                onPickGallery = {},
                onPickCamera = {},
                onStartVoice = { false },
                onStopVoice = {},
                onCancelVoice = {},
            )
        }
    }
}

// ---------------------------------------------------------------------------
// Connection details
// ---------------------------------------------------------------------------

/**
 * Every name and time below is synthetic, and the clock is frozen. Real family
 * names must never reach a fixture in a public repository, and a fixture that
 * read the wall clock would re-render differently on every run.
 *
 * Times deliberately stay inside the fixture's "today" so the rows exercise
 * only the relative-time buckets. Absolute dates would drag the host's locale
 * and time zone into a reference image; `ConnectionDetailsLogicTest` covers
 * the yesterday/older buckets instead.
 */
private const val FIXTURE_NOW_MS = 1_760_000_000_000L
private const val FIXTURE_START_OF_TODAY_MS = FIXTURE_NOW_MS - 10 * 60 * 60 * 1000L

/** Waiting work in whichever of the core's shapes a fixture needs. */
private fun fixtureDelivery(
    count: Int,
    state: CoreDeliveryState = CoreDeliveryState.WILL_DELIVER_WHEN_RECONNECTED,
    delayed: Boolean = false,
    blockedReason: CoreDeliveryBlockedReason? = null,
    attention: CorePersonAttention? = null,
    waitingForMs: Long = 14 * 60_000L,
) = CoreDeliveryLine(
    count = count.toUInt(),
    state = state,
    delayed = delayed,
    blockedReason = blockedReason,
    attention = attention,
    oldestWaitingMs = FIXTURE_NOW_MS - waitingForMs,
)

private fun fixturePerson(
    name: String,
    status: PersonStatus,
    badge: ConnectionPathBadge? = null,
    delivery: CoreDeliveryLine? = null,
    bestRoute: CorePersonRoute = CorePersonRoute.NONE_NOW,
    lastSeenMs: Long = 0L,
    lastDeliveredMs: Long = 0L,
) = ConnectionPersonRow(
    userIdHex = name.lowercase().filter { it.isLetterOrDigit() },
    name = name,
    status = status,
    badge = badge,
    delivery = delivery,
    attention = delivery?.attention,
    detail = PersonDetail(
        bestRoute = bestRoute,
        lastSeenMs = lastSeenMs,
        lastDeliveredMs = lastDeliveredMs,
    ),
)

private fun fixtureState(
    health: HealthCardState,
    paths: PathsCardState,
    needsAttention: List<ConnectionPersonRow> = emptyList(),
    reachableNow: List<ConnectionPersonRow> = emptyList(),
    otherPeople: List<ConnectionPersonRow> = emptyList(),
) = ConnectionDetailsState(
    health = health,
    paths = paths,
    needsAttention = needsAttention,
    reachableNow = reachableNow,
    otherPeople = otherPeople,
    hasContacts = needsAttention.isNotEmpty() ||
        reachableNow.isNotEmpty() ||
        otherPeople.isNotEmpty(),
    activity = listOf(
        ConnectionActivityRow(
            name = "Riley's phone",
            evidence = PeerEvidence.MESSAGE_DELIVERED,
            transport = PeerConnectionTransport.LOCAL_WIFI,
            atMs = FIXTURE_NOW_MS - 12 * 60_000L,
        ),
    ),
    updatedAtMs = FIXTURE_NOW_MS - 20_000L,
    refreshing = false,
)

private val readyState = fixtureState(
    health = HealthCardState(
        state = CoreConnectionHealth.READY,
        nearbyFriendCount = 2,
        bluetooth = CoreDirectPathState.AVAILABLE,
        relay = CoreRelayPathState.CONNECTED,
        reason = null,
        action = null,
    ),
    paths = PathsCardState(
        bluetooth = CoreDirectPathState.AVAILABLE,
        bluetoothLinks = 1,
        bluetoothAudioActive = false,
        localWifiLinks = 1,
        relay = CoreRelayPathState.CONNECTED,
        relayLastSyncMs = FIXTURE_NOW_MS - 90_000L,
    ),
    reachableNow = listOf(
        // A live link is also the best route to that person: both come out of
        // the same core answer, so a fixture showing them disagreeing would
        // bless a contradiction the page cannot actually produce.
        fixturePerson(
            "Riley's phone",
            PersonStatus.ConnectedNow,
            ConnectionPathBadge.LOCAL_WIFI,
            bestRoute = CorePersonRoute.DIRECT_LOCAL_WIFI,
            lastSeenMs = FIXTURE_NOW_MS - 60_000L,
            lastDeliveredMs = FIXTURE_NOW_MS - 26 * 60_000L,
        ),
        fixturePerson(
            "Sam",
            PersonStatus.ConnectedNow,
            ConnectionPathBadge.BLUETOOTH,
            bestRoute = CorePersonRoute.DIRECT_BLUETOOTH,
            lastSeenMs = FIXTURE_NOW_MS - 2 * 60_000L,
        ),
    ),
    otherPeople = listOf(
        fixturePerson(
            "Ash",
            PersonStatus.History(PeerEvidence.MESSAGE_DELIVERED, FIXTURE_NOW_MS - 12 * 60_000L),
            ConnectionPathBadge.SHORE_PASS,
            bestRoute = CorePersonRoute.SHORE_PASS,
            lastSeenMs = FIXTURE_NOW_MS - 12 * 60_000L,
            lastDeliveredMs = FIXTURE_NOW_MS - 12 * 60_000L,
        ),
        fixturePerson("Dana", PersonStatus.NoHistory),
    ),
)

private val limitedState = fixtureState(
    health = HealthCardState(
        state = CoreConnectionHealth.LIMITED,
        nearbyFriendCount = 1,
        bluetooth = CoreDirectPathState.AVAILABLE,
        relay = CoreRelayPathState.WAITING_FOR_INTERNET,
        reason = CoreHealthReason.WAITING_FOR_INTERNET,
        action = null,
    ),
    paths = PathsCardState(
        bluetooth = CoreDirectPathState.AVAILABLE,
        bluetoothLinks = 1,
        bluetoothAudioActive = true,
        localWifiLinks = 0,
        relay = CoreRelayPathState.WAITING_FOR_INTERNET,
        relayLastSyncMs = FIXTURE_NOW_MS - 3 * 60 * 60 * 1000L,
    ),
    reachableNow = listOf(
        fixturePerson("Sam", PersonStatus.ConnectedNow, ConnectionPathBadge.BLUETOOTH),
    ),
    otherPeople = listOf(
        fixturePerson(
            "Ash",
            PersonStatus.History(PeerEvidence.CONNECTED, FIXTURE_NOW_MS - 45 * 60_000L),
            ConnectionPathBadge.BLUETOOTH,
            fixtureDelivery(2, CoreDeliveryState.WAITING_FOR_INTERNET),
        ),
    ),
)

private val needsAttentionState = fixtureState(
    health = HealthCardState(
        state = CoreConnectionHealth.NEEDS_ATTENTION,
        nearbyFriendCount = 0,
        bluetooth = CoreDirectPathState.OFF,
        relay = CoreRelayPathState.WAITING_FOR_INTERNET,
        reason = CoreHealthReason.BLUETOOTH_OFF,
        action = CoreHealthAction.TURN_ON_BLUETOOTH,
    ),
    paths = PathsCardState(
        bluetooth = CoreDirectPathState.OFF,
        bluetoothLinks = 0,
        bluetoothAudioActive = false,
        localWifiLinks = 0,
        relay = CoreRelayPathState.WAITING_FOR_INTERNET,
        relayLastSyncMs = FIXTURE_NOW_MS - 4 * 60 * 60 * 1000L,
    ),
    // The three treatments that have to be distinguishable at a glance and
    // must never all be red: a friend whose own setup needs repairing, a
    // working path that has stalled, and ordinary waiting.
    needsAttention = listOf(
        fixturePerson(
            "Alex",
            PersonStatus.History(PeerEvidence.CONNECTED, FIXTURE_NOW_MS - 3 * 60 * 60 * 1000L),
            delivery = fixtureDelivery(
                count = 2,
                blockedReason = CoreDeliveryBlockedReason.CONTACT_SETUP_REJECTED,
                attention = CorePersonAttention.SETUP_REJECTED,
            ),
            lastSeenMs = FIXTURE_NOW_MS - 3 * 60 * 60 * 1000L,
        ),
        fixturePerson(
            "Ash",
            PersonStatus.History(PeerEvidence.PRESENCE_SEEN, FIXTURE_NOW_MS - 40 * 60_000L),
            ConnectionPathBadge.SHORE_PASS,
            delivery = fixtureDelivery(
                count = 1,
                state = CoreDeliveryState.SENDING,
                delayed = true,
                attention = CorePersonAttention.DELAYED,
                waitingForMs = 22 * 60_000L,
            ),
            bestRoute = CorePersonRoute.SHORE_PASS,
            lastSeenMs = FIXTURE_NOW_MS - 40 * 60_000L,
        ),
    ),
    otherPeople = listOf(
        fixturePerson(
            "Sam",
            PersonStatus.History(PeerEvidence.DISCONNECTED, FIXTURE_NOW_MS - 8 * 60_000L),
            ConnectionPathBadge.BLUETOOTH,
            fixtureDelivery(3),
            lastSeenMs = FIXTURE_NOW_MS - 8 * 60_000L,
        ),
    ),
)

private val longListState = fixtureState(
    health = readyState.health,
    paths = readyState.paths,
    reachableNow = listOf(
        fixturePerson("Sam", PersonStatus.ConnectedNow, ConnectionPathBadge.BLUETOOTH),
    ),
    otherPeople = listOf("Ada", "Ash", "Bo", "Cam", "Dana", "Eli", "Fern", "Gil", "Hana", "Ivo")
        .mapIndexed { index, name ->
            fixturePerson(
                name,
                PersonStatus.History(
                    PeerEvidence.PRESENCE_SEEN,
                    FIXTURE_NOW_MS - (index + 1) * 7 * 60_000L,
                ),
                ConnectionPathBadge.SHORE_PASS,
            )
        },
)

@Composable
private fun ConnectionDetailsFixture(
    state: ConnectionDetailsState,
    expandedPersonHex: String? = null,
    expandedPersonEvents: List<ConnectionActivityRow>? = null,
) {
    CruiseMeshTheme {
        // The real page sits inside a Scaffold, which paints the themed
        // background; the preview has to supply it or the dark reference
        // renders light text on a white page and proves nothing.
        androidx.compose.material3.Surface(
            modifier = Modifier.fillMaxSize(),
            color = androidx.compose.material3.MaterialTheme.colorScheme.background,
        ) {
            ConnectionDetailsContent(
                state = state,
                nowMs = FIXTURE_NOW_MS,
                startOfTodayMs = FIXTURE_START_OF_TODAY_MS,
                expandedPersonHex = expandedPersonHex,
                expandedPersonEvents = expandedPersonEvents,
            )
        }
    }
}

@Preview(name = "connection_ready", widthDp = 360, heightDp = 900, showBackground = true)
@Preview(
    name = "connection_ready_dark",
    widthDp = 360,
    heightDp = 900,
    showBackground = true,
    uiMode = android.content.res.Configuration.UI_MODE_NIGHT_YES,
)
// Tall enough at 200 percent scaling to capture the people rows, not just the
// health card: truncation there is exactly what the reference has to catch.
@Preview(
    name = "connection_ready_large_font",
    widthDp = 360,
    heightDp = 2200,
    fontScale = 2.0f,
    showBackground = true,
)
@Composable
fun ConnectionDetailsReadyScreenshot() {
    ConnectionDetailsFixture(readyState)
}

@Preview(name = "connection_limited", widthDp = 360, heightDp = 900, showBackground = true)
@Composable
fun ConnectionDetailsLimitedScreenshot() {
    ConnectionDetailsFixture(limitedState)
}

@Preview(
    name = "connection_needs_attention",
    widthDp = 360,
    heightDp = 900,
    showBackground = true,
)
@Composable
fun ConnectionDetailsNeedsAttentionScreenshot() {
    ConnectionDetailsFixture(needsAttentionState)
}

@Preview(name = "connection_long_list", widthDp = 360, heightDp = 900, showBackground = true)
@Composable
fun ConnectionDetailsLongListScreenshot() {
    ConnectionDetailsFixture(longListState)
}

@Preview(
    name = "connection_person_expanded",
    widthDp = 360,
    heightDp = 900,
    showBackground = true,
)
@Composable
fun ConnectionDetailsPersonExpandedScreenshot() {
    ConnectionDetailsFixture(
        state = readyState,
        expandedPersonHex = "rileysphone",
        expandedPersonEvents = listOf(
            ConnectionActivityRow(
                name = "Riley's phone",
                evidence = PeerEvidence.CONNECTED,
                transport = PeerConnectionTransport.LOCAL_WIFI,
                atMs = FIXTURE_NOW_MS - 4 * 60_000L,
            ),
            ConnectionActivityRow(
                name = "Riley's phone",
                evidence = PeerEvidence.MESSAGE_DELIVERED,
                transport = PeerConnectionTransport.CARRIED,
                atMs = FIXTURE_NOW_MS - 26 * 60_000L,
            ),
        ),
    )
}
