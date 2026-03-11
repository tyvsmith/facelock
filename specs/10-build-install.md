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
    # Create facelock system group if it doesn't exist
    getent group facelock >/dev/null || groupadd -r facelock
    # Install binaries
    install -Dm755 target/release/facelock /usr/bin/facelock
    install -Dm755 target/release/facelock-daemon /usr/bin/facelock-daemon
    install -Dm755 target/release/libpam_facelock.so /lib/security/pam_facelock.so
    # Config (don't overwrite existing)
    install -Dm644 config/facelock.toml /etc/facelock/config.toml.default
    @[ -f /etc/facelock/config.toml ] || cp /etc/facelock/config.toml.default /etc/facelock/config.toml
    # systemd service
    install -Dm644 systemd/facelock-daemon.service /usr/lib/systemd/system/facelock-daemon.service
    # Directories with restricted permissions (biometric data protection)
    install -dm755 -o root -g root /var/lib/facelock/models
    install -dm750 -o root -g facelock /var/lib/facelock
    install -dm750 -o root -g facelock /var/log/facelock/snapshots
    install -dm755 -o root -g facelock /run/facelock
    # Set model file permissions
    @[ -d /var/lib/facelock/models ] && chmod 644 /var/lib/facelock/models/*.onnx 2>/dev/null || true
    # Set database permissions if exists
    @[ -f /var/lib/facelock/facelock.db ] && chown root:facelock /var/lib/facelock/facelock.db && chmod 640 /var/lib/facelock/facelock.db || true

# Uninstall binaries only (keeps config and data)
uninstall:
    rm -f /usr/bin/facelock /usr/bin/facelock-daemon /lib/security/pam_facelock.so
    rm -f /usr/lib/systemd/system/facelock-daemon.service

# Clean build artifacts
clean:
    cargo clean
```

### Install Locations

| Artifact | Path | Notes |
|----------|------|-------|
| CLI binary | `/usr/bin/facelock` | |
| Daemon binary | `/usr/bin/facelock-daemon` | |
| PAM module | `/lib/security/pam_facelock.so` | |
| Config | `/etc/facelock/config.toml` | Not overwritten if exists |
| Config default | `/etc/facelock/config.toml.default` | Always updated |
| Models | `/var/lib/facelock/models/` | Created empty |
| Database | `/var/lib/facelock/facelock.db` | Created by daemon |
| Snapshots | `/var/log/facelock/snapshots/` | Created empty |
| Socket | `/run/facelock/facelock.sock` | Created by daemon |
| systemd | `/usr/lib/systemd/system/facelock-daemon.service` | |

### PKGBUILD (Arch Linux)

```bash
pkgname=facelock
pkgver=0.1.0
pkgrel=1
pkgdesc="Face authentication for Linux (Rust rewrite)"
arch=('x86_64')
url="https://github.com/user/facelock"
license=('MIT')
depends=('pam')
makedepends=('rust' 'cargo')
backup=('etc/facelock/config.toml')

build() {
    cd "$srcdir/$pkgname-$pkgver"
    cargo build --release
}

package() {
    cd "$srcdir/$pkgname-$pkgver"
    install -Dm755 target/release/facelock "$pkgdir/usr/bin/facelock"
    install -Dm755 target/release/facelock-daemon "$pkgdir/usr/bin/facelock-daemon"
    install -Dm755 target/release/libpam_facelock.so "$pkgdir/usr/lib/security/pam_facelock.so"
    install -Dm644 config/facelock.toml "$pkgdir/etc/facelock/config.toml"
    install -Dm644 systemd/facelock-daemon.service "$pkgdir/usr/lib/systemd/system/facelock-daemon.service"
    install -dm755 "$pkgdir/var/lib/facelock/models"
    install -dm755 "$pkgdir/var/log/facelock/snapshots"
}
```

### Post-Install Guide

```
1. Edit /etc/facelock/config.toml, set device.path to your IR camera
2. Run: sudo facelock setup              (downloads ONNX models)
3. Run: sudo systemctl enable --now facelock-daemon
4. Run: sudo facelock enroll             (capture your face)
5. Run: sudo facelock test               (verify recognition)
6. Add to PAM (CAREFULLY, see docs/testing-safety.md):
   Edit /etc/pam.d/sudo, add before other auth lines:
     auth  sufficient  pam_facelock.so
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
