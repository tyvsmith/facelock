#!/usr/bin/env bash
# Only the fixed adapter enum is accepted; never evaluates documentation text.
set -euo pipefail
case "${1:-}" in
    arch|apt|deb|rpm|copr|source|verify-cli|manual) ;;
    *) echo 'unknown walkthrough adapter' >&2; exit 2 ;;
esac
[ "$#" = 1 ] || exit 2
exec python3 "$(dirname "$0")/guest.py" "$1"
