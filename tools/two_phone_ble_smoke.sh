#!/usr/bin/env bash
# Two-phone BLE regression smoke test (TI2).
#
# Drives the real thing that keeps breaking: a pair of phones that are talking
# over the local network, lose Wi-Fi, and have to keep delivering over
# Bluetooth. That is the B1 wedge (BLE never relinked after a LAN teardown),
# the B5 zombie header (the chat header kept claiming Wi-Fi over a dead
# radio), and the B6 connect storm (hundreds of doomed connects to rotating
# addresses in a dense fleet) in one run.
#
# The hard-won lesson from the sessions that found those bugs: NEVER gate a
# BLE test on the chat header. B5 made the header lie for ten minutes while
# messages were in fact flowing over Bluetooth, and it turned three good runs
# into false failures. Every assertion here is on logcat -- specifically on a
# delivery receipt coming back -- and the header is only ever captured as a
# screenshot for a human to look at afterwards.
#
# Usage:
#   tools/two_phone_ble_smoke.sh [-a SENDER_SERIAL] [-b PEER_SERIAL]
#                                [-p PEER_NAME] [-o ARTIFACT_DIR]
#                                [-n NEW_DEVICE_SERIAL]
#
#   -a  serial of the phone under test (the sender; needs the build you are
#       validating). Defaults to the first of exactly two attached devices.
#   -b  serial of the peer. It only has to be reachable and paired -- it does
#       not need the new build, because B5 is a bug in the *observer*.
#   -p  contact name to open on the sender (default: the first chat in the list)
#   -o  where to write logs and screenshots (default: ./smoke-artifacts/<ts>)
#   -n  serial of a THIRD, freshly installed handset, to run the WP6 link +
#       converge pass (section 5). Omit it and that section is skipped. It has
#       to be freshly installed: a phone that already holds contacts and
#       messages cannot be adopted as a new device (spec section 9.3), and both
#       phones in the ordinary rig do.
#
# Exit status is 0 only if every gate passes.

set -uo pipefail

# Git Bash rewrites anything that looks like a Unix path in an argument, so
# `adb shell ... /sdcard/ui.xml` silently becomes
# `C:/Program Files/Git/sdcard/ui.xml` on the device and every dump lands
# nowhere. Harmless to export on Linux and macOS.
export MSYS_NO_PATHCONV=1
export MSYS2_ARG_CONV_EXCL='*'

PEER_NAME=""
SENDER=""
PEER=""
ARTIFACTS=""
NEW_DEVICE=""

while getopts ":a:b:p:o:n:h" opt; do
    case "$opt" in
        a) SENDER="$OPTARG" ;;
        b) PEER="$OPTARG" ;;
        p) PEER_NAME="$OPTARG" ;;
        o) ARTIFACTS="$OPTARG" ;;
        n) NEW_DEVICE="$OPTARG" ;;
        h) sed -n '2,35p' "$0"; exit 0 ;;
        *) echo "unknown option -$OPTARG" >&2; exit 2 ;;
    esac
done

PKG=com.cruisemesh.app
ACTIVITY="$PKG/.MainActivity"
# How long to wait for a receipt to come back over Bluetooth. Generous: a
# fresh BLE link plus fragment reassembly is seconds, but a re-link after the
# LAN teardown can take a slow-probe cycle.
DELIVERY_TIMEOUT_S=90
# B6 guard. The capture that motivated the connect cap saw 488 failed
# connects in 7 minutes (~70/min) in a house full of CruiseMesh phones. With
# the cap the same desk measures well under 10/min. Fail well below the old
# rate but above normal churn, so this catches a regression without flaking
# on a quiet desk or a busy one.
STATUS_133_PER_MIN_MAX=25
# How long section 5 waits for a person to drive the ceremony on two handsets:
# a scan, six digits compared out loud, and a bootstrap on the wire. Generous
# because the slow part is human.
LINK_TIMEOUT_S=300

