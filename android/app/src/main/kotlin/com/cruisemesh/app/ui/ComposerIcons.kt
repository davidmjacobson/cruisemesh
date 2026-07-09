package com.cruisemesh.app.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.PathParser
import androidx.compose.ui.unit.dp

/**
 * A few Material-style glyphs the app needs for the Signal-style composer
 * (camera, microphone, send). These live in `material-icons-extended`, which
 * we deliberately don't depend on -- pulling in the whole extended icon pack
 * for three glyphs isn't worth the APK/build cost. Path data is the standard
 * 24dp Material filled set. Base fill is black; [androidx.compose.material3.Icon]
 * re-tints at draw time, so the fill color here is irrelevant.
 */
private fun materialIcon(name: String, pathData: String): ImageVector =
    ImageVector.Builder(
        name = name,
        defaultWidth = 24.dp,
        defaultHeight = 24.dp,
        viewportWidth = 24f,
        viewportHeight = 24f,
    ).apply {
        addPath(
            pathData = PathParser().parsePathString(pathData).toNodes(),
            fill = SolidColor(Color.Black),
        )
    }.build()

/** Filled "photo_camera" glyph. */
val ComposerCameraIcon: ImageVector by lazy {
    materialIcon(
        name = "ComposerCamera",
        pathData = "M12,15.2c1.77,0 3.2,-1.43 3.2,-3.2s-1.43,-3.2 -3.2,-3.2 -3.2,1.43 " +
            "-3.2,3.2 1.43,3.2 3.2,3.2zM9,2L7.17,4H4c-1.1,0 -2,0.9 -2,2v12c0,1.1 " +
            "0.9,2 2,2h16c1.1,0 2,-0.9 2,-2V6c0,-1.1 -0.9,-2 -2,-2h-3.17L15,2H9zM12," +
            "17c-2.76,0 -5,-2.24 -5,-5s2.24,-5 5,-5 5,2.24 5,5 -2.24,5 -5,5z",
    )
}

/** Filled "mic" glyph. */
val ComposerMicIcon: ImageVector by lazy {
    materialIcon(
        name = "ComposerMic",
        pathData = "M12,14c1.66,0 2.99,-1.34 2.99,-3L15,5c0,-1.66 -1.34,-3 -3,-3S9,3.34 " +
            "9,5v6c0,1.66 1.34,3 3,3zM17.3,11c0,3 -2.54,5.1 -5.3,5.1S6.7,14 6.7,11L5," +
            "11c0,3.41 2.72,6.23 6,6.72V21h2v-3.28c3.28,-0.48 6,-3.3 6,-6.72h-1.7z",
    )
}

/** Filled "send" glyph. */
val ComposerSendIcon: ImageVector by lazy {
    materialIcon(
        name = "ComposerSend",
        pathData = "M2.01,21L23,12 2.01,3 2,10l15,2 -15,2z",
    )
}
