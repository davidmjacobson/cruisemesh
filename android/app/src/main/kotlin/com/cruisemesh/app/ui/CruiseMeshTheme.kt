package com.cruisemesh.app.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color

/**
 * Semantic colors for [ReachabilityLevel]
 * dots/badges, kept separate from the Material3 [MaterialTheme.colorScheme]
 * tokens since they carry a fixed meaning (green=nearby, blue=relay,
 * amber=recent/mesh-carry) that must not shift with the app's primary hue.
 */
data class ReachabilityPalette(
    val nearby: Color,
    val onlineRelay: Color,
    val recent: Color,
    /** Also used for the MESH_CARRY hollow-ring stroke -- same hue as [recent], shape (not color) distinguishes them. */
    val meshCarry: Color,
    /** Neutral dot for mesh states that carry no reachability signal (STARTING/STOPPED/NO_BLUETOOTH). */
    val neutral: Color,
)

private val LightReachabilityPalette = ReachabilityPalette(
    nearby = Color(0xFF2E7D32),
    onlineRelay = Color(0xFF1565C0),
    recent = Color(0xFFF9A825),
    meshCarry = Color(0xFFF9A825),
    neutral = Color(0xFF8A9AA1),
)

private val DarkReachabilityPalette = ReachabilityPalette(
    nearby = Color(0xFF81C784),
    onlineRelay = Color(0xFF64B5F6),
    recent = Color(0xFFFFD54F),
    meshCarry = Color(0xFFFFD54F),
    neutral = Color(0xFF6E7B81),
)

val LocalReachabilityPalette = staticCompositionLocalOf { LightReachabilityPalette }

private val CruiseMeshLightColors = lightColorScheme(
    primary = androidx.compose.ui.graphics.Color(0xFF0E6777),
    onPrimary = androidx.compose.ui.graphics.Color.White,
    primaryContainer = androidx.compose.ui.graphics.Color(0xFFBDEAF4),
    onPrimaryContainer = androidx.compose.ui.graphics.Color(0xFF002129),
    secondary = androidx.compose.ui.graphics.Color(0xFF49646D),
    onSecondary = androidx.compose.ui.graphics.Color.White,
    secondaryContainer = androidx.compose.ui.graphics.Color(0xFFD2EAF4),
    onSecondaryContainer = androidx.compose.ui.graphics.Color(0xFF041F26),
    tertiary = androidx.compose.ui.graphics.Color(0xFF355C8C),
    onTertiary = androidx.compose.ui.graphics.Color.White,
    surface = androidx.compose.ui.graphics.Color(0xFFF7FAFC),
    onSurface = androidx.compose.ui.graphics.Color(0xFF151C1F),
    surfaceVariant = androidx.compose.ui.graphics.Color(0xFFDCE5EA),
    onSurfaceVariant = androidx.compose.ui.graphics.Color(0xFF3E4A50),
    background = androidx.compose.ui.graphics.Color(0xFFF7FAFC),
    onBackground = androidx.compose.ui.graphics.Color(0xFF151C1F),
    error = androidx.compose.ui.graphics.Color(0xFFB3261E),
    onError = androidx.compose.ui.graphics.Color.White,
)

private val CruiseMeshDarkColors = darkColorScheme(
    primary = androidx.compose.ui.graphics.Color(0xFF86D1E2),
    onPrimary = androidx.compose.ui.graphics.Color(0xFF003641),
    primaryContainer = androidx.compose.ui.graphics.Color(0xFF004E5D),
    onPrimaryContainer = androidx.compose.ui.graphics.Color(0xFFBDEAF4),
    secondary = androidx.compose.ui.graphics.Color(0xFFB4CDD7),
    onSecondary = androidx.compose.ui.graphics.Color(0xFF1C343C),
    secondaryContainer = androidx.compose.ui.graphics.Color(0xFF334C54),
    onSecondaryContainer = androidx.compose.ui.graphics.Color(0xFFD2EAF4),
    tertiary = androidx.compose.ui.graphics.Color(0xFFB8C8FF),
    onTertiary = androidx.compose.ui.graphics.Color(0xFF1D315D),
    surface = androidx.compose.ui.graphics.Color(0xFF0F1417),
    onSurface = androidx.compose.ui.graphics.Color(0xFFE3E8EB),
    surfaceVariant = androidx.compose.ui.graphics.Color(0xFF3E4A50),
    onSurfaceVariant = androidx.compose.ui.graphics.Color(0xFFC0CCD1),
    background = androidx.compose.ui.graphics.Color(0xFF0F1417),
    onBackground = androidx.compose.ui.graphics.Color(0xFFE3E8EB),
    error = androidx.compose.ui.graphics.Color(0xFFFFB4AB),
    onError = androidx.compose.ui.graphics.Color(0xFF690005),
)

@Composable
fun CruiseMeshTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit
) {
    CompositionLocalProvider(
        LocalReachabilityPalette provides if (darkTheme) DarkReachabilityPalette else LightReachabilityPalette,
    ) {
        MaterialTheme(
            colorScheme = if (darkTheme) CruiseMeshDarkColors else CruiseMeshLightColors,
            content = content,
        )
    }
}
