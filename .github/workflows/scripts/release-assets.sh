#!/usr/bin/env bash
# Decisions the release publication step delegates, so they can be proven by
# fixture instead of by tagging: the canonical asset allowlist, the maintainer
# tag check, the draft check, the builder digest attestations, and the
# publication manifest.
#
# Nothing here writes to the release or to git. `verify-tag` reads a tag and
# verifies a signature when one is present; it never creates, moves or pushes
# one.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
# shellcheck source=../../../scripts/release-versions.sh
# shellcheck disable=SC1091
source "$REPO_ROOT/scripts/release-versions.sh"

MANIFEST_ASSET='MANIFEST.json'

fail() {
    echo "release assets: $*" >&2
    exit 1
}

# An asset name as an anchored ERE that matches only itself.
literal() {
    printf '%s\n' "$1" | sed -E 's/[]|.[{}()*+?^$\\]/\\&/g'
}

# ------------------------------------------------------------------ allowlist

# The canonical asset names for one validated release identity. Each line is
# `<label><TAB><anchored ERE>`; the RPM's `%{?dist}` tag is decided inside the
# pinned Fedora container, so that one entry is a pattern bound to the
# validated epoch-version-release rather than a literal.
expected_assets() {
    local version="${1:?}" debian_revision="${2:?}" rpm_counter="${3:?}"
    local prerelease="${4:?}" stage="${5:?}"
    local suite architecture debian_version rpm_evr

    case "$stage" in
        builders | final) ;;
        *) fail "unknown allowlist stage: $stage (expected builders or final)" ;;
    esac
    case "$prerelease" in
        true | false) ;;
        *) fail "prerelease must be true or false, got: $prerelease" ;;
    esac
    release_validate_cargo_version "$version" || fail "invalid release version: $version"

    printf 'facelock-binary\t%s\n' "$(literal facelock-x86_64-linux-gnu)"
    printf 'pam-module\t%s\n' "$(literal pam_facelock.so)"
    printf 'polkit-agent\t%s\n' "$(literal facelock-polkit-agent-x86_64-linux-gnu)"

    while IFS='	' read -r suite architecture; do
        debian_version="$(release_debian_version "$version" "$debian_revision" "$suite")" ||
            fail "cannot derive the $suite Debian version"
        printf 'deb-%s\t%s\n' "$suite" \
            "$(literal "facelock_${debian_version}_${architecture}.deb")"
    done < <(debian_suites)

    rpm_evr="$(release_rpm_evr "$version" "$rpm_counter")" ||
        fail "cannot derive the RPM version-release"
    printf 'rpm\t%s\\.fc[0-9]+\\.x86_64\\.rpm\n' "$(literal "facelock-${rpm_evr}")"

    if [ "$prerelease" = false ]; then
        printf 'apt-repo\t%s\n' "$(literal apt-repo.tar.gz)"
    fi
    if [ "$stage" = final ]; then
        printf 'manifest\t%s\n' "$(literal "$MANIFEST_ASSET")"
    fi
}

# The published Debian suites and their architectures, from the release matrix.
debian_suites() {
    python3 - "$REPO_ROOT/dist/release-matrix.json" <<'PY'
import json
import sys

matrix = json.load(open(sys.argv[1], encoding="utf-8"))
for suite, details in sorted(matrix["apt_suites"].items()):
    if suite == "compat":
        continue
    print(f"{suite}\t{details['architecture']}")
PY
}

# Every asset the release carries matches exactly one canonical name, and every
# canonical name is carried exactly once.
verify_assets() {
    local expected_file="${1:?}" actual_file="${2:?}"
    local -a labels=() patterns=() actual=()
    local label pattern name matches duplicate

    while IFS='	' read -r label pattern; do
        [ -n "$label" ] || continue
        labels+=("$label")
        patterns+=("$pattern")
    done <"$expected_file"
    [ "${#labels[@]}" -gt 0 ] || fail "the canonical allowlist is empty"

    mapfile -t actual < <(grep -v '^[[:space:]]*$' "$actual_file" || true)
    [ "${#actual[@]}" -gt 0 ] || fail "the release carries no assets"

    duplicate="$(printf '%s\n' "${actual[@]}" | LC_ALL=C sort | uniq -d)"
    [ -z "$duplicate" ] ||
        fail "duplicate release asset: $(printf '%s' "$duplicate" | tr '\n' ' ')"

    for name in "${actual[@]}"; do
        matches=0
        for pattern in "${patterns[@]}"; do
            [[ "$name" =~ ^${pattern}$ ]] && matches=$((matches + 1))
        done
        [ "$matches" -ne 0 ] || fail "unexpected release asset: $name"
        [ "$matches" -eq 1 ] ||
            fail "allowlist overlap: $name matches $matches canonical names"
    done

    for index in "${!patterns[@]}"; do
        matches=0
        for name in "${actual[@]}"; do
            [[ "$name" =~ ^${patterns[$index]}$ ]] && matches=$((matches + 1))
        done
        [ "$matches" -ne 0 ] ||
            fail "no release asset matches the canonical name ${labels[$index]} (${patterns[$index]})"
        [ "$matches" -eq 1 ] ||
            fail "more than one release asset matches the canonical name ${labels[$index]}"
    done

    echo "release assets: ${#actual[@]} asset(s) match the canonical allowlist"
}

