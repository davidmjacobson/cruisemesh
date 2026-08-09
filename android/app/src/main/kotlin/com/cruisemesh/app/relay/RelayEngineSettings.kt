package com.cruisemesh.app.relay

import android.content.Context

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
 * either a migration to drop nothing or a dead column kept forever. Two
 * preference keys simply stop being read.
 *
 * There is no user-facing string here and no screen: this is an internal
 * switch for a closed-test canary. If it ever grows a control in Advanced, its
 * label goes through `strings.xml` like every other piece of copy.
 */
object RelayEngineSettings {

    private const val PREFS_NAME = "cruisemesh_relay"

    /** Absent means [RelayPassEngine.LEGACY]. */
    private const val PREF_PASS_ENGINE = "pass_engine_core"

    private const val PREF_SHADOW_ENABLED = "pass_engine_shadow"

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
     * Defaults to on, because a canary nobody switches on is not a canary. It
     * costs a bounded handful of comparisons a day and no network at all; see
     * `RelayShadowAdapter`.
     */
    fun shadowEnabled(context: Context): Boolean =
        prefs(context).getBoolean(PREF_SHADOW_ENABLED, true)

    fun setShadowEnabled(context: Context, enabled: Boolean) {
        prefs(context).edit().putBoolean(PREF_SHADOW_ENABLED, enabled).apply()
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
