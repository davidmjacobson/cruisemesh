# UI TODOs — agent-ready work items

Audit date: 2026-07-17, on the Rust-migration baseline (`claude/rust-migration-audit-todos-a7lly6`
lineage). Supersedes the actionable half of `UI_UX_REVIEW.md` (July 2026): that review's
items were re-verified against the current tree and most shipped — see §1.2 for what is
already done so nobody re-proposes it.

**Product bar:** CruiseMesh differentiates by being *very easy to use* on top of a
powerful, robust mesh. Every screen a family member touches must be obvious;
everything an engineer or power user needs (relay config, LAN tools, diagnostics)
must exist but live behind Advanced. When a change forces a choice between
capability on the surface and simplicity on the surface, pick simplicity and move
the capability behind one more tap.

## 0. Completion record

Implementation status: **U1-U13 complete** on the merged UI wave plus the
`agent/ui-todos-completion` follow-up. The follow-up closes the audit gaps that
were not actually end-to-end in the first wave: group attachment transport,
group rename/add-member convergence, and localization resources/CI enforcement.

| Item | Status | Shipped outcome |
|---|---|---|
| U1 | ✅ Complete | Simple settings surface with the LAN/relay/debug controls under Advanced |
| U2 | ✅ Complete | iOS reachability dots, status copy, details, group summary, and decay clock |
| U3 | ✅ Complete | iOS Files/share-sheet backup and restore with Android-compatible fixtures |
| U4 | ✅ Complete | Group photo/voice composition plus attachment encryption, receive, notification, and fan-out support |
| U5 | ✅ Complete | Expiry-aware delivery status and truthful message-info copy |
| U6 | ✅ Complete | Actionable notifications and local per-chat mute state |
| U7 | ✅ Complete | Conflict-safe group rename, add members, upgraded member rows, and kind-19 protocol spec |
| U8 | ✅ Complete | Persistent drafts, list actions/animation, group navigation, and send haptics |
| U9 | ✅ Complete | iOS hold-to-record voice memo flow |
| U10 | ✅ Complete | Structured message-info sheets and compact tick help |
| U11 | ✅ Complete | Android resources/plurals, iOS string catalog, and a hardcoded-string CI guard |
| U12 | ✅ Complete | Onboarding restore and small-screen hierarchy polish |
| U13 | ✅ Complete | Destructive styling, empty-state, casing, and Legal cleanup |

Validation evidence for the completion follow-up: 254 Rust core tests and 8 mesh
simulations pass; the Android clean debug unit-test task passes; a debug APK with
arm64-v8a, armeabi-v7a, and x86_64 native cores assembles successfully; and the
localization guard passes. iOS simulator validation is performed on the configured
macOS validation host before release publishing.

---

## 1. Current state

### 1.1 Screen inventory

| Surface | Android | iOS |
|---|---|---|
| Onboarding (4 slides, restore entry) | `ui/OnboardingScreen.kt` | `UI/OnboardingView.swift` (restore = stub alert) |
| Home / chat list | `ui/ChatListScreen.kt` | `UI/ChatListView.swift` |
| 1:1 chat | `chat/ChatScreen.kt` | `UI/ChatView.swift` |
| Group chat | `chat/GroupChatScreen.kt` | `UI/GroupChatView.swift` |
| Friending (card, scan, add, contacts) | `friending/FriendingScreens.kt` | `UI/FriendsView.swift`, `Friending/QRScannerView.swift` |
| New group | `ui/NewGroupScreen.kt` | `UI/NewGroupView.swift` |
| Profile & settings | `ui/ProfileScreen.kt` | `UI/ProfileView.swift` |
| Backup / restore | `identity/backup/BackupScreens.kt` | **missing** |
| Mesh status pill + legend | `ui/MeshStatusPill.kt` | `UI/MeshStatusPill.swift`, sheet in `ChatListView.swift` |
| Contact details | `ui/ContactDetailsSheet.kt` | `UI/ContactDetailsSheet.swift` |

### 1.2 Already done — do NOT re-propose

