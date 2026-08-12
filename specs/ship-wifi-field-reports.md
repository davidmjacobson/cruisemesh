# Ship Wi-Fi compatibility field reports

Status: **proposed, version 1**.

## Decision summary

CruiseMesh will collect ship Wi-Fi compatibility information only through a
user-initiated, preview-before-send field report. It will not add background
analytics, periodic uploads, a global telemetry opt-in, or a stable reporting
identifier.

The report describes one observation on one named ship during a coarse time
period. It contains a small, closed set of compatibility facts and never
contains messages, contacts, network addresses, Wi-Fi names, exact travel
dates, cabin or itinerary information, or a persistent device identifier.

The compatibility result is ship-and-period scoped. CruiseMesh will never turn
one observation into a claim about an entire cruise line. Runtime probing
remains authoritative on every sailing.

Version 1 has two contribution paths:

1. **Current-network report.** The user opens Connection details while still
   associated with the network, reviews evidence CruiseMesh already obtained,
   identifies the line and ship, and explicitly shares it.
2. **Guided two-phone test.** Two accepted contacts run a bounded LAN test. The
   phones coordinate through an already authenticated CruiseMesh link, then
   test the existing same-LAN listener directly. This is the only version 1
   path that may produce a qualifying negative result.

The first implementation exports a standalone JSON report through the platform
share sheet. A dedicated submission service and public compatibility directory
are a later phase. Building the local evidence model and export first preserves
the project's current no-telemetry promise and permits the schema to be tested
on real sailings before operating a collection service.

## Motivation

CruiseMesh can deliver instantly and ship-wide when the passenger Wi-Fi allows
guest devices to communicate. Some passenger networks isolate clients, some
only filter multicast discovery, and some change behavior by ship, Wi-Fi
authorization state, access point, or refit.

The public evidence base is thin. A 2026 review of published sources found one
historical Carnival report with isolation enabled, a direct non-isolated result
on Norwegian Jade, and non-isolated results on some unnamed Royal Caribbean
sailings. For the other major ocean lines, no public source gives a defensible
answer either way. That thinness is the motivation: this document exists
because the only people who can answer the question are passengers.

Unstructured reviews are difficult to use because they commonly conflate:

- association with Wi-Fi and access to the internet;
- an onboard app reaching a ship server and passengers reaching each other;
- mDNS/Bonjour filtering and unicast TCP isolation;
- travel-router or hotspot sharing and direct communication on the ship SSID;
- a missing peer and a network that blocked a known peer; and
- a line-wide policy and one ship's configuration on one date.

CruiseMesh already has more trustworthy local evidence: authenticated LAN
links, encrypted transport probes, discovery provenance, OS policy errors, and
completed sweep classifications. This design turns that evidence into a small
report the user can understand and deliberately contribute.

## Goals

- Collect enough information to set responsible expectations for a named ship.
- Preserve the standing no-background-telemetry product promise.
- Make every submitted field visible and understandable before it leaves the
  phone.
- Prefer machine-observed, authenticated evidence over user recollection.
- Distinguish discovery filtering, direct reachability, policy denial, no-peer
  evidence, and genuinely inconclusive outcomes.
- Support collection without buying an internet package and submission later
  when connectivity returns.
- Keep Android and iOS verdicts behaviorally identical.
- Make submitted reports deletable without requiring a CruiseMesh account.
- Publish recency, report counts, and conflicting evidence rather than an
  opaque compatibility score.
- Avoid creating incentives for broader network scanning.

## Non-goals

- Measuring internet speed, satellite provider, latency to the internet, or
  Wi-Fi package value.
- Mapping ship access points, passengers, local services, or network topology.
- Proving that a cruise line intentionally enabled or disabled isolation.
- Certifying future sailings or every SSID on a ship.
- Identifying independent people through accounts, advertising IDs, device
  attestation, or install identifiers.
- Collecting general product analytics, crash reports, message metrics, or
  engagement data.
- Replacing the existing field-report issue for narrative trip reports.
- Automatically changing transport policy from historical compatibility data.

## Product and privacy invariants

These are release-blocking requirements.

1. **No automatic upload.** Network observations remain local unless the user
   opens the report flow, reviews the final payload, and submits or shares that
   individual report.
2. **Consent is per report.** There is no one-time consent that permits future
   background submissions.
