#!/usr/bin/env bash
set -euo pipefail

image=${1:?usage: run-arch-package-systemd.sh <image>}
container=$(podman run -d --rm --systemd=always --security-opt unmask=ALL \
    "$image" /usr/lib/systemd/systemd)
trap 'podman rm -f "$container" >/dev/null 2>&1 || true' EXIT

booted=
for _ in $(seq 1 120); do
    state=$(podman exec "$container" systemctl is-system-running 2>/dev/null || true)
    case "$state" in
        running|degraded)
            booted=1
            break
            ;;
    esac
    sleep 1
done
if [ -z "$booted" ]; then
    podman exec "$container" systemctl --failed --no-pager 2>&1 || true
    exit 1
fi

podman exec "$container" /arch-package-validate.sh
