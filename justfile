# facelock build automation
# Usage: just <recipe>

# Build in debug mode (development)
build:
    cargo build --workspace

# Build in release mode (for install)
build-release:
    cargo build --release --workspace
    cargo build --release -p facelock-cli --features tpm

# Build with CUDA GPU acceleration (requires: sudo pacman -S onnxruntime-opt-cuda)
build-cuda:
    cargo build --workspace --features cuda

# Build release with CUDA GPU acceleration
build-release-cuda:
    cargo build --release --workspace --features cuda
    cargo build --release -p facelock-cli --features tpm,cuda

# Build with TensorRT GPU acceleration (requires CUDA + TensorRT SDK)
build-tensorrt:
    cargo build --workspace --features cuda,tensorrt

# Build release with TensorRT GPU acceleration
build-release-tensorrt:
    cargo build --release --workspace --features cuda,tensorrt
    cargo build --release -p facelock-cli --features tpm,cuda,tensorrt

# Run all unit tests
test:
    cargo test --workspace

# Run all tests including hardware-dependent (ignored) tests
test-all:
    cargo test --workspace -- --include-ignored

# Run clippy with warnings as errors
lint:
    cargo clippy --workspace -- -D warnings

# Format check
fmt-check:
    cargo fmt --all -- --check

# Format code
fmt:
    cargo fmt --all

# Run all checks (test + lint + format)
check: test lint fmt-check

# Run container PAM smoke tests
test-pam: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -f test/Containerfile ]; then
        podman build -t facelock-pam-test -f test/Containerfile .
        podman run --rm facelock-pam-test
    else
        echo "No test/Containerfile found"
        exit 1
    fi

# Run end-to-end integration tests in container (requires camera)
test-integration: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    podman build -t facelock-pam-test -f test/Containerfile .
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    podman run --rm $devices facelock-pam-test /run-integration-tests.sh

# Run oneshot (daemonless) end-to-end tests in container (requires camera)
test-oneshot: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    podman build -t facelock-pam-test -f test/Containerfile .
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    podman run --rm $devices facelock-pam-test /run-oneshot-tests.sh

# Open interactive shell in PAM test container (requires camera)
test-shell: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    podman build -t facelock-pam-test -f test/Containerfile .
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    echo "Starting interactive shell. Try:"
    echo "  facelock daemon &"
    echo "  sleep 2"
    echo "  facelock enroll --user testuser --label myface"
    echo "  facelock test --user testuser"
    echo "  pamtester facelock-test testuser authenticate"
    podman run --rm -it $devices facelock-pam-test /bin/bash

# Build release and install to system
# Run as: just install (builds as you, installs as root)
install: build-release
    sudo env PATH="$PATH" just install-files

# Build release with CUDA and install to system
# Requires: sudo pacman -S onnxruntime-opt-cuda
install-cuda:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! pacman -Q onnxruntime-opt-cuda &>/dev/null && ! pacman -Q onnxruntime-cuda &>/dev/null; then
        echo "System ONNX Runtime with CUDA not found."
        echo "Install with: sudo pacman -S onnxruntime-opt-cuda"
        exit 1
    fi
    just build-release-cuda
    sudo env PATH="$PATH" just install-files install-cuda-config

