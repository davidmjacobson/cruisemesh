package com.cruisemesh.app.chat

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.cruisemesh.app.R
import uniffi.cruisemesh_core.ContactSafetyFact
import uniffi.cruisemesh_core.ContactSafetyReason

/**
 * §10.4's changed-safety-state surface, in the pattern the app already has.
 *
 * `specs/multi-device-v1.md` §10 note 4 asks for "the standard
 * changed-safety-state surface treatment" when a contact's devices change under
 * them. On this shell that pattern is [IdentityCloneNotice]: a persistent,
 * non-modal banner pinned above the composer, not a dialog and not a row three
 * taps away — because the thing it describes is otherwise silent, and the moment
 * that matters is the moment before somebody types.
 *
 * The facts come from `MessageStore.contactSafetyFacts`, which core raises once
 * per stored version. This file adds words and an acknowledgement and decides
 * nothing: which reason applies, which devices it names and when it may stop
 * being shown are all core's, and the fork case is never resolved by arithmetic
 * here any more than it is there.
 *
 * # Wording
 *
 * No roster, no epoch, no tombstone, no device certificate. A family reads
 * "removed a device", "set up again from a backup", "does not add up", and the
 * one instruction that is actually actionable: check another way before sending
 * anything private.
 */
fun contactSafetyCopy(reason: ContactSafetyReason): Int = when (reason) {
    ContactSafetyReason.DEVICE_REVOKED -> R.string.ui_contact_removed_a_device
    ContactSafetyReason.IDENTITY_RECOVERED -> R.string.ui_contact_set_up_again
    ContactSafetyReason.ROSTER_FORKED -> R.string.ui_contact_devices_dont_add_up
}

/**
 * Whether this reason is one a person can settle themselves after checking
 * out of band.
 *
 * Only the fork is. DL-2 quarantines a contact's roster updates until a person
 * says the fork was resolved, and `MessageStore.clearRosterQuarantine` is the
 * action that says so. The other two reasons are things that happened, not
 * states to be cleared: acknowledging them puts the banner away and changes
 * nothing else.
 */
fun offersOutOfBandCheck(reason: ContactSafetyReason): Boolean =
    reason == ContactSafetyReason.ROSTER_FORKED

/**
 * The fact to show when several are outstanding for one contact.
 *
 * The newest by `observedSeq`, which is core's own monotone observation order
 * (deliberately not a wall clock — nothing on the write path has a trustworthy
 * one). Acknowledging it acknowledges everything at or below it, so a person who
 * dismisses the banner is not handed the same contact's older news afterwards.
 */
fun latestSafetyFact(facts: List<ContactSafetyFact>, personUserId: ByteArray): ContactSafetyFact? =
    facts.filter { it.personUserId.contentEquals(personUserId) && !it.acknowledged }
        .maxByOrNull { it.observedSeq }

@Composable
internal fun ContactSafetyNotice(
    fact: ContactSafetyFact,
    contactName: String,
    onAcknowledge: () -> Unit,
    onCheckedOutOfBand: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Surface(
        modifier = modifier
            .fillMaxWidth()
            .padding(bottom = 8.dp),
        shape = RoundedCornerShape(12.dp),
        color = MaterialTheme.colorScheme.errorContainer,
        contentColor = MaterialTheme.colorScheme.onErrorContainer,
    ) {
        Column(modifier = Modifier.padding(horizontal = 12.dp, vertical = 8.dp)) {
            Text(
                text = stringResource(contactSafetyCopy(fact.reason), contactName),
                style = MaterialTheme.typography.bodySmall,
            )
            Row {
                TextButton(onClick = onAcknowledge) {
                    Text(stringResource(R.string.ui_got_it))
                }
                if (offersOutOfBandCheck(fact.reason)) {
                    TextButton(onClick = onCheckedOutOfBand) {
                        Text(stringResource(R.string.ui_i_checked_its_them))
                    }
                }
            }
        }
    }
}
