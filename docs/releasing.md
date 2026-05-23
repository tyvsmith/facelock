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
1. Bump version in `Cargo.toml`, `dist/PKGBUILD`, `dist/PKGBUILD-bin`, `dist/facelock.spec`, `dist/debian/changelog`
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
6. Publishes to AUR — `facelock` (source build), `facelock-bin` (prebuilt), and `facelock-git` (VCS) — if `AUR_SSH_KEY` secret is configured
7. Publishes signed APT repository (if `APT_GPG_PRIVATE_KEY` and `APT_GPG_PASSPHRASE` are configured)
8. Triggers GitHub Pages rebuild to include updated APT repo

Pre-release tags (containing `alpha`, `beta`, or `rc`) skip AUR and APT publishing.

COPR (Fedora) is **not** built by `release.yml`. It is handled by [Packit](https://packit.dev),
which reacts to the GitHub Release published in step 1. See the COPR section below.

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
just test-copr           # COPR-equivalent — Packit SRPM + mock from-source rebuild (slow)

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

`just release-preflight` checks local tools, required packaging files (including
`.packit.yaml`), and whether `AUR_SSH_KEY`, `APT_GPG_PRIVATE_KEY`, and
`APT_GPG_PASSPHRASE` are configured in GitHub secrets (via `gh`). COPR needs no
secret — it is driven by Packit.

### Package repository setup (one-time)

#### AUR (Arch Linux)

Automated after setup. The release workflow publishes to AUR when `AUR_SSH_KEY` is configured.

**One-time setup (~10 minutes):**

1. Create an AUR account at https://aur.archlinux.org/register
2. Add your SSH public key to your AUR account at https://aur.archlinux.org/account
3. Register the package names. CI's `publish-aur.sh` will create any of
   these on first push if they don't already exist, but you can also pre-register
   them manually:
   ```bash
   REPO_ROOT="$(pwd)"

   # facelock (source build — default for `yay -S facelock`)
   git clone ssh://aur@aur.archlinux.org/facelock.git aur-facelock
   cd aur-facelock
   cp "$REPO_ROOT/dist/PKGBUILD" .
   cp "$REPO_ROOT/dist/facelock.install" .
   makepkg --printsrcinfo > .SRCINFO
   git add PKGBUILD facelock.install .SRCINFO
   git commit -m "Initial commit"
   git push
   cd ..

   # facelock-bin (prebuilt binaries from the GitHub Release — no cargo build)
   git clone ssh://aur@aur.archlinux.org/facelock-bin.git aur-facelock-bin
   cd aur-facelock-bin
   cp "$REPO_ROOT/dist/PKGBUILD-bin" PKGBUILD
   cp "$REPO_ROOT/dist/facelock.install" .
   makepkg --printsrcinfo > .SRCINFO
   git add PKGBUILD facelock.install .SRCINFO
   git commit -m "Initial commit"
   git push
   cd ..

   # facelock-git (VCS package tracking main)
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

Automated after setup, via [Packit](https://packit.dev) — the Fedora-ecosystem
standard for upstream→COPR builds. There is **no webhook and no GitHub secret**.

The configuration lives in `.packit.yaml` at the repository root. It declares a
single `copr_build` job with `trigger: release`: when `release.yml` publishes a
GitHub Release, Packit generates an SRPM from `dist/facelock.spec` and builds it
into the existing COPR project `tyvsmith/facelock` for `fedora-rawhide`,
`fedora-42`, and `fedora-43` (x86_64).

The COPR RPM is built **from source** and does **not** bundle ONNX Runtime — the
spec's `Requires: onnxruntime` pulls Fedora's system `onnxruntime` package
instead. (The `ort` crate feature `api-20` keeps the binary compatible with
Fedora's ONNX Runtime.)

**One-time setup (~10 minutes):**

1. Create a Fedora Account at https://accounts.fedoraproject.org
2. Log in to COPR at https://copr.fedorainfracloud.org and ensure the
   `tyvsmith/facelock` project exists with the `fedora-rawhide-x86_64`,
   `fedora-42-x86_64`, and `fedora-43-x86_64` chroots enabled
   (Settings → Chroots).
3. Install the **Packit-as-a-Service** GitHub App on the repository:
   https://github.com/marketplace/packit-as-a-service
4. In the COPR project → Settings → Permissions, grant the `packit` user
   **builder** permission so Packit can build into the existing project. If an
   "allowed forge projects" field is present, add `github.com/tyvsmith/facelock`.
5. In the COPR project → Settings, enable **"Enable internet access during
   builds"**. The RPM is built from source and `cargo` fetches crates from
   crates.io during `%build`; COPR's build chroot is network-isolated by
   default, so this toggle is required or the build fails resolving crates.

Verify the COPR build locally before relying on it with `just test-copr`, which
reproduces the Packit SRPM + `mock` from-source rebuild on a Fedora chroot.

After setup, every non-prerelease GitHub Release triggers a COPR build
automatically. To populate COPR without cutting a release (e.g. after first-time
setup), run `packit build in-copr --owner tyvsmith --project facelock` locally.

Note: a previously published release will **not** retroactively build — Packit
reacts only to *new* Release events.

The old `COPR_WEBHOOK_URL` GitHub secret is no longer used and can be deleted
(`gh secret delete COPR_WEBHOOK_URL`).

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

The `ort` crate is built with feature `api-20`, so facelock requires ONNX Runtime
**1.20 or newer** at runtime. ONNX Runtime is forward-compatible, so a single
build works against any runtime ≥ 1.20.

ONNX Runtime is sourced differently per channel:

- **GitHub-Release `.deb` and `.rpm`**: bundle a CPU-only `libonnxruntime.so`
  (ORT 1.20.1) at `/usr/lib/facelock/libonnxruntime.so`, because ONNX Runtime is
  not available in Ubuntu repositories.
- **COPR RPM** (built from source by Packit): does **not** bundle ORT. The spec's
  `Requires: onnxruntime` pulls Fedora's system `onnxruntime` package.
- **Arch Linux** (PKGBUILD): depends on the system `onnxruntime` package
  (available in official repos).

The bundled ORT is a CPU-only fallback — users who install a system-wide
GPU-enabled ONNX Runtime (CUDA, ROCm, OpenVINO) will have it take precedence
automatically (the search order prefers system paths over the bundled copy).

The bundled ORT version is pinned in `.github/workflows/release.yml` as
`ORT_VERSION`. When upgrading the `ort` crate dependency, update `ORT_VERSION`
and, if the new crate requires a higher floor, the `api-NN` feature in
`crates/facelock-face/Cargo.toml`.

## Upgrade Safety

Since facelock is a PAM module, broken releases can lock users out. Every release must:

1. Pass `just check` (tests + clippy + fmt)
2. Pass `just test-arch-pam` (Arch container PAM smoke tests)
3. Pass `just test-rpm` and `just test-deb` (multi-distro package validation)
4. Not change PAM auth semantics without explicit changelog entry
5. Preserve `/etc/pam.d/sudo` backup on install (`sudo.facelock-backup`)
6. Default to `PAM_IGNORE` on internal errors (fall through to password)
