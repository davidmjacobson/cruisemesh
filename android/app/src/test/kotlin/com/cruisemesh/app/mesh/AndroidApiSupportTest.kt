package com.cruisemesh.app.mesh

import android.os.Build
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AndroidApiSupportTest {
    @Test
    fun networkScopedServiceInfoRequiresAndroid14OrTExtension3() {
        assertFalse(
            supportsNetworkScopedServiceInfo(
                sdkInt = Build.VERSION_CODES.S_V2,
                tiramisuExtension = 99,
            ),
        )
        assertFalse(
            supportsNetworkScopedServiceInfo(
                sdkInt = Build.VERSION_CODES.TIRAMISU,
                tiramisuExtension = 2,
            ),
        )
        assertTrue(
            supportsNetworkScopedServiceInfo(
                sdkInt = Build.VERSION_CODES.TIRAMISU,
                tiramisuExtension = 3,
            ),
        )
        assertTrue(
            supportsNetworkScopedServiceInfo(
                sdkInt = Build.VERSION_CODES.UPSIDE_DOWN_CAKE,
                tiramisuExtension = 0,
            ),
        )
    }

    @Test
    fun networkScopedDiscoveryRequiresAndroid15OrTExtension12() {
        assertFalse(
            supportsNetworkScopedDiscovery(
                sdkInt = Build.VERSION_CODES.TIRAMISU,
                tiramisuExtension = 11,
            ),
        )
        assertFalse(
            supportsNetworkScopedDiscovery(
                sdkInt = Build.VERSION_CODES.UPSIDE_DOWN_CAKE,
                tiramisuExtension = 11,
            ),
        )
        assertTrue(
            supportsNetworkScopedDiscovery(
                sdkInt = Build.VERSION_CODES.TIRAMISU,
                tiramisuExtension = 12,
            ),
        )
        assertTrue(
            supportsNetworkScopedDiscovery(
                sdkInt = Build.VERSION_CODES.VANILLA_ICE_CREAM,
                tiramisuExtension = 0,
            ),
        )
    }
}
