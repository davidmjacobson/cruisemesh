package com.cruisemesh.app

import android.content.SharedPreferences
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Restore hard-exits the process the moment it finishes, so a write left on the
 * `apply()` background queue never lands. These pin which of the two commit
 * paths each caller takes; getting it wrong is silent on-device and costs the
 * restored identity.
 */
class PrefsPersistTest {

    private class RecordingEditor : SharedPreferences.Editor {
        val calls = mutableListOf<String>()

        override fun apply() {
            calls += "apply"
        }

        override fun commit(): Boolean {
            calls += "commit"
            return true
        }

        override fun putString(key: String, value: String?) = this
        override fun putStringSet(key: String, values: MutableSet<String>?) = this
        override fun putInt(key: String, value: Int) = this
        override fun putLong(key: String, value: Long) = this
        override fun putFloat(key: String, value: Float) = this
        override fun putBoolean(key: String, value: Boolean) = this
        override fun remove(key: String) = this
        override fun clear() = this
    }

    @Test
    fun `a durable write is synchronous so a process exit cannot outrun it`() {
        val editor = RecordingEditor()

        editor.persist(durable = true)

        assertEquals(listOf("commit"), editor.calls)
    }

    @Test
    fun `an ordinary write stays asynchronous`() {
        val editor = RecordingEditor()

        editor.persist(durable = false)

        assertEquals(listOf("apply"), editor.calls)
    }
}
