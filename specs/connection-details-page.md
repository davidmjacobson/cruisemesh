# Connection details page design specification

Status: Proposed (rev 2)
Platforms: Android and iOS
Scope: Replacement for the existing Connection details screen

## Outcome

The Connection details page must help a person answer three questions without
understanding CruiseMesh internals:

1. Is CruiseMesh able to connect and exchange messages right now?
2. How can each friend currently be reached?
3. If delivery is impaired, what can I do about it?

The page is a user-facing connection-health dashboard. It is not primarily a
transport log or a developer diagnostics screen.

## Problem

The current page exposes useful facts but does not interpret them. In
particular, it can say that the mesh is running and Shore Pass is connected
while showing a red `Pending relay upload` warning beneath nearly every friend.
It can also show that a friend received a message and then immediately show a
pending-upload warning. Those combinations look contradictory even when the
underlying system is behaving normally.

The current People section also places every friend in one long card and gives
active, recently reached, waiting, and never-reached friends equal visual
weight. The user must read every row to find the few that matter.

Some data is live while some is a snapshot. Runtime, direct paths, and relay
health update reactively, but connection history and relay queue counts are
loaded only when the page opens. The page can therefore become stale while a
user watches it during troubleshooting.

## Design principles

- Lead with an interpretation, then provide evidence.
- Describe user outcomes, not internal queues or protocol mechanics.
- Treat an intermittently reachable friend as expected, not broken. Waiting is
  what this product is *for*: messages travel when phones meet, not only when
  the internet is up.
- Use warning colors only when the user may need to act.
- Put the most actionable people first.
- Keep live state visibly fresh — without letting freshness cost
  responsiveness (see Event volume and performance).
- Preserve detailed activity and diagnostics without letting them dominate the
  normal experience.
- Keep Android and iOS behavior and wording equivalent while using native
  platform components. Equivalence is achieved by putting the classification
  in the core, not by mirroring it twice (see State and data requirements).

## Non-goals

- This page does not let users manually select Bluetooth, local Wi-Fi, or Shore
  Pass for message delivery. CruiseMesh continues to choose the path.
- This page does not promise that a particular friend will receive a message
  immediately.
- This page does not expose relay tokens, IP addresses, Wi-Fi names, internal
  peer IDs, message IDs, or cryptographic details.
- This page does not replace the exported diagnostic archive.
- This page does not present group-chat delivery health as its own object in
  v1. Group delivery is per-member underneath; members appear in People like
  anyone else. A grouped roll-up can come later if field evidence asks for it.
- Opening the page changes no radio or sync behavior. The page is a passive
  reader of existing state; the sole exception is pull-to-refresh (see Refresh
  behavior), which the user performs deliberately.

## Information architecture

The page contains these sections in order:

1. Connection health
2. Paths
3. Needs attention, when non-empty
4. Reachable now, when non-empty
5. Other people
6. Recent activity, collapsed by default
7. Troubleshooting and diagnostics, collapsed by default

Empty sections are omitted. The page must not reserve blank cards for states
that do not apply.

Blocked contacts never appear anywhere on this page — not in any People group,
not in Recent activity. A block is a tombstone; surfacing the person here would
undo it.

## Reference layout

All names and times in mocks, screenshots, and snapshot fixtures are
synthetic. This includes this document: never use real family names in a
design artifact that lives in a public repository.

```text
Connection details                                      [Close]

+-------------------------------------------------------+
| Working normally                                      |
| 2 friends nearby · Shore Pass connected               |
| Updated just now                                      |
+-------------------------------------------------------+

Paths
+-------------------------------------------------------+
| Bluetooth       Listening              Ready          |
| Local Wi-Fi     1 active connection    Ready          |
| Shore Pass      Connected              Ready          |
+-------------------------------------------------------+
  CruiseMesh chooses the best available path automatically.

Reachable now (2)
+-------------------------------------------------------+
| Riley's phone                          Local Wi-Fi    |
| Connected now                                         |
|-------------------------------------------------------|
| Sam                                    Bluetooth      |
| Connected now                                         |
+-------------------------------------------------------+

Other people (12)                               [Show]

Recent activity                                  [Show]
Troubleshooting & diagnostics                    [Show]
```

