# UI/UX Review — Suggestions for Polish, Professionalism & Personality

A review of the Android (Jetpack Compose) and iOS (SwiftUI) shells as of July 2026.
Overall the bones are good: the connectivity indicator system is genuinely well thought
out, the Android composer is Signal-quality, onboarding explains a hard concept clearly,
and both apps handle dark mode. The suggestions below are ordered by impact, with file
references so each item is actionable.

**Legend:** 🔴 high impact · 🟡 solid improvement · 🟢 nice-to-have polish

---

## 1. Cross-cutting themes

### 🔴 1.1 Replace the "👥" emoji group marker with real group avatars

Both platforms prefix group names with a raw emoji:

- `android/.../ui/ChatListScreen.kt:265` — `"👥 $displayName"`
- `ios/.../UI/ChatListView.swift:320` — `"👥 \(displayName)"`
- `android/.../friending/FriendingScreens.kt:400` — 👥 as the "New group" icon

Emoji-in-text renders differently per platform/font and reads as a prototype.
Instead, give groups a proper avatar treatment in `AvatarBadge`/`AvatarView`:
either a distinct group glyph inside the colored circle, or the classic
overlapping mini-avatars of the first 2–3 members. Keep the name column clean.

### 🔴 1.2 Stop showing "Lamport" to users

Message Info exposes internal protocol jargon:

- `android/.../chat/ChatScreen.kt:1168` — `"Lamport: ${message.lamport}"`
- `ios/.../UI/ChatView.swift:555` — same

No family member on a cruise knows what a Lamport clock is. Drop it from the
user-facing dialog (or show it only when the debug log toggle is on). The rest
of the info dialog could also become a nicer bottom sheet with labeled rows
instead of a `\n`-joined string in an alert.

### 🔴 1.3 Localization groundwork: extract strings, stop hardcoding `Locale.US`

Every user-facing string on both platforms is a hardcoded literal, and dates are
pinned to US English:

- `android/.../chat/ChatScreen.kt:921` — `SimpleDateFormat("MMMM d, yyyy", Locale.US)`
- `ios/.../UI/ChatView.swift:343,473,551` — `Locale(identifier: "en_US")`

Cruise ships are one of the most multilingual environments imaginable. Even if
translation isn't on the roadmap, use `DateFormatter`/`DateUtils` with the user's
locale for dates and times, and move Android strings into `strings.xml` (this also
unlocks plurals — see 1.5). This is much cheaper to do now than after more screens land.

### 🟡 1.4 Normalize copy casing and voice

Titles and dialogs mix Title Case and sentence case, sometimes within one screen:

- `ChatListScreen.kt:186` — `"Delete group"` vs `"Delete Contact"` (same dialog!)
- `ProfileScreen.kt` — "Profile & Settings", "Account backup", "Your device identity",
  "Mesh Status", "Relay Presence", "Legal"

Pick sentence case (the Material and Apple HIG default) and apply it everywhere:
"Mesh status", "Relay presence", "Delete contact". Small thing, big perceived-quality gain.

### 🟡 1.5 Unread badge: handle two digits and beyond

- Android (`ChatListScreen.kt:327-342`) uses a fixed `20.dp` circle — "12" already
  crowds it and "120" will clip. iOS uses a capsule (better) but has no cap either.
- Use a capsule/pill on Android too, with `99+` as the maximum label, and animate
  its appearance (scale-in) for a bit of life.

### 🟡 1.6 Motion and haptics — the cheapest "premium feel" wins

There is almost no motion in either app today. A few targeted additions:

- **Mesh status dot**: a slow pulse animation while state is "Meshing"/"Starting"
  makes the mesh feel alive — it's the product's hero feature and today it's a
  static 8dp circle (`MeshStatusPill.kt:185-191`, `MeshStatusPill.swift:10-12`).
- **Message send**: animate the new bubble in (slide/fade from the composer) and
  add a light haptic tick on send and on tick-status upgrade (delivered → read).
