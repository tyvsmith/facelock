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

### Prerelease identity conversions

Release input is strict SemVer: `X.Y.Z` or `X.Y.Z-{alpha,beta,rc}.N`. The same
identity is converted explicitly for each package manager:

| Surface | Alpha 1 | Stable |
|---------|---------|--------|
| Git tag | `v0.2.0-alpha.1` | `v0.2.0` |
| Cargo | `0.2.0-alpha.1` | `0.2.0` |
| Debian upstream | `0.2.0~alpha.1` | `0.2.0` |
| RPM Version-Release | `0.2.0-0.1.alpha.1` | `0.2.0-1` |
| Arch pkgver-pkgrel | `0.2.0alpha1-1` | `0.2.0-1` |
| GitHub Release | prerelease | release |

The first alpha Debian revisions are
`0.2.0~alpha.1-1~deb13u1` (trixie),
`0.2.0~alpha.1-1~deb12u1` (bookworm),
`0.2.0~alpha.1-1~ubuntu26.04.1` (resolute), and
`0.2.0~alpha.1-1~ubuntu24.04.1` (noble).

Package rebuilds advance independently of the Cargo version. Debian and Arch
increment their package revision for a rebuild of the same prerelease and reset
it for the next semantic prerelease. RPM uses one monotonic prerelease counter
across the whole series:

```text
Debian: 0.1.4-1 < 0.2.0~alpha.1-1 < 0.2.0~alpha.1-2 < 0.2.0~alpha.2-1 < 0.2.0~beta.1-1 < 0.2.0~rc.1-1 < 0.2.0-1
RPM:    0.1.4-1 < 0.2.0-0.1.alpha.1 < 0.2.0-0.2.alpha.1 < 0.2.0-0.3.alpha.2 < 0.2.0-0.4.beta.1 < 0.2.0-0.5.rc.1 < 0.2.0-1
Arch:   0.1.4-1 < 0.2.0alpha1-1 < 0.2.0alpha1-2 < 0.2.0alpha2-1 < 0.2.0beta1-1 < 0.2.0rc1-1 < 0.2.0-1
```

`scripts/release-versions.sh` is the executable conversion contract.
Repeating the same prerelease is a package rebuild; repeating a stable version
is rejected because Debian/Arch would otherwise advance while RPM remains at
release 1. Semantic version regressions are rejected before any file is edited.
`just test-release-matrix` verifies the exact order with native
`dpkg --compare-versions`, `rpmdev-vercmp`, and `vercmp` in disposable,
digest-pinned containers.

## How to Release

### Automated (recommended)

```bash
just release 0.2.0
# or
just release 0.2.0-alpha.1
```

This will:
1. Convert and bump Cargo, Arch tag/pkgver/pkgrel, RPM Version/Release, and Debian upstream/revision metadata
2. Run `cargo check --workspace` to verify the version bump compiles
3. Preserve package rebuild ordering, including the monotonic RPM prerelease counter
4. Prompt you to update `CHANGELOG.md` (add entries under the new version heading)
5. Print the `git commit` / `git tag` / `git push` commands for you to run

Then push the tag to trigger the release workflow:

```bash
git push origin main --tags
```

### What happens on tag push

The `.github/workflows/release.yml` workflow:

1. Builds release binaries and creates a GitHub Release
2. Downloads ONNX Runtime for bundling in non-Arch packages
3. Builds four suite-specific `.deb` packages for trixie, bookworm, resolute, and noble
4. Builds the direct `.rpm` package in the pinned Fedora 44 container and validates contents
5. Validates Nix flake evaluation
6. Publishes stable releases to AUR — `facelock`, `facelock-bin`, and `facelock-git` — if `AUR_SSH_KEY` is configured
7. Publishes stable releases to the signed, codenamed APT suites if the APT signing secrets are configured
8. Triggers GitHub Pages rebuild to include updated APT repo

Validated prerelease tags set the GitHub Release `prerelease` output and upload
direct artifacts, but skip stable APT and all AUR publication. The workflow
guards use the validated release identity rather than substring matching.

