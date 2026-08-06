package com.cruisemesh.app.chat

import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class ChatSummaryRefreshCoordinatorTest {
    @Test
    fun eventDuringLoadDoesNotCancelItAndConflatesOneFollowUp() = runBlocking {
        val loads = AtomicInteger(0)
        val firstStarted = CompletableDeferred<Unit>()
        val releaseFirst = CompletableDeferred<Unit>()
        val firstCancelled = AtomicBoolean(false)
        val applied = Channel<Int>(Channel.UNLIMITED)
        val coordinator = ChatSummaryRefreshCoordinator(
            scope = this,
            debounceMs = 10,
            maxLatencyMs = 100,
            load = {
                val number = loads.incrementAndGet()
                if (number == 1) {
                    firstStarted.complete(Unit)
                    try {
                        releaseFirst.await()
                    } catch (cancelled: CancellationException) {
                        firstCancelled.set(true)
                        throw cancelled
                    }
                }
                number
            },
            onLoaded = { applied.trySend(it).getOrThrow() },
        )

        coordinator.request(immediate = true)
        withTimeout(1_000) { firstStarted.await() }
        coordinator.request(immediate = false)
        delay(30)
        assertFalse(firstCancelled.get())

        releaseFirst.complete(Unit)
        assertEquals(1, withTimeout(1_000) { applied.receive() })
        assertEquals(2, withTimeout(1_000) { applied.receive() })
        assertEquals(2, loads.get())
    }

    @Test
    fun sustainedEventsStillRefreshAtMaximumLatency() = runBlocking {
        val started = CompletableDeferred<Unit>()
        val coordinator = ChatSummaryRefreshCoordinator(
            scope = this,
            debounceMs = 60,
            maxLatencyMs = 120,
            load = {
                started.complete(Unit)
                Unit
            },
            onLoaded = {},
        )
        val storm = launch {
            repeat(20) {
                coordinator.request(immediate = false)
                delay(30)
            }
        }

        withTimeout(400) { started.await() }
        assertTrue(storm.isActive)
        storm.cancelAndJoin()
    }
}
