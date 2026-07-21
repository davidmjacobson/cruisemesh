package com.cruisemesh.app.relay

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class RelayPushClientHintReplyTest {

    private val config = RelayConfig(relayUrl = "https://relay.example", relayToken = "tok")
    private val otherConfig = RelayConfig(relayUrl = "https://relay-2.example", relayToken = "tok-2")

    @Test
    fun `a reply for the still-desired config while running is current`() {
        assertTrue(isPushHintReplyCurrent(stopped = false, desiredConfig = config, replyConfig = config))
    }

    @Test
    fun `a reply arriving after stop is stale even if the config still matches`() {
        // FA3: RelayPushClient.stop() sets desiredConfig = null but a hint
        // computation kicked off before stop() can still land afterward --
        // must not resurrect a socket for a client that was told to stop.
        assertFalse(isPushHintReplyCurrent(stopped = true, desiredConfig = config, replyConfig = config))
    }

    @Test
    fun `a reply for a config that has since been superseded by a newer start is stale`() {
        // FA3: start() was called again with a different config while the
        // first config's hint computation was still in flight -- the stale
        // reply must not open a socket for the config we've already moved on
        // from.
        assertFalse(isPushHintReplyCurrent(stopped = false, desiredConfig = otherConfig, replyConfig = config))
    }

    @Test
    fun `a reply with no desired config at all (stopped mid-flight) is stale`() {
        assertFalse(isPushHintReplyCurrent(stopped = false, desiredConfig = null, replyConfig = config))
    }
}
