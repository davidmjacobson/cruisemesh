package com.cruisemesh.app.mesh

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class WifiHoldPolicyTest {
    @Test
    fun holdsWhenNoVpnOwnsTheDefaultRoute() {
        assertTrue(WifiHoldPolicy.shouldHold(isVpnDefault = false))
    }

    @Test
    fun noOpUnderAVpn() {
        assertFalse(WifiHoldPolicy.shouldHold(isVpnDefault = true))
    }
}

class WifiDropPolicyTest {
    private val joined = 1_000_000L

    @Test
    fun countsAnEarlyDropWhileCellularStaysUp() {
        assertTrue(
            WifiDropPolicy.isPrematureDrop(
                joinedAtMs = joined,
                lostAtMs = joined + 30_000,
                cellularUp = true,
            ),
        )
    }

    @Test
    fun ignoresDropsWhenAllConnectivityIsGone() {
        // No cellular either -> the user genuinely lost coverage, not adaptive
        // connectivity dropping an internet-less association.
        assertFalse(
            WifiDropPolicy.isPrematureDrop(
                joinedAtMs = joined,
                lostAtMs = joined + 30_000,
                cellularUp = false,
            ),
        )
    }

    @Test
    fun ignoresDropsLongAfterJoining() {
        assertFalse(
            WifiDropPolicy.isPrematureDrop(
                joinedAtMs = joined,
                lostAtMs = joined + WifiDropPolicy.PREMATURE_WINDOW_MS + 1,
                cellularUp = true,
            ),
        )
    }

    @Test
    fun ignoresAClockThatWentBackwards() {
        assertFalse(
            WifiDropPolicy.isPrematureDrop(
                joinedAtMs = joined,
                lostAtMs = joined - 5_000,
                cellularUp = true,
            ),
        )
    }

    @Test
    fun tipShowsOnlyAfterThePatternRepeats() {
        assertFalse(WifiDropPolicy.shouldShowTip(0))
        assertFalse(WifiDropPolicy.shouldShowTip(1))
        assertTrue(WifiDropPolicy.shouldShowTip(2))
        assertTrue(WifiDropPolicy.shouldShowTip(5))
    }
}
