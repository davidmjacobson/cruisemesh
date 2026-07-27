#!/usr/bin/env bash
# relay_backup.sh -- WAL-safe nightly SQLite backup + disk watchdog for the
# hosted CruiseMesh relay. Install, verification, and restore steps live in
# relayd/DEPLOY.md §9; the systemd units that run this live in relayd/deploy/.
#
# What one run does, in order:
#   1. sqlite3 ".backup" of the live mailbox DB. That is SQLite's online
#      backup API -- the WAL-safe way to copy a database under a running
#      writer. A plain cp of only the .sqlite file would silently drop the
#      rows still sitting in the -wal sidecar (DEPLOY.md §9).
#   2. PRAGMA integrity_check plus row counts on the copy, so a bad backup
#      fails tonight, in the journal, and not on restore day.
#   3. gzip, then rotate: the newest CRUISEMESH_BACKUP_KEEP (default 14)
#      nightly files are kept in CRUISEMESH_BACKUP_DIR.
#   4. OPTIONAL off-box push via rclone -- see the marked block in cmd_run;
#      off unless CRUISEMESH_BACKUP_RCLONE_REMOTE is set.
#   5. Disk watchdog: if the filesystem holding the data volume or the
#      backup directory is more than CRUISEMESH_DISK_ALERT_PCT (default 85)
#      percent full, print an ALERT line and exit non-zero so the systemd
#      unit's OnFailure= hook fires.
#
# `relay_backup.sh alert` is that OnFailure= hook: it emails the operator
# through the Resend API. The API key is read from OPS_ALERT_ENV_FILE
# (default /etc/cruisemesh/ops-alert.env), never from argv -- any local
# process can read /proc/*/cmdline.
#
# Backups contain FULL family bearer tokens (the families table) and the
# sealed envelopes. The script forces 0700/0600 modes on everything it
# writes; an off-box remote must be a private bucket.
#
# Needs: bash, sqlite3 (apt-get install -y sqlite3), gzip, docker (only to
# locate the data volume), and python3 + curl for `alert`.

set -euo pipefail
umask 077

BACKUP_DIR=${CRUISEMESH_BACKUP_DIR:-/var/backups/cruisemesh-relayd}
KEEP=${CRUISEMESH_BACKUP_KEEP:-14}
DISK_ALERT_PCT=${CRUISEMESH_DISK_ALERT_PCT:-85}
# HOST path of the live DB. Deliberately NOT named CRUISEMESH_RELAY_DB: that
# variable in the deploy .env is the path INSIDE the container (/data/...),
# and reusing the name here would recreate the §5 path gotcha on the host.
DB_PATH=${CRUISEMESH_BACKUP_DB:-}
VOLUME_NAME=${CRUISEMESH_BACKUP_VOLUME:-}
RCLONE_REMOTE=${CRUISEMESH_BACKUP_RCLONE_REMOTE:-}
OPS_ALERT_ENV_FILE=${OPS_ALERT_ENV_FILE:-/etc/cruisemesh/ops-alert.env}

WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

die() { echo "error: $*" >&2; exit 1; }

resolve_db_path() {
  if [ -n "$DB_PATH" ]; then
    [ -f "$DB_PATH" ] || die "CRUISEMESH_BACKUP_DB=$DB_PATH does not exist"
    return
  fi
  command -v docker >/dev/null ||
    die "docker not found and CRUISEMESH_BACKUP_DB unset -- point CRUISEMESH_BACKUP_DB at the live DB"
  # The compose volume is `relay-data`, prefixed with the compose project
  # name -- `relayd_relay-data` when started from relayd/ (DEPLOY.md §9).
  local volume mountpoint
  for volume in "$VOLUME_NAME" relayd_relay-data relay-data; do
    [ -n "$volume" ] || continue
    if mountpoint=$(docker volume inspect --format '{{ .Mountpoint }}' "$volume" 2>/dev/null); then
      DB_PATH="$mountpoint/cruisemesh-relayd.sqlite"
      [ -f "$DB_PATH" ] || die "volume $volume has no cruisemesh-relayd.sqlite at $mountpoint"
      return
    fi
  done
  die "no docker volume relayd_relay-data or relay-data here; set CRUISEMESH_BACKUP_VOLUME or CRUISEMESH_BACKUP_DB"
}

