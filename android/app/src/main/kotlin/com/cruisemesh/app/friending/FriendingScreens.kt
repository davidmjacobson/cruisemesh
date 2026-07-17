package com.cruisemesh.app.friending

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.os.Build
import android.view.HapticFeedbackConstants
import android.widget.Toast
import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.border
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Person
import androidx.compose.foundation.clickable
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.draw.clip
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.chat.ChatEvents
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.AppStore
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.ui.AvatarBadge
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.FriendSuggestion
import uniffi.cruisemesh_core.friendCardUserId
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.makeFriendCard
import uniffi.cruisemesh_core.makeFriendLink
import uniffi.cruisemesh_core.parseFriendText

/** Shows this device's own FriendCard (DESIGN.md §6.2) as a QR code to be scanned by a peer. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun MyQrScreen(identity: Identity, onSayHi: (Contact) -> Unit, onBack: () -> Unit) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val savedRelay = remember { RelayConfigStore.load(context) }
    var name by remember { mutableStateOf(ProfileStore.loadDisplayName(context)) }
    var relayUrl by remember { mutableStateOf(savedRelay?.relayUrl.orEmpty()) }
    var relayToken by remember { mutableStateOf(savedRelay?.relayToken.orEmpty()) }
    var showAdvanced by remember { mutableStateOf(false) }
    var connectedFriend by remember { mutableStateOf<FriendAddedOutcome?>(null) }
    val pendingImports by FriendImportEvents.pendingImports.collectAsState()
    val fingerprint = remember(identity.userId) { fingerprintWords(identity.userId) }
    val friendLink = remember(name, relayUrl, relayToken, identity) {
        val cardJson =
        makeFriendCard(
            name.trim().ifEmpty { ProfileStore.defaultDisplayName() },
            identity,
            relayUrl.trim().ifEmpty { null },
            relayToken.trim().ifEmpty { null },
        )
        makeFriendLink(cardJson)
    }
    val appLink = remember(friendLink) { "https://cruisemesh.app/f#$friendLink" }
    val qrBitmap = remember(appLink) { encodeQrBitmap(appLink) }

    androidx.compose.runtime.LaunchedEffect(pendingImports, connectedFriend) {
        if (connectedFriend != null) return@LaunchedEffect
        val pending = pendingImports.firstOrNull() ?: return@LaunchedEffect
        FriendImportEvents.consume(pending.id)
        val event = pending.value
        if (event.directBle) {
            connectedFriend = FriendAddedOutcome(
                contact = event.contact,
                delivery = FriendRequestDelivery(reachedDirectly = true, lamport = 0uL),
                relayConfigured = RelayConfigStore.load(context) != null,
            )
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("My friend card") },
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
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text(
                "Let a friend scan this code to add you.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Surface(
                shape = RoundedCornerShape(20.dp),
                color = Color.White,
                tonalElevation = 2.dp,
                shadowElevation = 2.dp,
            ) {
                Image(
                    bitmap = qrBitmap,
                    contentDescription = "Friend card QR code",
                    modifier = Modifier
                        .padding(16.dp)
                        .size(240.dp),
                )
            }
            Text(
                fingerprint.joinToString(" "),
                style = MaterialTheme.typography.bodyLarge,
                modifier = Modifier.padding(horizontal = 8.dp),
            )
            OutlinedTextField(
                value = name,
                onValueChange = {
                    name = it
                    ProfileStore.saveDisplayName(context, it)
                },
                label = { Text("Your name") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth(),
            )

            TextButton(onClick = { showAdvanced = !showAdvanced }) {
                Text(if (showAdvanced) "Hide advanced relay settings" else "Advanced relay settings")
            }
            if (showAdvanced) {
                OutlinedTextField(
                    value = relayUrl,
                    onValueChange = {
                        relayUrl = it
                        RelayConfigStore.save(context, relayUrl = it, relayToken = relayToken)
                    },
                    label = { Text("Relay URL (optional)") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = relayToken,
                    onValueChange = {
                        relayToken = it
                        RelayConfigStore.save(context, relayUrl = relayUrl, relayToken = it)
                    },
                    label = { Text("Relay token (optional)") },
                    visualTransformation = PasswordVisualTransformation(),
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
            }

            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(12.dp),
            ) {
                Button(
                    onClick = {
                        val text = "Add me on CruiseMesh: $appLink"
                        val intent = Intent(Intent.ACTION_SEND)
                            .setType("text/plain")
                            .putExtra(Intent.EXTRA_TEXT, text)
                        context.startActivity(Intent.createChooser(intent, "Share friend card"))
                    },
                    modifier = Modifier.weight(1f),
                ) {
                    Text("Share card")
                }
                TextButton(
                    onClick = {
                        val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                        clipboard.setPrimaryClip(ClipData.newPlainText("CruiseMesh friend card", appLink))
                        Toast.makeText(context, "Copied friend card link", Toast.LENGTH_SHORT).show()
                    },
                ) {
                    Text("Copy")
                }
            }
        }
    }

    connectedFriend?.let { outcome ->
        FriendConfirmationSheet(
            outcome = outcome,
            ownUserId = identity.userId,
            store = store,
            onSayHi = { onSayHi(outcome.contact) },
            onAddAnother = null,
            onDone = { connectedFriend = null },
        )
    }
}

/** Scans a peer's FriendCard QR code and imports them as a contact (DESIGN.md §6.2). */
@Composable
fun ScanScreen(
    ownUserId: ByteArray,
    store: uniffi.cruisemesh_core.MessageStore,
    onContactAdded: (Contact) -> FriendAddedOutcome,
    onSayHi: (Contact) -> Unit,
    onDone: () -> Unit,
    onBack: () -> Unit = onDone,
) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    val view = LocalView.current
    var status by remember { mutableStateOf("Point the camera at a CruiseMesh friend card") }
    var added by remember { mutableStateOf<FriendAddedOutcome?>(null) }
    var frozenFrame by remember { mutableStateOf<androidx.compose.ui.graphics.ImageBitmap?>(null) }

    Box(modifier = Modifier.fillMaxSize()) {
        if (added == null) {
            AndroidView(
                modifier = Modifier.fillMaxSize(),
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
                            QrAnalyzer { decoded ->
                                if (added != null) return@QrAnalyzer
                                try {
                                    val card = parseFriendText(decoded)
                                    val userId = friendCardUserId(card)
                                    if (userId.contentEquals(ownUserId)) {
                                        status = "That's your own card"
                                        return@QrAnalyzer
                                    }
                                    val contact = Contact(
                                        userId = userId,
                                        name = card.name,
                                        signPk = card.signPk,
                                        agreePk = card.agreePk,
                                        relayUrl = card.relayUrl,
                                        relayToken = card.relayToken,
                                    )
                                    frozenFrame = previewView.bitmap?.asImageBitmap()
                                    val outcome = onContactAdded(contact)
                                    added = outcome
                                    view.performHapticFeedback(
                                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                                            HapticFeedbackConstants.CONFIRM
                                        } else {
                                            HapticFeedbackConstants.LONG_PRESS
                                        },
                                    )
                                } catch (e: Exception) {
                                    status = "Not a CruiseMesh friend card"
                                }
                            },
                        )
                        try {
                            provider.unbindAll()
                            provider.bindToLifecycle(
                                lifecycleOwner,
                                CameraSelector.DEFAULT_BACK_CAMERA,
                                preview,
                                analysis,
                            )
                        } catch (e: Exception) {
                            status = "Camera error: ${e.message}"
                        }
                    }, ContextCompat.getMainExecutor(ctx))
                    previewView
                },
            )
            ScanViewfinderOverlay()
        }

        if (added != null && frozenFrame != null) {
            Image(
                bitmap = frozenFrame!!,
                contentDescription = null,
                contentScale = ContentScale.Crop,
                modifier = Modifier.fillMaxSize(),
            )
        }

        if (added == null) Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Bottom,
        ) {
            val currentAdded = added
            Surface(
                shape = RoundedCornerShape(24.dp),
                color = MaterialTheme.colorScheme.surface.copy(alpha = 0.88f),
                contentColor = MaterialTheme.colorScheme.onSurface,
                tonalElevation = 2.dp,
            ) {
                Text(
                    status,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 10.dp),
                )
            }
            if (currentAdded == null) {
                Button(onClick = onBack, modifier = Modifier.padding(top = 16.dp)) { Text("Cancel") }
            }
        }
    }

    added?.let { outcome ->
        FriendConfirmationSheet(
            outcome = outcome,
            ownUserId = ownUserId,
            store = store,
            onSayHi = { onSayHi(outcome.contact) },
            onAddAnother = {
                added = null
                frozenFrame = null
                status = "Point the camera at a CruiseMesh friend card"
            },
            onDone = onDone,
        )
    }
}

