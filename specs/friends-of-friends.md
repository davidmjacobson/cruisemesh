# Spec: Friends-of-friends introductions

**Status:** implemented
**Design reference:** `DESIGN.md` §6.2 (identity and friending)

## Outcome

A family of `N` people should be able to establish a mutually connected
CruiseMesh contact graph with only `N - 1` QR scans, instead of
`N(N - 1) / 2` scans.

The `N - 1` scans must form a connected graph (a spanning tree). Each scan
already creates a mutual contact edge: the scanner imports the displayed card,
then the signed `kind=3` friend request imports the scanner on the other phone.
Friends-of-friends introductions close the remaining edges without more QR
work.

For example, Alice can scan Bob, Carol, and Dan. Alice's phone then offers
Carol and Dan to Bob, Bob taps **Add all**, and the signed introductions make
those connections mutual. Three scans are enough for four people.

This feature reduces *QR scans* to the theoretical minimum. It does not silently
turn every social graph into a clique: a person still taps **Add** or **Add
all** before their phone requests the suggested connections.

## Product decisions

1. **Enabled by default, visible in settings.** A new **Friends of friends**
   switch is on by default. Turning it off stops the phone from appearing in
   suggestions, brokering suggestions, showing received suggestions, and
   accepting introduced-friend requests. Direct QR and friend-link friending
   continue to work.
2. **CruiseMesh contacts only.** “Friends” means accepted CruiseMesh contacts.
   The feature never requests access to the iOS/Android address book and never
   shares phone numbers, email addresses, chats, groups, presence, or activity.
3. **Explicit add, automatic mutual completion.** Suggestions are inert until
   the user taps **Add**. The suggested person's phone may then accept the
   cryptographically valid introduction automatically while its setting is on,
   matching the existing one-sided QR action that produces a mutual contact.
4. **The introducer is part of the trust story.** The UI says **Through Alice**
   (or lists multiple mutual friends). An introduced contact is not described
   as QR-verified. Scanning that person's QR later upgrades the stored
   provenance to direct.
5. **No suggestions-of-suggestions.** A phone exports only accepted contacts,
   never candidates in its suggestion inbox. A candidate can be re-shared only
   after an explicit add completes and makes them a real contact. This bounds
   graph expansion behind user decisions.
6. **No relay bearer credentials in suggestions.** A pending suggestion carries
   the public identity needed to seal an introduction, but not `relay_token`.
   The request uses the recipient's current relay or BLE muling. Ordinary
   friend-card/profile exchange reconciles relay details after the introduction
   completes.

The default-on choice deserves clear disclosure. New installs should explain it
in onboarding, and upgraded installs should show a one-time informational card
the first time **Add a friend** is opened. This is not a permission dialog.

## User experience

### Add a friend

Do not label a section “Local contacts”; on both platforms that implies the OS
address book. Use **Add directly** for the existing mechanisms and **Friends of
friends** for introductions:

```text
Add a friend

Friends of friends                              Add all (3)
  [AJ] Avery Jones       Through Mom and Jordan       Add
  [RS] Riley Smith       Through Mom                  Add
  [KT] Kai Taylor        Through Jordan               Add

Add directly
  Scan QR code
  Paste or open a friend link
```

Rules for the list:

- Merge candidates by UserID and show all known introducers.
- Exclude the local identity, existing contacts, blocked identities, dismissed
  suggestions, candidates who have turned discovery off, and candidates whose
  app has not advertised support for this protocol.
- Sort by mutual-friend count, then display name.
- Show initials until the contact's own profile sync arrives. Do not forward
  profile-photo bytes through an introducer in v1.
- **Add all** opens one confirmation sheet listing every person and introducer;
  it is not a silent bulk operation.
- A row becomes **Requested** while its introduction is queued. It disappears
  when the mutual friend request completes and the person moves into the normal
  Friends list.
- A swipe/overflow action can **Hide suggestion**. Hidden candidates do not
  reappear merely because another directory snapshot arrives.

If there are no candidates, explain the asynchronous behavior: “Suggestions
appear after your friends' phones sync. You can still scan a QR code or use a
friend link.” Do not notify for every new suggestion; a count badge on **Add a
friend** is enough.

