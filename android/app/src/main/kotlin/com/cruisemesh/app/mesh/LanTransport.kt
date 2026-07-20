package com.cruisemesh.app.mesh

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.nsd.DiscoveryRequest
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.ext.SdkExtensions
import android.util.Log
import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.EOFException
import java.io.IOException
import java.net.BindException
import java.net.Inet4Address
import java.net.InetAddress
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
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.Frame
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
    private val onNetworkReady: (Frame.LanEndpoint, networkId: String?) -> Unit,
    private val onEndpointObserved: (
        userId: ByteArray,
        endpoint: LanManualEndpoint,
        networkId: String?,
    ) -> Unit,
    private val onAuthenticated: (
        address: String,
        userId: ByteArray,
        endpoint: LanManualEndpoint?,
        networkId: String?,
    ) -> Unit,
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
    // A /16 sweep queues up to ~65k connect probes, so it needs far more
    // parallelism than a /24 -- but those worker threads only need to exist
    // while a scan is running. allowCoreThreadTimeOut lets all SCAN_CONCURRENCY
    // threads reap after they go idle between scans instead of lingering for
    // the process lifetime.
    private val scanExecutor = ThreadPoolExecutor(
        SCAN_CONCURRENCY,
        SCAN_CONCURRENCY,
        SCAN_THREAD_KEEPALIVE_SECONDS,
        TimeUnit.SECONDS,
        LinkedBlockingQueue(),
    ).apply { allowCoreThreadTimeOut(true) }
    private val writeExecutor = Executors.newSingleThreadExecutor()
    private val secureRandom = SecureRandom()
    private val activeSocketCount = AtomicInteger(0)
    private val scanGeneration = AtomicInteger(0)
    private val connectionBackoff = ReconnectBackoffTracker()
    private val sockets = ConcurrentHashMap.newKeySet<Socket>()
    private val connections = ConcurrentHashMap<String, LanConnection>()
    private val authenticatedUserIds = ConcurrentHashMap<String, String>()
    private val outboundServiceKeys = ConcurrentHashMap.newKeySet<String>()
    private val reconnectTargets = ConcurrentHashMap<String, ReconnectTarget>()
    private val resolvedServices = ConcurrentHashMap<String, NsdServiceInfo>()

    @Volatile
    private var started = false

    @Volatile
    private var wifiNetwork: Network? = null

    @Volatile
    private var endpointHint: Frame.LanEndpoint? = null

    @Volatile
    private var currentNetworkId: String? = null

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
    private val eligibleWifiNetworks = linkedSetOf<Network>()
    private val instanceTokenBytes = ByteArray(8).also(secureRandom::nextBytes)
    private val instanceToken = instanceTokenBytes.toHex()
    private val scanPlanner = LanScanPlanner()

    @Volatile
    private var runningSweep: RunningSweep? = null
    private val automaticScanRunnable = Runnable {
        if (!started || wifiNetwork == null) return@Runnable
        if (
            shouldRunAutomaticLanScan(
                activeConnections = connections.size,
                outboundAttempts = outboundServiceKeys.size,
                scanRemaining = runningSweep?.outcomes?.remainingCandidates() ?: 0,
            )
        ) {
            scanPlanner.takeDueScan(System.currentTimeMillis())?.let { breadth ->
                Log.i(TAG, "Starting automatic local Wi-Fi fallback search (${breadth.name})")
                startSubnetScan(breadth)
            }
        }
        scheduleAutomaticSubnetScan(AUTO_SCAN_RETRY_INTERVAL_MS)
    }

    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            mainHandler.post {
                if (!started || !isEligibleWifiNetwork(network)) return@post
                eligibleWifiNetworks += network
                if (wifiNetwork != null) return@post
                wifiNetwork = network
                restartNetworkSession(network)
            }
        }

        override fun onLost(network: Network) {
            mainHandler.post {
                eligibleWifiNetworks -= network
                if (wifiNetwork != network) return@post
                wifiNetwork = null
                teardownNetworkSession()
                eligibleWifiNetworks.firstOrNull()?.let { replacement ->
                    wifiNetwork = replacement
                    restartNetworkSession(replacement)
                }
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
            LanTransportDiagnostics.registerManualConnector { endpoint ->
                mainHandler.post { connectManually(endpoint) }
            }
            // The manual diagnostics button always sweeps the full subnet.
            LanTransportDiagnostics.registerScanRequester { startSubnetScan() }
        } catch (error: RuntimeException) {
            Log.w(TAG, "Unable to monitor Wi-Fi for LAN transport", error)
        }
    }

    fun stop() {
        check(Looper.myLooper() == Looper.getMainLooper())
        if (!started) return
        started = false
        LanTransportDiagnostics.unregisterManualConnector()
        LanTransportDiagnostics.unregisterScanRequester()
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
        eligibleWifiNetworks.clear()
        acceptExecutor.shutdownNow()
        connectionExecutor.shutdownNow()
        scanExecutor.shutdownNow()
        writeExecutor.shutdownNow()
    }

    /** MeshRouter send function. Encryption and socket writes stay ordered. */
    fun sendFrame(address: String, frame: ByteArray) {
        val connection = connections[address] ?: return
        try {
            writeExecutor.execute {
                try {
                    connection.sendFrame(frame)
                    LanTransportDiagnostics.frameSent()
                } catch (error: Exception) {
                    Log.w(TAG, "LAN frame send failed; closing link", error)
                    connection.close()
                }
            }
        } catch (_: RuntimeException) {
            // Executor is shutting down with the service.
        }
    }

    fun closeLink(address: String) {
        connections[address]?.close()
    }

    fun startSubnetScan(breadth: LanScanBreadth = LanScanBreadth.FULL_SUBNET): String? {
        if (!started) return "Start the mesh before searching the local subnet"
        if ((runningSweep?.outcomes?.remainingCandidates() ?: 0) > 0) {
            return "A local subnet search is already running"
        }
        val network = wifiNetwork ?: return "This phone is not connected to Wi-Fi"
        val local = endpointHint?.host
            ?.let { runCatching { InetAddress.getByName(it) }.getOrNull() }
            as? Inet4Address
            ?: return "The selected Wi-Fi network has no IPv4 address to search"
        val networkPrefixLength = localPrefixLength(network, local)
        val prefixLength = when (breadth) {
            LanScanBreadth.FULL_SUBNET -> networkPrefixLength
            // A larger prefix is a narrower sweep, so this caps the local tier
            // at our /24 without widening a network that is narrower than one.
            LanScanBreadth.LOCAL_24 -> maxOf(networkPrefixLength, DEFAULT_SCAN_PREFIX_LENGTH)
        }
        val candidates = subnetHosts(local, prefixLength).shuffled()
        val generation = scanGeneration.incrementAndGet()
        val sweep = RunningSweep(
            outcomes = SweepOutcomes(generation, candidates.size),
            breadth = breadth,
            prefixLength = effectiveScanPrefixLength(prefixLength),
        )
        runningSweep = sweep
        Log.i(
            TAG,
            "Scanning ${candidates.size} subnet hosts (/${sweep.prefixLength}) " +
                "for CruiseMesh peers",
        )
        LanTransportDiagnostics.scanStarted(candidates.size)
        for (candidate in candidates) {
            try {
                scanExecutor.execute { scanHost(network, candidate, sweep) }
            } catch (_: RuntimeException) {
                recordScanOutcome(sweep, SweepProbeOutcome.OTHER)
            }
        }
        return null
    }

    private fun scanHost(network: Network, candidate: InetAddress, sweep: RunningSweep) {
        if (sweep.outcomes.generation != scanGeneration.get()) return
        val endpoint = InetSocketAddress(candidate, lanDefaultTcpPort().toInt())
        val serviceKey = "scan:${candidate.hostAddress}"
        var outcomeRecorded = false
        try {
            val socket = network.socketFactory.createSocket().apply {
                tcpNoDelay = true
                keepAlive = true
                connect(endpoint, SCAN_CONNECT_TIMEOUT_MS)
            }
            recordScanOutcome(sweep, SweepProbeOutcome.CONNECTED)
            outcomeRecorded = true
            if (sweep.outcomes.generation != scanGeneration.get()) {
                socket.closeQuietly()
                return
            }
            if (!outboundServiceKeys.add(serviceKey)) {
                socket.closeQuietly()
                return
            }
            if (!tryAcquireSocketSlot()) {
                outboundServiceKeys.remove(serviceKey)
                socket.closeQuietly()
                return
            }
            rememberReconnectTarget(serviceKey, listOf(endpoint), expectedUserId = null)
            runConnection(
                socket = socket,
                initiator = true,
                outboundServiceKey = serviceKey,
                expectedUserId = null,
                advertisedEndpoint = endpoint,
            )
        } catch (error: Exception) {
            if (!outcomeRecorded) {
                recordScanOutcome(sweep, classifySweepProbeFailure(error))
                outcomeRecorded = true
            }
        } finally {
            if (!outcomeRecorded) recordScanOutcome(sweep, SweepProbeOutcome.OTHER)
        }
    }

    private fun recordScanOutcome(sweep: RunningSweep, outcome: SweepProbeOutcome) {
        if (runningSweep !== sweep || sweep.outcomes.generation != scanGeneration.get()) return
        val update = sweep.outcomes.record(sweep.outcomes.generation, outcome) ?: return
        if (runningSweep !== sweep || sweep.outcomes.generation != scanGeneration.get()) return
        LanTransportDiagnostics.scanAdvanced()
        update.completedSummary?.let { onScanCompleted(sweep, it) }
    }

    /**
     * Every candidate of the running sweep has been probed. Runs on whichever
     * scan worker retired the last candidate. A completed local-tier sweep
     * makes the full sweep eligible ([LanScanPlanner.onScanCompleted]) and
     * pulls the next check forward so escalation doesn't wait out the periodic
     * interval -- the check's loneliness gate still applies, so if this sweep
     * (or NSD) produced a connection, no escalation happens.
     */
    private fun onScanCompleted(sweep: RunningSweep, summary: SweepOutcomeSummary) {
        if (runningSweep !== sweep || sweep.outcomes.generation != scanGeneration.get()) return
        runningSweep = null
        Log.i(TAG, summary.logLine(sweep.prefixLength))
        scanPlanner.onScanCompleted(sweep.breadth)
        val verdict = lanSweepVerdict(summary)
        LanTransportDiagnostics.sweepCompleted(summary)
        when (verdict) {
            LanSweepVerdict.ISOLATION_SUSPECTED -> {
                scanPlanner.onIsolationSuspected(System.currentTimeMillis())
            }
            else -> Unit
        }
        if (sweep.breadth == LanScanBreadth.LOCAL_24) {
            scheduleAutomaticSubnetScan(AUTO_SCAN_ESCALATE_DELAY_MS)
        }
    }

    fun currentEndpointHint(): Frame.LanEndpoint? = endpointHint
    fun currentNetworkId(): String? = currentNetworkId

    fun connectToHint(hint: Frame.LanEndpoint, expectedUserId: ByteArray) {
        mainHandler.post {
            val remoteToken = hint.instanceToken.toHex()
            val endpoint = LanManualEndpoint(hint.host, hint.port.toInt())
            onEndpointObserved(expectedUserId, endpoint, currentNetworkId)
            if (!started) return@post
            scanPlanner.onPeerEvidence(System.currentTimeMillis())
            LanTransportDiagnostics.peerEvidence()
            if (!shouldInitiateLanConnection(instanceToken, remoteToken)) {
                Log.i(
                    TAG,
                    "Resolved LAN peer ${endpoint.display}; awaiting their connection (tie-break)",
                )
                return@post
            }
            val network = wifiNetwork ?: return@post
            Log.i(TAG, "BLE introduced LAN peer at ${endpoint.display}")
            connectToEndpoints(
                network = network,
                key = remoteToken,
                endpoints = listOf(
                    InetSocketAddress(endpoint.host, endpoint.port),
                ),
                expectedUserId = expectedUserId,
            )
        }
    }

    fun connectCached(endpoint: LanManualEndpoint, expectedUserId: ByteArray) {
        mainHandler.post {
            val network = wifiNetwork ?: return@post
            connectToEndpoints(
                network = network,
                key = "cache:${expectedUserId.toHex()}:${endpoint.display}",
                endpoints = listOf(InetSocketAddress(endpoint.host, endpoint.port)),
                expectedUserId = expectedUserId,
            )
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
            if (supportsNetworkScopedNsd()) {
                setNetwork(network)
            }
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
            if (supportsNetworkScopedNsd()) {
                val request = DiscoveryRequest.Builder(lanServiceType())
                    .setNetwork(network)
                    .build()
                nsdManager.discoverServices(request, appContext.mainExecutor, discovery)
            } else {
                @Suppress("DEPRECATION")
                nsdManager.discoverServices(
                    lanServiceType(),
                    NsdManager.PROTOCOL_DNS_SD,
                    discovery,
                )
            }
        } catch (error: RuntimeException) {
            Log.w(TAG, "Unable to discover LAN peers", error)
        }

        val localEndpoint = localEndpoint(network, listener.localPort)
        currentNetworkId = lanNetworkId(connectivityManager, network)
        endpointHint = localEndpoint?.let {
            Frame.LanEndpoint(
                instanceToken = instanceTokenBytes.copyOf(),
                host = it.host,
                port = it.port.toUShort(),
            )
        }
        LanTransportDiagnostics.listening(localEndpoint?.display)
        Log.i(
            TAG,
            "LAN session ready on ${localEndpoint?.display ?: "the selected Wi-Fi network"}",
        )
        endpointHint?.let { onNetworkReady(it, currentNetworkId) }
        scanPlanner.onNetworkJoined(System.currentTimeMillis())
        LanTransportDiagnostics.networkJoined()
        scheduleAutomaticSubnetScan(AUTO_SCAN_INITIAL_DELAY_MS)
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
        if (
            supportsNetworkScopedNsd() &&
            serviceInfo.network != null &&
            serviceInfo.network != network
        ) {
            Log.d(TAG, "Ignoring LAN service resolved on a different network")
            return
        }
        val key = serviceInfo.serviceName
        resolvedServices[key] = serviceInfo
        val endpoints = resolvedHosts(serviceInfo).map { InetSocketAddress(it, serviceInfo.port) }
        if (endpoints.isEmpty()) {
            LanTransportDiagnostics.connectionFailed(
                serviceInfo.serviceName,
                "Peer discovery returned no usable address",
            )
            return
        }
        LanTransportDiagnostics.discovered(endpointDisplay(endpoints.first()))
        Log.d(TAG, "Resolved CruiseMesh LAN peer at ${endpointDisplay(endpoints.first())}")
        connectToEndpoints(network, key, endpoints)
    }

    private fun connectManually(endpoint: LanManualEndpoint) {
        if (!started) return
        val network = wifiNetwork
        if (network == null) {
            LanTransportDiagnostics.connectionFailed(
                endpoint.display,
                "This phone is not connected to Wi-Fi",
            )
            return
        }
        Log.i(TAG, "Manual LAN connection requested to ${endpoint.display}")
        val key = "manual:${endpoint.display}"
        connectionBackoff.recordSuccess(key)
        connectToEndpoints(
            network = network,
            key = key,
            endpoints = listOf(InetSocketAddress(endpoint.host, endpoint.port)),
        )
    }

    private fun connectToEndpoints(
        network: Network,
        key: String,
        endpoints: List<InetSocketAddress>,
        expectedUserId: ByteArray? = null,
    ) {
        if (endpoints.isEmpty()) return
        rememberReconnectTarget(key, endpoints, expectedUserId)
        if (
            expectedUserId != null &&
            authenticatedUserIds.containsValue(expectedUserId.toHex())
        ) {
            return
        }
        if (!connectionBackoff.canAttempt(key, System.currentTimeMillis())) return
        if (!outboundServiceKeys.add(key)) return
        if (!tryAcquireSocketSlot()) {
            outboundServiceKeys.remove(key)
            scheduleReconnect(key)
            return
        }
        try {
            connectionExecutor.execute {
                var socket: Socket? = null
                var connectedEndpoint: InetSocketAddress? = null
                var lastError: Exception? = null
                for (endpoint in endpoints) {
                    LanTransportDiagnostics.connecting(endpointDisplay(endpoint))
                    try {
                        socket = network.socketFactory.createSocket().apply {
                            tcpNoDelay = true
                            keepAlive = true
                            connect(endpoint, CONNECT_TIMEOUT_MS)
                        }
                        connectedEndpoint = endpoint
                        break
                    } catch (error: Exception) {
                        lastError = error
                        socket?.closeQuietly()
                        socket = null
                    }
                }
                if (socket == null) {
                    val endpoint = endpoints.firstOrNull()?.let(::endpointDisplay) ?: key
                    Log.d(TAG, "LAN peer connection attempt failed", lastError)
                    LanTransportDiagnostics.connectionFailed(
                        endpoint,
                        lastError?.message ?: "Connection timed out",
                    )
                    connectionBackoff.recordFailure(key, System.currentTimeMillis())
                    outboundServiceKeys.remove(key)
                    releaseSocketSlot()
                    scheduleReconnect(key)
                    return@execute
                }
                runConnection(
                    socket,
                    initiator = true,
                    outboundServiceKey = key,
                    expectedUserId = expectedUserId,
                    advertisedEndpoint = connectedEndpoint,
                )
            }
        } catch (_: RuntimeException) {
            outboundServiceKeys.remove(key)
            releaseSocketSlot()
        }
    }

    private fun submitConnection(socket: Socket, initiator: Boolean, outboundServiceKey: String?) {
        try {
            connectionExecutor.execute {
                runConnection(
                    socket,
                    initiator,
                    outboundServiceKey,
                    expectedUserId = null,
                    advertisedEndpoint = null,
                )
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
        expectedUserId: ByteArray?,
        advertisedEndpoint: InetSocketAddress?,
    ) {
        sockets += socket
        val peerEndpoint = socket.remoteSocketAddress?.toString()?.removePrefix("/") ?: "peer"
        var address: String? = null
        var connection: LanConnection? = null
        var noise: LanNoiseSession? = null
        var authenticated = false
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
                if (
                    expectedUserId != null &&
                    !userId.contentEquals(expectedUserId)
                ) {
                    throw IOException("LAN responder does not match the BLE endpoint hint")
                }
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
            authenticatedUserIds[address] = trustedUserId.toHex()
            outboundServiceKey?.let { key ->
                // computeIfPresent keeps this a single atomic step; scan and
                // reconnect attempts touch the same key from other executor
                // threads and a plain read-modify-write could lose a racing
                // update to this reconnect target's endpoint list.
                reconnectTargets.computeIfPresent(key) { _, target ->
                    target.copy(expectedUserId = trustedUserId.copyOf())
                }
            }
            val authenticatedEndpoint = advertisedEndpoint?.let {
                LanManualEndpoint(
                    socket.inetAddress?.hostAddress ?: it.hostString,
                    it.port,
                )
            }
            onAuthenticated(
                address,
                trustedUserId,
                authenticatedEndpoint,
                currentNetworkId,
            )
            outboundServiceKey?.let(connectionBackoff::recordSuccess)
            authenticated = true
            scheduleAutomaticSubnetScan(AUTO_SCAN_RETRY_INTERVAL_MS)
            Log.i(TAG, "Authenticated CruiseMesh peer over local Wi-Fi")

            while (started && !socket.isClosed) {
                val record = readPacket(input)
                val frame = session.decryptRecord(record) ?: continue
                LanTransportDiagnostics.frameReceived()
                onFrameReceived(address, frame)
            }
        } catch (_: EOFException) {
            // Normal peer disconnect.
        } catch (_: SocketTimeoutException) {
            Log.d(TAG, "LAN connection timed out during setup")
            LanTransportDiagnostics.connectionFailed(peerEndpoint, "Secure setup timed out")
        } catch (error: CoreException) {
            Log.w(TAG, "LAN cryptographic session failed", error)
            LanTransportDiagnostics.connectionFailed(peerEndpoint, "Secure setup failed")
        } catch (error: IOException) {
            Log.d(TAG, "LAN connection closed: ${error.message}")
            if (!authenticated) {
                LanTransportDiagnostics.connectionFailed(
                    peerEndpoint,
                    error.message ?: "Secure connection closed",
                )
            }
        } catch (error: RuntimeException) {
            Log.w(TAG, "LAN connection failed", error)
            if (!authenticated) {
                LanTransportDiagnostics.connectionFailed(
                    peerEndpoint,
                    error.message ?: "Connection failed",
                )
            }
        } finally {
            connection?.markClosed()
            if (connection == null) noise?.close()
            address?.let {
                connections.remove(it, connection)
                authenticatedUserIds.remove(it)
                onDisconnected(it)
            }
            sockets.remove(socket)
            socket.closeQuietly()
            releaseSocketSlot()
            outboundServiceKey?.let {
                outboundServiceKeys.remove(it)
                if (shouldRetainLanReconnectTarget(it, authenticated)) {
                    connectionBackoff.recordFailure(it, System.currentTimeMillis())
                    scheduleReconnect(it)
                } else {
                    reconnectTargets.remove(it)
                }
            }
            if (authenticated) {
                scheduleAutomaticSubnetScan(AUTO_SCAN_RECONNECT_DELAY_MS)
            }
        }
    }

    private fun scheduleReconnect(serviceKey: String) {
        val now = System.currentTimeMillis()
        val delayMs = connectionBackoff.retryDelayMs(serviceKey, now)
            ?: if (connectionBackoff.isGivenUp(serviceKey)) return else RECONNECT_SLOT_DELAY_MS
        mainHandler.postDelayed(
            {
                if (!started || wifiNetwork == null || outboundServiceKeys.contains(serviceKey)) {
                    return@postDelayed
                }
                val network = wifiNetwork ?: return@postDelayed
                val target = reconnectTargets[serviceKey] ?: return@postDelayed
                Log.i(TAG, "Retrying secure local Wi-Fi connection")
                connectToEndpoints(
                    network = network,
                    key = serviceKey,
                    endpoints = target.endpoints,
                    expectedUserId = target.expectedUserId,
                )
            },
            delayMs,
        )
    }

    private fun rememberReconnectTarget(
        serviceKey: String,
        endpoints: List<InetSocketAddress>,
        expectedUserId: ByteArray?,
    ) {
        reconnectTargets[serviceKey] = ReconnectTarget(
            endpoints = endpoints.toList(),
            expectedUserId = expectedUserId?.copyOf(),
        )
    }

    private fun scheduleAutomaticSubnetScan(delayMs: Long) {
        mainHandler.removeCallbacks(automaticScanRunnable)
        if (!started || wifiNetwork == null) return
        mainHandler.postDelayed(automaticScanRunnable, delayMs)
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
                        version == "1" &&
                        serviceInfo.port in 1..65_535 &&
                        (
                            !supportsNetworkScopedNsd() ||
                                serviceInfo.network == null ||
                                serviceInfo.network == wifiNetwork
                            )
                    ) {
                        scanPlanner.onPeerEvidence(System.currentTimeMillis())
                        LanTransportDiagnostics.peerEvidence()
                        if (shouldInitiateLanConnection(instanceToken, token)) {
                            connectToService(serviceInfo)
                        } else {
                            val endpoint = resolvedHosts(serviceInfo).firstOrNull()
                                ?.let { endpointDisplay(InetSocketAddress(it, serviceInfo.port)) }
                                ?: serviceInfo.serviceName
                            Log.i(
                                TAG,
                                "Resolved LAN peer $endpoint; awaiting their connection (tie-break)",
                            )
                        }
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
        mainHandler.removeCallbacks(automaticScanRunnable)
        scanPlanner.onNetworkLost()
        runningSweep = null
        scanGeneration.incrementAndGet()
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
        reconnectTargets.clear()
        outboundServiceKeys.clear()
        serverSocket?.closeQuietly()
        serverSocket = null
        sockets.toList().forEach(Socket::closeQuietly)
        connections.clear()
        authenticatedUserIds.clear()
        endpointHint = null
        currentNetworkId = null
        LanTransportDiagnostics.waitingForWifi()
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

    private fun localEndpoint(network: Network, port: Int): LanManualEndpoint? {
        val addresses = connectivityManager.getLinkProperties(network)
            ?.linkAddresses
            ?.map { it.address }
            ?.filterNot { it.isAnyLocalAddress || it.isLoopbackAddress }
            .orEmpty()
        val address = addresses.firstOrNull { it is Inet4Address } ?: addresses.firstOrNull()
        return address?.hostAddress?.let { LanManualEndpoint(it, port) }
    }

    /**
     * The subnet prefix length the network advertises for [local] -- this is
     * what makes the scan cover a whole /16 cruise LAN rather than just our own
     * /24. Falls back to [DEFAULT_SCAN_PREFIX_LENGTH] when the platform reports
     * no matching link address; [subnetHosts] clamps the breadth either way.
     */
    private fun localPrefixLength(network: Network, local: Inet4Address): Int =
        connectivityManager.getLinkProperties(network)
            ?.linkAddresses
            ?.firstOrNull { it.address == local }
            ?.prefixLength
            ?: DEFAULT_SCAN_PREFIX_LENGTH

    private fun isEligibleWifiNetwork(network: Network): Boolean {
        val capabilities = connectivityManager.getNetworkCapabilities(network) ?: return false
        if (!capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI)) return false
        // minSdk is 31 (S), so NET_CAPABILITY_WIFI_P2P (added in API 31) is always checkable.
        if (capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_WIFI_P2P)) {
            return false
        }
        val interfaceName = connectivityManager.getLinkProperties(network)?.interfaceName.orEmpty()
        return !interfaceName.startsWith("p2p", ignoreCase = true)
    }

    @Suppress("DEPRECATION")
    private fun resolvedHosts(serviceInfo: NsdServiceInfo): List<InetAddress> {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            serviceInfo.hostAddresses
        } else {
            listOfNotNull(serviceInfo.host)
        }
    }

    private fun endpointDisplay(endpoint: InetSocketAddress): String {
        val host = endpoint.address?.hostAddress ?: endpoint.hostString
        return if (host.contains(':')) "[$host]:${endpoint.port}" else "$host:${endpoint.port}"
    }

    private fun supportsNetworkScopedNsd(): Boolean {
        return when {
            Build.VERSION.SDK_INT > Build.VERSION_CODES.TIRAMISU -> true
            Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU -> false
            else -> SdkExtensions.getExtensionVersion(Build.VERSION_CODES.TIRAMISU) >= 3
        }
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
        private const val SCAN_CONNECT_TIMEOUT_MS = 350
        // Sized for the /16 case: at 64-way parallelism a fully unresponsive
        // /16 finishes its ~65k probes in roughly six minutes worst case
        // (SCAN_CONNECT_TIMEOUT_MS per dead host), and far faster in practice
        // since unallocated addresses fail fast. A /24 still finishes in
        // seconds. Threads reap between scans -- see scanExecutor.
        private const val SCAN_CONCURRENCY = 64
        private const val SCAN_THREAD_KEEPALIVE_SECONDS = 10L
        private const val RECONNECT_SLOT_DELAY_MS = 5_000L
        private const val AUTO_SCAN_INITIAL_DELAY_MS = 5_000L
        private const val AUTO_SCAN_RECONNECT_DELAY_MS = 2_000L
        private const val AUTO_SCAN_ESCALATE_DELAY_MS = 2_000L
        private const val AUTO_SCAN_RETRY_INTERVAL_MS = 5 * 60_000L
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

    private data class ReconnectTarget(
        val endpoints: List<InetSocketAddress>,
        val expectedUserId: ByteArray?,
    )

    private data class RunningSweep(
        val outcomes: SweepOutcomes,
        val breadth: LanScanBreadth,
        val prefixLength: Int,
    )
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

internal fun shouldRunAutomaticLanScan(
    activeConnections: Int,
    outboundAttempts: Int,
    scanRemaining: Int,
): Boolean = activeConnections == 0 && outboundAttempts == 0 && scanRemaining == 0

internal fun shouldRetainLanReconnectTarget(
    serviceKey: String,
    wasAuthenticated: Boolean,
): Boolean = wasAuthenticated || !serviceKey.startsWith("scan:")

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
