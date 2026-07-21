package com.cruisemesh.app.mesh

/**
 * Pure, Android-import-free battery policy shared by [BleCentral] (scan
 * duty), [BlePeripheral] (advertise duty), and [MeshService] (relay poll
 * cadence) -- the TODO.md §3.4 pattern ("pure schedule/policy logic = plain
 * classes, `@Synchronized` leaf monitors, no Android imports, unit-tested
 * directly"), same as [GattWriteQueue]/[ReconnectBackoffTracker]/
 * [WifiHoldPolicy].
 *
 * ## Duty mode (scan + advertise)
 *
 * A single [RadioDutyMode] output is reused for both BLE roles --
 * [BleCentral] maps it to `ScanSettings.SCAN_MODE_*`, [BlePeripheral] to
 * `AdvertiseSettings.ADVERTISE_MODE_*` -- so when this device is worth being
 * loud about (about to find a peer, or a peer might be looking for us) both
 * roles turn up together, and when it's quiet both turn down together.
 * Default is [RadioDutyMode.LOW_POWER]; [evaluate] escalates to
 * [RadioDutyMode.BALANCED] whenever any of, per [shouldEscalate]:
 *  - the screen is on and there are zero live links (actively unreachable,
 *    no reason to be quiet about it).
 *  - a link connected or disconnected within [ESCALATION_WINDOW_MS] (a
 *    disconnected peer's readvertised address, or a peer that just found
 *    us, is worth a faster duty cycle for a while).
 *  - the carry queue holds mail for a contact we're not currently linked to.
 *    The store has no per-recipient carried count today (TODO.md T2 only
 *    added an aggregate `carriedLen()`), so
 *    [RadioPowerInputs.carryQueueHasUnlinkedMail] is approximated as
 *    "carrying anything at all" -- a false positive (carrying only for
 *    already-linked contacts) just means a few extra quiet-period minutes at
 *    BALANCED, never a correctness problem.
 *
 * ## Android scan-throttling hysteresis
 *
 * Android throttles apps that start/stop a BLE scan more than 5 times in a
 * rolling 30s window. [evaluate] never downshifts BALANCED -> LOW_POWER
 * before [MIN_DWELL_MS] (60s) has passed since the mode last changed, but
 * always upshifts LOW_POWER -> BALANCED immediately on any trigger above --
 * "escalate fast, relax slow." Because a fresh BALANCED period always
 * requires another full [MIN_DWELL_MS] before it can downshift again, mode
 * churn is bounded to at most ~2 real transitions per [MIN_DWELL_MS] window
 * (one downshift, immediately followed by at most one upshift), comfortably
 * under the 5-per-30s throttle threshold. [evaluate] itself is cheap and
 * side-effect-free to call as often as useful -- once per periodic tick *and*
 * immediately from every link-connect/disconnect callback -- since it only
 * actually changes [mode] when the escalate condition or the dwell timer
 * calls for it. Callers must apply the change (stop-then-start the
 * scan/advertise, per the existing SCAN_FAILED_ALREADY_STARTED /
 * ADVERTISE_FAILED_ALREADY_STARTED restart path in each class) only when the
 * returned [RadioDutyMode] actually differs from what's currently applied --
 * [BleCentral.setScanDutyMode] and [BlePeripheral.setAdvertiseDutyMode] both
 * carry their own idempotence guard for exactly this, so callers can call
 * them unconditionally on every [evaluate] result.
 *
 * ## A2DP coexistence
 *
 * This class deliberately does not take Bluetooth-audio state as an input.
 * [MeshService] no longer pauses the BLE roles for A2DP audio (2026-07-09
 * policy change -- see `MeshService.refreshBluetoothAudioStatus`'s KDoc);
 * before this class existed, scan mode was unconditionally
 * `SCAN_MODE_BALANCED`, which is this class's *most* aggressive possible
 * output. Since [evaluate] only asks for BALANCED in the specific situations
 * above and otherwise stays at the strictly quieter LOW_POWER, this policy
 * can never make scanning more aggressive than the status quo it replaces --
 * there is nothing to compose with today. If A2DP-aware scanning is ever
 * reintroduced, it must clamp this policy's output *down* (e.g. force
 * LOW_POWER while audio is connected), never escalate past what [evaluate]
 * already asked for.
 *
 * ## Relay poll cadence
 *
 * Independent of duty mode: [relayPollIntervalMs] decides how often
 * `MeshService.relayPollRunnable` reposts itself. DTN audit finding F1
 * already made the WS push path ([RelayPushClient]) call `requestRelaySync`
 * on every pushed envelope, so the poll only needs to be a safety net while
 * push is healthy -- the poll itself stays correctness-authoritative; only
 * its cadence changes. [relayPollIntervalMs] is a plain function of the
 * previous/current health pair (not part of [evaluate]'s stateful dwell
 * tracking) so [MeshService] can call it both from the poll tick itself and
 * from [RelayPushClient]'s health-change callback for the immediate
 * healthy-to-down reschedule.
 */
