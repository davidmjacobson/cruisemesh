package com.cruisemesh.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.identity.ProfileStore

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProfileScreen(
    displayId: String,
    fingerprint: List<String>,
    meshStatus: String,
    onStartMesh: (() -> Unit)?,
    onShowMyQr: () -> Unit,
    onBack: () -> Unit
) {
    val context = LocalContext.current
    var displayName by remember { mutableStateOf(ProfileStore.loadDisplayName(context)) }

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
                .padding(24.dp),
            horizontalAlignment = Alignment.CenterHorizontally
        ) {
            OutlinedTextField(
                value = displayName,
                onValueChange = { 
                    displayName = it
                    ProfileStore.saveDisplayName(context, it)
                },
                label = { Text("Display Name") },
                modifier = Modifier.fillMaxWidth()
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
@Composable
fun ProfileScreenPreview() {
    MaterialTheme {
        ProfileScreen(
            displayId = "CM-K7QX-9M2P-3F8J-QRTZ-AB",
            fingerprint = listOf("anchor", "beacon", "coral", "dock"),
            meshStatus = "Mesh off",
            onStartMesh = {},
            onShowMyQr = {},
            onBack = {}
        )
    }
}
