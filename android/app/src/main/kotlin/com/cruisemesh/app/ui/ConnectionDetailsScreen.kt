package com.cruisemesh.app.ui

import android.content.Intent
import androidx.annotation.PluralsRes
import androidx.annotation.StringRes
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Info
import androidx.compose.material.icons.filled.KeyboardArrowDown
import androidx.compose.material.icons.filled.KeyboardArrowUp
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.minimumInteractiveComponentSize
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.clearAndSetSemantics
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.R
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.debug.ConflictDiagnosticsExport
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.debug.DiagnosticsShare
import com.cruisemesh.app.debug.FieldMetricsExport
import com.cruisemesh.app.mesh.LanTransportDiagnostics
import com.cruisemesh.app.mesh.MeshConnectivityStatus
import com.cruisemesh.app.mesh.MeshRuntimeStatus
import com.cruisemesh.app.mesh.RelaySyncEvents
import com.cruisemesh.app.relay.RelayConfigStore
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.cruisemesh_core.CoreConnectionHealth
import uniffi.cruisemesh_core.CoreDeliveryState
import uniffi.cruisemesh_core.CoreDirectPathState
import uniffi.cruisemesh_core.CoreHealthAction
import uniffi.cruisemesh_core.CoreHealthReason
import uniffi.cruisemesh_core.CoreRelayPathState
import uniffi.cruisemesh_core.MessageStore
import uniffi.cruisemesh_core.PeerConnectionTransport
import uniffi.cruisemesh_core.coreContactDisplayName
import java.text.DateFormat
import java.util.Calendar
import java.util.Date

