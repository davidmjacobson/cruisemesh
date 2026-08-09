package com.cruisemesh.app.relay

import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.SocketPolicy
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.CoreRelayHeader
import uniffi.cruisemesh_core.CoreRelayHttpRequest
import uniffi.cruisemesh_core.CoreRelayOperation
import uniffi.cruisemesh_core.CoreRelayTransportError

/**
 * What the driver is allowed to decide, and what it must not.
 *
 * Each test here is one clause of the seam: the ids it echoes, the cap it
 * enforces, the headers it drops, the failures it names, and the one thing it
 * is emphatically not allowed to do -- interpret a status code.
 */
class CoreRelayDriverTest {

    @Test
    fun `a result echoes the ids it was handed`() = withServer(
        MockResponse().setResponseCode(200).setBody("""{"id":7}"""),
    ) { base ->
        val result = CoreRelayDriver.execute("pass-7", 42u, post(base), null, NOW)
        assertEquals("pass-7", result.passId)
        assertEquals(42uL, result.actionId)
        assertEquals(NOW, result.completedAtMs)
        assertEquals(200u.toUShort(), result.status)
        assertNull(result.error)
    }

    @Test
    fun `only the response headers core asked for come back`() = withServer(
        MockResponse().setResponseCode(429)
            .setHeader("Retry-After", "30")
            .setHeader("X-Relay-Node", "node-4")
            .setBody("""{"code":"rate_limited"}"""),
    ) { base ->
        val result = CoreRelayDriver.execute("p", 1u, post(base), null, NOW)
        assertEquals(listOf(CoreRelayHeader("Retry-After", "30")), result.headers)
    }

    @Test
    fun `a failing status is reported as a status, never as a failure to reach the relay`() =
        withServer(
            MockResponse().setResponseCode(507).setBody("""{"code":"mailbox_full"}"""),
        ) { base ->
            val result = CoreRelayDriver.execute("p", 1u, post(base), null, NOW)
            // The driver classifies nothing. Core reads the status and the
            // relay's own code out of the body and decides what they mean.
            assertEquals(507u.toUShort(), result.status)
            assertNull(result.error)
            assertTrue(String(result.body).contains("mailbox_full"))
        }

    @Test
    fun `a body past the declared cap is refused rather than accumulated`() = withServer(
        MockResponse().setResponseCode(200).setBody("x".repeat(4_096)),
    ) { base ->
        val result = CoreRelayDriver.execute("p", 1u, post(base).copy(maxResponseBytes = 512u), null, NOW)
        assertEquals(CoreRelayTransportError.BODY_TOO_LARGE, result.error)
        assertEquals(0u.toUShort(), result.status)
        assertArrayEquals(ByteArray(0), result.body)
    }

    @Test
    fun `an oversized error page is still a status, not an oversized page`() = withServer(
        // A captive portal or a proxy banner. Calling this an oversized page
        // sends a fetch down the shrink ladder and discards the Retry-After a
        // rate limit would have carried.
        MockResponse().setResponseCode(502)
            .setHeader("Retry-After", "5")
            .setBody("<html>" + "y".repeat(8_192) + "</html>"),
    ) { base ->
        val result = CoreRelayDriver.execute("p", 1u, post(base).copy(maxResponseBytes = 512u), null, NOW)
        assertEquals(502u.toUShort(), result.status)
        assertNull(result.error)
        assertEquals(listOf(CoreRelayHeader("Retry-After", "5")), result.headers)
    }

    @Test
    fun `a connection that is never answered is a transport failure, not a status`() = withServer(
        MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AT_START),
    ) { base ->
        val result = CoreRelayDriver.execute("p", 1u, post(base), null, NOW)
        assertEquals(0u.toUShort(), result.status)
        assertTrue(
            "an unanswered connection must map to a transport error, got ${result.error}",
            result.error == CoreRelayTransportError.CONNECTION_FAILED ||
                result.error == CoreRelayTransportError.TIMEOUT,
        )
        assertEquals(NOW, result.completedAtMs)
    }

    @Test
    fun `a cancelled driver says so instead of reporting an outage`() = withServer(
        MockResponse().setResponseCode(200).setBody("{}"),
    ) { base ->
        val result = CoreRelayDriver.execute("p", 1u, post(base), null, NOW, isCancelled = { true })
        assertEquals(CoreRelayTransportError.CANCELLED, result.error)
        // Nothing was sent: a cancellation checked before the request means
        // the relay never heard from this pass at all.
        assertEquals(0u.toUShort(), result.status)
    }

    @Test
    fun `a GET carries no body and a POST carries exactly the bytes core formed`() {
        val server = MockWebServer()
        server.enqueue(MockResponse().setResponseCode(200).setBody("{}"))
        server.enqueue(MockResponse().setResponseCode(200).setBody("{}"))
        server.start()
        try {
            val base = normalizeRelayUrl(server.url("/").toString())
            CoreRelayDriver.execute("p", 1u, post(base), null, NOW)
            assertArrayEquals(BODY, server.takeRequest().body.readByteArray())

            CoreRelayDriver.execute(
                "p",
                2u,
                post(base).copy(
                    operation = CoreRelayOperation.FETCH_PAGE,
                    method = "GET",
                    path = "/envelopes?after=0&limit=8",
                    body = ByteArray(0),
                ),
                null,
                NOW,
            )
            val fetch = server.takeRequest()
            assertEquals("GET", fetch.method)
            assertEquals("/envelopes?after=0&limit=8", fetch.path)
            assertEquals(0L, fetch.bodySize)
        } finally {
            server.shutdown()
        }
    }

    private fun post(base: String) = CoreRelayHttpRequest(
        operation = CoreRelayOperation.POST_ENVELOPE,
        method = "POST",
        baseUrl = base,
        path = "/envelopes",
        headers = listOf(
            CoreRelayHeader("Authorization", "Bearer member-token"),
            CoreRelayHeader("Content-Type", "application/json"),
            CoreRelayHeader("Accept", "application/json"),
        ),
        body = BODY,
        maxResponseBytes = 65_536u,
        responseHeadersWanted = listOf("Retry-After"),
    )

    private fun withServer(response: MockResponse, block: (String) -> Unit) {
        val server = MockWebServer()
        server.enqueue(response)
        server.start()
        try {
            block(normalizeRelayUrl(server.url("/").toString()))
        } finally {
            server.shutdown()
        }
    }

    private companion object {
        const val NOW = 1_700_000_000_000L
        val BODY = """{"msg_id":"EREREREREREREREREREREQ"}""".toByteArray()
    }
}