usage_pct() { df -P "$1" | awk 'NR == 2 { sub("%", "", $5); print $5 }'; }

check_disk() {
  local failed=0 location pct
  for location in "$(dirname "$DB_PATH")" "$BACKUP_DIR"; do
    pct=$(usage_pct "$location")
    if [ "$pct" -gt "$DISK_ALERT_PCT" ]; then
      # The ALERT prefix is the grep handle; the non-zero exit is what
      # actually fires the OnFailure= email.
      echo "ALERT: filesystem holding $location is ${pct}% full (threshold ${DISK_ALERT_PCT}%)" >&2
      failed=1
    else
      echo "disk ok: $location at ${pct}%"
    fi
  done
  return "$failed"
}

prune_old() {
  local -a snapshots
  mapfile -t snapshots < <(ls -1t "$BACKUP_DIR"/cruisemesh-relayd-*.sqlite.gz 2>/dev/null)
  if [ "${#snapshots[@]}" -gt "$KEEP" ]; then
    local victim
    for victim in "${snapshots[@]:$KEEP}"; do
      rm -f -- "$victim"
      echo "rotated out: $(basename "$victim")"
    done
  fi
}

cmd_run() {
  command -v sqlite3 >/dev/null || die "sqlite3 not installed (apt-get install -y sqlite3)"
  resolve_db_path
  mkdir -p "$BACKUP_DIR"
  chmod 700 "$BACKUP_DIR"

  local stamp snapshot
  stamp=$(date -u +%Y%m%d-%H%M%S)
  snapshot="$BACKUP_DIR/cruisemesh-relayd-$stamp.sqlite"

  # SQLite's online backup: a consistent copy while relayd keeps writing.
  # .timeout keeps one busy moment from failing the whole night's run.
  sqlite3 "$DB_PATH" ".timeout 15000" ".backup '$snapshot'"

  local verdict counts
  verdict=$(sqlite3 "$snapshot" "PRAGMA integrity_check;")
  if [ "$verdict" != "ok" ]; then
    rm -f -- "$snapshot"
    die "integrity_check on the fresh backup said: $verdict"
  fi
  # Row counts prove the schema came across, and give the journal a one-line
  # audit trail per night.
  counts=$(sqlite3 "$snapshot" \
    "SELECT (SELECT COUNT(*) FROM envelopes) || ' envelopes, ' || (SELECT COUNT(*) FROM families) || ' families';")
  gzip -f -9 "$snapshot"
  echo "backup ok: $snapshot.gz ($counts)"

  prune_old

  # --- OPTIONAL: off-box push ---------------------------------------------
  # A backup on the same disk as the database does not survive the disk.
  # Set CRUISEMESH_BACKUP_RCLONE_REMOTE to an rclone destination (e.g.
  # "b2:cruisemesh-backups/relayd") to copy each night's file off the box.
  # Credentials live in root's rclone.conf (`rclone config`, once), never
  # here and never in argv; the remote must be a PRIVATE bucket, because
  # these files contain family bearer tokens.
  if [ -n "$RCLONE_REMOTE" ]; then
    command -v rclone >/dev/null ||
      die "CRUISEMESH_BACKUP_RCLONE_REMOTE is set but rclone is not installed"
    rclone copy --no-traverse "$snapshot.gz" "$RCLONE_REMOTE/"
    echo "pushed off-box: $RCLONE_REMOTE/$(basename "$snapshot.gz")"
  fi
  # --------------------------------------------------------------------------

  # Last, so a nearly-full disk still gets tonight's backup before the run
  # is marked failed.
  check_disk || exit 1
}