### Confirmation

The confirmation sheet shows:

- display name and formatted UserID;
- the four fingerprint words;
- “Introduced by Alice” (or all mutual friends);
- the existing same-name/different-key warning;
- honest delivery copy if no relay is configured: the request will wait for a
  BLE/mule path.

The primary action is **Add friend**. This means “ask this discoverable person
to connect through the named introducer,” not “the introducer verified this
person in real life.”

### Setting

Add this to the profile/settings privacy section on both platforms:

> **Friends of friends**
>
> Let your CruiseMesh friends introduce you to people they know, and show you
> introductions from them. Your messages and phone contacts are never shared.

Turning it off must immediately clear locally displayed suggestions and reject
new introduced requests. It also queues policy updates and empty directory
snapshots so other standard clients eventually stop showing stale suggestions.
Existing accepted friends are unaffected.

## Trust and privacy model

### What is shared

For an eligible contact, an introducer shares only:

- display name;
- UserID;
- Ed25519 public signing key;
- X25519 public agreement key;
- that the introducer is already connected to the candidate;
- the candidate's current friends-of-friends policy revision; and
- a short-lived, invitee-bound introduction ticket.

These directory snapshots and requests are pairwise sealed like other
CruiseMesh traffic. Relays and mules see only normal encrypted envelopes.

Do not include relay URL/token, avatar bytes, group membership, message
metadata, timestamps describing social activity, or the candidate's other
friends.

### Meaning of an introduction

Direct QR friending is first-hand TOFU: the scanned card is the key being
trusted, optionally checked aloud with fingerprint words. A friends-of-friends
connection is transitive TOFU: “Alice, whom I already trust, introduced this
key as Carol.” The UI must preserve that provenance and must not use a
“verified” badge.

Public keys are not secret and cannot be made forgettable after sharing. The
off switch is enforced by conforming clients and by rejecting the introduction
handshake on the candidate's own phone; it cannot force a modified client to
erase a card it previously received. This is the same bearer-card limitation as
today's copied friend links, not a new promise of cryptographic anonymity.

## Protocol

Use two new hidden, pairwise-sealed message kinds:

- `kind=6`: `friend-directory` — a full replaceable snapshot sent from one
  accepted contact to another.
- `kind=7`: `introduced-friend-request` — an invitee's own friend card plus the
  introduction ticket it received.

Both kinds use the existing signed `MessageBody`, persistent outbound queue,
relay sync, BLE digest resend, muling, dedupe, and delivered-receipt machinery.
They are never rendered in chat history or used for unread/snippet state.

All codecs and signature verification belong in the Rust core and are exported
through UniFFI. The Android and iOS shells must not implement their own wire
formats.

### Discovery policy

Extend `ProfileSyncContent` with a backwards-decodable v2 carrying:

```text
friends_of_friends_version   u8   (1 for this protocol)
friends_of_friends_enabled   bool
friends_of_friends_revision  u64
```

The revision is a persisted, monotonically increasing local counter. Increment
it whenever the user changes the setting. Apply this policy independently of
`avatar_epoch`, so a policy update cannot be lost because an older profile
photo timestamp is already stored.

New clients must decode profile-sync v1 and v2. A contact whose discovery
protocol version is absent/zero is **not eligible** for forwarding. On first
launch after the upgrade, a client sends a v2 profile sync to every existing
contact even if its name/avatar did not change. This gives every capable user
the requested default-on behavior without treating an old client—which has no
off switch—as having consented.

### Directory snapshot (`kind=6`)

Conceptual core records:

```text
FriendDirectoryContent {
    version: u8 = 1,
    revision: u64,
    entries: [FriendDirectoryEntry]
}

FriendDirectoryEntry {
    candidate: SuggestedFriendCard,
    candidate_policy_revision: u64,
    ticket: IntroductionTicket
}

SuggestedFriendCard {
    name: UTF-8 string,
    user_id: 16 bytes,
    sign_pk: 32 bytes,
    agree_pk: 32 bytes
}
```

`revision` is the introducer's persisted directory counter, incremented once
for a logical fan-out. Each recipient gets a personalized snapshot that:

