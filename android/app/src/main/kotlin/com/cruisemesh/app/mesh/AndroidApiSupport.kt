package com.cruisemesh.app.mesh

import android.os.Build

private const val NETWORK_SCOPED_SERVICE_INFO_EXTENSION = 3
private const val NETWORK_SCOPED_DISCOVERY_EXTENSION = 12

/**
 * `NsdServiceInfo.network` is available in Android 14 or T extension 3.
 */
internal fun supportsNetworkScopedServiceInfo(
    sdkInt: Int,
    tiramisuExtension: Int,
): Boolean =
    sdkInt >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE ||
        (
            sdkInt >= Build.VERSION_CODES.TIRAMISU &&
                tiramisuExtension >= NETWORK_SCOPED_SERVICE_INFO_EXTENSION
            )

/**
 * `NsdManager.registerServiceInfoCallback` is available in Android 14.
 *
 * It replaces the deprecated one-shot `resolveService`: the platform keeps
 * pushing service-info updates instead of answering once and leaving a failed
 * resolve to drop the peer until its record refreshes. Unlike
 * [supportsNetworkScopedServiceInfo] there is no T extension backport, so this
 * is a plain SDK-level check.
 */
internal fun supportsServiceInfoCallback(sdkInt: Int): Boolean =
    sdkInt >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE

/**
 * `DiscoveryRequest` is available in Android 15 or T extension 12.
 */
internal fun supportsNetworkScopedDiscovery(
    sdkInt: Int,
    tiramisuExtension: Int,
): Boolean =
    sdkInt >= Build.VERSION_CODES.VANILLA_ICE_CREAM ||
        (
            sdkInt >= Build.VERSION_CODES.TIRAMISU &&
                tiramisuExtension >= NETWORK_SCOPED_DISCOVERY_EXTENSION
            )