# Install pre-built binaries to system (requires root, no build)
install-files:
    #!/usr/bin/env bash
    set -euo pipefail
    PAM_LINE="auth  sufficient  pam_facelock.so"

    # Verify binaries exist
    for f in target/release/facelock target/release/libpam_facelock.so; do
        [ -f "$f" ] || { echo "Error: $f not found. Run 'just build-release' first."; exit 1; }
    done

    # Create facelock system group and add the installing user
    getent group facelock >/dev/null || groupadd -r facelock
    REAL_USER="${SUDO_USER:-${DOAS_USER:-}}"
    if [ -n "$REAL_USER" ] && ! id -nG "$REAL_USER" 2>/dev/null | grep -qw facelock; then
        usermod -aG facelock "$REAL_USER"
        echo "Added $REAL_USER to facelock group (log out and back in to take effect)."
    fi

    # Binaries
    install -Dm755 target/release/facelock /usr/bin/facelock
    install -Dm755 target/release/libpam_facelock.so /lib/security/pam_facelock.so

    # Config (don't overwrite existing)
    install -Dm644 config/facelock.toml /etc/facelock/config.toml.default
    [ -f /etc/facelock/config.toml ] || cp /etc/facelock/config.toml.default /etc/facelock/config.toml

    # Hardware quirks database
    install -dm755 /usr/share/facelock/quirks.d
    install -Dm644 config/quirks.d/*.toml /usr/share/facelock/quirks.d/

    # systemd unit
    install -Dm644 systemd/facelock-daemon.service /usr/lib/systemd/system/facelock-daemon.service

    # D-Bus policy and activation
    install -Dm644 dbus/org.facelock.Daemon.conf /usr/share/dbus-1/system.d/org.facelock.Daemon.conf
    install -Dm644 dbus/org.facelock.Daemon.service /usr/share/dbus-1/system-services/org.facelock.Daemon.service

    # Polkit agent binary (optional, do NOT install autostart — agent is not production-ready
    # and will steal polkit auth from the DE's agent, causing all privilege prompts to hang)
    [ -f target/release/facelock-polkit-agent ] && install -Dm755 target/release/facelock-polkit-agent /usr/bin/facelock-polkit-agent || true

    # Directories
    install -dm770 -o root -g facelock /var/lib/facelock
    install -dm755 -o root -g root /var/lib/facelock/models
    install -dm750 -o root -g facelock /var/log/facelock
    install -dm750 -o root -g facelock /var/log/facelock/snapshots

    # Enable D-Bus activation (if systemd present)
    if [ -d /run/systemd/system ]; then
        systemctl stop facelock-daemon.service 2>/dev/null || true
        systemctl daemon-reload
        systemctl reset-failed facelock-daemon.service 2>/dev/null || true
        systemctl enable facelock-daemon.service 2>/dev/null || true
        echo "D-Bus activation enabled."
    fi

    # Add to /etc/pam.d/sudo (if not already present)
    if [ -f /etc/pam.d/sudo ] && ! grep -qF "$PAM_LINE" /etc/pam.d/sudo; then
        cp /etc/pam.d/sudo /etc/pam.d/sudo.facelock-backup
        sed -i "0,/^auth/{s/^auth/${PAM_LINE}\nauth/}" /etc/pam.d/sudo
        echo "Added face auth to /etc/pam.d/sudo (backup: /etc/pam.d/sudo.facelock-backup)"
    fi

    # Fix permissions on existing data
    [ -d /var/lib/facelock/models ] && chmod 644 /var/lib/facelock/models/*.onnx 2>/dev/null || true
    [ -f /var/lib/facelock/facelock.db ] && chown root:facelock /var/lib/facelock/facelock.db && chmod 640 /var/lib/facelock/facelock.db || true

    echo ""
    echo "Installed. Two steps remaining:"
    echo "  1. sudo facelock setup       (download face recognition models)"
    echo "  2. sudo facelock enroll      (register your face)"

# Build release with TensorRT and install to system
# Requires: CUDA toolkit + TensorRT SDK (libnvinfer)
install-tensorrt:
    #!/usr/bin/env bash
    set -euo pipefail
    # Check for TensorRT library
    if ! ldconfig -p 2>/dev/null | grep -q libnvinfer; then
        echo "TensorRT SDK (libnvinfer) not found."
        echo "Install from: https://developer.nvidia.com/tensorrt"
        echo "  Arch/CachyOS: yay -S libnvinfer  (AUR)"
        echo "  Ubuntu/Debian: apt install libnvinfer-dev"
        exit 1
    fi
    if ! pacman -Q onnxruntime-opt-cuda &>/dev/null && ! pacman -Q onnxruntime-cuda &>/dev/null; then
        echo "System ONNX Runtime with CUDA not found."
        echo "Install with: sudo pacman -S onnxruntime-opt-cuda"
        exit 1
    fi
    just build-release-tensorrt
    sudo env PATH="$PATH" just install-files install-tensorrt-config

# Enable TensorRT execution provider in installed config (requires root)
install-tensorrt-config:
    #!/usr/bin/env bash
    set -euo pipefail
    CONFIG="/etc/facelock/config.toml"
    [ -f "$CONFIG" ] || { echo "Error: $CONFIG not found. Run 'just install' first."; exit 1; }
    if grep -q '^execution_provider' "$CONFIG"; then
        sed -i 's/^execution_provider.*/execution_provider = "tensorrt"/' "$CONFIG"
    elif grep -q '^# execution_provider' "$CONFIG"; then
        sed -i 's/^# execution_provider.*/execution_provider = "tensorrt"/' "$CONFIG"
    else
        sed -i '/^\[recognition\]/a execution_provider = "tensorrt"' "$CONFIG"
    fi
    echo "TensorRT execution provider enabled in $CONFIG"
    if systemctl is-active facelock-daemon.service &>/dev/null; then
        systemctl restart facelock-daemon.service
        echo "Daemon restarted with TensorRT."
    fi

# Enable CUDA execution provider in installed config (requires root)
install-cuda-config:
    #!/usr/bin/env bash
    set -euo pipefail
    CONFIG="/etc/facelock/config.toml"
    [ -f "$CONFIG" ] || { echo "Error: $CONFIG not found. Run 'just install' first."; exit 1; }
    if grep -q '^execution_provider' "$CONFIG"; then
        sed -i 's/^execution_provider.*/execution_provider = "cuda"/' "$CONFIG"
    elif grep -q '^# execution_provider' "$CONFIG"; then
        sed -i 's/^# execution_provider.*/execution_provider = "cuda"/' "$CONFIG"
    else
        sed -i '/^\[recognition\]/a execution_provider = "cuda"' "$CONFIG"
    fi
    echo "CUDA execution provider enabled in $CONFIG"
    # Restart daemon to pick up new config
    if systemctl is-active facelock-daemon.service &>/dev/null; then
        systemctl restart facelock-daemon.service
        echo "Daemon restarted with CUDA."
    fi

