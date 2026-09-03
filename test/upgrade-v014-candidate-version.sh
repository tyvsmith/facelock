#!/usr/bin/env bash
# Decide which version the upgrade lanes build the candidate as.
#
# An upgrade lane needs a candidate that sorts strictly above the pinned
# predecessor, and on a development tree it does not have one: the workspace
# version stays at the last release until `just release` bumps it, so the .deb
# built from it is `0.1.4-1~deb13u1` — *below* the published `0.1.4-1`, and the
# lane would be testing a downgrade.
#
# So the version is chosen, not assumed. When the workspace version already
# sorts above the predecessor the candidate is built exactly as it ships; when
# it does not, the lane builds the same payload as the upgrade-test version
# below and says so. This mirrors what the retired-authselect fixture already
# does (test/build-rpm-authselect-fixtures.sh builds its candidate at 0.2.0),
# and the native comparator inside the container is still the authority — a
# wrong answer here fails the lane rather than passing quietly.
#
# Usage: upgrade-v014-candidate-version.sh <version|restamped>
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
field="${1:?usage: upgrade-v014-candidate-version.sh <version|restamped>}"
[ "$#" -eq 1 ] || {
    echo "usage: upgrade-v014-candidate-version.sh <version|restamped>" >&2
    exit 2
}

FACELOCK_UPGRADE_TEST_VERSION="${FACELOCK_UPGRADE_TEST_VERSION:-0.2.0}" \
python3 - "$repo_root" "$field" <<'PY'
import os
import re
import sys
from pathlib import Path

root, field = Path(sys.argv[1]), sys.argv[2]


def triple(version):
    match = re.match(r"^(\d+)\.(\d+)\.(\d+)", version)
    if not match:
        raise SystemExit(f"unparseable version: {version!r}")
    return tuple(int(part) for part in match.groups())


matrix = __import__("json").loads((root / "dist/release-matrix.json").read_text())
predecessor = matrix["predecessors"]["v0.1.4"]["upstream_version"]

cargo = (root / "Cargo.toml").read_text()
match = re.search(r'(?m)^version = "([^"]+)"', cargo)
if not match:
    raise SystemExit("workspace version not found in Cargo.toml")
workspace = match.group(1)

if triple(workspace) > triple(predecessor):
    version, restamped = workspace, "false"
else:
    version, restamped = os.environ["FACELOCK_UPGRADE_TEST_VERSION"], "true"

if triple(version) <= triple(predecessor):
    raise SystemExit(
        f"upgrade-test version {version} does not sort above predecessor {predecessor}"
    )

print(version if field == "version" else restamped)
PY
