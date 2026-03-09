# howdy-rust build automation
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

# Install binaries, PAM module, config, and systemd service (requires root)
install: build
    #!/usr/bin/env bash
    set -euo pipefail

    # Create howdy system group if it doesn't exist
    getent group howdy >/dev/null || groupadd -r howdy

    # Install binaries
    install -Dm755 target/release/howdy /usr/bin/howdy
    install -Dm755 target/release/howdy-daemon /usr/bin/howdy-daemon
    install -Dm755 target/release/libpam_howdy.so /lib/security/pam_howdy.so

    # Config (don't overwrite existing)
    install -Dm644 config/howdy.toml /etc/howdy/config.toml.default
    [ -f /etc/howdy/config.toml ] || cp /etc/howdy/config.toml.default /etc/howdy/config.toml

    # systemd service
    install -Dm644 systemd/howdy-daemon.service /usr/lib/systemd/system/howdy-daemon.service

    # Directories with restricted permissions (biometric data protection)
    # Order matters: parent first with restrictive perms, then children
    install -dm750 -o root -g howdy /var/lib/howdy
    install -dm755 -o root -g root /var/lib/howdy/models
    install -dm750 -o root -g howdy /var/log/howdy
    install -dm750 -o root -g howdy /var/log/howdy/snapshots
    install -dm755 -o root -g howdy /run/howdy

    # Set model file permissions if models exist
    [ -d /var/lib/howdy/models ] && chmod 644 /var/lib/howdy/models/*.onnx 2>/dev/null || true

    # Set database permissions if exists
    [ -f /var/lib/howdy/howdy.db ] && chown root:howdy /var/lib/howdy/howdy.db && chmod 640 /var/lib/howdy/howdy.db || true

    echo "Installation complete."
    echo "Next steps:"
    echo "  1. Edit /etc/howdy/config.toml (set device.path to your IR camera)"
    echo "  2. Run: sudo howdy setup              (downloads ONNX models)"
    echo "  3. Run: sudo systemctl enable --now howdy-daemon"
    echo "  4. Run: sudo howdy enroll             (capture your face)"
    echo "  5. Run: sudo howdy test               (verify recognition)"

# Uninstall binaries only (keeps config and data)
uninstall:
    rm -f /usr/bin/howdy /usr/bin/howdy-daemon /lib/security/pam_howdy.so
    rm -f /usr/lib/systemd/system/howdy-daemon.service
    @echo "Binaries and service removed. Config and data preserved in /etc/howdy and /var/lib/howdy."

# Clean build artifacts
clean:
    cargo clean

# Show installed file locations
show-paths:
    @echo "Binary:   /usr/bin/howdy"
    @echo "Daemon:   /usr/bin/howdy-daemon"
    @echo "PAM:      /lib/security/pam_howdy.so"
    @echo "Config:   /etc/howdy/config.toml"
    @echo "Models:   /var/lib/howdy/models/"
    @echo "Database: /var/lib/howdy/howdy.db"
    @echo "Socket:   /run/howdy/howdy.sock"
    @echo "Service:  /usr/lib/systemd/system/howdy-daemon.service"
    @echo "Logs:     /var/log/howdy/"
