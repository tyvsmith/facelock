# Testing Strategy, Coverage Gaps, and Deployment Roadmap

Last updated: 2026-03-14

## 1. Current Testing Tiers

### Tier 1: Unit Tests

**How:** `cargo test --workspace` / `just test`
**Status:** Active, runs in CI on every push and PR.

Exercises pure logic across all library crates: config parsing, embedding
comparison, rate limiting, audit log formatting, image preprocessing, alignment
math, database CRUD, TPM sealing/unsealing (with swtpm in CI), liveness
heuristics, quality scoring, and CLI argument parsing.

CI also runs `cargo clippy --workspace -- -D warnings` and `cargo fmt --check`.

### Tier 2: Hardware Tests

**How:** `cargo test --workspace -- --ignored` / `just test-all`
**Status:** Manual only. Requires a physical camera (V4L2 device).

Tests marked `#[ignore]` that need a real camera or IR emitter:
- `facelock-camera`: 2 ignored tests (capture from real device, IR emitter control)
- `facelock-face`: 5 ignored tests (full ONNX pipeline with real frames)

These cannot run in GitHub Actions (no `/dev/video*`). Developers run them
locally before merging camera-related changes.

### Tier 3a: Container PAM Smoke Tests

**How:** `just test-arch-pam` (podman, Arch Linux container)
**Status:** Active, runs in CI (`container-pam-test` job). 13 tests.

Validates the PAM module in an isolated container without a camera:
- Module loads without crashing
- Graceful behavior when daemon is not running
- Handles missing config
- Respects `disabled = true` config
- Exports required PAM symbols (`pam_sm_authenticate`, `pam_sm_setcred`)
- Privilege enforcement (`setup` and `daemon` require root)
- Smart skip: exits quickly with PAM_IGNORE when no faces enrolled
- PAM conversation messages ("Identifying face...")
- Notification mode=off suppresses messages
- Oneshot mode returns quickly with no enrolled faces

### Tier 3b: Container E2E (Daemon Mode)

**How:** `just test-arch-integration` (podman, passes `/dev/video*` devices)
**Status:** Active locally. Cannot run in CI (needs camera). 7 tests.

Full daemon-mode flow inside a container with a real camera:
1. Start daemon, verify it responds to status check
2. List available devices
3. Enroll a face
4. List enrolled models
5. Authenticate via CLI (`facelock test`)
6. Authenticate via PAM (`pamtester`)
7. Clear enrolled models

### Tier 3c: Container E2E (Oneshot Mode)

**How:** `just test-arch-oneshot` (podman, passes `/dev/video*` devices)
**Status:** Active locally. Cannot run in CI (needs camera). 10 tests.

Same as 3b but fully daemonless: verifies no socket exists, enrollment,
listing, CLI auth, `facelock auth` binary (PAM path), pamtester, unknown
user rejection, clear, and empty-after-clear verification.

### Tier 4: VM Testing

**How:** Disposable VM with filesystem snapshots.
**Status:** Manual, ad-hoc. Used before host PAM changes.

Tests the full install/uninstall cycle (`just install` / `just uninstall`),
systemd service activation, D-Bus policy, PAM stack integration with real
`/etc/pam.d/sudo`, and upgrade paths.

### Tier 5: Host PAM

**How:** Direct install on developer machine with root shell backup.
**Status:** Manual, last resort. Only after tiers 3-4 pass.

Real-world validation of sudo/login integration. Always keep a root shell
open as a recovery path.

## 2. Test Coverage Audit

### Unit test counts by crate (approximate)

