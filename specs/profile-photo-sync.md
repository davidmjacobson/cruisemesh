# Spec: Profile photo sync (contact avatars actually reach friends)

**Status:** ready to implement
**Design reference:** DESIGN.md §6.2.1 (contact profile photos), §7.1 (`profile-sync=5` reserved kind), §14.2 (avatar bubble)

## Problem

Today a profile photo is purely local. On Android, `ProfilePhotoStore` saves your own
512×512 `avatar.jpg` under `files/profile/`, and it is only ever shown for *yourself*
(top-bar avatar in `ChatListScreen.kt:108`, `LocalProfileEditor.kt:42`). Contacts always
render as the deterministic color+initials bubble — `AvatarBadge` has a `photoPath`
parameter but nothing ever passes one for a contact. iOS is worse: `AvatarView` has no
photo support at all and `ProfileView` has no photo picker.

Nothing on the wire carries photos: DESIGN.md §7.1 reserves `profile-sync=5`, but
`core/src/protocol.rs` defines kinds 1–4 and 16–18 only, the `contacts` table
(`core/src/store.rs`, `SCHEMA` const) has no avatar columns, and neither shell sends or
handles a kind-5 envelope.

**Goal:** when I set/change/remove my profile photo, every friend's app shows it (and
vice versa), over BLE or the relay, with all the DTN retry machinery the app already
has. Per DESIGN.md §6.2.1: canonical avatar is a square JPEG, max 256×256, target
≤24 KiB, newest `avatar_epoch` wins.

## Architecture decision (follow this)

Ship the avatar as a **hidden chat-stream message**, `kind=5`, one pairwise-sealed
envelope per contact, with the full avatar bytes inline. Rationale:

- Kind=3 friend requests already prove the pattern: a hidden chat-stream message rides
  the entire existing pipeline for free — `buildOutboundAuthoredEnvelope` → persistent
  `outbound_envelopes` queue → BLE digest-based resend on every encounter → relay
  upload/fetch → delivered receipts. No new sync machinery.
- 24 KiB is far below the 180 KiB inline-blob precedent set by `AttachmentPayload`
  (`MAX_BLOB_BYTES`), so an inline blob is unremarkable on this transport.
- Skip DESIGN.md's 64×64 thumbnail entirely for this milestone. It exists for QR-card
  piggybacking, which we are not doing. One canonical 256px image, always.

Epoch ordering (`avatar_epoch` = ms timestamp of the local photo change) makes delivery
idempotent and order-independent: receivers apply a profile-sync only if its epoch is
strictly newer than what they have stored for that contact.

## Part 1 — Core (Rust, `core/`)

### 1.1 `protocol.rs`: new kind + payload codec

Add the constant (fills the reserved gap between 4 and 16):

```rust
/// `MessageBody.kind` value for a profile-sync (DESIGN.md §6.2.1, §7.1):
/// durable contact metadata (display name + avatar), newest epoch wins.
pub const KIND_PROFILE_SYNC: u8 = 5;
```

Add a uniffi-exported record + encode/decode pair, mirroring the existing
`ReceiptContent` style (checked `Cursor` decoding, trailing-garbage rejection, tests):

```text
ProfileSyncContent wire layout (big-endian), carried as MessageBody.content:
offset  size  field
0       1     version       (u8; = 1. Unknown version => decode error, receiver drops.)
1       8     avatar_epoch  (i64 BE; ms since epoch of the *local* profile change)
9       2     name_len      (u16 BE)
11      N     name_utf8     (sender's current display name; propagates renames)
11+N    4     avatar_len    (u32 BE)
15+N    M     avatar_jpeg   (square JPEG <= 256x256; M == 0 means "photo removed")
```

