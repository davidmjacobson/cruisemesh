package com.cruisemesh.app.ui

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.selection.toggleable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Info
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.res.pluralStringResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R
import com.cruisemesh.app.identity.TERMS_OF_USE_URL
import com.cruisemesh.app.mesh.MeshStartupPreferences
import com.cruisemesh.app.mesh.PassIndicator
import com.cruisemesh.app.mesh.RelayHealth
import com.cruisemesh.app.mesh.passIndicator
import com.cruisemesh.app.mesh.shorePassDeliveryThroughMs
import com.cruisemesh.app.mesh.shorePassOffersRenewal
import com.cruisemesh.app.mesh.shorePassRenewUrl
import com.cruisemesh.app.relay.FamilyStatusStore
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.relay.RelayEngineSettings
import com.cruisemesh.app.friending.FriendsOfFriendsStore
import com.cruisemesh.app.mesh.RelaySyncEvents
import kotlinx.coroutines.delay

const val SUPPORT_URL = "https://cruisemesh.app/support/"

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    meshEnabled: Boolean,
    meshStatus: String,
    relayHealth: RelayHealth,
    appearancePreference: AppearancePreference,
    onAppearancePreferenceChange: (AppearancePreference) -> Unit,
    onShorePass: () -> Unit,
    onSailChecklist: () -> Unit,
    onConnectionDetails: () -> Unit,
    onDeveloperSettings: () -> Unit,
    onYourDevices: () -> Unit,
    onBackUp: () -> Unit,
    onMeshEnabledChange: (Boolean) -> Unit,
    onFriendsOfFriendsChanged: (Boolean) -> Unit,
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    var startAutomatically by remember {
        mutableStateOf(MeshStartupPreferences.isAutoStartEnabled(context))
    }
    var shareOnline by remember { mutableStateOf(RelayConfigStore.shareOnline(context)) }
    var friendsOfFriends by remember {
        mutableStateOf(FriendsOfFriendsStore.isEnabled(context))
    }
    var useRoamingData by remember { mutableStateOf(RelayEngineSettings.allowsRoamingData(context)) }
    val relayConfig = RelayConfigStore.load(context)
    val relayConfigured = relayConfig != null
    // The pass's own end date, read once per app session. Read here as well as
    // on the Shore Pass screen so the renewal row is there for someone who
    // never opens that screen; the second ask costs nothing once one has
    // landed.
    val familyStatus by FamilyStatusStore.status.collectAsState()
    LaunchedEffect(relayConfig) { relayConfig?.let { FamilyStatusStore.refresh(it) } }
    // Debuggable builds show the entry outright, as they always have. A
    // release build shows it once someone has done the seven-tap run on the
    // version line at the bottom of this screen -- the only way a closed-test
    // tester on a release-signed build can reach the engine switches.
    //
    // Read once, and deliberately not updated when a run lands. Inserting a
    // row above the version line would push the line out from under the finger
    // still tapping it, and the seventh tap of a run is usually followed by an
    // eighth on the way to stopping. The entry appears the next time this
    // screen is opened, which the row's own text says out loud.
    val showDeveloperSettings = remember { developerSettingsVisible(context) }
    val unlockTaps = remember { DeveloperSettingsTapCounter() }
    val haptics = LocalHapticFeedback.current
    // What the version row reads right now, plus a token that restarts the
    // revert countdown on every tap. Keying the effect on the token means a
    // run of taps cancels and replaces one pending revert rather than queueing
    // several, so nothing keeps appearing after the tapping stops.
    var versionRowLabel by remember {
        mutableStateOf<DeveloperSettingsLabel>(DeveloperSettingsLabel.Version)
    }
    var versionRowLabelToken by remember { mutableIntStateOf(0) }
    LaunchedEffect(versionRowLabelToken) {
        if (versionRowLabel != DeveloperSettingsLabel.Version) {
            delay(DEVELOPER_SETTINGS_LABEL_REVERT_MS)
            versionRowLabel = DeveloperSettingsLabel.Version
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_settings)) },
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
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(20.dp),
        ) {
            // The checklist's permanent home. The home-screen card can be
            // dismissed, and dismissing it must not be a way to lose the
            // checklist -- there is always this row.
            SettingsGroup(stringResource(R.string.ui_getting_ready)) {
                SettingsLink(
                    title = stringResource(R.string.ui_before_you_sail),
                    detail = stringResource(R.string.ui_before_you_sail_summary),
                    onClick = onSailChecklist,
                )
            }

            SettingsGap()
            SettingsGroup(stringResource(R.string.ui_shore_pass)) {
                SettingsLink(
                    title = relayTitle(relayHealth, relayConfigured),
                    detail = relayDetail(relayHealth, relayConfigured),
                    indicator = passIndicator(relayHealth, relayConfigured),
                    onClick = onShorePass,
                )
                val deliveryThroughMs =
                    shorePassDeliveryThroughMs(familyStatus, System.currentTimeMillis())
                // When delivery runs to, in the same words and the same quiet
                // supporting style as the Shore Pass screen's line. An ordinary
                // future date is not a warning and must never be coloured as
                // one; nothing shows at all until the status read lands, or for
                // a pass with no end date -- the household that owns the relay
                // never expires and must not be told a date or nagged.
                deliveryThroughMs?.let { throughMs ->
                    Text(
                        stringResource(R.string.ui_internet_delivery_through, passDate(throughMs)),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.padding(bottom = 4.dp),
                    )
                }
                // Renewal is a purchase, so it happens on the site rather than
                // in the app -- the row above still leads to the pass screen,
                // which is where everything the app itself can do lives. The
                // family token rides the URL fragment, which never reaches a
                // server log.
                if (shorePassOffersRenewal(relayHealth, deliveryThroughMs)) {
                    relayConfig?.relayToken?.let(::shorePassRenewUrl)?.let { renewUrl ->
                        SettingsLink(
                            title = stringResource(R.string.ui_renew_shore_pass),
                            detail = stringResource(R.string.ui_renew_shore_pass_detail),
                        ) {
                            context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(renewUrl)))
                        }
                    }
                }
            }

            SettingsGap()
            SettingsGroup(stringResource(R.string.ui_cruisemesh_operation)) {
                SettingsSwitch(
                    title = stringResource(R.string.ui_mesh_running),
                    detail = stringResource(R.string.ui_mesh_running_detail),
                    checked = meshEnabled,
                    onCheckedChange = onMeshEnabledChange,
                )
                Text(
                    meshStatus,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.primary,
                )
                SettingsSwitch(
                    title = stringResource(R.string.ui_start_mesh_after_restart),
                    detail = stringResource(R.string.ui_start_mesh_after_restart_detail),
                    checked = startAutomatically,
                    onCheckedChange = {
                        startAutomatically = it
                        MeshStartupPreferences.setAutoStartEnabled(context, it)
                    },
                )
                SettingsLink(
                    title = stringResource(R.string.ui_connection_details),
                    detail = stringResource(R.string.ui_connection_details_summary),
                    onClick = onConnectionDetails,
                )
                if (showDeveloperSettings) {
                    SettingsLink(
                        title = stringResource(R.string.ui_developer_settings_entry),
                        detail = stringResource(R.string.ui_developer_settings_summary),
                        onClick = onDeveloperSettings,
                    )
                }
            }

            SettingsGap()
            SettingsGroup(stringResource(R.string.ui_appearance)) {
                AppearanceChoices(
                    selected = appearancePreference,
                    onSelected = onAppearancePreferenceChange,
                )
            }

            SettingsGap()
            SettingsGroup(stringResource(R.string.ui_privacy)) {
                SettingsSwitch(
                    // One switch, one meaning: it governs manual "Share
                    // contact" as well as automatic introductions
                    // (specs/share-contact.md decision 4).
                    title = stringResource(R.string.ui_friends_of_friends),
                    detail = stringResource(R.string.ui_friends_of_friends_detail),
                    checked = friendsOfFriends,
                    onCheckedChange = {
                        friendsOfFriends = it
                        onFriendsOfFriendsChanged(it)
                    },
                )
                SettingsSwitch(
                    title = stringResource(R.string.ui_share_when_online),
                    detail = stringResource(R.string.ui_share_when_online_summary),
                    checked = shareOnline,
                    onCheckedChange = {
                        shareOnline = it
                        RelayConfigStore.setShareOnline(context, it)
                    },
                )
            }

            SettingsGap()
            SettingsGroup(stringResource(R.string.ui_advanced)) {
                SettingsSwitch(
                    title = stringResource(R.string.ui_use_roaming_data_for_shore_pass),
                    detail = stringResource(R.string.ui_use_roaming_data_for_shore_pass_detail),
                    checked = useRoamingData,
                    onCheckedChange = {
                        useRoamingData = it
                        RelayEngineSettings.setAllowsRoamingData(context, it)
                        // The relay front door reads the preference per call,
                        // so this nudge takes effect without a restart.
                        RelaySyncEvents.requestSync()
                    },
                )
            }

            // specs/multi-device-v1.md §13 WP6. Next to Backup on purpose: a
            // person opening Settings after losing a phone is looking for one of
            // these two, and which one they need depends on whether the phone is
            // gone or merely lost.
            SettingsGap()
            SettingsGroup(stringResource(R.string.ui_your_devices)) {
                SettingsLink(
                    title = stringResource(R.string.ui_your_devices),
                    detail = stringResource(R.string.ui_your_devices_summary),
                    onClick = onYourDevices,
                )
            }

            SettingsGap()
            SettingsGroup(stringResource(R.string.ui_backup)) {
                SettingsLink(
                    title = stringResource(R.string.ui_back_up_account),
                    detail = stringResource(R.string.ui_backup_account_summary),
                    onClick = onBackUp,
                )
            }

            SettingsGap()
            SettingsGroup(stringResource(R.string.ui_about_legal)) {
                SettingsLink(stringResource(R.string.ui_help_support), SUPPORT_URL) {
                    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(SUPPORT_URL)))
                }
                SettingsLink(stringResource(R.string.ui_terms_of_use), null) {
                    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(TERMS_OF_USE_URL)))
                }
                SettingsLink(stringResource(R.string.ui_privacy_policy), null) {
                    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(PRIVACY_POLICY_URL)))
                }
            }

            // The version line doubles as the door to developer settings: seven
            // taps turn them on, seven more hide them again. Deliberately
            // undiscoverable -- a family member scrolling to the bottom of
            // Settings should never arrive here by accident.
            //
            // All of the feedback is this row's own text, swapped in place at
            // the same size and reverted a moment after the last tap. Nothing
            // is drawn over it and nothing below it moves, so the row stays
            // exactly where the finger already is for the whole run.
            Text(
                versionRowText(versionRowLabel, appVersionLabel(context)),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 28.dp)
                    .clickable(
                        // No ripple and no click role: this is a line of text
                        // that happens to count taps, not a button, and
                        // announcing it as one to TalkBack would advertise a
                        // door nobody is meant to find.
                        interactionSource = remember { MutableInteractionSource() },
                        indication = null,
                    ) {
                        val tap = unlockTaps.tap(System.currentTimeMillis())
                        val unlocked = if (tap == DeveloperSettingsTap.Reached) {
                            !DeveloperSettingsUnlockStore.isUnlocked(context)
                        } else {
                            DeveloperSettingsUnlockStore.isUnlocked(context)
                        }
                        if (tap == DeveloperSettingsTap.Reached) {
                            DeveloperSettingsUnlockStore.setUnlocked(context, unlocked)
                        }
                        if (tap != DeveloperSettingsTap.Quiet) {
                            haptics.performHapticFeedback(HapticFeedbackType.LongPress)
                        }
                        versionRowLabel = developerSettingsLabelFor(tap, unlocked)
                        versionRowLabelToken += 1
                    },
            )
            // The author's dedication, in the traditional place for one: the
            // very bottom of the last screen, after everything functional.
            // Latin and untranslated -- a fixed phrase, the way Bach's
            // manuscripts carry it.
            Text(
                stringResource(R.string.ui_soli_deo_gloria),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.65f),
                textAlign = TextAlign.Center,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 6.dp, bottom = 8.dp),
            )
        }
    }
}

