#!/bin/bash
# The end-to-end tier gate of `just release-preflight` (#139).
#
# `just test-arch-integration` and `just test-arch-oneshot` are the only
# automated evidence that face authentication works end to end: real D-Bus
# activation, the real PAM stack, real capture, the one-shot fallback.
# Preflight cannot run them, so it refuses to pass until one of two records
# names HEAD:
#
#   .hardware-tiers-verified   written by `just test-arch-camera-required`,
#                              a real sensor and a person in frame: proves
#                              real frames of a real face match
#   .loopback-tier-verified    written by `just test-arch-loopback`, a
#                              synthetic v4l2loopback camera: proves the
#                              capture, IR classification, liveness, enroll,
#                              daemon, one-shot and PAM paths, not real-sensor
#                              recognition
#
# Either satisfies the gate. A run done by hand at this exact commit is
# acknowledged by naming the commit in FACELOCK_HARDWARE_TIERS_ACK or
# FACELOCK_LOOPBACK_TIER_ACK (at least seven leading characters of the sha),
# so the acknowledgement cannot become a habit the way a bare =1 would.
#
# Usage: test/e2e-tier-evidence.sh <head-sha> [record-dir]
# Exit 0 when the gate is satisfied, 1 when it is not; the report goes to
# stdout either way.
set -euo pipefail

head_sha="${1:?usage: e2e-tier-evidence.sh <head-sha> [record-dir]}"
dir="${2:-.}"

# Does an acknowledgement name this commit? Seven characters is git's own
# short-sha floor; anything shorter could match by accident.
acknowledges() {
    local ack="$1"
    [ "${#ack}" -ge 7 ] && [ "${head_sha#"$ack"}" != "$head_sha" ]
}

recorded_at() {
    local file="$1"
    if [ -f "$file" ]; then
        head -1 "$file"
    fi
}

hardware="$(recorded_at "$dir/.hardware-tiers-verified")"
loopback="$(recorded_at "$dir/.loopback-tier-verified")"
satisfied=0

if [ "$hardware" = "$head_sha" ]; then
    echo "OK: camera-required tiers recorded green at $head_sha (real sensor, real face)"
    satisfied=1
elif acknowledges "${FACELOCK_HARDWARE_TIERS_ACK:-}"; then
    echo "OK: camera-required tiers acknowledged by hand at $head_sha (real sensor, real face)"
    satisfied=1
fi

if [ "$loopback" = "$head_sha" ]; then
    echo "OK: loopback tier recorded green at $head_sha (synthetic camera; the pipeline, not real-sensor recognition)"
    satisfied=1
elif acknowledges "${FACELOCK_LOOPBACK_TIER_ACK:-}"; then
    echo "OK: loopback tier acknowledged by hand at $head_sha (synthetic camera; the pipeline, not real-sensor recognition)"
    satisfied=1
fi

if [ "$satisfied" -eq 1 ]; then
    exit 0
fi

for pair in "camera-required tiers:$hardware" "loopback tier:$loopback"; do
    name="${pair%%:*}"
    at="${pair#*:}"
    if [ -z "$at" ]; then
        echo "MISSING: no $name run recorded for any commit"
    else
        echo "STALE: $name recorded at $at, HEAD is $head_sha"
    fi
done
echo "  Either record satisfies the gate. Without a person to hand, the synthetic camera:"
echo "    just test-arch-loopback"
echo "  With a camera and someone in front of it (the only run that proves real-sensor recognition):"
echo "    just test-arch-camera-required"
echo "  If one was already run by hand at this exact commit, say which:"
echo "    FACELOCK_LOOPBACK_TIER_ACK=$head_sha just release-preflight"
echo "    FACELOCK_HARDWARE_TIERS_ACK=$head_sha just release-preflight"
exit 1