```rust
#[derive(uniffi::Record, Clone, Debug, PartialEq)]
pub struct ProfileSyncContent {
    pub avatar_epoch: i64,
    pub name: String,
    pub avatar: Vec<u8>,   // empty = photo removed
}

#[uniffi::export] pub fn encode_profile_sync_content(content: ProfileSyncContent) -> Vec<u8>;
#[uniffi::export] pub fn decode_profile_sync_content(bytes: Vec<u8>) -> Result<ProfileSyncContent, CoreError>;
```

Both platforms must use these — no per-shell codec (that divergence is exactly what the
Rust core exists to prevent).

Tests (same patterns as the existing protocol tests): round-trip, empty avatar,
empty name, truncation at each field, trailing garbage, unknown version byte rejected,
seal/open round-trip like `message_body_survives_seal_and_open_round_trip`.

### 1.2 `store.rs`: avatar columns on `contacts`

Add two columns via the existing lazy-migration helper (see
`open_migrates_an_old_contacts_table_to_add_relay_token` test and the
`PRAGMA table_info` / `ALTER TABLE` helper around `store.rs:1581`):

```sql
avatar        BLOB,                       -- canonical 256px JPEG, NULL = none
avatar_epoch  INTEGER NOT NULL DEFAULT 0  -- ms; 0 = never received one
```

New store methods (uniffi-exported like the rest of `MessageStore`):

- `set_contact_avatar(user_id, avatar: Option<Vec<u8>>, epoch: i64) -> bool` —
  applies only if `epoch > stored avatar_epoch` for that contact (returns whether it
  applied). `None`/empty clears the blob but still records the epoch (a removal must
  beat an older photo that arrives late).
- `contact_avatar(user_id) -> Option<Vec<u8>>` and `contact_avatar_epoch(user_id) -> i64`.

**Critical:** `upsert_contact` must NOT touch the avatar columns. It is called on every
QR scan, paste import, and incoming kind=3 friend request
(`MeshService.handleIncomingFriendRequest`), and would otherwise wipe a stored avatar
every time a friend request re-arrives. Use an `ON CONFLICT ... DO UPDATE` that lists
only the existing columns, or keep the current INSERT-OR-REPLACE but re-write avatar
columns from the previous row. Add a regression test: upsert over a contact with an
avatar preserves avatar + epoch.

Decision: avatar bytes live in the core DB, not in shell-managed files. One source of
truth, one migration story, and 4–10 contacts × 24 KiB is nothing. (The `Contact`
record itself stays as-is — don't add the blob to `list_contacts`; UI fetches avatars
per-contact via `contact_avatar` and caches decoded bitmaps.)

After core changes, regenerate bindings: `core/build-android.sh` (and note in the PR
that `core/build-ios.sh` must be run on a Mac before the iOS project builds).

## Part 2 — Android shell

### 2.1 Sending

New object `ProfileSyncSender` (put it next to `FriendRequestSender.kt`, package
`com.cruisemesh.app.friending` or a new `profile` package), modeled directly on
`FriendRequestSender.queueForScannedContact`:

`fun queueToContact(context, store, identity, contact, epoch: Long)`:
1. Load own avatar via `ProfilePhotoStore.loadAvatarPath` and **re-encode to the wire
   form**: center-crop square (already done at save time), scale to 256×256, JPEG
   quality starting at 85, stepping down (85→70→55→40) until ≤ 24 KiB (`24 * 1024`).
   No photo ⇒ empty byte array. Do this once per queue-fan-out, not per contact.
2. Build `ProfileSyncContent(avatarEpoch = epoch, name = ProfileStore.loadDisplayName(context), avatar = bytes)`,
   encode with the core codec.
3. Same lamport ratchet as `FriendRequestSender` (`nextAuthoredLamport` over
   `highestContiguousLamport` + acked receipts), `kind = 5u`, insert via
   `store.insertOutgoingMessage(message, buildOutboundAuthoredEnvelope(...), timestamp)`,
   `RelaySyncEvents.requestSync()`, and best-effort immediate
   `MeshRouter.sendToUserId(...)` — copy the friend-request shape exactly.

`fun queueToAllContacts(context, store, identity, epoch)`: loop `store.listContacts()`.

