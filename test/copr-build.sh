#!/bin/bash
# Local COPR-equivalent build verification — runs inside test/Containerfile.copr.
#
# Reproduces what COPR does for the facelock package: Packit generates the SRPM
# from dist/facelock.spec, then mock builds the RPM from source in a clean
# Fedora chroot, and the result is installed with dnf.
#
# `--enable-network` mirrors the COPR project's required "internet access during
# builds" setting — the Rust build fetches crates from crates.io.
#
# /repo is mounted READ-ONLY; we copy it to a writable workdir because
# `packit srpm` rewrites the spec file in place.
set -uo pipefail

CHROOT="${COPR_CHROOT:-fedora-44-x86_64}"
RESULT=0
section() { echo; echo "==== $* ===="; }

section "Copy repo to writable workdir"
mkdir -p /work
tar -C /repo --exclude='./target' -cf - . | tar -C /work -xf -
cd /work || { echo "FAIL: no workdir"; exit 1; }
if [ -f .git ]; then
  # A mounted git worktree carries a pointer to the host's common .git
  # directory, which is intentionally outside /repo and unavailable here.
  # Recreate only the disposable copy so Packit's git-archive source contains
  # the exact mounted working tree, including the uncommitted test subject.
  rm -f .git
  git init -q
  git config user.name "Facelock COPR Test"
  git config user.email "facelock@example.invalid"
  git add -A
  git commit -q -m "COPR test snapshot"
fi
git config --global --add safe.directory /work
echo "Branch: $(git branch --show-current 2>/dev/null)  HEAD: $(git rev-parse --short HEAD 2>/dev/null)"

section "Packit configuration schema"
packit config validate --offline -c .packit.yaml || { echo "FAIL: invalid Packit configuration"; exit 1; }

section "packit srpm"
packit srpm 2>&1 | tee /tmp/srpm.log
SRPM=$(grep -oE '/[^ ]+\.src\.rpm' /tmp/srpm.log | tail -1)
if [ -z "$SRPM" ]; then
  for candidate in /work/*.src.rpm; do
    [ -f "$candidate" ] || continue
    SRPM="$candidate"
    break
  done
fi
if [ -z "$SRPM" ] || [ ! -f "$SRPM" ]; then echo "FAIL: packit srpm produced no SRPM"; exit 1; fi
cp "$SRPM" /tmp/ ; SRPM="/tmp/$(basename "$SRPM")"
echo "SRPM: $SRPM"

section "mock rebuild ($CHROOT, from source, --enable-network)"
useradd -G mock mockbuilder 2>/dev/null || true
chmod 644 "$SRPM"
if su mockbuilder -c "mock -r '$CHROOT' --isolation=simple --enable-network --rebuild '$SRPM' --resultdir /tmp/mock"; then
  echo "mock build: OK"
else
  echo "FAIL: mock build failed"
  tail -80 /tmp/mock/build.log 2>/dev/null || true
  exit 1
fi

section "Built RPM checks"
BIN_RPM=""
for candidate in /tmp/mock/facelock-*.x86_64.rpm; do
  [ -f "$candidate" ] || continue
  case "$candidate" in *debug*) continue ;; esac
  BIN_RPM="$candidate"
  break
done
if [ -z "$BIN_RPM" ]; then echo "FAIL: no binary RPM produced"; exit 1; fi
echo "RPM: $BIN_RPM"
.github/workflows/scripts/validate-rpm.sh "$BIN_RPM" copr || RESULT=1
if grep -q 'live_runtime_creates_session_from_checksum_pinned_minimal_model.*ok' /tmp/mock/build.log; then
  echo "real ORT session smoke: OK"
else
  echo "FAIL: pinned-model real ORT session smoke did not pass"; RESULT=1
fi

section "Install test"
if dnf install -y "$BIN_RPM"; then
  if rpm -q onnxruntime >/dev/null; then echo "onnxruntime pulled by dnf: OK"; else echo "FAIL: onnxruntime not pulled"; RESULT=1; fi
  if rpm -q onnxruntime-devel >/dev/null; then echo "FAIL: onnxruntime-devel was installed"; RESULT=1; else echo "onnxruntime-devel absent: OK"; fi
  if facelock --version >/dev/null; then echo "facelock runs: OK"; else echo "FAIL: facelock did not run"; RESULT=1; fi
else
  echo "FAIL: dnf install of built RPM failed"; RESULT=1
fi

section "RESULT"
if [ "$RESULT" -eq 0 ]; then echo "test-copr: PASS"; else echo "test-copr: FAIL"; fi
exit $RESULT
