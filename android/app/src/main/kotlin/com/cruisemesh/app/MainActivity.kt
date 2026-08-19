package com.cruisemesh.app

import android.Manifest
import android.annotation.SuppressLint
import android.app.Activity
import android.bluetooth.BluetoothAdapter
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Bundle
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import android.util.Log
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import com.cruisemesh.app.chat.ChatSummaryLoader
import com.cruisemesh.app.chat.ChatSummaryRefreshCoordinator
import com.cruisemesh.app.chat.ChatSummaryRefreshPolicy
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.navigation.NavController
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.NavType
import androidx.navigation.navArgument
import com.cruisemesh.app.chat.ChatScreen
import com.cruisemesh.app.chat.GroupChatScreen
import com.cruisemesh.app.chat.GroupSender
import com.cruisemesh.app.chat.DraftStore
import com.cruisemesh.app.chat.RealMeshSender
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.friending.ContactsScreen
import com.cruisemesh.app.friending.FriendRequestSender
import com.cruisemesh.app.friending.AddFriendScreen
import com.cruisemesh.app.friending.ImportFriendResult
import com.cruisemesh.app.friending.FriendAddedOutcome
import com.cruisemesh.app.friending.FriendPreview
import com.cruisemesh.app.sail.SailChecklistCardStore
import com.cruisemesh.app.sail.SailChecklistEvidence
import com.cruisemesh.app.sail.SailChecklistInputs
import com.cruisemesh.app.ui.ConnectivityWarning
import com.cruisemesh.app.ui.ConnectivityWarningSeverity
import com.cruisemesh.app.ui.SailChecklistProgress
import com.cruisemesh.app.ui.SailChecklistScreen
import android.widget.Toast
import androidx.core.app.ActivityCompat
import androidx.core.app.NotificationManagerCompat
import com.cruisemesh.app.friending.MyQrScreen
import com.cruisemesh.app.friending.ProfileSyncSender
import com.cruisemesh.app.friending.FriendDirectorySender
import com.cruisemesh.app.friending.FriendsOfFriendsStore
import com.cruisemesh.app.friending.ScanScreen
import com.cruisemesh.app.friending.PendingSharedRequestRow
import com.cruisemesh.app.friending.ShareContactAvailability
import com.cruisemesh.app.friending.ShareContactPolicy
import com.cruisemesh.app.friending.ShareContactScreen
import com.cruisemesh.app.friending.SharedCardImport
import com.cruisemesh.app.friending.WaitingToConnectScreen
import com.cruisemesh.app.identity.IdentityStore
import com.cruisemesh.app.identity.OnboardingStore
import com.cruisemesh.app.identity.TermsAcceptanceStore
import com.cruisemesh.app.identity.backup.BackupExportScreen
import com.cruisemesh.app.identity.backup.BackupRestoreScreen
import com.cruisemesh.app.identity.ProfilePhotoStore
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.media.createCameraCaptureUri
import com.cruisemesh.app.mesh.ChatViewEvents
import com.cruisemesh.app.mesh.ContactReachability
import com.cruisemesh.app.mesh.LanTransportDiagnostics
import com.cruisemesh.app.mesh.MeshConnectivityStatus
import com.cruisemesh.app.mesh.MeshRuntimeState
import com.cruisemesh.app.mesh.MeshRuntimeStatus
import com.cruisemesh.app.mesh.MeshService
import com.cruisemesh.app.mesh.MeshStartupPreferences
import com.cruisemesh.app.mesh.shouldStartMeshOnAppOpen
import com.cruisemesh.app.mesh.ReachabilityLevel
import com.cruisemesh.app.mesh.RelayHealth
import com.cruisemesh.app.mesh.parseLanEndpointLink
import com.cruisemesh.app.mesh.parseLanManualEndpoint
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.notify.MessageNotifier
import com.cruisemesh.app.notify.ChatMuteStore
import com.cruisemesh.app.relay.RelayImport
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.ui.CheckingClock
import com.cruisemesh.app.ui.ConnectionInputs
import com.cruisemesh.app.ui.connectionCheckPending
import com.cruisemesh.app.ui.ChatListLogic
import com.cruisemesh.app.ui.ChatListScreen
import com.cruisemesh.app.ui.ChatSummary
import com.cruisemesh.app.ui.AppearancePreference
import com.cruisemesh.app.ui.AppearancePreferences
import com.cruisemesh.app.ui.CruiseMeshTheme
import com.cruisemesh.app.ui.LocalReachabilityPalette
import com.cruisemesh.app.ui.InternetDeliveryService
import com.cruisemesh.app.ui.MeshStatusDotColor
import com.cruisemesh.app.ui.MeshStatusLegendDialog
import com.cruisemesh.app.ui.MeshStatusTextLogic
import com.cruisemesh.app.ui.NewGroupScreen
import com.cruisemesh.app.ui.OnboardingScreen
import com.cruisemesh.app.ui.ProfileScreen
import com.cruisemesh.app.ui.TermsAcceptanceScreen
import com.cruisemesh.app.ui.ConnectionDetailsScreen
import com.cruisemesh.app.ui.ShorePassScreen
import com.cruisemesh.app.ui.HelpSupportScreen
import com.cruisemesh.app.devicelink.AddDeviceScreen
import com.cruisemesh.app.devicelink.DeviceRemovalStatus
import com.cruisemesh.app.devicelink.DeviceRemovedScreen
import com.cruisemesh.app.devicelink.YourDevicesScreen
import com.cruisemesh.app.ui.DeveloperSettingsScreen
import com.cruisemesh.app.ui.SettingsScreen
import uniffi.cruisemesh_core.CoreLinkRole
import uniffi.cruisemesh_core.CoreSailChecklistReport
import uniffi.cruisemesh_core.CoreSailPermission
import uniffi.cruisemesh_core.DeepLinkRoute
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.coreSailChecklist
import uniffi.cruisemesh_core.deepLinkRoute
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.friendCardMatch
import uniffi.cruisemesh_core.formatUserId
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.ContactDelivery
import uniffi.cruisemesh_core.contactDelivery
import uniffi.cruisemesh_core.composerReach
import uniffi.cruisemesh_core.ContactProvenance
import uniffi.cruisemesh_core.friendCardUserId
import uniffi.cruisemesh_core.FriendImport
import uniffi.cruisemesh_core.OutgoingSharedRequest
import com.cruisemesh.app.friending.friendImportFailureResId
import uniffi.cruisemesh_core.parseFriendImport
import uniffi.cruisemesh_core.parseFriendText
import uniffi.cruisemesh_core.parseRelaySetupText
import uniffi.cruisemesh_core.relaySetupIsOfficial
import uniffi.cruisemesh_core.lanDefaultTcpPort
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R

private const val RECEIPT_TYPE_DELIVERED: kotlin.UByte = 1u
private const val RECEIPT_TYPE_READ: kotlin.UByte = 2u
private const val UI_PREFS_NAME = "cruisemesh_ui"
private const val PREF_HIDE_BLUETOOTH_AUDIO_WARNING = "hide_bluetooth_audio_warning"

data class PendingDeepLink(
    val idHex: String = "",
    val isGroup: Boolean = false,
    val friendToken: String? = null,
    val lanEndpoint: String? = null,
    val relayCard: String? = null,
)

class MainActivity : ComponentActivity() {

    private val pendingDeepLink = mutableStateOf<PendingDeepLink?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // T21: a killed process can leave its ongoing mesh notification
        // posted. Drop it before the auto-start below decides what to do, so
        // the shade never claims the mesh is running while it isn't.
        MeshService.clearStaleNotification(this)
        enableEdgeToEdge()
        // Debug builds: start capturing this process's log to a file so it can
        // be shared without adb (no-op in release). Idempotent with MeshService.
        DebugFileLog.start(this)
        // Which Shore Pass this device is on, recorded right after capture
        // starts so it is the first thing in every shared log.
        RelayConfigStore.logSummary(this)
        pendingDeepLink.value = deepLinkFromIntent(intent)
        setContent {
            var appearance by remember {
                mutableStateOf(AppearancePreferences.load(this@MainActivity))
            }
            CruiseMeshTheme(appearance = appearance) {
                Surface(modifier = Modifier.fillMaxSize()) {
                    CruiseMeshApp(
                        appearancePreference = appearance,
                        onAppearancePreferenceChange = { preference ->
                            AppearancePreferences.save(this@MainActivity, preference)
                            appearance = preference
                        },
                        pendingDeepLink = pendingDeepLink.value,
                        onPendingDeepLinkConsumed = { pendingDeepLink.value = null },
                    )
                }
            }
        }
        handleBluetoothEnableRequest(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        pendingDeepLink.value = deepLinkFromIntent(intent)
        handleBluetoothEnableRequest(intent)
    }

    // ACTION_REQUEST_ENABLE needs BLUETOOTH_CONNECT on API 31+; the
    // SecurityException that a missing grant throws is already handled by the
    // runCatching -> Settings fallback below, but lint's MissingPermission
    // check can't see through runCatching, so suppress it here (the runtime
    // handling is the fallback, not a pre-check).
    @SuppressLint("MissingPermission")
    private fun handleBluetoothEnableRequest(intent: Intent?) {
        if (intent?.action != ACTION_REQUEST_BLUETOOTH_ENABLE) return
        // Consume the trampoline action before opening the system prompt so a
        // later activity recreation cannot show it a second time.
        intent.action = null
        runCatching {
            startActivity(Intent(BluetoothAdapter.ACTION_REQUEST_ENABLE))
        }.onFailure {
            startActivity(Intent(Settings.ACTION_BLUETOOTH_SETTINGS))
        }
    }

    /**
     * Derive the pending deep link from [intent] and *consume* it.
     *
     * The consuming half matters as much as the deriving half. `onNewIntent`
     * calls `setIntent`, so the link outlives the moment it was handled: every
     * later Activity recreation -- a rotation, a theme change, the process
     * coming back -- re-reads the same intent, re-derives the same link, and
     * navigates to Add Friend again. From the user's side an already-handled
     * friend card silently reopens, which reads as the app refusing to let go
     * of a screen they already finished with.
     *
     * [onPendingDeepLinkConsumed] does not cover this: it clears the
     * in-memory state for one Activity instance, and a recreation builds a new
     * one from the intent that was never cleared.
     *
     * This mirrors [handleBluetoothEnableRequest] directly below, which nulls
     * its trampoline action for exactly the same reason.
     */
    private fun deepLinkFromIntent(intent: Intent?): PendingDeepLink? {
        return consumePendingDeepLink(intent)
    }

