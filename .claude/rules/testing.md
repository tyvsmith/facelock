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
| 3a | Arch container E2E, camera-free | `just test-arch-camera-free` |
| 3b | Arch container E2E (daemon), needs a camera | `just test-arch-integration` |
| 3c | Arch container E2E (oneshot), needs a camera | `just test-arch-oneshot` |
| 4 | VM testing | Disposable VM with snapshots |
| 5 | Host PAM | After tiers 3-4, with root shell backup |

**Never** install `pam_facelock.so` or edit `/etc/pam.d/*` on the host until container tests pass.

Fedora recipes take a release and default to 44 (`just test-rpm-pkg 43`). Tier 3e
covers all three declared targets at the depth `dist/release-matrix.json` gives
each; Rawhide is experimental and never a lane.

## What CI runs, and when

`.github/workflows/ci.yml` gates every pull request: build, test, clippy, audit,
the PAM standalone surface, agent docs, translation catalogs, and tier 3/3a in
`container-pam-test`.

`.github/workflows/packaging.yml` gates the packaged artifacts: tiers 3d and 3e,
both Debian suite lanes, and the native version-ordering matrix. It runs on three
schedules:

| When | What | Filter |
|---|---|---|
| Pull request | every lane | only when the diff reaches a package |
| Nightly (07:00 UTC) | every lane | none |
| `just release-preflight` | lane evidence uploaded by a green run at HEAD, or the marker a local `just test-packaging-matrix` wrote at HEAD | none |

The pull-request filter is a `changes` job running
`.github/workflows/scripts/classify-changes.sh`, plain bash over a merge-base
diff. It is not GitHub's `paths:`, which strands a required check as pending
forever, and not a third-party filter action, which would be another pinned SHA
to review. Add a path there when a new file can reach a built package.

So a green pull request is **not** packaging-verified unless the packaging jobs
actually ran on it. A Rust-only change that breaks the packaged runtime is caught
by the nightly matrix within a day, and by the release gate before it ships.
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
at; `just release-preflight` fails until that record names HEAD.
