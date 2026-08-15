#!/usr/bin/env python3
"""Best-effort release provenance recording for the publish scripts.

The Play and TestFlight publish scripts learn facts nothing else in the run
knows -- the version code Play assigned, the build id App Store Connect matched,
the tester group the build actually landed in -- and until now they only printed
them into the job log, where they are lost as soon as the log rotates. `record()`
prints them exactly as before AND, when the environment points at them, appends
them to the CI run summary and to a provenance file the workflow uploads next to
the release.

Everything here is best effort on purpose: a release must never fail because a
note about it could not be written, so writes swallow their own errors.

Environment:
  GITHUB_STEP_SUMMARY         markdown file rendered on the run page (set by CI)
  CRUISEMESH_PROVENANCE_FILE  plain-text file the release workflow uploads
"""
from __future__ import annotations

import os
import sys
from typing import Iterable, Tuple

Fields = Iterable[Tuple[str, object]]


def record(title: str, fields: Fields) -> None:
    """Print `title` + `fields` to stdout, the run summary and the provenance file."""
    pairs = [(str(k), "" if v is None else str(v)) for k, v in fields]
    for key, value in pairs:
        print(f"{title}: {key}={value}")
    _append(
        os.environ.get("GITHUB_STEP_SUMMARY"),
        [f"### {title}", ""] + [f"- **{k}**: `{v}`" for k, v in pairs] + [""],
    )
    _append(
        os.environ.get("CRUISEMESH_PROVENANCE_FILE"),
        [f"# {title}"] + [f"{k}: {v}" for k, v in pairs] + [""],
    )


def _append(path: str | None, lines: list) -> None:
    if not path:
        return
    try:
        with open(path, "a", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")
    except OSError as exc:  # pragma: no cover - never worth failing a release
        print(f"(provenance: could not write {path}: {exc})", file=sys.stderr)
