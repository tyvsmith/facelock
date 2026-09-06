# Testing Strategy and Remaining Gaps

This page describes the current strategy, not the test counts or publication
state recorded by older releases. Counts drift with every patch; the test tree,
CI workflows, release matrix, and generated
[Developer Commands](developer-commands.md) inventory are authoritative.

## Current layers

| Layer | Scope | Typical entry point |
|-------|-------|---------------------|
| Unit and static | workspace logic, formatting, clippy, contracts, docs, supply chain | `just check` |
| Hardware | ignored camera/model tests | `cargo test --workspace -- --ignored` |
| Container | PAM smoke, camera-free E2E, real-camera daemon/oneshot flows | `just test-arch-pam`, `just test-arch-integration`, `just test-arch-oneshot` |
| Package | Debian suites, Fedora direct RPM and COPR modes, Arch package, upgrade/lifecycle behavior | package matrix recipes and `.github/workflows/packaging.yml` |
| Booted guest | clean-install and authentication walkthrough evidence | [Testing Walkthrough](testing-walkthrough.md) |
| Host PAM | final manual confidence only, with retained root recovery shell | [Testing Safety](testing-safety.md) |

The PAM crate is not untested: it has Rust tests and isolated module/dependency
checks, and the container tiers exercise PAM loading and conversations. The
polkit crate also has unit tests and a container D-Bus boundary. That does not
make the experimental polkit agent production-ready; live desktop agent
selection and password-agent fallback remain separate evidence gaps.

Supply-chain auditing is active. `just audit` runs `cargo audit` with the
repository policy, and `.github/workflows/ci.yml` has a dedicated
`cargo-audit` job. Do not repeat the historical claim that no RustSec audit
exists.

## What CI establishes

The main CI workflow covers build/test/clippy, the PAM dependency ceiling,
RustSec, TPM tests, agent-document consistency, catalogs, PAM smoke, and the
camera-free E2E tier. Packaging has its own workflow and change classifier. A
green pull request does not imply that path-filtered packaging jobs ran; the
nightly and release preflight provide the unfiltered package evidence.

Camera-required tests do not run on ordinary hosted runners. Their evidence is
commit-bound at release time. Keep new assertions in the camera-free tier
unless they genuinely require a frame.

## Remaining gaps

- no hosted real-camera CI or maintained self-hosted camera runner
- no broad hardware matrix beyond the devices named in
  [Compatibility](compatibility.md)
- no production claim for live desktop polkit-agent coexistence/fallback
- no continuous inference-performance regression gate across CPU/GPU hardware
- no fuzzing or property-test program for config, image, and protocol inputs
- no repository-wide coverage percentage reporting
- no packaged distribution claim for the source-only OpenRC, runit, s6, or Nix
  integration fragments

These are limits, not implied support. A source template, successful local
build, staged artifact, or passing rebuilt package is evidence only for the
case it actually exercised.

## Release and packaging status

v0.2.0 is the latest published stable release, with direct package artifacts.
Prereleases do not enter stable APT, AUR, or production COPR. The AUR entries,
both APT suites, and the production COPR all serve 0.2.0; Fedora staging is a
candidate channel. The exact supported release matrix is Fedora 43/44/45,
Debian 13, and Ubuntu 26.04, not RHEL.

A stable release is expected to carry two suite-specific `.deb` artifacts for
trixie and resolute. v0.2.0 carries both, and both codenamed APT suites are
published from them.

The Nix flake/module and OpenRC, runit, and s6 templates live in the source
tree. Nix is not in nixpkgs and its derivation disables `doCheck`; the init
templates have no independent package channel. Their existence is not a
published or boot-tested distribution result.
