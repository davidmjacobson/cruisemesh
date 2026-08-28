# Same-LAN TCP transport

Status: implemented, version 1.

## Goal

Some ship Wi-Fi networks allow associated devices to communicate locally even
when neither device has purchased internet access. CruiseMesh should use that
path opportunistically without changing message encryption, storage, receipts,
deduplication, or mule behavior.

The LAN is an additional link transport. It carries the same HELLO, DIGEST, and
sealed-envelope frames as BLE after authenticating the peer. An old client that
does not implement this transport remains fully compatible over BLE and relay.

## Discovery and port

- Default listen port: TCP **45892**.
- DNS-SD service type: `_cruisemesh._tcp.`
- Service instance name: random per process; it contains no user or device
  identity.
- TXT data: protocol version and a random self-suppression token only.
- Primary discovery uses Android NSD or Apple Bonjour.
- mDNS is link-local and may not cross routed client subnets even when TCP
  does. Updated clients therefore exchange a `LAN_ENDPOINT` link-control frame
  after HELLO on any existing updated link. It contains the listener address,
  port, and random instance token; the token preserves the same
  single-initiator election used by DNS-SD.
- A successful endpoint is cached for seven days under a hash of the local
  IPv4 `/24` and the accepted contact's UserID. The raw network name and raw
  subnet are not persisted. A cached entry is re-checked against the host rule
  below every time it is read, so an entry written by an older build is
  dropped instead of dialed if it names anything but a local address.
- A peer that has demonstrated `LAN_ENDPOINT` support may receive a short-lived
  endpoint hint through its existing end-to-end-encrypted relay mailbox. The
  hint expires after 15 minutes. A fresh hint is dialed whenever the receiver
  is on any Wi-Fi network: the network fingerprint is stored with the cached
  endpoint but does not gate the dial, because routed multi-subnet LANs (the
  case the hint exists for) produce different fingerprints on each client
  subnet. A cross-network false positive costs one bounded TCP attempt to an
  endpoint the contact sealed pairwise, and the Noise handshake still
  authenticates. This allows two accepted contacts on the same LAN to find
  each other before BLE or mDNS succeeds.
- A hint carries the sender's own address on the local network and nothing
  else. The receiver accepts only an address literal in a range a phone's own
  interface address can be in -- RFC1918, `169.254/16`, RFC 6598 `100.64/10`,
  IPv6 `fe80::/10` (with an optional scope id) and `fc00::/7` -- so a hint can
  never name a public address and never causes a name to be resolved. The same
  rule applies to the `LAN_ENDPOINT` link-control frame.
- Dialing a hint is single-shot. A hinted address is never installed as a
  reconnect target, so a failed attempt is not retried on a timer; a later
  hint, mDNS discovery, or the cached endpoint starts the next attempt.
- A manual `IP[:port]` field and endpoint QR are available for diagnosis when
  automatic discovery is unavailable.
- The user may explicitly search the phone's own IPv4 subnet, as the network
  reports it. The scanned prefix is clamped to the range `/16` to `/30`, and a
  network that reports no prefix length is treated as a `/24`. A home `/24` is
  therefore 254 candidate hosts; a genuinely huge flat network (a `/8`, say) is
  clamped *down* to a `/16` around this phone's address rather than scanned
  whole. The search runs eight concurrent TCP attempts with a 350-millisecond
  attempt timeout. The `/16` ceiling is deliberate and applies to this manual
  button only -- the user asked for it -- and CruiseMesh never widens beyond
  the phone's own reported subnet, nor scans anything broader than a `/16` on
  any path.
- An automatic fallback sweep runs while discovery has produced nothing. Its
  ceiling is narrower than the manual button's: `/20` (~4,094 hosts) rather
  than `/16`, because ship and hotel Wi-Fi are exactly where the underlying
  network is one huge flat subnet, and an unattended sweep there must not cost
  minutes of sustained radio. It is deliberately hard to escalate and easy to
  quiet:
  - It only runs while the transport has no links at all, or while a contact
    that recently demonstrated LAN support still has no LAN link -- one
    connected family member must not stop discovery of the rest. "Recently"
    is a bounded window (two weeks); any link or endpoint hint refreshes it,
    so someone who is ashore stops motivating sweeps instead of keeping the
    phone searching forever.
  - A sweep that reaches an accepted friend counts as a find and leaves the
    wider sweep unarmed. So does reaching a friend this phone is already
    linked to, whether that link came from this sweep or an earlier one, and
    so does running out of link slots while a friend is connected: a full
    link table is the healthiest network there is, not an empty one. A bare
    TCP response is never a find -- an unrelated service on the default port
    must not disarm the wider sweep either way.
  - Evidence that peers exist here (a resolved service or an endpoint hint)
    brings the next sweep forward, but only a small number of times per
    network join. The evidence is data other devices choose, so it can
    shorten a wait but cannot drive repeated searching. The per-network
    bookkeeping it feeds is bounded the same way, forgetting its oldest
    entries first: a network full of made-up advertisements must never lock
    out a real family member who arrives afterwards.
