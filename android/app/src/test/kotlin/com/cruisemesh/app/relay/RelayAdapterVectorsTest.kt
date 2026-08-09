package com.cruisemesh.app.relay

import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.cruisemesh_core.CoreRelayAdapterVector
import uniffi.cruisemesh_core.CoreRelayHttpRequest
import uniffi.cruisemesh_core.OutboundEnvelope
import uniffi.cruisemesh_core.coreRelayAdapterVectors

/**
 * The exit criterion of the driver migration: for the slice C1 moves, the
 * request the core session forms and the request the legacy engine sends are
 * the same bytes.
 *
 * Both are put on the wire against one server and the two recordings are
 * compared, rather than each being asserted against a written-down
 * expectation. That difference matters. A test that asserted "the path is
 * `/envelopes`" twice would pass while both engines sent the wrong path
 * together, which is precisely the failure a migration produces: the new code
 * is written by reading the old code, so the two agree on a mistake.
 * Comparing recordings can only pass when they genuinely agree, and comparing
 * both against `coreRelayAdapterVectors` -- the table iOS will assert in C2 --
 * is what stops them agreeing on something the core session does not actually
 * send.
 *
 * # The one difference, stated rather than absorbed
 *
 * A [CoreRelayHttpRequest] carries the protocol headers and only those:
 * `Authorization`, `Content-Type`, `Accept`. The user agent and the
 * tunnel-bypass hint belong to the HTTP client rather than to the protocol,
 * and both engines get them from the same place
 * ([RelayClient.openTransport]), so they appear identically in both
 * recordings and are compared like everything else. Nothing is excluded from
 * the comparison.
 */
class RelayAdapterVectorsTest {

    @Test
    fun `every adapter vector is what the legacy engine already sends`() {
        for (vector in coreRelayAdapterVectors()) {
            compare(vector)
        }
    }

    @Test
    fun `the table covers the whole relay surface a driver executes`() {
        assertEquals(
            listOf("post-envelope", "fetch-page", "ack-page", "presence"),
            coreRelayAdapterVectors().map { it.name },
        )
    }

    private fun compare(vector: CoreRelayAdapterVector) {
        val server = MockWebServer()
        server.enqueue(reply(vector.name))
        server.enqueue(reply(vector.name))
        server.start()
        try {
            val base = normalizeRelayUrl(server.url("/").toString())
            val config = RelayConfig(base, TOKEN)

            runLegacy(vector.name, config)
            val legacy = server.takeRequest()

            CoreRelayDriver.execute(
                passId = "p1",
                actionId = 1u,
                request = vector.request.copy(baseUrl = base),
                network = null,
                nowMs = 1_700_000_000_000L,
            )
            val driven = server.takeRequest()

            assertEquals("${vector.name}: method", legacy.method, driven.method)
            assertEquals("${vector.name}: path", legacy.path, driven.path)
            assertArrayEquals(
                "${vector.name}: body bytes",
                legacy.body.readByteArray(),
                driven.body.readByteArray(),
            )
            assertEquals("${vector.name}: headers", headers(legacy), headers(driven))

            // And both are the vector, so C2's Swift suite is asserting the
            // same bytes rather than a Swift-shaped reading of them.
            assertEquals("${vector.name}: vector path", vector.request.path, driven.path)
            assertEquals("${vector.name}: vector method", vector.request.method, driven.method)
            for (header in vector.request.headers) {
                assertEquals(
                    "${vector.name}: vector header ${header.name}",
                    header.value,
                    driven.getHeader(header.name),
                )
            }
        } finally {
            server.shutdown()
        }
    }

    /**
     * Every header the request actually carried, normalised for comparison
     * rather than filtered: `Host` and `Content-Length` are derived from the
     * connection and would differ only if the two requests differed.
     */
    private fun headers(request: RecordedRequest): Map<String, String> =
        request.headers.toMultimap()
            .mapKeys { it.key.lowercase() }
            .mapValues { it.value.joinToString(",") }

    private fun runLegacy(name: String, config: RelayConfig) {
        when (name) {
            "post-envelope" -> RelayClient.postOutboundEnvelope(config, vectorEnvelope())
            "fetch-page" -> RelayClient.fetchEnvelopes(config, HINTS, AFTER_ID, LIMIT)
            "ack-page" -> RelayClient.ackEnvelopes(config, ACK_IDS)
            "presence" -> RelayClient.syncPresence(config, listOf(HINT_A), listOf(HINT_B))
            else -> error("no legacy call is wired for the $name vector")
        }
    }

    private fun reply(name: String): MockResponse = MockResponse().setResponseCode(200).setBody(
        when (name) {
            "post-envelope" -> """{"id":7}"""
            "fetch-page" -> """{"envelopes":[],"next_cursor":$AFTER_ID}"""
            "presence" -> """{"now_ms":1700000000000,"presence":[]}"""
            else -> "{}"
        },
    )

    /** The `post-envelope` vector's own field values, as a queued row. */
    private fun vectorEnvelope() = OutboundEnvelope(
        msgId = ByteArray(16) { 0x11 },
        recipientUserId = ByteArray(32) { 0x55 },
        chatId = ByteArray(32) { 0x66 },
        senderUserId = ByteArray(32) { 0x77 },
        kind = 1u,
        lamport = 1uL,
        timestamp = 1_700_000_000_000L,
        hopTtl = 4u,
        expiry = 1_700_000_000_000L,
        recipientHint = HINT_A,
        sealed = ByteArray(48) { 0x33 },
    )

    private companion object {
        const val TOKEN = "member-token"
        const val AFTER_ID = 8L
        const val LIMIT = 256
        val HINT_A = ByteArray(8) { 0x22 }
        val HINT_B = ByteArray(8) { 0x44 }
        val HINTS = listOf(HINT_A, HINT_B)
        val ACK_IDS = listOf(3L, 5L, 8L)
    }
}