pass_count=0
fail_count=0
declare -a FAILURES=()

say()  { printf '\n\033[1m%s\033[0m\n' "$*"; }
ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$*"; pass_count=$((pass_count + 1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$*"; fail_count=$((fail_count + 1)); FAILURES+=("$*"); }
info() { printf '        %s\n' "$*"; }

adb_a() { adb -s "$SENDER" "$@"; }
adb_b() { adb -s "$PEER" "$@"; }

# ---------------------------------------------------------------- preflight

mapfile -t DEVICES < <(adb devices | awk '/\tdevice$/ {print $1}')
if [ -z "$SENDER" ] || [ -z "$PEER" ]; then
    if [ "${#DEVICES[@]}" -ne 2 ]; then
        echo "error: need exactly two attached devices (found ${#DEVICES[@]}); pass -a and -b" >&2
        exit 2
    fi
    SENDER="${SENDER:-${DEVICES[0]}}"
    PEER="${PEER:-${DEVICES[1]}}"
fi

for serial in "$SENDER" "$PEER"; do
    if ! printf '%s\n' "${DEVICES[@]}" | grep -qx "$serial"; then
        echo "error: $serial is not an attached device in state 'device'" >&2
        exit 2
    fi
    if ! adb -s "$serial" shell pm path "$PKG" 2>/dev/null | grep -q package:; then
        echo "error: $PKG is not installed on $serial" >&2
        exit 2
    fi
done

if [ -n "$NEW_DEVICE" ]; then
    if ! printf '%s\n' "${DEVICES[@]}" | grep -qx "$NEW_DEVICE"; then
        echo "error: $NEW_DEVICE is not an attached device in state 'device'" >&2
        exit 2
    fi
    if ! adb -s "$NEW_DEVICE" shell pm path "$PKG" 2>/dev/null | grep -q package:; then
        echo "error: $PKG is not installed on $NEW_DEVICE" >&2
        exit 2
    fi
fi

ARTIFACTS="${ARTIFACTS:-smoke-artifacts/$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$ARTIFACTS"

say "Two-phone BLE smoke test"
info "sender (under test): $SENDER  $(adb_a shell getprop ro.product.model | tr -d '\r')"
info "peer:                $PEER  $(adb_b shell getprop ro.product.model | tr -d '\r')"
info "artifacts:           $ARTIFACTS"

# Remember Wi-Fi state so the rig is left as we found it even on failure.
wifi_state() { adb -s "$1" shell settings get global wifi_on 2>/dev/null | tr -d '\r'; }
SENDER_WIFI_WAS="$(wifi_state "$SENDER")"
PEER_WIFI_WAS="$(wifi_state "$PEER")"

restore() {
    say "Restoring"
    # wifi_on is not a clean boolean -- it reports 2 on a phone whose Wi-Fi is
    # on while airplane mode is engaged. Anything but 0 means "was on".
    [ "${SENDER_WIFI_WAS:-0}" != "0" ] && adb_a shell svc wifi enable >/dev/null 2>&1
    [ "${PEER_WIFI_WAS:-0}" != "0" ] && adb_b shell svc wifi enable >/dev/null 2>&1
    info "Wi-Fi restored (sender was=$SENDER_WIFI_WAS peer was=$PEER_WIFI_WAS)"
}
trap restore EXIT

for serial in "$SENDER" "$PEER"; do
    # Screen-off mid-run kills unattended sessions once the lockscreen grabs
    # focus, and a locked phone stops driving the UI.
    adb -s "$serial" shell svc power stayon true >/dev/null 2>&1
    if [ "$(adb -s "$serial" shell settings get global bluetooth_on | tr -d '\r')" != "1" ]; then
        echo "error: Bluetooth is off on $serial -- this test is meaningless without it" >&2
        exit 2
    fi
done

# ------------------------------------------------------- ui helpers (sender)

