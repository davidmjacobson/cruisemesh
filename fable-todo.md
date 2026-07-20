# fable-todo.md — fresh audit backlog

Written 2026-07-20 from a four-agent audit (Rust core, Android, iOS,
relayd/infra/docs) of master @ `1910158`. This file contains **only new
findings** — nothing here duplicates `TODO.md` (T1–T17, D8/D9, L3/V1/V2, the
T4 series) or `AGENT-TODO.md`; those stay the plan of record for the existing
backlog. Every item below was verified against the code with file:line
evidence at audit time.

IDs: `XP` = cross-platform, `FA` = Android, `FI` = iOS, `FC` = Rust core,
`FR` = relayd/infra/docs. Severity: 🔴 field-impacting bug or missing safety
net · 🟡 real cost, not urgent · 🟢 hygiene/polish.

---

## 0. How to work this file (read before writing code)

This file is written to be executed by any capable agent without access to
the audit conversation. Rules:

1. **`TODO.md` §3 ground rules apply to every item**: worktree per task
   (`git worktree add ../CruiseMesh-<slug>`; copy `kotlin-gen/` + `jniLibs/`
   from the main checkout and `cargo build` in the worktree for JVM unit
   tests), commit author email
   `14227840+davidmjacobson@users.noreply.github.com`, one branch per item
   (`agent/<slug>`), core-first for shared behavior, strings in resources,
   sentence-case literal copy.
2. **Re-verify before coding.** Line numbers were correct at `1910158` but
   drift; open the cited file, confirm the defect is still there and still
   unfixed, then work from the code, not from this description. If the claim
   no longer holds, mark the item ✅/❌ here with a note instead of forcing a
   fix.
3. **Test commands**: core `cargo test -p cruisemesh-core`; workspace
   `cargo test --workspace`; Android
   `cd android && ./gradlew :app:testDebugUnitTest --rerun-tasks`. iOS
   builds/tests run on the Mac validation host — **back up as of
   2026-07-20** (see `AGENTS.md` for the runbook). Items marked *(Mac)*
   must be validated there before merging; only fall back to
   "iOS suite pending Mac" in the PR if the host is unreachable again.
4. **If a core change touches the UniFFI surface**, regenerate bindings
   (see `AGENTS.md`) — never hand-edit `kotlin-gen/` or the Swift bindings.
5. **relayd items are safe to land in the repo but NOT to deploy.** The live
   relay serves the family fleet; deploying needs David's explicit
   go-ahead (same rule that deferred relay-presence Phase 2).
