#!/usr/bin/env bash
# relay_admin.sh -- operator CLI for a hosted CruiseMesh relay ("Shore Pass").
#
# Everything here goes through the admin HTTP API documented in
# relayd/DEPLOY.md §12. Nothing touches SQLite: an earlier version of this
# script did, because relayd had no list endpoint, and the result was a tool
# that only worked while sitting on the same box as the database. If you find
# yourself reaching for `docker run ... sqlite3` again, add the endpoint
# instead.
#
# Run `relay_admin.sh --help` for usage -- it lives in usage() below rather
# than in this comment block, because the documented way to run this script is
# to pipe it into a remote shell (`ssh box 'bash -s' < relay_admin.sh`), where
# $0 is "bash" and the script cannot read its own source to print a header.
#
# Needs: bash, curl (7.76+, for --fail-with-body), python3.

set -euo pipefail

ENV_FILE=${RELAYD_ENV_FILE:-/opt/cruisemesh/relayd/.env}
SITE_ORIGIN=${CRUISEMESH_SITE_ORIGIN:-https://cruisemesh.app}
# Mirrors relayd's FAMILY_EXPIRY_GRACE_MS. Display only -- the server is the
# authority; if you change it there, change it here.
GRACE_MS=$((7 * 24 * 60 * 60 * 1000))

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

die() { echo "error: $*" >&2; exit 1; }

# Python helpers live in files rather than `python3 -c '...'`: several of them
# need single quotes, and escaping those through a single-quoted bash argument
# is how this script grows bugs nobody can read.
write_helpers() {
  cat > "$WORK/setup_link.py" <<'PY'
# argv: relay_url family_token site_origin
# base64url, no padding -- must byte-match relaySetupToken() in
# cruisemesh-web/src/relay.js or the phones reject the card.
import base64, json, sys

relay_url, token, site = sys.argv[1:4]
payload = json.dumps({"v": 1, "relay_url": relay_url, "relay_token": token},
                     separators=(",", ":")).encode()
card = "CMRELAY1:" + base64.urlsafe_b64encode(payload).decode().rstrip("=")
print(f"{site}/r#{card}")
PY

  cat > "$WORK/build_provision.py" <<'PY'
# argv: token plan note expires_ms quota_bytes  (empty string = omit)
import json, sys

token, plan, note, expires, quota = sys.argv[1:6]
body = {"token": token, "plan": plan, "note": note}
if expires:
    body["expires_ms"] = int(expires)
if quota:
    body["quota_bytes"] = int(quota)
print(json.dumps(body))
PY

  cat > "$WORK/reprovision_without_expiry.py" <<'PY'
# stdin: a family JSON. Re-provision body that keeps everything but the
# expiry -- PATCH is merge-only and cannot clear a field.
import json, sys

f = json.load(sys.stdin)
keep = ("token", "plan", "quota_bytes", "note")
print(json.dumps({k: f[k] for k in keep if f[k] is not None}))
PY

  cat > "$WORK/match_prefix.py" <<'PY'
# argv: prefix ; stdin: a list response. Prints the full TOKEN of every family
# whose token *or* stable family id starts with the prefix -- both of the
# columns `list` prints are meant to be copied straight back into another
# command, and both resolve to the one credential the caller needs.
import json, sys

prefix = sys.argv[1]
for family in json.load(sys.stdin)["families"]:
    # A relay predating the stable id omits the field entirely; "" never
    # matches a non-empty prefix, so the token match still stands alone.
    family_id = family.get("family_id") or ""
    if family["token"].startswith(prefix) or family_id.startswith(prefix):
        print(family["token"])
PY

  cat > "$WORK/family_field.py" <<'PY'
# argv: field ; stdin: a family JSON. Prints that field, or nothing at all if
# the server did not return it -- callers treat empty as "this relay is older
# than the field" and fall back, rather than dying on a KeyError.
import json, sys

value = json.load(sys.stdin).get(sys.argv[1])
if value is not None:
    print(value)
PY

  cat > "$WORK/render_list.py" <<'PY'
# argv: reveal("0"|"1") grace_ms ; stdin: a list response.
import json, sys, time

reveal = sys.argv[1] == "1"
grace_ms = int(sys.argv[2])
page = json.load(sys.stdin)
now = time.time() * 1000

# A family id is "cmfid1-" plus twelve base64url characters. Fixed-width so
# the column stays aligned, and printed in full rather than truncated: unlike
# the token it is not a credential, it is the handle every follow-up command
# and every support note wants, so it has to be copy-pasteable as printed.
FAMILY_ID_WIDTH = 19


def human(size):
    for unit in ("B", "KiB", "MiB", "GiB"):
        if size < 1024 or unit == "GiB":
            return f"{size:.0f}B" if unit == "B" else f"{size:.1f}{unit}"
        size /= 1024


def state(family):
    # Mirrors relayd's authorize_family(): suspension beats expiry, and an
    # expired family stays readable through the grace window.
    if family["status"] != "active":
        return family["status"]
    expires = family["expires_ms"]
    if expires is None:
        return "active"
    if now > expires + grace_ms:
        return "expired"
    if now > expires:
        return "grace"
    return "active"


def day(ms):
    return "never" if ms is None else time.strftime("%Y-%m-%d", time.gmtime(ms / 1000))


rows = page["families"]
if not rows:
    print("no families provisioned")
    raise SystemExit(0)

width = 66 if reveal else 15
header = "token".ljust(width)
print(f"{header} {'family_id':<{FAMILY_ID_WIDTH}} {'state':<9} {'plan':<16} "
      f"{'expires':<11} {'used':>9} {'msgs':>6}  note")
for family in rows:
    token = family["token"] if reveal else family["token"][:12] + "…"
    # "-" rather than a blank keeps every row the same number of
    # whitespace-separated fields, so `awk '{print $2}'` stays honest against a
    # relay too old to send the id.
    family_id = family.get("family_id") or "-"
    plan = (family["plan"] or "-")[:16]
    note = (family["note"] or "")[:40]
    print(f"{token:<{width}} {family_id:<{FAMILY_ID_WIDTH}} {state(family):<9} {plan:<16} "
          f"{day(family['expires_ms']):<11} "
          f"{human(family['usage_bytes']):>9} {family['envelope_count']:>6}  {note}")

shown = page["offset"] + len(rows)
if shown < page["total"]:
    print(f"\n{shown} of {page['total']} -- next page: --offset {shown}")
else:
    print(f"\n{page['total']} total")
if not reveal:
    print("tokens truncated; the visible part is a valid prefix for "
          "show/link/extend/suspend/purge (--reveal prints the credential)")
if any(family.get("family_id") for family in rows):
    print("family_id is accepted by those same commands and, unlike the token, "
          "survives a rotation -- record it, not the token")
else:
    print("this relay is older than the stable family id, so that column is "
          "empty; redeploy it to get ids")
PY

  cat > "$WORK/pretty.py" <<'PY'
import json, sys

print(json.dumps(json.load(sys.stdin), indent=2))
PY

  cat > "$WORK/mint_token.py" <<'PY'
# 32 cryptographically random bytes as hex -- the same shape
# generateFamilyToken() mints in the purchase Worker, so nothing downstream
# can tell a hand-provisioned family from a bought one.
import secrets

print(secrets.token_hex(32))
PY
}

env_value() {
  [ -r "$ENV_FILE" ] || return 1
  local line
  line=$(grep -a "^$1=" "$ENV_FILE" | head -1) || return 1
  printf '%s' "${line#*=}" | tr -d '"\r'
}

resolve_config() {
  if [ -n "${CRUISEMESH_RELAY_ADMIN_TOKEN:-}" ]; then
    ADMIN_TOKEN=$CRUISEMESH_RELAY_ADMIN_TOKEN
    API_BASE=${RELAY_ADMIN_ORIGIN:-https://relay.cruisemesh.app}
  elif [ -r "$ENV_FILE" ]; then
    ADMIN_TOKEN=$(env_value CRUISEMESH_RELAY_ADMIN_TOKEN || true)
    # Even on the box, go in through Caddy: compose does not publish relayd's
    # 8080 on the host (only Caddy is on the compose network with it), so
    # 127.0.0.1:8080 just fails to connect. Kept as the fallback for a deploy
    # that does publish the port.
    local api_domain
    api_domain=$(env_value RELAY_DOMAIN || true)
    if [ -n "${RELAY_ADMIN_ORIGIN:-}" ]; then
      API_BASE=$RELAY_ADMIN_ORIGIN
    elif [ -n "$api_domain" ]; then
      API_BASE="https://$api_domain"
    else
      API_BASE=http://127.0.0.1:8080
    fi
  else
    die "no admin token: set CRUISEMESH_RELAY_ADMIN_TOKEN, or run this on the relay box"
  fi
  [ -n "${ADMIN_TOKEN:-}" ] ||
    die "CRUISEMESH_RELAY_ADMIN_TOKEN is empty -- the admin API is off and every route 404s"

  if [ -n "${RELAY_PUBLIC_URL:-}" ]; then
    PUBLIC_URL=$RELAY_PUBLIC_URL
  else
    local domain
    domain=$(env_value RELAY_DOMAIN || true)
    if [ -n "$domain" ]; then
      PUBLIC_URL="https://$domain"
    elif [ "${API_BASE#https://}" != "$API_BASE" ]; then
      PUBLIC_URL=$API_BASE
    else
      # Only setup links need this; every other command works without it.
      PUBLIC_URL=""
    fi
  fi
}

# The bearer never appears in argv (any local process can read /proc/*/cmdline)
# -- curl picks it up from a config file instead.
api() {
  local method=$1 path=$2 body=${3-}
  {
    printf 'url = "%s%s"\n' "$API_BASE" "$path"
    printf 'request = "%s"\n' "$method"
    printf 'header = "authorization: Bearer %s"\n' "$ADMIN_TOKEN"
    if [ -n "$body" ]; then
      printf '%s' "$body" > "$WORK/body.json"
      printf 'header = "content-type: application/json"\n'
      printf 'data = "@%s"\n' "$WORK/body.json"
    fi
  } > "$WORK/curl.cfg"
  # --fail-with-body writes the error body to stdout, which callers capture
  # into a variable -- so on failure it has to be re-emitted on stderr or the
  # server's actual complaint ("unknown admin token") is lost and all the
  # operator sees is curl's "error: 401".
  local out status=0
  out=$(curl -sS --fail-with-body -K "$WORK/curl.cfg") || status=$?
  if [ "$status" -ne 0 ]; then
    [ -n "$out" ] && printf '%s\n' "$out" >&2
    return "$status"
  fi
  printf '%s' "$out"
}

# --days N -> absolute epoch-ms; --days never -> empty (field omitted).
expiry_from_days() {
  case $1 in
    never) printf '' ;;
    ''|*[!0-9]*) die "--days takes a positive integer or \"never\"" ;;
    *) printf '%d' $(( ($(date +%s) + $1 * 86400) * 1000 )) ;;
  esac
}