/**
 * The Connection details page.
 *
 * Reads state; it does not change it. Opening the page starts no scan, no
 * advertising change, and no sync — the single exception is pull-to-refresh,
 * which the user performs deliberately and which asks for exactly one bounded
 * sync pass through the existing [RelaySyncEvents] plumbing.
 *
 * Live signals (runtime, transports, relay health, presence) come straight off
 * their observable state and land on screen within a frame. Everything that
 * needs the store — people, waiting work, activity — is loaded on a background
 * dispatcher, coalesced through [StoreChangeCoalescer], and never queried on
 * the main thread. Those rules are not decoration: this page sits on the same
 * store and the same event stream that has already driven the app into
 * input-dispatch ANRs when a mesh flood was allowed to reload it unthrottled.
 *
 * All interpretation lives in the core (`core/src/connection_health.rs`) and
 * all copy lives in `strings.xml`. This file is the join between them.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ConnectionDetailsScreen(
    onBack: () -> Unit,
    onStartMesh: () -> Unit = {},
    onManageShorePass: () -> Unit = {},
    onTurnOnBluetooth: () -> Unit = {},
) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }

    // Live, observable signals. The transports map is the one the router
    // actually sends on, so a LAN->Bluetooth handoff flips the rows instead of
    // freezing them on a dead link.
    val runtimeState by MeshRuntimeStatus.state.collectAsState()
    val bluetoothAudio by MeshRuntimeStatus.bluetoothAudioConnected.collectAsState()
    val transports by MeshConnectivityStatus.nearbyTransports.collectAsState()
    val relayHealth by MeshConnectivityStatus.relay.collectAsState()
    val staleRelayContacts by MeshConnectivityStatus.staleRelayContacts.collectAsState()
    val presenceLastSeen by MeshConnectivityStatus.presenceLastSeen.collectAsState()
    val contactLastSeen by MeshConnectivityStatus.contactLastSeen.collectAsState()
    val lanState by LanTransportDiagnostics.state.collectAsState()
    val relayConfigured = remember { RelayConfigStore.load(context) != null }

    val resumed = rememberPageResumed()
    val nowMs = rememberPageClock(resumed)

    var snapshot by remember { mutableStateOf(ConnectionStoreSnapshot.EMPTY) }
    var refreshing by remember { mutableStateOf(false) }
    // Separate from [refreshing]: the pull indicator belongs to the gesture,
    // and a background poll flashing it every few seconds would read as the
    // page constantly reloading itself.
    var pullRefreshing by remember { mutableStateOf(false) }
    val coalescer = remember { StoreChangeCoalescer() }
    // Conflated: a burst of signals collapses into one pending reload, which
    // is the same guarantee the coalescer makes, stated twice on purpose.
    val requests = remember { Channel<Unit>(Channel.CONFLATED) }
    // Touched only from the composition's main dispatcher (the reload loop,
    // the poll tick, and pull-to-refresh all run there), so the coalescer
    // needs no locking.
    val signal = remember(coalescer, requests) {
        {
            if (coalescer.onSignal(System.currentTimeMillis())) {
                requests.trySend(Unit)
            }
        }
    }

    // The loop lives in ConnectionDetailsLogic so its lifecycle -- what
    // survives a pause, what is owed after a cancellation -- has a test. It is
    // cancelled on every ON_PAUSE and restarted on ON_RESUME, and it resets the
    // coalescer on the way in for exactly that reason.
    LaunchedEffect(resumed) {
        if (!resumed) return@LaunchedEffect
        runConnectionRefreshLoop(
            coalescer = coalescer,
            requests = requests,
            signal = signal,
            nowMs = System::currentTimeMillis,
            pollIntervalMs = STORE_POLL_INTERVAL_MS,
            onRefreshingChanged = { running ->
                refreshing = running
                if (!running) pullRefreshing = false
            },
            // The only place this page touches the store, and it is never the
            // main thread.
            load = {
                withContext(Dispatchers.IO) {
                    loadConnectionSnapshot(store, System.currentTimeMillis())
                }
            },
            onLoaded = { snapshot = it },
        )
    }

    val lanListening = lanState.localEndpoint != null
    val checkingClock = remember { CheckingClock() }
    // The same clock the classification is given: a mark stamped from a fresher
    // clock than `nowMs` would look like it came from the future and resolve the
    // bound instantly, so Checking would never be shown.
    val checkingSinceMs = checkingClock.mark(
        connectionCheckPending(
            ConnectionInputs.runtime(runtimeState),
            ConnectionInputs.bluetooth(runtimeState),
            ConnectionInputs.localWifi(runtimeState, lanListening),
            ConnectionInputs.relay(relayHealth, relayConfigured),
        ),
        nowMs,
    )
    // Remembered on its inputs rather than recomputed per recomposition. Two
    // FFI round trips marshalling the whole address book each way is not work
    // to repeat because an unrelated disclosure section was expanded -- and the
    // observables above emit at mesh-flood rates, so "once per recomposition"
    // is the wrong budget for it.
    val state = remember(
        runtimeState,
        transports,
        relayHealth,
        lanListening,
        bluetoothAudio,
        staleRelayContacts,
        presenceLastSeen,
        contactLastSeen,
        snapshot,
        checkingSinceMs,
        refreshing,
        nowMs,
    ) {
        buildConnectionDetailsState(
            runtimeState = runtimeState,
            transports = transports,
            relayHealth = relayHealth,
            relayConfigured = relayConfigured,
            lanListening = lanListening,
            bluetoothAudioActive = bluetoothAudio,
            staleRelayContacts = staleRelayContacts,
            presenceLastSeen = presenceLastSeen,
            contactLastSeen = contactLastSeen,
            snapshot = snapshot,
            checkingSinceMs = checkingSinceMs,
            refreshing = refreshing,
            nowMs = nowMs,
        )
    }

    var showClear by remember { mutableStateOf(false) }
    var troubleshootingExpanded by remember { mutableStateOf(false) }
    var howToFixReason by remember { mutableStateOf<CoreHealthReason?>(null) }
    val scope = rememberCoroutineScope()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_connection_details)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.AutoMirrored.Filled.ArrowBack,
                            contentDescription = stringResource(R.string.ui_back),
                        )
                    }
                },
            )
        },
    ) { innerPadding ->
        PullToRefreshBox(
            isRefreshing = pullRefreshing,
            onRefresh = {
                pullRefreshing = true
                signal()
                // The one deliberate "check again now" on this page: a single
                // bounded pass, the same one a network change would trigger.
                RelaySyncEvents.requestSync()
            },
            modifier = Modifier.fillMaxSize().padding(innerPadding),
        ) {
            ConnectionDetailsContent(
                state = state,
                nowMs = nowMs,
                startOfTodayMs = rememberStartOfToday(nowMs),
                troubleshootingExpanded = troubleshootingExpanded,
                howToFixReason = howToFixReason,
                onToggleTroubleshooting = { troubleshootingExpanded = !troubleshootingExpanded },
                onHealthAction = { action ->
                    when (action) {
                        CoreHealthAction.START_MESH -> onStartMesh()
                        CoreHealthAction.TURN_ON_BLUETOOTH -> onTurnOnBluetooth()
                        CoreHealthAction.MANAGE_SHORE_PASS -> onManageShorePass()
                        CoreHealthAction.HOW_TO_FIX -> {
                            // Never drop someone at the top of a long section
                            // to hunt for the answer: expand it *and* name the
                            // reason inside it.
                            howToFixReason = state.health.reason
                            troubleshootingExpanded = true
                        }
                    }
                },
                onClearHistory = { showClear = true },
                onStoreChanged = signal,
            )
        }
    }

    if (showClear) {
        AlertDialog(
            onDismissRequest = { showClear = false },
            title = { Text(stringResource(R.string.ui_clear_connection_history_confirm)) },
            text = { Text(stringResource(R.string.ui_this_removes_local_connection_events_and_per_person)) },
            confirmButton = {
                TextButton(
                    onClick = {
                        showClear = false
                        // A delete over the whole event table, plus the wait for
                        // a store lock the receive path also wants: not work for
                        // the thread that has to keep answering taps.
                        scope.launch {
                            withContext(Dispatchers.IO) {
                                runCatching { store.clearPeerConnectionHistory() }
                            }
                            signal()
                        }
                    },
                ) { Text(stringResource(R.string.ui_clear)) }
            },
            dismissButton = {
                TextButton(onClick = { showClear = false }) {
                    Text(stringResource(R.string.ui_cancel))
                }
            },
        )
    }
}

/**
 * How often the polling fallback asks for a reload while the page is visible.
 *
 * Four seconds, not five: the coalescing window adds up to another 500 ms
 * before a load even starts, and the acceptance criterion is that a newly
 * recorded connection event appears *within* five. A tick exactly at the
 * budget spends the whole of it and then some.
 */
private const val STORE_POLL_INTERVAL_MS = 4_000L

/**
 * How often relative times, the freshness label, and the bounded Checking
 * state are re-evaluated.
 *
 * The spec asks for the freshness label to move at least once a minute; ten
 * seconds is faster because the same clock also decides when `Checking`
 * resolves, and a card that stayed on a spinner for most of a minute after the
 * bound expired would defeat the bound.
 */
private const val CLOCK_TICK_MS = 10_000L

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/**
 * The whole page below the app bar, rendered from a finished view state.
 *
 * Separate from [ConnectionDetailsScreen] so the screenshot tests can feed it
 * synthetic fixtures without a store, a service, or a clock.
 */
