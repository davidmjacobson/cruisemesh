package com.cruisemesh.app.mesh

import android.annotation.SuppressLint
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
import uniffi.cruisemesh_core.lanHostsShareLocalNetwork
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
    private val unlinkedCapableContacts: () -> Int,
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
    private val scanBuildGate = LanScanBuildGate()
    private val connectionBackoff = ReconnectBackoffTracker()
    private val sockets = ConcurrentHashMap.newKeySet<Socket>()
    private val connections = ConcurrentHashMap<String, LanConnection>()
    private val authenticatedUserIds = ConcurrentHashMap<String, String>()
    private val outboundServiceKeys = ConcurrentHashMap.newKeySet<String>()
    private val reconnectTargets = ConcurrentHashMap<String, ReconnectTarget>()
    private val resolvedServices = ConcurrentHashMap<String, NsdServiceInfo>()

    // Peer instance tokens peer evidence has already been counted for on
    // this network join -- repeated evidence about the same token (an
    // already-connected/linked peer's NSD record refreshing, or a resent
    // endpoint hint) must not keep resetting the full-sweep backoff. Cleared
    // alongside the other per-network state in teardownNetworkSession, and
    // bounded (oldest forgotten first) because the token is chosen by
    // whatever is advertising -- see BoundedLanKeySet.
    private val knownPeerInstanceTokens = BoundedLanKeySet(MAX_TRACKED_PEER_KEYS)

    // Keys an election-loser fallback connect has already been scheduled for
    // on this network join, so repeated NSD re-resolves of the same service
    // don't stack duplicate fallback timers. Cleared with the other
    // per-network state in teardownNetworkSession, and bounded the same way.
    private val electionFallbackKeys = BoundedLanKeySet(MAX_TRACKED_PEER_KEYS)

    // Outbound service keys whose connection completed the Noise handshake.
    // outboundServiceKeys retains a key for the whole life of a healthy
    // outbound link, so "attempts still in flight" for the automatic-scan
    // gate is the set of outbound keys not yet in here. Tracking the
    // authenticated keys themselves rather than a count keeps the gate
    // self-correcting: clearing both sets on teardown while connections are
    // still winding down leaves a late per-connection cleanup as a harmless
    // no-op, where a counter would be driven below zero and wedge the gate
    // shut for the life of the process.
    private val authenticatedOutboundKeys = ConcurrentHashMap.newKeySet<String>()

    // Consecutive completed sweeps whose verdict was ISOLATION_SUSPECTED. A
    // single congested sweep can look isolated (every probe timing out), so
    // the planner is only told to back off to its cap once the verdict
    // repeats.
    private val consecutiveIsolationVerdicts = AtomicInteger(0)

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

    // API 34+ continuous service-info trackers, keyed by service name. Main
    // handler only; bounded by MAX_SERVICE_INFO_CALLBACKS and cleared with
    // the other per-network state in teardownNetworkSession.
    private val serviceInfoCallbacks = mutableMapOf<String, NsdManager.ServiceInfoCallback>()
    private val pendingServices = ArrayDeque<NsdServiceInfo>()
    // Service names discovery has already queued for resolution on this
    // network join. Bounded (oldest forgotten first) rather than add-only:
    // the names come from whatever advertises here, and refusing new ones at
    // a cap would silently lock out a real family member arriving later.
    private val queuedServiceNames = BoundedLanKeySet(MAX_TRACKED_PEER_KEYS)
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
                pendingOutboundAttempts = pendingLanOutboundAttempts(
                    outboundServiceKeys,
                    authenticatedOutboundKeys,
                ),
                scanRemaining = runningSweep?.outcomes?.remainingCandidates() ?: 0,
                unlinkedCapableContacts = unlinkedCapableContacts(),
            )
        ) {
            scanPlanner.takeDueScan(System.currentTimeMillis())?.let { breadth ->
                Log.i(TAG, "Starting automatic local Wi-Fi fallback search (${breadth.name})")
                startSubnetScan(breadth, automatic = true)
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

    fun startSubnetScan(
        breadth: LanScanBreadth = LanScanBreadth.FULL_SUBNET,
        automatic: Boolean = false,
    ): String? {
        if (!started) return "Start the mesh before searching the local subnet"
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
        // The automatic full-subnet sweep is capped at /20 (~4,094 hosts);
        // the user-initiated "Search my local network" button (automatic ==
        // false) keeps the wider /16 (~65k hosts) ceiling since the user
        // explicitly asked for it.
        val effectivePrefix = if (automatic) {
            effectiveAutomaticScanPrefixLength(prefixLength)
        } else {
            effectiveScanPrefixLength(prefixLength)
        }
        // Reserve the build before leaving this thread. `runningSweep` cannot
        // be published until candidate materialization finishes on the scan
        // executor; the reservation closes that gap for manual double-taps
        // and automatic/manual overlap.
        val buildToken = scanBuildGate.tryReserve(
            sweepRunning = { (runningSweep?.outcomes?.remainingCandidates() ?: 0) > 0 },
            nextGeneration = scanGeneration::incrementAndGet,
        ) ?: return "A local subnet search is already running"
        // G5: build the host list and enqueue probes off the calling thread
        // (often main via UI / connectivity callbacks) so a /24 or larger
        // sweep cannot ANR during candidate materialization.
        val generation = buildToken.generation
        try {
            scanExecutor.execute {
                try {
                    val candidates = subnetHosts(local, effectivePrefix).shuffled()
                    val sweep = RunningSweep(
                        outcomes = SweepOutcomes(generation, candidates.size),
                        breadth = breadth,
                        prefixLength = effectivePrefix,
                    )
                    val activated = scanBuildGate.activate(buildToken) {
                        if (
                            !started ||
                            wifiNetwork != network ||
                            scanGeneration.get() != generation
                        ) {
                            false
                        } else {
                            runningSweep = sweep
                            true
                        }
                    }
                    if (!activated) return@execute
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
                } catch (error: RuntimeException) {
                    Log.w(TAG, "Could not build the local subnet search", error)
                } finally {
                    scanBuildGate.release(buildToken)
                }
            }
        } catch (_: RuntimeException) {
            scanBuildGate.release(buildToken)
            return "Could not start the local subnet search"
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
                // A healthy scan-dialled link holds its key for its whole
                // life, so an authenticated key here means this probe just
                // re-found a friend an earlier sweep already linked. That is
                // a find -- without crediting it, every sweep after the one
                // that linked the family reports "nobody home" and arms the
                // expensive tier on a demonstrably working network.
                if (
                    lanSweepProbeFoundFriend(
                        keyAlreadyAuthenticated = serviceKey in authenticatedOutboundKeys,
                        linkTableFull = false,
                        authenticatedLinks = authenticatedUserIds.size,
                    )
                ) {
                    markSweepFoundFriend(sweep)
                }
                socket.closeQuietly()
                return
            }
            if (!tryAcquireSocketSlot()) {
                // The link table is full, which with a friend on one of those
                // links is the healthiest network there is -- not an empty
                // one. A table of in-flight handshakes to unrelated services
                // is not, hence the authenticated-link requirement.
                if (
                    lanSweepProbeFoundFriend(
                        keyAlreadyAuthenticated = false,
                        linkTableFull = true,
                        authenticatedLinks = authenticatedUserIds.size,
                    )
                ) {
                    markSweepFoundFriend(sweep)
                }
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
                sweep = sweep,
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
     * that authenticated NO friend is what arms the full sweep
     * ([LanScanPlanner.onScanCompleted]'s `foundPeer`) -- a raw TCP connect
     * is deliberately not enough, because any unrelated service (or a
     * stranger's CruiseMesh) squatting the default port would otherwise
     * disarm the wider sweep that could still find an actual friend. A
     * friend whose handshake finishes after the last probe retires can
     * spuriously arm the tier; the automatic-scan gate keeps that armed
     * tier from firing unless some capable contact is still unlinked. Also
     * pulls the next check forward so escalation doesn't wait out the
     * periodic interval.
     */
    private fun onScanCompleted(sweep: RunningSweep, summary: SweepOutcomeSummary) {
        if (
            !scanBuildGate.finishSweep(
                isCurrent = {
                    runningSweep === sweep && sweep.outcomes.generation == scanGeneration.get()
                },
                clear = { runningSweep = null },
            )
        ) {
            return
        }
        Log.i(TAG, summary.logLine(sweep.prefixLength))
        scanPlanner.onScanCompleted(
            sweep.breadth,
            System.currentTimeMillis(),
            sweep.authenticatedFriend,
        )
        val verdict = lanSweepVerdict(summary)
        LanTransportDiagnostics.sweepCompleted(summary)
        when (verdict) {
            LanSweepVerdict.ISOLATION_SUSPECTED -> {
                // One congested sweep can time out every probe and look
                // isolated; only a repeat verdict jumps the planner to its
                // backoff cap.
                if (consecutiveIsolationVerdicts.incrementAndGet() >= ISOLATION_CONFIRM_SWEEPS) {
                    scanPlanner.onIsolationSuspected(System.currentTimeMillis())
                }
            }
            else -> consecutiveIsolationVerdicts.set(0)
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
            val hintKey = lanHintConnectKey(remoteToken)
            val endpoint = LanManualEndpoint(hint.host, hint.port.toInt())
            // The dial below is deliberately attempted even when the hinted
            // host sits on another subnet -- a routed LAN can carry TCP where
            // mDNS cannot, and it is one bounded attempt Noise authenticates.
            // Filing that address in the endpoint cache is a different matter:
            // the cache is keyed by THIS phone's network, is re-dialed on
            // every Wi-Fi join and lives for seven days, so a foreign-subnet
            // host written here became a permanent background probe of an
            // address that can never answer on this network. Only remember an
            // address we can show is on the network we are on.
            //
            // When this phone has no comparable address of its own the hint
            // is simply not filed. That is the safe direction, and it is not
            // the last chance to learn the address: if the dial below
            // authenticates, onLanPeerAuthenticated files the endpoint on the
            // stronger authority of having reached it.
            val localHost = endpointHint?.host
            if (
                localHost != null &&
                lanHostsShareLocalNetwork(localHost = localHost, candidateHost = endpoint.host)
            ) {
                onEndpointObserved(expectedUserId, endpoint, currentNetworkId)
            }
            if (!started) return@post
            notePeerEvidence(remoteToken)
            if (!shouldInitiateLanConnection(instanceToken, remoteToken)) {
                Log.i(
                    TAG,
                    "Resolved LAN peer ${endpoint.display}; awaiting their connection (tie-break)",
                )
                scheduleElectionFallback(
                    key = hintKey,
                    endpoints = listOf(InetSocketAddress(endpoint.host, endpoint.port)),
                    expectedUserId = expectedUserId,
                )
                return@post
            }
            val network = wifiNetwork ?: return@post
            Log.i(TAG, "BLE introduced LAN peer at ${endpoint.display}")
            connectToEndpoints(
                network = network,
                key = hintKey,
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
                key = lanCachedConnectKey(expectedUserId.toHex(), endpoint.display),
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
            if (supportsNetworkScopedServiceInfo()) {
                setNetworkCompat(this, network)
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
            if (supportsNetworkScopedDiscovery()) {
                discoverServicesOnNetwork(network, discovery)
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
            supportsNetworkScopedServiceInfo() &&
            networkCompat(serviceInfo) != null &&
            networkCompat(serviceInfo) != network
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
        // A hinted address is one the contact told us about, not one this
        // phone discovered for itself, so it gets a single attempt: no
        // reconnect target means every scheduleReconnect for the key finds
        // nothing to retry. A later hint (or discovery, or the cached
        // endpoint) can try again -- the attempt just never reschedules
        // itself. Completing Noise is what promotes such an address from
        // "claimed" to "proven": runConnection remembers it then, so a link
        // that really worked still comes back on the reconnect timer.
        if (!isSingleShotLanConnectKey(key)) {
            rememberReconnectTarget(key, endpoints, expectedUserId)
        }
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
                    // A single-shot key only holds a reconnect target because
                    // this address once authenticated. It just failed to
                    // answer, so it is unproven again -- retire it rather than
                    // let one good handshake license a standing probe.
                    if (isSingleShotLanConnectKey(key)) reconnectTargets.remove(key)
                    scheduleReconnect(key)
                    // A failed direct connect is exactly when a fallback
                    // sweep becomes worth checking promptly, instead of
                    // waiting out the periodic interval.
                    scheduleAutomaticSubnetScan(AUTO_SCAN_RECONNECT_DELAY_MS)
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
        /** The sweep that dialed this candidate, if any -- see [markSweepFoundFriend]. */
        sweep: RunningSweep? = null,
    ) {
        sockets += socket
        val peerEndpoint = socket.remoteSocketAddress?.toString()?.removePrefix("/") ?: "peer"
        var address: String? = null
        var connection: LanConnection? = null
        var noise: LanNoiseSession? = null
        var authenticated = false
        var abortedDuplicateLink = false
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
                if (authenticatedUserIds.containsValue(userId.toHex())) {
                    // Election fallbacks and sweeps may dial a contact that
                    // connected to us in the meantime. Close the redundant
                    // socket before it becomes a second live link -- but a
                    // sweep that ran into a friend it is already linked to
                    // has still proved discovery works on this network, so
                    // credit it exactly as an authenticated find would.
                    // Otherwise every sweep on a healthy network reports
                    // "found nobody" and arms the expensive full tier.
                    markSweepFoundFriend(sweep)
                    abortedDuplicateLink = true
                    throw IOException("Contact already has an active LAN link")
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
                if (isSingleShotLanConnectKey(key) && advertisedEndpoint != null) {
                    // A hinted or cached address is dialed once precisely
                    // because nothing proved it was real. Finishing Noise is
                    // that proof, so it earns a reconnect target now: a link
                    // the AP or Doze kills comes back on the backoff timer
                    // instead of waiting for the next Wi-Fi join. If the
                    // retry then fails, connectToEndpoints retires the target
                    // again and the address is back to single-shot.
                    rememberReconnectTarget(key, listOf(advertisedEndpoint), trustedUserId)
                } else {
                    // computeIfPresent keeps this a single atomic step; scan
                    // and reconnect attempts touch the same key from other
                    // executor threads and a plain read-modify-write could
                    // lose a racing update to this reconnect target's
                    // endpoint list.
                    reconnectTargets.computeIfPresent(key) { _, target ->
                        target.copy(expectedUserId = trustedUserId.copyOf())
                    }
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
            if (outboundServiceKey != null) {
                authenticatedOutboundKeys += outboundServiceKey
                if (outboundServiceKey.startsWith("scan:")) {
                    // Only an authenticated friend counts as a sweep find --
                    // see onScanCompleted. Harmless no-op if the sweep has
                    // already completed or been replaced.
                    markSweepFoundFriend(sweep)
                }
            }
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
                authenticatedOutboundKeys.remove(it)
                if (abortedDuplicateLink) {
                    // Not a failure: the contact already has a live LAN
                    // link. No backoff, no reconnect -- rediscovery covers
                    // a later drop of the surviving link.
                    reconnectTargets.remove(it)
                } else if (shouldRetainLanReconnectTarget(it, authenticated)) {
                    connectionBackoff.recordFailure(it, System.currentTimeMillis())
                    scheduleReconnect(it)
                } else {
                    reconnectTargets.remove(it)
                }
            }
            if (authenticated) {
                scheduleAutomaticSubnetScan(AUTO_SCAN_RECONNECT_DELAY_MS)
            } else if (
                outboundServiceKey != null &&
                !abortedDuplicateLink &&
                !outboundServiceKey.startsWith("scan:")
            ) {
                // A failed secure setup to a discovered/hinted peer: check
                // promptly whether a fallback sweep is due instead of
                // waiting out the periodic interval.
                scheduleAutomaticSubnetScan(AUTO_SCAN_RECONNECT_DELAY_MS)
            }
        }
    }

    /**
     * The tie-break said the peer initiates -- but discovery is often
     * asymmetric (the peer may never have resolved us, or its connect may
     * fail), which used to strand both sides forever. If nothing has
     * connected for this key within [ELECTION_FALLBACK_DELAY_MS], initiate
     * anyway: duplicate connections are safe by design (spec: msg_id
     * deduplication), and [runConnection]'s duplicate-link guard closes a
     * redundant socket mid-handshake before it becomes a second live link.
     */
    private fun scheduleElectionFallback(
        key: String,
        endpoints: List<InetSocketAddress>,
        expectedUserId: ByteArray?,
    ) {
        if (endpoints.isEmpty()) return
        val scheduledNetwork = wifiNetwork ?: return
        if (!electionFallbackKeys.claim(key, ::logForgottenLanKey)) return
        mainHandler.postDelayed(
            {
                if (!started || wifiNetwork != scheduledNetwork) return@postDelayed
                Log.d(TAG, "Tie-break peer never connected; initiating ourselves")
                connectToEndpoints(scheduledNetwork, key, endpoints, expectedUserId)
            },
            ELECTION_FALLBACK_DELAY_MS,
        )
    }

    private fun scheduleReconnect(serviceKey: String) {
        val now = System.currentTimeMillis()
        // retryDelayMs is non-null for any key with failure history — including
        // given-up keys, which now decay to ReconnectBackoffTracker's slow probe
        // cadence instead of stopping forever (the null fallback is only a
        // never-failed key's first schedule).
        val delayMs = connectionBackoff.retryDelayMs(serviceKey, now) ?: RECONNECT_SLOT_DELAY_MS
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

    /**
     * A peer advertised itself (NSD resolution or an endpoint hint) under
     * [token]. Only genuinely NEW evidence is worth reacting to: an
     * already-connected/linked peer's record keeps reappearing (re-resolves,
     * periodic discovery updates, resent hints) and must not keep
     * re-triggering full sweeps.
     *
     * The token is chosen by whatever is advertising, so "new" is not a
     * trustworthy signal on a busy or hostile network: the remembered set is
     * bounded ([BoundedLanKeySet]) and [LanScanPlanner.onPeerEvidence] only
     * rewinds the sweep schedule a bounded number of times per network join.
     * Past either bound the caller still discovers and dials the peer
     * normally -- only the schedule pull-forward stops.
     */
    private fun notePeerEvidence(token: String) {
        if (!knownPeerInstanceTokens.claim(token, ::logForgottenLanKey)) return
        LanTransportDiagnostics.peerEvidence()
        if (!scanPlanner.onPeerEvidence(System.currentTimeMillis())) return
        scheduleAutomaticSubnetScan(PEER_EVIDENCE_SCAN_DELAY_MS)
    }

    /**
     * A per-network-join key was forgotten to stay inside [BoundedLanKeySet]'s
     * bound, which only happens when far more distinct services or tokens
     * have appeared on this Wi-Fi than any real fleet produces.
     */
    private fun logForgottenLanKey(key: String) {
        Log.i(TAG, "Forgetting the oldest tracked local Wi-Fi peer to make room (${key.take(8)})")
    }

    /**
     * Credits [sweep] with having found a friend on this LAN. Only an
     * authenticated friend -- or one this sweep discovered is already linked
     * -- counts; see [onScanCompleted]. The generation check keeps a
     * handshake that finishes after its own sweep was replaced or cancelled
     * from crediting whatever sweep is running now.
     */
    private fun markSweepFoundFriend(sweep: RunningSweep?) {
        val dialed = sweep ?: return
        if (
            !lanSweepCreditApplies(
                sweepGeneration = dialed.outcomes.generation,
                currentGeneration = scanGeneration.get(),
                sweepStillRunning = runningSweep === dialed,
            )
        ) {
            return
        }
        dialed.authenticatedFriend = true
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
                // Bounded: the discovered-service set is fed by whatever
                // advertises on this Wi-Fi, and every entry costs a resolve.
                if (!queuedServiceNames.claim(name, ::logForgottenLanKey)) return@post
                when (
                    lanServiceRoute(
                        sdkInt = Build.VERSION.SDK_INT,
                        liveServiceInfoCallbacks = serviceInfoCallbacks.size,
                        maxServiceInfoCallbacks = MAX_SERVICE_INFO_CALLBACKS,
                    )
                ) {
                    LanServiceRoute.LIVE_CALLBACK -> registerServiceInfoCallback(serviceInfo)
                    LanServiceRoute.ONE_SHOT_RESOLVE -> fallBackToDeprecatedResolve(serviceInfo)
                }
            }
        }

        override fun onServiceLost(serviceInfo: NsdServiceInfo) {
            mainHandler.post {
                val name = serviceInfo.serviceName
                unregisterServiceInfoCallback(name)
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

    /**
     * Service info arrived for a discovered peer, from either resolution
     * path (the deprecated one-shot resolve below API 34, or the continuous
     * service-info callback above it). Both paths must behave identically:
     * TXT version/token validation, the per-network-join peer-evidence
     * dedup, the initiator election, and the election fallback for the
     * tie-break loser all live here so neither path can drift.
     *
     * Called on the main handler.
     */
    private fun handleResolvedService(serviceInfo: NsdServiceInfo) {
        if (!started) return
        val token = serviceInfo.attributes[TXT_INSTANCE]?.toString(Charsets.UTF_8)
        val version = serviceInfo.attributes[TXT_VERSION]?.toString(Charsets.UTF_8)
        if (
            token == null ||
            version != "1" ||
            serviceInfo.port !in 1..65_535 ||
            (
                supportsNetworkScopedServiceInfo() &&
                    networkCompat(serviceInfo) != null &&
                    networkCompat(serviceInfo) != wifiNetwork
                )
        ) {
            return
        }
        notePeerEvidence(token)
        if (shouldInitiateLanConnection(instanceToken, token)) {
            connectToService(serviceInfo)
        } else {
            val endpoints = resolvedHosts(serviceInfo)
                .map { InetSocketAddress(it, serviceInfo.port) }
            Log.i(
                TAG,
                "Resolved LAN peer ${
                    endpoints.firstOrNull()?.let(::endpointDisplay)
                        ?: serviceInfo.serviceName
                }; awaiting their connection (tie-break)",
            )
            scheduleElectionFallback(
                key = serviceInfo.serviceName,
                endpoints = endpoints,
                expectedUserId = null,
            )
        }
    }

    /**
     * API 34+ replacement for [resolveNext]'s deprecated one-shot resolve.
     *
     * `registerServiceInfoCallback` keeps delivering service info for as long
     * as the record lives, so a slow or failed first resolve no longer drops
     * the peer until mDNS refreshes it. Live callbacks are unregistered on
     * service loss and network teardown. Callers route here only while a slot
     * is free (see [lanServiceRoute]); if registration is rejected the service
     * falls back to the deprecated queue so discovery still works.
     *
     * Called on the main handler.
     */
    @SuppressLint("NewApi")
    private fun registerServiceInfoCallback(found: NsdServiceInfo) {
        val name = found.serviceName
        if (serviceInfoCallbacks.containsKey(name)) return
        val request = NsdServiceInfo().apply {
            serviceName = name
            serviceType = lanServiceType()
            wifiNetwork?.let { network ->
                if (supportsNetworkScopedServiceInfo()) setNetworkCompat(this, network)
            }
        }
        lateinit var callback: NsdManager.ServiceInfoCallback
        callback = object : NsdManager.ServiceInfoCallback {
            override fun onServiceInfoCallbackRegistrationFailed(errorCode: Int) {
                mainHandler.post {
                    if (serviceInfoCallbacks[name] !== callback) return@post
                    // Never registered, so it must not be unregistered.
                    serviceInfoCallbacks.remove(name)
                    Log.w(TAG, "LAN service info callback registration failed: $errorCode")
                    fallBackToDeprecatedResolve(found)
                }
            }

            override fun onServiceUpdated(serviceInfo: NsdServiceInfo) {
                mainHandler.post {
                    if (serviceInfoCallbacks[name] !== callback) return@post
                    handleResolvedService(serviceInfo)
                }
            }

            override fun onServiceLost() {
                mainHandler.post {
                    if (serviceInfoCallbacks[name] !== callback) return@post
                    unregisterServiceInfoCallback(name)
                    resolvedServices.remove(name)
                    queuedServiceNames.remove(name)
                }
            }

            override fun onServiceInfoCallbackUnregistered() = Unit
        }
        serviceInfoCallbacks[name] = callback
        try {
            nsdManager.registerServiceInfoCallback(request, appContext.mainExecutor, callback)
        } catch (error: RuntimeException) {
            serviceInfoCallbacks.remove(name)
            Log.d(TAG, "Unable to track LAN service info", error)
            fallBackToDeprecatedResolve(found)
        }
    }

    /**
     * The deprecated one-shot resolve, which is the only path a pre-34 device
     * ever had. API 34+ services that cannot hold a live callback -- the cap
     * is full, or registration was rejected -- come here too, so those peers
     * degrade to a single resolve instead of never being resolved at all.
     */
    private fun fallBackToDeprecatedResolve(found: NsdServiceInfo) {
        if (!started || !queuedServiceNames.contains(found.serviceName)) return
        pendingServices.addLast(found)
        resolveNext()
    }

    @SuppressLint("NewApi")
    private fun unregisterServiceInfoCallback(name: String) {
        // Teardown paths call this on every API level; below 34 nothing was
        // ever registered and the platform type must not be touched at all.
        if (!supportsServiceInfoCallback(Build.VERSION.SDK_INT)) return
        val callback = serviceInfoCallbacks.remove(name) ?: return
        try {
            nsdManager.unregisterServiceInfoCallback(callback)
        } catch (_: RuntimeException) {
            // Not registered or already unregistered; a late callback is
            // ignored because the map no longer holds it.
        }
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
                    handleResolvedService(serviceInfo)
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
        scanBuildGate.reset {
            runningSweep = null
            scanGeneration.incrementAndGet()
        }
        discoveryListener?.let(::stopDiscovery)
        discoveryListener = null
        registrationListener?.let(::unregisterService)
        registrationListener = null
        resolveListener = null
        resolving = false
        serviceInfoCallbacks.keys.toList().forEach(::unregisterServiceInfoCallback)
        serviceInfoCallbacks.clear()
        pendingServices.clear()
        queuedServiceNames.clear()
        requestedServiceName = null
        registeredServiceName = null
        resolvedServices.clear()
        knownPeerInstanceTokens.clear()
        electionFallbackKeys.clear()
        consecutiveIsolationVerdicts.set(0)
        authenticatedOutboundKeys.clear()
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

    private fun supportsNetworkScopedServiceInfo(): Boolean =
        supportsNetworkScopedServiceInfo(
            sdkInt = Build.VERSION.SDK_INT,
            tiramisuExtension = SdkExtensions.getExtensionVersion(Build.VERSION_CODES.TIRAMISU),
        )

    private fun supportsNetworkScopedDiscovery(): Boolean =
        supportsNetworkScopedDiscovery(
            sdkInt = Build.VERSION.SDK_INT,
            tiramisuExtension = SdkExtensions.getExtensionVersion(Build.VERSION_CODES.TIRAMISU),
        )

    // Lint cannot propagate the combined platform/SDK-extension guards through
    // helper methods. Keep the suppressions on the smallest possible wrappers;
    // every caller first checks the corresponding tested support predicate.
    @SuppressLint("NewApi")
    private fun setNetworkCompat(serviceInfo: NsdServiceInfo, network: Network) {
        serviceInfo.setNetwork(network)
    }

    @SuppressLint("NewApi")
    private fun networkCompat(serviceInfo: NsdServiceInfo): Network? = serviceInfo.network

    @SuppressLint("NewApi")
    private fun discoverServicesOnNetwork(
        network: Network,
        discovery: NsdManager.DiscoveryListener,
    ) {
        val request = DiscoveryRequest.Builder(lanServiceType())
            .setNetwork(network)
            .build()
        nsdManager.discoverServices(request, appContext.mainExecutor, discovery)
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

        // Live API 34+ service-info callbacks. Each one is a standing
        // platform registration, so the count is bounded the same way live
        // sockets are; peers past the cap still get a one-shot resolve.
        private const val MAX_SERVICE_INFO_CALLBACKS = 8
        private const val CONNECT_TIMEOUT_MS = 3_000
        private const val HANDSHAKE_TIMEOUT_MS = 5_000
        private const val SCAN_CONNECT_TIMEOUT_MS = 350
        // Sized for the manual /16 case: at 64-way parallelism a fully
        // unresponsive /16 finishes its ~65k probes in roughly six minutes
        // worst case (SCAN_CONNECT_TIMEOUT_MS per dead host), and far faster
        // in practice since unallocated addresses fail fast. The automatic
        // full sweep is capped at /20 (~4,094 hosts, well under 30s worst
        // case); a /24 still finishes in seconds. Threads reap between
        // scans -- see scanExecutor.
        private const val SCAN_CONCURRENCY = 64
        private const val SCAN_THREAD_KEEPALIVE_SECONDS = 10L
        private const val RECONNECT_SLOT_DELAY_MS = 5_000L
        private const val AUTO_SCAN_INITIAL_DELAY_MS = 5_000L
        private const val AUTO_SCAN_RECONNECT_DELAY_MS = 2_000L

        // How long the tie-break loser waits for the elected side's
        // connection before initiating anyway. Covers the winner's worst
        // case (connect timeout + handshake timeout) with margin.
        private const val ELECTION_FALLBACK_DELAY_MS = 15_000L

        // Prompt scan-check pull-forward when fresh peer evidence arrives;
        // the planner and loneliness gate still decide whether anything runs.
        private const val PEER_EVIDENCE_SCAN_DELAY_MS = 2_000L

        // Ceiling on the per-network-join bookkeeping sets (seen peer
        // tokens, scheduled election fallbacks, queued service names). Their
        // keys come from whatever advertises on the Wi-Fi, so they are only
        // as bounded as the network is honest; 256 is far above any real
        // fleet and keeps a busy network from growing them without limit.
        private const val MAX_TRACKED_PEER_KEYS = 256

        // Consecutive ISOLATION_SUSPECTED sweep verdicts required before the
        // planner defers full sweeps to its backoff cap.
        private const val ISOLATION_CONFIRM_SWEEPS = 2

        // A prompt recheck after a /24 sweep completes, not an escalation
        // trigger by itself: LanScanPlanner only arms the full-subnet tier
        // on an empty /24 sweep and holds it off for
        // LanScanPlanner.EMPTY_LOCAL_SWEEP_FULL_DELAY_MS (60s) after that,
        // so this recheck will usually find nothing due yet.
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

    private class RunningSweep(
        val outcomes: SweepOutcomes,
        val breadth: LanScanBreadth,
        val prefixLength: Int,
    ) {
        /**
         * A "scan:"-keyed connection completed the Noise handshake with an
         * accepted friend while this sweep ran. This -- not a bare TCP
         * connect -- is what [LanScanPlanner.onScanCompleted] receives as
         * `foundPeer`, so unrelated services on the default port can't
         * disarm the full-subnet tier.
         */
        @Volatile
        var authenticatedFriend = false
    }
}

internal fun trustedLanPeerUserId(contacts: List<Contact>, remoteStaticKey: ByteArray): ByteArray? =
    contacts.firstOrNull { it.agreePk.contentEquals(remoteStaticKey) }?.userId?.copyOf()

/** True when the Noise static key is this device's own agreement key — a live clone. */
internal fun ownLanStaticKeyMatches(ownAgreePk: ByteArray, remoteStaticKey: ByteArray): Boolean =
    ownAgreePk.contentEquals(remoteStaticKey)

internal fun sameLanServiceType(value: String): Boolean =
    value.trimEnd('.') == lanServiceType().trimEnd('.')

/**
 * Both peers discover each other. The opaque per-process tokens provide a
 * stable tie-break so exactly one side opens the TCP connection.
 */
internal fun shouldInitiateLanConnection(localToken: String, remoteToken: String): Boolean =
    localToken != remoteToken && localToken < remoteToken

internal enum class LanServiceRoute { LIVE_CALLBACK, ONE_SHOT_RESOLVE }

/**
 * How a newly found LAN service gets resolved.
 *
 * A live API 34+ service-info callback is a standing platform registration,
 * so only so many are held at once. Everything else -- older Android, and the
 * peers a dense network turns up once the callbacks are full -- takes the
 * deprecated one-shot resolve. A ship's Wi-Fi can advertise far more services
 * than the cap (a whole fleet plus strangers on the same service type), and a
 * peer discovered past it must still be resolved rather than sit invisible
 * for the rest of the Wi-Fi session.
 */
internal fun lanServiceRoute(
    sdkInt: Int,
    liveServiceInfoCallbacks: Int,
    maxServiceInfoCallbacks: Int,
): LanServiceRoute =
    if (
        supportsServiceInfoCallback(sdkInt) &&
        liveServiceInfoCallbacks < maxServiceInfoCallbacks
    ) {
        LanServiceRoute.LIVE_CALLBACK
    } else {
        LanServiceRoute.ONE_SHOT_RESOLVE
    }

/**
 * Outbound connection attempts that have not reached an authenticated link
 * yet: every dialled service key without a live authenticated connection.
 * Derived from the two live sets rather than tracked as a running total, so
 * the result can never drift below zero and permanently close the
 * automatic-scan gate.
 */
internal fun pendingLanOutboundAttempts(
    outboundServiceKeys: Set<String>,
    authenticatedOutboundKeys: Set<String>,
): Int = outboundServiceKeys.count { it !in authenticatedOutboundKeys }

/**
 * The "have I already handled this?" memory the transport keeps for one
 * network join: seen peer tokens, scheduled election fallbacks, queued
 * service names.
 *
 * Every key comes from something a device on the Wi-Fi chose, so the set
 * cannot be bounded by honesty alone -- a network full of made-up names
 * would grow it without limit. It is therefore capped at [limit], and at the
 * cap the OLDEST key is forgotten rather than the newest refused. That
 * direction matters: refusing new keys would let a flood of made-up ones
 * permanently lock out a real family member who joins afterwards, silently.
 * Forgetting the oldest only risks repeating work already done once, which
 * every caller here tolerates.
 */
internal class BoundedLanKeySet(private val limit: Int) {
    private val keys = LinkedHashSet<String>()

    /**
     * Records [key] and reports whether it is brand-new work. [onEvicted]
     * runs for a key forgotten to make room (for logging; it is never the
     * key just claimed).
     */
    @Synchronized
    fun claim(key: String, onEvicted: (String) -> Unit = {}): Boolean {
        if (!keys.add(key)) return false
        while (keys.size > limit) {
            val oldest = keys.iterator().next()
            keys.remove(oldest)
            onEvicted(oldest)
        }
        return true
    }

    @Synchronized
    fun contains(key: String): Boolean = key in keys

    @Synchronized
    fun remove(key: String) {
        keys.remove(key)
    }

    @Synchronized
    fun clear() {
        keys.clear()
    }

    @Synchronized
    fun size(): Int = keys.size
}

/**
 * Whether a sweep probe that stopped before it could open a link still found
 * a friend on this network.
 *
 * Both stopping points look like "nothing here" from inside the probe and
 * are anything but: [keyAlreadyAuthenticated] means the address is already
 * carrying an authenticated link to a friend (a healthy link holds its
 * service key for its whole life, so every sweep after the one that linked
 * the family collides here), and a full link table with a friend on it is
 * the healthiest network there is. A full table of in-flight handshakes to
 * unrelated services is not, hence [authenticatedLinks].
 */
internal fun lanSweepProbeFoundFriend(
    keyAlreadyAuthenticated: Boolean,
    linkTableFull: Boolean,
    authenticatedLinks: Int,
): Boolean = keyAlreadyAuthenticated || (linkTableFull && authenticatedLinks > 0)

/**
 * Whether a connection dialed by the sweep at [sweepGeneration] may still
 * credit that sweep with a find. A handshake can finish after its own sweep
 * completed, was cancelled, or was replaced by a newer one; crediting then
 * would either do nothing useful or, worse, mark a sweep that never met the
 * peer.
 */
internal fun lanSweepCreditApplies(
    sweepGeneration: Int,
    currentGeneration: Int,
    sweepStillRunning: Boolean,
): Boolean = sweepStillRunning && sweepGeneration == currentGeneration

/**
 * Whether a contact that once demonstrated LAN support should still keep the
 * automatic sweep running. Capability itself never expires -- a contact who
 * supports LAN endpoints always will -- but "might be on this Wi-Fi right
 * now" does, and that is what the sweep is spending battery on. Without a
 * bound, one family member who stayed ashore keeps every remaining phone
 * sweeping the subnet forever.
 *
 * [LAN_CAPABILITY_RECENCY_WINDOW_MS] is deliberately generous: any LAN link,
 * any endpoint hint over BLE, and any hint through the relay all refresh the
 * timestamp, so a contact who is genuinely nearby re-motivates sweeps within
 * seconds of the first contact of a trip. A contact with no such evidence for
 * two weeks is not worth a subnet sweep every five minutes.
 */
internal fun lanCapabilityMotivatesScan(
    lastSupportedAtMs: Long?,
    nowMs: Long,
    windowMs: Long = LAN_CAPABILITY_RECENCY_WINDOW_MS,
): Boolean {
    val lastSeen = lastSupportedAtMs ?: return false
    return nowMs - lastSeen < windowMs
}

/** Two weeks; see [lanCapabilityMotivatesScan]. */
internal const val LAN_CAPABILITY_RECENCY_WINDOW_MS = 14L * 24 * 60 * 60 * 1_000

/**
 * Whether the periodic check may claim a scan from [LanScanPlanner]. A scan
 * is worthwhile while the transport has no links at all, OR while some
 * contact that has recently demonstrated LAN support still has no
 * authenticated LAN link ([lanCapabilityMotivatesScan]) -- one connected
 * family member must not stop discovery of the rest.
 * In-flight work (pending outbound attempts, a running sweep) always defers.
 */
internal fun shouldRunAutomaticLanScan(
    activeConnections: Int,
    pendingOutboundAttempts: Int,
    scanRemaining: Int,
    unlinkedCapableContacts: Int,
): Boolean = (activeConnections == 0 || unlinkedCapableContacts > 0) &&
    // <= 0, not == 0: a caller that ever miscounts in-flight work low must
    // slow discovery down, never disable it.
    pendingOutboundAttempts <= 0 &&
    scanRemaining <= 0

/**
 * Whether a closed link's reconnect target survives the close.
 *
 * An authenticated link always keeps it: the address is proven, and the peer
 * may simply have gone to sleep. A close without authentication keeps it only
 * for evidence this phone gathered itself and can gather again -- a subnet
 * sweep hit that turned out not to be a friend is dropped, and so is a hinted
 * or cached address, which only ever holds a target because an earlier
 * handshake proved it (see [isSingleShotLanConnectKey]). Failing now makes it
 * unproven again.
 */
internal fun shouldRetainLanReconnectTarget(
    serviceKey: String,
    wasAuthenticated: Boolean,
): Boolean = wasAuthenticated ||
    !(serviceKey.startsWith("scan:") || isSingleShotLanConnectKey(serviceKey))

/** Prefix marking a connection key that came from a contact's LAN hint. */
internal const val LAN_HINT_KEY_PREFIX = "hint:"

/** Prefix marking a connection key replayed from the saved endpoint cache. */
internal const val LAN_CACHED_KEY_PREFIX = "cache:"

internal fun lanHintConnectKey(remoteInstanceToken: String): String =
    "$LAN_HINT_KEY_PREFIX$remoteInstanceToken"

internal fun lanCachedConnectKey(userIdHex: String, endpointDisplay: String): String =
    "$LAN_CACHED_KEY_PREFIX$userIdHex:$endpointDisplay"

/**
 * Whether a connection key may only ever be attempted once per piece of
 * evidence. Keys this phone found itself (mDNS, a subnet scan, a manual
 * address a human typed) keep their reconnect target and retry on a timer.
 * Two kinds do not:
 *
 * - a hint carries an address supplied by the contact rather than one this
 *   phone observed, so it is tried when it arrives and never retried;
 * - a cached endpoint is a *remembered* hint, so it is no better evidence
 *   than the hint was. Retrying it on a timer is what turned a single stale
 *   address into a dial every sixty seconds for as long as the phone stayed
 *   on the network.
 *
 * Retry coverage is not lost. `MeshService.onLanNetworkReady` replays every
 * cached endpoint on each Wi-Fi join, so a cached address still gets one
 * attempt per network join, plus another whenever a fresh hint or discovery
 * arrives -- which is the only kind of event that can make a dead address
 * live again. And single-shot is only the state an *unproven* address is in:
 * once one of these completes a Noise handshake, `runConnection` gives it a
 * reconnect target like any other proven link, so a dropped link still comes
 * back on the timer. The target is retired the moment an attempt fails again.
 */
internal fun isSingleShotLanConnectKey(serviceKey: String): Boolean =
    serviceKey.startsWith(LAN_HINT_KEY_PREFIX) || serviceKey.startsWith(LAN_CACHED_KEY_PREFIX)

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
