#!/usr/bin/env python3
"""Last-mile TestFlight distribution: after an altool upload, wait for the build
to finish processing, add it to an external beta group, and submit it for beta
review. `altool --upload-app` gets the binary to App Store Connect but does NOT
make it visible to an external group's testers -- that needs the group + review
steps below.

Exit code is the verdict: 0 means the build is in the beta group and
review-submit did not fail. Anything else means testers may not see it.
This used to return 0 on every giving-up path -- build never visible,
still processing, group not found -- which made the caller write
"Distributed build ... to the beta group" to the run summary for a build
no tester could see. 1.0.4 shipped to TestFlight and reached nobody
exactly that way. The release job keeps `continue-on-error` on this
step, so a slow processing queue still does not red a landed release; it
just stops claiming a distribution that did not happen.

Usage: testflight_distribute.py <key.p8> <key_id> <issuer_id> <build_number> [group_name]

Set EXPECT_MARKETING_VERSION to have the build's own marketing version checked
before distributing -- the build number alone does not prove App Store Connect
matched the version this release meant to ship.

Auth uses PyJWT's ES256 (App Store Connect keys are ECDSA P-256); PyJWT handles
the JOSE raw-signature encoding that a hand-rolled cryptography signer would get
wrong.
"""
import json
import os
import sys
import time
from typing import NoReturn

import jwt
import requests

KEY_PATH = sys.argv[1]
KEY_ID = sys.argv[2]
ISSUER_ID = sys.argv[3]
BUILD_NUMBER = sys.argv[4]
GROUP_NAME = sys.argv[5] if len(sys.argv) > 5 else "Family Cruise"
EXPECT_MARKETING_VERSION = os.environ.get("EXPECT_MARKETING_VERSION", "").strip()

BUNDLE_ID = "com.cruisemesh.app"
API = "https://api.appstoreconnect.apple.com"
POLL_SECONDS = 30
POLL_TRIES = 30  # ~15 minutes


def token() -> str:
    now = int(time.time())
    with open(KEY_PATH) as fh:
        key = fh.read()
    return jwt.encode(
        {"iss": ISSUER_ID, "iat": now - 10, "exp": now + 1100, "aud": "appstoreconnect-v1"},
        key,
        algorithm="ES256",
        headers={"kid": KEY_ID, "typ": "JWT"},
    )


def api(method: str, path: str, body: dict = None) -> requests.Response:
    return requests.request(
        method,
        API + path,
        headers={"Authorization": "Bearer " + token(), "Content-Type": "application/json"},
        data=json.dumps(body) if body else None,
        timeout=30,
    )


def die(message: str) -> NoReturn:
    """Stop with a nonzero exit so the caller's summary tells the truth."""
    print(f"FAIL: {message}")
    sys.exit(1)


def get_list(path: str, what: str) -> list:
    """GET a collection, refusing to treat an API error as 'no results'.

    Every caller below used to index `["data"][0]` straight off the response,
    which turns an auth failure or a 500 into a KeyError traceback rather than a
    legible verdict -- and an empty list into an IndexError.
    """
    resp = api("GET", path)
    if resp.status_code >= 300:
        die(f"{what} query HTTP {resp.status_code}: {resp.text[:300]}")
    try:
        data = resp.json().get("data")
    except ValueError:
        die(f"{what} query returned non-JSON: {resp.text[:300]}")
    if not isinstance(data, list):
        die(f"{what} query returned no data list: {resp.text[:300]}")
    return data