When a genuine delivery problem exists, `Needs attention` appears between
Paths and Reachable now:

```text
Needs attention (1)
+-------------------------------------------------------+
| Alex                                                  |
| 2 messages can't be sent · 14 min                     |
| Alex's saved Shore Pass setup was rejected            |
|                                      [How to fix]     |
+-------------------------------------------------------+
```

## Connection health

The first card is a summary of the entire connection system. It contains:

- a status icon;
- a short status title;
- one evidence line;
- a freshness label; and
- at most one primary action.

This card's classification is not page-local. It is the same core-computed
health state that should drive the in-chat status pill on both platforms, so
the pill and this page can never disagree (see State and data requirements).

### Health states

#### Ready

Use the ready treatment (title on the order of `Working normally`) when the
mesh runtime is active and no blocking condition is known. A lack of currently
nearby friends does not make the system unhealthy; Bluetooth may be listening
for future encounters.

Example evidence:

- `2 friends nearby · Shore Pass connected`
- `Listening for nearby friends · Shore Pass not set up`
- `1 friend nearby · Waiting for internet`

Ready uses the success treatment but must carry an affirmative textual label;
color alone must not communicate the state.

#### Limited

Use `Working, with limits` when CruiseMesh is running and at least one useful
path remains, but another expected path is unavailable or temporarily
degraded.

Examples:

- Bluetooth is available but Shore Pass is waiting for internet.
- Shore Pass is connected but Bluetooth is unavailable.
- Relay syncing has been slowed by the shared family limit and is expected to
  recover automatically.

The card explains which path remains available. It uses the caution treatment.

#### Needs attention

Use `Needs attention` when a user action is required or all useful paths are
unavailable. Examples include:

- the mesh is stopped;
- Bluetooth is off and no working relay path is available;
- Shore Pass is expired or suspended and there is no direct route; or
- our own relay setup was rejected.

The card uses the error treatment and supplies one direct action when the app
can offer one, such as `Start mesh`, `Turn on Bluetooth`, `Manage Shore Pass`,
or `How to fix`.

A problem with one *friend's* saved setup never drives the overall card by
itself — that is a per-person condition and belongs in the Needs attention
People group. The overall card reflects this device's ability to participate.

#### Checking

Use `Checking connections…` only during startup or an active health check. Do
not display a failure state until the relevant check has completed or timed
out. Checking is bounded: after at most 10 seconds without a verdict, resolve
to the best-supported real state rather than staying in Checking forever.
Checking uses a neutral treatment and an indeterminate progress indicator.

## Paths

The Paths section describes **this phone's** delivery paths only. A problem
with a friend's endpoint is that friend's row, never a Paths row — mixing the
two is how the current page manufactures contradictions.

One compact row per path:

| Path | Primary state | Optional detail | Possible action |
| --- | --- | --- | --- |
| Bluetooth | Listening, Off, Starting, or `N active` | Audio-sharing note when relevant | Turn on Bluetooth |
| Local Wi-Fi | No active connections or `N active` | Never show the Wi-Fi name | None |
| Shore Pass | Not set up, Checking, Connected, Waiting for internet, Unreachable, Pass expired, Pass suspended, Setup rejected (ours), Storage full, or Syncing slowed | Relative time of last successful sync when useful | Set up, Manage, Retry, or How to fix |

`Message too large` is not a path state; it is a per-recipient blocked reason
and lives in the delivery states table below.

Rows use a leading path icon, a path name, and a trailing textual state. A row
may expand to show an explanation and action. Normal states remain compact.
The Bluetooth and Local Wi-Fi rows bind to the same observable transport state
the rest of the app uses — never to a cached snapshot that can outlive the
link (the "zombie header" failure class).

The existing explanatory sentence about automatic path selection appears once
below the rows:

`CruiseMesh chooses the best available path automatically.`

## People

### Grouping and order

People are divided into these groups:

1. `Needs attention`
2. `Reachable now`
3. `Other people`

