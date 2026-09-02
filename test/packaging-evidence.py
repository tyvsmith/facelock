#!/usr/bin/env python3
"""Packaging matrix evidence: record a lane, aggregate the marker, validate it.

`just release-preflight` used to accept `.packaging-matrix-verified` when its
one line equalled HEAD. A `FACELOCK_ALLOW_MISSING_MODELS=1` run wrote that same
line after skipping every daemon-start assertion, so the marker could not tell
a diagnostic partial run from the release gate (#313). The marker is now a JSON
document that names the commit, every lane the release matrix requires, and
each lane's assertion counts by class, and it is refused unless every required
lane is present with zero skips of either class and its models on hand.

Three producers, one consumer:

  record     a lane runner turns the validator's RESULTS_JSON line into
             .packaging-evidence/<lane>.json, tagged with what the lane claims
  aggregate  `just test-packaging-matrix` folds the records into the marker,
             refusing to write one that would not validate
  ci-run     `just release-preflight` downloads the evidence artifacts a
             packaging.yml run uploaded and aggregates them the same way
  validate   `just release-preflight` checks a marker against HEAD

Standard library only: preflight runs wherever `python3` does.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MATRIX_PATH = ROOT / "dist" / "release-matrix.json"
EVIDENCE_DIR = ROOT / ".packaging-evidence"
MARKER_PATH = ROOT / ".packaging-matrix-verified"

SCHEMA = 1
LANE_FIELDS = ("target", "channel", "build_origin", "runtime_policy", "depth")
COUNTERS = ("pass", "fail", "skip", "allowed_skip", "mandatory_skip")
LANE_NAME = re.compile(r"[a-z][a-z0-9-]*")
COMMIT_SHA = re.compile(r"[0-9a-f]{40}")
RESULTS_PREFIX = "RESULTS_JSON: "
# The matrix spells lifecycle depth out; records carry one token per lane.
DEPTH_BY_LIFECYCLE = {"full": "full", "build/runtime smoke": "smoke"}


class EvidenceError(Exception):
    """A refusal, with the reasons the caller should print."""

    def __init__(self, *problems: str) -> None:
        super().__init__("; ".join(problems))
        self.problems = list(problems)


def now_utc() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def head_commit() -> str:
    try:
        completed = subprocess.run(
            ["git", "-C", str(ROOT), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.CalledProcessError) as error:
        detail = (getattr(error, "stderr", "") or str(error)).strip()
        raise EvidenceError(f"cannot resolve HEAD in {ROOT}: {detail}") from error
    return completed.stdout.strip()


def parse_timestamp(value: str) -> datetime | None:
    """An ISO 8601 timestamp that carries its UTC offset; None for anything else."""
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return parsed if parsed.tzinfo is not None else None


def load_matrix() -> dict:
    try:
        return json.loads(MATRIX_PATH.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise EvidenceError(f"cannot read the release matrix {MATRIX_PATH}: {error}") from error


# --------------------------------------------------------------- required lanes


def eligible(row: dict) -> bool:
    """A platform row that can supply release evidence at all.

    Rawhide is the row this excludes: optional, not a release target, and its
    evidence_eligibility says lifecycle evidence is never accepted from it.
    """
    if row.get("release_target") is not True or row.get("optional") is True:
        return False
    return row.get("evidence_eligibility", {}).get("lifecycle", True) is not False


def runtime_policy(runtime: str) -> str:
    if runtime.startswith("bundled ORT"):
        return "bundled-ort"
    if runtime == "system ORT":
        return "system-ort"
    raise EvidenceError(f"release matrix runtime {runtime!r} maps to no runtime policy")


def lane_depth(row: dict) -> str:
    depth = row.get("lifecycle_depth")
    if depth not in DEPTH_BY_LIFECYCLE:
        raise EvidenceError(f"release matrix lifecycle depth {depth!r} maps to no lane depth")
    return DEPTH_BY_LIFECYCLE[depth]


def fedora_rows(rows: list[dict], release: str) -> list[dict]:
    """Every eligible platform row for one Fedora release.

    A release has more than one: Fedora 44 carries a system-ORT row for the
    COPR path and a bundled-ORT row for the direct .rpm, and both describe the
    same release at the same depth.
    """
    return [row for row in rows if re.fullmatch(rf"Fedora {release}(?: .*)?", row.get("platform", ""))]


def fedora_depth(rows: list[dict], release: str) -> str:
    depths = {lane_depth(row) for row in fedora_rows(rows, release)}
    if len(depths) != 1:
        raise EvidenceError(
            f"Fedora {release} platform rows declare {sorted(depths) or 'no'} lifecycle depth"
        )
    return depths.pop()


def required_lanes(matrix: dict) -> dict[str, dict[str, str]]:
    """The lanes the release gate requires, and what each must claim.

    Derived from the release-target platform rows so a new target cannot be
    declared without a lane to prove it. Each Packit release target needs two
    Fedora lanes, because the matrix declares two delivery paths for it and one
    cannot stand in for the other: the direct .rpm built from host binaries
    around a bundled ONNX Runtime (`just test-rpm-lanes`), and the package
    COPR itself would publish -- rebuilt from source in mock, resolving
    Fedora's system ONNX Runtime (`just test-copr-lanes`, #230). The lane
    attributes are what keep them apart: a direct-RPM record offered for a COPR
    target is refused on `channel` before anything else is read.
    """
    lanes: dict[str, dict[str, str]] = {}
    rows = [row for row in matrix.get("platforms", []) if eligible(row)]
    suite_by_platform: dict[str, str] = {}
    for name, suite in matrix.get("apt_suites", {}).items():
        if name == "compat":
            continue
        platform_id = suite.get("platform_id")
        if platform_id is None:
            raise EvidenceError(f"APT suite {name} has no platform_id in the release matrix")
        suite_by_platform[platform_id] = name
    for row in rows:
        platform_id = row.get("id", "")
        if row.get("channel") == "staged APT/direct deb":
            suite = suite_by_platform.get(platform_id)
            if suite is None:
                raise EvidenceError(f"platform {platform_id} has no APT suite in the release matrix")
            family = platform_id.split("-")[0]
            lanes[f"test-deb-{suite}-pkg"] = {
                "target": f"{family}-{suite}",
                "channel": "apt",
                "build_origin": "container-source-build",
                "runtime_policy": runtime_policy(row.get("runtime", "")),
                "depth": lane_depth(row),
            }
        elif row.get("variant") == "PKGBUILD and binary recipe":
            lanes["test-arch-pkg"] = {
                "target": "arch",
                "channel": "aur",
                "build_origin": "makepkg-source-build",
                "runtime_policy": runtime_policy(row.get("runtime", "")),
                "depth": lane_depth(row),
            }
        elif not row.get("platform", "").startswith("Fedora "):
            raise EvidenceError(f"platform {platform_id} is a release target with no lane to prove it")
    for target in matrix.get("fedora", {}).get("packit_release_targets", []):
        release = target.split("-")[1]
        depth = fedora_depth(rows, release)
        direct = "test-rpm-pkg" if depth == "full" else "test-rpm-smoke"
        lanes[f"{direct}-{release}"] = {
            "target": f"fedora-{release}",
            "channel": "direct-rpm",
            "build_origin": "host-binaries",
            "runtime_policy": "bundled-ort",
            "depth": depth,
        }
        # The COPR lane's runtime policy is read off the matrix rather than
        # asserted here: if no row for this release declares system ORT, the
        # release matrix no longer describes a COPR delivery path and the lane
        # would be proving something nothing asked for.
        if "system-ort" not in {
            runtime_policy(row.get("runtime", "")) for row in fedora_rows(rows, release)
        }:
            raise EvidenceError(
                f"Fedora {release} is a Packit release target with no system-ORT platform row"
            )
        copr = "test-copr-pkg" if depth == "full" else "test-copr-smoke"
        lanes[f"{copr}-{release}"] = {
            "target": f"fedora-{release}",
            "channel": "copr",
            "build_origin": "mock-source-rebuild",
            "runtime_policy": "system-ort",
            "depth": depth,
        }
    return lanes


# --------------------------------------------------------------------- records


def parse_lane_spec(spec: str) -> tuple[str, dict[str, str]]:
    """`<name> target=… channel=… build_origin=… runtime_policy=… depth=…`."""
    parts = spec.split()
    if not parts or not LANE_NAME.fullmatch(parts[0]):
        raise EvidenceError(f"lane spec must start with a lane name: {spec!r}")
    attrs: dict[str, str] = {}
    for part in parts[1:]:
        key, sep, value = part.partition("=")
        if not sep or key not in LANE_FIELDS or not value:
            raise EvidenceError(f"lane spec field {part!r} is not one of {', '.join(LANE_FIELDS)}")
        attrs[key] = value
    missing = [field for field in LANE_FIELDS if field not in attrs]
    if missing:
        raise EvidenceError(f"lane spec {parts[0]} omits {', '.join(missing)}")
    return parts[0], attrs


def parse_results(log: Path) -> dict:
    """The last RESULTS_JSON line a lane validator printed."""
    try:
        lines = log.read_text(encoding="utf-8", errors="replace").splitlines()
    except OSError as error:
        raise EvidenceError(f"cannot read the lane log {log}: {error}") from error
    results_lines = [line[len(RESULTS_PREFIX):] for line in lines if line.startswith(RESULTS_PREFIX)]
    if not results_lines:
        raise EvidenceError(f"the lane printed no {RESULTS_PREFIX.strip()} line; nothing to record")
    try:
        results = json.loads(results_lines[-1])
    except json.JSONDecodeError as error:
        raise EvidenceError(f"the lane's RESULTS_JSON line is not JSON: {error}") from error
    if not isinstance(results, dict):
        raise EvidenceError("the lane's RESULTS_JSON line is not an object")
    for counter in COUNTERS:
        value = results.get(counter)
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            raise EvidenceError(f"RESULTS_JSON {counter} is not a non-negative integer: {value!r}")
    if not isinstance(results.get("models_present"), bool):
        raise EvidenceError("RESULTS_JSON models_present is not a boolean")
    return results


def lane_status(record: dict, exit_status: int) -> str:
    if exit_status != 0 or record["fail"] > 0 or record["mandatory_skip"] > 0:
        return "fail"
    if record["skip"] > 0 or not record["models_present"]:
        return "partial"
    return "pass"


def record(args: argparse.Namespace) -> int:
    name, attrs = parse_lane_spec(args.lane)
    results = parse_results(Path(args.results_log))
    lane = {"schema": SCHEMA, "name": name, **attrs}
    lane["commit"] = head_commit()
    lane["recorded_at"] = now_utc()
    lane["models_present"] = results["models_present"]
    for counter in COUNTERS:
        lane[counter] = results[counter]
    for extra in args.extra_skip:
        lane["skip"] += 1
        lane[f"{extra}_skip"] += 1
    lane["status"] = lane_status(lane, args.exit_status)
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    path = output_dir / f"{name}.json"
    path.write_text(json.dumps(lane, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(
        f"packaging evidence: {name} {lane['status']} "
        f"({lane['pass']} passed, {lane['fail']} failed, {lane['skip']} skipped) -> {path}"
    )
    return 0


# ------------------------------------------------------------------ validation


def check_record(record: dict, head: str, expected: dict[str, str] | None) -> list[str]:
    name = record.get("name", "<unnamed>")
    problems: list[str] = []

    def problem(message: str) -> None:
        problems.append(f"lane {name}: {message}")

    schema = record.get("schema")
    if not isinstance(schema, int) or isinstance(schema, bool) or schema != SCHEMA:
        problem(f"schema is {schema!r}, not {SCHEMA}")
    for field in ("name", "commit", "status", *LANE_FIELDS):
        if not isinstance(record.get(field), str) or not record[field]:
            problem(f"{field} is missing")
    for counter in COUNTERS:
        value = record.get(counter)
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            problem(f"{counter} is not a non-negative integer")
    if not isinstance(record.get("models_present"), bool):
        problem("models_present is not a boolean")
    if problems:
        return problems
    if record["commit"] != head:
        problem(f"commit {record['commit']} is not HEAD {head}")
    if record["models_present"] is not True:
        problem("models_present is false: the model-dependent assertions did not run")
    if record["skip"] != record["allowed_skip"] + record["mandatory_skip"]:
        problem(
            f"skip {record['skip']} is not allowed_skip {record['allowed_skip']} "
            f"plus mandatory_skip {record['mandatory_skip']}"
        )
    for counter in ("fail", "skip", "allowed_skip", "mandatory_skip"):
        if record[counter] != 0:
            problem(f"{counter} is {record[counter]}, not 0")
    if record["pass"] < 1:
        problem("no assertion passed; an empty lane is not evidence")
    if record["status"] != "pass":
        problem(f"status is {record['status']!r}, not 'pass'")
    if expected is not None:
        for field in LANE_FIELDS:
            if record[field] != expected[field]:
                problem(f"{field} is {record[field]!r}; the release matrix requires {expected[field]!r}")
    return problems


def check_marker(marker: object, head: str, matrix: dict) -> list[str]:
    """Every reason this marker is not release evidence for `head`; [] accepts."""
    if not isinstance(marker, dict):
        return ["the marker is not a JSON object"]
    problems: list[str] = []
    schema = marker.get("schema")
    if not isinstance(schema, int) or isinstance(schema, bool) or schema != SCHEMA:
        problems.append(f"schema is {schema!r}, not {SCHEMA}")
    if marker.get("commit") != head:
        problems.append(f"commit {marker.get('commit')!r} is not HEAD {head}")
    if marker.get("tree_clean") is not True:
        problems.append("tree_clean is not true: the lanes did not run on a committed tree")
    stamps: dict[str, datetime] = {}
    for stamp in ("started_at", "finished_at"):
        value = marker.get(stamp)
        if not isinstance(value, str) or not value:
            problems.append(f"{stamp} is missing" + (": the run did not finish" if stamp == "finished_at" else ""))
            continue
        parsed = parse_timestamp(value)
        if parsed is None:
            problems.append(f"{stamp} {value!r} is not an ISO 8601 timestamp with a UTC offset")
        else:
            stamps[stamp] = parsed
    if len(stamps) == 2 and stamps["finished_at"] < stamps["started_at"]:
        problems.append(f"finished_at {marker['finished_at']} is before started_at {marker['started_at']}")
    required = required_lanes(matrix)
    declared = marker.get("required_lanes")
    if not isinstance(declared, list) or not all(isinstance(name, str) for name in declared):
        problems.append("required_lanes is not a list of lane names")
    elif declared != sorted(required):
        problems.append(f"required lane set {declared} differs from the release matrix's {sorted(required)}")
    lanes = marker.get("lanes")
    if not isinstance(lanes, list) or not all(isinstance(lane, dict) for lane in lanes):
        problems.append("lanes is not a list of lane records")
        return problems
    seen: dict[str, int] = {}
    for lane in lanes:
        name = lane.get("name")
        if isinstance(name, str):
            seen[name] = seen.get(name, 0) + 1
            if name not in required:
                problems.append(f"lane {name} is not a lane the release matrix requires")
        problems.extend(check_record(lane, head, required.get(name) if isinstance(name, str) else None))
    for name, count in sorted(seen.items()):
        if count > 1:
            problems.append(f"lane {name} is recorded more than once")
    for name in sorted(required):
        if name not in seen:
            problems.append(f"required lane {name} has no record")
    return problems


def read_marker(path: Path) -> object:
    """The marker's JSON, or the reason it is not one."""
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        raise EvidenceError(f"no packaging evidence at {path}") from None
    except OSError as error:
        raise EvidenceError(f"cannot read {path}: {error}") from error
    try:
        return json.loads(text)
    except json.JSONDecodeError as error:
        if COMMIT_SHA.fullmatch(text.strip()):
            raise EvidenceError(
                f"{path} is a legacy one-line commit marker ({text.strip()[:12]}); release evidence "
                f"is now the schema {SCHEMA} JSON document `just test-packaging-matrix` writes, "
                "so re-run the matrix at this commit"
            ) from None
        raise EvidenceError(f"{path} is not JSON: {error}") from error


