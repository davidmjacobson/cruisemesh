package com.cruisemesh.app.mesh

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat

private const val TAG = "MeshService"
private const val NOTIFICATION_CHANNEL_ID = "cruisemesh_mesh"
private const val NOTIFICATION_ID = 1

/**
 * Runs both BLE GATT roles simultaneously (DESIGN.md §5.2) so this device can
 * be discovered by, and discover, any other CruiseMesh phone in range. This
 * is Milestone 0 transport plumbing: received frames are logged, not yet
 * handed to a sync engine (that lands with the Rust core's sync module,
 * DESIGN.md §7.3).
 */
class MeshService : Service() {

    private val peripheral by lazy { BlePeripheral(this, ::onFrameReceived) }
    private val central by lazy { BleCentral(this, ::onFrameReceived, ::onPeerConnected) }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startForeground(NOTIFICATION_ID, buildNotification())

        if (!hasRequiredPermissions()) {
            Log.w(TAG, "Missing BLE permissions; stopping")
            stopSelf()
            return START_NOT_STICKY
        }

        peripheral.start()
        central.start()
        return START_STICKY
    }

    override fun onDestroy() {
        peripheral.stop()
        central.stop()
        super.onDestroy()
    }

    private fun onFrameReceived(frame: ByteArray) {
        // TODO(sync engine): hand off to core for dedupe + digest exchange + receipts.
        Log.i(TAG, "Received frame (${frame.size} bytes): ${String(frame, Charsets.UTF_8)}")
    }

    // Milestone-0 proof of life: greet a newly connected peer over the wire
    // we just negotiated. Replace with the real handshake in Milestone 1.
    private fun onPeerConnected(deviceAddress: String) {
        val greeting = "hello from ${Build.MODEL}"
        Log.i(TAG, "Sending greeting to $deviceAddress: $greeting")
        central.sendFrame(deviceAddress, greeting.toByteArray(Charsets.UTF_8))
    }

    private fun hasRequiredPermissions(): Boolean {
        val permissions = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            listOf(
                Manifest.permission.BLUETOOTH_SCAN,
                Manifest.permission.BLUETOOTH_ADVERTISE,
                Manifest.permission.BLUETOOTH_CONNECT,
            )
        } else {
            emptyList()
        }
        return permissions.all {
            ContextCompat.checkSelfPermission(this, it) == PackageManager.PERMISSION_GRANTED
        }
    }

    private fun buildNotification(): Notification {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                NOTIFICATION_CHANNEL_ID,
                "CruiseMesh mesh sync",
                NotificationManager.IMPORTANCE_LOW,
            )
            getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
        }
        return NotificationCompat.Builder(this, NOTIFICATION_CHANNEL_ID)
            .setContentTitle("CruiseMesh")
            .setContentText("Relaying messages nearby")
            .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
            .setOngoing(true)
            .build()
    }

    companion object {
        /** Permissions MeshService needs before it will start its BLE roles. */
        fun requiredPermissions(): Array<String> {
            val base = mutableListOf<String>()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                base += Manifest.permission.BLUETOOTH_SCAN
                base += Manifest.permission.BLUETOOTH_ADVERTISE
                base += Manifest.permission.BLUETOOTH_CONNECT
            }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                base += Manifest.permission.POST_NOTIFICATIONS
            }
            return base.toTypedArray()
        }
    }
}