3. **No report identity.** A report may have a random report ID and deletion
   secret, but neither may be reused by another report or linked to a
   CruiseMesh identity.
4. **No relationship data.** Reports never contain UserIDs, public keys,
   contact names, group membership, chat tags, relay identifiers, or counts of
   contacts.
5. **No network identifiers.** Reports never contain SSID, BSSID, MAC address,
   IP address, port, gateway, DNS server, raw subnet, or a hash derived from any
   of them.
6. **No precise travel record.** Reports contain a user-approved month and year
   or year only, never an exact sailing or observation timestamp, itinerary,
   port, cabin, deck, or coordinates.
7. **No stranger scanning for reporting.** Contributing a report cannot cause
   any probe beyond the behavior already permitted by
   [same-LAN transport](same-lan-transport.md). The guided test targets only an
   accepted contact's advertised CruiseMesh listener.
8. **No general diagnostics attachment.** The report is a new, standalone
   artifact. `Share diagnostics` archives are never silently included.
9. **No line-wide inference.** A report is keyed to a ship and period. Parent
   company and sister-brand behavior is irrelevant.
10. **Current behavior wins.** Historical data may inform copy or retry timing,
    but it cannot suppress a runtime LAN attempt.

## Terminology

**Observation session**
: An in-memory evidence window for the currently joined Wi-Fi network. It
  begins on network join and ends on network loss or replacement. It has no
  persisted or submitted network fingerprint.

**Peer evidence**
: Evidence that an accepted CruiseMesh contact was available for a relevant
  test, such as an authenticated BLE link carrying that contact's current LAN
  endpoint or participation in the guided test. A generic open port or mDNS
  record from an unknown device is not peer evidence.

**Authenticated LAN link**
: A TCP connection that completed the same-LAN Noise handshake and matched an
  accepted contact as specified by `same-lan-transport.md`.

**Direct probe**
: An encrypted `TRANSPORT_PROBE` request/response over an authenticated LAN
  link. ICMP ping is never report evidence.

**Discovery filtering**
: mDNS/NSD did not produce the peer, but the peer was reached by an authenticated
  endpoint hint, bounded sweep, cache, or manual endpoint. This is not client
  isolation.

**Qualifying negative**
: A guided test in which the user confirmed both phones were on the same ship
  Wi-Fi, both phones had operational local-network permissions, a VPN was not
  known to be intercepting local traffic, peer endpoints were exchanged over
  an authenticated non-LAN link, and repeated direct attempts failed in both
  directions. It is still reported as `likely_isolated`, not proven isolation.

## Evidence model

### Normalized events

The platform LAN implementations feed a shared, pure evidence reducer with
normalized events. The reducer belongs in the Rust core so the same event
sequence produces the same candidate report on Android and iOS.

```text
NetworkJoined
NetworkLost
LocalPermissionReady
LocalPermissionDenied
VpnInterferenceSuspected
MdnsBrowseReady
MdnsPeerResolved
PeerEndpointReceived(source = ble | relay | lan)
SweepCompleted(verdict)
LanAuthenticated(discovery_source)
LanProbeSucceeded(direction, latency_bucket)
LanProbeFailed(direction, failure_class)
GuidedTestStarted
GuidedPeerConfirmedSameShipWifi
GuidedTestCompleted
```

The core reducer stores only the current session's coarse facts. It does not
receive raw endpoints, names, network fingerprints, precise timestamps, or
contact identifiers.

The reducer may distinguish individual test directions with ephemeral slots
inside one guided test. Those slots are destroyed when the test ends and are
never serialized as peer identifiers.

### Discovery source

When a LAN authentication succeeds, the shell records how the successful
endpoint was first obtained:

```text
mdns
authenticated_endpoint
cached_endpoint
bounded_sweep
manual
unknown
```

`authenticated_endpoint` covers an endpoint received from an accepted contact
over BLE, LAN, or the pairwise-encrypted relay hint. The report does not expose
which existing transport supplied it because that adds little compatibility
value and can reveal whether a family purchased internet access.

### Failure classes

Direct connection failures are reduced before reaching the report model:

```text
timed_out
refused
policy_denied
network_lost
handshake_unknown_peer
other
```

`handshake_unknown_peer` can never contribute to a compatibility verdict. It
means the endpoint was not the accepted contact and is relevant only to local
diagnostics.