@Composable
fun ConnectionDetailsContent(
    state: ConnectionDetailsState,
    nowMs: Long,
    startOfTodayMs: Long,
    troubleshootingExpanded: Boolean = false,
    howToFixReason: CoreHealthReason? = null,
    onToggleTroubleshooting: () -> Unit = {},
    onHealthAction: (CoreHealthAction) -> Unit = {},
    onClearHistory: () -> Unit = {},
    onStoreChanged: () -> Unit = {},
) {
    var otherPeopleExpanded by remember { mutableStateOf(false) }
    var activityExpanded by remember { mutableStateOf(false) }
    var showAllActivity by remember { mutableStateOf(false) }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp, vertical = 16.dp),
    ) {
        HealthCard(state.health, state.updatedAtMs, state.refreshing, nowMs, onHealthAction)

        Spacer(modifier = Modifier.height(18.dp))
        PathsCard(state.paths, nowMs, startOfTodayMs)

        if (state.reachableNow.isNotEmpty()) {
            Spacer(modifier = Modifier.height(18.dp))
            SectionHeading(
                pluralStringResource(
                    R.plurals.ui_section_reachable_now,
                    state.reachableNow.size,
                    state.reachableNow.size,
                ),
            )
            PeopleCard(state.reachableNow, nowMs, startOfTodayMs)
        }

        if (state.otherPeople.isNotEmpty()) {
            Spacer(modifier = Modifier.height(18.dp))
            SectionHeading(
                pluralStringResource(
                    R.plurals.ui_section_other_people,
                    state.otherPeople.size,
                    state.otherPeople.size,
                ),
            )
            val collapsed = state.otherPeople.size > CONNECTION_OTHER_PEOPLE_COLLAPSE_AT &&
                !otherPeopleExpanded
            PeopleCard(
                if (collapsed) {
                    state.otherPeople.take(CONNECTION_OTHER_PEOPLE_COLLAPSE_AT)
                } else {
                    state.otherPeople
                },
                nowMs,
                startOfTodayMs,
            )
            if (state.otherPeople.size > CONNECTION_OTHER_PEOPLE_COLLAPSE_AT) {
                val hidden = state.otherPeople.size - CONNECTION_OTHER_PEOPLE_COLLAPSE_AT
                val label = if (collapsed) {
                    pluralStringResource(R.plurals.ui_show_people, hidden, hidden)
                } else {
                    stringResource(R.string.ui_show_less)
                }
                TextButton(
                    onClick = { otherPeopleExpanded = !otherPeopleExpanded },
                    modifier = Modifier.fillMaxWidth().minimumInteractiveComponentSize(),
                ) { Text(label) }
            }
        }

        // Only once a snapshot has actually been read. "No friends added yet"
        // is a claim, and asserting it on the first frame of every open --
        // before the background load has returned -- is a false one for
        // everybody who has friends.
        if (state.updatedAtMs > 0L && !state.hasContacts) {
            Spacer(modifier = Modifier.height(18.dp))
            Text(
                stringResource(R.string.ui_no_friends_added_yet),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }

        Spacer(modifier = Modifier.height(18.dp))
        CollapsibleSection(
            title = stringResource(R.string.ui_section_recent_activity),
            // Collapsed, the newest event time is the only signal that
            // anything happened at all; without it the row gives a reader no
            // reason to open it.
            detail = state.activity.firstOrNull()?.let {
                eventTimeText(it.atMs, nowMs, startOfTodayMs)
            },
            expanded = activityExpanded,
            onToggle = { activityExpanded = !activityExpanded },
        ) {
            if (state.activity.isEmpty()) {
                Text(
                    stringResource(
                        R.string.ui_connection_activity_will_appear_here_as_cruisemesh_reaches,
                    ),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                val shown = if (showAllActivity) {
                    state.activity
                } else {
                    state.activity.take(CONNECTION_ACTIVITY_PREVIEW_COUNT)
                }
                shown.forEach { ActivityLine(it, nowMs, startOfTodayMs) }
                if (state.activity.size > CONNECTION_ACTIVITY_PREVIEW_COUNT) {
                    val label = if (showAllActivity) {
                        stringResource(R.string.ui_show_less)
                    } else {
                        stringResource(R.string.ui_show_all_activity)
                    }
                    TextButton(
                        onClick = { showAllActivity = !showAllActivity },
                        modifier = Modifier.fillMaxWidth().minimumInteractiveComponentSize(),
                    ) { Text(label) }
                }
            }
        }

        Spacer(modifier = Modifier.height(12.dp))
        CollapsibleSection(
            title = stringResource(R.string.ui_section_troubleshooting),
            expanded = troubleshootingExpanded,
            onToggle = onToggleTroubleshooting,
        ) {
            howToFixReason?.let { reason ->
                howToFixTextId(reason)?.let { id ->
                    Text(
                        stringResource(id),
                        style = MaterialTheme.typography.bodyMedium,
                        modifier = Modifier.padding(bottom = 12.dp),
                    )
                }
            }
            TroubleshootingControls(onClearHistory = onClearHistory, onStoreChanged = onStoreChanged)
        }

        Spacer(modifier = Modifier.height(24.dp))
    }
}

@Composable
private fun HealthCard(
    health: HealthCardState,
    updatedAtMs: Long,
    refreshing: Boolean,
    nowMs: Long,
    onAction: (CoreHealthAction) -> Unit,
) {
    val title = stringResource(healthTitleId(health.state))
    val evidence = healthEvidenceText(health)
    // One combined label so the interpretation is announced before its
    // evidence, and never as two unrelated items. Scoped to the two lines it
    // describes: merging the whole card would swallow the action button's own
    // label, which is the one thing on the card a person can act on.
    val label = stringResource(R.string.ui_a11y_two_sentences, title, evidence)
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.46f),
        ),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalAlignment = Alignment.Top,
        ) {
            HealthIcon(health.state)
            Spacer(modifier = Modifier.width(12.dp))
            Column(modifier = Modifier.weight(1f)) {
                Column(
                    modifier = Modifier
                        .fillMaxWidth()
                        .clearAndSetSemantics { contentDescription = label },
                ) {
                    Text(
                        title,
                        style = MaterialTheme.typography.titleMedium.copy(
                            fontWeight = FontWeight.SemiBold,
                        ),
                    )
                    Text(
                        evidence,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(top = 4.dp),
                    )
                }
                FreshnessLine(updatedAtMs, refreshing, nowMs)
                health.action?.let { action ->
                    Button(
                        onClick = { onAction(action) },
                        modifier = Modifier.padding(top = 12.dp).minimumInteractiveComponentSize(),
                    ) { Text(stringResource(healthActionLabelId(action))) }
                }
            }
        }
    }
}

