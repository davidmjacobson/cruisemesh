package com.cruisemesh.app.ui

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.net.Uri
import com.cruisemesh.app.R
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.formatUserId

const val ABUSE_REPORT_ADDRESS = "abuse@cruisemesh.app"

enum class ContactReportOutcome {
    MAIL_APP_OPENED,
    ADDRESS_COPIED,
}

/**
 * Build an interoperable mailto URI. ACTION_SENDTO handlers are not required
 * to read Intent extras, so the recipient, subject, and body live in the URI
 * itself rather than in EXTRA_EMAIL / EXTRA_SUBJECT / EXTRA_TEXT.
 */
internal fun abuseReportMailtoUri(subject: String, body: String): Uri =
    Uri.parse(
        "mailto:$ABUSE_REPORT_ADDRESS" +
            "?subject=${Uri.encode(subject)}&body=${Uri.encode(body)}",
    )

/** Side-effect boundary kept injectable so both launch branches stay unit-testable. */
internal fun openContactReport(
    intent: Intent,
    openIntent: (Intent) -> Unit,
    copyAddress: (String) -> Unit,
): ContactReportOutcome = try {
    openIntent(intent)
    ContactReportOutcome.MAIL_APP_OPENED
} catch (_: ActivityNotFoundException) {
    copyAddress(ABUSE_REPORT_ADDRESS)
    ContactReportOutcome.ADDRESS_COPIED
} catch (_: SecurityException) {
    copyAddress(ABUSE_REPORT_ADDRESS)
    ContactReportOutcome.ADDRESS_COPIED
}

fun copyAbuseReportAddress(context: Context) {
    val clipboard = context.getSystemService(ClipboardManager::class.java)
    clipboard?.setPrimaryClip(ClipData.newPlainText(ABUSE_REPORT_ADDRESS, ABUSE_REPORT_ADDRESS))
}

/**
 * Opens the user's email app with a pre-filled abuse report. E2E stays
 * intact: nothing sends automatically and no message content is attached —
 * the reporter writes what happened and owns their copy of anything they
 * choose to include.
 */
fun launchContactReport(
    context: Context,
    contact: Contact,
    reporterUserId: ByteArray,
): ContactReportOutcome {
    val body = context.getString(
        R.string.ui_report_email_body,
        coreContactDisplayName(contact),
        formatUserId(contact.userId),
        fingerprintWords(contact.userId).joinToString(" "),
        formatUserId(reporterUserId),
    )
    val intent = Intent(
        Intent.ACTION_SENDTO,
        abuseReportMailtoUri(context.getString(R.string.ui_report_email_subject), body),
    )
    return openContactReport(
        intent = intent,
        openIntent = context::startActivity,
        copyAddress = { copyAbuseReportAddress(context) },
    )
}
