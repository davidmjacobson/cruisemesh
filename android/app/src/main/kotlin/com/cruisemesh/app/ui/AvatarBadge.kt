package com.cruisemesh.app.ui

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp

@Composable
fun AvatarBadge(
    userId: ByteArray,
    name: String,
    displayId: String,
    modifier: Modifier = Modifier,
    size: Dp = 48.dp,
) {
    val (avatarColor, initials) = remember(userId, name, displayId) {
        ChatListLogic.avatarHueAndInitials(userId, name, displayId)
    }
    val contentColor = remember(avatarColor) { ChatListLogic.avatarTextColor(avatarColor) }
    val contentDescription = remember(name, displayId) {
        ChatListLogic.avatarContentDescription(name, displayId)
    }

    Surface(
        modifier = modifier
            .size(size)
            .semantics { this.contentDescription = contentDescription },
        shape = CircleShape,
        color = avatarColor,
        contentColor = contentColor,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.onSurface.copy(alpha = 0.12f)),
    ) {
        Box(contentAlignment = Alignment.Center) {
            Text(
                text = initials,
                style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.Bold),
            )
        }
    }
}
