package com.cruisemesh.app.ui

import android.content.ActivityNotFoundException
import android.content.Intent
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class ReportContactTest {
    @Test
    fun mailtoUriCarriesRecipientSubjectAndBody() {
        val uri = abuseReportMailtoUri(
            subject = "CruiseMesh abuse report",
            body = "Reporting: River & Reed\nWhat happened:",
        )

        assertEquals("mailto", uri.scheme)
        assertEquals(ABUSE_REPORT_ADDRESS, uri.schemeSpecificPart.substringBefore('?'))
        val decoded = android.net.Uri.decode(uri.toString())
        assertTrue(decoded.contains("subject=CruiseMesh abuse report"))
        assertTrue(decoded.contains("body=Reporting: River & Reed\nWhat happened:"))
        assertFalse(decoded.contains("Intent.EXTRA"))
    }

    @Test
    fun openingMailDoesNotTouchTheClipboard() {
        val intent = Intent(Intent.ACTION_SENDTO, abuseReportMailtoUri("Subject", "Body"))
        var opened: Intent? = null
        var copied: String? = null

        val outcome = openContactReport(
            intent = intent,
            openIntent = { opened = it },
            copyAddress = { copied = it },
        )

        assertEquals(ContactReportOutcome.MAIL_APP_OPENED, outcome)
        assertEquals(intent, opened)
        assertEquals(null, copied)
    }

    @Test
    fun missingMailAppCopiesThePublishedAddress() {
        var copied: String? = null

        val outcome = openContactReport(
            intent = Intent(Intent.ACTION_SENDTO),
            openIntent = { throw ActivityNotFoundException("no mail app") },
            copyAddress = { copied = it },
        )

        assertEquals(ContactReportOutcome.ADDRESS_COPIED, outcome)
        assertEquals(ABUSE_REPORT_ADDRESS, copied)
    }

    @Test
    fun blockedMailHandlerAlsoKeepsReportingReachable() {
        var copied: String? = null

        val outcome = openContactReport(
            intent = Intent(Intent.ACTION_SENDTO),
            openIntent = { throw SecurityException("handler blocked") },
            copyAddress = { copied = it },
        )

        assertEquals(ContactReportOutcome.ADDRESS_COPIED, outcome)
        assertTrue(copied == ABUSE_REPORT_ADDRESS)
    }
}