`policy_denied` produces `os_or_vpn_interference`, never `likely_isolated`.
`network_lost` produces `inconclusive`.

### Report verdicts

The serialized verdict is one of:

```text
direct_confirmed
discovery_filtered_direct_worked
likely_isolated
os_or_vpn_interference
no_peer_evidence
inconclusive
```

#### `direct_confirmed`

Requires at least one authenticated LAN link during the observation session.
An encrypted probe success increases evidence strength but is not required:
completing the authenticated Noise handshake already proves direct client
reachability on that path.

If the user says the phones were in different parts of the ship, the report may
carry `separation = different_ship_areas`. CruiseMesh must not infer physical
separation from signal strength, BSSID, or IP topology.

#### `discovery_filtered_direct_worked`

Requires all of:

- an authenticated LAN link;
- no `MdnsPeerResolved` event for that peer during a ready browse window; and
- successful discovery through an authenticated endpoint, bounded sweep,
  cache, or manual entry.

The UI wording is “Bonjour did not find this phone, but direct local Wi-Fi
worked.” It must not say the network blocks Bonjour categorically; absence of a
single resolution is finite evidence.

#### `likely_isolated`

Requires a qualifying negative from the guided test. A completed `/24` sweep,
including two consecutive current `isolationSuspected` verdicts, is supporting
evidence only. It can never produce this report verdict without a known peer
and bidirectional direct attempts.

#### `os_or_vpn_interference`

Used when local-network permission, a VPN, Private Relay-like routing, or an OS
socket policy prevents a meaningful test. This result is useful for improving
instructions but is excluded from ship compatibility aggregation.

#### `no_peer_evidence`

Used when CruiseMesh searched but never had evidence that an accepted peer was
available on the current Wi-Fi. The current all-silent sweep result belongs
here unless a guided test supplies peer evidence. This result is excluded from
ship compatibility aggregation.

#### `inconclusive`

Used for network loss, partial tests, mismatched user confirmations, unknown
failures, or any event sequence that does not meet a stronger rule. It is
excluded from ship compatibility aggregation.

### Evidence strength

The core assigns one of the following transparent strengths:

```text
strong_positive
positive
qualifying_negative
non_qualifying
```

- `strong_positive`: authenticated LAN plus an encrypted round trip, or an
  authenticated LAN link the user confirms worked across different ship areas.
- `positive`: authenticated LAN without the additional evidence.
- `qualifying_negative`: the exact guided-test conditions above.
- `non_qualifying`: everything else.

The public service recomputes strength from the submitted evidence fields and
does not trust the client's strength label.

## Guided two-phone test

### Purpose

Passive success is trustworthy: an authenticated LAN link cannot exist unless
direct traffic worked. Passive failure is ambiguous. The guided test supplies
the peer evidence and user confirmation necessary for a responsible negative
report.

### Preconditions

- Both phones are accepted CruiseMesh contacts.
- Both show Wi-Fi associated.
- At least one authenticated non-LAN link is live for coordination, normally
  BLE while the phones are together.
- The user confirms both phones are connected to the same ship guest Wi-Fi.
- Local-network permission is ready on both platforms.
- The UI asks users to disable VPN/local-network filtering when practical. If
  either user cannot confirm, the test may run but cannot yield
  `likely_isolated`.

The test does not require disabling Bluetooth. It measures a specific TCP path,
not whether a chat message happened to arrive through another transport.

### Procedure

1. One user chooses **Test ship Wi-Fi with a friend** and selects a currently
   connected accepted contact.
2. CruiseMesh shows both participants what will happen: each phone will try to
   open CruiseMesh's existing encrypted local connection to the other; no
   message content or other local devices are tested.
3. The phones exchange an ephemeral test ID, their existing validated local
   endpoint advertisements, readiness, and coarse permission/VPN readiness over
   the authenticated coordination link.
4. Each phone makes a bounded direct attempt to the other's advertised
   CruiseMesh endpoint. Connection and Noise timeouts reuse the same-LAN
   transport limits.
5. If a LAN link authenticates, each direction sends a nonce-bound encrypted
   `TRANSPORT_PROBE`. A success immediately produces a positive result.
6. If both directions time out, each phone repeats once after a short jittered
   delay. Refusal is recorded separately: it shows the network forwarded
   traffic to a host, so it does not support client isolation by itself.
7. The coordination link exchanges only terminal categories, not raw addresses
   or timing logs. Both devices must agree on completion before a qualifying
   negative is available.