COPR (Fedora) is **not** built by `release.yml`. It is handled by [Packit](https://packit.dev),
which reacts to the GitHub Release published in step 1. See the COPR section below.

#### Debian package channels

| Channel | Build env | TPM | Version suffix | Use case |
|---------|-----------|-----|----------------|----------|
| `trixie` | Debian 13 | Yes | `X.Y.Z-1~deb13u1` | Debian 13 |
| `bookworm` | Debian 12 | No | `X.Y.Z-1~deb12u1` | Debian 12 LTS |
| `resolute` | Ubuntu 26.04 | Yes | `X.Y.Z-1~ubuntu26.04.1` | Ubuntu 26.04 LTS |
| `noble` | Ubuntu 24.04 | No | `X.Y.Z-1~ubuntu24.04.1` | Ubuntu 24.04 LTS |

All four `.deb` packages are uploaded to the GitHub Release for direct download.
Stable packages are published under the matching codename at
`https://tysmith.me/facelock/apt/`; ABI-distinct packages never share one suite.

### Supported release matrix

`dist/release-matrix.json` is the checked-in authority. The release workflow,
APT configuration, Packit targets, and this table are checked against it.

| Platform | Architecture | Variant/channel | Runtime | Support tier | Release target | Lifecycle depth |
|----------|--------------|-----------------|---------|--------------|----------------|-----------------|
| Debian 13 trixie | amd64 | TPM, staged APT/direct deb | bundled ORT 1.20.1 | supported | yes | full |
| Debian 12 bookworm LTS | amd64 | legacy, staged APT/direct deb | bundled ORT 1.20.1 | supported | yes | full |
| Ubuntu 26.04 LTS | amd64 | TPM, staged APT/direct deb | bundled ORT 1.20.1 | supported | yes | full |
| Ubuntu 24.04 LTS | amd64 | legacy, staged APT/direct deb | bundled ORT 1.20.1 | supported | yes | full |
| Fedora 43 | x86_64 | staging COPR | system ORT | supported | yes | full through the 2026-12-02 EOL gate |
| Fedora 44 | x86_64 | staging COPR | system ORT | supported | yes | full |
| Fedora 45 branched | x86_64 | staging COPR | system ORT | supported | yes | required build/runtime smoke |
| Fedora Rawhide (Fedora 46 development) | x86_64 | optional experimental production COPR chroot | system ORT | experimental | no | best-effort pinned Track D smoke only |
| Fedora 44 | x86_64 | direct RPM | bundled ORT 1.20.1 | supported | yes | full |
| Arch Linux Archive snapshot 2026-08-18 | x86_64 | PKGBUILD and binary recipe | system ORT | supported | yes | full |

Production COPR requires Fedora 43, Fedora 44, and Fedora 45. Rawhide is the
only optional allowed experimental production chroot, so it may be present or
absent; missing any required chroot or enabling any unknown extra fails closed.
Every Packit `copr_build` target must be an explicit member of the checked-in
allowlist: `fedora-43-x86_64`, `fedora-44-x86_64`, or
`fedora-45-x86_64`. Mutable aliases such as `fedora-all`,
`fedora-development`, and their architecture-suffixed forms are rejected, as
is any other undeclared target. Rawhide is not a release target and is not a
Packit staging or production release target. Both `fedora-rawhide` and
`fedora-rawhide-x86_64` fail validation, and no alpha may publish to Rawhide.

Fedora 43 and Fedora 44 carry the full lifecycle. Fedora 45 carries required
build/runtime smoke. Rawhide remains best-effort pinned Track D smoke only; a
Rawhide-only failure is not alpha-blocking, and Rawhide cannot supply lifecycle,
artifact, upgrade, rollback, served-version, or availability evidence.
Promotion requires a separately reviewed amendment and full Fedora gates.
Issue #236 owns the pre-tag and post-publication proof that optional Rawhide
serves no alpha or candidate build.

Container identities are pinned by registry/index digest, with the linux/amd64
manifest digest retained where the registry exposes both. They were resolved
from Docker Hub registry metadata and Fedora registry `Docker-Content-Digest`
on 2026-08-18. The Arch repository identity is pinned separately to
`https://archive.archlinux.org/repos/2026/08/18/`; every matrix-associated CI
and AUR `pacman` invocation installs that exact mirror before refreshing package
metadata.

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
scripts, dependency resolution, ORT bundling, tmpfiles triggers, and the full
install path.

The `*-dev-shell` recipes mount host models for fast interactive camera testing.
The `*-release-shell` recipes start from a clean package install with nothing from the
host — run `facelock setup` to download models, then enroll and test.

### Release preflight (recommended)

Run this before creating/pushing a release tag:

```bash
just release-preflight              # stable release checks
just release-preflight v0.2.0-rc.1 # prerelease checks; no stable secret access
just test-release-matrix            # converters, matrix drift, native ordering
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
secret — it is driven by Packit. Preflight and CI also read the public
production COPR API and require its enabled chroots to equal the checked-in
authority: Fedora 43/44/45 are required and Rawhide is the only optional
experimental chroot. Rawhide may be present or absent; a missing required
chroot or any unknown extra is release-blocking drift. The checker never
modifies the project. When the Packit CLI is available,
preflight runs `packit config validate --offline`; `just test-copr` always runs
that real schema gate inside its Fedora Packit container.

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

Packit reads `.packit.yaml` from the released tag. Packit's documented
`upstream_tag_exclude` filtering applies to downstream synchronization jobs,
not to `copr_build`, so it is not a prerelease safety boundary.

The prerelease-capable configuration keeps the production
`tyvsmith/facelock` job at `trigger: ignore`. That makes an alpha-tagged config
structurally incapable of selecting a release-triggered production project.
Before a stable release, the maintainer deliberately changes that trigger to
`release` in the stable-tagged config. `just release-preflight` rejects a
production release job for a prerelease and rejects its absence for a stable.
The deliberate stable restoration targets `fedora-43-x86_64`,
`fedora-44-x86_64`, and the separate `fedora-45-x86_64` branched target.
Rawhide is Fedora 46 development in this matrix, not an alias for Fedora 45,
and not a staging or production Packit target. A configuration that targets
Rawhide for release fails the matrix check.

`.packit.yaml` deliberately uses JSON syntax, which is a valid YAML subset.
Release guards therefore parse its jobs semantically with the Python standard
library instead of comparing YAML spelling; general YAML outside that subset
fails closed. The production project is `tyvsmith/facelock`. The planned
prerelease staging project is `tyvsmith/facelock-testing`, but provisioning or
changing it belongs to issue #236 and is not performed by this release-identity
change.

The COPR RPM is built **from source** and does **not** bundle ONNX Runtime — the
spec's `Requires: onnxruntime` pulls Fedora's system `onnxruntime` package
instead. (The `ort` crate feature `api-20` keeps the binary compatible with
Fedora's ONNX Runtime.)

**One-time setup (~10 minutes):**

1. Create a Fedora Account at https://accounts.fedoraproject.org
2. Log in to COPR at https://copr.fedorainfracloud.org and ensure the
   `tyvsmith/facelock` project exists with the `fedora-43-x86_64`,
   `fedora-44-x86_64`, and `fedora-45-x86_64` chroots enabled. The optional
   `fedora-rawhide-x86_64` experimental chroot may be enabled or absent; no
   other chroot is allowed (Settings → Chroots).
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

Only a stable-tagged config with the deliberately restored production release
trigger can populate production COPR automatically. Prerelease staging project
provisioning and served-repository validation are separate release-infrastructure
work; do not point prerelease tags at production while it is unavailable.

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

The repository configuration lives in `dist/apt/conf/distributions`. Four
codenamed suites are published:

- **`trixie`**: Debian 13 TPM build
- **`bookworm`**: Debian 12 legacy build
- **`resolute`**: Ubuntu 26.04 TPM build
- **`noble`**: Ubuntu 24.04 legacy build

The former ambiguous `main` and `legacy` suite names are retired. Existing
clients must replace that suite component in their Facelock source entry with
their operating-system codename before the first codenamed stable publication.
Prerelease packages are never inserted into these stable suites.
Stable publication requires exactly one suite-matching package for all four
codenames before signing or repository writes begin.

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
The version fields synced by `just release` are:

| File | Field |
|------|-------|
| `Cargo.toml` | `[workspace.package] version` |
| `dist/PKGBUILD`, `dist/PKGBUILD-bin` | upstream `_tag`, converted `pkgver`, package `pkgrel` |
| `dist/PKGBUILD-git` | converted display `pkgver` |
| `dist/facelock.spec` | converted `Version` and monotonic prerelease `Release` |
| `dist/debian/changelog` | converted upstream and package revision in first entry |

The independently maintained `dist/release-matrix.json` records supported
targets, lifecycle depth, and immutable environment identities. Release
preflight, CI, and release metadata checks validate that authority; `just
release` does not rewrite it.

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
5. Preserve `/etc/pam.d/sudo` backup on install (`/var/lib/facelock/pam-backups/sudo.<timestamp>`)
6. Default to `PAM_IGNORE` on internal errors (fall through to password)
