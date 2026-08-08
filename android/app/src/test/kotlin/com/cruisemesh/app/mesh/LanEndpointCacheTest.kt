package com.cruisemesh.app.mesh

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import com.cruisemesh.app.chat.UserIdHex
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.cruisemesh_core.LanEndpointCacheDecision
import uniffi.cruisemesh_core.LanEndpointCacheEntry
import uniffi.cruisemesh_core.LanEndpointProvenance
import uniffi.cruisemesh_core.lanEndpointCacheDecision
import uniffi.cruisemesh_core.lanEndpointCacheDecode

@RunWith(AndroidJUnit4::class)
class LanEndpointCacheTest {
    private val context: Context get() = ApplicationProvider.getApplicationContext()
    private val networkId = "NcJ68sf-sL-VO63PUTnngg=="
    private val userId = ByteArray(32) { 7 }
    private val savedAt = 1_000L
    private val now = 2_000L

    /** This phone's own address: 192.168.86.0/24, the field case's network. */
    private val localHost = "192.168.86.31"

    @Before
    fun clearStoredEndpoints() {
        context.getSharedPreferences("cruisemesh_lan_endpoints", Context.MODE_PRIVATE)
            .edit().clear().commit()
    }

    /** Writes the exact bytes a build without provenance wrote. */
    private fun writeLegacyValue(host: String, port: Int, savedAtMs: Long) {
        val encoded = android.util.Base64.encodeToString(
            host.toByteArray(Charsets.UTF_8),
            android.util.Base64.NO_WRAP or android.util.Base64.URL_SAFE,
        )
        context.getSharedPreferences("cruisemesh_lan_endpoints", Context.MODE_PRIVATE)
            .edit()
            .putString("$networkId:${UserIdHex.encode(userId)}", "$encoded|$port|$savedAtMs")
            .commit()
    }

    private fun storedValue(): String? =
        context.getSharedPreferences("cruisemesh_lan_endpoints", Context.MODE_PRIVATE)
            .getString("$networkId:${UserIdHex.encode(userId)}", null)

    @Test
    fun manualEndpointDisplayHandlesIpv4AndIpv6() {
        assertEquals("10.0.0.2:45892", LanManualEndpoint("10.0.0.2", 45_892).display)
        assertEquals("[fe80::1234]:45892", LanManualEndpoint("fe80::1234", 45_892).display)
    }

    @Test
    fun malformedManualEndpointsRemainRejected() {
        assertNull(parseLanManualEndpoint("10.0.0.2:0", 45_892))
        assertNull(parseLanManualEndpoint("bad host", 45_892))
    }

    @Test
    fun aValueFromABuildWithoutProvenanceReadsAsUnproven() {
        writeLegacyValue("192.168.86.23", 45_892, savedAt)
        val entry = lanEndpointCacheDecode(storedValue()!!)
        assertEquals(
            LanEndpointCacheEntry(
                host = "192.168.86.23",
                port = 45_892u,
                savedAtMs = savedAt,
                provenance = LanEndpointProvenance.HINTED,
            ),
            entry,
        )
    }

    @Test
    fun aPoisonedEntryFromAShippedBuildIsEvictedOnLoad() {
        // The field case: a hint naming 10.80.209.68 was filed under the id of
        // the 192.168.86.0/24 network this phone is on, and cost a connect
        // timeout on every Wi-Fi join for the seven days it lived.
        writeLegacyValue("10.80.209.68", 45_892, savedAt)
        val cache = LanEndpointCache(context)

        assertNull(cache.load(networkId, userId, localHost, now))
        assertNull("the entry is deleted, not left to age out", storedValue())
    }

    @Test
    fun aLegacyEntryOnThisPhonesOwnSubnetSurvives() {
        writeLegacyValue("192.168.86.23", 45_892, savedAt)
        val cache = LanEndpointCache(context)

        assertEquals(
            LanManualEndpoint("192.168.86.23", 45_892),
            cache.load(networkId, userId, localHost, now),
        )
        assertTrue(storedValue() != null)
    }

    @Test
    fun anAuthenticatedEntryOnAnotherSubnetSurvives() {
        // A peer reached over a routed LAN is legitimately cross-subnet: the
        // handshake is proof the address answers from here, which no amount
        // of subnet comparison can supply.
        val cache = LanEndpointCache(context)
        cache.save(
            networkId,
            userId,
            LanManualEndpoint("10.80.209.68", 45_892),
            LanEndpointProvenance.AUTHENTICATED,
            savedAt,
        )

        assertEquals(
            LanManualEndpoint("10.80.209.68", 45_892),
            cache.load(networkId, userId, localHost, now),
        )
        assertTrue(storedValue() != null)
    }

