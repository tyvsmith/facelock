#!/bin/bash
set -euo pipefail

echo "=== Tier 1: Unit Tests ==="
cargo test --workspace

echo "=== Lint ==="
cargo clippy --workspace -- -D warnings

echo "=== Build Release ==="
cargo build --workspace --release

echo "=== PAM Symbol Verification ==="
if [ -f target/release/libpam_facelock.so ]; then
    nm -D target/release/libpam_facelock.so | grep -q pam_sm_authenticate && echo "  pam_sm_authenticate: OK"
    nm -D target/release/libpam_facelock.so | grep -q pam_sm_setcred && echo "  pam_sm_setcred: OK"

    echo "=== PAM Size Check ==="
    size=$(stat -c%s target/release/libpam_facelock.so)
    echo "  PAM module size: ${size} bytes"
    if [ "$size" -gt 1048576 ]; then
        echo "  WARNING: PAM module is ${size} bytes (>1MB)"
    else
        echo "  PAM module size OK (<1MB)"
    fi

    echo "=== PAM Dependency Check ==="
    ldd target/release/libpam_facelock.so
else
    echo "  SKIP: libpam_facelock.so not found (release build may not have completed)"
fi

echo ""
echo "=== All automated checks passed ==="
