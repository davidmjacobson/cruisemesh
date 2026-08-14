package com.cruisemesh.app.mesh

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class GroupDigestAnswersTest {
    private val groupA = ByteArray(16) { 0xA1.toByte() }
    private val groupB = ByteArray(16) { 0xB2.toByte() }

    @Test
    fun `an answered group is skipped on the same link`() {
        val answers = GroupDigestAnswers()
        answers.note("aa:bb", groupA)

        assertTrue(answers.answered("aa:bb", groupA))
    }

    @Test
    fun `other groups on the same link still get the fallback`() {
        val answers = GroupDigestAnswers()
        answers.note("aa:bb", groupA)

        assertFalse(answers.answered("aa:bb", groupB))
    }

    @Test
    fun `a second link to the same phone still gets the fallback`() {
        val answers = GroupDigestAnswers()
        answers.note("aa:bb", groupA)

        assertFalse(answers.answered("192.168.1.7:7000", groupA))
    }

    /**
     * The bug this class exists to prevent: a peer that reconnects without
     * sending a group digest -- a reinstall, a wiped database, a downgrade --
     * must get the lamport-0 catch-up again, not be skipped for the life of
     * the service.
     */
    @Test
    fun `a reconnect after disconnect gets the fallback again`() {
        val answers = GroupDigestAnswers()
        answers.note("aa:bb", groupA)
        answers.forget("aa:bb")

        assertFalse(answers.answered("aa:bb", groupA))
    }

    @Test
    fun `forgetting one link leaves the others intact`() {
        val answers = GroupDigestAnswers()
        answers.note("aa:bb", groupA)
        answers.note("cc:dd", groupA)
        answers.forget("aa:bb")

        assertFalse(answers.answered("aa:bb", groupA))
        assertTrue(answers.answered("cc:dd", groupA))
    }

    @Test
    fun `an equal group id from a different array still matches`() {
        val answers = GroupDigestAnswers()
        answers.note("aa:bb", groupA)

        assertTrue(answers.answered("aa:bb", groupA.copyOf()))
    }
}
