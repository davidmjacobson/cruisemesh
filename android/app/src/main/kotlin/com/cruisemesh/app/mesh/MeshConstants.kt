package com.cruisemesh.app.mesh

import java.util.UUID
import kotlin.random.Random

/** BLE GATT constants for the CruiseMesh mesh transport (DESIGN.md §5.2). */
object MeshConstants {
    // Placeholder UUID — regenerate a real random v4 UUID before any real deployment.
    val SERVICE_UUID: UUID = UUID.fromString("6d657368-6372-7569-7365-6d657368a001")
    val INBOUND_CHARACTERISTIC_UUID: UUID = UUID.fromString("6d657368-6372-7569-7365-6d657368a002")
    val OUTBOUND_CHARACTERISTIC_UUID: UUID = UUID.fromString("6d657368-6372-7569-7365-6d657368a003")
    val CLIENT_CONFIG_DESCRIPTOR_UUID: UUID = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")

    /**
     * Random per-process token, advertised by [BlePeripheral] in its scan
     * response service data and checked by [BleCentral] against every scan
     * result. Android apps cannot reliably read their own BLE MAC address
     * (BluetoothAdapter#getAddress returns a dummy constant since API 23),
     * and that address rotates anyway under Bluetooth privacy -- so address
     * comparison can't detect "this advertisement is mine." Because a single
     * MeshService process runs both roles, a shared in-memory token lets the
     * central recognize (and skip connecting to) its own peripheral's
     * advertisement regardless of address rotation. See the self-connection
     * hypothesis discussed against the 2026-07-08 connection-churn bug.
     */
    val LOCAL_INSTANCE_ID: ByteArray = Random.nextBytes(8)
}
