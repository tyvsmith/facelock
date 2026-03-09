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
    # Create howdy system group if it doesn't exist
    getent group howdy >/dev/null || groupadd -r howdy
    # Install binaries
    install -Dm755 target/release/howdy /usr/bin/howdy
    install -Dm755 target/release/howdy-daemon /usr/bin/howdy-daemon
    install -Dm755 target/release/libpam_howdy.so /lib/security/pam_howdy.so
    # Config (don't overwrite existing)
    install -Dm644 config/howdy.toml /etc/howdy/config.toml.default
    @[ -f /etc/howdy/config.toml ] || cp /etc/howdy/config.toml.default /etc/howdy/config.toml
    # systemd service
    install -Dm644 systemd/howdy-daemon.service /usr/lib/systemd/system/howdy-daemon.service
    # Directories with restricted permissions (biometric data protection)
    install -dm755 -o root -g root /var/lib/howdy/models
    install -dm750 -o root -g howdy /var/lib/howdy
    install -dm750 -o root -g howdy /var/log/howdy/snapshots
    install -dm755 -o root -g howdy /run/howdy
    # Set model file permissions
    @[ -d /var/lib/howdy/models ] && chmod 644 /var/lib/howdy/models/*.onnx 2>/dev/null || true
    # Set database permissions if exists
    @[ -f /var/lib/howdy/howdy.db ] && chown root:howdy /var/lib/howdy/howdy.db && chmod 640 /var/lib/howdy/howdy.db || true

# Uninstall binaries only (keeps config and data)
uninstall:
    rm -f /usr/bin/howdy /usr/bin/howdy-daemon /lib/security/pam_howdy.so
    rm -f /usr/lib/systemd/system/howdy-daemon.service

# Clean build artifacts
clean:
    cargo clean
```

### Install Locations

| Artifact | Path | Notes |
|----------|------|-------|
| CLI binary | `/usr/bin/howdy` | |
| Daemon binary | `/usr/bin/howdy-daemon` | |
| PAM module | `/lib/security/pam_howdy.so` | |
| Config | `/etc/howdy/config.toml` | Not overwritten if exists |
| Config default | `/etc/howdy/config.toml.default` | Always updated |
| Models | `/var/lib/howdy/models/` | Created empty |
| Database | `/var/lib/howdy/howdy.db` | Created by daemon |
| Snapshots | `/var/log/howdy/snapshots/` | Created empty |
| Socket | `/run/howdy/howdy.sock` | Created by daemon |
| systemd | `/usr/lib/systemd/system/howdy-daemon.service` | |

### PKGBUILD (Arch Linux)

```bash
pkgname=howdy-rust
pkgver=0.1.0
pkgrel=1
pkgdesc="Face authentication for Linux (Rust rewrite)"
arch=('x86_64')
url="https://github.com/user/howdy-rust"
license=('MIT')
depends=('pam')
makedepends=('rust' 'cargo')
backup=('etc/howdy/config.toml')

build() {
    cd "$srcdir/$pkgname-$pkgver"
    cargo build --release
}

package() {
    cd "$srcdir/$pkgname-$pkgver"
    install -Dm755 target/release/howdy "$pkgdir/usr/bin/howdy"
    install -Dm755 target/release/howdy-daemon "$pkgdir/usr/bin/howdy-daemon"
    install -Dm755 target/release/libpam_howdy.so "$pkgdir/usr/lib/security/pam_howdy.so"
    install -Dm644 config/howdy.toml "$pkgdir/etc/howdy/config.toml"
    install -Dm644 systemd/howdy-daemon.service "$pkgdir/usr/lib/systemd/system/howdy-daemon.service"
    install -dm755 "$pkgdir/var/lib/howdy/models"
    install -dm755 "$pkgdir/var/log/howdy/snapshots"
}
```

### Post-Install Guide

```
1. Edit /etc/howdy/config.toml, set device.path to your IR camera
2. Run: sudo howdy setup              (downloads ONNX models)
3. Run: sudo systemctl enable --now howdy-daemon
4. Run: sudo howdy enroll             (capture your face)
5. Run: sudo howdy test               (verify recognition)
6. Add to PAM (CAREFULLY, see docs/testing-safety.md):
   Edit /etc/pam.d/sudo, add before other auth lines:
     auth  sufficient  pam_howdy.so
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
