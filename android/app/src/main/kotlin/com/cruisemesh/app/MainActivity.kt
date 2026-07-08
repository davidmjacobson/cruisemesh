package com.cruisemesh.app

import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
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
import com.cruisemesh.app.mesh.MeshService
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.formatUserId
import uniffi.cruisemesh_core.generateIdentity

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        setContent {
            MaterialTheme {
                Surface(modifier = Modifier.fillMaxSize()) {
                    CruiseMeshApp()
                }
            }
        }
    }
}

/**
 * Milestone-1 proof of life: generate an identity via the Rust core (JNA/UniFFI
 * across the JNI boundary) and render its display ID + fingerprint. This
 * identity is not yet persisted — that lands with on-device secure storage.
 * The "Start mesh" button requests BLE runtime permissions, then (if not
 * already exempted) the Doze/App Standby battery-optimization exemption
 * bitchat also requests, before launching MeshService (DESIGN.md §5.2), the
 * Milestone 0 transport skeleton.
 */
@Composable
fun CruiseMeshApp() {
    val context = LocalContext.current
    val identity = remember { generateIdentity() }
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
    )
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