    @Test
    fun aHandshakePromotesAStoredHintInPlace() {
        writeLegacyValue("10.80.209.68", 45_892, savedAt)
        val cache = LanEndpointCache(context)
        cache.save(
            networkId,
            userId,
            LanManualEndpoint("10.80.209.68", 45_892),
            LanEndpointProvenance.AUTHENTICATED,
            savedAt,
        )

        assertEquals(
            LanEndpointProvenance.AUTHENTICATED,
            lanEndpointCacheDecode(storedValue()!!)!!.provenance,
        )
        // And a hint repeating the same proven address does not undo it.
        cache.save(
            networkId,
            userId,
            LanManualEndpoint("10.80.209.68", 45_892),
            LanEndpointProvenance.HINTED,
            savedAt + 500,
        )
        assertEquals(
            LanEndpointProvenance.AUTHENTICATED,
            lanEndpointCacheDecode(storedValue()!!)!!.provenance,
        )
        assertEquals(
            LanManualEndpoint("10.80.209.68", 45_892),
            cache.load(networkId, userId, localHost, now),
        )
    }

    @Test
    fun anUnreadableStoredValueIsDiscarded() {
        context.getSharedPreferences("cruisemesh_lan_endpoints", Context.MODE_PRIVATE)
            .edit()
            .putString("$networkId:${UserIdHex.encode(userId)}", "nonsense")
            .commit()
        val cache = LanEndpointCache(context)

        assertNull(cache.load(networkId, userId, localHost, now))
        assertNull(storedValue())
    }

    @Test
    fun aValueCarryingAFieldThisBuildDoesNotKnowIsStillRead() {
        // Room for the next append. An unreadable value is deleted, so
        // rejecting a field this build has no name for would mean that adding
        // a fifth one later wipes the cache on any phone that rolls back here.
        val endpoint = LanManualEndpoint("10.80.209.68", 45_892)
        val cache = LanEndpointCache(context)
        cache.save(networkId, userId, endpoint, LanEndpointProvenance.AUTHENTICATED, savedAt)
        context.getSharedPreferences("cruisemesh_lan_endpoints", Context.MODE_PRIVATE)
            .edit()
            .putString(
                "$networkId:${UserIdHex.encode(userId)}",
                storedValue() + "|whatever-comes-next",
            )
            .commit()

        assertEquals(endpoint, cache.load(networkId, userId, localHost, now))
    }

    @Test
    fun aCachedHostFromAnOlderBuildIsDroppedUnlessItIsALocalAddress() {
        fun decisionFor(host: String, savedAtMs: Long = savedAt, nowMs: Long = now) =
            lanEndpointCacheDecision(
                LanEndpointCacheEntry(
                    host = host,
                    port = 45_892u,
                    savedAtMs = savedAtMs,
                    provenance = LanEndpointProvenance.AUTHENTICATED,
                ),
                localHost,
                nowMs,
            )

        // What older builds could have stored: the cache took any string.
        for (host in listOf("phone.local", "cruisemesh.app", "8.8.8.8", "")) {
            assertEquals(host, LanEndpointCacheDecision.EVICT, decisionFor(host))
        }
        // A real cached LAN address still works, and still expires.
        assertEquals(LanEndpointCacheDecision.USE, decisionFor("10.0.0.7"))
        assertEquals(LanEndpointCacheDecision.USE, decisionFor("fe80::1%wlan0"))
        assertEquals(
            LanEndpointCacheDecision.EVICT,
            decisionFor("10.0.0.7", nowMs = savedAt + 7 * 24 * 60 * 60_000 + 1),
        )
    }

    @Test
    fun anUnprovenEntryIsKeptWhenThisPhoneHasNothingToCompareWith() {
        // Not dialing is enough to stop the loop; deleting on a load that
        // cannot judge the entry would throw away a usable address because
        // the local interface happened to be unreadable.
        writeLegacyValue("10.80.209.68", 45_892, savedAt)
        val cache = LanEndpointCache(context)

        assertNull(cache.load(networkId, userId, localHost = null, nowMs = now))
        assertTrue(storedValue() != null)
    }

    @Test
    fun aHintForANonLocalHostIsNeverStoredAtAll() {
        val cache = LanEndpointCache(context)
        cache.save(
            networkId,
            userId,
            LanManualEndpoint("8.8.8.8", 45_892),
            LanEndpointProvenance.HINTED,
            savedAt,
        )
        assertNull(storedValue())
        assertFalse(cache.load(networkId, userId, localHost, now) != null)
    }

    @Test
    fun networkFingerprintUsesTheSharedIpv4Slash24() {
        assertEquals(
            "NcJ68sf-sL-VO63PUTnngg==",
            lanNetworkIdForIpv4("10.154.189.58"),
        )
        assertEquals(
            "NcJ68sf-sL-VO63PUTnngg==",
            lanNetworkIdForIpv4("10.154.189.201"),
        )
        assertNull(lanNetworkIdForIpv4("not-an-ip"))
    }
}