@Composable
private fun HealthIcon(state: CoreConnectionHealth) {
    val palette = LocalReachabilityPalette.current
    if (state == CoreConnectionHealth.CHECKING) {
        CircularProgressIndicator(
            modifier = Modifier
                .size(24.dp)
                .semantics { contentDescription = "" },
            strokeWidth = 2.dp,
        )
        return
    }
    val icon: ImageVector
    val tint: Color
    when (state) {
        CoreConnectionHealth.READY -> {
            icon = Icons.Default.CheckCircle
            tint = palette.nearby
        }
        CoreConnectionHealth.LIMITED -> {
            icon = Icons.Default.Info
            tint = palette.recent
        }
        else -> {
            icon = PassExclamationIcon
            tint = MaterialTheme.colorScheme.error
        }
    }
    // The title beside it already names the state in words; announcing the
    // icon too would read the same thing twice.
    Icon(icon, contentDescription = null, tint = tint, modifier = Modifier.size(24.dp))
}

@Composable
private fun FreshnessLine(updatedAtMs: Long, refreshing: Boolean, nowMs: Long) {
    val label = when (val freshness = ConnectionTimes.freshness(updatedAtMs, nowMs)) {
        is FreshnessLabel.Never -> null
        is FreshnessLabel.JustNow -> stringResource(R.string.ui_updated_just_now)
        is FreshnessLabel.Minutes -> pluralStringResource(
            R.plurals.ui_updated_minutes_ago,
            freshness.value,
            freshness.value,
        )
        is FreshnessLabel.Hours -> pluralStringResource(
            R.plurals.ui_updated_hours_ago,
            freshness.value,
            freshness.value,
        )
    } ?: return
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.padding(top = 6.dp),
    ) {
        Text(
            label,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (refreshing) {
            Spacer(modifier = Modifier.width(8.dp))
            val refreshingLabel = stringResource(R.string.ui_a11y_refreshing)
            CircularProgressIndicator(
                modifier = Modifier
                    .size(12.dp)
                    .semantics { contentDescription = refreshingLabel },
                strokeWidth = 2.dp,
            )
        }
    }
}

@Composable
private fun PathsCard(paths: PathsCardState, nowMs: Long, startOfTodayMs: Long) {
    SectionHeading(stringResource(R.string.ui_section_paths))
    DetailCard {
        PathRow(
            icon = PathBluetoothIcon,
            name = stringResource(R.string.ui_badge_bluetooth),
            state = bluetoothStateText(paths),
            note = if (paths.bluetoothAudioActive) {
                stringResource(R.string.ui_path_bluetooth_audio_note)
            } else {
                null
            },
        )
        HorizontalDivider()
        PathRow(
            icon = PathLocalWifiIcon,
            name = stringResource(R.string.ui_badge_local_wifi),
            state = activeLinksText(paths.localWifiLinks),
            note = null,
        )
        HorizontalDivider()
        PathRow(
            icon = PathShorePassIcon,
            name = stringResource(R.string.ui_badge_shore_pass),
            state = stringResource(relayRowStateId(paths.relay)),
            note = lastSyncedNote(paths, nowMs, startOfTodayMs),
        )
    }
    Text(
        stringResource(R.string.ui_cruisemesh_chooses_the_best_available_path_automatically_a),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(top = 10.dp, start = 4.dp),
    )
}