- Every endpoint mechanism is reachability data, not authentication. A TCP
  responder must still present the agreement key of an accepted friend during
  the Noise handshake.

### Port 45892 and its IANA status

Re-checked against IANA's service-name and port-number registry on 2026-08-27.

- 45892 sits in the **User Ports** range (1024–49151), not the dynamic/private
  range (49152–65535). The distinction matters: user ports are the range IANA
  does assign, under Expert Review. Dynamic ports are never assigned, so a
  number up there could not be registered at all.
- 45892/tcp and 45892/udp are **unassigned**. The nearest assignments on either
  side are 45825 (`qdb2service`) and 45966 (`ssr-servermgr`). Nothing in the
  registry conflicts with CruiseMesh's use.
- Unassigned is not the same as reserved for us. Another application may squat
  the same number, which is why the listener never depends on it (below) and
  why a bare TCP response on 45892 is never treated as finding a peer — only a
  completed Noise handshake is.
- The number has one definition, `LAN_DEFAULT_TCP_PORT` in the core, which both
  shells and the desktop helper read over the FFI. Everything else that names
  45892 is prose or a test fixture, with one functional exception: the desktop
  helper opens a Windows firewall rule for the port. If a registration ever
  returns a different number, that constant and that rule are the two places
  that have to move.

Registering it is an external submission, deliberately not automated. It would
mean filing IANA's service-name and port-number application — the process
defined in RFC 6335 — with an assignee and contact, a service name, a short
description, and a reference document describing the protocol, for which this
spec would serve. User-port requests go through Expert Review, so a request can
be declined or answered with a different number, and CruiseMesh has to keep
working either way. No request has been filed; the port stays provisional until
one is.

The listener first tries 45892. If another local process already owns it, the
app may bind an ephemeral port and advertise that actual port through DNS-SD.
Clients always use the discovered service port, so the default does not become
a single point of failure.

## Authentication and encryption

TCP and DNS-SD provide reachability, not trust.

Every connection completes `Noise_XX_25519_ChaChaPoly_BLAKE2s` with the
prologue `CruiseMesh same-LAN transport v1`. Each side uses the X25519 agreement
private key already present in its CruiseMesh identity. Noise encrypts both
static public keys during the handshake and provides mutual authentication,
forward secrecy, replay resistance, and an encrypted transport channel.

After the remote static key is revealed, it must exactly match an accepted
contact's agreement public key:

- The initiator checks the responder after Noise message 2 and does not send
  message 3 to an unknown device.
- The responder checks the initiator after message 3 and closes an unknown
  connection before accepting CruiseMesh protocol frames.

The existing Ed25519 signatures and end-to-end sealed envelopes remain
authoritative for message authenticity and confidentiality. Noise additionally
protects link metadata such as HELLO and DIGEST inventories from other devices
on the Wi-Fi network.

Version 1 does not exchange full inventories with strangers. Anonymous
ciphertext carrying can be designed separately with strict resource limits.

## Stream framing

Each Noise handshake message and encrypted transport record is prefixed on TCP
with an unsigned four-byte big-endian length. A record is at most 65535 bytes.

One CruiseMesh frame may span multiple encrypted Noise records. The plaintext
inside each record is:

```text
record_type(1) | frame_id(4) | index(2) | total(2) | chunk
```

All integers are big-endian. Records are ordered by TCP and never interleaved
within one connection. A one-megabyte frame ceiling bounds reassembly memory;
current message and inline-attachment envelopes are far below it.

Once a frame is complete, the native shell passes it into the existing frame
parser and mesh sync path exactly as if BLE had reassembled it.

## Connection lifecycle

- The transport listens and browses while the platform grants runtime.
- Discovery starts only while a Wi-Fi network is available.
- Accept, connect, handshake, and idle operations use short timeouts.
- Concurrent accepted/connecting sockets are bounded.
- The random discovery tokens are compared lexicographically so exactly one
  side initiates for each device pair. Discovery is often asymmetric (one
  side resolves the other, but not vice versa), so the electing loser does
  not wait forever: if the expected connection has not arrived after 15
  seconds it initiates anyway. Duplicate connections remain safe: `msg_id`
  deduplication and per-peer sync digests make delivery idempotent, and an
  initiator that learns mid-handshake that the contact already has a live
  LAN link closes the redundant socket before it becomes a second link.
