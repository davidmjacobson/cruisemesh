package com.cruisemesh.app.chat

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch

/**
 * Serial, non-cancelling home-summary refresh coordinator.
 *
 * The supplied [scope] must be serial (the Compose main scope in production).
 * Change events use a trailing debounce, plus a maximum latency so a sustained
 * storm still refreshes periodically. An active [load] is never cancelled;
 * one follow-up is conflated behind it instead.
 */
internal class ChatSummaryRefreshCoordinator<T>(
    private val scope: CoroutineScope,
    private val debounceMs: Long,
    private val maxLatencyMs: Long,
    private val load: suspend () -> T,
    private val onLoaded: (T) -> Unit,
) {
    init {
        require(debounceMs > 0)
        require(maxLatencyMs >= debounceMs)
    }

    private var debounceJob: Job? = null
    private var maxLatencyJob: Job? = null
    private var loadJob: Job? = null
    private var refreshAfterLoad = false

    fun request(immediate: Boolean = false) {
        scope.launch {
            if (immediate) {
                cancelTimers()
                startRefresh()
            } else {
                scheduleChangeRefresh()
            }
        }
    }

    private fun scheduleChangeRefresh() {
        debounceJob?.cancel()
        debounceJob = scope.launch {
            delay(debounceMs)
            triggerScheduledRefresh()
        }
        if (maxLatencyJob?.isActive != true) {
            maxLatencyJob = scope.launch {
                delay(maxLatencyMs)
                triggerScheduledRefresh()
            }
        }
    }

    private fun triggerScheduledRefresh() {
        cancelTimers()
        startRefresh()
    }

    private fun cancelTimers() {
        debounceJob?.cancel()
        maxLatencyJob?.cancel()
        debounceJob = null
        maxLatencyJob = null
    }

    private fun startRefresh() {
        if (loadJob?.isActive == true) {
            refreshAfterLoad = true
            return
        }
        loadJob = scope.launch {
            try {
                onLoaded(load())
            } finally {
                loadJob = null
                if (refreshAfterLoad) {
                    refreshAfterLoad = false
                    startRefresh()
                }
            }
        }
    }
}
