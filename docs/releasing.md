# Releasing

## Versioning

Facelock uses [Semantic Versioning](https://semver.org/):

- **MAJOR** (`1.0.0`): Breaking changes to config format, database schema, D-Bus interface, or CLI flags
- **MINOR** (`0.2.0`): New features, non-breaking config additions
- **PATCH** (`0.1.1`): Bug fixes, documentation, dependency updates

The project is pre-1.0. The public contract is:

| Surface | What constitutes "breaking" |
|---------|---------------------------|
| Config (`/etc/facelock/config.toml`) | Removing or renaming keys, changing defaults that affect security |
| Database schema | Incompatible schema changes without migration |
| D-Bus interface (`org.facelock.Daemon`) | Removing methods, changing signatures |
| CLI flags | Removing subcommands or changing flag semantics |
| PAM behavior | Changing auth/ignore/deny semantics |

Rust crate APIs are internal and not part of the versioning contract.

## How to Release

### Automated (recommended)

```bash
just release 0.2.0
```

This will:
1. Bump version in `Cargo.toml`, `dist/PKGBUILD`, `dist/facelock.spec`, `dist/debian/changelog`
2. Run `cargo check --workspace` to verify the version bump compiles
3. Prompt you to update `CHANGELOG.md` (add entries under the new version heading)
4. Commit: `chore: release v0.2.0`
5. Tag: `v0.2.0`

Then push the tag to trigger the release workflow:

```bash
git push origin main --tags
```

### What happens on tag push

The `.github/workflows/release.yml` workflow:

1. Builds release binaries (with TPM feature)
2. Generates SHA256 checksums
3. Creates a GitHub Release with auto-generated release notes
4. Builds and uploads `.deb` package
5. Builds and uploads `.rpm` package

### Manual steps after release

- Update AUR package (see below)
- Update any external package repositories
- Announce on relevant channels

### AUR Update

After the GitHub Release is created (triggered by the tag push):

1. Download the release tarball and compute the checksum:
   ```bash
   curl -sL https://github.com/tyvsmith/facelock/archive/v$VERSION.tar.gz | sha256sum
   ```
2. Clone the AUR repo (first time only):
   ```bash
   git clone ssh://aur@aur.archlinux.org/facelock.git aur-facelock
   ```
3. Copy `dist/PKGBUILD` and `dist/facelock.install` into the AUR repo
4. Update `sha256sums` in the PKGBUILD with the real checksum from step 1
5. Generate `.SRCINFO`:
   ```bash
   cd aur-facelock
   makepkg --printsrcinfo > .SRCINFO
   ```
6. Commit and push to AUR:
   ```bash
   git add PKGBUILD facelock.install .SRCINFO
   git commit -m "Update to v$VERSION"
   git push
   ```

## Version Sources

The canonical version is in the root `Cargo.toml` under `[workspace.package]`.
All other files are synced by `just release`:

| File | Field |
|------|-------|
| `Cargo.toml` | `[workspace.package] version` |
| `dist/PKGBUILD` | `pkgver` |
| `dist/facelock.spec` | `Version` |
| `dist/debian/changelog` | Version in first entry |

## Upgrade Safety

Since facelock is a PAM module, broken releases can lock users out. Every release must:

1. Pass `just check` (tests + clippy + fmt)
2. Pass `just test-pam` (container PAM smoke tests)
3. Not change PAM auth semantics without explicit changelog entry
4. Preserve `/etc/pam.d/sudo` backup on install (`sudo.facelock-backup`)
5. Default to `PAM_IGNORE` on internal errors (fall through to password)
