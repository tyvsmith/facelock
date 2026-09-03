#!/usr/bin/env bash
# Build one released-predecessor upgrade lane image (#231, Track K).
#
# Usage:
#   build-upgrade-v014-image.sh deb <image> <candidate-deb-output>
#   build-upgrade-v014-image.sh rpm <image>
#
# The Debian half reuses test/build-deb-package-image.sh, so the candidate is
# the same artifact the Debian lifecycle gate proves; the Fedora half derives
# from test/Containerfile.rpm-e2e for the same reason. Neither lane invents its
# own packaging path.
#
# Image tags are taken as arguments rather than fixed, because two lanes sharing
# a constant tag silently overwrite each other when they run at the same time.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# The candidate version is a Cargo version, and neither packager spells one the
# way Cargo does. `0.2.0-alpha.3` is `0.2.0~alpha.3-1~deb13u1` to dpkg and
# `Version: 0.2.0` / `Release: 0.1.alpha.3` to rpm; the hyphenated form is not
# a version either would ever ship, and the Debian one sorts *above* the real
# release, so the "is this an upgrade" guard below would not notice it. Derive
# both from the same file the release pipeline derives them from rather than
# splicing strings here.
# shellcheck source=/dev/null
source "$repo_root/scripts/release-versions.sh"
family="${1:?usage: build-upgrade-v014-image.sh <deb|rpm> <image> [candidate-output]}"
image="${2:?usage: build-upgrade-v014-image.sh <deb|rpm> <image> [candidate-output]}"

candidate_version="$(bash "$repo_root/test/upgrade-v014-candidate-version.sh" version)"
workspace_version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$repo_root/Cargo.toml" | head -1)"
predecessor_version="$(bash "$repo_root/test/upgrade-v014-predecessor.sh" \
    "$([ "$family" = deb ] && echo deb-trixie || echo rpm-fedora)" upstream_version)"

# Always say which version the candidate is built as and why. A lane that
# silently re-versions is a lane whose result nobody can interpret later.
if [ "$(bash "$repo_root/test/upgrade-v014-candidate-version.sh" restamped)" = true ]; then
    cat >&2 <<EOF
NOTE: candidate version $candidate_version (re-versioned).
      The workspace is at $workspace_version, which does not sort above the
      pinned v$predecessor_version predecessor, so the candidate payload is built
      and versioned as $candidate_version to keep this an upgrade rather than a
      downgrade. Override with FACELOCK_UPGRADE_TEST_VERSION. Once the workspace
      version sorts above $predecessor_version this re-versioning stops happening
      and the override does nothing: the lane then installs the shipped version
      exactly. The native package comparator inside the container decides either
      way, so a wrong value here fails the lane rather than passing quietly.
EOF
else
    cat >&2 <<EOF
NOTE: candidate version $candidate_version (as built).
      The workspace version already sorts above the pinned v$predecessor_version
      predecessor, so the lane installs the candidate exactly as it ships and
      FACELOCK_UPGRADE_TEST_VERSION is ignored.
EOF
fi

# Emits an alternating --build-arg / KEY=VALUE stream for `mapfile`, so the
# pin reaches podman as argv entries rather than through a word-split string.
predecessor_build_args() {
    local lane="$1" line
    while IFS= read -r line; do
        printf '%s\n--build-arg\n' "$line" | tac
    done < <(bash "$repo_root/test/upgrade-v014-predecessor.sh" "$lane" --build-args)
}

