package com.cruisemesh.app.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.chat.MessageComposer

@Preview(name = "terms_compact", widthDp = 360, heightDp = 640, showBackground = true)
@Preview(name = "terms_compact_large_font", widthDp = 360, heightDp = 640, fontScale = 1.3f, showBackground = true)
@Composable
fun TermsScreenshot() {
    CruiseMeshTheme {
        TermsAcceptanceScreen(onAccept = {})
    }
}

@Preview(name = "onboarding_compact", widthDp = 360, heightDp = 640, showBackground = true)
@Preview(name = "onboarding_compact_large_font", widthDp = 360, heightDp = 640, fontScale = 1.3f, showBackground = true)
@Composable
fun OnboardingScreenshot() {
    CruiseMeshTheme {
        OnboardingScreen(
            userId = ByteArray(32) { 1 },
            displayId = "CM-K7QX-9M2P-3F8J-QRTZ-AB",
            displayName = "",
            avatarPath = null,
            meshPermissionsGranted = false,
            batteryExemptionGranted = false,
            onDisplayNameChange = {},
            onTakePhoto = {},
            onChoosePhoto = {},
            onRemovePhoto = {},
            onRequestMeshPermissions = {},
            onRequestBatteryExemption = {},
            onRestore = {},
            onComplete = {},
        )
    }
}

@Preview(name = "composer_empty", widthDp = 360, heightDp = 120, showBackground = true)
@Composable
fun EmptyComposerScreenshot() {
    ComposerFrame(draft = "", hasPendingAttachment = false)
}

@Preview(name = "composer_caption", widthDp = 360, heightDp = 160, fontScale = 1.3f, showBackground = true)
@Composable
fun CaptionComposerScreenshot() {
    ComposerFrame(draft = "Meet by the pool after dinner", hasPendingAttachment = true)
}

@Composable
private fun ComposerFrame(draft: String, hasPendingAttachment: Boolean) {
    CruiseMeshTheme {
        Column(modifier = Modifier.fillMaxSize().padding(horizontal = 12.dp, vertical = 8.dp)) {
            MessageComposer(
                draft = draft,
                onDraftChange = {},
                onSend = {},
                hasPendingAttachment = hasPendingAttachment,
                ownBubbleColor = Color(0xFF236A5B),
                onPickGallery = {},
                onPickCamera = {},
                onStartVoice = { false },
                onStopVoice = {},
                onCancelVoice = {},
            )
        }
    }
}
