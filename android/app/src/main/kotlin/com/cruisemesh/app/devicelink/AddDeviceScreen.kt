package com.cruisemesh.app.devicelink

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Column
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
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
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
 * §9's ceremony, in the words a family reads (`specs/multi-device-v1.md` §13
 * WP6).
 *
 * One screen, two ends, and which end this is never asked as a question — it is
 * decided by the door the person came through:
 *
 * * From **Settings → Your devices → Add a device**, this phone is the one that
 *   is already set up, so it scans and it holds the confirm ([§9.2]'s "the user
 *   confirms match on the existing device").
 * * From **the restore flow's "Set up as a new device"**, this phone is the new
 *   one, so it shows the code and waits.
 *
 * There is no role picker because there is no question: a person holding two
 * phones already knows which is which, and asking them to say so is the kind of
 * step that gets answered wrong.
 *
 * The code on screen carries ephemeral link material and rendezvous hints only —
 * §9.1's rule, enforced in core, and the reason this screen may show a QR at
 * all. Nothing here reads, renders or logs an identity secret.
 *
 * @param onLinked where a phone that was just adopted goes: into the app, never
 *   back the way it came. See [LinkCompletion] for who this is for and why the
 *   other endings must not use it.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AddDeviceScreen(
    identity: Identity,
    role: CoreLinkRole,
    expectedPersonId: ByteArray? = null,
    onBack: () -> Unit,
    onLinked: () -> Unit = onBack,
) {
    val context = LocalContext.current
    val session = remember(identity.userId, role) {
        LinkSession(context, identity, expectedPersonId)
    }
    DisposableEffect(session) { onDispose { session.close() } }
    val state by session.state.collectAsState()

    var transport by remember { mutableStateOf(LinkTransport.LAN) }
    var scannedCode by remember { mutableStateOf("") }
    var showHowTheyConnect by remember { mutableStateOf(false) }
    val hasPass = remember { RelayConfigStore.load(context) != null }
    // §9.3: a phone that already holds someone's contacts and messages cannot be
    // adopted. Read once, before anything starts -- the answer cannot change
    // during a ceremony, and being told this after comparing six digits with
    // somebody is the wrong end of the run to find out.
    val readiness = remember(identity.userId) {
        if (role == CoreLinkRole.NEW_DEVICE) session.importReadiness() else CoreLinkImportReadiness.READY
    }
    // The backstop for the same rule "Your devices" already gates the button on
    // (§9.5: only the approving device can sign the roster the new one joins).
    // Asked here as well because this screen is reachable by other routes -- a
    // saved back stack, a deep link, a future entry point -- and the failure it
    // prevents happens at the very END of the ceremony, after two people have
    // compared six digits. Read once, before anything starts, for the same
    // reason as the readiness check above.
    val canApprove = remember(identity.userId) {
        role != CoreLinkRole.APPROVING_DEVICE || session.canSignRoster()
    }
    // Every way out of this screen, not just the button. A phone that has been
    // adopted is set up, and the back arrow and the system back gesture must
    // not be the two doors that land it back on first-run setup -- which is
    // exactly what the 2026-08-18 two-phone session saw: step 1 again, still
    // offering "This is another of my devices", then asking a linked person
    // their own name. [LinkCompletion] decides, once, for all three.
    val onExit = {
        // Read before closing: the answer is about the run that just ended, and
        // nothing a teardown does afterwards may change it.
        val entersApp = LinkCompletion.entersApp(role, state.step)
        session.close()
        if (entersApp) onLinked() else onBack()
    }
    BackHandler(onBack = onExit)

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_add_a_device)) },
                navigationIcon = {
                    IconButton(onClick = onExit) {
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
            if (state.step == LinkStep.IDLE) {
                BeforeYouStart(
                    role = role,
                    readiness = readiness,
                    canApprove = canApprove,
                    transport = transport,
                    showHowTheyConnect = showHowTheyConnect,
                    hasPass = hasPass,
                    scannedCode = scannedCode,
                    onToggleHowTheyConnect = { showHowTheyConnect = !showHowTheyConnect },
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
                LinkInProgress(
                    state = state,
                    onAnswer = session::answerDigits,
                    onStop = session::cancel,
                    // One ending, two meanings: a phone that was just adopted is
                    // set up and belongs in the app, and everything else belongs
                    // back where it came from. Shared with the back arrow and
                    // the system back gesture so no exit can disagree.
                    onFinish = onExit,
                )
            }
        }
    }
}

