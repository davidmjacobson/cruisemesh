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

    @Test
    fun `callbacks from the socket this client is actually using are acted on`() {
        assertTrue(isCurrentPushSocket(stopped = false, currentGeneration = 7L, callbackGeneration = 7L))
    }

    @Test
    fun `callbacks from a socket that was deliberately replaced are ignored`() {
        // Cancelling a socket does not silence it: OkHttp delivers onFailure
        // for the cancel afterwards, on its own thread. resubscribe() replaces
        // a socket precisely so a changed subscribe cursor reaches relayd, and
        // it cannot fall back on the `stopped` flag the way stop() does -- the
        // client is still meant to be running. Acting on the dead socket's
        // callback would null out its successor's reference and schedule a
        // reconnect beside it: two sockets, one unreachable and never closed.
        assertFalse(isCurrentPushSocket(stopped = false, currentGeneration = 8L, callbackGeneration = 7L))
    }

    @Test
    fun `callbacks arriving after stop are ignored whatever the generation`() {
        assertFalse(isCurrentPushSocket(stopped = true, currentGeneration = 7L, callbackGeneration = 7L))
    }
}