| Crate | Approx. tests | Notes |
|-------|-------------:|-------|
| facelock-core | ~40+ | Good coverage: config parsing, types, paths, D-Bus types |
| facelock-camera | ~40+ | Good: preprocessing, device detection, quirks. A few hardware tests ignored. |
| facelock-face | ~20 | Moderate: alignment, detector, embedder, model verification. Some integration tests need camera+models. |
| facelock-store | ~30 | Good: SQLite CRUD, embedding storage, migration. |
| facelock-daemon | ~50+ | Strong: rate limiting, audit, quality scoring, auth logic, liveness. |
| facelock-cli | ~60+ | Strong: daemon command, preview rendering, bench, list, encrypt, setup, TPM, IPC client, notifications. |
| facelock-tpm | ~25+ | Good: PCR policy, sealing/unsealing (swtpm in CI). |
| pam-facelock | 0 | No unit tests. Tested via container PAM smoke (Tier 3a). |
| facelock-polkit | 0 | No tests. Agent is not production-ready. |
| facelock-bench | 0 | Benchmark binary, not a test target. |
| facelock-test-support | 0 | Mocks/fixtures only, no self-tests. |
| **Total** | **~270+** | |

### CI jobs

| Job | Trigger | What it does |
|-----|---------|--------------|
| `build-and-test` | push/PR to main | fmt check, build, build+tpm, clippy, clippy+tpm, `cargo test` |
| `tpm-tests` | after build-and-test | Starts swtpm, runs `cargo test --features tpm` |
| `container-pam-test` | after build-and-test | Release build, podman container, 13 PAM smoke tests |

### Assessment

- **Well-covered:** core, camera (non-hardware), store, daemon, cli, tpm.
- **Adequately covered via integration:** pam-facelock (Tier 3a container tests cover the critical paths).
- **Thin:** facelock-face has only 12 non-ignored unit tests for ONNX inference logic. Model loading and SHA256 verification are tested, but embedding comparison edge cases are limited.
- **Uncovered:** facelock-polkit has zero tests and is flagged as not production-ready.

## 3. Identified Gaps

### No hardware CI
Cameras are unavailable in GitHub Actions. Tiers 2, 3b, and 3c only run on
developer machines. A self-hosted runner with a USB camera would close this
gap, but is not worth the maintenance cost at this stage.

### No fuzzing
No `cargo-fuzz` targets exist. High-value fuzz targets would be:
- ONNX model parsing (malformed `.onnx` files)
- Image preprocessing pipeline (corrupt/malformed frames)
- Config TOML parsing (adversarial config files)
- Embedding comparison (NaN, infinity, zero-length vectors)

### No coverage reporting
Neither `cargo-tarpaulin` nor `llvm-cov` is configured. No visibility into
which code paths are actually exercised by the unit tests.

### No supply chain auditing
No `cargo-audit` or `cargo-deny` in CI. The project pulls in `ort` (ONNX
Runtime), `rusqlite` (bundled SQLite), `zbus`, and other non-trivial
dependencies that should be audited for known vulnerabilities.

### No property-based testing
No `proptest` or `quickcheck` usage. Embedding comparison (cosine
similarity with `subtle` constant-time operations) and rate-limit window
logic are good candidates for property-based tests.

### No load/stress testing for daemon
The daemon has rate limiting (5 attempts/user/60s) but no stress test to
verify it holds under concurrent connections or resource exhaustion.

### No benchmarks in CI
Benchmarks exist (`facelock bench`) but are manual-only. No regression
detection for inference latency or auth round-trip time.

### PAM module has no unit tests
`pam-facelock` has zero `#[test]` functions. The container tests in Tier 3a
cover runtime behavior, but there are no tests for config parsing logic,
D-Bus client construction, or error-path handling within the crate itself.

### Polkit agent untested
`facelock-polkit` has no tests at all. It is explicitly marked as not
production-ready in the justfile install recipe.

## 4. Deployment Roadmap

### Current state: dev builds only
- `just install` / `just uninstall` for local development
- No published packages in any repository

### Packaging status

