package com.cruisemesh.app.mesh

import com.cruisemesh.app.R
import com.cruisemesh.app.relay.RelayHttpException
import java.io.IOException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import javax.net.ssl.SSLException

/**
 * Pure policy for the Shore Pass first-check flow, kept out of the
 * composable so it stays unit-testable. Mirrored by the private helpers in
 * iOS CruisePassView.
 */

/**
 * One silent retry absorbs the cold-start transport race observed on real
 * devices. Only transport-level failures qualify: HTTP rejections are
 * deterministic, and anything outside the IOException family (decode bugs,
 * cancellation) would fail identically on a second attempt.
 */
fun shouldRetryFirstRelayCheck(error: Throwable): Boolean =
    error is IOException && error !is RelayHttpException

/** Maps a failed relay check to the user-facing explanation. */
fun relayCheckFailureRes(error: Throwable, hasValidatedInternet: Boolean): Int = when {
    (error as? RelayHttpException)?.relayCode == "family_expired" ->
        R.string.ui_this_cruise_pass_has_expired
    (error as? RelayHttpException)?.relayCode == "family_suspended" ->
        R.string.ui_this_cruise_pass_is_suspended
    error is RelayHttpException ->
        R.string.ui_this_setup_card_was_rejected
    error is SocketTimeoutException ->
        R.string.ui_cruise_pass_check_timed_out
    error is UnknownHostException ->
        R.string.ui_cruise_pass_service_not_found
    error is SSLException ->
        R.string.ui_cruise_pass_secure_connection_failed
    !hasValidatedInternet ->
        R.string.ui_android_has_not_verified_internet
    else ->
        R.string.ui_cruise_pass_check_failed_network
}
