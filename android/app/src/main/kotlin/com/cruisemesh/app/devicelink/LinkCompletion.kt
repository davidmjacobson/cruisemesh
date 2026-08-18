package com.cruisemesh.app.devicelink

import uniffi.cruisemesh_core.CoreLinkRole

/**
 * Where a person lands when §9's ceremony stops, and who is allowed to say the
 * phone is set up.
 *
 * Both ends of the ceremony share one screen and one "Done" button, and the
 * button is shown for a run that failed as well as one that finished — so the
 * tap alone says nothing about what happened. Neither does the exit taken: the
 * back arrow and the system back gesture leave the same screen at the same
 * point, so all three are routed through this one answer rather than letting
 * two of them quietly mean "not finished". Getting that wrong is not cosmetic
 * in either direction:
 *
 * * a finished adoption that goes back the way it came lands on the still-live
 *   first-run wizard, which offers "This is another of my devices" again and
 *   then asks a linked person their own name (the two-phone session on
 *   2026-08-18 saw exactly this);
 * * a *failed* run that is treated as a finish marks a phone set up that was
 *   never set up.
 *
 * Plain and Android-free so both are pinned by a unit test rather than by
 * reading a navigation graph.
 */
internal object LinkCompletion {

    /**
     * True only for the phone that was just adopted, and only for a run that
     * reached the end: it now holds this person's contacts, groups and history,
     * so first-run setup has nothing left to ask it.
     *
     * False for [CoreLinkRole.APPROVING_DEVICE] — that phone was already set up
     * and came here from "Your devices", which is where it belongs afterwards —
     * and false for every unfinished run.
     */
    fun entersApp(role: CoreLinkRole, step: LinkStep): Boolean =
        role == CoreLinkRole.NEW_DEVICE && step == LinkStep.DONE
}
