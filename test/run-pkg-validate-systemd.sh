#!/usr/bin/env bash
# Boot a package-test container (deb-e2e / rpm-e2e image) with systemd as
# PID 1 and run /pkg-validate.sh inside it via podman exec.
#
# Running under a real systemd lets pkg-validate.sh verify the Phase 3
# hardening directives of facelock-daemon.service (systemctl show), start
# the daemon inside the sandbox, and probe the seccomp/address-family
# restrictions with transient units.
#
# Usage: test/run-pkg-validate-systemd.sh <image>
set -euo pipefail

IMAGE="${1:?usage: run-pkg-validate-systemd.sh <image>}"

# Bind-mount repo ONNX models so the daemon-start test can run: `facelock
# daemon` loads models at startup. Models are large and gitignored, so a fresh
# worktree does not have them — which used to mean the daemon-start block (and
# the runtime CAP_CHOWN thread walk inside it) quietly vanished from an
# otherwise green run. It is now a failure inside the container unless
# FACELOCK_ALLOW_MISSING_MODELS=1 says a partial run is wanted, and that
# opt-out is forwarded into the exec below.
mounts=()
shopt -s nullglob
onnx=(models/*.onnx)
shopt -u nullglob
if [ "${#onnx[@]}" -gt 0 ]; then
    # Stage read-only rather than mounting over the runtime model directory.
    # pkg-validate removes its disposable runtime copy before the uninstall
    # assertions to prove package cleanup has no model dependency.
    mounts=(-v "$PWD/models:/facelock-test-models:ro")
elif [ "${FACELOCK_ALLOW_MISSING_MODELS:-0}" = "1" ]; then
    echo "WARNING: no models/*.onnx in repo — the daemon-start assertions will be" >&2
    echo "         reported as skipped (FACELOCK_ALLOW_MISSING_MODELS=1)." >&2
else
    echo "WARNING: no models/*.onnx in repo — the daemon-start assertions cannot run" >&2
    echo "         and pkg-validate.sh will FAIL. Copy them in first:" >&2
    echo "           sudo cp /var/lib/facelock/models/*.onnx models/" >&2
    echo "         or set FACELOCK_ALLOW_MISSING_MODELS=1 to accept a partial run." >&2
fi

# Forward the opt-out into the container; pkg-validate.sh reads it.
exec_env=()
if [ -n "${FACELOCK_ALLOW_MISSING_MODELS:-}" ]; then
    exec_env=(-e "FACELOCK_ALLOW_MISSING_MODELS=$FACELOCK_ALLOW_MISSING_MODELS")
fi

# --systemd=always: podman sets up /run, /tmp, cgroups and SIGRTMIN+3 for a
#   systemd payload.
# --security-opt unmask=ALL: leave /proc unmasked so systemd can set up
#   ProtectProc=/ProcSubset= (they need a fresh procfs mount, which the
#   kernel refuses when parts of /proc are overmounted).
cid=$(podman run -d --rm --systemd=always --security-opt unmask=ALL \
    "${mounts[@]}" "$IMAGE" /lib/systemd/systemd)
trap 'podman rm -f "$cid" >/dev/null 2>&1 || true' EXIT

# Wait for systemd to finish booting (degraded is fine — minimal containers
# routinely have a failed getty/timesyncd; the validation doesn't need them).
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

if [ "${#onnx[@]}" -gt 0 ]; then
    podman exec "$cid" /bin/sh -c \
        'install -d /var/lib/facelock/models && cp /facelock-test-models/*.onnx /var/lib/facelock/models/'
fi

podman exec "${exec_env[@]}" "$cid" /pkg-validate.sh