# Dump the view hierarchy and echo "x y" for the centre of the first node
# whose content-desc or text matches $1. uiautomator can hang forever on an
# animated screen ("could not get idle state"), so it is always time-boxed.
find_tap_point() {
    local needle="$1" xml="$ARTIFACTS/ui.xml"
    # `timeout` execs a binary and cannot run the adb_a shell function, so
    # these two call adb directly.
    timeout 20 adb -s "$SENDER" shell uiautomator dump /sdcard/ui.xml >/dev/null 2>&1 || return 1
    timeout 20 adb -s "$SENDER" shell cat /sdcard/ui.xml > "$xml" 2>/dev/null || return 1
    # The XML goes in on stdin rather than by path: under Git Bash the Python
    # here is a native Windows build that cannot open an /c/... MSYS path.
    python3 -c '
import re, sys
needle = sys.argv[1].lower()
best = None
for tag in re.findall(r"<node\b[^>]*>", sys.stdin.read()):
    desc = (re.search(r"content-desc=\"([^\"]*)\"", tag) or [None, ""])[1] if re.search(r"content-desc=\"([^\"]*)\"", tag) else ""
    text = (re.search(r"\stext=\"([^\"]*)\"", tag) or [None, ""])[1] if re.search(r"\stext=\"([^\"]*)\"", tag) else ""
    b = re.search(r"bounds=\"\[(\d+),(\d+)\]\[(\d+),(\d+)\]\"", tag)
    if not b:
        continue
    # Rank matches so a chat row titled "Emma" wins over the avatar beside it
    # whose description merely mentions Emma -- tapping the avatar opens the
    # contact sheet instead of the conversation.
    if text.lower() == needle:      rank = 0
    elif desc.lower() == needle:    rank = 1
    elif needle in text.lower():    rank = 2
    elif needle in desc.lower():    rank = 3
    else:                           continue
    if best is None or rank < best[0]:
        x1, y1, x2, y2 = map(int, b.groups())
        best = (rank, (x1 + x2) // 2, (y1 + y2) // 2)
if best:
    print(best[1], best[2])
' "$needle" < "$xml"
}

# uiautomator intermittently refuses to dump while the UI is animating
# ("could not get idle state"), which is common on a screen with a live
# presence indicator. Retry a few times before giving up.
find_tap_point_retry() {
    local needle="$1" point="" i
    for i in 1 2 3; do
        point="$(find_tap_point "$needle")"
        [ -n "$point" ] && { printf '%s' "$point"; return 0; }
        sleep 3
    done
    return 1
}

screencap() { adb_a exec-out screencap -p > "$ARTIFACTS/$1.png" 2>/dev/null; info "screenshot -> $ARTIFACTS/$1.png"; }

# Newest "Receipt from ...: ackedSender=... throughLamport=N" watermark the
# sender has seen. A receipt with a HIGHER lamport after we send is proof the
# peer actually took delivery -- this is the only honest delivery gate.
max_lamport() {
    adb_a logcat -d -s MeshService 2>/dev/null \
        | grep -o 'throughLamport=[0-9]*' | grep -o '[0-9]*' | sort -n | tail -1
}

# --------------------------------------------------------------- 1. baseline

say "1. Baseline: app up, peer reachable"
adb_a shell svc wifi enable >/dev/null 2>&1
adb_b shell svc wifi enable >/dev/null 2>&1
sleep 5
adb_a shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1
adb_a shell cmd statusbar collapse >/dev/null 2>&1
adb_a shell am start -n "$ACTIVITY" >/dev/null 2>&1
sleep 6

if [ -n "$PEER_NAME" ]; then
    CHAT_POINT="$(find_tap_point_retry "$PEER_NAME")"
else
    CHAT_POINT="$(find_tap_point_retry 'Avatar for')"
fi
if [ -z "${CHAT_POINT:-}" ]; then
    bad "could not find a chat to open on the sender"
    screencap "01-no-chat-found"
    say "Summary"; echo "  $pass_count passed, $fail_count failed"; exit 1
fi
# shellcheck disable=SC2086
adb_a shell input tap $CHAT_POINT >/dev/null 2>&1
sleep 4
screencap "01-baseline-header"
ok "opened a chat on the sender"

# ------------------------------------------------- 2. LAN teardown behaviour

say "2. Drop Wi-Fi on both phones"
adb_a logcat -c >/dev/null 2>&1
LAMPORT_BEFORE="$(max_lamport)"; LAMPORT_BEFORE="${LAMPORT_BEFORE:-0}"
info "highest receipt watermark before the test: $LAMPORT_BEFORE"

adb_b shell svc wifi disable >/dev/null 2>&1
adb_a shell svc wifi disable >/dev/null 2>&1
info "Wi-Fi disabled on both at $(date +%H:%M:%S)"

teardown_seen=0
for _ in $(seq 1 20); do
    if adb_a logcat -d 2>/dev/null | grep -qE 'LAN connection closed|LAN peer disconnected'; then
        teardown_seen=1; break
    fi
    sleep 1
done
if [ "$teardown_seen" = "1" ]; then
    ok "sender saw the LAN link tear down"
else
    # Not fatal: the pair may have been Bluetooth-only to begin with, in which
    # case there was no LAN link to lose and the delivery gate still applies.
    info "no LAN teardown logged -- the pair was probably already BLE-only"
fi
sleep 12
screencap "02-after-wifi-off-header"

# ------------------------------------------------------- 3. delivery over BLE

say "3. Send a probe and require a receipt back"
# A stray tap can leave a contact sheet open over the composer; dismiss it.
if find_tap_point 'Close sheet' >/dev/null 2>&1 && [ -n "$(find_tap_point 'Close sheet')" ]; then
    info "dismissing an open sheet"
    adb_a shell input keyevent KEYCODE_BACK >/dev/null 2>&1
    sleep 2
fi
# Anchor on the attach button: it is the one composer control that is always
# present. The mic is swapped out for Send the moment the field has text --
# including a draft left behind by an earlier run -- so it is not a reliable
# landmark.
ATTACH_POINT="$(find_tap_point_retry 'Attach photo')"
if [ -z "${ATTACH_POINT:-}" ]; then
    bad "could not locate the composer (no attach button found)"
    screencap "03-no-composer"
else
    ATTACH_X="${ATTACH_POINT% *}"; ROW_Y="${ATTACH_POINT#* }"
    SCREEN_W="$(adb_a shell wm size | tr -d '\r' | sed 's/.*: *//' | cut -dx -f1)"
    SCREEN_W="${SCREEN_W:-1080}"
    adb_a shell input tap $((SCREEN_W / 2)) "$ROW_Y" >/dev/null 2>&1
    sleep 2
    # Clear any draft so the probe text is exactly what we think it is.
    adb_a shell input keyevent KEYCODE_MOVE_END >/dev/null 2>&1
    adb_a shell input keyevent $(printf '67 %.0s' $(seq 1 60)) >/dev/null 2>&1
    sleep 1
    PROBE="ble-smoke-$(date +%H%M%S)"
    adb_a shell input text "$PROBE" >/dev/null 2>&1
    sleep 2
    # Dismiss the keyboard before locating Send. The send button only exists
    # once there is text, and with the IME up the whole composer row has
    # shifted, so its position must be re-resolved rather than assumed from
    # where the mic used to be.
    adb_a shell input keyevent KEYCODE_BACK >/dev/null 2>&1
    sleep 2
    SEND_POINT="$(find_tap_point_retry 'Send')"
    if [ -z "${SEND_POINT:-}" ]; then
        bad "typed the probe but could not find the Send button"
        screencap "03-no-send-button"
        SEND_POINT=""
    else
        # shellcheck disable=SC2086
        adb_a shell input tap $SEND_POINT >/dev/null 2>&1
        info "sent \"$PROBE\" at $(date +%H:%M:%S)"
    fi

    delivered=0
    for _ in $(seq 1 "$DELIVERY_TIMEOUT_S"); do
        now="$(max_lamport)"; now="${now:-0}"
        if [ "$now" -gt "$LAMPORT_BEFORE" ]; then delivered=1; break; fi
        sleep 1
    done
    sleep 2
    adb_a shell input keyevent KEYCODE_BACK >/dev/null 2>&1
    sleep 1
    screencap "03-after-send"
    if [ "$delivered" = "1" ]; then
        ok "receipt came back over Bluetooth (watermark $LAMPORT_BEFORE -> $(max_lamport))"
    else
        bad "no delivery receipt within ${DELIVERY_TIMEOUT_S}s -- messages are not getting through on BLE"
    fi
fi

# ------------------------------------------------------ 4. connect-churn cap

say "4. Connect churn (B6)"
adb_a logcat -d -v time > "$ARTIFACTS/sender-logcat.txt" 2>/dev/null
# grep -c prints 0 AND exits non-zero when there are no matches, so a
# `|| echo 0` fallback would append a second line and corrupt the number.
churn="$(grep -c 'status=133' "$ARTIFACTS/sender-logcat.txt" 2>/dev/null)"
capped="$(grep -c 'central link cap' "$ARTIFACTS/sender-logcat.txt" 2>/dev/null)"
churn="${churn:-0}"; capped="${capped:-0}"
# Everything since the `logcat -c` in step 2.
elapsed_min="$(python3 -c "print(max(1,round(${SECONDS}/60)))")"
rate="$(python3 -c "print(round(${churn}/${elapsed_min},1))")"
info "failed connects (status=133): $churn over ~${elapsed_min}min  = ${rate}/min"
info "'at central link cap' log lines: $capped"
if python3 -c "import sys; sys.exit(0 if ${rate} <= ${STATUS_133_PER_MIN_MAX} else 1)"; then
    ok "connect churn within budget (<= ${STATUS_133_PER_MIN_MAX}/min)"
else
    bad "connect churn ${rate}/min exceeds ${STATUS_133_PER_MIN_MAX}/min -- the central link cap may have regressed"
fi

# --------------------------------------------------- 5. link + converge (WP6)

# `specs/multi-device-v1.md` §13's WP6 gate: "two-phone smoke script extended
# with a link + converge pass". Opt-in, and it needs a THIRD handset: §9.3
# refuses to adopt a phone that already holds somebody, and both phones in the
# ordinary rig do -- which is the whole point of the four sections above.
#
# Same lesson as everywhere else in this file, applied to a new screen: the gate
# is logcat, never what the UI says. LinkSession writes one "link complete:
# role=..." line on each phone carrying the device-list head it ended up
# holding, and the two heads matching IS convergence -- it is the exact value
# §9.4 makes the new phone acknowledge before it is allowed to speak at all. A
# head is a hash of a public document and a device id is derived from a public
# key; no identity secret, QR payload or authentication string is logged, and
# this gate must never be changed to need one.
#
# Driving the ceremony is a person's job and is left that way on purpose. It has
# a camera, a code held up between two handsets and six digits read out loud;
# scripting taps through it would be testing uiautomator's aim rather than the
# ceremony, and the parts that matter here are what the two phones end up
# holding and whether the contacts were told.

if [ -z "$NEW_DEVICE" ]; then
    say "5. Link + converge (WP6): skipped"
    info "pass -n SERIAL with a freshly installed third handset to run it."
    info "a phone that already holds contacts cannot be adopted (spec §9.3)."
else
    say "5. Link + converge (WP6)"
    adb_a shell svc wifi enable >/dev/null 2>&1
    adb -s "$NEW_DEVICE" shell svc wifi enable >/dev/null 2>&1
    adb -s "$NEW_DEVICE" shell input keyevent KEYCODE_WAKEUP >/dev/null 2>&1
    sleep 5
    adb_a logcat -c >/dev/null 2>&1
    adb -s "$NEW_DEVICE" logcat -c >/dev/null 2>&1

    info "drive the ceremony by hand now:"
    info "  new phone ($NEW_DEVICE): Restore from backup -> Set up as a new device"
    info "  this phone ($SENDER):    Settings -> Your devices -> Add a device"
    info "  compare the six digits, then tap 'They match' on THIS phone."
    info "waiting up to ${LINK_TIMEOUT_S}s for both phones to report a finished link..."

    # The newest completion line from one phone, or empty.
    link_line_for() {
        adb -s "$1" logcat -d -s LinkSession 2>/dev/null \
            | grep -o 'link complete: role=[^ ]* deviceId=[0-9a-f]* rosterHead=[0-9a-f]*.*' \
            | tail -1
    }
    head_of() { printf '%s' "$1" | grep -o 'rosterHead=[0-9a-f]*' | cut -d= -f2; }

    linked=0
    A_LINE=""
    N_LINE=""
    for _ in $(seq 1 "$LINK_TIMEOUT_S"); do
        A_LINE="$(link_line_for "$SENDER")"
        N_LINE="$(link_line_for "$NEW_DEVICE")"
        if [ -n "$A_LINE" ] && [ -n "$N_LINE" ]; then linked=1; break; fi
        sleep 1
    done

    screencap "05-existing-phone-after-link"
    adb -s "$NEW_DEVICE" exec-out screencap -p > "$ARTIFACTS/05-new-phone-after-link.png" 2>/dev/null
    adb -s "$NEW_DEVICE" logcat -d -v time > "$ARTIFACTS/new-device-logcat.txt" 2>/dev/null

    if [ "$linked" != "1" ]; then
        bad "the ceremony did not finish on both phones within ${LINK_TIMEOUT_S}s"
    else
        A_HEAD="$(head_of "$A_LINE")"
        N_HEAD="$(head_of "$N_LINE")"
        info "existing phone: $A_LINE"
        info "new phone:      $N_LINE"
        if [ -n "$A_HEAD" ] && [ "$A_HEAD" = "$N_HEAD" ]; then
            ok "both phones converged on the same device list ($A_HEAD)"
        else
            bad "the phones ended on different device lists ($A_HEAD vs $N_HEAD)"
        fi

        # §9.3: the export really carried something. A ceremony that finishes
        # onto an empty store is a handshake that worked and a bootstrap that
        # did not, and the screens look identical.
        imported="$(printf '%s' "$N_LINE" | grep -o 'contacts=[0-9]*' | cut -d= -f2)"
        imported="${imported:-0}"
        if [ "$imported" -gt 0 ]; then
            ok "the new phone brought over $imported contact(s)"
        else
            bad "the new phone finished the ceremony holding no contacts"
        fi

        # §9.5 / DL-3, which is WP6's own slice: the moment the fleet grew, the
        # existing phone had to seal the new device list to each contact. This
        # line is the only proof that happened on real hardware.
        told="$(adb_a logcat -d -s RosterGossipSender 2>/dev/null \
            | grep -o 'Told [0-9]* contact' | grep -o '[0-9]*' | sort -n | tail -1)"
        told="${told:-0}"
        if [ "$told" -gt 0 ]; then
            ok "the existing phone told $told contact(s) about the new device"
        else
            bad "no contact was told about the new device (the §9.5 gossip never fired)"
        fi
    fi
fi

# ------------------------------------------------------------------ summary

say "Summary"
echo "  $pass_count passed, $fail_count failed"
for f in ${FAILURES+"${FAILURES[@]}"}; do echo "    - $f"; done
echo
echo "  Look at $ARTIFACTS/02-after-wifi-off-header.png: with Wi-Fi down the"
echo "  chat header must NOT still say 'Nearby via Wi-Fi' (that is B5)."
[ "$fail_count" -eq 0 ]
