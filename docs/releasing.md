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
4. Print the `git commit` / `git tag` / `git push` commands for you to run

Then push the tag to trigger the release workflow:

```bash
git push origin main --tags
```

### What happens on tag push

The `.github/workflows/release.yml` workflow:

1. Builds release binaries and creates a GitHub Release
2. Downloads ONNX Runtime for bundling in non-Arch packages
3. Builds `.deb` package — **TPM** (Debian trixie container) and **legacy** (Ubuntu 24.04)
4. Builds `.rpm` package in Fedora container (with TPM feature) and validates contents
5. Validates Nix flake evaluation
6. Publishes to AUR (if `AUR_SSH_KEY` secret is configured)
7. Triggers COPR rebuild (if `COPR_WEBHOOK_URL` secret is configured)
8. Publishes signed APT repository (if `APT_GPG_PRIVATE_KEY` and `APT_GPG_PASSPHRASE` are configured)
9. Triggers GitHub Pages rebuild to include updated APT repo

Pre-release tags (containing `alpha`, `beta`, or `rc`) skip AUR, COPR, and APT publishing.

#### Debian package channels

| Channel | Build env | TPM | Version suffix | Use case |
|---------|-----------|-----|----------------|----------|
| `main` | Debian trixie container | Yes | `X.Y.Z-1` | Modern systems (trixie+, Ubuntu 25.04+) |
| `legacy` | Ubuntu 24.04 runner | No | `X.Y.Z-1~legacy1` | Older systems (bookworm, Ubuntu 24.04) |

Both `.deb` packages are uploaded to the GitHub Release for direct download. For `apt install`, they are published to the signed APT repository at `https://tysmith.me/facelock/apt/`.

### Local distro validation

Before releasing, validate packages build and install correctly on each target:

```bash
# Automated (no camera needed)
just test-arch-pam       # Arch container PAM smoke tests
just test-rpm            # Fedora — validate file layout from manual install
just test-deb            # Ubuntu — validate file layout from manual install
just test-deb-pkg        # Ubuntu 24.04 — build real .deb, install via dpkg, validate
just test-deb-tpm-pkg    # Debian trixie — build real TPM .deb, install via dpkg, validate
just test-rpm-pkg        # Fedora — build real .rpm, install via dnf, validate

# Interactive (requires camera)
just test-deb-dev-shell      # Ubuntu .deb with host models — fast iteration
just test-rpm-dev-shell      # Fedora .rpm with host models — fast iteration
just test-deb-release-shell  # Ubuntu .deb clean room — real user experience
just test-rpm-release-shell  # Fedora .rpm clean room — real user experience
```

The `test-rpm` / `test-deb` recipes validate file layout from manually installed binaries.
The `*-pkg` recipes build real packages using the same scripts as CI, install them with
the actual package manager (`dnf` / `dpkg`), and validate the result — testing postinst
scripts, dependency resolution, ORT bundling, sysusers/tmpfiles triggers, and the full
install path.

The `*-dev-shell` recipes mount host models for fast interactive camera testing.
The `*-release-shell` recipes start from a clean package install with nothing from the
host — run `facelock setup` to download models, then enroll and test.

### Release preflight (recommended)

Run this before creating/pushing a release tag:

```bash
just release-preflight              # stable release checks
just release-preflight v0.2.0-rc1  # prerelease checks (AUR/COPR secrets optional)
just check
just test-arch-pam
just test-rpm
just test-deb
just test-deb-pkg
just test-deb-tpm-pkg
just test-rpm-pkg
```

`just release-preflight` checks local tools, required packaging files, and whether
`AUR_SSH_KEY`, `COPR_WEBHOOK_URL`, `APT_GPG_PRIVATE_KEY`, and `APT_GPG_PASSPHRASE` are configured in GitHub secrets (via `gh`).

### Package repository setup (one-time)

#### AUR (Arch Linux)

Automated after setup. The release workflow publishes to AUR when `AUR_SSH_KEY` is configured.

**One-time setup (~10 minutes):**

1. Create an AUR account at https://aur.archlinux.org/register
2. Add your SSH public key to your AUR account at https://aur.archlinux.org/account
3. Register the package names:
   ```bash
   REPO_ROOT="$(pwd)"

   git clone ssh://aur@aur.archlinux.org/facelock.git aur-facelock
   cd aur-facelock
   cp "$REPO_ROOT/dist/PKGBUILD" .
   cp "$REPO_ROOT/dist/facelock.install" .
   makepkg --printsrcinfo > .SRCINFO
   git add PKGBUILD facelock.install .SRCINFO
   git commit -m "Initial commit"
   git push
   cd ..

   git clone ssh://aur@aur.archlinux.org/facelock-git.git aur-facelock-git
   cd aur-facelock-git
   cp "$REPO_ROOT/dist/PKGBUILD-git" PKGBUILD
   cp "$REPO_ROOT/dist/facelock.install" .
   makepkg --printsrcinfo > .SRCINFO
   git add PKGBUILD facelock.install .SRCINFO
   git commit -m "Initial commit"
   git push
   ```
