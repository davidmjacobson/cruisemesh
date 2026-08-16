package com.cruisemesh.app.ui

import android.content.Intent
import android.content.res.Configuration
import android.widget.Toast
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.OutlinedButton
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
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.debug.FieldMetricsExport
import com.cruisemesh.app.mesh.InboundEngine
import com.cruisemesh.app.mesh.InboundEngineSettings
import com.cruisemesh.app.mesh.MeetEngine
import com.cruisemesh.app.mesh.MeetEngineSettings
import com.cruisemesh.app.relay.RelayEngineSettings
import com.cruisemesh.app.relay.RelayPassEngine
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R

/**
 * Engine rollout switches and diagnostic exports, and nothing else.
 *
 * Reached on a debuggable build outright, and on a release build once someone
 * has done the seven-tap run on the version line in Settings. It has to be
 * reachable on a release build: a closed-test build is release-signed, and a
 * staged-rollout canary whose switches only exist in a developer's own build
 * can never produce the field evidence it exists to produce.
 *
 * Deliberately not a second home for anything a person can already do on a
 * visible screen. Relay URL and token entry lives on the Shore Pass screen,
 * under "Custom relay", which checks the pair against the relay before saving
 * it; this screen used to carry a second copy that saved whatever was typed,
 * unchecked, on the way out.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DeveloperSettingsScreen(onBack: () -> Unit) {
    val context = LocalContext.current

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_developer_settings)) },
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
                .padding(24.dp),
        ) {
            // On a release build these switches are only here because someone
            // did the seven-tap run. Say plainly what they do before they are
            // touched. Debuggable builds skip it -- a developer knows.
            if (developerSettingsUnlockedOnRelease(context)) {
                Text(
                    stringResource(R.string.ui_developer_settings_release_warning),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.fillMaxWidth().padding(bottom = 20.dp),
                )
            }

            Text(stringResource(R.string.ui_diagnostics), style = MaterialTheme.typography.titleMedium)
            if (!DebugFileLog.isDebuggableBuild(context)) {
                var diagnosticLogging by remember { mutableStateOf(DebugFileLog.isOptedIn(context)) }
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
                ) {
                    Text(
                        stringResource(R.string.ui_diagnostic_logging),
                        modifier = Modifier.weight(1f),
                    )
                    Switch(
                        checked = diagnosticLogging,
                        onCheckedChange = {
                            diagnosticLogging = it
                            DebugFileLog.setOptIn(context, it)
                        },
                    )
                }
            }
            Text(stringResource(R.string.ui_diagnostic_logging_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )

            // The relay engine switch. It lives behind the seven-tap door
            // because a closed-test build is release-signed and cannot be
            // reached with `run-as` -- so a flag settable only from a unit test
            // could never produce the on-device evidence the migration needs
            // before its default may move.
            var rebuiltRelayEngine by remember {
                mutableStateOf(RelayEngineSettings.passEngine(context) == RelayPassEngine.CORE)
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
            ) {
                Text(
                    stringResource(R.string.ui_rebuilt_internet_sync),
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = rebuiltRelayEngine,
                    onCheckedChange = {
                        rebuiltRelayEngine = it
                        RelayEngineSettings.setPassEngine(
                            context,
                            if (it) RelayPassEngine.CORE else RelayPassEngine.LEGACY,
                        )
                    },
                )
            }
            Text(stringResource(R.string.ui_rebuilt_internet_sync_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )

            // The canary switch, which iOS has already. It defaults to on and
            // changes nothing the device sends or stores -- but a tester who
            // has to rule the canary out as a cause of something needs a way
            // to turn it off without a new build.
            var relayShadow by remember {
                mutableStateOf(RelayEngineSettings.shadowEnabled(context))
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
            ) {
                Text(
                    stringResource(R.string.ui_relay_migration_canary),
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = relayShadow,
                    onCheckedChange = {
                        relayShadow = it
                        RelayEngineSettings.setShadowEnabled(context, it)
                    },
                )
            }
            Text(stringResource(R.string.ui_relay_migration_canary_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )

            // The receive engine switch, here for the same reason as the one
            // above: the evidence that has to be gathered before its default
            // may move can only come from a release-signed device.
            var rebuiltInbound by remember {
                mutableStateOf(InboundEngineSettings.inboundEngine(context) == InboundEngine.CORE)
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
            ) {
                Text(
                    stringResource(R.string.ui_rebuilt_message_handling),
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = rebuiltInbound,
                    onCheckedChange = {
                        rebuiltInbound = it
                        InboundEngineSettings.setInboundEngine(
                            context,
                            if (it) InboundEngine.CORE else InboundEngine.LEGACY,
                        )
                    },
                )
            }
            Text(stringResource(R.string.ui_rebuilt_message_handling_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )

            // The nearby-exchange engine switch, alongside the two above and
            // for the same reason: a rollout switch whose evidence can only be
            // gathered on a release-signed device has to be reachable on one.
            var rebuiltMeet by remember {
                mutableStateOf(MeetEngineSettings.meetEngine(context) == MeetEngine.CORE)
            }
            Row(
                verticalAlignment = Alignment.CenterVertically,
                modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
            ) {
                Text(
                    stringResource(R.string.ui_rebuilt_nearby_exchange),
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = rebuiltMeet,
                    onCheckedChange = {
                        rebuiltMeet = it
                        MeetEngineSettings.setMeetEngine(
                            context,
                            if (it) MeetEngine.CORE else MeetEngine.LEGACY,
                        )
                    },
                )
            }
            Text(stringResource(R.string.ui_rebuilt_nearby_exchange_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )

            Button(
                onClick = {
                    val intent = DebugFileLog.shareIntent(context)
                    if (intent != null) {
                        context.startActivity(Intent.createChooser(intent, "Share debug log"))
                    } else {
                        Toast.makeText(context, R.string.ui_no_log_captured_yet, Toast.LENGTH_SHORT).show()
                    }
                },
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            ) { Text(stringResource(R.string.ui_share_debug_log)) }

            Text(stringResource(R.string.ui_export_field_metrics_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 12.dp),
            )
            OutlinedButton(
                onClick = {
                    val intent = FieldMetricsExport.shareIntent(context)
                    if (intent != null) {
                        context.startActivity(Intent.createChooser(intent, "Export field metrics"))
                    } else {
                        Toast.makeText(context, R.string.ui_no_field_metrics_yet, Toast.LENGTH_SHORT).show()
                    }
                },
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            ) { Text(stringResource(R.string.ui_export_field_metrics)) }

            // The way back out, for anyone who would rather not hunt for the
            // version line again. Only offered where it can do anything: a
            // debuggable build shows this screen regardless of the flag.
            //
            // No confirmation message: this navigates straight back to
            // Settings, and a toast fired on the way would land on top of the
            // version row there -- the one place in the app a stray tap
            // matters. The entry being gone is the confirmation.
            if (developerSettingsUnlockedOnRelease(context)) {
                OutlinedButton(
                    onClick = {
                        DeveloperSettingsUnlockStore.setUnlocked(context, false)
                        onBack()
                    },
                    modifier = Modifier.fillMaxWidth().padding(top = 24.dp),
                ) { Text(stringResource(R.string.ui_developer_settings_hide)) }
            }
        }
    }
}

@Preview(showBackground = true)
@Preview(showBackground = true, name = "Advanced Dark", uiMode = Configuration.UI_MODE_NIGHT_YES)
@Composable
private fun DeveloperSettingsScreenPreview() {
    CruiseMeshTheme { DeveloperSettingsScreen(onBack = {}) }
}
