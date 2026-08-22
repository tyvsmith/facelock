#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
destination="${1:?usage: prepare-deb-test-context.sh <empty-destination>}"

if [ ! -d "$destination" ] || [ -n "$(find "$destination" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
    echo "destination must be an existing empty directory: $destination" >&2
    exit 1
fi

# Copy exactly the candidate's tracked and intentionally untracked files. This
# includes the uncommitted Track E tree while excluding ignored build outputs.
git -C "$repo_root" ls-files --cached --others --exclude-standard -z |
    while IFS= read -r -d '' path; do
        [ -e "$repo_root/$path" ] && printf '%s\0' "$path"
    done |
    tar -C "$repo_root" --null -T - -cf - |
    tar -C "$destination" -xf -

git -C "$destination" init --quiet
# The destination contains only the restricted candidate file set copied
# above, so force-add preserves tracked paths that are ignored in a fresh
# repository (for example, checked-in internal documentation).
git -C "$destination" add --all --force
git -C "$destination" \
    -c user.name='Ty Smith' \
    -c user.email='ty@tysmith.me' \
    -c commit.gpgsign=false \
    commit --quiet -m 'test: exact Debian package candidate'

# shellcheck source=/dev/null
source "$destination/scripts/release-versions.sh"
version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$destination/Cargo.toml" | head -1)"
tag="$(release_tag_from_cargo "$version")"
git -C "$destination" tag "$tag"

"$repo_root/test/verify-deb-test-context.sh" "$repo_root" "$destination" >/dev/null

printf '%s\n' "$tag"