def summary(marker: dict, what: str) -> str:
    passed = sum(lane["pass"] for lane in marker["lanes"])
    return (
        f"packaging evidence: {what} carries {len(marker['lanes'])} lanes, "
        f"{passed} assertions passed, no skips, at {marker['commit']}"
    )


def refuse(what: str, problems: list[str]) -> int:
    print(f"packaging evidence: REFUSED {what}", file=sys.stderr)
    for problem in problems:
        print(f"  - {problem}", file=sys.stderr)
    return 1


def validate(args: argparse.Namespace) -> int:
    head = args.commit or head_commit()
    path = Path(args.marker)
    marker = read_marker(path)
    problems = check_marker(marker, head, load_matrix())
    if problems:
        return refuse(str(path), problems)
    print(summary(marker, str(path)))
    return 0


# ----------------------------------------------------------------- aggregation


def load_records(evidence_dir: Path) -> list[dict]:
    records: list[dict] = []
    for path in sorted(evidence_dir.rglob("*.json")):
        try:
            record = json.loads(path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as error:
            raise EvidenceError(f"lane record {path} is not JSON: {error}") from error
        if not isinstance(record, dict):
            raise EvidenceError(f"lane record {path} is not a JSON object")
        records.append(record)
    return records


def build_marker(
    head: str,
    evidence_dir: Path,
    started_at: str,
    finished_at: str,
    tree_clean: bool,
    matrix: dict,
) -> tuple[dict, list[str]]:
    marker = {
        "schema": SCHEMA,
        "commit": head,
        "tree_clean": tree_clean,
        "started_at": started_at,
        "finished_at": finished_at,
        "required_lanes": sorted(required_lanes(matrix)),
        "lanes": load_records(evidence_dir),
    }
    return marker, check_marker(marker, head, matrix)


def aggregate(args: argparse.Namespace) -> int:
    head = args.commit or head_commit()
    marker, problems = build_marker(
        head,
        Path(args.evidence_dir),
        args.started_at,
        args.finished_at or now_utc(),
        args.tree_clean,
        load_matrix(),
    )
    if problems:
        return refuse(f"the lane records in {args.evidence_dir}", problems)
    output = Path(args.output)
    output.write_text(json.dumps(marker, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(summary(marker, str(output)))
    return 0


def gh(*args: str) -> str:
    completed = subprocess.run(["gh", *args], capture_output=True, text=True)
    if completed.returncode != 0:
        raise EvidenceError(f"gh {' '.join(args[:2])} failed: {completed.stderr.strip() or completed.stdout.strip()}")
    return completed.stdout


NO_ARTIFACTS = ("no artifacts match", "no valid artifacts")


def packaging_workflow_name() -> str:
    """The `name:` of .github/workflows/packaging.yml, which gh reports as workflowName."""
    path = ROOT / ".github" / "workflows" / "packaging.yml"
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise EvidenceError(f"cannot read {path}: {error}") from error
    match = re.search(r"(?m)^name:\s*(\S.*?)\s*$", text)
    if match is None:
        raise EvidenceError(f"{path} declares no workflow name")
    return match.group(1)


def ci_run(args: argparse.Namespace) -> int:
    """A packaging.yml run is evidence only through the artifacts its lanes uploaded."""
    head = args.commit or head_commit()
    what = f"packaging workflow run {args.run}"
    try:
        view = json.loads(
            gh("run", "view", args.run, "--json", "conclusion,event,headSha,workflowName,createdAt,updatedAt")
        )
    except json.JSONDecodeError as error:
        raise EvidenceError(f"gh run view returned no JSON: {error}") from error
    problems: list[str] = []
    workflow = packaging_workflow_name()
    if view.get("workflowName") != workflow:
        problems.append(f"workflow is {view.get('workflowName')!r}, not {workflow!r}")
    if view.get("event") == "pull_request":
        problems.append(
            "pull-request runs build the merge commit, not this one; use a workflow_dispatch "
            "or scheduled run on this commit"
        )
    if view.get("conclusion") != "success":
        problems.append(f"conclusion is {view.get('conclusion')!r}, not 'success'")
    if view.get("headSha") != head:
        problems.append(f"head {view.get('headSha')!r} is not HEAD {head}")
    if problems:
        return refuse(what, problems)
    with tempfile.TemporaryDirectory(prefix="facelock-packaging-evidence.") as download_dir:
        completed = subprocess.run(
            ["gh", "run", "download", args.run, "--pattern", "packaging-evidence-*", "--dir", download_dir],
            capture_output=True,
            text=True,
        )
        if completed.returncode != 0:
            detail = completed.stderr.strip() or completed.stdout.strip()
            if any(phrase in detail.lower() for phrase in NO_ARTIFACTS):
                return refuse(
                    what,
                    [
                        "the run uploaded no packaging evidence artifact; a path-filtered or "
                        f"pre-evidence run is not evidence: {detail}"
                    ],
                )
            return refuse(what, [f"gh run download failed, so the run's evidence could not be read: {detail}"])
        # A workflow run checks out the commit it names, so there is no dirty
        # tree to record; the lane records still have to name that commit.
        marker, problems = build_marker(
            head,
            Path(download_dir),
            str(view.get("createdAt") or ""),
            str(view.get("updatedAt") or ""),
            True,
            load_matrix(),
        )
    if problems:
        return refuse(what, problems)
    print(summary(marker, what))
    return 0


# ------------------------------------------------------------------------- CLI


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)

    record_parser = commands.add_parser("record", help="write one lane record from a validator's RESULTS_JSON line")
    record_parser.add_argument("--lane", required=True, help="'<name> target=… channel=… build_origin=… runtime_policy=… depth=…'")
    record_parser.add_argument("--results-log", required=True, help="captured stdout of the lane validator")
    record_parser.add_argument("--exit-status", type=int, required=True, help="the lane's overall exit status")
    record_parser.add_argument(
        "--extra-skip",
        action="append",
        choices=("allowed", "mandatory"),
        default=[],
        help="a skip the runner took outside the validator, by class",
    )
    record_parser.add_argument("--output-dir", default=str(EVIDENCE_DIR))
    record_parser.set_defaults(func=record)

    aggregate_parser = commands.add_parser("aggregate", help="fold lane records into the marker, or refuse")
    aggregate_parser.add_argument("--commit", help="the commit the lanes ran at (default: HEAD)")
    aggregate_parser.add_argument("--evidence-dir", default=str(EVIDENCE_DIR))
    aggregate_parser.add_argument("--started-at", required=True)
    aggregate_parser.add_argument("--finished-at", help="default: now")
    aggregate_parser.add_argument("--tree-clean", action="store_true", help="the caller verified a clean tree")
    aggregate_parser.add_argument("--output", default=str(MARKER_PATH))
    aggregate_parser.set_defaults(func=aggregate)

    validate_parser = commands.add_parser("validate", help="accept or refuse a marker for a commit")
    validate_parser.add_argument("--commit", help="the commit being released (default: HEAD)")
    validate_parser.add_argument("marker", nargs="?", default=str(MARKER_PATH))
    validate_parser.set_defaults(func=validate)

    ci_parser = commands.add_parser("ci-run", help="accept or refuse a packaging.yml run through its evidence artifacts")
    ci_parser.add_argument("--commit", help="the commit being released (default: HEAD)")
    ci_parser.add_argument("--run", required=True, help="the workflow run id")
    ci_parser.set_defaults(func=ci_run)

    args = parser.parse_args(argv)
    try:
        return args.func(args)
    except EvidenceError as error:
        return refuse(args.command, error.problems)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
