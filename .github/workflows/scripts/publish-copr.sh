#!/usr/bin/env bash
set -euo pipefail

echo "=== Triggering COPR build ==="

if [ -z "${COPR_WEBHOOK_URL:-}" ]; then
  echo "COPR_WEBHOOK_URL secret not configured. Skipping COPR trigger."
  echo "See docs/releasing.md for setup instructions."
  exit 0
fi

curl -X POST "$COPR_WEBHOOK_URL" \
  --fail --silent --show-error

echo "COPR build triggered successfully"