@Composable
private fun PathRow(icon: ImageVector, name: String, state: String, note: String?) {
    val label = stringResource(R.string.ui_a11y_two_sentences, name, state)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .padding(vertical = 8.dp)
            .semantics(mergeDescendants = true) { contentDescription = label },
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Icon(
            icon,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(20.dp),
        )
        Spacer(modifier = Modifier.width(12.dp))
        // Side by side there is not enough width for both halves at large font
        // scales, and a name column narrower than its longest word wraps one
        // letter per line. Stacking is the honest answer: nothing truncates
        // and nothing has to fit.
        if (isLargeTextScale()) {
            Column(modifier = Modifier.weight(1f)) {
                Text(name, style = MaterialTheme.typography.bodyMedium)
                Text(
                    state,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                PathRowNote(note)
            }
        } else {
            Column(modifier = Modifier.weight(1f)) {
                Text(name, style = MaterialTheme.typography.bodyMedium)
                PathRowNote(note)
            }
            Spacer(modifier = Modifier.width(12.dp))
            Text(
                state,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun PathRowNote(note: String?) {
    note?.let {
        Text(
            it,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/**
 * Is the reader's text scale large enough that side-by-side rows stop working?
 *
 * The threshold is deliberately below 200 percent: the spec requires the page
 * to stay usable there, and a row that only just fits at 150 percent has
 * already lost.
 */
@Composable
private fun isLargeTextScale(): Boolean =
    androidx.compose.ui.platform.LocalDensity.current.fontScale >= 1.5f

@Composable
private fun PeopleCard(
    rows: List<ConnectionPersonRow>,
    nowMs: Long,
    startOfTodayMs: Long,
) {
    DetailCard {
        rows.forEachIndexed { index, row ->
            if (index > 0) HorizontalDivider()
            PersonRow(row, nowMs, startOfTodayMs)
        }
    }
}

@Composable
private fun PersonRow(row: ConnectionPersonRow, nowMs: Long, startOfTodayMs: Long) {
    val status = personStatusText(row.status, nowMs, startOfTodayMs)
    val badge = row.badge?.let { stringResource(pathBadgeLabelId(it)) }
    val delivery = row.delivery?.let {
        pluralStringResource(deliveryTextId(it.kind), it.count, it.count)
    }
    // One sentence per fact, in the order they are read on screen. The
    // delivery line has to be in here: the row clears its children's semantics
    // so the whole row is announced once, and anything left out is silent.
    val statusPhrase = if (badge == null) {
        status
    } else {
        stringResource(R.string.ui_a11y_via_path, status, badge)
    }
    val label = if (delivery == null) {
        stringResource(R.string.ui_a11y_two_sentences, row.name, statusPhrase)
    } else {
        stringResource(R.string.ui_a11y_three_sentences, row.name, statusPhrase, delivery)
    }
    Column(
        modifier = Modifier
            .fillMaxWidth()
            .heightIn(min = 48.dp)
            .padding(vertical = 10.dp)
            .clearAndSetSemantics { contentDescription = label },
    ) {
        val stacked = isLargeTextScale()
        Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
            Text(
                row.name,
                style = MaterialTheme.typography.bodyLarge,
                modifier = Modifier.weight(1f),
            )
            if (!stacked) badge?.let { PathBadge(it) }
        }
        if (stacked) {
            badge?.let {
                Box(modifier = Modifier.padding(top = 4.dp)) { PathBadge(it) }
            }
        }
        Text(
            status,
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 2.dp),
        )
        delivery?.let {
            Text(
                it,
                style = MaterialTheme.typography.bodyMedium,
                // Neutral, always. Waiting is what this product does; the old
                // page's red line under every friend is the bug being removed.
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 2.dp),
            )
        }
    }
}

@Composable
private fun PathBadge(label: String) {
    Box(
        modifier = Modifier
            .border(
                width = 1.dp,
                color = MaterialTheme.colorScheme.outline,
                shape = RoundedCornerShape(6.dp),
            )
            .padding(horizontal = 8.dp, vertical = 3.dp),
    ) {
        Text(
            label,
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

@Composable
private fun ActivityLine(row: ConnectionActivityRow, nowMs: Long, startOfTodayMs: Long) {
    val who = row.name ?: stringResource(R.string.ui_unnamed_friend)
    // An event with no usable timestamp is not an event anyone can act on,
    // and it must never come out the other side as a date in 1970.
    val time = eventTimeText(row.atMs, nowMs, startOfTodayMs) ?: return
    val pathId = transportLabelId(row.transport)
    val line = if (pathId == null) {
        stringResource(activityTextNoPathId(row.evidence), who, time)
    } else {
        stringResource(activityTextId(row.evidence), who, stringResource(pathId), time)
    }
    Text(
        line,
        style = MaterialTheme.typography.bodyMedium,
        modifier = Modifier.padding(vertical = 5.dp),
    )
}

@Composable
private fun SectionHeading(title: String) {
    Text(
        title,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 4.dp, bottom = 8.dp),
    )
}

@Composable
private fun DetailCard(content: @Composable () -> Unit) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.46f),
        ),
    ) {
        Column(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp)) {
            content()
        }
    }
}

@Composable
private fun CollapsibleSection(
    title: String,
    expanded: Boolean,
    onToggle: () -> Unit,
    detail: String? = null,
    content: @Composable () -> Unit,
) {
    val toggleLabel = if (expanded) {
        stringResource(R.string.ui_hide)
    } else {
        stringResource(R.string.ui_show)
    }
    val heading = if (detail == null) {
        title
    } else {
        stringResource(R.string.ui_section_with_detail, title, detail)
    }
    val label = stringResource(R.string.ui_a11y_two_sentences, heading, toggleLabel)
    Column(modifier = Modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .heightIn(min = 48.dp)
                .clickable(onClick = onToggle)
                .padding(horizontal = 4.dp, vertical = 8.dp)
                .semantics(mergeDescendants = true) { contentDescription = label },
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                heading,
                style = MaterialTheme.typography.titleSmall,
                modifier = Modifier.weight(1f),
            )
            Text(
                toggleLabel,
                style = MaterialTheme.typography.labelLarge,
                color = MaterialTheme.colorScheme.primary,
            )
            Icon(
                if (expanded) Icons.Default.KeyboardArrowUp else Icons.Default.KeyboardArrowDown,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.primary,
            )
        }
        if (expanded) {
            Column(modifier = Modifier.fillMaxWidth().padding(top = 4.dp)) { content() }
        }
    }
}

/**
 * The existing support controls, moved verbatim into the collapsed
 * Troubleshooting section. Destructive actions keep their confirmations and
 * Share diagnostics keeps producing the single archive.
 */
@Composable
private fun TroubleshootingControls(onClearHistory: () -> Unit, onStoreChanged: () -> Unit) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var diagnosticLogging by remember { mutableStateOf(DebugFileLog.isEnabled(context)) }
    // Counts the delivery metrics too: they are captured whether or not
    // diagnostic logging is on, so a tester who never touched the switch can
    // still have rows worth erasing, and a greyed-out delete would be wrong.
    var hasCapturedDiagnostics by remember {
        mutableStateOf(DiagnosticsShare.hasAnythingCaptured(context))
    }
    var supportMessage by remember { mutableStateOf<String?>(null) }

    Row(verticalAlignment = Alignment.CenterVertically, modifier = Modifier.fillMaxWidth()) {
        Column(modifier = Modifier.weight(1f)) {
            Text(stringResource(R.string.ui_diagnostic_logging))
            Text(
                stringResource(
                    if (DebugFileLog.isDebuggableBuild(context)) {
                        R.string.ui_diagnostic_logging_always_on
                    } else {
                        R.string.ui_diagnostic_logging_tester_desc
                    },
                ),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        Switch(
            checked = diagnosticLogging,
            enabled = !DebugFileLog.isDebuggableBuild(context),
            onCheckedChange = {
                diagnosticLogging = it
                DebugFileLog.setOptIn(context, it)
                supportMessage = if (it) {
                    context.getString(R.string.ui_diagnostic_logging_enabled_message)
                } else {
                    context.getString(R.string.ui_diagnostic_logging_disabled_message)
                }
            },
        )
    }
    Button(
        onClick = {
            // One button, everything captured -- see DiagnosticsShare.
            DiagnosticsShare.shareIntent(context)?.let {
                context.startActivity(
                    Intent.createChooser(it, context.getString(R.string.ui_share_diagnostics)),
                )
                hasCapturedDiagnostics = true
            } ?: run {
                supportMessage = context.getString(R.string.ui_no_diagnostics_captured_yet)
            }
        },
        modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
    ) { Text(stringResource(R.string.ui_share_diagnostics)) }
    OutlinedButton(
        onClick = {
            hasCapturedDiagnostics = false
            supportMessage = context.getString(R.string.ui_diagnostics_deleted)
            // Two table-wide deletes and several file removals, each of them
            // waiting on a store lock the receive path also wants.
            scope.launch {
                withContext(Dispatchers.IO) {
                    DebugFileLog.deleteCapturedLogs(context)
                    // The metrics are captured whether or not diagnostic
                    // logging is on, so a delete that skipped them would leave
                    // behind the one captured thing it did not name.
                    runCatching { AppStore.get(context).clearDeliveryMetrics() }
                    FieldMetricsExport.deleteCsvFile(context)
                    runCatching { AppStore.get(context).clearMessageConflicts() }
                    ConflictDiagnosticsExport.deleteCsvFile(context)
                    // The last share left a zip holding copies of them all.
                    DiagnosticsShare.deleteArchive(context)
                }
                onStoreChanged()
            }
        },
        enabled = hasCapturedDiagnostics,
        modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
    ) { Text(stringResource(R.string.ui_delete_captured_diagnostics)) }
    OutlinedButton(
        onClick = onClearHistory,
        modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
    ) { Text(stringResource(R.string.ui_clear_connection_history)) }
    Text(
        stringResource(R.string.ui_diagnostics_contain_identity_paths_and_timings),
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        modifier = Modifier.padding(top = 8.dp),
    )
    supportMessage?.let {
        Text(
            it,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 6.dp),
        )
    }
}