Add `5u` (`KIND_PROFILE_SYNC`) to `isAuthoredChatKind` in `OutgoingTextEnvelope.kt` —
without this `buildOutboundAuthoredEnvelope` refuses the kind and nothing sends.

**Epoch source:** persist `own_avatar_epoch` (ms) in `ProfileStore`'s prefs. Bump it to
`System.currentTimeMillis()` whenever the user takes/chooses/removes a photo **or edits
their display name** (renames propagate through the same message), then fan out.

Trigger points:
- Photo changed/removed or name saved: `ProfileScreen` / wherever `ProfilePhotoStore.saveFromUri`,
  `saveFromBitmap`, `clear`, and `ProfileStore.saveDisplayName` are invoked from UI
  (see `MainActivity.kt` ProfileRoute) → bump epoch → `queueToAllContacts`.
  Debounce the name-edit trigger to on-save/on-exit, not per keystroke.
- New contact imported (QR `ScanRoute` in `MainActivity.kt:596-603`, and incoming
  friend request in `MeshService.handleIncomingFriendRequest` after `upsertContact`):
  if an own avatar or name exists, `queueToContact` with the stored epoch. This is the
  DESIGN §6.2.1 "friending exchanges full photos on the spot" behavior — both sides
  queue their kind=5 at friend time, and the live BLE link (or relay) delivers it.

Onboarding already captures a photo (`OnboardingScreen`); make sure the initial epoch
is set there too so the first friending sends it.

### 2.2 Receiving

In `MeshService.kt`:
- Add `private const val KIND_PROFILE_SYNC: UByte = 5u` next to the other kind consts,
  and a `KIND_PROFILE_SYNC -> handleIncomingProfileSync(...)` arm in the envelope
  dispatch (`MeshService.kt:1513-1522`).
- `handleIncomingProfileSync(address, senderUserId, body, identity)`, modeled on
  `handleIncomingFriendRequest` (`MeshService.kt:~1684`):
  1. Sender must already be a contact (`store.getContact(senderUserId)`); if unknown,
     drop with a log (a profile-sync from a stranger is meaningless — we don't hold
     their keys as a friend).
  2. `decodeProfileSyncContent(body.content)`; malformed ⇒ log + return.
  3. `store.insertMessage(...)` the row (kind=5, hidden) — **required** so the lamport
     stream stays contiguous for digest sync; if not newly inserted, stop (duplicate).
  4. Apply: `store.setContactAvatar(senderUserId, content.avatar.ifEmpty { null }, content.avatarEpoch)`.
     If the name differs and the epoch applied, update the contact's name via
     `upsertContact(contact.copy(name = content.name))` (avatar-preserving upsert from
     Part 1.2 makes this safe). Then `ChatEvents.notifyChatChanged(senderUserId)` so
     open screens refresh.
  5. Send delivered receipts exactly like `handleIncomingFriendRequest` does
     (`highestLamport` watermark → `recordOutgoingReceipt` → wire + relay receipt) —
     this is what stops the sender's queue from resending forever.

Keep kind=5 invisible in chat: `isVisibleChatKind` (`AttachmentPayload.kt:157`) already
excludes it — verify snippet/unread logic in `ChatListLogic` also ignores hidden kinds
the same way it does kind=3 today.

### 2.3 Displaying

`AvatarBadge` gains a `photoBytes: ByteArray?` parameter (decoded with
`remember(photoBytes)`), used when `photoPath == null`. Then pass contact avatars at
every contact render site:

- `ChatListScreen` conversation rows (1:1 chats only; groups keep initials)
- `ContactsScreen` rows (`FriendingScreens.kt:342`)
- `ChatScreen` top bar avatar
- `ContactDetailsSheet`
- `NewGroupScreen` member picker

