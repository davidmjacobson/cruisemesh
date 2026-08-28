package com.cruisemesh.app.identity

import android.content.Context
import com.cruisemesh.app.persist

private const val PREFS_NAME = "cruisemesh_onboarding"
private const val PREF_COMPLETED = "completed"
private const val PREF_PERMISSIONS_STEP_DONE = "permissions_step_done"

/** Persists whether the first-run onboarding flow has already been completed. */
object OnboardingStore {

    fun isCompleted(context: Context): Boolean {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        if (prefs.contains(PREF_COMPLETED)) {
            return prefs.getBoolean(PREF_COMPLETED, false)
        }
        // Legacy installs already have a message store on disk; do not block
        // them behind onboarding after an app update.
        return context.applicationContext.filesDir.resolve("cruisemesh.sqlite").exists()
    }

    /**
     * @param durable when the caller is about to exit the process (restore),
     *   write synchronously so the flag cannot be lost in flight.
     */
    fun markCompleted(context: Context, durable: Boolean = false) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(PREF_COMPLETED, true)
            .persist(durable)
    }

    /**
     * Whether this phone has been walked through the permissions step, or
     * `null` when nothing ever recorded an answer.
     *
     * Three states rather than two, because the two doors that mark setup
     * complete without ever showing the step ([markPermissionsStepPending])
     * have to be told apart from the installs that predate this flag. A
     * `false` here means "a route skipped the step and owes it"; a missing
     * value means "this install is older than the question" and must not be
     * pulled back into first-run setup by an app update.
     */
    fun permissionsStepDone(context: Context): Boolean? {
        val prefs = context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
        return if (prefs.contains(PREF_PERMISSIONS_STEP_DONE)) {
            prefs.getBoolean(PREF_PERMISSIONS_STEP_DONE, true)
        } else {
            null
        }
    }

    /** The person has seen the permissions step and moved past it. */
    fun markPermissionsStepDone(context: Context, durable: Boolean = false) {
        setPermissionsStepDone(context, done = true, durable = durable)
    }

    /**
     * This phone was set up by a route that never showed the permissions step
     * — an own-device link or a backup restore — so it still owes one.
     */
    fun markPermissionsStepPending(context: Context, durable: Boolean = false) {
        setPermissionsStepDone(context, done = false, durable = durable)
    }

    private fun setPermissionsStepDone(context: Context, done: Boolean, durable: Boolean) {
        context.getSharedPreferences(PREFS_NAME, Context.MODE_PRIVATE)
            .edit()
            .putBoolean(PREF_PERMISSIONS_STEP_DONE, done)
            .persist(durable)
    }
}
