# Spec 10: Build & Installation

**Phase**: 5 (Polish) | **Depends on**: all prior | **Parallel with**: 08, 09

## Goal

Build automation, system installation, and Arch Linux packaging.

## Deliverables

### justfile (Build Automation)

```makefile
# Build all crates in release mode
build:
    cargo build --release

# Run all unit tests
test:
    cargo test --workspace

# Run clippy with warnings as errors
lint:
    cargo clippy --workspace -- -D warnings

# Install binaries, PAM module, config, and systemd service (requires root)
install: build
    # Create visage system group if it doesn't exist
    getent group visage >/dev/null || groupadd -r visage
    # Install binaries
    install -Dm755 target/release/visage /usr/bin/visage
    install -Dm755 target/release/visage-daemon /usr/bin/visage-daemon
    install -Dm755 target/release/libpam_visage.so /lib/security/pam_visage.so
    # Config (don't overwrite existing)
    install -Dm644 config/visage.toml /etc/visage/config.toml.default
    @[ -f /etc/visage/config.toml ] || cp /etc/visage/config.toml.default /etc/visage/config.toml
    # systemd service
    install -Dm644 systemd/visage-daemon.service /usr/lib/systemd/system/visage-daemon.service
    # Directories with restricted permissions (biometric data protection)
    install -dm755 -o root -g root /var/lib/visage/models
    install -dm750 -o root -g visage /var/lib/visage
    install -dm750 -o root -g visage /var/log/visage/snapshots
    install -dm755 -o root -g visage /run/visage
    # Set model file permissions
    @[ -d /var/lib/visage/models ] && chmod 644 /var/lib/visage/models/*.onnx 2>/dev/null || true
    # Set database permissions if exists
    @[ -f /var/lib/visage/visage.db ] && chown root:visage /var/lib/visage/visage.db && chmod 640 /var/lib/visage/visage.db || true

# Uninstall binaries only (keeps config and data)
uninstall:
    rm -f /usr/bin/visage /usr/bin/visage-daemon /lib/security/pam_visage.so
    rm -f /usr/lib/systemd/system/visage-daemon.service

# Clean build artifacts
clean:
    cargo clean
```

### Install Locations

| Artifact | Path | Notes |
|----------|------|-------|
| CLI binary | `/usr/bin/visage` | |
| Daemon binary | `/usr/bin/visage-daemon` | |
| PAM module | `/lib/security/pam_visage.so` | |
| Config | `/etc/visage/config.toml` | Not overwritten if exists |
| Config default | `/etc/visage/config.toml.default` | Always updated |
| Models | `/var/lib/visage/models/` | Created empty |
| Database | `/var/lib/visage/visage.db` | Created by daemon |
| Snapshots | `/var/log/visage/snapshots/` | Created empty |
| Socket | `/run/visage/visage.sock` | Created by daemon |
| systemd | `/usr/lib/systemd/system/visage-daemon.service` | |

### PKGBUILD (Arch Linux)

```bash
pkgname=visage
pkgver=0.1.0
pkgrel=1
pkgdesc="Face authentication for Linux (Rust rewrite)"
arch=('x86_64')
url="https://github.com/user/visage"
license=('MIT')
depends=('pam')
makedepends=('rust' 'cargo')
backup=('etc/visage/config.toml')

build() {
    cd "$srcdir/$pkgname-$pkgver"
    cargo build --release
}

package() {
    cd "$srcdir/$pkgname-$pkgver"
    install -Dm755 target/release/visage "$pkgdir/usr/bin/visage"
    install -Dm755 target/release/visage-daemon "$pkgdir/usr/bin/visage-daemon"
    install -Dm755 target/release/libpam_visage.so "$pkgdir/usr/lib/security/pam_visage.so"
    install -Dm644 config/visage.toml "$pkgdir/etc/visage/config.toml"
    install -Dm644 systemd/visage-daemon.service "$pkgdir/usr/lib/systemd/system/visage-daemon.service"
    install -dm755 "$pkgdir/var/lib/visage/models"
    install -dm755 "$pkgdir/var/log/visage/snapshots"
}
```

### Post-Install Guide

```
1. Edit /etc/visage/config.toml, set device.path to your IR camera
2. Run: sudo visage setup              (downloads ONNX models)
3. Run: sudo systemctl enable --now visage-daemon
4. Run: sudo visage enroll             (capture your face)
5. Run: sudo visage test               (verify recognition)
6. Add to PAM (CAREFULLY, see docs/testing-safety.md):
   Edit /etc/pam.d/sudo, add before other auth lines:
     auth  sufficient  pam_visage.so
```

## Acceptance Criteria

1. `just build` produces all binaries
2. `just test` runs all unit tests
3. `just lint` passes
4. `just install` places files in correct locations
5. Config not overwritten if already exists
6. PKGBUILD builds and packages correctly

## Verification

```bash
just build
just test
just lint
```