setup_link() {
  [ -n "$PUBLIC_URL" ] ||
    die "can't build a setup link: set RELAY_PUBLIC_URL (no RELAY_DOMAIN in $ENV_FILE)"
  python3 "$WORK/setup_link.py" "$PUBLIC_URL" "$1" "$SITE_ORIGIN"
}

# Accept the 12-character prefix that `list` prints, not just a full token:
# the masked column is deliberately a usable prefix so it can be copied
# straight back into another command. Same for the family_id column -- and
# there this must resolve to the row's TOKEN rather than echoing back what it
# was handed. Every by-family admin route takes either, so passing the id
# through would work everywhere except the one place it matters most:
# `link` embeds its argument in the CMRELAY1 card as the relay credential, and
# a card carrying a family id installs cleanly and then 401s on every phone.
resolve_token() {
  local candidate=$1 direct resolved matches count
  # 2>/dev/null, not >/dev/null 2>&1: the body is wanted, only api()'s
  # re-emitted error is not.
  if direct=$(api GET "/admin/families/$candidate" 2>/dev/null); then
    resolved=$(printf '%s' "$direct" | python3 "$WORK/family_field.py" token)
    printf '%s' "${resolved:-$candidate}"
    return
  fi
  matches=$(api GET "/admin/families?limit=500" | python3 "$WORK/match_prefix.py" "$candidate")
  count=$(printf '%s' "$matches" | grep -c . || true)
  case $count in
    0) die "no family matches \"$candidate\"" ;;
    1) printf '%s' "$matches" ;;
    *) die "\"$candidate\" matches $count families -- use more characters" ;;
  esac
}