    companion object {
        const val ACTION_REQUEST_BLUETOOTH_ENABLE =
            "com.cruisemesh.app.action.REQUEST_BLUETOOTH_ENABLE"
    }
}

@Composable
fun CruiseMeshApp(
    appearancePreference: AppearancePreference = AppearancePreference.SYSTEM,
    onAppearancePreferenceChange: (AppearancePreference) -> Unit = {},
    pendingDeepLink: PendingDeepLink? = null,
    onPendingDeepLinkConsumed: () -> Unit = {},
) {
    val context = LocalContext.current
    val identity = remember {
        IdentityStore.load(context) ?: generateIdentity().also { IdentityStore.save(context, it) }
    }
    val navController = rememberNavController()
    var termsAccepted by remember {
        mutableStateOf(TermsAcceptanceStore.isCurrentVersionAccepted(context))
    }
    var onboardingCompleted by remember { mutableStateOf(OnboardingStore.isCompleted(context)) }

    if (!termsAccepted) {
        TermsAcceptanceScreen {
            TermsAcceptanceStore.acceptCurrentVersion(context)
            termsAccepted = true
        }
        return
    }

    // specs/multi-device-v1.md §10 step 5. Read once at launch, because a device
    // ejected in a previous process must still know, and watched from there,
    // because the notice can land while somebody is looking at their chats. The
    // core stage is the fact; this is the only screen that may be drawn on top
    // of it, for the reason [DeviceRemovedScreen] states.
    val deviceRemoved by DeviceRemovalStatus.removed.collectAsState()
    LaunchedEffect(Unit) {
        withContext(Dispatchers.IO) { DeviceRemovalStatus.refresh(AppStore.get(context)) }
    }
    if (deviceRemoved) {
        // Core already refuses to advertise, author or ack from this stage and
        // the service stops itself when the notice lands -- but a service
        // started before that, or revived by the system since, has to be told
        // to stay down rather than left running behind a screen that says the
        // opposite.
        LaunchedEffect(Unit) { stopMesh(context) }
        DeviceRemovedScreen()
        return
    }

    NavHost(
        navController = navController,
        startDestination = if (onboardingCompleted) "home" else "onboarding",
    ) {
        composable("onboarding") {
            OnboardingRoute(
                identity = identity,
                onRestore = { navController.navigate("restore") },
                // specs/multi-device-v1.md §9: a person who bought a second
                // phone and never made a backup needs a door too, and this is
                // it. No person id to expect -- they have not opened a backup,
                // so the only thing this phone knows is that the other one is
                // theirs, and the ceremony's own confirm is what checks that.
                onSetUpAsAnotherDevice = { navController.navigate("addDevice?role=new") },
            ) {
                onboardingCompleted = true
                navController.navigate("home") {
                    popUpTo("onboarding") { inclusive = true }
                }
            }
        }
        composable("home") { HomeRoute(identity, navController) }
        composable("profile") { ProfileRoute(identity, navController) }
        composable("settings") {
            SettingsRoute(
                identity = identity,
                navController = navController,
                appearancePreference = appearancePreference,
                onAppearancePreferenceChange = onAppearancePreferenceChange,
            )
        }
        composable("connectionDetails") {
            ConnectionDetailsScreen(
                ownUserId = identity.userId,
                onBack = { navController.popOrExit(context) },
                onStartMesh = { startMesh(context) },
                onManageShorePass = { navController.navigate("shorePass") },
                // The system Bluetooth settings screen rather than
                // ACTION_REQUEST_ENABLE: the enable prompt needs
                // BLUETOOTH_CONNECT, and a health card that offers an action
                // must not be able to offer one that silently fails.
                onTurnOnBluetooth = {
                    context.startActivity(Intent(Settings.ACTION_BLUETOOTH_SETTINGS))
                },
            )
        }
        composable("sailChecklist") { SailChecklistRoute(navController) }
        composable("developerSettings") {
            DeveloperSettingsScreen(onBack = { navController.popOrExit(context) })
        }
        // specs/multi-device-v1.md §13 WP6. "Your devices" is a family surface
        // in Settings, not an Internal Tools entry: the person who needs it most
        // is the one whose phone was just stolen.
        composable("yourDevices") {
            YourDevicesScreen(
                identity = identity,
                onBack = { navController.popOrExit(context) },
                onAddDevice = { navController.navigate("addDevice?role=approving") },
            )
        }
        composable(
            "addDevice?role={role}&person={person}",
            arguments = listOf(
                navArgument("role") { type = NavType.StringType; defaultValue = "approving" },
                navArgument("person") {
                    type = NavType.StringType
                    nullable = true
                    defaultValue = null
                },
            ),
        ) { entry ->
            // Which end of §9 this phone is comes from the door it came through
            // -- Settings means "already set up", the restore fork means "new"
            // -- so it is never a question put to the person mid-ceremony.
            val newDevice = entry.arguments?.getString("role") == "new"
            AddDeviceScreen(
                identity = identity,
                role = if (newDevice) CoreLinkRole.NEW_DEVICE else CoreLinkRole.APPROVING_DEVICE,
                expectedPersonId = entry.arguments?.getString("person")
                    ?.let { runCatching { UserIdHex.decode(it) }.getOrNull() },
                onBack = { navController.popOrExit(context) },
                // A phone that was just adopted holds this person's contacts,
                // groups and history, so first-run setup has nothing left to
                // ask it. Popping back instead landed it on the wizard it came
                // through -- still on step 1, still offering "This is another
                // of my devices", and still asking a linked person their own
                // name (two-phone session, 2026-08-18).
                //
                // [LinkAdoption] has already made the same fact durable, so
                // this is the in-memory half plus the navigation. Cleared back
                // to the graph's start rather than to "onboarding" by name:
                // the route is also reachable as onboarding -> restore ->
                // addDevice, and only one of those spellings survives both.
                onLinked = {
                    onboardingCompleted = true
                    navController.navigate("home") {
                        popUpTo(navController.graph.findStartDestination().id) {
                            inclusive = true
                        }
                    }
                },
            )
        }
        composable("help") {
            HelpSupportScreen(
                onShorePass = { navController.navigate("shorePass") },
                onConnectionDetails = { navController.navigate("connectionDetails") },
                onBack = { navController.popOrExit(context) },
            )
        }
        composable(
            "shorePass?card={card}",
            arguments = listOf(
                navArgument("card") {
                    type = NavType.StringType
                    nullable = true
                    defaultValue = null
                },
            ),
        ) { entry ->
            ShorePassScreen(
                initialCard = entry.arguments?.getString("card"),
                onBack = { navController.popOrExit(context) },
            )
        }
        composable("backup") { BackupExportScreen(onBack = { navController.popOrExit(context) }) }
        composable("restore") {
            BackupRestoreScreen(
                onBack = { navController.popOrExit(context) },
                // §9's second meaning of "restore": this phone joins the person
                // in the backup instead of becoming them.
                onSetUpAsNewDevice = { personId ->
                    navController.navigate(
                        "addDevice?role=new&person=${UserIdHex.encode(personId)}",
                    )
                },
            )
        }
        composable("myQr") {
            MyQrScreen(
                identity,
                onSayHi = { openFriendChat(navController, it) },
                onBack = { navController.popOrExit(context) },
            )
        }
        composable(
            "addFriend?token={token}",
            arguments = listOf(navArgument("token") { type = NavType.StringType; nullable = true; defaultValue = null }),
        ) { entry -> AddFriendRoute(identity, navController, entry.arguments?.getString("token")) }
        composable("scan") { ScanRoute(identity, navController) }
        composable("contacts") { ContactsRoute(identity, navController) }
        composable("waitingToConnect") { WaitingToConnectRoute(identity, navController) }
        composable("shareContact/{userIdHex}") { entry ->
            ShareContactRoute(
                identity,
                entry.arguments?.getString("userIdHex").orEmpty(),
                navController,
            )
        }
        composable("newGroup") { NewGroupRoute(identity, navController) }
        composable("chat/{userIdHex}") { backStackEntry ->
            val userIdHex = backStackEntry.arguments?.getString("userIdHex").orEmpty()
            ChatRoute(identity, userIdHex, navController)
        }
        composable("group/{groupIdHex}") { backStackEntry ->
            val groupIdHex = backStackEntry.arguments?.getString("groupIdHex").orEmpty()
            GroupChatRoute(identity, groupIdHex, navController)
        }
    }

    LaunchedEffect(pendingDeepLink, onboardingCompleted) {
        val link = pendingDeepLink ?: return@LaunchedEffect
        if (!onboardingCompleted) return@LaunchedEffect
        link.relayCard?.let { relayCard ->
            navController.navigate("shorePass?card=${Uri.encode(relayCard)}") {
                launchSingleTop = true
            }
            onPendingDeepLinkConsumed()
            return@LaunchedEffect
        }
        link.lanEndpoint?.let { endpointText ->
            val endpoint = parseLanManualEndpoint(endpointText, lanDefaultTcpPort().toInt())
            if (endpoint != null) {
                LanTransportDiagnostics.queueManualConnection(endpoint)
                if (MeshRuntimeStatus.state.value == MeshRuntimeState.STOPPED) {
                    startMesh(context)
                }
                navController.navigate("profile") { launchSingleTop = true }
            }
            onPendingDeepLinkConsumed()
            return@LaunchedEffect
        }
        val route = link.friendToken?.let { "addFriend?token=${Uri.encode(it)}" }
            ?: if (link.isGroup) "group/${link.idHex}" else "chat/${link.idHex}"
        navController.navigate(route) {
            launchSingleTop = true
        }
        onPendingDeepLinkConsumed()
    }
}

