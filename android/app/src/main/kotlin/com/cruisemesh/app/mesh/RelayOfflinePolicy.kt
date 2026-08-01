package com.cruisemesh.app.mesh

/**
 * Which relay health to publish when the internet path has gone away.
 *
 * Losing internet is only news to a phone that has somewhere to be offline
 * *from*. With no relay config of our own and none on any contact's card,
 * there is no internet delivery to have lost: nearby delivery is the whole
 * arrangement, it is working, and reporting "offline" would dress the free
 * default up as a fault -- the same mistake the pill made by reporting an
 * absent Cruise Pass as amber.
 *
 * This matters more at sea than at a desk. [RelaySyncEngine.requestRelaySync]
 * runs on every queue change, so a phone with no pass and no internet -- the
 * product's normal case on a ship -- would otherwise flip from a quiet
 * "Mesh on" to an amber "offline" on the first message the user sent, and
 * stay there.
 *
 * A phone with no pass of its own but contacts whose cards carry relays does
 * still hear about it: those configs make [anyRelayConfigKnown] true, and
 * their delivery genuinely did just stop.
 *
 * @param anyRelayConfigKnown whether the last config sweep found any relay at
 *   all, ours or a contact's. Null before the first sweep has run, where the
 *   honest answer is that we do not know yet, so nothing is quieted.
 */
fun offlineRelayHealth(anyRelayConfigKnown: Boolean?): RelayHealth =
    if (anyRelayConfigKnown == false) RelayHealth.NoConfig else RelayHealth.NoInternet