4. Generate an SSH key for CI and add the **public** key to your AUR account:
   ```bash
   ssh-keygen -t ed25519 -f aur-deploy-key -N ""
   ```
5. Add the **private** key as a GitHub repository secret named `AUR_SSH_KEY`:
   ```bash
   gh secret set AUR_SSH_KEY < aur-deploy-key
   ```

   Or use the web UI: https://github.com/tyvsmith/facelock/settings/secrets/actions

After this, every non-prerelease tag push automatically updates the AUR package.

#### COPR (Fedora/RHEL)

Automated after setup. The release workflow triggers a COPR rebuild when `COPR_WEBHOOK_URL` is configured.

**One-time setup (~10 minutes):**

1. Create a Fedora Account at https://accounts.fedoraproject.org
2. Log in to COPR at https://copr.fedorainfracloud.org
3. Create a new project:
   - Name: `facelock`
   - Chroots: `fedora-rawhide-x86_64`, `fedora-41-x86_64`, `fedora-40-x86_64`
   - SCM source type: select "custom"
4. Under project settings → Webhooks, copy the webhook URL
   - Use the **Custom webhook** URL with package suffix, e.g. `.../webhooks/custom/<ID>/<UUID>/facelock/`
5. Add it as a GitHub repository secret named `COPR_WEBHOOK_URL`:
   ```bash
   gh secret set COPR_WEBHOOK_URL --body "$COPR_WEBHOOK_URL"
   ```

   Or use the web UI: https://github.com/tyvsmith/facelock/settings/secrets/actions

Alternatively, configure COPR to build from the GitHub source directly using the SCM integration (points at `dist/facelock.spec`).

#### APT (Debian/Ubuntu)

Automated after setup. The release workflow publishes a signed APT repository to GitHub Pages when `APT_GPG_PRIVATE_KEY` and `APT_GPG_PASSPHRASE` are configured.

**One-time setup (~15 minutes):**

1. Generate a GPG signing key (if you don't have one):
   ```bash
   gpg --full-generate-key
   # Select RSA 4096, expiry 3y
   # UID: Ty Smith Package Signing <packages@tysmith.me>
   ```

2. Export and add the private key as a GitHub secret:
   ```bash
   gpg --armor --export-secret-keys "packages@tysmith.me" | gh secret set APT_GPG_PRIVATE_KEY
   ```

3. Add the passphrase as a GitHub secret:
   ```bash
   gh secret set APT_GPG_PASSPHRASE --body "your-passphrase"
   ```

   Or use the web UI: https://github.com/tyvsmith/facelock/settings/secrets/actions

The repository configuration lives in `dist/apt/conf/distributions`. Two suites are published:

- **`main`**: TPM-enabled build (Debian trixie+, Ubuntu 25.04+)
- **`legacy`**: Non-TPM build (Ubuntu 24.04, Debian bookworm)

The APT repo is hosted at `https://tysmith.me/facelock/apt/` alongside the docs site. The public keyring is at `https://tysmith.me/facelock/apt/tysmith-archive-keyring.gpg`.

**GPG key rotation**: When the signing key expires, generate a new key, update the `APT_GPG_PRIVATE_KEY` and `APT_GPG_PASSPHRASE` secrets, and cut a new release. The public keyring is re-exported on every release, so users who re-fetch it will get the updated key.

#### Manual AUR update (fallback)

If CI is not configured or fails:

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

## ONNX Runtime Bundling

The `.deb` and `.rpm` packages bundle a CPU-only `libonnxruntime.so` at `/usr/lib/facelock/libonnxruntime.so`. This is necessary because ONNX Runtime is not available in Fedora or Ubuntu repositories.

On Arch Linux, the PKGBUILD depends on the system `onnxruntime` package instead (available in official repos). The bundled ORT is a CPU-only fallback — users who install a system-wide GPU-enabled ONNX Runtime (CUDA, ROCm, OpenVINO) will have it take precedence automatically (the search order prefers system paths over the bundled copy).

The bundled ORT version is pinned in `.github/workflows/release.yml` as `ORT_VERSION`. Update it when upgrading the `ort` crate dependency.

## Upgrade Safety

Since facelock is a PAM module, broken releases can lock users out. Every release must:

1. Pass `just check` (tests + clippy + fmt)
2. Pass `just test-arch-pam` (Arch container PAM smoke tests)
3. Pass `just test-rpm` and `just test-deb` (multi-distro package validation)
4. Not change PAM auth semantics without explicit changelog entry
5. Preserve `/etc/pam.d/sudo` backup on install (`sudo.facelock-backup`)
6. Default to `PAM_IGNORE` on internal errors (fall through to password)
