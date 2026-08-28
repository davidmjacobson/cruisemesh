#!/usr/bin/env bash
# Assert a produced Android artifact is aligned and signed, using the Android
# SDK's own tooling, before anything can hand it to a store.
#
#   tools/assert_android_artifact.sh <path/to/app.apk|app.aab>
#
# APK: `zipalign -c` (4-byte alignment, and 16 KiB page alignment where the
# installed build-tools support -P) then `apksigner verify`, which is the only
# check that actually parses the signature blocks -- an unsigned, truncated or
# tampered APK fails here rather than at install time on a tester's phone.
#
# AAB: a bundle is jar-signed, not APK-signed, so apksigner cannot read it at
# all. `jarsigner -verify` is the equivalent assertion; Play rejects an unsigned
# upload, but it rejects it after the workflow has already gone green.
#
# tools/check_android_elf_alignment.py stays the check for the *contents* (the
# packaged .so files' ELF load segments); this one is about the container.
set -eo pipefail

artifact="$1"
if [ -z "$artifact" ]; then
    echo "usage: tools/assert_android_artifact.sh <path/to/app.apk|app.aab>" >&2
    exit 2
fi
if [ ! -f "$artifact" ]; then
    echo "ERROR: no artifact at '$artifact' -- the build step did not produce one." >&2
    exit 1
fi

sdk="${ANDROID_HOME:-$ANDROID_SDK_ROOT}"

find_build_tool() {
    tool="$1"
    if [ -n "$sdk" ] && [ -d "$sdk/build-tools" ]; then
        # Highest installed build-tools version wins.
        for dir in $(ls -1 "$sdk/build-tools" 2>/dev/null | sort -V -r); do
            if [ -x "$sdk/build-tools/$dir/$tool" ]; then
                echo "$sdk/build-tools/$dir/$tool"
                return 0
            fi
        done
    fi
    command -v "$tool" 2>/dev/null && return 0
    return 1
}

fail_missing_tool() {
    {
        echo "ERROR: could not find '$1' in the Android SDK."
        echo "ANDROID_HOME='${ANDROID_HOME}' ANDROID_SDK_ROOT='${ANDROID_SDK_ROOT}'"
        echo "This check must not be skipped silently: an unverified artifact is"
        echo "exactly the thing it exists to stop."
    } >&2
    exit 1
}

case "$artifact" in
*.apk)
    zipalign="$(find_build_tool zipalign)" || fail_missing_tool zipalign
    apksigner="$(find_build_tool apksigner)" || fail_missing_tool apksigner

    echo "== zipalign -c (via $zipalign)"
    # -P 16 asserts 16 KiB page alignment and needs build-tools 35+; older
    # tooling rejects the flag, and the plain 4-byte check still runs.
    if ! "$zipalign" -c -P 16 4 "$artifact"; then
        echo "-- retrying without -P (build-tools too old for the page-alignment flag)"
        "$zipalign" -c 4 "$artifact" || {
            echo "ERROR: $artifact is not zipaligned; it must not be published." >&2
            exit 1
        }
    fi

    echo "== apksigner verify (via $apksigner)"
    "$apksigner" verify --verbose --print-certs "$artifact" || {
        echo "ERROR: $artifact failed signature verification; it must not be published." >&2
        exit 1
    }
    ;;
*.aab)
    echo "== jarsigner -verify"
    if ! out="$(jarsigner -verify -verbose:summary "$artifact" 2>&1)"; then
        printf '%s\n' "$out" >&2
        echo "ERROR: $artifact failed jarsigner verification; it must not be uploaded." >&2
        exit 1
    fi
    printf '%s\n' "$out" | tail -20
    case "$out" in
    *"jar verified"*) ;;
    *)
        echo "ERROR: $artifact is not signed (jarsigner did not report 'jar verified')." >&2
        echo "Play would reject it, but only after this workflow reported success." >&2
        exit 1
        ;;
    esac
    echo "== signer certificate"
    keytool -printcert -jarfile "$artifact" | grep -Ei 'Owner|SHA256:' || true
    ;;
*)
    echo "ERROR: don't know how to verify '$artifact' (expected .apk or .aab)." >&2
    exit 2
    ;;
esac

echo "OK: $(basename "$artifact") is aligned and signed."