- Socket writes are serialized per connection so Noise record nonces and frame
  chunks remain ordered.
- Reconnect attempts use exponential backoff. Authenticated links exchange
  encrypted `TRANSPORT_PROBE` request/response frames; three consecutive probe
  timeouts close the stale socket so discovery can establish a fresh link.
- Every subnet-sweep probe is classified (connected, refused, timed out,
  denied, other) and a sweep that probed every candidate produces one verdict,
  identically on both platforms. A broad sweep where nothing answered and
  nothing was even refused suggests Wi-Fi client isolation: after the verdict
  repeats on two consecutive sweeps, further expensive sweeps are deferred to
  the backoff cap. Fresh peer evidence or a network change lifts that deferral
  by resetting the scan schedule; the repeat count itself is cleared only by a
  network change or a sweep that returns some other verdict, so a network that
  still looks isolated defers again on its very next such sweep. A sweep whose
  probes were denied outright — a VPN or OS policy refusing the sockets — is
  reported as such instead, and never changes sweep scheduling. Diagnostics
  shows the verdict; peer evidence and a network change clear it.
- Network loss closes every connection and restarts discovery when Wi-Fi
  returns.

Android runs this transport under the existing connected-device foreground
service. iOS runs it while the app has execution time; Bonjour/TCP does not
create a promise of continuous background execution.

## Platform privacy and permissions

Reviewed 2026-08-27 against the current platform documentation, sources listed
at the end of this section. Both shells are compliant as they ship today; the
work this section exists to protect is the Android target bump, which breaks
the transport quietly rather than loudly.

### What the transport actually does on the wire

Worth stating plainly, because every permission question below turns on it:

- Registers and browses one DNS-SD service type, `_cruisemesh._tcp.`, through
  the platform's own resolver (Android `NsdManager`, Apple `NWListener` /
  `NWBrowser`). Neither phone shell opens a multicast socket of its own or
  takes a multicast lock; the platform daemon does the mDNS. (The desktop
  helper is the exception — it runs mDNS in-process through the `mdns-sd`
  crate — but desktop platforms have no equivalent permission regime, so
  nothing below applies to it.)
- Listens for and accepts inbound TCP on 45892 (or an ephemeral port).
- Dials outbound TCP to local addresses: resolved services, cached endpoints,
  sealed endpoint hints, and the bounded subnet sweep. Every sweep probe dials
  only the listen port; the automatic tier is capped at a `/20` and the manual
  button at a `/16`, per "Discovery and port" above. It is not a `/24`-only
  sweep, which matters here only in that a wider sweep is still nothing but
  ordinary outbound TCP to local addresses.
- Sends and receives no UDP of its own, and no broadcast.

### Android

Local network protection gates every one of those operations at the socket
layer — outgoing TCP to a local address, inbound TCP accept, UDP in either
direction, `.local` resolution, and `NsdManager`. Because the check sits in the
networking stack it applies to all APIs; there is no library that routes around
it. When the grant is missing the app is not told so: TCP fails as a timeout and
UDP as `EPERM`. From the user's chair, LAN delivery would simply stop working.

State at targetSdk 36:

- Not required, and must not be declared. Apps targeting SDK 36 or lower get an
  implicit `ACCESS_LOCAL_NETWORK` grant from `INTERNET`; the platform docs say
  in as many words not to add the permission to the manifest or request it at
  runtime below target 37.
- The restriction can still be exercised on 36 for testing, because Android 16
  ships it as a per-app opt-in behind a compat flag:

  ```sh
  # com.cruisemesh.app.debug for a debug build
  adb shell am compat enable RESTRICT_LOCAL_NETWORK com.cruisemesh.app
  adb reboot            # the flag only takes effect after a reboot
  # ... exercise LAN discovery and delivery; expect it to fail ...
  adb shell am compat disable RESTRICT_LOCAL_NETWORK com.cruisemesh.app
  ```

  Under that flag `NEARBY_WIFI_DEVICES` is what restores access, so a build used
  for this test needs it declared temporarily. Do not merge that declaration:
  the app calls none of the Wi-Fi APIs `NEARBY_WIFI_DEVICES` actually gates
  (`WifiManager.startLocalOnlyHotspot`, Wi-Fi Aware, Wi-Fi Direct, Wi-Fi RTT),
  and a declared-but-unexercised permission is a review flag.
