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
  subnet are not persisted.
- A peer that has demonstrated `LAN_ENDPOINT` support may receive a short-lived
  endpoint hint through its existing end-to-end-encrypted relay mailbox. The
  hint expires after 15 minutes and is used only when both devices derive the
  same network fingerprint. This allows two accepted contacts on the same LAN
  to find each other before BLE or mDNS succeeds.
- A manual `IP[:port]` field and endpoint QR are available for diagnosis when
  automatic discovery is unavailable.
- The user may explicitly search the phone's current IPv4 `/24`. The search is
  bounded to 253 candidate hosts, eight concurrent TCP attempts, and a
  350-millisecond attempt timeout. CruiseMesh never silently expands this to a
  `/16` or broader scan.
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
  side initiates for each device pair. Duplicate connections remain safe:
  `msg_id` deduplication and per-peer sync digests make delivery idempotent.
- Socket writes are serialized per connection so Noise record nonces and frame
  chunks remain ordered.
- Reconnect attempts use exponential backoff. Authenticated links exchange
  encrypted `TRANSPORT_PROBE` request/response frames; three consecutive probe
  timeouts close the stale socket so discovery can establish a fresh link.
- Network loss closes every connection and restarts discovery when Wi-Fi
  returns.

Android runs this transport under the existing connected-device foreground
service. iOS runs it while the app has execution time; Bonjour/TCP does not
create a promise of continuous background execution.

## Platform privacy

Android currently targets API 35 and does not request location for same-LAN
DNS-SD. The local-network permission introduced for later target SDKs must be
reviewed before raising the target.

iOS declares `_cruisemesh._tcp` in `NSBonjourServices` and provides an
`NSLocalNetworkUsageDescription`. The user may deny local-network access; BLE
and relay continue to work.

## Delivery policy

LAN links are preferred over BLE for large frames. Small control and text
frames race over LAN plus one BLE route so one unhealthy path does not add a
full retry delay. Message IDs keep the duplicate arrival harmless. Relay upload
remains useful whenever internet is available because it provides durable
delivery after the local encounter ends.

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
