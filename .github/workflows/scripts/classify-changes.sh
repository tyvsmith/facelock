#!/usr/bin/env bash
# Decide which packaging lanes a diff can affect.
#
# Usage: classify-changes.sh [BASE_REF [HEAD_REF]]
#
# Emits one `<lane>=true|false` line per lane on stdout and, when running
# under Actions, into $GITHUB_OUTPUT so each job can gate on its own lane with
# `if: needs.changes.outputs.<lane> == 'true'`:
#
#   deb               both Debian suite lifecycle gates
#   rpm               the Fedora direct-RPM lanes, and the COPR lanes off PR
#   arch              the Arch package built from dist/PKGBUILD
#   release_binaries  the Arch-container build the rpm lanes stage from, and
#                     the one lane a Rust-only diff runs
#   release_matrix    the native version-ordering matrix
#   packaging         any of the above
#
# Why this and not a filter action or GitHub's own `paths:`
#
#   A third-party filter action would be a fourth pinned SHA to review on the
#   Monday Renovate opens the bump (#235 pinned every action by commit and took
#   automerge away from them on purpose).
#
#   GitHub's native `paths:` filter skips the *workflow*, and a required check
#   that never runs sits "expected -- waiting for status" forever, so a PR that
#   touches no packaging can never merge. A job-level `if:` reports a real
#   "skipped" conclusion instead, which branch protection accepts.
#
# The filter is deliberately generous. A false positive costs 30-60 minutes of
# runner time; a false negative ships a broken package. A path that only one
# family's recipe or harness reads selects that family's lane; everything else
# that reaches a package selects every lane.
set -euo pipefail

# `pattern=lanes`, first match wins, so a family-specific rule must precede the
# generic rule for the same directory. Lanes are `deb`, `rpm`, `arch`, `matrix`,
# `binaries` or `all`. Note that `*` in a bash pattern match crosses `/`, so `dist/*` is
# `dist/**`.
#
# What a package is built from, plus what its scriptlets execute at install,
# upgrade and removal time:
#
#   debian/ dist/ .packit.yaml       the recipes themselves
#   systemd/ dbus/ config/           payload every package installs and validates
#   scripts/                         release identity, ORT/vendor bundles, and
#                                    the source-install daemon lifecycle the
#                                    deb and rpm gates re-enter
#   test/                            the harnesses the gates are made of
#   justfile                         every lane's entry point
#   Cargo.toml Cargo.lock            the dependency closure the deb source
#                                    build vendors and the spec bundles
#   .github/workflows/               these gates, and the release workflow
#
# And the Rust the maintainer scripts call. `%preun`, Arch's `pre_remove` and
# Debian's `prerm` all run `facelock pam remove --all`, so a change to the PAM
# command can abort a package removal without touching a packaging file.
# lifecycle.rs owns the purge exclusion interval a Debian purge runs inside,
# and daemon.rs is what postinst try-restarts and what pkg-validate.sh starts
# under the hardened unit.
#
# Every other Rust file runs only the release-binaries build: `just
# build-release` in the pinned Arch container proves the workspace still
# compiles the way the packages consume it, in minutes rather than the hour
# the lifecycle lanes take. The deb, rpm and Arch lifecycles stay nightly for
# a Rust-only diff (docs/releasing.md, "Residual risk").
RULES=(
    # Recipes.
    'debian/*=deb'
    'dist/apt/*=deb'
    'dist/PKGBUILD*=arch'
    'dist/facelock.install=arch'
    'dist/facelock-pam-remove.hook=arch'
    'dist/facelock.spec=rpm'
    'dist/rpm/*=rpm'
    '.packit.yaml=rpm'
    'dist/*=all'

    # Payload every package installs, and the scripts every recipe calls.
    'systemd/*=all'
    'dbus/*=all'
    'config/*=all'
    'scripts/*=all'

    # Harnesses. The family-specific ones first; the shared validators, the
    # base Containerfile the Arch image builds on, and the PAM/polkit/TPM
    # checks every booted lane runs stay on every lane.
    'test/Containerfile.deb*=deb'
    'test/Containerfile.apt*=deb'
    'test/*deb*=deb'
    'test/*apt*=deb'
    'test/Containerfile.arch*=arch'
    'test/*arch*=arch'
    'test/Containerfile.fedora=rpm'
    'test/Containerfile.copr*=rpm'
    'test/Containerfile.rpm*=rpm'
    'test/Containerfile.packit=rpm'
    'test/*rpm*=rpm'
    'test/*copr*=rpm'
    'test/*packit*=rpm'
    'test/fedora-lane-image.sh=rpm'
    'test/release-*=matrix'
    'test/Containerfile*=all'
    'test/*pkg*=all'
    'test/packaging-evidence.py=all'
    'test/polkit-agent-validate.sh=all'
    'test/install-pamtester.sh=all'
    'test/pam.d/*=all'
    'test/tpm-pcr-e2e.sh=all'
    'test/check-release-matrix.py=all'

    # Entry points and the dependency closure.
    'justfile=all'
    'Cargo.toml=all'
    'Cargo.lock=all'
    'crates/*/Cargo.toml=all'

    # The gates themselves and the release workflow. ci.yml and the other
    # workflows never build a package and are not listed.
    '.github/workflows/packaging.yml=all'
    '.github/workflows/release.yml=all'
    '.github/workflows/scripts/*deb*=deb'
    '.github/workflows/scripts/*apt*=deb'
    '.github/workflows/scripts/*rpm*=rpm'
    '.github/workflows/scripts/*copr*=rpm'
    '.github/workflows/scripts/*aur*=arch'
    '.github/workflows/scripts/*=all'
    '.github/actions/*=all'

    # The Rust the maintainer scripts execute.
    'crates/facelock-cli/src/commands/pam.rs=all'
    'crates/facelock-cli/src/commands/daemon.rs=all'
    'crates/facelock-cli/src/lifecycle.rs=all'
    'crates/*=binaries'
)

