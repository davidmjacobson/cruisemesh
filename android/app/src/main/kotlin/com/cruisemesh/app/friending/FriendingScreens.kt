package com.cruisemesh.app.friending

import androidx.camera.core.CameraSelector
import androidx.camera.core.ImageAnalysis
import androidx.camera.core.Preview
import androidx.camera.lifecycle.ProcessCameraProvider
import androidx.camera.view.PreviewView
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
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
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.friendCardUserId
import uniffi.cruisemesh_core.makeFriendCard
import uniffi.cruisemesh_core.parseFriendCard

/** Shows this device's own FriendCard (DESIGN.md §6.2) as a QR code to be scanned by a peer. */
@Composable
fun MyQrScreen(identity: Identity, onBack: () -> Unit) {
    var name by remember { mutableStateOf(android.os.Build.MODEL ?: "CruiseMesh user") }
    val cardJson = remember(name, identity) { makeFriendCard(name, identity, null) }
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
                onValueChange = { name = it },
                label = { Text("Your name") },
                modifier = Modifier.padding(top = 16.dp),
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

/** Lists accepted contacts (DESIGN.md §6.2); tapping a row opens its 1:1 chat. */
@Composable
fun ContactsScreen(contacts: List<Contact>, onContactClick: (Contact) -> Unit, onBack: () -> Unit) {
    Scaffold { innerPadding ->
        Column(modifier = Modifier.fillMaxSize().padding(innerPadding).padding(24.dp)) {
            Text("Contacts", style = MaterialTheme.typography.headlineSmall)
            if (contacts.isEmpty()) {
                Text(
                    "No contacts yet -- scan a friend card to add one.",
                    modifier = Modifier.padding(top = 16.dp),
                )
            } else {
                LazyColumn(modifier = Modifier.fillMaxWidth().padding(top = 16.dp)) {
                    items(contacts) { contact ->
                        Text(
                            contact.name,
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable { onContactClick(contact) }
                                .padding(vertical = 8.dp),
                        )
                    }
                }
            }
            Button(onClick = onBack, modifier = Modifier.padding(top = 24.dp)) {
                Text("Back")
            }
        }
    }
}
