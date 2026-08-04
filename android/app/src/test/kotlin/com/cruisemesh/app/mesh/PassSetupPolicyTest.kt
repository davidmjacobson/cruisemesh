package com.cruisemesh.app.mesh

import com.cruisemesh.app.R
import com.cruisemesh.app.relay.RelayHttpException
import java.io.IOException
import java.net.ConnectException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import java.util.concurrent.CancellationException
import javax.net.ssl.SSLException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PassSetupPolicyTest {

    @Test
    fun `transport failures earn the one silent retry`() {
        assertTrue(shouldRetryFirstRelayCheck(SocketTimeoutException("read timed out")))
        assertTrue(shouldRetryFirstRelayCheck(UnknownHostException("relay.cruisemesh.app")))
        assertTrue(shouldRetryFirstRelayCheck(ConnectException("connection refused")))
        assertTrue(shouldRetryFirstRelayCheck(SSLException("handshake reset")))
        assertTrue(shouldRetryFirstRelayCheck(IOException("stream closed")))
    }

    @Test
    fun `deterministic failures are never retried`() {
        // An HTTP rejection will reject identically in 750ms; retrying it
        // only delays the real error message.
        assertFalse(shouldRetryFirstRelayCheck(RelayHttpException(403, "family_suspended", "no")))
        assertFalse(shouldRetryFirstRelayCheck(RelayHttpException(401, null, "no")))
        // Non-transport failures (decode bugs, cancellation) would fail the
        // same way again; surfacing them immediately is the honest move.
        assertFalse(shouldRetryFirstRelayCheck(IllegalStateException("decode bug")))
        assertFalse(shouldRetryFirstRelayCheck(CancellationException()))
    }

    @Test
    fun `relay verdicts map to their specific explanations`() {
        assertEquals(
            R.string.ui_this_shore_pass_has_expired,
            relayCheckFailureRes(RelayHttpException(403, "family_expired", "x"), true),
        )
        assertEquals(
            R.string.ui_this_shore_pass_is_suspended,
            relayCheckFailureRes(RelayHttpException(403, "family_suspended", "x"), true),
        )
        assertEquals(
            R.string.ui_this_setup_card_was_rejected,
            relayCheckFailureRes(RelayHttpException(401, null, "x"), true),
        )
    }

    @Test
    fun `transport failures map by cause not by connectivity`() {
        // A timeout with validated internet is still reported as a timeout;
        // the connectivity hint is only for the otherwise-unexplained case.
        assertEquals(
            R.string.ui_shore_pass_check_timed_out,
            relayCheckFailureRes(SocketTimeoutException("t"), false),
        )
        assertEquals(
            R.string.ui_shore_pass_service_not_found,
            relayCheckFailureRes(UnknownHostException("h"), false),
        )
        assertEquals(
            R.string.ui_shore_pass_secure_connection_failed,
            relayCheckFailureRes(SSLException("s"), false),
        )
    }

    @Test
    fun `unexplained failures fall back to the connectivity hint`() {
        assertEquals(
            R.string.ui_android_has_not_verified_internet,
            relayCheckFailureRes(IOException("reset"), false),
        )
        assertEquals(
            R.string.ui_shore_pass_check_failed_network,
            relayCheckFailureRes(IOException("reset"), true),
        )
    }
}
