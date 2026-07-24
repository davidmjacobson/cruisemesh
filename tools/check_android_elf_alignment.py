#!/usr/bin/env python3
"""Fail when an Android artifact contains a 64-bit ELF with sub-16 KiB LOAD alignment."""

from __future__ import annotations

import argparse
import struct
import sys
import zipfile
from pathlib import Path

ELF_MAGIC = b"\x7fELF"
ELFCLASS64 = 2
ELFDATA2LSB = 1
ELFDATA2MSB = 2
PT_LOAD = 1
MIN_LOAD_ALIGNMENT = 16 * 1024


class ElfFormatError(ValueError):
    """Raised when a packaged native library is not a readable ELF file."""


def load_alignments(data: bytes) -> list[int] | None:
    """Return ELF64 PT_LOAD alignments, or None for a valid non-64-bit ELF."""
    if len(data) < 64 or data[:4] != ELF_MAGIC:
        raise ElfFormatError("missing or truncated ELF header")

    elf_class = data[4]
    if elf_class != ELFCLASS64:
        return None

    encoding = data[5]
    if encoding == ELFDATA2LSB:
        endian = "<"
    elif encoding == ELFDATA2MSB:
        endian = ">"
    else:
        raise ElfFormatError(f"unsupported ELF byte order {encoding}")

    program_offset = struct.unpack_from(f"{endian}Q", data, 0x20)[0]
    program_entry_size = struct.unpack_from(f"{endian}H", data, 0x36)[0]
    program_entry_count = struct.unpack_from(f"{endian}H", data, 0x38)[0]

    if program_entry_size < 56:
        raise ElfFormatError(f"invalid ELF64 program-header size {program_entry_size}")
    if program_offset + program_entry_size * program_entry_count > len(data):
        raise ElfFormatError("program headers extend past the end of the file")

    alignments: list[int] = []
    for index in range(program_entry_count):
        offset = program_offset + index * program_entry_size
        program_type = struct.unpack_from(f"{endian}I", data, offset)[0]
        if program_type == PT_LOAD:
            alignments.append(struct.unpack_from(f"{endian}Q", data, offset + 0x30)[0])

    if not alignments:
        raise ElfFormatError("ELF64 file contains no PT_LOAD segments")
    return alignments


def native_entries(archive: zipfile.ZipFile) -> list[zipfile.ZipInfo]:
    return sorted(
        (
            entry
            for entry in archive.infolist()
            if not entry.is_dir()
            and entry.filename.endswith(".so")
            and (entry.filename.startswith("lib/") or "/lib/" in entry.filename)
        ),
        key=lambda entry: entry.filename,
    )


def check_artifact(path: Path) -> int:
    failures: list[str] = []
    checked = 0

    try:
        with zipfile.ZipFile(path) as archive:
            entries = native_entries(archive)
            if not entries:
                print(f"ERROR: {path} contains no packaged native libraries", file=sys.stderr)
                return 1

            for entry in entries:
                try:
                    alignments = load_alignments(archive.read(entry))
                except ElfFormatError as error:
                    failures.append(f"{entry.filename}: {error}")
                    continue

                if alignments is None:
                    continue

                checked += 1
                minimum = min(alignments)
                status = "PASS" if minimum >= MIN_LOAD_ALIGNMENT else "FAIL"
                print(f"{status} {entry.filename}: minimum PT_LOAD alignment {minimum} bytes")
                if minimum < MIN_LOAD_ALIGNMENT:
                    failures.append(
                        f"{entry.filename}: {minimum}-byte PT_LOAD alignment is below "
                        f"{MIN_LOAD_ALIGNMENT} bytes"
                    )
    except (OSError, zipfile.BadZipFile) as error:
        print(f"ERROR: cannot inspect {path}: {error}", file=sys.stderr)
        return 1

    if checked == 0:
        failures.append("artifact contains no 64-bit ELF libraries")

    if failures:
        print("\n16 KiB native-library compatibility check failed:", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1

    print(f"\nChecked {checked} 64-bit native libraries; all support 16 KiB pages.")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Check every packaged 64-bit ELF PT_LOAD segment in an Android APK "
            "or AAB for at least 16 KiB alignment."
        )
    )
    parser.add_argument("artifact", type=Path, help="Path to an Android APK or AAB")
    args = parser.parse_args()

    if not args.artifact.is_file():
        parser.error(f"artifact does not exist: {args.artifact}")
    return check_artifact(args.artifact)


if __name__ == "__main__":
    raise SystemExit(main())