cmd_provision() {
  local days=90 plan=dev-test note="provisioned by relay_admin.sh" token="" quota=""
  while [ $# -gt 0 ]; do
    case $1 in
      --days) days=$2; shift 2 ;;
      --plan) plan=$2; shift 2 ;;
      --note) note=$2; shift 2 ;;
      --token) token=$2; shift 2 ;;
      --quota-bytes) quota=$2; shift 2 ;;
      *) die "unknown flag $1" ;;
    esac
  done
  [ -n "$token" ] || token=$(python3 "$WORK/mint_token.py")
  local expires body
  expires=$(expiry_from_days "$days")
  body=$(python3 "$WORK/build_provision.py" "$token" "$plan" "$note" "$expires" "$quota")
  # The response carries the family id this call just minted. Discarding it
  # meant the operator had to `show` the family right back to learn the one
  # handle worth writing down.
  local created family_id
  created=$(api POST /admin/families "$body")
  family_id=$(printf '%s' "$created" | python3 "$WORK/family_field.py" family_id)

  if [ -n "$expires" ]; then
    echo "provisioned  plan=$plan  expires $(date -u -d "@$((expires / 1000))" '+%Y-%m-%d') UTC (${days}d)"
  else
    echo "provisioned  plan=$plan  never expires"
  fi
  echo
  if [ -n "$family_id" ]; then
    echo "family id (stable across token rotation -- record this one):"
    echo "  $family_id"
    echo
  fi
  echo "family token:"
  echo "  $token"
  echo
  echo "setup link (paste into Shore Pass on the first phone; it QRs to the second):"
  echo "  $(setup_link "$token")"
}