case "$family" in
    deb)
        candidate_output="${3:?usage: build-upgrade-v014-image.sh deb <image> <candidate-deb-output>}"
        [ "$#" -eq 3 ] || {
            echo "usage: build-upgrade-v014-image.sh deb <image> <candidate-deb-output>" >&2
            exit 2
        }
        base_image="$image-base"
        raw_candidate="$(mktemp -d "${TMPDIR:-/tmp}/facelock-upgrade-v014-deb.XXXXXX")"
        trap 'rm -rf -- "$raw_candidate"' EXIT
        "$repo_root/test/build-deb-package-image.sh" trixie "$base_image" \
            "$raw_candidate/facelock.deb"

        # Re-version the built candidate when the workspace has not been bumped
        # past the predecessor yet. Only DEBIAN/control changes; the payload
        # archive is the assembler's own, byte for byte.
        built_version="$(podman run --rm -v "$raw_candidate:/work:Z" "$base_image" \
            dpkg-deb --field /work/facelock.deb Version)"

        # The revision comes from the package the assembler just built rather
        # than from a constant here, so the two cannot drift apart; the suite
        # is trixie because that is what this lane asked for above. Everything
        # after the last hyphen is the Debian revision, and the suite suffix
        # rides on it -- splitting at the *first* hyphen loses that the moment
        # the upstream version carries one.
        built_revision="${built_version##*-}"
        lane_version="$(release_debian_version "$candidate_version" \
            "${built_revision%%\~*}" trixie)"
        podman run --rm -v "$raw_candidate:/work:Z" \
            -e "LANE_VERSION=$lane_version" "$base_image" \
            bash -euo pipefail -c '
                target="$(dpkg-deb --field /work/facelock.deb Version)"
                if [ "$target" = "$LANE_VERSION" ]; then
                    cp /work/facelock.deb /work/candidate.deb
                else
                    dpkg-deb --raw-extract /work/facelock.deb /work/root
                    sed -i -E "s/^Version:.*/Version: $LANE_VERSION/" /work/root/DEBIAN/control
                    dpkg-deb --build --root-owner-group /work/root /work/candidate.deb >/dev/null
                fi
                dpkg --compare-versions "$(dpkg-deb --field /work/candidate.deb Version)" \
                    gt "$target" ||
                    [ "$(dpkg-deb --field /work/candidate.deb Version)" = "$target" ]
                dpkg-deb --field /work/candidate.deb Version > /work/candidate.version
            '
        stamped_version="$(cat "$raw_candidate/candidate.version")"
        [ "$stamped_version" = "$lane_version" ] || {
            echo "re-stamped candidate reports $stamped_version, expected $lane_version" >&2
            exit 1
        }
        echo "candidate .deb: $built_version -> $lane_version"

        mapfile -t pin_args < <(predecessor_build_args deb-trixie)
        podman build \
            --build-arg "BASE_IMAGE=$base_image" \
            --build-arg "FACELOCK_CANDIDATE_VERSION=$lane_version" \
            "${pin_args[@]}" \
            -t "$image" -f "$repo_root/test/Containerfile.upgrade-v014-deb" "$repo_root"

        install -m 0444 -- "$raw_candidate/candidate.deb" "$candidate_output"
        ;;
    rpm)
        [ "$#" -eq 2 ] || {
            echo "usage: build-upgrade-v014-image.sh rpm <image>" >&2
            exit 2
        }
        # The Fedora lane packages host-built binaries rather than compiling in
        # the container, so binaries that are not this checkout make the lane
        # test something else. That is not hypothetical: a green Debian half and
        # a red Fedora half once turned out to be one lane running the rebased
        # daemon and the other running binaries from eight days earlier.
        #
        # Only the opt-out path needs policing. When the recipe builds them
        # itself, cargo's dependency tracking is authoritative and an mtime
        # comparison here only invents failures -- libpam_facelock.so does not
        # depend on the daemon's sources, so cargo rightly leaves it alone when
        # only those change.
        if [ "${FACELOCK_RELEASE_BINARIES_PREBUILT:-0}" = 1 ]; then
            newest_source="$(git -C "$repo_root" ls-files -z -- \
                '*.rs' 'Cargo.toml' 'Cargo.lock' |
                xargs -0 stat -c %Y | sort -n | tail -1)"
            for binary in facelock facelock-polkit-agent libpam_facelock.so; do
                built="$(stat -c %Y "$repo_root/target/release/$binary" 2>/dev/null || echo 0)"
                [ "$built" -ge "$newest_source" ] || {
                    cat >&2 <<EOF
ERROR: target/release/$binary is older than the workspace source.

FACELOCK_RELEASE_BINARIES_PREBUILT=1 says these binaries are ready to package,
and the Fedora lane packages them as-is, so the lane would test code that is not
this checkout. Rebuild them:

  just build-release

or unset FACELOCK_RELEASE_BINARIES_PREBUILT and let the recipe do it.
EOF
                    exit 1
                }
            done
        fi

        release="$(bash "$repo_root/test/upgrade-v014-predecessor.sh" rpm-fedora release)"
        base_fedora="$(bash "$repo_root/test/fedora-lane-image.sh" "$release")"
        ort_version="$(
            for ort in /usr/lib/libonnxruntime.so /usr/lib64/libonnxruntime.so; do
                if [ -e "$ort" ]; then
                    readlink -f "$ort" | grep -oP '\d+\.\d+\.\d+$'
                    exit
                fi
            done
            echo "1.20.1"
        )"
        base_image="$image-base"
        podman build --build-arg "BASE_IMAGE=$base_fedora" \
            --build-arg "ORT_VERSION=$ort_version" \
            -t "$base_image" -f "$repo_root/test/Containerfile.rpm-e2e" "$repo_root"
        # RPM has nowhere to put a pre-release inside Version: rpmbuild rejects
        # a hyphen there outright, and the release pipeline puts the base
        # version in Version and the pre-release in Release instead. Counter 1
        # matches what `just release` writes for the first build of a version.
        rpm_version="$(release_rpm_version "$candidate_version")"
        rpm_release="$(release_rpm_release "$candidate_version" 1)"
        mapfile -t pin_args < <(predecessor_build_args rpm-fedora)
        podman build \
            --build-arg "BASE_IMAGE=$base_image" \
            --build-arg "FACELOCK_CANDIDATE_RPM_VERSION=$rpm_version" \
            --build-arg "FACELOCK_CANDIDATE_RPM_RELEASE=$rpm_release" \
            "${pin_args[@]}" \
            -t "$image" -f "$repo_root/test/Containerfile.upgrade-v014-rpm" "$repo_root"
        ;;
    *)
        echo "unsupported upgrade lane family: $family" >&2
        exit 2
        ;;
esac
