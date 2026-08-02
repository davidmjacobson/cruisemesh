package com.cruisemesh.app.relay

import com.google.gson.JsonParser
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.OutgoingReceiptEnvelope
import uniffi.cruisemesh_core.relayMaxResponseBytes
import java.nio.charset.StandardCharsets
import java.io.ByteArrayInputStream
import java.io.IOException
import java.util.Base64

class RelayClientTest {

    @Test
    fun `relay URL normalization adds https and removes trailing slash`() {
        assertEquals("https://relay.example", normalizeRelayUrl(" relay.example/ "))
        assertEquals("https://relay.example", normalizeRelayUrl("https://relay.example/"))
        assertEquals("http://10.0.2.2:8080", normalizeRelayUrl("http://10.0.2.2:8080/"))
    }

    @Test
    fun `a non-loopback http relay URL is refused instead of stored`() {
        assertEquals("", normalizeRelayUrl("http://relay.example"))
        assertEquals("", normalizeRelayUrl("http://192.168.1.50:8080"))
    }

    @Test
    fun `posting to a non-https relay fails with a legible error`() {
        val config = RelayConfig("http://relay.example", "family-token")
        val error = assertThrows(IOException::class.java) {
            RelayClient.postOutboundEnvelope(config, sampleOutboundEnvelope())
        }
        assertEquals("Relay URL must use https", error.message)
    }

