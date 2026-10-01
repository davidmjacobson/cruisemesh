# Deploying `cruisemesh-relayd`

Replaces the localtunnel hack used during Milestone 3 bring-up. The relay is a
deliberately dumb mailbox (DESIGN.md §9): HTTPS in front of an Axum + SQLite
process that stores sealed envelopes only. It never sees plaintext.

## What you need

- A cheap Linux VPS (1 vCPU / 512 MB is enough) with a public IPv4 address
- A DNS name pointing at that VPS (e.g. `relay.example.com` → A record)
- Docker Engine + Compose plugin
- Ports **80** and **443** open (Caddy uses them for ACME + HTTPS)

## 1. Provision family tokens

Each family gets one long random bearer token — the **member** token. Phones
send it as `Authorization: Bearer <token>` on every request.

```sh
# One token per family (store these somewhere safe; rotating is a re-QR).
openssl rand -hex 32
```

**Use a long random value, not a memorable phrase.** Beyond ordinary
credential hygiene, relayd derives a semi-public deposit token (next section)
from this one with a plain hash — a guessable member token could be
brute-forced offline by anyone holding the deposit token.

### Token classes

Every family has **two** credentials:

| Class | Token | Can do | Rides on |
|---|---|---|---|
| **member** | the token you provision (env var or admin API) | post, fetch, ack, presence, WebSocket | Shore Pass setup card (`CMRELAY1`), each family phone's own config |
| **deposit** | `cmdep1-` + base64url(BLAKE2b-256(context ‖ member token)), derived automatically | `POST /envelopes` only | friend cards (`CMFRIEND…`) |

You never provision or distribute the deposit token yourself: relayd derives
and stores it automatically (at provisioning for new families, at startup
for pre-existing ones), and phones derive the identical value locally when
stamping a friend card. Every operation other than posting an envelope is
rejected for a deposit token with:

```
HTTP 403 Forbidden
{ "error": "deposit tokens can only post envelopes; ...", "code": "deposit_only" }
```

Enforcement sits in the shared authorization path, ahead of every handler.
Envelopes posted with a deposit token land in the family's one mailbox
(keyed by the member token), count against the same storage quota, and obey
the same suspension/expiry rules; only the rate-limit buckets differ (§10).

