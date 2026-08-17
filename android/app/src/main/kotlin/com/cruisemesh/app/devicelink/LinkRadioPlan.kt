package com.cruisemesh.app.devicelink

/**
 * One radio action the §9.4 gate asks the mesh service to take.
 *
 * Named rather than expressed as two branches of an `if` so the answer can be
 * asserted directly. [com.cruisemesh.app.mesh.MeshService] is a foreground
 * Android service with a BLE stack, an NSD registration and a socket accept
 * loop inside it, and none of that is reachable from a JVM unit test -- so the
 * one thing that must never quietly change, *which radios the disallow branch
 * takes down*, is decided here where a test can read it.
 *
 * The bug this exists to make impossible: the disallow branch used to call
 * `stopMeshRoles()` alone, which stops BLE. LAN went on advertising the phone
 * over NSD, accepting connections, and answering handshakes for the whole
 * pre-activation window -- so a device the spec calls "invisible on the mesh"
 * was, on the transport it is most visible on, entirely visible.
 */
internal enum class LinkRadioStep {
    /** BLE scanning, advertising, and the GATT roles. */
    START_BLE,
    STOP_BLE,

    /**
     * The LAN transport: the NSD service registration that publishes this
     * phone, the accept loop, discovery, and every live LAN link and route.
     */
    START_LAN,
    STOP_LAN,
}

internal object LinkRadioPlan {
    /**
     * What the mesh service does when the gate's answer becomes [allowed].
     *
     * Order is load-bearing on the way down: BLE first, then LAN. A phone that
     * has to go quiet should stop shouting before it stops listening, and
     * stopping LAN tears down live links whose disconnects the BLE roles would
     * otherwise still be reacting to.
     */
    fun stepsFor(allowed: Boolean): List<LinkRadioStep> = if (allowed) {
        listOf(LinkRadioStep.START_LAN, LinkRadioStep.START_BLE)
    } else {
        listOf(LinkRadioStep.STOP_BLE, LinkRadioStep.STOP_LAN)
    }
}
