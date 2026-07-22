#!/usr/bin/env bash
# Provision a fresh Hetzner CAX11/CX22 box (Ubuntu 24.04) as the CruiseMesh
# hosted relay: firewall, Docker, source checkout, admin token, and the
# relayd + Caddy stack behind automatic Let's Encrypt TLS.
#
# Idempotent: safe to re-run. It preserves an already-generated admin token
# and just rebuilds/redeploys.
#
# PREREQUISITES (do these first — see the header of DEPLOY.md too):
#   1. DNS: in Cloudflare add an A record  relay -> <this box's IPv4>, set to
#      "DNS only" (grey cloud). Caddy terminates TLS itself; Cloudflare's proxy
#      would break the ACME certificate challenge. Add an AAAA (DNS only) too
#      if the box has IPv6.
#   2. The BRANCH below (default: master) must be pushed to the REPO_URL so
#      this box can clone it.
#
# Usage (as root on the fresh box):
#   bash provision-hetzner.sh
set -euo pipefail

### ---- Settings (override via env before running if needed) ----
RELAY_DOMAIN="${RELAY_DOMAIN:-relay.cruisemesh.app}"
REPO_URL="${REPO_URL:-https://github.com/davidmjacobson/cruisemesh.git}"
BRANCH="${BRANCH:-master}"
APP_DIR="${APP_DIR:-/opt/cruisemesh}"
### -------------------------------------------------------------

if [ "$(id -u)" -ne 0 ]; then
  echo "Run as root (e.g. 'sudo -i' first)." >&2
  exit 1
fi

echo "==> [1/6] Base packages + firewall"
export DEBIAN_FRONTEND=noninteractive
apt-get update -y
apt-get install -y git curl ufw ca-certificates openssl unattended-upgrades
ufw allow OpenSSH >/dev/null 2>&1 || ufw allow 22/tcp
ufw allow 80/tcp
ufw allow 443/tcp
ufw --force enable

echo "==> [2/6] Docker Engine + compose plugin"
if ! command -v docker >/dev/null 2>&1; then
  # Official Docker install script (handles ARM64 and x86 automatically).
  curl -fsSL https://get.docker.com | sh
fi
systemctl enable --now docker

echo "==> [3/6] Fetch source (branch: $BRANCH)"
if [ -d "$APP_DIR/.git" ]; then
  git -C "$APP_DIR" fetch origin "$BRANCH"
  git -C "$APP_DIR" checkout "$BRANCH"
  git -C "$APP_DIR" pull --ff-only origin "$BRANCH"
else
  git clone --branch "$BRANCH" "$REPO_URL" "$APP_DIR"
fi
cd "$APP_DIR/relayd"

echo "==> [4/6] Environment + admin token"
ENV_FILE="$APP_DIR/relayd/.env"
ADMIN_TOKEN=""
if [ -f "$ENV_FILE" ]; then
  # Reuse the token from a previous run (rotating it would lock out the
  # Cloudflare Worker until you update the secret there too).
  ADMIN_TOKEN="$(grep -E '^CRUISEMESH_RELAY_ADMIN_TOKEN=' "$ENV_FILE" | cut -d= -f2- || true)"
fi
if [ -z "$ADMIN_TOKEN" ]; then
  ADMIN_TOKEN="$(openssl rand -hex 32)"
fi
# CRUISEMESH_RELAY_TOKENS is intentionally empty: this is a hosted deploy where
# every family is provisioned through the admin API, not the static allowlist.
cat > "$ENV_FILE" <<EOF
RELAY_DOMAIN=$RELAY_DOMAIN
CRUISEMESH_RELAY_TOKENS=
CRUISEMESH_RELAY_ADMIN_TOKEN=$ADMIN_TOKEN
GIT_SHA=$(git rev-parse --short HEAD)
EOF
chmod 600 "$ENV_FILE"

echo "==> [5/6] Build & start (relayd + Caddy)"
docker compose up -d --build

echo "==> [6/6] Waiting for https://$RELAY_DOMAIN/healthz (TLS issuance can take ~30s)"
ok=""
for _ in $(seq 1 40); do
  if curl -fsS "https://$RELAY_DOMAIN/healthz" >/dev/null 2>&1; then ok=1; break; fi
  sleep 3
done

echo
if [ -n "$ok" ]; then
  echo "Relay is up:"
  curl -fsS "https://$RELAY_DOMAIN/healthz"; echo
else
  echo "Not reachable over HTTPS yet. Most likely DNS for $RELAY_DOMAIN isn't"
  echo "pointing here (grey-cloud it in Cloudflare), or the cert is still issuing."
  echo "Watch it with:  docker compose -f $APP_DIR/relayd/docker-compose.yml logs -f caddy"
fi

cat <<EOF

======================================================================
ADMIN TOKEN — store this as the RELAY_ADMIN_TOKEN secret in Cloudflare:

    $ADMIN_TOKEN

On your dev machine, in the CruiseMesh-web repo:
    npx wrangler secret put RELAY_ADMIN_TOKEN
and paste the value above.

Optional admin-API smoke test from this box:
    T=$ADMIN_TOKEN
    curl -s -X POST https://$RELAY_DOMAIN/admin/families \\
      -H "authorization: Bearer \$T" -H "content-type: application/json" \\
      -d '{"token":"smoke-test-0001","plan":"smoke"}'
    curl -s -X DELETE https://$RELAY_DOMAIN/admin/families/smoke-test-0001 \\
      -H "authorization: Bearer \$T"
======================================================================
EOF