@Composable
private fun OnboardingRoute(
    identity: Identity,
    onRestore: () -> Unit,
    onSetUpAsAnotherDevice: () -> Unit,
    onComplete: () -> Unit,
) {
    val context = LocalContext.current
    val displayId = remember(identity) { formatUserId(identity.userId) }
    // Stored name, not the fallback: onboarding must open with an empty field
    // so the user supplies a real one (see ProfileStore.loadStoredDisplayName).
    var displayName by remember { mutableStateOf(ProfileStore.loadStoredDisplayName(context)) }
    var avatarPath by remember { mutableStateOf(ProfilePhotoStore.loadAvatarPath(context)) }
    var permissionRefreshToken by remember { mutableStateOf(0) }
    val meshPermissionsGranted = remember(context, permissionRefreshToken) {
        hasMeshPermissions(context)
    }
    val notificationPermissionGranted = remember(context, permissionRefreshToken) {
        hasNotificationPermission(context)
    }
    val batteryExemptionGranted = remember(context, permissionRefreshToken) {
        isIgnoringBatteryOptimizations(context)
    }
    val meshPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { grants ->
        permissionRefreshToken += 1
        if (!grants.values.all { it }) {
            val activity = context as? ComponentActivity
            val permanentlyDenied = activity != null && MeshService.requiredPermissions().any { perm ->
                grants[perm] == false &&
                    !ActivityCompat.shouldShowRequestPermissionRationale(activity, perm)
            }
            if (permanentlyDenied) {
                Toast.makeText(
                    context,
                    context.getString(R.string.ui_enable_nearby_in_app_permissions),
                    Toast.LENGTH_LONG,
                ).show()
                openAppPermissionSettings(context)
            } else {
                Toast.makeText(
                    context,
                    context.getString(R.string.ui_nearby_required_for_messages),
                    Toast.LENGTH_LONG,
                ).show()
            }
        }
    }
    val notificationPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        permissionRefreshToken += 1
        if (!granted) {
            Toast.makeText(
                context,
                context.getString(R.string.ui_notifications_denied_mesh_continues),
                Toast.LENGTH_LONG,
            ).show()
        }
    }
    val batteryOptimizationLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) {
        permissionRefreshToken += 1
    }
    val pickPhotoLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.PickVisualMedia(),
    ) { uri ->
        if (uri != null) {
            val saved = ProfilePhotoStore.saveFromUri(context, uri)
            if (saved != null) {
                avatarPath = saved
                ProfileStore.bumpOwnAvatarEpoch(context)
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
                ProfileStore.bumpOwnAvatarEpoch(context)
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

    OnboardingScreen(
        userId = identity.userId,
        displayId = displayId,
        displayName = displayName,
        avatarPath = avatarPath,
        meshPermissionsGranted = meshPermissionsGranted,
        notificationPermissionGranted = notificationPermissionGranted,
        batteryExemptionGranted = batteryExemptionGranted,
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
            ProfileStore.bumpOwnAvatarEpoch(context)
        },
        onRequestMeshPermissions = {
            if (!meshPermissionsGranted) {
                meshPermissionLauncher.launch(MeshService.requiredPermissions())
            }
        },
        onRequestNotificationPermission = {
            if (!notificationPermissionGranted && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                notificationPermissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
            }
        },
        onRequestBatteryExemption = {
            if (!batteryExemptionGranted) {
                batteryOptimizationLauncher.launch(batteryOptimizationIntent(context))
            }
        },
        onRestore = onRestore,
        onSetUpAsAnotherDevice = onSetUpAsAnotherDevice,
        onComplete = {
            // No silent substitution: OnboardingScreen keeps the final button
            // disabled until a name is entered, so reaching here means the user
            // chose one.
            if (ProfileStore.loadOwnAvatarEpoch(context) == 0L) {
                ProfileStore.bumpOwnAvatarEpoch(context)
            }
            OnboardingStore.markCompleted(context)
            onComplete()
        },
    )
}

/**
 * How often the status pill re-evaluates its clock-dependent state.
 *
 * Ten seconds, matching `CLOCK_TICK_MS` on the Connection details page, and
 * deliberately faster than [CONNECTIVITY_TICK_MS]. The pill and that page now
 * consume the same core verdict, and the spec's acceptance criterion is that
 * the two can never contradict each other -- but a shared classification only
 * buys that if both shells ask it at comparable times. The bounded `Checking`
 * window is ten seconds: on the slower tick the page would resolve to a fault
 * while the pill still showed a neutral "still checking" dot beside it for up
 * to twenty seconds more.
 */
private const val PILL_TICK_MS = 10_000L

/** The tick everything else on the home screen ages on. */
private const val CONNECTIVITY_TICK_MS = 30_000L

/**
 * Reachability levels decay purely with time
 * (a contact drifts ONLINE_RELAY -> RECENT -> OFFLINE with no event firing),
 * so the UI needs a clock tick to re-evaluate on, not just flow updates. Ticks
 * every [tickMs], and only while the activity is RESUMED -- no background work.
 */
@Composable
private fun rememberConnectivityNowMs(tickMs: Long = CONNECTIVITY_TICK_MS): Long {
    val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    var isResumed by remember { mutableStateOf(false) }
    DisposableEffect(lifecycleOwner) {
        val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
            when (event) {
                androidx.lifecycle.Lifecycle.Event.ON_RESUME -> isResumed = true
                androidx.lifecycle.Lifecycle.Event.ON_PAUSE -> isResumed = false
                else -> {}
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }
    var nowMs by remember { mutableStateOf(System.currentTimeMillis()) }
    LaunchedEffect(isResumed, tickMs) {
        if (isResumed) {
            nowMs = System.currentTimeMillis()
            while (true) {
                kotlinx.coroutines.delay(tickMs)
                nowMs = System.currentTimeMillis()
            }
        }
    }
    return nowMs
}

/** Reachability for one userId from a snapshot of [MeshConnectivityStatus]. */
private fun reachabilityLevelForUserId(
    userId: ByteArray,
    nearbyPeerIds: Set<String>,
    relayHealth: RelayHealth,
    contactLastSeen: Map<String, Long>,
    presenceLastSeen: Map<String, Long>,
    nowMs: Long,
    pushHealthy: Boolean,
): ReachabilityLevel {
    val hex = UserIdHex.encode(userId)
    val presenceSeen = presenceLastSeen[hex]
    val peerSeen = listOfNotNull(contactLastSeen[hex], presenceSeen).maxOrNull()
    return ContactReachability.compute(
        directLink = hex in nearbyPeerIds,
        presenceLastSeenMs = presenceSeen,
        selfRelayHealthy = ContactReachability.selfRelayHealthy(relayHealth, nowMs, pushHealthy),
        peerLastSeenMs = peerSeen,
        nearbyPeerCount = nearbyPeerIds.size,
        nowMs = nowMs,
    )
}

/** §2.4: a group's badge shows the best level among its members, excluding self. */
private fun computeSummaryReachability(
    summary: ChatSummary,
    ownUserId: ByteArray,
    nearbyPeerIds: Set<String>,
    relayHealth: RelayHealth,
    contactLastSeen: Map<String, Long>,
    presenceLastSeen: Map<String, Long>,
    nowMs: Long,
    pushHealthy: Boolean,
): ReachabilityLevel {
    fun levelFor(userId: ByteArray) =
        reachabilityLevelForUserId(userId, nearbyPeerIds, relayHealth, contactLastSeen, presenceLastSeen, nowMs, pushHealthy)

    if (!summary.isGroup) {
        val contact = summary.contact ?: return ReachabilityLevel.OFFLINE
        return levelFor(contact.userId)
    }
    val group = summary.group ?: return ReachabilityLevel.OFFLINE
    return group.memberUserIds
        .filterNot { it.contentEquals(ownUserId) }
        .map { levelFor(it) }
        // Enum declaration order is best-to-worst (see ReachabilityLevel KDoc).
        .minByOrNull { it.ordinal }
        ?: ReachabilityLevel.OFFLINE
}

/** §2.4: "{n} of {m} reachable" -- n = members at NEARBY or ONLINE_RELAY, m = member count excluding self. */
private fun groupReachableCounts(
    group: Group,
    ownUserId: ByteArray,
    nearbyPeerIds: Set<String>,
    relayHealth: RelayHealth,
    contactLastSeen: Map<String, Long>,
    presenceLastSeen: Map<String, Long>,
    nowMs: Long,
    pushHealthy: Boolean,
): Pair<Int, Int> {
    val others = group.memberUserIds.filterNot { it.contentEquals(ownUserId) }
    val reachable = others.count { userId ->
        val level = reachabilityLevelForUserId(userId, nearbyPeerIds, relayHealth, contactLastSeen, presenceLastSeen, nowMs, pushHealthy)
        level == ReachabilityLevel.NEARBY || level == ReachabilityLevel.ONLINE_RELAY
    }
    return reachable to others.size
}

private fun freshRelayHealthForDisplay(relayHealth: RelayHealth, nowMs: Long, pushHealthy: Boolean): RelayHealth =
    if (relayHealth is RelayHealth.Ok && !ContactReachability.selfRelayHealthy(relayHealth, nowMs, pushHealthy)) {
        RelayHealth.Failing(relayHealth.lastSyncMs)
    } else {
        relayHealth
    }

/** Derive a pending route and remove the one-shot data from its source Intent. */
internal fun consumePendingDeepLink(intent: Intent?): PendingDeepLink? {
    val link = derivePendingDeepLink(intent) ?: return null
    intent?.removeExtra(MessageNotifier.EXTRA_CHAT_USER_ID_HEX)
    intent?.removeExtra(MessageNotifier.EXTRA_CHAT_IS_GROUP)
    intent?.data = null
    return link
}

internal fun derivePendingDeepLink(intent: Intent?): PendingDeepLink? {
    val hex = intent?.getStringExtra(MessageNotifier.EXTRA_CHAT_USER_ID_HEX)
    if (hex != null) {
        return PendingDeepLink(hex, intent.getBooleanExtra(MessageNotifier.EXTRA_CHAT_IS_GROUP, false))
    }
    val uri = intent?.data ?: return null
    // T20: the core owns the routing table so both shells agree, and so the
    // https link and the cruisemesh:// scheme resolve identically.
    val route = deepLinkRoute(uri.scheme ?: "", uri.host ?: "", uri.path ?: "") ?: return null
    val fragment = uri.fragment ?: return null
    return when (route) {
        DeepLinkRoute.FRIEND ->
            // Always hand a non-empty fragment to the friends screen. A
            // future CMFRIEND4+/CMLINK scheme must fail soft there with
            // "update the app", not vanish. An empty `#` is not a card.
            fragment.takeIf { it.isNotBlank() }?.let { PendingDeepLink(friendToken = it) }
        DeepLinkRoute.RELAY_SETUP ->
            fragment.takeIf { runCatching { parseRelaySetupText(it) }.isSuccess }
                ?.let { PendingDeepLink(relayCard = it) }
        DeepLinkRoute.LAN ->
            parseLanEndpointLink(fragment)?.let { PendingDeepLink(lanEndpoint = it.display) }
        DeepLinkRoute.DEVICE_LINK ->
            // A device-link offer is scanned inside the linking ceremony, by
            // the device that is already part of this person. There is no
            // screen to open cold yet, and opening the wrong one would drop
            // someone into a flow that cannot finish what the link starts.
            null
    }
}

/**
 * The [Activity] behind a composable's context, unwrapping the theme and
 * configuration wrappers Compose layers on top of it.
 */
internal tailrec fun Context.findActivity(): Activity? = when (this) {
    is Activity -> this
    is ContextWrapper -> baseContext.findActivity()
    else -> null
}

/**
 * Go back one screen, or leave the app when this is the last screen.
 *
 * Always prefer this to a bare `popBackStack()` for a user-facing back
 * action. `popBackStack()` will pop the *only* entry on the stack and report
 * success: the `NavHost` is then left with no current destination, so it
 * renders nothing at all. The Activity is still alive and responsive, but the
 * user is looking at a blank screen with no way out except force-quitting the
 * app -- which is what a family member hit going back from Settings.
 *
 * Checking `popBackStack()`'s return value does not catch this. It returns
 * true precisely because the pop succeeded; the emptiness is the *result* of
 * the successful pop, not a failure. So the guard has to be asked before the
 * fact: is there anything underneath to go back *to*? If there isn't, "back"
 * means "leave", which is what the system back button would have done.
 */
internal fun NavController.popOrExit(context: Context) {
    if (previousBackStackEntry != null) {
        popBackStack()
        return
    }
    context.findActivity()?.finish()
}

private fun configuredInternetDeliveryService(context: Context): InternetDeliveryService? =
    RelayConfigStore.load(context)?.let { config ->
        if (relaySetupIsOfficial(config.relayUrl)) {
            InternetDeliveryService.SHORE_PASS
        } else {
            InternetDeliveryService.CUSTOM_RELAY
        }
    }

/** Resolves a [MeshStatusDotColor] to an actual [androidx.compose.ui.graphics.Color] via the current theme palette. */
@Composable
private fun MeshStatusDotColor?.toComposeColor(): androidx.compose.ui.graphics.Color? {
    val palette = LocalReachabilityPalette.current
    return when (this) {
        MeshStatusDotColor.GREEN -> palette.nearby
        MeshStatusDotColor.BLUE -> palette.onlineRelay
        MeshStatusDotColor.AMBER -> palette.recent
        MeshStatusDotColor.NEUTRAL, null -> null
    }
}

@Composable
private fun HomeRoute(identity: Identity, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    LaunchedEffect(Unit) {
        ProfileSyncSender.queueToAllContacts(
            context,
            store,
            identity,
            ProfileStore.loadOwnAvatarEpoch(context),
        )
        FriendDirectorySender.queueToAllContacts(context, store, identity)
    }
    val runtimeStatus by MeshRuntimeStatus.state.collectAsState()
    val bluetoothAudioConnected by MeshRuntimeStatus.bluetoothAudioConnected.collectAsState()
    val nearbyPeerIds by MeshConnectivityStatus.nearbyPeerIds.collectAsState()
    val relayHealth by MeshConnectivityStatus.relay.collectAsState()
    val pushHealthy by MeshConnectivityStatus.pushHealthy.collectAsState()
    val contactLastSeen by MeshConnectivityStatus.contactLastSeen.collectAsState()
    val presenceLastSeen by MeshConnectivityStatus.presenceLastSeen.collectAsState()
    val connectivityNowMs = rememberConnectivityNowMs()
    // The pill's own clock. Its verdict is the core's, shared with the
    // Connection details page, and a page open beside the pill must never see
    // the two disagree; see [PILL_TICK_MS].
    val pillNowMs = rememberConnectivityNowMs(PILL_TICK_MS)
    var transientMeshStatus by remember { mutableStateOf<String?>(null) }
    var ownDisplayName by remember { mutableStateOf(ProfileStore.loadDisplayName(context)) }
    var ownAvatarPath by remember { mutableStateOf(ProfilePhotoStore.loadAvatarPath(context)) }
    var internetDeliveryService by remember {
        mutableStateOf(configuredInternetDeliveryService(context))
    }
    var showMeshStatusLegend by remember { mutableStateOf(false) }
    val uiPrefs = remember(context) { context.getSharedPreferences(UI_PREFS_NAME, Context.MODE_PRIVATE) }
    var bluetoothAudioWarningDismissed by remember { mutableStateOf(false) }
    var hideBluetoothAudioWarning by remember {
        mutableStateOf(uiPrefs.getBoolean(PREF_HIDE_BLUETOOTH_AUDIO_WARNING, false))
    }

    var permissionRefreshToken by remember { mutableStateOf(0) }
    val hasPermissions = remember(context, permissionRefreshToken) {
        hasMeshPermissions(context)
    }
    val notificationPermissionGranted = remember(context, permissionRefreshToken) {
        hasNotificationPermission(context)
    }
    var bluetoothEnabled by remember { mutableStateOf(isBluetoothRadioEnabled(context)) }

    // Re-check after App info / system dialogs so the blocking banner clears
    // as soon as the user grants access without restarting the app.
    val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    DisposableEffect(lifecycleOwner) {
        val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
            if (event == androidx.lifecycle.Lifecycle.Event.ON_RESUME) {
                permissionRefreshToken += 1
                bluetoothEnabled = isBluetoothRadioEnabled(context)
            }
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    DisposableEffect(context) {
        val receiver = object : android.content.BroadcastReceiver() {
            override fun onReceive(receiverContext: Context, intent: Intent) {
                bluetoothEnabled = isBluetoothRadioEnabled(context)
            }
        }
        context.registerReceiver(
            receiver,
            android.content.IntentFilter(android.bluetooth.BluetoothAdapter.ACTION_STATE_CHANGED),
        )
        onDispose { context.unregisterReceiver(receiver) }
    }

    val batteryOptimizationLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) {
        startMesh(context)
    }
    val bluetoothEnableLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) {
        bluetoothEnabled = isBluetoothRadioEnabled(context)
    }

    val activity = context as? ComponentActivity
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { grants ->
        permissionRefreshToken += 1
        if (grants.values.all { it }) {
            if (isIgnoringBatteryOptimizations(context)) {
                startMesh(context)
            } else {
                transientMeshStatus = context.getString(R.string.ui_requesting_background_permission)
                batteryOptimizationLauncher.launch(batteryOptimizationIntent(context))
            }
        } else {
            transientMeshStatus = context.getString(R.string.ui_permissions_denied_mesh_cannot_run)
            // If the system will not show the dialog again, send the user to
            // App info so they can flip Nearby devices access manually.
            val permanentlyDenied = activity != null && MeshService.requiredPermissions().any { perm ->
                grants[perm] == false &&
                    !ActivityCompat.shouldShowRequestPermissionRationale(activity, perm)
            }
            if (permanentlyDenied) {
                Toast.makeText(
                    context,
                    context.getString(R.string.ui_enable_nearby_in_app_permissions_short),
                    Toast.LENGTH_LONG,
                ).show()
                openAppPermissionSettings(context)
            }
        }
    }
    val notificationPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted ->
        permissionRefreshToken += 1
        if (!granted) {
            transientMeshStatus = context.getString(R.string.ui_notifications_off_mesh_running)
        }
    }

    LaunchedEffect(hasPermissions, runtimeStatus) {
        if (shouldStartMeshOnAppOpen(
                meshEnabled = MeshStartupPreferences.isMeshEnabled(context),
                permissionsGranted = hasPermissions,
                runtimeStopped = runtimeStatus == MeshRuntimeState.STOPPED,
            )
        ) {
            startMesh(context)
        }
    }

    LaunchedEffect(runtimeStatus) {
        if (runtimeStatus != MeshRuntimeState.STOPPED) {
            transientMeshStatus = null
        }
    }
    LaunchedEffect(bluetoothAudioConnected) {
        if (!bluetoothAudioConnected) {
            bluetoothAudioWarningDismissed = false
        }
    }

    var summaries by remember { mutableStateOf(emptyList<ChatSummary>()) }
    var ownCloneWarning by remember { mutableStateOf(false) }
    // The before-you-sail card. Null unless there is genuinely something left
    // to do: the card is the nudge, the Settings row is the permanent way in.
    val sailCardDismissed by SailChecklistCardStore.dismissed.collectAsState()
    var sailChecklistProgress by remember { mutableStateOf<SailChecklistProgress?>(null) }
    // The evidence latches recompute the card the moment the airplane-mode
    // test succeeds or a backup lands, not on the next resume.
    val sailEvidenceVersion by SailChecklistEvidence.changes.collectAsState()
    LaunchedEffect(Unit) { SailChecklistCardStore.refresh(context) }
    LaunchedEffect(sailCardDismissed, permissionRefreshToken, summaries.size, sailEvidenceVersion) {
        sailChecklistProgress = if (sailCardDismissed) {
            null
        } else {
            withContext(Dispatchers.IO) {
                val report = SailChecklistInputs.report(
                    context,
                    store,
                    nearbyPermissionGranted = hasPermissions,
                    notificationsPermissionGranted = notificationsDeliverable(context),
                    batteryOptimizationExempt = isIgnoringBatteryOptimizations(context),
                )
                // Gone the moment the family is ready, without anyone having
                // to dismiss it, and counted out of every step rather than
                // only the required ones -- see the core's own note on why.
                if (report.ready) {
                    null
                } else {
                    SailChecklistProgress(report.doneCount.toInt(), report.totalCount.toInt())
                }
            }
        }
    }
    val summaryScope = rememberCoroutineScope()
    // G1: never load summaries on main. The coordinator debounces bursts,
    // guarantees a periodic refresh during a sustained storm, and never
    // cancels an in-flight UniFFI/SQLite load (those calls are blocking and
    // would keep consuming IO after coroutine cancellation anyway).
    val summaryRefreshCoordinator = remember(summaryScope, context, store, identity) {
        ChatSummaryRefreshCoordinator(
            scope = summaryScope,
            debounceMs = ChatSummaryRefreshPolicy.DEBOUNCE_MS,
            maxLatencyMs = ChatSummaryRefreshPolicy.MAX_LATENCY_MS,
            load = {
                withContext(Dispatchers.IO) {
                    val loaded = ChatSummaryLoader.loadAll(context, store, identity)
                    val warning = runCatching {
                        store.hasIdentityCloneWarning(identity.userId)
                    }.getOrDefault(false)
                    loaded to warning
                }
            },
            onLoaded = { (loaded, warning) ->
                summaries = loaded
                ownCloneWarning = warning
            },
        )
    }

    fun scheduleSummaryReload(immediate: Boolean = false) {
        summaryRefreshCoordinator.request(immediate)
    }

    LaunchedEffect(Unit) {
        scheduleSummaryReload(immediate = true)
        com.cruisemesh.app.chat.ChatEvents.changes.collect {
            scheduleSummaryReload(immediate = false)
        }
    }

    // Refresh when navigating back to home
    DisposableEffect(navController) {
        val listener = NavController.OnDestinationChangedListener { _, dest, _ ->
            if (dest.route == "home") {
                ownDisplayName = ProfileStore.loadDisplayName(context)
                ownAvatarPath = ProfilePhotoStore.loadAvatarPath(context)
                internetDeliveryService = configuredInternetDeliveryService(context)
                permissionRefreshToken += 1
                bluetoothEnabled = isBluetoothRadioEnabled(context)
                scheduleSummaryReload(immediate = true)
            }
        }
        navController.addOnDestinationChangedListener(listener)
        onDispose { navController.removeOnDestinationChangedListener(listener) }
    }

    val displaySummaries = remember(summaries, nearbyPeerIds, relayHealth, pushHealthy, contactLastSeen, presenceLastSeen, connectivityNowMs) {
        summaries.map { summary ->
            summary.copy(
                reachability = computeSummaryReachability(
                    summary,
                    identity.userId,
                    nearbyPeerIds,
                    relayHealth,
                    contactLastSeen,
                    presenceLastSeen,
                    connectivityNowMs,
                    pushHealthy,
                ),
            )
        }
    }
    // On the pill's clock, not the chat list's: this feeds the pill's core
    // verdict, and a staleness window evaluated 20 s later than the page's
    // would reintroduce the disagreement the shared classification removed.
    val displayRelayHealth = remember(relayHealth, pushHealthy, pillNowMs) {
        freshRelayHealthForDisplay(relayHealth, pillNowMs, pushHealthy)
    }
    // The pill's severity is now the core's, which needs the local Wi-Fi path
    // as well. Only whether the transport holds a listening socket is taken,
    // mapped before it is collected: the full LAN snapshot changes on every
    // peer and every sweep, and collecting it here would recompose the whole
    // home screen at LAN-event rates for a boolean that flips when the mesh
    // starts and stops. The endpoint itself never leaves this line.
    val lanListening by remember {
        LanTransportDiagnostics.state
            .map { it.localEndpoint != null }
            .distinctUntilChanged()
    }.collectAsState(initial = false)
    // Held across recompositions so the core's bounded-Checking window is
    // measured from when the wait actually began; a mark restamped on every
    // recomposition can never expire.
    val pillCheckingClock = remember { CheckingClock() }
    val pillStatus = remember(
        runtimeStatus,
        nearbyPeerIds,
        displayRelayHealth,
        internetDeliveryService,
        lanListening,
        pillNowMs,
    ) {
        val relayPath = ConnectionInputs.relay(displayRelayHealth, internetDeliveryService != null)
        MeshStatusTextLogic.build(
            runtimeState = runtimeStatus,
            nearbyCount = nearbyPeerIds.size,
            relayHealth = displayRelayHealth,
            internetDeliveryService = internetDeliveryService,
            lanListening = lanListening,
            checkingSinceMs = pillCheckingClock.mark(
                connectionCheckPending(
                    ConnectionInputs.runtime(runtimeStatus),
                    ConnectionInputs.bluetooth(runtimeStatus),
                    ConnectionInputs.localWifi(runtimeStatus, lanListening),
                    relayPath,
                ),
                pillNowMs,
            ),
            nowMs = pillNowMs,
        )
    }
    val pillDotColor = pillStatus.dot.toComposeColor()

    ChatListScreen(
        ownUserId = identity.userId,
        ownDisplayName = ownDisplayName,
        ownAvatarPath = ownAvatarPath,
        onChatClick = { summary ->
            if (summary.isGroup) {
                navController.navigate("group/${UserIdHex.encode(summary.chatId)}")
            } else {
                navController.navigate("chat/${UserIdHex.encode(summary.chatId)}")
            }
        },
        onDeleteSummary = { summary ->
            summaryScope.launch(Dispatchers.IO) {
                if (summary.isGroup) {
                    store.deleteGroup(summary.chatId)
                } else {
                    store.deleteContact(summary.chatId, System.currentTimeMillis())
                    FriendDirectorySender.queueToAllContacts(context, store, identity)
                }
                scheduleSummaryReload(immediate = true)
            }
        },
        onMarkRead = { summary ->
            summaryScope.launch(Dispatchers.IO) {
                val senderIds = if (summary.isGroup) {
                    summary.group?.memberUserIds.orEmpty().filterNot { it.contentEquals(identity.userId) }
                } else {
                    listOf(summary.chatId)
                }
                for (senderId in senderIds) {
                    val through = store.highestLamport(summary.chatId, senderId)
                    if (through > 0uL) {
                        store.recordOutgoingReceipt(summary.chatId, senderId, RECEIPT_TYPE_READ, through)
                    }
                }
                MessageNotifier.cancel(context, summary.chatId)
                scheduleSummaryReload(immediate = true)
            }
        },
        onNewChatClick = { navController.navigate("contacts") },
        onAddFriendClick = { navController.navigate("addFriend") },
        onNewGroupClick = { navController.navigate("newGroup") },
        onProfileClick = { navController.navigate("profile") },
        onFriendsClick = { navController.navigate("contacts") },
        onConnectionDetailsClick = { navController.navigate("connectionDetails") },
        onSettingsClick = { navController.navigate("settings") },
        onHelpClick = { navController.navigate("help") },
        onMeshStatusClick = { showMeshStatusLegend = true },
        meshStatusText = transientMeshStatus ?: pillStatus.text,
        meshStatusDotColor = if (transientMeshStatus != null) null else pillDotColor,
        sailChecklistProgress = sailChecklistProgress,
        onSailChecklistClick = { navController.navigate("sailChecklist") },
        onDismissSailChecklist = { SailChecklistCardStore.dismiss(context) },
        ownCloneWarning = ownCloneWarning,
        onDismissOwnCloneWarning = {
            summaryScope.launch(Dispatchers.IO) {
                if (runCatching { store.clearIdentityCloneWarning(identity.userId) }.isSuccess) {
                    withContext(Dispatchers.Main.immediate) { ownCloneWarning = false }
                }
            }
        },
        connectivityWarning = when {
            !hasPermissions -> ConnectivityWarning(
                title = stringResource(R.string.ui_permissions_required_mesh_off),
                body = stringResource(R.string.ui_nearby_permission_blocking_body),
                actionLabel = stringResource(R.string.ui_enable_permissions),
                severity = ConnectivityWarningSeverity.Blocking,
            )
            !bluetoothEnabled -> ConnectivityWarning(
                title = stringResource(R.string.ui_bluetooth_is_off),
                body = stringResource(R.string.ui_bluetooth_is_off_body),
                actionLabel = stringResource(R.string.ui_turn_on_bluetooth),
                severity = ConnectivityWarningSeverity.Blocking,
            )
            !notificationPermissionGranted -> ConnectivityWarning(
                title = stringResource(R.string.ui_notifications_are_off),
                body = stringResource(R.string.ui_notifications_off_body),
                actionLabel = stringResource(R.string.ui_enable_notifications),
                severity = ConnectivityWarningSeverity.Caution,
            )
            bluetoothAudioConnected && !bluetoothAudioWarningDismissed && !hideBluetoothAudioWarning -> ConnectivityWarning(
                title = stringResource(R.string.ui_bluetooth_audio_connected),
                body = stringResource(R.string.ui_bluetooth_audio_connected_body),
                actionLabel = stringResource(R.string.ui_dismiss),
                secondaryActionLabel = stringResource(R.string.ui_dont_show_again),
                severity = ConnectivityWarningSeverity.Caution,
            )
            else -> null
        },
        onConnectivityWarningClick = {
            when {
                !hasPermissions -> permissionLauncher.launch(MeshService.requiredPermissions())
                !bluetoothEnabled -> runCatching {
                    bluetoothEnableLauncher.launch(Intent(BluetoothAdapter.ACTION_REQUEST_ENABLE))
                }.onFailure {
                    context.startActivity(Intent(Settings.ACTION_BLUETOOTH_SETTINGS))
                }
                !notificationPermissionGranted && Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU ->
                    notificationPermissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
                bluetoothAudioConnected -> bluetoothAudioWarningDismissed = true
            }
        },
        onConnectivityWarningSecondaryClick = {
            if (bluetoothAudioConnected) {
                hideBluetoothAudioWarning = true
                bluetoothAudioWarningDismissed = true
                uiPrefs.edit().putBoolean(PREF_HIDE_BLUETOOTH_AUDIO_WARNING, true).apply()
            }
        },
        summaries = displaySummaries
    )

    if (showMeshStatusLegend) {
        MeshStatusLegendDialog(
            statusText = transientMeshStatus ?: pillStatus.text,
            canStartMesh = runtimeStatus == MeshRuntimeState.STOPPED,
            onStartMesh = {
                if (hasPermissions) {
                    MeshStartupPreferences.setMeshEnabled(context, true)
                    startMesh(context)
                } else {
                    permissionLauncher.launch(MeshService.requiredPermissions())
                }
            },
            onConnectionDetails = {
                showMeshStatusLegend = false
                navController.navigate("connectionDetails")
            },
            onDismiss = { showMeshStatusLegend = false },
        )
    }
}

