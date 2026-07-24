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
