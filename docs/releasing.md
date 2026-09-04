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
`0.2.0~alpha.1-1~deb13u1` (trixie) and
`0.2.0~alpha.1-1~ubuntu26.04.1` (resolute).

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

1. Validates the tag against the checked-in release identity and target matrix
2. Builds release binaries and uploads them as workflow artifacts
3. Prepares the pinned ONNX Runtime and lock-bound Cargo-vendor source
   components with their reviewed manifests and checksums
4. Builds two suite-specific TPM-enabled `.deb` packages for trixie and resolute
5. Builds the direct `.rpm` package in the pinned Fedora 44 container and validates contents
6. Validates Nix flake evaluation
7. Publishes stable releases to the signed, codenamed APT suites, and to the `main` and `legacy` compatibility suites until 0.3.0, if the APT signing secrets are configured
8. Verifies the tag, assembles and validates every asset, writes `MANIFEST.json`, and publishes the release exactly once
9. Publishes stable releases to AUR — `facelock`, `facelock-bin`, and `facelock-git` — if `AUR_SSH_KEY` is configured
10. Triggers GitHub Pages rebuild to include updated APT repo

Validated prerelease tags set the GitHub Release `prerelease` output and upload
direct artifacts, but skip stable APT and all AUR publication. The workflow
guards use the validated release identity rather than substring matching.

