#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:?Usage: build-deb.sh <VERSION> <VARIANT> [EXTRA_DEPENDS]}"
VARIANT="${2:?Usage: build-deb.sh <VERSION> <VARIANT> [EXTRA_DEPENDS]}"
EXTRA_DEPENDS="${3:-}"

# Set version suffix and description based on variant
case "$VARIANT" in
  legacy)
    PKG_VERSION="${VERSION}-1~legacy1"
    DESCRIPTION="Face authentication for Linux PAM (legacy, no TPM)"
    DESCRIPTION_LONG=" Facelock provides Windows Hello-style face authentication for Linux
 using IR anti-spoofing, ONNX inference, and PAM integration.
 .
 This is the legacy build without TPM support, for systems that lack
 libtss2-esys >= 4.1.3 (e.g. Ubuntu 24.04, Debian bookworm).
 .
 After installation, run 'sudo facelock setup' to download face
 recognition models, then 'sudo facelock enroll' to register your face."
    DEPENDS="libpam-runtime, dbus"
    ;;
  tpm)
    PKG_VERSION="${VERSION}-1"
    DESCRIPTION="Face authentication for Linux PAM (TPM enabled)"
    DESCRIPTION_LONG=" Facelock provides Windows Hello-style face authentication for Linux
 using IR anti-spoofing, ONNX inference, and PAM integration.
 .
 This build includes TPM 2.0 support for hardware-sealed face
 template encryption. Requires libtss2-esys >= 4.1.3.
 .
 After installation, run 'sudo facelock setup' to download face
 recognition models, then 'sudo facelock enroll' to register your face."
    DEPENDS="libpam-runtime, dbus"
    if [ -n "$EXTRA_DEPENDS" ]; then
      DEPENDS="${DEPENDS}, ${EXTRA_DEPENDS}"
    fi
    ;;
  *)
    echo "ERROR: Unknown variant '$VARIANT'. Use 'legacy' or 'tpm'."
    exit 1
    ;;
esac

PKG_DIR="facelock_${PKG_VERSION}_amd64"
echo "=== Building .deb package (${VARIANT}) ==="
echo "Version: ${PKG_VERSION}"
echo "Package dir: ${PKG_DIR}"

# Create directory tree
mkdir -p "${PKG_DIR}/DEBIAN"
mkdir -p "${PKG_DIR}/usr/bin"
mkdir -p "${PKG_DIR}/lib/security"
mkdir -p "${PKG_DIR}/etc/facelock"
mkdir -p "${PKG_DIR}/usr/lib/systemd/system"
mkdir -p "${PKG_DIR}/usr/lib/sysusers.d"
mkdir -p "${PKG_DIR}/usr/lib/tmpfiles.d"
mkdir -p "${PKG_DIR}/usr/lib/facelock"
mkdir -p "${PKG_DIR}/usr/share/pam-configs"
mkdir -p "${PKG_DIR}/usr/share/dbus-1/system.d"
mkdir -p "${PKG_DIR}/usr/share/dbus-1/system-services"
mkdir -p "${PKG_DIR}/usr/share/facelock/quirks.d"
mkdir -p "${PKG_DIR}/usr/share/doc/facelock"

# Control file
cat > "${PKG_DIR}/DEBIAN/control" <<CTRL
Package: facelock
Version: ${PKG_VERSION}
Architecture: amd64
Maintainer: Facelock Contributors <facelock@m.tysmith.me>
Depends: ${DEPENDS}
Section: admin
Priority: optional
Homepage: https://github.com/tyvsmith/facelock
Description: ${DESCRIPTION}
${DESCRIPTION_LONG}
CTRL

# postinst and prerm scripts
cp dist/debian/postinst "${PKG_DIR}/DEBIAN/postinst"
chmod 755 "${PKG_DIR}/DEBIAN/postinst"
cp dist/debian/prerm "${PKG_DIR}/DEBIAN/prerm"
chmod 755 "${PKG_DIR}/DEBIAN/prerm"

# Binaries
install -m755 target/release/facelock "${PKG_DIR}/usr/bin/facelock"
if [ -f target/release/facelock-polkit-agent ]; then
  install -m755 target/release/facelock-polkit-agent "${PKG_DIR}/usr/bin/facelock-polkit-agent"
fi

# PAM module
install -m755 target/release/libpam_facelock.so "${PKG_DIR}/lib/security/pam_facelock.so"

# Configuration
install -m644 config/facelock.toml "${PKG_DIR}/etc/facelock/config.toml"

# Quirks database
install -m644 -t "${PKG_DIR}/usr/share/facelock/quirks.d/" config/quirks.d/*.toml

# systemd units
install -m644 systemd/facelock-daemon.service "${PKG_DIR}/usr/lib/systemd/system/facelock-daemon.service"

# D-Bus policy and activation service
install -m644 dbus/org.facelock.Daemon.conf "${PKG_DIR}/usr/share/dbus-1/system.d/org.facelock.Daemon.conf"
install -m644 dbus/org.facelock.Daemon.service "${PKG_DIR}/usr/share/dbus-1/system-services/org.facelock.Daemon.service"

# sysusers.d and tmpfiles.d
install -m644 dist/facelock.sysusers "${PKG_DIR}/usr/lib/sysusers.d/facelock.conf"
install -m644 dist/facelock.tmpfiles "${PKG_DIR}/usr/lib/tmpfiles.d/facelock.conf"

# pam-auth-update profile
if [ -f dist/debian/pam-auth-update ]; then
  install -m644 dist/debian/pam-auth-update "${PKG_DIR}/usr/share/pam-configs/facelock"
fi

# Bundled CPU ONNX Runtime
if [ -f onnxruntime/lib/libonnxruntime.so ]; then
  install -m755 onnxruntime/lib/libonnxruntime.so "${PKG_DIR}/usr/lib/facelock/libonnxruntime.so"
fi

# Copyright
install -m644 dist/debian/copyright "${PKG_DIR}/usr/share/doc/facelock/copyright"

dpkg-deb --build "${PKG_DIR}"

echo "=== .deb package built: ${PKG_DIR}.deb ==="
