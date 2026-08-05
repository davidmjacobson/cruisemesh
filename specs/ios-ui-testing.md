# iOS UI test strategy

Status: initial application gate implemented

The repository now has isolated launch scenarios, an XCUITest target, the P0
bootstrap/navigation/friend-keyboard/chat/contact smoke tests, `.xcresult`
artifacts in macOS CI, and a Windows workflow launcher. The snapshot golden set,
broader scenario inventory, accessibility audit, and nightly device matrix
remain the later rollout stages described below.

The goal is not to maximize UI-test count. The goal is to make a blank screen,
an unreachable action, a broken critical flow, or a badly damaged layout very
unlikely to reach a person without making CI slow and unreliable.

## Decision

Use three layers, all executed by Xcode on macOS:

| Layer | Runs where | What it protects | When it runs |
|---|---|---|---|
| Swift/XCTest logic and state tests | macOS runner, iOS Simulator | View state, navigation decisions, formatting, and event contracts | Every PR |
| SwiftUI snapshot tests | macOS runner, iOS Simulator | Clipping, overlap, missing content, color scheme, Dynamic Type, and localization regressions | Every PR |
| XCUITest application smoke tests | macOS runner, iOS Simulator | Real app launch, lifecycle, navigation, sheets, keyboard, deep links, and packaged Rust framework | Every PR; broader matrix nightly |

