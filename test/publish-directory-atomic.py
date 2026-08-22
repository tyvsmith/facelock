#!/usr/bin/env python3
"""Atomically publish one prepared directory without replacing a destination."""

from __future__ import annotations

import ctypes
import errno
import os
from pathlib import Path
import sys


AT_FDCWD = -100
RENAME_NOREPLACE = 1


def fail(message: str) -> None:
    raise SystemExit(f"atomic directory publish: {message}")


def main() -> None:
    if len(sys.argv) != 3:
        fail("usage: publish-directory-atomic.py <prepared-dir> <absent-destination>")

    source = Path(sys.argv[1]).absolute()
    destination = Path(sys.argv[2]).absolute()
    if not source.is_dir() or source.is_symlink():
        fail(f"source is not a real directory: {source}")
    if source.parent.resolve() != destination.parent.resolve():
        fail("source and destination must have the same canonical parent")
    if source.stat().st_dev != destination.parent.stat().st_dev:
        fail("source and destination are not on the same filesystem")

    libc = ctypes.CDLL(None, use_errno=True)
    try:
        renameat2 = libc.renameat2
    except AttributeError:
        fail("libc does not expose renameat2; refusing a non-atomic fallback")
    renameat2.argtypes = [
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_int,
        ctypes.c_char_p,
        ctypes.c_uint,
    ]
    renameat2.restype = ctypes.c_int
    result = renameat2(
        AT_FDCWD,
        os.fsencode(source),
        AT_FDCWD,
        os.fsencode(destination),
        RENAME_NOREPLACE,
    )
    if result == 0:
        return

    error = ctypes.get_errno()
    if error == errno.EEXIST:
        fail(f"destination appeared before publication: {destination}")
    if error in (errno.ENOSYS, errno.EINVAL):
        fail("renameat2(RENAME_NOREPLACE) is unavailable; refusing a non-atomic fallback")
    fail(f"renameat2 failed: {os.strerror(error)}")


if __name__ == "__main__":
    main()
