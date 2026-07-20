package com.cruisemesh.app.mesh

import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.util.Log

private const val TAG = "WifiHold"

/**
 * T15 phase 2: holds an internet-less `TRANSPORT_WIFI` [NetworkRequest] while
 * the mesh is up, which asks Android not to tear down a Wi-Fi association just
 * because it has no validated internet (ship / captive Wi-Fi). The request is
 * passive -- we never bind traffic to it; its mere existence is the signal to
 * the framework. Independent of the relay-sync INTERNET request in
 * [MeshService], with its own callback, so the two never interfere.
 *
 * [WifiHoldPolicy.shouldHold] gates whether this is active; the caller
 * ([MeshService.refreshWifiHold]) starts/stops it as VPN state and mesh
 * lifecycle change.
 */
class WifiAssociationHold(
    private val connectivityManager: ConnectivityManager,
    // T15 phase 3: the same Wi-Fi request doubles as our drop detector -- when
    // the held association actually goes away, `onWifiLost` fires so
    // [MeshService] can apply [WifiDropPolicy].
    private val onWifiLost: () -> Unit = {},
) {
    private var callback: ConnectivityManager.NetworkCallback? = null

    val isHolding: Boolean get() = callback != null

    fun start() {
        if (callback != null) return
        // Request Wi-Fi specifically, and drop the INTERNET capability the
        // builder adds by default -- an internet-less association is exactly
        // what we want to keep alive. VALIDATED is never requestable.
        val request = NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
            .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .build()
        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onLost(network: Network) {
                onWifiLost()
            }
        }
        try {
            connectivityManager.requestNetwork(request, cb)
            callback = cb
            Log.i(TAG, "Holding internet-less Wi-Fi association for nearby delivery")
        } catch (e: RuntimeException) {
            // requestNetwork throws if too many requests are outstanding; never
            // let a diagnostics-grade hold take down the service.
            Log.w(TAG, "Could not hold Wi-Fi association: ${e.message}")
        }
    }

    fun stop() {
        val cb = callback ?: return
        try {
            connectivityManager.unregisterNetworkCallback(cb)
        } catch (e: RuntimeException) {
            Log.w(TAG, "Could not release Wi-Fi association hold: ${e.message}")
        }
        callback = null
        Log.i(TAG, "Released Wi-Fi association hold")
    }
}
