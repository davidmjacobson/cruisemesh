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
import com.cruisemesh.app.chat.ChatScreen
import com.cruisemesh.app.chat.RealMeshSender
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.friending.ContactsScreen
import com.cruisemesh.app.friending.FriendRequestSender
import com.cruisemesh.app.friending.MyQrScreen
import com.cruisemesh.app.friending.ScanScreen
import com.cruisemesh.app.identity.IdentityStore
import com.cruisemesh.app.identity.ProfileStore
import com.cruisemesh.app.mesh.ChatViewEvents
import com.cruisemesh.app.mesh.MeshRuntimeState
import com.cruisemesh.app.mesh.MeshRuntimeStatus
import com.cruisemesh.app.mesh.MeshService
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.notify.MessageNotifier
import com.cruisemesh.app.ui.CruiseMeshTheme
import com.cruisemesh.app.ui.ChatListScreen
import com.cruisemesh.app.ui.ChatSummary
import com.cruisemesh.app.ui.ProfileScreen
import com.cruisemesh.app.ui.ChatListLogic
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.formatUserId
import uniffi.cruisemesh_core.generateIdentity

private const val RECEIPT_TYPE_DELIVERED: kotlin.UByte = 1u
private const val RECEIPT_TYPE_READ: kotlin.UByte = 2u

class MainActivity : ComponentActivity() {

    private val pendingChatUserIdHex = mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        pendingChatUserIdHex.value = intent?.getStringExtra(MessageNotifier.EXTRA_CHAT_USER_ID_HEX)
        setContent {
            CruiseMeshTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    CruiseMeshApp(
                        pendingChatUserIdHex = pendingChatUserIdHex.value,
                        onPendingChatConsumed = { pendingChatUserIdHex.value = null },
                    )
                }
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        intent.getStringExtra(MessageNotifier.EXTRA_CHAT_USER_ID_HEX)?.let {
            pendingChatUserIdHex.value = it
        }
    }
}

@Composable
fun CruiseMeshApp(
    pendingChatUserIdHex: String? = null,
    onPendingChatConsumed: () -> Unit = {},
) {
    val context = LocalContext.current
    val identity = remember {
        IdentityStore.load(context) ?: generateIdentity().also { IdentityStore.save(context, it) }
    }
    val navController = rememberNavController()

    NavHost(navController = navController, startDestination = "home") {
        composable("home") { HomeRoute(identity, navController) }
        composable("profile") { ProfileRoute(identity, navController) }
        composable("myQr") { MyQrScreen(identity, onBack = { navController.popBackStack() }) }
        composable("scan") { ScanRoute(identity, navController) }
        composable("contacts") { ContactsRoute(navController) }
        composable("chat/{userIdHex}") { backStackEntry ->
            val userIdHex = backStackEntry.arguments?.getString("userIdHex").orEmpty()
            ChatRoute(identity, userIdHex, navController)
        }
    }

    LaunchedEffect(pendingChatUserIdHex) {
        if (pendingChatUserIdHex != null) {
            navController.navigate("chat/$pendingChatUserIdHex")
            onPendingChatConsumed()
        }
    }
}

