package com.cruisemesh.app.ui

import android.Manifest
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.net.Uri
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
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
import com.cruisemesh.app.friending.FriendsOfFriendsStore
import com.cruisemesh.app.identity.ProfilePhotoStore
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.media.createCameraCaptureUri
import com.cruisemesh.app.mesh.MeshStartupPreferences
import com.cruisemesh.app.relay.RelayConfigStore
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R

/** Hosted privacy policy (Play Console + in-app link). */
const val PRIVACY_POLICY_URL = "https://cruisemesh.app/privacy"

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
    onAdvanced: () -> Unit,
    onProfileChanged: (Long) -> Unit = {},
    onFriendsOfFriendsChanged: (Boolean) -> Unit = {},
    onBack: () -> Unit,
) {
    val context = LocalContext.current
    var displayName by remember { mutableStateOf(ProfileStore.loadDisplayName(context)) }
    val initialDisplayName = remember { ProfileStore.loadDisplayName(context) }
    var avatarPath by remember { mutableStateOf(ProfilePhotoStore.loadAvatarPath(context)) }
    var shareOnline by remember { mutableStateOf(RelayConfigStore.shareOnline(context)) }
    var friendsOfFriends by remember { mutableStateOf(FriendsOfFriendsStore.isEnabled(context)) }
    var startAutomatically by remember { mutableStateOf(MeshStartupPreferences.isAutoStartEnabled(context)) }

    fun bumpAndSync() = onProfileChanged(ProfileStore.bumpOwnAvatarEpoch(context))

    val pickPhotoLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        if (uri != null) {
            ProfilePhotoStore.saveFromUri(context, uri)?.let {
                avatarPath = it
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
            ProfilePhotoStore.saveFromUri(context, uri)?.let {
                avatarPath = it
                bumpAndSync()
            }
        }
    }
    val cameraPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        if (granted) {
            createCameraCaptureUri(context).let { uri ->
                pendingCameraUri = uri
                takePhotoLauncher.launch(uri)
            }
        }
    }

    fun leaveScreen() {
        if (displayName.trim() != initialDisplayName.trim()) bumpAndSync()
        onBack()
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_profile_settings)) },
                navigationIcon = {
                    IconButton(onClick = ::leaveScreen) {
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
        ) {
            ProfileSection(title = "You") {
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
                        if (ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                            PackageManager.PERMISSION_GRANTED
                        ) {
                            createCameraCaptureUri(context).let { uri ->
                                pendingCameraUri = uri
                                takePhotoLauncher.launch(uri)
                            }
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
                Text(
                    displayId,
                    style = MaterialTheme.typography.titleMedium.copy(fontFamily = FontFamily.Monospace),
                    modifier = Modifier.padding(top = 16.dp),
                )
                Text(fingerprint.joinToString(" "), modifier = Modifier.padding(top = 8.dp))
                Text(stringResource(R.string.ui_read_these_aloud_to_verify),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(top = 4.dp),
                )
            }

            SettingsSpacer()
            ProfileSection(title = "My friend card") {
                Button(onClick = onShowMyQr, modifier = Modifier.fillMaxWidth()) {
                    Text(stringResource(R.string.ui_show_my_friend_card))
                }
            }

            SettingsSpacer()
            ProfileSection(title = "Backup") {
                Text(stringResource(R.string.ui_save_your_identity_and_messages_to_an_encrypted),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Button(
                    onClick = onBackUp,
                    modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
                ) { Text(stringResource(R.string.ui_back_up_account)) }
            }

            SettingsSpacer()
            ProfileSection(title = "Mesh") {
                Text(meshStatus, modifier = Modifier.padding(top = 4.dp))
                if (onStartMesh != null) {
                    Button(onClick = onStartMesh, modifier = Modifier.padding(top = 12.dp)) {
                        Text(stringResource(R.string.ui_start_mesh))
                    }
                }
                SettingsToggle(
                    title = "Start CruiseMesh automatically",
                    detail = "Run the mesh after this phone restarts.",
                    checked = startAutomatically,
                    onCheckedChange = {
                        startAutomatically = it
                        MeshStartupPreferences.setAutoStartEnabled(context, it)
                    },
                )
                SettingsToggle(
                    title = "Share when I'm online",
                    detail = "Help friends know whether the relay can reach you.",
                    checked = shareOnline,
                    onCheckedChange = {
                        shareOnline = it
                        RelayConfigStore.setShareOnline(context, it)
                    },
                )
            }

            SettingsSpacer()
            ProfileSection(title = "Privacy") {
                SettingsToggle(
                    title = "Friends of friends",
                    detail = "Let friends introduce you to people they know. Messages and phone contacts are never shared.",
                    checked = friendsOfFriends,
                    onCheckedChange = {
                        friendsOfFriends = it
                        onFriendsOfFriendsChanged(it)
                    },
                )
            }

            SettingsSpacer()
            ProfileSection(title = "Advanced") {
                Text(stringResource(R.string.ui_relay_configuration_local_wi_fi_tools_and_diagnostics),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Button(
                    onClick = onAdvanced,
                    modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
                ) { Text(stringResource(R.string.ui_open_advanced_settings)) }
            }

            SettingsSpacer()
            ProfileSection(title = "Legal") {
                TextButton(
                    onClick = {
                        context.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(PRIVACY_POLICY_URL)))
                    },
                    modifier = Modifier.fillMaxWidth(),
                ) { Text(stringResource(R.string.ui_privacy_policy)) }
            }
        }
    }
}

@Composable
private fun SettingsToggle(
    title: String,
    detail: String,
    checked: Boolean,
    onCheckedChange: (Boolean) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(top = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(modifier = Modifier.weight(1f)) {
            Text(title)
            Text(
                detail,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 2.dp),
            )
        }
        Switch(checked = checked, onCheckedChange = onCheckedChange)
    }
}

@Composable
private fun SettingsSpacer() = Spacer(modifier = Modifier.height(16.dp))

@Composable
private fun ProfileSection(
    title: String,
    content: @Composable ColumnScope.() -> Unit,
) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(
            containerColor = MaterialTheme.colorScheme.surfaceVariant.copy(alpha = 0.46f),
        ),
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            horizontalAlignment = Alignment.Start,
        ) {
            Text(title, style = MaterialTheme.typography.titleMedium)
            content()
        }
    }
}

@Preview(showBackground = true)
@Preview(showBackground = true, name = "Profile Dark", uiMode = Configuration.UI_MODE_NIGHT_YES)
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
            onAdvanced = {},
            onBack = {},
        )
    }
}
