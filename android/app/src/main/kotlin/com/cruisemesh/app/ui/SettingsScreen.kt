package com.cruisemesh.app.ui

import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.net.Uri
import androidx.compose.foundation.clickable
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
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R
import com.cruisemesh.app.identity.TERMS_OF_USE_URL
import com.cruisemesh.app.mesh.MeshStartupPreferences
import com.cruisemesh.app.mesh.PassIndicator
import com.cruisemesh.app.mesh.RelayHealth
import com.cruisemesh.app.mesh.passIndicator
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.friending.FriendsOfFriendsStore

const val SUPPORT_URL = "https://cruisemesh.app/support/"

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    meshStatus: String,
    relayHealth: RelayHealth,
    onCruisePass: () -> Unit,
    onConnectionDetails: () -> Unit,
    onInternalTools: () -> Unit,
    onBackUp: () -> Unit,
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
    val relayConfigured = RelayConfigStore.load(context) != null
    val showInternalTools =
        context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_settings)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
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
            SettingsGroup("Cruise Pass") {
                SettingsLink(
                    title = relayTitle(relayHealth, relayConfigured),
                    detail = relayDetail(relayHealth, relayConfigured),
                    indicator = passIndicator(relayHealth, relayConfigured),
                    onClick = onCruisePass,
                )
            }

            SettingsGap()
            SettingsGroup("CruiseMesh operation") {
                Text(
                    meshStatus,
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.primary,
                )
                SettingsSwitch(
                    title = "Start automatically",
                    detail = "Keep nearby delivery available after this phone restarts.",
                    checked = startAutomatically,
                    onCheckedChange = {
                        startAutomatically = it
                        MeshStartupPreferences.setAutoStartEnabled(context, it)
                    },
                )
                SettingsLink(
                    title = "Connection details",
                    detail = "See active paths, people, recent activity, and support diagnostics.",
                    onClick = onConnectionDetails,
                )
                if (showInternalTools) {
                    SettingsLink(
                        title = "Internal field tools",
                        detail = "Manual local-network probes, raw route counters, and diagnostic exports.",
                        onClick = onInternalTools,
                    )
                }
            }

            SettingsGap()
            SettingsGroup("Privacy") {
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
                    title = "Share when I'm online",
                    detail = "Let accepted friends see recent relay availability.",
                    checked = shareOnline,
                    onCheckedChange = {
                        shareOnline = it
                        RelayConfigStore.setShareOnline(context, it)
                    },
                )
            }

            SettingsGap()
            SettingsGroup("Backup") {
                SettingsLink(
                    title = "Back up account",
                    detail = "Export an encrypted copy of your identity and messages.",
                    onClick = onBackUp,
                )
            }

            SettingsGap()
            SettingsGroup("About & legal") {
                SettingsLink("Help & support", SUPPORT_URL) {
                    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(SUPPORT_URL)))
                }
                SettingsLink("Terms of use", null) {
                    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(TERMS_OF_USE_URL)))
                }
                SettingsLink("Privacy policy", null) {
                    context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(PRIVACY_POLICY_URL)))
                }
            }

            Text(
                appVersionLabel(context),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 28.dp),
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
    if (!configured) return "Set up Cruise Pass"
    return when (health) {
        RelayHealth.NoConfig,
        RelayHealth.Checking -> "Checking Cruise Pass setup…"
        is RelayHealth.Ok -> "Cruise Pass is working"
        RelayHealth.NoInternet -> "Cruise Pass is waiting for internet"
        is RelayHealth.Failing -> "Cruise Pass needs attention"
        is RelayHealth.Expired -> "Cruise Pass expired"
        is RelayHealth.Suspended -> "Cruise Pass suspended"
        is RelayHealth.TokenRejected -> "Cruise Pass setup was rejected"
        is RelayHealth.QuotaFull -> stringResource(R.string.ui_cruise_pass_storage_full_title)
        is RelayHealth.MessageTooLarge -> stringResource(R.string.ui_cruise_pass_message_too_large_title)
        is RelayHealth.RateLimited -> stringResource(R.string.ui_cruise_pass_slowed_title)
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
        is RelayHealth.Failing -> "The relay could not be reached."
        is RelayHealth.Expired -> "Renew your pass to resume internet delivery."
        is RelayHealth.Suspended -> "Contact support for help with this pass."
        is RelayHealth.TokenRejected -> "Paste the setup card again, or use a different Cruise Pass."
        is RelayHealth.QuotaFull -> stringResource(R.string.ui_cruise_pass_storage_full_detail)
        is RelayHealth.MessageTooLarge -> stringResource(R.string.ui_cruise_pass_message_too_large_detail)
        is RelayHealth.RateLimited -> stringResource(R.string.ui_cruise_pass_slowed_detail)
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
            stringResource(R.string.ui_cruise_pass_ready),
        )
        PassIndicator.WAITING -> Triple(
            Icons.Filled.Info,
            palette.neutral,
            stringResource(R.string.ui_cruise_pass_waiting_for_internet),
        )
        PassIndicator.ATTENTION -> Triple(
            PassQuestionIcon,
            palette.recent,
            stringResource(R.string.ui_cruise_pass_needs_attention),
        )
        PassIndicator.ACTION_REQUIRED -> Triple(
            PassExclamationIcon,
            MaterialTheme.colorScheme.error,
            stringResource(R.string.ui_cruise_pass_needs_action),
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
        modifier = Modifier.fillMaxWidth().padding(vertical = 8.dp),
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
        Switch(checked = checked, onCheckedChange = onCheckedChange)
    }
}

@Composable
private fun SettingsGap() = Spacer(modifier = Modifier.height(20.dp))
