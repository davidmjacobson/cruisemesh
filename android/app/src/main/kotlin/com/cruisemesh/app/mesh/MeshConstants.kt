package com.cruisemesh.app.mesh

import java.util.UUID
import kotlin.random.Random

/** BLE GATT constants for the CruiseMesh mesh transport (DESIGN.md §5.2). */
object MeshConstants {
    // Frozen protocol surface: this is the fixed 128-bit UUID space CruiseMesh
    // advertises/scans for over BLE, and iOS discovers/advertises the exact
    // same values (see ios/CruiseMesh/Mesh/MeshConstants.swift). Changing any
    // of these partitions the mesh -- old and new builds will not discover
    // each other. Do not edit without a coordinated fleet upgrade on both
    // platforms.
    val SERVICE_UUID: UUID = UUID.fromString("a5987315-cdcf-4e09-b036-ce10af3c05d3")
    val INBOUND_CHARACTERISTIC_UUID: UUID = UUID.fromString("a5987315-cdcf-4e09-b036-ce10af3c05d4")
    val OUTBOUND_CHARACTERISTIC_UUID: UUID = UUID.fromString("a5987315-cdcf-4e09-b036-ce10af3c05d5")
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
