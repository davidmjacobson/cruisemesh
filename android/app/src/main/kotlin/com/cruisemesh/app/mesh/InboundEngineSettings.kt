package com.cruisemesh.app.mesh

import android.content.Context

/**
 * Which engine dispositions one inbound envelope.
 *
 * One selection per envelope, and there is deliberately no third value. A
 * per-stage mix -- core decides dedupe, the shell decides carry -- is the shape
 * of rollback nobody can reason about: the seen-set record, the carry row and
 * the relay ack all have to come from one owner or the DTN ordering rules stop
 * meaning anything. The rollback story is "the next envelope runs the other
 * engine", nothing finer.
 */
enum class InboundEngine {
    /** The engine that has been receiving mail since before this refactor. */
    LEGACY,

    /** `MessageStore.processInboundFrame`, driven through [CoreInboundAdapter]. */
    CORE,
}

/**
 * The internal receive settings that are not a person's to change: which engine
 * dispositions an inbound envelope.
 *
 * Kept in preferences rather than in the message store, for the same reason the
 * relay engine flag is: the whole selection has to be removable once the legacy
 * path is deleted, and a flag that had earned a column in the store's schema
 * would not be. A preference key simply stops being read.
 *
 * The switch is reachable from the internal tools screen. It has to be: a
 * closed-test build is release-signed, `run-as` is refused on it, and a rollout
 * switch that can only be set by a JVM test can never produce the field
 * evidence it exists to produce.
 *
 * There is deliberately no shadow comparison here, and that is the one place
 * this differs from the relay waves. A relay shadow works because the legacy
 * pass leaves a transcript a pure comparator can read after the fact. An
 * inbound envelope has no such transcript: running both engines over one frame
 * would mean opening, carrying and recording it twice, so the comparison would
 * itself be the behaviour change it exists to measure. Evidence comes from
 * running the core engine on a device and watching delivery, not from a
 * side-by-side.
 */
object InboundEngineSettings {

    private const val PREFS_NAME = "cruisemesh_mesh"

    /** Absent means [InboundEngine.LEGACY]. */
    private const val PREF_INBOUND_ENGINE = "inbound_engine_core"

    /**
     * The engine the *next* envelope will use.
     *
     * Read once per envelope, before the disposition starts, and not consulted
     * again -- so flipping the switch changes nothing about an envelope already
     * in flight. Preferences are an in-memory map after the first read, so this
     * is a lock and a lookup on the receive path, not a disk touch.
     *
     * Defaults to legacy, and stays there until field evidence says otherwise.
     * A default that quietly moved with a release would make "roll back" mean
     * "ship again".
     */
    fun inboundEngine(context: Context): InboundEngine =
        if (prefs(context).getBoolean(PREF_INBOUND_ENGINE, false)) {
            InboundEngine.CORE
        } else {
            InboundEngine.LEGACY
        }

    fun setInboundEngine(context: Context, engine: InboundEngine) {
        prefs(context).edit()
            .putBoolean(PREF_INBOUND_ENGINE, engine == InboundEngine.CORE)
            .apply()
    }

    private fun prefs(context: Context) =
        context.applicationContext.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
}