- **List changes**: `animateItemPlacement()` on the Android chat list; iOS gets most
  of this free once ids are stable (see 3.2).
- **Long-press**: haptic feedback when the reaction overlay opens.

### 🟡 1.7 Reachability legend is color-only at a glance

The green/blue/amber dot system (`CruiseMeshTheme.kt:18-42`) is documented and has
content descriptions, but a sighted color-blind user gets nothing at a glance, and
new users can't discover what the colors mean. Suggestions:

- Tapping the mesh status pill should open a small "What do these dots mean?" legend
  sheet (with dot + shape + label per row) instead of / in addition to the current
  status text. iOS already has a "Mesh status" alert — upgrade it to a sheet with the legend.
- The hollow-ring vs. filled distinction for MESH_CARRY is good shape-coding — consider
  extending that idea (e.g. relay = dot with tiny cloud/ring) so no state is color-only.

### 🟢 1.8 A touch of brand personality (professional, not cutesy)

The nautical identity is already latent in the product: fingerprint words like
"anchor beacon coral dock", "Bridge Crew" in previews, "CruiseMesh" itself. Lean
into it in *low-frequency* copy where charm doesn't cost clarity:

- Empty chat list: "All quiet on deck. Add a friend to start messaging — no signal required."
- Empty thread (new chat): "You're connected, ship-to-ship. Say hi 👋"
- Gap indicator: current lowercase "some messages may still be in transit" → "Some messages
  are still making their way across the ship" (and capitalize it).