def main() -> None:
    build = None
    for i in range(POLL_TRIES):
        resp = api("GET", f"/v1/builds?filter[version]={BUILD_NUMBER}&limit=1")
        if resp.status_code >= 300:
            print(f"[{i}] builds query HTTP {resp.status_code}: {resp.text[:200]}")
        else:
            data = resp.json().get("data") or []
            if data:
                build = data[0]
                state = build["attributes"]["processingState"]
                print(f"[{i}] build {BUILD_NUMBER}: {state}")
                if state != "PROCESSING":
                    break
            else:
                print(f"[{i}] build {BUILD_NUMBER} not visible yet")
        time.sleep(POLL_SECONDS)

    if not build:
        die(
            f"build {BUILD_NUMBER} never became visible after "
            f"{POLL_TRIES * POLL_SECONDS // 60} minutes; it is uploaded but with no tester. "
            "Re-run this script once App Store Connect shows the build."
        )
    state = build["attributes"]["processingState"]
    if state != "VALID":
        die(f"processingState={state}, not VALID; nothing was distributed.")
    build_id = build["id"]

    # The build number alone can match a build App Store Connect assembled from
    # a different marketing version; check the train before handing it to
    # testers.
    if EXPECT_MARKETING_VERSION:
        pre = api("GET", f"/v1/builds/{build_id}/preReleaseVersion")
        if pre.status_code >= 300:
            die(f"preReleaseVersion query HTTP {pre.status_code}: {pre.text[:300]}")
        actual = ((pre.json().get("data") or {}).get("attributes") or {}).get("version")
        if actual != EXPECT_MARKETING_VERSION:
            die(
                f"build {BUILD_NUMBER} reports marketing version {actual!r}, "
                f"expected {EXPECT_MARKETING_VERSION!r}; refusing to distribute."
            )
        print(f"marketing version {actual} matches the release.")

    apps = get_list(f"/v1/apps?filter[bundleId]={BUNDLE_ID}", "apps")
    if not apps:
        die(f"no app with bundle id {BUNDLE_ID} is visible to this API key.")
    app_id = apps[0]["id"]
    groups = [
        g
        for g in get_list(f"/v1/betaGroups?filter[app]={app_id}&limit=200", "beta groups")
        if g["attributes"]["name"] == GROUP_NAME
    ]
    if not groups:
        die(f"beta group '{GROUP_NAME}' not found; nothing was distributed.")
    group_id = groups[0]["id"]

    resp = api(
        "POST",
        f"/v1/betaGroups/{group_id}/relationships/builds",
        {"data": [{"type": "builds", "id": build_id}]},
    )
    print(f"add to '{GROUP_NAME}': HTTP {resp.status_code}")
    if resp.status_code >= 300:
        # 409 on a re-run is expected once the build is already in the group.
        # Membership GET below is the verdict.
        print(f"add returned {resp.status_code}: {resp.text[:300]}")

    resp = api(
        "POST",
        "/v1/betaAppReviewSubmissions",
        {"data": {"type": "betaAppReviewSubmissions", "relationships": {"build": {"data": {"type": "builds", "id": build_id}}}}},
    )
    # A build whose version was already beta-approved re-submits cleanly.
    if resp.status_code >= 300 and "already" not in resp.text.lower():
        die(f"beta review submit HTTP {resp.status_code}: {resp.text[:300]}")
    print(f"beta review submit: HTTP {resp.status_code}")

    # Read the membership back rather than trusting the POST: this is the exact
    # fact the run summary claims, so assert it against App Store Connect.
    member_of = [
        g["attributes"]["name"]
        for g in get_list(f"/v1/builds/{build_id}/betaGroups?limit=200", "build beta groups")
    ]
    if GROUP_NAME not in member_of:
        die(
            f"'{GROUP_NAME}' is not among the build's beta groups after the add "
            f"(saw: {member_of or 'none'})."
        )

    detail = (api("GET", f"/v1/builds/{build_id}/buildBetaDetail").json().get("data") or {}).get("attributes", {})
    external = detail.get("externalBuildState")
    if external == "MISSING_EXPORT_COMPLIANCE":
        die(
            f"build {BUILD_NUMBER} is in '{GROUP_NAME}' but externalBuildState="
            f"{external}; testers cannot install."
        )
    print(f"DONE: internal={detail.get('internalBuildState')} external={external}")
    print(f"Build {BUILD_NUMBER} is in '{GROUP_NAME}'.")


if __name__ == "__main__":
    main()