8. The ephemeral test record and peer association are destroyed after the
   result is reduced.

The test is capped at two attempts per direction and one minute. It cannot
start a broad sweep, expand beyond the existing `/24` rule, probe other ports,
or retry in the background.

### Test frame compatibility

If coordination needs a new link-control frame, it is optional and
capability-gated:

```text
LAN_FIELD_TEST_OFFER(test_id, expires_in_seconds)
LAN_FIELD_TEST_READY(test_id, endpoint_generation, readiness_flags)
LAN_FIELD_TEST_RESULT(test_id, direction, terminal_category)
LAN_FIELD_TEST_CANCEL(test_id)
```

The frame travels only over an already authenticated live link. It is never a
sealed store-and-forward message, never enters relay storage, and never carries
an IP address itself; endpoint data continues to use the existing validated
`LAN_ENDPOINT` mechanism. Unknown frames are ignored. Test IDs are random,
single-use, and live for at most two minutes.

Version 1 may instead coordinate the UI locally around existing endpoint and
probe machinery if both shells can do so without a new frame. The privacy and
verdict requirements do not change.

## User experience

### Entry points

Connection details gains a **Ship Wi-Fi report** section with:

- **Share this Wi-Fi result**, available while associated with Wi-Fi; and
- **Test ship Wi-Fi with a friend**, available when an accepted contact is
  reachable for coordination.

The report action is a passive row or card, not a modal, notification, badge,
or repeated prompt. CruiseMesh does not infer that a network is on a ship and
does not prompt merely because a captive portal exists.

The existing GitHub field-report template remains available for users who want
to describe a previous sailing. A machine-evidenced current-network report must
be started while still on that network; CruiseMesh will not retain a history of
unidentified Wi-Fi sessions in order to prompt later.

### Identification form

The user supplies:

- cruise line;
- ship;
- observation period, defaulting to month/year with a year-only option;
- Wi-Fi authorization state for the two tested phones;
- approximate phone separation; and
- optional phone models.

Wi-Fi authorization state is one of:

```text
both_onboard_only
both_paid
mixed
unknown
```

Separation is one of:

```text
same_area
different_ship_areas
unknown
```

“Same area” and “different ship areas” are deliberately coarse. Do not ask for
deck, venue, cabin, or distance.

The line and ship selector should use a bundled, offline canonical catalog with
an **Other / not listed** path. A later catalog update may be downloaded, but
the report flow must work without internet. Free-text values are normalized
locally, length-limited, and shown verbatim in the preview. The dedicated
service must quarantine unknown names for alias review rather than publishing
them automatically.

### Preview

The preview is a human-readable rendering of the exact JSON artifact. It lists
every included field and a separate “Never included” summary.

Recommended copy:

> **Share a ship Wi-Fi result**
>
> This report contains the cruise line, ship, month or year, whether an
> authenticated local Wi-Fi connection worked, how the test was performed,
> approximate phone separation, and app/OS versions.
>
> It does not contain messages, people, IP or device addresses, Wi-Fi names,
> exact travel dates, cabin or itinerary information, or an identifier reused
> by another report.

Actions:

- **Share report**
- **Save for later** when no internet is available
- **Cancel**

“Agree,” “anonymous,” “improve your experience,” and preselected consent
checkboxes are not used. The action says what happens.

### Offline behavior

In the share-sheet phase, **Save for later** stores the finished report file in
app-private storage for 30 days. It never sends automatically. Connection
details shows **Saved ship Wi-Fi reports**, from which the user can preview,
share, or delete each file.

In the dedicated-service phase, the user may explicitly choose **Send when
online**. This creates an outbox item with the approved payload and consent
policy version. The UI makes clear that the app will send that one report after
connectivity returns and provides **Cancel pending upload**. A transient failure
uses bounded retry for seven days, after which the item returns to manual
action. Consent for one queued report does not authorize another.

### Completion and deletion receipt

A successful dedicated submission returns a report ID and a deletion secret.
The app stores them together in the saved-report area and displays:

> Report submitted. Keep this receipt if you may want to remove it later.

Users may delete the local receipt without deleting the server report, so the
UI must distinguish **Remove my local copy** from **Delete submitted report**.

## Report data contract

### Canonical JSON

The standalone artifact is UTF-8 JSON, at most 8 KiB, with strict schema
validation and no extension/free-form object.

