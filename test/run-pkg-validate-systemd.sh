#!/usr/bin/env bash
# Boot a package-test container (deb-e2e / rpm-e2e image) with systemd as
# PID 1 and run /pkg-validate.sh inside it via podman exec.
#
# Running under a real systemd lets pkg-validate.sh verify the Phase 3
# hardening directives of facelock-daemon.service (systemctl show), start
# the daemon inside the sandbox, and probe the seccomp/address-family
# restrictions with transient units.
#
# Usage: test/run-pkg-validate-systemd.sh <image> [exact-package.deb]
set -euo pipefail

IMAGE="${1:?usage: run-pkg-validate-systemd.sh <image> [exact-package.deb]}"
PACKAGE="${2:-}"
[ "$#" -le 2 ] || {
    echo "usage: run-pkg-validate-systemd.sh <image> [exact-package.deb]" >&2
    exit 2
}
package_mount=()
if [ -n "$PACKAGE" ]; then
    [ -f "$PACKAGE" ] && [ ! -L "$PACKAGE" ] || {
        echo "exact Debian package is not a regular file: $PACKAGE" >&2
        exit 1
    }
    PACKAGE="$(cd "$(dirname "$PACKAGE")" && pwd)/$(basename "$PACKAGE")"
    [ "$(stat -c %a "$PACKAGE")" = 444 ] || {
        echo "exact Debian package must have mode 0444: $PACKAGE" >&2
        exit 1
    }
    package_mount=(-v "$PACKAGE:/facelock-test-package.deb:ro,Z")
fi

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

# When a lane recipe names itself in PACKAGING_LANE, the run ends by writing
# .packaging-evidence/<lane>.json through test/packaging-evidence.py: the
# counts from pkg-validate.sh's RESULTS_JSON line, plus any skip this runner
# took on its own, so `just test-packaging-matrix` can refuse a partial run
# instead of recording it as the release gate.
runner_skips=()
validate_log="$(mktemp "${TMPDIR:-/tmp}/facelock-pkg-validate.XXXXXX")"
trap 'rm -f -- "$validate_log"' EXIT

# --systemd=always: podman sets up /run, /tmp, cgroups and SIGRTMIN+3 for a
#   systemd payload.
# --security-opt unmask=ALL: leave /proc unmasked so systemd can set up
#   ProtectProc=/ProcSubset= (they need a fresh procfs mount, which the
#   kernel refuses when parts of /proc are overmounted).
cid=$(podman run -d --rm --systemd=always --security-opt unmask=ALL \
    "${mounts[@]}" "${package_mount[@]}" "$IMAGE" /lib/systemd/systemd)
trap 'podman rm -f "$cid" >/dev/null 2>&1 || true; rm -f -- "$validate_log"' EXIT

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

# Never expose checkout files at Facelock's mutable runtime path. Copy only
# model payloads from the read-only mount into this disposable container after
# systemd has booted. The genuine active-service upgrade cases need the daemon
# to load them before the shared package validator begins.
if [ "${#onnx[@]}" -gt 0 ]; then
    podman exec "$cid" sh -eu -c '
        install -d -m 0755 /var/lib/facelock/models
        for model in /facelock-test-models/*.onnx; do
            [ -f "$model" ]
            install -m 0644 "$model" /var/lib/facelock/models/
        done
    '
fi

# The presence test below is the deb/rpm switch, so it is also the one place a
# dropped COPY would turn the whole Debian lifecycle into silence and still let
# the gate finish green. An exact package means the Debian lanes must run.
if [ -n "$PACKAGE" ] && ! podman exec "$cid" test -x /deb-package-lifecycle.sh; then
    echo "ERROR: exact Debian package supplied but the image carries no lifecycle harness" >&2
    exit 1
fi

if podman exec "$cid" test -x /deb-package-lifecycle.sh; then
    [ -n "$PACKAGE" ] || {
        echo "ERROR: Debian lifecycle image requires an exact package argument" >&2
        exit 1
    }
    podman exec "$cid" /deb-package-lifecycle.sh install-remove-reinstall
    podman exec "$cid" /deb-package-lifecycle.sh versioned-upgrade-inactive
    if [ "${#onnx[@]}" -gt 0 ]; then
        podman exec "$cid" /deb-package-lifecycle.sh versioned-upgrade-active
    elif [ "${FACELOCK_ALLOW_MISSING_MODELS:-0}" = 1 ]; then
        echo "SKIP: Debian active-service versioned upgrades (no ONNX models, FACELOCK_ALLOW_MISSING_MODELS=1)" >&2
        runner_skips+=(--extra-skip allowed)
    else
        echo "ERROR: Debian active-service versioned upgrades require the reviewed ONNX models" >&2
        exit 1
    fi
    podman exec "$cid" install -m 0644 /facelock-test.pam /etc/pam.d/facelock-test
fi

# An RPM image is one with no Debian lifecycle harness, and its full depth is
# three stages: the service/PAM lifecycle, pkg-validate.sh, and the
# %config(noreplace) upgrade lifecycle. A lane that runs fewer must not record
# `depth=full`: the stages are optional-by-presence, which is exactly how a
# lane could otherwise claim a complete lifecycle it never ran (#230). Missing
# stages downgrade the recorded depth to `partial`, which the release matrix
# requires of nothing and the aggregate therefore refuses.
missing_stages=()
if podman exec "$cid" test -x /deb-package-lifecycle.sh; then
    :
else
    for stage in /rpm-service-pam-lifecycle.sh /rpm-config-lifecycle.sh; do
        podman exec "$cid" test -x "$stage" || missing_stages+=("$stage")
    done
fi

if podman exec "$cid" test -x /rpm-service-pam-lifecycle.sh; then
    podman exec "$cid" /rpm-service-pam-lifecycle.sh
fi

lane_status=0
podman exec "${exec_env[@]}" "$cid" /pkg-validate.sh | tee "$validate_log" || lane_status=$?

# pkg-validate.sh pins %config(noreplace) on erase and finishes with the package
# uninstalled, which is the clean slate the upgrade half needs.
after_validate() {
    if podman exec "$cid" test -x /rpm-config-lifecycle.sh; then
        podman exec "$cid" /rpm-config-lifecycle.sh || return
    fi
    if podman exec "$cid" test -x /deb-package-lifecycle.sh; then
        podman exec "$cid" /deb-package-lifecycle.sh purge || return
    fi
}
if [ "$lane_status" -eq 0 ]; then
    after_validate || lane_status=$?
fi

# Recorded last, so the record's status covers every stage above it.
if [ -n "${PACKAGING_LANE:-}" ]; then
    lane_spec="$PACKAGING_LANE"
    if [ "${#missing_stages[@]}" -gt 0 ]; then
        echo "WARNING: image carries no ${missing_stages[*]}; recording depth=partial," >&2
        echo "         which no release-matrix lane accepts, instead of the declared depth." >&2
        lane_spec="${lane_spec/depth=full/depth=partial}"
    fi
    python3 "$(dirname "$0")/packaging-evidence.py" record --lane "$lane_spec" \
        --results-log "$validate_log" --exit-status "$lane_status" "${runner_skips[@]}"
fi
exit "$lane_status"
