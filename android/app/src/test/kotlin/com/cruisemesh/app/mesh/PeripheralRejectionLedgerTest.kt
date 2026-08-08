package com.cruisemesh.app.mesh

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The bookkeeping behind the drop ladder for a central turned away at the
 * inbound link cap. The properties that matter are all about a *stale* ladder:
 * BLE addresses are reused within the ~15 min an RPA lives, so posted attempts
 * from a rejection that is over must not act on the connection that took the
 * address next.
 */
class PeripheralRejectionLedgerTest {

    @Test
    fun `a rejection owns its own generation and nothing else`() {
        val ledger = PeripheralRejectionLedger()

        val generation = ledger.reject("aa:bb")

        assertTrue(ledger.isRejected("aa:bb"))
        assertTrue(ledger.ownsRejection("aa:bb", generation))
        assertFalse(ledger.ownsRejection("aa:bb", generation + 1))
        assertFalse(ledger.ownsRejection("cc:dd", generation))
    }

    @Test
    fun `a reconnect retires the previous rejection's ladder`() {
        // The whole point: address X is rejected, disconnects, and reconnects
        // under the same address. The old ladder's remaining attempts must not
        // fire cancelConnection at the new connection.
        val ledger = PeripheralRejectionLedger()
        val stale = ledger.reject("aa:bb")

        ledger.clear("aa:bb")

        assertFalse(ledger.ownsRejection("aa:bb", stale))
        assertFalse(ledger.isRejected("aa:bb"))
    }

    @Test
    fun `a second rejection of the same address supersedes the first`() {
        val ledger = PeripheralRejectionLedger()
        val first = ledger.reject("aa:bb")
        val second = ledger.reject("aa:bb")

        assertNotEquals(first, second)
        assertFalse(ledger.ownsRejection("aa:bb", first))
        assertTrue(ledger.ownsRejection("aa:bb", second))
        // One address, one rejection -- superseding must not leave two.
        assertEquals(1, ledger.rejectedCount())
    }

    @Test
    fun `generations are never reused after a disconnect and reconnect cycle`() {
        // With a per-address counter, "reject, clear, reject" would hand the
        // second rejection the same number as the first, and a ladder left over
        // from the first would start matching again. One process-wide sequence
        // makes that impossible.
        val ledger = PeripheralRejectionLedger()
        val seen = mutableSetOf<Long>()

        repeat(20) {
            seen += ledger.reject("aa:bb")
            ledger.clear("aa:bb")
        }

        assertEquals(20, seen.size)
    }

    @Test
    fun `only the owning generation may end a rejection`() {
        // The end of the ladder adopts the link it could not drop, which puts
        // the inbound count over the cap. A stale ladder reaching that point
        // would adopt a connection whose own attempts had not been spent.
        val ledger = PeripheralRejectionLedger()
        val stale = ledger.reject("aa:bb")
        ledger.clear("aa:bb")
        val current = ledger.reject("aa:bb")

        assertFalse(ledger.clearIfOwned("aa:bb", stale))
        assertTrue(ledger.isRejected("aa:bb"))

        assertTrue(ledger.clearIfOwned("aa:bb", current))
        assertFalse(ledger.isRejected("aa:bb"))
        // ...and it is not clearable twice, so two ladders cannot both adopt.
        assertFalse(ledger.clearIfOwned("aa:bb", current))
    }

    @Test
    fun `clearing an address that was never rejected reports nothing to clear`() {
        val ledger = PeripheralRejectionLedger()

        assertFalse(ledger.clear("never-rejected"))
        assertFalse(ledger.isRejected("never-rejected"))
        assertEquals(0, ledger.rejectedCount())
    }

    @Test
    fun `rejections are per address`() {
        val ledger = PeripheralRejectionLedger()
        val a = ledger.reject("aa:bb")
        val b = ledger.reject("cc:dd")

        ledger.clear("aa:bb")

        assertFalse(ledger.isRejected("aa:bb"))
        assertTrue(ledger.ownsRejection("cc:dd", b))
        assertFalse(ledger.ownsRejection("cc:dd", a))
        assertEquals(1, ledger.rejectedCount())
    }

    @Test
    fun `clearAll retires every ladder for a restarted peripheral role`() {
        val ledger = PeripheralRejectionLedger()
        val a = ledger.reject("aa:bb")
        val b = ledger.reject("cc:dd")

        ledger.clearAll()

        assertEquals(0, ledger.rejectedCount())
        assertFalse(ledger.ownsRejection("aa:bb", a))
        assertFalse(ledger.ownsRejection("cc:dd", b))
    }
}
