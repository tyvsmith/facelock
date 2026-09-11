---
paths:
  - "test/**"
  - "justfile"
  - "crates/**/tests/**"
  - ".github/workflows/**"
  - ".github/actions/**"
---

# Testing Strategy

| Tier | What | How |
|------|------|-----|
| 1 | Unit tests | `cargo test --workspace` |
| 2 | Hardware tests | `cargo test --workspace -- --ignored` |
| 3 | Arch container PAM smoke | `just test-arch-pam` |
| 3b | Arch container E2E (daemon) | `just test-arch-integration` |
| 3c | Arch container E2E (oneshot) | `just test-arch-oneshot` |
| 3d | Arch package from `dist/PKGBUILD` | `just test-arch-pkg` |
| 3e | Fedora package lifecycle, every declared release | `just test-rpm-lanes` |
| 3f | Fedora COPR path, rebuilt from source on system ORT | `just test-copr-lanes` |
| 3g | Released v0.1.4 upgrade and rollback, deb and rpm | `just test-upgrade-v014` |
| 3a | Arch container E2E, camera-free | `just test-arch-camera-free` |
| 3b | Arch container E2E (daemon), needs a camera | `just test-arch-integration` |
| 3c | Arch container E2E (oneshot), needs a camera | `just test-arch-oneshot` |
| 3h | 3b + 3c against a synthetic v4l2loopback camera, no person | `just test-arch-loopback` |
| 4 | VM testing | Disposable VM with snapshots |
| 5 | Host PAM | After tiers 3-4, with root shell backup |

**Never** install `pam_facelock.so` or edit `/etc/pam.d/*` on the host until container tests pass.

Tier 3f is not a duplicate of 3e. 3e builds the direct `.rpm` from host
binaries with a bundled ONNX Runtime; 3f rebuilds the package from source in a
mock chroot and runs it against Fedora's system ONNX Runtime, which is the path
Packit publishes to COPR. The release gate requires both, and a direct-RPM
record cannot satisfy a COPR target. The COPR lanes never run on a pull request
-- mock needs a privileged container -- so only the nightly, a
`workflow_dispatch`, or a local run covers them.

Fedora recipes take a release and default to 44 (`just test-rpm-pkg 43`). Tier 3e
covers all three declared targets at the depth `dist/release-matrix.json` gives
each; Rawhide is experimental and never a lane.

## What CI runs, and when

`.github/workflows/ci.yml` gates every pull request: format, clippy (with and
without `tpm`), test, docs contracts, the PAM standalone surface, audit, agent
docs, translation catalogs, tier 3/3a in `container-pam-test`, and tier 3h in
`loopback-e2e` (the runner is a VM, so it can load the out-of-tree v4l2loopback
module). The `build-and-test` job is a sequence of justfile recipes
(`fmt-check`, `lint`, `lint-tpm`, `test`, `check-docs`,
`check-pam-standalone`, `build-smoke-binaries`), so what CI checks is what the
recipe says; there is no separate `cargo build` step because clippy
`--all-targets` type-checks every target and `test` builds the workspace.
`tpm-tests` is the only job that runs `cargo test --features tpm`.

`.github/workflows/packaging.yml` gates the packaged artifacts: tiers 3d and 3e,
both Debian suite lanes, and the native version-ordering matrix. It runs on three
schedules:

| When | What | Filter |
|---|---|---|
| Pull request | every lane except COPR; Debian lanes without the `.dsc` rebuild | per lane, only the lanes the diff reaches |
| Nightly (07:00 UTC) | every lane | none |
| `just release-preflight` | lane evidence uploaded by a green run at HEAD, or the marker a local `just test-packaging-matrix` wrote at HEAD | none |

The pull-request filter is a `changes` job running
`.github/workflows/scripts/classify-changes.sh`, plain bash over a merge-base
diff and emits one output per lane (`deb`, `rpm`, `arch`, `release_binaries`,
`release_matrix`). It is not GitHub's `paths:`, which strands a required check
as pending forever, and not a third-party filter action, which would be another
pinned SHA to review. Add a path there when a new file can reach a built
package, mapped to one family's lane when only that family reads it and to
every lane otherwise; the table is in docs/releasing.md and
`just test-classify-changes` pins it.

The Debian lanes on a pull request run with `FACELOCK_DEB_SKIP_DSC_REBUILD=1`:
the candidate `.deb` is still built from source and put through the booted
lifecycle, but the clean-image rebuild of the emitted `.dsc`, a second full LTO
compile, is left to the nightly and the dispatch (#337). Such a lane records
`depth=partial`, which `just release-preflight` refuses.

So a green pull request is **not** packaging-verified unless the packaging jobs
actually ran on it. A change to Rust source alone (Cargo manifests and the
lockfile reach every lane) runs only the release-binaries build on its pull
request; if it breaks the packaged runtime, the nightly matrix catches
it within a day, and the release gate before it ships.
`just test-packaging-matrix` runs every lane locally and records each lane's
evidence, for a maintainer without CI in reach; a run that skipped anything is
refused, not recorded.

## Which E2E tier a new assertion belongs in

Tier 3a is everything in the two E2E suites that reaches its subject before any
capture: bus policy, D-Bus authorization, pre-flight rejections and their exit
codes, schema migrations, the shape of the status document. CI runs it on every
pull request. Tiers 3b and 3c keep only what a real sensor produces: a frame, a
match, a device fingerprint, a warm-hold timing.

Put a new assertion in 3a unless it needs a frame. An assertion parked in 3b or
3c that did not need one is unwatched: those tiers run on one machine, and three
of their assertions rotted there undetected (#139).

Tiers 3b and 3c are gated at release time, not at review time.
`just test-arch-camera-required` runs both and records the commit they passed
at in `.hardware-tiers-verified`; `just release-preflight` fails until that
record or tier 3h's names HEAD (`test/e2e-tier-evidence.sh`). What each
proves differs: 3b/3c on a real sensor prove real frames of a real face
match; 3h proves the pipeline on a device the product treats as an IR sensor.

Tier 3h runs the same two scripts against a v4l2loopback node fed with a
procedurally rendered face (`test/loopback/`, nobody's face), so it needs
no camera and no person. The fed node enumerates GREY only and classifies as
IR by format evidence — the residual `docs/security.md` §A documents — and
the run keeps `require_ir` and `require_frame_variance` on because the
sequence drifts frame to frame the way a person does. It records
`.loopback-tier-verified`, which satisfies the release gate on its own. It is
cheaper evidence, not the same evidence: only 3b/3c on a real sensor say that
real frames match a real face. The fixture's bands against
the real models are pinned by
`crates/facelock-daemon/tests/synthetic_face_contract.rs` (tier 2). Never
loosen a classification or liveness rule to make 3h pass.
