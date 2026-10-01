package com.cruisemesh.app.ui

/**
 * Stable selectors for UI surfaces that cannot be identified reliably by
 * user-visible semantics alone. Keep this list deliberately small: tests
 * should prefer text, roles, state, and content descriptions whenever they
 * describe what a person can actually perceive or operate.
 */
object UiTestTags {
    const val TERMS_SCREEN = "terms_screen"
    const val ONBOARDING_SCREEN = "onboarding_screen"
    const val PERMISSIONS_SETUP_SCREEN = "permissions_setup_screen"
    const val NEW_GROUP_SCREEN = "new_group_screen"
    const val MESSAGE_COMPOSER = "message_composer"
}
