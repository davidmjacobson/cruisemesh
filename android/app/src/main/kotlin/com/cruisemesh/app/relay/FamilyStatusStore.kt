package com.cruisemesh.app.relay

import android.util.Log
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import uniffi.cruisemesh_core.CoreFamilyStatus
import uniffi.cruisemesh_core.relayTokenIsDeposit

/**
 * The family's pass status, read once per app session.
 *
 * Once per session rather than per screen open, because what this carries is
 * a renewal date: it moves when someone renews, which is minutes at the very
 * fastest and months in the ordinary case. Polling it on every visit to the
 * Shore Pass screen would spend a round trip on a ship's Wi-Fi to re-learn a
 * date that has not moved, and the sync pass -- the thing that actually has to
 * be quick -- shares that link.
 *
 * A failed read leaves [status] as it was, which for a cold session is null:
 * the end-date line is something extra a screen shows when it knows, never a
 * surface that reports its own absence. The pass's *health* has its own live
 * signal ([com.cruisemesh.app.mesh.MeshConnectivityStatus.relay]) and is not
 * this object's business.
 *
 * Process-wide object with a [StateFlow], the same pattern as
 * [com.cruisemesh.app.mesh.MeshConnectivityStatus]. iOS holds the same
 * once-per-session read in its own status store.
 */
object FamilyStatusStore {

    private val _status = MutableStateFlow<CoreFamilyStatus?>(null)

    /** The last status read this session, or null if none has landed. */
    val status: StateFlow<CoreFamilyStatus?> = _status.asStateFlow()

    /**
     * The config the value in [status] describes, so swapping in a different
     * pass re-reads rather than showing the previous family's end date.
     */
    private var readFor: RelayConfig? = null

    private val lock = Mutex()

    /**
     * Read the status for [config] unless this session already has it.
     *
     * Safe to call from any screen that shows a pass, as often as it likes --
     * the first call does the work and the rest return immediately.
     */
    suspend fun refresh(config: RelayConfig) {
        // A deposit credential is post-only; asking would earn the same
        // structured 403 the other read routes give it, so the round trip is
        // skipped rather than spent to be refused.
        if (relayTokenIsDeposit(config.relayToken)) return
        lock.withLock {
            if (readFor == config) return
            // A different pass than the one [status] describes. Drop the old
            // family's answer before asking rather than after: whatever is
            // shown while the new read is in flight must not be the previous
            // family's end date.
            if (readFor != null) {
                readFor = null
                _status.value = null
            }
            val fetched = runCatching {
                withContext(Dispatchers.IO) { RelayClient.fetchFamilyStatus(config) }
            }.getOrElse { error ->
                // A screen the reader has left cancels this, and a cancelled
                // read is not a failed one -- it must reach the caller as a
                // cancellation, not as a swallowed exception and a log line.
                currentCoroutineContext().ensureActive()
                // Already logged by the client with its status line. Nothing
                // here is worth telling anyone: the screen simply shows no
                // end-date line, exactly as it does before the first read.
                Log.i(RelayClient.TAG, "Family status unavailable: ${error.javaClass.simpleName}")
                return
            }
            readFor = config
            _status.value = fetched
        }
    }

    /** Drops the cached status, for a test or a pass that was just replaced. */
    fun clear() {
        readFor = null
        _status.value = null
    }
}