@Composable
private fun ScanViewfinderOverlay() {
    BoxWithConstraints(modifier = Modifier.fillMaxSize()) {
        val frameSize = minOf(maxWidth - 64.dp, 280.dp)
        val sideScrimWidth = ((maxWidth - frameSize) / 2).coerceAtLeast(0.dp)
        val topScrimHeight = ((maxHeight - frameSize) / 2).coerceAtLeast(0.dp)
        val scrim = Color.Black.copy(alpha = 0.46f)

        Box(
            modifier = Modifier
                .align(Alignment.TopCenter)
                .fillMaxWidth()
                .height(topScrimHeight)
                .background(scrim),
        )
        Box(
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .fillMaxWidth()
                .height(topScrimHeight)
                .background(scrim),
        )
        Box(
            modifier = Modifier
                .align(Alignment.CenterStart)
                .width(sideScrimWidth)
                .height(frameSize)
                .background(scrim),
        )
        Box(
            modifier = Modifier
                .align(Alignment.CenterEnd)
                .width(sideScrimWidth)
                .height(frameSize)
                .background(scrim),
        )
        Box(
            modifier = Modifier
                .align(Alignment.Center)
                .size(frameSize)
                .border(3.dp, Color.White.copy(alpha = 0.92f), RoundedCornerShape(28.dp)),
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AddFriendScreen(
    onScanClick: () -> Unit,
    onImportText: (String) -> ImportFriendResult,
    onConfirmContact: (Contact) -> FriendAddedOutcome,
    onRequestSuggestion: (FriendSuggestion) -> Boolean,
    onHideSuggestion: (FriendSuggestion) -> Unit,
    ownUserId: ByteArray,
    store: uniffi.cruisemesh_core.MessageStore,
    initialText: String = "",
    onSayHi: (Contact) -> Unit,
    onDone: () -> Unit,
    onBack: () -> Unit,
) {
    var pasted by remember(initialText) { mutableStateOf(initialText) }
    var error by remember { mutableStateOf<String?>(null) }
    var preview by remember { mutableStateOf<FriendPreview?>(null) }
    var added by remember { mutableStateOf<FriendAddedOutcome?>(null) }
    var suggestions by remember { mutableStateOf(emptyList<FriendSuggestion>()) }
    var confirmAddAll by remember { mutableStateOf(false) }
    val context = LocalContext.current

    fun reloadSuggestions() {
        suggestions = if (FriendsOfFriendsStore.isEnabled(context)) {
            store.listFriendSuggestions(System.currentTimeMillis())
        } else {
            emptyList()
        }
    }

    androidx.compose.runtime.LaunchedEffect(Unit) {
        reloadSuggestions()
        ChatEvents.changes.collect { reloadSuggestions() }
    }

    val groupedSuggestions = suggestions.groupBy { UserIdHex.encode(it.candidate.userId) }

    androidx.compose.runtime.LaunchedEffect(initialText) {
        if (initialText.isNotBlank()) {
            when (val result = onImportText(initialText)) {
                is ImportFriendResult.Preview -> preview = result.preview
                is ImportFriendResult.Error -> error = result.message
            }
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Add a friend") },
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
                .padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Text("Friends of friends", style = MaterialTheme.typography.titleMedium)
            if (!FriendsOfFriendsStore.isEnabled(context)) {
                Text(
                    "Friends-of-friends introductions are off in Profile & settings.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else if (groupedSuggestions.isEmpty()) {
                Text(
                    "Suggestions appear after your friends' phones sync.",
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                val available = groupedSuggestions.values.map { it.first() }.filter { it.state == 0.toUByte() }
                if (available.size > 1) {
                    TextButton(onClick = { confirmAddAll = true }, modifier = Modifier.align(Alignment.End)) {
                        Text("Add all (${available.size})")
                    }
                }
                groupedSuggestions.values.forEach { sources ->
                    val suggestion = sources.first()
                    val mutualNames = sources.mapNotNull { store.getContact(it.introducerUserId)?.name }.distinct()
                    Surface(
                        modifier = Modifier.fillMaxWidth(),
                        color = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.46f),
                        shape = RoundedCornerShape(16.dp),
                    ) {
                        Row(
                            modifier = Modifier.fillMaxWidth().padding(16.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text(suggestion.candidate.name, fontWeight = FontWeight.Medium)
                                Text(
                                    "Through ${mutualNames.ifEmpty { listOf("a mutual friend") }.joinToString()}",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                                TextButton(onClick = {
                                    onHideSuggestion(suggestion)
                                    reloadSuggestions()
                                }) { Text("Hide") }
                            }
                            Button(
                                onClick = {
                                    onRequestSuggestion(suggestion)
                                    reloadSuggestions()
                                },
                                enabled = suggestion.state == 0.toUByte(),
                            ) {
                                Text(if (suggestion.state == 1.toUByte()) "Requested" else "Add")
                            }
                        }
                    }
                }
            }

            Text("Add directly", style = MaterialTheme.typography.titleMedium)
            Button(onClick = onScanClick, modifier = Modifier.fillMaxWidth()) {
                Text("Scan QR code")
            }
            OutlinedTextField(
                value = pasted,
                onValueChange = {
                    pasted = it
                    error = null
                },
                label = { Text("Friend card") },
                minLines = 4,
                modifier = Modifier.fillMaxWidth(),
            )
            TextButton(
                onClick = {
                    val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
                    pasted = clipboard.primaryClip?.getItemAt(0)?.coerceToText(context)?.toString().orEmpty()
                    error = null
                },
                modifier = Modifier.align(Alignment.End),
            ) { Text("Paste") }
            Button(
                onClick = {
                    when (val result = onImportText(pasted)) {
                        is ImportFriendResult.Preview -> {
                            error = null
                            preview = result.preview
                        }
                        is ImportFriendResult.Error -> error = result.message
                    }
                },
                enabled = pasted.isNotBlank(),
                modifier = Modifier.fillMaxWidth(),
            ) {
                Text("Preview friend")
            }
            error?.let {
                Text(it, color = MaterialTheme.colorScheme.error)
            }
        }
    }

    if (confirmAddAll) {
        val available = groupedSuggestions.values.map { it.first() }.filter { it.state == 0.toUByte() }
        AlertDialog(
            onDismissRequest = { confirmAddAll = false },
            title = { Text("Add ${available.size} friends?") },
            text = { Text("CruiseMesh will request each connection through the mutual friends shown in the list.") },
            confirmButton = {
                TextButton(onClick = {
                    available.forEach { onRequestSuggestion(it) }
                    confirmAddAll = false
                    reloadSuggestions()
                }) { Text("Add all") }
            },
            dismissButton = {
                TextButton(onClick = { confirmAddAll = false }) { Text("Cancel") }
            },
        )
    }

    preview?.let { current ->
        FriendPreviewSheet(
            preview = current,
            onConfirm = {
                added = onConfirmContact(current.contact)
                preview = null
                pasted = ""
            },
            onDismiss = { preview = null },
        )
    }
    added?.let { outcome ->
        FriendConfirmationSheet(
            outcome = outcome,
            ownUserId = ownUserId,
            store = store,
            onSayHi = { onSayHi(outcome.contact) },
            onAddAnother = {
                added = null
                pasted = ""
            },
            onDone = onDone,
        )
    }
}

sealed interface ImportFriendResult {
    data class Preview(val preview: FriendPreview) : ImportFriendResult
    data class Error(val message: String) : ImportFriendResult
}

/**
 * Lists accepted contacts (DESIGN.md §6.2); tapping a row opens its 1:1 chat,
 * long-pressing offers deletion behind a confirmation dialog. Deleting exists
 * mainly for dead contacts -- a peer whose identity changed (e.g. reinstall)
 * leaves a row whose chat can never receive again -- and it removes the chat
 * history along with the contact (see MessageStore.deleteContact), which the
 * dialog copy states plainly.
 */
@OptIn(ExperimentalFoundationApi::class, ExperimentalMaterial3Api::class)
@Composable
fun ContactsScreen(
    contacts: List<Contact>,
    avatarBytesByUserId: Map<String, ByteArray> = emptyMap(),
    onContactClick: (Contact) -> Unit,
    onContactDelete: (Contact) -> Unit,
    onAddFriendClick: () -> Unit,
    onMyCardClick: () -> Unit,
    onNewGroupClick: () -> Unit = {},
    onBack: () -> Unit,
) {
    var pendingDelete by remember { mutableStateOf<Contact?>(null) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("New chat") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                }
            )
        }
    ) { innerPadding ->
        Column(modifier = Modifier.fillMaxSize().padding(innerPadding)) {
            LazyColumn(modifier = Modifier.fillMaxWidth()) {
                item {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable(onClick = onAddFriendClick)
                            .padding(16.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Box(
                            modifier = Modifier
                                .size(48.dp)
                                .clip(CircleShape)
                                .background(MaterialTheme.colorScheme.surfaceVariant),
                            contentAlignment = Alignment.Center
                        ) {
                            Icon(Icons.Default.Add, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        Spacer(modifier = Modifier.width(16.dp))
                        Text(
                            text = "Add a friend",
                            style = MaterialTheme.typography.bodyLarge.copy(fontWeight = FontWeight.Medium)
                        )
                    }
                }
                item {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable(onClick = onNewGroupClick)
                            .padding(horizontal = 16.dp, vertical = 8.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Box(
                            modifier = Modifier
                                .size(48.dp)
                                .clip(CircleShape)
                                .background(MaterialTheme.colorScheme.surfaceVariant),
                            contentAlignment = Alignment.Center
                        ) {
                            AvatarBadge(
                                userId = byteArrayOf(0x47, 0x52),
                                name = "New group",
                                displayId = "New group",
                                size = 48.dp,
                                isGroup = true,
                            )
                        }
                        Spacer(modifier = Modifier.width(16.dp))
                        Text(
                            text = "New group",
                            style = MaterialTheme.typography.bodyLarge.copy(fontWeight = FontWeight.Medium)
                        )
                    }
                }
                item {
                    Row(
                        modifier = Modifier
                            .fillMaxWidth()
                            .clickable(onClick = onMyCardClick)
                            .padding(horizontal = 16.dp, vertical = 8.dp)
                            .padding(bottom = 8.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        Box(
                            modifier = Modifier
                                .size(48.dp)
                                .clip(CircleShape)
                                .background(MaterialTheme.colorScheme.surfaceVariant),
                            contentAlignment = Alignment.Center
                        ) {
                            Icon(Icons.Default.Person, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
                        }
                        Spacer(modifier = Modifier.width(16.dp))
                        Text(
                            text = "My friend card",
                            style = MaterialTheme.typography.bodyLarge.copy(fontWeight = FontWeight.Medium)
                        )
                    }
                }
                
                if (contacts.isNotEmpty()) {
                    item {
                        Text(
                            "Contacts",
                            style = MaterialTheme.typography.labelMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(start = 16.dp, top = 8.dp, bottom = 8.dp)
                        )
                    }
                    items(contacts) { contact ->
                        val displayId = uniffi.cruisemesh_core.formatUserId(contact.userId)
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .combinedClickable(
                                    onClick = { onContactClick(contact) },
                                    onLongClick = { pendingDelete = contact },
                                )
                                .padding(16.dp),
                            verticalAlignment = Alignment.CenterVertically
                        ) {
                            AvatarBadge(
                                userId = contact.userId,
                                name = contact.name,
                                displayId = displayId,
                                photoBytes = avatarBytesByUserId[displayId],
                            )
                            Spacer(modifier = Modifier.width(16.dp))
                            Text(
                                contact.name,
                                style = MaterialTheme.typography.bodyLarge
                            )
                        }
                    }
                } else {
                    item {
                        Text(
                            "No contacts yet",
                            style = MaterialTheme.typography.bodyMedium,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            modifier = Modifier.padding(16.dp)
                        )
                    }
                }
            }
        }
    }

    val toDelete = pendingDelete
    if (toDelete != null) {
        AlertDialog(
            onDismissRequest = { pendingDelete = null },
            title = { Text("Delete ${toDelete.name}?") },
            text = { Text("This removes the contact and deletes your chat history with them. It can't be undone.") },
            confirmButton = {
                TextButton(
                    onClick = {
                        pendingDelete = null
                        onContactDelete(toDelete)
                    },
                ) {
                    Text("Delete")
                }
            },
            dismissButton = {
                TextButton(onClick = { pendingDelete = null }) {
                    Text("Cancel")
                }
            },
        )
    }
}