# Uninstall from system
# Run as: just uninstall (elevates to root, preserving PATH)
uninstall:
    sudo env PATH="$PATH" just uninstall-files

# Uninstall files from system (requires root, called by uninstall)
uninstall-files:
    #!/usr/bin/env bash
    set -euo pipefail
    PAM_LINE="auth  sufficient  pam_facelock.so"

    # Stop and disable daemon
    systemctl stop facelock-daemon.service 2>/dev/null || true
    systemctl disable facelock-daemon.service 2>/dev/null || true

    # Remove PAM line
    if [ -f /etc/pam.d/sudo ]; then
        sed -i "\|^${PAM_LINE}$|d" /etc/pam.d/sudo
        echo "Removed face auth from /etc/pam.d/sudo"
    fi

    # Kill facelock polkit agent if running (so the DE's agent can take over)
    pkill -f facelock-polkit-agent 2>/dev/null || true

    # Remove binaries and units
    rm -f /usr/bin/facelock /lib/security/pam_facelock.so
    rm -f /usr/lib/systemd/system/facelock-daemon.service
    rm -f /usr/share/dbus-1/system.d/org.facelock.Daemon.conf
    rm -f /usr/share/dbus-1/system-services/org.facelock.Daemon.service
    rm -f /usr/bin/facelock-polkit-agent
    rm -f /etc/xdg/autostart/org.facelock.AuthAgent.desktop
    systemctl daemon-reload 2>/dev/null || true

    echo "Uninstalled. Config and data preserved in /etc/facelock and /var/lib/facelock."
    echo "To remove all data: rm -rf /etc/facelock /var/lib/facelock /var/log/facelock"

# Bump version and prepare a release commit + tag
# Usage: just release 0.2.0
release version:
    #!/usr/bin/env bash
    set -euo pipefail
    VERSION="{{version}}"

    # Validate version format
    if ! echo "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
        echo "Error: Version must be semver (e.g. 0.2.0), got '$VERSION'"
        exit 1
    fi

    # Check for clean working tree
    if [ -n "$(git status --porcelain)" ]; then
        echo "Error: Working tree is not clean. Commit or stash changes first."
        exit 1
    fi

    OLD_VERSION=$(grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/')
    echo "Bumping version: $OLD_VERSION → $VERSION"

    # 1. Cargo.toml (workspace version)
    sed -i "s/^version = \"$OLD_VERSION\"/version = \"$VERSION\"/" Cargo.toml
    echo "  ✓ Cargo.toml"

    # 2. dist/PKGBUILD
    if [ -f dist/PKGBUILD ]; then
        sed -i "s/^pkgver=.*/pkgver=$VERSION/" dist/PKGBUILD
        echo "  ✓ dist/PKGBUILD"
    fi

    # 3. dist/facelock.spec
    if [ -f dist/facelock.spec ]; then
        sed -i "s/^Version:.*/Version:        $VERSION/" dist/facelock.spec
        echo "  ✓ dist/facelock.spec"
    fi

    # 4. dist/debian/changelog (prepend new entry)
    if [ -f dist/debian/changelog ]; then
        DATE=$(date -R)
        sed -i "1i facelock ($VERSION-1) unstable; urgency=medium\n\n  * Release v$VERSION.\n\n -- Facelock Contributors <facelock@example.com>  $DATE\n" dist/debian/changelog
        echo "  ✓ dist/debian/changelog"
    fi

    # 5. Verify it compiles
    echo ""
    echo "Running cargo check..."
    cargo check --workspace
    echo "  ✓ cargo check passed"

    # 6. Remind to update CHANGELOG.md
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  Update CHANGELOG.md with the changes for v$VERSION"
    echo "  then run:"
    echo ""
    echo "    git add -A && git commit -m 'chore: release v$VERSION'"
    echo "    git tag v$VERSION"
    echo "    git push origin main --tags"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

# Show current version
version:
    @grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/'

# Clean build artifacts
clean:
    cargo clean

# Show installed file locations
show-paths:
    @echo "Binary:   /usr/bin/facelock"
    @echo "PAM:      /lib/security/pam_facelock.so"
    @echo "Config:   /etc/facelock/config.toml"
    @echo "Models:   /var/lib/facelock/models/"
    @echo "Database: /var/lib/facelock/facelock.db"
    @echo "D-Bus:    /usr/share/dbus-1/system.d/org.facelock.Daemon.conf"
    @echo "Service:  /usr/lib/systemd/system/facelock-daemon.service"
    @echo "Logs:     /var/log/facelock/"