    @Test
    fun `post outbound envelope sends bearer auth and public header fields`() {
        val server = MockWebServer()
        server.enqueue(MockResponse().setResponseCode(200).setBody("""{"id":7}"""))
        server.start()
        try {
            val config = RelayConfig(server.url("/").toString(), "family-token")
            val id = RelayClient.postOutboundEnvelope(config, sampleOutboundEnvelope())
            assertEquals(7L, id)

            val request = server.takeRequest()
            assertEquals("/envelopes", request.path)
            assertEquals("Bearer family-token", request.getHeader("Authorization"))
            assertEquals("CruiseMeshRelayClient/0.1", request.getHeader("User-Agent"))
            assertEquals("1", request.getHeader("bypass-tunnel-reminder"))
            val json = JsonParser.parseString(request.body.readUtf8()).asJsonObject
            assertEquals(base64Url(ByteArray(16) { 1 }), json["msg_id"].asString)
            assertEquals(7, json["hop_ttl"].asInt)
            assertEquals(base64Url(ByteArray(8) { 2 }), json["recipient_hint"].asString)
            assertEquals(base64Url("sealed".toByteArray(StandardCharsets.UTF_8)), json["sealed"].asString)
            assertEquals(1_700_000_060_000L, json["expiry_ms"].asLong)
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun `auth reject surfaces as RelayHttpException with its status code`() {
        val server = MockWebServer()
        server.enqueue(MockResponse().setResponseCode(401).setBody("""{"error":"unknown family token"}"""))
        server.start()
        try {
            val config = RelayConfig(server.url("/").toString(), "stale-token")
            try {
                RelayClient.postOutboundEnvelope(config, sampleOutboundEnvelope())
                org.junit.Assert.fail("expected RelayHttpException")
            } catch (e: RelayHttpException) {
                assertEquals(401, e.code)
                assertEquals(null, e.relayCode)
            }
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun `hosted pass rejection surfaces stable relay code`() {
        val server = MockWebServer()
        server.enqueue(
            MockResponse()
                .setResponseCode(403)
                .setBody("""{"error":"relay pass expired","code":"family_expired"}"""),
        )
        server.start()
        try {
            val config = RelayConfig(server.url("/").toString(), "expired-token")
            try {
                RelayClient.syncPresence(config, emptyList(), emptyList())
                org.junit.Assert.fail("expected RelayHttpException")
            } catch (e: RelayHttpException) {
                assertEquals(403, e.code)
                assertEquals("family_expired", e.relayCode)
            }
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun `post receipt envelope uses the same relay contract`() {
        val server = MockWebServer()
        server.enqueue(MockResponse().setResponseCode(200).setBody("""{"id":11}"""))
        server.start()
        try {
            val config = RelayConfig(server.url("/").toString(), "family-token")
            val id = RelayClient.postReceiptEnvelope(config, sampleReceiptEnvelope())
            assertEquals(11L, id)

            val request = server.takeRequest()
            assertEquals("/envelopes", request.path)
            assertEquals("Bearer family-token", request.getHeader("Authorization"))
            val json = JsonParser.parseString(request.body.readUtf8()).asJsonObject
            assertEquals(base64Url(ByteArray(16) { 6 }), json["msg_id"].asString)
            assertEquals(7, json["hop_ttl"].asInt)
            assertEquals(base64Url(ByteArray(8) { 7 }), json["recipient_hint"].asString)
            assertEquals(base64Url("receipt-sealed".toByteArray(StandardCharsets.UTF_8)), json["sealed"].asString)
            assertEquals(1_700_000_070_000L, json["expiry_ms"].asLong)
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun `fetch and ack round trip the relay envelope contract`() {
        val server = MockWebServer()
        server.enqueue(
            MockResponse().setResponseCode(200).setBody(
                """
                {
                  "envelopes": [
                    {
                      "id": 9,
                      "msg_id": "${base64Url(ByteArray(16) { 3 })}",
                      "hop_ttl": 5,
                      "recipient_hint": "${base64Url(ByteArray(8) { 4 })}",
                      "sealed": "${base64Url("relay-sealed".toByteArray(StandardCharsets.UTF_8))}",
                      "expiry_ms": 1700009999999,
                      "created_at_ms": 1700000000000
                    }
                  ],
                  "next_cursor": 9
                }
                """.trimIndent(),
            ),
        )
        server.enqueue(MockResponse().setResponseCode(200).setBody("""{"deleted":1}"""))
        server.start()
        try {
            val config = RelayConfig(server.url("/").toString(), "family-token")
            val page = RelayClient.fetchEnvelopes(config, listOf(ByteArray(8) { 4 }), afterId = 0, limit = 16)
            assertEquals(1, page.envelopes.size)
            assertEquals(9L, page.nextCursor)
            assertEquals(9L, page.envelopes[0].id)
            assertEquals(5u.toUByte(), page.envelopes[0].hopTtl)
            assertArrayEquals(ByteArray(16) { 3 }, page.envelopes[0].msgId)
            assertArrayEquals("relay-sealed".toByteArray(StandardCharsets.UTF_8), page.envelopes[0].sealed)

            RelayClient.ackEnvelopes(config, listOf(9L))

            val fetchRequest = server.takeRequest()
            assertEquals("/envelopes?hints=BAQEBAQEBAQ&after=0&limit=16", fetchRequest.path)
            assertEquals("CruiseMeshRelayClient/0.1", fetchRequest.getHeader("User-Agent"))
            assertEquals("1", fetchRequest.getHeader("bypass-tunnel-reminder"))
            val ackRequest = server.takeRequest()
            assertEquals("/envelopes/ack", ackRequest.path)
            assertEquals("CruiseMeshRelayClient/0.1", ackRequest.getHeader("User-Agent"))
            assertEquals("1", ackRequest.getHeader("bypass-tunnel-reminder"))
            val ids = JsonParser.parseString(ackRequest.body.readUtf8()).asJsonObject["ids"].asJsonArray
            assertEquals(listOf(9L), ids.map { it.asLong })
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun `sync presence sends announce and query hints`() {
        val server = MockWebServer()
        server.enqueue(
            MockResponse().setResponseCode(200).setBody(
                """
                {
                  "now_ms": 1700000060000,
                  "presence": [
                    {
                      "hint": "${base64Url(ByteArray(8) { 5 })}",
                      "last_seen_ms": 1700000055000
                    }
                  ]
                }
                """.trimIndent(),
            ),
        )
        server.start()
        try {
            val config = RelayConfig(server.url("/").toString(), "family-token")
            val page = RelayClient.syncPresence(
                config,
                announce = listOf(ByteArray(8) { 1 }),
                query = listOf(ByteArray(8) { 5 }),
            )
            assertEquals(1, page.presence.size)
            assertEquals(1_700_000_060_000L, page.nowMs)
            assertEquals(1_700_000_055_000L, page.presence[0].lastSeenMs)
            assertArrayEquals(ByteArray(8) { 5 }, page.presence[0].hint)

            val request = server.takeRequest()
            assertEquals("/presence", request.path)
            assertEquals("Bearer family-token", request.getHeader("Authorization"))
            assertEquals("CruiseMeshRelayClient/0.1", request.getHeader("User-Agent"))
            assertEquals("1", request.getHeader("bypass-tunnel-reminder"))
            val json = JsonParser.parseString(request.body.readUtf8()).asJsonObject
            assertEquals(base64Url(ByteArray(8) { 1 }), json["announce"].asJsonArray[0].asString)
            assertEquals(base64Url(ByteArray(8) { 5 }), json["query"].asJsonArray[0].asString)
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun `bounded response reader rejects a body above its limit`() {
        val error = runCatching {
            ByteArrayInputStream(ByteArray(9)).readBounded(8)
        }.exceptionOrNull()

        // Its own IOException subtype, not a bare one: an oversize *fetch*
        // page is recoverable by asking for fewer rows, and the walk can only
        // tell that failure apart from "the network broke" by its type.
        assertEquals(RelayResponseTooLargeException::class.java, error?.javaClass)
        assertEquals(8, (error as RelayResponseTooLargeException).maxBytes)
        // Still an IOException, so every existing "the relay call failed"
        // catch keeps working unchanged.
        assertTrue(IOException::class.java.isAssignableFrom(error.javaClass))
        val exact = ByteArray(8) { it.toByte() }
        assertArrayEquals(exact, ByteArrayInputStream(exact).readBounded(8))
    }

    @Test
    fun `a fetch page too big to decode is retried at half the limit, not failed`() {
        // The stall: `limit` bounds rows, not bytes, so a mailbox of large
        // attachment chunks can produce a window past the response cap. The
        // next pass would ask the same relay for the same window from the
        // same cursor and fail identically -- the frontier never advances.
        // A self-hosted relay predating the server-side byte budget is
        // exactly this case, so the client must recover on its own.
        val server = MockWebServer()
        server.enqueue(
            MockResponse()
                .setResponseCode(200)
                .setBody("""{"envelopes":[],"next_cursor":0}""")
                .setHeader("Content-Length", (relayMaxResponseBytes().toLong() + 1).toString()),
        )
        server.enqueue(
            MockResponse().setResponseCode(200).setBody("""{"envelopes":[],"next_cursor":42}"""),
        )
        server.start()
        try {
            val config = RelayConfig(server.url("/").toString(), "family-token")
            val fetched = RelayClient.fetchEnvelopesWithinResponseCap(
                config,
                listOf(ByteArray(8) { 2 }),
                afterId = 42L,
                limit = 256,
            )

            assertEquals(128, fetched.limit)
            assertEquals(42L, fetched.page.nextCursor)
            val first = server.takeRequest()
            assertTrue(first.path!!.contains("limit=256"))
            assertTrue(first.path!!.contains("after=42"))
            val second = server.takeRequest()
            // Same cursor, half the rows: nothing is skipped by recovering.
            assertTrue(second.path!!.contains("limit=128"))
            assertTrue(second.path!!.contains("after=42"))
        } finally {
            server.shutdown()
        }
    }

    @Test
    fun `an oversize single-row page is reported rather than retried forever`() {
        // Nothing smaller than one row can be asked for, so retrying would
        // just spin. Surface it instead.
        val server = MockWebServer()
        repeat(2) {
            server.enqueue(
                MockResponse()
                    .setResponseCode(200)
                    .setBody("""{"envelopes":[],"next_cursor":0}""")
                    .setHeader("Content-Length", (relayMaxResponseBytes().toLong() + 1).toString()),
            )
        }
        server.start()
        try {
            val config = RelayConfig(server.url("/").toString(), "family-token")
            assertThrows(RelayResponseTooLargeException::class.java) {
                RelayClient.fetchEnvelopesWithinResponseCap(
                    config,
                    listOf(ByteArray(8) { 2 }),
                    afterId = 7L,
                    limit = 2,
                )
            }
            assertTrue(server.takeRequest().path!!.contains("limit=2"))
            assertTrue(server.takeRequest().path!!.contains("limit=1"))
        } finally {
            server.shutdown()
        }
    }

    private fun sampleOutboundEnvelope() = OutboundEnvelope(
        msgId = ByteArray(16) { 1 },
        recipientUserId = ByteArray(16) { 9 },
        chatId = ByteArray(16) { 9 },
        senderUserId = ByteArray(16) { 8 },
        kind = 1u,
        lamport = 1uL,
        timestamp = 1_700_000_000_000L,
        hopTtl = 7u,
        expiry = 1_700_000_060_000L,
        recipientHint = ByteArray(8) { 2 },
        sealed = "sealed".toByteArray(StandardCharsets.UTF_8),
    )

    private fun sampleReceiptEnvelope() = OutgoingReceiptEnvelope(
        msgId = ByteArray(16) { 6 },
        recipientUserId = ByteArray(16) { 9 },
        chatId = ByteArray(16) { 9 },
        senderUserId = ByteArray(16) { 8 },
        receiptType = 2u,
        throughLamport = 5uL,
        timestamp = 1_700_000_000_000L,
        hopTtl = 7u,
        expiry = 1_700_000_070_000L,
        recipientHint = ByteArray(8) { 7 },
        sealed = "receipt-sealed".toByteArray(StandardCharsets.UTF_8),
    )

    private fun base64Url(bytes: ByteArray): String =
        Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)
}