- excludes the recipient, the introducer, and ineligible contacts;
- contains only contacts with protocol version >= 1 and enabled policy;
- contains a ticket bound to that specific recipient; and
- is capped at 64 entries and 64 KiB decoded size.

The receiver applies only a revision greater than the last revision stored for
that introducer, then atomically replaces all suggestion-source rows from that
introducer. Full-snapshot replacement supplies deletion/opt-out tombstones
without an unbounded event log. An empty newer snapshot removes every
suggestion sourced solely by that introducer.

### Introduction ticket

The outer `kind=6` envelope proves who delivered a snapshot to the invitee, but
that proof is not transferable to the candidate. The entry therefore contains
a normal Ed25519 signature by the introducer over domain-separated application
data:

```text
IntroductionTicket {
    version: u8 = 1,
    introducer_user_id: 16 bytes,
    candidate_user_id: 16 bytes,
    invitee_user_id: 16 bytes,
    candidate_policy_revision: u64,
    issued_at_ms: i64,
    expires_at_ms: i64,
    offer_id: 16 random bytes,
    signature: 64 bytes
}

signed_bytes =
    "CruiseMesh introduction ticket v1\0" || encode(all fields except signature)
```

Use the maintained Ed25519 library whole, as elsewhere in the core. Tickets
expire after 30 days and tolerate 24 hours of device-clock skew. They are bound
to one candidate and one invitee, so another recipient cannot reuse one. The
candidate-policy revision invalidates tickets issued before a setting change,
including off-then-on.

Core ticket tests must cover signature forgery, changed candidate, changed
invitee, changed policy revision, expiry/skew, malformed lengths, and unknown
versions.

### Introduced request (`kind=7`)

When Bob taps **Add Carol** on an entry from Alice:

1. Bob does **not** add Carol to `contacts` yet. He stores a pending request.
2. Bob builds his current ordinary `FriendCard` and a `kind=7` body containing
   that card plus Alice's ticket.
3. Bob seals it directly to Carol's suggested X25519 key. It can travel via the
   configured relay or through mules, including Alice.
4. Carol opens the envelope and validates all of the following:
   - the FriendCard UserID equals the authenticated envelope sender (Bob);
   - the ticket candidate is Carol's own UserID;
   - the ticket invitee is Bob's authenticated UserID;
   - Alice is still an accepted, non-blocked contact;
   - the ticket signature verifies with Alice's stored signing key;
   - the ticket is unexpired;
   - Carol's local friends-of-friends setting is enabled; and
   - the ticket policy revision equals Carol's current local revision.
5. Carol imports Bob, records provenance **introduced by Alice**, and queues the
   existing ordinary signed `kind=3` mutual friend request back to Bob.
6. Bob's existing `kind=3` handler authenticates Carol, promotes the pending
   suggestion to a real contact, records the same provenance, and starts the
   usual profile sync.

The `offer_id` makes pending-state correlation and duplicate processing
idempotent. A replay from the same Bob is harmless; a replay by anyone else
fails the invitee binding. Invalid or disabled requests do not create a contact.

Keep direct QR/link requests as `kind=3`. That preserves the direct-friending
escape hatch when either user disables friends of friends.

## Persistence

Do not overload the existing `contacts` table with pending candidates. Add
core-owned tables equivalent to:

```sql
contact_discovery_policy(
    user_id PRIMARY KEY,
    protocol_version,
    enabled,
    revision
)

friend_directory_sources(
    introducer_user_id PRIMARY KEY,
    applied_revision
)

friend_suggestions(
    candidate_user_id PRIMARY KEY,
    name,
    sign_pk,
    agree_pk,
    candidate_policy_revision,
    first_seen_ms,
    state                 -- available | requested | hidden
)

friend_suggestion_sources(
    candidate_user_id,
    introducer_user_id,
    ticket,
    offer_id,
    expires_at_ms,
    PRIMARY KEY(candidate_user_id, introducer_user_id)
)

contact_provenance(
    user_id PRIMARY KEY,
    source                 -- direct | introduced
    introducer_user_id,
    introduced_at_ms
)
```

The exact schema may normalize ticket fields differently, but the Rust store is
the single source of truth on both platforms. Foreign keys/source cleanup must
not delete an already accepted contact. Re-scanning an introduced contact sets
provenance to `direct`; a later directory snapshot never downgrades it.