cmd_list() {
  local reveal=0 raw=0 status="" limit=100 offset=0 query
  while [ $# -gt 0 ]; do
    case $1 in
      --status) status=$2; shift 2 ;;
      --limit) limit=$2; shift 2 ;;
      --offset) offset=$2; shift 2 ;;
      --reveal) reveal=1; shift ;;
      --json) raw=1; shift ;;
      *) die "unknown flag $1" ;;
    esac
  done
  query="limit=$limit&offset=$offset"
  if [ -n "$status" ]; then
    query="$query&status=$status"
  fi
  # Captured rather than piped straight into python so a failed request
  # surfaces curl's error, not a JSONDecodeError traceback on empty input.
  local response
  response=$(api GET "/admin/families?$query")
  if [ "$raw" = 1 ]; then
    printf '%s\n' "$response"
  else
    printf '%s' "$response" | python3 "$WORK/render_list.py" "$reveal" "$GRACE_MS"
  fi
}

# resolve_token's die() only exits the *subshell* a command substitution runs
# in, and `set -e` does not fire for a substitution embedded in another
# command's arguments. So every caller assigns it to its own variable first:
# there the failed substitution is the assignment's status and the script
# stops. Inlining it instead used to mean `link <unknown>` printed
# "no family matches" on stderr and then, on stdout and with exit status 0, a
# CMRELAY1 card carrying an empty token -- a paste-ready link to nowhere.
cmd_show() {
  [ $# -eq 1 ] || die "usage: show <token>"
  local token response
  token=$(resolve_token "$1")
  response=$(api GET "/admin/families/$token")
  printf '%s' "$response" | python3 "$WORK/pretty.py"
}

cmd_link() {
  [ $# -eq 1 ] || die "usage: link <token>"
  local token
  token=$(resolve_token "$1")
  setup_link "$token"
}

cmd_extend() {
  { [ $# -eq 3 ] && [ "$2" = "--days" ]; } || die "usage: extend <token> --days N|never"
  local token expires body
  token=$(resolve_token "$1")
  expires=$(expiry_from_days "$3")
  if [ -z "$expires" ]; then
    # PATCH is merge-only, so it cannot clear expires_ms -- only a
    # re-provision drops the field back to absent.
    body=$(api GET "/admin/families/$token" | python3 "$WORK/reprovision_without_expiry.py")
    api POST /admin/families "$body" >/dev/null
    echo "expiry cleared -- this family never expires"
  else
    api PATCH "/admin/families/$token" "{\"expires_ms\":$expires}" >/dev/null
    echo "expires $(date -u -d "@$((expires / 1000))" '+%Y-%m-%d') UTC"
  fi
}

set_status() {
  local token
  token=$(resolve_token "$1")
  api PATCH "/admin/families/$token" "{\"status\":\"$2\"}" >/dev/null
  echo "$3"
}

cmd_suspend() {
  [ $# -eq 1 ] || die "usage: suspend <token>"
  # authorize_family() re-reads the row on every request, so this bites on the
  # family's very next call -- no restart, and no grace window.
  set_status "$1" suspended "suspended -- every request from this family now fails (mailbox kept)"
}

cmd_resume() {
  [ $# -eq 1 ] || die "usage: resume <token>"
  set_status "$1" active "active again"
}

cmd_purge() {
  { [ $# -eq 2 ] && [ "$2" = "--yes" ]; } ||
    die "usage: purge <token> --yes  (irreversible: drops the family and its stored envelopes)"
  local token
  token=$(resolve_token "$1")
  api DELETE "/admin/families/$token" >/dev/null
  echo "purged -- family deleted, stored envelopes and presence dropped"
}

usage() {
  # Delimiter is deliberately not "USAGE": the text below has a section
  # header on its own line at column 0, which would end the heredoc there.
  cat <<'__HELP__'
relay_admin.sh -- operator CLI for a hosted CruiseMesh relay ("Shore Pass").

USAGE
  relay_admin.sh <command> [args]

COMMANDS
  provision [--days N|never] [--plan P] [--note "..."] [--quota-bytes N]
            [--token T]
        Mint a family and print its stable family id and token plus a
        paste-ready CMRELAY1 setup link. Defaults: --days 90, --plan dev-test.
        Re-running with an existing --token re-provisions it (and reactivates
        a suspended one).

  list [--status active|suspended] [--limit N] [--offset N] [--reveal|--json]
        Every family with its stable id, usage and effective state (active /
        grace / expired / suspended). Tokens are truncated to 12 characters,
        which is still a valid argument for the commands below; --reveal
        prints them in full, --json emits the raw API response.

  show <token>        One family as JSON, including usage_bytes and
                      envelope_count.
  link <token>        Re-print the CMRELAY1 setup link for an existing family.
  extend <token> --days N|never
                      Move (or clear) the expiry.
  suspend <token>     Stop service immediately, keep the mailbox.
  resume <token>      Undo a suspend.
  purge <token> --yes Delete the family AND its stored envelopes.

  --help, -h          This message.

  <token> may be any unambiguous prefix -- including the 12 characters `list`
  prints. An ambiguous prefix is refused rather than guessed. It may equally
  be a `cmfid1-...` family id, the stable handle in `list`'s family_id column:
  a token changes under `POST /family/rotate` and an id never does, so the id
  is what to record against a customer. `link` given an id still emits a card
  carrying the family's current token, never the id.

  A relay older than the stable id prints "-" in that column; every command
  keeps working on tokens there.

SUSPEND VS PURGE
  A pass that lapsed or was refunded should be SUSPENDED: reversible, and the
  family's queued mail survives if they renew. A token that leaked should be
  PURGED and a fresh one provisioned -- that is irreversible and drops stored
  envelopes, which is why it demands --yes.

RUNNING IT
  On the relay box, where the admin token is read from the deploy's .env so it
  never enters a shell history:

      ssh root@relay.cruisemesh.app 'bash -s list' < tools/relay_admin.sh

  Note the `--` when piping a flag: bash would otherwise claim it for itself.

      ssh root@relay.cruisemesh.app 'bash -s -- --help' < tools/relay_admin.sh

  or, if installed on the box (a symlink to the git checkout, so `git pull`
  keeps it current):

      relay-admin list

  From anywhere else, against the public endpoint:

      export CRUISEMESH_RELAY_ADMIN_TOKEN=...   # Cloudflare secret
      tools/relay_admin.sh list

ENVIRONMENT
  CRUISEMESH_RELAY_ADMIN_TOKEN  Admin bearer. If unset, read from the .env
                                below (i.e. you are on the relay box).
  RELAYD_ENV_FILE               Deploy .env to read that token from.
                                Default: /opt/cruisemesh/relayd/.env
  RELAY_ADMIN_ORIGIN            API base. Default: https://relay.cruisemesh.app
  RELAY_PUBLIC_URL              Relay URL embedded in setup cards. Default:
                                https://<RELAY_DOMAIN from the .env>
  CRUISEMESH_SITE_ORIGIN        Host of the /r# link. Default:
                                https://cruisemesh.app

See relayd/DEPLOY.md §12 for the API these commands call.
__HELP__
}

main() {
  local command=${1:-}
  case $command in
    -h|--help|help)
      usage
      exit 0
      ;;
    "")
      # No command is a usage error, so the text goes to stderr and the exit
      # status is nonzero -- but an explicit --help is a successful request
      # and prints to stdout, so it can be piped without tripping a caller.
      usage >&2
      exit 1
      ;;
    provision|list|show|link|extend|suspend|resume|purge) ;;
    # Reject a bad command name here, before resolve_config, so a typo says
    # "unknown command" rather than complaining about a missing admin token.
    *) die "unknown command \"$command\" (try --help)" ;;
  esac
  shift
  write_helpers
  resolve_config
  case $command in
    provision) cmd_provision "$@" ;;
    list)      cmd_list "$@" ;;
    show)      cmd_show "$@" ;;
    link)      cmd_link "$@" ;;
    extend)    cmd_extend "$@" ;;
    suspend)   cmd_suspend "$@" ;;
    resume)    cmd_resume "$@" ;;
    purge)     cmd_purge "$@" ;;
  esac
}

main "$@"