# ----------------------------------------------------------------- identities

# The maintainer's tag is an input to publication, never an output of it.
verify_tag() {
    local tag="${1:?}" version="${2:?}" commit="${3:?}" repo="${4:-.}"
    local expected_tag tagged_commit object_type

    expected_tag="$(release_tag_from_cargo "$version")" ||
        fail "cannot derive the release tag for version $version"
    [ "$tag" = "$expected_tag" ] ||
        fail "tag $tag does not match the validated version $version (expected $expected_tag)"
    git -C "$repo" rev-parse -q --verify "refs/tags/$tag" >/dev/null ||
        fail "tag $tag does not exist in this repository"
    tagged_commit="$(git -C "$repo" rev-list -n 1 "refs/tags/$tag")" ||
        fail "tag $tag does not resolve to a commit"
    [ "$tagged_commit" = "$commit" ] ||
        fail "tag $tag does not point at the built commit $commit (points at $tagged_commit)"

    object_type="$(git -C "$repo" cat-file -t "refs/tags/$tag")"
    if [ "$object_type" = tag ] &&
        git -C "$repo" cat-file tag "$tag" |
        grep -Eq '^-----BEGIN (PGP|SSH) SIGNATURE-----$'; then
        git -C "$repo" tag -v "$tag" >/dev/null 2>&1 ||
            fail "tag $tag signature verification failed; publication needs a verifiable tag"
        echo "release assets: tag $tag verified at $commit (signature verified)"
    else
        echo "release assets: tag $tag verified at $commit (unsigned)"
    fi
}

# The release this workflow is about to publish must still be an unpublished
# draft for this exact tag and channel. A rerun after publication stops here.
verify_draft() {
    local tag="${1:?}" prerelease="${2:?}" release_json="${3:?}"
    python3 - "$tag" "$prerelease" "$release_json" <<'PY'
import json
import sys

tag, prerelease, path = sys.argv[1], sys.argv[2] == "true", sys.argv[3]
releases = json.load(open(path, encoding="utf-8"))
if isinstance(releases, dict):
    candidates = [releases] if releases else []
else:
    candidates = [entry for entry in releases if entry.get("tag_name") == tag]
if len(candidates) != 1:
    raise SystemExit(
        f"release assets: expected exactly one release for tag {tag}, found {len(candidates)}"
    )
release = candidates[0]
if release.get("tag_name") != tag:
    raise SystemExit(
        f"release assets: draft {release.get('id')} belongs to another tag: {release.get('tag_name')}"
    )
if not release.get("draft"):
    raise SystemExit(
        f"release assets: release {release.get('id')} for {tag} is already published; refusing to publish twice"
    )
if bool(release.get("prerelease")) is not prerelease:
    raise SystemExit(
        f"release assets: draft {release.get('id')} carries prerelease="
        f"{release.get('prerelease')}, which is not the validated prerelease identity"
    )
print(f"release assets: draft {release.get('id')} for {tag} is unpublished")
PY
}

# The single release this tag names, read back from the API listing.
read_release() {
    local mode="${1:?}" releases_json="${2:?}" tag="${3:?}"
    python3 - "$mode" "$releases_json" "$tag" <<'PY'
import json
import sys

mode, path, tag = sys.argv[1], sys.argv[2], sys.argv[3]
releases = json.load(open(path, encoding="utf-8"))
if isinstance(releases, dict):
    candidates = [releases] if releases else []
else:
    candidates = [release for release in releases if release.get("tag_name") == tag]
if len(candidates) != 1:
    raise SystemExit(
        f"release assets: expected exactly one release for tag {tag}, found {len(candidates)}"
    )
release = candidates[0]
if mode == "release-id":
    print(release["id"])
else:
    for asset in release.get("assets", []):
        print(asset["name"])
PY
}

# ---------------------------------------------------------------- attestations

