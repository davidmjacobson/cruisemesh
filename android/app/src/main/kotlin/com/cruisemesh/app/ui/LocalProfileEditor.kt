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
import androidx.compose.ui.res.stringResource
import com.cruisemesh.app.R

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
    nameError: String? = null,
    // Onboarding requires the name to proceed, and at large font scale the
    // slide's viewport ends before the avatar block — a trailing field leaves
    // the user staring at a disabled Next button with the reason off-screen.
    // The profile screen keeps the avatar-first layout.
    nameFirst: Boolean = false,
) {
    val nameField: @Composable (topPadding: Int) -> Unit = { topPadding ->
        OutlinedTextField(
            value = displayName,
            onValueChange = onDisplayNameChange,
            label = { Text(stringResource(R.string.ui_display_name)) },
            modifier = Modifier
                .fillMaxWidth()
                .padding(top = topPadding.dp),
            singleLine = true,
            isError = nameError != null,
            supportingText = nameError?.let { error -> { Text(error) } },
        )
    }
    Column(
        modifier = modifier.fillMaxWidth(),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        if (nameFirst) {
            nameField(0)
        }

        AvatarBadge(
            userId = userId,
            name = displayName,
            displayId = displayId,
            size = 108.dp,
            photoPath = avatarPath,
            modifier = if (nameFirst) Modifier.padding(top = 16.dp) else Modifier,
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
                Text(stringResource(R.string.ui_take_photo))
            }
            Button(
                onClick = onChoosePhoto,
                modifier = Modifier.weight(1f),
            ) {
                Text(stringResource(R.string.ui_choose_photo))
            }
        }

        if (onRemovePhoto != null && avatarPath != null) {
            TextButton(onClick = onRemovePhoto, modifier = Modifier.padding(top = 4.dp)) {
                Text(stringResource(R.string.ui_remove_photo))
            }
        }

        if (!nameFirst) {
            nameField(12)
        }

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
