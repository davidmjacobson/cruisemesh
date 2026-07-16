package com.cruisemesh.app

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Bundle
import android.os.PowerManager
import android.provider.Settings
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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.navigation.NavController
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.NavType
import androidx.navigation.navArgument
import com.cruisemesh.app.chat.ChatScreen
import com.cruisemesh.app.chat.GroupChatScreen
import com.cruisemesh.app.chat.GroupSender
import com.cruisemesh.app.chat.RealMeshSender
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.debug.DebugFileLog
import com.cruisemesh.app.friending.ContactsScreen
import com.cruisemesh.app.friending.FriendRequestSender
import com.cruisemesh.app.friending.AddFriendScreen
import com.cruisemesh.app.friending.ImportFriendResult
import com.cruisemesh.app.friending.FriendAddedOutcome
import com.cruisemesh.app.friending.FriendPreview
import com.cruisemesh.app.ui.ConnectivityWarning
import com.cruisemesh.app.ui.ConnectivityWarningSeverity
import android.widget.Toast
import androidx.core.app.ActivityCompat
import com.cruisemesh.app.friending.MyQrScreen
import com.cruisemesh.app.friending.ProfileSyncSender
import com.cruisemesh.app.friending.FriendDirectorySender
import com.cruisemesh.app.friending.FriendsOfFriendsStore
import com.cruisemesh.app.friending.ScanScreen
import com.cruisemesh.app.friending.extractFriendToken
import com.cruisemesh.app.identity.IdentityStore
import com.cruisemesh.app.identity.OnboardingStore
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
import com.cruisemesh.app.mesh.ReachabilityLevel
import com.cruisemesh.app.mesh.RelayHealth
import com.cruisemesh.app.mesh.parseLanEndpointLink
import com.cruisemesh.app.mesh.parseLanManualEndpoint
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.notify.MessageNotifier
import com.cruisemesh.app.relay.RelayImport
import com.cruisemesh.app.relay.RelayConfigStore
import com.cruisemesh.app.ui.ChatListLogic
import com.cruisemesh.app.ui.ChatListScreen
import com.cruisemesh.app.ui.ChatSummary
import com.cruisemesh.app.ui.CruiseMeshTheme
import com.cruisemesh.app.ui.LocalReachabilityPalette
import com.cruisemesh.app.ui.MeshStatusDotColor
import com.cruisemesh.app.ui.MeshStatusLegendDialog
import com.cruisemesh.app.ui.MeshStatusTextLogic
import com.cruisemesh.app.ui.NewGroupScreen
import com.cruisemesh.app.ui.OnboardingScreen
import com.cruisemesh.app.ui.ProfileScreen
import uniffi.cruisemesh_core.Group
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.formatUserId
import uniffi.cruisemesh_core.generateIdentity
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.ContactProvenance
import uniffi.cruisemesh_core.friendCardUserId
import uniffi.cruisemesh_core.parseFriendText
import uniffi.cruisemesh_core.lanDefaultTcpPort

private const val RECEIPT_TYPE_DELIVERED: kotlin.UByte = 1u
private const val RECEIPT_TYPE_READ: kotlin.UByte = 2u
private const val UI_PREFS_NAME = "cruisemesh_ui"
private const val PREF_HIDE_BLUETOOTH_AUDIO_WARNING = "hide_bluetooth_audio_warning"

data class PendingDeepLink(
    val idHex: String = "",
    val isGroup: Boolean = false,
    val friendToken: String? = null,
    val lanEndpoint: String? = null,
)

class MainActivity : ComponentActivity() {

    private val pendingDeepLink = mutableStateOf<PendingDeepLink?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        MeshStartupPreferences.clearExplicitStop(this)
        enableEdgeToEdge()
        // Debug builds: start capturing this process's log to a file so it can
        // be shared without adb (no-op in release). Idempotent with MeshService.
        DebugFileLog.start(this)
        pendingDeepLink.value = deepLinkFromIntent(intent)
        setContent {
            CruiseMeshTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    CruiseMeshApp(
                        pendingDeepLink = pendingDeepLink.value,
                        onPendingDeepLinkConsumed = { pendingDeepLink.value = null },
                    )
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        MeshStartupPreferences.clearExplicitStop(this)
        pendingDeepLink.value = deepLinkFromIntent(intent)
    }