- No location permission is requested, and none is needed: DNS-SD and TCP are
  not location-gated. (BLE scanning is, which is why `BLUETOOTH_SCAN` carries
  `neverForLocation` — a separate matter.)

At targetSdk 37 (Android 17) the protection becomes mandatory, and the docs
offer two paths. The picker path is the wrong one here, which is worth writing
down because the code sits one flag away from it:

- **Not the picker.** `NsdManager` accepts a `DiscoveryRequest` carrying
  `FLAG_SHOW_PICKER`, and this app already builds a `DiscoveryRequest` (for
  `setNetwork`), so the change would look trivial. It is not: the picker shows
  the user a system dialog and returns *one device they choose*, which suits an
  app casting to a speaker and not a mesh that must keep finding every accepted
  contact on the network, unattended, for as long as the service runs. Android's
  own guidance points apps needing "broad, persistent access to the local
  network" at the permission instead.
- **The permission.** So, when the target is raised:
  1. Declare `ACCESS_LOCAL_NETWORK` in the manifest.
  2. Request it at runtime *before* starting the transport, and handle denial
     and later revocation by falling back to BLE and relay, exactly as the
     transport already handles a LAN that carries no peers. Revocation matters
     as much as denial: the docs are explicit that local network traffic is
     blocked from that moment on.
  3. Expect to surface nothing new in the permission sheet in the common case.
     `ACCESS_LOCAL_NETWORK` is in the `NEARBY_DEVICES` group, and CruiseMesh
     already holds Bluetooth permissions from that group, so a user who granted
     nearby devices for the BLE mesh is not prompted again. Anyone who denied it
     is a person for whom the BLE mesh is already off.

Google Play's target-API floor is 36 as of 31 August 2026, which this app meets.
Google has announced no equivalent date for 37 yet; whenever it lands, it is the
deadline that forces the work above.

### iOS

Already correct, and the reasoning is worth keeping so nobody "tidies" it:

- `NSLocalNetworkUsageDescription` is present. Apple's rule is that an app
  accessing the local network carries one, and its own table makes both halves
  of this transport qualify: *every* Bonjour operation (register, browse,
  resolve) requires local network access, and so does making an outgoing TCP
  connection to a local address. The subnet sweep is therefore covered by the
  same key as discovery and needs nothing extra. The string describes finding
  and exchanging messages with accepted contacts over local Wi-Fi, which is
  what the transport does.

  Note the key is *not* required merely for using something that happens to
  speak Bonjour underneath — AirPlay, UIKit printing, `DeviceDiscoveryUI` and
  `AccessorySetupKit` are all exempt because they keep the app away from the
  network's details. CruiseMesh gets no such exemption: it drives Bonjour and
  the sockets itself.
- `NSBonjourServices` lists `_cruisemesh._tcp`, matching the one type the app
  registers and browses. The shared constant carries a trailing dot
  (`_cruisemesh._tcp.`); the iOS shell trims it before handing the type to
  `NWListener`/`NWBrowser`, which is why the plist entry has no dot. Keep those
  two in step — a mismatch would break browsing without breaking the build.
- The multicast entitlement is deliberately absent. iOS requires
  `com.apple.developer.networking.multicast` only for sending or receiving real
  UDP multicast or broadcast, for working with arbitrary Bonjour service types,
  or for browsing all advertised types with a `_services._dns-sd._udp.local.`
  query. CruiseMesh does none of those: one fixed service type, resolved out of
  process, and no UDP at all. It is also a managed capability — Apple grants it
  on request rather than automatically — so not needing it is a feature, not an
  oversight to correct.
