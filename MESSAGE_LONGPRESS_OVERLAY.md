# Message Long-Press Overlay — Signal-style focus mode for message actions

**Status: spec, not implemented.** Android only. No wire-protocol, storage, or
relay changes — this is purely a presentation change to how the existing
react/copy/info actions are surfaced.

## 1. Goal

Long-pressing a message bubble should open a **full-screen overlay** like
Signal's:

- The rest of the chat (message list, top bar, composer) **dims behind a
  scrim**.
- The pressed bubble stays at **full brightness at its on-screen position**
  and does a quick **scale "pulse"** as the overlay opens.
- The **reaction bar floats above** the bubble; the **action menu floats
  below** it. Both are overlay layers — they do **not** get inserted into the
  message stream.
- Tapping the scrim (or Back) dismisses everything with no side effects.

This replaces today's behavior, which inserts the pickers **inline into the
LazyColumn item**, pushing surrounding messages apart and shifting scroll
position (see §2). It also unifies 1:1 and group chats, which currently have
two different long-press experiences.

Reference: Signal Android's long-press interaction (screenshots reviewed
2026-07-11). We copy the *presentation pattern* — scrim + focused bubble +
floating reaction bar + floating menu — not Signal's action set.

## 2. Current state (what to replace)

**1:1 chat — `android/.../chat/ChatScreen.kt`, `MessageBubble` (~line 860):**
- `showMessageActions` is per-bubble `remember` state. When true, a
  `ReactionPickerBar` is composed *above* the bubble and a
  `MessageActionPanel` (Copy / Info) *below* it, inside the bubble's own
  `Column` **inside the LazyColumn item** → the list reflows, neighbors jump,
  and the panels scroll away with the item.
- Tap on the bubble either dismisses the panels or opens the tick-status
  legend dialog (own messages).

**Group chat — `android/.../chat/GroupChatScreen.kt`, `GroupMessageBubble`
(~line 316):**
- Long-press opens `ReactionPickerDialog` (a plain `AlertDialog`). No Copy,
  no Info at all.

**Shared pieces that stay as-is:**
- `REACTION_CHOICES` (`ChatScreen.kt:127`): 👍 ❤️ 😂 😮 😢 🙏
- Reaction wire format & toggle semantics: `MessageInteractions.kt`
  (`ReactionPayload`, `MessageTarget`; sending the same emoji again sends
  `""` to remove — the toggle logic lives at the `MessageBubble` callsite in
  `ChatScreen` ~line 465 and its group equivalent).
- `ReactionRow` (the inline chips under a bubble showing accumulated
  reactions) is unchanged.
- The tick-legend and Message Info `AlertDialog`s are unchanged; only how
  they are *reached* changes.

## 3. Interaction spec

### 3.1 Opening

1. User long-presses a message bubble (any visible kind: text, image
   attachment, voice memo — not the centered `KIND_GROUP_INVITE` system rows,
   which keep no long-press behavior).
2. Haptic: `LocalHapticFeedback.current.performHapticFeedback(HapticFeedbackType.LongPress)`.
3. Overlay opens **immediately** (no delay beyond the platform long-press
   timeout):
   - Full-screen scrim over everything including the top bar and composer:
     `Color.Black.copy(alpha = 0.55f)` in both themes (dimming, not a color
     shift — matches Signal).
   - The focused bubble is re-rendered **undimmed** at the exact screen
     coordinates it occupied (see §4 for how), including its `ReactionRow`
     chips if present.
   - Pulse: the focused bubble animates scale `1f → 1.05f → 1f` with a
     spring (`Spring.DampingRatioMediumBouncy`, ~250 ms total), transform
     origin at bubble center. One pulse on open; it does not loop.
   - Reaction bar and action menu fade/scale in (~150 ms, `tween`) at their
     computed positions (§5).

### 3.2 While open

- **Reaction bar** (floating above the bubble): the 6 `REACTION_CHOICES` in a
  pill-shaped `Surface` (reuse current `ReactionPickerBar` styling:
  `RoundedCornerShape(28.dp)`, `surfaceVariant`, elevation 6.dp, 40.dp
  touch targets).
  - If the user has **already reacted** to this message, that emoji gets a
    highlighted circular background (`primaryContainer`) — tapping it removes
    the reaction (existing `""` toggle path).
  - Tapping any emoji applies/toggles the reaction and dismisses the overlay.
