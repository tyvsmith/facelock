#!/usr/bin/env bash
set -euo pipefail

source_repo="${1:?usage: verify-deb-test-context.sh <source-repo> <candidate-repo>}"
candidate_repo="${2:?usage: verify-deb-test-context.sh <source-repo> <candidate-repo>}"

fail() {
    echo "exact tagged context: $*" >&2
    exit 1
}

[ -d "$source_repo/.git" ] || git -C "$source_repo" rev-parse --git-dir >/dev/null 2>&1 ||
    fail "source is not a Git worktree: $source_repo"
git -C "$candidate_repo" rev-parse --verify 'HEAD^{commit}' >/dev/null 2>&1 ||
    fail "candidate has no source commit"

verification_root="$(mktemp -d "${TMPDIR:-/tmp}/facelock-context-verification.XXXXXX")"
trap 'rm -rf -- "$verification_root"' EXIT
candidate_paths="$verification_root/candidate-paths"

# Match the preparation contract: every existing tracked path (even when it
# is ignored now), plus every intentional untracked, non-ignored path.
git -C "$source_repo" ls-files --cached --others --exclude-standard -z |
    while IFS= read -r -d '' path; do
        if [ -e "$source_repo/$path" ] || [ -L "$source_repo/$path" ]; then
            printf '%s\0' "$path"
        fi
    done >"$candidate_paths"

expected_index="$verification_root/expected.index"
expected_objects="$verification_root/objects"
source_objects="$(git -C "$source_repo" rev-parse --path-format=absolute --git-path objects)"
mkdir "$expected_objects"
GIT_INDEX_FILE="$expected_index" \
    GIT_OBJECT_DIRECTORY="$expected_objects" \
    GIT_ALTERNATE_OBJECT_DIRECTORIES="$source_objects" \
    git -C "$source_repo" read-tree --empty
GIT_INDEX_FILE="$expected_index" \
    GIT_OBJECT_DIRECTORY="$expected_objects" \
    GIT_ALTERNATE_OBJECT_DIRECTORIES="$source_objects" \
    git -C "$source_repo" add --force \
    --pathspec-from-file="$candidate_paths" --pathspec-file-nul
expected_tree="$(
    GIT_INDEX_FILE="$expected_index" \
        GIT_OBJECT_DIRECTORY="$expected_objects" \
        GIT_ALTERNATE_OBJECT_DIRECTORIES="$source_objects" \
        git -C "$source_repo" write-tree
)"
candidate_tree="$(git -C "$candidate_repo" rev-parse 'HEAD^{tree}')"
[ "$candidate_tree" = "$expected_tree" ] ||
    fail "candidate tree $candidate_tree does not match exact source tree $expected_tree"

[ -z "$(git -C "$candidate_repo" status --porcelain=v1 --untracked-files=all)" ] ||
    fail "candidate repository is not clean after committing the exact source tree"

archive_root="$verification_root/archive"
mkdir "$archive_root"
git -C "$candidate_repo" archive --format=tar HEAD | tar -xf - -C "$archive_root"
git -C "$archive_root" init --quiet
git -C "$archive_root" add --all --force
archive_tree="$(git -C "$archive_root" write-tree)"
[ "$archive_tree" = "$candidate_tree" ] ||
    fail "source archive tree $archive_tree does not match candidate commit tree $candidate_tree"

echo "exact tagged context: ok ($candidate_tree)"
