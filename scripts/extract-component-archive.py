#!/usr/bin/env python3
"""Safely extract one normalized .tar.xz source-component tree."""

from __future__ import annotations

import os
import shutil
import sys
import tarfile
from pathlib import Path


def fail(message: str) -> "NoReturn":
    raise SystemExit(f"component archive: {message}")


def main() -> None:
    if len(sys.argv) != 3:
        fail(f"usage: {sys.argv[0]} ARCHIVE_PATH NEW_COMPONENT_DIR")

    archive = Path(sys.argv[1])
    destination_argument = Path(sys.argv[2])
    if archive.is_symlink() or not archive.is_file():
        fail(f"archive is not a regular file: {archive}")
    if destination_argument.name in {"", ".", ".."}:
        fail(f"unsafe component destination: {destination_argument}")
    destination_parent = destination_argument.parent.resolve(strict=True)
    if not destination_parent.is_dir():
        fail(f"component destination parent is not a directory: {destination_parent}")
    destination = destination_parent / destination_argument.name
    if destination.exists() or destination.is_symlink():
        fail(f"component destination already exists: {destination}")

    expected_root = destination.name
    with tarfile.open(archive, mode="r:xz") as component_archive:
        members = component_archive.getmembers()
        if not members:
            fail("archive is empty")

        by_name: dict[str, tarfile.TarInfo] = {}
        directories: set[str] = set()
        files: set[str] = set()
        for member in members:
            name = member.name[:-1] if member.isdir() and member.name.endswith("/") else member.name
            parts = name.split("/")
            if (
                not name
                or name.startswith("/")
                or any(part in {"", ".", ".."} for part in parts)
                or parts[0] != expected_root
            ):
                fail(f"unsafe or unexpected archive member: {member.name}")
            if name in by_name:
                fail(f"duplicate archive member: {name}")
            if member.isdir():
                if member.mode != 0o755:
                    fail(f"directory mode is not 0755: {name}")
                directories.add(name)
            elif member.isreg():
                if member.mode not in {0o644, 0o755}:
                    fail(f"file mode is not normalized: {name}")
                files.add(name)
            else:
                fail(f"archive member is not a regular file or directory: {name}")
            by_name[name] = member

        if expected_root not in directories:
            fail(f"archive lacks its exact component root: {expected_root}")
        for name in directories | files:
            parts = name.split("/")
            for end in range(1, len(parts)):
                parent = "/".join(parts[:end])
                if parent not in directories:
                    fail(f"archive member has an undeclared directory parent: {name}")

        destination_created = False
        try:
            for name in sorted(directories, key=lambda value: (value.count("/"), value)):
                path = destination_parent / name
                path.mkdir()
                os.chmod(path, by_name[name].mode)
                if name == expected_root:
                    destination_created = True
            for name in sorted(files):
                member = by_name[name]
                source = component_archive.extractfile(member)
                if source is None:
                    fail(f"could not read regular archive member: {name}")
                path = destination_parent / name
                with source, path.open("xb") as output:
                    shutil.copyfileobj(source, output)
                os.chmod(path, member.mode)
        except BaseException:
            if destination_created:
                shutil.rmtree(destination)
            raise


if __name__ == "__main__":
    main()