Deleting an introduced contact should also create a local dismissal/block
tombstone so a delayed duplicate request cannot immediately recreate the
contact. A deliberate direct QR/link confirmation may clear that tombstone.

## Fan-out and convergence

Queue a new directory revision, debounced into one update, when:

- a contact is added, updated, deleted, blocked, or unblocked;
- a contact's discovery policy changes;
- the local friends-of-friends setting changes;
- an app upgrade first advertises v1 support; or
- periodic repair detects that a current contact has never received a snapshot.

Send one personalized snapshot to every accepted contact. Family-scale limits
(normally 4–10 contacts, hard cap 64) make a full snapshot simpler and safer
than a distributed graph protocol.

Convergence is iterative. With initial scan edges forming any spanning tree,
directory exchange exposes distance-two candidates. Each accepted introduction
adds an edge; new directory snapshots then expose more candidates. With all
users enabled and all queued messages eventually delivered, repeatedly adding
available suggestions reaches the complete graph. No central server or global
directory is required.

## Compatibility and rollout

- Old clients ignore unknown kinds 6 and 7.
- New clients continue decoding old `kind=3` FriendCard payloads and
  profile-sync v1.
- Do not offer a contact until that contact has advertised discovery protocol
  v1. This prevents a new invitee from sending `kind=7` to an old candidate that
  cannot handle it.
- A mixed-version family can continue using QR codes and friend links.
- Release Android and iOS support together before enabling the setting in
  production builds.

Suggested implementation slices:

1. Rust codecs, ticket signing/verification, policy/suggestion store, and graph
   simulation tests.
2. Android and iOS profile-policy v2 send/receive plus the settings switch.
3. Directory snapshot generation, receipt handling, and persistence.
4. Add-friend suggestions UI and pending introduced-request flow on both
   platforms.
5. Bulk add, provenance display, dismissal/block behavior, and upgrade notice.

## Acceptance tests

### Core and store

- Directory, ticket, and introduced-request codecs round-trip and reject every
  truncated length, oversized count, trailing byte, and unknown version.
- Ticket bindings/signature/expiry/policy-revision checks fail closed.
- A newer snapshot atomically replaces one source; an older out-of-order
  snapshot cannot resurrect candidates.
- Multiple introducers merge under one candidate UserID and are removed
  independently.
- Suggestions never appear in `list_contacts()` before the mutual `kind=3`
  response.
- Hidden/requested/blocked candidates remain suppressed across restart and
  delayed replay.

### Graph behavior

Use the Rust mesh simulation for `N = 2..10` and several seed shapes (star,
line, and random trees):

- seed exactly `N - 1` mutual scan edges;
- exchange directories and accept every available suggestion until no work
  remains;
- assert that every node ends with `N - 1` contacts;
- assert no candidate came from another unaccepted suggestion;
- repeat with one disabled node and assert it is neither suggested nor accepts
  introduced requests, while direct QR still works.

### End-to-end

- Four phones, star: Alice scans three phones (three scans), each peripheral
  uses **Add all**, and all six mutual pairs eventually exist.
- Four phones, line: three scans still converge after more than one suggestion
  round.
- Repeat relay-only with BLE off, then BLE/mule-only with internet off.
- Toggle Carol off after Alice has sent a directory: Bob's stale ticket is
  rejected and Carol is eventually removed from Bob's suggestions.
- Forge/change every ticket binding and confirm no contact is created.
- Delete an introduced contact and deliver duplicate old traffic; the contact
  stays deleted.
- Upgrade one phone late: it is not suggested before advertising v1, then
  appears after profile/directory sync.

## Success metrics (local-only field-test logs)

Keep the project's no-telemetry rule. For the family field test, log locally:

- physical QR scans per connected family;
- suggestions displayed, requested, completed, expired, or rejected;
- time from **Add** to mutual `kind=3` completion;
- delivery path (direct BLE, mule, or relay); and
- directory/suggestion counts and payload sizes.

The headline acceptance criterion is a fully connected `N`-person test family
with `N - 1` QR scans and no exchange of OS-address-book or conversation data.