// ---------------------------------------------------------------------------
// Copy lookups
// ---------------------------------------------------------------------------

@StringRes
private fun healthTitleId(state: CoreConnectionHealth): Int = when (state) {
    CoreConnectionHealth.READY -> R.string.ui_health_working_normally
    CoreConnectionHealth.LIMITED -> R.string.ui_health_working_with_limits
    CoreConnectionHealth.NEEDS_ATTENTION -> R.string.ui_health_needs_attention
    CoreConnectionHealth.CHECKING -> R.string.ui_health_checking
}

@StringRes
private fun healthActionLabelId(action: CoreHealthAction): Int = when (action) {
    CoreHealthAction.START_MESH -> R.string.ui_start_mesh
    CoreHealthAction.TURN_ON_BLUETOOTH -> R.string.ui_turn_on_bluetooth
    CoreHealthAction.MANAGE_SHORE_PASS -> R.string.ui_manage_shore_pass
    CoreHealthAction.HOW_TO_FIX -> R.string.ui_how_to_fix
}

/** The How-to-fix explanation for the reasons this release can offer one for. */
@StringRes
private fun howToFixTextId(reason: CoreHealthReason): Int? = when (reason) {
    CoreHealthReason.OWN_SETUP_REJECTED -> R.string.ui_how_to_fix_setup_rejected
    CoreHealthReason.STORAGE_FULL -> R.string.ui_how_to_fix_storage_full
    else -> null
}

/**
 * The evidence line: what is happening nearby, then the Shore Pass state.
 *
 * A stopped mesh gets the runtime half instead of a friend count, because
 * "0 friends nearby" on a stopped service reads as an absence of friends
 * rather than an absence of a running app.
 */
@Composable
private fun healthEvidenceText(health: HealthCardState): String {
    val nearby = when {
        health.reason == CoreHealthReason.MESH_STOPPED ->
            stringResource(R.string.ui_health_mesh_is_stopped)
        health.nearbyFriendCount > 0 -> pluralStringResource(
            R.plurals.ui_health_friends_nearby,
            health.nearbyFriendCount,
            health.nearbyFriendCount,
        )
        health.bluetooth == CoreDirectPathState.OFF ->
            stringResource(R.string.ui_health_bluetooth_is_off)
        health.bluetooth == CoreDirectPathState.STARTING ->
            stringResource(R.string.ui_health_starting_up)
        else -> stringResource(R.string.ui_health_listening_for_friends)
    }
    return stringResource(
        R.string.ui_health_evidence_join,
        nearby,
        stringResource(relayEvidenceId(health.relay)),
    )
}

@StringRes
private fun relayEvidenceId(relay: CoreRelayPathState): Int = when (relay) {
    CoreRelayPathState.NOT_SET_UP -> R.string.ui_shore_pass_state_not_set_up
    CoreRelayPathState.CHECKING -> R.string.ui_shore_pass_state_checking
    CoreRelayPathState.CONNECTED -> R.string.ui_shore_pass_state_connected
    CoreRelayPathState.WAITING_FOR_INTERNET -> R.string.ui_shore_pass_state_waiting_for_internet
    CoreRelayPathState.UNREACHABLE -> R.string.ui_shore_pass_state_unreachable
    CoreRelayPathState.PASS_EXPIRED -> R.string.ui_shore_pass_state_expired
    CoreRelayPathState.PASS_SUSPENDED -> R.string.ui_shore_pass_state_suspended
    CoreRelayPathState.SETUP_REJECTED -> R.string.ui_shore_pass_state_setup_rejected
    CoreRelayPathState.STORAGE_FULL -> R.string.ui_shore_pass_state_storage_full
    CoreRelayPathState.SYNCING_SLOWED -> R.string.ui_shore_pass_state_syncing_slowed
}

@StringRes
private fun relayRowStateId(relay: CoreRelayPathState): Int = when (relay) {
    CoreRelayPathState.NOT_SET_UP -> R.string.ui_path_shore_pass_not_set_up
    CoreRelayPathState.CHECKING -> R.string.ui_path_shore_pass_checking
    CoreRelayPathState.CONNECTED -> R.string.ui_path_shore_pass_connected
    CoreRelayPathState.WAITING_FOR_INTERNET -> R.string.ui_path_shore_pass_waiting_for_internet
    CoreRelayPathState.UNREACHABLE -> R.string.ui_path_shore_pass_unreachable
    CoreRelayPathState.PASS_EXPIRED -> R.string.ui_path_shore_pass_expired
    CoreRelayPathState.PASS_SUSPENDED -> R.string.ui_path_shore_pass_suspended
    CoreRelayPathState.SETUP_REJECTED -> R.string.ui_path_shore_pass_setup_rejected
    CoreRelayPathState.STORAGE_FULL -> R.string.ui_path_shore_pass_storage_full
    CoreRelayPathState.SYNCING_SLOWED -> R.string.ui_path_shore_pass_syncing_slowed
}

