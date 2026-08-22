---
name: release
description: Cut a facelock release. Overrides the generic releasing skill with this repo's known procedure — just release-preflight, just release, the six version files, and the tag-triggered publish workflow. Use for "cut a release", "release vX.Y.Z", "ship a release", "bump the version", "tag a release".
---

# Releasing facelock

This is the repo-local override of the global `releasing` skill. That skill
discovers a project's release procedure; here the answer is known, so skip
discovery and use what follows. Keep its two habits regardless: preflight
before touching anything, and verify every version file actually changed.

Authoritative reference: `docs/releasing.md`. Read it if anything below
disagrees with the tree — the doc wins and this skill is stale.

## Step 1: Preflight

```bash
just release-preflight v<X.Y.Z>
```

Pass the tag you intend to push. Tags are parsed strictly as `vX.Y.Z` or `vX.Y.Z-{alpha,beta,rc}.N`.
A validated prerelease relaxes the stable secret requirements; a final version
does not. A bare invocation derives the version from `Cargo.toml` and classifies it with the same parser.

It checks local tools (`git`, `cargo`, `just`, `podman`), the packaging files,
and release secrets. Fix every `MISSING` before continuing.

For Debian-family artifacts, the active release set is exactly Trixie and
Resolute. Run both `just test-deb-trixie-pkg` and
`just test-deb-resolute-pkg`; each proves the TPM-enabled package, exact source
components, networkless `.dsc` rebuild, and booted lifecycle.

Then confirm independently:

- working tree clean (`just release` also enforces this and aborts)
- on `main`, in sync with `origin/main`
- CI green **on the exact commit being released** — compare `gh run list --branch main --limit 1 --json conclusion,headSha` against `git rev-parse HEAD`

## Step 2: Bump

```bash
just release <X.Y.Z>
```

Semver only, no `v` prefix — the recipe rejects anything else.

It rewrites **six** files with `sed -i`:

| File | Note |
|---|---|
| `Cargo.toml` | workspace version, inherited by all crates |
| `dist/PKGBUILD` | |
| `dist/PKGBUILD-bin` | per-binary sha256sums are filled in by CI, not here |
| `dist/PKGBUILD-git` | the runtime `pkgver()` computes the real version; this is the fallback |
| `dist/facelock.spec` | |
| `debian/changelog` | prepends a new entry |

Then it runs `cargo check --workspace` and **stops**. It does not commit, tag,
or push — it prints those as instructions. Steps 3 to 5 are yours.

## Step 3: Verify all six changed

The recipe runs six independent `sed -i` calls and checks none of them. A
pattern that fails to match leaves that file at the old version, silently, and
the mismatch does not surface until a package ships wrong.

```bash
git diff --stat
```

All six files above must appear. Then confirm each carries the new version:

```bash
git diff -U0 | rg -n '^\+' | rg '<X.Y.Z>'
```

Any file missing from the diff is a stop. Fix the bump before going further.

## Step 4: CHANGELOG.md

Hand-written, not generated. Draft from commits since the last tag:

```bash
git log "$(git describe --tags --abbrev=0)"..HEAD --oneline
```

Write for someone deciding whether to upgrade. Apply `~/.claude/PROSE.md`.
Never edit an entry for an already-released version.

## Step 5: Commit, tag, push

```bash
git add -A
git commit -m "chore: release v<X.Y.Z>"
git tag v<X.Y.Z>
git push origin main --tags
```

Only after explicit confirmation. No AI attribution in the commit or tag message.

## Step 6: Watch the publish

The `v*` tag triggers `.github/workflows/release.yml`, which has 10 jobs:

```
metadata · build · download-ort · prepare-cargo-vendor
build-deb · build-rpm · build-nix
publish-aur · publish-apt · trigger-pages
```

```bash
gh run watch
gh release view v<X.Y.Z> --json assets --jq '.assets[].name'
```

`publish-aur` and `publish-apt` push to external package repositories. A failure
after those succeed is only partially recoverable — check them first when
triaging a bad release.

## Guardrails

- Never tag or push without explicit confirmation.
- Never proceed when CI is not green on the exact release commit.
- Never hand-edit a version file as a workaround for a failed `just release` — fix the recipe.
- Never edit a changelog entry for a released version.
- Prereleases: use an `alpha`/`beta`/`rc` tag so preflight applies the right secret rules.
