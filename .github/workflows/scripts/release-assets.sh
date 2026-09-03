#!/usr/bin/env bash
# Decisions the release publication step delegates, so they can be proven by
# fixture instead of by tagging: the canonical asset allowlist, staging those
# assets out of the builders' workflow artifacts, the maintainer tag check, the
# draft checks, the builder digest attestations, and the publication manifest.
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
# `<label><TAB><anchored ERE>`; the RPMs' `%{?dist}` tag is decided inside the
# pinned Fedora container, so those entries are patterns bound to the validated
# epoch-version-release rather than literals.
expected_assets() {
    local version="${1:?}" debian_revision="${2:?}" rpm_counter="${3:?}"
    local prerelease="${4:?}" stage="${5:?}"
    local suite architecture debian_version rpm_evr rpm_kind rpm_prefix

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
    # v0.1.4 published the payload package and its two debug packages; the
    # release keeps all three, each bound to the validated version-release.
    for rpm_kind in payload debuginfo debugsource; do
        rpm_prefix=''
        [ "$rpm_kind" = payload ] || rpm_prefix="$rpm_kind-"
        printf 'rpm-%s\t%s\\.fc[0-9]+\\.x86_64\\.rpm\n' "$rpm_kind" \
            "$(literal "facelock-${rpm_prefix}${rpm_evr}")"
    done

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
    local label pattern name matches duplicate index

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
        [ "$matches" -ne 0 ] ||
            fail "unexpected release asset: $name; an earlier run at another version leaves one behind, and it is removed with: gh release delete-asset \"\$TAG\" $name"
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

# -------------------------------------------------------------------- staging

# Collect exactly the canonical assets out of the builders' workflow artifacts.
# The allowlist decides what a release carries, so nothing else is copied and no
# builder can smuggle a file in beside the one it was asked to produce.
stage_assets() {
    local expected_file="${1:?}" artifacts_dir="${2:?}" assets_dir="${3:?}"
    python3 - "$expected_file" "$artifacts_dir" "$assets_dir" <<'PY'
import re
import shutil
import sys
from pathlib import Path

expected_file, artifacts_dir, assets_dir = sys.argv[1:4]

candidates: list[Path] = [path for path in Path(artifacts_dir).rglob("*") if path.is_file()]
destination = Path(assets_dir)
destination.mkdir(parents=True, exist_ok=True)

staged: list[str] = []
for line in Path(expected_file).read_text(encoding="utf-8").splitlines():
    if not line.strip():
        continue
    label, _, pattern = line.partition("\t")
    matched = [path for path in candidates if re.fullmatch(pattern, path.name)]
    if not matched:
        raise SystemExit(
            f"release assets: no builder artifact provides the canonical asset {label} ({pattern})"
        )
    names = {path.name for path in matched}
    if len(names) > 1 or len(matched) > 1:
        found = ", ".join(sorted(str(path) for path in matched))
        raise SystemExit(
            f"release assets: more than one builder artifact provides {label}: {found}"
        )
    shutil.copy2(matched[0], destination / matched[0].name)
    staged.append(matched[0].name)

for name in staged:
    print(name)
PY
}

# ----------------------------------------------------------------- identities

# The maintainer's tag is an input to publication, never an output of it.
verify_tag() {
    local tag="${1:?}" version="${2:?}" commit="${3:?}" repo="${4:-.}"
    local expected_tag tagged_commit built_commit object_type

    expected_tag="$(release_tag_from_cargo "$version")" ||
        fail "cannot derive the release tag for version $version"
    [ "$tag" = "$expected_tag" ] ||
        fail "tag $tag does not match the validated version $version (expected $expected_tag)"
    git -C "$repo" rev-parse -q --verify "refs/tags/$tag" >/dev/null ||
        fail "tag $tag does not exist in this repository"
    tagged_commit="$(git -C "$repo" rev-list -n 1 "refs/tags/$tag")" ||
        fail "tag $tag does not resolve to a commit"
    # GITHUB_SHA is the tag object for an annotated tag; peel both sides.
    built_commit="$(git -C "$repo" rev-parse -q --verify "${commit}^{commit}")" ||
        fail "the built revision $commit does not resolve to a commit"
    [ "$tagged_commit" = "$built_commit" ] ||
        fail "tag $tag does not point at the built commit $built_commit (points at $tagged_commit)"

    object_type="$(git -C "$repo" cat-file -t "refs/tags/$tag")"
    if [ "$object_type" = tag ] &&
        git -C "$repo" cat-file tag "$tag" |
        grep -Eq '^-----BEGIN (PGP|SSH) SIGNATURE-----$'; then
        git -C "$repo" tag -v "$tag" >/dev/null 2>&1 ||
            fail "tag $tag signature verification failed; publication needs a verifiable tag"
        echo "release assets: tag $tag verified at $built_commit (signature verified)"
    else
        echo "release assets: tag $tag verified at $built_commit (unsigned)"
    fi
}

# The release listing this tag names, read back from the API.
#
# `verify-creatable` runs before the draft is written: this tag may have no
# release at all, or the draft an interrupted run left behind, never a published
# one. `verify-draft` runs before the flip. `names` and `release-id` read the
# selected release. Accepts a JSON array, one release object, gh's paginated
# object stream, and gh's slurped pages.
release_query() {
    local mode="${1:?}" releases_json="${2:?}" tag="${3:?}" prerelease="${4:-}"
    python3 - "$mode" "$releases_json" "$tag" "$prerelease" <<'PY'
import json
import sys
from pathlib import Path

mode, path, tag, prerelease = sys.argv[1:5]


def load(source: str) -> tuple[list, bool]:
    """Every shape gh emits: a JSON array, one object, a paginated object
    stream, or slurped pages. The flag says the caller already selected one."""
    text = Path(source).read_text(encoding="utf-8")
    try:
        document = json.loads(text) if text.strip() else []
    except json.JSONDecodeError:
        document = [json.loads(line) for line in text.splitlines() if line.strip()]
    if isinstance(document, dict):
        return ([document] if document else []), True
    releases: list = []
    for entry in document:
        if isinstance(entry, list):
            releases.extend(entry)
        else:
            releases.append(entry)
    return releases, False


releases, selected = load(path)
# A single object is a caller-selected release: hold it to the tag it claims.
candidates = releases if selected else [r for r in releases if r.get("tag_name") == tag]

if mode == "verify-creatable":
    if not candidates:
        print(f"release assets: no release exists for {tag} yet")
        raise SystemExit(0)
    if len(candidates) > 1:
        raise SystemExit(f"release assets: {len(candidates)} releases already exist for {tag}")
    release = candidates[0]
    if not release.get("draft"):
        raise SystemExit(
            f"release assets: release {release.get('id')} for {tag} is already published; "
            "refusing to publish twice"
        )
    print(f"release assets: reusing the unpublished draft {release.get('id')} for {tag}")
    raise SystemExit(0)

if len(candidates) != 1:
    raise SystemExit(
        f"release assets: expected exactly one release for tag {tag}, found {len(candidates)}"
    )
release = candidates[0]

if mode == "release-id":
    print(release["id"])
elif mode == "names":
    for asset in release.get("assets", []):
        print(asset["name"])
elif mode == "verify-draft":
    if release.get("tag_name") != tag:
        raise SystemExit(
            f"release assets: draft {release.get('id')} belongs to another tag: {release.get('tag_name')}"
        )
    if not release.get("draft"):
        raise SystemExit(
            f"release assets: release {release.get('id')} for {tag} is already published; "
            "refusing to publish twice"
        )
    if bool(release.get("prerelease")) is not (prerelease == "true"):
        raise SystemExit(
            f"release assets: draft {release.get('id')} carries prerelease="
            f"{release.get('prerelease')}, which is not the validated prerelease identity"
        )
    print(f"release assets: draft {release.get('id')} for {tag} is unpublished")
else:
    raise SystemExit(f"release assets: unknown release query: {mode}")
PY
}

# ---------------------------------------------------------------- attestations

# Every asset was produced by exactly one builder and still has the bytes that
# builder attested.
verify_digests() {
    local digests_dir="${1:?}" assets_dir="${2:?}" actual_file="${3:?}"
    python3 - "$digests_dir" "$assets_dir" "$actual_file" <<'PY'
import hashlib
import json
import sys
from pathlib import Path

digests_dir, assets_dir, actual_file = sys.argv[1:4]


def attestations(root: str) -> list[Path]:
    """Digest attestations, and only from the artifacts that carry them.

    The publish job downloads the payload artifacts into the same tree. A
    `digests.json` anywhere else is a builder claiming provenance for work it
    did not do, so it stops the release rather than being skipped."""
    base = Path(root)
    smuggled = [
        path
        for path in sorted(base.rglob("digests.json"))
        if not path.relative_to(base).parts[0].startswith("release-digests-")
    ]
    if smuggled:
        raise SystemExit(
            f"release assets: {smuggled[0]} is a digest attestation inside a payload artifact"
        )
    return sorted(base.glob("release-digests-*/**/digests.json"))


attested: dict[str, list[tuple[str, str]]] = {}
for path in attestations(digests_dir):
    document = json.loads(path.read_text(encoding="utf-8"))
    for name, digest in document.get("assets", {}).items():
        attested.setdefault(name, []).append((document.get("job", path.parent.name), digest))

actual = [line.strip() for line in Path(actual_file).read_text(encoding="utf-8").splitlines() if line.strip()]

for name in actual:
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
# component digests the release was built from. It cannot cover itself.
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

def attestations(root: str) -> list[Path]:
    """Digest attestations, and only from the artifacts that carry them.

    The publish job downloads the payload artifacts into the same tree. A
    `digests.json` anywhere else is a builder claiming provenance for work it
    did not do, so it stops the release rather than being skipped."""
    base = Path(root)
    smuggled = [
        path
        for path in sorted(base.rglob("digests.json"))
        if not path.relative_to(base).parts[0].startswith("release-digests-")
    ]
    if smuggled:
        raise SystemExit(
            f"release assets: {smuggled[0]} is a digest attestation inside a payload artifact"
        )
    return sorted(base.glob("release-digests-*/**/digests.json"))


build_images: dict[str, str] = {}
components: dict[str, object] = {}
for path in attestations(digests_dir):
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
        raise SystemExit(f"release assets: {name} is not present in the staged release assets")
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
    stage)
        shift
        [ "$#" -eq 3 ] || fail "usage: $0 stage <expected-list> <artifacts-dir> <assets-dir>"
        stage_assets "$@"
        ;;
    verify-tag)
        shift
        [ "$#" -eq 3 ] || [ "$#" -eq 4 ] ||
            fail "usage: $0 verify-tag <tag> <version> <commit> [repo]"
        verify_tag "$@"
        ;;
    verify-creatable)
        shift
        [ "$#" -eq 2 ] || fail "usage: $0 verify-creatable <tag> <releases-json>"
        release_query verify-creatable "$2" "$1"
        ;;
    verify-draft)
        shift
        [ "$#" -eq 3 ] || fail "usage: $0 verify-draft <tag> <prerelease> <releases-json>"
        release_query verify-draft "$3" "$1" "$2"
        ;;
    names | release-id)
        mode="$1"
        shift
        [ "$#" -eq 2 ] || fail "usage: $0 $mode <releases-json> <tag>"
        release_query "$mode" "$1" "$2"
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
        fail "usage: $0 {expected|verify|stage|verify-tag|verify-creatable|verify-draft|names|release-id|verify-digests|manifest}"
        ;;
esac