@Composable
private fun HomeRoute(identity: Identity, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val runtimeStatus by MeshRuntimeStatus.state.collectAsState()
    var transientMeshStatus by remember { mutableStateOf<String?>(null) }
    var ownDisplayName by remember { mutableStateOf(ProfileStore.loadDisplayName(context)) }
    
    val hasPermissions = remember(context) {
        MeshService.requiredPermissions().all {
            ContextCompat.checkSelfPermission(context, it) == PackageManager.PERMISSION_GRANTED
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
            transientMeshStatus = "BLE permissions denied"
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

    var summaries by remember { mutableStateOf(emptyList<ChatSummary>()) }
    
    fun reloadSummaries() {
        val contacts = store.listContacts()
        summaries = contacts.map { c ->
            val messages = store.messagesForChat(c.userId)
            val readThrough = store.receiptThrough(c.userId, identity.userId, RECEIPT_TYPE_READ)
            val deliveredThrough = store.receiptThrough(c.userId, identity.userId, RECEIPT_TYPE_DELIVERED)
            val unreadCount = ChatListLogic.computeUnread(messages, identity.userId, readThrough)
            ChatSummary(
                contact = c,
                lastMessage = messages.maxByOrNull { it.timestamp },
                unreadCount = unreadCount,
                ownDeliveredThrough = deliveredThrough,
                ownReadThrough = readThrough
            )
        }.sortedByDescending { it.lastMessage?.timestamp ?: 0L }
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
                reloadSummaries()
            }
        }
        navController.addOnDestinationChangedListener(listener)
        onDispose { navController.removeOnDestinationChangedListener(listener) }
    }

    ChatListScreen(
        ownUserId = identity.userId,
        ownDisplayName = ownDisplayName,
        onChatClick = { contact -> navController.navigate("chat/${UserIdHex.encode(contact.userId)}") },
        onDeleteContact = { contact ->
            store.deleteContact(contact.userId)
            reloadSummaries()
        },
        onNewChatClick = { navController.navigate("contacts") },
        onProfileClick = { navController.navigate("profile") },
        onMeshStatusClick = {
            if (runtimeStatus == MeshRuntimeState.STOPPED) {
                permissionLauncher.launch(MeshService.requiredPermissions())
            } else {
                navController.navigate("profile")
            }
        },
        meshStatusText = transientMeshStatus ?: runtimeStatus.label,
        summaries = summaries
    )
}

@Composable
private fun ProfileRoute(identity: Identity, navController: NavHostController) {
    val context = LocalContext.current
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
            transientMeshStatus = "BLE permissions denied"
        }
    }

    ProfileScreen(
        displayId = displayId,
        fingerprint = fingerprint,
        meshStatus = transientMeshStatus ?: runtimeStatus.label,
        onStartMesh = if (runtimeStatus == MeshRuntimeState.STOPPED) {
            { permissionLauncher.launch(MeshService.requiredPermissions()) }
        } else null,
        onShowMyQr = { navController.navigate("myQr") },
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
            onContactAdded = { contact ->
                store.upsertContact(contact)
                FriendRequestSender.queueForScannedContact(context, store, identity, contact)
            },
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
private fun ContactsRoute(navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    var contacts by remember { mutableStateOf(store.listContacts()) }
    
    // Refresh list when resuming screen
    DisposableEffect(navController) {
        val listener = NavController.OnDestinationChangedListener { _, dest, _ ->
            if (dest.route == "contacts") {
                contacts = store.listContacts()
            }
        }
        navController.addOnDestinationChangedListener(listener)
        onDispose { navController.removeOnDestinationChangedListener(listener) }
    }

    ContactsScreen(
        contacts = contacts,
        onContactClick = { contact -> navController.navigate("chat/${UserIdHex.encode(contact.userId)}") },
        onContactDelete = { contact ->
            store.deleteContact(contact.userId)
            contacts = store.listContacts()
        },
        onAddFriendClick = { navController.navigate("scan") },
        onMyCardClick = { navController.navigate("myQr") },
        onBack = { navController.popBackStack() },
    )
}

@Composable
private fun ChatRoute(identity: Identity, userIdHex: String, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val contact = remember(userIdHex) { store.getContact(UserIdHex.decode(userIdHex)) }

    if (contact != null) {
        DisposableEffect(userIdHex) {
            ChatVisibility.setVisible(contact.userId)
            ChatViewEvents.onChatViewed(contact.userId)
            onDispose { ChatVisibility.clearVisible(contact.userId) }
        }
        val sender = remember { RealMeshSender(store, identity) }
        ChatScreen(
            contact = contact,
            ownUserId = identity.userId,
            sender = sender,
            store = store,
            onBack = { navController.popBackStack() },
            onDeleteContact = {
                store.deleteContact(contact.userId)
                navController.popBackStack()
            },
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

private fun isIgnoringBatteryOptimizations(context: Context): Boolean {
    val powerManager = context.getSystemService(Context.POWER_SERVICE) as PowerManager
    return powerManager.isIgnoringBatteryOptimizations(context.packageName)
}

@SuppressLint("BatteryLife")
private fun batteryOptimizationIntent(context: Context): Intent =
    Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:${context.packageName}"))