```json
{
  "schema_version": 1,
  "report_nonce": "QmFzZTY0dXJsLTEyOC1iaXQ",
  "ship": {
    "line_id": "norwegian-cruise-line",
    "ship_id": "norwegian-jade",
    "line_other": null,
    "ship_other": null
  },
  "period": {
    "value": "2026-05",
    "precision": "month"
  },
  "network_context": {
    "authorization": "both_onboard_only",
    "separation": "different_ship_areas"
  },
  "result": {
    "verdict": "direct_confirmed",
    "origin": "observed_session",
    "discovery_source": "authenticated_endpoint",
    "authenticated_lan": true,
    "encrypted_round_trip": true,
    "directions_attempted": "one",
    "completed_sweep": "not_run",
    "local_permission": "ready",
    "vpn_readiness": "user_confirmed_clear"
  },
  "reporting_client": {
    "platform": "android",
    "os_major": "16",
    "app_version": "1.2.0",
    "device_model": null
  },
  "consent": {
    "policy_version": 1
  }
}
```

`report_nonce` is 128 random bits generated for this artifact. It is used for
idempotency and is never reused. It is not a CruiseMesh message ID, UserID,
installation ID, or cryptographic identity.

`device_model` is optional, off by default, and omitted rather than serialized
as `null` in production. The preview explains that rare device/OS combinations
can make a report more distinctive. Device model is useful for troubleshooting
but never affects ship compatibility aggregation.

### Period

`precision` is `month` or `year`:

- month: `YYYY-MM`;
- year: `YYYY`.

The app may default from the current local calendar but must let the user change
or coarsen it. No timezone or exact timestamp is serialized.

### Sweep field

`completed_sweep` is one of:

```text
not_run
found_peer
healthy_but_empty
all_silent
blocked_by_policy
inconclusive
```

Counts of connected, refused, timed-out, or denied addresses are not submitted.
They do not add enough aggregation value to justify fingerprinting a particular
network configuration.

### Direction field

`directions_attempted` is:

```text
none
one
both
```

No peer identifier is attached to a direction.

### Explicitly forbidden keys

Both client and service reject artifacts containing any key intended for:

```text
ssid, bssid, mac, ip, address, endpoint, port, subnet, gateway, dns,
user_id, contact, friend, group, chat, message, relay, cabin, deck,
venue, itinerary, latitude, longitude, exact_time, installation_id,
advertising_id, vendor_id
```

This denylist is defense in depth; the primary protection is a closed schema
with unknown fields rejected. String values are also scanned for obvious IP and
MAC-address forms before preview/export. A match blocks export and reports a
local implementation error rather than silently redacting an unexpected value.

## Local storage

CruiseMesh does not persist generic network observations. The reducer is reset
on network change and process restart.

It may persist only:

- a user-created draft;
- a user-approved saved report;
- a user-approved pending upload; and
- the report ID/deletion secret returned for a submitted report.

Saved drafts and unsent reports expire after 30 days unless the user explicitly
chooses **Keep**. Submitted deletion receipts do not expire locally until the
user removes them. All are deleted by **Delete captured diagnostics and saved
reports** only if the UI names both categories; the existing diagnostics delete
action must not silently gain broader meaning.

Backup behavior is explicit: report drafts, pending uploads, and deletion
secrets are excluded from identity backup. Restoring an identity on another
phone must not duplicate a queued submission or copy a server-deletion
credential.

## Component ownership

### Shared Rust core

The core owns:

- normalized evidence enums;
- the pure observation reducer;
- verdict and strength derivation;
- schema-versioned report value types;
- canonical JSON serialization;
- closed-schema validation for imported report drafts; and
- redaction-safety tests.

The core does not own HTTP submission, line/ship presentation, platform share
sheets, or OS permission detection.

### Android and iOS shells

Each shell owns:

- translating existing LAN events into normalized core events;
- current-network and guided-test UI;
- offline line/ship catalog UI;
- user confirmations;
- preview rendering from the canonical report object;
- share-sheet export;
- app-private draft and receipt storage;
- optional future upload/outbox behavior; and
- platform privacy declarations.

The shells must not independently derive the final verdict.

### Existing diagnostics

`LanTransportDiagnostics` remains the live, detailed support surface. The new
evidence reducer receives selected coarse events at the same call sites, but it
does not serialize the existing snapshot because that snapshot contains local
and peer endpoints and peer display names.

