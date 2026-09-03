#!/usr/bin/env bash
# Boot an rpm-e2e image with systemd as PID 1 and run the branched-release
# runtime smoke inside it.
#
# test/run-pkg-validate-systemd.sh is the full lifecycle runner for the Fedora
# releases the matrix marks "full". This is the shorter path for a branched
# release whose declared depth is build/runtime smoke, so it needs no models,
# no exact package argument, and none of the Debian lifecycle stages.
set -euo pipefail

IMAGE="${1:?usage: run-rpm-smoke-systemd.sh <image>}"
[ "$#" -eq 1 ] || {
    echo "usage: run-rpm-smoke-systemd.sh <image>" >&2
    exit 2
}

smoke_log="$(mktemp "${TMPDIR:-/tmp}/facelock-rpm-smoke.XXXXXX")"
trap 'rm -f -- "$smoke_log"' EXIT
cid=$(podman run -d --rm --systemd=always --security-opt unmask=ALL \
    "$IMAGE" /lib/systemd/systemd)
trap 'podman rm -f "$cid" >/dev/null 2>&1 || true; rm -f -- "$smoke_log"' EXIT

booted=""
for _ in $(seq 1 120); do
    state=$(podman exec "$cid" systemctl is-system-running 2>/dev/null || true)
    case "$state" in
        running|degraded) booted=1; break ;;
    esac
    sleep 1
done
if [ -z "$booted" ]; then
    echo "ERROR: systemd did not reach running/degraded state" >&2
    podman exec "$cid" systemctl --failed --no-pager 2>&1 || true
    exit 1
fi

lane_status=0
podman exec "$cid" /rpm-runtime-smoke.sh | tee "$smoke_log" || lane_status=$?

# The lane's packaging-evidence record, when the recipe named itself in
# PACKAGING_LANE (see test/run-pkg-validate-systemd.sh).
if [ -n "${PACKAGING_LANE:-}" ]; then
    python3 "$(dirname "$0")/packaging-evidence.py" record --lane "$PACKAGING_LANE" \
        --results-log "$smoke_log" --exit-status "$lane_status"
fi
exit "$lane_status"
