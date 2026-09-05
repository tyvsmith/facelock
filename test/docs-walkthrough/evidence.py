#!/usr/bin/env python3
"""Validate walkthrough evidence; never writes packaging release-gate markers."""
from __future__ import annotations

import argparse
from datetime import datetime
import hashlib
import json
from pathlib import Path
import re
import sys

LEVELS = ("syntax-only", "fixture", "container", "booted-vm", "physical-hardware")
STATUSES = ("pass", "fail", "blocked", "not-applicable")
STABLE_CHANNELS = ("apt", "aur", "copr-production")


def need(condition, message):
    if not condition:
        raise ValueError(message)


def nonempty(value):
    return isinstance(value, str) and bool(value.strip())


def digest(value, length=64):
    return isinstance(value, str) and re.fullmatch(rf"[0-9a-f]{{{length}}}", value) is not None


def validate_identity(identity, case, complete=True):
    need(isinstance(identity, dict), "identity must be an object")
    for key in ("release", "version", "channel"):
        need(nonempty(identity.get(key)), f"identity omits {key}")
    need(identity["release"] == "v" + identity["version"], "release/version mismatch")
    need(re.fullmatch(r"v\d+\.\d+\.\d+(?:-[A-Za-z0-9.]+)?", identity["release"]), "invalid explicit release")
    need(identity["channel"] == case["channel"], "scenario channel mismatch")
    if not complete:
        return
    channel = identity["channel"]
    need(not ("-" in identity["version"] and channel in STABLE_CHANNELS), "prerelease cannot be verified through a stable publication channel")
    need(digest(identity.get("artifact_commit"), 40), "artifact commit missing")
    need(nonempty(identity.get("native_version")), "native package version missing")
    need(identity.get("runtime_policy") == case.get("runtime_policy", identity.get("runtime_policy")), "runtime policy mismatch")
    if channel.startswith("github-") or channel in ("source", "nix"):
        artifact = identity.get("artifact", {})
        for key in ("name", "url"):
            need(nonempty(artifact.get(key)), f"artifact omits {key}")
        need(digest(artifact.get("sha256")), "artifact SHA256 missing")
        need(type(artifact.get("size")) is int and artifact["size"] > 0, "artifact size missing")
        if channel.startswith("github-"):
            need(type(artifact.get("asset_id")) is int and artifact["asset_id"] > 0, "artifact asset ID missing")
            expected = f"https://github.com/tyvsmith/facelock/releases/download/{identity['release']}/"
            need(artifact["url"] == expected + artifact["name"], "artifact URL does not identify the requested release asset")
        else:
            need(artifact["url"] == f"https://github.com/tyvsmith/facelock/archive/refs/tags/{identity['release']}.tar.gz", "artifact URL does not identify the published source tag")
    else:
        repository = identity.get("repository", {})
        need(nonempty(repository.get("url")), "repository URL missing")
        if channel != "aur":
            need(digest(identity.get("package_sha256")), "served package SHA256 missing")
        if channel == "apt":
            need(repository.get("url") == "https://tysmith.me/facelock/apt", "APT repository mismatch")
            need(repository.get("suite") == case.get("suite"), "APT suite mismatch")
        elif channel == "aur":
            need(repository.get("url") == f"https://aur.archlinux.org/{case['package']}.git", "AUR repository mismatch")
            need(digest(repository.get("aur_commit"), 40), "published AUR commit missing")
        else:
            need(repository.get("chroot") == case.get("chroot"), "COPR chroot mismatch")
            project = "facelock-testing" if channel == "copr-staging" else "facelock"
            need(repository.get("url") == f"https://copr.fedorainfracloud.org/coprs/tyvsmith/{project}/", "COPR repository mismatch")