    private fun deepLinkFromIntent(intent: Intent?): PendingDeepLink? {
        val hex = intent?.getStringExtra(MessageNotifier.EXTRA_CHAT_USER_ID_HEX)
        if (hex != null) {
            return PendingDeepLink(hex, intent.getBooleanExtra(MessageNotifier.EXTRA_CHAT_IS_GROUP, false))
        }
        val uri = intent?.data ?: return null
        if (
            uri.scheme == "https" &&
            uri.host == "cruisemesh.app" &&
            (uri.path == "/f" || uri.path == "/f/")
        ) {
            val token = uri.fragment?.let(::extractFriendToken)?.takeIf { it.startsWith("CMFRIEND1:") }
            if (token != null) return PendingDeepLink(friendToken = token)
        }
        if (
            uri.scheme == "https" &&
            uri.host == "cruisemesh.app" &&
            (uri.path == "/lan" || uri.path == "/lan/")
        ) {
            val endpoint = parseLanEndpointLink(uri.fragment)
            if (endpoint != null) return PendingDeepLink(lanEndpoint = endpoint.display)
        }
        return null
    }
}

@Composable
fun CruiseMeshApp(
    pendingDeepLink: PendingDeepLink? = null,
    onPendingDeepLinkConsumed: () -> Unit = {},
) {
    val context = LocalContext.current
    val identity = remember {
        IdentityStore.load(context) ?: generateIdentity().also { IdentityStore.save(context, it) }
    }
    val navController = rememberNavController()
    var onboardingCompleted by remember { mutableStateOf(OnboardingStore.isCompleted(context)) }

    NavHost(
        navController = navController,
        startDestination = if (onboardingCompleted) "home" else "onboarding",
    ) {
        composable("onboarding") {
            OnboardingRoute(
                identity = identity,
                onRestore = { navController.navigate("restore") },
            ) {
                onboardingCompleted = true
                navController.navigate("home") {
                    popUpTo("onboarding") { inclusive = true }
                }
            }
        }
        composable("home") { HomeRoute(identity, navController) }
        composable("profile") { ProfileRoute(identity, navController) }
        composable("backup") { BackupExportScreen(onBack = { navController.popBackStack() }) }
        composable("restore") { BackupRestoreScreen(onBack = { navController.popBackStack() }) }
        composable("myQr") {
            MyQrScreen(
                identity,
                onSayHi = { openFriendChat(navController, it) },
                onBack = { navController.popBackStack() },
            )
        }
        composable(
            "addFriend?token={token}",
            arguments = listOf(navArgument("token") { type = NavType.StringType; nullable = true; defaultValue = null }),
        ) { entry -> AddFriendRoute(identity, navController, entry.arguments?.getString("token")) }
        composable("scan") { ScanRoute(identity, navController) }
        composable("contacts") { ContactsRoute(identity, navController) }
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
private fun OnboardingRoute(identity: Identity, onRestore: () -> Unit, onComplete: () -> Unit) {
    val context = LocalContext.current
    val displayId = remember(identity) { formatUserId(identity.userId) }
    var displayName by remember { mutableStateOf(ProfileStore.loadDisplayName(context)) }
    var avatarPath by remember { mutableStateOf(ProfilePhotoStore.loadAvatarPath(context)) }
    var permissionRefreshToken by remember { mutableStateOf(0) }
    val meshPermissionsGranted = remember(context, permissionRefreshToken) {
        hasMeshPermissions(context)
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
                    "Enable Nearby devices (and notifications) in App permissions — without them the mesh cannot run",
                    Toast.LENGTH_LONG,
                ).show()
                openAppPermissionSettings(context)
            } else {
                Toast.makeText(
                    context,
                    "Nearby permissions are required for CruiseMesh to send and receive messages",
                    Toast.LENGTH_LONG,
                ).show()
            }
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
        onRequestBatteryExemption = {
            if (!batteryExemptionGranted) {
                batteryOptimizationLauncher.launch(batteryOptimizationIntent(context))
            }
        },
        onRestore = onRestore,
        onComplete = {
            if (displayName.isBlank()) {
                displayName = ProfileStore.defaultDisplayName()
                ProfileStore.saveDisplayName(context, displayName)
            }
            if (ProfileStore.loadOwnAvatarEpoch(context) == 0L) {
                ProfileStore.bumpOwnAvatarEpoch(context)
            }
            OnboardingStore.markCompleted(context)
            onComplete()
        },
    )
}

