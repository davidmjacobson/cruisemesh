package com.cruisemesh.app.friending

import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.Image
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
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
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Person
import androidx.compose.foundation.clickable
import androidx.compose.foundation.background
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.size
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalLifecycleOwner
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.core.content.ContextCompat
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.relay.RelayConfigStore
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.friendCardUserId
import uniffi.cruisemesh_core.makeFriendCard
import uniffi.cruisemesh_core.parseFriendCard

/** Shows this device's own FriendCard (DESIGN.md §6.2) as a QR code to be scanned by a peer. */
@Composable
fun MyQrScreen(identity: Identity, onBack: () -> Unit) {
    val context = LocalContext.current
    val savedRelay = remember { RelayConfigStore.load(context) }
    var name by remember { mutableStateOf(ProfileStore.loadDisplayName(context)) }
    var relayUrl by remember { mutableStateOf(savedRelay?.relayUrl.orEmpty()) }
    var relayToken by remember { mutableStateOf(savedRelay?.relayToken.orEmpty()) }
    val cardJson = remember(name, relayUrl, relayToken, identity) {
        makeFriendCard(
            name.trim().ifEmpty { ProfileStore.defaultDisplayName() },
            identity,
            relayUrl.trim().ifEmpty { null },
            relayToken.trim().ifEmpty { null },
        )
    }
    val qrBitmap = remember(cardJson) { encodeQrBitmap(cardJson) }

    Scaffold { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            Text("Your friend card", style = MaterialTheme.typography.headlineSmall)
            OutlinedTextField(
                value = name,
                onValueChange = {
                    name = it
                    ProfileStore.saveDisplayName(context, it)
                },
                label = { Text("Your name") },
                modifier = Modifier.padding(top = 16.dp),
            )
            OutlinedTextField(
                value = relayUrl,
                onValueChange = {
                    relayUrl = it
                    RelayConfigStore.save(context, relayUrl = it, relayToken = relayToken)
                },
                label = { Text("Relay URL (optional)") },
                modifier = Modifier.padding(top = 12.dp),
            )
            OutlinedTextField(
                value = relayToken,
                onValueChange = {
                    relayToken = it
                    RelayConfigStore.save(context, relayUrl = relayUrl, relayToken = it)
                },
                label = { Text("Relay token (optional)") },
                modifier = Modifier.padding(top = 12.dp),
            )
            Image(
                bitmap = qrBitmap,
                contentDescription = "Friend card QR code",
                modifier = Modifier.padding(top = 24.dp),
            )
            Button(onClick = onBack, modifier = Modifier.padding(top = 24.dp)) {
                Text("Back")
            }
        }
    }
}

/** Scans a peer's FriendCard QR code and imports them as a contact (DESIGN.md §6.2). */
@Composable
fun ScanScreen(onContactAdded: (Contact) -> Unit, onBack: () -> Unit) {
    val context = LocalContext.current
    val lifecycleOwner = LocalLifecycleOwner.current
    var status by remember { mutableStateOf("Point the camera at a CruiseMesh friend card") }
    var added by remember { mutableStateOf<Contact?>(null) }

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
                                    val card = parseFriendCard(decoded)
                                    val contact = Contact(
                                        userId = friendCardUserId(card),
                                        name = card.name,
                                        signPk = card.signPk,
                                        agreePk = card.agreePk,
                                        relayUrl = card.relayUrl,
                                        relayToken = card.relayToken,
                                    )
                                    added = contact
                                    onContactAdded(contact)
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
        }

        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Bottom,
        ) {
            val currentAdded = added
            Text(
                if (currentAdded != null) "Added ${currentAdded.name} as a contact" else status,
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Button(onClick = onBack, modifier = Modifier.padding(top = 16.dp)) {
                Text(if (currentAdded != null) "Done" else "Cancel")
            }
        }
    }
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
    onContactClick: (Contact) -> Unit,
    onContactDelete: (Contact) -> Unit,
    onAddFriendClick: () -> Unit,
    onMyCardClick: () -> Unit,
    onBack: () -> Unit,
) {
    var pendingDelete by remember { mutableStateOf<Contact?>(null) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("New chat") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.Default.ArrowBack, contentDescription = "Back")
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
                        val (avatarColor, initials) = com.cruisemesh.app.ui.ChatListLogic.avatarHueAndInitials(
                            contact.userId, contact.name, uniffi.cruisemesh_core.formatUserId(contact.userId)
                        )
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
                            Box(
                                modifier = Modifier
                                    .size(48.dp)
                                    .clip(CircleShape)
                                    .background(avatarColor),
                                contentAlignment = Alignment.Center
                            ) {
                                Text(
                                    text = initials,
                                    color = Color.White,
                                    style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.Bold)
                                )
                            }
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
