package com.cruisemesh.app.devicelink

import android.Manifest
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.collectAsState
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import com.cruisemesh.app.R
import com.cruisemesh.app.friending.QrAnalyzer
import com.cruisemesh.app.friending.encodeQrBitmap
import com.cruisemesh.app.relay.RelayConfigStore
import uniffi.cruisemesh_core.CoreLinkImportReadiness
import uniffi.cruisemesh_core.CoreLinkOutcome
import uniffi.cruisemesh_core.CoreLinkRole
import uniffi.cruisemesh_core.Identity

/**
 * Internal Tools' device-link runner (`specs/multi-device-v1.md` §13, WP3
 * gate).
 *
 * Two dev builds, one ceremony, on the two transports the gate names: Wi‑Fi and
 * relay-only. It is not the family flow. WP6 owns "Your devices", the link and
 * remove journeys, and the words a family reads; what this screen owes is
 * enough surface to run §9 end to end on real hardware and see, plainly, which
 * step it reached and how it ended.
 *
 * It sits behind the same door as the rollout switches, and for the same
 * reason: a closed-test build is release-signed, and evidence that can only be
 * gathered on a release-signed device has to be reachable on one.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DeviceLinkTestScreen(identity: Identity, onBack: () -> Unit) {
    val context = LocalContext.current
    val session = remember(identity.userId) { LinkDevSession(context, identity) }
    DisposableEffect(session) { onDispose { session.close() } }
    val state by session.state.collectAsState()

    var role by remember { mutableStateOf(CoreLinkRole.NEW_DEVICE) }
    var transport by remember { mutableStateOf(LinkDevTransport.LAN) }
    var scannedCode by remember { mutableStateOf("") }
    val hasPass = remember { RelayConfigStore.load(context) != null }
    // §9.3: a phone that already holds someone's contacts and messages cannot
    // be adopted as a new device -- importing would fold two people's worlds
    // together with no way back. Read once, before anything starts: the answer
    // cannot change during a ceremony, and being told this after comparing six
    // digits with someone is the wrong end of the run to find out.
    val readiness = remember(identity.userId) { session.importReadiness() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_link_device_test)) },
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
            Text(
                stringResource(R.string.ui_link_device_test_desc),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )

            if (state.step == LinkDevStep.IDLE) {
                SetupControls(
                    role = role,
                    transport = transport,
                    hasPass = hasPass,
                    readiness = readiness,
                    scannedCode = scannedCode,
                    onRole = { role = it },
                    onTransport = { transport = it },
                    onScannedCode = { scannedCode = it },
                    onStart = {
                        when (role) {
                            CoreLinkRole.NEW_DEVICE -> session.startAsNewDevice(transport)
                            CoreLinkRole.APPROVING_DEVICE ->
                                session.startAsApprovingDevice(scannedCode.trim(), transport)
                        }
                    },
                )
            } else {
                RunningCeremony(
                    state = state,
                    onAnswer = session::answerDigits,
                    onStop = session::cancel,
                    onStartOver = {
                        scannedCode = ""
                        session.close()
                        onBack()
                    },
                )
            }
        }
    }
}

@Composable
private fun SetupControls(
    role: CoreLinkRole,
    transport: LinkDevTransport,
    hasPass: Boolean,
    readiness: CoreLinkImportReadiness,
    scannedCode: String,
    onRole: (CoreLinkRole) -> Unit,
    onTransport: (LinkDevTransport) -> Unit,
    onScannedCode: (String) -> Unit,
    onStart: () -> Unit,
) {
    Text(
        stringResource(R.string.ui_link_device_role),
        style = MaterialTheme.typography.titleMedium,
        modifier = Modifier.padding(top = 20.dp),
    )
    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.padding(top = 8.dp),
    ) {
        FilterChip(
            selected = role == CoreLinkRole.NEW_DEVICE,
            onClick = { onRole(CoreLinkRole.NEW_DEVICE) },
            label = { Text(stringResource(R.string.ui_link_device_role_new)) },
        )
        FilterChip(
            selected = role == CoreLinkRole.APPROVING_DEVICE,
            onClick = { onRole(CoreLinkRole.APPROVING_DEVICE) },
            label = { Text(stringResource(R.string.ui_link_device_role_approving)) },
        )
    }

    Text(
        stringResource(R.string.ui_link_device_how),
        style = MaterialTheme.typography.titleMedium,
        modifier = Modifier.padding(top = 20.dp),
    )
    Row(
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        modifier = Modifier.padding(top = 8.dp),
    ) {
        FilterChip(
            selected = transport == LinkDevTransport.LAN,
            onClick = { onTransport(LinkDevTransport.LAN) },
            label = { Text(stringResource(R.string.ui_link_device_over_wifi)) },
        )
        FilterChip(
            selected = transport == LinkDevTransport.RELAY,
            enabled = hasPass,
            onClick = { onTransport(LinkDevTransport.RELAY) },
            label = { Text(stringResource(R.string.ui_link_device_over_internet)) },
        )
    }
    if (!hasPass) {
        Text(
            stringResource(R.string.ui_link_device_needs_pass),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 4.dp),
        )
    }

    // Only the NEW-device role imports anything, so only it is refused here.
    val newDeviceReady = readiness == CoreLinkImportReadiness.READY
    if (role == CoreLinkRole.NEW_DEVICE && !newDeviceReady) {
        Text(
            stringResource(readinessLabel(readiness)),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(top = 20.dp),
        )
    }

    // The approving device needs the offer before there is anything to drive:
    // §9.2 starts with a scan, so the code is gathered here rather than mid-run.
    if (role == CoreLinkRole.APPROVING_DEVICE) {
        Text(
            stringResource(R.string.ui_link_device_scan_hint),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 20.dp),
        )
        LinkCodeScanner(onDecoded = onScannedCode)
        OutlinedTextField(
            value = scannedCode,
            onValueChange = onScannedCode,
            label = { Text(stringResource(R.string.ui_link_device_code_label)) },
            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
        )
    }

    Button(
        onClick = onStart,
        enabled = when (role) {
            CoreLinkRole.NEW_DEVICE -> newDeviceReady
            CoreLinkRole.APPROVING_DEVICE -> scannedCode.isNotBlank()
        },
        modifier = Modifier.fillMaxWidth().padding(top = 20.dp),
    ) { Text(stringResource(R.string.ui_link_device_start)) }
}

/**
 * Why this phone cannot be adopted, in a sentence. Never reached for
 * [CoreLinkImportReadiness.READY], which is the case with nothing to say.
 */
