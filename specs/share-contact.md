# Spec: Share a contact

**Status:** implemented
**Design reference:** `DESIGN.md` §6.2 (identity and friending),
`specs/friends-of-friends.md` (decision 7 scopes introductions to one pass)

## Outcome

A person can deliberately hand one specific contact's friend card to someone
in front of them, as a QR code, and have that connection complete safely —
without any mechanism that spreads contacts on its own.

Introductions are now scoped to a single Shore Pass, which is the right
default and the wrong absolute. You meet another family on a cruise. Your kid
friends one of their kids. Nothing should start propagating between the two
families — but your second kid should be able to connect to that same kid
without hunting down a stranger's phone to scan it again.

That is a deliberate, one-at-a-time act, and it should look like one.

## Why this is a safety change, not only a convenience

A friend link is **already** a forwardable bearer card. Anyone can paste one
into a message today, and the receiving phone imports it and sends a `kind=3`
friend request that the original person's phone accepts **with no
confirmation at all** (`handleIncomingFriendRequest` — the import happens
before any user is asked anything, because a `kind=3` has always meant "this
person physically scanned your code").

So the status quo is the permissive case: cards travel, and the person whose
card travelled never finds out until a stranger appears in their chat list.
`specs/friends-of-friends.md` says so plainly when it explains that the off
switch "cannot force a modified client to erase a card it previously
received."

Building **Share contact** as a first-class action makes it possible to
tighten *part* of that. A shared card is distinguishable from a scanned one,
so the receiving phone can stop auto-accepting and ask.

Be precise about how much this buys, because the first draft of this section
oversold it. A `kind=3` with no shared-card tail still auto-imports, by
design, forever — that is the compatibility path for direct scans. So the
confirmation gate engages only when the requesting client volunteers the
marker. It raises the floor against **honest carelessness** — Mom pastes a
link where she should have tapped Share contact — which is the common case in
a family app and worth having. It does nothing against anyone who simply
omits the tail. The old door stays open beside the new one.

**Considered and rejected:** gating auto-import on whether this phone has
recently displayed its own QR or exported its own link — confirm otherwise.
That closes the forwarded-link hole for *every* sender, hostile ones
included, with no wire change at all. It is rejected because a legitimate
scan's request can arrive days later over BLE or a mule, long after the
window closed, and turning genuine in-person scans into prompts attacks the
one flow that must stay frictionless. Worth revisiting if the display window
can be made generous enough (a week) without becoming meaningless.

This matters most for the youngest people using the app, who are not the
primary audience but are very much in the room: they will scan things, accept
things, and hand phones around. The rules below are written so that the worst
outcome of a kid tapping everything is a connection that an adult-legible
screen described accurately, that the other person also agreed to, and that
stops working on its own.

## Product decisions

1. **Always an explicit act.** Sharing happens only when a person picks a
   contact and taps **Share contact**. Nothing shares automatically, on a
   schedule, in bulk, or as a side effect of any other action. There is no
   "share all", by design.
2. **A displayed code, never a copyable link.** Sharing shows a QR code on
   screen and offers no **Copy link**. Sharing your *own* link is your call;
   putting somebody else's name, keys, and their family's mailbox deposit
   token into SMS or a group chat — logged by infrastructure neither of you
   controls, with no notification to them — is a different act wearing the
   same button. A displayed code is bounded by being in the room, which is
   also the only situation this feature is for. It keeps the durable-artifact
   and group-chat-blast problems from existing rather than mitigating them
   afterwards.
3. **Only accepted contacts.** You can share a card only for somebody already
   in your Friends list. A pending suggestion, a blocked identity, or a
   candidate from someone else's directory cannot be shared. This is the same
   bound as friends-of-friends decision 5, and it keeps every share one hop
   from a real relationship.
4. **The shared person's switch governs.** **Share contact** is unavailable
   for a contact whose discovery policy is off or absent. Turning off
   **Friends of friends** already means "do not hand me around"; it would be
   incoherent for that to stop automatic introductions but permit manual ones.
   One switch, one meaning. The setting's copy is updated to say so.
5. **The shared person confirms.** A friend request that originated from a
   shared card does **not** auto-import. The receiving phone shows a
   confirmation naming both the requester and the sharer, and nothing is
   written to `contacts` until it is accepted. This is the one place this spec
   deliberately behaves differently from a QR scan, and the reason is that a
   QR scan is self-evidencing — the person was standing there — while a shared
   card is not.
6. **Shared cards expire.** A shared card carries an expiry, default seven
   days. A displayed code is already bounded by being in the room, so the
   expiry is defence in depth rather than the main control — it covers a
   screenshotted code, and it bounds how long a card lives on the phone that
   scanned it before being used. Seven days covers "we'll add each other
   tomorrow" and little else.
7. **Honest provenance, never a verified badge.** A contact added this way is
   recorded as **shared** and displayed as "Shared by Mom". It is not
   described as QR-verified. Scanning that person's own code later upgrades
   the stored provenance to direct, exactly as an introduced contact does.
8. **Never share a credential you hold and they don't.** The card ships with
   the relay fields exactly as stored for that contact — a deposit-class
   token. The sharer's own member token is never substituted, and the sharer's
   own relay config is never used to fill in a contact's missing fields.
9. **Blocking stops your own participation; it cannot recall a card.** A
   shared card is a bearer artifact already in someone else's hands, and every
   validity check runs on the *shared person's* phone — is the sharer my
   accepted contact, does their signature verify, is my switch on. Nothing in
   the format carries "the sharer has since blocked this person" back to that
   phone, so blocking cannot invalidate a card already issued. What blocking
   does: you stop issuing, a shared-card request naming a blocked sharer or
   blocked requester is dropped without a prompt on your own phone, and any
   outstanding card simply dies at its expiry.

   State that plainly in the UI rather than implying recall. Adding a
   revocation channel for a bearer credential is precisely the complexity
   bearer credentials exist to avoid, and it is the strongest argument for
   preferring targeted shares (open question 3), which is why decision 2 removes
   the copyable form outright.
10. **A policy change kills cards issued before it.** The card carries the
   shared person's discovery-policy revision, and the recipient checks it
   against their current one — the same mechanism `IntroductionTicket` uses.
   Without it, someone who turns discovery off and later on again silently
   revives every card ever issued for them, since the live on/off check alone
   only catches the currently-off case.

## User experience

### Sharing

Long-press a contact in Friends (or use the overflow in their detail sheet) →
**Share contact**. The sheet shows:

- the contact's display name and formatted UserID;
- a QR code;
- one line of plain copy: "Anyone with this code can ask to connect with
  Avery. Avery chooses whether to accept. The code stops working in 7 days."

If the contact's discovery setting is off, the action is not offered; the
detail sheet explains "Avery has turned off being introduced to others."

No count badge, no history of who you shared with, no notification to the
contact at share time. The contact learns about it at the only moment it
matters — when somebody actually asks to connect, and they are asked to
approve it.

### Receiving

Scanning a shared code lands on the existing friend confirmation screen, with the introducer line reading "Shared by Mom" and the
same four fingerprint words and same-name/different-key warning as any other
import. The primary action stays **Add friend**.

An expired card gets a specific, literal message: "This code has expired. Ask
for a new one." Not a generic parse failure — an expired share is the common
case, not a malformed one.

### Confirming (the shared person's phone)

A new confirmation, shaped like the existing friend confirmation:

```text
Riley Smith wants to connect
Shared by Mom

[RS]  Riley Smith
      a1b2 c3d4 e5f6 a7b8
      brave · candle · meadow · rust

      Connect          Not now
```

**Not now** dismisses and the same person may ask again — but not without
limit. A prompt that can be re-raised indefinitely, whose primary action is
**Connect**, is a war of attrition that a nine-year-old under social pressure
("just accept it") loses by design. So:

- at most one prompt per requester per day, further arrivals updating the
  pending row silently;
- on the **second** dismissal of the same requester, the sheet gains **Don't
  ask again**, which writes a quiet local tombstone — no notification to
  anyone, nothing that reads as a rebuke;
- a tombstone is cleared by directly scanning that person's own QR code,
  the same escape hatch friends-of-friends uses for a deleted introduced
  contact.

There is no **Block** on this screen — blocking stays where it is, so a child
tapping through a prompt cannot silently sever a relationship. But a person
who only exists in `pending_shared_requests` is invisible to a block list that
only shows contacts, so **Don't ask again** is the reachable exit, and the
pending sheet is reachable from Friends → **Waiting to connect** rather than
only as a transient prompt. A request that is never answered must not be the
only place its sender can be seen.

Until this is answered, the requester's phone shows the connection as pending,
with copy that does not promise delivery: "Waiting for Riley to accept."

## Trust and privacy model

A shared card contains exactly what a friend card contains: display name,
UserID, Ed25519 signing key, X25519 agreement key, and the relay fields as
stored. It adds the sharer's UserID, an issue and expiry time, and the
sharer's signature over all of it.

The signature is what a shared card buys over a forwarded link. It lets the
recipient's phone say truthfully who authorized the share, and it lets the
shared person's phone verify that the request came from a card one of their
own accepted contacts actually issued — rather than from anyone who once saw
their link.

What this does **not** claim:

- It is not identity verification. "Shared by Mom" means Mom passed this card
  along, not that Mom vouches for who the person is in real life.
- It is not enforceable against a modified client. A conforming phone honors
  the expiry, the discovery switch, and the confirmation step. A patched one
  can still strip the marker and send a plain `kind=3`, which is exactly
  today's behavior for a forwarded link — this spec raises the floor and does
  not claim a ceiling. Same bearer-card limitation the friends-of-friends
  spec names.
- It does not narrow the relay credential. The card carries a **family-scoped**
  deposit token: whoever receives it can post into that whole family's
  mailbox, not just to the one person. That is already true of every QR scan
  and every friend link, and the deposit-class rate bucket is what bounds the
  damage. It is called out here because sharing makes it easier to do
  repeatedly, and because a per-contact deposit credential would be the right
  long-term answer if that ever stops being enough.

## Protocol

No new message kind for the request itself. A shared card is a new encoding of
an existing card, and the resulting request stays `kind=3` so old clients keep
working.

**Shared card** (a new `parse_friend_text` variant, alongside friend cards and
friend links):

```text
SharedFriendCard {
    version: u8 = 1,
    card: FriendCard,          // the shared contact, byte-identical to stored
    sharer_user_id: 16 bytes,
    shared_policy_revision: u64,   // the shared person's discovery revision
    issued_at_ms: i64,
    expires_at_ms: i64,
    signature: 64 bytes        // Ed25519 by the sharer
}

signed_bytes =
    "CruiseMesh shared contact v1\0" || encode(all fields except signature)
```

The `kind=3` friend request grows a backwards-decodable optional tail carrying
the `SharedFriendCard` the requester imported from. A request with no tail is
a direct scan and keeps today's auto-import. A request with a tail is held for
confirmation, and is rejected outright unless:

- the tail's `card` UserID equals the recipient's own UserID;
- the sharer is an accepted, non-blocked contact of the recipient;
- the signature verifies against the sharer's stored signing key;
- the card is unexpired, tolerating 24 hours of clock skew;
- the recipient's own discovery setting is on; and
- `shared_policy_revision` equals the recipient's current revision.

Failing any of these drops the request without a prompt. All encoding and
verification lives in the Rust core and is exported through UniFFI; neither
shell implements the wire format or the checks.

## Persistence

Reuse `contact_provenance` with a third `source` value:

```text
source: 0 = direct | 1 = introduced | 2 = shared
```

`upsert_contact_provenance` currently rejects `source > 1` and must widen by
exactly one value. The existing "never downgrade a direct provenance" rule
applies unchanged: a later shared import cannot overwrite `direct`.

Pending inbound requests need somewhere to wait, since nothing may touch
`contacts` before confirmation:

```sql
pending_shared_requests(
    requester_user_id PRIMARY KEY,
    name, sign_pk, agree_pk, relay_url, relay_token,
    sharer_user_id,
    expires_at_ms,
    first_seen_ms
)
```

A duplicate delivery of the same request updates the row rather than stacking
prompts. Rows past `expires_at_ms` are swept on read. Accepting moves the row
into `contacts` with `source = 2` and queues the ordinary mutual `kind=3`
back. **Not now** deletes the row and increments a dismissal count kept
separately, so it survives the row it came from:

```sql
shared_request_dismissals(
    requester_user_id PRIMARY KEY,
    count,
    suppressed          -- 1 once "Don't ask again" was chosen
)
```

`suppressed = 1` drops matching requests before any prompt. A direct QR import
of that person clears the row.

Note on the provenance widening: `upsert_contact_provenance` protects only
`source = 0`, so `introduced` → `shared` is last-write-wins. That is
acceptable — both mean "not verified in person" — but it is narrower than the
phrase "never downgrade" suggests, and the guard change is exactly one value
(`source > 2`).

### Requester side

The requester needs state too, or "Waiting for Riley to accept" is a sentence
with no machine behind it. Every rejection path drops silently by design, and
**Not now** sends nothing back, so the honest common case is that no reply
ever comes:

```sql
outgoing_shared_requests(
    candidate_user_id PRIMARY KEY,
    expires_at_ms,
    sent_at_ms
)
```

The row expires with the card. On expiry the UI stops saying "waiting" and
says something actionable: "Riley didn't respond. Ask them to scan your code
directly." Without this the feature's most common failure is an unexplained
permanent hang.

## Compatibility and rollout

- Old clients cannot parse a shared card and show the existing "that doesn't
  look like a friend code" error. Acceptable: the sharer can still send a
  plain friend link.
- Old clients receiving a tailed `kind=3` ignore the unknown tail and
  auto-import, exactly as today. The confirmation step is a property of
  updated recipients, which is the honest way to describe it in the release
  notes.
- Ship both shells together, as with friends-of-friends.

Suggested slices: (1) core codec, signing, verification, expiry, and the
provenance widening; (2) the share sheet and QR generation; (3) the
pending-request store and confirmation screen on both platforms; (4) the
dismissal cap, suppression, and the settings copy change.

## Acceptance tests

### Core

- Shared-card codec round-trips and rejects every truncated length, trailing
  byte, unknown version, and malformed field length.
- Signature forgery, a swapped inner card, a changed sharer, a changed
  expiry, and a card past expiry all fail closed.
- Clock skew inside 24 hours is tolerated; outside it is not.
- `source = 2` persists and never downgrades an existing `direct`.

### Behavior

- A tailless `kind=3` still auto-imports — the direct-scan path is unchanged.
- A tailed `kind=3` creates no contact until accepted, survives app restart as
  pending, and does not stack prompts on redelivery.
- **Not now** permits a later request from the same person; a second dismissal
  offers **Don't ask again**; once suppressed, further requests never prompt;
  a direct QR import clears the suppression.
- At most one prompt per requester per day.
- A request naming a sharer who is not a contact, or who is blocked, is
  dropped silently.
- Sharing is unavailable for a contact with discovery off. A card issued
  before they turned it off is refused afterwards by the recipient's own live
  check, and a card issued before an off-then-on cycle is refused by the
  policy-revision mismatch.
- An outgoing request whose card expires with no answer surfaces the
  "didn't respond" state rather than waiting forever.

### End-to-end

- Two families, four phones: a kid shares a contact with a sibling, the
  sibling connects after the other kid accepts, and **no** suggestion appears
  on any phone in either family — the pass scoping from
  `specs/friends-of-friends.md` decision 7 still holds either side of the new
  edge.
- The same flow relay-only with BLE off, then BLE/mule-only with internet off.
- A shared code scanned after seven days is refused with the expiry message.
- No surface anywhere produces a shared card as copyable text.

## Dependency: passless outsiders (resolved, already fixed)

The first draft of the pass scoping treated a contact with **no** Shore Pass
as being on ours, reasoning that unknown is not foreign. A family met on a
cruise who never bought a pass landed in that same bucket, so connecting to
their kid — by a shared card *or* by a plain QR scan, which is how the
scenario actually starts — made that kid friends-of-friends eligible inside
your family, propagating exactly as the tester pass did. Provenance alone does
not close it: the motivating case is a *direct* scan, recorded as
`source = 0`, indistinguishable from scanning a relative.

Resolved in `specs/friends-of-friends.md` decision 7 and already implemented:
introductions require a shared Shore Pass, with no in-person fallback for
passless phones. An interim version did allow one, and it left this leak half
open — a passless family who met another family face to face still propagated
into each other's suggestion lists.

Closing it completely is what makes this spec's premise true: introductions
never cross a household, so crossing one is always a deliberate act. It also
raises the stakes on the deliberate act being well designed, since it is now
the *only* way across.

## Open questions

1. **Seven days.** Long enough to be useful, short enough that an old chat log
   goes stale. No field data behind the number yet.
2. **Should the sharer learn the outcome?** Currently they see nothing. A
   quiet "Riley accepted" would be friendly and is also a small disclosure
   about someone else's decision. Left out of v1.
3. **Targeted shares.** When the person you are sharing *with* is already a
   contact, the share could instead be sealed to them and bound to their
   UserID, reusing the friends-of-friends ticket machinery — no expiry
   ambiguity, no scannable artifact at all, and blocking becomes trivially
   effective because delivery simply stops. There is a real argument that this
   should have been v1: in the motivating scenario the person you share with
   is your own second kid, already your contact, on your pass, standing next
   to you.

   Deferred rather than dismissed. The displayed-code form covers the case
   where the second phone is not yet connected to anything, which the targeted
   form structurally cannot, and dropping **Copy link** (decision 2) removes
   most of what made the bearer form risky — what remains is a code on a
   screen, in a room, for seven days. Revisit if field use shows the in-family
   share is the dominant one; the two can coexist, with targeted preferred
   automatically whenever the recipient is already a contact.
