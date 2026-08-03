package com.cruisemesh.app

import android.content.SharedPreferences

/**
 * Commit a preference edit either lazily or durably.
 *
 * `apply()` hands the write to a background thread and returns immediately,
 * which is the right trade for ordinary settings. Restore is not ordinary: it
 * finishes by hard-exiting the process (so the identity and message store are
 * re-read cleanly on the next launch), and that exit kills the process before
 * the queued writes reach disk. The symptom was silent and severe -- a restored
 * phone kept its messages but came back with a freshly minted `user_id`,
 * no display name and no relay endpoint, because those three writes were still
 * in flight when the process died.
 *
 * So every write on a path that is about to exit the process must pass
 * `durable = true` and take the synchronous `commit()`.
 */
internal fun SharedPreferences.Editor.persist(durable: Boolean) {
    if (durable) {
        commit()
    } else {
        apply()
    }
}
