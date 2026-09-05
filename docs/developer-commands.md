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
| `just audit` | Requires cargo-audit: cargo install cargo-audit --locked |
| `just build` | Build in debug mode (development) |
| `just build-release` | Build in release mode (for install) |
| `just check` | Run all checks (test + lint + format + audit + PAM standalone surface + agent docs) |
| `just check-agent-docs [base=]` | Pass a git ref to also run the coupling check against it. |
| `just check-docs` | Verify instructional coverage, references and parser acceptance (no example execution). |
| `just check-package-names-live` | existing. |
| `just check-pam-standalone` | ("Build pam-facelock in isolation" + "Verify pam-facelock dependency surface"). |
| `just check-workflow-policy` | Pin the trust boundary of the comment-triggered Claude workflow (docs/security.md, CI Trust Boundary). |
| `just clean` | Clean build artifacts |
| `just docs-inventory` | Report the tracked documentation, public recipes and Cargo executables as JSON. |
| `just docs-site-check` | Build with mdBook 0.4.44 and check rendered links/assets; retain the temporary site for review. |
| `just fmt` | Format code |
| `just fmt-check` | Format check |
| `just install` | Run as: just install (builds as you, installs as root) |
| `just install-files` | Install pre-built binaries to system (requires root, no build) |
| `just link-models [src=]` | Populate models/*.onnx from an existing checkout or install tree |
| `just lint` | Keep in sync with .github/workflows/ci.yml. |
| `just mo` | that back off. |
| `just pot` | and the output stays byte-stable. |
| `just release <version>` | Usage: just release 0.2.0 |
| `just release-preflight [tag=]` | just release-preflight v0.2.0-rc.1     # prerelease (stable channels excluded) |
| `just show-paths` | Show installed file locations |
| `just test` | Run all unit tests |
| `just test-all` | Run all tests including hardware-dependent (ignored) tests |
| `just test-apt-repo [trixie_manifest=] [resolute_manifest=]` | Needs podman; reprepro, gpg, dpkg-deb and apt all run in the container. |
| `just test-arch-camera-free` | Automated camera-free E2E tests (Arch container, no camera needed) |
| `just test-arch-camera-required` | Both camera-required E2E tiers, recorded for release-preflight (requires camera) |
| `just test-arch-dev-shell` | Dev shell — interactive Arch container with host models for fast iteration (requires camera) |
| `just test-arch-integration` | Automated daemon integration tests (Arch, requires camera) |
| `just test-arch-layout` | defaults) end to end — unit tests cannot. |
| `just test-arch-oneshot` | Automated oneshot (daemonless) integration tests (Arch, requires camera) |
| `just test-arch-package-select` | and the lane that would otherwise catch it is a full release build. |
| `just test-arch-pam` | Automated PAM smoke tests (Arch container) |
| `just test-arch-pkg` | Package test — build the real dist/PKGBUILD with makepkg, install it with pacman, validate |
| `just test-arch-release-shell` | Release shell — clean-room Arch container, real user experience (requires camera) |
| `just test-cargo-vendor-contract` | Prove the deterministic, exact Cargo source component used by Debian builds. |
| `just test-classify-changes` | one-line commits in a temporary directory. |
| `just test-copr [release=44]` | COPR-equivalent build — Packit SRPM + mock from-source rebuild on a Fedora chroot (slow, opt-in) |
| `just test-copr-lanes` | Every Packit/COPR release target rebuilt from source at its declared depth |
| `just test-copr-pkg [release=44]` | COPR lifecycle lane — mock source rebuild, then the booted package lifecycle |
| `just test-copr-smoke [release=45]` | Branched-release COPR lane — mock source rebuild, then the runtime smoke |
| `just test-deb` | Run both exact supported-suite Debian package gates. |
| `just test-deb-dev-shell` | Dev shell — interactive .deb container with host models for fast iteration (requires camera) |
| `just test-deb-package-contract <manifest>` | Validate every binary package named by one exact generated manifest. |
| `just test-deb-package-contract-test` | Exercise exact Debian manifest identity, checksum, and atomic-staging mutations. |
| `just test-deb-release-shell` | Release shell — clean-room .deb container, real user experience (requires camera) |
| `just test-deb-resolute-pkg` | Ubuntu 26.04 Resolute package — exact source build, TPM/PCR, and booted lifecycle. |
| `just test-deb-source-contract` | Static Debian source/metadata/release-consumer contract. |
| `just test-deb-trixie-pkg` | Debian 13 Trixie package — exact source build, TPM/PCR, and booted lifecycle. |
| `just test-debian-postrm-purge` | Exercise Debian remove/purge policy below disposable fixed roots only. |
| `just test-docs-walkthrough <scenario> <identity> <output>` | Execute one explicit walkthrough scenario using a pinned identity inside a disposable guest. |
| `just test-legacy-system-assets` | Validate immutable system assets and migrate only exact historical /etc copies. |
| `just test-locale-install-contract` | `just check` keeps working on a machine that has none. |
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
| `just test-rpm-release-shell [release=44]` | Release shell — clean-room .rpm container, real user experience (requires camera) |
| `just test-rpm-smoke [release=45]` | Branched-release lane — build the package, then boot it for a runtime smoke |
| `just test-source-install-daemon-lifecycle` | Preserve the daemon's pre-install runtime state across source file replacement. |
| `just test-source-install-daemon-lifecycle-systemd` | Exercise the source-install barrier against a real systemd and system bus. |
| `just test-upgrade-v014` | Both released-predecessor upgrade lanes — the stable entrypoint for #231 |
| `just test-upgrade-v014-contract` | Released-predecessor upgrade lanes (#231) — container-free half, runs anywhere |
| `just test-upgrade-v014-deb` | Debian half: install the real v0.1.4 .deb, upgrade to the candidate, roll back |
| `just test-upgrade-v014-pins` | Confirm the pinned v0.1.4 assets are still the assets GitHub serves (needs gh) |
| `just test-upgrade-v014-rpm` | Fedora half: same proof against the released fc44 RPM |
| `just uninstall` | Run as: just uninstall (elevates to root with a trusted command path) |
| `just uninstall-files` | Uninstall files from system (requires root, called by uninstall) |
| `just version` | Show current version |
