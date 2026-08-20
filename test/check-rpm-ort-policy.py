#!/usr/bin/env python3
"""Static contract gate for the two RPM ONNX Runtime channels."""

from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


def require(text: str, needle: str, context: str) -> None:
    if needle not in text:
        raise SystemExit(f"{context}: missing {needle!r}")


def before(text: str, first: str, second: str, context: str) -> None:
    require(text, first, context)
    require(text, second, context)
    if text.index(first) >= text.index(second):
        raise SystemExit(f"{context}: {first!r} must precede {second!r}")


spec = read("dist/facelock.spec")
release = read(".github/workflows/release.yml")
build_rpm = read(".github/workflows/scripts/build-rpm.sh")
networkless = read(".github/workflows/scripts/run-networkless.sh")
validate_rpm = read(".github/workflows/scripts/validate-rpm.sh")
copr = read("test/copr-build.sh")
rpm_container = read("test/Containerfile.rpm-e2e")

for needle in (
    "%bcond_with bundled_ort",
    "%if %{without bundled_ort}",
    "BuildRequires:  onnxruntime",
    "Requires:       onnxruntime",
    "%if %{with bundled_ort}",
    "%global __strip /usr/bin/true",
    "libonnxruntime.so.1",
    "onnxruntime-manifest.json",
    "onnxruntime-SHA256SUMS",
    "%tmpfiles_create facelock.conf",
):
    require(spec, needle, "RPM spec channel contract")

for needle in (
    'ORT_VERSION: "1.20.1"',
    'ORT_SOURCE_URL: "https://github.com/microsoft/onnxruntime/releases/download/v1.20.1/onnxruntime-linux-x64-1.20.1.tgz"',
    'ORT_ARCHIVE_SHA256: "67db4dc1561f1e3fd42e619575c82c601ef89849afc7ea85a003abbac1a1a105"',
    'ORT_GIT_COMMIT: "5c1b7ccbff7e5141c1da7a9d963d660e5741c319"',
    'ORT_LICENSE: "MIT"',
    "onnxruntime/manifest.json",
    "onnxruntime/SHA256SUMS",
    "test/check-rpm-ort-policy.py",
):
    require(release, needle, "release workflow provenance contract")

before(release, "curl --output ort.tgz", "sha256sum --check", "release workflow download")
before(release, "sha256sum --check", 'tar -xzf ort.tgz', "release workflow extraction")
if "curl -fsSL" in release and "| tar" in release:
    raise SystemExit("release workflow download: streaming an unverified archive into tar is forbidden")

require(build_rpm, "--with bundled_ort", "direct RPM build mode")
require(build_rpm, "CARGO_NET_OFFLINE=true", "direct RPM offline assembly")
before(
    build_rpm,
    'exec "$SCRIPT_DIR/run-networkless.sh"',
    "mkdir -p ~/rpmbuild",
    "direct RPM whole-assembly network isolation",
)
before(
    build_rpm,
    "errno.ENOSYS",
    "mkdir -p ~/rpmbuild",
    "direct RPM unforgeable network-isolation probe",
)
for forbidden in ("curl ", "wget ", "git clone"):
    if forbidden in build_rpm:
        raise SystemExit(f"direct RPM package assembly must not use the network: {forbidden!r}")

for needle in (
    "enosys",
    "-s socket",
    "-s io_uring_setup",
    "FACELOCK_NETWORKLESS_ACTIVE",
    "socket.create_connection",
    "os.closerange",
):
    require(networkless, needle, "networkless RPM sandbox")

for needle in ('MODE="${2:', "direct", "copr", "rpm -qp --requires"):
    require(validate_rpm, needle, "RPM channel payload validator")
require(
    validate_rpm,
    "a5faaf78a37590d3fe640f887620e74f6022d34550172b91ad2131bf0ad77d64",
    "direct RPM reviewed library checksum",
)

for needle in (
    '.github/workflows/scripts/validate-rpm.sh "$BIN_RPM" copr',
    "rpm -q onnxruntime-devel",
    "live_runtime_creates_session_from_checksum_pinned_minimal_model",
):
    require(copr, needle, "COPR runtime contract")

if "ADD https://github.com/microsoft/onnxruntime" in rpm_container:
    raise SystemExit("RPM container fetch must not use Dockerfile ADD for an unverified archive")
before(rpm_container, "sha256sum --check", "tar -xzf /tmp/ort.tgz", "RPM container extraction")
require(
    rpm_container,
    '/validate-rpm.sh "$RPM_FILE" direct',
    "direct RPM container payload/dependency validation",
)
for needle in (
    "util-linux",
    "COPY .github/workflows/scripts/run-networkless.sh /run-networkless.sh",
    "/run-networkless.sh bash /build/test/build-rpm-prebuilt.sh",
):
    require(rpm_container, needle, "direct RPM networkless build regression")

print("RPM ORT channel policy: OK")
