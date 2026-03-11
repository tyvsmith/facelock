# WS7: CI/CD Enhancements — Spec

**Status:** In Progress

## Planned Changes

### Release workflow (`.github/workflows/release.yml`)
- Trigger on `v*` tags
- Build release binaries + PAM module
- Build .deb package
- Build .rpm package (Fedora container)
- Create GitHub Release with all artifacts + checksums

### CI updates (`.github/workflows/ci.yml`)
- Add libtss2-dev to build deps
- Add TPM feature build + clippy check
- Optional swtpm test job

### Model hosting
- Self-hosted model URLs in manifest.toml pointing to repo releases

## Verification

Tag `v0.1.0`, release workflow runs, all artifacts published.
