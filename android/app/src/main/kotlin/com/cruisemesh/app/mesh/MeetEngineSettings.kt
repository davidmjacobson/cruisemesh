package com.cruisemesh.app.mesh

import android.content.Context

/**
 * Which engine sequences one encounter -- everything that happens between two
 * phones from the moment they identify each other until the burst is over.
 *
 * One selection per encounter, and deliberately no third value. A per-lane mix
 * -- core picks the digest, the shell picks the drain -- is the shape of
 * rollback nobody can reason about: the digest, the targeted drain and the
 * mule spray share one carried-offer reservation, one byte allowance and one
 * cadence verdict, so two owners inside one encounter would each spend the
 * other's budget. The rollback story is "the next encounter runs the other
 * engine", nothing finer. Same shape as [InboundEngine].
 */
enum class MeetEngine {
    /** The sequencing [MeshService] has run since before this refactor. */
    LEGACY,

    /** `MessageStore.corePlanMeshMeet`, driven through [CoreMeetAdapter]. */
    CORE,
}

/**
 * The internal encounter settings that are not a person's to change: which
 * engine sequences one encounter.
 *
 * Kept in preferences rather than in the message store, for the same reason
 * [InboundEngineSettings] is: the whole selection has to be removable once the
 * legacy sequencing is deleted, and a flag that had earned a column in the
 * store's schema would not be. A preference key simply stops being read.
 *
 * The switch is reachable from the developer settings screen. It has to be: a
 * closed-test build is release-signed, `run-as` is refused on it, and a
 * rollout switch that can only be set by a JVM test can never produce the
 * field evidence it exists to produce.
 *
 * There is deliberately no shadow comparison here. Running both sequencers
 * over one encounter would put two digests, two drains and two sprays on the
 * same link, so the comparison would itself be a far larger behaviour change
 * than the thing it set out to measure. Evidence comes from running the core
 * engine on a device and watching delivery.
 */
object MeetEngineSettings {

    private const val PREFS_NAME = "cruisemesh_mesh"

    /** Absent means [MeetEngine.LEGACY]. */
    private const val PREF_MEET_ENGINE = "meet_engine_core"

    /**
     * The engine the *next* encounter will use.
     *
     * Read once at the top of an encounter and not consulted again while that
     * encounter runs, so flipping the switch cannot split one burst between
     * two sequencers. Preferences are an in-memory map after the first read,
     * so this is a lock and a lookup, not a disk touch.
     *
     * Defaults to legacy, and stays there until field evidence says otherwise.
     * A default that quietly moved with a release would make "roll back" mean
     * "ship again".
     */
    fun meetEngine(context: Context): MeetEngine =
        if (prefs(context).getBoolean(PREF_MEET_ENGINE, false)) {
            MeetEngine.CORE
        } else {
            MeetEngine.LEGACY
        }

    fun setMeetEngine(context: Context, engine: MeetEngine) {
        prefs(context).edit()
            .putBoolean(PREF_MEET_ENGINE, engine == MeetEngine.CORE)
            .apply()
    }

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
}