@StringRes
private fun pathBadgeLabelId(badge: ConnectionPathBadge): Int = when (badge) {
    ConnectionPathBadge.BLUETOOTH -> R.string.ui_badge_bluetooth
    ConnectionPathBadge.LOCAL_WIFI -> R.string.ui_badge_local_wifi
    ConnectionPathBadge.SHORE_PASS -> R.string.ui_badge_shore_pass
}

@PluralsRes
private fun deliveryTextId(kind: CoreDeliveryState): Int = when (kind) {
    CoreDeliveryState.SENDING -> R.plurals.ui_delivery_sending
    CoreDeliveryState.WILL_DELIVER_WHEN_RECONNECTED ->
        R.plurals.ui_delivery_will_deliver_when_you_reconnect
    CoreDeliveryState.WAITING_FOR_INTERNET -> R.plurals.ui_delivery_waiting_for_internet
}

@Composable
private fun bluetoothStateText(paths: PathsCardState): String = when {
    paths.bluetoothLinks > 0 -> activeLinksText(paths.bluetoothLinks)
    paths.bluetooth == CoreDirectPathState.OFF -> stringResource(R.string.ui_path_state_off)
    paths.bluetooth == CoreDirectPathState.STARTING ->
        stringResource(R.string.ui_path_state_starting)
    else -> stringResource(R.string.ui_path_state_listening)
}

@Composable
private fun activeLinksText(links: Int): String = if (links == 0) {
    stringResource(R.string.ui_path_state_no_active_connections)
} else {
    pluralStringResource(R.plurals.ui_path_state_active_connections, links, links)
}

@Composable
private fun lastSyncedNote(
    paths: PathsCardState,
    nowMs: Long,
    startOfTodayMs: Long,
): String? {
    // Only useful when the pass is set up at all; on a phone with no pass it
    // would be a date attached to nothing.
    if (paths.relay == CoreRelayPathState.NOT_SET_UP) return null
    val time = eventTimeText(paths.relayLastSyncMs, nowMs, startOfTodayMs) ?: return null
    return stringResource(R.string.ui_path_last_synced, time)
}

@Composable
private fun personStatusText(
    status: PersonStatus,
    nowMs: Long,
    startOfTodayMs: Long,
): String = when (status) {
    is PersonStatus.ConnectedNow -> stringResource(R.string.ui_person_status_connected_now)
    is PersonStatus.NoHistory -> stringResource(R.string.ui_no_connection_history_yet)
    is PersonStatus.SeenOnline -> {
        val time = eventTimeText(status.atMs, nowMs, startOfTodayMs)
        if (time == null) {
            stringResource(R.string.ui_person_status_connected_now)
        } else {
            stringResource(R.string.ui_person_status_seen_online, time)
        }
    }
    is PersonStatus.History -> {
        val time = eventTimeText(status.atMs, nowMs, startOfTodayMs)
        if (time == null) {
            // A recorded moment with no usable timestamp is not a date; say
            // what is actually known, which is nothing.
            stringResource(R.string.ui_no_connection_history_yet)
        } else {
            stringResource(personStatusTextId(status.evidence), time)
        }
    }
}

@StringRes
private fun personStatusTextId(evidence: PeerEvidence): Int = when (evidence) {
    PeerEvidence.MESSAGE_RECEIVED -> R.string.ui_person_status_sent_you_a_message
    PeerEvidence.MESSAGE_DELIVERED -> R.string.ui_person_status_received_your_message
    PeerEvidence.PRESENCE_SEEN -> R.string.ui_person_status_seen
    PeerEvidence.CONNECTED -> R.string.ui_person_status_last_connected
    PeerEvidence.DISCONNECTED -> R.string.ui_person_status_last_disconnected
}

/**
 * A recorded moment as copy, or null when there is no usable timestamp.
 *
 * Null is the whole point: a zero or negative stamp must never come out the
 * other side as a date in 1970.
 */
@Composable
private fun eventTimeText(atMs: Long, nowMs: Long, startOfTodayMs: Long): String? =
    when (val time = ConnectionTimes.eventTime(atMs, nowMs, startOfTodayMs)) {
        is EventTime.Unknown -> null
        is EventTime.JustNow -> stringResource(R.string.ui_time_just_now)
        is EventTime.Minutes -> pluralStringResource(
            R.plurals.ui_time_minutes_ago,
            time.value,
            time.value,
        )
        is EventTime.Hours -> pluralStringResource(
            R.plurals.ui_time_hours_ago,
            time.value,
            time.value,
        )
        is EventTime.Yesterday -> stringResource(
            R.string.ui_time_yesterday_at,
            DateFormat.getTimeInstance(DateFormat.SHORT).format(Date(atMs)),
        )
        is EventTime.Older -> stringResource(
            R.string.ui_time_on,
            DateFormat.getDateTimeInstance(DateFormat.SHORT, DateFormat.SHORT).format(Date(atMs)),
        )
    }

@StringRes
private fun activityTextId(evidence: PeerEvidence): Int = when (evidence) {
    PeerEvidence.MESSAGE_RECEIVED -> R.string.ui_peer_activity_sent_you_a_message
    PeerEvidence.MESSAGE_DELIVERED -> R.string.ui_peer_activity_received_your_message
    PeerEvidence.PRESENCE_SEEN -> R.string.ui_peer_activity_was_reachable
    PeerEvidence.CONNECTED -> R.string.ui_peer_activity_connected
    PeerEvidence.DISCONNECTED -> R.string.ui_peer_activity_disconnected
}

