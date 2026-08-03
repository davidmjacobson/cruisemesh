# Relay operator console

Status: **designed, deliberately not built.** Build triggers in the last
section. Written 2026-07-27 after the question "should there be an admin
dashboard for the relay?"

## The short answer

Yes to an operator console, no to putting it on the relay.

The relay is the one host that holds every family's member token, every
family's deposit token, and every sealed envelope in flight. Adding an
authenticated HTML surface to that host means adding session handling,
CSRF and XSS exposure, and a dependency tree that must be patched forever
— to the single machine whose compromise is worst. `relayd` is a
deliberately dumb mailbox (DESIGN.md §9) and should stay one.

Everything an operator needs is already reachable from a host that is *not*
that machine: the cruisemesh-web Worker already holds `RELAY_ADMIN_TOKEN`,
already calls `GET /admin/families`, already owns the D1 `purchases` table,
and already runs the weekly reconciliation that joins the two. The console
belongs there, as read-mostly routes under `/admin`, and the relay's admin
API keeps its current shape: token-authenticated, no browser ever pointed
at it.

## What is actually missing today

Not monitoring — that gap closed on 2026-07-27. The Worker's 15-minute
`/healthz` cron emails on outage transitions, the weekly reconciliation
emails only when Stripe and the relay disagree, and the relay's nightly
backup emails on backup or disk failure. Health is **pushed**, and a
dashboard nobody opens is worse than an email that arrives.

What is missing is **support lookup**, and it has one shape:

> A customer emails support@cruisemesh.app saying their pass does not work.
> Answering requires knowing: did their payment succeed, was a family
> provisioned, what is its status and expiry, and are they near quota?

Today that answer lives in three places — Stripe, D1, and the relay — and
assembling it needs a laptop, an SSH key, and `tools/relay_admin.sh`. It
cannot be done from a phone, which is where support email is read.

The secondary gap is that `relay_admin.sh` is **write-capable by design**
(`suspend`, `purge --yes`). Routine questions should not require reaching
for a tool that can destroy a mailbox.

## Non-goals

- **Not a metrics/graphing system.** No time series, no charts. At family
  scale the numbers are small integers.
- **Not a replacement for `relay_admin.sh`.** The CLI stays the break-glass
  path and the only way to do anything genuinely destructive.
- **Not multi-operator.** One operator, and an access list of exactly one
  identity. No roles, no user management, no invitations.
- **Not customer-facing.** Customers get email and `/support`.
- **Never a SQL console.** No arbitrary query surface, ever.

## Where it lives

Routes on the existing cruisemesh-web Worker:

```
/admin            overview
/admin/families   list + filter
/admin/f/:prefix  one family
/admin/lookup     find by purchase email
```

`run_worker_first` is already true, so the Worker sees these before the
asset router. No new service, no new host, no new deploy target.

## Authentication

**Cloudflare Access in front of `/admin*`, with the Worker verifying the
assertion itself.** Two independent gates, and we write no login code:

1. **Access policy** — allow exactly one Google identity. Unauthenticated
   requests never reach the Worker. Free tier covers this comfortably.
2. **Worker-side verification** — every `/admin*` request must carry a valid
   `Cf-Access-Jwt-Assertion`, verified against the team's public keys
   (cached), with `aud` matched to the application and the email claim
   checked against an allowlist var. A misconfigured or removed Access
   policy must fail closed, not silently publish the console.

Rationale: rolling our own auth on a surface that can suspend a paying
customer's mailbox is exactly the kind of bespoke construction the project
refuses to build elsewhere (DESIGN.md §6.1). Access is the same reasoning
applied to ops.

If Access is ever unavailable, the fallback is **not** a password — it is
leaving the console unbuilt and using the CLI.

## Data sources and joining

| Source | Provides |
|---|---|
| D1 `purchases` | checkout email, session id, plan, provisioned/emailed timestamps, expiry |
| `GET /admin/families` (relay) | live status, quota bytes used, envelope count, expiry, plan, note |
| `GET /healthz` (relay) | up/down, deployed commit |
| D1 `ops_state` | last outage transition, last reconciliation result |

