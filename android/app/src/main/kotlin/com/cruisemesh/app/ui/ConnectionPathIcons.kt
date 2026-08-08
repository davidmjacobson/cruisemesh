package com.cruisemesh.app.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/**
 * The three path glyphs the Connection details page needs.
 *
 * Standard Material filled path data (Apache 2.0), inlined for the same reason
 * as [PassQuestionIcon] and the composer glyphs: the app depends only on
 * `material-icons-core`, which carries none of these, and pulling the whole
 * extended pack in for three shapes is not worth the build weight.
 *
 * These exist because the spec requires every colored status to be paired with
 * an icon and a textual label -- color alone must never be the signal.
 */
private fun pathGlyph(name: String, pathData: String): ImageVector =
    ImageVector.Builder(
        name = name,
        defaultWidth = 24.dp,
        defaultHeight = 24.dp,
        viewportWidth = 24f,
        viewportHeight = 24f,
    ).apply {
        addPath(pathData = addPathNodes(pathData), fill = SolidColor(Color.Black))
    }.build()

/** Material `bluetooth` filled. */
val PathBluetoothIcon: ImageVector by lazy {
    pathGlyph(
        "PathBluetooth",
        "M17.71 7.71 12 2h-1v7.59L6.41 5 5 6.41 10.59 12 5 17.59 6.41 19 11 14.41V22h1" +
            "l5.71-5.71-4.3-4.29 4.3-4.29zM13 5.83l1.88 1.88L13 9.59V5.83zm1.88 10.46L13 " +
            "18.17v-3.76l1.88 1.88z",
    )
}

/** Material `wifi` filled. */
val PathLocalWifiIcon: ImageVector by lazy {
    pathGlyph(
        "PathLocalWifi",
        "M1 9l2 2c4.97-4.97 13.03-4.97 18 0l2-2C16.93 2.93 7.08 2.93 1 9zm8 8l3 3 3-3" +
            "c-1.65-1.66-4.34-1.66-6 0zm-4-4l2 2c2.76-2.76 7.24-2.76 10 0l2-2C15.14 9.14 " +
            "8.87 9.14 5 13z",
    )
}

/** Material `cloud` filled, standing in for the Shore Pass path. */
val PathShorePassIcon: ImageVector by lazy {
    pathGlyph(
        "PathShorePass",
        "M19.35 10.04C18.67 6.59 15.64 4 12 4 9.11 4 6.6 5.64 5.35 8.04 2.34 8.36 0 " +
            "10.91 0 14c0 3.31 2.69 6 6 6h13c2.76 0 5-2.24 5-5 0-2.64-2.05-4.78-4.65-4.96z",
    )
}