def validate(record, case, require_pass=False):
    need(record.get("schema_version") == 1, "unknown evidence schema")
    need(record.get("scenario") == case["id"], "scenario mismatch")
    need(record.get("target") == case["target"], "target mismatch")
    need(record.get("status") in STATUSES, "invalid status")
    need(record.get("level") in LEVELS, "invalid level")
    need(record.get("sources") == case["sources"], "stale or missing source hashes")
    for key in ("docs_commit", "harness_commit"):
        need(digest(record.get(key), 40), f"{key} missing")
    need(digest(record.get("harness_sha256")), "harness source digest missing")
    need(type(record.get("harness_tree_dirty")) is bool, "harness dirty-tree state missing")
    for key in ("started_at", "finished_at"):
        try:
            stamp = datetime.fromisoformat(record[key].replace("Z", "+00:00"))
            need(stamp.tzinfo is not None, f"{key} must carry a timezone")
        except (KeyError, TypeError, ValueError) as error:
            raise ValueError(f"invalid {key}") from error
    need(datetime.fromisoformat(record["finished_at"].replace("Z", "+00:00")) >= datetime.fromisoformat(record["started_at"].replace("Z", "+00:00")), "timestamps reversed")
    passing = record["status"] == "pass"
    validate_identity(record.get("identity"), case, complete=passing)
    if require_pass:
        need(passing, "status does not establish completion")
        need(LEVELS.index(record["level"]) >= LEVELS.index(case["minimum_level"]), "evidence level does not establish this walkthrough")
    if not passing:
        need(nonempty(record.get("reason")), "non-pass record requires a reason")
        need(not any(step.get("status") == "pass" and step.get("executed") is not True for step in record.get("steps", [])), "unexecuted step relabeled pass")
        return
    environment = record.get("environment", {})
    for key in ("guest_id", "os", "image", "init", "snapshot"):
        need(nonempty(environment.get(key)), f"environment omits {key}")
    need(environment.get("isolation_verified") is True, "guest isolation was not verified")
    need(environment.get("pristine") is True, "missing pristine guest proof")
    pristine = environment.get("pristine_observations", {})
    for key in ("binary_absent", "config_absent", "state_absent", "package_absent"):
        need(pristine.get(key) is True, f"pristine observation {key} missing or false")
    steps = record.get("steps", [])
    need([step.get("id") for step in steps] == [step["id"] for step in case["steps"]], "ordered steps missing, duplicated or reordered")
    for observed, expected in zip(steps, case["steps"]):
        need(observed.get("status") == "pass" and observed.get("executed") is True, "step was not executed successfully")
        need(bool(observed.get("command")), "executed command missing")
        if expected.get("raw"):
            need(observed.get("documented_command") == expected["raw"], "documented invocation differs from the ordered example")
            need(isinstance(observed.get("fixture_bindings"), dict), "manual invocation fixture bindings missing")
        for invocation in expected.get("expected_invocations", []):
            commands = observed["command"]
            if commands and isinstance(commands[0], str):
                commands = [commands]
            normalized = []
            for command in commands:
                argv = list(command)
                if argv and argv[0] == "sudo":
                    argv = argv[1:]
                if argv and argv[0] == "runuser" and "--" in argv:
                    argv = argv[argv.index("--") + 1:]
                normalized.append(argv)
            need(any(argv and Path(argv[0]).name == invocation["program"] and all(argument in argv[1:] for argument in invocation.get("arguments", [])) for argv in normalized), "observed invocation does not execute the documented command")
        need(type(observed.get("exit_code")) is int and observed["exit_code"] == expected["expect_exit"], "step exit differs from expectation")
        need(re.search(expected.get("expect_output", ""), observed.get("output", "")) is not None, "step output differs from expectation")
        for state in expected.get("expect_state", []):
            need(observed.get("states", {}).get(state) is True, f"state {state} not observed")
        log = observed.get("log", {})
        need(nonempty(log.get("path")) and digest(log.get("sha256")) and log.get("sanitized") is True, "sanitized log identity missing")
    installed = record.get("installed", {})
    identity = record["identity"]
    for key in ("version", "native_version"):
        need(installed.get(key) == identity[key], f"installed {key} mismatches requested identity")
    expected_hash = identity.get("artifact", {}).get("sha256") or identity.get("package_sha256")
    if identity["channel"] == "aur" and expected_hash is None:
        need(digest(installed.get("artifact_sha256")), "installed AUR build payload hash missing")
        need(installed.get("aur_commit") == identity["repository"]["aur_commit"], "installed AUR recipe commit mismatch")
    else:
        need(installed.get("artifact_sha256") == expected_hash, "installed artifact hash mismatches requested identity")
    if require_pass:
        requirements = set(case.get("requirements", []))
        hardware = environment.get("hardware", [])
        need(requirements <= set(hardware), "required hardware/manual observations missing")
        if any(item in requirements for item in ("ir-camera", "physical-tpm", "gpu", "y16-camera")):
            need(record["level"] == "physical-hardware", "physical hardware evidence required")
        if case.get("adapter") == "manual":
            review = record.get("manual_review", {})
            need(nonempty(review.get("operator")) and nonempty(review.get("notes")) and review.get("expectations_reviewed") is True and isinstance(review.get("fixture_bindings"), dict), "manual operator, fixture bindings and reviewed expectations are required")


def validate_record_file(path, case, require_pass=False):
    record = json.loads(path.read_text())
    validate(record, case, require_pass)
    for step in record.get("steps", []):
        if "log" in step:
            log = path.parent / step["log"]["path"]
            need(not log.is_symlink() and log.resolve().is_relative_to(path.parent.resolve()), "log escapes evidence directory or is symlinked")
            need(hashlib.sha256(log.read_bytes()).hexdigest() == step["log"]["sha256"], "log hash mismatch")
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=["validate", "aggregate"])
    parser.add_argument("record", type=Path)
    parser.add_argument("--require-pass", action="store_true")
    args = parser.parse_args()
    import run
    try:
        if args.command == "aggregate":
            cases = run.load_cases()
            run.check_sources(cases)
            coverage = run.coverage(cases, run.source_occurrences())
            complete = set()
            for path in args.record.glob("**/evidence.json"):
                record = json.loads(path.read_text())
                case = next(case for case in cases if case["id"] == record["scenario"])
                validate_record_file(path, case)
                try:
                    validate_record_file(path, case, require_pass=True)
                    complete.add(case["id"])
                except ValueError:
                    pass
            missing = sorted({case["id"] for case in cases} - complete)
            print(json.dumps({"schema_version": 1, "completed_cases": sorted(complete), "missing_cases": missing, "unmapped_occurrences": coverage["unmapped"]}, indent=2))
            if args.require_pass:
                need(not missing and not coverage["unmapped"], "incomplete cases or uncovered documented obligations")
            return 0
        record = json.loads(args.record.read_text())
        case = next(case for case in run.load_cases() if case["id"] == record["scenario"])
        run.check_sources([case])
        validate_record_file(args.record, case, args.require_pass)
    except (ValueError, KeyError, OSError, StopIteration) as error:
        print(f"walkthrough evidence: {error}", file=sys.stderr)
        return 1
    print(f"walkthrough evidence: {record['scenario']} {record['status']} ({record['level']})")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
