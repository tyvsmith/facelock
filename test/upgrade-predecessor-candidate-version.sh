#!/usr/bin/env bash
# Decide which version the upgrade lanes build the candidate as.
#
# An upgrade lane needs a candidate that sorts strictly above the pinned
# predecessor, and on a development tree it does not have one: the workspace
# version stays at the last release until `just release` bumps it, so the .deb
# built from it carries the predecessor's own upstream version with a suffixed
# revision — which sorts *below* the published one — and the lane would be
# testing a downgrade.
#
# So the version is chosen, not assumed. When the workspace version already
# sorts above the predecessor the candidate is built exactly as it ships; when
# it does not, the lane builds the same payload as the upgrade-test version
# and says so: the predecessor's patch version plus one unless
# FACELOCK_UPGRADE_TEST_VERSION overrides it, so the default rolls with the
# pin rather than sitting at a literal that stops sorting above it. This
# mirrors what the retired-authselect fixture already does
# (test/build-rpm-authselect-fixtures.sh builds its candidate at 0.2.0), and
# the native comparator inside the container is still the authority — a wrong
# answer here fails the lane rather than passing quietly.
#
# Usage: upgrade-predecessor-candidate-version.sh <version|restamped>
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
field="${1:?usage: upgrade-predecessor-candidate-version.sh <version|restamped>}"
[ "$#" -eq 1 ] || {
    echo "usage: upgrade-predecessor-candidate-version.sh <version|restamped>" >&2
    exit 2
}

FACELOCK_UPGRADE_TEST_VERSION="${FACELOCK_UPGRADE_TEST_VERSION:-}" \
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
predecessors = matrix["predecessors"]
predecessor = predecessors[predecessors["current"]]["upstream_version"]
major, minor, patch = triple(predecessor)
default_test_version = f"{major}.{minor}.{patch + 1}"

cargo = (root / "Cargo.toml").read_text()
match = re.search(r'(?m)^version = "([^"]+)"', cargo)
if not match:
    raise SystemExit("workspace version not found in Cargo.toml")
workspace = match.group(1)

if triple(workspace) > triple(predecessor):
    version, restamped = workspace, "false"
else:
    version = os.environ["FACELOCK_UPGRADE_TEST_VERSION"] or default_test_version
    restamped = "true"

if triple(version) <= triple(predecessor):
    raise SystemExit(
        f"upgrade-test version {version} does not sort above predecessor {predecessor}"
    )

print(version if field == "version" else restamped)
PY