| Format | Location | Status |
|--------|----------|--------|
| Raw binaries | `release.yml` | Working. Triggered on `v*` tags. Uploads `facelock-x86_64-linux-gnu`, `pam_facelock.so`, SHA256SUMS to GitHub Releases. |
| `.deb` | `release.yml` (build-deb job) | Working. Built in CI, uploaded to GitHub Release. Not in any PPA. |
| `.rpm` | `release.yml` (build-rpm job) | Working. Built on Fedora container in CI, uploaded to GitHub Release. Includes authselect profile. Not in COPR. |
| PKGBUILD (Arch) | `dist/PKGBUILD` | Exists but not submitted to AUR. References `facelock.install` file. |
| Nix flake | `dist/nix/flake.nix` | Exists with NixOS module (`module.nix`), derivation (`default.nix`), and dev shell. Not in nixpkgs. `doCheck = false` (needs camera). |
| openrc | `dist/openrc/facelock-daemon` | Init script exists. |
| runit | `dist/runit/run`, `dist/runit/log/run` | Service scripts exist. |
| s6 | `dist/s6/facelock-daemon/run` | Service script exists. |

### Model hosting
- Default: upstream URLs (visomaster GitHub releases, HuggingFace)
- Self-hosted mirror: GitHub release tag `v0.1.0-models` (documented in `models/manifest.toml`)
- Models are downloaded at runtime via `facelock setup`, not bundled in packages
- 4 models total: scrfd_2.5g (3MB), arcface_r50 (166MB), scrfd_10g (17MB, optional), arcface_r100 (249MB, optional)
- SHA256 verified at download time and at model load time

### GitHub release workflow
- Triggered by pushing a `v*` tag
- Builds release binaries (with TPM support), .deb, and .rpm
- Uploads all artifacts to the GitHub Release with auto-generated release notes
- Single architecture: x86_64 only

### Distro submission path

| Target | Effort | Blockers |
|--------|--------|----------|
| AUR | Low | PKGBUILD exists. Needs `source=()` array pointing to release tarball, `.SRCINFO`, and AUR account. |
| PPA (Ubuntu/Debian) | Medium | .deb builds work. Needs Launchpad account, GPG signing, and `dput` integration. |
| COPR (Fedora) | Medium | .spec exists. Needs COPR account and source RPM workflow. |
| nixpkgs | Medium | Flake works. Needs PR to nixpkgs with proper `fetchFromGitHub` source, maintainer entry. |
| Distro main repos | High | Requires stable release, maintainer adoption, and review process per distro. Long-term goal. |

## 5. Recommended Improvements

Listed in priority order (highest impact, lowest effort first).

### P0: Supply chain security
Add `cargo-audit` and `cargo-deny` to CI. Single step: add a job that runs
`cargo audit` and `cargo deny check`. Catches known CVEs in dependencies
before they ship.

### P1: Coverage reporting
Add `cargo-llvm-cov` to CI with a coverage summary comment on PRs. Provides
visibility into what the ~270+ tests actually cover and highlights dead code.

### P2: PAM module unit tests
Add unit tests to `pam-facelock` for config parsing, D-Bus client fallback
logic, and PAM_IGNORE edge cases. The crate is security-critical but has
zero in-crate tests.

### P3: Property-based tests for embeddings
Add `proptest` tests in `facelock-daemon` and `facelock-face` for:
- Cosine similarity: symmetry, range [0,1], identity property
- Rate limiter: monotonic window advancement, never exceeds limit
- Config round-trip: serialize/deserialize identity

### P4: Fuzz targets
Create `cargo-fuzz` targets for ONNX model loading and image preprocessing.
These are the primary attack surfaces (untrusted model files, malformed
camera frames).

### P5: AUR submission
The PKGBUILD is ready. Submit to AUR to get early adopters and feedback from
the Arch Linux community.

### P6: Benchmark regression detection
Run `facelock bench` in CI on a consistent runner and store results. Flag
PRs that regress inference latency by more than 10%. Can use
`criterion`-based benchmarks with `github-action-benchmark`.

### P7: Self-hosted runner for hardware tests
Set up a self-hosted GitHub Actions runner with a USB IR camera to run
Tiers 2, 3b, and 3c automatically. Only worthwhile once the project has
regular contributors.
