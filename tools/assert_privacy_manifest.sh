#!/usr/bin/env bash
# Asserts a built CruiseMesh app really carries PrivacyInfo.xcprivacy, and that
# its answers still match what the App Store listing declares.
#
# The manifest is wired up in ios/project.yml (a `resources` build phase entry),
# and xcodegen regenerates the Xcode project on every CI run -- so the only
# thing that proves the manifest reached the binary is looking inside the
# binary. Nothing did: the file's presence was confirmed by hand in a simulator
# build once, at the time it was added, and never again. Apple rejects at
# submission, which is the most expensive place to find out.
#
# Usage: assert_privacy_manifest.sh <path-to-.app-or-.ipa>
#
# macOS only (plutil). Runs on the CI runners that build the app.
set -euo pipefail

target="${1:-}"
if [ -z "$target" ] || [ ! -e "$target" ]; then
  echo "usage: $0 <path-to-.app-or-.ipa>" >&2
  exit 2
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

case "$target" in
  *.ipa)
    # The IPA is the artifact Apple receives; read the manifest out of its
    # Payload rather than trusting that the export preserved the archive's.
    unzip -o -q "$target" 'Payload/*/PrivacyInfo.xcprivacy' -d "$workdir" || true
    manifest="$(find "$workdir" -name PrivacyInfo.xcprivacy -print -quit)"
    ;;
  *)
    manifest="$target/PrivacyInfo.xcprivacy"
    [ -f "$manifest" ] || manifest=""
    ;;
esac

if [ -z "$manifest" ]; then
  echo "FAIL: $target contains no PrivacyInfo.xcprivacy." >&2
  echo "      Check the resources wiring in ios/project.yml (CruiseMesh/PrivacyInfo.xcprivacy)." >&2
  exit 1
fi

plutil -convert json -o "$workdir/manifest.json" "$manifest"

python3 - "$workdir/manifest.json" "$target" <<'PY'
import json
import sys

manifest = json.load(open(sys.argv[1]))
target = sys.argv[2]
problems = []

# These three are the repo-side source of truth for the App Store Connect App
# Privacy answers: "Data Not Collected", no tracking, no tracking domains.
if manifest.get("NSPrivacyTracking") is not False:
    problems.append(
        f"NSPrivacyTracking is {manifest.get('NSPrivacyTracking')!r}, expected false "
        "(the listing answers 'no tracking')"
    )
if manifest.get("NSPrivacyCollectedDataTypes") != []:
    problems.append(
        "NSPrivacyCollectedDataTypes is not empty, but the listing answers "
        "'Data Not Collected'"
    )
if manifest.get("NSPrivacyTrackingDomains") != []:
    problems.append("NSPrivacyTrackingDomains is not empty, but the app declares no tracking")

# Presence-only would accept a stub. Require the three APIs this app uses
# and the reason codes in ios/CruiseMesh/PrivacyInfo.xcprivacy.
REQUIRED_API_TYPES = {
    "NSPrivacyAccessedAPICategoryDiskSpace": {"E174.1"},
    "NSPrivacyAccessedAPICategoryFileTimestamp": {"C617.1", "3B52.1"},
    "NSPrivacyAccessedAPICategoryUserDefaults": {"CA92.1"},
}
declared = {}
for entry in manifest.get("NSPrivacyAccessedAPITypes") or []:
    api_type = entry.get("NSPrivacyAccessedAPIType")
    if api_type:
        declared[api_type] = set(entry.get("NSPrivacyAccessedAPITypeReasons") or [])
for api_type, reasons in REQUIRED_API_TYPES.items():
    if api_type not in declared:
        problems.append(f"{api_type} is missing")
    elif not reasons.issubset(declared[api_type]):
        missing = ", ".join(sorted(reasons - declared[api_type]))
        problems.append(f"{api_type} is missing reason(s): {missing}")

if problems:
    print(f"FAIL: privacy manifest in {target} does not match the declared answers:")
    for problem in problems:
        print(f"  - {problem}")
    sys.exit(1)

print(f"Privacy manifest present in {target}: no tracking, no collected data.")
print(f"Required-reason APIs declared: {', '.join(sorted(declared))}")
PY