/**
 * Gathers the before-you-sail checklist's inputs and keeps them current while
 * the screen is open.
 *
 * Every grant row here opens a real system screen, and the answer only lands
 * when the user comes back, so the whole report is recomputed on every resume
 * as well as after each launcher returns. Anything less and a step someone
 * just finished stays stubbornly unticked.
 */
@Composable
private fun SailChecklistRoute(navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val activity = context as? ComponentActivity
    var refreshToken by remember { mutableStateOf(0) }
    var report by remember { mutableStateOf<CoreSailChecklistReport?>(null) }
    var contactCount by remember { mutableStateOf(0) }

    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { grants ->
        refreshToken += 1
        // A row that does nothing is worse than no row: once the system has
        // stopped offering the dialog, send the user where the switch is.
        val permanentlyDenied = activity != null && MeshService.requiredPermissions().any { perm ->
            grants[perm] == false &&
                !ActivityCompat.shouldShowRequestPermissionRationale(activity, perm)
        }
        if (permanentlyDenied) openAppPermissionSettings(context)
    }
    val notificationPermissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { refreshToken += 1 }
    val batteryOptimizationLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) { refreshToken += 1 }

    val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
    DisposableEffect(lifecycleOwner) {
        val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
            if (event == androidx.lifecycle.Lifecycle.Event.ON_RESUME) refreshToken += 1
        }
        lifecycleOwner.lifecycle.addObserver(observer)
        onDispose { lifecycleOwner.lifecycle.removeObserver(observer) }
    }

    LaunchedEffect(refreshToken) {
        val gathered = withContext(Dispatchers.IO) {
            val state = SailChecklistInputs.deviceState(
                context,
                store,
                nearbyPermissionGranted = hasMeshPermissions(context),
                notificationsPermissionGranted = notificationsDeliverable(context),
                batteryOptimizationExempt = isIgnoringBatteryOptimizations(context),
            )
            state.contactCount to coreSailChecklist(SailChecklistInputs.coreInput(state))
        }
        contactCount = gathered.first
        report = gathered.second
    }

    report?.let { current ->
        SailChecklistScreen(
            report = current,
            contactCount = contactCount,
            onShorePass = { navController.navigate("shorePass") },
            onAddFamily = { navController.navigate("addFriend") },
            onGrantPermission = { permission ->
                when (permission) {
                    CoreSailPermission.BLUETOOTH ->
                        permissionLauncher.launch(MeshService.requiredPermissions())
                    CoreSailPermission.NOTIFICATIONS ->
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                            notificationPermissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
                        } else {
                            // Older Android has no runtime prompt to raise;
                            // the switch lives in the app's own settings.
                            openAppPermissionSettings(context)
                        }
                    CoreSailPermission.BATTERY_OPTIMIZATION ->
                        batteryOptimizationLauncher.launch(batteryOptimizationIntent(context))
                }
            },
            onBackUp = { navController.navigate("backup") },
            onBack = { navController.popOrExit(context) },
        )
    }
}

