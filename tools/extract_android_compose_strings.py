#!/usr/bin/env python3
"""Extract static Compose Text("...") literals into Android string resources.

This intentionally handles only plain literals. Interpolated and conditional
copy needs a human-selected formatted string or plural resource.
"""

from __future__ import annotations

import hashlib
import html
import pathlib
import re
import unicodedata


ROOT = pathlib.Path(__file__).resolve().parents[1]
SOURCE_ROOT = ROOT / "android/app/src/main/kotlin"
STRINGS_XML = ROOT / "android/app/src/main/res/values/strings.xml"
PATTERN = re.compile(
    r"(?<![A-Za-z0-9_])Text\(\s*(?P<named>text\s*=\s*)?"
    r'"(?P<value>(?:[^"\\]|\\.)*)"',
    re.DOTALL,
)


def kotlin_value(raw: str) -> str:
    return (
        raw.replace(r"\n", "\n")
        .replace(r"\t", "\t")
        .replace(r'\"', '"')
        .replace(r"\\", "\\")
    )


def resource_key(value: str, used: dict[str, str]) -> str:
    if value == "CruiseMesh":
        return "app_name"
    normalized = unicodedata.normalize("NFKD", value).encode("ascii", "ignore").decode()
    words = re.findall(r"[a-z0-9]+", normalized.lower())
    stem = "_".join(words[:8])[:64].strip("_") or "text"
    candidate = f"ui_{stem}"
    if candidate in used and used[candidate] != value:
        digest = hashlib.sha1(value.encode()).hexdigest()[:8]
        candidate = f"{candidate[:55]}_{digest}"
    return candidate


def xml_value(value: str) -> str:
    escaped = html.escape(value, quote=False)
    # aapt treats apostrophes and quotes specially even though XML does not.
    return escaped.replace("'", r"\'").replace('"', r'\"')


def add_imports(source: str) -> str:
    additions = []
    if "import androidx.compose.ui.res.stringResource" not in source:
        additions.append("import androidx.compose.ui.res.stringResource")
    if "import com.cruisemesh.app.R" not in source:
        additions.append("import com.cruisemesh.app.R")
    if not additions:
        return source
    imports = list(re.finditer(r"^import .+$", source, re.MULTILINE))
    if not imports:
        raise RuntimeError("Kotlin file has no import block")
    offset = imports[-1].end()
    return source[:offset] + "\n" + "\n".join(additions) + source[offset:]


def main() -> None:
    values: dict[str, str] = {"app_name": "CruiseMesh"}
    changed_files = 0
    replacements = 0

    for path in sorted(SOURCE_ROOT.rglob("*.kt")):
        source = path.read_text(encoding="utf-8")
        touched = False

        def replace(match: re.Match[str]) -> str:
            nonlocal touched, replacements
            raw = match.group("value")
            if "$" in raw:
                return match.group(0)
            value = kotlin_value(raw)
            key = resource_key(value, values)
            values[key] = value
            named = match.group("named") or ""
            touched = True
            replacements += 1
            return f"Text({named}stringResource(R.string.{key})"

        rewritten = PATTERN.sub(replace, source)
        if touched:
            rewritten = add_imports(rewritten)
            path.write_text(rewritten, encoding="utf-8", newline="\n")
            changed_files += 1

    lines = ["<resources>"]
    for key, value in sorted(values.items()):
        lines.append(f'    <string name="{key}">{xml_value(value)}</string>')
    lines.append("</resources>")
    STRINGS_XML.write_text("\n".join(lines) + "\n", encoding="utf-8", newline="\n")
    print(f"Extracted {replacements} literals from {changed_files} Kotlin files")


if __name__ == "__main__":
    main()