/** [activityTextId]'s wording for evidence whose path we never observed. */
@StringRes
private fun activityTextNoPathId(evidence: PeerEvidence): Int = when (evidence) {
    PeerEvidence.MESSAGE_RECEIVED -> R.string.ui_peer_activity_sent_you_a_message_no_path
    PeerEvidence.MESSAGE_DELIVERED -> R.string.ui_peer_activity_received_your_message_no_path
    PeerEvidence.PRESENCE_SEEN -> R.string.ui_peer_activity_was_reachable_no_path
    PeerEvidence.CONNECTED -> R.string.ui_peer_activity_connected_no_path
    PeerEvidence.DISCONNECTED -> R.string.ui_peer_activity_disconnected_no_path
}

/**
 * The copy naming this path, or null when there is no path to name.
 *
 * Null exactly when core says the path was not observed
 * (`core_peer_transport_is_observed`) -- pinned by
 * `ConnectionActivityLogicTest`, so the two cannot drift apart. A caller that
 * gets null must switch to the no-path wording rather than substituting a
 * plausible-looking radio; that substitution is the bug this screen was fixed
 * for.
 */
@StringRes
internal fun transportLabelId(transport: PeerConnectionTransport): Int? = when (transport) {
    PeerConnectionTransport.BLUETOOTH -> R.string.ui_path_bluetooth
    PeerConnectionTransport.LOCAL_WIFI -> R.string.ui_path_local_wifi
    PeerConnectionTransport.SHORE_PASS -> R.string.ui_path_shore_pass
    PeerConnectionTransport.CARRIED -> null
}

// ---------------------------------------------------------------------------
// Store reading
// ---------------------------------------------------------------------------

/**
 * Everything this page needs from the store, in one bounded pass.
 *
 * Runs on a background dispatcher only. Every query is limited: contacts to
 * [CONNECTION_PEOPLE_LIMIT], events to [CONNECTION_ACTIVITY_QUERY_LIMIT].
 * Nothing here scales with total history size.
 *
 * Blocked identities are dropped from Recent activity here and from the People
 * groups by the core, so a block is honoured on this page in both directions.
 * The block tombstones come from one query rather than a per-contact question:
 * a block can outlive the contact row and can sort past the people cap, and
 * either way an activity row for a blocked identity is the tombstone leaking.
 */
internal fun loadConnectionSnapshot(store: MessageStore, nowMs: Long): ConnectionStoreSnapshot {
    val contacts = runCatching { store.listContacts() }
        .getOrDefault(emptyList())
        .take(CONNECTION_PEOPLE_LIMIT)
    val depths = runCatching { store.pendingRelayOutboundDepthByRecipient(nowMs) }
        .getOrDefault(emptyList())
        .associate { UserIdHex.encode(it.recipientUserId) to it.queued.toInt() }
    val summaries = runCatching { store.peerConnectionSummaries() }
        .getOrDefault(emptyList())
        .groupBy { UserIdHex.encode(it.userId) }
    val blocked = runCatching { store.listBlockedUsers() }
        .getOrDefault(emptyList())
        .map { UserIdHex.encode(it) }
        .toSet()

    val people = contacts.map { contact ->
        val hex = UserIdHex.encode(contact.userId)
        ConnectionPerson(
            userIdHex = hex,
            userId = contact.userId,
            name = coreContactDisplayName(contact),
            blocked = hex in blocked,
            hasRelayEndpoint = !contact.relayUrl.isNullOrBlank(),
            queued = depths[hex] ?: 0,
            latest = latestPeerStatus(summaries[hex].orEmpty()),
        )
    }

    val names = people.associate { it.userIdHex to it.name }
    val activity = runCatching {
        store.peerConnectionEvents(null, CONNECTION_ACTIVITY_QUERY_LIMIT.toUInt())
    }
        .getOrDefault(emptyList())
        .mapNotNull { event ->
            val hex = UserIdHex.encode(event.userId)
            if (hex in blocked) return@mapNotNull null
            ConnectionActivityRow(
                name = names[hex],
                evidence = peerEvidenceOf(event.kind),
                transport = event.transport,
                atMs = event.occurredAtMs,
            )
        }

    return ConnectionStoreSnapshot(people = people, activity = activity, loadedAtMs = nowMs)
}

// ---------------------------------------------------------------------------
// Clocks and visibility
// ---------------------------------------------------------------------------

/**
 * Is the page actually on screen?
 *
 * Every ticker and the reload loop key off this, so navigating away stops all
 * page-driven observation and polling — the spec's requirement, and the only
 * way a diagnostics page earns its place on a battery-limited phone.
 */
@Composable
private fun rememberPageResumed(): Boolean {
    val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    var resumed by remember { mutableStateOf(false) }
    DisposableEffect(lifecycleOwner) {
        val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
            when (event) {
                androidx.lifecycle.Lifecycle.Event.ON_RESUME -> resumed = true
                androidx.lifecycle.Lifecycle.Event.ON_PAUSE -> resumed = false
                else -> {}
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    return resumed
}

/**
 * A clock that ticks while the page is visible, so relative times and the
 * freshness label age on screen instead of freezing at the moment of the last
 * store change.
 */
@Composable
private fun rememberPageClock(resumed: Boolean): Long {
    var nowMs by remember { mutableStateOf(System.currentTimeMillis()) }
    LaunchedEffect(resumed) {
        if (!resumed) return@LaunchedEffect
        while (true) {
            nowMs = System.currentTimeMillis()
            delay(CLOCK_TICK_MS)
        }
    }
    return nowMs
}

/** Local midnight for [nowMs]; the calendar boundary `Yesterday` is measured from. */
@Composable
private fun rememberStartOfToday(nowMs: Long): Long = remember(nowMs) { startOfDayMs(nowMs) }

internal fun startOfDayMs(nowMs: Long): Long = Calendar.getInstance().apply {
    timeInMillis = nowMs
    set(Calendar.HOUR_OF_DAY, 0)
    set(Calendar.MINUTE, 0)
    set(Calendar.SECOND, 0)
    set(Calendar.MILLISECOND, 0)
}.timeInMillis
