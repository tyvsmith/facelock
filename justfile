# visage build automation
# Usage: just <recipe>

# Build in debug mode (development)
build:
    cargo build --workspace

# Build in release mode (for install)
build-release:
    cargo build --release --workspace

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
        podman build -t visage-pam-test -f test/Containerfile .
        podman run --rm visage-pam-test
    else
        echo "No test/Containerfile found"
        exit 1
    fi

# Run end-to-end integration tests in container (requires camera)
test-integration: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    podman build -t visage-pam-test -f test/Containerfile .
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    podman run --rm $devices visage-pam-test /run-integration-tests.sh

# Run oneshot (daemonless) end-to-end tests in container (requires camera)
test-oneshot: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    podman build -t visage-pam-test -f test/Containerfile .
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    podman run --rm $devices visage-pam-test /run-oneshot-tests.sh

# Open interactive shell in PAM test container (requires camera)
test-shell: build-release
    #!/usr/bin/env bash
    set -euo pipefail
    podman build -t visage-pam-test -f test/Containerfile .
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    echo "Starting interactive shell. Try:"
    echo "  visage daemon &"
    echo "  sleep 2"
    echo "  visage enroll --user testuser --label myface"
    echo "  visage test --user testuser"
    echo "  pamtester visage-test testuser authenticate"
    podman run --rm -it $devices visage-pam-test /bin/bash

# Build release and install to system
# Run as: just install (builds as you, installs as root)
install: build-release
    sudo env PATH="$PATH" just install-files

# Install pre-built binaries to system (requires root, no build)
install-files:
    #!/usr/bin/env bash
    set -euo pipefail
    PAM_LINE="auth  sufficient  pam_visage.so"

    # Verify binaries exist
    for f in target/release/visage target/release/libpam_visage.so; do
        [ -f "$f" ] || { echo "Error: $f not found. Run 'just build-release' first."; exit 1; }
    done

    # Create visage system group and add the installing user
    getent group visage >/dev/null || groupadd -r visage
    REAL_USER="${SUDO_USER:-${DOAS_USER:-}}"
    if [ -n "$REAL_USER" ] && ! id -nG "$REAL_USER" 2>/dev/null | grep -qw visage; then
        usermod -aG visage "$REAL_USER"
        echo "Added $REAL_USER to visage group (log out and back in to take effect)."
    fi

    # Binaries
    install -Dm755 target/release/visage /usr/bin/visage
    install -Dm755 target/release/libpam_visage.so /lib/security/pam_visage.so

    # Config (don't overwrite existing)
    install -Dm644 config/visage.toml /etc/visage/config.toml.default
    [ -f /etc/visage/config.toml ] || cp /etc/visage/config.toml.default /etc/visage/config.toml

    # systemd units
    install -Dm644 systemd/visage-daemon.service /usr/lib/systemd/system/visage-daemon.service
    install -Dm644 systemd/visage-daemon.socket /usr/lib/systemd/system/visage-daemon.socket

    # Directories
    install -dm770 -o root -g visage /var/lib/visage
    install -dm755 -o root -g root /var/lib/visage/models
    install -dm750 -o root -g visage /var/log/visage
    install -dm750 -o root -g visage /var/log/visage/snapshots
    install -dm755 -o root -g visage /run/visage

    # Enable socket activation (if systemd present)
    if [ -d /run/systemd/system ]; then
        systemctl stop visage-daemon.service visage-daemon.socket 2>/dev/null || true
        systemctl daemon-reload
        systemctl reset-failed visage-daemon.service 2>/dev/null || true
        systemctl enable --now visage-daemon.socket 2>/dev/null || true
        echo "Socket activation enabled."
    fi

    # Add to /etc/pam.d/sudo (if not already present)
    if [ -f /etc/pam.d/sudo ] && ! grep -qF "$PAM_LINE" /etc/pam.d/sudo; then
        cp /etc/pam.d/sudo /etc/pam.d/sudo.visage-backup
        sed -i "0,/^auth/{s/^auth/${PAM_LINE}\nauth/}" /etc/pam.d/sudo
        echo "Added face auth to /etc/pam.d/sudo (backup: /etc/pam.d/sudo.visage-backup)"
    fi

    # Fix permissions on existing data
    [ -d /var/lib/visage/models ] && chmod 644 /var/lib/visage/models/*.onnx 2>/dev/null || true
    [ -f /var/lib/visage/visage.db ] && chown root:visage /var/lib/visage/visage.db && chmod 640 /var/lib/visage/visage.db || true

    echo ""
    echo "Installed. Two steps remaining:"
    echo "  1. sudo visage setup       (download face recognition models)"
    echo "  2. sudo visage enroll      (register your face)"

# Uninstall (requires root)
uninstall:
    #!/usr/bin/env bash
    set -euo pipefail
    PAM_LINE="auth  sufficient  pam_visage.so"

    # Stop and disable daemon
    systemctl stop visage-daemon.socket visage-daemon 2>/dev/null || true
    systemctl disable visage-daemon.socket visage-daemon 2>/dev/null || true

    # Remove PAM line
    if [ -f /etc/pam.d/sudo ]; then
        sed -i "\|^${PAM_LINE}$|d" /etc/pam.d/sudo
        echo "Removed face auth from /etc/pam.d/sudo"
    fi

    # Remove binaries and units
    rm -f /usr/bin/visage /lib/security/pam_visage.so
    rm -f /usr/lib/systemd/system/visage-daemon.service
    rm -f /usr/lib/systemd/system/visage-daemon.socket
    systemctl daemon-reload 2>/dev/null || true

    echo "Uninstalled. Config and data preserved in /etc/visage and /var/lib/visage."
    echo "To remove all data: rm -rf /etc/visage /var/lib/visage /var/log/visage"

# Clean build artifacts
clean:
    cargo clean

# Show installed file locations
show-paths:
    @echo "Binary:   /usr/bin/visage"
    @echo "PAM:      /lib/security/pam_visage.so"
    @echo "Config:   /etc/visage/config.toml"
    @echo "Models:   /var/lib/visage/models/"
    @echo "Database: /var/lib/visage/visage.db"
    @echo "Socket:   /run/visage/visage.sock"
    @echo "Service:  /usr/lib/systemd/system/visage-daemon.service"
    @echo "Logs:     /var/log/visage/"
