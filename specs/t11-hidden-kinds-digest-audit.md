# T11 — Hidden kinds in the digest resend: no-device audit

**Question (from the consolidated backlog, T11):** verify that hidden kinds
(receipts, friend requests, profile sync, directory, group invites/metadata,
LAN-endpoint hints) are included in digest frames / the digest-driven resend.
Suspect **4b** was: *"kind=3 (friend request) is excluded from digest resend, so
a lost first send never retries,"* which would explain iPhone→Android friending
not reciprocating and "other one-shot-message losses."

**Verdict: 4b is ruled out.** Hidden kinds are fully carried by the resend path
on both platforms. The audit is code-only (no device needed). Details below so
the next person doesn't have to re-derive them.

## What the resend actually does

`handleDigest` (Android `MeshService.kt`, iOS `MeshController.swift`) is
identical in shape on both platforms:

```
if contact != null {
    syncReceiptsFirst(...)                      // owed receipts, oldest first
    peerHasThrough = throughLamportForSelf(entries, ownUserId)
    queued  = outboundEnvelopesAfter(contact.userId, ownUserId, peerHasThrough)
    missing = messagesAfter(contact.userId, ownUserId, peerHasThrough)
    for message in missing:
        send(queued[message.lamport] ?? backfillOutbound(...))
}
sprayDigestPlanTo(...)   // carry/mule queue, keyed by msg_id set
```

The retry set is driven by **`messagesAfter`** (the `messages` table) joined to
**`outboundEnvelopesAfter`** (the `outbound_envelopes` table) by lamport.
Neither query filters on `kind`:

- `core/src/store.rs` `messages_after`: `... WHERE chat_id=?1 AND sender_user_id=?2 AND lamport > ?3 ORDER BY lamport ASC` — no kind predicate.
- `core/src/store.rs` `outbound_envelopes_after`: same shape, no kind predicate.

## Why every hidden kind is present to be resent

1. **Authoring writes hidden kinds into both tables.** `author_pairwise_message`
   → `insert_authored_rows` (`core/src/authoring.rs`) does
   `INSERT OR IGNORE INTO messages (...)` **and** `INSERT ... INTO
   outbound_envelopes (...)` for every pairwise kind, and `is_pairwise_kind`
   explicitly includes `KIND_FRIEND_REQUEST`, `KIND_GROUP_INVITE`,
   `KIND_PROFILE_SYNC`, `KIND_FRIEND_DIRECTORY`, etc. So a friend request the
   sender authored is a row in `messages` (so `messages_after` returns it) with
   a matching sealed `outbound_envelopes` row (so the resend has ciphertext).

2. **Receiving writes hidden kinds into `messages` at their lamport slot.**
   e.g. `handleIncomingFriendRequest` calls
   `store.insertMessage(StoredMessage(kind = KIND_FRIEND_REQUEST, lamport =
   body.lamport, ...))`. So a received hidden kind fills its lamport slot rather
   than leaving a permanent gap.

3. **The receipt watermark uses `highestLamport` (plain MAX), not
   `highestContiguousLamport`.** See the comment at the friend-request insert:
   after the lamport ratchet a stream can legitimately start above 1, so a
   contiguous count would stall at 0 forever. Using MAX means a hidden kind
   never wedges the delivered/read watermark.

## The one real constraint (not a friending bug)

The resend loop is gated on **`contact != null`** — we only replay our authored
stream to a peer we already have as a contact. This is correct for the friending
*initiator*: when A scans B's QR, A imports B as a contact and authors the
kind=3 request, so on every (re)connect A receives B's digest, `getContact(B)`
is non-null, and A re-sends the request. It does mean a message authored to
someone who is **not yet your contact** is not digest-resent — but that path
doesn't arise in normal QR friending.

## Where T11 should look next (needs the device session)

- **Suspect 4a — iOS relay fallback mailbox.** Confirm iOS posts the friend
  request to the *contact's* relay mailbox, not its own family mailbox, when no
  direct link exists. This is the remaining code suspect and is best confirmed
  with the on-device repro + iOS Console for "Friend request queued for later
  delivery".
- **On-device repro** per the T11 script (adb logcat
  `FriendRequest|Dropping envelope|Dropping friend request`; iPhone scans the
  Android QR; branch on nothing-logged vs decode error vs identity mismatch).

## Bottom line

The digest/resend subsystem is kind-agnostic and carries friend requests
correctly on both platforms; do not spend more time on a "hidden kinds are
dropped from the digest" theory. Move T11's remaining effort to the iOS relay
fallback path and the device repro.
