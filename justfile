# facelock build automation
# Usage: just <recipe>

# Build in debug mode (development)
build:
    cargo build --workspace

# Build in release mode (for install)
build-release:
    cargo build --release --workspace
    cargo build --release -p facelock-cli --features tpm

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