Verified in-tree 2026-07-17 (most came from the merged `claude/ui-ux-improvements-b7dmwo`
pass and later PRs):

- Group avatars (no more 👥-in-text), `AvatarBadge`/`AvatarView` with `isGroup`.
- "Lamport" removed from Message info (pinned by `MessageInfoTextTest.kt`).
- Unread badge caps at `99+` on both platforms; scale-in animation on Android.
- iOS composer parity: capsule field, icon send with disabled state, pending-photo
  card with caption, camera/library/voice under "+" menu.
- iOS bubble grouping + stable row identity (`stableRowId`), day separators,
  `.contextMenu` for react/reply/copy/info.
- iOS onboarding (4 slides matching `ONBOARDING_SCRIPT.md`).
- Locale-aware date/time formatting in chat UI on both platforms.
- Mesh status pill: pulse animation while starting/meshing (Android), tappable
  legend (Android dialog, iOS sheet with Start/Stop toggle).
- QR scan viewfinder with scrim + haptic on success; frozen-frame confirmation.
- My friend card: top bar, white QR card, fingerprint words, share/copy, relay
  fields behind an "Advanced relay settings" expander (Android).
- Chat-list empty state with personality; long-press → menu → delete (Android),
  swipe-to-delete (iOS); delete confirmations everywhere.
- Message info shows arrival route + hops ("Arrived via relay · ~2 hops").
- Friend confirmation sheets with delivery outcome; friends-of-friends suggestions
  with "Add all" confirm.

### 1.3 Fresh findings (what the rest of this file fixes)

- **The Profile screen is the opposite of the product bar.** Both platforms dump a
  "Local Wi-Fi (experimental)" diagnostics lab — frame counters, probe status, last
  peer endpoint, subnet scan, manual IP entry — into main settings
  (`ProfileScreen.kt:253-399`, `ProfileView.swift:83-149`). iOS additionally shows
  raw Relay URL/token fields as a top-level section (`ProfileView.swift:67-75`).
- **iOS settings parity holes:** no Account backup section, no "Start automatically",
  no "Relay presence / share when I'm online" toggle; profile edits require an
  explicit Save (Android saves live); privacy-policy URL printed redundantly
  under its button.
- **Reachability — the app's differentiating feature — does not exist on iOS.**
  No dots on chat-list avatars, static "Contact details" caption in the chat header
  (`ChatView.swift:193-195`), no connectivity card in contact details, group header
  shows "N members" instead of Android's "N of M reachable".
- **Backup/restore is Android-only**; iOS onboarding restore is a dead-end alert.
- **Group chats are text-only on both platforms** — no photos, no voice memos —
  while 1:1 has both. Groups also have no ✓/✓✓ (blocked on DTN D9 wire receipts).
- **No group management:** can't rename, can't add members after creation; member
  list is text bullets (`GroupChatScreen.kt:303-310`); no reachability shown per member.
- **Messages that can't deliver look "sent" forever** — a single tick with no
  explanation, no countdown, no expired state (= DTN_TODOS D5).
- **Notifications are bare:** Android uses plain notifications (no MessagingStyle,
  no avatar, no inline reply, no mark-as-read); no per-chat mute on either platform.
- Quality-of-life absences: no draft persistence, no chat-list reorder animation
  (Android), no "Mark as read" action, iOS voice memo is a modal sheet instead of
  hold-to-record, iOS `NewGroupView` doesn't open the group after creating it,
  `ContactDetailsSheet` (Android) styles "Delete contact" as a primary button.
- All user-facing strings are hardcoded literals on both platforms (translation
  impossible, plurals hand-rolled).

---

## 2. Ground rules for agents

Same regime as `DTN_TODOS.md` §2, plus UI-specific rules:

1. **Base branch:** the current DTN stack tip (ask the orchestrator which branch is
   the integration head that day). Never base on raw `master`/`68abbe1`.
2. **Worktree per task** (`git worktree add`); Android tests need
   `cargo build -p cruisemesh-core --features cruisemesh-core/cli` + uniffi
   kotlin-gen regeneration if you touch core (see AGENTS.md). iOS must build and
   pass tests on the Mac validation host before a PR is opened.
