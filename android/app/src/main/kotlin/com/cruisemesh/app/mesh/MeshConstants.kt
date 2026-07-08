package com.cruisemesh.app.mesh

import java.util.UUID

/** BLE GATT constants for the CruiseMesh mesh transport (DESIGN.md §5.2). */
object MeshConstants {
    // Placeholder UUID — regenerate a real random v4 UUID before any real deployment.
    val SERVICE_UUID: UUID = UUID.fromString("6d657368-6372-7569-7365-6d657368a001")
    val INBOUND_CHARACTERISTIC_UUID: UUID = UUID.fromString("6d657368-6372-7569-7365-6d657368a002")
    val OUTBOUND_CHARACTERISTIC_UUID: UUID = UUID.fromString("6d657368-6372-7569-7365-6d657368a003")
    val CLIENT_CONFIG_DESCRIPTOR_UUID: UUID = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb")
}
