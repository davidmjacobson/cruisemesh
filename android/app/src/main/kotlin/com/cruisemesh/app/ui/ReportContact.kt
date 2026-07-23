package com.cruisemesh.app.ui

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.widget.Toast
import com.cruisemesh.app.R
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.fingerprintWords
import uniffi.cruisemesh_core.formatUserId

const val ABUSE_REPORT_ADDRESS = "abuse@cruisemesh.app"

/**
 * Opens the user's email app with a pre-filled abuse report. E2E stays
 * intact: nothing sends automatically and no message content is attached —
 * the reporter writes what happened and owns their copy of anything they
 * choose to include.
 */
fun launchContactReport(context: Context, contact: Contact, reporterUserId: ByteArray) {
    val body = context.getString(
        R.string.ui_report_email_body,
        coreContactDisplayName(contact),
        formatUserId(contact.userId),
        fingerprintWords(contact.userId).joinToString(" "),
        formatUserId(reporterUserId),
    )
    val intent = Intent(Intent.ACTION_SENDTO).apply {
        data = Uri.parse("mailto:")
        putExtra(Intent.EXTRA_EMAIL, arrayOf(ABUSE_REPORT_ADDRESS))
        putExtra(Intent.EXTRA_SUBJECT, context.getString(R.string.ui_report_email_subject))
        putExtra(Intent.EXTRA_TEXT, body)
    }
    try {
        context.startActivity(intent)
    } catch (e: ActivityNotFoundException) {
        Toast.makeText(
            context,
            context.getString(R.string.ui_no_email_app, ABUSE_REPORT_ADDRESS),
            Toast.LENGTH_LONG,
        ).show()
    }
}