`DiagnosticLogExport`, MetricKit/process-exit files, field-metrics CSVs, and the
diagnostics ZIP are never inputs to a ship Wi-Fi report.

## Share-sheet phase

The first shippable phase creates a file named:

```text
cruisemesh-ship-wifi-report-v1.json
```

The OS share sheet lets the user send it to a project-controlled intake address,
attach it to the GitHub field-report issue, save it, or inspect it with another
app. CruiseMesh does not claim that third-party destinations are private. The
UI recommends the project intake path and links its privacy notice.

The repository's field-report template should accept the JSON as an optional
attachment and add structured human questions matching the schema. GitHub
reports remain public and attributable to the contributor's GitHub account;
the template must state that plainly.

## Dedicated submission service

This section defines the later service so the local schema does not need to be
redesigned when collection scales. It does not select a hosting provider.

### API

```text
POST   /v1/ship-wifi-reports
DELETE /v1/ship-wifi-reports/{report_id}
GET    /v1/ship-wifi-compatibility/{ship_id}
```

`POST` accepts only `application/json`, requires TLS, enforces an 8 KiB body
limit, rejects unknown keys, normalizes canonical catalog identifiers, and
returns:

```json
{
  "report_id": "random-server-id",
  "deletion_secret": "random-single-report-secret",
  "accepted": true,
  "publication_state": "included|quarantined"
}
```

The deletion secret is shown once, stored hashed by the service, and authorizes
only deletion of that report. `DELETE` requires the secret in an authorization
header. It removes the individual report from future aggregates and schedules
its stored row for deletion. Aggregates are recomputed; deletion must not leave
the result influential through a cached count.

`GET` returns aggregate compatibility only, never individual report payloads,
report IDs, device models, client versions, or deletion metadata.

### Service-side storage

The accepted row contains only the canonical report plus:

- server report ID;
- hash of deletion secret;
- received date coarsened to a day for retention enforcement;
- normalization/moderation state; and
- abuse-review state.

Source IP, user agent, TLS fingerprint, edge request ID, and request headers are
not copied into application storage. Infrastructure may see transient network
metadata; the privacy notice must say so and document the actual provider's
logs. Application request logs omit bodies and use the shortest operational
retention practical, with a target maximum of seven days.

Accepted individual reports are retained for 36 months, then deleted or reduced
to year-level aggregate counts that cannot be traced to a report ID. Reports
are excluded from the “recent” compatibility window after 18 months even while
retained for historical trend analysis.

### No account requirement

Submission does not require a CruiseMesh identity, email address, store account,
GitHub account, hosted-pass purchase, or relay configuration. The service never
receives the reporter's CruiseMesh public keys.

## Aggregation and publication

### Canonical ship catalog

Every active ship has a stable `ship_id` independent of display-name changes.
The catalog records line ownership over time, aliases, and retirement dates.
A ship moving brands retains its physical ship ID while reports retain the line
selected for their observation period.

Unknown user-entered ships are quarantined until a maintainer maps them to a
canonical ship or confirms a new entry. User text is escaped and never rendered
directly into public HTML.

### Inclusion

Only these evidence strengths affect compatibility:

- `strong_positive`;
- `positive`; and
- `qualifying_negative`.

`os_or_vpn_interference`, `no_peer_evidence`, `inconclusive`, unsupported app
versions, malformed reports, and manual claims without machine evidence may be
counted in a research-quality appendix but never change the compatibility
headline.

### Published states

For the rolling 18-month window, a ship is shown as:

```text
observed_working
mixed_reports
often_blocked
insufficient_evidence
stale_evidence
```

Rules:

- `observed_working`: at least one positive or strong-positive report and no
  qualifying negative in the window.
- `mixed_reports`: at least one positive and one qualifying negative in the
  window. A positive does not erase a negative; SSID, access-point, and sailing
  differences may make both true.
- `often_blocked`: at least three qualifying-negative submissions spanning at
  least two observation months, with no positive in the window.
- `insufficient_evidence`: anything else in the window, including one or two
  qualifying negatives.
- `stale_evidence`: qualifying evidence exists, but none is within 18 months.

Because the service deliberately has no stable user identity, it cannot promise
that submissions came from independent people. Public copy says “reports” or
“submissions,” never “independent reports.” Abuse controls and moderation can
reduce duplication but do not turn it into verified independence.