- **Action menu** (floating below the bubble): same items as today, in a
  rounded `Surface` card:
  - **Copy** — enabled iff `messageCopyText(message).isNotBlank()`; copies +
    existing "Copied" toast; dismisses.
  - **Info** — dismisses the overlay, then opens the existing Message Info
    dialog.
  - No new actions. Reply / Forward / Select / Pin / Delete from the Signal
    screenshot are explicitly **out of scope** (§8).
- The list behind the scrim must **not scroll** while the overlay is open
  (the scrim consumes all pointer input).

### 3.3 Dismissing

- Tap anywhere on the scrim → close, no action.
- System Back (`BackHandler`) → close, no action.
- Any action (react / copy / info) → close as described above.
- Close animation: reverse of open (fade scrim + bar/menu out, ~120 ms). The
  bubble copy just disappears — the real bubble underneath was never removed,
  so there is no layout jump.

### 3.4 What plain tap does (unchanged)

- Own message with a tick: tap still opens the tick-status legend dialog.
- Otherwise tap is a no-op. (Remove the current "tap toggles the panels
  closed" branch — it dies with the inline panels.)

## 4. Implementation approach

**State hoisting is the core change.** Today `showMessageActions` is
`remember`ed per LazyColumn item (it silently resets when the item is
recycled off-screen). Instead:

- Hoist to the screen level in both `ChatScreen` and `GroupChatScreen`:

```kotlin
// null = overlay closed
var focused: FocusedMessage? by remember { mutableStateOf(null) }

data class FocusedMessage(
    val target: MessageTarget,      // stable identity (MessageInteractions.kt)
    val boundsInRoot: Rect,         // captured at long-press time
)
```

- Each bubble reports its bounds via
  `Modifier.onGloballyPositioned { coords -> bounds = coords.boundsInRoot() }`
  (kept in a local `var` inside the item; only *read* at the moment of
  long-press, so no recomposition churn) and `onLongClick` sets
  `focused = FocusedMessage(target, bounds)`.
- The overlay itself is composed **at the screen root, as a sibling drawn
  after the `Scaffold`** (wrap the screen in a `Box`; the overlay is the
  second child). Do **not** use `Dialog`/`Popup` — matching
  root-window coordinates is exactly what `boundsInRoot()` gives us, and a
  same-window `Box` keeps IME/insets behavior trivial.
- The overlay re-renders the bubble content by calling the **same extracted
  composable** the list uses. Refactor: split the current bubble drawing into
  `MessageBubbleContent(message, isOwn, tick, contactColor, shape, ...)`
  (pure visual, no click handlers) used by (a) the list item and (b) the
  overlay copy. Do the same split for the group bubble, or — better —
  converge `GroupMessageBubble` onto the shared content composable if the
  diff is small (sender label is the only structural extra).
- Position the copy with `Modifier.offset { IntOffset(bounds.left, y) }`
  inside a full-size `Box`, where `y` comes from `OverlayPlacement` (§5).
- Scrim: full-size `Box` with the scrim color and
  `Modifier.pointerInput(Unit) { detectTapGestures { focused = null } }` so
  it eats scroll + tap.
- `BackHandler(enabled = focused != null) { focused = null }`.
- One overlay implementation shared by both screens — put it in
  `android/.../chat/MessageFocusOverlay.kt` alongside a
  `MessageOverlayActions(canCopy, onReact, onCopy, onInfo, ownReaction)`
  parameter bundle so `ChatScreen` and `GroupChatScreen` wire their own
  callbacks.

**Deletions:** the inline `if (showMessageActions) ReactionPickerBar(...)` /
`MessageActionPanel(...)` blocks in `MessageBubble`, the
`ReactionPickerDialog` usage in `GroupMessageBubble` (delete the composable
if nothing else uses it), and `Modifier.groupMessageActions`.

## 5. Placement logic (pure, unit-testable)

Same pattern as `ConversationLayout.kt` / `MeshStatusTextLogic`: a pure
object with no Compose/Android deps, exercised by JVM unit tests.

```kotlin
// android/.../chat/OverlayPlacement.kt
object OverlayPlacement {
    data class Result(
        val bubbleTop: Float,   // may differ from captured top if shifted
        val barTop: Float,      // reaction bar
        val menuTop: Float,     // action menu
    )

    fun compute(
        bubbleBounds: Rect,     // captured boundsInRoot
        bubbleHeight: Float,    // = bubbleBounds.height
        barHeight: Float,
        menuHeight: Float,
        screenTop: Float,       // status-bar-safe top inset
        screenBottom: Float,    // nav-bar-safe bottom
        spacing: Float,         // gap between bubble and bar/menu (8dp in px)
    ): Result
}
```

Rules (all verified by tests):

1. **Default:** bubble stays where it was; bar sits `spacing` above it, menu
   `spacing` below.
2. **Near top:** if the bar would poke above `screenTop`, shift the *whole
   stack* (bar + bubble + menu) down just enough.
3. **Near bottom:** if the menu would poke below `screenBottom`, shift the
   stack up just enough.
4. **Too tall to fit** (e.g. a 360.dp image bubble on a small screen, or
   keyboard open): pin the bar to `screenTop`, pin the menu bottom to
   `screenBottom`, and place the bubble between them, top-aligned under the
   bar (the bubble copy may extend under the menu; the menu draws on top —
   acceptable, matches Signal's behavior for oversized media).

Horizontal placement needs no logic: bar and menu are start-aligned with the
bubble's edge on its own side (own messages: right-aligned to
`bubbleBounds.right`; others: left-aligned to `bubbleBounds.left`), clamped
to 16.dp screen margins.

## 6. Accessibility

- The bubble keeps a custom accessibility action ("Message options") mapped
  to the long-press handler so TalkBack users can open the overlay.
- Scrim gets `contentDescription = null` but an `onClick` semantics label
  "Dismiss".
- Reaction emojis get content descriptions ("React thumbs up", …); the
  highlighted own-reaction reads "…, selected. Tap to remove".
- Overlay open/close should not trap focus incorrectly: when open, the list
  behind is not focusable (scrim consumes); when closed, focus returns
  naturally (same window, so nothing special needed).
- If system animations are disabled (`animatorDurationScale == 0`), skip the
  pulse; overlay still opens instantly.

## 7. Testing

**Unit (JVM, `android/app/src/test/...`):**
- `OverlayPlacementTest` — the four placement rules in §5, boundary-exact
  (fits-exactly vs. 1px overflow), for both own/other alignment.
- Existing `MessageInteractions` / reaction toggle tests are untouched and
  must stay green.

**Manual checklist (Pixel 7 + Pixel 10 Pro fleet):**
- Long-press: short text bubble mid-screen (1:1 and group) — scrim dims all
  chrome, bubble pulses, bar above / menu below.
- Bubble at very top of the list and at very bottom (just above composer) —
  stack shifts, nothing clipped.
- Tall image attachment — rule 4 pinning.
- With the keyboard open — overlay lays out within the IME-shrunk viewport.
- React, re-react (removal), Copy on an image with/without caption (Copy
  disabled when blank), Info.
- Dismiss via scrim tap and via Back; verify no scroll jump afterward.
- Own-message tap still shows tick legend; group invite rows inert.
- Dark and light theme.

## 8. Non-goals (future work, not this PR)

- New message actions: Reply, Forward, Select, Pin, Delete (each is its own
  feature with protocol implications).
- Signal's "…" expanded full-emoji picker at the end of the reaction bar.
- Swipe-to-reply, multi-select mode.
- iOS parity (track separately once Android ships).
- Any change to reaction wire format, storage, or mesh/relay behavior.

## 9. Files expected to change

| File | Change |
|---|---|
| `android/.../chat/MessageFocusOverlay.kt` | **new** — scrim + bubble copy + bar + menu + animations |
| `android/.../chat/OverlayPlacement.kt` | **new** — pure placement logic |
| `android/.../chat/ChatScreen.kt` | hoist focus state; extract `MessageBubbleContent`; remove inline panels; wire overlay |
| `android/.../chat/GroupChatScreen.kt` | same hoist/wiring; drop `ReactionPickerDialog` + `groupMessageActions`; gains Copy/Info parity |
| `android/app/src/test/.../chat/OverlayPlacementTest.kt` | **new** |

Everything else (reaction encode/decode, `ReactionRow`, dialogs, theme,
mesh code) is untouched.
