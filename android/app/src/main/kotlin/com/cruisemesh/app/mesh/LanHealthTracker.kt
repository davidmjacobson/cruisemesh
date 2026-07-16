package com.cruisemesh.app.mesh

internal class LanHealthTracker(
    private val timeoutMs: Long = 20_000,
    private val maxConsecutiveTimeouts: Int = 3,
) {
    sealed interface Decision {
        data class Send(val nonce: ULong) : Decision
        data object Wait : Decision
        data object Close : Decision
    }

    private data class LinkState(
        val pendingNonce: ULong?,
        val sentAtMs: Long,
        val consecutiveTimeouts: Int,
    )

    private val links = mutableMapOf<String, LinkState>()

    fun next(address: String, nowMs: Long, nonce: ULong): Decision {
        val current = links[address]
        if (current == null || current.pendingNonce == null) {
            links[address] = LinkState(nonce, nowMs, current?.consecutiveTimeouts ?: 0)
            return Decision.Send(nonce)
        }
        if (nowMs - current.sentAtMs < timeoutMs) return Decision.Wait

        val failures = current.consecutiveTimeouts + 1
        if (failures >= maxConsecutiveTimeouts) {
            links.remove(address)
            return Decision.Close
        }
        links[address] = LinkState(nonce, nowMs, failures)
        return Decision.Send(nonce)
    }

    fun response(address: String, nonce: ULong, nowMs: Long): Long? {
        val current = links[address] ?: return null
        if (current.pendingNonce != nonce) return null
        links[address] = LinkState(
            pendingNonce = null,
            sentAtMs = 0,
            consecutiveTimeouts = 0,
        )
        return (nowMs - current.sentAtMs).coerceAtLeast(0)
    }

    fun remove(address: String) {
        links.remove(address)
    }

    fun clear() {
        links.clear()
    }
}
