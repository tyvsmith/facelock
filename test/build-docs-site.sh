#!/usr/bin/env bash
# Assemble only the static site. APT publication remains owned by Pages.
set -euo pipefail
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
output=${1:?usage: bash test/build-docs-site.sh OUTPUT_DIRECTORY}
mdbook_bin=${MDBOOK_BIN:-mdbook}
command -v "$mdbook_bin" >/dev/null || { echo 'mdbook 0.4.44 is required (set MDBOOK_BIN to its path)' >&2; exit 1; }
"$mdbook_bin" --version | grep -qx 'mdbook v0.4.44' || { echo 'expected mdbook v0.4.44, matching Pages' >&2; exit 1; }
# Refuse an existing destination: stale output must not hide a broken link.
if [[ -e "$output" ]]; then
    echo "site output must not exist: $output" >&2
    exit 1
fi
mkdir -p -- "$output"
output=$(cd -- "$output" && pwd)
"$mdbook_bin" build "$repo/book" --dest-dir "$output/docs"
cp -a -- "$repo/website/." "$output/"
python3 "$repo/test/check-docs-site.py" "$output"
