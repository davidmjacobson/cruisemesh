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
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
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
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import com.cruisemesh.app.chat.ChatScreen
import com.cruisemesh.app.chat.RealMeshSender
import com.cruisemesh.app.chat.UserIdHex
import com.cruisemesh.app.friending.ContactsScreen
import com.cruisemesh.app.friending.MyQrScreen
import com.cruisemesh.app.friending.ScanScreen
import com.cruisemesh.app.identity.IdentityStore
import com.cruisemesh.app.mesh.ChatViewEvents
import com.cruisemesh.app.mesh.MeshService
import com.cruisemesh.app.notify.ChatVisibility
import com.cruisemesh.app.notify.MessageNotifier
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.formatUserId
import uniffi.cruisemesh_core.generateIdentity

class MainActivity : ComponentActivity() {

    /**
     * Chat requested by a message-notification tap ([MessageNotifier]'s
     * PendingIntent), as the [UserIdHex]-encoded userId, or null when there is
     * nothing pending. Covers both launch paths: cold start reads the extra
     * off the launch intent here in [onCreate]; warm start (activity already
     * on top, `launchMode="singleTop"` in the manifest) receives it via
     * [onNewIntent]. Consumed (navigated to, then nulled) by a
     * `LaunchedEffect` next to the NavHost in [CruiseMeshApp].
     */
    private val pendingChatUserIdHex = mutableStateOf<String?>(null)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        pendingChatUserIdHex.value = intent?.getStringExtra(MessageNotifier.EXTRA_CHAT_USER_ID_HEX)
        setContent {
            MaterialTheme {
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

/**
 * Milestone-1 proof of life: load (or generate and persist) an identity via
 * the Rust core (JNA/UniFFI across the JNI boundary) and render its display
 * ID + fingerprint. Persistence is Android Keystore-backed (see
 * [IdentityStore]), so the UserID stays stable across app restarts instead
 * of regenerating every launch. The "Start mesh" button requests BLE runtime
 * permissions, then (if not already exempted) the Doze/App Standby
 * battery-optimization exemption bitchat also requests, before launching
 * MeshService (DESIGN.md §5.2), the Milestone 0 transport skeleton.
 */
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

    NavHost(navController = navController, startDestination = "identity") {
        composable("identity") { IdentityRoute(identity, navController) }
        composable("myQr") { MyQrScreen(identity, onBack = { navController.popBackStack() }) }
        composable("scan") { ScanRoute(navController) }
        composable("contacts") { ContactsRoute(navController) }
        composable("chat/{userIdHex}") { backStackEntry ->
            val userIdHex = backStackEntry.arguments?.getString("userIdHex").orEmpty()
            ChatRoute(identity, userIdHex, navController)
        }
    }

    // Notification-tap deep link (see MainActivity.pendingChatUserIdHex):
    // navigate once the NavHost above is composed, then clear so the same
    // tap isn't re-navigated on recomposition or a later config change.
    LaunchedEffect(pendingChatUserIdHex) {
        if (pendingChatUserIdHex != null) {
            navController.navigate("chat/$pendingChatUserIdHex")
            onPendingChatConsumed()
        }
    }
}

@Composable
private fun IdentityRoute(identity: Identity, navController: NavHostController) {
    val context = LocalContext.current
    val displayId = remember(identity) { formatUserId(identity.userId) }
    val fingerprint = remember(identity) { fingerprintWords(identity.userId) }
    var meshStatus by remember { mutableStateOf("Mesh stopped") }

    val batteryOptimizationLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) {
        // Ignored either way: declining the system dialog just means less
        // reliable backgrounded sync, not a hard failure, so mesh starts regardless.
        startMesh(context) { meshStatus = it }
    }

    val permissionLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { grants ->
        if (grants.values.all { it }) {
            if (isIgnoringBatteryOptimizations(context)) {
                startMesh(context) { meshStatus = it }
            } else {
                meshStatus = "Requesting background permission…"
                batteryOptimizationLauncher.launch(batteryOptimizationIntent(context))
            }
        } else {
            meshStatus = "BLE permissions denied"
        }
    }

    IdentityScreen(
        displayId = displayId,
        fingerprint = fingerprint,
        meshStatus = meshStatus,
        onStartMesh = { permissionLauncher.launch(MeshService.requiredPermissions()) },
        onShowMyQr = { navController.navigate("myQr") },
        onScanFriend = { navController.navigate("scan") },
        onShowContacts = { navController.navigate("contacts") },
    )
}

/** Requests the CAMERA permission (declared in the manifest, never requested until now) before showing [ScanScreen]. */
@Composable
private fun ScanRoute(navController: NavHostController) {
    val context = LocalContext.current
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
            onContactAdded = { contact -> AppStore.get(context).upsertContact(contact) },
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
    val contacts = remember { AppStore.get(context).listContacts() }
    ContactsScreen(
        contacts = contacts,
        onContactClick = { contact -> navController.navigate("chat/${UserIdHex.encode(contact.userId)}") },
        onBack = { navController.popBackStack() },
    )
}

/**
 * Resolves the `userIdHex` route arg back into a [uniffi.cruisemesh_core.Contact] (via
 * [AppStore]) and hosts [ChatScreen]. [RealMeshSender] is the real
 * [com.cruisemesh.app.chat.MeshSender] -- see its KDoc for why the UI never depends on it directly.
 */
@Composable
private fun ChatRoute(identity: Identity, userIdHex: String, navController: NavHostController) {
    val context = LocalContext.current
    val store = remember { AppStore.get(context) }
    val contact = remember(userIdHex) { store.getContact(UserIdHex.decode(userIdHex)) }

    if (contact != null) {
        // Registers this chat as "on screen" for the whole time it is
        // composed -- consumed by notification suppression and read
        // receipts; see ChatVisibility's KDoc. clearVisible (not an
        // unconditional clear) because during a chat->chat transition the
        // incoming route's setVisible can run before this route's onDispose.
        // ChatViewEvents.onChatViewed fires once per composition (not on
        // dispose) so MeshService can send a read receipt for whatever's
        // already stored from this contact -- see ChatViewEvents' KDoc.
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
        )
    } else {
        // Unknown contact (e.g. a stale/malformed route arg) -- nothing sensible to
        // render, so just bounce back rather than crash.
        LaunchedEffect(Unit) { navController.popBackStack() }
    }
}