- Accepting inbound TCP does not itself require local network access on iOS
  (Apple's table lists it as the one common local operation that does not). But
  every Bonjour operation and every outbound dial to a local address does, so in
  practice the transport always needs the grant.
- Two behaviors to design around rather than fix:
  - The system may deny an operation *immediately*, before the user has answered
    the alert it just raised. Apple's remedy is an API that waits for
    connectivity, or retry logic. The shell already satisfies this: `NWBrowser`
    and `NWListener` sit in `.waiting` and are retried, and `LanTransport`
    tracks how long each has been stuck there so a genuinely denied privilege is
    surfaced to diagnostics instead of retried forever in silence. Keep that
    property if this code is reworked — the first attempt failing is normal.
  - If the privilege is undetermined and the app performs a local network
    operation while in the background, the system denies it without showing the
    alert *and without recording a decision*. The prompt appears the first time
    the app tries while in the foreground. So a first LAN attempt that happens
    while backgrounded is expected to fail once, and must not be cached as a
    permanent denial.
- The user may deny local network access outright; BLE and relay continue to
  work, which is the whole reason the transport is opportunistic.
- The simulator does not implement local network privacy at all. None of the
  above can be verified there — it needs a real device, which is worth knowing
  before anyone reads a green CI run as evidence about this.

### Declared versus used

No gaps in either direction as of this review — checked both ways, because a
permission declared and never exercised draws review attention just as a missing
one breaks the feature.

| Shell | Declared | Exercised by | Verdict |
| --- | --- | --- | --- |
| Android | `INTERNET` | relay sync, and the LAN transport's TCP | used |
| Android | `ACCESS_NETWORK_STATE` | `ConnectivityManager` capability reads | used |
| Android | `CHANGE_NETWORK_STATE` | `requestNetwork` for relay sync and the LAN transport's network binding | used |
| Android | `BLUETOOTH_SCAN` (`neverForLocation`), `BLUETOOTH_ADVERTISE`, `BLUETOOTH_CONNECT` | BLE mesh | used |
| Android | *(no `NEARBY_WIFI_DEVICES`)* | no `WifiManager` scan, Wi-Fi Direct, Aware or RTT call exists | correctly absent |
| Android | *(no `ACCESS_LOCAL_NETWORK`)* | implicit from `INTERNET` below target 37 | correctly absent |
| Android | *(no `CHANGE_WIFI_MULTICAST_STATE`)* | no multicast lock is taken | correctly absent |
| Android | *(no location permission)* | DNS-SD and TCP are not location-gated | correctly absent |
| iOS | `NSLocalNetworkUsageDescription` | Bonjour plus every outbound local dial | used |
| iOS | `NSBonjourServices` = `_cruisemesh._tcp` | the one type registered and browsed | used |
| iOS | *(no multicast entitlement)* | no UDP, one fixed service type | correctly absent |

Two invariants hide in that table and are easy to break by accident. The service
type has a single source of truth in the core (`LAN_SERVICE_TYPE`) and carries a
**trailing dot**; the iOS shell trims it before handing the type to `NWListener`
and `NWBrowser`, which is why the plist entry has none. A mismatch between the
plist and the type actually browsed would stop discovery without failing the
build. And `NEARBY_WIFI_DEVICES` must not be merged even though the Android 16
test recipe above temporarily needs it.

### Sources

Checked 2026-08-27. Re-read these rather than this summary before acting on the
target bump, since the Android side is still moving.

- Android, local network permission:
  <https://developer.android.com/privacy-and-security/local-network-permission>
- Android 17 behavior changes:
  <https://developer.android.com/about/versions/17/behavior-changes-17>
- Google Play target API level requirements:
  <https://developer.android.com/google/play/requirements/target-sdk>
- Apple TN3179, Understanding local network privacy:
  <https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy>
- IANA service name and port number registry:
  <https://www.iana.org/assignments/service-names-port-numbers/>
- RFC 6335, the port ranges and IANA's assignment procedures:
  <https://www.rfc-editor.org/rfc/rfc6335.html>

## Delivery policy

Each authenticated logical peer has one selected application-data route. LAN
wins over BLE; when both BLE roles exist, authenticated user IDs elect the same
physical direction at both endpoints. Superseded links remain available for
exact-link handshake/control replies and bounded failover, but do not multiply
message, digest, or epidemic fanout. When the selected link disconnects, the
best remaining live route takes over immediately. Relay upload remains useful
whenever internet is available because it provides durable delivery after the
local encounter ends.

The transport reuses existing digest synchronization and deduplication. The
profile screen exposes listener/peer endpoints, authenticated peer names,
encrypted-frame counters, probe latency, scan progress, and the most recent
error. Message Info stores whether an incoming message or outgoing delivery
confirmation used direct LAN or a LAN mule, alongside the existing hop estimate
and receive time.

## Compatibility

The endpoint and probe frames are optional link-control extensions. Older
clients reject or ignore an unknown frame without altering the BLE or relay
network. Sealed endpoint messages are capability-gated, so they are never
queued for a contact until that peer has sent a supported endpoint frame.

## Validation gates

Before enabling by default:

1. Android-to-Android, iOS-to-iOS, and Android-to-iOS message delivery.
2. Screen-off and background transition behavior.
3. Two devices on a permissive ship or captive-portal LAN.
4. Client-isolated Wi-Fi: discovery or TCP failure must fall back cleanly.
5. Reconnect after Wi-Fi roaming, airplane mode, and process restart.
6. Duplicate BLE plus LAN delivery renders once.
7. Photo transfer, A2DP coexistence, and overnight battery drain.
