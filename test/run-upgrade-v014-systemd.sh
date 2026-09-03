#!/usr/bin/env bash
# Boot one released-predecessor upgrade lane under systemd and run its harness.
#
# Usage:
#   run-upgrade-v014-systemd.sh deb <image> <candidate.deb>
#   run-upgrade-v014-systemd.sh rpm <image>
#
# systemd is not optional here. The lane starts and stops the packaged daemon,
# and the rollback proof turns on the candidate daemon having actually opened
# and migrated the database before the downgrade — none of which a container
# without an init can do.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
family="${1:?usage: run-upgrade-v014-systemd.sh <deb|rpm> <image> [candidate.deb]}"
image="${2:?usage: run-upgrade-v014-systemd.sh <deb|rpm> <image> [candidate.deb]}"

mounts=()
case "$family" in
    deb)
        candidate="${3:?usage: run-upgrade-v014-systemd.sh deb <image> <candidate.deb>}"
        [ "$#" -eq 3 ] || {
            echo "usage: run-upgrade-v014-systemd.sh deb <image> <candidate.deb>" >&2
            exit 2
        }
        [ -f "$candidate" ] && [ ! -L "$candidate" ] || {
            echo "candidate package is not a regular file: $candidate" >&2
            exit 1
        }
        candidate="$(cd "$(dirname "$candidate")" && pwd)/$(basename "$candidate")"
        mounts+=(-v "$candidate:/artifacts/facelock-candidate.deb:ro,Z")
        ;;
    rpm)
        [ "$#" -eq 2 ] || {
            echo "usage: run-upgrade-v014-systemd.sh rpm <image>" >&2
            exit 2
        }
        ;;
    *)
        echo "unsupported upgrade lane family: $family" >&2
        exit 2
        ;;
esac

# The reviewed models, staged read-only. Unlike the package validator there is
# no partial-run opt-out: without models the candidate daemon cannot start, and
# a lane that skipped the daemon would report a rollback proof it never made.
shopt -s nullglob
onnx=("$repo_root"/models/*.onnx)
shopt -u nullglob
if [ "${#onnx[@]}" -eq 0 ]; then
    cat >&2 <<'EOF'
ERROR: no models/*.onnx in this checkout.

The upgrade lane starts the candidate daemon so the database is migrated by the
thing that migrates it in production, then downgrades under it. Without models
the daemon cannot load, so there is nothing to prove. Stage them first:

  just link-models
EOF
    exit 1
fi
# `z`, not `Z`: on an SELinux-enforcing host an unlabelled bind mount is denied
# outright, and the private label `Z` would take the models away from whatever
# else is reading them — the tree is hardlinked across worktrees and both lane
# halves can be running at once. Shared and read-only is what a model directory
# wants.
mounts+=(-v "$repo_root/models:/facelock-test-models:ro,z")

# --shm-size: the disk-full fault fills /dev/shm to get a real ENOSPC out of
# the kernel rather than simulating one. 16M is small enough to fill in a
# second and large enough for the fixture database that lives there.
cid="$(podman run -d --rm --systemd=always --security-opt unmask=ALL \
    --shm-size=16m "${mounts[@]}" "$image")"
trap 'podman rm -f "$cid" >/dev/null 2>&1 || true' EXIT

booted=
for _ in $(seq 1 120); do
    state="$(podman exec "$cid" systemctl is-system-running 2>/dev/null || true)"
    case "$state" in
        running | degraded)
            booted=1
            break
            ;;
    esac
    sleep 1
done
[ -n "$booted" ] || {
    podman exec "$cid" systemctl --failed --no-pager 2>&1 || true
    echo "ERROR: the upgrade lane container did not boot" >&2
    exit 1
}

podman exec "$cid" /upgrade-v014-lane.sh "$family"
