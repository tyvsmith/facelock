#!/usr/bin/env python3
"""Prove Packit's configured hooks resolve local non-archive RPM sources."""

from __future__ import annotations

import json
import re
import shutil
import subprocess
import tempfile
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
CONFIG = ROOT / ".packit.yaml"
SPEC = ROOT / "dist" / "facelock.spec"
CANONICAL_DIR = ROOT / "dist" / "rpm"


def fail(message: str) -> None:
    raise SystemExit(f"FAIL: {message}")


def post_modification_actions() -> list[str]:
    try:
        config = json.loads(CONFIG.read_text())
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read Packit config as JSON-subset YAML: {error}")
    actions = config.get("actions", {}).get("post-modifications", [])
    if isinstance(actions, str):
        actions = [actions]
    if not isinstance(actions, list) or not all(isinstance(action, str) for action in actions):
        fail("Packit post-modifications must be a string or list of strings")
    return actions


def local_extra_sources(spec: str) -> list[str]:
    sources = []
    for match in re.finditer(r"(?m)^Source([1-9][0-9]*):\s*(\S+)\s*$", spec):
        source = match.group(2)
        if "/" in source or "%" in source:
            fail(f"Source{match.group(1)} must remain a plain local basename: {source}")
        sources.append(source)
    if not sources:
        fail("RPM spec has no local Source1+ payload to validate")
    return sources


sources = local_extra_sources(SPEC.read_text())
for source in sources:
    canonical = CANONICAL_DIR / source
    staged = SPEC.parent / source
    if not canonical.is_file():
        fail(f"missing canonical RPM source: {canonical.relative_to(ROOT)}")
    if staged.exists() or staged.is_symlink():
        fail(f"stale staged RPM source can diverge from canonical file: {staged.relative_to(ROOT)}")

with tempfile.TemporaryDirectory(prefix="facelock-packit-sources-") as tmp:
    checkout = Path(tmp) / "repo"
    (checkout / "dist" / "rpm").mkdir(parents=True)
    shutil.copy2(CONFIG, checkout / ".packit.yaml")
    shutil.copy2(SPEC, checkout / "dist" / "facelock.spec")
    for source in sources:
        shutil.copy2(CANONICAL_DIR / source, checkout / "dist" / "rpm" / source)

    for action in post_modification_actions():
        subprocess.run(["bash", "-euo", "pipefail", "-c", action], cwd=checkout, check=True)

    for source in sources:
        canonical = checkout / "dist" / "rpm" / source
        staged = checkout / "dist" / source
        if not staged.is_file():
            fail(f"Packit post-modifications did not resolve dist/{source}")
        if staged.read_bytes() != canonical.read_bytes():
            fail(f"Packit staged dist/{source} differs from canonical dist/rpm/{source}")

print("Packit SRPM source contract: OK")