/**
 * "CruiseMesh 1.0.2 (1784978966)".
 *
 * Read from the installed package rather than `BuildConfig` (which this module
 * does not generate) so it reports what is actually on the phone. The version
 * code is the part that matters in a bug report: `versionName` falls back to a
 * hardcoded "1.0.0" for any build not made from a release tag, so several
 * different builds can share it.
 */
/**
 * The version row's text: the version string at rest, tap feedback while a run
 * is in progress.
 *
 * One row, one line of text, one hit target. The alternative -- a toast or a
 * snackbar -- floats at the bottom of the screen, which is exactly where this
 * row is, and every tap it swallows makes the run longer.
 */
@Composable
private fun versionRowText(label: DeveloperSettingsLabel, version: String): String = when (label) {
    DeveloperSettingsLabel.Version -> version
    is DeveloperSettingsLabel.Countdown -> pluralStringResource(
        R.plurals.ui_developer_settings_taps_left,
        label.remaining,
        label.remaining,
    )
    DeveloperSettingsLabel.Unlocked -> stringResource(R.string.ui_developer_settings_unlocked)
    DeveloperSettingsLabel.Hidden -> stringResource(R.string.ui_developer_settings_hidden)
}

@Composable
private fun appVersionLabel(context: Context): String {
    val fallback = stringResource(R.string.app_name)
    val format = stringResource(R.string.ui_app_version_label)
    return remember(context, format, fallback) {
        runCatching {
            val info = context.packageManager.getPackageInfo(context.packageName, 0)
            String.format(format, info.versionName ?: "?", info.longVersionCode)
        }.getOrDefault(fallback)
    }
}