# Every asset was produced by exactly one builder and still has the bytes that
# builder attested.
verify_digests() {
    local digests_dir="${1:?}" assets_dir="${2:?}" actual_file="${3:?}"
    python3 - "$digests_dir" "$assets_dir" "$actual_file" "$MANIFEST_ASSET" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

digests_dir, assets_dir, actual_file, manifest_asset = sys.argv[1:5]

attested: dict[str, list[tuple[str, str]]] = {}
for path in sorted(Path(digests_dir).rglob("*.json")):
    document = json.loads(path.read_text(encoding="utf-8"))
    for name, digest in document.get("assets", {}).items():
        attested.setdefault(name, []).append((document.get("job", path.parent.name), digest))

actual = [line.strip() for line in Path(actual_file).read_text(encoding="utf-8").splitlines() if line.strip()]

for name in actual:
    if name == manifest_asset:
        continue
    claims = attested.get(name, [])
    if not claims:
        raise SystemExit(f"release assets: {name} is attested by no builder")
    if len(claims) > 1:
        jobs = ", ".join(sorted(job for job, _ in claims))
        raise SystemExit(f"release assets: {name} is attested by more than one builder: {jobs}")

for name, claims in sorted(attested.items()):
    asset = Path(assets_dir) / name
    if not asset.is_file():
        raise SystemExit(f"release assets: attested asset {name} is not present in the release")
    actual_digest = hashlib.sha256(asset.read_bytes()).hexdigest()
    for job, digest in claims:
        if digest != actual_digest:
            raise SystemExit(
                f"release assets: {name} does not match the digest {job} attested "
                f"({digest} attested, {actual_digest} published)"
            )

print(f"release assets: {len(attested)} asset(s) match their builder attestations")
PY
}

# ------------------------------------------------------------------- manifest

# One document covering every published asset plus the source, build-image and
# component digests the release was built from.
generate_manifest() {
    local tag="${1:?}" version="${2:?}" commit="${3:?}" prerelease="${4:?}"
    local repository="${5:?}" source_sha256="${6:?}" digests_dir="${7:?}"
    local assets_dir="${8:?}" actual_file="${9:?}" output="${10:?}"
    python3 - "$tag" "$version" "$commit" "$prerelease" "$repository" \
        "$source_sha256" "$digests_dir" "$assets_dir" "$actual_file" "$output" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

(tag, version, commit, prerelease, repository, source_sha256,
 digests_dir, assets_dir, actual_file, output) = sys.argv[1:11]

build_images: dict[str, str] = {}
components: dict[str, object] = {}
for path in sorted(Path(digests_dir).rglob("*.json")):
    document = json.loads(path.read_text(encoding="utf-8"))
    job = document.get("job", path.parent.name)
    key = f"{job}:{document['suite']}" if document.get("suite") else job
    if document.get("image"):
        build_images[key] = document["image"]
    for name, value in document.get("components", {}).items():
        components[name] = value

assets = []
for name in sorted({line.strip() for line in Path(actual_file).read_text(encoding="utf-8").splitlines() if line.strip()}):
    asset = Path(assets_dir) / name
    if not asset.is_file():
        raise SystemExit(f"release assets: {name} is not present in the downloaded release assets")
    payload = asset.read_bytes()
    assets.append({
        "name": name,
        "size": len(payload),
        "sha256": hashlib.sha256(payload).hexdigest(),
    })

manifest = {
    "schema": "facelock-release-manifest/1",
    "repository": repository,
    "tag": tag,
    "version": version,
    "commit": commit,
    "prerelease": prerelease == "true",
    "source": {
        "url": f"https://github.com/{repository}/archive/refs/tags/{tag}.tar.gz",
        "sha256": source_sha256,
    },
    "build_images": dict(sorted(build_images.items())),
    "components": dict(sorted(components.items())),
    "assets": assets,
}
Path(output).write_text(json.dumps(manifest, indent=2, sort_keys=False) + "\n", encoding="utf-8")
print(f"release assets: manifest covers {len(assets)} asset(s)")
PY
}

case "${1:-}" in
    expected)
        shift
        [ "$#" -eq 5 ] ||
            fail "usage: $0 expected <version> <debian-revision> <rpm-counter> <prerelease> <stage>"
        expected_assets "$@"
        ;;
    verify)
        shift
        [ "$#" -eq 2 ] || fail "usage: $0 verify <expected-list> <actual-list>"
        verify_assets "$@"
        ;;
    verify-tag)
        shift
        [ "$#" -eq 3 ] || [ "$#" -eq 4 ] ||
            fail "usage: $0 verify-tag <tag> <version> <commit> [repo]"
        verify_tag "$@"
        ;;
    names | release-id)
        mode="$1"
        shift
        [ "$#" -eq 2 ] || fail "usage: $0 $mode <releases-json> <tag>"
        read_release "$mode" "$1" "$2"
        ;;
    verify-draft)
        shift
        [ "$#" -eq 3 ] || fail "usage: $0 verify-draft <tag> <prerelease> <release-json>"
        verify_draft "$@"
        ;;
    verify-digests)
        shift
        [ "$#" -eq 3 ] ||
            fail "usage: $0 verify-digests <digests-dir> <assets-dir> <actual-list>"
        verify_digests "$@"
        ;;
    manifest)
        shift
        [ "$#" -eq 10 ] ||
            fail "usage: $0 manifest <tag> <version> <commit> <prerelease> <repository> <source-sha256> <digests-dir> <assets-dir> <actual-list> <output>"
        generate_manifest "$@"
        ;;
    *)
        fail "usage: $0 {expected|verify|verify-tag|verify-draft|verify-digests|manifest|names|release-id}"
        ;;
esac
