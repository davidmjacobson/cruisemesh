package com.cruisemesh.app.identity

import android.content.Context

const val CURRENT_TERMS_VERSION = "2026-07-23"
const val TERMS_OF_USE_URL = "https://cruisemesh.app/terms/"
const val PRIVACY_POLICY_URL = "https://cruisemesh.app/privacy/"

private const val PREFS_NAME = "cruisemesh_terms"
private const val ACCEPTED_VERSION_KEY = "accepted_version"

internal fun isCurrentTermsVersion(version: String?): Boolean = version == CURRENT_TERMS_VERSION

/** Records acceptance of the exact published terms version, so future versions can be gated again. */
object TermsAcceptanceStore {
    fun isCurrentVersionAccepted(context: Context): Boolean =
        isCurrentTermsVersion(
            context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
                .getString(ACCEPTED_VERSION_KEY, null),
        )

    fun acceptCurrentVersion(context: Context) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putString(ACCEPTED_VERSION_KEY, CURRENT_TERMS_VERSION)
            .apply()
    }
}