**Reachable now** means: a live direct link to that person exists (Bluetooth
or local Wi-Fi), **or** their relay presence is fresh (seen within the
presence window) *and* this phone's own relay path is currently usable. A
presence-based entry reads `Seen online 4 min ago` with the Shore Pass badge;
a link-based entry reads `Connected now` with its path badge.

Within Needs attention, order by severity and then by the age of the oldest
affected user-visible message. Within Reachable now, order by display name.
Within Other people, order by most recent useful evidence and then by display
name. Friends with no history appear last.

The Other people group is collapsed when it contains more than five rows. Its
control includes the number of hidden people, for example `Show 12 people`.

### Person row

Every person row contains:

- display name;
- a status sentence;
- a path badge when a path is known; and
- an optional delivery line when user-visible messages are waiting.

Examples:

- `Connected now` with a `Local Wi-Fi` badge
- `Connected now` with a `Bluetooth` badge
- `Seen online 4 min ago` with a `Shore Pass` badge
- `Received your message 12 min ago`
- `Last connected yesterday at 8:03 PM`
- `No connection history yet`

Use relative time for events less than 24 hours old. Use `Yesterday` when
applicable and a localized short date and time for older events. Never render
a zero or invalid timestamp as a date.

Selecting a person opens an inline expansion or native detail sheet containing:

- best known route now;
- last seen time;
- last successful delivery or receipt;
- waiting-message status, if applicable; and
- up to five recent connection events for that person.

This expansion is informational. It must not expose a manual transport picker.
"Best known route now" is the core's routing answer, restated in user terms.
The UI never re-derives routing: some friend endpoints are post-only by design
(newer friend cards), and a page that re-derived reachability from "can I poll
it" would wrongly report them broken.

## Delivery and queue language

The main People list must not display the raw phrase `Pending relay upload`.
That is an implementation state, not a user outcome.

A raw relay-upload count is not sufficient evidence that a message is delayed.
In particular, do not show a warning after the page already states that the
friend received the relevant message. Redundant or bookkeeping work may still
exist after user-visible delivery succeeded — leaving durable copies in place
is deliberate, not a failure.

Use these user-facing states:

| Derived state | Copy | Treatment |
| --- | --- | --- |
| Active work with a usable route | `Sending 2 messages…` | Neutral, optional progress indicator |
| No current route to the friend | `2 messages will deliver when you reconnect` | Neutral |
| No internet while relay is the only known route | `2 messages waiting for internet` | Neutral, or caution when the overall card is Limited |
| A usable route exists but progress has not advanced for 10 minutes | `2 messages delayed · 10 min` | Caution |
| A terminal or configuration error blocks the available route | `2 messages can't be sent` plus the reason | Error with an action |
| Delivery receipt covers all relevant messages | No queue line | None |

"No current route" copy deliberately promises future delivery rather than
implying failure: store-and-forward through encounters is the product's core
behavior, and the words must carry that expectation for a non-technical
reader.

**A route is "usable" only by the core's definition**, evaluated per
recipient: a live direct link to that person, or validated internet plus a
resolved relay endpoint that is not currently resting, rejected, or
rate-limited. The 10-minute delayed threshold applies only while a route is
usable under that definition. A friend who is simply offline may remain queued
indefinitely without being classified as an error. Keep the threshold a single
named constant in the core so it can be tuned from field evidence.

Per-message blocked reasons (for the error row) include a rejected saved
setup, an expired or suspended pass, storage full, and message too large. Each
reason maps to specific `How to fix` content (below).

Raw relay queue counts remain available in expanded troubleshooting details
and diagnostic exports.

## "How to fix" content

An error state that offers `How to fix` must resolve to concrete, ordered
steps a family member can follow, written for someone who will not open
settings screens on their own. Required content, per reason:

- **A friend's saved setup was rejected**: explain that the *friend* needs to
  fix their own Shore Pass first, and only then share a fresh friend card,
  which you then rescan. The order matters — a card re-shared before the pass
  is fixed reproduces the problem — and the copy must make the order
  unmistakable.
- **Our pass expired or suspended**: route to Manage Shore Pass.
- **Storage full**: explain that space frees as friends collect their
  messages, and what to do if it persists.
