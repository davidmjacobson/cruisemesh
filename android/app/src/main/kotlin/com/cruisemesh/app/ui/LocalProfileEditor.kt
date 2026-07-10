package com.cruisemesh.app.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp

@Composable
fun LocalProfileEditor(
    userId: ByteArray,
    displayId: String,
    displayName: String,
    avatarPath: String?,
    onDisplayNameChange: (String) -> Unit,
    onTakePhoto: () -> Unit,
    onChoosePhoto: () -> Unit,
    onRemovePhoto: (() -> Unit)?,
    modifier: Modifier = Modifier,
    helperText: String? = null,
) {
    Column(
        modifier = modifier.fillMaxWidth(),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        AvatarBadge(
            userId = userId,
            name = displayName,
            displayId = displayId,
            size = 108.dp,
            photoPath = avatarPath,
        )

        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 16.dp),
            horizontalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            OutlinedButton(
                onClick = onTakePhoto,
                modifier = Modifier.weight(1f),
            ) {
                Text("Take photo")
            }
            Button(
                onClick = onChoosePhoto,
                modifier = Modifier.weight(1f),
            ) {
                Text("Choose photo")
            }
        }

        if (onRemovePhoto != null && avatarPath != null) {
            TextButton(onClick = onRemovePhoto, modifier = Modifier.padding(top = 4.dp)) {
                Text("Remove photo")
            }
        }

        OutlinedTextField(
            value = displayName,
            onValueChange = onDisplayNameChange,
            label = { Text("Display name") },
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = 12.dp),
            singleLine = true,
        )

        helperText?.let {
            Text(
                text = it,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
                modifier = Modifier.padding(top = 10.dp),
            )
        }
    }
}