declare -A lane=([deb]=false [rpm]=false [arch]=false [matrix]=false [binaries]=false)

select_lanes() {
    case "$1" in
        all) lane[deb]=true; lane[rpm]=true; lane[arch]=true; lane[matrix]=true; lane[binaries]=true ;;
        # Versions live in debian/changelog, the spec and the PKGBUILD, so any
        # package lane also re-proves their ordering. The rpm lanes stage the
        # release binaries, so rpm implies binaries.
        rpm) lane[rpm]=true; lane[binaries]=true; lane[matrix]=true ;;
        deb|arch) lane[$1]=true; lane[matrix]=true ;;
        matrix|binaries) lane[$1]=true ;;
    esac
}

emit() {
    local reason="$1" any=false selected=()
    local -A out=(
        [deb]="${lane[deb]}"
        [rpm]="${lane[rpm]}"
        [arch]="${lane[arch]}"
        [release_binaries]="${lane[binaries]}"
        [release_matrix]="${lane[matrix]}"
    )
    for name in deb rpm arch release_binaries release_matrix; do
        if [ "${out[$name]}" = true ]; then
            any=true
            selected+=("$name")
        fi
    done
    out[packaging]="$any"
    for name in deb rpm arch release_binaries release_matrix packaging; do
        echo "$name=${out[$name]}"
        if [ -n "${GITHUB_OUTPUT:-}" ]; then
            echo "$name=${out[$name]}" >>"$GITHUB_OUTPUT"
        fi
    done
    echo "packaging lanes: ${selected[*]:-none}  ($reason)"
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        echo "packaging lanes: **${selected[*]:-none}** -- $reason" >>"$GITHUB_STEP_SUMMARY"
    fi
    exit 0
}

# Fail open: when the diff cannot be classified, every lane runs.
decide_all() {
    select_lanes all
    emit "$1"
}

event="${GITHUB_EVENT_NAME:-local}"
base="${1:-${BASE_SHA:-}}"
head="${2:-HEAD}"

# Only a pull request is filtered. The nightly matrix and a manual dispatch are
# unfiltered by design -- they exist to catch what path filtering cannot.
if [ "$event" != "pull_request" ] && [ "$#" -eq 0 ]; then
    decide_all "$event runs the full matrix unfiltered"
fi

if [ -z "$base" ]; then
    decide_all "no merge base to diff against"
fi

if ! files="$(git diff --name-only "$base...$head" 2>&1)"; then
    echo "$files" >&2
    decide_all "cannot diff $base...$head"
fi

if [ -z "$files" ]; then
    decide_all "empty diff against $base"
fi

matched=()
while IFS= read -r file; do
    [ -n "$file" ] || continue
    for rule in "${RULES[@]}"; do
        pattern="${rule%=*}"
        lanes="${rule##*=}"
        # shellcheck disable=SC2053  # the right side is a pattern on purpose
        if [[ $file == $pattern ]]; then
            matched+=("$file -> $lanes")
            select_lanes "$lanes"
            break
        fi
    done
done <<<"$files"

echo "changed files: $(echo "$files" | wc -l)"
if [ ${#matched[@]} -gt 0 ]; then
    printf 'packaging path: %s\n' "${matched[@]}"
    emit "${#matched[@]} changed file(s) reach a package"
fi

emit "no changed file reaches a package"