- **Message too large**: name the affected conversation and the size limit in
  plain terms.

`How to fix` may expand the Troubleshooting section and scroll to the relevant
explanation, or present a sheet — but the user must never be dropped at the
top of a long section to search for the answer themselves.

## Recent activity

Recent activity is collapsed by default. Its collapsed row says `Recent
activity` and, when available, includes the newest event time.

When expanded:

- show the newest ten events;
- retain the existing directionally correct wording for sent and received
  messages;
- show the observed path only when a path was actually observed — a message
  that arrived carried through the mesh must not invent Bluetooth, local
  Wi-Fi, or Shore Pass as its final-hop path;
- provide `Show all activity` when more events exist; and
- update while the screen remains visible.

Activity is evidence for curious users and support, not the primary health
signal. It must appear below the grouped People section.

## Troubleshooting and diagnostics

The existing support controls move into a collapsed `Troubleshooting &
diagnostics` section at the bottom of the page. Expanding it reveals:

- diagnostic logging toggle and its explanation;
- Share diagnostics;
- Delete captured diagnostics;
- Clear connection history; and
- the existing privacy explanation.

Destructive actions retain confirmation dialogs. Sharing diagnostics retains
the single archive containing all captured diagnostic categories.

## Refresh behavior

The page must remain accurate while visible — and must remain responsive
while accurate. These are requirements of equal rank.

- Runtime, direct-path, push, relay-health, stale-contact, last-seen, and
  presence signals update immediately from their existing observable state.
- Connection summaries, recent events, and delivery state refresh after their
  underlying store data changes, via a store-change signal.
- Provide pull-to-refresh on both platforms. Pull-to-refresh reloads the
  snapshot queries **and** requests one bounded connectivity re-check (the
  same single sync pass the app would run on a network change). This is the
  one deliberate "check again now" affordance on the page, and the only way
  the page influences radio or sync behavior.
- Display `Updated just now`, `Updated 1 min ago`, and similar relative copy
  in the health card. The freshness label itself updates at least once per
  minute.
- Stop all page-driven observation and polling when the page is not visible.

Loading a refresh must not replace valid content with a blank screen. Keep the
last successful snapshot visible and show a compact progress indicator.

## Event volume and performance

This page sits on top of the same store and the same event stream that, at
mesh-flood rates, has previously driven the app into input-dispatch ANRs via
undebounced change signals reloading on the main thread. The design must
assume thousands of store events per minute as a normal condition, not a
stress test:

- **Coalesce**: store-change signals are debounced/coalesced (on the order of
  500 ms) before triggering any reload. N events inside a window cost one
  reload.
- **Never on main**: no store query runs on the main/UI thread — ever. Reload
  work executes on a background queue and posts finished view state.
- **Single flight**: at most one snapshot reload is in progress at a time; a
  signal arriving mid-reload schedules exactly one follow-up.
- **Bounded queries**: every query this page runs is LIMIT-bounded (people
  page sizes, ten activity events); nothing scales with total history size.
- If a polling fallback is used before the store-change signal exists (every
  five seconds while foregrounded is acceptable), the same three rules apply,
  and a tick is skipped when a reload is already in flight.

## State and data requirements

The current platform state already supplies most of the required inputs:

- mesh runtime state;
- live Bluetooth and local-Wi-Fi paths (observable, not snapshot);
- Shore Pass health and last successful sync;
- relay push health;
- stale friend relay setups (rejection and silence streaks);
- contact and relay-presence last-seen times;
- peer connection summaries and events; and
- per-recipient relay queue depth.

The main missing input is the age and user-visible delivery meaning of queued
work. Extend the read model so the UI can determine, per recipient:

