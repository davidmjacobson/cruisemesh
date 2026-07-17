package com.cruisemesh.app.mesh

import uniffi.cruisemesh_core.CoreLanHealthAction
import uniffi.cruisemesh_core.CoreLanHealthTracker

internal class LanHealthTracker(timeoutMs: Long = 20_000, maxConsecutiveTimeouts: Int = 3) {
    sealed interface Decision {
        data class Send(val nonce: ULong) : Decision
        data object Wait : Decision
        data object Close : Decision
    }

    private val core = CoreLanHealthTracker(timeoutMs, maxConsecutiveTimeouts.toUInt())

    fun next(address: String, nowMs: Long, nonce: ULong): Decision {
        val decision = core.next(address, nowMs, nonce)
        return when (decision.action) {
            CoreLanHealthAction.SEND -> Decision.Send(requireNotNull(decision.nonce))
            CoreLanHealthAction.WAIT -> Decision.Wait
            CoreLanHealthAction.CLOSE -> Decision.Close
        }
    }

    fun response(address: String, nonce: ULong, nowMs: Long): Long? = core.response(address, nonce, nowMs)
    fun remove(address: String) = core.remove(address)
    fun clear() = core.clear()
}