3. **Both platforms or say why not.** Every U-item lands Android + iOS together
   unless the item explicitly says one platform. A single-platform PR must state
   the parity plan in its description.
4. **Pure logic goes in the Rust core** when it's a decision (reachability compute,
   copy for statuses, badge text) — the migration exists so iOS gets fixes for free.
   Presentation stays in Compose/SwiftUI.
5. **Tests required:** unit tests for every pure function you add or move;
   Compose `@Preview`s (light + dark) for new Android surfaces; existing test
   suites must stay green (`gradlew cleanTestDebugUnitTest testDebugUnitTest`,
   `xcodebuild test`, `cargo test`). Paste test output in the PR.
6. **Copy rules:** sentence case everywhere ("Mesh status", "Delete contact");
   status/error/blocking copy stays strictly literal — personality is allowed only
   in empty states and low-frequency delight moments; never invent protocol
   jargon in user-facing text.
7. **Don't remove capability, relocate it.** LAN tools, relay config, and debug
   affordances are load-bearing for development and power users. "Hide" means
   "behind Advanced", never "delete".
8. **No new dependencies** without orchestrator approval.
9. **Don't touch DTN decision logic** (engine.rs rules, ack invariants) — if a UI
   item needs new core data (e.g. expiry timestamps), add read-only accessors.

---

## 3. Recorded decisions (do not re-litigate)

- **D-UI-1 Settings information architecture** (see U1): main settings shows only
  You · My friend card · Backup · Mesh · Privacy · Advanced · Legal. Everything
  currently in "Local Wi-Fi (experimental)" plus relay URL/token plus debug log
  moves under Advanced. Main settings may show a one-line transport health summary.
- **D-UI-2 iOS saves live**, like Android. The Save button pattern in
  `ProfileView.swift` goes away; sync side-effects (profile sync, friend directory)
  fire on change with the same debounce/back-navigation semantics Android uses.
- **D-UI-3 Reachability palette and levels are fixed** by CONNECTIVITY_INDICATOR.md.
  iOS implements the same five levels, same colors, same shape-coding
  (hollow ring = carried). No redesign.
- **D-UI-4 Group member removal is out of scope** until group key rotation exists
  (removing a member without rotating the shared key is security theater).
  Rename + add-member are in scope (added members get the existing key via invite,
  which matches the current trust model).
- **D-UI-5 Muting is local-only** state (no wire protocol); it silences
  notifications but never stops sync or carry.
- **D-UI-6 Attachment size caps don't change** for groups; the D6 fan-out storage
  multiplier was accepted knowingly (specs/group-relay-durability.md §7).

---

## 4. Work items

Each item = one agent task = one PR. 🔴 core to the product bar · 🟡 strong · 🟢 polish.

### U1 🔴 Settings restructure: simple surface, Advanced drawer (both platforms)

The flagship item — it *is* the product-bar statement.

- Restructure `ProfileScreen.kt` and `ProfileView.swift` to D-UI-1's IA.
- New "Advanced" destination (Android: nav route; iOS: `NavigationLink` screen)
  containing: Local Wi-Fi tools (everything from the current experimental section,
  unchanged in behavior), relay URL/token config (iOS — Android's lives in the
  friend-card expander; move it here too so both platforms have exactly one home
  for it, and keep the friend-card expander as a shortcut or drop it, your call,
  stated in the PR), and the debug-log share (Android, debug builds).
- Main "Mesh" section: status line + Start button (Android) / running toggle (iOS),
  "Start automatically" switch (add to iOS — check what iOS boot semantics allow;
  if none, hide the switch there and say so), "Share when I'm online" relay-presence
  toggle (add to iOS, wired to `RelayConfigStore`).
- iOS: delete the Save button (D-UI-2), remove the printed privacy URL, add the
  Account backup section as a stub that opens U3's flow (or a "coming soon" row
  until U3 lands — coordinate).
