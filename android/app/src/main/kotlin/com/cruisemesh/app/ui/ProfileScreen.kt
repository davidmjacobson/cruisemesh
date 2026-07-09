package com.cruisemesh.app.ui

import android.content.res.Configuration
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
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
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.identity.ProfilePhotoStore
import com.cruisemesh.app.identity.ProfileStore

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProfileScreen(
    profileUserId: ByteArray,
    displayId: String,
    fingerprint: List<String>,
    meshStatus: String,
    onStartMesh: (() -> Unit)?,
    onShowMyQr: () -> Unit,
    onBack: () -> Unit
) {
    val context = LocalContext.current
    var displayName by remember { mutableStateOf(ProfileStore.loadDisplayName(context)) }
    var avatarPath by remember { mutableStateOf(ProfilePhotoStore.loadAvatarPath(context)) }
    val pickPhotoLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        if (uri != null) {
            avatarPath = ProfilePhotoStore.saveFromUri(context, uri) ?: avatarPath
        }
    }
    val takePhotoLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.TakePicturePreview(),
    ) { bitmap ->
        if (bitmap != null) {
            avatarPath = ProfilePhotoStore.saveFromBitmap(context, bitmap) ?: avatarPath
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Profile & Settings") },
                navigationIcon = {
                    IconButton(onClick = onBack) {
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
                onTakePhoto = { takePhotoLauncher.launch(null) },
                onChoosePhoto = {
                    pickPhotoLauncher.launch(
                        PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly),
                    )
                },
                onRemovePhoto = {
                    ProfilePhotoStore.clear(context)
                    avatarPath = null
                },
                helperText = "Your profile photo is local to this device for now.",
            )

            Spacer(modifier = Modifier.height(24.dp))

            Button(onClick = onShowMyQr, modifier = Modifier.fillMaxWidth()) {
                Text("Show my friend card")
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
            onBack = {}
        )
    }
}
