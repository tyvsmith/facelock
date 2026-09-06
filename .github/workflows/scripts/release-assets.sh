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
# The checked-in authority for what a release carries and what it is built in.
RELEASE_MATRIX="${FACELOCK_RELEASE_MATRIX:-$REPO_ROOT/dist/release-matrix.json}"
ATTESTATIONS_MODULE="${FACELOCK_RELEASE_ATTESTATIONS:-$SCRIPT_DIR/release_attestations.py}"
export ATTESTATIONS_MODULE
# Loading that module by path would otherwise leave __pycache__ in the checkout,
# and the recipes that demand a clean tree would fail on it.
export PYTHONDONTWRITEBYTECODE=1
# The heredocs below run from the publish job's checkout; a module planted
# there must not shadow the standard library.
export PYTHONSAFEPATH=1

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
# The name to expect at one stage. `builders` compares the files the builders
# staged, which carry the artifact's own name. `final` compares what the API
# lists, and GitHub rewrites `~` when it accepts an upload, so the two stages
# do not expect the same string for a Debian package.
stage_asset_name() {
    local stage="${1:?}" name="${2:?}"
    if [ "$stage" = final ]; then
        release_github_asset_name "$name" || return 1
    else
        printf '%s\n' "$name"
    fi
}

expected_assets() {
    local version="${1:?}" debian_revision="${2:?}" rpm_counter="${3:?}"
    local prerelease="${4:?}" stage="${5:?}"
    local entry suite architecture debian_version rpm_evr rpm_kind rpm_prefix

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

    read_debian_suites
    for entry in "${DEBIAN_SUITES[@]}"; do
        IFS='	' read -r suite architecture _ <<<"$entry"
        debian_version="$(release_debian_version "$version" "$debian_revision" "$suite")" ||
            fail "cannot derive the $suite Debian version"
        printf 'deb-%s\t%s\n' "$suite" \
            "$(literal "$(stage_asset_name "$stage" "facelock_${debian_version}_${architecture}.deb")")"
    done

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

# The artifacts that attest this release, one tab-separated line per slot:
# `<slot> <job> <output> <suite> <image> <components>`, `-` where a slot has
# none. One slot per `release-digests-*` artifact the workflow uploads: the
# publish job requires exactly these, so a builder cannot add an artifact of
# its own and have its claims believed. The output is the job output under
# which the job recorded its document's SHA-256; the artifact store is writable
# by every job in the run, so an attestation is trusted only once it hashes to
# that value. build-deb is a matrix job with one output per suite. The suite,
# image and component names are what the slot's document must declare, no
# more and no less: an attestation is a self-report, and the image and
# component digests it carries reach MANIFEST.json, so the pinned matrix image
# and the reviewed component set are held here rather than taken from the
# document. test/release-artifacts-contract.sh holds this list against the
# workflow's attest-digests.sh call sites and outputs.
expected_attestations() {
    local prerelease="${1:?}"
    local entry suite architecture image rpm

    case "$prerelease" in
        true | false) ;;
        *) fail "prerelease must be true or false, got: $prerelease" ;;
    esac

    printf 'build\tbuild\tattestation\t-\t-\t-\n'
    printf 'onnxruntime\tdownload-ort\tattestation\t-\t-\tonnxruntime\n'
    printf 'cargo-vendor\tprepare-cargo-vendor\tattestation\t-\t-\tcargo-vendor\n'
    read_debian_suites
    for entry in "${DEBIAN_SUITES[@]}"; do
        IFS='	' read -r suite architecture image <<<"$entry"
        printf 'deb-%s\tbuild-deb\tattestation-%s\t%s\t%s\t-\n' "$suite" "$suite" "$suite" "$image"
    done
    rpm="$(rpm_image)" || fail "cannot read the direct RPM image from $RELEASE_MATRIX"
    [ -n "$rpm" ] || fail "the release matrix pins no direct RPM image: $RELEASE_MATRIX"
    printf 'rpm\tbuild-rpm\tattestation\t-\t%s\t-\n' "$rpm"
    if [ "$prerelease" = false ]; then
        printf 'apt\tpublish-apt\tattestation\t-\t-\t-\n'
    fi
}

# The published Debian suites, their architectures and their pinned images,
# from the release matrix, as `<suite><TAB><architecture><TAB><image>`.
debian_suites() {
    python3 - "$RELEASE_MATRIX" <<'PY'
import json
import sys

matrix = json.load(open(sys.argv[1], encoding="utf-8"))
for suite, details in sorted(matrix["apt_suites"].items()):
    if suite == "compat":
        continue
    print(f"{suite}\t{details['architecture']}\t{details['image']}")
PY
}

# The pinned image the direct RPM is built in. test/check-release-matrix.py
# holds the workflow's container image to this same row.
rpm_image() {
    python3 - "$RELEASE_MATRIX" <<'PY'
import json
import sys

matrix = json.load(open(sys.argv[1], encoding="utf-8"))
print(next(row["image"] for row in matrix["platforms"] if row["id"] == "fedora-44-direct"))
PY
}