@Composable
private fun relayTitle(health: RelayHealth, configured: Boolean): String {
    if (!configured) return "Set up Shore Pass"
    return when (health) {
        RelayHealth.NoConfig,
        RelayHealth.Checking -> "Checking Shore Pass setup…"
        is RelayHealth.Ok -> "Shore Pass is working"
        RelayHealth.NoInternet -> "Shore Pass is waiting for internet"
        RelayHealth.DeferredRoaming -> stringResource(R.string.ui_relay_deferred_roaming)
        is RelayHealth.Failing -> "Shore Pass needs attention"
        is RelayHealth.Expired -> "Shore Pass expired"
        is RelayHealth.ExpiredReadOnly ->
            stringResource(R.string.ui_shore_pass_expired_still_receiving_title)
        is RelayHealth.Suspended -> "Shore Pass suspended"
        is RelayHealth.TokenRejected -> "Shore Pass setup was rejected"
        is RelayHealth.QuotaFull -> stringResource(R.string.ui_shore_pass_storage_full_title)
        is RelayHealth.MessageTooLarge -> stringResource(R.string.ui_shore_pass_message_too_large_title)
        is RelayHealth.RateLimited -> stringResource(R.string.ui_shore_pass_slowed_title)
    }
}

@Composable
private fun relayDetail(health: RelayHealth, configured: Boolean): String {
    if (!configured) return "CruiseMesh still works nearby. Add a pass for internet delivery."
    return when (health) {
        RelayHealth.NoConfig -> "Setup is saved and will be checked when CruiseMesh runs."
        RelayHealth.Checking -> "Setup is saved; CruiseMesh has not completed an authenticated check yet."
        is RelayHealth.Ok -> "Internet delivery is ready · checked ${relativeAge(health.lastSyncMs)}."
        RelayHealth.NoInternet -> "Configured; this phone is currently offline."
        RelayHealth.DeferredRoaming -> stringResource(R.string.ui_relay_deferred_roaming)
        is RelayHealth.Failing -> "The relay could not be reached."
        is RelayHealth.Expired -> "Renew your pass to resume internet delivery."
        is RelayHealth.ExpiredReadOnly ->
            stringResource(R.string.ui_shore_pass_expired_still_receiving_detail)
        is RelayHealth.Suspended -> "Contact support for help with this pass."
        is RelayHealth.TokenRejected -> "Paste the setup card again, or use a different Shore Pass."
        is RelayHealth.QuotaFull -> stringResource(R.string.ui_shore_pass_storage_full_detail)
        is RelayHealth.MessageTooLarge -> stringResource(R.string.ui_shore_pass_message_too_large_detail)
        is RelayHealth.RateLimited -> stringResource(R.string.ui_shore_pass_slowed_detail)
    }
}

