package com.cruisemesh.app.notify

/**
 * Collision-free `PendingIntent` request-code allocator (audit FA9).
 * [MessageNotifier] used to reuse `chatId.contentHashCode()` -- an `Int`
 * hash of 32 raw bytes -- as both the notification id and every
 * `PendingIntent` request code for that chat. Two different `chatId`s can
 * hash-collide, at which point `PendingIntent.FLAG_UPDATE_CURRENT` silently
 * rewrites one chat's tap/reply/mark-read target with another's.
 *
 * This keys purely on ([chatIdHex], [purpose]) -- e.g. `("a1b2..",
 * "content")`, `("a1b2..", "com.cruisemesh.app.action.REPLY")` -- and hands
 * out a fresh, never-reused `Int` the first time a combination is asked for,
 * reusing it on every later call for the same combination (so re-posting or
 * re-cancelling the same chat's notification keeps targeting the same
 * `PendingIntent`s instead of leaking a new one each time). Collision-free
 * by construction: distinct keys can never map to the same code, however
 * many chats/purposes accumulate over the process's lifetime -- unlike a
 * hash, which only gets *less* likely to collide with more entries, never
 * guaranteed not to.
 */
class NotificationRequestCodes {
    private val assigned = mutableMapOf<String, Int>()
    private var next = 0

    @Synchronized
    fun requestCodeFor(chatIdHex: String, purpose: String): Int =
        assigned.getOrPut("$chatIdHex:$purpose") { next++ }
}
