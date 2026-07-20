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
| `CRUISEMESH_RELAY_TOKENS` | *(required)* | Comma-separated allowlist. Empty → process refuses to start. |
| `CRUISEMESH_RELAY_DB` | `cruisemesh-relayd.sqlite` | **Use an absolute path.** Relative paths resolve against the process CWD, which is easy to get wrong under systemd/Docker/IDE launchers. |
| `CRUISEMESH_RELAY_BIND` | `0.0.0.0:8080` | Inside Docker keep `0.0.0.0:8080`; Caddy is the public listener. |
| `CRUISEMESH_RELAY_FAMILY_QUOTA_BYTES` | `268435456` (256 MiB) | Per-family-token storage quota. See §11. Must be a positive integer; unset uses the default. |
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
  with `expiry_ms <= now` are pruned on fetch.
- **30-day server ceiling**: insert clamps `expiry_ms` to
  `created_at_ms + 30 days`; prune also drops rows whose `created_at_ms` is
  older than 30 days (belt-and-suspenders for any pre-clamp data).
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

The SQLite file is the entire mailbox state:

```sh
docker compose exec relayd ls -la /data/
# Copy the volume, or:
docker run --rm -v relayd_relay-data:/data -v "${PWD}:/backup" alpine \
  cp /data/cruisemesh-relayd.sqlite /backup/relayd-backup.sqlite
```

Volume name may be prefixed with the compose project name (`relayd_relay-data`
if started from this directory).

## 10. Resource limits (DTN_TODOS.md D7)

The relay is content-agnostic (§6) and never inspects `sealed`, so the only
protection against unbounded SQLite growth on the $4 VPS is server-side
size/quota gating on ingest. Two independent limits apply to every
`POST /envelopes`:

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

## 11. Not in this deploy yet

- Multi-region / federation — single VPS is the intended family-scale deploy.
- Android/iOS clients still primarily poll today; wiring the phone apps to
  `GET /ws` is a client change, not a server gap.
- Client-side handling of the D7 413/507 error bodies (see §10) — surfacing
  a distinct "mailbox full" state to the user, rather than the current
  generic log-and-retry.