@Composable
private fun SettingsRoute(
    identity: Identity,
    navController: NavHostController,
    appearancePreference: AppearancePreference,
    onAppearancePreferenceChange: (AppearancePreference) -> Unit,
) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val runtimeStatus by MeshRuntimeStatus.state.collectAsState()
    val relayHealth by MeshConnectivityStatus.relay.collectAsState()
    var meshEnabled by remember { mutableStateOf(MeshStartupPreferences.isMeshEnabled(context)) }
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { grants ->
        if (grants.values.all { it }) {
            meshEnabled = true
            MeshStartupPreferences.setMeshEnabled(context, true)
            startMesh(context)
        } else {
            meshEnabled = false
            MeshStartupPreferences.setMeshEnabled(context, false)
            Toast.makeText(
                context,
                context.getString(R.string.ui_nearby_required_to_turn_on_mesh),
                Toast.LENGTH_LONG,
            ).show()
        }
    }
    SettingsScreen(
        meshEnabled = meshEnabled,
        meshStatus = runtimeStatus.label,
        relayHealth = relayHealth,
        appearancePreference = appearancePreference,
        onAppearancePreferenceChange = onAppearancePreferenceChange,
        onShorePass = { navController.navigate("shorePass") },
        onSailChecklist = { navController.navigate("sailChecklist") },
        onConnectionDetails = { navController.navigate("connectionDetails") },
        onDeveloperSettings = { navController.navigate("developerSettings") },
        onYourDevices = { navController.navigate("yourDevices") },
        onBackUp = { navController.navigate("backup") },
        onMeshEnabledChange = { enabled ->
            if (enabled) {
                if (hasMeshPermissions(context)) {
                    meshEnabled = true
                    MeshStartupPreferences.setMeshEnabled(context, true)
                    startMesh(context)
                } else {
                    permissionLauncher.launch(MeshService.requiredPermissions())
                }
            } else {
                meshEnabled = false
                stopMesh(context)
            }
        },
        onFriendsOfFriendsChanged = { enabled ->
            FriendsOfFriendsStore.setEnabled(context, enabled)
            if (!enabled) store.clearFriendSuggestions()
            ProfileSyncSender.queueToAllContacts(
                context,
                store,
                identity,
                ProfileStore.loadOwnAvatarEpoch(context),
            )
            FriendDirectorySender.queueToAllContacts(context, store, identity)
        },
        onBack = { navController.popOrExit(context) },
    )
}