/**
 * CONNECTIVITY_INDICATOR.md §3.5: reachability levels decay purely with time
 * (a contact drifts ONLINE_RELAY -> RECENT -> OFFLINE with no event firing),
 * so the UI needs a clock tick to re-evaluate on, not just flow updates. Ticks
 * every 30 s, and only while the activity is RESUMED -- no background work.
 */
@Composable
private fun rememberConnectivityNowMs(): Long {
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
    LaunchedEffect(isResumed) {
        if (isResumed) {
            nowMs = System.currentTimeMillis()
            while (true) {
                kotlinx.coroutines.delay(30_000L)
                nowMs = System.currentTimeMillis()
            }
        }
    }
    return nowMs
}

/** CONNECTIVITY_INDICATOR.md §2.2: reachability for one userId from a snapshot of [MeshConnectivityStatus]. */
private fun reachabilityLevelForUserId(
    userId: ByteArray,
    nearbyPeerIds: Set<String>,
    relayHealth: RelayHealth,
    contactLastSeen: Map<String, Long>,
    presenceLastSeen: Map<String, Long>,
    nowMs: Long,
): ReachabilityLevel {
    val hex = UserIdHex.encode(userId)
    val presenceSeen = presenceLastSeen[hex]
    val peerSeen = listOfNotNull(contactLastSeen[hex], presenceSeen).maxOrNull()
    return ContactReachability.compute(
        directLink = hex in nearbyPeerIds,
        presenceLastSeenMs = presenceSeen,
        selfRelayHealthy = ContactReachability.selfRelayHealthy(relayHealth, nowMs),
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
): ReachabilityLevel {
    fun levelFor(userId: ByteArray) =
        reachabilityLevelForUserId(userId, nearbyPeerIds, relayHealth, contactLastSeen, presenceLastSeen, nowMs)

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
): Pair<Int, Int> {
    val others = group.memberUserIds.filterNot { it.contentEquals(ownUserId) }
    val reachable = others.count { userId ->
        val level = reachabilityLevelForUserId(userId, nearbyPeerIds, relayHealth, contactLastSeen, presenceLastSeen, nowMs)
        level == ReachabilityLevel.NEARBY || level == ReachabilityLevel.ONLINE_RELAY
    }
    return reachable to others.size
}

private fun freshRelayHealthForDisplay(relayHealth: RelayHealth, nowMs: Long): RelayHealth =
    if (relayHealth is RelayHealth.Ok && !ContactReachability.selfRelayHealthy(relayHealth, nowMs)) {
        RelayHealth.Failing(relayHealth.lastSyncMs)
    } else {
        relayHealth
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
    val contactLastSeen by MeshConnectivityStatus.contactLastSeen.collectAsState()
    val presenceLastSeen by MeshConnectivityStatus.presenceLastSeen.collectAsState()
    val connectivityNowMs = rememberConnectivityNowMs()
    var transientMeshStatus by remember { mutableStateOf<String?>(null) }
    var ownDisplayName by remember { mutableStateOf(ProfileStore.loadDisplayName(context)) }
    var ownAvatarPath by remember { mutableStateOf(ProfilePhotoStore.loadAvatarPath(context)) }
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

    val activity = context as? ComponentActivity
    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { grants ->
        permissionRefreshToken += 1
        if (grants.values.all { it }) {
            if (isIgnoringBatteryOptimizations(context)) {
                startMesh(context)
            } else {
                transientMeshStatus = "Requesting background permission…"
                batteryOptimizationLauncher.launch(batteryOptimizationIntent(context))
            }
        } else {
            transientMeshStatus = "Permissions denied — mesh cannot run"
            // If the system will not show the dialog again, send the user to
            // App info so they can flip Nearby / Notifications manually.
            val permanentlyDenied = activity != null && MeshService.requiredPermissions().any { perm ->
                grants[perm] == false &&
                    !ActivityCompat.shouldShowRequestPermissionRationale(activity, perm)
            }
            if (permanentlyDenied) {
                Toast.makeText(
                    context,
                    "Enable Nearby devices (and notifications) in App permissions",
                    Toast.LENGTH_LONG,
                ).show()
                openAppPermissionSettings(context)
            }
        }
    }

    LaunchedEffect(hasPermissions) {
        if (hasPermissions && runtimeStatus == MeshRuntimeState.STOPPED) {
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
    
    fun reloadSummaries() {
        val contacts = store.listContacts()
        val direct = contacts.map { c ->
            val messages = store.messagesForChat(c.userId)
            val readThrough = store.receiptThrough(c.userId, identity.userId, RECEIPT_TYPE_READ)
            val deliveredThrough = store.receiptThrough(c.userId, identity.userId, RECEIPT_TYPE_DELIVERED)
            // Unread uses our local read watermark of the peer's stream.
            val localReadThrough = store.outgoingReceiptThrough(c.userId, c.userId, RECEIPT_TYPE_READ)
            val unreadCount = ChatListLogic.computeUnread(messages, identity.userId, localReadThrough)
            ChatSummary(
                chatId = c.userId,
                title = c.name,
                isGroup = false,
                contact = c,
                lastMessage = ChatListLogic.lastVisibleMessage(messages),
                unreadCount = unreadCount,
                ownDeliveredThrough = deliveredThrough,
                ownReadThrough = readThrough,
                avatarBytes = store.contactAvatar(c.userId),
            )
        }
        val groups = store.listGroups().map { g ->
            val messages = store.messagesForChat(g.id)
            val unreadCount = ChatListLogic.computeGroupUnread(messages, identity.userId) { senderId ->
                store.outgoingReceiptThrough(g.id, senderId, RECEIPT_TYPE_READ)
            }
            ChatSummary(
                chatId = g.id,
                title = g.name,
                isGroup = true,
                group = g,
                lastMessage = ChatListLogic.lastVisibleMessage(messages),
                unreadCount = unreadCount,
                ownDeliveredThrough = 0uL,
                ownReadThrough = 0uL,
            )
        }
        summaries = (direct + groups).sortedByDescending { it.lastMessage?.timestamp ?: 0L }
    }

    LaunchedEffect(Unit) {
        reloadSummaries()
        com.cruisemesh.app.chat.ChatEvents.changes.collect {
            reloadSummaries()
        }
    }

    // Refresh when navigating back to home
    DisposableEffect(navController) {
        val listener = NavController.OnDestinationChangedListener { _, dest, _ ->
            if (dest.route == "home") {
                ownDisplayName = ProfileStore.loadDisplayName(context)
                ownAvatarPath = ProfilePhotoStore.loadAvatarPath(context)
                permissionRefreshToken += 1
                bluetoothEnabled = isBluetoothRadioEnabled(context)
                reloadSummaries()
            }
        }
        navController.addOnDestinationChangedListener(listener)
        onDispose { navController.removeOnDestinationChangedListener(listener) }
    }

    val displaySummaries = remember(summaries, nearbyPeerIds, relayHealth, contactLastSeen, presenceLastSeen, connectivityNowMs) {
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
                ),
            )
        }
    }
    val displayRelayHealth = remember(relayHealth, connectivityNowMs) {
        freshRelayHealthForDisplay(relayHealth, connectivityNowMs)
    }
    val pillStatus = remember(runtimeStatus, nearbyPeerIds, displayRelayHealth) {
        MeshStatusTextLogic.build(runtimeStatus, nearbyPeerIds.size, displayRelayHealth)
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
            if (summary.isGroup) {
                store.deleteGroup(summary.chatId)
            } else {
                store.deleteContact(summary.chatId)
                FriendDirectorySender.queueToAllContacts(context, store, identity)
            }
            reloadSummaries()
        },
        onNewChatClick = { navController.navigate("contacts") },
        onProfileClick = { navController.navigate("profile") },
        onMeshStatusClick = { showMeshStatusLegend = true },
        meshStatusText = transientMeshStatus ?: pillStatus.text,
        meshStatusDotColor = if (transientMeshStatus != null) null else pillDotColor,
        connectivityWarning = when {
            !hasPermissions -> ConnectivityWarning(
                title = "Permissions required — mesh is off",
                body = "Without Nearby devices (and notification) access, CruiseMesh cannot scan, connect, send, or receive messages. The app will not work as designed until you grant them.",
                actionLabel = "Enable permissions",
                severity = ConnectivityWarningSeverity.Blocking,
            )
            !bluetoothEnabled -> ConnectivityWarning(
                title = "Bluetooth is off",
                body = "CruiseMesh needs Bluetooth on to find nearby phones and deliver messages. Turn it on to use the mesh.",
                actionLabel = "Open Bluetooth settings",
                severity = ConnectivityWarningSeverity.Blocking,
            )
            bluetoothAudioConnected && !bluetoothAudioWarningDismissed && !hideBluetoothAudioWarning -> ConnectivityWarning(
                title = "Bluetooth audio connected",
                body = "The mesh is still running, but wireless earbuds/speakers can slow nearby delivery. Watch for glitches.",
                actionLabel = "Dismiss",
                secondaryActionLabel = "Don't show this again",
                severity = ConnectivityWarningSeverity.Caution,
            )
            else -> null
        },
        onConnectivityWarningClick = {
            when {
                !hasPermissions -> permissionLauncher.launch(MeshService.requiredPermissions())
                !bluetoothEnabled -> context.startActivity(Intent(Settings.ACTION_BLUETOOTH_SETTINGS))
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
            onStartMesh = { permissionLauncher.launch(MeshService.requiredPermissions()) },
            onDismiss = { showMeshStatusLegend = false },
        )
    }
}

