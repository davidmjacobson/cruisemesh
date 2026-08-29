# Android UI regression testing

The suite deliberately uses three layers. Most interaction and semantics tests
run on the JVM with Robolectric, deterministic Compose previews protect a small
set of critical layouts, and a thin managed-emulator suite catches integration
failures that a host process cannot model (activity startup, system insets, the
IME, and packaged JNI).

## What is gated

- Host tests cover terms acceptance, the complete onboarding path, group
  creation, composer state transitions and 48dp touch targets, one-shot deep
  links, and back/exit navigation.
- Screenshot baselines cover terms, onboarding, and the message composer at a
  compact 360dp width, including 1.3x font scale variants.
- The managed Pixel 6/API 36 test cold-starts the real activity, crosses terms
  into onboarding, and checks the composer while the software keyboard is up.
  Pixel 2/API 31 (the minimum supported OS) runs nightly.

Tests should assert user-visible state or semantics, not implementation details.
Add a host interaction test for every reproducible UI regression. Add a
screenshot only when geometry, wrapping, clipping, or overlap is the failure;
keep the screenshot matrix intentionally small.

## Local commands

First generate host bindings as documented in `AGENTS.md`, then:

```sh
cd android
./gradlew :app:testDebugUnitTest
./gradlew :app:validateDebugScreenshotTest
```

When an intentional visual change is reviewed, regenerate and inspect the PNGs:

```sh
./gradlew :app:updateDebugScreenshotTest
```

The references live in `android/app/src/screenshotTestDebug/reference/`.
Never update them merely to make a red test green; the image change is the
review artifact.

Every preview in `src/screenshotTest` needs `@PreviewTest` alongside `@Preview`:
since screenshot plugin alpha10 that annotation is the only thing that makes a
preview collectable, and a preview without it is rendered by nobody while
`validateDebugScreenshotTest` still reports success. That is not a hypothetical
— the gate spent several days reporting green while rendering zero of these
previews. `:app:verifyScreenshotPreviewsCollected` now runs after every
validation and fails unless the previews declared, the reference images
committed, and the testcases in
`app/build/test-results/validateDebugScreenshotTest/*.xml` are the same number.
Read that count, not the build status, when judging whether the gate ran.

Device tests need all ABI libraries and matching generated bindings:

```sh
core/build-android.sh
cd android
./gradlew :app:pixel6Api36DebugAndroidTest \
  -Pandroid.testoptions.manageddevices.emulator.gpu=swiftshader_indirect
```

Use `pixel2Api31DebugAndroidTest` to reproduce the nightly minimum-SDK lane.
Android Test Orchestrator clears app data between tests, and animations are
disabled to keep failures reproducible.
