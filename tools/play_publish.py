#!/usr/bin/env python3
"""Publish a signed AAB to a Google Play testing track via the Play Developer
API, authenticating with a service-account JSON key.

Usage: play_publish.py <service_account.json> <app.aab> [version_name] [track]

Self-contained on purpose: signs the OAuth2 JWT with `cryptography` and talks
REST with `requests`, so CI only needs those two wheels (no google-api-python
stack, which lags new Python releases). Never prints secrets. Exits non-zero
with the API response body on any failure so the CI step fails loudly.
"""
import base64
import hashlib
import json
import os
import sys
import time

import requests
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding

# Import by explicit path so the script works from any working directory.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from provenance import record  # noqa: E402

KEY_PATH = sys.argv[1]
AAB_PATH = sys.argv[2]
VERSION_NAME = sys.argv[3] if len(sys.argv) > 3 else ""
TRACK = sys.argv[4] if len(sys.argv) > 4 else "internal"

PACKAGE = "com.cruisemesh.app"
SCOPE = "https://www.googleapis.com/auth/androidpublisher"
BASE = "https://androidpublisher.googleapis.com/androidpublisher/v3/applications"
UPLOAD = "https://androidpublisher.googleapis.com/upload/androidpublisher/v3/applications"


def b64url(raw: bytes) -> bytes:
    return base64.urlsafe_b64encode(raw).rstrip(b"=")


def access_token(sa: dict) -> str:
    now = int(time.time())
    header = {"alg": "RS256", "typ": "JWT"}
    claims = {
        "iss": sa["client_email"],
        "scope": SCOPE,
        "aud": sa["token_uri"],
        "iat": now,
        "exp": now + 3600,
    }
    signing_input = b64url(json.dumps(header).encode()) + b"." + b64url(json.dumps(claims).encode())
    key = serialization.load_pem_private_key(sa["private_key"].encode(), password=None)
    signature = key.sign(signing_input, padding.PKCS1v15(), hashes.SHA256())
    assertion = (signing_input + b"." + b64url(signature)).decode()
    resp = requests.post(
        sa["token_uri"],
        data={"grant_type": "urn:ietf:params:oauth:grant-type:jwt-bearer", "assertion": assertion},
        timeout=30,
    )
    resp.raise_for_status()
    return resp.json()["access_token"]


def die(step: str, resp: requests.Response) -> None:
    print(f"FAILED at {step}: HTTP {resp.status_code}\n{resp.text[:2000]}")
    sys.exit(1)


def main() -> None:
    with open(KEY_PATH) as fh:
        sa = json.load(fh)
    print(f"Auth as {sa['client_email']} (project {sa['project_id']})")
    headers = {"Authorization": f"Bearer {access_token(sa)}"}
    print("OAuth token acquired.")

    resp = requests.post(f"{BASE}/{PACKAGE}/edits", headers=headers, timeout=30)
    if resp.status_code >= 300:
        die("create edit", resp)
    edit_id = resp.json()["id"]
    print(f"Edit created: {edit_id}")

    with open(AAB_PATH, "rb") as fh:
        aab = fh.read()
    aab_sha256 = hashlib.sha256(aab).hexdigest()
    print(f"Uploading AAB ({len(aab)} bytes, sha256 {aab_sha256})...")
    resp = requests.post(
        f"{UPLOAD}/{PACKAGE}/edits/{edit_id}/bundles?uploadType=media",
        headers={**headers, "Content-Type": "application/octet-stream"},
        data=aab,
        timeout=600,
    )
    if resp.status_code >= 300:
        die("upload bundle", resp)
    version_code = resp.json()["versionCode"]
    print(f"Bundle accepted: versionCode {version_code}")

    note = f"Automated release {VERSION_NAME}".strip() or "Automated release."

    def assign_track(status: str) -> None:
        body = {
            "track": TRACK,
            "releases": [
                {
                    "versionCodes": [str(version_code)],
                    "status": status,
                    "releaseNotes": [{"language": "en-US", "text": note}],
                }
            ],
        }
        resp = requests.put(
            f"{BASE}/{PACKAGE}/edits/{edit_id}/tracks/{TRACK}",
            headers={**headers, "Content-Type": "application/json"},
            data=json.dumps(body),
            timeout=30,
        )
        if resp.status_code >= 300:
            die("assign track", resp)
        print(f"Assigned versionCode {version_code} to '{TRACK}' (status {status}).")

    def commit() -> requests.Response:
        return requests.post(f"{BASE}/{PACKAGE}/edits/{edit_id}:commit", headers=headers, timeout=60)

    def record_publish(release_status: str) -> None:
        """Put the identifiers this run just minted somewhere durable."""
        record(
            "Play upload",
            [
                ("package", PACKAGE),
                ("track", TRACK),
                ("version_name", VERSION_NAME or "(unset)"),
                ("version_code", version_code),
                ("edit_id", edit_id),
                ("release_status", release_status),
                ("aab_bytes", len(aab)),
                ("aab_sha256", aab_sha256),
            ],
        )

    assign_track("completed")
    resp = commit()
    if resp.status_code == 400 and "draft app" in resp.text:
        # An app that has never passed its first Google review only accepts
        # draft releases on reviewed tracks ("Only releases with status draft
        # may be created on draft app") — the internal track is exempt, which
        # is why earlier internal publishes never hit this. A failed commit
        # leaves the edit open, so stage the same bundle as a draft instead of
        # failing the run: the first rollout must come from the Play Console
        # (it starts the app's initial review), and once the app is published
        # anywhere the completed path works again and this branch goes dormant.
        print("App is still unpublished (draft app); staging a DRAFT release instead.")
        assign_track("draft")
        resp = commit()
        if resp.status_code >= 300:
            die("commit draft release", resp)
        print(
            f"COMMITTED edit {edit_id}: build {version_code} STAGED AS DRAFT on the "
            f"'{TRACK}' track. Roll it out in the Play Console to start the app's "
            "first review; subsequent releases will publish automatically."
        )
        record_publish("draft")
        return
    if resp.status_code >= 300:
        die("commit", resp)
    print(f"COMMITTED edit {edit_id}: build {version_code} is live on the {TRACK} track.")
    record_publish("completed")


if __name__ == "__main__":
    main()
