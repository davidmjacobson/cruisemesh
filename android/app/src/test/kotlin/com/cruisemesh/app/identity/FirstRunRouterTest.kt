package com.cruisemesh.app.identity

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The rule the three first-run doors share: every one of them shows the
 * permissions step exactly once, and none of them shows it twice.
 *
 * Each test walks a door end to end -- the store values a real route would have
 * written at each point -- rather than asserting a single mapping, because the
 * bug this pins was never a wrong answer to one question. It was a route that
 * never asked.
 */
class FirstRunRouterTest {

    @Test
    fun `wizard carries its own permissions step and does not repeat it`() {
        // A fresh install: nothing recorded at all.
        assertEquals(
            FirstRunDestination.WIZARD,
            FirstRunRouter.destination(
                setupComplete = false,
                permissionsStepDone = null,
                meshPermissionsGranted = false,
            ),
        )
        // Finishing the wizard records both facts. Slide 4 was the step.
        assertEquals(
            FirstRunDestination.HOME,
            FirstRunRouter.destination(
                setupComplete = true,
                permissionsStepDone = true,
                meshPermissionsGranted = false,
            ),
        )
    }

    @Test
    fun `own-device link does not skip the permissions step`() {
        // LinkAdoption marks setup complete and the step pending, in one go.
        assertEquals(
            FirstRunDestination.PERMISSIONS,
            FirstRunRouter.destination(
                setupComplete = true,
                permissionsStepDone = false,
                meshPermissionsGranted = false,
            ),
        )
        // And once through it, the app -- not the step again.
        assertEquals(
            FirstRunDestination.HOME,
            FirstRunRouter.destination(
                setupComplete = true,
                permissionsStepDone = true,
                meshPermissionsGranted = false,
            ),
        )
    }

    @Test
    fun `backup restore does not skip the permissions step either`() {
        assertEquals(
            FirstRunDestination.PERMISSIONS,
            FirstRunRouter.destination(
                setupComplete = true,
                permissionsStepDone = false,
                meshPermissionsGranted = false,
            ),
        )
    }

    @Test
    fun `a phone that already has the permission is not asked again`() {
        assertEquals(
            FirstRunDestination.HOME,
            FirstRunRouter.destination(
                setupComplete = true,
                permissionsStepDone = false,
                meshPermissionsGranted = true,
            ),
        )
    }

    @Test
    fun `an install older than the flag is never pulled back into setup`() {
        assertEquals(
            FirstRunDestination.HOME,
            FirstRunRouter.destination(
                setupComplete = true,
                permissionsStepDone = null,
                meshPermissionsGranted = false,
            ),
        )
    }

    @Test
    fun `an unfinished wizard is the wizard whatever else is recorded`() {
        for (stepDone in listOf(null, true, false)) {
            for (granted in listOf(true, false)) {
                assertEquals(
                    FirstRunDestination.WIZARD,
                    FirstRunRouter.destination(
                        setupComplete = false,
                        permissionsStepDone = stepDone,
                        meshPermissionsGranted = granted,
                    ),
                )
            }
        }
    }
}
