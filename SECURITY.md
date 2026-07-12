# Security Policy

## Reporting a vulnerability

**Please do not report security issues in public GitHub issues.**

Use GitHub's private vulnerability reporting: go to this repository's
**Security** tab → **Report a vulnerability**. Reports go directly and
privately to the maintainer.

You can expect an acknowledgment within a week. This is a solo-maintained
project, so please calibrate expectations accordingly — but security reports
jump the queue.

## What counts

Especially interesting:

- Anything that lets a relay, mule (a stranger's phone carrying envelopes),
  or network observer learn message contents, sender identity, or read
  state — the design intends them to learn none of these
  ([SECURITY-DESIGN.md](SECURITY-DESIGN.md)).
- Envelope forgery, receipt forgery, or group-key exposure.
- relayd authentication bypass or cross-family data access.
- Key material leaving the device (it never should).

Out of scope: denial of service via radio jamming or BLE flooding (physical
proximity attacks on availability are accepted limitations of the medium),
and traffic-analysis observations already documented as known trade-offs in
SECURITY-DESIGN.md.

## Honest status

CruiseMesh's cryptographic design is deliberately boring (libsodium
primitives used whole, documented in one page), but it has **not yet had an
independent security review**. Until it has, treat it as suitable for its
stated threat model — "no internet," not nation-states — and read
[SECURITY-DESIGN.md](SECURITY-DESIGN.md) before trusting it with anything
beyond family logistics.

## Supported versions

Pre-1.0: only the latest commit on `master` is supported. There are no
backported fixes.
