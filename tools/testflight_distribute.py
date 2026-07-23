#!/usr/bin/env python3
"""Last-mile TestFlight distribution: after an altool upload, wait for the build
to finish processing, add it to an external beta group, and submit it for beta
review. `altool --upload-app` gets the binary to App Store Connect but does NOT
make it visible to an external group's testers -- that needs the group + review
steps below.

Best-effort by design: it logs outcomes and never raises to fail the release
(the caller runs it continue-on-error), because the upload -- the hard part --
has already succeeded by the time this runs. A slow processing queue or a
transient API hiccup should not red a release whose build is safely on
TestFlight.

Usage: testflight_distribute.py <key.p8> <key_id> <issuer_id> <build_number> [group_name]

Auth uses PyJWT's ES256 (App Store Connect keys are ECDSA P-256); PyJWT handles
the JOSE raw-signature encoding that a hand-rolled cryptography signer would get
wrong.
"""
import json
import sys
import time

import jwt
import requests

KEY_PATH = sys.argv[1]
KEY_ID = sys.argv[2]
ISSUER_ID = sys.argv[3]
BUILD_NUMBER = sys.argv[4]
GROUP_NAME = sys.argv[5] if len(sys.argv) > 5 else "Family Cruise"

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
        print("WARN: build never became visible; it's uploaded — distribute manually if needed.")
        return
    state = build["attributes"]["processingState"]
    if state != "VALID":
        print(f"WARN: processingState={state}, not VALID; skipping distribution.")
        return
    build_id = build["id"]

    app_id = api("GET", f"/v1/apps?filter[bundleId]={BUNDLE_ID}").json()["data"][0]["id"]
    groups = [
        g
        for g in api("GET", f"/v1/betaGroups?filter[app]={app_id}&limit=200").json()["data"]
        if g["attributes"]["name"] == GROUP_NAME
    ]
    if not groups:
        print(f"WARN: beta group '{GROUP_NAME}' not found; skipping distribution.")
        return
    group_id = groups[0]["id"]

    resp = api(
        "POST",
        f"/v1/betaGroups/{group_id}/relationships/builds",
        {"data": [{"type": "builds", "id": build_id}]},
    )
    print(f"add to '{GROUP_NAME}': HTTP {resp.status_code}")

    resp = api(
        "POST",
        "/v1/betaAppReviewSubmissions",
        {"data": {"type": "betaAppReviewSubmissions", "relationships": {"build": {"data": {"type": "builds", "id": build_id}}}}},
    )
    # A build whose version was already beta-approved re-submits cleanly / is a
    # no-op; only log the body when it's an unexpected error.
    if resp.status_code >= 300 and "already" not in resp.text.lower():
        print(f"beta review submit: HTTP {resp.status_code}: {resp.text[:300]}")
    else:
        print(f"beta review submit: HTTP {resp.status_code}")

    detail = (api("GET", f"/v1/builds/{build_id}/buildBetaDetail").json().get("data") or {}).get("attributes", {})
    print(f"DONE: internal={detail.get('internalBuildState')} external={detail.get('externalBuildState')}")


if __name__ == "__main__":
    main()
