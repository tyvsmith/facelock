# visage build automation
# Usage: just <recipe>

# Build all crates in release mode
build:
    cargo build --release

# Build in debug mode
build-debug:
    cargo build --workspace

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
test-pam: build
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
test-integration: build
    #!/usr/bin/env bash
    set -euo pipefail
    podman build -t visage-pam-test -f test/Containerfile .
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    podman run --rm $devices visage-pam-test /run-integration-tests.sh

# Open interactive shell in PAM test container (requires camera)
test-shell: build
    #!/usr/bin/env bash
    set -euo pipefail
    podman build -t visage-pam-test -f test/Containerfile .
    devices=""
    for d in /dev/video*; do
        [ -e "$d" ] && devices="$devices --device $d"
    done
    echo "Starting interactive shell. Try:"
    echo "  visage-daemon &"
    echo "  sleep 2"
    echo "  visage enroll --user testuser --label myface"
    echo "  visage test --user testuser"
    echo "  pamtester visage-test testuser authenticate"
    podman run --rm -it $devices visage-pam-test /bin/bash

# Install binaries, PAM module, config, and systemd service (requires root)
install: build
    #!/usr/bin/env bash
    set -euo pipefail

    # Create visage system group if it doesn't exist
    getent group visage >/dev/null || groupadd -r visage

    # Install binaries
    install -Dm755 target/release/visage /usr/bin/visage
    install -Dm755 target/release/visage-daemon /usr/bin/visage-daemon
    install -Dm755 target/release/libpam_visage.so /lib/security/pam_visage.so

    # Config (don't overwrite existing)
    install -Dm644 config/visage.toml /etc/visage/config.toml.default
    [ -f /etc/visage/config.toml ] || cp /etc/visage/config.toml.default /etc/visage/config.toml

    # systemd units
    install -Dm644 systemd/visage-daemon.service /usr/lib/systemd/system/visage-daemon.service
    install -Dm644 systemd/visage-daemon.socket /usr/lib/systemd/system/visage-daemon.socket

    # Directories with restricted permissions (biometric data protection)
    # Order matters: parent first with restrictive perms, then children
    install -dm750 -o root -g visage /var/lib/visage
    install -dm755 -o root -g root /var/lib/visage/models
    install -dm750 -o root -g visage /var/log/visage
    install -dm750 -o root -g visage /var/log/visage/snapshots
    install -dm755 -o root -g visage /run/visage

    # Set model file permissions if models exist
    [ -d /var/lib/visage/models ] && chmod 644 /var/lib/visage/models/*.onnx 2>/dev/null || true

    # Set database permissions if exists
    [ -f /var/lib/visage/visage.db ] && chown root:visage /var/lib/visage/visage.db && chmod 640 /var/lib/visage/visage.db || true

    echo "Installation complete."
    echo "Next steps:"
    echo "  1. Edit /etc/visage/config.toml (optional — camera auto-detected)"
    echo "  2. Run: sudo visage setup              (downloads ONNX models)"
    echo "  3. Run: sudo systemctl enable --now visage-daemon.socket"
    echo "  4. Run: sudo visage enroll             (capture your face)"
    echo "  5. Run: sudo visage test               (verify recognition)"

# Uninstall binaries only (keeps config and data)
uninstall:
    rm -f /usr/bin/visage /usr/bin/visage-daemon /lib/security/pam_visage.so
    rm -f /usr/lib/systemd/system/visage-daemon.service
    @echo "Binaries and service removed. Config and data preserved in /etc/visage and /var/lib/visage."

# Clean build artifacts
clean:
    cargo clean

# Show installed file locations
show-paths:
    @echo "Binary:   /usr/bin/visage"
    @echo "Daemon:   /usr/bin/visage-daemon"
    @echo "PAM:      /lib/security/pam_visage.so"
    @echo "Config:   /etc/visage/config.toml"
    @echo "Models:   /var/lib/visage/models/"
    @echo "Database: /var/lib/visage/visage.db"
    @echo "Socket:   /run/visage/visage.sock"
    @echo "Service:  /usr/lib/systemd/system/visage-daemon.service"
    @echo "Logs:     /var/log/visage/"