6. **Conflict groups — don't parallelize within a group:**
   - `ChatScreen.kt`/`GroupChatScreen.kt`/`ChatView.swift`/`GroupChatView.swift`:
     XP1, FA4, FA7, FA10, FA11, FI8, FI12 (and TODO.md's T1/T8 if still open).
   - `MeshService.kt`: FA2 (calls into it), FA3, FA5, FA8, FA15.
   - `MeshController.swift`: FI1, FI2, FI4, FI5, FI6.
   - `core/src/store.rs`: FC1, FC3, FC4, FC7, FC9, FC10.
   - relayd `lib.rs`/`main.rs`: FR2, FR3, FR4, FR6, FR7, FR8.
7. **When an item ships**, tick it here (`✅ merged <PR/commit>`), don't
   delete it — this file doubles as the audit record.

### Suggested order

| Wave | Items | Why |
|---|---|---|
| A — stop the bleeding | XP1, FI1, FA1, FA2 | Active field bugs: keystroke reload storm (both platforms), false read receipts while locked, mesh broken on Android 8–11, BLE write races |
| B — safety nets | FR1, FR5, FR12, FR2+FR3+FR4 (one relayd PR) | CI that actually runs tests; observable relay; committed plan-of-record |
| C — cruise-data correctness | FC1, FC4, FI2, FI3, FI5 | The field test must produce trustworthy metrics and symmetric delivery |
| D — perf + robustness | FC2, FA3, FA4, FA5, FA6, FI4 (incremental), FI6–FI9, FR6, FR7 | Main-thread and hot-path costs that grow with a cruise week of history |
| E — hygiene/polish | everything 🟢 + docs (FR10, FR11, FR13–FR15, XP3) | Batchable; several are XS |

Decisions David must make before certain items start are collected in §7.

---

## 1. Cross-platform

### XP1 🔴 Every keystroke triggers a full chat reload (and on iOS, a read receipt + relay sync) — both platforms, same root cause

The draft autosave fires the chat-changed event, and the chat screen's own
listener treats it like a new message.

- **Android chain:** `ChatScreen.kt:328-330` `LaunchedEffect(draft)` saves per
  keystroke → `chat/DraftStore.kt:16` calls
  `ChatEvents.notifyChatChanged(chatId)` → ChatScreen's collector
  (`ChatScreen.kt:320-326`) calls `reload()` (`:217-224`) = full
  `store.messagesForChat` **including inline attachment blobs**, avatar, and
  three receipt queries, on the main thread, per character. Identical loop in
  `GroupChatScreen.kt:227-237`. If the chat list is visible, the same event
  drives `HomeRoute.reloadSummaries()` (`MainActivity.kt:642-687`) which loads
  *every chat's full history* to find the last message.
- **iOS chain (worse):** `Chat/DraftStore.swift:17` fires on save ←
  `UI/ChatView.swift:271` `.onChange(of: draft)` → own sink (`:251-253`) →
  `reload()` (`:413-433`) which also calls
  `MeshController.notifyChatViewed` (`Mesh/MeshController.swift:296-325`) —
  that **authors and sends a read-receipt frame over the mesh and kicks
  `RelaySyncEvents.requestSync()`** per keystroke. `ChatListView.swift:209`
  rebuilds every chat summary per event, loading full histories
  (`ChatListView.swift:253-311`).
- **Cost:** typing "hello there" = 11× (full-thread DB read + all-chats scan
  + on iOS a receipt frame on BLE/LAN + a relay sync attempt). Jank, battery,
  and duplicate receipt traffic on the flagship screens.
- **Fix (surgical, per platform):**
  (a) stop firing the chat-changed event from `DraftStore.save` on both
  platforms — the chat list only needs draft presence; either give drafts
  their own lightweight signal or rely on the existing reload-on-navigation
  (`MainActivity.kt:690-702` already covers Android home navigation);
  (b) iOS: remove `notifyChatViewed` from `ChatView.reload()` — keep it in
  `onAppear` and in the incoming-message path (which already fires it);
  (c) both: add a store accessor for "last visible message per chat" so
  summary reloads never load full histories (this sub-part is core-first —
  one query, exported via UniFFI, both shells adopt).
- **Accept:** typing produces zero `messagesForChat` calls and (iOS) zero
  receipt sends/sync kicks — assert via a store-call counter in a unit test;
  drafts still appear in the chat list; message/receipt arrival still
  refreshes the open thread. *(Mac for iOS validation.)*
- **Effort:** S for (a)+(b); +S for (c). Do (a)+(b) first; (c) can follow.

### XP2 🟡 Localization CI gate only sees `Text("…")` — large classes of user-facing English bypass `strings.xml`/the string catalog

- **Evidence:** `tools/check_ui_localization.py:16-18` matches only `Text(`.
  Android slip-throughs: 17 hardcoded Toasts (`ChatScreen.kt:228,302,310-313`,
  `GroupChatScreen.kt:145-215`, `MainActivity.kt:292-303,613-617`), 28
  hardcoded `contentDescription`s (12 files), snackbar copy
  (`GroupChatScreen.kt:139`), the entire foreground-service notification
  ("Relaying messages nearby", channel name — `MeshService.kt:3506-3547`),
  message-notification strings (`MessageNotifier.kt:95,190-199,224-231`), and
  sentence-assembling helpers (`ChatScreen.kt:1457-1541`,
  `MainActivity.kt:767-787`). iOS slip-throughs: user-facing copy assigned to
  `String` state rendered via `Text(variable)` — `UI/ChatView.swift:266`,
  `Mesh/LanTransport.swift:147-153`,
  `Mesh/LanTransportDiagnostics.swift:100-228`,
  `Mesh/MeshConnectivityStatus.swift:49-94`, `Mesh/MeshRuntimeStatus.swift:24-32`.
- **Cost:** the copy-review pipeline (T5) misses these strings; TalkBack/
  VoiceOver get unreviewed phrasing; the §3.7 ground rule is structurally
  dodged for exactly the status/error copy class it names.
- **Fix:** first extend the checker (patterns for `Toast.makeText(ctx, "`,
  `contentDescription = "`, `showSnackbar("`, `setContentTitle/Text("`, and a
  policy for Swift `String`-typed user-facing assignments — see §7 decision),
  then migrate mechanically.
- **Accept:** checker fails on a seeded literal in each category; both apps
  build with zero violations.
- **Effort:** checker XS; migration M.

---

## 2. Android

### FA1 🔴 Mesh is broken on Android 8–11: legacy Bluetooth permissions are missing entirely

- **Evidence:** `android/app/src/main/AndroidManifest.xml:10-14` declares only
  API-31 permissions (`BLUETOOTH_SCAN/ADVERTISE/CONNECT`); there is no
  `BLUETOOTH`, `BLUETOOTH_ADMIN`, or `ACCESS_FINE_LOCATION` with
  `maxSdkVersion="30"` anywhere, yet `minSdk = 26`
  (`android/app/build.gradle.kts:32`). `MeshService.hasRequiredPermissions()`
  returns an empty list below S (`MeshService.kt:3489-3502`) so the service
  starts, then `BleCentral.start()` calls `startScan` unguarded
  (`BleCentral.kt:327`) and `BlePeripheral` advertises — on API 26–30 that is
  a `SecurityException` (permission not declared) or a silent no-result scan.
- **Cost:** any Android 8–11 device crashes or silently never joins the mesh;
  onboarding asks for nothing on those devices.
- **Fix:** decision required (§7): either raise `minSdk` to 31 and delete the
  dead `SDK_INT < S` branches (**recommended** — the test fleet is modern,
  and it's honest about what's actually supported), or add the legacy trio
  with `maxSdkVersion="30"` plus a runtime location request and
  location-services check.
- **Accept:** minSdk path — build installs refuse API < 31 and no
  `SDK_INT < S` dead code remains; legacy path — an API-28 emulator scans and
  advertises without SecurityException.
- **Effort:** XS (minSdk) / S (legacy support).

### FA2 🔴 `BleCentral`'s shared state is completely unsynchronized — the exact race `BlePeripheral` was already hardened against

- **Evidence:** `mesh/BleCentral.kt:90-96` — `connections`, `negotiatedMtu`,
  `reassemblers`, `writeQueues`, `writeInFlight`, `fullyConnected`,
  `scanDiagnostics` are plain `mutableMapOf`/`mutableSetOf`, mutated from GATT
  binder threads (`onMtuChanged`:209, `onCharacteristicWrite`:267-296,
  `tearDownLink`:454-466), the main thread (scan callback:138-162,
  watchdog:420-435), and any caller thread via `sendFrame` (356-370) —
  MeshRouter dispatches into it from peripheral binder threads, LAN reader
  threads, and the relay-sync thread. The check-then-act on `writeInFlight`
  (`sendNextQueuedFragment`:374) is precisely the field-observed race
  documented and fixed with a `lock` in `BlePeripheral.kt:141-153`
  ("onNotificationSent for ONE address delivered on two different binder
  threads"); BleCentral never got the same fix.
- **Cost:** two concurrent in-flight GATT writes (violates the one-op
  constraint → status 133/129 churn), lost queue entries, or HashMap
  corruption/CME crash — worst during the busy on-HELLO spray.
- **Fix:** mirror BlePeripheral: one `lock` guarding all per-address maps,
  taken by `sendFrame`, all callbacks, the watchdog, `stop()`, and
  `tearDownLink`. Extract the queue/in-flight decision as a pure class with
  unit tests (the `BlePeripheralFramePacingTest` pattern) — BleCentral
  currently has **zero** tests despite being the historically buggier half.
- **Accept:** every read-modify-write of those collections is under the lock;
  a unit test on the extracted admission logic covers the in-flight gate.
- **Effort:** M.

### FA3 🟡 MeshService does its heavy DB work on the main thread

- **Evidence:** `MeshService.kt` — `onStartCommand` (main thread) runs
  `seedSeenIdsFromOwnHistory` (full outbound-envelope scans for every contact
  and group plus carried ids — `:446`, `:539-557`) and
  `publishInitialRelayHealth` (`:721-728`); `checkDigestMaintenance` runs on
  `relayMainHandler` (main looper) every 60 s calling `store.chatDigest` +
  `store.coreDigestAdvertisedMsgIds()` per live link (`:1686-1695`,
  `:1713-1726`); the relay-push hints lambda computes 8-day hint sets for
  every contact and group on the main thread on each (re)connect
  (`:798-813`).
- **Cost:** service-start jank climbing toward ANR as a cruise week of
  history accumulates; periodic main-thread stalls while chatting.
- **Fix:** a single-thread executor in MeshService for store work (seed,
  digest maintenance, hint computation); post only `MeshRouter.sendToAddress`
  results back to the appropriate thread.
- **Accept:** no `MessageStore` call from the main thread in MeshService
  (debug-build StrictMode-style assert passes).
- **Effort:** M. Conflicts with FA5/FA8/FA15 (same file).

### FA4 🟡 DB queries and full-size bitmap decodes inside composition on the chat list

- **Evidence:** `ChatScreen.kt:503-511` — `remember(messages,...)` executes
  `loadMessageReplyMetadata(store, ...)` during composition (same in
  `GroupChatScreen.kt:242-244`); `ChatScreen.kt:642-646` calls
  `store.outboundMessageExpiry(...)` inside the `LazyColumn` item lambda per
  own message per recomposition; `ChatImageAttachment` decodes the full JPEG
  via `BitmapFactory.decodeByteArray` in composition-time `remember`
  (`:1583-1586`, also `PendingPhotoCard`:832-834) with no downsampling.
- **Fix:** fold reply metadata + expiry into the load that produces
  `messages` (backgrounded per FA3/XP1); decode images with `inSampleSize`
  targeting bubble size (≤300 dp) off the main thread.
- **Accept:** item lambdas make no store calls; decoded bitmaps ≤2× display
  size.
- **Effort:** M.

### FA5 🟡 Seen-set check-then-record isn't atomic across the four concurrent receive paths — the D4 KDoc's "runs synchronously per received frame" claim is false

- **Evidence:** `MeshService.kt:2002-2008` gates on
  `!GossipState.seenIds.contains(...)` and records only at terminal returns
  (`:2077`, `:2099`, `:2119`); the justification at `:2365-2379` asserts
  single-threaded processing. In reality frames arrive concurrently on
  central-GATT binder threads, peripheral-GATT binder threads, LAN reader
  threads (`LanTransport.kt:724-729` `connectionExecutor`), and the
  relay-sync thread (`MeshService.kt:838`). Two copies of one `msg_id`
  (BLE + LAN simultaneously — routine for a nearby contact) both pass the
  gate and both deliver/flood/carry. Related: `running`, `meshRolesRunning`,
  `identity` are plain non-volatile fields (`:237-241`), and
  `MeshConnectivityStatus.mergeLastSeen` does a non-atomic map
  read-modify-write from binder threads (`MeshConnectivityStatus.kt:49-62`).
- **Cost:** duplicate floods/receipts doubling radio traffic exactly in the
  two-transports-up case; a false invariant that will mislead future changes.
- **Fix:** atomic dispatch admission (per-msgId in-flight set:
  `tryBegin(msgId)`/`finish(msgId, terminal)` as a testable plain class);
  `@Volatile` on the flags; `MutableStateFlow.update {}` for the status
  merges; correct the KDoc.
- **Accept:** unit test shows a second concurrent copy of an in-flight msgId
  is rejected; docs match reality.
- **Effort:** S.

### FA6 🟡 Notification direct-reply seals and sends on the main thread inside a BroadcastReceiver

- **Evidence:** `notify/NotificationActionReceiver.kt:22-32` runs
  `RealMeshSender.sendText` synchronously in `onReceive` — crypto sealing +
  SQLite transactions, then **re-encoding and re-sending every still-unacked
  outbound envelope including attachment blobs** (`MeshSender.kt:154-170`) —
  inside the receiver's ~10 s ANR budget. `ACTION_MARK_READ` loops store
  writes per group member (`:33-42`).
- **Fix:** `goAsync()` + background executor, `finish()` when done.
- **Accept:** `onReceive` returns immediately; reply still lands and the
  notification clears.
- **Effort:** S.

### FA7 🟡 New/backfilled messages yank the reader to the bottom while scrolled up

- **Evidence:** `ChatScreen.kt:566-571` and `GroupChatScreen.kt:292-297` —
  `LaunchedEffect(visibleMessages.size) { listState.scrollToItem(0) }` fires
  on any size change, including digest-sync backfill of older messages,
  regardless of scroll position.
- **Cost:** reading history during reconnect catch-up bursts (the ship-Wi-Fi
  norm) is constantly interrupted.
- **Fix:** auto-scroll only when already at bottom
  (`firstVisibleItemIndex <= 1`) or the appended message is own; otherwise a
  "new messages ↓" chip. Check iOS `scrollToLatest`
  (`UI/ChatView.swift:480-513`) for the same behavior while there.
- **Accept:** scrolled-up position preserved on incoming bursts; sending your
  own message still scrolls to it.
- **Effort:** S.

### FA8 🟡 "Added you to " string-prefix protocol between MeshService and MessageNotifier

- **Evidence:** `MeshService.kt:2777-2782` passes the literal
  `"Added you to ${group.name}"`; `MessageNotifier.kt:112` branches on
  `text.startsWith("Added you to ")`. Localizing either side (XP2) silently
  breaks the branch; a genuine message starting with that phrase is
  mis-rendered.
- **Fix:** explicit `isSystemEvent: Boolean` (or a separate
  `notifyGroupInvite`) parameter.
- **Accept:** no string sniffing; both call sites use the typed API.
- **Effort:** XS.

### FA9 🟢 Notification ids, PendingIntent request codes, and chat-list keys all use `ByteArray.contentHashCode()` — Int collisions cross wires between chats

- **Evidence:** `MessageNotifier.kt:160` (notification id =
  `chatId.contentHashCode()`, doubling as PendingIntent request code
  `:169-187`) — a collision replaces chat A's notification with chat B's and
  can target a stored reply intent at the wrong chat; `ChatListScreen.kt:206`
  uses the same hash as a LazyColumn key — collision = duplicate-key crash.
- **Fix:** notification `tag` = userId hex + fixed id; request codes from a
  per-chat registry; LazyColumn key = `UserIdHex.encode(chatId)`. While
  there: both notifications use `android.R.drawable` system icons
  (`MeshService.kt:3538`, `MessageNotifier.kt:203`) — supply app-owned icons.
- **Accept:** no `contentHashCode()` used anywhere user-routing depends on.
- **Effort:** XS.

### FA10 🟢 Touch targets below 48 dp on the chat screen

- **Evidence:** `ChatScreen.kt:854-860` remove-pending-photo button is 22 dp;
  `:934-940` attach "+" is 44 dp; reaction chips (`:1423-1440`) ~24 dp tall.
- **Fix:** `Modifier.minimumInteractiveComponentSize()` or padded clickable
  areas at unchanged visual size.
- **Accept:** Accessibility Scanner reports no touch-target violations on the
  conversation screen.
- **Effort:** S.

### FA11 🟢 Voice-memo playback leaks temp files and blocks the main thread

- **Evidence:** `ChatScreen.kt:1642-1653` — play path writes the whole blob
  to `cacheDir/play-<ts>.m4a` and calls `prepare()` synchronously in the
  click handler; manual stop (`:1636-1639`) and `DisposableEffect`
  (`:1624-1630`) never delete the temp file (only the completion listener
  does, `:1650`).
- **Fix:** remember the temp file, delete in stop + onDispose; move
  write/prepare off the main thread.
- **Accept:** cacheDir contains no `play-*.m4a` after stopping mid-playback.
- **Effort:** XS–S.

### FA12 🟢 Debug builds export a command receiver any co-installed app can invoke

- **Evidence:** `android/app/src/debug/AndroidManifest.xml:5-13` —
  `DebugCommandReceiver` is `exported="true"` with implicit-action filters;
  `SEND_TEXT` sends real sealed messages as the user
  (`DebugCommandReceiver.kt:31-68`). The test fleet runs debug builds in the
  field.
- **Fix:** `android:exported="false"` — explicit-component
  `adb shell am broadcast` still works from the shell uid.
- **Accept:** a third-party APK's broadcast is rejected; existing adb recipes
  still work.
- **Effort:** XS.

### FA13 🟢 RelayPushClient retries every 2 s forever when the hint set is empty

- **Evidence:** `RelayPushClient.kt:117-124` — empty hints →
  `scheduleReconnect()` without `backoff.recordFailure()`, so
  `RelayPushBackoff.nextDelayMs()` stays at the 2 s floor
  (`RelayPushBackoff.kt:22-25,38`) indefinitely — exactly the
  fresh-onboarding state (relay config, no contacts yet).
- **Fix:** record a failure on the empty-hints branch, or use a fixed long
  delay (~60 s; the poll covers correctness).
- **Accept:** with zero contacts, reconnect attempts back off to the cap.
- **Effort:** XS.

### FA14 🟡 Placeholder protocol UUID is still shipping

- **Evidence:** `MeshConstants.kt:8` — "Placeholder UUID — regenerate a real
  random v4 UUID before any real deployment". Every fielded build makes this
  compat decision harder (old and new UUIDs won't discover each other).
- **Fix:** decision required (§7): regenerate now and coordinate a fleet
  upgrade, or explicitly bless the current UUID as permanent and delete the
  comment. Either close is fine; leaving it open is not.
- **Effort:** XS (decision + one-line change) — fleet coordination is the
  real cost.

### FA15 🟢 God-class and duplication debt (refactor, incremental)

- `MeshService.kt` is 3,571 lines spanning FGS lifecycle, the whole relay
  sync engine (~`:766-1322`), envelope dispatch + nine kind handlers
  (~`:1956-3140`), DTN carry, Wi-Fi hold, digest maintenance. Extract
  `RelaySyncEngine` and `InboundEnvelopeProcessor` as plain testable classes
  per the repo's own §3.4 pattern — this is also what makes FA3/FA5 cleanly
  testable, and pays for itself on D9. Effort: L (incremental).
- `ChatScreen.kt` (1,733) and `GroupChatScreen.kt` (984) duplicate ~500 lines
  of scaffolding verbatim (gallery/camera/mic launchers, voice send, photo
  staging, overlay wiring — compare `ChatScreen.kt:226-330` with
  `GroupChatScreen.kt:143-237`). A shared `ConversationHost` lands future
  chat work once instead of twice. Effort: M. (iOS mirror: FI12.)
- Tracked-worthy TODO: `MeshService.kt:1129` relay-proxy cursor follow-up.

---

## 3. iOS

*(All iOS findings came from static analysis — the Mac validation host was
unreachable at audit time. It is back up as of 2026-07-20, so items marked
*(Mac)* should be validated there before merging.)*

### FI1 🔴 Open chat stays "visible" while the app is backgrounded → false read receipts + suppressed notifications

- **Evidence:** `UI/ChatView.swift:245-258` and `UI/GroupChatView.swift:205-221`
  set/clear `ChatVisibility` only in `onAppear`/`onDisappear`; nothing
  observes `scenePhase` (`CruiseMeshApp.swift:37-39` only calls
  `setAppForeground`, which touches LAN scans). Android explicitly clears on
  `ON_STOP`, and its KDoc (`notify/ChatVisibility.kt:12-19`) names this exact
  failure mode.
- **Failure:** user locks the phone with a chat open; BLE background modes
  keep delivering; `handleIncomingChat` (`MeshController.swift:1206-1231`)
  sees the chat as visible → sends a **read** receipt and skips the
  notification. Sender sees "Read" on a message nobody read; recipient gets
  no banner.
- **Fix:** clear/restore the visible chat on scenePhase transitions
  (`.onChange(of: scenePhase)` in the chat views, or `ChatVisibility.reset()`
  from `setAppForeground(false)` with re-registration on foreground).
- **Accept:** message arriving while locked with a chat open posts a
  notification and sends only a delivered receipt; reopening sends the read
  receipt. *(Mac/device.)*
- **Effort:** S.

### FI2 🔴 iOS never proxy-fetches relay mail for nearby contacts (Android does)

- **Evidence:** iOS relay fetch hints are self + own groups only
  (`Mesh/MeshController.swift:2269-2287`); the code says so itself
  (`:2370-2373` — "Unlike Android's `relayHintsForConfig`/`relayProxyHints`,
  there is no 'proxy' hint set here"). Android fetches every contact's hints
  each pass and hands mail over BLE (`MeshService.kt:786, 1148, 1215`).
- **Cost:** an iPhone with internet won't ferry mailbox mail to a BLE-only
  family member nearby; the mail waits until the recipient's own device gets
  internet — asymmetric delivery latency in the app's core field scenario.
- **Fix:** mirror `relayProxyHints` in `relaySyncBlocking`; the core
  disposition/ack machinery already distinguishes proxy-fetched rows (see
  `coreRelayAckIdsWithConsumed` comments) and the never-ack-a-carried-copy
  invariant is enforced core-side.
- **Accept:** iPhone-with-internet + Android-BLE-only pair delivers mailbox
  mail to the Android via the iPhone. *(Mac + device pair.)*
- **Effort:** M.

### FI3 🔴 No BLE state restoration or background refresh — mesh dies silently on process termination

- **Evidence:** `Mesh/BleTransport.swift:41-42` creates
  `CBCentralManager`/`CBPeripheralManager` without restore-identifier
  options; zero hits for `willRestoreState`, `BGTaskScheduler`, or
  `beginBackgroundTask` in `ios/`. `Info.plist:43-47` declares both
  bluetooth background modes — so the app runs backgrounded, but once jetsam
  terminates it nothing relaunches it, and relay sync (60 s `relayTimer`)
  only exists while the process lives (`RelayPushClient.swift:37-50`
  documents the WS half).
- **Cost:** overnight, iPhones drop out of the mesh until manually reopened;
  Android's foreground service keeps carrying. Mule-duty parity failure.
- **Fix:** restoration identifiers +
  `centralManager(_:willRestoreState:)`/`peripheralManager(_:willRestoreState:)`
  re-adopting restored peripherals and restarting scan/advertise; optionally
  a `BGAppRefreshTask` running `runRelaySync`.
- **Accept:** kill via jetsam pressure, bring a peer into range → app
  relaunches and reconnects. *(Mac/device; restoration is untestable
  statically.)*
- **Effort:** M.

### FI4 🟡 All store + crypto work runs on the main actor

- **Evidence:** `MeshController` is `@MainActor`
  (`Mesh/MeshController.swift:8-9`); every inbound BLE/LAN frame does SQLite
  inserts, digest building, spray planning, receipt authoring on main
  (`onFrameReceived` → `handleDigest`/`processInboundEnvelope`, `:353-611`);
  relay catch-up hops the main actor per envelope (`:2299-2317`);
  `MessageBubbleView.body` runs a store query per own-message row per
  recompute (`UI/ChatView.swift:577-583`, `UI/GroupChatView.swift:474-480`);
  `RealMeshSender.enqueue` seals attachments on main
  (`Chat/MeshSender.swift:61-98`).
- **Cost:** scroll hitches and input lag exactly during reconnect/digest
  bursts; a 200-envelope catch-up = 200 main-thread store transactions
  interleaved with 200 chat-list rebuilds (XP1).
- **Fix:** move the frame path onto a dedicated serial actor with main hops
  only for `@Published` mutations (mirrors Android's handler thread).
  Incremental start: lift the per-row `outboundMessageExpiry` query into
  `reload()` (XS).
- **Accept:** no store calls on the main thread during a synthetic
  200-message ingest; UI still updates. *(Mac.)*
- **Effort:** L incremental; the bubble-row query alone XS.

### FI5 🟡 Hidden-kind handlers still ack on store failure — the T4-06 invariant isn't applied to friend requests / profile sync / directory / LAN hints

- **Evidence:** T4-06 made chat/receipt/group paths propagate primary-store
  throws so `.failed` leaves the relay copy un-acked
  (`MeshController.swift:1163-1190, 771-786`). But `deliverOpened`'s other
  cases (`:884-918`) call non-throwing handlers:
  `handleIncomingFriendRequest` swallows everything
  (`_ = try? store.upsertImportedContact` `:1338`, `try? insertMessage`
  `:1356-1363`), as do `handleIncomingProfileSync` (`:1439-1446`),
  `handleIncomingFriendDirectory` (`:1513-1520`),
  `handleIncomingIntroducedFriendRequest` (`:1568-1586`),
  `handleIncomingLanEndpointHint` (`:1413-1420`). The caller then records the
  msgId and returns `.consumed` → relay copy acked away.
- **Cost:** a transient store failure (disk-full, busy) while ingesting a
  relay-fetched kind=3 friend request **permanently destroys it** — the same
  one-shot-loss class behind T11, but a distinct statically-verifiable gap.
- **Fix:** make these handlers `throws` on their primary store write,
  matching the chat path; keep deterministic rejects (bad decode, identity
  mismatch) as swallowed terminal states.
- **Accept:** with an injected failing store, a friend request ingest returns
  `.failed` and the relay copy is not acked. *(Mac for suite.)*
- **Effort:** S.

### FI6 🟡 BLE connect/disconnect callbacks race via mixed sync/async dispatch

- **Evidence:** `Mesh/MeshController.swift:160-181` —
  `onCentralConnected`/`onPeripheralSubscribed` hop via `Task { @MainActor }`,
  but `onCentralDisconnected`/`onPeripheralUnsubscribed` call
  `MeshRouter.onDisconnected(address:)` synchronously on the BLE queue. A
  connect immediately followed by a disconnect (flaky link) can execute the
  disconnect *before* the queued connect runs → dead address re-registered as
  a live route: inflated "Meshing · N nearby", frames routed to a gone peer,
  stale digest bookkeeping.
- **Fix:** route all four callbacks through the same `Task { @MainActor }`
  hop so task-enqueue order preserves event order.
- **Accept:** MeshRouterState ordering unit test.
- **Effort:** XS.

### FI7 🟡 Local Network permission denial is silent — LAN transport just never works

- **Evidence:** `Mesh/LanTransport.swift:249-262` — browser `.failed` posts a
  diagnostics hint, but a denied Local Network permission actually produces
  `.waiting(...)`, which only does `log.debug` (`:257-258`). Nothing checks
  or prompts for local-network authorization; contrast the prominent
  Bluetooth banner (`UI/ChatListView.swift:44-73`) — no LAN equivalent.
- **Cost:** one "Don't Allow" on the system prompt → Bonjour, subnet sweep,
  and manual connect fail forever with generic retry noise and no path to
  Settings.
- **Fix:** detect persistent `.waiting` with a POSIX/DNS policy error on the
  browser/listener; dedicated diagnostics state ("Local Network permission is
  off — enable it in Settings") + reuse the warning-banner slot;
  deep-link to Settings.
- **Accept:** with permission denied, Advanced settings and the home banner
  say so. *(Mac/device — the exact NWError must be observed.)*
- **Effort:** S.

### FI8 🟡 Chat thread body recomputation is O(n²) with per-row DateFormatter allocations

- **Evidence:** `visible` is a computed filter over all messages
  (`UI/ChatView.swift:70-72`); `isNewDay`, `messageGrouping`, `scrollToLatest`
  re-evaluate it per row (`:480-513`; same in `GroupChatView.swift:49-51,
  415-422`); `dayLabel`/`timeLabel` construct a fresh `DateFormatter` per row
  per pass (`:488-493, 725-730`; `GroupChatView.swift:581-586`); `reactions`
  recomputes over all messages each pass (`:74-76`).
- **Fix:** compute `visible`, groupings, and day-break flags once in
  `reload()` into a row-model array; cache static `DateFormatter`s.
- **Accept:** instrument a 1,000-message chat; body time flat vs message
  count. *(Mac.)*
- **Effort:** S–M.

### FI9 🟡 `try!` crash points in launch/critical paths

- **Evidence:** `Core/AppStore.swift:21` — `try! MessageStore.open(path:)` —
  a corrupt/unopenable DB is a **crash loop at launch** with no recovery
  (also reached from the notification-response path,
  `CruiseMeshApp.swift:65`). `Chat/MessageInteractions.swift:21` —
  `try! encodeReactionPayload` (lower risk).
- **Fix:** on open failure, move the bad DB aside
  (`messages.sqlite.corrupt-<ts>`), reopen fresh, surface a "history could
  not be read" notice; drop the reaction `try!` to `try?` + no-op.
- **Accept:** corrupting the sqlite file then launching yields a working
  (empty) app, not a crash loop. *(Mac.)*
- **Effort:** S.

### FI10 🟢 `didReceiveWrite` responds once per request in a batch

- **Evidence:** `Mesh/BleTransport.swift:377-388` loops `requests` calling
  `peripheral.respond(to:withResult:)` for each. CoreBluetooth requires
  exactly one respond per `didReceiveWrite` group (to the first request);
  extras are API misuse and the batch isn't treated atomically.
- **Fix:** process all values, respond once to `requests[0]`.
- **Accept:** no behavior change with Android centrals. *(Mac/device.)*
- **Effort:** XS.

### FI11 🟢 One-notification-per-chat replacement loses message count

- **Evidence:** `Notify/MessageNotifier.swift:45-47, 74-76` — the request
  identifier is the chat's userId hex, so each new message replaces the
  previous banner; a burst of 5 shows 1. `notifyFriendAdded` (`:57-61`)
  shares the identifier space and can be clobbered.
- **Fix:** unique request ids (chatId + lamport) with
  `threadIdentifier = chatId` for grouping.
- **Effort:** XS.

### FI12 🟢 ~300 lines duplicated between ChatView and GroupChatView, plus dead code

- **Evidence:** composer row, photo/camera/voice pipeline, voice-memo sheet
  near-identical (`UI/ChatView.swift:161-327, 383-457` vs
  `UI/GroupChatView.swift:130-284, 340-388`). Dead private views
  `ReactionActionBar`/`MessageActionPanel` (`ChatView.swift:779-840`)
  referenced nowhere.
- **Fix:** extract shared `ChatComposer`/`AttachmentPickerSheet`; delete the
  dead structs (delete-only part is XS).
- **Effort:** M. (Android mirror: FA15.)

---

## 4. Rust core

*(Baseline at audit: `cargo test -p cruisemesh-core` = 299 passed, plus 8
`mesh_sim` integration tests, 0 failures. Any core change touching the
UniFFI surface ⇒ regenerate bindings, both platforms.)*

### FC1 🟡 Group field-metrics silently drop rows: `delivery_metrics` PK omits the sender

- **Evidence:** `core/src/store.rs:3168-3178` —
  `PRIMARY KEY(chat_hash, lamport, direction)`, inserted with
  `INSERT OR IGNORE` (`:907-918`). In a group, every member has an
  independent lamport stream, so two senders routinely share `lamport = 1`
  in the same chat_hash; the second arrival metric is silently discarded.
- **Cost:** the V2 cruise-metrics export (the entire point of the field test
  instrumentation) undercounts group deliveries in the common case. 1:1 is
  unaffected.
- **Fix:** add `sender_user_id` (or an 8-byte hash) to the table and PK;
  include it in `record_message_arrival` and `export_delivery_metrics_csv`.
- **Accept:** a test recording arrivals for two senders sharing a lamport in
  one group yields two rows, both in the CSV.
- **Effort:** S. **Land before the next cruise test.**

### FC2 🟡 Digest spray materializes unbounded ciphertext every exchange

- **Evidence:** `core/src/engine.rs:511-545` (`core_digest_spray_plan`) +
  `core/src/store.rs:2494-2520` (`carried_envelopes_for_peer_sync`). Every
  digest exchange: (a) `carried_envelopes_for_peer_sync` selects **all**
  unexpired carried rows including `sealed` blobs (no LIMIT; carry ceiling
  is 64 MiB) and filters `known_msg_ids`/`peer_hints` in Rust afterwards
  (`store.rs:2515-2519`); (b) the spray plan loops all contacts
  (`engine.rs:517-531`) issuing `receipt_through` +
  `outbound_envelopes_after` per contact, materializing every pending
  outbound ciphertext *before* `select_own_outbound` applies the byte
  budget. With D8's 3–5 min re-digest now live, this cost recurs on every
  long-lived link.
- **Cost:** mule with a large carry queue + many contacts does up to 64 MiB
  of SQLite reads/allocations per connect and per re-digest tick — battery
  and latency scaling with total carried bytes, not with what the peer needs.
- **Fix:** push the exclusions into SQL; select `msg_id`/`recipient_hint`
  first and fetch `sealed` only for surviving rows within budget; bound the
  per-contact fan-in by the shared budget during iteration.
- **Accept:** a store with N carried envelopes the peer already knows
  performs no `sealed`-blob reads for those rows during a spray plan
  (row/size probe or benchmark).
- **Effort:** M.

### FC3 🟢 Carry-digest migration is O(n) round-trips under one transaction at store open

- **Evidence:** `core/src/store.rs:2650-2698`
  (`migrate_carried_content_digests`) — a loop of
  `SELECT ... LIMIT 1` + single-row `UPDATE`/`DELETE` per legacy row, inside
  `MessageStore::open` (called synchronously at the UniFFI boundary).
- **Fix:** batch — read all NULL-digest rows once, apply set-based updates;
  dedupe with `DELETE ... WHERE rowid NOT IN (SELECT MIN(rowid) ... GROUP BY
  content_digest)`.
- **Accept:** migrating K rows issues O(1) statements; existing
  dedupe/budget tests pass.
- **Effort:** S.

### FC4 🟢 First delivery confirmation with unknown transport permanently hides the route (T6 follow-up)

- **Evidence:** `core/src/store.rs:1457-1463` — `via_transport` is
  overwritten only `WHEN excluded.through_lamport > through_lamport AND
  excluded.via_transport IS NOT NULL`. If the first receipt at a watermark
  has `via = NULL`, a later re-send at the same watermark that *does* know
  its route can never fill it — Message info shows "Delivered" with no
  "confirmed via …" forever.
- **Fix:** also fill when the stored value is NULL at an equal-or-greater
  watermark (COALESCE/CASE in the upsert).
- **Accept:** receipt at N with `via=None` then N with `via=BLE` leaves BLE;
  a later `via=None` at N never clears it.
- **Effort:** XS.

### FC5 🟢 Fuzz coverage misses two untrusted decoders: backup file + LAN endpoint parsers

- **Evidence:** `fuzz/fuzz_targets/*` cover frames, message/receipt/profile/
  LAN-content/group/attachment/reaction/friend decoders, relay wire, BLE
  reassembly, Noise. Not fuzzed: `open_backup`/`decode_inner`
  (`core/src/backup.rs:173,319` — parses an attacker/user-selected `.cmbak`
  with its own Cursor TLV decoder that allocates from length prefixes) and
  `core_parse_lan_endpoint`/`core_parse_lan_endpoint_link`
  (`core/src/lan_util.rs:27,73` — untrusted text, bracket/base64 slicing).
- **Fix:** add a `backup_wire` target invoking `open_backup(pw, data)` +
  `decode_identity_bytes`; fold the LAN endpoint parsers into
  `protocol_decoders`. Add both to the CI smoke list.
- **Accept:** new targets build under `cargo fuzz`, appear in CI, short run
  finds no panics.
- **Effort:** S.

### FC6 🟢 Poisoned store mutex converts any one panic into a permanent process-wide store outage

- **Evidence:** pervasive `self.conn.lock().expect("store mutex poisoned")`
  (`core/src/store.rs:483, 743, 1453, 2281`, etc.; `.lock().unwrap()` in
  `transport_policy.rs:105`). Poisoning is sticky: one panic under the lock
  ⇒ every later store call aborts natively across the UniFFI boundary —
  crash loop until process restart.
- **Fix:** small helper using
  `lock().unwrap_or_else(|e| e.into_inner())` (SQLite transactions make
  partial state safe to reuse), or map poison to `CoreError::Store`.
- **Accept:** a test that poisons the mutex then calls another store method
  gets `Err`/recovery, not a panic.
- **Effort:** S.

### FC7 🟢 `family_carried_envelopes` relay-upload query has no supporting index

- **Evidence:** `core/src/store.rs:2569-2588` — `WHERE is_family = 1 AND
  from_relay = 0 AND expiry > ?1 ORDER BY received_at ASC`; only
  `idx_carried_hint(recipient_hint)` and `idx_carried_expiry(expiry)` exist
  (`:3235-3236`) ⇒ filter + temp-sort per relay-upload pass.
- **Fix:** `CREATE INDEX idx_carried_family_upload ON
  carried_envelopes(is_family, from_relay, expiry, received_at)` via the
  existing pattern in `open`.
- **Accept:** `EXPLAIN QUERY PLAN` uses the new index, no
  `USE TEMP B-TREE FOR ORDER BY`.
- **Effort:** XS.

### FC8 🟢 `core_unread_count` is 1:1-only but exported without that contract

- **Evidence:** `core/src/semantic.rs:72-86` compares every non-self
  sender's lamport against a single scalar `read_through` — correct only
  with one other sender; the store-backed `semantic_unread_count`
  (`:198-223`) handles groups correctly. Android routes correctly
  (`ChatListLogic.kt:63`), so this is a latent API trap, not a live bug.
- **Fix:** rename to `core_one_to_one_unread_count` or add a doc-comment
  stating single-sender-only.
- **Effort:** XS.

### FC9 🟢 `insert_message` fork-recovery treats a reply-target mismatch as a stream reset

- **Evidence:** `core/src/store.rs:527-533` — the true-duplicate test
  compares `(timestamp, kind, payload, reply_to_msg_id)`, but plain
  `insert_message` always passes `reply_to_msg_id = None` (`:454`). The same
  logical message arriving once with and once without a reply target is
  misclassified as a **fork**, deleting the tail `lamport >= N` and wiping
  `outgoing_receipts` for that sender. Not reachable in the current pipeline
  (each kind uses one fixed insert path) — defense-in-depth for a
  destructive recovery.
- **Fix:** exclude `reply_to_msg_id` from the fork discriminator; reconcile
  it via COALESCE as the duplicate path already does for `msg_id`.
- **Accept:** same `(chat,sender,lamport,timestamp,kind,payload)` twice with
  differing `reply_to_msg_id` is a merge, not a tail-delete.
- **Effort:** S.

### FC10 🟢 SQLite opened without WAL / `busy_timeout`; `VACUUM INTO` blocks all store calls

- **Evidence:** `MessageStore::open` (`core/src/store.rs:302-388`) sets no
  pragmas; `backup_to` runs `VACUUM INTO` (`:418`) under the shared mutex —
  every write pays a rollback-journal fsync, and a backup on a large DB
  blocks the whole store for its duration (UI jank during backup).
- **Fix:** after open: `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
  PRAGMA busy_timeout=<ms>;` (WAL persists on the file).
- **Accept:** new stores report `journal_mode=wal`; existing tests pass.
- **Effort:** S.

---

## 5. relayd + CI + docs + repo hygiene

*(Reminder: relayd code changes land in the repo freely; **deploying** to
the live relay needs David's go-ahead — §7.)*

### FR1 🔴 No CI runs any test suite — the 330-test Rust workspace and Android units are entirely uncovered

- **Evidence:** `.github/workflows/` contains exactly `fuzz-smoke.yml`
  (path-filtered to `core/**`+`fuzz/**`) and `ui-localization.yml` (string
  lint). Nothing runs `cargo test --workspace`, clippy, fmt, or
  `gradlew testDebugUnitTest`. A relayd-only PR triggers zero compile/test
  jobs; the only control is a PR-template checkbox.
- **Fix:** one `rust.yml` (checkout → `Swatinem/rust-cache` →
  `cargo fmt --check`, `cargo clippy --workspace -D warnings`,
  `cargo test --workspace`) on PR + push; a second job doing the AGENTS.md
  host-bindgen recipe + `gradlew testDebugUnitTest`; **fold in a kotlin-gen
  drift check** — regenerate bindings on the Linux host and
  `git diff --exit-code android/app/src/main/kotlin-gen` so stale bindings
  become a red check instead of a runtime crash (the known 8-branch hazard).
  Clippy may need a `-A` allowlist pass first — if the workspace isn't
  clippy-clean, land the workflow with `clippy` non-blocking and tighten in
  a follow-up rather than blocking the whole item.
- **Accept:** a PR breaking a core/relayd/Android test, or changing the
  UniFFI surface without regenerated bindings, shows a red check.
- **Effort:** S (+M later for an iOS smoke build when a Mac runner exists).

### FR2 🔴 relayd emits effectively zero logs — a field incident is undebuggable

- **Evidence:** the only log statement in the crate is the startup `info!`
  (`relayd/src/main.rs:32`). `ApiError::internal` (`relayd/src/lib.rs:1238`)
  sends the raw store error to the client but never logs it.
  `EnvFilter::from_default_env()` (`main.rs:14`) defaults to ERROR-only, and
  neither `docker-compose.yml` nor the Dockerfile sets `RUST_LOG` — the
  deployed container prints **nothing, ever**, including the startup line.
  No Caddy access log either.
- **Fix:**
  `EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())`;
  `info!`/`warn!` on auth reject, envelope post (family + size), 413/507, WS
  connect/disconnect/lag-drop, prune counts; log the detail inside
  `ApiError::internal` (see also FR8); add a `log` directive to the
  Caddyfile.
- **Accept:** default `docker compose up` produces one line per rejected
  request and per WS lifecycle event.
- **Effort:** S.

### FR3 🟡 No graceful shutdown — the tokio `signal` feature is enabled but never wired

- **Evidence:** `relayd/src/main.rs:35` — plain `axum::serve(...)`, no
  `.with_graceful_shutdown(...)`; `relayd/Cargo.toml:14` enables tokio's
  `"signal"` feature (intended, dropped). `docker stop` kills mid-request;
  WS clients get RST instead of Close.
- **Fix:** `.with_graceful_shutdown(...)` on ctrl_c/SIGTERM.
- **Accept:** `docker stop` drains in-flight requests; WS closes cleanly.
- **Effort:** XS. Bundle with FR2/FR4 in one relayd PR.

### FR4 🟡 The deployed relay version is untrackable

- **Evidence:** `/healthz` returns only `{"status":"ok"}`
  (`relayd/src/lib.rs:730`); `Cargo.toml` frozen at 0.1.0; compose builds
  whatever tree is checked out, no image tag or embedded SHA. Master's
  relayd has grown `/presence`, D7 quotas, and T4-09 limits — no way to ask
  the live VPS which it runs.
- **Fix:** embed `env!("CARGO_PKG_VERSION")` + a build-time git SHA
  (`build.rs` or Dockerfile `--build-arg GIT_SHA`) into the startup log and
  `/healthz`.
- **Accept:** `curl https://relay/healthz` reports the exact live commit.
- **Effort:** XS.

### FR5 🟡 `ws_live_push_after_connect` flake: logic is provably race-free — the 3 s wall-clock timeout is the whole problem

- **Evidence:** `relayd/tests/e2e_ws.rs:216` —
  `timeout(Duration::from_secs(3), socket.next())`. In `handle_ws`
  (`lib.rs:1085`) subscribe precedes the replay fetch, and in
  `post_envelope` the insert precedes the broadcast — any envelope missed by
  the broadcast is guaranteed to appear in the replay, so loss is
  impossible; the only failure mode is CPU starvation under parallel
  build/test load (matches the observed flake pattern).
- **Fix:** raise the timeout to 30–60 s (timeouts guard hangs, not
  performance); same for the 15 s slow-consumer deadline; optionally
  `#[tokio::test(flavor = "multi_thread")]`. Leave the 200 ms *negative*
  timeout in `ws_two_families_isolated` alone.
- **Accept:** 50 consecutive runs under `cargo test --workspace` with a
  parallel build running, zero flakes.
- **Effort:** XS.

### FR6 🟡 No cap on concurrent WebSocket connections, and no server-side keepalive

- **Evidence:** `ws_handler`/`handle_ws` (`relayd/src/lib.rs:1002-1176`)
  accept unbounded upgrades — T4-09 capped request *shapes*, not socket
  count. No server Ping: a silently-dead phone's socket lingers until the
  next broadcast hits the 5 s `WS_WRITE_TIMEOUT` — for an idle family,
  hours/days. Family tokens are semi-public (in QR friend cards), so anyone
  who has seen a card can hold thousands of sockets on the 512 MB VPS.
- **Fix:** per-token `Semaphore` (e.g. 16) + global cap (e.g. 256), reject
  429; periodic server Ping (30–60 s) in the `select!` loop, dropping peers
  that don't answer.
- **Accept:** the 257th concurrent upgrade is refused; a vanished client is
  reaped within ~2 ping intervals.
- **Effort:** S.

### FR7 🟡 Mailbox maintenance is entirely fetch-driven: no background prune, no VACUUM, writes on every read

- **Evidence:** `prune_expired` runs only inside `fetch_envelopes`
  (`relayd/src/lib.rs:433`), `sync_presence`, and the quota-overflow path —
  no timer; conversely every `GET /envelopes` poll runs a `DELETE` first
  (write txn on the hot read path). No `VACUUM`/`auto_vacuum` — the SQLite
  file stays at its high-water mark forever; disk-full surfaces as unlogged
  raw 500s.
- **Fix:** hourly `tokio::spawn` interval: `prune_expired(now)` +
  `PRAGMA incremental_vacuum` (enable `auto_vacuum=INCREMENTAL` in SCHEMA),
  log deleted counts; drop the prune from the fetch path.
- **Accept:** rows expire with no client traffic; `GET /envelopes` performs
  no writes; file shrinks after mass expiry.
- **Effort:** S.

### FR8 🟢 Blocking SQLite on tokio workers + internal error strings leaked to clients

- **Evidence:** every handler does `self.conn.lock()` + synchronous rusqlite
  in async context (`relayd/src/lib.rs:308, 434`) — no `spawn_blocking`, no
  WAL/`busy_timeout`; `ApiError::internal` puts raw rusqlite error text
  (which can include the DB path) in 500 bodies (`:1238-1244`).
- **Fix:** `spawn_blocking` around store calls (or document the accepted
  tradeoff), `journal_mode=WAL` + `busy_timeout`; `internal()` logs the
  detail, returns a generic body.
- **Accept:** 500 responses carry no SQLite text; store calls off the
  reactor.
- **Effort:** S.

### FR9 🟢 Fuzz nightly never accumulates a corpus, and the workflow rebuilds cold every run

- **Evidence:** `fuzz-smoke.yml` — no `actions/cache` for `fuzz/corpus/`
  (every nightly 600 s run restarts exploration from zero, defeating the
  stated goal at line 16); no Rust build cache (4 matrix jobs cold-compile);
  `cargo install cargo-fuzz --locked || true` (line 40) swallows install
  failures; the `cargo-fuzz-bin` cache key (line 38) never invalidates
  against new nightlies.
- **Fix:** cache `fuzz/corpus/${{ matrix.target }}` per-target with
  restore-keys; add `Swatinem/rust-cache`; key the binary cache on the
  nightly version; drop `|| true`.
- **Accept:** consecutive nightlies show a growing corpus; PR smoke under
  ~5 min warm.
- **Effort:** S.

### FR10 🟡 ROADMAP/DESIGN say media attachments are "not started" — they shipped

- **Evidence:** `ROADMAP.md:14` — "Media attachments … 📋 Designed, not
  started". But `core/src/content.rs:6` has
  `ATTACHMENT_MAX_BLOB_BYTES = 180 KiB` (inline attachments in sealed
  envelopes, shipped), and relayd's quota rationale
  (`relayd/src/lib.rs:132-139`, `DEPLOY.md §10`) is sized around cruise
  photos flowing **over the relay** — the opposite of the roadmap's "direct
  links only". `DESIGN.md §8` still describes only the unshipped
  manifest/chunk scheme.
- **Fix:** flip M6 to shipped (inline ≤180 KiB blobs, any transport incl.
  relay); short DESIGN.md §8 note that inline shipped and manifest/chunk
  remains the design for larger media.
- **Effort:** XS.

### FR11 🟡 SECURITY-DESIGN.md's "key material never leaves the device" is no longer true; LOCAL_BACKUP_RESTORE.md still says "proposal"

- **Evidence:** SECURITY-DESIGN.md (Identity + In-scope): "private keys never
  leave it… Key material must never leave the device." But
  `core/src/store.rs:393 backup_to` and the shipped `.cmbak` passphrase
  backup export identity keys (encrypted) to user-chosen storage including
  cloud drives; `LOCAL_BACKUP_RESTORE.md` §1 still reads "Status: proposal
  (v1)" despite the feature being merged. This is the document written for a
  skeptical outside reader; a false security claim on page one is the exact
  credibility failure it exists to avoid.
- **Fix:** update the Identity section — keys leave the device only inside
  the passphrase-KDF-encrypted `.cmbak` bundle (reference the T4-07 KDF
  params); flip LOCAL_BACKUP_RESTORE.md to "shipped".
- **Effort:** XS.

### FR12 🟡 `AGENT-TODO.md` — the self-declared plan of record — is untracked; `.codex-remote-attachments/` is neither ignored nor tracked

- **Evidence:** `git status` shows both untracked; `git check-ignore`
  confirms no rule matches `.codex-remote-attachments/` (unlike `logs/`,
  `tmp/`, `target/`, `HANDOFF.md`, `PRIVATE-*.md` — all correctly excluded).
  Bonus: `.DESIGN.md.swp` (deletable), and local `master` was 1 commit
  behind `origin/master` at audit time.
- **Cost:** the plan of record can be lost to a worktree mishap (a
  documented past incident in this repo); the attachments dir will
  eventually be committed by an inattentive `git add -A`.
- **Fix:** commit `AGENT-TODO.md` (and this file); add
  `/.codex-remote-attachments/` to `.gitignore`; delete the swp; pull.
- **Accept:** `git status` clean on master.
- **Effort:** XS.

### FR13 🟡 ~55 stale remote branches drown the signal

- **Evidence:** `git branch -r --merged origin/master` lists 34
  merged-but-undeleted remote branches; of the ~27 "unmerged" ones most are
  squash-merged and actually landed (`agent/t1-swipe-to-reply` #96,
  `agent/t6-…` #97, `agent/t10/t12/t13/t14/t15/t16`,
  `agent/v2-field-metrics` #98, `agent/d8-periodic-redigest` #102,
  `agent/wave-docs` #103, `agent/dtn-*`), plus dead exploration branches and
  local `pr*` scratch.
- **Fix:** enable GitHub "Automatically delete head branches"; one cleanup
  pass cross-referencing `gh pr list --state merged`; prune local scratch.
  **Do not delete** branches whose PR hasn't merged without checking
  (e.g. `agent/t5-onboarding-script` awaits sign-off).
- **Accept:** `git branch -r | wc -l` under ~15, all live.
- **Effort:** S.

### FR14 🟢 No CLAUDE.md — AGENTS.md's critical setup knowledge isn't surfaced to agent tooling

- **Evidence:** no `CLAUDE.md` at root or in `.claude/`; `AGENTS.md` holds
  the fresh-worktree bindgen recipe and Mac runbook, TODO.md §3 the ground
  rules — sessions only get them if someone remembers to read the files.
- **Fix:** a short `CLAUDE.md` pointing at AGENTS.md, TODO.md §3,
  AGENT-TODO.md, and this file (or inlining the ~10 most load-bearing
  lines: worktree rule, bindgen recipe, commit email, test commands).
- **Effort:** XS.

### FR15 🟢 No release/versioning automation; APK builds are artisanal

- **Evidence:** tags exist (`v1.0.0`, `android-v1.0.0`,
  `jacobson-family-cruise-pre-release-2026-07-13`) but no release workflow;
  `android/app/build.gradle.kts:34` hardcodes
  `versionCode = 1784406677` (manually bumped epoch timestamp); no CI job
  runs even `assembleDebug`, so APK reproducibility rests on one
  workstation's NDK/cargo-ndk setup.
- **Fix:** tag-triggered workflow: `core/build-android.sh` (NDK via
  `android-actions/setup-android` + rustup targets) → `assembleRelease` →
  attach APK to a GitHub Release; derive `versionCode` from tag/commit
  count.
- **Accept:** pushing an `android-v*` tag yields a downloadable versioned
  APK artifact.
- **Effort:** M.

---

## 6. What was checked and found healthy (don't re-audit)

- Core wire decoders: uniformly bounds-checked single-`Cursor` idiom,
  `CoreError::Malformed` over panics; T4 hardening present and exercised.
  No memory-safety or forgery issue found.
- iOS transports: correct weak-capture discipline — no retain cycles found;
  25 unit-test files covering backoff/digest/scan-planner/tick/reply policy.
- DTN ack/dedupe/expiry invariants: carefully reasoned, documented, tested
  on both platforms (modulo FA5's admission race and FI5's ack-on-failure
  gap).
- relayd WS replay logic: provably race-free (subscribe-before-replay,
  insert-before-broadcast).
- Workflow security: no `pull_request_target`, no secrets exposure to
  untrusted PR code.

---

## 7. Decisions needed from David (agents: don't start these without an answer)

1. **FA1 — minSdk:** raise to 31 (recommended; deletes dead code and matches
   reality) or add legacy pre-31 Bluetooth+location support? Depends on
   whether any Android 8–11 device should ever run CruiseMesh.
2. **FA14 — protocol UUID:** regenerate the placeholder mesh UUID now
   (requires coordinated fleet upgrade — old/new builds won't discover each
   other) or bless the current one permanently?
3. **relayd deploys (FR2/3/4/6/7/8):** code can land in the repo; when may
   the live relay be redeployed, and in what batch?
4. **XP2 policy for iOS:** route `String`-typed status copy through
   `String(localized:)` (localizable, more churn) or just extend the checker
   and accept en-only for diagnostics copy?
5. **FR13:** confirm which unmerged remote branches are live before the
   cleanup pass (known-live: `agent/t5-onboarding-script`).