cmd_alert() {
  # The OnFailure= hook. It has to work on a box that is already unhappy,
  # so it touches nothing but the env file and the Resend API.
  [ -r "$OPS_ALERT_ENV_FILE" ] ||
    die "no $OPS_ALERT_ENV_FILE (DEPLOY.md §9) -- failure NOT emailed; see journalctl -u cruisemesh-relay-backup.service"
  local resend_key
  resend_key=$(grep -a '^RESEND_API_KEY=' "$OPS_ALERT_ENV_FILE" | head -1 | cut -d= -f2- | tr -d '"\r')
  [ -n "$resend_key" ] || die "RESEND_API_KEY missing from $OPS_ALERT_ENV_FILE"
  command -v python3 >/dev/null || die "python3 required to build the alert body"

  local host stamp
  host=$(hostname -f 2>/dev/null || hostname)
  stamp=$(date -u '+%Y-%m-%d %H:%M:%S UTC')
  python3 - "$host" "$stamp" "$BACKUP_DIR" > "$WORK/email.json" <<'PY'
import json, sys

host, stamp, backup_dir = sys.argv[1:4]
text = f"""The nightly relay backup FAILED on {host} at {stamp}.

Either the sqlite3 .backup / integrity check failed, or the disk watchdog
tripped (data or backup filesystem over its threshold) -- the journal says
which:

    journalctl -u cruisemesh-relay-backup.service -n 50

Backups land in {backup_dir}; relayd/DEPLOY.md §9 has the verification and
restore steps.
"""
print(json.dumps({
    "from": "CruiseMesh Ops <ops@cruisemesh.app>",
    "to": "davejake@gmail.com",
    "reply_to": "support@cruisemesh.app",
    "subject": f"ALERT: relay backup failed on {host}",
    "text": text,
}))
PY

  # Same pattern as relay_admin.sh: the key rides a curl config file, so it
  # never appears in argv.
  {
    printf 'url = "https://api.resend.com/emails"\n'
    printf 'header = "authorization: Bearer %s"\n' "$resend_key"
    printf 'header = "content-type: application/json"\n'
    printf 'data = "@%s"\n' "$WORK/email.json"
  } > "$WORK/curl.cfg"
  curl -sS --fail-with-body --max-time 30 -K "$WORK/curl.cfg" >/dev/null
  echo "backup-failure alert emailed"
}

usage() {
  cat <<'__HELP__'
relay_backup.sh -- WAL-safe nightly backup + disk watchdog for the hosted
CruiseMesh relay. Normally run by cruisemesh-relay-backup.timer (see
relayd/deploy/ and DEPLOY.md §9), not by hand.

USAGE
  relay_backup.sh [run]     Back up, verify, rotate, (optionally) push
                            off-box, then check disk headroom. Non-zero exit
                            on any failure or when a disk is past threshold.
  relay_backup.sh alert     Email the operator that the run failed. Wired as
                            the backup unit's OnFailure= hook.
  relay_backup.sh --help    This message.

ENVIRONMENT
  CRUISEMESH_BACKUP_DIR            Where snapshots land.
                                   Default: /var/backups/cruisemesh-relayd
  CRUISEMESH_BACKUP_KEEP           Nightly files to keep. Default: 14
  CRUISEMESH_DISK_ALERT_PCT        Fullness threshold. Default: 85
  CRUISEMESH_BACKUP_DB             Host path of the live DB. Default: found
                                   via `docker volume inspect`.
  CRUISEMESH_BACKUP_VOLUME         Docker volume to look in first.
  CRUISEMESH_BACKUP_RCLONE_REMOTE  Optional rclone destination for the
                                   off-box push. Default: unset (no push).
  OPS_ALERT_ENV_FILE               File holding RESEND_API_KEY for `alert`.
                                   Default: /etc/cruisemesh/ops-alert.env
__HELP__
}

main() {
  case ${1:-run} in
    run) cmd_run ;;
    alert) cmd_alert ;;
    -h|--help|help) usage ;;
    *) die "unknown command \"$1\" (run | alert | --help)" ;;
  esac
}

main "$@"