@Composable
private fun ProfileRoute(identity: Identity, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val displayId = remember(identity) { formatUserId(identity.userId) }
    val fingerprint = remember(identity) { fingerprintWords(identity.userId) }
    val runtimeStatus by MeshRuntimeStatus.state.collectAsState()
    var transientMeshStatus by remember { mutableStateOf<String?>(null) }

    LaunchedEffect(runtimeStatus) {
        if (runtimeStatus != MeshRuntimeState.STOPPED) {
            transientMeshStatus = null
        }
    }

    val batteryOptimizationLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) {
        startMesh(context)
    }

    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { grants ->
        if (grants.values.all { it }) {
            if (isIgnoringBatteryOptimizations(context)) {
                startMesh(context)
            } else {
                transientMeshStatus = "Requesting background permission…"
                batteryOptimizationLauncher.launch(batteryOptimizationIntent(context))
            }
        } else {
            transientMeshStatus = "Permissions denied — mesh cannot run"
            val activity = context as? ComponentActivity
            val permanentlyDenied = activity != null && MeshService.requiredPermissions().any { perm ->
                grants[perm] == false &&
                    !ActivityCompat.shouldShowRequestPermissionRationale(activity, perm)
            }
            if (permanentlyDenied) {
                Toast.makeText(
                    context,
                    "Enable Nearby devices (and notifications) in App permissions",
                    Toast.LENGTH_LONG,
                ).show()
                openAppPermissionSettings(context)
            }
        }
    }

    ProfileScreen(
        profileUserId = identity.userId,
        displayId = displayId,
        fingerprint = fingerprint,
        meshStatus = transientMeshStatus ?: runtimeStatus.label,
        onStartMesh = if (runtimeStatus == MeshRuntimeState.STOPPED) {
            { permissionLauncher.launch(MeshService.requiredPermissions()) }
        } else null,
        onShowMyQr = { navController.navigate("myQr") },
        onBackUp = { navController.navigate("backup") },
        onProfileChanged = { epoch ->
            ProfileSyncSender.queueToAllContacts(context, store, identity, epoch)
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
        onBack = { navController.popBackStack() }
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
                val contact = RelayImport.reconcileOnImport(context, store, scanned)
                store.upsertContact(contact)
                store.upsertContactProvenance(
                    ContactProvenance(contact.userId, 0u, null, System.currentTimeMillis()),
                )
                store.removeFriendSuggestion(contact.userId)
                val delivery = FriendRequestSender.queueForScannedContact(context, store, identity, contact)
                ProfileSyncSender.queueToContact(
                    context,
                    store,
                    identity,
                    contact,
                    ProfileStore.loadOwnAvatarEpoch(context),
                )
                FriendDirectorySender.queueToAllContacts(context, store, identity)
                FriendAddedOutcome(contact, delivery, RelayConfigStore.load(context) != null)
            },
            onSayHi = { openFriendChat(navController, it) },
            onDone = { returnToContacts(navController) },
            onBack = { navController.popBackStack() },
        )
    } else {
        Scaffold { innerPadding ->
            Column(
                modifier = Modifier.fillMaxSize().padding(innerPadding).padding(24.dp),
                verticalArrangement = Arrangement.Center,
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                Text("Camera permission is needed to scan a friend card.")
                Button(onClick = { navController.popBackStack() }, modifier = Modifier.padding(top = 16.dp)) {
                    Text("Back")
                }
            }
        }
    }
}

