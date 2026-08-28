# Security Policy

## Reporting a vulnerability

**Please do not report security issues in public GitHub issues.**

Use GitHub's private vulnerability reporting: go to this repository's
**Security** tab → **Report a vulnerability**. Reports go directly and
privately to the maintainer.

You can expect an acknowledgment within a week. This is a solo-maintained
project, so please calibrate expectations accordingly — but security reports
jump the queue.

**This page is for security vulnerabilities only.** To report abusive
behaviour, harassment, or objectionable content from someone using the app,
email abuse@cruisemesh.app — [`docs/moderation.md`](docs/moderation.md)
describes the reporting and blocking process and what happens after a report
arrives.

## What counts

Especially interesting:

- Anything that lets a relay, mule (a stranger's phone carrying envelopes),
  or network observer learn message contents, sender identity, or read
  state — the design intends them to learn none of these
  ([SECURITY-DESIGN.md](SECURITY-DESIGN.md)).
- Envelope forgery, receipt forgery, or group-key exposure.
- relayd authentication bypass or cross-family data access, including a
  post-only deposit token that gets fetch, ack, or WebSocket access.
- Anything on the same-LAN transport: impersonating a contact through the
  Noise XX handshake, or getting a phone to accept or forward an endpoint it
  wasn't told about by its owner.
- Key material leaving the device (it never should).

Out of scope: denial of service via radio jamming or BLE flooding (physical
proximity attacks on availability are accepted limitations of the medium),
and traffic-analysis observations already documented as known trade-offs in
SECURITY-DESIGN.md.

## Dependency, advisory and license policy

This section is the written half of a pair. The other half is
[`deny.toml`](deny.toml), enforced by
[`.github/workflows/dependency-audit.yml`](.github/workflows/dependency-audit.yml)
on every pull request, every push to `master`, and once a day. If this page and
that file ever disagree, that is a bug in one of them; fix both in the same
change.

### What blocks a release

A release is cut from a tag, and a tag is pushed only at a commit whose
`Dependency audit` job is green on `master`. That job fails — and therefore the
release does not happen — when any of these is true:

- **A security advisory applies to the dependency graph**: any RustSec
  advisory, at any severity. There is no threshold below which one is waved
  through. `cargo deny` has no severity filter and this project does not want
  one; severity decides how long an exception may stand (below), never whether
  an exception is needed.
- **A crate version in `Cargo.lock` has been yanked.** The author withdrew it;
  that is reason enough.
- **A crate is unmaintained**, including deep in the transitive graph.
- **A dependency's license is not on the allowlist** below.
- **A dependency comes from anywhere but crates.io.** A git dependency or a
  second registry is code with no published provenance and no advisory
  database behind it.

Duplicate versions of one crate are the deliberate exception: they warn and do
not fail. That is a binary-size and tidiness matter, and the graph carries a
dozen of them for reasons outside this project's control.

### How fast

The clock starts when the advisory is published or when the daily audit first
goes red, whichever is earlier. The daily run exists so that an advisory filed
against code nobody touched that week is still noticed within a day.

| Severity | Response |
| --- | --- |
| Critical or high | No further release ships until it is fixed or mitigated. Fixed within **7 days**. A build already sitting in a store review queue is pulled or superseded rather than left to be approved. |
| Medium | Fixed within **30 days**, or in the next release, whichever comes first. |
| Low, or an unsoundness advisory | Fixed within **90 days**, or converted into a written exception. |
| Unmaintained crate | No deadline, but it must carry an exception entry to stay, and that entry is re-read at every release. |

Anything in `relayd` is handled one step above its published rating. It is the
one always-on, internet-facing component, and the only place a vulnerability
can be reached by someone who is not standing next to a phone.

### When there is no fix

An advisory with no fixed version available is the case this policy exists for,
and "wait and hope" is not one of the options.

1. **Establish reachability.** Say, in writing, whether the vulnerable path is
   reachable from the shipped app, from `relayd`, or from neither — build
   scripts and dev-dependencies are the common "neither".
2. **If it is not reachable**, add an `ignore` entry to `deny.toml` carrying
   the advisory id and a reason that states why the risk is accepted and when
   the exception is next reviewed. All three parts are required: an entry
   without a reason is not an exception, it is a silence.
3. **If it is reachable**, the dependency is patched, forked, vendored or
   removed. Shipping with it is the last resort, requires a mitigation written
   down here, and is still bound by the deadlines above whether or not
   upstream has moved.
4. **Every exception is re-read at every release.** One whose stated review
   has passed counts as an open finding, not as settled.

Exceptions live in `deny.toml` and nowhere else, so the list of accepted risks
is always exactly the list the tool is enforcing.

### License allowlist

These, and only these, may appear in the dependency graph. Adding one is a
deliberate pull request against [`deny.toml`](deny.toml), never something that
arrives with a transitive version bump:

`AGPL-3.0-or-later`, `Apache-2.0`, `Apache-2.0 WITH LLVM-exception`,
`BSD-1-Clause`, `BSD-2-Clause`, `BSD-3-Clause`, `BSL-1.0`,
`CDLA-Permissive-2.0`, `ISC`, `LGPL-2.1-or-later`, `MIT`, `MPL-2.0`,
`Unicode-3.0`, `Unlicense`, `Zlib`.

A license expression is accepted only when the detector is at least 80%
confident of it, so a crate whose licensing is too vague to identify fails
rather than being guessed at. The app itself ships under `AGPL-3.0-or-later`.
The two copyleft entries on the list (`LGPL-2.1-or-later`, `MPL-2.0`) are
library- and file-scoped and are compatible with shipping a store binary.
Stronger copyleft inside a dependency is a decision for a human, and is
deliberately absent from the list.

### Dependency updates

[Dependabot](.github/dependabot.yml) opens weekly update pull requests for
Cargo, Gradle and GitHub Actions. Those pull requests run the same audit as
everything else. They are a convenience for staying current, not the mechanism
that enforces any of the above.

## Honest status

CruiseMesh's cryptographic design is deliberately boring (libsodium
primitives used whole, documented in one page), but it has **not yet had an
independent security review**. Until it has, treat it as suitable for its
stated threat model — "no internet," not nation-states — and read
[SECURITY-DESIGN.md](SECURITY-DESIGN.md) before trusting it with anything
beyond family logistics.

## Supported versions

Only the current release (1.0.7 on both platforms) and the latest commit on
`master` are supported. Fixes ship forward in the next release; there are no
backports to older builds.