@Composable
private fun ProfileRoute(identity: Identity, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val displayId = remember(identity) { formatUserId(identity.userId) }
    val fingerprint = remember(identity) { fingerprintWords(identity.userId) }

    ProfileScreen(
        profileUserId = identity.userId,
        displayId = displayId,
        fingerprint = fingerprint,
        onShowMyQr = { navController.navigate("myQr") },
        onProfileChanged = { epoch ->
            ProfileSyncSender.queueToAllContacts(context, store, identity, epoch)
        },
        onBack = { navController.popOrExit(context) }
    )
}

@Composable
private fun ScanRoute(identity: Identity, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    var hasCameraPermission by remember {
        mutableStateOf(
            ContextCompat.checkSelfPermission(context, Manifest.permission.CAMERA) ==
                PackageManager.PERMISSION_GRANTED,
        )
    }
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { granted -> hasCameraPermission = granted }

    LaunchedEffect(Unit) {
        if (!hasCameraPermission) permissionLauncher.launch(Manifest.permission.CAMERA)
    }

    if (hasCameraPermission) {
        ScanScreen(
            ownUserId = identity.userId,
            store = store,
            onContactAdded = { scanned ->
                // Pointing a camera at their screen is co-presence by
                // construction -- no need to consult the peer set, which may
                // not have HELLO'd them yet.
                SharedCardImport.confirm(
                    context,
                    store,
                    identity,
                    FriendPreview(scanned),
                    addedNearby = true,
                )
            },
            onSharedCard = { shared ->
                SharedCardImport.previewShared(context, store, identity.userId, shared)
            },
            onConfirmShared = { preview ->
                SharedCardImport.confirm(context, store, identity, preview, addedNearby = true)
            },
            onSayHi = { openFriendChat(navController, it) },
            onDone = { returnToContacts(navController) },
            onBack = { navController.popOrExit(context) },
        )
    } else {
        Scaffold { innerPadding ->
            Column(
                modifier = Modifier.fillMaxSize().padding(innerPadding).padding(24.dp),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text(stringResource(R.string.ui_camera_permission_is_needed_to_scan_a_friend))
                Button(onClick = { navController.popOrExit(context) }, modifier = Modifier.padding(top = 16.dp)) {
                    Text(stringResource(R.string.ui_back))
                }
            }
        }
    }
}

@Composable
private fun AddFriendRoute(identity: Identity, navController: NavHostController, initialToken: String? = null) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }

    // Re-read on every return to this screen: answering a request elsewhere
    // must not leave a stale "(1)" pointing at an empty list.
    var waitingCount by remember { mutableStateOf(0) }
    DisposableEffect(navController) {
        fun refresh() {
            waitingCount = store.listPendingSharedRequests(System.currentTimeMillis()).size
        }
        refresh()
        val listener = NavController.OnDestinationChangedListener { _, dest, _ ->
            if (dest.route?.startsWith("addFriend") == true) refresh()
        }
        navController.addOnDestinationChangedListener(listener)
        onDispose { navController.removeOnDestinationChangedListener(listener) }
    }

    AddFriendScreen(
        onScanClick = { navController.navigate("scan") },
        onShowMyCardClick = { navController.navigate("myQr") },
        onImportText = { text ->
            try {
                when (val import = parseFriendImport(text)) {
                    is FriendImport.Shared ->
                        SharedCardImport.previewShared(context, store, identity.userId, import.shared)
                    is FriendImport.Direct -> {
                        val card = import.card
                        val userId = friendCardUserId(card)
                        if (userId.contentEquals(identity.userId)) {
                            ImportFriendResult.Error("That's your own card")
                        } else {
                            val candidate = Contact(
                                    userId = userId,
                                    name = card.name,
                                    signPk = card.signPk,
                                    agreePk = card.agreePk,
                                    relayUrl = card.relayUrl,
                                    relayToken = card.relayToken,
                            )
                            ImportFriendResult.Preview(
                                FriendPreview(
                                    contact = candidate,
                                    match = friendCardMatch(candidate, store.listContacts()),
                                    legacyUnverified = card.signature == null,
                                ),
                            )
                        }
                    }
                }
            } catch (e: Exception) {
                ImportFriendResult.Error(context.getString(friendImportFailureResId(e, text)))
            }
        },
        onConfirmContact = { preview ->
            // A pasted card says nothing about where its owner is: it may have
            // been handed over in person or forwarded from an aeroplane. Only
            // a live link to them counts as having met.
            SharedCardImport.confirm(
                context,
                store,
                identity,
                preview,
                addedNearby = MeshConnectivityStatus.nearbyPeerIds.value
                    .contains(UserIdHex.encode(preview.contact.userId)),
            )
        },
        waitingToConnectCount = waitingCount,
        onWaitingToConnect = { navController.navigate("waitingToConnect") },
        onRequestSuggestion = { suggestion ->
            FriendDirectorySender.requestSuggestedFriend(context, store, identity, suggestion)
        },
        onHideSuggestion = { suggestion ->
            store.setFriendSuggestionState(suggestion.candidate.userId, 2u)
        },
        ownUserId = identity.userId,
        store = store,
        initialText = initialToken.orEmpty(),
        onSayHi = { openFriendChat(navController, it) },
        onDone = { returnToContacts(navController) },
        onBack = { navController.popOrExit(context) },
    )
}

