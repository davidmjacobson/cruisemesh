package com.cruisemesh.app.devicelink

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R

/**
 * What this phone shows once its person has removed it (§10 step 5).
 *
 * Terminal, and in front of everything else, because every other screen would be
 * a lie: the chat list of a device that cannot send, receive or acknowledge
 * anything is a phone claiming to be part of a family it is no longer in. The
 * same reason the Terms gate sits where it does.
 *
 * Three sentences and no button, on purpose.
 *
 * * What happened, without asking the person to work out what "removed" means
 *   for their messages.
 * * That nothing is lost — their contacts, groups and messages are on the
 *   phone they still use, which is the first thing anybody wants to know.
 * * The way back, which is a real reinstall rather than a button: DL-4 makes a
 *   removed device id gone for good, so joining again means a fresh key on a
 *   fresh install, and offering "Set up again" here would offer something the
 *   core is right to refuse.
 *
 * Nothing on it can be operated, so nothing on it can put a removed device back
 * on the air by accident.
 */
@Composable
fun DeviceRemovedScreen() {
    Scaffold { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .verticalScroll(rememberScrollState())
                .padding(24.dp),
            verticalArrangement = Arrangement.Center,
        ) {
            Text(
                stringResource(R.string.ui_this_device_was_removed),
                style = MaterialTheme.typography.headlineLarge,
            )
            Text(
                stringResource(R.string.ui_this_device_was_removed_detail),
                style = MaterialTheme.typography.bodyLarge,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 16.dp),
            )
            Text(
                stringResource(R.string.ui_this_device_was_removed_elsewhere),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 12.dp),
            )
            Text(
                stringResource(R.string.ui_this_device_was_removed_set_up_again),
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(top = 12.dp),
            )
        }
    }
}
