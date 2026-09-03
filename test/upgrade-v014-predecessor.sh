#!/usr/bin/env bash
# Resolve one pinned released predecessor from dist/release-matrix.json.
#
# The upgrade lanes (#231) install a real published artifact, so the pin has to
# be strong enough that a re-uploaded or substituted asset fails the lane rather
# than quietly changing what it proved. `dist/release-matrix.json` holds that
# pin — asset id, node id, SHA256 and byte size — and this script is the only
# reader. Lane Containerfiles take the fields as build args and never carry a
# digest of their own; test/check-release-matrix.py enforces that.
#
# Usage:
#   upgrade-v014-predecessor.sh <deb-trixie|rpm-fedora> <field>
#   upgrade-v014-predecessor.sh <lane> --build-args     # podman --build-arg list
#   upgrade-v014-predecessor.sh <lane> --verify-live    # re-resolve against the API
#
# --verify-live needs `gh` and network. It never downloads the asset: it asks
# the release API what the pinned asset id is now and rejects a name, size or
# digest that moved. A re-upload gets a new id, so a stale id is equally fatal.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
lane="${1:?usage: upgrade-v014-predecessor.sh <deb-trixie|rpm-fedora> <field|--build-args|--verify-live>}"
field="${2:?usage: upgrade-v014-predecessor.sh <deb-trixie|rpm-fedora> <field|--build-args|--verify-live>}"
[ "$#" -eq 2 ] || {
    echo "usage: upgrade-v014-predecessor.sh <lane> <field|--build-args|--verify-live>" >&2
    exit 2
}

case "$lane" in
    deb-trixie | rpm-fedora) ;;
    *)
        echo "unknown predecessor lane: $lane" >&2
        exit 2
        ;;
esac

read_lane() {
    python3 - "$repo_root/dist/release-matrix.json" "$lane" "$1" <<'PY'
import json
import sys

matrix_path, lane, field = sys.argv[1], sys.argv[2], sys.argv[3]
with open(matrix_path, encoding="utf-8") as handle:
    matrix = json.load(handle)

release = matrix["predecessors"]["v0.1.4"]
row = release["lanes"][lane]
if field == "tag":
    print(release["tag"])
elif field == "upstream_version":
    print(release["upstream_version"])
elif field == "release_id":
    print(release["release_id"])
elif field == "repository":
    print(matrix["predecessors"]["repository"])
else:
    if field not in row:
        raise SystemExit(f"unknown predecessor field: {field}")
    print(row[field])
PY
}

case "$field" in
    --build-args)
        printf '%s\n' \
            "FACELOCK_PREDECESSOR_URL=$(read_lane url)" \
            "FACELOCK_PREDECESSOR_SHA256=$(read_lane sha256)" \
            "FACELOCK_PREDECESSOR_SIZE=$(read_lane size)" \
            "FACELOCK_PREDECESSOR_NAME=$(read_lane name)" \
            "FACELOCK_PREDECESSOR_VERSION=$(read_lane package_version)"
        ;;
    --verify-live)
        command -v gh >/dev/null 2>&1 || {
            echo "FAIL: --verify-live needs the gh CLI" >&2
            exit 1
        }
        repository="$(read_lane repository)"
        tag="$(read_lane tag)"
        asset_id="$(read_lane asset_id)"
        live="$(gh api "repos/$repository/releases/tags/$tag" \
            --jq ".assets[] | select(.id == $asset_id) | \"\(.name)\t\(.size)\t\(.digest)\"")"
        [ -n "$live" ] || {
            echo "FAIL: release $tag no longer serves asset id $asset_id (re-uploaded or deleted)" >&2
            exit 1
        }
        expected="$(read_lane name)	$(read_lane size)	sha256:$(read_lane sha256)"
        [ "$live" = "$expected" ] || {
            echo "FAIL: pinned asset $asset_id changed" >&2
            echo "  expected: $expected" >&2
            echo "  live:     $live" >&2
            exit 1
        }
        echo "predecessor $lane: asset $asset_id unchanged ($(read_lane name))"
        ;;
    *)
        read_lane "$field"
        ;;
esac