enum class RadioDutyMode { LOW_POWER, BALANCED }

/** Inputs [RadioPowerPolicy.evaluate] needs on every tick; gathered by [MeshService]. */
data class RadioPowerInputs(
    val screenInteractive: Boolean,
    val liveLinkCount: Int,
    val msSinceLastLinkChange: Long,
    val carryQueueHasUnlinkedMail: Boolean,
)

class RadioPowerPolicy {
    private var mode = RadioDutyMode.LOW_POWER
    private var modeChangedAtMs = Long.MIN_VALUE

    /**
     * Recomputes duty mode for [inputs] as of [nowMs] and returns it. See the
     * class doc for why this is safe (and expected) to call repeatedly.
     */
    fun evaluate(inputs: RadioPowerInputs, nowMs: Long): RadioDutyMode {
        val escalate = shouldEscalate(inputs)
        val next = nextDutyMode(mode, escalate, nowMs - modeChangedAtMs)
        if (next != mode) {
            mode = next
            modeChangedAtMs = nowMs
        }
        return mode
    }

    /** Current mode without recomputing -- for diagnostics/tests. */
    fun currentMode(): RadioDutyMode = mode

    companion object {
        /** Minimum time at BALANCED before [nextDutyMode] allows a downshift -- see the class doc's throttling section. */
        const val MIN_DWELL_MS: Long = 60_000L

        /** A link connect/disconnect within this window keeps duty mode escalated. */
        const val ESCALATION_WINDOW_MS: Long = 5 * 60_000L

        /** Safety-net relay-poll cadence while [RelayPushClient]'s WS push is healthy. */
        const val RELAY_POLL_HEALTHY_MS: Long = 900_000L

        /** Relay-poll cadence while push is unhealthy or has never connected -- the original fixed interval. */
        const val RELAY_POLL_UNHEALTHY_MS: Long = 60_000L

        /** One-shot reschedule delay right after a healthy-to-down push transition, to catch anything the dying socket missed. */
        const val RELAY_POLL_TRANSITION_MS: Long = 5_000L

        /**
         * Whether duty mode should be (or stay) escalated right now. Exposed
         * at companion level -- alongside [nextDutyMode] and
         * [relayPollIntervalMs] -- so each rule is independently
         * unit-testable without needing an instance's dwell state.
         */
        internal fun shouldEscalate(inputs: RadioPowerInputs): Boolean {
            val lonelyWhileAwake = inputs.screenInteractive && inputs.liveLinkCount == 0
            val recentLinkChurn = inputs.msSinceLastLinkChange in 0..ESCALATION_WINDOW_MS
            return lonelyWhileAwake || recentLinkChurn || inputs.carryQueueHasUnlinkedMail
        }

        /**
         * The dwell/hysteresis rule in isolation: upshift immediately when
         * [escalate], downshift only once [msSinceModeChanged] has reached
         * [MIN_DWELL_MS].
         */
        internal fun nextDutyMode(current: RadioDutyMode, escalate: Boolean, msSinceModeChanged: Long): RadioDutyMode =
            when {
                escalate -> RadioDutyMode.BALANCED
                current == RadioDutyMode.BALANCED && msSinceModeChanged >= MIN_DWELL_MS -> RadioDutyMode.LOW_POWER
                else -> current
            }

        /**
         * Next relay-poll interval given whether push was healthy at the
         * last decision ([previouslyHealthy], null before any decision has
         * been made yet) and whether it's healthy now ([currentlyHealthy]).
         * The healthy-to-down transition gets one short interval so a missed
         * push during the dying socket is still caught quickly; every other
         * case just uses the steady-state interval for the current health.
         */
        fun relayPollIntervalMs(previouslyHealthy: Boolean?, currentlyHealthy: Boolean): Long = when {
            previouslyHealthy == true && !currentlyHealthy -> RELAY_POLL_TRANSITION_MS
            currentlyHealthy -> RELAY_POLL_HEALTHY_MS
            else -> RELAY_POLL_UNHEALTHY_MS
        }
    }
}