# The matrix is the authority for what a release carries, so a matrix that
# cannot be read must stop the allowlist rather than shorten it. A failure
# inside a process substitution is invisible to `set -e`, and `mapfile` does
# not report it either; read through a command substitution and floor the
# result. Fills DEBIAN_SUITES.
read_debian_suites() {
    local listing
    listing="$(debian_suites)" ||
        fail "cannot read the Debian suites from $RELEASE_MATRIX"
    [ -n "$listing" ] || fail "the release matrix names no Debian suite: $RELEASE_MATRIX"
    mapfile -t DEBIAN_SUITES <<<"$listing"
}

# Every asset the release carries matches exactly one canonical name, and every
# canonical name is carried exactly once.
verify_assets() {
    local expected_file="${1:?}" actual_file="${2:?}"
    local -a labels=() patterns=() actual=()
    local label pattern name matches duplicate index delete_operand

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
        if [ "$matches" -eq 0 ]; then
            # The name reaches a command a human is invited to paste, and it
            # arrives from an API listing rather than from this script.
            printf -v delete_operand '%q' "$name"
            fail "unexpected release asset: $name; if an earlier run at another version left it behind, remove it with: gh release delete-asset \"\$TAG\" $delete_operand. If it differs from a canonical name only where a \`~\` became a \`.\`, GitHub stored the upload under a rewritten name and release_github_asset_name has to account for it -- do not delete it."
        fi
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

# A builder's artifact tree is attacker-controlled: a symlink lets it point a
# canonical name at bytes it does not own (another artifact, or the runner's
# filesystem). Path.is_file() follows symlinks, so the walk must reject any
# symlink before that check ever runs, not merely skip stray files.
candidates: list[Path] = []
for path in Path(artifacts_dir).rglob("*"):
    if path.is_symlink():
        raise SystemExit(f"release assets: refusing to stage a symlink under artifacts: {path}")
    if path.is_file():
        candidates.append(path)
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
            f"release assets: more than one builder artifact provides {label}: {found}; "
            "a builder produced a file it does not own, and re-running only the failed "
            "job keeps that artifact: fix the builder and re-run all jobs"
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
# one. `verify-draft` runs before the flip. `verify-published` holds every
# asset the API lists to MANIFEST.json by size, and by digest where the API
# exposes one, so one run's binaries cannot sit under another run's digests.
# `names` and `release-id` read the selected release. Accepts a JSON array, one
# release object, gh's paginated object stream, and gh's slurped pages.
release_query() {
    local mode="${1:?}" releases_json="${2:?}" tag="${3:?}" extra="${4:-}"
    python3 - "$mode" "$releases_json" "$tag" "$extra" <<'PY'
import hashlib
import json
import re
import sys
from pathlib import Path

# `extra` is the validated prerelease flag for verify-draft and the local
# MANIFEST.json path for verify-published.
mode, path, tag, extra = sys.argv[1:5]
prerelease = extra


def concurrent_drafts(candidates: list) -> str:
    """Two releases for one tag is what two concurrent runs leave behind; the
    concurrency group prevents it, and the refusal names the remedy anyway."""
    commands = " or ".join(
        f"gh api --method DELETE repos/{{owner}}/{{repo}}/releases/{release.get('id')}"
        for release in candidates
    )
    return (
        f"release assets: {len(candidates)} releases exist for {tag}; a concurrent run "
        f"left a second draft behind. Keep the one this run should finish and delete "
        f"the other with: {commands}"
    )


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
        raise SystemExit(concurrent_drafts(candidates))
    release = candidates[0]
    if not release.get("draft"):
        raise SystemExit(
            f"release assets: release {release.get('id')} for {tag} is already published; "
            "refusing to publish twice"
        )
    print(f"release assets: reusing the unpublished draft {release.get('id')} for {tag}")
    raise SystemExit(0)

if len(candidates) > 1:
    raise SystemExit(concurrent_drafts(candidates))
if not candidates:
    raise SystemExit(f"release assets: expected exactly one release for tag {tag}, found 0")
release = candidates[0]

if mode == "release-id":
    print(release["id"])
elif mode == "names":
    for asset in release.get("assets", []):
        print(asset["name"])
elif mode == "verify-published":
    manifest_path = Path(extra)
    manifest_bytes = manifest_path.read_bytes()
    manifest = json.loads(manifest_bytes)
    # The manifest records each artifact's own name; the API lists the name
    # GitHub stored. This is the same rule as `release_github_asset_name` in
    # scripts/release-versions.sh, including its refusal, and
    # test/release-artifacts-contract.sh holds the two to the same answers.
    # Refusing here matters as much as it does there: a name this rule cannot
    # account for would otherwise mismatch at publish time, which is the late
    # failure this whole conversion exists to prevent.
    def stored(name: str) -> str:
        mapped = name.replace("~", ".")
        if not re.fullmatch(r"[A-Za-z0-9._-]+", mapped):
            raise SystemExit(
                f"release assets: {manifest_path.name} names an asset whose stored name "
                f"cannot be derived: {name}"
            )
        return mapped

    # Built by hand rather than as a comprehension: two entries that differ
    # only by `~` collapse to one stored name, and a comprehension would drop
    # one silently and compare the survivor against both.
    expected: dict[str, tuple] = {}
    for entry in manifest["assets"]:
        key = stored(entry["name"])
        if key in expected:
            raise SystemExit(
                f"release assets: two {manifest_path.name} entries share the stored name {key}"
            )
        expected[key] = (entry["size"], entry["sha256"])
    # The manifest cannot cover itself; hold the uploaded copy to this file.
    expected[manifest_path.name] = (len(manifest_bytes), hashlib.sha256(manifest_bytes).hexdigest())
    listed = release.get("assets", [])
    compared = 0
    for asset in listed:
        name = asset.get("name")
        if name not in expected:
            raise SystemExit(f"release assets: published asset {name} is not covered by {manifest_path.name}")
        size, sha256 = expected[name]
        if asset.get("size") != size:
            raise SystemExit(
                f"release assets: published asset {name} has size {asset.get('size')}, "
                f"{manifest_path.name} records {size}"
            )
        digest = asset.get("digest")
        if digest is None:
            continue
        # A digest the API exposes in a form this check cannot compare is not
        # evidence; refuse rather than skip it.
        if digest != f"sha256:{sha256}":
            raise SystemExit(
                f"release assets: published asset {name} has digest {digest}, "
                f"{manifest_path.name} records sha256:{sha256}"
            )
        compared += 1
    print(
        f"release assets: {len(listed)} published asset(s) match {manifest_path.name} "
        f"by size, {compared} by digest"
    )
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
# builder attested. `job_outputs` is the publish job's `toJSON(needs)`: each
# attestation is trusted only once it hashes to the output its job recorded.
verify_digests() {
    local digests_dir="${1:?}" job_outputs="${2:?}" assets_dir="${3:?}"
    local actual_file="${4:?}" prerelease="${5:?}"
    local expected
    expected="$(expected_attestations "$prerelease")"
    python3 - "$digests_dir" "$job_outputs" "$assets_dir" "$actual_file" "$expected" <<'PY'
import hashlib
import importlib.util
import os
import sys
from pathlib import Path

digests_dir, job_outputs, assets_dir, actual_file, expected = sys.argv[1:6]

spec = importlib.util.spec_from_file_location(
    "release_attestations", os.environ["ATTESTATIONS_MODULE"]
)
release_attestations = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release_attestations)

try:
    documents = release_attestations.load(digests_dir, expected, job_outputs)
except release_attestations.AttestationError as error:
    raise SystemExit(f"release assets: {error}")

attested: dict[str, list[tuple[str, str]]] = {}
for document in documents:
    for name, digest in document.get("assets", {}).items():
        attested.setdefault(name, []).append((document["job"], digest))

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
    local job_outputs="${8:?}" assets_dir="${9:?}" actual_file="${10:?}" output="${11:?}"
    local expected
    expected="$(expected_attestations "$prerelease")"
    python3 - "$tag" "$version" "$commit" "$prerelease" "$repository" "$source_sha256" \
        "$digests_dir" "$job_outputs" "$assets_dir" "$actual_file" "$output" "$expected" <<'PY'
import hashlib
import importlib.util
import json
import os
import sys
from pathlib import Path

(tag, version, commit, prerelease, repository, source_sha256,
 digests_dir, job_outputs, assets_dir, actual_file, output, expected) = sys.argv[1:13]

spec = importlib.util.spec_from_file_location(
    "release_attestations", os.environ["ATTESTATIONS_MODULE"]
)
release_attestations = importlib.util.module_from_spec(spec)
spec.loader.exec_module(release_attestations)

try:
    documents = release_attestations.load(digests_dir, expected, job_outputs)
    build_images, components = release_attestations.provenance(documents)
except release_attestations.AttestationError as error:
    raise SystemExit(f"release assets: {error}")

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
    verify-published)
        shift
        [ "$#" -eq 3 ] || fail "usage: $0 verify-published <releases-json> <tag> <manifest>"
        release_query verify-published "$1" "$2" "$3"
        ;;
    expected-attestations)
        shift
        [ "$#" -eq 1 ] || fail "usage: $0 expected-attestations <prerelease>"
        expected_attestations "$@"
        ;;
    verify-digests)
        shift
        [ "$#" -eq 5 ] ||
            fail "usage: $0 verify-digests <digests-dir> <job-outputs> <assets-dir> <actual-list> <prerelease>"
        verify_digests "$@"
        ;;
    manifest)
        shift
        [ "$#" -eq 11 ] ||
            fail "usage: $0 manifest <tag> <version> <commit> <prerelease> <repository> <source-sha256> <digests-dir> <job-outputs> <assets-dir> <actual-list> <output>"
        generate_manifest "$@"
        ;;
    *)
        fail "usage: $0 {expected|expected-attestations|verify|stage|verify-tag|verify-creatable|verify-draft|verify-published|names|release-id|verify-digests|manifest}"
        ;;
esac
