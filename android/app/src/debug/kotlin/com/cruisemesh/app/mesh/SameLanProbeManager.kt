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
import java.io.IOException
import java.net.InetSocketAddress
import java.net.ServerSocket
import java.security.SecureRandom
import java.util.ArrayDeque
import java.util.UUID
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * Debug-only prototype that proves whether another CruiseMesh debug build is reachable over the
 * current Wi-Fi LAN. It exchanges only a random 20-byte challenge; it never sends identities,
 * contacts, messages, or envelopes.
 */
internal class SameLanProbeManager(context: Context) {
    private val appContext = context.applicationContext
    private val connectivityManager =
        appContext.getSystemService(Context.CONNECTIVITY_SERVICE) as ConnectivityManager
    private val nsdManager = appContext.getSystemService(Context.NSD_SERVICE) as NsdManager
    private val mainHandler = Handler(Looper.getMainLooper())
    private val ioExecutor: ExecutorService = Executors.newFixedThreadPool(3)
    private val secureRandom = SecureRandom()

    private var started = false
    private var callbackRegistered = false
    private var wifiNetwork: Network? = null
    private var sessionToken = 0L
    private var sessionActive = false
    private var sawPeer = false
    private var connectionFailed = false
    private var resolving = false
    private var connecting = false
    private var requestedServiceName: String? = null
    private var registeredServiceName: String? = null
    private var serverSocket: ServerSocket? = null
    private var discoveryListener: NsdManager.DiscoveryListener? = null
    private var registrationListener: NsdManager.RegistrationListener? = null
    private var resolveListener: NsdManager.ResolveListener? = null
    private var timeoutRunnable: Runnable? = null
    private var retryRunnable: Runnable? = null
    private val pendingServices = ArrayDeque<NsdServiceInfo>()
    private val queuedServiceNames = mutableSetOf<String>()

    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            mainHandler.post {
                if (!started || wifiNetwork == network) return@post
                wifiNetwork = network
                startSession(network)
            }
        }

        override fun onLost(network: Network) {
            mainHandler.post {
                if (wifiNetwork != network) return@post
                wifiNetwork = null
                teardownSession(invalidate = true)
                SameLanProbeStatus.publish(
                    SameLanProbeSnapshot(SameLanProbePhase.NO_WIFI, "Waiting for Wi-Fi"),
                )
            }
        }
    }

    fun start() {
        check(Looper.myLooper() == Looper.getMainLooper())
        if (started) return
        started = true
        SameLanProbeStatus.registerProbeAction { mainHandler.post(::probeAgain) }
        SameLanProbeStatus.publish(
            SameLanProbeSnapshot(SameLanProbePhase.NO_WIFI, "Waiting for Wi-Fi"),
        )

        // The app currently targets API 35. Revisit Android's local-network runtime permission
        // before raising the target to an API level where that permission is enforced.
        val request = NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
            .build()
        try {
            connectivityManager.registerNetworkCallback(request, networkCallback)
            callbackRegistered = true
        } catch (error: RuntimeException) {
            Log.w(TAG, "Unable to watch Wi-Fi for same-LAN probe", error)
            publishError("Wi-Fi status unavailable")
        }
    }

    fun stop() {
        check(Looper.myLooper() == Looper.getMainLooper())
        if (!started) return
        started = false
        SameLanProbeStatus.unregisterProbeAction()
        teardownSession(invalidate = true)
        retryRunnable?.let(mainHandler::removeCallbacks)
        retryRunnable = null
        if (callbackRegistered) {
            try {
                connectivityManager.unregisterNetworkCallback(networkCallback)
            } catch (_: IllegalArgumentException) {
                // Already removed by the platform.
            }
            callbackRegistered = false
        }
        wifiNetwork = null
        ioExecutor.shutdownNow()
    }

    private fun probeAgain() {
        if (!started) return
        retryRunnable?.let(mainHandler::removeCallbacks)
        retryRunnable = null
        val network = wifiNetwork
        if (network == null) {
            SameLanProbeStatus.publish(
                SameLanProbeSnapshot(SameLanProbePhase.NO_WIFI, "Waiting for Wi-Fi"),
            )
        } else {
            startSession(network)
        }
    }

    private fun startSession(network: Network) {
        retryRunnable?.let(mainHandler::removeCallbacks)
        retryRunnable = null
        teardownSession(invalidate = true)
        if (!started || wifiNetwork != network) return

        val token = sessionToken
        sessionActive = true
        sawPeer = false
        connectionFailed = false
        resolving = false
        connecting = false
        pendingServices.clear()
        queuedServiceNames.clear()
        requestedServiceName = UUID.randomUUID().toString()
        registeredServiceName = requestedServiceName
        SameLanProbeStatus.publish(
            SameLanProbeSnapshot(SameLanProbePhase.LOOKING, "Looking for a debug peer…"),
        )

        val server = try {
            ServerSocket(0)
        } catch (error: IOException) {
            Log.w(TAG, "Unable to open same-LAN probe listener", error)
            finishSession(token, SameLanProbePhase.ERROR, "Could not open a local listener")
            return
        }
        serverSocket = server
        ioExecutor.execute { acceptChallenges(server, token) }

        val serviceInfo = NsdServiceInfo().apply {
            serviceName = requestedServiceName
            serviceType = SERVICE_TYPE
            port = server.localPort
            setAttribute("v", "1")
        }
        val registration = registrationListener(token)
        registrationListener = registration
        try {
            nsdManager.registerService(serviceInfo, NsdManager.PROTOCOL_DNS_SD, registration)
        } catch (error: RuntimeException) {
            Log.w(TAG, "Unable to advertise same-LAN probe", error)
            finishSession(token, SameLanProbePhase.ERROR, "Could not advertise on this network")
            return
        }

        val discovery = discoveryListener(token)
        discoveryListener = discovery
        try {
            nsdManager.discoverServices(SERVICE_TYPE, NsdManager.PROTOCOL_DNS_SD, discovery)
        } catch (error: RuntimeException) {
            Log.w(TAG, "Unable to discover same-LAN peers", error)
            finishSession(token, SameLanProbePhase.ERROR, "Could not search this network")
            return
        }

        timeoutRunnable = Runnable {
            if (!isCurrent(token)) return@Runnable
            val phase = terminalProbePhase(sawPeer, connectionFailed)
            val detail = if (phase == SameLanProbePhase.BLOCKED) {
                "Peer found, but direct TCP was blocked"
            } else {
                "No debug peer found on this Wi-Fi"
            }
            finishSession(token, phase, detail)
        }.also { mainHandler.postDelayed(it, SESSION_TIMEOUT_MS) }
    }

    private fun registrationListener(token: Long) = object : NsdManager.RegistrationListener {
        override fun onServiceRegistered(serviceInfo: NsdServiceInfo) {
            val listener = this
            mainHandler.post {
                if (isCurrent(token)) {
                    registeredServiceName = serviceInfo.serviceName
                } else {
                    unregisterService(listener)
                }
            }
        }

        override fun onRegistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
            mainHandler.post {
                if (isCurrent(token)) {
                    finishSession(token, SameLanProbePhase.ERROR, "Could not advertise on this network")
                }
            }
        }

        override fun onServiceUnregistered(serviceInfo: NsdServiceInfo) = Unit
        override fun onUnregistrationFailed(serviceInfo: NsdServiceInfo, errorCode: Int) = Unit
    }

    private fun discoveryListener(token: Long) = object : NsdManager.DiscoveryListener {
        override fun onDiscoveryStarted(serviceType: String) {
            val listener = this
            mainHandler.post {
                if (!isCurrent(token)) stopDiscovery(listener)
            }
        }

        override fun onServiceFound(serviceInfo: NsdServiceInfo) {
            mainHandler.post {
                if (!isCurrent(token) || serviceInfo.serviceType != SERVICE_TYPE) return@post
                val name = serviceInfo.serviceName
                if (name == requestedServiceName || name == registeredServiceName || !queuedServiceNames.add(name)) {
                    return@post
                }
                sawPeer = true
                pendingServices.addLast(serviceInfo)
                resolveNext(token)
            }
        }

        override fun onServiceLost(serviceInfo: NsdServiceInfo) = Unit

        override fun onDiscoveryStopped(serviceType: String) = Unit

        override fun onStartDiscoveryFailed(serviceType: String, errorCode: Int) {
            mainHandler.post {
                if (isCurrent(token)) {
                    finishSession(token, SameLanProbePhase.ERROR, "Could not search this network")
                }
            }
        }

        override fun onStopDiscoveryFailed(serviceType: String, errorCode: Int) = Unit
    }

    @Suppress("DEPRECATION")
    private fun resolveNext(token: Long) {
        if (!isCurrent(token) || resolving || connecting || pendingServices.isEmpty()) return
        resolving = true
        val service = pendingServices.removeFirst()
        val listener = object : NsdManager.ResolveListener {
            override fun onResolveFailed(serviceInfo: NsdServiceInfo, errorCode: Int) {
                mainHandler.post {
                    if (!isCurrent(token)) return@post
                    resolving = false
                    connectionFailed = true
                    resolveNext(token)
                }
            }

            override fun onServiceResolved(serviceInfo: NsdServiceInfo) {
                mainHandler.post {
                    if (!isCurrent(token)) return@post
                    resolving = false
                    if (serviceInfo.serviceName == registeredServiceName) {
                        resolveNext(token)
                        return@post
                    }
                    connecting = true
                    connectAndChallenge(serviceInfo, token)
                }
            }
        }
        resolveListener = listener
        try {
            nsdManager.resolveService(service, listener)
        } catch (error: RuntimeException) {
            resolving = false
            connectionFailed = true
            Log.d(TAG, "Same-LAN peer resolution failed", error)
            resolveNext(token)
        }
    }

    @Suppress("DEPRECATION")
    private fun connectAndChallenge(serviceInfo: NsdServiceInfo, token: Long) {
        val network = wifiNetwork ?: return
        val nonce = ByteArray(SameLanProbeProtocol.NONCE_SIZE).also(secureRandom::nextBytes)
        val challenge = SameLanProbeProtocol.makeFrame(nonce)
        ioExecutor.execute {
            val succeeded = try {
                network.socketFactory.createSocket().use { socket ->
                    socket.soTimeout = SOCKET_TIMEOUT_MS
                    socket.connect(InetSocketAddress(serviceInfo.host, serviceInfo.port), SOCKET_TIMEOUT_MS)
                    socket.getOutputStream().apply {
                        write(challenge)
                        flush()
                    }
                    val response = readExactly(socket.getInputStream(), SameLanProbeProtocol.FRAME_SIZE)
                    SameLanProbeProtocol.isExpectedEcho(challenge, response)
                }
            } catch (_: IOException) {
                false
            } catch (_: RuntimeException) {
                false
            }
            mainHandler.post {
                if (!isCurrent(token)) return@post
                connecting = false
                if (succeeded) {
                    finishSession(token, SameLanProbePhase.DIRECT, "Direct TCP works on this Wi-Fi")
                } else {
                    connectionFailed = true
                    resolveNext(token)
                }
            }
        }
    }

    private fun acceptChallenges(server: ServerSocket, token: Long) {
        while (!server.isClosed) {
            try {
                server.accept().use { socket ->
                    socket.soTimeout = SOCKET_TIMEOUT_MS
                    val challenge = readExactly(socket.getInputStream(), SameLanProbeProtocol.FRAME_SIZE)
                    if (SameLanProbeProtocol.isProbeFrame(challenge)) {
                        socket.getOutputStream().apply {
                            write(challenge)
                            flush()
                        }
                        mainHandler.post {
                            if (isCurrent(token)) {
                                finishSession(token, SameLanProbePhase.DIRECT, "Direct TCP works on this Wi-Fi")
                            }
                        }
                    }
                }
            } catch (_: IOException) {
                break
            } catch (_: RuntimeException) {
                break
            }
        }
    }

    private fun finishSession(token: Long, phase: SameLanProbePhase, detail: String) {
        if (!isCurrent(token)) return
        SameLanProbeStatus.publish(
            SameLanProbeSnapshot(phase, detail, checkedAtMs = System.currentTimeMillis()),
        )
        teardownSession(invalidate = true)
        scheduleRetry(phase)
    }

    private fun publishError(detail: String) {
        SameLanProbeStatus.publish(
            SameLanProbeSnapshot(SameLanProbePhase.ERROR, detail, checkedAtMs = System.currentTimeMillis()),
        )
    }

    private fun scheduleRetry(phase: SameLanProbePhase) {
        val delayMs = retryDelayMs(phase)
        if (!started || delayMs <= 0L) return
        retryRunnable?.let(mainHandler::removeCallbacks)
        retryRunnable = Runnable { wifiNetwork?.let(::startSession) }
            .also { mainHandler.postDelayed(it, delayMs) }
    }

    private fun teardownSession(invalidate: Boolean) {
        sessionActive = false
        if (invalidate) sessionToken += 1
        timeoutRunnable?.let(mainHandler::removeCallbacks)
        timeoutRunnable = null
        resolveListener = null
        resolving = false
        connecting = false
        pendingServices.clear()
        queuedServiceNames.clear()

        discoveryListener?.let(::stopDiscovery)
        discoveryListener = null
        registrationListener?.let(::unregisterService)
        registrationListener = null
        try {
            serverSocket?.close()
        } catch (_: IOException) {
            // Closing a probe listener is best effort.
        }
        serverSocket = null
        requestedServiceName = null
        registeredServiceName = null
    }

    private fun isCurrent(token: Long): Boolean = started && sessionActive && sessionToken == token

    private fun stopDiscovery(listener: NsdManager.DiscoveryListener) {
        try {
            nsdManager.stopServiceDiscovery(listener)
        } catch (_: RuntimeException) {
            // Discovery never started or already stopped. A late onDiscoveryStarted callback
            // retries this cleanup for sessions invalidated during startup.
        }
    }

    private fun unregisterService(listener: NsdManager.RegistrationListener) {
        try {
            nsdManager.unregisterService(listener)
        } catch (_: RuntimeException) {
            // Registration never completed or already stopped. A late onServiceRegistered
            // callback retries this cleanup for sessions invalidated during startup.
        }
    }

    private fun readExactly(input: java.io.InputStream, size: Int): ByteArray {
        val result = ByteArray(size)
        var offset = 0
        while (offset < size) {
            val read = input.read(result, offset, size - offset)
            if (read < 0) throw IOException("Probe connection closed early")
            offset += read
        }
        return result
    }

    companion object {
        private const val TAG = "SameLanProbe"
        private const val SERVICE_TYPE = "_cruisemesh._tcp."
        private const val SESSION_TIMEOUT_MS = 12_000L
        private const val SOCKET_TIMEOUT_MS = 2_000
    }
}
