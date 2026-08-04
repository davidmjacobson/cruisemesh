# CruiseMesh Onboarding Script

> **APPROVED and SHIPPED (T5).** Signed off by David 2026-07-25 with one
> change — the internet claim is now qualified (Slide 2) — and wired into
> `strings.xml` / `Localizable.xcstrings` on both platforms in the same PR, so
> this document and the shipped copy cannot drift. **Change one, change the
> other.** One deviation from the draft is recorded at Slide 3.

## The one idea

**CruiseMesh uses virtually every form of connectivity your phone has to get
your messages through.** Every slide should reinforce that, and each permission
slide says *what the permission buys*, not what breaks without it.

---

## Slide 1 — Welcome

Title: **Messages that find a way through**

Body:
CruiseMesh gets your messages to friends and family using virtually every
connection your phone has — Bluetooth, local Wi‑Fi, and even handing them
phone to phone when there's no signal at all.

> The draft listed "the internet" here. Removed: internet delivery needs a
> Shore Pass or a self-hosted relay, so listing it among the connections
> CruiseMesh "has" would promise the paid feature on slide 1 of a fresh
> install. It is named honestly on Slide 2 instead.

Support:
Built for cruises, hikes, festivals, stadiums, road trips — anywhere the network
is weak, missing, or overloaded.

---

## Slide 2 — How it works

Title: **It uses whatever's around**

Body:
Nearby, CruiseMesh talks phone‑to‑phone over Bluetooth and local Wi‑Fi. Farther
away, your message hops between other phones running CruiseMesh until it reaches
your friend — and, with a Shore Pass or your own server, over the internet
whenever any of those phones has a connection.

> The draft ended "And when the internet is available on any of them, it uses
> that too." Qualified per David's sign-off: internet delivery is the paid
> feature (or self-hosted), and this is the one slide that says so.

Support:
Every message is encrypted end to end, so the phones and networks that help
carry it can never read it.

---

## Slide 3 — Permissions (what they buy)

Title: **Give CruiseMesh more ways to connect**

Body:
Each of these opens up another path for your messages.

Rows (what the permission *buys*) — **shipped as two rows, not three**:
- **Nearby devices and notifications** — lets your phone hand messages directly
  to phones around you, and tells you the moment one arrives.
- **Background activity** — keeps the mesh working while your phone is in your
  pocket, so messages still move when the screen is off.

> The draft split nearby and notifications into separate rows. Not shipped:
> `meshPermissionsGranted` is a single boolean covering the whole
> nearby + `POST_NOTIFICATIONS` set, so a separate notifications row would show
> a green tick when only notifications had been denied. A status indicator that
> can lie is worse than one combined row (same failure class as the B5 zombie
> transport header). Splitting it needs real per-permission state first.

Buttons:
- Enable nearby access
- Enable background activity
- Next

Footer:
You can turn these on later in Settings — CruiseMesh just has fewer ways to reach
people until you do.

---

## Slide 4 — Keep Wi‑Fi on

Title: **Leave Wi‑Fi on, even with no internet**

Body:
On a ship or anywhere the Wi‑Fi has no internet, keep it connected anyway —
CruiseMesh uses that local network to reach phones near you faster than Bluetooth
alone.

Support:
It won't use the dead connection for the internet; it just uses it to find and
talk to nearby phones.

---

## Slide 5 — Your profile

Title: **What name would you like to go by?**

Body:
This is what people see when you share your friend card or add each other nearby.
You can change it anytime.

Controls (unchanged):
- Default the name field to the device model
- Let the user take or choose a local profile photo
- Show the device ID below the editor

Note:
The profile photo is shared with friends after you connect.

---

## Where this lives now (wired 2026-07-25)

All five slides are string resources — they used to be hardcoded Kotlin and
Swift literals, which the localization gate never saw because it only matches
`Text(...)`.

- Android: `ui_onboarding_*` in `android/app/src/main/res/values/strings.xml`,
  rendered by `OnboardingScreen.kt` (`pages = 5`; the new Wi-Fi slide is
  `WifiSlide`, index 3).
- iOS: literals in `OnboardingView.swift`, extracted into
  `Localizable.xcstrings` by `tools/generate_ios_string_catalog.py`.
  `OnboardingView.pageCount` replaced three separate hardcoded `4`s.

Still open:

- Slide 4 (Wi‑Fi) overlaps the T15 guidance in the LAN diagnostics and the
  `DeliverySlide` highlight card — they do not contradict each other, but they
  have not been read side by side for tone.
- Splitting the permission rows three ways (see Slide 3) needs per-permission
  state on Android.
- Nobody has seen these five screens rendered on a device.
