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
import json
import sys
import time

import requests
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding

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
    print(f"Uploading AAB ({len(aab)} bytes)...")
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
    body = {
        "track": TRACK,
        "releases": [
            {
                "versionCodes": [str(version_code)],
                "status": "completed",
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
    print(f"Assigned versionCode {version_code} to '{TRACK}' (status completed).")

    resp = requests.post(f"{BASE}/{PACKAGE}/edits/{edit_id}:commit", headers=headers, timeout=60)
    if resp.status_code >= 300:
        die("commit", resp)
    print(f"COMMITTED edit {edit_id}: build {version_code} is live on the {TRACK} track.")


if __name__ == "__main__":
    main()
