---
name: packaging-test
description: Pick and run the right facelock packaging or container test for a change. Use after touching dist/, debian/, the spec, PKGBUILDs, systemd units, D-Bus policy, polkit, the PAM module, or CI packaging jobs. Triggers on "test the packaging", "will this break the deb", "check the rpm", "test the AUR package", "which container test should I run".
---

# Packaging and container tests

CI runs exactly one container job — `container-pam-test` in `ci.yml`. Every
`.deb`, `.rpm`, COPR and APT-repo path is **local only**. If you changed
packaging and did not run one of these yourself, it is untested until a release
tag fires `release.yml`, which is the worst place to find out.

All recipes need `podman`. None of the ones in the routing table need a camera.

## Routing — what you touched, what to run

| Changed | Run |
|---|---|
| `debian/**`, `dist/facelock.spec` shared install logic | `just test-deb-trixie-pkg`, `just test-deb-resolute-pkg`, and `just test-rpm-pkg` |
| `debian/**` only | `just test-deb-trixie-pkg` and `just test-deb-resolute-pkg` |
| `dist/facelock.spec`, `dist/facelock.install` | `just test-rpm-pkg` |
| TPM packaging or `facelock-tpm` build features | `just test-deb-trixie-pkg` and `just test-deb-resolute-pkg` |
| `dist/PKGBUILD*` | `just test-arch-pam`, then a release shell (below) |
| `.packit.yaml`, or anything COPR consumes | `just test-copr` — slow, opt-in, Packit SRPM plus a mock from-source rebuild |
| APT repo generation, `publish-apt` workflow | `just test-apt-repo` — needs `reprepro` and `gpg` |
| `systemd/`, `dbus/`, `polkit/`, install paths | both Debian suite recipes or `just test-rpm-pkg` — all validate under booted systemd |
| `crates/pam-facelock/**`, `/etc/pam.d` handling | `just test-arch-pam` and `just check-pam-standalone` |
| File layout, installed paths | `just test-arch-layout` |

`just test-deb` delegates to both exact supported-suite package gates. The
remaining quick syntax-level check is `just test-rpm` (Fedora container), which
is weaker than `test-rpm-pkg` because it does not install and boot.

## The `-pkg` recipes are the real ones

`test-deb-trixie-pkg`, `test-deb-resolute-pkg`, and `test-rpm-pkg` build a real package,
install it with `dpkg` or `dnf`, and validate under **booted systemd**. That is
the only path that catches unit-file, D-Bus policy, polkit and post-install
scriptlet problems. `test-deb` is an alias for both Debian suite gates; prefer
`test-rpm-pkg` over `test-rpm` whenever the change could affect installed state
rather than just packaging syntax.

## Camera-gated tests

These need a real camera and a person in frame, so neither CI nor an agent can
run them:

- `just test-arch-integration` — daemon-mode end to end
- `just test-arch-oneshot` — daemonless end to end
- `just test-arch-dev-shell`, `test-arch-release-shell`, `test-deb-dev-shell`, `test-rpm-dev-shell`, `test-deb-release-shell`, `test-rpm-release-shell`

**Do not attempt these.** When a change needs one, say so plainly and name the
recipe so a human can run it. Never report a change as validated on the strength
of tests that were skipped.

Dev shells mount host models for fast iteration; release shells are clean-room
and reproduce the real first-run user experience. Reach for a release shell when
the question is "does a fresh install work", a dev shell when iterating.

## Before claiming packaging works

- `just check` covers test, lint, format, audit and the PAM standalone surface — it does **not** cover packaging
- Name which packaging recipe you ran; if none, say so
- Report camera-gated tests as not run, never as passed

## Cost

`test-copr` is self-described as slow and opt-in. The `-pkg` recipes boot a
container under systemd. Run the narrowest recipe the routing table allows
rather than the whole set.
