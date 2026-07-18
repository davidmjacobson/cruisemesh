#!/usr/bin/env python3
"""Fail when Compose bypasses resources or SwiftUI keys miss the catalog."""

from __future__ import annotations

import json
import pathlib
import re
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
ANDROID = ROOT / "android/app/src/main/kotlin"
IOS = ROOT / "ios/CruiseMesh"
CATALOG = IOS / "Localizable.xcstrings"
ANDROID_LITERAL = re.compile(
    r'(?<![A-Za-z0-9_])Text\(\s*(?:text\s*=\s*)?(?:"|if\s*\()'
)
SWIFT_CALLS = r"Text|Button|Label|TextField|SecureField|Section|LabeledContent"
SWIFT_PATTERNS = [
    re.compile(rf"(?<![A-Za-z0-9_])(?:{SWIFT_CALLS})\(\s*\"((?:[^\"\\]|\\.)*)\""),
    re.compile(r"\.(?:navigationTitle|accessibilityLabel|alert)\(\s*\"((?:[^\"\\]|\\.)*)\""),
]


def line_number(source: str, offset: int) -> int:
    return source.count("\n", 0, offset) + 1


def main() -> int:
    errors: list[str] = []
    for path in ANDROID.rglob("*.kt"):
        source = path.read_text(encoding="utf-8")
        for match in ANDROID_LITERAL.finditer(source):
            errors.append(
                f"{path.relative_to(ROOT)}:{line_number(source, match.start())}: "
                "Compose Text must use stringResource/pluralStringResource"
            )

    try:
        catalog_keys = set(json.loads(CATALOG.read_text(encoding="utf-8"))["strings"])
    except (FileNotFoundError, KeyError, json.JSONDecodeError) as exc:
        errors.append(f"invalid iOS string catalog: {exc}")
        catalog_keys = set()

    for path in IOS.rglob("*.swift"):
        if "Generated" in path.parts:
            continue
        source = path.read_text(encoding="utf-8")
        for pattern in SWIFT_PATTERNS:
            for match in pattern.finditer(source):
                raw = match.group(1)
                if r"\(" in raw:
                    continue
                value = raw.replace(r'\"', '"').replace(r"\\", "\\")
                if value not in catalog_keys:
                    errors.append(
                        f"{path.relative_to(ROOT)}:{line_number(source, match.start())}: "
                        f"SwiftUI localization key is missing from catalog: {value!r}"
                    )

    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print("UI localization check passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
