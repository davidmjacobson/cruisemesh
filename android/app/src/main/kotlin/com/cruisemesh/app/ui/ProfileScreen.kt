package com.cruisemesh.app.ui

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.identity.ProfilePhotoStore
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.media.createCameraCaptureUri
import com.cruisemesh.app.relay.RelayConfigStore
import android.widget.Toast

/** Hosted privacy policy (Play Console + in-app link). */
const val PRIVACY_POLICY_URL = "https://cruisemesh.davidjacobson.work/privacy.html"

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProfileScreen(
    profileUserId: ByteArray,
    displayId: String,
    fingerprint: List<String>,
    meshStatus: String,
    onStartMesh: (() -> Unit)?,
    onShowMyQr: () -> Unit,
    onBackUp: () -> Unit,
    onProfileChanged: (Long) -> Unit = {},
    onBack: () -> Unit
) {
    val context = LocalContext.current
    var displayName by remember { mutableStateOf(ProfileStore.loadDisplayName(context)) }
    val initialDisplayName = remember { ProfileStore.loadDisplayName(context) }
    var avatarPath by remember { mutableStateOf(ProfilePhotoStore.loadAvatarPath(context)) }
    var shareOnline by remember { mutableStateOf(RelayConfigStore.shareOnline(context)) }
    fun bumpAndSync() {
        onProfileChanged(ProfileStore.bumpOwnAvatarEpoch(context))
    }
    val pickPhotoLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        if (uri != null) {
            val saved = ProfilePhotoStore.saveFromUri(context, uri)
            if (saved != null) {
                avatarPath = saved
                bumpAndSync()
            }
        }
    }
    var pendingCameraUri by remember { mutableStateOf<Uri?>(null) }
    val takePhotoLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.TakePicture(),
    ) { success ->
        val uri = pendingCameraUri
        pendingCameraUri = null
        if (success && uri != null) {
            val saved = ProfilePhotoStore.saveFromUri(context, uri)
            if (saved != null) {
                avatarPath = saved
                bumpAndSync()
            }
        }
    }
    val cameraPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) {
            val uri = createCameraCaptureUri(context)
            pendingCameraUri = uri
            takePhotoLauncher.launch(uri)
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Profile & Settings") },
                navigationIcon = {
                    IconButton(onClick = {
                        if (displayName.trim() != initialDisplayName.trim()) {
                            bumpAndSync()
                        }
                        onBack()
                    }) {
                        Icon(Icons.Default.ArrowBack, contentDescription = "Back")
                    }
                }
            )
        }
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally
        ) {
            LocalProfileEditor(
                userId = profileUserId,
                displayId = displayId,
                displayName = displayName,
                avatarPath = avatarPath,
                onDisplayNameChange = {
                    displayName = it
                    ProfileStore.saveDisplayName(context, it)
                },
                onTakePhoto = {
                    val granted = ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                        PackageManager.PERMISSION_GRANTED
                    if (granted) {
                        val uri = createCameraCaptureUri(context)
                        pendingCameraUri = uri
                        takePhotoLauncher.launch(uri)
                    } else {
                        cameraPermissionLauncher.launch(Manifest.permission.CAMERA)
                    }
                },
                onChoosePhoto = {
                    pickPhotoLauncher.launch(
                        PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                    )
                },
                onRemovePhoto = {
                    ProfilePhotoStore.clear(context)
                    avatarPath = null
                    bumpAndSync()
                },
                helperText = "Your profile photo is shared with friends.",
            )

            Spacer(modifier = Modifier.height(24.dp))

            Button(onClick = onShowMyQr, modifier = Modifier.fillMaxWidth()) {
                Text("Show my friend card")
            }

            Spacer(modifier = Modifier.height(24.dp))

            Text("Account backup", style = MaterialTheme.typography.titleMedium)
            Text(
                "Save your identity and messages to an encrypted file so you can restore them if you reinstall or switch phones. Restore is offered on first launch.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )
            Button(
                onClick = onBackUp,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 12.dp),
            ) {
                Text("Back up account")
            }

            Spacer(modifier = Modifier.height(24.dp))

            Text("Your device identity", style = MaterialTheme.typography.bodyMedium)

            Text(
                displayId,
                style = MaterialTheme.typography.titleMedium.copy(fontFamily = FontFamily.Monospace),
                modifier = Modifier.padding(top = 16.dp)
            )

            Text(
                fingerprint.joinToString(" "),
                style = MaterialTheme.typography.bodyLarge,
                modifier = Modifier.padding(top = 8.dp)
            )

            Text(
                "(Read these aloud to verify)",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp)
            )

            Spacer(modifier = Modifier.height(32.dp))

            Text("Mesh Status", style = MaterialTheme.typography.titleMedium)

            Text(meshStatus, modifier = Modifier.padding(top = 8.dp))

            if (onStartMesh != null) {
                Button(onClick = onStartMesh, modifier = Modifier.padding(top = 16.dp)) {
                    Text("Start mesh")
                }
            }

            Spacer(modifier = Modifier.height(32.dp))

            Text("Relay Presence", style = MaterialTheme.typography.titleMedium)
            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 8.dp),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                Text(
                    "Share when I'm online",
                    modifier = Modifier.weight(1f),
                )
                Switch(
                    checked = shareOnline,
                    onCheckedChange = {
                        shareOnline = it
                        RelayConfigStore.setShareOnline(context, it)
                    },
                )
            }

            Spacer(modifier = Modifier.height(32.dp))

            Text("Legal", style = MaterialTheme.typography.titleMedium)
            TextButton(
                onClick = {
                    context.startActivity(
                        Intent(Intent.ACTION_VIEW, Uri.parse(PRIVACY_POLICY_URL)),
                    )
                },
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 8.dp),
            ) {
                Text("Privacy policy")
            }
            Text(
                text = PRIVACY_POLICY_URL,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 4.dp),
            )

            if (DebugFileLog.isEnabled(context)) {
                Spacer(modifier = Modifier.height(32.dp))

                Text("Debug", style = MaterialTheme.typography.titleMedium)
                Text(
                    "On-device log capture is on. Share it to send diagnostics.",
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
                            Toast.makeText(context, "No log captured yet", Toast.LENGTH_SHORT).show()
                        }
                    },
                    modifier = Modifier
                        .fillMaxWidth()
                        .padding(top = 8.dp),
                ) {
                    Text("Share debug log")
                }
            }
        }
    }
}

@Preview(showBackground = true)
@Preview(
    showBackground = true,
    name = "Profile Dark",
    uiMode = Configuration.UI_MODE_NIGHT_YES,
)
@Composable
fun ProfileScreenPreview() {
    CruiseMeshTheme {
        ProfileScreen(
            profileUserId = byteArrayOf(1, 2, 3, 4),
            displayId = "CM-K7QX-9M2P-3F8J-QRTZ-AB",
            fingerprint = listOf("anchor", "beacon", "coral", "dock"),
            meshStatus = "Mesh off",
            onStartMesh = {},
            onShowMyQr = {},
            onBackUp = {},
            onBack = {}
        )
    }
}
