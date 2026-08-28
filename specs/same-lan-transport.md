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

45892 is a provisional unassigned port in IANA's user-port range as of
2026-07-16. Before treating it as a permanent public assignment, the project
should request an IANA service-name and port registration.

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

## Platform privacy

Android currently targets API 36 and does not request location for same-LAN
DNS-SD. Android 16's local-network protection is opt-in only at target 36;
the `ACCESS_LOCAL_NETWORK` runtime permission becomes mandatory at target 37
(Android 17) and must be added before raising the target again.

iOS declares `_cruisemesh._tcp` in `NSBonjourServices` and provides an
`NSLocalNetworkUsageDescription`. The user may deny local-network access; BLE
and relay continue to work.

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
