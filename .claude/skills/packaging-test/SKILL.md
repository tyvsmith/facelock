---
name: packaging-test
description: Pick and run the right facelock packaging or container test for a change. Use after touching dist/, debian/, the spec, PKGBUILDs, systemd units, D-Bus policy, polkit, the PAM module, or CI packaging jobs. Triggers on "test the packaging", "will this break the deb", "check the rpm", "test the AUR package", "which container test should I run".
---

# Packaging and container tests

CI runs the packaging lanes in `.github/workflows/packaging.yml`: both Debian
suite gates, every declared Fedora lane, the Arch package built from the real
`dist/PKGBUILD`, and the native version-ordering matrix. On a pull request they
run **only when the diff reaches a package**. A `changes` job classifies the
merge-base diff, and every lane sits behind
`if: needs.changes.outputs.packaging == 'true'`. Unfiltered runs happen nightly
and at `just release-preflight`, which refuses to pass without complete lane
evidence at HEAD: the `packaging-evidence-*` artifacts a green run uploads, or
the record a local `just test-packaging-matrix` writes.

Two consequences for you. A green pull request says nothing about packaging
unless those jobs ran on it, so check rather than assume. COPR and the APT repo
are still local-only: nothing gates them until a release tag fires `release.yml`.

Running the right recipe yourself is still the fast way to find out. All of them
need `podman`; none in the routing table needs a camera.

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
| APT repo generation, `publish-apt` workflow, `dist/apt/**` | `just test-apt-repo` — the real publisher, signing, and a clean APT client for every suite, in the pinned trixie container |
| `systemd/`, `dbus/`, `polkit/`, install paths | both Debian suite recipes or `just test-rpm-pkg` — all validate under booted systemd |
| `crates/pam-facelock/**`, `/etc/pam.d` handling | `just test-arch-pam` and `just check-pam-standalone` |
| File layout, installed paths | `just test-arch-layout` |
| D-Bus policy, daemon authorization, pre-flight exit codes, schema migrations | `just test-arch-camera-free` |

`just test-deb` delegates to both exact supported-suite package gates. The
remaining quick syntax-level check is `just test-rpm` (Fedora container), which
is weaker than `test-rpm-pkg` because it does not install and boot.

## Which Fedora

Every Fedora recipe takes a release and defaults to 44 — `just test-rpm-pkg 43`,
`just test-copr 45`. `dist/release-matrix.json` declares 43, 44 and 45, so one
Fedora run is not the whole story: use `just test-rpm-lanes` when a change could
behave differently across releases (dnf/rpm behavior, scriptlets, dependency
resolution, systemd or SELinux versions). It runs 43 and 44 at full lifecycle
depth and branched 45 at build plus runtime smoke, which is the depth the matrix
gives each one.

Never add a Rawhide lane. Rawhide is optional and experimental in the matrix and
cannot substitute for a Fedora 43, 44 or 45 result. Lane images come from the
matrix through `test/fedora-lane-image.sh`, which also refuses a release past its
EOL gate — Fedora 43 stops on 2026-12-02.

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

It also exercises integrity the way the shipped recipe gets it: `dist/PKGBUILD`
carries a fail-closed `__SRC_SHA256__` placeholder that `publish-aur.sh`
finalizes at release time (#283), so the lane refuses a recipe declaring
`SKIP`, proves a wrong digest is rejected, then substitutes the staged
tarball's real digest and lets `makepkg` verify it.

Two things it does not do. It does not boot systemd, so unit runtime
behaviour stays with the deb and rpm gates. And it compiles the workspace
twice, release for `build()` and debug for `check()`, so it is the slowest
recipe here. Reach for `test-arch-pam` while iterating and run this before
pushing a packaging change. The `source=` URL is still never fetched, so a
wrong URL passes.

## Camera-gated tests

These need a real camera and a person in frame, so neither CI nor an agent can
run them:

- `just test-arch-integration` — daemon-mode end to end
- `just test-arch-oneshot` — daemonless end to end
- `just test-arch-camera-required` — both of the above, recorded against HEAD for `just release-preflight`
- `just test-arch-dev-shell`, `test-arch-release-shell`, `test-deb-dev-shell`, `test-rpm-dev-shell`, `test-deb-release-shell`, `test-rpm-release-shell`

**Do not attempt these.** When a change needs one, say so plainly and name the
recipe so a human can run it. Never report a change as validated on the strength
of tests that were skipped.

`just test-arch-camera-free` is the half of the first two that never opens a
camera, and an agent can and should run it. It needs `podman` and the ONNX
models baked into the image (`just link-models` once per checkout); the daemon
half of it refuses to run without them rather than skipping quietly.

Dev shells mount host models for fast iteration; release shells are clean-room
and reproduce the real first-run user experience. Reach for a release shell when
the question is "does a fresh install work", a dev shell when iterating.

## Before claiming packaging works

- `just check` covers test, lint, format, audit and the PAM standalone surface — it does **not** cover packaging
- Name which packaging recipe you ran; if none, say so
- Report camera-gated tests as not run, never as passed
- A green pull request only proves packaging if the `packaging.yml` jobs ran rather than reporting skipped
- `just test-packaging-matrix` is the whole gate in one command (30-60+ minutes), and it writes the lane evidence `just release-preflight` validates; a `FACELOCK_ALLOW_MISSING_MODELS=1` run is a diagnostic — it writes its `partial` per-lane records, but the marker is withheld

## Cost

`test-copr` and `test-arch-pkg` are slow and opt-in: both compile the workspace
from source inside the container. The Debian and Fedora `-pkg` recipes boot a
container under systemd. Run the narrowest recipe the routing table allows
rather than the whole set.