This closes the older hole where friend cards carried the full family
token, so a publicly posted card let strangers fetch and ack (= delete)
family mail. **Upgrade the relay before (or with) the phones**: current
apps put deposit tokens on friend cards, and a relayd predating the split
does not recognize them (contacts' posts would 401). All existing tokens migrate as
member class — zero behavior change for existing families — and old
full-token friend cards keep working, since member tokens still post.

Multiple families on one server: comma-separate tokens.

```sh
export CRUISEMESH_RELAY_TOKENS="$(openssl rand -hex 32),$(openssl rand -hex 32)"
```

## 2. Clone and configure

```sh
git clone <your-repo-url> CruiseMesh
cd CruiseMesh/relayd

export RELAY_DOMAIN=relay.example.com
export CRUISEMESH_RELAY_TOKENS="<paste token(s) here>"
```

Optional: put the exports in a root-only `.env` next to `docker-compose.yml`
(Compose loads it automatically). **Do not commit `.env`.**

`.env` holds only values that stay true between deploys — the domain, the
tokens, the APNs settings. **Never put `GIT_SHA` in it**; §3.1 explains what
that cost. An older version of `provision-hetzner.sh` wrote one, so on an
existing box delete the line if it is still there:

```sh
sed -i '/^GIT_SHA=/d' /opt/cruisemesh/relayd/.env
```

Nothing reads it any more, so a leftover line is inert rather than dangerous —
but it invites the next person to trust it.

## 3. Start

```sh
../tools/relay_deploy.sh
docker compose ps
curl -fsS "https://${RELAY_DOMAIN}/healthz"
# → {"status":"ok","version":"0.1.0","commit":"abc1234"}
```

Caddy obtains a Let's Encrypt cert for `RELAY_DOMAIN` on first start. If
`/healthz` fails, check `docker compose logs caddy` (DNS not pointed yet is
the usual cause).

### 3.1 Redeploying a new commit

```sh
git -C /opt/cruisemesh pull --ff-only origin master
/opt/cruisemesh/tools/relay_deploy.sh
```

That is the whole upgrade procedure. The script reads `HEAD` from the checkout,
passes it to the image build as `--build-arg GIT_SHA=…` for that invocation
only, starts the stack, then reads `/healthz` back and fails if the commit it
reports is not the one just built.

**Use it instead of `docker compose up -d --build`.** The commit in `/healthz`
is baked in at build time, and the build context deliberately excludes `.git`
(see `relayd/Dockerfile`), so the image cannot work out its own commit. It used
to be handed in from a static `GIT_SHA=` line in `.env`, which meant pulling
new code without also hand-editing `.env` produced a relay that ran the new
commit and reported the previous one. `/healthz` is exactly what you consult
when you are unsure whether a fix is live, so a wrong answer there is worse
than no answer — that one sent two deploy investigations chasing changes which
had in fact already shipped. A build with no `GIT_SHA` now fails outright
rather than guessing.

Two consequences:

- The script **refuses to deploy a tree with uncommitted changes to tracked
  files**, since the image would not be the commit it claims. Commit or revert
  them, or set `ALLOW_DIRTY=1` to proceed — the image is then stamped
  `<sha>-dirty`, so `/healthz` keeps saying that what is running is not exactly
  any commit. Untracked files (`.env`, a private
  `docker-compose.override.yml`, backups) are ignored; they do not change what
  gets compiled.
- `docker compose build --build-arg GIT_SHA=$(git rev-parse --short HEAD) relayd`
  followed by `docker compose up -d` is the same thing by hand, if you need to
  take it a step at a time.

## 4. Point phones at the relay

On each phone, the saved relay config (Shore Pass screen, under "Custom
relay") should be:

| Field | Value |
|---|---|
| relay URL | `https://relay.example.com` (no trailing slash) |
| relay token | the same family **member** token from step 1 |

Friend cards take care of themselves: when a phone with this config shares
a card, it stamps the derived **deposit** token onto it — contacts can
post into the family mailbox but never read it. Do not hand the member
token to people outside the family.

The Android client uploads queued envelopes and polls
`GET /envelopes?hints=...` with that bearer token. With a live network path
it can also open `wss://relay.example.com/ws?hints=...&after=...` for push
(see §6).

## 5. Environment reference

| Variable | Default | Notes |
|---|---|---|
| `CRUISEMESH_RELAY_TOKENS` | *(required unless admin API on)* | Comma-separated static allowlist. May be empty when `CRUISEMESH_RELAY_ADMIN_TOKEN` is set (families provisioned dynamically); if both are unset the process refuses to start. |
| `CRUISEMESH_RELAY_ADMIN_TOKEN` | *(unset = admin API off)* | Bearer token for the `/admin/families` provisioning API (§12). Unset → admin routes answer 404. Self-hosted deploys don't need it. |
| `CRUISEMESH_RELAY_DB` | `cruisemesh-relayd.sqlite` | **Use an absolute path.** Relative paths resolve against the process CWD, which is easy to get wrong under systemd/Docker/IDE launchers. |
| `CRUISEMESH_RELAY_BIND` | `0.0.0.0:8080` | Inside Docker keep `0.0.0.0:8080`; Caddy is the public listener. |
| `CRUISEMESH_RELAY_FAMILY_QUOTA_BYTES` | `268435456` (256 MiB) | Per-family-token storage quota. See §10. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_RELAY_WS_PER_TOKEN_MAX_CONNECTIONS` | `16` | Max concurrent `GET /ws` connections for a single family token. See §7. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_RELAY_WS_GLOBAL_MAX_CONNECTIONS` | `256` | Max concurrent `GET /ws` connections across all family tokens combined. See §7. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_RELAY_RATE_REQUESTS_PER_MIN` | `600` | Requests per minute allowed for a single family **member** token (also the burst size). See §10. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_RELAY_RATE_BYTES_PER_MIN` | `67108864` (64 MiB) | Uploaded `sealed` bytes per minute allowed for a single family **member** token (also the burst size). See §10. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_RELAY_DEPOSIT_RATE_REQUESTS_PER_MIN` | `60` | Requests per minute allowed for a single family **deposit** token (also the burst size). See §10. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_RELAY_DEPOSIT_RATE_BYTES_PER_MIN` | `6291456` (6 MiB) | Uploaded `sealed` bytes per minute allowed for a single family **deposit** token (also the burst size). See §10. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_RELAY_RATE_GLOBAL_REQUESTS_PER_MIN` | `6000` | Requests per minute across all tokens combined — the coarse backstop. See §10. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_RELAY_DEPOSIT_PRESENCE_QUERIES` | `4` | Cross-family `POST /presence` queries allowed for a single family **deposit** token per window (also the burst size). Charged to its own bucket, never the family's request/byte allowance. See §10. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_RELAY_DEPOSIT_PRESENCE_WINDOW_SECS` | `900` | The window the above allowance is spread over. See §10. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_APNS_KEY_ID` | *(unset = APNs off)* | Apple push notification key ID. Set together with the next three fields; see §7.1. |
| `CRUISEMESH_APNS_TEAM_ID` | *(unset = APNs off)* | Apple Developer Team ID. |
| `CRUISEMESH_APNS_BUNDLE_ID` | *(unset = APNs off)* | App topic, currently `com.cruisemesh.app`. |
| `CRUISEMESH_APNS_PRIVATE_KEY_FILE` | *(unset = APNs off)* | Absolute container path to the private `.p8` provider key. Never commit or bake it into the image. |
| `CRUISEMESH_APNS_ENVIRONMENT` | `production` | `production`, `sandbox`, or `development` (`development` aliases `sandbox`). |
| `RELAY_DOMAIN` | *(compose required)* | Hostname in the Caddyfile for TLS. |

`GIT_SHA` is **not** in that table and does not belong in `.env`: it is a
build-time argument, supplied per deploy by `tools/relay_deploy.sh` and baked
into the image, not a setting the running process reads. See §3.1.

### The `CRUISEMESH_RELAY_DB` path gotcha

During live bring-up the Windows binary was started without
`CRUISEMESH_RELAY_DB` set, so it quietly used the relative default
`cruisemesh-relayd.sqlite` in whatever directory the shell happened to be in.
Uploads “worked” (HTTP 200) while the SQLite file under `tmp/` that we were
watching stayed empty. **Always set an absolute DB path**, and if uploads
look fine but the DB you care about is empty, inspect the *process* environment
before trusting the path:

```sh
# Linux
tr '\0' '\n' < /proc/$(pidof cruisemesh-relayd)/environ | grep CRUISEMESH

# Docker
docker compose exec relayd printenv CRUISEMESH_RELAY_DB
```

The compose file pins `CRUISEMESH_RELAY_DB=/data/cruisemesh-relayd.sqlite` on a
named volume so this cannot silently drift.

## 6. Retention and API shape (ops notes)

- **Per-envelope `expiry_ms`**: clients send this (core default is 7 days). Rows
  with `expiry_ms <= now` are excluded from `GET /envelopes` responses
  immediately, and physically deleted (freeing disk) by the hourly
  background maintenance sweep — see §11. Fetch itself no longer deletes
  anything, so polling never does a write.
- **30-day server ceiling**: insert clamps `expiry_ms` to
  `created_at_ms + 30 days`; the background sweep also drops rows whose
  `created_at_ms` is older than 30 days (belt-and-suspenders for any
  pre-clamp data).
- **Dedupe**: unique on `(family_token, msg_id)`. Re-posts of the same msg_id
  (e.g. receipt envelopes re-uploaded every sync) are idempotent.
- **Ack is delete**: `POST /envelopes/ack` with `{ "ids": [...] }` removes
  those rows for the caller's family only. Fetch is non-destructive — clients
  can re-poll after a crash without losing mail.
- **Content-agnostic**: sealed blobs only. Text and receipt envelopes share the
  same routes; the server never inspects `kind`.
- **WebSocket push** (`GET /ws`): see §7. Acks remain `POST /envelopes/ack`;
  poll stays available and unchanged for offline/reconnect catch-up.
- **Self-service token rotation** (`POST /family/rotate`): see §6.1.
- **Pass status read** (`GET /family/status`): see §6.2.

### 6.1 Self-service token rotation (`POST /family/rotate`)

The one route where a family credential authorizes changing that credential.
It exists because every table here is scoped by `family_token`, so a phone that
has been removed from its owner's device roster keeps full fetch *and ack*
access to the family mailbox — and ack deletes — until the token itself moves.
Waiting for an operator would make "remove this stolen phone" a support ticket
(`specs/multi-device-v1.md` §10 step 2).

```sh
curl -sS -X POST https://relay.example.com/family/rotate \
  -H "Authorization: Bearer $CURRENT_MEMBER_TOKEN" \
  -H 'content-type: application/json' \
  -d '{"new_token":"cmfam1-...",
       "rotation_pk":"<32 bytes, base64url, no padding>",
       "rotation_sig":"<64 bytes, base64url, no padding>"}'
# → {"family_token":"cmfam1-…","deposit_token":"cmdep1-…",
#    "envelopes_moved":42,"rotated":true}
```

| Concern | Behavior |
|---|---|
| Auth | The family's **member** token **and** an Ed25519 signature (below). A deposit credential gets `403 deposit_only` before anything else — deposit tokens ride friend cards, so a rotatable one would let anyone the family ever showed a QR code lock them out. |
| Who picks the token | The **client**, which is what makes a lost response survivable: it writes the candidate down before calling, so it can always ask again. The app mints one with 32 bytes of randomness behind a `cmfam1-` prefix. Minimum accepted length is 24 characters — the only entropy check a server can make from outside. |
| Idempotency | Presenting the *new* token returns `rotated: false` with the same values. That is the recovery path after a dropped connection, and it is a success, not a conflict. The retry must be signed too, over `(new, new)`. |
| What moves | Envelopes (ids, hints and fetch order untouched) and presence, in one transaction. Nothing is deleted: a rotation must never cost a sibling an un-fetched row. |
| What is purged | **Push registrations.** Each row is one device's APNs wake channel, and the relay cannot tell the revoked device's from a sibling's — carrying them across would leave the evicted phone still being woken for the family's mail. Siblings re-register on their next round, so the cost is one round of notification latency. |
| What dies | The old member token **and** its derived deposit token, at once. Friend cards minted from the old token stop depositing immediately; the app repairs contacts with a `CAP_RELAY_UPDATE` notice carrying the new deposit token, and anyone sharing the pass with a re-shared `CMRELAY1` setup card. |
| Static families | `409 rotation_unsupported`. A token from `CRUISEMESH_RELAY_TOKENS` has no row to re-key — change the env var and hand out a new setup card. |
| Taken token | `409 rotation_token_taken` if another family already holds the proposed token as its member *or* deposit credential. |
| Not authorized | `403 rotation_unauthorized` — the token is real but the signature is not the family's. See below. |
| Malformed signature | `400` for a missing field, unparseable base64, or a wrong decoded length. Distinct from the 403 on purpose: the remedy is to fix the encoder, not to find a different key. |
| Rate limit | Its own small bucket, **not** the family's shared request allowance (§10). |

#### Rotation authority (why the token alone is not enough)

The device this ceremony exists to evict is holding the family's member token.
If possession authorized the rotation, that device could run the ceremony first
and lock the owner out. So a rotation must also be signed:

```
message = b"CruiseMesh family token rotation v1\0"
       || u16_be(len(current_token)) || current_token
       || u16_be(len(new_token))     || new_token
```

`current_token` is the bearer token presented on the request (trimmed);
`new_token` is the trimmed replacement. `rotation_pk` is the 32-byte Ed25519
public key, `rotation_sig` the 64-byte signature over those bytes, both
base64url **without** padding.

Verification happens inside the same transaction that performs the re-key:

- **No key registered yet** (`rotation_pk` NULL, which is every family
  provisioned before this shipped): the signature is checked against the
  *presented* key, and if it verifies that key is written to the row —
  trust on first rotation.
- **A key is registered**: the presented key must be that key, and the
  signature must verify under it. A different key is refused without its
  signature being examined.

Two consequences worth stating to an operator plainly:

1. **After a family's first rotation, exactly one key can ever rotate it
   again.** There is no recovery here for a family that loses that key short
   of re-provisioning. On a shared Shore Pass this means only the organizer's
   person root can rotate — which matches who actually administers a shared
   pass.
2. **A thief can race trust-on-first-rotation, once.** On the very first
   rotation of a legacy family there is no stored key to check against, so a
   revoked device holding a live member token could register its own key
   first and keep the authority. This residual is accepted deliberately and is
   bounded three ways: families provisioned before this shipped only, until
   their first rotation only, and it requires the hostile device to still hold
   a valid member token. Refusing legacy families their first rotation instead
   would leave exactly the families most likely to need a revocation unable to
   perform one.

Operational note: after a rotation the family appears under a **new token** in
`GET /admin/families`, but its `family_id` is unchanged — record that, not the
token, if you track a customer across time (§12).

### 6.2 Pass status (`GET /family/status`)

What each phone's Shore Pass surface reads to say when internet delivery runs
out, and to decide whether to offer a renewal. Member credential only — a
deposit token is refused with the same `deposit_only` 403 as any other
member-only op, because when someone else's pass lapses is not a friend's
business.

```sh
curl -sS https://relay.example.com/family/status \
  -H "Authorization: Bearer $MEMBER_TOKEN"
```

```json
{"plan": "shore-pass", "expires_ms": 1767225600000, "state": "active"}
```

- `state` is `active`, `grace` (past `expires_ms` but inside
  `FAMILY_EXPIRY_GRACE_MS`: queued mail still drains, new envelopes are
  refused) or `suspended` (an administrative suspension, or an expiry past the
  grace window — from a phone's side those are the same fact, and a client
  that wants to tell them apart already holds `expires_ms`).
- `expires_ms` is `null` for a family with no end date. `plan` is `null` for a
  static env-allowlist family (`CRUISEMESH_RELAY_TOKENS`), which has no
  `families` row because no pass was ever sold for it; those read as `active`
  with no expiry. A phone shows no delivery date at all in that case rather
  than inventing one, and offers no renewal.
- **This is the one route that answers while the family is suspended or past
  its grace window.** Every other authenticated route 403s in those states,
  which are exactly the states the renewal prompt exists for — a 403 here
  would leave the phone with nothing to show. Nothing else is relaxed: the
  class boundary above still applies, the read is of the caller's own row
  only, and no field is exposed that the family does not already hold.
- Every field already lives on the `families` row, so there is **no schema
  change** and nothing new is written. It costs one request unit, like
  `GET /envelopes`.

## 7. WebSocket push (`GET /ws`)

Phones with a live internet path can subscribe instead of only polling:

```
wss://relay.example.com/ws?hints=<base64url,...>&after=<cursor>
```

| Concern | Behavior |
|---|---|
| Auth | Same family bearer token as REST. Prefer `Authorization: Bearer <token>` on the handshake (native clients). `?token=` is also accepted because browser `WebSocket` cannot set headers — avoid query tokens in shared logs when you can. |
| `hints=` / `after=` | Same meaning as `GET /envelopes`. |
| On connect | Server **replays** every row poll would return for those hints since `after` (JSON pages shaped like the REST fetch body: `{ envelopes, next_cursor }`), then **streams** matching new POSTs the same way. |
| Ack | Still REST-only (`POST /envelopes/ack`). WS never deletes rows. |
| Slow clients | Bounded broadcast; lagging or stuck writers are **disconnected**. Reconnect with the last cursor and replay — that is what the cursor is for. |
| Connection caps | Family tokens are semi-public (QR friend cards), so unbounded WS upgrades would let anyone who has seen a card hold arbitrarily many sockets open. Two independent caps apply at upgrade time: `CRUISEMESH_RELAY_WS_PER_TOKEN_MAX_CONNECTIONS` (default 16) per family token and `CRUISEMESH_RELAY_WS_GLOBAL_MAX_CONNECTIONS` (default 256) across all tokens. Either cap being saturated returns `HTTP 429 Too Many Requests` with `{ "error": "...", "code": "ws_connection_cap" }` instead of upgrading. |
| Keepalive | The server pings every open socket roughly every 45 s. A peer that answers neither with a `Pong` nor any other client frame for 2 consecutive intervals is treated as dead and dropped — this is what reclaims the connection-cap slot for a phone that went silent (locked screen, killed app, lost network) without a clean close. |

Caddy already proxies WebSocket upgrades on the compose stack (see `Caddyfile`
comments). No extra port is required.

### 7.1 iOS background relay wakes (APNs)

iOS suspends ordinary WebSockets after the app backgrounds. CruiseMesh closes
that latency gap with an Apple-supported background-notification doorbell:

1. The app registers its APNs device token with `PUT /push/registrations`,
   authenticated by the family **member** token. Deposit credentials are
   rejected. The registration contains only the same rotating, salted
   recipient hints used by relay fetch/WebSocket subscription.
2. When a sealed envelope is stored, relayd matches its opaque hint and sends
   `content-available: 1` through APNs. No sender, chat, preview, or ciphertext
   is sent to Apple.
3. iOS wakes the app, which runs the normal authenticated fetch/decrypt/ack
   pass and holds the background completion callback until that pass finishes.
   WebSocket and periodic polling remain correctness fallbacks.

Apple schedules background notifications at its discretion, so this improves
locked/background latency but is not a permanent-execution entitlement. The
release gate must still exercise a locked physical iPhone against production
APNs; Simulator tests cannot validate provider delivery.

Create one APNs signing key in the Apple Developer portal, then set all four
provider values. For Compose, keep the key outside the checkout and mount it
read-only with a local, uncommitted `docker-compose.override.yml`:

```yaml
services:
  relayd:
    volumes:
      - /etc/cruisemesh/AuthKey_ABC123.p8:/run/secrets/cruisemesh-apns.p8:ro
```

```sh
export CRUISEMESH_APNS_KEY_ID=ABC123
export CRUISEMESH_APNS_TEAM_ID=DEF456
export CRUISEMESH_APNS_BUNDLE_ID=com.cruisemesh.app
export CRUISEMESH_APNS_PRIVATE_KEY_FILE=/run/secrets/cruisemesh-apns.p8
export CRUISEMESH_APNS_ENVIRONMENT=production
../tools/relay_deploy.sh          # §3.1 — supplies the GIT_SHA build arg
docker compose logs relayd | grep apns_wakes
```

If every APNs variable is absent/empty, the worker is disabled and relayd
behaves as before. A partial configuration is a startup error. Registrations
expire from matching after 45 days without a refresh, and APNs-invalid tokens
are removed when Apple reports them.

## 8. Local (non-Docker) run

Useful for development on the same machine as the phones' host:

```sh
# PowerShell
$env:CRUISEMESH_RELAY_TOKENS = "dev-family-token"
$env:CRUISEMESH_RELAY_DB = "C:\path\to\tmp\relayd-live.sqlite"   # absolute!
$env:CRUISEMESH_RELAY_BIND = "0.0.0.0:8080"
cargo run -p cruisemesh-relayd
```

For a quick public URL during bring-up only, a tunnel (localtunnel, cloudflared,
etc.) can still sit in front of that bind address — production should use the
compose + Caddy path above instead.

## 9. Backup

The SQLite file is the entire mailbox state. **relayd runs SQLite in
WAL mode**, so recently-written rows can live in a `cruisemesh-relayd.sqlite-wal`
sidecar file rather than the main file until SQLite checkpoints it back in
— a plain `cp` of only the `.sqlite` file while the process is live can
produce a backup that's missing the most recent writes or is internally
inconsistent. Copy the `-wal` and `-shm` sidecars alongside the main file
(they'll be adjacent in the same `/data/` volume), or stop the container
for the copy, or use SQLite's own [online backup](https://www.sqlite.org/backup.html)
/ the `.backup` CLI command instead of a raw file copy for a live database.

```sh
docker compose exec relayd ls -la /data/
# Copy the volume (all three files if -wal/-shm are present), or:
docker run --rm -v relayd_relay-data:/data -v "${PWD}:/backup" alpine \
  sh -c 'cp /data/cruisemesh-relayd.sqlite* /backup/'
```

Volume name may be prefixed with the compose project name (`relayd_relay-data`
if started from this directory).

### Automated nightly backups (`tools/relay_backup.sh`)

`tools/relay_backup.sh` automates all of the above the WAL-safe way — a
`sqlite3 ".backup"` (SQLite's online backup API) against the live DB in the
data volume, an immediate `PRAGMA integrity_check` + row-count verification
of the copy, gzip, rotation (newest 14 kept), an optional off-box push, and
a disk watchdog. It is driven by the systemd units in `relayd/deploy/`
(nightly at 03:17 UTC, deliberately off relayd's top-of-the-hour maintenance
sweep). The unit files assume the `provision-hetzner.sh` layout
(`/opt/cruisemesh`); adjust `ExecStart=` if your checkout lives elsewhere.

One-time install on the box, as root:

```sh
apt-get install -y sqlite3
cp /opt/cruisemesh/relayd/deploy/cruisemesh-relay-backup.service \
   /opt/cruisemesh/relayd/deploy/cruisemesh-relay-backup.timer \
   /opt/cruisemesh/relayd/deploy/cruisemesh-relay-backup-alert.service \
   /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now cruisemesh-relay-backup.timer
```

**Failure emails** — a failed run (backup error *or* disk past threshold)
fires the `OnFailure=` unit, which posts a plain-text alert through the
Resend API. Give it the key once (same Resend account the purchase Worker
uses; the file, not argv, so it never shows in a process list):

```sh
install -d -m 700 /etc/cruisemesh
(umask 077; printf 'RESEND_API_KEY=%s\n' '<resend key>' > /etc/cruisemesh/ops-alert.env)
```

Without that file the failure still lands in the journal and
`systemctl --failed`, but nobody is emailed. (Box-down outages are covered
separately by the cruisemesh-web Worker's 15-minute `/healthz` cron; this
hook only covers backup/disk failures, which `/healthz` cannot see.)

**Verify the install — including a restore test.** A backup nobody has ever
restored is a hope, not a backup. Right after installing (and again after
any SQLite upgrade):

```sh
# Run one backup by hand and read its journal line:
systemctl start cruisemesh-relay-backup.service
journalctl -u cruisemesh-relay-backup.service -n 5
ls -la /var/backups/cruisemesh-relayd/

# Restore test: unpack the newest snapshot and prove it opens clean with
# real data in it.
latest=$(ls -1t /var/backups/cruisemesh-relayd/cruisemesh-relayd-*.sqlite.gz | head -1)
gunzip -c "$latest" > /tmp/restore-test.sqlite
sqlite3 /tmp/restore-test.sqlite \
  "PRAGMA integrity_check; SELECT COUNT(*) FROM families; SELECT COUNT(*) FROM envelopes;"
rm /tmp/restore-test.sqlite
```

**Where backups land**: `/var/backups/cruisemesh-relayd/` as
`cruisemesh-relayd-<UTC-timestamp>.sqlite.gz`, newest 14 kept
(`CRUISEMESH_BACKUP_KEEP`), mode 0600 under a 0700 directory — these files
contain **full family bearer tokens** and the sealed mailbox, so treat them
like the database itself.

**Off-box copies (recommended once real families are aboard)**: a backup on
the same disk as the database does not survive the disk. Configure an rclone
remote once as root (`rclone config` — credentials stay in root's
`rclone.conf`, never in the unit or the script), point the service at it via
a drop-in, and use a **private** bucket:

```sh
systemctl edit cruisemesh-relay-backup.service
# In the editor, add:
#   [Service]
#   Environment=CRUISEMESH_BACKUP_RCLONE_REMOTE=b2:cruisemesh-backups/relayd
```

**Disk watchdog**: each run ends by checking the filesystems holding the
data volume and the backup directory; past 85 % (`CRUISEMESH_DISK_ALERT_PCT`)
it prints an `ALERT:` line and exits non-zero, which fires the same
`OnFailure=` email. The check runs last on purpose, so a nearly-full disk
still gets that night's backup.

**Restoring onto the live deploy** (the full-loss path):

```sh
cd /opt/cruisemesh/relayd
docker compose stop relayd
gunzip -c /var/backups/cruisemesh-relayd/<snapshot>.sqlite.gz > /tmp/restore.sqlite
# Replace the DB and drop stale WAL sidecars from the old incarnation:
docker run --rm -v relayd_relay-data:/data -v /tmp:/restore alpine \
  sh -c 'rm -f /data/cruisemesh-relayd.sqlite-wal /data/cruisemesh-relayd.sqlite-shm \
         && cp /restore/restore.sqlite /data/cruisemesh-relayd.sqlite'
docker compose start relayd
rm /tmp/restore.sqlite
curl -fsS "https://${RELAY_DOMAIN}/healthz"
```

## 10. Resource limits

The relay is content-agnostic (§6) and never inspects `sealed`, so the only
protection against unbounded SQLite growth on the $4 VPS is server-side
size/quota gating on ingest. Two independent limits bound what a family can
**store** — both applied to every `POST /envelopes` — and a third bounds how
fast anyone holding a family token can **ask** (see "Request and upload rate
limits" below):

### Per-envelope sealed-size cap

Hardcoded at **512 KiB** (`MAX_ENVELOPE_SEALED_BYTES` in `relayd/src/lib.rs`;
not configurable, since it is derived from the client-side attachment
ceiling rather than an operational trade-off). Oversized posts are
rejected with:

```
HTTP 413 Payload Too Large
{ "error": "sealed envelope of ... bytes exceeds the 524288-byte per-envelope cap",
  "code": "envelope_too_large" }
```

Derivation (full detail in the `MAX_ENVELOPE_SEALED_BYTES` doc comment):
the largest inline attachment blob a client will ever produce is 180 KiB
(`core/src/content.rs::ATTACHMENT_MAX_BLOB_BYTES`, the same constant
`AttachmentPayload.MAX_BLOB_BYTES` uses on Android), plus a generous
allowance for attachment-wire and sealing/signing overhead (~182 KiB
realistic ceiling), rounded up ~2x for headroom. This is well under axum's
default 2 MiB request-body limit, so this cap — not axum's — is what
actually fires on an oversized post.

### Per-family storage quota

Default **256 MiB** per family token (sum of `LENGTH(sealed)` across that
family's rows), configurable via `CRUISEMESH_RELAY_FAMILY_QUOTA_BYTES`
(§5). 256 MiB is meant to comfortably cover "a family's whole cruise of
photos": at the 180 KiB attachment ceiling that's ~1,450 max-size
attachments, or many times that for the smaller compressed photos
`MediaCompressor` normally produces — several phones, dozens of
photos/day, a week at sea, plus text/receipt traffic (negligible by
comparison).

**Durability over eviction.** Unlike a cache, this mailbox never silently
deletes unacked mail to make room — that would be data loss for a family
member who hasn't fetched yet. When a new envelope would push a family
over quota:

1. Expired rows for that family are pruned first (reusing the existing
   `prune_expired` used by every fetch) — this alone is often enough,
   since a device that's been offline past its `expiry_ms` was going to
   lose those rows anyway.
2. If the family is *still* over quota after pruning, the post is
   rejected — the unacked backlog is left completely untouched:

   ```
   HTTP 507 Insufficient Storage
   { "error": "family storage quota exceeded: ... bytes used, ... byte quota (expired rows already pruned)",
     "code": "family_quota_exceeded" }
   ```

507 (not 413) is deliberate: it is a distinct status from the size-cap
rejection because the client's remedy is different (wait for the mailbox
to drain / an existing member to ack, vs. shrink this one payload). Both
error bodies also carry a `code` field so a client can branch without
parsing `message` text.

**Re-posting an existing `msg_id` is never quota-checked** — dedupe (§6,
"Dedupe") never rewrites `sealed`, so a retried post (e.g. a receipt
envelope re-uploaded every sync) adds zero bytes and must not start
failing once a family's mailbox is merely full.

**Client-side handling has shipped**: both apps classify these
bodies in the core (`core/src/relay_status.rs`, keyed on the `code` field)
and surface them through the Shore Pass status indicator — 507 as a
persistent "storage full" state, 413 as a persistent "message too large"
state, 429 as a transient self-healing state that also honors
`Retry-After`. The upload loop itself still logs-and-continues per
envelope, so a rejected envelope stays queued locally for a later retry
(harmless for the size cap, which never succeeds on retry without
shrinking the payload; useful for the quota error, which resolves once the
family drains their mailbox).

### Deposit-class shares of that quota

The quota above is one pool per family, and a family's deposit credential
(§1, "Token classes") is stamped onto every friend card it hands out. Charged naively, the
pool a family's own phones depend on can be filled entirely by posts none of
those phones sent — and once it is full, the family's own posts are refused
too, until the deposited rows age out. Two additional ceilings, applied only
to deposit-class posts, take that away:

| Ceiling | Default | Applies to |
| --- | --- | --- |
| Family storage quota | 100% (`CRUISEMESH_RELAY_FAMILY_QUOTA_BYTES`) | every post, member or deposit |
| All deposit-class rows together | 50% of that quota | deposit-class posts only |
| Any one depositor | 25% of that quota | deposit-class posts only |

Both are fixed percentages of whatever quota the family has, so a per-family
`quota_bytes` override scales them with it; there is no separate knob and no
operator step.

**Member-class posts are unchanged** — the family's own devices still see the
whole quota, and are still the only class that can be told the mailbox is
full. The reservation is one-sided: a family whose friends post nothing
notices nothing, and a family whose friends post constantly still has half a
mailbox of its own that no friend card can reach.

Shares deliberately do not shrink as depositors arrive. A share divided by a
live depositor count would let any credential holder shrink everyone else's
allowance by inventing depositors, and would make an honest friend's
admission depend on strangers. Fixed shares oversubscribe instead (four
depositors at a quarter each would sum to the whole quota), which is exactly
what the 50% aggregate ceiling is for: however many friend cards are in
circulation, their shares can never add up to a family locked out of its own
mailbox.

A post over a deposit ceiling is refused with its own `code`, never the
family's:

```
HTTP 507 Insufficient Storage
{ "error": "this deposit credential's share of the family mailbox is full: ...",
  "code": "depositor_share_exceeded" }     # this credential alone is at its share
{ "error": "the deposit-class share of the family mailbox is full: ...",
  "code": "deposit_share_exceeded" }       # deposit-class rows together are at theirs
```

The status stays 507 on purpose — the server understood the request and will
not store the result — and `core/src/relay_status.rs` falls back to the
status when it does not recognize a `code`, so an app predating these codes
reads them exactly as it reads a full mailbox today (persistent storage
condition, envelope stays queued locally for retry). That is the correct
degrade. Reusing `family_quota_exceeded` would not be: it asserts the mailbox
is full, which in these two cases it is not, and it would send a family
looking for a backlog to drain when draining changes nothing.

Accounting is keyed on the *presented credential*. Today a family derives one
deposit token and every friend card carries it, so the relay genuinely cannot
tell one friend from another, and the credential is the finest depositor it
can honestly distinguish — the per-depositor and aggregate ceilings coincide
until friend cards carry per-friend credentials, at which point each gets its
own share with no further change. `POST /family/rotate` retires every
outstanding card, so rows deposited before a rotation keep counting against
the family quota but stop counting against any live depositor's share.

Rows are attributed by a `depositor` column on `envelopes`, added by an
additive startup migration whose constant default makes every row that
predates it read as member class — see §11, "Schema migrations".

### Request and upload rate limits

The two limits above bound how much a family can *store*; neither bounds how
*fast* it can ask. That gap matters because the family bearer token is
semi-public — it rides inside QR friend cards — so anyone who has ever seen a
card can call the API, and every call costs a SQLite round trip on a single
connection on a $4 VPS. Three token buckets close it:

| Bucket | Default | Env var |
|---|---|---|
| Requests, per member token | 600/min | `CRUISEMESH_RELAY_RATE_REQUESTS_PER_MIN` |
| Uploaded `sealed` bytes, per member token | 64 MiB/min | `CRUISEMESH_RELAY_RATE_BYTES_PER_MIN` |
| Requests, per deposit token | 60/min | `CRUISEMESH_RELAY_DEPOSIT_RATE_REQUESTS_PER_MIN` |
| Uploaded `sealed` bytes, per deposit token | 6 MiB/min | `CRUISEMESH_RELAY_DEPOSIT_RATE_BYTES_PER_MIN` |
| Requests, all tokens combined | 6,000/min | `CRUISEMESH_RELAY_RATE_GLOBAL_REQUESTS_PER_MIN` |
| Cross-family presence queries, per deposit token | 4 per 15 min | `CRUISEMESH_RELAY_DEPOSIT_PRESENCE_QUERIES`, `CRUISEMESH_RELAY_DEPOSIT_PRESENCE_WINDOW_SECS` |
| `POST /family/rotate`, per family | 10 per hour | not tunable by env yet |

The last one is shaped the other way round from the rest — a small burst over
a long window rather than a generous per-minute figure — and it is charged to
a dimension of its own. A deposit credential may put a presence query (see
`PRESENCE-01` in `specs/protocol-contract-v1.md`), and however hard it does
so, it spends neither the request nor the byte allowance of the family whose
relay answers. The client asks at most once per contact per fifteen minutes,
so four in a window is a device asking on schedule with three spare.

The rotation bucket is shaped that way too, and for a sharper reason: a
rotation is the *remedy* for a family whose member token is in hostile hands,
and the device holding that token can burn the family's shared request
allowance at will. Charging the remedy to the bucket the attacker controls
would let the attacker hold the family's own eviction call at 429
indefinitely. Ten per hour covers a real ceremony plus retries plus a client
fumbling the request shape a few times, and covers nothing else; the bucket is
charged per attempt, before the request is even validated, so garbage is not
free.

Buckets are keyed by the family's **stable id** (§12) namespaced by the
presented credential's class, so a family's deposit traffic (friend cards,
i.e. what strangers can hold) exhausts its own tighter allowance and never
spends the family's member-class budget — and vice versa. Keying on the id
rather than the token string is what stops `POST /family/rotate` from silently
handing a family a fresh full allowance (and orphaning its WebSocket
connection-cap entry) every time it re-keys. Static `CRUISEMESH_RELAY_TOKENS`
families have no table row and so no id; they key on the token string, which
is correct for them — an operator-configured token only changes when you edit
the config and restart. The deposit defaults are a tenth of the member ones: one post a
second sustained (~34 max-size attachments a minute) is generous for a real
contact and useless for a card-scraping flood, which now caps out at noise
instead of the family's full 64 MiB/min.

Each bucket's capacity **is** its per-minute allowance, so a family that has
been quiet can spend a whole minute's worth in one burst (phones back on
Wi-Fi in port dump their queue all at once) and then refills steadily at
`allowance / 60` per second. Refill is lazy and monotonic (`Instant`, never
the wall clock — an NTP step must not hand out free allowance or freeze a
family out), so an idle family costs nothing and there is no background
timer.

**These are floodgates, not plan tiers**, and the defaults are set so a real
family never meets them:

- **600 requests/min** is 10 requests/second *for the whole family,
  sustained*. A six-phone fleet polling `GET /envelopes` and `POST /presence`
  every 15 s is around 50 requests/min; posting a backlog costs one request
  per envelope, and 600 envelopes in a minute is far past anything a family
  produces (for media the byte bucket binds first anyway).
- **64 MiB/min** lets a family upload their *entire* 256 MiB storage quota in
  about four minutes, so the limiter can never be what keeps a real cruise
  backlog from landing — it only flattens the peak. That is roughly 360
  max-size (180 KiB) attachments or ~2,000 typical compressed photos per
  minute across all of a family's phones. Accounting is on **decoded**
  `sealed` bytes; the base64 JSON on the wire is about 1.33x that.
- **6,000 requests/min** globally is ten families simultaneously at their
  full per-family allowance — an order of magnitude above the real
  single-digit-family load — so it only fires when many tokens misbehave at
  once, or when the hosted service has outgrown this box.

Rejections are:

```
HTTP 429 Too Many Requests
Retry-After: <seconds>
{ "error": "family upload byte rate limit exceeded: retry after 3s",
  "code": "rate_limited" }
```

`Retry-After` is integer delta-seconds until the bucket holds enough tokens
again (at least 1, never more than 60 — a full window always refills a
bucket completely). The single `rate_limited` code covers all three buckets
because the client's remedy is identical (wait, then retry); the *message*
names which limit tripped — `family request rate limit`, `family upload byte
rate limit`, or `server-wide request rate limit` — which is what you want in
a support report. Server-side, every rejection logs at WARN with the family
token *prefix* (never the token itself), the scope, and the advertised wait.

Notes an operator will care about:

- **Enforced only after the bearer token authorizes.** The buckets are keyed
  by family token, so checking before authentication would let an
  unauthenticated caller grow the bucket map one entry per invented token —
  the very memory-exhaustion vector the limiter exists to prevent. Unknown
  tokens are rejected by auth (401) and never allocate a bucket. Idle
  buckets are evicted lazily once the map grows past a threshold; there is
  no background sweeper.
- **`/healthz` and `/admin/*` are exempt.** Uptime monitors poll healthz
  constantly and a 429 there would read as an outage; admin is a trusted
  operator path already guarded by its own token (§12), and the purchase
  flow provisioning a pass must never be throttled behind a family's
  traffic.
- **Charged routes** are `POST /envelopes` (1 request + its sealed bytes),
  `GET /envelopes`, `POST /envelopes/ack`, `POST /presence` (1 request each),
  `PUT /push/registrations`, and the `GET /ws` **upgrade** (1 request each).
  Frames on an established socket are not charged — the connection caps in
  §7 bound those instead.
- **Unlike the storage quota, a dedupe re-post *is* charged.** Re-uploading
  an existing `msg_id` adds zero stored bytes, but those bytes still crossed
  the wire and were decoded, which is exactly what this limit is protecting.
- **A rejected request is never partially charged.** Both dimensions are
  checked before either is debited, so a post that trips the byte allowance
  does not also burn a request token; and a family that is over its own
  limit never eats into the global backstop.

## 11. Background maintenance

`GET /envelopes` only ever `SELECT`s now — physical row deletion and disk
reclamation happen in a detached background task, started once at process
startup and running for the process's lifetime (default cadence: hourly,
`DEFAULT_PRUNE_INTERVAL` in `relayd/src/lib.rs`):

1. `prune_expired(now)` — deletes envelope rows past `expiry_ms` or the
   30-day retention ceiling, and presence rows past their own retention
   window. Only logs when it actually deletes something (same convention
   as the other periodic log lines — a zero-count line every hour would
   just be noise).
2. `PRAGMA incremental_vacuum` — reclaims the pages that delete just freed,
   shrinking the file on disk. Only effective once the database is in
   `auto_vacuum = INCREMENTAL` mode (next section); a harmless no-op
   otherwise.

Between sweeps, an expired-but-not-yet-deleted row is still excluded from
`GET /envelopes` responses (an `expiry_ms > now` predicate on the fetch
query does that filtering), so no client ever sees stale mail — only the
physical delete + disk reclaim is deferred to the hourly sweep instead of
running inline on every poll.

### `auto_vacuum` migration note

SQLite's `auto_vacuum` mode can only be set on a database that has no
tables yet — running `PRAGMA auto_vacuum = INCREMENTAL` on a database that
already has tables (i.e. every relayd database that existed before this
change; the previous default was `NONE`) is a **silent no-op**. Converting
an existing database requires that pragma immediately followed by a full
`VACUUM`, which is SQLite's documented way to toggle auto-vacuum on an
existing database.

relayd handles this automatically and transparently: `RelayStore::open`
checks the database's *current* `auto_vacuum` mode on every start, and if
it isn't already `INCREMENTAL`, runs the pragma + `VACUUM` once. On every
later start (including every restart after the first one post-upgrade)
the check is a single cheap read and nothing else happens.

**Practical effect for an operator upgrading an existing deployment**: the
*first* start of relayd after this change holds an exclusive lock and
rewrites the entire SQLite file (a full `VACUUM`) before the process
starts accepting connections. For the family-scale deployment this targets
(single-digit families, a few hundred MiB ceiling each) that is expected
to take at most a few seconds; there is no separate migration step to run
by hand. If you operate an unusually large relayd database, expect a
one-time longer startup on the first upgrade.

### Schema migrations

Column additions are applied by `RelayStore::open` on start, idempotently,
with no operator step and no downtime beyond the restart.

On `families`: `deposit_token` (backfilled by derivation from each existing
member token), then `family_id` (backfilled with a freshly minted `cmfid1-…`
per row) and `rotation_pk`. `rotation_pk` is deliberately **not** backfilled —
NULL is its meaningful value and means "no rotation authority registered
yet", which is the correct starting state for every family that predates
§6.1.

On `envelopes`: `depositor TEXT NOT NULL DEFAULT ''`, which carries the
deposit-share accounting in §10. The constant `DEFAULT` is what makes a
`NOT NULL` `ADD COLUMN` legal at all, and it means SQLite records the column
in the schema and synthesizes the default on read: **no existing row is
rewritten, moved, or deleted**, there is nothing to backfill, and no envelope
is at risk. Every row that predates the column therefore reads as member
class, the only safe reading of rows posted before the relay recorded who
deposited them — guessing "deposit" would charge a family's existing friend
mail against a share that did not exist when it was posted, and could refuse
that family's friends on restart for history rather than behavior.

Re-running any of them is a no-op, so a rollback and re-upgrade is safe; a
downgrade to a build that predates these columns also works, since it simply
ignores them — an older binary keeps writing envelopes without a `depositor`,
SQLite supplies `''`, and a later re-upgrade reads those rows as member class
exactly as it does the rest.

**Check after deploying**: `PRAGMA table_info(envelopes)` on the live
database should list `depositor`, and the startup log carries
`migration: envelopes.depositor added` once (and only once).

## 12. Hosted-family admin API (`/admin/families`)

Off by default. Setting `CRUISEMESH_RELAY_ADMIN_TOKEN` (generate like a family
token: `openssl rand -hex 32`) enables a small provisioning API used by the
cruisemesh.app purchase flow ("Shore Pass"); self-hosted deploys can ignore
this section entirely — with the variable unset the routes answer 404 and
behavior is identical to before.

Provisioned families live in a `families` table next to the mailbox and are
checked on every request alongside the static env allowlist (env tokens are
implicit always-active families). Semantics:

- **Expiry**: past `expires_ms` a family gets a 7-day read-only grace window
  (fetch/ack/WS still work so queued messages aren't stranded; `POST
  /envelopes` returns 403 `{code:"family_expired"}`). After the grace window
  every request is 403 `family_expired`. `expires_ms` absent = never expires.
- **Suspension**: `status:"suspended"` rejects everything with 403
  `{code:"family_suspended"}`; PATCH back to `active` restores service.
- **Per-family quota**: `quota_bytes` overrides
  `CRUISEMESH_RELAY_FAMILY_QUOTA_BYTES` for that family only.
- **Revocation** (`DELETE`) removes the family **and purges its stored
  envelopes, presence rows, and APNs registrations** — both credentials die
  together, since the deposit token is resolved through the same row.
- **Two-token response**: provisioning mints both credentials — you
  supply the member `token`, relayd derives and stores its post-only
  `deposit_token` — and every family object in every response carries both
  fields. The purchase flow keeps putting `token` on the setup card; nothing
  needs to deliver `deposit_token` anywhere (phones derive it themselves),
  it is returned for operator visibility and support. Member tokens starting
  with the reserved `cmdep1-` deposit prefix are rejected with a 400. A deposit
  token in the path is a 404.
- **Stable `family_id`**: every family object also carries an opaque
  `cmfid1-…` id, minted at provisioning and **never changed** — including by
  `POST /family/rotate`, which replaces both tokens. Existing families were
  backfilled with one on the first restart after this shipped. Record this,
  not the token, if you need to find a customer's family again later.
  `GET`/`PATCH`/`DELETE /admin/families/{id}` resolve the path segment as
  **family id first, then current member token**, so the pass-issuing flow
  that stored a token keeps working while that token is current, and an
  operator can still reach a family that has rotated. The two namespaces
  cannot collide — ids carry the `cmfid1-` prefix and provisioning and
  rotation both reject tokens wearing it — so the resolution order is a
  tie-break that never fires in practice. A token that has been rotated away
  resolves to nothing (404), which is the point of rotating it.
- All operations are idempotent — the billing webhook retries on failure,
  and re-provisioning the same member token derives the same deposit token.

All routes require `Authorization: Bearer $CRUISEMESH_RELAY_ADMIN_TOKEN`:

```sh
# Provision (or re-provision / renew — reactivates a suspended family)
curl -s -X POST https://relay.example.com/admin/families \
  -H "authorization: Bearer $ADMIN" -H "content-type: application/json" \
  -d '{"token":"<family-token>","plan":"cruise-pass-30d","expires_ms":1790000000000}'

# List (paged; the only way to find a family whose token you've lost)
curl -s "https://relay.example.com/admin/families?status=active&limit=50&offset=0" \
  -H "authorization: Bearer $ADMIN"

# Inspect (includes usage_bytes / envelope_count for support).
# The path segment may be the current member token OR the stable family id —
# after a rotation only the id still works.
curl -s https://relay.example.com/admin/families/<family-token> -H "authorization: Bearer $ADMIN"
curl -s https://relay.example.com/admin/families/cmfid1-<id> -H "authorization: Bearer $ADMIN"

# Suspend / extend
curl -s -X PATCH https://relay.example.com/admin/families/<family-token> \
  -H "authorization: Bearer $ADMIN" -H "content-type: application/json" \
  -d '{"status":"suspended"}'

# Revoke + purge
curl -s -X DELETE https://relay.example.com/admin/families/<family-token> -H "authorization: Bearer $ADMIN"
```

PATCH is merge-only (`null`/omitted fields keep their stored value); to clear
a field, re-provision via POST.

`GET /admin/families` returns
`{"families":[...],"total":N,"limit":L,"offset":O}`, each entry shaped exactly
like the single-family GET. `total` counts every family matching `status`
(not just the returned page), so compare it against `offset + families.length`
to know whether to page again. `limit` defaults to 100 and is **clamped** to
500 rather than rejected. `status` accepts only `active` or `suspended`; a
typo is a 400, never a silently empty list. Static `CRUISEMESH_RELAY_TOKENS`
entries are implicit families with no table row and do not appear.

Responses carry **full family tokens** — they are the credential. Prefer
`tools/relay_admin.sh list`, which masks them for display; pipe raw curl
output somewhere you would put a password.

## 13. Not in this deploy yet

- Multi-region / federation — single VPS is the intended family-scale deploy.
- Physical-device validation of production APNs scheduling remains a release
  step; the provider/client implementation and deterministic server tests are
  in-tree, but Apple delivery cannot be proven by a Simulator.
- ~~Client-side handling of the 413/507 error bodies (see §10)~~ —
  shipped: both apps now surface distinct storage-full /
  too-large / rate-limited states through the Shore Pass indicator.
