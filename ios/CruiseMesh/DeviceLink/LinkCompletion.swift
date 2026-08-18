import Foundation

/// Where a person lands when §9's ceremony stops, and who is allowed to say the
/// phone is set up.
///
/// Both ends of the ceremony share one screen and one "Done" button, and the
/// button is shown for a run that failed as well as one that finished — so the
/// tap alone says nothing about what happened. Getting that wrong is not
/// cosmetic in either direction:
///
/// * a finished adoption that goes back the way it came lands on the still-live
///   first-run wizard, which offers "This is another of my devices" again and
///   then asks a linked person their own name (the two-phone session on
///   2026-08-18 saw exactly this);
/// * a *failed* run that is treated as a finish marks a phone set up that was
///   never set up — which on this shell already happened, because `onFinished`
///   fired for `.done` and `.failed` alike.
///
/// Plain and SwiftUI-free so both are pinned by a unit test rather than by
/// reading a view hierarchy.
///
/// Mirrors Android's `LinkCompletion.kt`.
enum LinkCompletion {

    /// True only for the phone that was just adopted, and only for a run that
    /// reached the end: it now holds this person's contacts, groups and history,
    /// so first-run setup has nothing left to ask it.
    ///
    /// False for `CoreLinkRole.approvingDevice` — that phone was already set up
    /// and came here from "Your devices", which is where it belongs afterwards —
    /// and false for every unfinished run.
    static func entersApp(role: CoreLinkRole, step: LinkStep) -> Bool {
        role == .newDevice && step == .done
    }
}