private fun relativeAge(timestampMs: Long): String {
    val minutes = ((System.currentTimeMillis() - timestampMs).coerceAtLeast(0L) / 60_000L)
    return when {
        minutes == 0L -> "just now"
        minutes < 60L -> "${minutes}m ago"
        minutes < 24L * 60L -> "${minutes / 60L}h ago"
        else -> "${minutes / (24L * 60L)}d ago"
    }
}

@Composable
private fun SettingsGroup(title: String, content: @Composable () -> Unit) {
    Text(
        title,
        style = MaterialTheme.typography.labelLarge,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 4.dp, bottom = 8.dp),
    )
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.46f),
        ),
    ) {
        Column(modifier = Modifier.fillMaxWidth().padding(16.dp)) {
            content()
        }
    }
}

/**
 * Icon, tint and screen-reader label for a [PassIndicator].
 *
 * Each state gets a distinct *shape* as well as a distinct colour -- check,
 * info, "?", "!" -- so the row still reads correctly for anyone who cannot
 * tell the tints apart. CP2b (David's UX spec): the "?" circle marks
 * transient, self-healing conditions; the "!" circle marks persistent ones
 * that need a person. Colours come from [LocalReachabilityPalette], which
 * already carries light/dark variants and the app's fixed meanings
 * (green = good, amber = degraded), rather than new one-off literals.
 */