- count of user-visible messages not yet covered by a delivery receipt;
- oldest such message timestamp;
- last progress timestamp;
- whether a currently usable route exists (by the core's definition above);
  and
- the blocking reason, when one is known.

Do not make the UI infer delivery solely from the raw relay queue depth. Add a
new user-facing delivery-status record rather than overloading the diagnostic
relay-depth record with increasingly ambiguous fields.

**Classification lives in the core, not "when practical."** The health-state
decision, the per-recipient delivery-state decision, the route-usability
predicate, and the delayed threshold are pure core functions exported to both
shells, with table-driven tests in Rust. The shells render; they do not
decide. This is also what makes the health card reusable as the single source
for the status pill on both platforms — closing the existing gap where the
two platforms' pills disagree about relay health.

## Copy and localization

- Every user-facing string on this page lives in the platform string
  resources (`strings.xml` / `Localizable.xcstrings`); the localization gate
  rejects hardcoded literals, and this page must land gate-clean.
- House style applies: sentence case; literal status and error copy; no
  protocol jargon. Words like relay, envelope, hop, TTL, cursor, queue,
  frontier, and token never appear on the normal page. `Shore Pass` is the
  product name for the relay feature and is the only sanctioned way to refer
  to it.
- Counts are pluralized through proper plural resources on both platforms,
  not string concatenation.

## Visual design

- Use the platform background and surface colors; support light and dark mode.
- Use one card for Connection health and one compact card for Paths.
- Use individual rows with separators for people. Do not place the entire
  address book in one visually undifferentiated block.
- Use success color only for active readiness, caution for degradation, error
  for actionable failure, and the normal secondary text color for expected
  waiting.
- Pair every color with an icon and textual label.
- Use a small outlined badge for `Bluetooth`, `Local Wi-Fi`, or `Shore Pass`.
- Keep primary body text at the platform default readable size. Do not use
  caption styling for all person details.
- Maintain at least 44-by-44-point iOS and 48-by-48-dp Android interactive
  targets.

## Accessibility

- Screen readers announce the health title before its evidence.
- Each person row has one concise combined accessibility label, for example:
  `Riley's phone. Connected now via local Wi-Fi.`
- Expanded state is announced for collapsible groups and rows.
- Status is never communicated by color alone.
- Progress indicators have labels and do not announce continuously.
- Dynamic Type and Android font scaling must not truncate names, status text,
  queue age, or actions at 200 percent scaling.
- Keyboard, Switch Control, and TalkBack/VoiceOver traversal follows the
  visual top-to-bottom order.

## Privacy

- The normal page may display friend names, path type, event type, and
  localized times because these already form part of the visible connection
  history.
- Never display relay tokens, IP addresses, Wi-Fi names, internal IDs,
  message IDs, or message content.
- Blocked contacts are absent from every section of this page.
- Preserve the existing diagnostic privacy disclosure and deletion behavior.
- Screenshots, mocks, and snapshot-test fixtures use synthetic names and
  synthetic timestamps — including in this document and its successors.

## Platform behavior

Android uses Material 3 components and the existing top app bar/back behavior.
iOS uses SwiftUI List/Section behavior and the existing navigation/close
behavior. Section order, state classification, copy meaning, and warning
severity must remain equivalent across platforms even when native controls
differ visually — with equivalence enforced by shared core classification
rather than by convention.

## Acceptance criteria

### Information hierarchy

- The first visible card always provides one overall health interpretation.
- A healthy page does not show red queue text merely because relay work
  exists.
- Reachable friends appear before inactive friends.
- More than five Other people are collapsed by default.
- Recent activity and diagnostics are collapsed by default.
- A single friend's rejected setup surfaces in that friend's row and the
  Needs attention group — never as a Paths row and never, alone, as the
  overall health state.

### State correctness

- `Received your message` is never paired with an error for already satisfied
  delivery.
- Waiting for an offline friend is not classified as a failure based on age
  alone, at any age.
- A terminal Shore Pass error produces an actionable reason.
- A carried event never invents Bluetooth, local Wi-Fi, or Shore Pass as the
  path to the final friend.
- A post-only friend endpoint is not classified as broken for being
  unpollable.
- Invalid or zero timestamps never render as a real date.
- Blocked contacts appear nowhere on the page.
- The health card and the status pill can never display contradictory states,
  because they consume the same core classification.

### Freshness and performance

- A new direct connection updates within one second of the observable path
  state changing.
- A newly recorded connection event appears within five seconds while the
  page remains open.
- A cleared or delivered queue line disappears within five seconds.
- Pull-to-refresh reloads history, activity, and delivery state, and requests
  exactly one bounded connectivity re-check.
- Navigating away stops page-specific polling and observation.
- With the page open during a sustained mesh flood (thousands of store events
  per minute), input dispatch stays responsive: no store query on the main
  thread, coalesced reloads, single-flight refresh. This is exercised by an
  automated test that pumps synthetic store-change events, not by hope.
- Opening the page causes no scan, sync, or advertising change.

### Copy, accessibility, and layout

- Every string passes the localization gate; no hardcoded literals.
- The page remains usable at 200 percent text scaling.
- Every status has a non-color label.
- All actions meet platform minimum target sizes.
- Android and iOS screenshot tests cover Ready, Limited, Needs attention, a
  long People list, dark mode, and large text — with synthetic fixture data.

## Recommended delivery sequence

### Phase 1: truthful and scannable

- Add the health card (backed by the core health classification) and Paths
  rows bound to observable transport state.
- Group people into Reachable now and Other people; exclude blocked contacts.
- Remove red styling and the raw `Pending relay upload` phrase from normal
  person rows.
- Collapse activity and diagnostics.
- Add visible freshness and foreground refresh with the coalescing,
  off-main-thread, single-flight rules in place from day one — the polling
  fallback is acceptable, the performance rules are not optional.

### Phase 2: actionable delivery health

- Add the per-recipient delivery-status read model in the core, with
  table-driven classification tests.
- Add Needs attention classification, queue age, blocking reasons, actions,
  and the per-reason How-to-fix content.
- Add the per-person detail expansion.
- Point the status pill at the shared health classification on both
  platforms.

### Phase 3: validation and tuning

- Tune the delayed threshold from local-only field metrics.
- Validate wording with non-technical users who have experienced intermittent
  shipboard connectivity.
- Confirm Android/iOS parity with shared scenario fixtures and screenshot
  tests.

---

## Appendix: changes from the previous draft

For reviewers diffing against the prior proposal:

1. **Performance is now a first-class requirement.** The prior draft's
   "publish a store-change signal and reload" is, undebounced and on the main
   thread, the exact mechanism behind a recent field ANR loop. Added the
   Event volume and performance section (coalescing, never-on-main,
   single-flight, bounded queries) plus a flood-soak acceptance test.
2. **Paths now means our paths.** `Setup rejected` for a *friend's* saved
   card moved out of the Shore Pass path row into the person row / Needs
   attention group; `Message too large` moved from path state to per-recipient
   blocked reason. Added the rule that one friend's problem never drives the
   overall card.
3. **Core-first is mandatory, not "when practical."** Health, delivery
   classification, route usability, and thresholds are core functions with
   Rust table tests; shells only render. The health state is shared with the
   status pill, closing a known cross-platform pill divergence.
4. **Delay-tolerant delivery is in the copy model.** "Waiting for a
   connection" became "will deliver when you reconnect"; carried arrivals
   keep honest pathing; queued-for-offline-friend is explicitly never an
   error at any age.
5. **"Usable route" is defined** (live direct link, or validated internet +
   resolved endpoint not resting/rejected/rate-limited) instead of "believed
   usable."
6. **Reachable now is defined** to include fresh relay presence only while
   our own relay path works, and specifies which status sentence each entry
   type gets.
7. **Blocked contacts are excluded everywhere** (tombstone semantics).
8. **Post-only friend endpoints** can't be misread as broken; the UI consumes
   the core's routing answers rather than re-deriving reachability.
9. **How-to-fix content is specified per reason**, including the
   order-sensitive friend-card repair flow (friend fixes pass → re-shares
   card → you rescan).
10. **Page opening is side-effect-free**; pull-to-refresh is the one
    deliberate re-check and additionally triggers a single bounded sync pass.
11. **Checking is bounded** (10 s) so it cannot become a resting state.
12. **Localization and copy-tone requirements added** (resource-only strings,
    plural resources, jargon ban) matching the CI gate.
13. **Group chats scoped out explicitly** for v1.
14. **Real family names removed from mocks**; synthetic-data rule extended to
    design documents themselves.
