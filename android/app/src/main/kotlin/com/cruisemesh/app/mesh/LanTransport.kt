package com.cruisemesh.app.mesh

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Handler
import android.os.Looper
import android.util.Log
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.EOFException
import java.io.IOException
import java.net.BindException
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketTimeoutException
import java.security.SecureRandom
import java.util.ArrayDeque
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicInteger
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.LanNoiseSession
import uniffi.cruisemesh_core.lanDefaultTcpPort
import uniffi.cruisemesh_core.lanServiceType

/**
 * Opportunistic same-LAN transport.
 *
 * Android NSD discovers peers and advertises the actual listener port. Every
 * socket completes the shared Rust Noise XX handshake before it is exposed to
 * [MeshRouter], then carries the exact same protocol frames as BLE.
 */
internal class LanTransport(
    context: Context,
    private val identity: Identity,
    private val trustedPeerForStaticKey: (ByteArray) -> ByteArray?,
    private val onAuthenticated: (address: String, userId: ByteArray) -> Unit,
    private val onDisconnected: (address: String) -> Unit,
    private val onFrameReceived: (address: String, frame: ByteArray) -> Unit,
) {
    private val appContext = context.applicationContext
    private val connectivityManager =
        appContext.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
    private val nsdManager = appContext.getSystemService(Context.NSD_SERVICE) as NsdManager
    private val mainHandler = Handler(Looper.getMainLooper())
    private val acceptExecutor = Executors.newSingleThreadExecutor()
    private val connectionExecutor = Executors.newFixedThreadPool(MAX_CONNECTIONS + 2)
    private val writeExecutor = Executors.newSingleThreadExecutor()
    private val secureRandom = SecureRandom()
    private val activeSocketCount = AtomicInteger(0)
    private val sockets = ConcurrentHashMap.newKeySet<Socket>()
    private val connections = ConcurrentHashMap<String, LanConnection>()
    private val outboundServiceKeys = ConcurrentHashMap.newKeySet<String>()
    private val resolvedServices = ConcurrentHashMap<String, NsdServiceInfo>()

    @Volatile
    private var started = false

    @Volatile
    private var wifiNetwork: Network? = null

    private var networkCallbackRegistered = false
    private var serverSocket: ServerSocket? = null
    private var requestedServiceName: String? = null
    private var registeredServiceName: String? = null
    private var registrationListener: NsdManager.RegistrationListener? = null
    private var discoveryListener: NsdManager.DiscoveryListener? = null
    private var resolveListener: NsdManager.ResolveListener? = null
    private var resolving = false
    private val pendingServices = ArrayDeque<NsdServiceInfo>()
    private val queuedServiceNames = mutableSetOf<String>()
    private val instanceToken = ByteArray(8).also(secureRandom::nextBytes).toHex()

    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            mainHandler.post {
                if (!started || wifiNetwork == network) return@post
                wifiNetwork = network
                restartNetworkSession(network)
            }
        }

        override fun onLost(network: Network) {
            mainHandler.post {
                if (wifiNetwork != network) return@post
                wifiNetwork = null
                teardownNetworkSession()
            }
        }
    }

    fun start() {
        check(Looper.myLooper() == Looper.getMainLooper())
        if (started) return
        started = true
        val request = NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
            .build()
        try {
            connectivityManager.registerNetworkCallback(request, networkCallback)
            networkCallbackRegistered = true
        } catch (error: RuntimeException) {
            Log.w(TAG, "Unable to monitor Wi-Fi for LAN transport", error)
        }
    }

    fun stop() {
        check(Looper.myLooper() == Looper.getMainLooper())
        if (!started) return
        started = false
        teardownNetworkSession()
        mainHandler.removeCallbacksAndMessages(null)
        if (networkCallbackRegistered) {
            try {
                connectivityManager.unregisterNetworkCallback(networkCallback)
            } catch (_: IllegalArgumentException) {
                // Already removed by the platform.
            }
            networkCallbackRegistered = false
        }
        wifiNetwork = null
        acceptExecutor.shutdownNow()
        connectionExecutor.shutdownNow()
        writeExecutor.shutdownNow()
    }

    /** MeshRouter send function. Encryption and socket writes stay ordered. */
    fun sendFrame(address: String, frame: ByteArray) {
        val connection = connections[address] ?: return
        try {
            writeExecutor.execute {
                try {
                    connection.sendFrame(frame)
                } catch (error: Exception) {
                    Log.w(TAG, "LAN frame send failed; closing link", error)
                    connection.close()
                }
            }
        } catch (_: RuntimeException) {
            // Executor is shutting down with the service.
        }
    }

    private fun restartNetworkSession(network: Network) {
        teardownNetworkSession()
        if (!started || wifiNetwork != network) return

        val listener = openListener() ?: return
        serverSocket = listener
        acceptExecutor.execute { acceptLoop(listener) }

        // The opaque token is also the cross-platform connection-election
        // value. Publishing it as the service name lets Apple Bonjour choose
        // the same single initiator without exposing an identity.
        requestedServiceName = instanceToken
        registeredServiceName = requestedServiceName
        val serviceInfo = NsdServiceInfo().apply {
            serviceName = requestedServiceName
            serviceType = lanServiceType()
            port = listener.localPort
            setAttribute(TXT_VERSION, "1")
            setAttribute(TXT_INSTANCE, instanceToken)
        }
        val registration = makeRegistrationListener()
        registrationListener = registration
        try {
            nsdManager.registerService(serviceInfo, NsdManager.PROTOCOL_DNS_SD, registration)
        } catch (error: RuntimeException) {
            Log.w(TAG, "Unable to advertise LAN transport", error)
        }

        val discovery = makeDiscoveryListener()
        discoveryListener = discovery
        try {
            nsdManager.discoverServices(lanServiceType(), NsdManager.PROTOCOL_DNS_SD, discovery)
        } catch (error: RuntimeException) {
            Log.w(TAG, "Unable to discover LAN peers", error)
        }
    }

    private fun openListener(): ServerSocket? {
        val defaultPort = lanDefaultTcpPort().toInt()
        return try {
            ServerSocket().apply {
                reuseAddress = true
                bind(InetSocketAddress(defaultPort))
            }.also {
                Log.i(TAG, "Listening for CruiseMesh LAN peers on TCP $defaultPort")
            }
        } catch (_: BindException) {
            try {
                ServerSocket(0).also {
                    Log.w(TAG, "TCP $defaultPort is occupied; advertising fallback port ${it.localPort}")
                }
            } catch (error: IOException) {
                Log.w(TAG, "Unable to open LAN listener", error)
                null
            }
        } catch (error: IOException) {
            Log.w(TAG, "Unable to open LAN listener", error)
            null
        }
    }

    private fun acceptLoop(server: ServerSocket) {
        while (started && !server.isClosed) {
            val socket = try {
                server.accept()
            } catch (_: IOException) {
                break
            }
            if (!tryAcquireSocketSlot()) {
                socket.closeQuietly()
                continue
            }
            submitConnection(socket, initiator = false, outboundServiceKey = null)
        }
    }

    private fun connectToService(serviceInfo: NsdServiceInfo) {
        val network = wifiNetwork ?: return
        val key = serviceInfo.serviceName
        resolvedServices[key] = serviceInfo
        if (!outboundServiceKeys.add(key)) return
        if (!tryAcquireSocketSlot()) {
            outboundServiceKeys.remove(key)
            scheduleReconnect(key)
            return
        }
        try {
            connectionExecutor.execute {
                val socket = try {
                    network.socketFactory.createSocket().apply {
                        tcpNoDelay = true
                        keepAlive = true
                        connect(
                            InetSocketAddress(resolvedHost(serviceInfo), serviceInfo.port),
                            CONNECT_TIMEOUT_MS,
                        )
                    }
                } catch (error: Exception) {
                    Log.d(TAG, "LAN peer connection attempt failed", error)
                    outboundServiceKeys.remove(key)
                    releaseSocketSlot()
                    scheduleReconnect(key)
                    return@execute
                }
                runConnection(socket, initiator = true, outboundServiceKey = key)
            }
        } catch (_: RuntimeException) {
            outboundServiceKeys.remove(key)
            releaseSocketSlot()
        }
    }

    private fun submitConnection(socket: Socket, initiator: Boolean, outboundServiceKey: String?) {
        try {
            connectionExecutor.execute {
                runConnection(socket, initiator, outboundServiceKey)
            }
        } catch (_: RuntimeException) {
            socket.closeQuietly()
            releaseSocketSlot()
            outboundServiceKey?.let(outboundServiceKeys::remove)
        }
    }

    private fun runConnection(
        socket: Socket,
        initiator: Boolean,
        outboundServiceKey: String?,
    ) {
        sockets += socket
        var address: String? = null
        var connection: LanConnection? = null
        var noise: LanNoiseSession? = null
        try {
            socket.tcpNoDelay = true
            socket.keepAlive = true
            socket.soTimeout = HANDSHAKE_TIMEOUT_MS
            val input = DataInputStream(socket.getInputStream())
            val output = DataOutputStream(socket.getOutputStream())
            val session = LanNoiseSession(initiator, identity.agreeSk)
            noise = session

            val trustedUserId = if (initiator) {
                writePacket(output, session.writeHandshakeMessage())
                session.readHandshakeMessage(readPacket(input))
                val remoteStatic = session.remoteStaticKey()
                    ?: throw IOException("LAN responder did not provide a static key")
                val userId = trustedPeerForStaticKey(remoteStatic)
                    ?: throw IOException("LAN responder is not an accepted contact")
                writePacket(output, session.writeHandshakeMessage())
                userId
            } else {
                session.readHandshakeMessage(readPacket(input))
                writePacket(output, session.writeHandshakeMessage())
                session.readHandshakeMessage(readPacket(input))
                val remoteStatic = session.remoteStaticKey()
                    ?: throw IOException("LAN initiator did not provide a static key")
                trustedPeerForStaticKey(remoteStatic)
                    ?: throw IOException("LAN initiator is not an accepted contact")
            }
            if (!session.isHandshakeFinished()) {
                throw IOException("LAN Noise handshake did not finish")
            }

            socket.soTimeout = 0
            address = "lan:${UUID.randomUUID()}"
            connection = LanConnection(address, socket, output, session)
            connections[address] = connection
            onAuthenticated(address, trustedUserId)
            Log.i(TAG, "Authenticated CruiseMesh peer over local Wi-Fi")

            while (started && !socket.isClosed) {
                val record = readPacket(input)
                val frame = session.decryptRecord(record) ?: continue
                onFrameReceived(address, frame)
            }
        } catch (_: EOFException) {
            // Normal peer disconnect.
        } catch (_: SocketTimeoutException) {
            Log.d(TAG, "LAN connection timed out during setup")
        } catch (error: CoreException) {
            Log.w(TAG, "LAN cryptographic session failed", error)
        } catch (error: IOException) {
            Log.d(TAG, "LAN connection closed: ${error.message}")
        } catch (error: RuntimeException) {
            Log.w(TAG, "LAN connection failed", error)
        } finally {
            connection?.markClosed()
            if (connection == null) noise?.close()
            address?.let {
                connections.remove(it, connection)
                onDisconnected(it)
            }
            sockets.remove(socket)
            socket.closeQuietly()
            releaseSocketSlot()
            outboundServiceKey?.let {
                outboundServiceKeys.remove(it)
                scheduleReconnect(it)
            }
        }
    }

    private fun scheduleReconnect(serviceKey: String) {
        mainHandler.postDelayed(
            {
                if (!started || wifiNetwork == null || outboundServiceKeys.contains(serviceKey)) {
                    return@postDelayed
                }
                resolvedServices[serviceKey]?.let(::connectToService)
            },
            RECONNECT_DELAY_MS,
        )
    }

    private fun makeRegistrationListener() = object : NsdManager.RegistrationListener {
        override fun onServiceRegistered(serviceInfo: NsdServiceInfo) {
            val listener = this
            mainHandler.post {
                if (started && serverSocket != null) {
                    registeredServiceName = serviceInfo.serviceName
                } else {
                    unregisterService(listener)
                }
            }
        }

        override fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
            Log.w(TAG, "LAN service registration failed: $errorCode")
        }

        override fun onServiceUnregistered(serviceInfo: NsdServiceInfo) = Unit
        override fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) = Unit
    }

    private fun makeDiscoveryListener() = object : NsdManager.DiscoveryListener {
        override fun onDiscoveryStarted(serviceType: String) {
            val listener = this
            mainHandler.post {
                if (!started || serverSocket == null) stopDiscovery(listener)
            }
        }

        override fun onServiceFound(serviceInfo: NsdServiceInfo) {
            mainHandler.post {
                if (!started || !sameLanServiceType(serviceInfo.serviceType)) return@post
                val name = serviceInfo.serviceName
                if (name == requestedServiceName || name == registeredServiceName) return@post
                if (!queuedServiceNames.add(name)) return@post
                pendingServices.addLast(serviceInfo)
                resolveNext()
            }
        }

        override fun onServiceLost(serviceInfo: NsdServiceInfo) {
            mainHandler.post {
                val name = serviceInfo.serviceName
                resolvedServices.remove(name)
                queuedServiceNames.remove(name)
            }
        }

        override fun onDiscoveryStopped(serviceType: String) = Unit

        override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
            Log.w(TAG, "LAN discovery failed to start: $errorCode")
        }

        override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) = Unit
    }

    @Suppress("DEPRECATION")
    private fun resolveNext() {
        if (!started || resolving || pendingServices.isEmpty()) return
        resolving = true
        val service = pendingServices.removeFirst()
        val listener = object : NsdManager.ResolveListener {
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                mainHandler.post {
                    resolving = false
                    queuedServiceNames.remove(serviceInfo.serviceName)
                    resolveNext()
                }
            }

            override fun onServiceResolved(serviceInfo: NsdServiceInfo) {
                mainHandler.post {
                    resolving = false
                    val token = serviceInfo.attributes[TXT_INSTANCE]?.toString(Charsets.UTF_8)
                    val version = serviceInfo.attributes[TXT_VERSION]?.toString(Charsets.UTF_8)
                    if (
                        token != null &&
                        shouldInitiateLanConnection(instanceToken, token) &&
                        version == "1" &&
                        serviceInfo.port in 1..65_535
                    ) {
                        connectToService(serviceInfo)
                    }
                    resolveNext()
                }
            }
        }
        resolveListener = listener
        try {
            nsdManager.resolveService(service, listener)
        } catch (error: RuntimeException) {
            resolving = false
            queuedServiceNames.remove(service.serviceName)
            Log.d(TAG, "LAN service resolution failed", error)
            resolveNext()
        }
    }

    private fun teardownNetworkSession() {
        discoveryListener?.let(::stopDiscovery)
        discoveryListener = null
        registrationListener?.let(::unregisterService)
        registrationListener = null
        resolveListener = null
        resolving = false
        pendingServices.clear()
        queuedServiceNames.clear()
        requestedServiceName = null
        registeredServiceName = null
        resolvedServices.clear()
        outboundServiceKeys.clear()
        serverSocket?.closeQuietly()
        serverSocket = null
        sockets.toList().forEach(Socket::closeQuietly)
        connections.clear()
    }

    private fun stopDiscovery(listener: NsdManager.DiscoveryListener) {
        try {
            nsdManager.stopServiceDiscovery(listener)
        } catch (_: RuntimeException) {
            // Not started or already stopped. A late callback retries cleanup.
        }
    }

    private fun unregisterService(listener: NsdManager.RegistrationListener) {
        try {
            nsdManager.unregisterService(listener)
        } catch (_: RuntimeException) {
            // Not registered or already stopped. A late callback retries cleanup.
        }
    }

    private fun tryAcquireSocketSlot(): Boolean {
        while (true) {
            val current = activeSocketCount.get()
            if (current >= MAX_CONNECTIONS) return false
            if (activeSocketCount.compareAndSet(current, current + 1)) return true
        }
    }

    private fun releaseSocketSlot() {
        activeSocketCount.updateAndGet { current -> (current - 1).coerceAtLeast(0) }
    }

    private inner class LanConnection(
        val address: String,
        private val socket: Socket,
        private val output: DataOutputStream,
        private val noise: LanNoiseSession,
    ) {
        @Volatile
        private var closed = false

        fun sendFrame(frame: ByteArray) {
            if (closed || socket.isClosed) return
            for (record in noise.encryptFrame(frame)) {
                writePacket(output, record)
            }
        }

        fun close() {
            markClosed()
            socket.closeQuietly()
        }

        fun markClosed() {
            if (!closed) {
                closed = true
                noise.close()
            }
        }
    }

    companion object {
        private const val TAG = "LanTransport"
        private const val TXT_VERSION = "v"
        private const val TXT_INSTANCE = "i"
        private const val MAX_CONNECTIONS = 8
        private const val CONNECT_TIMEOUT_MS = 3_000
        private const val HANDSHAKE_TIMEOUT_MS = 5_000
        private const val RECONNECT_DELAY_MS = 30_000L
        private const val MAX_PACKET_SIZE = 65_535

        private fun writePacket(output: DataOutputStream, bytes: ByteArray) {
            require(bytes.isNotEmpty() && bytes.size <= MAX_PACKET_SIZE)
            output.writeInt(bytes.size)
            output.write(bytes)
            output.flush()
        }

        private fun readPacket(input: DataInputStream): ByteArray {
            val size = input.readInt()
            if (size !in 1..MAX_PACKET_SIZE) {
                throw IOException("invalid LAN packet length $size")
            }
            return ByteArray(size).also(input::readFully)
        }
    }
}

internal fun trustedLanPeerUserId(contacts: List<Contact>, remoteStaticKey: ByteArray): ByteArray? =
    contacts.firstOrNull { it.agreePk.contentEquals(remoteStaticKey) }?.userId?.copyOf()

internal fun sameLanServiceType(value: String): Boolean =
    value.trimEnd('.') == lanServiceType().trimEnd('.')

/**
 * Both peers discover each other. The opaque per-process tokens provide a
 * stable tie-break so exactly one side opens the TCP connection.
 */
internal fun shouldInitiateLanConnection(localToken: String, remoteToken: String): Boolean =
    localToken != remoteToken && localToken < remoteToken

@Suppress("DEPRECATION")
private fun resolvedHost(serviceInfo: NsdServiceInfo) = serviceInfo.host

private fun ByteArray.toHex(): String = joinToString("") { "%02x".format(it) }

private fun Socket.closeQuietly() {
    try {
        close()
    } catch (_: IOException) {
        // Best effort during network/service teardown.
    }
}

private fun ServerSocket.closeQuietly() {
    try {
        close()
    } catch (_: IOException) {
        // Best effort during network/service teardown.
    }
}
