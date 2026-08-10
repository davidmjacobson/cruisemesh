#!/usr/bin/env python3
"""Keep the Android and iOS accepted Terms versions in lockstep."""

from __future__ import annotations

import datetime
import pathlib
import re
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCES = {
    "Android": (
        ROOT
        / "android/app/src/main/kotlin/com/cruisemesh/app/identity/TermsAcceptanceStore.kt",
        re.compile(r'^const val CURRENT_TERMS_VERSION = "([^"]+)"$', re.MULTILINE),
    ),
    "iOS": (
        ROOT / "ios/CruiseMesh/Core/TermsAcceptanceStore.swift",
        re.compile(r'^\s*static let currentVersion = "([^"]+)"$', re.MULTILINE),
    ),
}


def main() -> int:
    versions: dict[str, str] = {}
    errors: list[str] = []

    for platform, (path, pattern) in SOURCES.items():
        matches = pattern.findall(path.read_text(encoding="utf-8"))
        if len(matches) != 1:
            errors.append(
                f"{path.relative_to(ROOT)}: expected exactly one Terms version, "
                f"found {len(matches)}"
            )
            continue
        version = matches[0]
        try:
            datetime.date.fromisoformat(version)
        except ValueError:
            errors.append(
                f"{path.relative_to(ROOT)}: Terms version must be an ISO date, got {version!r}"
            )
        versions[platform] = version

    if len(set(versions.values())) > 1:
        rendered = ", ".join(f"{platform}={version}" for platform, version in versions.items())
        errors.append(f"Terms versions do not match: {rendered}")

    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1

    version = next(iter(versions.values()), "unknown")
    print(f"Android and iOS Terms versions match: {version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
