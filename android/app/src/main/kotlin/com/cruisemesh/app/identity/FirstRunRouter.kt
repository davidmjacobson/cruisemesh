package com.cruisemesh.app.identity

/** The three places first-run setup can send a person. */
enum class FirstRunDestination {
    /** The six-slide wizard, which carries the permissions step inside it. */
    WIZARD,

    /** The permissions step on its own, for a route that arrived past the wizard. */
    PERMISSIONS,

    /** The app. */
    HOME,
}

/**
 * Which screen a launch — or the end of a setup route — should land on.
 *
 * There are three ways onto a person's fleet and only one of them is the
 * wizard. "This is another of my devices" and "Restore from a backup" both
 * finish by marking setup complete from underneath, which used to drop the
 * phone straight onto the chat list with the mesh off behind a permissions
 * notice; a two-phone session had to grant Nearby devices by hand from system
 * settings. The step is not optional on one route and mandatory on the others,
 * so the decision lives here instead of being spelled three times in the
 * navigation graph.
 *
 * Plain and Android-free, like [com.cruisemesh.app.devicelink.LinkCompletion],
 * so the rule is pinned by a unit test rather than by reading a nav graph.
 */
object FirstRunRouter {

    /**
     * @param setupComplete [OnboardingStore.isCompleted].
     * @param permissionsStepDone [OnboardingStore.permissionsStepDone] — `null`
     *   for an install older than the flag, which is never sent back through
     *   setup by an app update.
     * @param meshPermissionsGranted whether Nearby devices access is already
     *   held. Nobody is walked through a step whose only outcome they have
     *   already produced, however they produced it.
     */
    fun destination(
        setupComplete: Boolean,
        permissionsStepDone: Boolean?,
        meshPermissionsGranted: Boolean,
    ): FirstRunDestination = when {
        !setupComplete -> FirstRunDestination.WIZARD
        meshPermissionsGranted -> FirstRunDestination.HOME
        permissionsStepDone == false -> FirstRunDestination.PERMISSIONS
        else -> FirstRunDestination.HOME
    }
}
