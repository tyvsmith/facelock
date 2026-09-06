#!/bin/bash
# Local COPR-equivalent build verification — runs inside test/Containerfile.copr.
#
# Reproduces what COPR does for the facelock package: Packit generates the SRPM
# from dist/facelock.spec, then mock builds the RPM from source in a clean
# Fedora chroot, and the result is installed with dnf.
#
# The chroot's network is read out of .packit.yaml rather than hardcoded. COPR
# resolves network access per build and Packit sends the value, so a job without
# `enable_net: true` builds with no resolver no matter what the project's
# "internet access during builds" toggle says. Taking the flag from the config
# is what makes this lane fail the way real COPR would; hardcoding
# `--enable-network` is what let v0.2.0 pass here and fail there.
#
# /repo is mounted READ-ONLY; we copy it to a writable workdir because
# `packit srpm` rewrites the spec file in place.
set -uo pipefail

CHROOT="${COPR_CHROOT:-fedora-44-x86_64}"
RESULT=0
section() { echo; echo "==== $* ===="; }

# dist/release-matrix.json is the authority for which chroots are staging
# targets. Refusing anything outside it keeps a lane from silently building
# against Rawhide, which the matrix marks optional/experimental and which may
# never stand in for a Fedora 43, 44, or 45 result.
if ! python3 - "$CHROOT" <<'PY'
import json
import sys

with open("/repo/dist/release-matrix.json", encoding="utf-8") as handle:
    targets = json.load(handle)["fedora"]["staging_copr_targets"]
if sys.argv[1] not in targets:
    print(f"COPR_CHROOT {sys.argv[1]!r} is not a declared staging target: {sorted(targets)}", file=sys.stderr)
    raise SystemExit(1)
PY
then
  echo "FAIL: undeclared COPR chroot"
  exit 1
fi

# Every copr_build job has to agree, because one SRPM is rebuilt here for all of
# them and a lane cannot model two different chroot networks at once.
if ! NETWORK_FLAG=$(python3 - <<'NET'
import json

with open("/repo/.packit.yaml", encoding="utf-8") as handle:
    jobs = json.load(handle)["jobs"]
values = {job.get("enable_net") for job in jobs if job.get("job") == "copr_build"}
if len(values) != 1:
    raise SystemExit(f"copr_build jobs disagree on enable_net: {sorted(map(repr, values))}")
enable_net = values.pop()
if enable_net is not True and enable_net is not False:
    raise SystemExit(f"copr_build enable_net must be a bool, got {enable_net!r}")
print("--enable-network" if enable_net else "")
NET
); then
  echo "FAIL: cannot read enable_net from .packit.yaml"
  exit 1
fi

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

section "mock rebuild ($CHROOT, from source, network=${NETWORK_FLAG:-off})"
useradd -G mock mockbuilder 2>/dev/null || true
chmod 644 "$SRPM"
if su mockbuilder -c "mock -r '$CHROOT' --isolation=simple ${NETWORK_FLAG} --rebuild '$SRPM' --resultdir /tmp/mock"; then
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

# The lifecycle lanes (`just test-copr-pkg` / `just test-copr-smoke`) mount a
# writable /out and take the mock-built RPM back to the host, so the exact
# artifact this chroot produced can be installed in a booted systemd container
# and put through the same validation the direct-RPM lanes run. `just test-copr`
# mounts nothing and is unaffected.
if [ -d /out ]; then
  section "Export the mock-built RPM"
  if cp "$BIN_RPM" /out/facelock.rpm && chmod 0644 /out/facelock.rpm; then
    echo "exported $(basename "$BIN_RPM") to /out/facelock.rpm"
  else
    echo "FAIL: could not export the built RPM to /out"; RESULT=1
  fi
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