- Acceptance: a first-time user scrolling main settings sees zero IP addresses,
  zero tokens, zero frame counters; every removed control is reachable within
  two taps via Advanced; all existing LAN-tool behaviors still work from there.

### U2 🔴 iOS reachability parity (the differentiating feature)

- Port the reachability computation to iOS. Strong preference: move
  `ContactReachability.compute` (+ header/details copy builders in
  `MeshStatusTextLogic`/`ContactReachability`) into the Rust core with UniFFI
  exports, then have *both* platforms call core (ground rule 4). If core export
  is blocked, a faithful Swift port with the same unit-test vectors is acceptable —
  state which in the PR.
- iOS `MeshConnectivityStatus` equivalent: nearby peer IDs, relay health,
  contact/presence last-seen feeds already exist in `MeshController` — surface
  them as an observable snapshot.
- Apply to: `AvatarView` (dot badge param like Android's `AvatarBadge`), chat-list
  rows, 1:1 chat header status line ("Nearby via Bluetooth" replacing the static
  "Contact details" caption), `ContactDetailsSheet.swift` connectivity card,
  group header "N of M reachable".
- Include the 30 s foreground clock tick (`rememberConnectivityNowMs` pattern from
  `MainActivity.kt:401-427`) so levels decay without events.
- Tests: reuse Android's `ContactReachabilityTest` vectors.

### U3 🔴 iOS local backup & restore (closes the platform trap)

- Port `identity/backup/` (BackupCrypto, BackupFormat, BackupPassphrase,
  BackupService, screens) to iOS per LOCAL_BACKUP_RESTORE.md. Same `.cmbak`
  format — a backup written on Android must restore on iOS and vice versa;
  add a cross-platform fixture test (encrypt on one, decrypt via shared core/test
  vector on the other).
- Wire the existing onboarding "Restore from backup" button (currently a stub
  alert, `OnboardingView.swift:103-107`) and the Profile backup section from U1.
- Files app / share-sheet based export & import.
- Note: core `backup_to` hardening is still an open TODO — coordinate with the
  orchestrator; don't silently depend on unhardened behavior.

### U4 🔴 Group attachments: photos + voice (both platforms)

- 1:1 already sends `AttachmentPayload` (photo with caption, voice memo); groups
  are text-only. Add attachment support to `GroupSender` (Kotlin + Swift) and the
  group composers, reusing the 1:1 pending-photo card and voice UX verbatim.
- Same size caps (D-UI-6). Verify the attachment kind flows through group
  encryption and the D6 relay fan-out path (an engine/store test that a group
  attachment envelope fans out like a text one).
- Acceptance: photo with caption and voice memo send/receive in a 3-member group
  across two real devices (orchestrator will field-test; provide the build).

### U5 🔴 Truthful delivery: undelivered/expiring message UI (= DTN_TODOS D5)

- Today an undeliverable message shows one gray tick forever. Surface what the
  DTN layer knows:
  - Message info: "Still trying — expires in 2 days" (from envelope expiry;
    needs a read-only core accessor for outbound-envelope expiry by message).
  - After expiry with no delivery receipt: a subtle inline state on the bubble
    (e.g. hollow/struck tick + "Not delivered — expired" in info), never a loud
    error banner.
  - Chat-list preview unaffected.
- Copy must be literal (ground rule 6). Coordinate with DTN_TODOS D5 — this item
  is its UI half; land as one PR if the same agent takes both.

### U6 🟡 Notifications that behave like a messenger (both platforms)

- Android `MessageNotifier`: `MessagingStyle` with `Person` + avatar icon,
  conversation grouping, inline reply via `RemoteInput` (sends through the normal
  sender path), "Mark as read" action (advances the same read watermark the chat
  screen does).
- iOS: notification category with a reply text action; mark-as-read on tap-through
  already works — verify and pin.
- Per-chat mute (D-UI-5): flag in local store (core store preferred so both
  platforms share it), toggle in chat header menu / contact & group details sheets,
  muted chats show a small muted glyph in the list row and skip notification
  posting only.

### U7 🟡 Group management v1 (design-first, like D6 was)

Write a short spec in `specs/` first, get orchestrator sign-off, then implement:

- Rename group (broadcasts an updated invite/name to members — check what the
  invite kind already tolerates; if a name-update needs a new protocol kind, the
  spec must say so and get sign-off).
- Add members post-creation (existing key via invite, D-UI-4; UI mirrors
  NewGroup's member picker).
- Group details sheet upgrade on both platforms: member rows with avatars +
  reachability dots (U2 dependency on iOS), "You" row, added-by context if cheap.
- Explicit non-goals in the spec: member removal, admin roles, key rotation.

### U8 🟡 Chat-list and composer quality-of-life (both platforms)

- Draft persistence per chat (in-memory + disk survives process death; restore
  into composer; show "Draft: …" preview in the list row like every messenger).
- Android: `animateItem()` on chat-list rows so reorders glide; add "Mark as read"
  to the row long-press menu (menu currently has only Delete,
  `ChatListScreen.kt:229-241`); optionally swipe actions to match iOS.
- iOS: after `NewGroupView` creates a group, navigate into it (Android does;
  iOS just dismisses); show member count on the Create button.
- Send haptic on Android to match iOS's (`UIImpactFeedbackGenerator` on send
  exists; Android has none on send).

### U9 🟡 iOS voice memo: hold-to-record parity

- Replace the modal "Start recording / Stop and send" sheet with Android's
  pattern: mic icon in the composer when the draft is empty, hold to record with
  a slide-to-cancel affordance, release to send. Keep the 60 s cap and the
  too-large guard. Keep the sheet as the accessibility fallback if hold gestures
  are a VoiceOver problem (state what you did).

### U10 🟢 Message info & tick legend presentation

- Both platforms show Message info in an alert with `\n`-joined text; upgrade to a
  bottom sheet (Android `ModalBottomSheet`, iOS `.sheet` + detents) with labeled
  rows: direction, time, status, route, hops, expiry (U5). Tick-legend tap
  becomes a compact tooltip/snackbar, not an alert.

### U11 🟡 Localization groundwork

- Android: extract all user-facing literals to `strings.xml` with plurals;
  iOS: `Localizable.xcstrings`. No behavior change, no actual translations yet.
- Add a lint/CI grep that fails on new hardcoded quoted literals inside Text()
  composables/views (pragmatic heuristic is fine; document escapes).
- Do this AFTER the copy-touching items above (U1, U5, U6) so strings only get
  extracted once — last in wave order on purpose.

### U12 🟢 Onboarding & small-screen polish

- Android: make the slide title the headline; demote "Step N of 4" to a caption
  (progress dots already carry it) — `OnboardingScreen.kt:142-151`.
- iOS: when U3 lands, onboarding restore goes live (tracked there; verify here).
- Both: verify all four slides on a small phone (SE-class) with large dynamic
  type — the permission cards overflow risk is untested.

### U13 🟢 Detail polish sweep (single PR)

- `ContactDetailsSheet.kt`: "Delete contact" → destructive styling (error color /
  `role: .destructive` parity with iOS alerts).
- Contacts screen "No contacts yet" → match the chat-list empty-state treatment.
- Casing audit pass over remaining Title Case strays.
- iOS Legal section: drop the printed URL (covered by U1 if done there — check).

---

## 5. Suggested wave order

| Wave | Items | Why this order |
|---|---|---|
| 1 | U1, U13 | The product-bar statement; small blast radius; unblocks nothing but sets the IA every later item slots into |
| 2 | U2, U9 | iOS parity on the differentiator; U2 unblocks U7's member rows |
| 3 | U4, U5 | Feature gaps users hit daily; U5 pairs with DTN D5 |
| 4 | U6, U8 | Messenger table-stakes behaviors |
| 5 | U3, U7 | Largest items; U7 is design-first |
| 6 | U10, U12, U11 | Polish, then extract strings exactly once |

Hand an agent: the U-item verbatim, §2 ground rules, §3 decisions, and the
"already done" list (§1.2). Require pasted test output. Review diffs against §3
and the product bar before merging.