The join key is the family token. **The console displays only the 12-character
prefix**, the same truncation `relay_admin.sh list` already uses and enough
to identify a row and drive the CLI. Full tokens are never rendered, never
logged, and never placed in a URL.

Stripe is deliberately *not* called live. The purchase record in D1 is the
system of record for "did they pay"; a link out to the Stripe dashboard
covers the rare case needing more.

## Screens

Mobile-first. The whole point is answering a support email from a phone, so
every screen must be usable one-handed at 375 px. Server-rendered HTML in
the existing site styles, no client framework, no build step — matching how
the rest of the site is written.

### `/admin` — overview

- Relay: up/down, deployed commit, last outage transition.
- Counts: active families, suspended, expiring within 7 days.
- Storage: total sealed bytes, and any family above 80 % of quota.
- Last successful reconciliation, and its verdict.
- Anything the last reconciliation flagged, shown inline — the same
  content as the alert email, so the console and the email never disagree.

### `/admin/families` — list

One row per family: token prefix, plan, status badge, expiry (relative:
"in 12 days"), usage bar, envelope count, and the purchase email when D1
has one. Default sort puts problems first: suspended, then expiring
soonest, then highest usage. Filters: status, expiring soon, over quota,
unmatched (relay family with no purchase, or purchase with no family).

### `/admin/f/:prefix` — one family

Everything the list shows, plus the D1 purchase record beside the relay
record — the exact join that currently requires three tools. Then the
actions, in two visually distinct groups:

- **Safe:** extend expiry (+30 / +90 days), resume a suspended family.
- **Dangerous:** suspend, revoke. Each requires typing the token prefix to
  confirm, the way `purge --yes` demands intent today.
- **`purge` is not exposed at all.** Irreversible destruction of a paying
  customer's mailbox stays on the CLI, over SSH, deliberately inconvenient.

### `/admin/lookup` — by purchase email

One field, one answer. Given an email: the purchase, whether provisioning
completed, the live family status, and a link to the family detail. This is
the screen that justifies the whole console.

## Write path

All writes proxy to the relay admin API using the Worker's existing
`RELAY_ADMIN_TOKEN`; the browser never holds a relay credential. Every write:

- is a POST with an origin check and the Access assertion re-verified,
- is written to a D1 `admin_audit` table (timestamp, operator email from the
  Access claim, action, token prefix, outcome) — because an action that can
  suspend a customer must be attributable after the fact,
- and reports the relay's own response verbatim on failure rather than a
  generic error.

## Security requirements

These are the properties that make the console safe to exist. A change that
breaks one of them means the console should be withdrawn, not patched
around:

1. Nothing on the relay host changes. No new port, no new binding, no
   browser-facing surface on the machine holding envelopes.
2. Full family tokens are never rendered, logged, or put in a URL.
3. Sealed envelope contents are never fetched or displayed. The console
   shows counts and byte totals, never payloads — the relay cannot read
   them and neither can its operator.
4. Fails closed: no valid Access assertion, no response.
5. No arbitrary query interface.
6. Every state-changing action is audited with the operator's identity.
7. The console can be deleted entirely — reverting to the CLI — without
   affecting delivery, provisioning, or billing.

## Build triggers

Not yet. At 7 families, one of them a test fixture, the CLI is adequate and
the alerting covers health. Build it when **any** of these is true:

- Paying customers who are not friends or family exist in more than
  token numbers, so support email arrives from strangers on their schedule.
- A support question has needed an SSH session more than a handful of
  times, or has had to wait for a laptop.
- Family count passes roughly 25, where scanning `relay_admin.sh list`
  output stops being pleasant.
- The weekly reconciliation starts finding real discrepancies that need
  investigating rather than merely reporting.

Phase order when the time comes, each independently useful:

1. `/admin/lookup` and `/admin/f/:prefix`, read-only. This alone solves the
   support workflow and is a day of work.
2. `/admin` overview and `/admin/families`.
3. Safe writes (extend, resume) plus the audit table.
4. Dangerous writes (suspend, revoke) behind typed confirmation — or never,
   if the CLI keeps proving sufficient.

Stop after any phase that turns out to be enough.