COPR (Fedora) is **not** built by `release.yml`. It is handled by [Packit](https://packit.dev),
which reacts to the release the `publish` job makes public in step 8. A draft
raises no release event, so nothing downstream fires until validation passes.
See the COPR section below.

#### Builders build, publish publishes

No builder writes to the release. Each one uploads a workflow artifact and a
digest attestation naming what it produced, the image it produced it in, and
the components it consumed. Until the `publish` job runs, the tag has no
release at all: nothing is public and no downstream automation has seen
anything.

That split is the point. Every builder compiles or packages code this project
does not own, from every dependency's `build.rs` to `rpmbuild` and
`dpkg-buildpackage`. A builder holding the publication credential is a builder
that can publish whatever it likes. `publish` compiles nothing, so it is the
only job that holds `RELEASE_PAT` and the only one with `contents: write`;
every other job holds `contents: read`, and the workflow's own default is
deny-all.

`publish` runs after every builder and validator, and it:

- verifies the tag exists, names the validated version, and points at the built
  commit; where the tag carries a signature it must verify. The job reads the
  tag and never creates, moves, or replaces one.
- stages exactly the canonical assets out of the builders' artifacts. The
  allowlist is derived from the validated version, Debian revision, and RPM
  counter, so an artifact built from another identity has no canonical name and
  a file a builder added beside the one it was asked to produce is never
  staged.
- holds every staged asset to the SHA-256 its builder attested. An asset that
  changed between its build and publication stops the release, as does one no
  builder attested or one two builders claim.
- holds each attestation to the provenance its slot may declare: the suite,
  the image `dist/release-matrix.json` pins, and the component names. A
  builder cannot report another image or an extra component into
  `MANIFEST.json`; a matrix the job cannot read stops the release instead of
  shortening the allowlist.
- trusts an attestation only once it hashes to the job output its builder
  recorded. Artifacts are shared, writable storage for every job in the run;
  a job output belongs to the job that wrote it. An attestation that was
  replaced after its job finished, or whose job recorded no output, is refused
  by name.
- creates the release as a draft carrying those assets, then writes
  `MANIFEST.json` over them, plus the source tarball digest, the pinned
  build-image digests, and the reviewed ONNX Runtime and Cargo-vendor component
  digests. It replaces the three-binary `SHA256SUMS` file, which covered a
  fraction of the release and was written before most of it existed.
- reads the draft back from the API, holds it to the allowlist a last time,
  holds each published asset's size, and digest where the API exposes one, to
  `MANIFEST.json` and the uploaded manifest to the file it wrote, and flips
  the draft to published once. A tag whose release is already published is
  refused before anything is written, so re-running the workflow after a
  failure is safe; re-running it after success goes red by design, at
  `verify-creatable`, with nothing written. One case needs a hand: if the
  Debian revision or RPM counter changed between runs, the draft still carries
  the asset built under the old name, and the readback refuses it. The
  failure names the file and the command that removes it,
  `gh release delete-asset`.

The workflow runs once at a time per tag (`concurrency` keyed by the ref,
never cancelling the run in progress), so a re-run started while a run is
inside `publish` queues behind it instead of racing it. Two drafts for one
tag, which only a race could leave behind, are refused with the
`gh api --method DELETE` command that removes the extra one.

A builder's extra output fails the release closed: a canonically named file in
an artifact the allowlist does not expect it from is refused at staging, and
the failure says so. Re-running only the failed `publish` job keeps that
artifact, so the remedy is fixing the builder and re-running all jobs.
A partial re-run of a single `build-deb` leg can similarly leave the other
suite's attestation unbound (`attestation deb-<suite> is not bound to a job
output`); the remedy is the same, re-run all jobs.

Two consequences for the maintainer:

- **`RELEASE_PAT` is required.** It is now the only credential that can write
  the release, so an unset secret fails the `publish` job.
  `just release-preflight` checks for it, and fails when it cannot check:
  without `gh`, or unauthenticated, the check is reported as unchecked and
  preflight does not pass.
- **A signed tag must be verifiable on the runner.** Importing the maintainer's
  public key is release infrastructure tracked by #235; until it lands, an
  unsigned tag is accepted and a signed one that the runner cannot verify stops
  the release.

Every job in the graph gates publication, `build-nix` included: its flake
evaluation is deterministic against `flake.lock` and must pass, while its
`nix build` step is advisory because the build depends on nixpkgs state. An
evaluation failure means the release stays a draft; fix the flake and re-run
the failed jobs, never tag again.

`test/release-artifacts-contract.sh` (`just test-release-artifacts`) proves this
shape by fixture and by mutation. The workflow itself runs only on a tag, so
the gate never tags anything to test it.

#### Debian package channels

| Channel | Build env | Rust toolchain | TPM | Version suffix |
|---------|-----------|----------------|-----|----------------|
| `trixie` | Debian 13 | official Trixie Backports `cargo` and `rustc` | Yes | `X.Y.Z-1~deb13u1` |
| `resolute` | Ubuntu 26.04 | native distro `cargo` and `rustc` | Yes | `X.Y.Z-1~ubuntu26.04.1` |

Debian-family release support is exactly Debian 13 (Trixie) and Ubuntu 26.04
LTS (Resolute). Both codenamed suites ship one binary package named `facelock` with TPM
support enabled. No `rustup` toolchain participates in Debian source builds.
Bookworm and Noble artifacts may remain in historical releases, but those
suites are unsupported and receive no new packages.
Trixie package builds use the official Trixie Backports `cargo` and `rustc`;
Resolute package builds use the native Ubuntu toolchain.

Both `.deb` packages are uploaded to the GitHub Release for direct download.
Stable packages are published under the matching codename at
`https://tysmith.me/facelock/apt/`.

Each Debian source package consists of the exact tagged main upstream tarball,
the reviewed ORT component, the deterministic Cargo-vendor component, and the
Debian quilt delta. Complete `.dsc` rebuilds run with network denied and empty
Cargo/Rustup caches. Stable APT publication consumes exactly two suite
manifests, one for Trixie and one for Resolute, before signing or writing the
repository.

Each suite manifest contains exactly eight artifacts in this order: the main
orig tarball, ORT orig component, Cargo-vendor orig component, Debian quilt
delta, `.dsc`, `.buildinfo`, `.deb`, and `.changes`. The Cargo component carries
a generated legal inventory covering every exact lock-bound crate; its specific
DEP-5 stanza precedes the Facelock source catch-all. CI prepares that component
with Rust 1.95.0 through the immutable `dtolnay/rust-toolchain` action commit
`4360b52568e2003a75bf9bc1d59f33a8e3fc893c`, matching the repository's pinned
1.95 toolchain channel.

Every built `.deb` passes `.github/workflows/scripts/validate-deb.sh` in the
suite container before staging: package identity, forbidden transition fields,
generated dependencies, the required file set, the hash-verified ORT bundle,
and a lintian run that fails on error-severity tags. Deliberate deviations are
suppressed in that script, each with a recorded reason; warnings are printed
for review but do not gate.

### Supported release matrix

`dist/release-matrix.json` is the checked-in authority. The release workflow,
APT configuration, Packit targets, and this table are checked against it.

| Platform | Architecture | Packaging/channel | Runtime | Support tier | Release target | Lifecycle depth |
|----------|--------------|-----------------|---------|--------------|----------------|-----------------|
| Debian 13 trixie | amd64 | one `facelock` package; TPM required; staged APT/direct deb | bundled ORT 1.20.1 | supported | yes | full |
| Ubuntu 26.04 LTS | amd64 | one `facelock` package; TPM required; staged APT/direct deb | bundled ORT 1.20.1 | supported | yes | full |
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
just test-deb            # delegate to both exact supported-suite package gates
just test-deb-trixie-pkg    # Debian 13 — offline source rebuild, install, TPM, lifecycle
just test-deb-resolute-pkg  # Ubuntu 26.04 — the same complete package gate
just test-rpm-pkg        # Fedora — build real .rpm, install via dnf, validate
just test-rpm-lanes      # every declared Fedora target at its declared depth
just test-rpm-authselect # Fedora — retired-profile upgrade guard lifecycle
just test-packit-config  # Packit config schema — real `packit` in a pinned Fedora container
just test-copr           # COPR-equivalent build only — Packit SRPM + mock from-source rebuild (slow)
just test-copr-pkg 43    # the same rebuild, then install it and run the booted lifecycle
just test-copr-lanes     # every Packit/COPR target rebuilt from source at its declared depth

# Interactive (requires camera)
just test-deb-dev-shell      # Ubuntu .deb with host models — fast iteration
just test-rpm-dev-shell      # Fedora .rpm with host models — fast iteration
just test-deb-release-shell  # Ubuntu .deb clean room — real user experience
just test-rpm-release-shell  # Fedora .rpm clean room — real user experience
```

The `test-rpm` recipe validates file layout from manually installed binaries.
`test-deb` delegates to both supported-suite `*-pkg` recipes. The `*-pkg`
recipes build real packages using the same scripts as CI, install them with
the actual package manager (`dnf` / `dpkg`), and validate the result — testing postinst
scripts, dependency resolution, ORT bundling, tmpfiles triggers, and the full
install path.

The `*-dev-shell` recipes mount host models for fast interactive camera testing.
The `*-release-shell` recipes start from a clean package install with nothing from the
host — run `facelock setup` to download models, then enroll and test.

#### Fedora lanes

Every Fedora recipe takes a release — `just test-rpm-pkg 43`, `just test-copr 45`
— and defaults to 44. `just test-rpm-lanes` runs each declared release target at
the lifecycle depth `dist/release-matrix.json` gives it: full lifecycle for
Fedora 43 and 44, build plus runtime smoke for branched Fedora 45. Rawhide is
optional and experimental, has no lane, and can never stand in for a Fedora 43,
44, or 45 result.

Each Fedora target needs two lanes, not one. `test-rpm-lanes` proves the direct
`.rpm`: host-built binaries, bundled ONNX Runtime. That is not the delivery
path the matrix declares for Fedora, which is Packit publishing to COPR against
Fedora's system ONNX Runtime. `just test-copr-lanes` proves that one at the
same declared depths — `test-copr-pkg 43`, `test-copr-pkg 44`,
`test-copr-smoke 45`. Each rebuilds the package from source in a mock chroot
(the `test-copr` half), exports the RPM it built, installs it with `dnf` so the
package's own `Requires: onnxruntime` resolves, and boots it for the same
validation the direct lane runs. `just test-packaging-matrix` requires both,
and `test/packaging-evidence.py` refuses a direct-RPM record offered as a COPR
target's evidence.

The COPR lanes are slow even by this file's standards: each one compiles the
whole workspace and runs the spec's `%check` inside the mock chroot, and mock
needs a privileged container, so they run serially through a single staging
path (`target/copr-lane/facelock.rpm`). Never run two at once.

`test/fedora-lane-image.sh` resolves each lane's digest-pinned base image from
the matrix, so no Containerfile carries its own Fedora digest. It refuses a
release the matrix does not declare and refuses one that has reached its EOL
gate: Fedora 43 goes EOL on 2026-12-02, and from that date the Fedora 43 lane
stops with a message instead of quietly testing an unmaintained release. Set
`RELEASE_MATRIX_TODAY` to rehearse that date, the same override
`test/check-release-matrix.py` reads. Retiring the lane means retiring its
matrix rows, and moving the date is a deliberate matrix edit.

Fedora 43 is the only release carrying a gate today. The lookup is generic on
`fedora.<release>_eol_gate`, so adding a `44_eol_gate` or `45_eol_gate` key
gates those lanes immediately; until one exists, 44 and 45 run past their own
end of life without complaint.

`just test-rpm-lanes` runs each release through the recipe its matrix
`lifecycle_depth` names, and `test/check-release-matrix.py` requires that exact
pairing, so a full lifecycle lane cannot be quietly downgraded to a smoke lane.

The full lifecycle lane also pins `%config(noreplace)`: an unmodified
`/etc/facelock/config.toml` is replaced in place on upgrade, a modified one
survives byte for byte with the new file diverted to `.rpmnew`, and erase
removes an unmodified copy outright while retaining a modified one as
`.rpmsave`. `docs/contracts.md` carries the same contract.

The RPM embeds a read-only retired-profile upgrade guard in `%pre`. The
model-free `test-rpm-authselect` gate boots Fedora with systemd and exercises
real authselect and PAM password success/failure across fresh, unselected,
selected-retired, custom-profile, malformed-state, and authselect-absent
transactions. It never changes the host PAM stack. The exact retired
`facelock` selection blocks with manual backup-and-reselection guidance;
ordinary selections are preserved and the new RPM ships no authselect profile
or dependency.

An already-installed v0.1.4 RPM cannot be retroactively guarded: direct
uninstall runs only the scriptlets already installed from v0.1.4. Users must
install a guarded release before a later uninstall so the guarded upgrade can
retire the old authselect payload first.

### Packaging gates in CI

`.github/workflows/packaging.yml` runs the lanes above in CI: both Debian suite
gates, every declared Fedora lane, the Arch package built from the real
`dist/PKGBUILD`, and the native version-ordering matrix. It downloads and
checksum-verifies the ONNX models first, through `.github/actions/fetch-models`,
so the daemon-start assertions execute instead of being counted as skipped.

Three schedules, because the full matrix takes about 1 h 45 min (measured
2026-09-02) and most pull requests touch no packaging:

| When | Lanes | Filtered |
|---|---|---|
| Pull request | all but `copr` | yes, only when the diff reaches a package |
| Nightly, 07:00 UTC | all | no |
| `just release-preflight` | evidence of a green run at HEAD | no |

The `copr` job never runs on a pull request regardless of the filter above --
mock needs a privileged container.

The pull-request filter is a `changes` job running
`.github/workflows/scripts/classify-changes.sh`, which classifies the merge-base
diff in plain bash. It covers `debian/`, `dist/`, `systemd/`, `dbus/`,
`config/`, `scripts/`, the packaging halves of `test/`, the justfile,
`.packit.yaml`, `.github/workflows/`, and the Rust the maintainer scripts
execute. `facelock pam remove --all` runs from `%preun`, from Arch's
`pre_remove` and from Debian's `prerm`, so a change to that command can abort a
package removal without touching a packaging file. Each lane then gates on
`if: needs.changes.outputs.packaging == 'true'`, which reports a real "skipped"
conclusion; GitHub's own `paths:` filter would leave a required check pending
forever instead.

**Residual risk.** Path filtering means a change that touches no packaging path
can still break the *packaged* runtime. A Rust change to daemon startup, a new
runtime dependency, a file the spec does not ship: each of those leaves its own
pull request green with every packaging job reported as skipped. Do not read
that as packaging-verified. The nightly matrix catches it within a day, and the
release gate below catches it before anything ships. When a change is
packaging-relevant in a way the filter cannot see, run the lane by hand or add
the path to `classify-changes.sh`.

**The COPR jobs go further and skip pull requests entirely.** Each one compiles
the workspace inside a mock chroot, which needs a privileged container, and no
pull request has been shown to get one on a rootless-podman runner; making that
a required check before it is proven would block every packaging merge on an
unproven capability. So a COPR-only break — something that shows up when the
package is rebuilt from source or run against Fedora's system ONNX Runtime, and
not otherwise — survives its own pull request even when the filter fires. The
nightly and the pre-release `workflow_dispatch` are unfiltered and do run them;
locally, `just test-copr-lanes`.

### Release preflight (recommended)

Run this before creating/pushing a release tag:

```bash
just test-arch-camera-required        # camera + a person in frame; records the commit
gh workflow run packaging.yml --ref main   # the packaging matrix, at this commit
just release-preflight                # stable release checks
just release-preflight v0.2.0-rc.1   # prerelease checks; no stable secret access
just check
just test-arch-pam
just test-arch-camera-free
```

`just test-arch-camera-required` comes first because it is the only step a
human has to perform. It runs `test-arch-integration` and `test-arch-oneshot`,
the two tiers that need `/dev/video*` and a live face, and writes the commit
they passed at to `.hardware-tiers-verified`. Preflight fails while that record
is absent or names an older commit, so run it after the last commit that will
ship, not before.

Those two tiers are the only automated evidence that face authentication works
end to end: real D-Bus activation, the real PAM stack, real capture, and the
one-shot path PAM falls back to. Nothing else ran them, and three of their
assertions rotted undetected as a result (#139). If they were already run by
hand at this exact commit, acknowledge that by naming it:
`FACELOCK_HARDWARE_TIERS_ACK=<sha> just release-preflight`.

Preflight also refuses to pass without complete packaging evidence for HEAD.
Every packaging lane writes a record of what it claimed and what it counted,
and `test/packaging-evidence.py` accepts the set only when every lane the
release matrix requires is present at this commit with zero skips and the ONNX
models on hand (the contract is in `docs/contracts.md`, "Packaging matrix
evidence"). That set includes a COPR lane per Packit release target beside the
direct-RPM lane, so a green Fedora `.rpm` result alone leaves the evidence
incomplete. Preflight reads it from the `packaging-evidence-*` artifacts a
successful `packaging.yml` run at that exact commit uploaded, fetched with
`gh run download`, or from `.packaging-matrix-verified`, which
`just test-packaging-matrix` writes after running every lane locally. A run's
green conclusion alone is not evidence: a path-filtered pull-request run skips
every lane and still concludes "success". A pull-request run cannot satisfy it
either: it builds the merge commit, not the commit being released. A
`FACELOCK_ALLOW_MISSING_MODELS=1` run is a diagnostic: it writes its `partial`
lane records, and the marker is withheld. The one-line commit marker from before
0.2.0 is refused with a message naming the new format. A nightly run does not
satisfy it either: nightly builds whatever `main` was at 07:00 UTC, and a
release commit is a version bump nobody has built a package from yet.

`just release-preflight` checks local tools, required packaging files (including
`.packit.yaml`), and whether `AUR_SSH_KEY`, `APT_GPG_PRIVATE_KEY`, and
`APT_GPG_PASSPHRASE` are configured in GitHub secrets (via `gh`). COPR needs no
secret — it is driven by Packit. Preflight and CI also read the public
production COPR API and require its enabled chroots to equal the checked-in
authority: Fedora 43/44/45 are required and Rawhide is the only optional
experimental chroot. Rawhide may be present or absent; a missing required
chroot or any unknown extra is release-blocking drift. The checker never
modifies the project. Preflight always runs `packit config validate --offline`
against `.packit.yaml`, in the digest-pinned Fedora container built from
`test/Containerfile.packit` — the same real schema gate `just test-copr` runs,
reachable without a host `packit` install. It has no skip path: podman is a
preflight prerequisite, and without it the gate fails rather than passing
unrun. `just test-packit-config` runs the same gate on its own.

Preflight also holds the APT compatibility window: `main` and `legacy`
compatibility suites present until 0.3.0, as `dist/release-matrix.json`
declares, and absent from the first 0.3.0 tree on. `just test-apt-repo` proves
the published shape when run, as a clean APT client; no workflow runs it.

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
   # dist/PKGBUILD ships a __SRC_SHA256__ placeholder; substitute the real
   # tarball digest before pushing or the recipe refuses to build. Download
   # first, hash the file after: piping curl into sha256sum hashes empty
   # input when the download fails, and that digest must never be published.
   TAG="$(sed -n 's/^_tag=//p' PKGBUILD)"
   curl -fSsL -o "/tmp/facelock-v${TAG}.tar.gz" \
     "https://github.com/tyvsmith/facelock/archive/v${TAG}.tar.gz" &&
     SUM="$(sha256sum "/tmp/facelock-v${TAG}.tar.gz" | cut -d' ' -f1)" &&
     sed -i "s/__SRC_SHA256__/${SUM}/" PKGBUILD
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

The COPR RPM is built **from source** with the spec's default
`%bcond_with bundled_ort` mode and does **not** bundle ONNX Runtime. Its
`BuildRequires`/`Requires: onnxruntime` use Fedora's runtime-only package; the
package check asserts `onnxruntime-devel` is absent and creates a real ORT
session from the checksum-pinned minimal model in `test/fixtures/`. (The `ort`
crate feature `api-20` keeps the binary compatible with Fedora's runtime.)

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
reproduces the Packit SRPM + `mock` from-source rebuild on a Fedora chroot and
checks that the payload has no bundle while its dependencies select Fedora ORT.

Only a stable-tagged config with the deliberately restored production release
trigger can populate production COPR automatically. Do not point a prerelease
tag at production while staging is unavailable.

##### Staging COPR (`tyvsmith/facelock-testing`)

`.packit.yaml` declares a second `copr_build` job, for
`tyvsmith/facelock-testing`. It carries `trigger: ignore`, so nothing runs it
automatically: a maintainer dispatches it by hand with a `/packit build`
comment. A tag never publishes into staging.

The project does not exist yet. Issue #236 owns creating it and granting Packit
builder access; until then `dist/release-matrix.json` keeps
`copr_channels.staging.provisioned` false and
`python3 test/check-live-release-channels.py --channel staging` reports `not
provisioned` and queries nothing.

The trigger and that switch move together, and `test/check-release-matrix.py`
enforces the pairing. While `provisioned` is false the staging job must stay on
`trigger: ignore`; a pull-request trigger would aim every pull request's Packit
run at a project that answers 404.

Provisioning is therefore three edits, and the contract rejects any two of them
without the third. Create the project with exactly the Fedora 43, 44, and 45
chroots, then:

1. set `copr_channels.staging.provisioned` to true in `dist/release-matrix.json`
2. move the staging job in `.packit.yaml` to `trigger: pull_request`
3. delete the `staging COPR provisioning must stay unclaimed until issue #236
   creates the project` assertion from `test/check-release-matrix.py`

The third edit is the point of review: that assertion exists so provisioning
cannot be claimed by a config change alone, and retiring it is the moment
someone confirms the project really exists. From then on the same checker
compares the live project with the checked-in authority on every pull request
and in preflight.

Staging tolerates no optional experimental chroot. Production accepts Rawhide's
presence or absence; in staging any chroot beyond the supported three is drift.

##### Pre-tag attestation

`scripts/release-attestation.py` renders and validates the document that binds a
candidate to what its channels serve: the candidate commit, the EVR each channel
serves per target, artifact and repository digests, signing key fingerprints, and
when each channel last refreshed its repository metadata.

```bash
python3 scripts/release-attestation.py render --input facts.json --output attestation.json
python3 scripts/release-attestation.py validate --attestation attestation.json --expect expect.json
```

`validate` fails closed on a drifted candidate commit, a served EVR or digest
that disagrees with the recorded expectations, a changed signing fingerprint,
metadata older than `metadata_max_age_seconds` or stamped in the future, and on
any channel carrying the production COPR identity. Gathering those facts from
live staging repositories is issue #236's remaining infrastructure work; the
script and its contract cases run against fixtures today.

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

The repository configuration lives in `dist/apt/conf/distributions`. Two
codenamed suites are published:

- **`trixie`**: Debian 13 TPM build using Trixie Backports Rust/Cargo
- **`resolute`**: Ubuntu 26.04 TPM build using native Rust/Cargo

Two compatibility suites are published alongside them until 0.3.0, for
clients whose source entry was written for v0.1.4:

- **`main`**: the `trixie` package, included by `publish-apt.sh` from the same
  validated artifact
- **`legacy`**: no package; `reprepro export` writes signed empty indexes so
  `apt update` keeps succeeding

Those clients must replace the suite in their Facelock source entry
with their operating-system codename before 0.3.0. `dist/release-matrix.json`
declares the window under `apt_suites.compat`, and `check-release-matrix.py`
fails the first tree at or past `retire_at` that still carries the stanzas.
Prerelease packages are never inserted into any of these stable suites.
Stable publication requires exactly one suite-matching package for both
codenames before signing or repository writes begin.

The APT repo is hosted at `https://tysmith.me/facelock/apt/` alongside the docs site. The public keyring is at `https://tysmith.me/facelock/apt/tysmith-archive-keyring.gpg`.

**GPG key rotation**: When the signing key expires, generate a new key, update the `APT_GPG_PRIVATE_KEY` and `APT_GPG_PASSPHRASE` secrets, and cut a new release. The public keyring is re-exported on every release, so users who re-fetch it will get the updated key.

#### Manual AUR update (fallback)

If CI is not configured or fails:

1. Download the release tarball, then compute the checksum from the file.
   Piping curl into sha256sum prints the digest of empty input when the
   download fails; downloading first prints no digest at all:
   ```bash
   curl -fSsL -o "facelock-v$VERSION.tar.gz" \
     "https://github.com/tyvsmith/facelock/archive/v$VERSION.tar.gz" &&
     sha256sum "facelock-v$VERSION.tar.gz"
   ```
2. Clone the AUR repo (first time only):
   ```bash
   git clone ssh://aur@aur.archlinux.org/facelock.git aur-facelock
   ```
3. Copy `dist/PKGBUILD` and `dist/facelock.install` into the AUR repo
4. Replace the `__SRC_SHA256__` placeholder in the PKGBUILD with the real checksum from step 1
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
| `debian/changelog` | converted upstream and package revision in first entry |

The independently maintained `dist/release-matrix.json` records supported
targets, lifecycle depth, and immutable environment identities. Release
preflight, CI, and release metadata checks validate that authority; `just
release` does not rewrite it.

### The version `facelock-git` actually installs

`dist/PKGBUILD-git`'s `pkgver` field is display only. AUR's web page and
`.SRCINFO` show it because `makepkg --printsrcinfo` runs without a checkout to
describe, and `just release` keeps it level with the release so the page does
not drift. What a build installs is whatever `pkgver()` computes at build time:

```text
<released pkgver>.r<commits since that tag>.g<abbreviated object name>
```

`git describe --abbrev=7` sets a floor, not a width: the object name is seven
hex characters, or more where seven would be ambiguous. So a build off `v0.1.4`
reads like `0.1.4.r650.ga8c48b7`, and one off `v0.2.0-alpha.1` like
`0.2.0alpha1.r7.gdeadbee`. Two properties make that version usable, and both are
enforced rather than assumed:

- it must outrank the release it descends from, or pacman refuses the upgrade
  and every AUR helper reports the package as permanently out of date
- it must rank below the next release, or the git package blocks the real one

Four things earn that, and all four were live faults (#330):

| | |
|---|---|
| `--tags` | Every release tag since `v0.1.2` is lightweight. Without it, describe walks back to the last annotated tag, `v0.1.0-rc4`. |
| `--match 'v[0-9]*'` | The repository carries a non-version tag (`assets`), and describe takes it whenever it sits nearer HEAD. |
| stripped leading `v` | pacman ranks an alphabetic first segment below a numeric one, so a surviving `v` sorts the build under every release. |
| converted prerelease suffix | `v0.2.0-alpha.1` becomes `0.2.0alpha1`, the same conversion the released package gets. pacman compares separator runs before segments, so keeping the punctuation ranks the build above `0.2.0alpha2`, `0.2.0beta1` and the stable `0.2.0` alike. |

`test/release-version-contract.sh` holds the recipe to that shape against a
synthetic tag graph, and `test/release-native-ordering.sh` hands the result to
`vercmp` inside the pinned Arch container. `release_arch_git_pkgver` in
`scripts/release-versions.sh` is the one definition both read.

## ONNX Runtime Bundling

The `ort` crate is built with feature `api-20`, so facelock requires ONNX Runtime
**1.20 or newer** at runtime. ONNX Runtime is forward-compatible, so a single
build works against any runtime ≥ 1.20.

ONNX Runtime is sourced differently per channel:

- **GitHub-Release `.deb`**: bundles CPU-only ORT 1.20.1 under
  `/usr/lib/facelock/`, because ONNX Runtime is not available in Ubuntu
  repositories.
- **GitHub-Release direct `.rpm`**: builds the spec with
  `--with bundled_ort`, installs `libonnxruntime.so.1` under
  `%{_libdir}/facelock/`, and has no system `onnxruntime` dependency.
- **COPR RPM** (built from source by Packit): leaves the spec's
  `%bcond_with bundled_ort` disabled, contains no bundled runtime, and requires
  Fedora's system `onnxruntime` package.
- **Arch Linux** (PKGBUILD): depends on the system `onnxruntime` package
  (available in official repos).

The bundled ORT is a CPU-only fallback — users who install a system-wide
GPU-enabled ONNX Runtime (CUDA, ROCm, OpenVINO) will have it take precedence
automatically (the search order prefers system paths over the bundled copy).

The reviewed pins in `.github/workflows/release.yml` include the version,
upstream URL, archive and library SHA-256 values, upstream commit, and MIT
license identity. The download job verifies the archive before extraction and
the library after extraction, then emits `manifest.json`, `SHA256SUMS`, and
`PROVENANCE.md` beside upstream `LICENSE`, `ThirdPartyNotices.txt`,
`VERSION_NUMBER`, and `GIT_COMMIT_ID`. Direct RPM assembly re-verifies those
inputs and enters `.github/workflows/scripts/run-networkless.sh` before creating
the source archive or rpmbuild tree. That wrapper uses util-linux `enosys` to
deny socket and `io_uring` network syscalls, closes inherited non-stdio file
descriptors, and requires its network probe to fail with `ENOSYS` before
invoking `rpmbuild`; `CARGO_NET_OFFLINE=true` remains defense in depth. The RPM
ships the reviewed inputs under its package documentation/license directories
for SBOM and provenance consumers.

When upgrading the `ort` crate dependency, update every reviewed ORT pin and
the RPM bundle filename together and, if the crate requires a higher floor,
the `api-NN` feature in `crates/facelock-face/Cargo.toml`.

Rawhide may be attempted only with the digest-pinned experimental environment
recorded in `dist/release-matrix.json`. A Rawhide system-ORT build/session smoke
is best effort: absence or failure is nonblocking and can never stand in for
lifecycle, upgrade, rollback, artifact, served-version, availability, or alpha
release evidence. It must not publish or modify a COPR channel.

## Upgrade Safety

Since facelock is a PAM module, broken releases can lock users out. Every release must:

1. Pass `just check` (tests + clippy + fmt)
2. Pass `just test-arch-pam` (Arch container PAM smoke tests)
3. Pass `just test-arch-camera-free` (camera-free daemon and one-shot E2E)
4. Pass `just test-arch-camera-required` against the final release commit, with
   a camera and a person in frame; `just release-preflight` fails until it has
5. Pass `just test-rpm` and `just test-deb` (multi-distro package validation)
6. Not change PAM auth semantics without explicit changelog entry
5. Preserve `/etc/pam.d/sudo` backup on install (`/var/lib/facelock/pam-backups/sudo.<timestamp>`)
6. Default to `PAM_IGNORE` on internal errors (fall through to password)

### Upgrading from the last release

`just test-upgrade-v014` proves that state written by v0.1.4 survives an upgrade
to the candidate and a rollback back to v0.1.4. Two lanes, Debian trixie and
Fedora 44, each install the real published artifact rather than a synthesized
older build of the candidate.

**What the lanes pin.** `dist/release-matrix.json` carries a `predecessors` block
holding the GitHub release id, the asset id, the SHA256 and the byte size of
each predecessor artifact. The lane Containerfiles take those as build args and
carry no digest of their own, so one review changes the pin everywhere.
`just test-upgrade-v014-pins` asks the release API whether those assets are still
the assets it serves, which is how a re-uploaded or substituted predecessor gets
caught before a lane silently proves something about a different file.

**What the lane images carry.** Each image installs the runtime libraries the
released binary needs before the predecessor goes on. v0.1.4 wrote its Debian
control file by hand and never declared libxkbcommon0, which its own binary
links, so that release cannot start on a minimal Debian 13 at all. The candidate
is built from `debian/control` and derives the list with `${shlibs:Depends}`.
Nothing is masked by supplying it: candidate dependency resolution belongs to
`test/deb-dependency-closure.sh` on a pristine suite base.

**What the lanes build.** Predecessor state comes from the released v0.1.4
binary, never from the candidate: plaintext rows, keyfile-encrypted rows, mixed
rows, and two swtpm-sealed shapes, one PCR-bound and one not. Each shape also
carries a modified config, the reviewed models, an enrollment marker, an audit
log and a hand-wired PAM service, because v0.1.4 has no `facelock pam`
subcommand and that is the shape a real upgrade finds.

**What each lane proves after the upgrade.** The V5 database reaches V6 with
legacy rows at `device_id = NULL`. A known embedding still decrypts to the exact
plaintext it was enrolled as, which a file hash cannot show: a preserved key and
a preserved ciphertext nobody can open any more hash identically. No key
artifact is replaced and none appears that was not there before. Modes converge
to ADR 010 without content changing. The enrollment marker keeps its owner and
mode and its content is reconciled against the database rather than preserved
byte for byte — the one piece of state the upgrade is supposed to rewrite
(#137). The administrator's PAM service is byte-identical, a correct password
still authenticates, and a wrong one still fails.

**Version ordering on a development tree.** Until `just release` bumps the
workspace, the candidate .deb built from the tree is `0.1.4-1~deb13u1`, which
sorts *below* the published `0.1.4-1`. The lanes build the same payload as an
upgrade-test version instead, and every run prints the version it chose and why.
Once the workspace version sorts above 0.1.4 the re-versioning stops and
`FACELOCK_UPGRADE_TEST_VERSION` becomes a no-op: the lane installs the shipped
version exactly. The native comparator inside the container decides either way,
so a lane can never quietly become a downgrade test. Whatever version it lands
on is spelled by `scripts/release-versions.sh`, the same file the release
workflow uses, so a pre-release candidate reaches the lane as
`0.2.0~alpha.3-1~deb13u1` rather than in a Cargo spelling neither packager
would ever ship. The RPM release counter it passes is local to the lane, not
the series counter from "Prerelease identity conversions" above: the lane's
only ordering requirement is against the published predecessor, never against
a previously published prerelease.

**Upgraders from v0.1.4 already have face authentication enabled.** That
release's `pam-auth-update` profile shipped `Default: yes`, so installing it
switched Facelock on in `common-auth`. The packaged profile is `Default: no`
now, which applies to fresh installs; an upgrade leaves the global stack exactly
as it found it, and the lane fails if it is edited in either direction. Removing
an enabled profile would take face authentication away from someone using it, so
the lane treats that as the more dangerous direction, not a clean result.

**Where it runs.** Locally, by design: a cached run is about twenty minutes and
a cold one considerably more, so `packaging.yml` does not carry it and a
nightly-only job is the follow-up. `just check` runs the container-free contract
(`just test-upgrade-v014-contract`), so a broken lane definition still fails
every pull request.

**Rollback.** The candidate daemon starts and migrates the database before the
downgrade, so the predecessor is handed the file production would hand it. V6
has no down-migration and the schema stays at 6 after the package rolls back.
See `docs/contracts.md` for what that does and does not guarantee.

