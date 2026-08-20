#!/usr/bin/env bash
# Run a package-assembly command under a fail-closed seccomp network sandbox.
set -euo pipefail

if [ "$#" -eq 0 ]; then
    echo "Usage: run-networkless.sh <command> [args...]" >&2
    exit 2
fi

command -v enosys >/dev/null || {
    echo "ERROR: util-linux enosys is required for networkless RPM assembly" >&2
    exit 1
}
command -v python3 >/dev/null || {
    echo "ERROR: python3 is required to verify the networkless RPM sandbox" >&2
    exit 1
}

# Block both the ordinary socket API and io_uring, which otherwise has its own
# socket/connect operations. The Python launcher verifies the filter produces
# ENOSYS, closes every inherited non-stdio descriptor, and only then execs the
# package assembly command. This works inside the Fedora GitHub job container
# without CAP_SYS_ADMIN or nested user/network namespaces.
exec env FACELOCK_NETWORKLESS_ACTIVE=1 enosys \
    -s socket \
    -s socketpair \
    -s connect \
    -s bind \
    -s listen \
    -s accept \
    -s accept4 \
    -s sendto \
    -s sendmsg \
    -s sendmmsg \
    -s recvfrom \
    -s recvmsg \
    -s recvmmsg \
    -s shutdown \
    -s io_uring_setup \
    python3 -c '
import errno
import os
import resource
import socket
import sys

try:
    socket.create_connection(("1.1.1.1", 443), timeout=0.1)
except OSError as error:
    if error.errno != errno.ENOSYS:
        raise SystemExit(f"network sandbox probe failed with unexpected errno: {error}")
else:
    raise SystemExit("network sandbox probe unexpectedly connected")

soft_limit, _ = resource.getrlimit(resource.RLIMIT_NOFILE)
max_fd = 1 << 20 if soft_limit == resource.RLIM_INFINITY else min(soft_limit, 1 << 20)
os.closerange(3, max_fd)
print("networkless RPM sandbox: socket and io_uring network access denied", flush=True)
os.execvp(sys.argv[1], sys.argv[1:])
' "$@"