### Public presentation

Show the underlying facts:

> **Norwegian Jade — local Wi-Fi observed working**  
> 2 working reports · 0 qualifying blocked reports · most recent May 2026  
> Ship configurations can change. CruiseMesh tests your current Wi-Fi
> automatically.

For mixed evidence:

> **Mixed reports**  
> Local Wi-Fi worked on some sailings and appeared isolated on others.

Never publish:

- “Norwegian supports CruiseMesh”;
- “Carnival blocks CruiseMesh”;
- an operator security-quality rating;
- an isolation percentage without showing counts and recency; or
- a line-level rollup that hides ship variance.

The compatibility data is advisory. It does not change the app's transport
selection or promise delivery.

## Abuse resistance and data quality

The no-account design trades strong Sybil resistance for privacy. Mitigations
must remain proportionate:

- idempotency by per-report nonce;
- strict schema and size limits;
- transient per-source rate limiting without durable IP storage;
- quarantine for unknown ships, impossible periods, future app versions, and
  contradictory field combinations;
- server-side recomputation of verdict/strength;
- duplicate-pattern detection over the non-identifying payload;
- a moderation queue for sudden result reversals or bursts; and
- public counts so weak evidence is visible rather than laundered into a badge.

Do not add advertising identifiers, store receipts, phone attestation, stable
installation keys, proof-of-work, or CruiseMesh identity signatures solely to
improve report deduplication. Revisit this only if real abuse makes the directory
unusable, with a new privacy review.

The server treats every string and enum as attacker-controlled. Rendering uses
escaping, database access uses parameters, JSON parsing is depth-limited, and
error responses never echo an entire rejected payload.

## Platform and policy obligations

Local-only evidence processing is not off-device collection. Once a dedicated
service retains a submitted report, CruiseMesh must accurately disclose that
collection even though it is optional and user initiated.

Before the dedicated endpoint ships:

- update the in-app privacy policy with fields, purpose, retention, deletion,
  infrastructure visibility, and contact information;
- update Google Play Data safety for diagnostics/other user-provided data as
  applicable to the final implementation;
- update App Store privacy answers for diagnostics and any optional user
  content that is retained;
- verify the consent screen immediately precedes submission;
- document that declining has no effect on messaging; and
- change README's promise only if necessary and only to the precise statement
  that remains true, such as “no automatic telemetry.” A voluntary report must
  not be disguised as something other than data collection.

Relevant platform guidance:

- [Apple App Privacy Details](https://developer.apple.com/app-store/app-privacy-details/)
- [Apple App Review Guidelines §5.1](https://developer.apple.com/app-store/review/guidelines/)
- [Google Play Data safety](https://support.google.com/googleplay/android-developer/answer/10787469)
- [Google prominent disclosure and consent guidance](https://support.google.com/googleplay/android-developer/answer/11150561)

Store declarations are release artifacts and must be tested against actual
network behavior. An optional field is still collected when the user elects to
submit it.

## Failure handling

- If evidence changes while the preview is open because the network changes,
  the report is invalidated and must be regenerated. It cannot silently attach
  evidence from the new network.
- If the app restarts, the generic observation disappears. A saved user-created
  draft remains clearly labeled as a draft but cannot acquire new machine
  evidence from another session.
- If line/ship catalog normalization fails, share-sheet export is still allowed
  with bounded `Other` fields; service submission is quarantined.
- If a submitted report conflicts with the public directory, it is accepted as
  another observation unless abuse checks fail. Conflict is information.
- If deletion fails offline, the receipt remains and retry is manual or follows
  the same explicit queued-action rules.
- If server policy or schema version advances, old reports remain importable for
  preview but are not silently rewritten. A migration must show any newly
  submitted fields to the user again.

## Relationship to the existing field-report issue

The current [field-report template](../.github/ISSUE_TEMPLATE/field_report.md)
already asks for cruise line, ship, whether two phones communicated instantly,
and Wi-Fi retention. It should evolve to:

- accept the v1 JSON attachment;
- use the report verdict vocabulary;
- ask whether the result came from an authenticated LAN link or guided test;
- avoid exact dates, cabin, itinerary, SSID, and network addresses;
- state that GitHub issues are public and tied to a GitHub account; and
- keep narrative delivery, battery, and notification questions separate from
  the machine compatibility artifact.

Narrative reports are valuable qualitative evidence, but only qualifying JSON
evidence affects an automated compatibility headline.

## Rollout

### Phase 0 — schema fixtures and manual intake

- Add shared core enums, reducer, serializer, and golden fixtures.
- Update the GitHub field-report template to accept a fixture-compatible JSON
  attachment.
- Validate sample reports from permissive, client-isolated, no-peer, VPN-denied,
  and multicast-filtered test networks.
- Keep the feature behind internal tools.

### Phase 1 — guided local report and share sheet

- Add the current-network evidence reducer to both shells.
- Add the guided two-phone test.
- Add line/ship selection, preview, share sheet, save/delete, and 30-day expiry.
- Do not add any CruiseMesh-operated upload endpoint.
- Run the feature on at least two real sailings or equivalent captive-portal
  networks and review every emitted field manually.

### Phase 2 — private intake service

- Complete privacy/store disclosures.
- Deploy strict POST/DELETE endpoints and deletion receipts.
- Keep compatibility output private to maintainers while aliasing, abuse, and
  aggregation behavior are evaluated.
- Compare accepted reports to manually reviewed attachments.

### Phase 3 — public ship directory

- Publish count-and-recency summaries under the rules above.
- Add an in-app read-only ship lookup if useful, cached for offline access.
- Never gate or reorder the current network's runtime transport attempt solely
  from directory data.

## Validation and acceptance gates

### Shared reducer

- The same event fixture produces byte-identical canonical JSON through Android
  and iOS UniFFI callers.
- An authenticated LAN link always yields a positive verdict even when mDNS
  failed.
- An all-silent `/24` sweep without peer evidence yields `no_peer_evidence`.
- Policy denial always yields `os_or_vpn_interference`.
- Only the completed bidirectional guided sequence yields
  `qualifying_negative`.
- Network loss and process restart clear generic evidence.
- Refused ports alone never yield `likely_isolated`.

### Privacy

- Golden reports contain none of the forbidden fields.
- Property tests feed IP, MAC, SSID-like, contact, and message-like strings into
  every input surface and confirm they cannot appear in serialized output.
- Unknown JSON fields are rejected.
- Share preview is rendered from the exact object exported, not a parallel
  summary model.
- General diagnostic files are absent from every report/share intent.
- Drafts and deletion secrets are absent from identity backups.
- No request is emitted before the final per-report action.

### Guided test

- Android-to-Android, iOS-to-iOS, and Android-to-iOS on a permissive LAN.
- All three pairs on a client-isolated LAN.
- mDNS blocked but direct TCP allowed.
- VPN or local-network permission denial on either participant.
- One phone loses Wi-Fi during each test phase.
- Coordination link drops while direct LAN succeeds and while it fails.
- Unknown/old client ignores optional coordination frames safely.
- The test never contacts an address other than the accepted contact's
  validated endpoint and never exceeds attempt/time caps.

### UX and accessibility

- Report can be completed entirely offline through saving/share sheet.
- Decline/cancel is as prominent as submit.
- Every serialized field appears in the preview in plain language.
- Screen readers announce verdict, evidence limitations, submission action, and
  deletion receipt.
- Long line/ship names and translations do not truncate the consent meaning.
- Year-only period and no-device-model paths are first-class.

### Service

- POST is idempotent by report nonce.
- Invalid size, enum, period, catalog, forbidden content, and unknown keys fail
  closed.
- DELETE removes the report from recomputed aggregates.
- Request bodies and deletion secrets never enter logs.
- Aggregate state transitions exactly match published-state fixtures.
- Reports older than 18 months become stale; individual rows age out at 36
  months.
- A burst of duplicates is quarantined without requiring a stable client ID.

## Success criteria

The design succeeds when CruiseMesh can truthfully show a statement such as:

> Local Wi-Fi was observed working on this ship in two recent reports. Ship
> configurations can change; CruiseMesh will test yours automatically.

and every report behind that statement was:

- deliberately submitted;
- scoped to a named ship and coarse period;
- backed by authenticated transport evidence;
- free of relationship and network identifiers;
- visible to the contributor before submission; and
- removable without an account.

Report volume is not itself a success metric. A smaller trustworthy dataset is
more valuable than automatic telemetry that weakens the project's privacy
contract or converts ambiguous network failures into false fleet-wide claims.
