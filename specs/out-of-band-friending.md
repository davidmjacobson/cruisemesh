# Spec: Out-of-band friending (add a friend you're not standing next to)

**Status:** ready to implement
**Design reference:** DESIGN.md §6.2 ("Friending via out-of-band ID string or in-person
QR code scan"), §9 (relay)

## Problem

QR scanning requires being in the same room. There's no good way to friend someone
remotely — e.g. texting/emailing a friend card to a family member before the cruise so
you're already connected when everyone boards.

What exists today:

- **The core already supports pasted cards.** `parse_friend_card`
  (`core/src/identity.rs:141`) parses the same JSON whether it came from a QR scan or
  pasted text, and `FriendCard` carries `relay_url`/`relay_token`.
- **The mutual-friend mechanism already works remotely.** Importing a card queues a
  signed `kind=3` friend request (`FriendRequestSender`, both platforms) into the
  persistent outbound queue, which uploads via the relay
  (`RelaySyncEvents.requestSync()`). The other side's `handleIncomingFriendRequest`
  (`MeshService.kt:~1684`, and the iOS equivalent in `MeshController.swift`) verifies
  the card against the envelope's authenticated sender and auto-imports the contact.
  So if both phones share a relay, friendship completes with no BLE contact at all.
- **iOS has partial UI:** `FriendsView.swift` has a "Paste friend card JSON" text
  field + Import, and `MyQRView` has a `ShareLink` that shares the raw card JSON.
- **Android has none of it:** "Add a friend" goes straight to the camera
  (`MainActivity.kt` `ScanRoute`), and `MyQrScreen` (`FriendingScreens.kt:66`) shows
  only the QR image — no copy/share, no paste import.

Remaining problems this spec fixes:

1. Android can neither export a card as text nor import pasted text.
2. The raw card JSON is hostile to messaging apps: serde serializes the key byte
   arrays as JSON number arrays (`"sign_pk":[12,240,...]`), producing a ~700-character
   blob that's easy to mangle. We need a compact, copy-paste-robust text form.
3. Nobody tells the user that a remote friendship needs a relay to actually deliver
   messages (or even the friend request) before the next physical meeting.

## Part 1 — Core: a compact "friend link" text format

Add to `core/src/identity.rs` (uniffi-exported, tested):

```rust
/// Compact, chat-app-safe text form of a FriendCard:
/// "CMFRIEND1:" + base64url_nopad(friend-card JSON bytes)
#[uniffi::export]
pub fn make_friend_link(card_json: String) -> Result<String, CoreError>;

/// One entry point for anything a user scanned or pasted. Accepts, in order:
/// 1. the CMFRIEND1: link form (with surrounding whitespace tolerated),
/// 2. raw FriendCard JSON (back-compat: existing QR codes and iOS's current
///    ShareLink output keep working).
/// Returns the parsed card or CoreError::InvalidFriendCard.
#[uniffi::export]
pub fn parse_friend_text(text: String) -> Result<FriendCard, CoreError>;
```

Implementation notes:

- Prefix `CMFRIEND1:` — versioned (a future format bumps to `CMFRIEND2:`), greppable,
  and obviously CruiseMesh's when it lands in a group chat. Use
  `data_encoding::BASE64URL_NOPAD` (the `data-encoding` crate is already a
  dependency — `BASE32_NOPAD` is imported in this file).
- `parse_friend_text` must `trim()` first, then: starts-with prefix ⇒ base64-decode +
  `parse_friend_card`; otherwise fall through to `parse_friend_card` on the raw text.
  All existing key-length validation stays in `parse_friend_card`.
- Tolerate the mangling messaging apps inflict: trim whitespace/newlines anywhere
  around the token, and strip internal whitespace/line breaks from the base64 segment
  before decoding (long strings get wrapped in email clients).

Tests: round-trip link ⇔ card; raw JSON still parses; leading/trailing whitespace and
embedded newlines in the base64 body; wrong prefix and corrupt base64 give
`InvalidFriendCard`; a link built from a card with/without relay fields.

**QR codes also switch to the link form** (`make_friend_link` output) — smaller QR
payload, one format everywhere. `parse_friend_text` keeps old raw-JSON QRs scannable,
and old apps scanning a *new* QR will fail politely with the existing "Not a
CruiseMesh friend card" path (acceptable for a family app; note it in the PR).

Run `core/build-android.sh` after core changes (and flag that `core/build-ios.sh`
must run on a Mac).

## Part 2 — Android UI

### 2.1 Share your card ("My friend card" screen, `FriendingScreens.kt` `MyQrScreen`)

Below the QR image add two buttons:

- **"Share card as text"** — `Intent.ACTION_SEND` (`text/plain`) with
  `makeFriendLink(cardJson)`, via the system share sheet. Prefix the shared text with
  one human line so recipients know what to do, e.g.:
  `"Add me on CruiseMesh — copy this whole message and paste it in the app:\nCMFRIEND1:..."`
  (`parse_friend_text`'s trimming means the sentence must be stripped by the *paster*;
  therefore: put the link on its own line AND make the Android/iOS paste handlers
  extract the `CMFRIEND1:\S+` token from pasted text before calling core — do that
  token-extraction in the shells' paste path, one small shared-logic function per
  platform, unit-tested).
- **"Copy"** — copies just the bare link to the clipboard
  (`ClipboardManager.setPrimaryClip`), with a toast/snackbar.

The screen already has name + relay URL/token fields that feed `makeFriendCard`, so a
card shared from here carries relay info if configured — that's what makes the remote
flow deliverable. See Part 4 for the warning when it doesn't.

### 2.2 Import a pasted card

Replace the current "Add a friend" → camera jump with a small add-friend screen (new
route, e.g. `"addFriend"`, navigated from the existing `onAddFriendClick` in
`ContactsScreen`) offering:

1. **"Scan QR code"** → existing `ScanRoute` (unchanged camera flow).
2. **"Paste friend card"** → multiline `OutlinedTextField` + Import button.

Import handler (mirror `ScanRoute`'s `onContactAdded` at `MainActivity.kt:596-603`,
and iOS `FriendsView.importJSON` for the guards):

```kotlin
val card = parseFriendText(extractFriendToken(pastedText))   // CoreException => inline error
val userId = friendCardUserId(card)
if (userId.contentEquals(identity.userId)) { error = "That's your own card"; return }
val contact = RelayImport.reconcileOnImport(context, store, Contact(userId, card.name, card.signPk, card.agreePk, card.relayUrl, card.relayToken))
store.upsertContact(contact)
FriendRequestSender.queueForScannedContact(context, store, identity, contact)
// navigate to the new 1:1 chat (route "chat/{userIdHex}") on success
```

Notes:
- `RelayImport.reconcileOnImport` already handles adopting the card's relay as our own
  fallback when we have none, and never wiping an existing relay with a blank card —
  reuse it untouched. This is the step that makes a freshly-installed phone able to
  reach the remote friend at all.
- Also add the own-card guard to the QR path (`ScanScreen`'s analyzer) while here —
  Android currently lets you scan yourself; iOS already guards.
- `ScanScreen`'s analyzer should switch `parseFriendCard(decoded)` →
  `parseFriendText(decoded)` so new link-form QRs scan.
- If the profile-photo-sync spec (`specs/profile-photo-sync.md`) has landed, the paste
  import must also queue a `kind=5` profile-sync to the new contact, exactly like the
  scan path.

### 2.3 Deep link (nice-to-have, do last, skippable)

`ACTION_VIEW` intent-filter is **not** in scope — `CMFRIEND1:` is not a URI scheme and
registering one (plus iOS Universal Links) is a separate feature. Keep the paste flow
as the contract.

## Part 3 — iOS UI (small deltas)

- `MyQRView`: switch the QR content and `ShareLink(item:)` to `makeFriendLink(json)`
  with the same one-line human preamble; add a "Copy link" button
  (`UIPasteboard.general.string`).
- `FriendsView.importJSON`: rename to `importText`, run the same
  `CMFRIEND1:\S+` token extraction, and call `parseFriendText` instead of
  `parseFriendCard`. Retitle the paste section "Paste friend card" (it's no longer
  JSON-specific). Everything downstream (own-card guard, relay adoption,
  `FriendRequestSender.sendMutualFriendRequest`) stays as-is.
- `QRScannerView` handler: `parseFriendCard` → `parseFriendText`.

## Part 4 — Honest UX about deliverability

A remotely-added friend is reachable only via the relay until you physically meet.
After a successful paste-import, check whether a relay is now configured
(`RelayConfigStore.load()`):

- **Relay configured** (own fallback or adopted from the card): proceed silently —
  the friend request and any messages will flush next time either phone has internet.
- **No relay anywhere**: show a non-blocking notice on the success state:
  *"Added ⟨name⟩. You don't have a relay set up, so messages will wait until your
  phones are near each other."*

Also: after import the other side hasn't imported *us* yet until our kind=3 arrives.
No UI work needed — the existing ✓/✓✓ ticks on the first message already communicate
"sent but not delivered" — but don't add any "friendship pending" state; mutual import
is invisible plumbing by design (DESIGN.md §6.2).

## Edge cases

- Pasting garbage / truncated base64: inline error ("Not a CruiseMesh friend card"),
  keep the text for correction — don't clear the field on failure.
- Pasting a card for an existing contact: allowed; it's how a renamed/re-keyed friend
  updates (upsert semantics, relay reconciliation preserves a working relay). If the
  profile-photo spec landed first, confirm the avatar-preserving upsert is in place.
- Pasting while mesh service isn't running: `FriendRequestSender` already handles the
  not-connected case (persistent queue + `RelaySyncEvents.requestSync()`); nothing
  extra.
- Whitespace-only input: disable the Import button.
- Both users paste each other's cards simultaneously: both queue kind=3; both handlers
  are idempotent (`insertMessage` dedupe + upsert) — no action needed, but add it to
  the manual test list.

## Test plan

- Core: `parse_friend_text` test matrix from Part 1 (`cargo test --workspace`).
- Android unit: `extractFriendToken` (token amid prose, newlines, no token, multiple
  tokens → first), import-guard logic if extracted into a testable function.
- iOS unit: same extraction tests in `CruiseMeshTests`.
- Manual end-to-end (the acceptance test): phone A shares card text via any messaging
  app → phone B (different network, BLE off/out of range) pastes it → B sees the new
  chat, sends "hi" → within one relay poll A has imported B and sees the message; A
  replies; ticks advance on both sides. Repeat with a card that has no relay info and
  verify the Part 4 warning appears.
