#!/usr/bin/env python3
"""Generate the English Xcode string catalog from static SwiftUI keys."""

from __future__ import annotations

import json
import pathlib
import re


ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE_ROOT = ROOT / "ios/CruiseMesh"
CATALOG = SOURCE_ROOT / "Localizable.xcstrings"
CALLS = r"Text|Button|Label|TextField|SecureField|Section|LabeledContent"
PATTERNS = [
    re.compile(rf"(?<![A-Za-z0-9_])(?:{CALLS})\(\s*\"((?:[^\"\\]|\\.)*)\""),
    re.compile(r"\.(?:navigationTitle|accessibilityLabel|alert)\(\s*\"((?:[^\"\\]|\\.)*)\""),
]


def decode_swift(raw: str) -> str:
    return (
        raw.replace(r"\n", "\n")
        .replace(r"\t", "\t")
        .replace(r'\"', '"')
        .replace(r"\\", "\\")
    )


def keys() -> list[str]:
    result = {"CruiseMesh"}
    for path in SOURCE_ROOT.rglob("*.swift"):
        if "Generated" in path.parts:
            continue
        source = path.read_text(encoding="utf-8")
        for pattern in PATTERNS:
            for match in pattern.finditer(source):
                value = decode_swift(match.group(1))
                if r"\(" not in value:
                    result.add(value)
    return sorted(result)


def main() -> None:
    strings = {
        key: {
            "localizations": {
                "en": {"stringUnit": {"state": "translated", "value": key}}
            }
        }
        for key in keys()
    }
    payload = {"sourceLanguage": "en", "strings": strings, "version": "1.0"}
    CATALOG.write_text(
        json.dumps(payload, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
        newline="\n",
    )
    print(f"Wrote {len(strings)} iOS localization keys to {CATALOG}")


if __name__ == "__main__":
    main()