/**
 * The pending shared-card requests and their decisions
 * (specs/share-contact.md). Nothing here is written to `contacts` until
 * **Connect**.
 */
@Composable
private fun WaitingToConnectRoute(identity: Identity, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    var rows by remember { mutableStateOf(emptyList<PendingSharedRequestRow>()) }

    fun reload() {
        rows = store.listPendingSharedRequests(System.currentTimeMillis()).map { request ->
            PendingSharedRequestRow(
                request = request,
                sharerName = store.getContact(request.sharerUserId)?.let(::coreContactDisplayName)
                    ?: formatUserId(request.sharerUserId),
                offerSuppression = ShareContactPolicy.offerSuppression(
                    store.getSharedRequestDismissal(request.requesterUserId),
                ),
            )
        }
    }

    LaunchedEffect(Unit) { reload() }

    WaitingToConnectScreen(
        rows = rows,
        onConnect = { request ->
            val contact = RelayImport.reconcileOnImport(
                context,
                store,
                Contact(
                    userId = request.requesterUserId,
                    name = request.name,
                    signPk = request.signPk,
                    agreePk = request.agreePk,
                    relayUrl = request.relayUrl,
                    relayToken = request.relayToken,
                ),
            )
            store.upsertContactProvenance(
                ContactProvenance(
                    userId = contact.userId,
                    source = 2u,
                    introducerUserId = request.sharerUserId,
                    introducedAtMs = System.currentTimeMillis(),
                    addedNearby = MeshConnectivityStatus.nearbyPeerIds.value
                        .contains(UserIdHex.encode(contact.userId)),
                ),
            )
            store.removeFriendSuggestion(contact.userId)
            FriendRequestSender.queueForScannedContact(context, store, identity, contact)
            ProfileSyncSender.queueToContact(
                context,
                store,
                identity,
                contact,
                ProfileStore.loadOwnAvatarEpoch(context),
            )
            FriendDirectorySender.queueToAllContacts(context, store, identity)
            store.deletePendingSharedRequest(contact.userId)
            reload()
        },
        onNotNow = { request ->
            store.deletePendingSharedRequest(request.requesterUserId)
            store.recordSharedRequestDismissal(request.requesterUserId)
            reload()
        },
        onDontAskAgain = { request ->
            // Quiet by construction: nothing is sent, nobody is told.
            store.suppressSharedRequests(request.requesterUserId)
            store.deletePendingSharedRequest(request.requesterUserId)
            reload()
        },
        onBack = { navController.popOrExit(context) },
    )
}

/** Hands one contact's card on as a signed, expiring QR code (specs/share-contact.md). */
@Composable
private fun ShareContactRoute(identity: Identity, userIdHex: String, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val contact = remember(userIdHex) { store.getContact(UserIdHex.decode(userIdHex)) }
    val policy = remember(userIdHex) { contact?.let { store.getContactDiscoveryPolicy(it.userId) } }

    // Their switch could have gone off between opening the chat and getting
    // here; issuing a card then would be issuing one they had just refused.
    if (contact == null ||
        ShareContactPolicy.availability(policy, store.isUserBlocked(contact.userId)) !=
        ShareContactAvailability.AVAILABLE
    ) {
        LaunchedEffect(userIdHex) { navController.popOrExit(context) }
        return
    }
    ShareContactScreen(
        identity = identity,
        contact = contact,
        sharedPolicyRevision = policy?.revision ?: 0uL,
        onBack = { navController.popOrExit(context) },
    )
}

private fun openFriendChat(navController: NavHostController, contact: Contact) {
    navController.navigate("chat/${UserIdHex.encode(contact.userId)}") {
        popUpTo("home")
        launchSingleTop = true
    }
}

private fun returnToContacts(navController: NavHostController) {
    navController.navigate("contacts") {
        popUpTo("home")
        launchSingleTop = true
    }
}

@Composable
private fun ContactsRoute(identity: Identity, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    var contacts by remember { mutableStateOf(store.listContacts()) }
    var avatars by remember {
        mutableStateOf(contacts.associate { formatUserId(it.userId) to store.contactAvatar(it.userId) }.filterValues { it != null }.mapValues { it.value!! })
    }
    var outgoingShared by remember { mutableStateOf(emptyMap<String, OutgoingSharedRequest>()) }
    fun reloadContacts() {
        contacts = store.listContacts()
        avatars = contacts.associate { formatUserId(it.userId) to store.contactAvatar(it.userId) }
            .filterValues { it != null }
            .mapValues { it.value!! }
        outgoingShared = store.listOutgoingSharedRequests()
            .associateBy { formatUserId(it.candidateUserId) }
    }
    LaunchedEffect(Unit) { reloadContacts() }
    
    // Refresh list when resuming screen
    DisposableEffect(navController) {
        val listener = NavController.OnDestinationChangedListener { _, dest, _ ->
            if (dest.route == "contacts") {
                reloadContacts()
            }
        }
        navController.addOnDestinationChangedListener(listener)
        onDispose { navController.removeOnDestinationChangedListener(listener) }
    }

    ContactsScreen(
        contacts = contacts,
        avatarBytesByUserId = avatars,
        outgoingSharedByUserId = outgoingShared,
        onContactClick = { contact -> navController.navigate("chat/${UserIdHex.encode(contact.userId)}") },
        onContactDelete = { contact ->
            store.deleteContact(contact.userId, System.currentTimeMillis())
            FriendDirectorySender.queueToAllContacts(context, store, identity)
            reloadContacts()
        },
        onAddFriendClick = { navController.navigate("addFriend") },
        onMyCardClick = { navController.navigate("myQr") },
        onNewGroupClick = { navController.navigate("newGroup") },
        onBack = { navController.popOrExit(context) },
    )
}

@Composable
private fun NewGroupRoute(identity: Identity, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val contacts = remember { store.listContacts() }
    val avatars = remember(contacts) {
        contacts.associate { formatUserId(it.userId) to store.contactAvatar(it.userId) }
            .filterValues { it != null }
            .mapValues { it.value!! }
    }
    val groupSender = remember { GroupSender(store, identity) }

    NewGroupScreen(
        contacts = contacts,
        avatarBytesByUserId = avatars,
        onCreate = { name, members ->
            val group = groupSender.createAndInvite(name, members)
            if (group != null) {
                navController.navigate("group/${UserIdHex.encode(group.id)}") {
                    popUpTo("contacts") { inclusive = false }
                }
            }
        },
        onBack = { navController.popOrExit(context) },
    )
}

