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

    # Fix permissions on existing data
    [ -d /var/lib/facelock/models ] && chmod 644 /var/lib/facelock/models/*.onnx 2>/dev/null || true
    [ -f /var/lib/facelock/facelock.db ] && chown root:facelock /var/lib/facelock/facelock.db && chmod 640 /var/lib/facelock/facelock.db || true

    echo ""
    echo ""

    # Check what's still needed
    NEEDS_SETUP=false
    NEEDS_ORT=false

    # Models present?
    if ! ls /var/lib/facelock/models/*.onnx >/dev/null 2>&1; then
        NEEDS_SETUP=true
    fi

    # Config present?
    if [ ! -f /etc/facelock/config.toml ]; then
        NEEDS_SETUP=true
    fi

    # PAM configured?
    if ! grep -qs pam_facelock /etc/pam.d/sudo 2>/dev/null; then
        NEEDS_SETUP=true
    fi

    # ORT installed? Check file paths directly.
    if [ ! -f /usr/lib/libonnxruntime.so ] && \
       [ ! -f /usr/lib64/libonnxruntime.so ] && \
       [ ! -f /usr/lib/facelock/libonnxruntime.so ]; then
        NEEDS_ORT=true
    fi

    if $NEEDS_SETUP || $NEEDS_ORT; then
        echo "Installed."
        if $NEEDS_ORT; then
            echo ""
            echo "Requires: onnxruntime (pacman -S onnxruntime-cpu)"
            echo "Optional: onnxruntime-opt-cuda (NVIDIA) or onnxruntime-opt-rocm (AMD)"
        fi
        if $NEEDS_SETUP; then
            echo ""
            echo "Run 'sudo facelock setup' to complete configuration."
            echo "  (downloads models, configures PAM services, enrolls your face)"
        fi
    else
        echo "Installed and up to date."
    fi

# Uninstall from system
# Run as: just uninstall (elevates to root, preserving PATH)
uninstall:
    sudo env PATH="$PATH" just uninstall-files

# Uninstall files from system (requires root, called by uninstall)
uninstall-files:
    #!/usr/bin/env bash
    set -euo pipefail
    # Stop and disable daemon
    systemctl stop facelock-daemon.service 2>/dev/null || true
    systemctl disable facelock-daemon.service 2>/dev/null || true

    # Remove PAM lines from all known services (match on module name, not exact spacing)
    for PAM_FILE in /etc/pam.d/sudo /etc/pam.d/polkit-1 /etc/pam.d/hyprlock; do
        if [ -f "$PAM_FILE" ] && grep -q 'pam_facelock\.so' "$PAM_FILE"; then
            sed -i '/pam_facelock\.so/d' "$PAM_FILE"
            echo "Removed face auth from $PAM_FILE"
        fi
    done

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

# Add face auth icon to omarchy hyprlock placeholder text (no root required)
omarchy-enable:
    #!/usr/bin/env bash
    set -euo pipefail
    HYPRLOCK_CONF="$HOME/.config/hypr/hyprlock.conf"

    if [ ! -d "$HOME/.local/share/omarchy" ]; then
        echo "Error: omarchy not detected. This target is for omarchy systems only."
        exit 1
    fi

    if [ ! -f "$HYPRLOCK_CONF" ]; then
        echo "Error: $HYPRLOCK_CONF not found."
        exit 1
    fi

    if grep -q '󰄀' "$HYPRLOCK_CONF"; then
        echo "Face auth icon already present in hyprlock config."
        exit 0
    fi

    # Preserve fingerprint icon if present
    if grep -q '󰈷' "$HYPRLOCK_CONF"; then
        sed -i 's/placeholder_text = .*/placeholder_text = <span> Enter Password 󰄀 󰈷 <\/span>/' "$HYPRLOCK_CONF"
    else
        sed -i 's/placeholder_text = .*/placeholder_text = <span> Enter Password 󰄀 <\/span>/' "$HYPRLOCK_CONF"
    fi

    # Source the faceauth overlay if not already sourced
    if [ -f "$HOME/.config/hypr/hyprlock-faceauth.conf" ] && ! grep -q 'hyprlock-faceauth.conf' "$HYPRLOCK_CONF"; then
        echo 'source = ~/.config/hypr/hyprlock-faceauth.conf' >> "$HYPRLOCK_CONF"
    fi

    echo "Enabled face auth in hyprlock."

# Remove face auth icon from omarchy hyprlock placeholder text (no root required)
omarchy-disable:
    #!/usr/bin/env bash
    set -euo pipefail
    HYPRLOCK_CONF="$HOME/.config/hypr/hyprlock.conf"

    if [ ! -f "$HYPRLOCK_CONF" ]; then
        echo "Error: $HYPRLOCK_CONF not found."
        exit 1
    fi

    if ! grep -q '󰄀' "$HYPRLOCK_CONF"; then
        echo "Face auth icon not present in hyprlock config."
        exit 0
    fi

    # Preserve fingerprint icon if present
    if grep -q '󰈷' "$HYPRLOCK_CONF"; then
        sed -i 's/placeholder_text = .*/placeholder_text = <span> Enter Password 󰈷 <\/span>/' "$HYPRLOCK_CONF"
    else
        sed -i 's/placeholder_text = .*/placeholder_text = Enter Password/' "$HYPRLOCK_CONF"
    fi

    # Remove the faceauth overlay source line
    sed -i '\|hyprlock-faceauth.conf|d' "$HYPRLOCK_CONF"

    echo "Disabled face auth in hyprlock."

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