private fun readinessLabel(readiness: CoreLinkImportReadiness): Int = when (readiness) {
    CoreLinkImportReadiness.READY -> R.string.ui_link_device_needs_fresh_phone
    CoreLinkImportReadiness.STORE_HOLDS_SOMEONE -> R.string.ui_link_device_needs_fresh_phone
    CoreLinkImportReadiness.STORE_HOLDS_ANOTHER_PERSON ->
        R.string.ui_link_device_belongs_to_someone_else
}

@Composable
private fun RunningCeremony(
    state: LinkDevState,
    onAnswer: (Boolean) -> Unit,
    onStop: () -> Unit,
    onStartOver: () -> Unit,
) {
    val context = LocalContext.current
    Text(
        stringResource(stepLabel(state.step)),
        style = MaterialTheme.typography.titleMedium,
        modifier = Modifier.padding(top = 20.dp),
    )

    val qrText = state.qrText
    if (state.role == CoreLinkRole.NEW_DEVICE && qrText != null && state.step != LinkDevStep.DONE) {
        val bitmap = remember(qrText) { encodeQrBitmap(qrText) }
        Image(
            bitmap = bitmap,
            contentDescription = null,
            modifier = Modifier.fillMaxWidth().height(280.dp).padding(top = 12.dp),
        )
        Text(
            stringResource(R.string.ui_link_device_offer_hint),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 8.dp),
        )
        OutlinedButton(
            onClick = { copyToClipboard(context, qrText) },
            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
        ) { Text(stringResource(R.string.ui_link_device_copy_code)) }
    }

    val sas = state.sas
    if (sas != null && state.step == LinkDevStep.COMPARING_DIGITS) {
        Text(
            sas,
            style = MaterialTheme.typography.displaySmall,
            modifier = Modifier.padding(top = 16.dp),
        )
        if (state.warnSoftCap) {
            Text(
                stringResource(R.string.ui_link_device_soft_cap),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(top = 8.dp),
            )
        }
        // §9.2: the buttons exist only on the device that is already part of
        // this person. The other screen shows the same digits and waits.
        if (state.confirmHere) {
            Text(
                stringResource(R.string.ui_link_device_compare),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 8.dp),
            )
            Button(
                onClick = { onAnswer(true) },
                modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
            ) { Text(stringResource(R.string.ui_link_device_numbers_match)) }
            OutlinedButton(
                onClick = { onAnswer(false) },
                modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
            ) { Text(stringResource(R.string.ui_link_device_numbers_differ)) }
        }
    }

    val report = state.report
    if (report != null) {
        Text(
            stringResource(
                R.string.ui_link_device_report,
                report.deviceIdHex,
                report.rosterHeadHex,
                report.rosterSeq.toString(),
                report.contacts.toString(),
                report.groups.toString(),
                report.messages.toString(),
                report.catchUpChats.toString(),
            ),
            style = MaterialTheme.typography.bodySmall,
            modifier = Modifier.padding(top = 16.dp),
        )
    }

    outcomeLabel(state.outcome)?.let { label ->
        Text(
            stringResource(label),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 12.dp),
        )
    }

    // Raw, untranslated, and deliberately so: this is the exception text a
    // developer needs off a phone that failed, not copy anyone should read.
    state.failure?.let { detail ->
        Text(
            detail,
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(top = 12.dp),
        )
    }

    if (state.step == LinkDevStep.DONE || state.step == LinkDevStep.FAILED) {
        OutlinedButton(
            onClick = onStartOver,
            modifier = Modifier.fillMaxWidth().padding(top = 24.dp),
        ) { Text(stringResource(R.string.ui_link_device_start_over)) }
    } else {
        OutlinedButton(
            onClick = onStop,
            modifier = Modifier.fillMaxWidth().padding(top = 24.dp),
        ) { Text(stringResource(R.string.ui_link_device_stop)) }
    }
}

