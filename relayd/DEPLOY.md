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
docker compose up -d --build
docker compose ps
curl -fsS "https://${RELAY_DOMAIN}/healthz"
# → {"status":"ok"}
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

## 10. Not in this deploy yet

- Multi-region / federation — single VPS is the intended family-scale deploy.
- Android/iOS clients still primarily poll today; wiring the phone apps to
  `GET /ws` is a client change, not a server gap.
