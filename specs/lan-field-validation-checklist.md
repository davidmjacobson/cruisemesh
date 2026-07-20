# LAN field validation — one-pager (L3 + V1, plus T15 phase-2)

Printable checklist for the combined device session. **Run every check twice:
VPN-off and VPN-on** — the test Pixel's always-on WireGuard full-tunnel skews
LAN results (VPN owns the default route; socket-bind past it = EPERM). Note the
result of each in both columns.

Devices: at least one Android (Pixel) + one iPhone; a third phone for muling/
election checks where noted. Capture Android logs with `adb logcat`; capture iOS
with the **Share diagnostics** button (T13, Advanced settings → Diagnostics) or
Console.app.

## Global logcat filter

```
adb logcat -v time \
  MeshService:I LanTransport:I BleCentral:I BlePeripheral:I WifiHold:I *:S
```
Narrow per-check with the grep column below (pipe logcat through it).

---

## V1 gates (from same-lan-transport.md §Validation gates)

| # | Check | How | Pass grep / signal | VPN-off | VPN-on |
|---|---|---|---|---|---|
| 1 | A↔A, i↔i, A↔i delivery over LAN | Send text both directions on shared Wi-Fi | `LanTransport.*session ready`, message renders once | ☐ | ☐ |
| 2 | Screen-off / background | Lock both, send, wait, unlock | delivered while locked (foreground-service keeps it up) | ☐ | ☐ |
| 3 | Permissive ship / captive LAN | Repeat #1 on captive Wi-Fi (no internet) | delivery succeeds; relay may be down, LAN up | ☐ | ☐ |
| 4 | Client-isolated Wi-Fi fallback | On AP-isolation Wi-Fi, send | `probes were denied`/no LAN session → **BLE** used, still delivers | ☐ | ☐ |
| 5 | Reconnect: roam / airplane / restart | Toggle each, then send | link re-forms, digest re-syncs, no dupes | ☐ | ☐ |
| 6 | Dedup: BLE + LAN | Force both links up, send | message renders **once** (seen-set) | ☐ | ☐ |
| 7 | Photo + A2DP + overnight | Send photo with earbuds connected; leave overnight | photo arrives; **no audio stutter**; acceptable battery | ☐ | ☐ |

## L3 checks (LAN discovery reliability)

| Check | How | Pass grep / signal | VPN-off | VPN-on |
|---|---|---|---|---|
| **Sweep timing** | Trigger subnet scan (Advanced → Search my local network) | `Checked N of M addresses` completes < a few s on /24; note /16 time | ☐ | ☐ |
| **Tie-break / election** | Two phones discover each other simultaneously | exactly one initiates; log shows `awaiting their connection (tie-break)` on the loser, `session ready` on the winner; **no silent election loss** | ☐ | ☐ |
| **Hint replay** | Deliver a LAN endpoint hint before HELLO (reconnect race) | `Replaying held LAN endpoint hint` then session ready (not dropped) | ☐ | ☐ |
| **VPN denied-counts** | VPN-on run of any LAN check | `Local Wi-Fi probes were denied` / bind EPERM counted, **clean BLE fallback**, no crash/hang | ☐ | n/a |

## T15 phase-2 — Wi-Fi hold (piggybacked; the reason phase 2 needs the device)

| Check | How | Pass grep / signal | VPN-off | VPN-on |
|---|---|---|---|---|
| Hold registered | Start mesh, VPN-off | `WifiHold: Holding internet-less Wi‑Fi association` | ☐ | n/a |
| No-op under VPN | Start mesh, VPN-on | **no** WifiHold "Holding…" line (gated off) | n/a | ☐ |
| Association survives dead Wi-Fi | Join captive/no-internet Wi-Fi, wait 5-10 min idle | Wi-Fi **stays associated**; LAN transport still reaches nearby phone (compare against a build without the hold) | ☐ | n/a |
| Premature-drop detection | If Wi-Fi drops early with cellular up | `Wi‑Fi association dropped early…noting for keep-Wi‑Fi tip`; after 2 occurrences the chat-list tip appears | ☐ | n/a |
| Released on stop | Stop mesh | `WifiHold: Released Wi‑Fi association hold` | ☐ | ☐ |

**Tuning to capture for a follow-up PR:** actual time-to-teardown without the
hold vs with it; whether `WifiDropPolicy.PREMATURE_WINDOW_MS` (3 min) matches
observed drop timing; whether a captive-portal **re-auth** (the ~1 h ship-Wi-Fi
sign-out) is distinguishable in logs so we can add the "Ship Wi-Fi signed you
out — rejoin" prompt.

## Reporting

For each failed row, attach: the device pair, VPN state, the relevant log slice
(adb logcat for Android, Share-diagnostics file for iOS), and repro steps. File
V1 blockers against the LAN-transport enablement; L3/T15 findings feed
`same-lan-transport.md` and a T15 tuning PR respectively.
