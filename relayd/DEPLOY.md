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

Each family gets one long random bearer token. Phones send it as
`Authorization: Bearer <token>` on every request; it is also what you bake
into friend cards as `relay_token`.

```sh
# One token per family (store these somewhere safe; rotating is a re-QR).
openssl rand -hex 32
```

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

## 3. Start

```sh
# Optional but recommended: bakes the exact commit into the image so
# /healthz reports what's actually running (FR4) instead of "unknown".
export GIT_SHA=$(git rev-parse --short HEAD)

docker compose up -d --build
docker compose ps
curl -fsS "https://${RELAY_DOMAIN}/healthz"
# → {"status":"ok","version":"0.1.0","commit":"abc1234"}
```

Caddy obtains a Let's Encrypt cert for `RELAY_DOMAIN` on first start. If
`/healthz` fails, check `docker compose logs caddy` (DNS not pointed yet is
the usual cause).

## 4. Point phones at the relay

On each phone, the friend card / contact fields should be:

| Field | Value |
|---|---|
| relay URL | `https://relay.example.com` (no trailing slash) |
| relay token | the same family token from step 1 |

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
| `CRUISEMESH_RELAY_RATE_REQUESTS_PER_MIN` | `600` | Requests per minute allowed for a single family token (also the burst size). See §10. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_RELAY_RATE_BYTES_PER_MIN` | `67108864` (64 MiB) | Uploaded `sealed` bytes per minute allowed for a single family token (also the burst size). See §10. Must be a positive integer; unset uses the default. |
| `CRUISEMESH_RELAY_RATE_GLOBAL_REQUESTS_PER_MIN` | `6000` | Requests per minute across all family tokens combined — the coarse backstop. See §10. Must be a positive integer; unset uses the default. |
| `RELAY_DOMAIN` | *(compose required)* | Hostname in the Caddyfile for TLS. |

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

The SQLite file is the entire mailbox state. **FR8: relayd runs SQLite in
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

## 10. Resource limits (DTN_TODOS.md D7)

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

**Client-side handling of these two new error shapes is a follow-up, not
yet implemented** — today's upload loop already logs-and-continues per
envelope on any non-2xx response, so a rejected envelope is simply left
queued locally for a later retry (harmless for the size cap, which never
succeeds on retry without shrinking the payload; more useful for the quota
error, which can resolve once the family drains their mailbox).

### Request and upload rate limits

The two limits above bound how much a family can *store*; neither bounds how
*fast* it can ask. That gap matters because the family bearer token is
semi-public — it rides inside QR friend cards — so anyone who has ever seen a
card can call the API, and every call costs a SQLite round trip on a single
connection on a $4 VPS. Three token buckets close it:

| Bucket | Default | Env var |
|---|---|---|
| Requests, per family token | 600/min | `CRUISEMESH_RELAY_RATE_REQUESTS_PER_MIN` |
| Uploaded `sealed` bytes, per family token | 64 MiB/min | `CRUISEMESH_RELAY_RATE_BYTES_PER_MIN` |
| Requests, all tokens combined | 6,000/min | `CRUISEMESH_RELAY_RATE_GLOBAL_REQUESTS_PER_MIN` |

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
  and the `GET /ws` **upgrade** (1 request). Frames on an established socket
  are not charged — the connection caps in §7 bound those instead.
- **Unlike the storage quota, a dedupe re-post *is* charged.** Re-uploading
  an existing `msg_id` adds zero stored bytes, but those bytes still crossed
  the wire and were decoded, which is exactly what this limit is protecting.
- **A rejected request is never partially charged.** Both dimensions are
  checked before either is debited, so a post that trips the byte allowance
  does not also burn a request token; and a family that is over its own
  limit never eats into the global backstop.

## 11. Background maintenance (FR7)

`GET /envelopes` only ever `SELECT`s now — physical row deletion and disk
reclamation happen in a detached background task, started once at process
startup and running for the process's lifetime (default cadence: hourly,
`DEFAULT_PRUNE_INTERVAL` in `relayd/src/lib.rs`):

1. `prune_expired(now)` — deletes envelope rows past `expiry_ms` or the
   30-day retention ceiling, and presence rows past their own retention
   window. Only logs when it actually deletes something (same convention
   as every other FR2-era log line — a zero-count line every hour would
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

## 12. Hosted-family admin API (`/admin/families`)

Off by default. Setting `CRUISEMESH_RELAY_ADMIN_TOKEN` (generate like a family
token: `openssl rand -hex 32`) enables a small provisioning API used by the
cruisemesh.app purchase flow ("Cruise Pass"); self-hosted deploys can ignore
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
  envelopes and presence rows**.
- All operations are idempotent — the billing webhook retries on failure.

All routes require `Authorization: Bearer $CRUISEMESH_RELAY_ADMIN_TOKEN`:

```sh
# Provision (or re-provision / renew — reactivates a suspended family)
curl -s -X POST https://relay.example.com/admin/families \
  -H "authorization: Bearer $ADMIN" -H "content-type: application/json" \
  -d '{"token":"<family-token>","plan":"cruise-pass-30d","expires_ms":1790000000000}'

# List (paged; the only way to find a family whose token you've lost)
curl -s "https://relay.example.com/admin/families?status=active&limit=50&offset=0" \
  -H "authorization: Bearer $ADMIN"

# Inspect (includes usage_bytes / envelope_count for support)
curl -s https://relay.example.com/admin/families/<family-token> -H "authorization: Bearer $ADMIN"

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
- Android/iOS clients still primarily poll today; wiring the phone apps to
  `GET /ws` is a client change, not a server gap.
- Client-side handling of the D7 413/507 error bodies (see §10) — surfacing
  a distinct "mailbox full" state to the user, rather than the current
  generic log-and-retry.
