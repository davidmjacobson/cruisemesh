package com.cruisemesh.app.ui

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/**
 * CP2b: the "?" and "!" glyphs for the Shore Pass status indicator --
 * David's UX spec gives the transient/self-healing states a question mark
 * and the persistent/actionable states an exclamation mark (iOS uses
 * `questionmark.circle.fill` / `exclamationmark.circle.fill`).
 *
 * These are the standard Material `Help` and `Error` filled icons (Apache
 * 2.0 path data), inlined because the app deliberately depends only on
 * `material-icons-core`, which carries neither -- pulling the whole
 * material-icons-extended artifact in for two glyphs isn't worth the build
 * weight.
 */

/** Material `Help` filled: a question mark in a circle. */
val PassQuestionIcon: ImageVector by lazy {
    passGlyph(
        "PassQuestion",
        "M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 17h-2v-2h2v2z" +
            "m2.07-7.75l-.9.92C13.45 12.9 13 13.5 13 15h-2v-.5c0-1.1.45-2.1 1.17-2.83l1.24-1.26" +
            "c.37-.36.59-.86.59-1.41 0-1.1-.9-2-2-2s-2 .9-2 2H8c0-2.21 1.79-4 4-4s4 1.79 4 4" +
            "c0 .88-.36 1.68-.93 2.25z",
    )
}

/** Material `Error` filled: an exclamation mark in a circle. */
val PassExclamationIcon: ImageVector by lazy {
    passGlyph(
        "PassExclamation",
        "M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm1 15h-2v-2h2v2z" +
            "m0-4h-2V7h2v6z",
    )
}

private fun passGlyph(name: String, pathData: String): ImageVector =
    ImageVector.Builder(
        name = name,
        defaultWidth = 24.dp,
        defaultHeight = 24.dp,
        viewportWidth = 24f,
        viewportHeight = 24f,
    ).addPath(
        pathData = addPathNodes(pathData),
        fill = SolidColor(Color.Black),
    ).build()