private fun startMesh(context: Context, setStatus: (String) -> Unit) {
    ContextCompat.startForegroundService(context, Intent(context, MeshService::class.java))
    setStatus("Mesh starting…")
}

private fun isIgnoringBatteryOptimizations(context: Context): Boolean {
    val powerManager = context.getSystemService(Context.POWER_SERVICE) as PowerManager
    return powerManager.isIgnoringBatteryOptimizations(context.packageName)
}

@SuppressLint("BatteryLife")
private fun batteryOptimizationIntent(context: Context): Intent =
    Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:${context.packageName}"))

@Composable
private fun IdentityScreen(
    displayId: String,
    fingerprint: List<String>,
    meshStatus: String = "",
    onStartMesh: (() -> Unit)? = null,
    onShowMyQr: (() -> Unit)? = null,
    onScanFriend: (() -> Unit)? = null,
    onShowContacts: (() -> Unit)? = null,
) {
    Scaffold { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .padding(24.dp),
            verticalArrangement = Arrangement.Center,
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text("CruiseMesh", style = MaterialTheme.typography.headlineMedium)
            Text(
                "Your device identity",
                style = MaterialTheme.typography.bodyMedium,
            )
            Text(
                displayId,
                style = MaterialTheme.typography.titleMedium.copy(fontFamily = FontFamily.Monospace),
                modifier = Modifier.padding(top = 16.dp),
            )
            Text(
                fingerprint.joinToString(" "),
                style = MaterialTheme.typography.bodyLarge,
                modifier = Modifier.padding(top = 8.dp),
            )
            if (onStartMesh != null) {
                Button(onClick = onStartMesh, modifier = Modifier.padding(top = 24.dp)) {
                    Text("Start mesh")
                }
                Text(meshStatus, modifier = Modifier.padding(top = 8.dp))
            }
            if (onShowMyQr != null) {
                Button(onClick = onShowMyQr, modifier = Modifier.padding(top = 24.dp)) {
                    Text("Show my friend card")
                }
            }
            if (onScanFriend != null) {
                Button(onClick = onScanFriend, modifier = Modifier.padding(top = 8.dp)) {
                    Text("Add a friend")
                }
            }
            if (onShowContacts != null) {
                Button(onClick = onShowContacts, modifier = Modifier.padding(top = 8.dp)) {
                    Text("Contacts")
                }
            }
        }
    }
}

@Preview(showBackground = true)
@Composable
fun CruiseMeshAppPreview() {
    // Static data — Compose preview rendering doesn't load the JNI native lib.
    MaterialTheme {
        IdentityScreen("CM-K7QX-9M2P-3F8J-QRTZ-AB", listOf("anchor", "beacon", "coral", "dock"))
    }
}
