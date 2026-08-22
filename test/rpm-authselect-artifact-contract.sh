#!/usr/bin/env bash
set -euo pipefail

test_rpm="${FACELOCK_TEST_RPM:?FACELOCK_TEST_RPM is required}"

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

rpm_payload=
if ! rpm_payload="$(rpm -qpl "$test_rpm")"; then
    fail "cannot query RPM payload inventory: $test_rpm"
fi
if grep -Fq '/authselect/' <<< "$rpm_payload"; then
    fail "new RPM contains an authselect path"
fi

rpm_requires=
if ! rpm_requires="$(rpm -qp --requires "$test_rpm")"; then
    fail "cannot query RPM dependency inventory: $test_rpm"
fi
if grep -Eq '(^|[[:space:]])authselect([[:space:]]|$)' <<< "$rpm_requires"; then
    fail "new RPM depends on authselect"
fi

echo "RPM authselect artifact contract: OK"
