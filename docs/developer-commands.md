# Developer Commands

This index is derived from Cargo targets and the public justfile metadata. Regenerate
it with `python3 test/docs-inventory.py --write`; `just check-docs` detects drift.

Run recipes from a repository checkout. Recipes can build, download, install, remove,
or publish state: inspect `just --show RECIPE` and read the linked guide before using
one. An entry here records an interface, not evidence that a release or hardware test ran.

## Executables

| Executable | Crate | Reference |
|---|---|---|
| `facelock` | `facelock-cli` | [Reference](cli.md) |
| `facelock-bench` | `facelock-bench` | [Reference](auxiliary-commands.md) |
| `facelock-polkit-agent` | `facelock-polkit` | [Reference](auxiliary-commands.md) |

The PAM module is a shared library, not a command: see [contracts](contracts.md#binaries).

## Prerequisites and effects

- Build/test/lint recipes need the [development dependencies](quickstart.md)
- Package/container recipes need Podman, their declared images and build tools; [testing safety](testing-safety.md) explains the tiers
- Camera/TPM/GPU recipes need the named devices/models; skipped hardware is not verification
- Install/uninstall recipes change system files through sudo; use a disposable guest for testing
- Release recipes may change versions or publish externally; follow [releasing](releasing.md) and inspect the recipe before invocation
- Documentation checks inspect examples; [walkthroughs](testing-walkthrough.md) establish actual clean-system results

## Public recipes

Arguments in square brackets are optional; defaults are shown. This is a syntax
index, so substitute real values for metavariables before running a recipe.

| Invocation | Description |
|---|---|
| `just audit` | Scan Cargo.lock for RustSec advisories; requires cargo-audit and applies .cargo/audit.toml. |
| `just build` | Build in debug mode (development) |
| `just build-release` | Build in release mode (for install) |
| `just check` | Run local tests, lint, format, audit, PAM isolation and documentation/install/release contracts; excludes full packaging and hardware lanes. |
| `just check-agent-docs [base=]` | Check repository instructions and lifecycle contracts; optional base ref adds a coupling check. |
| `just check-docs` | Verify instructional coverage, references and parser acceptance (no example execution). |
| `just check-package-names-live` | Resolve documented dependency names against live upstream repositories (network required). |
| `just check-pam-standalone` | Build PAM independently and reject forbidden async-io backend dependencies. |
| `just check-workflow-policy` | Pin the trust boundary of the comment-triggered Claude workflow (docs/security.md, CI Trust Boundary). |
| `just clean` | Clean build artifacts |
| `just docs-inventory` | Report the tracked documentation, public recipes and Cargo executables as JSON. |
| `just docs-site-check` | Build with mdBook 0.4.44 and check rendered links/assets; retain the temporary site for review. |
| `just fmt` | Format code |
| `just fmt-check` | Format check |
| `just install` | Build release binaries as the invoking user, then elevate for system file installation. |
| `just install-files` | Install pre-built binaries to system (requires root, no build) |
| `just link-models [src=]` | Populate models/*.onnx from an existing checkout or install tree |
| `just lint` | Lint every workspace target with Clippy, denying warnings (matches CI). |
| `just mo` | Compile and validate available PO catalogs into target/locale (requires msgfmt). |
| `just pot` | Regenerate both gettext POT templates from source messages. |
| `just release <version>` | Validate and update release versions, then print the commit/tag/push steps; does not publish. |
| `just release-preflight [tag=]` | Check release prerequisites and pinned evidence; infer tag from Cargo.toml unless supplied. |
| `just show-paths` | Show installed file locations |
| `just test` | Run all unit tests |
| `just test-all` | Run all tests including hardware-dependent (ignored) tests |
| `just test-apt-repo [trixie_manifest=] [resolute_manifest=]` | Test local signed APT publication/client resolution using both supplied manifests or stable stand-in packages. |
| `just test-arch-camera-free` | Automated camera-free E2E tests (Arch container, no camera needed) |
| `just test-arch-camera-required` | Both camera-required E2E tiers, recorded for release-preflight (requires camera) |
| `just test-arch-dev-shell` | Dev shell — interactive Arch container with host models for fast iteration (requires camera) |
| `just test-arch-integration` | Automated daemon integration tests (Arch, requires camera) |
| `just test-arch-layout` | Check installed state-directory permissions and enrollment-marker visibility in Arch. |
| `just test-arch-oneshot` | Automated oneshot (daemonless) integration tests (Arch, requires camera) |
| `just test-arch-package-select` | Test selection of the main Arch package rather than its debug split. |
| `just test-arch-pam` | Automated PAM smoke tests (Arch container) |
| `just test-arch-pkg` | Package test — build the real dist/PKGBUILD with makepkg, install it with pacman, validate |
| `just test-arch-release-shell` | Interactive Arch shell with locally staged binaries, no host model mounts (for camera testing). |
| `just test-cargo-vendor-contract` | Prove the deterministic, exact Cargo source component used by Debian builds. |
| `just test-classify-changes` | Test CI packaging path classification using temporary Git histories. |
| `just test-copr [release=44]` | COPR-equivalent build — Packit SRPM + mock from-source rebuild on a Fedora chroot (slow, opt-in) |
| `just test-copr-lanes` | Every Packit/COPR release target rebuilt from source at its declared depth |
| `just test-copr-pkg [release=44]` | COPR lifecycle lane — mock source rebuild, then the booted package lifecycle |
| `just test-copr-smoke [release=45]` | Branched-release COPR lane — mock source rebuild, then the runtime smoke |
| `just test-deb` | Run both exact supported-suite Debian package gates. |
| `just test-deb-dev-shell` | Dev shell — interactive .deb container with host models for fast iteration (requires camera) |
| `just test-deb-package-contract <manifest>` | Validate every binary package named by one exact generated manifest. |
| `just test-deb-package-contract-test` | Exercise exact Debian manifest identity, checksum, and atomic-staging mutations. |
| `just test-deb-release-shell` | Interactive Ubuntu 26.04 shell with a locally built .deb and test config, no host model mounts. |
| `just test-deb-resolute-pkg` | Ubuntu 26.04 Resolute package — exact source build, TPM/PCR, and booted lifecycle. |
| `just test-deb-source-contract` | Static Debian source/metadata/release-consumer contract. |
| `just test-deb-trixie-pkg` | Debian 13 Trixie package — exact source build, TPM/PCR, and booted lifecycle. |
| `just test-debian-postrm-purge` | Exercise Debian remove/purge policy below disposable fixed roots only. |
| `just test-docs-walkthrough <scenario> <identity> <output>` | Execute one explicit walkthrough scenario using a pinned identity inside a disposable guest. |
| `just test-legacy-system-assets` | Validate immutable system assets and migrate only exact historical /etc copies. |
| `just test-locale-install-contract` | Check locale installation across package paths; compile a fixture when gettext is available. |
| `just test-packaging-matrix` | Every packaging lane the release gate requires, recorded for release-preflight |
| `just test-packit-config` | Packit config schema gate — runs the real `packit` in a digest-pinned Fedora container |
| `just test-release-artifacts` | Static contract: the release publishes exactly once, after validation |
| `just test-release-contract` | Fast release contract tests that do not require distro package tools. |
| `just test-release-matrix` | Complete Track V version/matrix gate. |
| `just test-release-native-ordering` | Native version comparison tools run only inside disposable, digest-pinned containers. |
| `just test-rpm [release=44]` | Test RPM packaging in Fedora container |
| `just test-rpm-authselect [release=44]` | Static and booted, model-free Fedora authselect retirement lifecycle |
| `just test-rpm-dev-shell [release=44]` | Dev shell — interactive .rpm container with host models for fast iteration (requires camera) |
| `just test-rpm-lanes` | Every declared Fedora release target at its declared lifecycle depth |
| `just test-rpm-pkg [release=44]` | Package test — build real .rpm, install via dnf, validate under booted systemd |
| `just test-rpm-release-shell [release=44]` | Interactive Fedora shell with a locally built .rpm and test config, no host model mounts. |
| `just test-rpm-smoke [release=45]` | Branched-release lane — build the package, then boot it for a runtime smoke |
| `just test-source-install-daemon-lifecycle` | Preserve the daemon's pre-install runtime state across source file replacement. |
| `just test-source-install-daemon-lifecycle-systemd` | Exercise the source-install barrier against a real systemd and system bus. |
| `just test-upgrade-v014` | Both released-predecessor upgrade lanes — the stable entrypoint for #231 |
| `just test-upgrade-v014-contract` | Released-predecessor upgrade lanes (#231) — container-free half, runs anywhere |
| `just test-upgrade-v014-deb` | Debian half: install the real v0.1.4 .deb, upgrade to the candidate, roll back |
| `just test-upgrade-v014-pins` | Confirm the pinned v0.1.4 assets are still the assets GitHub serves (needs gh) |
| `just test-upgrade-v014-rpm` | Fedora half: same proof against the released fc44 RPM |
| `just uninstall` | Remove source-installed system assets through sudo; retain biometric state and models. |
| `just uninstall-files` | Uninstall files from system (requires root, called by uninstall) |
| `just version` | Show current version |