@Composable
private fun passIndicatorIcon(
    indicator: PassIndicator,
): Triple<ImageVector, Color, String>? {
    val palette = LocalReachabilityPalette.current
    return when (indicator) {
        PassIndicator.NONE -> null
        PassIndicator.READY -> Triple(
            Icons.Filled.CheckCircle,
            palette.nearby,
            stringResource(R.string.ui_shore_pass_ready),
        )
        PassIndicator.WAITING -> Triple(
            Icons.Filled.Info,
            palette.neutral,
            stringResource(R.string.ui_shore_pass_waiting_for_internet),
        )
        PassIndicator.ATTENTION -> Triple(
            PassQuestionIcon,
            palette.recent,
            stringResource(R.string.ui_shore_pass_needs_attention),
        )
        PassIndicator.ACTION_REQUIRED -> Triple(
            PassExclamationIcon,
            MaterialTheme.colorScheme.error,
            stringResource(R.string.ui_shore_pass_needs_action),
        )
    }
}

// `indicator` sits before `onClick` on purpose: several call sites pass the
// click handler as a trailing lambda, which Kotlin binds to the *last*
// parameter. With the indicator last, those calls silently bound the lambda
// to the wrong parameter and failed to compile.
@Composable
private fun SettingsLink(
    title: String,
    detail: String?,
    indicator: PassIndicator = PassIndicator.NONE,
    onClick: () -> Unit,
) {
    val icon = passIndicatorIcon(indicator)
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(title, style = MaterialTheme.typography.bodyLarge)
            detail?.let {
                Text(
                    it,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 2.dp),
                )
            }
        }
        icon?.let { (vector, tint, description) ->
            Icon(
                vector,
                contentDescription = description,
                tint = tint,
                modifier = Modifier.padding(start = 12.dp).size(20.dp),
            )
        }
    }
}

@Composable
private fun SettingsSwitch(
    title: String,
    detail: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .semantics(mergeDescendants = true) {}
            .toggleable(
                value = checked,
                role = Role.Switch,
                onValueChange = onCheckedChange,
            )
            .padding(vertical = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(title)
            Text(
                detail,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        // The whole labeled row is the single switch target. A separate
        // callback here would expose a duplicate, unlabeled control to TalkBack.
        Switch(checked = checked, onCheckedChange = null)
    }
}

@Composable
private fun AppearanceChoices(
    selected: AppearancePreference,
    onSelected: (AppearancePreference) -> Unit,
) {
    Text(
        stringResource(R.string.ui_theme),
        style = MaterialTheme.typography.bodyLarge,
        modifier = Modifier.padding(bottom = 4.dp),
    )
    Column(modifier = Modifier.fillMaxWidth().selectableGroup()) {
        AppearancePreference.entries.forEach { preference ->
            val label = stringResource(
                when (preference) {
                    AppearancePreference.SYSTEM -> R.string.ui_theme_system
                    AppearancePreference.LIGHT -> R.string.ui_theme_light
                    AppearancePreference.DARK -> R.string.ui_theme_dark
                },
            )
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .semantics(mergeDescendants = true) {}
                    .selectable(
                        selected = preference == selected,
                        role = Role.RadioButton,
                        onClick = { onSelected(preference) },
                    )
                    .padding(vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                RadioButton(selected = preference == selected, onClick = null)
                Text(label, modifier = Modifier.padding(start = 12.dp))
            }
        }
    }
}

@Composable
private fun SettingsGap() = Spacer(modifier = Modifier.height(20.dp))
