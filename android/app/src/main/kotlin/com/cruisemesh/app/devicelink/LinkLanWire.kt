package com.cruisemesh.app.devicelink

import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.EOFException
import java.io.IOException
import java.net.Inet4Address
import java.net.InetSocketAddress
import java.net.NetworkInterface
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException
import uniffi.cruisemesh_core.CoreLanEndpoint
import uniffi.cruisemesh_core.coreFormatLanEndpoint
import uniffi.cruisemesh_core.coreParseLanEndpoint

/**
 * The §13 gate's LAN leg: a plain TCP socket between the two phones, carrying
 * length-framed ceremony messages.
 *
 * There is deliberately no Noise here, and no contact check. The mesh's
 * same-LAN transport ([com.cruisemesh.app.mesh.LanTransport]) promotes a socket
 * only once the peer's Noise static key matches an accepted contact -- but the
 * two devices in a link ceremony are not contacts and never will be, and the
 * key they must match is the ephemeral one printed in the QR. That check
 * belongs to the ceremony, which makes it (`CoreLinkApprovingDevice` refuses
 * any peer whose static is not the scanned key) on a channel it establishes
 * itself. This socket's whole job is to move opaque bytes.
 *
 * Which means: this socket is untrusted, and is treated that way. Every read is
 * bounded, every message is length-capped, and nothing that arrives on it is
 * interpreted anywhere but inside the ceremony.
 */
internal class LinkLanWire(private val socket: Socket) : LinkWire {
    private val input = DataInputStream(socket.getInputStream().buffered())
    private val output = DataOutputStream(socket.getOutputStream().buffered())

    override fun send(bytes: ByteArray) {
        require(bytes.size <= LinkWireLimits.MAX_MESSAGE_BYTES) {
            "link message is too large to send: ${bytes.size}"
        }
        synchronized(output) {
            output.writeInt(bytes.size)
            output.write(bytes)
            output.flush()
        }
    }

    override fun receive(waitMs: Long): ByteArray? {
        socket.soTimeout = waitMs.coerceIn(1L, LinkWireLimits.MAX_RECEIVE_WAIT_MS).toInt()
        val length = try {
            input.readInt()
        } catch (_: SocketTimeoutException) {
            return null
        } catch (e: EOFException) {
            throw IOException("the other device closed the link", e)
        }
        if (length <= 0 || length > LinkWireLimits.MAX_MESSAGE_BYTES) {
            throw IOException("link message length is out of range: $length")
        }
        // The header arrived, so the body is on its way: read it without a
        // per-read deadline short enough to tear a message in half, but still
        // bounded so a peer that stops mid-message cannot hold the thread.
        socket.soTimeout = BODY_READ_TIMEOUT_MS
        val body = ByteArray(length)
        input.readFully(body)
        return body
    }

    override fun close() {
        runCatching { socket.close() }
    }

    private companion object {
        const val BODY_READ_TIMEOUT_MS = 15_000
    }
}

/**
 * The new device's side of the LAN leg: a listener on an ephemeral port whose
 * address goes into the QR (§9.1).
 *
 * The endpoints it publishes are this device's own, and only this device's --
 * the QR is an invitation to knock here, never a report of what else is on the
 * network (DL-5's rule, one layer out).
 */
internal class LinkLanListener private constructor(private val server: ServerSocket) : AutoCloseable {

    /** `host:port` as the core formats them, ready for the QR's hints. */
    val endpoints: List<String> = localAddresses().map { host ->
        coreFormatLanEndpoint(CoreLanEndpoint(host = host, port = server.localPort.toUShort()))
    }

    /**
     * Wait for the approving device to connect, or return null if nobody came
     * within [waitMs]. Called in a loop by the runner so that the ceremony's own
     * deadline stays the only clock that matters.
     */
    fun accept(waitMs: Long): LinkLanWire? {
        server.soTimeout = waitMs.coerceIn(1L, LinkWireLimits.MAX_RECEIVE_WAIT_MS).toInt()
        return try {
            LinkLanWire(server.accept())
        } catch (_: SocketTimeoutException) {
            null
        }
    }

    override fun close() {
        runCatching { server.close() }
    }

    companion object {
        fun open(): LinkLanListener = LinkLanListener(ServerSocket(0))

        /**
         * The listener as a [LinkWire] that accepts on first use.
         *
         * This is what lets the offer's own expiry be the only clock in the
         * room. The new device's first action is "show this QR", answered by a
         * bounded wait for the peer; if nobody knocks, the wait returns null,
         * the driver ticks, and the *core* decides whether the offer has
         * expired. A shell that ran its own accept loop first would be a shell
         * inventing a second timeout beside the one §9.2 declares.
         */
        fun accepting(listener: LinkLanListener): LinkWire = object : LinkWire {
            private var accepted: LinkLanWire? = null

            override fun send(bytes: ByteArray) {
                val wire = accepted ?: throw IOException("nobody has connected to this offer yet")
                wire.send(bytes)
            }

            override fun receive(waitMs: Long): ByteArray? {
                val wire = accepted ?: listener.accept(waitMs)?.also { accepted = it } ?: return null
                return wire.receive(waitMs)
            }

            override fun close() {
                accepted?.close()
                listener.close()
            }
        }

        /**
         * Dial one of the endpoints the QR advertised, trying them in order:
         * a device with two interfaces publishes both, and only one of them is
         * on the network the scanner is standing in.
         */
        fun connect(endpoints: List<String>, connectTimeoutMs: Int): LinkLanWire {
            var failure: IOException? = null
            for (text in endpoints) {
                val endpoint = coreParseLanEndpoint(text, 0u) ?: continue
                try {
                    val socket = Socket()
                    socket.connect(
                        InetSocketAddress(endpoint.host, endpoint.port.toInt()),
                        connectTimeoutMs,
                    )
                    return LinkLanWire(socket)
                } catch (e: IOException) {
                    failure = e
                }
            }
            throw failure ?: IOException("the offer carries no reachable LAN endpoint")
        }

        /**
         * This device's own routable IPv4 addresses. IPv4 only, and not because
         * IPv6 is unwelcome: the QR is a few hundred bytes and a link-local IPv6
         * address needs a scope id the other phone cannot use anyway.
         */
        private fun localAddresses(): List<String> =
            runCatching {
                NetworkInterface.getNetworkInterfaces()
                    ?.toList()
                    .orEmpty()
                    .filter { it.isUp && !it.isLoopback }
                    .flatMap { it.inetAddresses.toList() }
                    .filterIsInstance<Inet4Address>()
                    .filterNot { it.isLoopbackAddress || it.isAnyLocalAddress || it.isLinkLocalAddress }
                    .mapNotNull { it.hostAddress }
                    .distinct()
            }.getOrDefault(emptyList())
    }
}