- Voice recording hint "release to send" → keep, it's good.
- Keep *status/error copy strictly literal* (the connectivity warnings are excellent
  as-is — don't add whimsy to anything blocking).

A tiny wordmark/logo in the home top bar (instead of plain `Text("CruiseMesh")`)
plus one display-weight font for onboarding headlines would carry the brand a long way.

---

## 2. Android-specific

### 🔴 2.1 Chat list: deletion is undiscoverable, and long-press is overloaded

Deleting a conversation is only reachable via long-press (`ChatListScreen.kt:213`),
with no visual affordance. iOS uses swipe actions. Recommendation:

- Add swipe-to-delete (Compose `SwipeToDismissBox`) with a red trash reveal, and/or
  a long-press context menu (`DropdownMenu`) offering "Delete" so the dialog isn't
  the immediate response to a long-press.
- Bonus: "Delete" as the *only* long-press action feels dangerous; a menu also gives
  you a home for future actions (mute, pin, mark read).

### 🔴 2.2 The overflow menu duplicates the FAB

`ChatListScreen.kt:120-137`: the top-bar ⋮ menu contains exactly one item, "Friends",
which calls the same `onNewChatClick` as the FAB. Either remove the overflow menu
entirely, or make it earn its place (Friends, New group, Mesh status — matching iOS's
menu). A one-item menu that duplicates the FAB reads as unfinished.

### 🟡 2.3 Mesh status pill placement

The full-width pill sits between the app bar and the list on every launch
(`ChatListScreen.kt:155-160`), permanently costing a row of vertical space even when
everything is healthy. Options:

- Shrink it to a compact centered pill (like iOS, which pads it to content width), or
- Move healthy-state status into the top app bar (small dot + text under the title),
  showing the prominent pill only for non-nominal states.

### 🟡 2.4 Empty state deserves more warmth

`ChatListScreen.kt:162-176` is a bare "No conversations yet" + button. iOS already has
an icon + explanatory subtitle. Match and exceed: icon or lightweight illustration,
one sentence of value ("Message family and friends over Bluetooth — no Wi-Fi needed"),
primary "Add a friend" button, secondary "Show my friend card". First-run is where
polish is judged most.

### 🟡 2.5 My friend card screen needs structure

`FriendingScreens.kt:72-162` (`MyQrScreen`):

- No `TopAppBar`; navigation is a bottom-center "Back" `Button` — inconsistent with
  every other screen. Give it a top bar with back arrow and title "My friend card".
- Relay URL / relay token fields are advanced configuration sitting *above the QR code*
  in a screen whose job is "let a friend scan this". Move relay config to Profile
  (where iOS keeps it) or behind an "Advanced" expander; also, the relay token is a
  secret and iOS treats it as a `SecureField` — Android shows it in plaintext.
- Put the QR on a white rounded card (as iOS does at `FriendsView.swift:154`) so it
  stays scannable in dark mode, and show the fingerprint words under it for verification
  parity with iOS.
- "Share card as text" + "Copy" as two side-by-side buttons could be one primary
  Share button + icon button.

### 🟡 2.6 QR scan screen: add a viewfinder and scrim

`FriendingScreens.kt:166-252`: status text renders directly over the live camera feed
with no scrim (white text over a bright scene will vanish), and there's no scan-frame
overlay to tell users where to aim. Add a dimmed scrim with a rounded cutout, put the
status text on a pill, and consider a torch toggle (ship corridors are dark).

### 🟡 2.7 Onboarding: lead with the message, not the step counter

`OnboardingScreen.kt:142-151` renders "Step 1 of 4" in `headlineSmall` — the largest
text on screen — while the actual slide title lives inside the card. Swap the hierarchy:
the slide title should be the headline; the step indicator is already communicated by
the progress dots (which are nicely done). Also consider a "Skip" affordance on the
info slides — the permissions slide is the only one that truly needs a stop.

### 🟢 2.8 Deprecated back icon

`Icons.Default.ArrowBack` (used in `ChatScreen.kt`, `ProfileScreen.kt`,
`FriendingScreens.kt`, `NewGroupScreen.kt`) is deprecated and doesn't mirror in RTL.
Switch to `Icons.AutoMirrored.Filled.ArrowBack`.

### 🟢 2.9 Profile screen: group into cards

`ProfileScreen.kt` is one long centered column where "Account backup", "Device identity",
"Mesh Status", "Relay Presence", "Legal" all run together. Wrap each section in a
`Card`/`Surface` with a consistent section-header style (or adopt a settings-list
pattern). Also: printing the raw privacy-policy URL under the button (`:264-269`)
is redundant — the button is enough.

### 🟢 2.10 New group: show selection count

`NewGroupScreen.kt:137-148` — "Create group" gives no feedback about how many members
are selected. "Create group (3)" or a selected-avatars strip above the button confirms
the action before commit.

### 🟢 2.11 Tick legend via AlertDialog

Tapping your own bubble opens a full `AlertDialog` to explain ✓/✓✓
(`ChatScreen.kt:1014-1025`). A snackbar or small anchored tooltip would be lighter;
save dialogs for decisions.

---

## 3. iOS-specific

The iOS shell is functionally close to Android but visually a generation behind it
in the chat thread. If you only polish one platform surface, make it this one.

### 🔴 3.1 Composer parity

`ChatView.swift:96-127`: a `.roundedBorder` TextField plus a *text* "Send" button,
with photos/camera/voice buried in a "+" menu. Compare Android's Signal-style composer
(circular + button, pill input with inline camera, mic that morphs into a send icon).
Recommendations:

- Icon send button (`arrow.up.circle.fill`), enabled state tied to non-empty draft.
- Capsule-style input field on a `.bar` background rather than the bordered text field.
- Photo flow: iOS sends the photo immediately on pick with no preview and no caption
  (`ChatView.swift:170-190`); Android stages the photo above the composer so users can
  add a caption and confirm. Port the pending-photo card — sending a photo sight-unseen
  is the kind of thing users remember as "the app feels cheap".
- Voice: the modal "Start recording / Stop and send" sheet works, but hold-to-record
  on a mic button (matching Android) is faster and more familiar.

### 🔴 3.2 Message list identity + bubble polish

- `ChatView.swift:46` — `ForEach(Array(visible.enumerated()), id: \.offset)` uses the
  index as identity. Every reload re-identifies every row: no insert animations, wasted
  re-renders, and scroll glitches at scale. Use the stable key Android uses
  (`sender hash + lamport`) — you already compute `.id(message.lamport)` for scrolling,
  which collides across senders anyway.
- No bubble grouping: Android joins consecutive bubbles with 6dp corners and shows
  timestamps once per cluster; iOS shows a fixed-radius bubble *and a timestamp under
  every single message* (`ChatView.swift:430-432`), which makes threads noisy. Port
  `bubbleGroupingFor` (it's pure logic, already shared conceptually) to iOS.
- Reactions/actions: long-press *inserts* the reaction bar and action panel inline
  into the list (`ChatView.swift:373-424`), shoving messages around. Use the native
  `.contextMenu` (you already use it for image save) or replicate Android's focus
  overlay (documented in MESSAGE_LONGPRESS_OVERLAY.md) so layout never jumps.

### 🔴 3.3 No onboarding on iOS

`CruiseMeshApp.swift` drops straight into `ChatListView`. Android has a 4-page
onboarding covering the mesh concept, permissions, and profile setup — on iOS the
Bluetooth permission prompt will just appear naked, with no explanation, and the user
never gets asked their display name up front. Port the onboarding (ONBOARDING_SCRIPT.md
exists precisely for this), including the restore-from-backup entry point.

### 🟡 3.4 Reachability indicators are missing entirely

Android shows per-contact reachability dots on avatars and status text in the chat
header ("Nearby via Bluetooth"). iOS `AvatarView` has no reachability parameter and
the chat header shows a static "Contact details" caption (`ChatView.swift:149-151`).
This is the app's differentiating feature — CONNECTIVITY_INDICATOR.md §3.1 defines the
palette; bring iOS to parity.

### 🟡 3.5 Floating compose button is a non-native pattern that duplicates the toolbar

`ChatListView.swift:167-181` overlays a hand-rolled FAB (a Material pattern) whose menu
("New message" / "New group") mostly duplicates the toolbar ⋮ menu ("New group" /
"Friends" / "Mesh status"). On iOS, drop the FAB and use a single
`square.and.pencil` toolbar button — or keep the FAB on both platforms as a deliberate
brand choice, but then make the two menus consistent.

### 🟡 3.6 Mesh controls hidden in an alert

The "Mesh" alert (`ChatListView.swift:197-203`) with "Start mesh" / destructive
"Stop mesh" / "OK" is doing sheet work in an alert body, and stopping the mesh isn't
destructive. Replace with a small sheet: current state + dot legend (see 1.7), a
Start/Stop toggle, and the "open the app when you sit down with family" tip formatted
as a callout instead of being concatenated into the alert message.

### 🟢 3.7 Backup/restore missing from iOS Profile

Android Profile has "Back up account"; iOS `ProfileView` has none, and LOCAL_BACKUP_RESTORE.md
implies parity is intended. Even a placeholder section ("Coming soon") is better than
users discovering the asymmetry after switching phones.

---

## 4. Suggested priority order

| # | Item | Effort | Payoff |
|---|------|--------|--------|
| 1 | iOS composer + photo caption/preview parity (3.1) | M | Biggest perceived-quality gap |
| 2 | Group avatars instead of 👥 (1.1) | S | Both list screens instantly look designed |
| 3 | Remove "Lamport" from Message Info (1.2) | XS | Stops leaking internals |
| 4 | iOS message identity + bubble grouping (3.2) | M | Smooth thread, fewer redraws |
| 5 | iOS onboarding (3.3) | L | First-run experience & permission grant rate |
| 6 | Android chat-list swipe/delete affordance (2.1) + menu cleanup (2.2) | S | Discoverability |
| 7 | Mesh pill pulse + send haptics/motion (1.6) | S | "Alive" feel for the hero feature |
| 8 | My friend card screen restructure (2.5) | M | The screen every new user hits |
| 9 | Copy casing pass + empty states with personality (1.4, 1.8, 2.4) | S | Cheap charm |
| 10 | Locale-aware dates + string extraction (1.3) | M | Pays compound interest |

*Effort: XS < S < M < L*
