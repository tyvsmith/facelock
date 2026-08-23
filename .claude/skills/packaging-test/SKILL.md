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
| `dist/facelock.spec` | `just test-rpm-pkg` |
| TPM packaging or `facelock-tpm` build features | `just test-deb-trixie-pkg` and `just test-deb-resolute-pkg` |
| `dist/PKGBUILD*`, `dist/facelock.install`, `dist/facelock-pam-remove.hook` | `just test-arch-pkg` |
| `.packit.yaml` schema only | `just test-packit-config` — real `packit` in a pinned Fedora container, seconds |
| `.packit.yaml` semantics, or anything COPR consumes | `just test-copr` — slow, opt-in, Packit SRPM plus a mock from-source rebuild |
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

`test-arch-pkg` is the Arch equivalent and the only recipe that executes
`dist/PKGBUILD` itself: `source=`, `depends`, `makedepends`, `prepare()`,
`build()` and `check()`. `makepkg` runs as a non-root builder, `pacman -U`
installs the result, and the validation covers the installed inventory, the
`facelock.install` scriptlet, the first documented commands, and the libalpm
hook that cleans PAM up on removal. It also resolves every dependency name all
three PKGBUILDs declare, including `PKGBUILD-git`, which is what Omarchy's
`omarchy-pkg-aur-add facelock-git` pulls.

Three things it does not do. It does not boot systemd, so unit runtime
behaviour stays with the deb and rpm gates. It checks no source digest:
`sha256sums` is `SKIP` (#283), the staged tarball is a repack of the working
tree, and the `source=` URL is never fetched, so a wrong URL passes. And it
compiles the workspace twice, release for `build()` and debug for `check()`, so
it is the slowest recipe here. Reach for `test-arch-pam` while iterating and run
this before pushing a packaging change.

When #283 replaces `SKIP` with a real digest, this lane breaks: a staged tarball
cannot match a published sum. Move the staged build to `--skipchecksums` and
assert the real digest separately.

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

`test-copr` and `test-arch-pkg` are slow and opt-in: both compile the workspace
from source inside the container. The Debian and Fedora `-pkg` recipes boot a
container under systemd. Run the narrowest recipe the routing table allows
rather than the whole set.
