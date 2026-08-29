package com.cruisemesh.app.mesh

import android.annotation.SuppressLint
import android.content.Context
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.net.nsd.DiscoveryRequest
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
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
import java.net.NetworkInterface
import java.net.ServerSocket
import java.net.Socket
import java.net.SocketException
import java.net.SocketTimeoutException
import java.security.SecureRandom
import java.util.ArrayDeque
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.RejectedExecutionException
import java.util.concurrent.ThreadPoolExecutor
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.CoreException
import uniffi.cruisemesh_core.CoreLanOwnDeviceProof
import uniffi.cruisemesh_core.CoreLanProofRole
import uniffi.cruisemesh_core.Frame
import uniffi.cruisemesh_core.Identity
import uniffi.cruisemesh_core.LanNoiseSession
import uniffi.cruisemesh_core.coreLanHostIsReachableEndpoint
import uniffi.cruisemesh_core.coreLanReconnectTargetIsExhausted
import uniffi.cruisemesh_core.coreLanScanGateOpen
import uniffi.cruisemesh_core.lanDefaultTcpPort
import uniffi.cruisemesh_core.lanHostsAreSameAddress
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
    /**
     * **The clone guard's one call site.** Raised once per link that reached the
     * own-device arm — the peer is nobody's contact — with the Noise static key
     * the far end proved it holds and the device id §10 step 5's roster proof
     * named for *this* session, or null when it named none.
     *
     * Deliberately here rather than inside [trustedPeerForStaticKey], which is
     * where it used to live. That lookup is asked in the middle of the
     * handshake, and a proof signs a transcript hash that is not final until the
     * last handshake message has gone out — so a guard placed there could never
     * have a device id to reason about, and passed a hardcoded null that
     * `core_own_identity_peer` can only read as a clone. This runs one decision
     * later, on both roles, and before the cross-connect rule has had a chance
     * to turn the link away: a peer that got far enough to present this
     * identity's key has been seen whether or not its socket is the one kept.
     *
     * Not raised for a contact's link, and not raised for a peer the proof
     * exchange refused. Neither can be holding this identity's own agreement
     * key — the clone arm of [acceptOwnDeviceOrRefuse] answers before any proof
     * is exchanged, so a peer that reaches the exchange at all is one the key
     * test has already cleared.
     *
     * The corollary is worth being blunt about: the two arms are disjoint, so on
     * today's wire this is raised either with a device id and a static key that
     * is *not* ours, or with our static key and a null device id — never both.
     * [OwnIdentityClonePolicy] therefore answers `SIBLING` for nobody yet. That
     * is a property of §10 step 5's symmetric clone arm, not of the rule, and
     * the rule is where it belongs so that changing the arm is the only work
     * left when a proof can cross it.
     */
    private val onOwnIdentityPeer: (ByteArray, ByteArray?) -> Unit,
    /**
     * This device's own §10 step 5 proof for a finished LAN Noise session:
     * [uniffi.cruisemesh_core.coreOwnDeviceLanProof] over the session's
     * transcript hash and the end this device is speaking from, signed with
     * this device's roster signing key.
     *
     * Null whenever no proof should go on the wire at all: an install that
     * holds no device key or no roster, and one whose roster names no device
     * but itself. See [acceptOwnDeviceOrRefuse].
     */
    private val ownDeviceLanProof: (
        handshakeHash: ByteArray,
        role: CoreLanProofRole,
    ) -> ByteArray?,
    /**
     * The peer's proof, checked against the roster this phone holds. Null for
     * anything that is not one of this person's devices, live or tombstoned.
     *
     * `peerRole` is the end the *peer* speaks from, which is always the
     * opposite of this device's: a proof minted for the other end does not
     * open, which is what stops a host we dialed from handing our own proof
     * straight back to us.
     */
    private val openOwnDeviceLanProof: (
        handshakeHash: ByteArray,
        payload: ByteArray,
        peerRole: CoreLanProofRole,
    ) -> CoreLanOwnDeviceProof?,
    private val unlinkedCapableContacts: () -> Int,
    /**
     * Whether this phone is inside the bounded window during which it looks
     * for one of this person's own devices (`specs/multi-device-v1.md` §10
     * step 5). [OwnDeviceSearchWindow] keeps it; core defines the window.
     *
     * A sibling shares this person's user id, so it has no contact row and
     * cannot appear in [unlinkedCapableContacts] however hard it looks for us.
     * Without a motive of its own a phone whose only missing peer is its other
     * phone never sweeps, and mDNS is left as the sole channel between them --
     * which is precisely the state the field capture caught. Bounded, because
     * a second phone that is switched off or left at home is missing forever:
     * an unbounded motive would sweep the subnet every five minutes on every
     * Wi-Fi this person joins, on battery, for the life of every join.
     */
    private val ownDeviceSearchLive: () -> Boolean,
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
    /**
     * A link whose Noise static key is this identity's own agreement key
     * (`specs/multi-device-v1.md` §10 step 5). Separate from [onAuthenticated]
     * because it is deliberately *not* a peer: it has this person's user id,
     * and a route to this person leads straight back to this phone.
     */
    private val onOwnDeviceAuthenticated: (address: String) -> Unit,
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
    //
    // A device of this person's own goes in here too. It is not a contact and
    // earns no sweep credit, but the key it was dialed at is held for the life
    // of the link exactly like a contact's, and the automatic-scan gate reads
    // "outbound keys not yet authenticated" as attempts still in flight.
    // Leaving it out left that count permanently at one on whichever phone
    // dialed its sibling, which shut the automatic subnet sweep off for the
    // life of the link -- so a family member joining that Wi-Fi afterwards was
    // never swept for.
    private val authenticatedOutboundKeys = ConcurrentHashMap.newKeySet<String>()

    // Live links admitted as a device of this person's own
    // (specs/multi-device-v1.md §10 step 5), and what each one's standing is.
    // Capped at one per standing -- see [ownDeviceLinkDecision].
    //
    // Such a link is deliberately never filed under a user id, so the
    // duplicate-link test that bounds a contact to a single link cannot see it
    // -- and the device most likely to open several is the one that was just
    // removed, which still holds this person's agreement key (§10.1 rotates
    // the inbox key, never the LAN Noise static). Uncapped it could hold every
    // one of MAX_CONNECTIONS and keep real contacts off this Wi-Fi
    // indefinitely: the "block" leg of §10's threat model, which refusing the
    // handshake outright used to close.
    private val ownDeviceLinks = ConcurrentHashMap<String, OwnDeviceLinkStanding>()

    // Connection keys whose address has completed a Noise handshake at least
    // once on this network join. Only such a key's reconnect target survives
    // an unbounded run of failures; see [retireExhaustedReconnectTarget].
    private val provenReconnectKeys = ConcurrentHashMap.newKeySet<String>()

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

    // Cache for ownLanHostAddresses; see that method.
    @Volatile
    private var ownHostAddresses: Set<String>? = null

    @Volatile
    private var ownHostAddressesAtMs = 0L

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
                // Own-device links are subtracted deliberately: one is not a
                // friend on this Wi-Fi, and being route-less it also sat
                // outside checkLanHealth until this change, so a half-open one
                // could read as company for the whole Wi-Fi join.
                peerLinks = connections.size - ownDeviceLinks.size,
                pendingOutboundAttempts = pendingLanOutboundAttempts(
                    outboundServiceKeys,
                    authenticatedOutboundKeys,
                ),
                scanRemaining = runningSweep?.outcomes?.remainingCandidates() ?: 0,
                unlinkedCapableContacts = unlinkedCapableContacts(),
                ownDeviceSearchLive = ownDeviceSearchLive(),
            )
        ) {
            scanPlanner.takeDueScan(System.currentTimeMillis())?.let { breadth ->
                Log.i(TAG, "Starting automatic local Wi-Fi fallback search (${breadth.name})")
                startSubnetScan(breadth, automatic = true)
            }
        }
        scheduleAutomaticSubnetScan(AUTO_SCAN_RETRY_INTERVAL_MS)
    }

    // Set when [openListener] could not take the default TCP port. Main
    // handler only, alongside the rest of the per-network session state.
    private var listeningOnFallbackPort = false

    private val defaultPortRebindRunnable = Runnable {
        if (!started || wifiNetwork == null || !listeningOnFallbackPort) return@Runnable
        probeDefaultLanPort()
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

        override fun onLinkPropertiesChanged(network: Network, linkProperties: LinkProperties) {
            // The session's endpoint hint is computed once, at the moment the
            // network became available -- which on a join whose DHCPv4 lease
            // has not landed yet is before this phone has any address another
            // phone could dial. [localEndpoint] refuses to publish an
            // unreachable one, so without this the phone would advertise
            // nothing for the whole join and never repair it. Addresses
            // arriving later is exactly what this callback reports.
            mainHandler.post {
                if (!started || wifiNetwork != network) return@post
                republishLocalEndpoint(network)
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
        } catch (error: RuntimeException) {
            Log.w(TAG, "Unable to monitor Wi-Fi for LAN transport", error)
        }
    }

    fun stop() {
        check(Looper.myLooper() == Looper.getMainLooper())
        if (!started) return
        goOffTheLan()
        // Terminal, and the one thing [stopForLinkSilence] must not do: these
        // pools are constructed once with this transport, so shutting them down
        // is what makes this object unusable rather than merely idle.
        acceptExecutor.shutdownNow()
        connectionExecutor.shutdownNow()
        scanExecutor.shutdownNow()
        writeExecutor.shutdownNow()
    }

    /**
     * Take this phone off the LAN for §9.4's pre-activation window, without
     * ending the transport (`specs/multi-device-v1.md` §9.4).
     *
     * Everything [stop] does except the executor shutdown: the NSD service
     * registration is unregistered, discovery stops, the accept socket closes,
     * every live link is torn down, and the Wi-Fi callback is dropped. What is
     * left is an object [start] can bring back, which is the whole difference --
     * the window ends, and this phone has to be able to rejoin the mesh it was
     * just adopted into.
     *
     * Core cannot do any of this. It has never heard of an NSD registration,
     * and a phone still publishing `_cruisemesh._tcp` and answering handshakes
     * is not invisible whatever its store refuses to do.
     */
    fun stopForLinkSilence() {
        check(Looper.myLooper() == Looper.getMainLooper())
        if (!started) return
        goOffTheLan()
    }

    private fun goOffTheLan() {
        started = false
        LanTransportDiagnostics.unregisterManualConnector()
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

    /**
     * The Noise static key the peer on [address] proved it holds. Present for
     * every live LAN link, because the handshake completes before the address
     * exists at all. Null for an address this transport does not own, or one
     * whose link has already been torn down.
     */
    fun remoteStaticKeyFor(address: String): ByteArray? =
        connections[address]?.remoteStaticKey?.copyOf()

    /**
     * Which of this person's own devices the peer on [address] proved it is
     * (`specs/multi-device-v1.md` §10 step 5), or null if this link is a
     * contact's, a clone's, or gone.
     *
     * This is the handle §10 step 5's roster notice is gated on: a link that
     * answers here has produced a signature over this session's Noise
     * transcript with a device signing key the roster names.
     */
    fun ownDeviceIdFor(address: String): ByteArray? =
        connections[address]?.ownDeviceId?.copyOf()

    fun startSubnetScan(
        breadth: LanScanBreadth,
        automatic: Boolean,
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
            if (lanHintMayBeCached(localHost = localHost, candidateHost = endpoint.host)) {
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
        listeningOnFallbackPort = listener.localPort != lanDefaultTcpPort().toInt()
        acceptExecutor.execute { acceptLoop(listener) }

        advertiseListener(network, listener.localPort)

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
        if (listeningOnFallbackPort) scheduleDefaultPortRebind()
    }

    /** Publishes this listener over mDNS. Also used when the port moves. */
    private fun advertiseListener(network: Network, port: Int) {
        // The opaque token is also the cross-platform connection-election
        // value. Publishing it as the service name lets Apple Bonjour choose
        // the same single initiator without exposing an identity.
        requestedServiceName = instanceToken
        registeredServiceName = requestedServiceName
        val serviceInfo = NsdServiceInfo().apply {
            serviceName = requestedServiceName
            serviceType = lanServiceType()
            this.port = port
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
    }

    /**
     * Re-publish this phone's own address when it has changed -- including from
     * "nothing publishable" to a real one.
     *
     * [localEndpoint] refuses to advertise an address no other phone could dial,
     * and on a Wi-Fi join whose DHCPv4 lease has not landed there is no such
     * address yet. The session's hint was computed once, so refusing would have
     * meant publishing nothing for the whole join; this is the repair, driven by
     * the link-properties callback that reports the address arriving.
     */
    private fun republishLocalEndpoint(network: Network) {
        val port = serverSocket?.localPort ?: return
        val endpoint = localEndpoint(network, port) ?: return
        val existing = endpointHint
        if (existing != null && existing.host == endpoint.host && existing.port.toInt() == endpoint.port) {
            return
        }
        currentNetworkId = lanNetworkId(connectivityManager, network)
        val hint = Frame.LanEndpoint(
            instanceToken = instanceTokenBytes.copyOf(),
            host = endpoint.host,
            port = endpoint.port.toUShort(),
        )
        endpointHint = hint
        ownHostAddresses = null
        ownHostAddressesAtMs = 0L
        LanTransportDiagnostics.listening(endpoint.display)
        Log.i(TAG, "This phone's local Wi-Fi address is now ${endpoint.display}")
        onNetworkReady(hint, currentNetworkId)
    }

    /**
     * A listener that had to settle for a fallback port is not merely untidy:
     * a subnet sweep probes [lanDefaultTcpPort] and nothing else, so this
     * phone is invisible to every other phone's fallback search for as long as
     * it stays there -- mDNS becomes the only channel that can find it, and
     * the field capture is what one stale mDNS record does to that.
     *
     * The port is usually taken by this app's own previous process during a
     * restart, so it frees itself within seconds. The probe is a bind -- a
     * blocking syscall -- so it runs off the main looper, and the move itself
     * ([moveListenerToDefaultPort]) replaces only the listener: every
     * established link survives it.
     */
    private fun scheduleDefaultPortRebind() {
        mainHandler.removeCallbacks(defaultPortRebindRunnable)
        mainHandler.postDelayed(defaultPortRebindRunnable, DEFAULT_PORT_REBIND_RETRY_MS)
    }

    private fun probeDefaultLanPort() {
        // Not [acceptExecutor]: that single thread is inside a blocking
        // accept() for the life of the listener, so anything queued behind it
        // would never run.
        try {
            connectionExecutor.execute {
                val free = try {
                    ServerSocket().use { probe ->
                        probe.reuseAddress = true
                        probe.bind(InetSocketAddress(lanDefaultTcpPort().toInt()))
                    }
                    true
                } catch (_: IOException) {
                    false
                }
                mainHandler.post {
                    if (!started || wifiNetwork == null || !listeningOnFallbackPort) return@post
                    if (free) moveListenerToDefaultPort() else scheduleDefaultPortRebind()
                }
            }
        } catch (_: RejectedExecutionException) {
            // Shutting down with the service; nothing left to move.
        }
    }

    /**
     * Move the listener back to the default port without disturbing anything
     * else about the session.
     *
     * Deliberately not a [restartNetworkSession]: that tears down every live
     * LAN socket, forgets the reconnect targets and the proven-address set, and
     * re-runs discovery. Losing a family member's established link is far too
     * much to pay for a tidier port, and on a port that a *different* app holds
     * this would repeat every retry period.
     */
    private fun moveListenerToDefaultPort() {
        val network = wifiNetwork ?: return
        val previous = serverSocket
        val listener = openListener() ?: run {
            scheduleDefaultPortRebind()
            return
        }
        if (listener.localPort != lanDefaultTcpPort().toInt()) {
            // Something took the port between the probe and here. Moving to a
            // *different* fallback port buys nothing, so keep the one that is
            // already serving and try again later.
            listener.closeQuietly()
            scheduleDefaultPortRebind()
            return
        }
        serverSocket = listener
        listeningOnFallbackPort = false
        // Closing the old socket ends its accept loop; links it already
        // produced are untouched.
        previous?.closeQuietly()
        acceptExecutor.execute { acceptLoop(listener) }
        registrationListener?.let(::unregisterService)
        registrationListener = null
        advertiseListener(network, listener.localPort)
        endpointHint = endpointHint?.let { hint ->
            Frame.LanEndpoint(
                instanceToken = hint.instanceToken,
                host = hint.host,
                port = listener.localPort.toUShort(),
            )
        }
        endpointHint?.let { onNetworkReady(it, currentNetworkId) }
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
        // Filtered here, on every dial, against every address this phone
        // currently holds -- not once at discovery time against the single
        // advertised address. A remembered target is replayed for as long as
        // the phone stays on this network, so an address that was not
        // recognisable as this phone's own when it was first seen (a restart
        // racing a Wi-Fi join, a second interface, IPv6) has to be re-checked
        // before every attempt. Nothing left to dial retires the target, so
        // the retry timer stops instead of looping forever.
        val remoteEndpoints =
            orderedLanDialCandidates(remoteLanEndpoints(ownLanHostAddresses(), endpoints))
        if (remoteEndpoints.isEmpty()) {
            reconnectTargets.remove(key)
            Log.i(TAG, "Ignoring LAN endpoint that resolves to this phone")
            return
        }
        // A hinted address is one the contact told us about, not one this
        // phone discovered for itself, so it gets a single attempt: no
        // reconnect target means every scheduleReconnect for the key finds
        // nothing to retry. A later hint (or discovery, or the cached
        // endpoint) can try again -- the attempt just never reschedules
        // itself. Completing Noise is what promotes such an address from
        // "claimed" to "proven": runConnection remembers it then, so a link
        // that really worked still comes back on the reconnect timer.
        if (!isSingleShotLanConnectKey(key)) {
            rememberReconnectTarget(key, remoteEndpoints, expectedUserId)
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
                var lastFailedEndpoint: String? = null
                for (endpoint in remoteEndpoints) {
                    val display = endpointDisplay(endpoint)
                    LanTransportDiagnostics.connecting(display)
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
                        lastFailedEndpoint = display
                        // Per endpoint, because a peer that publishes several
                        // addresses fails them for different reasons, and only
                        // one of them is the one worth reading.
                        Log.d(TAG, "LAN connect to $display failed: ${error.message}")
                        socket?.closeQuietly()
                        socket = null
                    }
                }
                if (socket == null) {
                    // The endpoint that produced [lastError], not the first one
                    // in the list. Pairing the first address with the last
                    // exception is what made a 2026-08-24 field log read
                    // "resolved 192.168.86.37 / failed fe80::... ECONNREFUSED"
                    // and sent a whole investigation after an address-family
                    // bug that was not there.
                    val endpoint = lastFailedEndpoint
                        ?: remoteEndpoints.firstOrNull()?.let(::endpointDisplay)
                        ?: key
                    Log.d(TAG, "LAN peer connection attempt to $endpoint failed", lastError)
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
                    retireExhaustedReconnectTarget(key)
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

    /**
     * A link that is nobody's contact: either a device of this person's own,
     * which is kept, or a stranger, which is refused.
     *
     * Before §10 step 5 both ended the same way -- "not an accepted contact",
     * socket closed -- and that is exactly what left a removed phone sitting on
     * the same Wi-Fi as the phone that removed it with no way to be told.
     *
     * # Why the agreement key cannot be the test
     *
     * The first build asked one question: is the peer's Noise static this
     * identity's own agreement key? That admits a *clone* -- a `.cmbak` restore
     * running this person's identity -- and it admits nothing else, because §9's
     * link ceremony deliberately withholds the person root secret and gives a
     * new device keys of its own. Two genuine siblings therefore share no
     * private key at all, and the test refused both of them, in both roles. A
     * 2026-08-24 two-phone capture is the record of it: 25 refusals across 15
     * minutes on one /24, an own-device link that never came up once, and a
     * removed phone that never learned.
     *
     * # What replaces it
     *
     * The roster, which is the document that actually says who is whose. Each
     * side signs this session's Noise transcript hash with its device signing
     * key ([uniffi.cruisemesh_core.coreOwnDeviceLanProof]) and checks the
     * other's against the roster it holds
     * ([uniffi.cruisemesh_core.coreOwnDeviceLanProofOpen]). Bound to the
     * transcript, so a recorded proof is worthless on the next session and no
     * machine in the middle can forward one.
     *
     * Order matters and is not symmetric: the initiator proves first, and a
     * responder that cannot verify it closes without answering. Each side signs
     * *which end it is speaking from* as well as the transcript, so the proof
     * this phone sends does not open as the answer it expects back -- see
     * [uniffi.cruisemesh_core.CoreLanProofRole]. Without that a host we dialed
     * could simply re-encrypt our own proof and return it, and we would read our
     * own device id out of our own roster and admit a stranger as a sibling.
     *
     * Nothing is minted at all unless this person's roster names a device
     * besides this one ([ownDeviceLanProof] returns null otherwise). A solo
     * phone has no sibling to recognise and none that could recognise it, so a
     * proof from it could only ever come back reflected; refusing to sign one
     * keeps a stable device identifier off the wire on every install that has
     * no use for the feature.
     *
     * Both halves of the roster count, live devices and tombstones alike. A
     * removed device MUST still be admitted or the notice that exists to tell
     * it can never arrive; it gains nothing by it (no user id, so no route, no
     * contact bookkeeping, no counters -- and §10.1 rotated the inbox key at the
     * moment of removal, long before this meeting).
     *
     * Returns what the roster proof named, or null for the clone case, which is
     * kept exactly as it was so the clone warning still has a link to be raised
     * on.
     */
    private fun acceptOwnDeviceOrRefuse(
        session: LanNoiseSession,
        socket: Socket,
        input: DataInputStream,
        output: DataOutputStream,
        remoteStaticKey: ByteArray,
        initiator: Boolean,
        role: String,
        peerEndpoint: String,
    ): CoreLanOwnDeviceProof? {
        // The clone case first, and unchanged. Symmetric, so both ends take
        // this branch together and no proof frame is exchanged on a link where
        // one would never arrive.
        if (ownLanStaticKeyMatches(identity.agreePk, remoteStaticKey)) return null

        // S3: the address is in the message, so a field log attributes each
        // refusal instead of leaving the sibling and the neighbour's phone
        // indistinguishable.
        fun refuse(): Nothing = throw IOException("$role is not an accepted contact ($peerEndpoint)")

        val handshakeHash = session.handshakeHash() ?: refuse()
        return exchangeOwnDeviceProof(
            initiator = initiator,
            channel = SocketOwnDeviceProofChannel(session, socket, input, output),
            mint = { proofRole -> ownDeviceLanProof(handshakeHash, proofRole) },
            open = { payload, peerRole ->
                openOwnDeviceLanProof(handshakeHash, payload, peerRole)
            },
        ) ?: refuse()
    }

    /**
     * [exchangeOwnDeviceProof]'s two moves, over this link's socket.
     *
     * The read is bounded twice over, because it is the one read a *stranger*
     * can make this phone wait on: a proof is a hundred-odd bytes and always
     * arrives as one record, so a peer still feeding partial records is
     * stalling rather than proving. [MAX_OWN_DEVICE_PROOF_RECORDS] caps how
     * many records it may send, and the deadline caps the whole exchange in
     * wall clock -- each read is given only what is *left* of the budget rather
     * than a fresh copy of it, so a peer that dribbles records cannot collect
     * one socket timeout per record and hold a connection slot for a multiple
     * of the handshake budget.
     */
    private inner class SocketOwnDeviceProofChannel(
        private val session: LanNoiseSession,
        private val socket: Socket,
        private val input: DataInputStream,
        private val output: DataOutputStream,
    ) : OwnDeviceProofChannel {
        private val deadlineMs = SystemClock.elapsedRealtime() + HANDSHAKE_TIMEOUT_MS

        override fun send(proof: ByteArray) {
            for (record in session.encryptFrame(proof)) writePacket(output, record)
        }

        override fun receive(): ByteArray? {
            repeat(MAX_OWN_DEVICE_PROOF_RECORDS) {
                val remainingMs = deadlineMs - SystemClock.elapsedRealtime()
                if (remainingMs <= 0) return null
                // Never 0: on a Socket that means "block forever".
                socket.soTimeout = remainingMs.coerceIn(1L, HANDSHAKE_TIMEOUT_MS.toLong()).toInt()
                session.decryptRecord(readPacket(input))?.let { return it }
            }
            return null
        }
    }

    /**
     * File [address] among the live own-device links, closing the ones it
     * replaces -- or refuse it, when the link it would replace is the one both
     * phones agreed to keep.
     *
     * A contact is bounded to a single link by [authenticatedUserIds]; a device
     * of this person's own has no user id, so nothing bounded it at all. That
     * matters because the device most motivated to open several is the one that
     * was just removed: §10.1 rotates the inbox key, not the LAN Noise static,
     * so a removed phone -- or a `.cmbak` clone -- still presents the key that
     * gets admitted here, and every accepted socket holds one of
     * [MAX_CONNECTIONS]. Filling the table is the "block" leg of §10's threat
     * model, and it is the one refusing the handshake used to close for free.
     *
     * Returns false when the incoming link must be dropped -- see
     * [ownDeviceLinkDecision] for the two rules and why plain newest-wins is not
     * enough now that this arm admits anybody.
     */
    private fun supersedeOtherOwnDeviceLinks(
        address: String,
        standing: OwnDeviceLinkStanding,
    ): Boolean {
        val superseded = synchronized(ownDeviceLinks) {
            when (val decision = ownDeviceLinkDecision(ownDeviceLinks, address, standing)) {
                OwnDeviceLinkDecision.Refuse -> return false
                is OwnDeviceLinkDecision.Admit -> {
                    decision.superseded.forEach(ownDeviceLinks::remove)
                    ownDeviceLinks[address] = standing
                    decision.superseded
                }
            }
        }
        superseded.forEach { older ->
            connections[older]?.let {
                Log.i(TAG, "Closing an older link to another device of ours")
                it.close()
            }
        }
        return true
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
        var abortedSelfConnection = false
        /** What the far end's §10 step 5 roster proof named, if it made one. */
        var provenOwnDevice: CoreLanOwnDeviceProof? = null
        try {
            // Last line of defence against dialing this phone's own listener.
            // Every earlier gate compares advertised addresses; this one asks
            // the socket where it actually landed, against every address this
            // device holds. Without it a self-dial completes Noise with this
            // identity's own key, which the identity-clone check can only read
            // as a stranger holding our key -- a durable warning the user
            // dismisses and immediately sees again on the next retry.
            //
            // Genuine clone detection is untouched: this only fires for a
            // remote address that is demonstrably one of ours.
            if (isSelfConnection(socket)) {
                abortedSelfConnection = true
                Log.i(TAG, SELF_CONNECTION_LOG)
                throw IOException(SELF_CONNECTION_LOG)
            }
            socket.tcpNoDelay = true
            socket.keepAlive = true
            socket.soTimeout = HANDSHAKE_TIMEOUT_MS
            val input = DataInputStream(socket.getInputStream())
            val output = DataOutputStream(socket.getOutputStream())
            val session = LanNoiseSession(initiator, identity.agreeSk)
            noise = session

            val trustedUserId = if (initiator) run {
                writePacket(output, session.writeHandshakeMessage())
                session.readHandshakeMessage(readPacket(input))
                val remoteStatic = session.remoteStaticKey()
                    ?: throw IOException("LAN responder did not provide a static key")
                val userId = trustedPeerForStaticKey(remoteStatic)
                if (userId == null) {
                    // Nobody's contact. The own-device proof is bound to this
                    // session's transcript hash, which is not final until
                    // message 3 has gone out -- so unlike the contact arm, the
                    // handshake finishes before the admission question is
                    // answered.
                    //
                    // That is a real disclosure, not a free one: message 3
                    // carries this identity's Noise static and the proof
                    // behind it carries this device's signing key, both to a
                    // host that has proved nothing. Two things bound it. Only
                    // a phone whose roster names a second device gets this far
                    // at all -- [ownDeviceLanProof] returns null otherwise --
                    // so a solo install still sweeps a whole subnet without
                    // saying who it is. And what the host does learn it cannot
                    // use: the proof it receives was minted for the dialing
                    // end and does not open as the answer.
                    writePacket(output, session.writeHandshakeMessage())
                    provenOwnDevice = acceptOwnDeviceOrRefuse(
                        session = session,
                        socket = socket,
                        input = input,
                        output = output,
                        remoteStaticKey = remoteStatic,
                        initiator = true,
                        role = "LAN responder",
                        peerEndpoint = peerEndpoint,
                    )
                    onOwnIdentityPeer(remoteStatic, provenOwnDevice?.deviceId)
                    return@run null
                }
                if (expectedUserId != null && !userId.contentEquals(expectedUserId)) {
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
                val userId = trustedPeerForStaticKey(remoteStatic)
                if (userId == null) {
                    provenOwnDevice = acceptOwnDeviceOrRefuse(
                        session = session,
                        socket = socket,
                        input = input,
                        output = output,
                        remoteStaticKey = remoteStatic,
                        initiator = false,
                        role = "LAN initiator",
                        peerEndpoint = peerEndpoint,
                    )
                    onOwnIdentityPeer(remoteStatic, provenOwnDevice?.deviceId)
                }
                userId
            }
            if (!session.isHandshakeFinished()) {
                throw IOException("LAN Noise handshake did not finish")
            }

            socket.soTimeout = 0
            // Named but not yet adopted: a link the cross-connect rule turns
            // away must leave no trace behind it, so [address] -- which the
            // teardown reads as "this link was announced" -- is assigned only
            // once the link is certainly staying.
            val candidate = "lan:${UUID.randomUUID()}"
            if (trustedUserId == null) {
                val standing = OwnDeviceLinkStanding(
                    revoked = provenOwnDevice?.revoked == true,
                    prevails = ownDeviceLinkPrevails(
                        ownAgreePk = identity.agreePk,
                        remoteStaticKey = session.remoteStaticKey() ?: ByteArray(0),
                        initiator = initiator,
                    ),
                )
                if (!supersedeOtherOwnDeviceLinks(candidate, standing)) {
                    // The other socket of a simultaneous cross-connect, and the
                    // one both phones agreed to drop. Not a failure -- see
                    // [ownDeviceLinkDecision] -- so it is filed exactly as the
                    // contact arm files its own redundant socket.
                    abortedDuplicateLink = true
                    throw IOException("Another device of ours already has an active LAN link")
                }
            }
            address = candidate
            connection = LanConnection(candidate, socket, output, session, provenOwnDevice?.deviceId)
            connections[candidate] = connection
            // A device of this person's own is not a peer and is not filed as
            // one: no user id, so no route, no entry in the counters that say
            // how many friends are on this Wi-Fi, and nothing keyed to a
            // contact. It is a link that exists to carry §10 step 5's device
            // list and the HELLOs that precede it.
            if (trustedUserId != null) {
                authenticatedUserIds[address] = trustedUserId.toHex()
                outboundServiceKey?.let { key ->
                    if (isSingleShotLanConnectKey(key) && advertisedEndpoint != null) {
                        // A hinted or cached address is dialed once precisely
                        // because nothing proved it was real. Finishing Noise is
                        // that proof, so it earns a reconnect target now: a link
                        // the AP or Doze kills comes back on the backoff timer
                        // instead of waiting for the next Wi-Fi join. If the
                        // retry then fails, connectToEndpoints retires the
                        // target again and the address is back to single-shot.
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
            }
            val authenticatedEndpoint = advertisedEndpoint?.let {
                LanManualEndpoint(
                    socket.inetAddress?.hostAddress ?: it.hostString,
                    it.port,
                )
            }
            if (trustedUserId != null) {
                onAuthenticated(
                    address,
                    trustedUserId,
                    authenticatedEndpoint,
                    currentNetworkId,
                )
            } else {
                onOwnDeviceAuthenticated(address)
            }
            outboundServiceKey?.let {
                connectionBackoff.recordSuccess(it)
                // Proof this address is real, which is what buys its reconnect
                // target the right to be retried without a ceiling.
                provenReconnectKeys += it
            }
            authenticated = true
            if (outboundServiceKey != null) {
                // Every key that reached a finished handshake, contact or own
                // device alike: the automatic-scan gate reads the outbound keys
                // NOT in here as attempts still in flight, and an own-device
                // link holds its key for the whole life of the link. Leaving it
                // out left that count stuck at one and shut the sweep off for
                // as long as the two phones stayed linked.
                authenticatedOutboundKeys += outboundServiceKey
                if (outboundServiceKey.startsWith("scan:")) {
                    // A completed handshake -- a friend, or one of this
                    // person's own devices -- is what counts as a sweep find;
                    // a bare TCP connect to some unrelated service still does
                    // not (see onScanCompleted). A sibling answering proves
                    // discovery works on this network exactly as a contact
                    // does, so it must not leave the expensive full tier
                    // armed. Harmless no-op if the sweep has already completed
                    // or been replaced.
                    markSweepFoundFriend(sweep)
                }
            }
            scheduleAutomaticSubnetScan(AUTO_SCAN_RETRY_INTERVAL_MS)
            Log.i(
                TAG,
                if (trustedUserId != null) {
                    "Authenticated CruiseMesh peer over local Wi-Fi"
                } else {
                    // Named, because until this line landed the only positive
                    // signal an own-device link had ever produced in a field
                    // log was "Closing an older link", which needs two of
                    // them. What appears here is a device id, derived from a
                    // public key, and the socket address this transport
                    // already logs on every dial and every refusal -- no
                    // secret and no user id.
                    "Another device of ours is on this Wi-Fi at $peerEndpoint" +
                        (provenOwnDevice?.let { " (device ${it.deviceId.toHex()})" }
                            ?: " (our own key)")
                },
            )

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
            if (!abortedSelfConnection) Log.d(TAG, "LAN connection closed: ${error.message}")
            if (!authenticated && !abortedSelfConnection) {
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
                ownDeviceLinks.remove(it)
                onDisconnected(it)
            }
            sockets.remove(socket)
            socket.closeQuietly()
            releaseSocketSlot()
            outboundServiceKey?.let {
                outboundServiceKeys.remove(it)
                authenticatedOutboundKeys.remove(it)
                if (abortedSelfConnection) {
                    // The remembered address is this phone. Retire it so the
                    // retry timer stops instead of re-dialing ourselves every
                    // backoff period for as long as we stay on this Wi-Fi.
                    reconnectTargets.remove(it)
                } else if (abortedDuplicateLink) {
                    // Not a failure: the contact already has a live LAN
                    // link. No backoff, no reconnect -- rediscovery covers
                    // a later drop of the surviving link.
                    reconnectTargets.remove(it)
                } else if (shouldRetainLanReconnectTarget(it, authenticated)) {
                    connectionBackoff.recordFailure(it, System.currentTimeMillis())
                    retireExhaustedReconnectTarget(it)
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
                !abortedSelfConnection &&
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

    /**
     * Drop a reconnect target whose address has failed often enough, and has
     * never once answered, that retrying it is only costing battery.
     *
     * The rule is core's ([coreLanReconnectTargetIsExhausted]); this supplies
     * the two facts. Nothing here can strand a working link: a key that
     * completed a handshake is in [provenReconnectKeys] and keeps its target
     * forever, and an unproven address that is genuinely there is re-created
     * as a target by the next mDNS resolution or sweep hit.
     */
    private fun retireExhaustedReconnectTarget(serviceKey: String) {
        if (
            !coreLanReconnectTargetIsExhausted(
                everAuthenticated = serviceKey in provenReconnectKeys,
                consecutiveFailures = connectionBackoff.failureCount(serviceKey)
                    .coerceAtLeast(0)
                    .toUInt(),
            )
        ) {
            return
        }
        if (reconnectTargets.remove(serviceKey) != null) {
            Log.i(TAG, "Giving up on a local Wi-Fi address that never answered")
        }
    }

    private fun rememberReconnectTarget(
        serviceKey: String,
        endpoints: List<InetSocketAddress>,
        expectedUserId: ByteArray?,
    ) {
        // Filed already filtered, so a target can never be created holding
        // only this phone's own address. connectToEndpoints re-filters before
        // every dial regardless -- this phone's addresses can change while a
        // target sits remembered.
        val remembered = remoteLanEndpoints(ownLanHostAddresses(), endpoints)
        if (remembered.isEmpty()) {
            reconnectTargets.remove(serviceKey)
            return
        }
        reconnectTargets[serviceKey] = ReconnectTarget(
            endpoints = remembered,
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
        mainHandler.removeCallbacks(defaultPortRebindRunnable)
        listeningOnFallbackPort = false
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
        provenReconnectKeys.clear()
        reconnectTargets.clear()
        outboundServiceKeys.clear()
        serverSocket?.closeQuietly()
        serverSocket = null
        sockets.toList().forEach(Socket::closeQuietly)
        connections.clear()
        authenticatedUserIds.clear()
        ownDeviceLinks.clear()
        endpointHint = null
        currentNetworkId = null
        ownHostAddresses = null
        ownHostAddressesAtMs = 0L
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

    /**
     * Every host address this phone currently answers on: the address it
     * advertises plus every address on every live interface (v4 and v6).
     *
     * The advertised address alone is not enough. The mDNS instance token is
     * new for every process, so after a restart this phone's own stale
     * advertisement looks like a foreign peer; if it names an address the
     * transport is not currently tracking -- a second interface, an IPv6
     * address, or simply anything at all in the seconds after a Wi-Fi join
     * before the advertised address is known -- the phone dials itself,
     * handshakes with its own key, and records a durable identity-clone
     * warning every retry.
     *
     * Enumerating interfaces costs a handful of syscalls, so the result is
     * cached briefly; addresses do not change faster than that, and a connect
     * during the stale window is still caught by the pre-handshake check on
     * the socket's actual remote address.
     */
    private fun ownLanHostAddresses(): Set<String> {
        val now = System.currentTimeMillis()
        val cached = ownHostAddresses
        if (cached != null && now - ownHostAddressesAtMs < OWN_ADDRESS_CACHE_MS) return cached
        val hosts = linkedSetOf<String>()
        endpointHint?.host?.let(hosts::add)
        try {
            for (networkInterface in NetworkInterface.getNetworkInterfaces()?.toList().orEmpty()) {
                for (address in networkInterface.inetAddresses.toList()) {
                    if (address.isAnyLocalAddress) continue
                    address.hostAddress?.let(hosts::add)
                }
            }
        } catch (_: SocketException) {
            // Interface enumeration is unavailable; the advertised address
            // (and the pre-handshake socket check) still apply.
        }
        ownHostAddresses = hosts
        ownHostAddressesAtMs = now
        return hosts
    }

    /** True when [socket] is connected to this very device. */
    private fun isSelfConnection(socket: Socket): Boolean {
        val remoteHost = socket.inetAddress?.hostAddress ?: return false
        return lanHostIsOwnDevice(ownLanHostAddresses(), remoteHost)
    }

    /**
     * The address this phone publishes as its own -- in mDNS, in the endpoint
     * hint, and as the self-address filter every dial is checked against.
     *
     * IPv4 first, then any other address another phone could actually dial.
     * The old fallback was "whatever is left", which on a Wi-Fi join that has
     * not produced an IPv4 address yet is an `fe80::/10` link-local one. That
     * is a local address but not a reachable one: it resolves only against the
     * *dialer's* scope id, so publishing it hands every peer a target that can
     * never answer. One of those was observed in the field being retried for
     * half an hour while the phone it belonged to sat on the same Wi-Fi.
     *
     * Publishing nothing is the honest alternative: discovery still runs, and
     * the next link-property change re-runs this with a real address.
     */
    private fun localEndpoint(network: Network, port: Int): LanManualEndpoint? {
        val addresses = connectivityManager.getLinkProperties(network)
            ?.linkAddresses
            ?.map { it.address }
            ?.filterNot { it.isAnyLocalAddress || it.isLoopbackAddress }
            .orEmpty()
        val address = addresses.firstOrNull { it is Inet4Address }
            ?: addresses.firstOrNull { candidate ->
                candidate.hostAddress?.let(::coreLanHostIsReachableEndpoint) == true
            }
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
        /**
         * Which of this person's devices the far end proved it is, when the
         * §10 step 5 roster proof named one. Null both for a contact link and
         * for the clone case, which proves an identity rather than a device.
         */
        val ownDeviceId: ByteArray? = null,
    ) {
        @Volatile
        private var closed = false

        /**
         * Captured while the session is still open (the handshake has just
         * finished) so it stays readable after [markClosed] disposes the
         * session.
         */
        val remoteStaticKey: ByteArray? = noise.remoteStaticKey()

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

        /**
         * The one distinct line a self-connect logs, so a future field log
         * names the condition instead of showing a clone warning and a
         * retry loop with no explanation between them.
         */
        internal const val SELF_CONNECTION_LOG =
            "Closed a connection to this phone's own listener"

        // How long ownLanHostAddresses reuses an interface enumeration.
        private const val OWN_ADDRESS_CACHE_MS = 5_000L
        private const val TXT_VERSION = "v"
        private const val TXT_INSTANCE = "i"
        private const val MAX_CONNECTIONS = 8

        // Live API 34+ service-info callbacks. Each one is a standing
        // platform registration, so the count is bounded the same way live
        // sockets are; peers past the cap still get a one-shot resolve.
        private const val MAX_SERVICE_INFO_CALLBACKS = 8
        private const val CONNECT_TIMEOUT_MS = 3_000
        private const val HANDSHAKE_TIMEOUT_MS = 5_000

        /**
         * How many Noise records the own-device proof exchange will read
         * before giving up. A proof is a hundred-odd bytes and always arrives
         * as one record; the slack is for a peer that sends an empty frame
         * first, not for one that streams.
         */
        private const val MAX_OWN_DEVICE_PROOF_RECORDS = 4
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

        // How often a listener that had to settle for a fallback port checks
        // whether the default one has come free. See [openListener]: a phone
        // on a fallback port is invisible to every other phone's subnet
        // sweep, which probes the default port and nothing else, so this is
        // not a cosmetic tidy-up -- it is the difference between being
        // findable on this Wi-Fi and being findable only through mDNS.
        private const val DEFAULT_PORT_REBIND_RETRY_MS = 60_000L
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

/**
 * Whether a HELLO that names this device's own user id may be persisted as an
 * identity clone warning. The core store documents the contract: only an
 * authenticated sighting counts. A user id in a HELLO is just a claim — the
 * proof is the Noise static key the link's peer actually holds, so the warning
 * is recorded only on a LAN link whose session key is this identity's own
 * agreement key. Anything else (a cleartext BLE HELLO, a LAN link belonging to
 * some other peer, a link already gone) is ignored.
 */
internal fun ownIdentityHelloIsAuthenticated(
    isLanLink: Boolean,
    ownAgreePk: ByteArray,
    sessionRemoteStaticKey: ByteArray?,
): Boolean =
    isLanLink &&
        sessionRemoteStaticKey != null &&
        ownLanStaticKeyMatches(ownAgreePk, sessionRemoteStaticKey)

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
 * The two moves §10 step 5's proof exchange makes on a finished Noise session:
 * seal and write one proof, and read the peer's back.
 *
 * An interface rather than the socket itself so [exchangeOwnDeviceProof] --
 * which is the whole ordering rule, and the only part of this handshake a
 * stranger can steer -- can be driven by a test over two in-process sessions.
 */
internal interface OwnDeviceProofChannel {
    /** Seal [proof] into Noise records and write them. */
    fun send(proof: ByteArray)

    /**
     * The peer's next decrypted payload, or null if it stalled, ran out of the
     * records it is allowed, or never sent one.
     */
    fun receive(): ByteArray?
}

/**
 * Prove to the far end of a finished LAN Noise session that this is one of the
 * same person's devices, and find out whether it is
 * (`specs/multi-device-v1.md` §10 step 5).
 *
 * Returns what the peer's proof named, or null for every refusal — no device
 * key of our own, a peer that sent something else, a peer that sent nothing.
 * The caller turns null into a closed socket.
 *
 * # The order is the security property
 *
 * The initiator proves first and the responder answers only once the
 * initiator's proof has verified, so a stranger who *dials us* is never handed
 * anything: it must produce a roster proof before this phone signs a byte.
 * Reversing that would put this device's signing key in front of every host on
 * the Wi-Fi that cares to open a socket.
 *
 * Dialing costs something, and it is deliberate rather than free. A host we
 * dial does learn our device signing public key. That is why [mint] is allowed
 * to refuse: it does, on every install whose roster names no second device, so
 * the disclosure is confined to phones that actually have a fleet to find. Two
 * further guards make the disclosure useless as a lever rather than merely
 * cheap — [mint] and [open] carry the *end* each proof was made for, so the
 * proof this phone just sent cannot be re-encrypted and returned as the answer
 * (`CoreLanProofRole`), and `core_own_device_lan_proof_open` refuses any proof
 * that derives to this very device.
 */
internal fun exchangeOwnDeviceProof(
    initiator: Boolean,
    channel: OwnDeviceProofChannel,
    mint: (role: CoreLanProofRole) -> ByteArray?,
    open: (payload: ByteArray, peerRole: CoreLanProofRole) -> CoreLanOwnDeviceProof?,
): CoreLanOwnDeviceProof? {
    val ourRole = if (initiator) CoreLanProofRole.INITIATOR else CoreLanProofRole.RESPONDER
    val peerRole = if (initiator) CoreLanProofRole.RESPONDER else CoreLanProofRole.INITIATOR
    // Minted before anything is read, so a phone with nothing to prove refuses
    // here rather than verifying a proof it could never answer.
    val ours = mint(ourRole) ?: return null
    if (initiator) {
        channel.send(ours)
        return open(channel.receive() ?: return null, peerRole)
    }
    val proven = open(channel.receive() ?: return null, peerRole) ?: return null
    channel.send(ours)
    return proven
}

/**
 * What a live own-device link is worth when another one turns up.
 *
 * @param revoked whether the roster this phone holds has already buried the
 *   device on the far end.
 * @param prevails whether this end of a simultaneous cross-connect is the one
 *   *both* phones keep — see [ownDeviceLinkPrevails]. Null when nothing
 *   distinguishes the two ends, which is the clone case.
 */
internal data class OwnDeviceLinkStanding(
    val revoked: Boolean,
    val prevails: Boolean?,
)

/**
 * Whether this end of a link is the one both phones will keep, when the same
 * pair happens to dial each other at once.
 *
 * Both sides must reach the same answer about the same socket from what each
 * one already knows, and no symmetric rule can: "keep the link I dialed" makes
 * each phone keep its own and close the other's, and so does "keep the link I
 * answered" — both leave the pair with two dead sockets, a rediscovery, and a
 * repeat. So the rule is asymmetric and settled by the keys themselves: **the
 * link dialed by the phone with the lower Noise static key survives.** A dials
 * B and A's key is lower, so A keeps what it dialed and B keeps what it
 * answered — one socket, agreed without a message.
 *
 * Null when the two keys are identical, which is the clone case: two installs
 * of one identity genuinely have nothing to tell them apart, and the caller
 * falls back to newest-wins there exactly as it always did.
 */
internal fun ownDeviceLinkPrevails(
    ownAgreePk: ByteArray,
    remoteStaticKey: ByteArray,
    initiator: Boolean,
): Boolean? {
    val order = compareLanKeys(ownAgreePk, remoteStaticKey)
    if (order == 0) return null
    return (order < 0) == initiator
}

/** Unsigned lexicographic order over two keys; shorter sorts first. */
private fun compareLanKeys(left: ByteArray, right: ByteArray): Int {
    for (index in 0 until minOf(left.size, right.size)) {
        val difference = (left[index].toInt() and 0xff) - (right[index].toInt() and 0xff)
        if (difference != 0) return difference
    }
    return left.size - right.size
}

/** What happens to a new own-device link, given the ones already live. */
internal sealed interface OwnDeviceLinkDecision {
    /** Filed, and these links are the ones it replaces. */
    data class Admit(val superseded: List<String>) : OwnDeviceLinkDecision

    /** Dropped: a live link already won this cross-connect on both phones. */
    data object Refuse : OwnDeviceLinkDecision
}

/**
 * Which links to a device of this person's own must close when a new one is
 * admitted (`specs/multi-device-v1.md` §10 step 5), and whether it may be
 * admitted at all.
 *
 * A cap is a safety rule, not tidiness. Such a link carries no user id, so the
 * duplicate-link test that bounds a *contact* to one link cannot see it, and a
 * removed phone still holds the agreement key that reaches this arm — §10.1
 * rotates the inbox key, never the LAN Noise static. Without a cap it could
 * hold every slot and keep the family's real contacts off this Wi-Fi.
 *
 * Newest-wins alone is not the cap, though, and this is the first build where
 * that matters: the old admission gate admitted nobody, so this code has never
 * run to completion in the field. Two rules join it.
 *
 * - **A cross-connect is settled by [OwnDeviceLinkStanding.prevails], not by
 *   arrival order.** Two phones that dial each other at the same moment finish
 *   their handshakes in opposite orders on the two hosts, so newest-wins has
 *   each of them keep a different socket and close the one the other kept:
 *   both die, both rediscover, and the pair flaps. A link that lost the
 *   tie-break therefore steps aside while the winner is live, instead of
 *   killing it.
 * - **A revoked device and a live sibling do not compete.** Each keeps its own
 *   slot. A removed phone reconnecting in a loop would otherwise take the one
 *   slot every time and close the link that carries roster convergence between
 *   the devices that remain — the thief in §10's threat model starving the very
 *   mechanism this change exists to make work. Two slots is still a cap.
 *
 * Within one standing the newest still wins, so a half-dead link can never
 * wedge the notice channel shut.
 */
internal fun ownDeviceLinkDecision(
    liveOwnDeviceLinks: Map<String, OwnDeviceLinkStanding>,
    incoming: String,
    standing: OwnDeviceLinkStanding,
): OwnDeviceLinkDecision {
    val rivals = liveOwnDeviceLinks.filterKeys { it != incoming }
        .filterValues { it.revoked == standing.revoked }
    if (standing.prevails == false && rivals.values.any { it.prevails == true }) {
        return OwnDeviceLinkDecision.Refuse
    }
    return OwnDeviceLinkDecision.Admit(rivals.keys.toList())
}


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
 * Whether the periodic check may claim a scan from [LanScanPlanner].
 *
 * The rule itself is core's ([uniffi.cruisemesh_core.coreLanScanGateOpen]) --
 * both shells owned a copy each, and the copies drifted into the multi-device
 * bug the core doc now names. This is the counting: negatives clamped to zero
 * so a miscount slows discovery down instead of disabling it.
 */
internal fun shouldRunAutomaticLanScan(
    peerLinks: Int,
    pendingOutboundAttempts: Int,
    scanRemaining: Int,
    unlinkedCapableContacts: Int,
    ownDeviceSearchLive: Boolean,
): Boolean = coreLanScanGateOpen(
    peerLinks = peerLinks.coerceAtLeast(0).toUInt(),
    unlinkedCapableContacts = unlinkedCapableContacts.coerceAtLeast(0).toUInt(),
    ownDeviceSearchLive = ownDeviceSearchLive,
    pendingOutboundAttempts = pendingOutboundAttempts.coerceAtLeast(0).toUInt(),
    scanRemaining = scanRemaining.coerceAtLeast(0).toUInt(),
)

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

internal fun lanHintMayBeCached(localHost: String?, candidateHost: String): Boolean =
    localHost != null &&
        !lanHostsAreSameAddress(leftHost = localHost, rightHost = candidateHost) &&
        lanHostsShareLocalNetwork(localHost = localHost, candidateHost = candidateHost)

/**
 * Whether [candidateHost] is an address this device answers on.
 *
 * [localHosts] is every address this phone currently holds, not just the one
 * it advertises: a phone that restarted while joining a Wi-Fi network can find
 * its own stale advertisement under a different address (or none tracked yet)
 * and dial itself, which reads as a stranger holding this identity's key.
 */
internal fun lanHostIsOwnDevice(localHosts: Collection<String>, candidateHost: String): Boolean =
    localHosts.any { lanHostsAreSameAddress(leftHost = it, rightHost = candidateHost) }

/**
 * Removes only addresses that resolve to one of this phone's own hosts.
 * A Bonjour result can contain several addresses; a stale self address must
 * not suppress a different, usable address for the peer.
 */
internal fun remoteLanEndpoints(
    localHosts: Collection<String>,
    endpoints: List<InetSocketAddress>,
): List<InetSocketAddress> {
    if (localHosts.isEmpty()) return endpoints
    return endpoints.filterNot { endpoint ->
        val candidateHost = endpoint.address?.hostAddress ?: endpoint.hostString
        lanHostIsOwnDevice(localHosts, candidateHost)
    }
}

internal fun remoteLanEndpoints(
    localHost: String?,
    endpoints: List<InetSocketAddress>,
): List<InetSocketAddress> = remoteLanEndpoints(listOfNotNull(localHost), endpoints)

/**
 * The order a peer's addresses are dialed in: IPv4 first, then everything
 * else, each group in the order the platform gave them.
 *
 * `NsdServiceInfo.hostAddresses` comes back unsorted, and a phone that
 * publishes both an IPv4 address and a Wi-Fi link-local IPv6 one can hand
 * either first. Dialing the link-local first costs a connect timeout (or an
 * `ECONNREFUSED`, which is what a 2026-08-24 field log recorded) before the
 * address that was always going to work is tried at all. Every CruiseMesh
 * listener binds the wildcard, so if a peer is reachable at all it is
 * reachable on IPv4 -- this only decides which attempt pays the latency.
 *
 * Stable within each family, so nothing else about the platform's ordering
 * changes.
 */
internal fun orderedLanDialCandidates(
    endpoints: List<InetSocketAddress>,
): List<InetSocketAddress> =
    endpoints.sortedBy { if (it.address is Inet4Address) 0 else 1 }

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