@Composable
private fun AddFriendRoute(identity: Identity, navController: NavHostController, initialToken: String? = null) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }

    AddFriendScreen(
        onScanClick = { navController.navigate("scan") },
        onImportText = { text ->
            try {
                val card = parseFriendText(extractFriendToken(text))
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
                    val collision = store.listContacts().firstOrNull {
                        it.name.equals(candidate.name, ignoreCase = true) &&
                            !it.userId.contentEquals(candidate.userId)
                    }
                    val warning = collision?.let {
                        "You already have a ${candidate.name}; this card has different security keys. Compare the fingerprint words before adding it."
                    }
                    ImportFriendResult.Preview(FriendPreview(candidate, warning))
                }
            } catch (_: Exception) {
                if (text.contains("CMFRIEND1:")) {
                    ImportFriendResult.Error("That looks like a friend card but part of it is missing. Copy the whole message and try again.")
                } else {
                    ImportFriendResult.Error("Not a CruiseMesh friend card")
                }
            }
        },
        onConfirmContact = { candidate ->
            val contact = RelayImport.reconcileOnImport(context, store, candidate)
            store.upsertContact(contact)
            store.upsertContactProvenance(
                ContactProvenance(contact.userId, 0u, null, System.currentTimeMillis()),
            )
            store.removeFriendSuggestion(contact.userId)
            val delivery = FriendRequestSender.queueForScannedContact(context, store, identity, contact)
            ProfileSyncSender.queueToContact(
                context,
                store,
                identity,
                contact,
                ProfileStore.loadOwnAvatarEpoch(context),
            )
            FriendDirectorySender.queueToAllContacts(context, store, identity)
            FriendAddedOutcome(contact, delivery, RelayConfigStore.load(context) != null)
        },
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
        onBack = { navController.popBackStack() },
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
    fun reloadContacts() {
        contacts = store.listContacts()
        avatars = contacts.associate { formatUserId(it.userId) to store.contactAvatar(it.userId) }
            .filterValues { it != null }
            .mapValues { it.value!! }
    }
    
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
        onContactClick = { contact -> navController.navigate("chat/${UserIdHex.encode(contact.userId)}") },
        onContactDelete = { contact ->
            store.deleteContact(contact.userId)
            FriendDirectorySender.queueToAllContacts(context, store, identity)
            reloadContacts()
        },
        onAddFriendClick = { navController.navigate("addFriend") },
        onMyCardClick = { navController.navigate("myQr") },
        onNewGroupClick = { navController.navigate("newGroup") },
        onBack = { navController.popBackStack() },
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
        onBack = { navController.popBackStack() },
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
        val relayHealth by MeshConnectivityStatus.relay.collectAsState()
        val contactLastSeen by MeshConnectivityStatus.contactLastSeen.collectAsState()
        val presenceLastSeen by MeshConnectivityStatus.presenceLastSeen.collectAsState()
        val connectivityNowMs = rememberConnectivityNowMs()
        val reachability = remember(contact.userId, nearbyPeerIds, relayHealth, contactLastSeen, presenceLastSeen, connectivityNowMs) {
            reachabilityLevelForUserId(contact.userId, nearbyPeerIds, relayHealth, contactLastSeen, presenceLastSeen, connectivityNowMs)
        }
        val reachabilityStatusText = remember(reachability, contactLastSeen, presenceLastSeen, connectivityNowMs) {
            val hex = UserIdHex.encode(contact.userId)
            ContactReachability.chatHeaderCopy(
                reachability,
                listOfNotNull(contactLastSeen[hex], presenceLastSeen[hex]).maxOrNull(),
                connectivityNowMs,
            )
        }
        val reachabilityDetailsText = remember(reachability, contactLastSeen, presenceLastSeen, connectivityNowMs) {
            val hex = UserIdHex.encode(contact.userId)
            ContactReachability.contactDetailsCopy(
                reachability,
                listOfNotNull(contactLastSeen[hex], presenceLastSeen[hex]).maxOrNull(),
                presenceLastSeen[hex],
                connectivityNowMs,
            )
        }
        ChatScreen(
            contact = contact,
            ownUserId = identity.userId,
            sender = sender,
            store = store,
            onBack = { navController.popBackStack() },
            onDeleteContact = {
                store.deleteContact(contact.userId)
                FriendDirectorySender.queueToAllContacts(context, store, identity)
                navController.popBackStack()
            },
            reachability = reachability,
            reachabilityStatusText = reachabilityStatusText,
            reachabilityDetailsText = reachabilityDetailsText,
        )
    } else {
        LaunchedEffect(Unit) { navController.popBackStack() }
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
                        // Local read watermarks for every other member (no wire receipts
                        // yet). highestLamport (plain MAX), not highestContiguousLamport:
                        // this is a per-member peer-stream watermark, and the contiguous
                        // count stalls at 0 once a member's stream legitimately starts
                        // above lamport 1 (post chat-history-wipe ratchet), which would
                        // strand the unread badge for that member forever.
                        for (memberId in group.memberUserIds) {
                            if (memberId.contentEquals(identity.userId)) continue
                            val through = store.highestLamport(group.id, memberId)
                            if (through > 0uL) {
                                store.recordOutgoingReceipt(group.id, memberId, RECEIPT_TYPE_READ, through)
                            }
                        }
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
        val contactLastSeen by MeshConnectivityStatus.contactLastSeen.collectAsState()
        val presenceLastSeen by MeshConnectivityStatus.presenceLastSeen.collectAsState()
        val connectivityNowMs = rememberConnectivityNowMs()
        val reachableMemberCount = remember(group, nearbyPeerIds, relayHealth, contactLastSeen, presenceLastSeen, connectivityNowMs) {
            groupReachableCounts(group, identity.userId, nearbyPeerIds, relayHealth, contactLastSeen, presenceLastSeen, connectivityNowMs).first
        }
        GroupChatScreen(
            group = group,
            ownUserId = identity.userId,
            contactsByUserId = contactsByUserId,
            sender = sender,
            store = store,
            onBack = { navController.popBackStack() },
            onDeleteGroup = {
                store.deleteGroup(group.id)
                navController.popBackStack()
            },
            reachableMemberCount = reachableMemberCount,
        )
    } else {
        LaunchedEffect(Unit) { navController.popBackStack() }
    }
}

private fun startMesh(context: Context) {
    MeshRuntimeStatus.markStarting()
    try {
        ContextCompat.startForegroundService(context, Intent(context, MeshService::class.java))
    } catch (_: RuntimeException) {
        MeshRuntimeStatus.markStopped()
    }
}

private fun hasMeshPermissions(context: Context): Boolean =
    MeshService.requiredPermissions().all {
        ContextCompat.checkSelfPermission(context, it) == PackageManager.PERMISSION_GRANTED
    }

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
