package com.cruisemesh.app.ui

import android.graphics.BitmapFactory
import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
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
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.mesh.ContactReachability
import com.cruisemesh.app.mesh.ReachabilityLevel
import java.io.File

@Composable
fun AvatarBadge(
    userId: ByteArray,
    name: String,
    displayId: String,
    modifier: Modifier = Modifier,
    size: Dp = 48.dp,
    photoPath: String? = null,
    reachability: ReachabilityLevel? = null,
) {
    val (avatarColor, initials) = remember(userId, name, displayId) {
        ChatListLogic.avatarHueAndInitials(userId, name, displayId)
    }
    val contentColor = remember(avatarColor) { ChatListLogic.avatarTextColor(avatarColor) }
    val contentDescription = remember(name, displayId, reachability) {
        val base = ChatListLogic.avatarContentDescription(name, displayId)
        val suffix = reachability?.let { ContactReachability.contentDescriptionSuffix(it) }
        if (suffix != null) "$base. $suffix" else base
    }
    // The saved avatar always lives at the same path, so a replaced photo
    // needs the file's mtime in the key too or this keeps showing the stale
    // decoded bitmap from before the replacement.
    val avatarBitmap = remember(photoPath, photoPath?.let { File(it).lastModified() }) {
        photoPath?.let { path -> BitmapFactory.decodeFile(path)?.asImageBitmap() }
    }

    Box(modifier = modifier.size(size)) {
        Surface(
            modifier = Modifier
                .fillMaxSize()
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

        if (reachability != null && reachability != ReachabilityLevel.OFFLINE) {
            val badgeSize = size * 0.28f
            val borderColor = MaterialTheme.colorScheme.surface
            val palette = LocalReachabilityPalette.current
            val dotColor = when (reachability) {
                ReachabilityLevel.NEARBY -> palette.nearby
                ReachabilityLevel.ONLINE_RELAY -> palette.onlineRelay
                ReachabilityLevel.RECENT, ReachabilityLevel.MESH_CARRY -> palette.recent
                ReachabilityLevel.OFFLINE -> palette.neutral
            }
            val isHollow = reachability == ReachabilityLevel.MESH_CARRY
            Box(
                modifier = Modifier
                    .align(Alignment.BottomEnd)
                    .size(badgeSize)
                    .clip(CircleShape)
                    .background(if (isHollow) borderColor else dotColor)
                    .border(width = 2.dp, color = if (isHollow) dotColor else borderColor, shape = CircleShape),
            )
        }
    }
}
