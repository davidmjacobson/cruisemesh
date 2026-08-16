package com.cruisemesh.app.relay

import android.content.Context
import uniffi.cruisemesh_core.CoreRelayShadowSampler

/**
 * Which engine runs a relay pass.
 *
 * One selection for a whole pass, and there is deliberately no third value.
 * Per-stage mixing is the shape of rollback that cannot be reasoned about: a
 * pass whose uploads came from one engine and whose walk came from the other
 * has no single owner for the cursor it advanced, and no transcript that
 * explains what it did. The rollback story is "the next pass runs the other
 * engine", nothing finer.
 */
enum class RelayPassEngine {
    /** The engine that has been moving mail since before this refactor. */
    LEGACY,

    /** `CoreRelayPass`, driven through [CoreRelayDriver]. */
    CORE,
}

/**
 * The internal relay settings that are not a person's to change: which engine
 * runs a pass, and whether the migration canary may sample.
 *
 * Kept in the same preferences file as the rest of this device's relay
 * settings rather than in the message store, and that is load-bearing rather
 * than convenient. The whole selection has to be removable once the legacy
 * engine is deleted, and a flag that had earned a column in the store's schema
 * would not be: schemas here are forward-only, so removing it later would mean
 * either a migration to drop nothing or a dead column kept forever. A handful
 * of preference keys simply stop being read.
 *
 * The engine switch is reachable from the developer settings screen, behind the
 * same door as the manual relay fields. It has to be: a closed-test build is
 * release-signed, `run-as` is refused on it, and a canary whose flag can only
 * be set by a JVM test can never produce the evidence it exists to produce.
 */
object RelayEngineSettings {

    private const val PREFS_NAME = "cruisemesh_relay"

    /** Absent means [RelayPassEngine.LEGACY]. */
    private const val PREF_PASS_ENGINE = "pass_engine_core"

    private const val PREF_SHADOW_ENABLED = "pass_engine_shadow"

    /** A deliberate, user-facing opt-in for relay traffic on roaming data. */
    private const val PREF_ALLOW_ROAMING_DATA = "allow_roaming_data"

    private const val PREF_SHADOW_DAY = "pass_engine_shadow_day"
    private const val PREF_SHADOW_COUNT = "pass_engine_shadow_count"
    private const val PREF_SHADOW_LAST_MS = "pass_engine_shadow_last_ms"

    /**
     * The engine the *next* pass will use.
     *
     * Read once when a pass starts and not consulted again, so flipping the
     * switch mid-pass changes nothing about the pass already running. A pass
     * that could change engines halfway is exactly the per-stage mix
     * [RelayPassEngine] exists to forbid.
     *
     * Defaults to legacy, and stays there until canary evidence says
     * otherwise. A default that quietly moved with a release would make
     * "roll back" mean "ship again".
     */
    fun passEngine(context: Context): RelayPassEngine =
        if (prefs(context).getBoolean(PREF_PASS_ENGINE, false)) {
            RelayPassEngine.CORE
        } else {
            RelayPassEngine.LEGACY
        }

    fun setPassEngine(context: Context, engine: RelayPassEngine) {
        prefs(context).edit()
            .putBoolean(PREF_PASS_ENGINE, engine == RelayPassEngine.CORE)
            .apply()
    }

    /**
     * Whether the migration canary may sample legacy passes.
     *
     * Defaults to **on**, and that is worth stating plainly rather than
     * filing under "the default path is unchanged": a shipped device runs the
     * canary. What it does not do is change anything the device sends,
     * receives, marks or stores -- it reads what the pass already observed and
     * writes one bounded diagnostics record on a handful of passes a day. The
     * mail-moving path is byte-identical with it on or off; that is the claim,
     * and it is the one the tests pin.
     */
    fun shadowEnabled(context: Context): Boolean =
        prefs(context).getBoolean(PREF_SHADOW_ENABLED, true)

    fun setShadowEnabled(context: Context, enabled: Boolean) {
        prefs(context).edit().putBoolean(PREF_SHADOW_ENABLED, enabled).apply()
    }

    fun allowsRoamingData(context: Context): Boolean =
        prefs(context).getBoolean(PREF_ALLOW_ROAMING_DATA, false)

    fun setAllowsRoamingData(context: Context, enabled: Boolean) {
        prefs(context).edit().putBoolean(PREF_ALLOW_ROAMING_DATA, enabled).apply()
    }

    /**
     * The canary's sampling state, across process launches.
     *
     * Three integers, in the file the engine flag already lives in, and they
     * are what makes "a bounded number of samples a day" true. Held only in
     * memory, the count resets to zero on every service start and a fresh
     * sampler always samples its first pass -- so a phone whose foreground
     * service Android keeps killing and restarting would sample nearly every
     * pass, which is the opposite of a bound.
     */
    fun shadowSampler(context: Context): CoreRelayShadowSampler = prefs(context).let {
        CoreRelayShadowSampler(
            dayIndex = it.getLong(PREF_SHADOW_DAY, 0L),
            samplesToday = it.getInt(PREF_SHADOW_COUNT, 0).coerceAtLeast(0).toUInt(),
            lastSampleAtMs = it.getLong(PREF_SHADOW_LAST_MS, 0L),
        )
    }

    fun setShadowSampler(context: Context, state: CoreRelayShadowSampler) {
        prefs(context).edit()
            .putLong(PREF_SHADOW_DAY, state.dayIndex)
            .putInt(PREF_SHADOW_COUNT, state.samplesToday.toInt())
            .putLong(PREF_SHADOW_LAST_MS, state.lastSampleAtMs)
            .apply()
    }

    private fun prefs(context: Context) =
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
}

/**
 * Whether the canary may run at all, given the engine this pass chose.
 *
 * A rule rather than a condition inlined at the call site, because it is the
 * one that keeps the mechanism honest: shadowing the core engine with the core
 * planner compares a thing to itself and can only ever agree, so every record
 * it produced would be evidence of nothing while looking exactly like evidence
 * of something.
 */
fun relayShadowPermitted(engine: RelayPassEngine, shadowEnabled: Boolean): Boolean =
    shadowEnabled && engine == RelayPassEngine.LEGACY
