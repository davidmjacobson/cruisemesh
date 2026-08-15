# Moderation and abuse handling

How CruiseMesh handles objectionable content, reports, and abusive users. This
page is written for app store reviewers and for anyone who wants to know what
actually happens when they tap **Report**. Every behaviour described here is
implemented in this repository, and the file it lives in is named so you can
check.

Contact for abuse reports: **abuse@cruisemesh.app**
General support: **support@cruisemesh.app**

---

## 1. What kind of app this is

CruiseMesh is an end-to-end encrypted person-to-person messenger for groups who
are travelling together without internet — a cruise ship, a campsite, a
conference hall. Messages hop phone-to-phone over Bluetooth and the local
network, and optionally through a relay mailbox that stores only ciphertext.

Three things follow from that, and they shape everything below:

- **There is no public content.** No feed, no timeline, no profile discovery,
  no search directory, no nearby-stranger chat, no matchmaking. Nothing a user
  writes is ever visible to anyone other than the contacts or group members they
  addressed it to.
- **There are no accounts.** Identity is a key pair generated on the device.
  There is no sign-up, no username, no email address and no phone number, and
  the operator holds no account records. That constrains what enforcement is
  even possible — see §6, which says so plainly rather than implying otherwise.
- **The operator cannot read messages.** Everything is signed and sealed on the
  sending device. The relay sees ciphertext and routing hints only
  ([SECURITY-DESIGN.md](../SECURITY-DESIGN.md)). There is no server-side
  plaintext to scan, and no server-side copy of a message to attach to a report.

## 2. Preventing objectionable material from reaching a user

CruiseMesh does not scan message content, because it holds none to scan. The
protection is structural instead: **someone you have not accepted as a contact
cannot send you content at all.**

The rule lives in the shared Rust core so both apps enforce it identically —
`core_pairwise_sender_authorized` in
[`core/src/engine.rs`](../core/src/engine.rs). Cryptographically opening an
envelope proves who signed it; it does not make that person a contact. From an
unknown sender, only two message kinds are authorized: a direct friend request
and a ticket-bearing introduced friend request. Text, attachments, reactions,
receipts, group invites and profile updates all fail closed.

Both shells apply that check before any message handler runs —
[`InboundEnvelopeProcessor.kt`](../android/app/src/main/kotlin/com/cruisemesh/app/mesh/InboundEnvelopeProcessor.kt)
on Android,
[`MeshController.swift`](../ios/CruiseMesh/Mesh/MeshController.swift) on iOS.

So the only thing a stranger can put on your screen is a friend request, which
you must accept, and which the app asks you to confirm by comparing safety words
with the other person out of band. Every conversation in the app is the result
of a deliberate, mutual introduction.

**Rules of conduct.** Accepting the Terms of Use is a hard gate before any
message can be sent or received: the accepted version is stored on the device
(`TermsAcceptanceStore`, on both platforms, kept in lockstep by
[`tools/check_terms_version.py`](../tools/check_terms_version.py)) and the
Android boot receiver refuses to start the mesh until the current version is
accepted. The Terms carry an acceptable-use policy prohibiting illegal content,
child sexual abuse material, threats, harassment, hateful conduct,
impersonation, fraud, non-consensual intimate images, doxxing, malware, and
attempts to evade the app's safety controls. Publishing a new version re-gates
every user until they accept it again.

## 3. Reporting a contact

Every contact has a **Report contact** action, reachable from the contact
details sheet inside a chat on both platforms:

- Android — [`ui/ReportContact.kt`](../android/app/src/main/kotlin/com/cruisemesh/app/ui/ReportContact.kt),
  surfaced by [`ui/ContactDetailsSheet.kt`](../android/app/src/main/kotlin/com/cruisemesh/app/ui/ContactDetailsSheet.kt)
- iOS — [`UI/ReportContact.swift`](../ios/CruiseMesh/UI/ReportContact.swift),
  surfaced by [`UI/ChatView.swift`](../ios/CruiseMesh/UI/ChatView.swift)

Tapping it opens the phone's email app with a message to
**abuse@cruisemesh.app** already prepared, containing:

- the reported person's display name,
- their user ID,
- their safety words (the human-readable fingerprint of their key),
- the reporter's own user ID,
- and an empty "What happened:" section for the reporter to fill in.

**Nothing is sent automatically and no message content is attached.** That is
deliberate: the app is end-to-end encrypted, so the operator has no copy of
anything either party wrote, and the reporter decides for themselves what to
quote or describe. The identifiers included are enough to act on, because a user
ID is stable and is what any relay-side measure keys off.

If the phone has no mail app configured, the report does not dead-end: both
platforms fall back to copying `abuse@cruisemesh.app` to the clipboard and
telling the user, so the address is always reachable
(`ContactReportOutcome.ADDRESS_COPIED` on Android, `.showAddress` on iOS; both
branches are covered by unit tests in `ReportContactTest.kt` and
`ReportContactTests.swift`).

Reports can also be sent to `abuse@cruisemesh.app` directly, without using the
app, by anyone — including someone who is not a CruiseMesh user.

## 4. Blocking an abusive user

**Block contact** sits next to Report in the same contact details sheet, on both
platforms. It is a toggle, with a confirmation dialog that states exactly what
blocking does before it takes effect.

