package com.cruisemesh.app.ui

import android.graphics.BitmapFactory
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
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
    photoPath: String? = null,
) {
    val (avatarColor, initials) = remember(userId, name, displayId) {
        ChatListLogic.avatarHueAndInitials(userId, name, displayId)
    }
    val contentColor = remember(avatarColor) { ChatListLogic.avatarTextColor(avatarColor) }
    val contentDescription = remember(name, displayId) {
        ChatListLogic.avatarContentDescription(name, displayId)
    }
    val avatarBitmap = remember(photoPath) {
        photoPath?.let { path -> BitmapFactory.decodeFile(path)?.asImageBitmap() }
    }

    Surface(
        modifier = modifier
            .size(size)
            .semantics { this.contentDescription = contentDescription },
        shape = CircleShape,
        color = if (avatarBitmap == null) avatarColor else MaterialTheme.colorScheme.surface,
        contentColor = contentColor,
        border = BorderStroke(1.dp, MaterialTheme.colorScheme.onSurface.copy(alpha = 0.12f)),
    ) {
        Box(contentAlignment = Alignment.Center) {
            if (avatarBitmap != null) {
                Image(
                    bitmap = avatarBitmap,
                    contentDescription = null,
                    contentScale = ContentScale.Crop,
                    modifier = Modifier.fillMaxSize(),
                )
            } else {
                Text(
                    text = initials,
                    style = MaterialTheme.typography.titleMedium.copy(fontWeight = FontWeight.Bold),
                )
            }
        }
    }
}
