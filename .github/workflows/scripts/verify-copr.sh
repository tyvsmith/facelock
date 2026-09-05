#!/usr/bin/env bash
set -euo pipefail

# Wait for production COPR to serve the release that was just published.
#
# Packit reacts to the published release event and submits the COPR builds
# itself, so nothing in this workflow can report on them. When that submission
# fails there is no failed job anywhere -- only three red check runs on the tag
# commit that nobody is watching. That is how v0.1.4 was tagged, published, and
# never built, while COPR served 0.1.3 for three months (#333).
#
# This is the deadline that turns that silence into a failed release run. It
# publishes nothing and can undo nothing: the release is already public by the
# time it starts. It only makes the omission loud on the day it happens.

VERSION="${1:?Usage: verify-copr.sh <VERSION> <RPM_COUNTER>}"
RPM_COUNTER="${2:?Usage: verify-copr.sh <VERSION> <RPM_COUNTER>}"
# Three chroots, each a full from-source Rust build in a mock chroot, queued
# behind whatever else COPR is building. Ninety minutes is slack over what that
# has taken, not an estimate of it.
DEADLINE_SECONDS="${COPR_VERIFY_DEADLINE_SECONDS:-5400}"
INTERVAL_SECONDS="${COPR_VERIFY_INTERVAL_SECONDS:-120}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
# shellcheck source=../../../scripts/release-versions.sh
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/release-versions.sh"

release_validate_cargo_version "$VERSION"
if release_is_prerelease "$VERSION"; then
  echo "refusing to verify prerelease $VERSION against production COPR" >&2
  exit 1
fi
EVR="$(release_rpm_evr "$VERSION" "$RPM_COUNTER")"

echo "=== Waiting for production COPR to serve facelock-$EVR ==="
deadline=$((SECONDS + DEADLINE_SECONDS))
attempt=0
while :; do
  attempt=$((attempt + 1))
  status=0
  python3 "$REPO_ROOT/test/check-live-release-channels.py" \
    --channel production --expect-evr "$EVR" || status=$?
  case "$status" in
    0)
      echo "Production COPR serves facelock-$EVR after $attempt check(s)"
      exit 0
      ;;
    2)
      # Not yet: a build still running, or none submitted. Only the deadline
      # tells those apart, and only in one direction.
      ;;
    *)
      # The checker printed the reason above, and it is not always the build:
      # it compares the project's enabled chroots first, so drift there stops
      # the poll too. Naming a cause here would name the wrong one.
      echo "The live release-channel check failed; see its output above" >&2
      exit 1
      ;;
  esac
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "Production COPR did not serve facelock-$EVR within ${DEADLINE_SECONDS}s" >&2
    echo "Check the Packit check runs on the tag commit and the COPR project:" >&2
    echo "  https://github.com/${GITHUB_REPOSITORY:-tyvsmith/facelock}/commits/${GITHUB_REF_NAME:-$VERSION}" >&2
    echo "  https://copr.fedorainfracloud.org/coprs/tyvsmith/facelock/builds/" >&2
    echo "A submission that fails on a Copr project update means .packit.yaml" >&2
    echo "asked for a chroot the project does not have enabled; see docs/releasing.md." >&2
    exit 1
  fi
  sleep "$INTERVAL_SECONDS"
done
