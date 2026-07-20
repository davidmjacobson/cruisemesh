# CruiseMesh Onboarding Script

> **DRAFT for David's sign-off (T5).** This rewrite is copy only — no code
> changes. Per the plan, strings land in `strings.xml` /
> `Localizable.xcstrings` **after** this script is approved. Review the wording
> here; approving (merging) this is the go-ahead to wire it. Coordinate the
> Wi-Fi slide wording with T15's guidance copy (already shipped).

## The one idea

**CruiseMesh uses virtually every form of connectivity your phone has to get
your messages through.** Every slide should reinforce that, and each permission
slide says *what the permission buys*, not what breaks without it.

---

## Slide 1 — Welcome

Title: **Messages that find a way through**

Body:
CruiseMesh gets your messages to friends and family using virtually every
connection your phone has — Bluetooth, local Wi‑Fi, the internet — and even
hands them phone to phone when there's no signal at all.

Support:
Built for cruises, hikes, festivals, stadiums, road trips — anywhere the network
is weak, missing, or overloaded.

---

## Slide 2 — How it works

Title: **It uses whatever's around**

Body:
Nearby, CruiseMesh talks phone‑to‑phone over Bluetooth and local Wi‑Fi. Farther
away, your message hops between other phones running CruiseMesh until it reaches
your friend. And when the internet is available on any of them, it uses that
too.

Support:
Every message is encrypted end to end, so the phones and networks that help
carry it can never read it.

---

## Slide 3 — Permissions (what they buy)

Title: **Give CruiseMesh more ways to connect**

Body:
Each of these opens up another path for your messages.

Rows (what the permission *buys*):
- **Nearby devices** — lets your phone hand messages directly to phones around
  you over Bluetooth.
- **Notifications** — tells you the moment a message arrives.
- **Background activity** — keeps the mesh working while your phone is in your
  pocket, so messages still move when the screen is off.

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

## Notes for wiring (after sign-off)

- Slide 4 (Wi‑Fi) mirrors the T15 guidance already in the LAN diagnostics and
  the `DeliverySlide` highlight card — reconcile so they read consistently.
- Permission-row copy replaces the current "what breaks without it" framing
  (e.g. old Slide 3 footer "delivery is less reliable while backgrounded").
- Slide count goes 4 → 5; update the pager/`pages` count and the
  `ui_step_of` progress on both platforms.
