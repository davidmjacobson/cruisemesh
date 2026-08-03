package com.cruisemesh.app.notify

import android.content.Context
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.Group

/**
 * The delivery path's one outlet for "tell the user something arrived", kept
 * behind an interface for exactly the reason [InboundEnvelopeProcessor.LanHooks]
 * is: so the receive path can be driven from a JVM unit test without dragging
 * in the Android framework.
 *
 * ### Why this exists
 *
 * ROADMAP.md makes notification reliability a **release gate** — *"background
 * delivery must produce a timely local notification... the incumbent apps'
 * single most common failure is 'the message arrived and nobody knew' — this
 * project refuses to ship that."*
 *
 * Before this interface, the gate's decisive branch was the one branch of the
 * inbound path that no unit test could execute. [MessageNotifier] reaches
 * straight for `Context.getSystemService` and `Base64`, which throw on the
 * bare JVM, so every test that drove a real envelope through
 * [com.cruisemesh.app.mesh.InboundEnvelopeProcessor] had to *route around*
 * the notification:
 *
 * - `GroupFanoutRelayDeliveryTest` marked the chat on-screen so the notify
 *   branch would be skipped ("the production notification path is not what
 *   this test pins").
 * - `BlockedSenderTest` and `GroupMembershipEnforcementTest` wrapped delivery
 *   in `runCatching { }`, which swallows the throw — and would equally
 *   swallow a real delivery failure.
 *
 * So the release gate was enforced by nothing, and the test suite had grown
 * three workarounds that each make the notify branch *less* observable. With
 * an injectable announcer a test can assert the thing the gate is actually
 * about: a newly stored, user-visible message whose chat is not on screen
 * announces itself exactly once, on every arrival transport.
 *
 * ### What this is NOT
 *
 * This is a *sink*, not a policy. Suppression decisions stay exactly where
 * they were: the on-screen check lives at the call site in
 * [com.cruisemesh.app.mesh.InboundEnvelopeProcessor] (see [ChatVisibility]),
 * and per-chat muting stays inside [MessageNotifier] via [ChatMuteStore].
 * Nothing about which notifications fire changes by introducing this.
 *
 * Implementations must be safe to call from arbitrary BLE/LAN/relay-sync
 * threads, same as [MessageNotifier] already is.
 */
interface IncomingMessageAnnouncer {

    /** A 1:1 chat message from a known contact landed while its chat was off screen. */
    fun announceDirectMessage(contact: Contact, preview: String)

    /** A group chat message landed while that group's chat was off screen. */
    fun announceGroupMessage(group: Group, senderName: String, preview: String)

    /** This device was added to a group. */
    fun announceGroupInvite(group: Group)

    /** A mutual friend request completed and imported a new contact. */
    fun announceFriendAdded(contact: Contact)

    /**
     * Somebody a contact shared this user's card with is asking to connect
     * (specs/share-contact.md). Nothing has been written to `contacts`: this
     * only points at the pending decision. Defaulted to a no-op so a fake
     * announcer written before this existed still compiles.
     */
    fun announceSharedRequest(requesterUserId: ByteArray, requesterName: String) = Unit
}

/**
 * The production announcer: a thin pass-through to [MessageNotifier], which
 * keeps the mute check and the POST_NOTIFICATIONS check it already owned.
 */
class NotificationAnnouncer(private val context: Context) : IncomingMessageAnnouncer {

    override fun announceDirectMessage(contact: Contact, preview: String) =
        MessageNotifier.notifyIncomingMessage(context, contact, preview)

    override fun announceGroupMessage(group: Group, senderName: String, preview: String) =
        MessageNotifier.notifyIncomingGroupMessage(context, group, senderName, preview)

    override fun announceGroupInvite(group: Group) =
        MessageNotifier.notifyGroupInvite(context, group)

    override fun announceFriendAdded(contact: Contact) =
        MessageNotifier.notifyFriendAdded(context, contact)

    override fun announceSharedRequest(requesterUserId: ByteArray, requesterName: String) =
        MessageNotifier.notifySharedRequest(context, requesterUserId, requesterName)
}