There is no honest Windows-native version of the last two layers. Xcode, the
iOS Simulator, SwiftUI's Apple-platform renderer, and XCUIAutomation require
macOS. Windows can still be the developer's control plane: a PowerShell helper
pushes the current branch, dispatches the macOS GitHub Actions workflow, waits
for it, and downloads the `.xcresult`, screenshots, and logs. See
[Running from Windows](#running-from-windows).

Do not use a browser rendering of SwiftUI, a reimplementation of the app in a
cross-platform toolkit, or an Android screenshot as an iOS visual gate. Those
can test shared ideas but cannot see iOS navigation, safe areas, Dynamic Type,
the software keyboard, or SwiftUI layout.

Do not put BLE or end-to-end delivery into this suite. Core tests protect the
protocol, while background Bluetooth and real peer discovery require physical
devices. One Simulator UI test cannot honestly replace either.

## Why this split fits this repository

The current project has a `CruiseMeshTests` unit-test target and broad XCTest
coverage, but no UI-test target, snapshot dependency, test plan, UI-test launch
mode, or `accessibilityIdentifier` usage. `.github/workflows/ios.yml` already
builds `cruisemesh_core.xcframework`, generates the Xcode project, chooses a
Simulator, and runs the unit suite on `macos-latest`. The expensive native setup
therefore exists; the UI layers should extend it rather than create a parallel
build path.

The UI is a good fit for deterministic state testing. The root selects Terms,
Onboarding, or the chat list, and the app contains explicit SwiftUI surfaces
for the composer, contact details, settings, Shore Pass, groups, backup, and
photo viewing. At the same time, several views directly reach stores,
permissions, singleton controllers, sheets, and radios. Tests need a small
production seam around those dependencies, not elaborate taps that attempt to
manufacture mesh state through the real network.

The initial suite should protect these known risk classes:

| Risk | Test that should fail |
|---|---|
| Root state chooses the wrong Terms/Onboarding/Home surface | XCUITest cold-launch journey and focused state test |
| A navigation or sheet dismissal leaves no usable content | XCUITest navigation smoke plus a root-content assertion |
| A consumed friend or relay deep link returns after relaunch | XCUITest URL/lifecycle test |
| Keyboard or safe-area insets hide a primary action | Compact-device XCUITest and snapshot |
| A changing reachability state leaves a stale chat header | State contract test plus scenario-driven XCUITest |
| Message actions, reply UI, or the composer overlap | Snapshot and XCUITest interaction test |
| Large text truncates setup, destructive, or recovery actions | Accessibility-size snapshot and nightly XCUITest |
| A system permission or radio makes a test nondeterministic | Test launch mode fails closed if a real service starts |

Pure logic tests remain valuable. UI tests should prove that a person can see
and use the result of that logic, not repeat every input permutation already
covered by `CruiseMeshTests`.

## Test seams to add

Prefer one explicit application test mode over scattered `ProcessInfo` checks.

1. Add a `UITestConfiguration` parsed once from launch arguments and launch
   environment. It owns the scenario name, locale, clock, animation setting,
   and requested app state.
2. Add `--ui-testing` and `--ui-scenario <name>` arguments. In this mode use an
   isolated, cleared persistence namespace and temporary core database for
   every launch. Never read or delete a developer's normal app data.
3. Inject deterministic adapters for identity, app/core store, permissions,
   notification registration, clock, photos/camera, relay state, and mesh
   status. Bluetooth, LAN, relay polling, notifications, MetricKit, and audio
   capture must not start in UI-test mode.
4. Fail the test build immediately if a UI scenario tries to use a live radio,
   network, permission prompt, current clock, or random identity. Silent
   fallback to production services turns failures into flakes.
5. Keep side effects in route/container views. Extract state-oriented content
   only where a view currently cannot be constructed with immutable state and
   event callbacks. Existing explicit view APIs should remain as-is when they
   are already testable.
6. Put fixed fixture builders in test support: identities, stable IDs, names,
   timestamps, messages, image bytes, reachability, delivery state, groups,
   and Shore Pass results. The app and snapshot suites should share the
   scenario vocabulary without sharing mutable state.
7. Give each launch exactly one initial scenario. XCUITest runs in a separate
   process and must not reach into app memory or seed `UserDefaults` from the
   test runner after launch.

Recommended initial scenario names are:

- `terms`, `onboarding-name`, and `home-empty`;
- `home-populated`, `chat-rich`, `chat-late-arrival`, and `chat-offline`;
- `friends-populated`, `new-group-selected`, and `contact-verified`;
- `settings`, `shore-pass-unconfigured`, `shore-pass-checking`,
  `shore-pass-ready`, and `shore-pass-failed`;
- `backup-ready` and `backup-failed`.

For selectors, prefer visible labels, button roles, selected/enabled state, and
accessibility values. These assertions improve VoiceOver at the same time. Add
stable `.accessibilityIdentifier(...)` values only where labels are ambiguous
or localized:

- top-level screen roots;
- chat and message lists;
- a message row identified by its stable fixture key;
- the composer, reply strip, message actions, and photo viewer;
- non-text loading, empty, and error containers.

Do not identify every view or select by element index. Tests coupled to the
exact SwiftUI hierarchy make harmless refactors expensive and say little about
what a person can actually use.

## Layer 1: Swift/XCTest logic and state tests

Location: `ios/CruiseMeshTests/`

Keep the current XCTest suite as the broad, fast foundation. Add focused state
and event-contract tests as views are split into container and content types.
These do not replace native rendering; they cheaply cover permutations that
would be wasteful to drive through XCUITest.

### P0 state inventory

- Root state maps acceptance and onboarding state to exactly one surface.
- Completing onboarding replaces onboarding with Home.
- Invalid or deleted contact/group destinations resolve to safe navigation.
- A handled friend or relay URL is consumed once.
- Chat reachability changes publish a new header model.
- Whitespace-only text is not sendable; text or an attachment is sendable.
- Cancelling a reply removes the reply model without altering the draft.
- Late-arrival policy preserves a reader's scroll position and exposes the New
  messages action when appropriate.
- Destructive contact/group actions require an explicit confirmation state.
- Shore Pass and backup errors map to a named recovery state, never an empty or
  generic dead end.

State tests should have no sleeps, no live singleton, and no dependency on
Simulator permissions. If a test must launch the application to make its
assertion, it belongs in Layer 3.

## Layer 2: SwiftUI snapshot regression tests

Location: `ios/CruiseMeshSnapshotTests/`

Use one pinned snapshot framework that renders SwiftUI through
`UIHostingController` on an iOS Simulator. `swift-snapshot-testing` is the
recommended starting point; do not add a second golden-image framework beside
it. Store reference images under
`ios/CruiseMeshSnapshotTests/__Snapshots__/`.

Run validation on one pinned Xcode and Simulator runtime in CI. Snapshot pixels
can change across Xcode, OS, font, and host-renderer versions, so a developer's
different local toolchain is diagnostic, while the canonical CI runner is the
gate. Change the pinned image deliberately and re-review baselines when the
toolchain is upgraded.

Golden images are review artifacts, not snapshots to update blindly. A pull
request changing a reference must upload actual, reference, and diff images,
and the reviewer must confirm that the change is intended.

### Initial golden set

Keep the first set small enough that people inspect every diff:

| Surface | State | Required variants |
|---|---|---|
| Terms | first launch | compact iPhone; accessibility text size |
| Onboarding | name entry and permission explanation | compact iPhone; accessibility text size |
| Chat list | empty | compact iPhone |
| Chat list | populated, unread, offline warning | standard iPhone light/dark |
| One-to-one chat | replies, reactions, ticks, late arrival, long text | standard light/dark; compact accessibility text |
| Composer | reply plus attachment | compact iPhone; accessibility text size |
| Message actions | message near top and near bottom | compact iPhone |
| Contact details | verification expanded and destructive actions | compact iPhone; accessibility text size |
| Friends/new group | populated and selected | compact iPhone |
| Settings | normal and failing Shore Pass indicator | standard iPhone light/dark |
| Shore Pass | unconfigured, checking, ready, rejected | compact iPhone; accessibility text for failure |
| Backup/restore | ready and error | compact iPhone; accessibility text size |

Specify fixed point sizes, display scale, `colorScheme`, locale, layout
direction, and Dynamic Type category. Freeze the clock and animations. Never
put a current timestamp, random ID, live status, or device-specific safe-area
value into a baseline.

Snapshots are state-oriented rather than one image per screen. Add a baseline
when layout meaningfully differs: a sheet, expanded section, error, empty
state, keyboard-adjacent surface, or alternate size. A wording permutation
alone usually belongs in a behavior assertion.

## Layer 3: XCUITest application smoke tests

Location: `ios/CruiseMeshUITests/`

Add a `bundle.ui-testing` XcodeGen target hosted by `CruiseMesh`, include it in
the `CruiseMesh` scheme/test plan, and use `XCUIApplication` plus XCTest's UI
automation APIs. Launch the real app with a deterministic scenario; do not
replace the app entry point with a test-only demo application.

### P0 behavior inventory

#### Bootstrap and navigation

- A fresh launch exposes Terms as the only actionable root.
- Accepting Terms reveals onboarding and does not reveal Home early.
- Completing onboarding reveals Home; relaunch does not return to onboarding.
- Every visible Home destination opens and its Back/Done action returns to a
  usable Home root.
- Cancelling every presented sheet returns to the presenting surface.
- An invalid or deleted contact/group destination exits safely rather than
  displaying an empty root.
- Friend and relay setup URLs open the correct destination once. Relaunch and
  later navigation do not reopen a consumed URL.

#### Chats and messages

- Empty and populated chat lists expose the correct primary actions.
- A scenario-driven reachability change updates the visible chat header without
  reopening the conversation.
- Typing nonblank text enables Send; clearing it restores the voice action; an
  attachment alone is sendable; whitespace alone is not.
- Send produces one visible outgoing message and clears the composer once.
- Reply can be cancelled without losing the draft or obscuring the composer.
- Long-pressing a message opens actions; dismiss restores the conversation;
  cover the first and last visible fixture messages.
- A late incoming fixture message auto-scrolls only in the near-bottom case;
  otherwise New messages is visible and usable.
- Photo viewer opens and closes and has a meaningful accessible label.
- Destructive contact/group actions require confirmation and Cancel is safe.

#### Friending, groups, settings, and backup

- Add Friend exposes manual entry, scanning, and My Card without dead ends.
- With the keyboard visible on a compact phone, Preview friend remains
  hittable.
- New Group cannot create an empty group and reflects selected contacts.
- Contact details expands verification without collapsing the sheet.
- Settings toggles render their seeded state and visibly change once.
- Shore Pass renders unconfigured, checking, ready, and failed states with a
  recovery action where one exists.
- Backup/restore surfaces success and typed failure states, never a blank or
  generic dead end.

### Shared assertions and failure evidence

Add small helpers instead of duplicating polling and geometry math:

- `assertHasUsableContent()` — a top-level root exists with a heading or named
  primary surface and at least one usable action, progress indicator, or
  explicit empty state;
- `assertExistsAndIsHittable()` — wait for existence, then verify a critical
  action can receive input;
- `assertInsideWindow()` — a critical frame is contained by the app window;
- `assertNoIntersection(with:)` — for composer, reply strip, overlays, keyboard
  avoidance, and bottom setup actions.

At the start of each test set `continueAfterFailure = false`. On failure attach
a named screenshot, the relevant accessibility hierarchy, launch scenario,
locale, content-size category, OS/runtime, and current root/destination. Always
write an explicit `.xcresult` bundle using `-resultBundlePath`.

Do not use fixed sleeps. Use XCTest expectations and predicates such as
`waitForExistence(timeout:)`, and expose a deterministic UI state when the app
owns asynchronous work. Do not accept permission alerts by localized button
text in ordinary tests. Seed authorization through adapters and separately keep
one small system-dialog test only when the hand-off itself is the requirement.

### Device matrix

| Trigger | Destination | Purpose |
|---|---|---|
| Every PR | one pinned current iPhone Simulator, portrait, normal text | Packaging, launch, navigation, sheets, keyboard |
| Nightly | compact iPhone Simulator, iOS 16, accessibility text | Minimum deployment target and constrained layout |
| Nightly | pinned current iPhone and iPad, dark mode; one landscape pass | Current platform, size class, theme, rotation |
| Before a tester release | physical iPhones, plus iPhone/Android pair where needed | BLE/LAN, permissions, notifications, background behavior; outside this UI suite |

Use destination names only for a developer's convenience. CI should discover a
matching available Simulator and pass its UDID to `xcodebuild`, as the existing
iOS workflow does.

## Accessibility gate

Make ordinary UI tests accessibility tests too: every actionable icon has a
nonempty accessible name, toggles expose their value, decorative images are
hidden, headings are discoverable, and focus order follows task order. Test at
least one P0 journey with an accessibility Dynamic Type category and assert
that primary actions remain present and hittable.

Add an Accessibility Inspector audit and manual VoiceOver, Voice Control, and
Switch Control pass to the release checklist. XCUITest selectors benefit from
good accessibility metadata, but successful automation is not proof that the
spoken experience or focus order is good.

## Running from Windows

### Supported path: Windows controls a macOS runner

The existing `.github/workflows/ios.yml` exposes `workflow_dispatch`, and the
PowerShell wrapper lives at `tools/run-ios-ui-tests.ps1`. From Windows, a
developer can run:

```powershell
tools/run-ios-ui-tests.ps1 -Suite ui
```

The helper does the following:

1. Verify `git`, GitHub CLI (`gh`), authentication, repository identity, and a
   clean or explicitly acknowledged worktree.
2. Require the commit under test to exist on a remote branch. It must never
   silently test the default branch when local commits are unpushed; `-Push`
   explicitly pushes a clean worktree's `HEAD` first.
3. Dispatch `ios.yml` for the exact branch and suite, record the returned
   run by head SHA/event, and reject an ambiguous run instead of watching the
   wrong commit.
4. Run `gh run watch <run-id> --exit-status`.
5. Download `ios-test-results` with `gh run download` into a commit-specific,
   git-ignored directory under `tmp/ios-ui/` and print the result summary and
   paths.
6. Return a nonzero exit code when dispatch, build, tests, or artifact download
   fails.

The first dispatchable workflow must land on the default branch before GitHub
will allow arbitrary branches to invoke it. After that, branch code and tests
can be selected with `--ref`/the workflow's `ref`. Pull requests remain the
normal automatic path; the helper is for Windows developers who want an
on-demand result before or during review.

The same protocol can target a self-hosted Mac if hosted-runner time or queue
latency becomes a problem. Keep the workflow interface and artifacts identical
so callers do not care which Mac executed it. A remote Mac reached through SSH
is also valid, but it should run the same checked-in script used by CI rather
than a second handwritten command sequence.

### What Windows can run locally

Windows can still run shared Rust tests and non-Apple validation such as YAML,
fixture, localization-catalog, and golden-manifest checks. Swift code that only
uses cross-platform Foundation can potentially be moved into a Swift package
and tested with the Windows Swift toolchain, but the current app target imports
SwiftUI, UIKit, CoreBluetooth, and XCTest's Apple UI automation. Moving a small
amount of pure logic would not make the iOS UI suite Windows-native.

Do not advertise Docker, a macOS virtual machine on ordinary PC hardware, `act`,
or a third-party web emulator as equivalent. Containers share the Windows/Linux
kernel and cannot supply Apple's Simulator runtime; GitHub's `macos-*` jobs are
real remote macOS runners, not locally emulated Actions jobs.

## CI layout

### `ios-test` — required on every relevant PR

Keep the existing `.github/workflows/ios.yml` unit job or factor its setup into
a reusable workflow:

1. Select a pinned stable Xcode version and install the required Rust targets
   and XcodeGen.
2. Run `core/build-ios.sh` and the generated-bindings drift check.
3. Generate `CruiseMesh.xcodeproj` from `ios/project.yml`.
4. Build for testing once, then run `CruiseMeshTests` on the selected Simulator.
5. Write and upload the `.xcresult` even on failure.

### `ios-ui-snapshot` — required on every relevant PR

1. Reuse the built app/test products when practical.
2. Validate the snapshot target on the canonical Simulator/toolchain.
3. Upload the `.xcresult` always; on failure also upload reference, actual, and
   diff PNGs in a directly browsable artifact.

### `ios-ui-device` — required on every relevant PR

1. Run only the P0 XCUITest plan on the pinned PR Simulator.
2. Use `-resultBundlePath`, disable test parallelization for scenarios that
   share Simulator services, and keep every scenario's data isolated.
3. Upload the `.xcresult`, JUnit summary if generated, screenshots,
   accessibility hierarchy, app logs, and scenario metadata on failure.
4. The nightly workflow runs the broader device/configuration matrix.

A failure is not made green by blind retry. Xcode may retry only after a test
has a tracked flake diagnosis, and both the first failure and retry evidence
must remain visible. A flaky gate gets a short repair deadline or is removed
from the required plan until it is deterministic.

## Rollout

Implement this in four reviewable changes:

1. **Deterministic foundation:** add `UITestConfiguration`, dependency
   adapters, fixture scenarios, accessibility identifiers, and state-contract
   tests. Prove UI-test mode cannot start live services or touch normal data.
2. **Application gate:** add the XcodeGen UI-test target, P0 test plan, shared
   assertions/failure attachments, and bootstrap/navigation/composer/deep-link
   smoke tests.
3. **Visual gate:** add the pinned snapshot dependency, content seams, initial
   golden set, canonical runner, and actual/reference/diff artifacts.
4. **Windows and matrix:** add `workflow_dispatch`, the PowerShell wrapper,
   `.xcresult` uploads, and nightly iOS 16/large-text/current/iPad variants;
   document the physical-device release pass.

The suite is established when every production top-level screen has at least
one state assertion or reviewed golden, every critical journey has an XCUITest
path, each risk class above is protected by a test that fails when the defect is
deliberately reintroduced, and a Windows developer can launch the exact macOS
gate and retrieve its evidence with one command.

After that, the maintenance rule is simple: every UI regression fix adds a test
at the lowest layer capable of seeing it, and every new top-level screen adds a
deterministic fixture plus its critical interaction test.

## References

- [Apple: Testing](https://developer.apple.com/documentation/xcode/testing)
- [Apple: Adding tests to an Xcode project](https://developer.apple.com/documentation/xcode/adding-tests-to-your-xcode-project)
- [Apple: Organizing tests with test plans](https://developer.apple.com/documentation/xcode/organizing-tests-to-improve-feedback)
- [Apple: Running tests and interpreting results](https://developer.apple.com/documentation/xcode/running-tests-and-interpreting-results)
- [GitHub: Manually running a workflow](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow)
- [GitHub: Downloading workflow artifacts](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/download-workflow-artifacts)
- [swift-snapshot-testing](https://github.com/pointfreeco/swift-snapshot-testing)
