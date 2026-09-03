#!/usr/bin/env bash
# One builder's statement of what it produced: the SHA-256 of every artifact it
# put on the release, the image it built them in, and the reviewed components it
# consumed. The publish job holds the published bytes to these digests, so a
# release asset that changed between its builder and publication is refused.
set -euo pipefail

fail() {
    echo "attest digests: $*" >&2
    exit 1
}

job="${1:-}"
out_dir="${2:-}"
[ -n "$job" ] && [ -n "$out_dir" ] ||
    fail "usage: $0 <job> <out-dir> [--suite S] [--image I] [--component NAME=JSON] [--component-archive NAME=FILE] [asset...]"
shift 2

suite=""
image=""
declare -a assets=()
declare -a components=()
declare -a component_archives=()

while [ "$#" -gt 0 ]; do
    case "$1" in
        --suite) suite="${2:?--suite needs a value}"; shift 2 ;;
        --image) image="${2:?--image needs a value}"; shift 2 ;;
        --component) components+=("${2:?--component needs NAME=JSON}"); shift 2 ;;
        --component-archive)
            component_archives+=("${2:?--component-archive needs NAME=FILE}")
            shift 2
            ;;
        --*) fail "unknown option: $1" ;;
        *) assets+=("$1"); shift ;;
    esac
done

mkdir -p "$out_dir"
JOB="$job" SUITE="$suite" IMAGE="$image" python3 - "$out_dir/digests.json" \
    "${#components[@]}" "${components[@]+"${components[@]}"}" \
    "${#component_archives[@]}" "${component_archives[@]+"${component_archives[@]}"}" \
    "${assets[@]+"${assets[@]}"}" <<'PY'
import hashlib
import json
import os
import sys
from pathlib import Path


def digest(path: Path) -> str:
    if not path.is_file():
        raise SystemExit(f"attest digests: {path} is not a file")
    return hashlib.sha256(path.read_bytes()).hexdigest()


argv = sys.argv[1:]
output = Path(argv[0])
cursor = 1

component_count = int(argv[cursor])
cursor += 1
component_specs = argv[cursor : cursor + component_count]
cursor += component_count

archive_count = int(argv[cursor])
cursor += 1
archive_specs = argv[cursor : cursor + archive_count]
cursor += archive_count

assets = {}
for raw in argv[cursor:]:
    path = Path(raw)
    if path.name in assets:
        raise SystemExit(f"attest digests: two artifacts share the name {path.name}")
    assets[path.name] = digest(path)

components: dict[str, dict] = {}
for spec in component_specs:
    name, _, source = spec.partition("=")
    components.setdefault(name, {}).update(json.loads(Path(source).read_text(encoding="utf-8")))
for spec in archive_specs:
    name, _, source = spec.partition("=")
    archive = Path(source)
    components.setdefault(name, {}).update(
        {"archive": archive.name, "archive_sha256": digest(archive)}
    )

document = {"job": os.environ["JOB"], "assets": dict(sorted(assets.items()))}
if os.environ.get("SUITE"):
    document["suite"] = os.environ["SUITE"]
if os.environ.get("IMAGE"):
    document["image"] = os.environ["IMAGE"]
if components:
    document["components"] = dict(sorted(components.items()))

output.write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
print(f"attest digests: {output} covers {len(assets)} artifact(s)")
PY