@Composable
private fun ChatRoute(identity: Identity, userIdHex: String, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val contact = remember(userIdHex) { store.getContact(UserIdHex.decode(userIdHex)) }

    if (contact != null) {
        val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
        DisposableEffect(userIdHex, lifecycleOwner) {
            val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
                when (event) {
                    androidx.lifecycle.Lifecycle.Event.ON_START -> {
                        // Chat is actually on screen (this destination is
                        // resumed AND the app is foregrounded). Mark visible,
                        // clear any notification posted while backgrounded, and
                        // re-run read-on-view so returning to an already-open
                        // chat still advances read state.
                        ChatVisibility.setVisible(contact.userId)
                        MessageNotifier.cancel(context, contact.userId)
                        // Read state is local durable data and must advance even
                        // when MeshService has not registered yet (for example
                        // immediately after a restore while permissions are
                        // still being granted). The event below separately
                        // queues/transmits the peer-facing receipt when the
                        // service is available.
                        val through = store.highestLamport(contact.userId, contact.userId)
                        if (through > 0uL) {
                            store.recordOutgoingReceipt(
                                contact.userId,
                                contact.userId,
                                RECEIPT_TYPE_READ,
                                through,
                            )
                            com.cruisemesh.app.chat.ChatEvents.notifyChatChanged(contact.userId)
                        }
                        ChatViewEvents.onChatViewed(contact.userId)
                    }
                    androidx.lifecycle.Lifecycle.Event.ON_STOP ->
                        // Navigated away OR app backgrounded: no longer on
                        // screen, so incoming messages for it should notify.
                        ChatVisibility.clearVisible(contact.userId)
                    else -> {}
                }
            }
            lifecycleOwner.lifecycle.addObserver(observer)
            onDispose {
                lifecycleOwner.lifecycle.removeObserver(observer)
                ChatVisibility.clearVisible(contact.userId)
            }
        }
        val sender = remember { RealMeshSender(store, identity) }
        val nearbyPeerIds by MeshConnectivityStatus.nearbyPeerIds.collectAsState()
        val nearbyTransports by MeshConnectivityStatus.nearbyTransports.collectAsState()
        val relayHealth by MeshConnectivityStatus.relay.collectAsState()
        val pushHealthy by MeshConnectivityStatus.pushHealthy.collectAsState()
        val contactLastSeen by MeshConnectivityStatus.contactLastSeen.collectAsState()
        val presenceLastSeen by MeshConnectivityStatus.presenceLastSeen.collectAsState()
        val staleRelayContacts by MeshConnectivityStatus.staleRelayContacts.collectAsState()
        val connectivityNowMs = rememberConnectivityNowMs()
        val reachability = remember(contact.userId, nearbyPeerIds, relayHealth, pushHealthy, contactLastSeen, presenceLastSeen, connectivityNowMs) {
            reachabilityLevelForUserId(contact.userId, nearbyPeerIds, relayHealth, contactLastSeen, presenceLastSeen, connectivityNowMs, pushHealthy)
        }
        // nearbyPeerIds only proves *some* link HELLO'd, not which transport;
        // for NEARBY copy, read the live per-peer transport from the observable
        // map so the copy recomposes when a send would switch radios. Reading
        // MeshRouter.routeFor() imperatively here instead would freeze the copy
        // on the dead transport across a LAN->BLE handoff (the peer stays
        // HELLO'd, so nothing else this composable observes changes).
        val nearbyTransport = if (reachability == ReachabilityLevel.NEARBY) {
            nearbyTransports[UserIdHex.encode(contact.userId)]
        } else {
            null
        }
        // Whether this contact can be reached at all when no direct path
        // exists. A property of their friend card, not of the moment, so it
        // only recomputes when the card or our own config changes.
        val ownRelayConfig = remember { RelayConfigStore.load(context) }
        val delivery = remember(contact.relayUrl, contact.relayToken, ownRelayConfig) {
            contactDelivery(
                contact.relayUrl,
                contact.relayToken,
                ownRelayConfig?.relayUrl,
                ownRelayConfig?.relayToken,
            )
        }
        val contactHasInternetDelivery = delivery != ContactDelivery.NearbyOnly
        // Whether we ever stood next to this person. A durable fact, so read it
        // once per chat rather than on every connectivity tick.
        val addedNearby = remember(contact.userId) {
            store.getContactProvenance(contact.userId)?.addedNearby ?: false
        }
        // Which direction of this chat cannot cross the internet. Local
        // knowledge only: our own config, their card, and whether a link to
        // them exists right now.
        val composerReachVerdict = remember(delivery, ownRelayConfig, nearbyPeerIds, addedNearby, contact.userId) {
            composerReach(
                delivery,
                ownRelayConfig != null,
                nearbyPeerIds.contains(UserIdHex.encode(contact.userId)),
                addedNearby,
            )
        }
        val reachabilityStatusText = remember(reachability, contactLastSeen, presenceLastSeen, connectivityNowMs, nearbyTransport, contactHasInternetDelivery) {
            val hex = UserIdHex.encode(contact.userId)
            ContactReachability.chatHeaderCopy(
                reachability,
                listOfNotNull(contactLastSeen[hex], presenceLastSeen[hex]).maxOrNull(),
                connectivityNowMs,
                nearbyTransport,
                contactHasInternetDelivery,
            )
        }
        val reachabilityDetailsText = remember(reachability, contactLastSeen, presenceLastSeen, connectivityNowMs, nearbyTransport, contactHasInternetDelivery) {
            val hex = UserIdHex.encode(contact.userId)
            ContactReachability.contactDetailsCopy(
                reachability,
                listOfNotNull(contactLastSeen[hex], presenceLastSeen[hex]).maxOrNull(),
                presenceLastSeen[hex],
                connectivityNowMs,
                nearbyTransport,
                contactHasInternetDelivery,
            )
        }
        ChatScreen(
            contact = contact,
            ownUserId = identity.userId,
            sender = sender,
            store = store,
            onBack = { navController.popOrExit(context) },
            onDeleteContact = {
                store.deleteContact(contact.userId, System.currentTimeMillis())
                FriendDirectorySender.queueToAllContacts(context, store, identity)
                navController.popOrExit(context)
            },
            reachability = reachability,
            reachabilityStatusText = reachabilityStatusText,
            reachabilityDetailsText = reachabilityDetailsText,
            relayCardIsStale = staleRelayContacts.contains(UserIdHex.encode(contact.userId)),
            composerReach = composerReachVerdict,
            onShareContact = { navController.navigate("shareContact/${UserIdHex.encode(it.userId)}") },
        )
    } else {
        LaunchedEffect(Unit) { navController.popOrExit(context) }
    }
}

@Composable
private fun GroupChatRoute(identity: Identity, groupIdHex: String, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val groupId = remember(groupIdHex) { UserIdHex.decode(groupIdHex) }
    val group = remember(groupIdHex) { store.getGroup(groupId) }
    val contactsByUserId = remember {
        store.listContacts().associateBy { UserIdHex.encode(it.userId) }
    }

    if (group != null) {
        val lifecycleOwner = androidx.lifecycle.compose.LocalLifecycleOwner.current
        DisposableEffect(groupIdHex, lifecycleOwner) {
            val observer = androidx.lifecycle.LifecycleEventObserver { _, event ->
                when (event) {
                    androidx.lifecycle.Lifecycle.Event.ON_START -> {
                        ChatVisibility.setVisible(group.id)
                        MessageNotifier.cancel(context, group.id)
                        // Local read watermarks for every other member. highestLamport
                        // (plain MAX), not highestContiguousLamport: this is a per-member
                        // peer-stream watermark, and the contiguous count stalls at 0
                        // once a member's stream legitimately starts above lamport 1
                        // (post chat-history-wipe ratchet), which would strand the unread
                        // badge for that member forever. ChatViewEvents then authors
                        // the D9 wire receipts against those watermarks.
                        for (memberId in group.memberUserIds) {
                            if (memberId.contentEquals(identity.userId)) continue
                            val through = store.highestLamport(group.id, memberId)
                            if (through > 0uL) {
                                store.recordOutgoingReceipt(group.id, memberId, RECEIPT_TYPE_READ, through)
                            }
                        }
                        ChatViewEvents.onChatViewed(group.id)
                    }
                    androidx.lifecycle.Lifecycle.Event.ON_STOP ->
                        ChatVisibility.clearVisible(group.id)
                    else -> {}
                }
            }
            lifecycleOwner.lifecycle.addObserver(observer)
            onDispose {
                lifecycleOwner.lifecycle.removeObserver(observer)
                ChatVisibility.clearVisible(group.id)
            }
        }
        val sender = remember { GroupSender(store, identity) }
        val nearbyPeerIds by MeshConnectivityStatus.nearbyPeerIds.collectAsState()
        val relayHealth by MeshConnectivityStatus.relay.collectAsState()
        val pushHealthy by MeshConnectivityStatus.pushHealthy.collectAsState()
        val contactLastSeen by MeshConnectivityStatus.contactLastSeen.collectAsState()
        val presenceLastSeen by MeshConnectivityStatus.presenceLastSeen.collectAsState()
        val connectivityNowMs = rememberConnectivityNowMs()
        val reachableMemberCount = remember(group, nearbyPeerIds, relayHealth, pushHealthy, contactLastSeen, presenceLastSeen, connectivityNowMs) {
            groupReachableCounts(group, identity.userId, nearbyPeerIds, relayHealth, contactLastSeen, presenceLastSeen, connectivityNowMs, pushHealthy).first
        }
        val memberReachabilityByUserId = remember(
            contactsByUserId,
            nearbyPeerIds,
            relayHealth,
            pushHealthy,
            contactLastSeen,
            presenceLastSeen,
            connectivityNowMs,
        ) {
            contactsByUserId.values.associate { contact ->
                UserIdHex.encode(contact.userId) to reachabilityLevelForUserId(
                    contact.userId,
                    nearbyPeerIds,
                    relayHealth,
                    contactLastSeen,
                    presenceLastSeen,
                    connectivityNowMs,
                    pushHealthy,
                )
            }
        }
        GroupChatScreen(
            group = group,
            ownUserId = identity.userId,
            contactsByUserId = contactsByUserId,
            sender = sender,
            store = store,
            onBack = { navController.popOrExit(context) },
            onDeleteGroup = {
                store.deleteGroup(group.id)
                navController.popOrExit(context)
            },
            reachableMemberCount = reachableMemberCount,
            memberReachabilityByUserId = memberReachabilityByUserId,
        )
    } else {
        LaunchedEffect(Unit) { navController.popOrExit(context) }
    }
}

private fun startMesh(context: Context) {
    MeshRuntimeStatus.markStarting()
    try {
        ContextCompat.startForegroundService(context, Intent(context, MeshService::class.java))
    } catch (e: RuntimeException) {
        // T21: this used to swallow the exception silently, which made a
        // failed start invisible -- a phone whose mesh never came up looked
        // identical in logs to one that was never asked to start. The usual
        // cause is ForegroundServiceStartNotAllowedException: Android 12+
        // refuses a foreground-service start from the background, which is
        // exactly what happens when the activity is created and the screen
        // goes off before this effect runs.
        Log.w(TAG, "Mesh foreground service start refused: ${e.javaClass.simpleName}: ${e.message}")
        MeshRuntimeStatus.markStopped()
    }
}

private fun stopMesh(context: Context) {
    MeshStartupPreferences.setMeshEnabled(context, false)
    if (MeshRuntimeStatus.state.value == MeshRuntimeState.STOPPED) return
    context.startService(
        Intent(context, MeshService::class.java).setAction(MeshService.ACTION_STOP),
    )
}

private const val TAG = "CruiseMeshUi"

private fun hasMeshPermissions(context: Context): Boolean =
    MeshService.requiredPermissions().all {
        ContextCompat.checkSelfPermission(context, it) == PackageManager.PERMISSION_GRANTED
    }

private fun hasNotificationPermission(context: Context): Boolean =
    Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
        ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) ==
        PackageManager.PERMISSION_GRANTED

// The sail checklist asks "will an arriving message actually show?", which
// the runtime permission alone cannot answer: below API 33 there is no
// permission at all (the check above short-circuits to true), and on any
// version the app-level notification toggle can be off while the permission
// is granted. areNotificationsEnabled covers both.
private fun notificationsDeliverable(context: Context): Boolean =
    NotificationManagerCompat.from(context).areNotificationsEnabled()

private fun openAppPermissionSettings(context: Context) {
    val intent = Intent(
        Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
        Uri.fromParts("package", context.packageName, null),
    ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    context.startActivity(intent)
}

private fun isBluetoothRadioEnabled(context: Context): Boolean {
    val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? android.bluetooth.BluetoothManager
    val adapter = manager?.adapter ?: return false
    return try {
        adapter.isEnabled
    } catch (_: SecurityException) {
        // BLUETOOTH_CONNECT isn't granted yet; the permissions banner already covers this case.
        true
    }
}

private fun isIgnoringBatteryOptimizations(context: Context): Boolean {
    val powerManager = context.getSystemService(Context.POWER_SERVICE) as PowerManager
    return powerManager.isIgnoringBatteryOptimizations(context.packageName)
}

@SuppressLint("BatteryLife")
private fun batteryOptimizationIntent(context: Context): Intent =
    Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:${context.packageName}"))