Load pattern: fetch `store.contactAvatar(userId)` alongside the contact list
(remembered keyed on `avatar_epoch` or refreshed on `ChatEvents`), not inside
composition per frame. A tiny `AvatarCache` (userIdHex → decoded ImageBitmap,
invalidated on chat-changed events) is acceptable if plumbing bytes through every
screen gets noisy — implementer's choice, but decode-per-recomposition is not.

## Part 3 — iOS shell (feature parity)

iOS needs both halves: local own-photo support (doesn't exist yet) and send/receive.

1. **Own photo:** add a `ProfilePhotoStore` (Documents/`profile/avatar.jpg`, 512px
   square, same semantics as Android) and a `PhotosPicker` + remove button in
   `ProfileView`'s "You" section. Persist `ownAvatarEpoch` in `UserDefaults`
   (alongside the existing `ProfileStore.saveDisplayName`). Bump epoch on photo
   change/removal and on display-name save (`ProfileView` Save button).
2. **Send:** port `ProfileSyncSender` next to `FriendRequestSender.swift` (same
   lamport ratchet it already uses); resize/JPEG-iterate to ≤24 KiB with UIKit. Add
   kind 5 to iOS's authored-kind allowlist (`OutgoingEnvelope.swift` mirrors the
   Android gate). Triggers: profile save, and contact import in
   `FriendsView.importJSON` / the incoming friend-request handler in
   `MeshController.swift`.
3. **Receive:** add the kind=5 arm in `MeshController`'s envelope dispatch, same steps
   as Android 2.2 (insert hidden row, `setContactAvatar`, name update, receipts).
4. **Display:** `AvatarView` gains an optional `photo: UIImage?`; pass contact avatars
   in `ChatListView` rows, `FriendsView`, `ChatView` toolbar, `ContactDetailsSheet`,
   `NewGroupView`.

The core codec and store methods come through the generated bindings
(`ios/CruiseMesh/Generated/cruisemesh_core.swift` after `core/build-ios.sh`), so no
Swift wire code.

## Compatibility & edge cases

- **Old client receives kind=5:** current dispatch logs "unhandled kind" and drops
  *without inserting*, so the sender's digest resend will keep re-offering it and the
  peer's contiguous-lamport watermark stalls at the gap. Messages still deliver
  (inserts don't require contiguity), receipts for later messages still advance via
  `highestLamport` paths, but expect resend chatter in mixed-version families. This is
  acceptable for a family app — call it out in the PR description.
- **Out-of-order / duplicate delivery:** epoch comparison in `set_contact_avatar` makes
  application idempotent and last-writer-wins; duplicate envelopes die at
  `insertMessage`.
- **Removal:** empty `avatar` + newer epoch clears the contact's photo everywhere;
  UI falls back to color+initials (already automatic when bytes are null).
- **Photo changed twice before delivery:** both kind=5 rows deliver (they're distinct
  lamports); the older one no-ops on apply.
- **Never resize the received blob or trust its claimed size:** enforce a receive-side
  cap (reject `avatar_len` > 64 KiB in the decoder — pick the bound in core so both
  platforms share it) and decode defensively (a corrupt JPEG must render as initials,
  not crash — `BitmapFactory.decodeByteArray` / `UIImage(data:)` both return nil-safe).
- **Group chats:** avatars are per-contact; group bubbles unchanged.

## Test plan

- Core: codec tests (1.1), store tests — migration adds columns, epoch ordering
  (older/equal epoch rejected, removal epoch wins), upsert preserves avatar.
- Android unit: JPEG quality-iteration helper (pure function over a bitmap → bytes,
  asserting ≤ 24 KiB), `isAuthoredChatKind(5u)`, hidden-kind exclusion in
  `ChatListLogic` snippets.
- `cargo test --workspace` and Android `./gradlew test` must pass; run
  `core/build-android.sh` so `kotlin-gen` is current.
- Manual (two devices or device+emulator): set photo on A → appears on B's chat list
  over BLE; change photo on A while apart → B gets it on next encounter; remove photo
  on A → clears on B; verify over relay path with both phones on different networks.