/**
 * A camera pointed at the other phone's code.
 *
 * Deliberately thin: it hands whatever ZXing decodes straight to the caller and
 * lets the core decide whether it is a link offer at all
 * (`CoreLinkApprovingDevice.scan` refuses anything that is not, including a
 * newer scheme this build cannot read).
 */
@Composable
private fun LinkCodeScanner(onDecoded: (String) -> Unit) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    var granted by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED,
        )
    }
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted = it }

    if (!granted) {
        Text(
            stringResource(R.string.ui_link_device_camera_needed),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 8.dp),
        )
        OutlinedButton(
            onClick = { permissionLauncher.launch(Manifest.permission.CAMERA) },
            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
        ) { Text(stringResource(R.string.ui_link_device_allow_camera)) }
        return
    }

    AndroidView(
        modifier = Modifier.fillMaxWidth().height(240.dp).padding(top = 12.dp),
        factory = { ctx ->
            val previewView = PreviewView(ctx)
            val providerFuture = ProcessCameraProvider.getInstance(ctx)
            providerFuture.addListener({
                val provider = providerFuture.get()
                val preview = Preview.Builder().build().also {
                    it.surfaceProvider = previewView.surfaceProvider
                }
                val analysis = ImageAnalysis.Builder()
                    .setBackpressureStrategy(ImageAnalysis.STRATEGY_KEEP_ONLY_LATEST)
                    .build()
                analysis.setAnalyzer(
                    ContextCompat.getMainExecutor(ctx),
                    QrAnalyzer { decoded -> onDecoded(decoded) },
                )
                runCatching {
                    provider.unbindAll()
                    provider.bindToLifecycle(
                        lifecycleOwner,
                        CameraSelector.DEFAULT_BACK_CAMERA,
                        preview,
                        analysis,
                    )
                }
            }, ContextCompat.getMainExecutor(ctx))
            previewView
        },
    )
}

private fun copyToClipboard(context: Context, text: String) {
    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    clipboard.setPrimaryClip(
        ClipData.newPlainText(context.getString(R.string.ui_link_device_code_label), text),
    )
}

private fun stepLabel(step: LinkDevStep): Int = when (step) {
    LinkDevStep.IDLE, LinkDevStep.WAITING_FOR_PEER -> R.string.ui_link_device_waiting
    LinkDevStep.HANDSHAKING -> R.string.ui_link_device_handshaking
    LinkDevStep.COMPARING_DIGITS -> R.string.ui_link_device_comparing
    LinkDevStep.CARRYING_BOOTSTRAP -> R.string.ui_link_device_carrying
    LinkDevStep.ACTIVATING -> R.string.ui_link_device_activating
    LinkDevStep.DONE -> R.string.ui_link_device_done
    LinkDevStep.FAILED -> R.string.ui_link_device_stopped
}

/**
 * Every ending the core names, said once. `ChannelReady` has no line of its own
 * because the run kept going past it -- the report below is what happened next.
 */
private fun outcomeLabel(outcome: CoreLinkOutcome?): Int? = when (outcome) {
    null, CoreLinkOutcome.CHANNEL_READY -> null
    CoreLinkOutcome.DECLINED -> R.string.ui_link_device_outcome_declined
    CoreLinkOutcome.CANCELLED -> R.string.ui_link_device_outcome_cancelled
    CoreLinkOutcome.TIMED_OUT -> R.string.ui_link_device_outcome_timed_out
    CoreLinkOutcome.QR_EXPIRED -> R.string.ui_link_device_outcome_expired
    CoreLinkOutcome.DEVICE_CAP_REACHED -> R.string.ui_link_device_outcome_full
    CoreLinkOutcome.HANDSHAKE_FAILED -> R.string.ui_link_device_outcome_handshake
    CoreLinkOutcome.PROTOCOL_ERROR -> R.string.ui_link_device_outcome_unexpected
}