@Composable
private fun BeforeYouStart(
    role: CoreLinkRole,
    readiness: CoreLinkImportReadiness,
    canApprove: Boolean,
    transport: LinkTransport,
    showHowTheyConnect: Boolean,
    hasPass: Boolean,
    scannedCode: String,
    onToggleHowTheyConnect: () -> Unit,
    onTransport: (LinkTransport) -> Unit,
    onScannedCode: (String) -> Unit,
    onStart: () -> Unit,
) {
    Text(
        stringResource(
            when (role) {
                CoreLinkRole.NEW_DEVICE -> R.string.ui_add_device_new_intro
                CoreLinkRole.APPROVING_DEVICE -> R.string.ui_add_device_existing_intro
            },
        ),
        style = MaterialTheme.typography.bodyMedium,
    )

    val ready = readiness == CoreLinkImportReadiness.READY
    if (!ready) {
        Text(
            stringResource(readinessCopy(readiness)),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(top = 20.dp),
        )
    }
    if (!canApprove) {
        Text(
            stringResource(R.string.ui_add_a_device_not_the_approver),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.error,
            modifier = Modifier.padding(top = 20.dp),
        )
    }

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

    // Advanced, and behind a disclosure, because the answer is right by default:
    // two phones in the same hands are on the same Wi-Fi, and the only reason to
    // choose otherwise is a person linking a phone that is somewhere else.
    TextButton(
        onClick = onToggleHowTheyConnect,
        modifier = Modifier.padding(top = 12.dp),
    ) {
        Text(
            stringResource(
                if (showHowTheyConnect) R.string.ui_hide_details else R.string.ui_details,
            ),
        )
    }
    if (showHowTheyConnect) {
        Text(
            stringResource(R.string.ui_link_device_how),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        LinkTransportChoice(
            transport = transport,
            hasPass = hasPass,
            onTransport = onTransport,
        )
        if (!hasPass) {
            Text(
                stringResource(R.string.ui_link_device_needs_pass),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
        }
    }

    Button(
        onClick = onStart,
        enabled = when (role) {
            CoreLinkRole.NEW_DEVICE -> ready
            CoreLinkRole.APPROVING_DEVICE -> canApprove && scannedCode.isNotBlank()
        },
        modifier = Modifier.fillMaxWidth().padding(top = 20.dp),
    ) { Text(stringResource(R.string.ui_link_device_start)) }
}

@Composable
private fun LinkTransportChoice(
    transport: LinkTransport,
    hasPass: Boolean,
    onTransport: (LinkTransport) -> Unit,
) {
    TextButton(onClick = { onTransport(LinkTransport.LAN) }) {
        Text(
            stringResource(R.string.ui_link_device_over_wifi),
            color = if (transport == LinkTransport.LAN) {
                MaterialTheme.colorScheme.primary
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
        )
    }
    TextButton(onClick = { onTransport(LinkTransport.RELAY) }, enabled = hasPass) {
        Text(
            stringResource(R.string.ui_link_device_over_internet),
            color = if (transport == LinkTransport.RELAY) {
                MaterialTheme.colorScheme.primary
            } else {
                MaterialTheme.colorScheme.onSurfaceVariant
            },
        )
    }
}

@Composable
private fun LinkInProgress(
    state: LinkState,
    onAnswer: (Boolean) -> Unit,
    onStop: () -> Unit,
    onFinish: () -> Unit,
) {
    Text(
        stringResource(stepCopy(state.step)),
        style = MaterialTheme.typography.titleMedium,
    )

    val qrText = state.qrText
    if (state.role == CoreLinkRole.NEW_DEVICE && qrText != null && state.step != LinkStep.DONE) {
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
        // The way through when a camera will not focus, which on an old phone
        // held at arm's length is not rare. The code is ephemeral link material
        // and rendezvous hints (§9.1) -- never an identity secret -- so a person
        // may copy it and paste it into the other phone by any means they like.
        val context = LocalContext.current
        OutlinedButton(
            onClick = { copyLinkCode(context, qrText) },
            modifier = Modifier.fillMaxWidth().padding(top = 8.dp),
        ) { Text(stringResource(R.string.ui_link_device_copy_code)) }
    }

    val sas = state.sas
    if (sas != null && state.step == LinkStep.COMPARING_DIGITS) {
        Text(sas, style = MaterialTheme.typography.displaySmall, modifier = Modifier.padding(top = 16.dp))
        if (state.warnSoftCap) {
            Text(
                stringResource(R.string.ui_link_device_soft_cap),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.error,
                modifier = Modifier.padding(top = 8.dp),
            )
        }
        // §9.2: the buttons exist only on the phone that is already part of this
        // person. The other screen shows the same digits and waits.
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

    // What arrived, counted, with nothing a person cannot act on. The device id
    // and roster head the WP3 runner printed were evidence for a developer
    // standing over two phones; this is the same run seen by the family.
    //
    // Only on the new device. The approving side receives nothing -- it sends --
    // so its report carries zeroes by construction, and "Brought over 0 contacts
    // and 0 messages" under a successful link reads as a failure to the one
    // person who has to trust that it worked.
    state.report?.takeIf { state.role == CoreLinkRole.NEW_DEVICE }?.let { report ->
        Text(
            stringResource(
                R.string.ui_add_device_brought_over,
                report.contacts.toString(),
                report.messages.toString(),
            ),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 16.dp),
        )
        if (report.catchUpChats > 0) {
            Text(
                stringResource(R.string.ui_add_device_older_messages_coming),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
        }
    }

    val outcomeLabel = outcomeCopy(state.outcome)
    if (outcomeLabel != null) {
        Text(
            stringResource(outcomeLabel),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 12.dp),
        )
    } else if (state.step == LinkStep.FAILED) {
        // A ceremony that threw rather than ended has no CoreLinkOutcome to
        // word, and "Stopped" on its own reads as something the person did. The
        // generic line names the two things that are actually worth trying.
        Text(
            stringResource(R.string.ui_link_device_failed_generic),
            style = MaterialTheme.typography.bodyMedium,
            modifier = Modifier.padding(top = 12.dp),
        )
    }

    if (state.step == LinkStep.DONE || state.step == LinkStep.FAILED) {
        OutlinedButton(
            onClick = onFinish,
            modifier = Modifier.fillMaxWidth().padding(top = 24.dp),
        ) { Text(stringResource(R.string.ui_done)) }
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

private fun copyLinkCode(context: android.content.Context, code: String) {
    val clipboard = context.getSystemService(android.content.Context.CLIPBOARD_SERVICE)
        as android.content.ClipboardManager
    clipboard.setPrimaryClip(
        android.content.ClipData.newPlainText(
            context.getString(R.string.ui_link_device_code_label),
            code,
        ),
    )
}

/** Why this phone cannot be adopted. Never reached for a ready store. */
internal fun readinessCopy(readiness: CoreLinkImportReadiness): Int = when (readiness) {
    CoreLinkImportReadiness.READY -> R.string.ui_link_device_needs_fresh_phone
    CoreLinkImportReadiness.STORE_HOLDS_SOMEONE -> R.string.ui_link_device_needs_fresh_phone
    CoreLinkImportReadiness.STORE_HOLDS_ANOTHER_PERSON ->
        R.string.ui_link_device_belongs_to_someone_else
}

internal fun stepCopy(step: LinkStep): Int = when (step) {
    LinkStep.IDLE, LinkStep.WAITING_FOR_PEER -> R.string.ui_link_device_waiting
    LinkStep.HANDSHAKING -> R.string.ui_link_device_handshaking
    LinkStep.COMPARING_DIGITS -> R.string.ui_link_device_comparing
    LinkStep.CARRYING_BOOTSTRAP -> R.string.ui_link_device_carrying
    LinkStep.ACTIVATING -> R.string.ui_link_device_activating
    LinkStep.DONE -> R.string.ui_link_device_done
    LinkStep.FAILED -> R.string.ui_link_device_stopped
}

/**
 * Every ending the core names, said once. `ChannelReady` has no line of its own
 * because the run kept going past it — the counts above are what happened next.
 */
internal fun outcomeCopy(outcome: CoreLinkOutcome?): Int? = when (outcome) {
    null, CoreLinkOutcome.CHANNEL_READY -> null
    CoreLinkOutcome.DECLINED -> R.string.ui_link_device_outcome_declined
    CoreLinkOutcome.CANCELLED -> R.string.ui_link_device_outcome_cancelled
    CoreLinkOutcome.TIMED_OUT -> R.string.ui_link_device_outcome_timed_out
    CoreLinkOutcome.QR_EXPIRED -> R.string.ui_link_device_outcome_expired
    CoreLinkOutcome.DEVICE_CAP_REACHED -> R.string.ui_link_device_outcome_full
    CoreLinkOutcome.HANDSHAKE_FAILED -> R.string.ui_link_device_outcome_handshake
    CoreLinkOutcome.PROTOCOL_ERROR -> R.string.ui_link_device_outcome_unexpected
}
