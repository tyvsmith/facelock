---
paths:
  - "test/**"
  - "justfile"
  - "crates/**/tests/**"
  - ".github/workflows/**"
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
| 4 | VM testing | Disposable VM with snapshots |
| 5 | Host PAM | After tiers 3-4, with root shell backup |

**Never** install `pam_facelock.so` or edit `/etc/pam.d/*` on the host until container tests pass.