Blocking is stored in the shared core as a local tombstone —
`block_user` / `unblock_user` / `is_user_blocked` / `list_blocked_users` in
[`core/src/store.rs`](../core/src/store.rs), backed by a `blocked_identities`
table. Once an identity is blocked:

- **Their envelopes are discarded on arrival, before any handler runs.** Both
  shells check `isUserBlocked` at the top of inbound processing, for one-to-one
  messages, for group messages, and for friend requests shared through a mutual
  contact. Blocked content is never stored and never displayed.
- **No receipts are produced.** Because the drop happens before dispatch, the
  blocked person gets no delivery or read confirmation and no error — blocking
  is silent, and they are never notified.
- **A replayed friend request cannot resurrect them.** The block outlives the
  contact row.
- **They stop appearing as a suggested friend** (`list_friend_suggestions`
  excludes blocked identities) and are excluded from outbound delivery
  bookkeeping.
- **Unblocking is deliberate.** The toggle reverses it, and so does deliberately
  re-importing their contact card by scanning a QR code — a direct, in-person
  act by the user. No remote message can clear a block.

These behaviours are pinned by tests in `core/src/store.rs` (block round-trip
and listing, exclusion from suggestions, no delivery row for a blocked
recipient, and re-import clearing the block).

**Honest limitation.** Blocking stops future messages from reaching your device.
It cannot delete copies of an earlier message that already exist on other
people's phones — in a peer-to-peer mesh, no one holds a central copy to delete.
This is stated in the Terms rather than hidden.

## 5. Where reports go, and what happens next

Reports arrive at **abuse@cruisemesh.app**, a monitored mailbox read by the
developer. CruiseMesh is a small, solo-maintained project; there is no
outsourced moderation queue, and this page does not claim one.

**Commitment: every report is reviewed and acted on within 24 hours of
receipt.** "Acted on" means one of the outcomes below, and the reporter is told
which one, at the address they wrote from.

The review is a human reading the report. There is no automated content
classifier, because there is no content to classify — see §1.

Available responses, in the order they are normally considered:

1. **Help the reporter block and remove the person.** For the great majority of
   reports this is the effective remedy, because it is the only one that takes
   effect immediately, works with no internet connection, and cannot be evaded:
   the block is enforced on the reporter's own device, in the core, before
   dispatch (§4).
2. **Suspend the relay access of the family whose hosted pass carried the
   abuse.** Internet delivery for people who do not run their own relay is sold
   as a per-family pass, and each pass is a token in the relay's database. The
   operator tooling ([`tools/relay_admin.sh`](../tools/relay_admin.sh)) can set
   a family's status to `suspended`, after which every relay request from that
   pass is refused with `403 family_suspended`
   ([`relayd/src/lib.rs`](../relayd/src/lib.rs)), or purge the family and its
   stored envelopes outright. This removes the offender's hosted internet
   delivery.
3. **Refer to law enforcement.** For credible threats to life or safety, or
   child sexual abuse material, the operator will cooperate with a valid legal
   request. What can be produced is limited to what exists: pass purchase and
   token records, and relay transport metadata. Message content does not exist
   in readable form anywhere the operator can reach, and no amount of process
   changes that.

**What is honestly not available, and why.** There is no account to ban, because
there are no accounts — identity is a key pair on a device. There is no
device-level or network-level ban either: phone-to-phone Bluetooth and local
delivery involve no server, so nothing the operator controls sits in that path.
Relay suspension reaches only hosted internet delivery, and only for a family
that bought a hosted pass; anyone can run their own relay from this repository
for free. Enforcement that touches the offender is therefore genuinely limited,
and blocking on the recipient's device is the strong control. Saying so is more
useful to a reviewer than implying a takedown power that does not exist.

## 6. Published contact information

| Purpose | Address |
|---|---|
| Abuse reports and objectionable content | abuse@cruisemesh.app |
| Support | support@cruisemesh.app |
| Security vulnerabilities | see [SECURITY.md](../SECURITY.md) |

Terms of Use and Privacy Policy are published at
[cruisemesh.app](https://cruisemesh.app) and linked from the in-app Terms screen
that must be accepted before the app can be used. The abuse address is published
in the Terms and hard-coded into both shipping apps.

## 7. Summary against the App Store user-generated content requirements

| Requirement | How CruiseMesh meets it |
|---|---|
| A method for filtering objectionable material | Non-contacts cannot send content at all — only friend requests, enforced in the shared core before dispatch (§2). Content from an accepted contact can be removed from your device by blocking them (§4). |
| A mechanism to report offensive content, with timely responses | In-app **Report contact** on every contact, on both platforms, to a monitored mailbox; reviewed and acted on within 24 hours (§3, §5). |
| The ability to block abusive users | In-app **Block contact** on every contact, on both platforms; enforced in the core before any message handler runs (§4). |
| Published contact information | abuse@cruisemesh.app and support@cruisemesh.app, published in the Terms, in the apps, and here (§6). |

CruiseMesh is not a random or anonymous chat service. There is no matching, no
directory and no stranger chat; Bluetooth and local networking are transports
between people who have already introduced themselves in person and compared
safety words.
