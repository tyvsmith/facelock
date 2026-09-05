#!/usr/bin/env python3
"""Explicit published-channel walkthroughs in disposable guests.

There is deliberately no command that executes every discovered doc example.
`refresh` updates source pins for review; `check` only compares them. A container
result never satisfies a case whose minimum level is a booted VM.
"""
from __future__ import annotations

import argparse
import copy
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import uuid

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]
MANIFEST = HERE / "cases.json"
MANUAL_CASES = HERE / "manual-sections.json"
MATRIX = ROOT / "dist/release-matrix.json"
MARKER = Path("/etc/facelock-walkthrough-guest.json")
ADAPTERS = frozenset(("arch", "apt", "deb", "rpm", "copr", "source", "verify-cli", "manual"))
spec = importlib.util.spec_from_file_location("walkthrough_evidence", HERE / "evidence.py")
evidence = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evidence)


def now():
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat()


def load_json(path):
    return json.loads(path.read_text())


def write_json(path, value):
    # Exclusive output creation prevents a later invocation overwriting evidence.
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, indent=2)
        stream.write("\n")


def load_cases(include_manual=True):
    manifest = load_json(MANIFEST)
    matrix = load_json(MATRIX)
    cases = []
    arch = next(row for row in matrix["platforms"] if row.get("variant") == "PKGBUILD and binary recipe")
    for template in manifest["templates"]:
        expansion = template.get("expand")
        if expansion == "apt":
            bindings = [{"suite": name, "target": row["platform_id"], "image": row["image"]} for name, row in matrix["apt_suites"].items() if name != "compat"]
        elif expansion == "fedora":
            bindings = []
            for chroot in matrix["fedora"]["packit_release_targets"]:
                release = chroot.split("-")[1]
                row = next(row for row in matrix["platforms"] if row["platform"].startswith(f"Fedora {release}") and row.get("release_target"))
                bindings.append({"release": release, "target": f"fedora-{release}", "chroot": chroot, "image": row["image"], "depth": row["lifecycle_depth"]})
        elif expansion == "aur":
            bindings = [{"package": name, "target": arch["id"], "image": arch["image"]} for name in ("facelock", "facelock-bin", "facelock-git")]
        else:
            bindings = [{}]
        for binding in bindings:
            case = copy.deepcopy(manifest["defaults"])
            case.update(template)
            case.update(binding)
            # These fixed adapters test an installation route. The cited
            # examples supply context; they are not literal shell replay.
            case["source_role"] = "route-reference"
            case["id"] = template["id"].format(**binding)
            case["sources"] = manifest.get("source_pins", {}).get(template["id"], [])
            if "image" not in case:
                row = next((row for row in matrix["platforms"] if row["id"] == case["target"]), None)
                if row:
                    case["image"] = row["image"]
            case["steps"] = [
                {"id": "documented-install" if case["adapter"] != "manual" else "manual-walkthrough", "adapter": case["adapter"], "expect_exit": 0, "expect_output": "", "expect_state": ["installed"], "timeout_seconds": case["timeout_seconds"]},
                {"id": "installed-cli", "adapter": "verify-cli", "expect_exit": 0, "expect_output": "facelock", "expect_state": ["identity-matched"], "timeout_seconds": 30},
            ]
            invocations = {
                "arch": [{"program": "yay", "arguments": ["-S", case.get("package", "facelock")]}],
                "apt": [{"program": "apt", "arguments": ["update"]}, {"program": "apt", "arguments": ["install", "facelock"]}],
                "deb": [{"program": "apt", "arguments": ["install"]}],
                "rpm": [{"program": "dnf", "arguments": ["install"]}],
                "copr": [{"program": "dnf", "arguments": ["copr", "enable"]}, {"program": "dnf", "arguments": ["install", "facelock"]}],
                "source": [{"program": "just", "arguments": ["install"]}],
                "manual": [],
            }
            case["steps"][0]["expected_invocations"] = invocations[case["adapter"]]
            case["steps"][1]["expected_invocations"] = [{"program": "facelock", "arguments": ["--help"]}, {"program": "facelock", "arguments": ["--version"]}]
            cases.append(case)
    if include_manual and MANUAL_CASES.exists():
        cases.extend(load_json(MANUAL_CASES)["cases"])
    return cases


def source_occurrences():
    snapshot = ROOT / "walkthrough-provenance.json"
    if snapshot.exists():
        provenance = load_json(snapshot)
        for path, expected in provenance["source_files"].items():
            if hashlib.sha256((ROOT / path).read_bytes()).hexdigest() != expected:
                raise ValueError(f"copied guest source changed: {path}")
        return load_json(ROOT / "walkthrough-occurrences.json")
    result = subprocess.run([sys.executable, str(ROOT / "test/docs-examples.py"), "--json"], capture_output=True, text=True, check=True)
    return json.loads(result.stdout)["occurrences"]


def coverage(cases, occurrences):
    obligations = []
    for row in occurrences:
        if row["classification"] not in ("executable", "manual"):
            continue
        matching = [case["id"] for case in cases if case.get("source_role") != "route-reference" and row["source"] in case["sources"]]
        obligations.append({"source": row["source"], "raw": row["raw"], "classification": row["classification"], "scenario_ids": matching, "status": "pending", "reason": "execution evidence required" if matching else "no reviewed ordered scenario covers this occurrence"})
    return {"schema_version": 1, "obligations": obligations, "unmapped": sum(not row["scenario_ids"] for row in obligations), "pending": len(obligations)}


def manual_sections(cases, occurrences):
    """Define ordered manual cases for remaining sections, never execute them.

    Each occurrence keeps its own source identity even when another guide
    spells the same command. Classification sets prerequisites, not outcomes.
    A reviewer must supply fixtures and observations before claiming a pass.
    """
    sections = {}
    for row in occurrences:
        if row["classification"] in ("executable", "manual"):
            source = row["source"]
            sections.setdefault((source["path"], source["anchor"]), []).append(row)
    result = []
    for (path, anchor), rows in sections.items():
        if all(any(case.get("source_role") != "route-reference" and row["source"] in case["sources"] for case in cases) for row in rows):
            continue
        text = "\n".join(row["raw"] for row in rows)
        requirements = ["human-command-review", "section-prerequisites", "fixture-bindings", "observed-postconditions"]
        if re.search(r"\b(enroll|preview|bench|calibrate|warm-auth|cold-auth)\b|facelock\s+(?:auth|test)\b|test-arch-(?:integration|oneshot|camera-required)", text):
            requirements.extend(["ir-camera", "models", "human-pam-recovery"])
        if re.search(r"\btpm\b|PCR|swtpm", text):
            requirements.extend(["tpm-environment-selection", "key-backup", "pcr-change-recovery"])
        if re.search(r"cuda|rocm|openvino|nvidia", text, re.I):
            requirements.extend(["gpu", "gpu-runtime"])
        if re.search(r"pam|sudo|systemctl|login|su\b|uninstall|purge|decrypt|encrypt", text):
            requirements.append("human-pam-recovery")
        if re.search(r"hyprlock|polkit|notify|preview", text):
            requirements.append("desktop-session")
        if re.search(r"nix|nixos", text):
            requirements.append("nixos-vm")
        if path.startswith(("docs/releasing", ".claude/skills/release")) or re.search(r"git\s+push|gh\s+release\s+(?:create|upload)|just\s+release\b", text):
            requirements.extend(["maintainer-environment", "explicit-publication-authority"])
        if re.search(r"\b(cargo|just)\b|test/|scripts/", text):
            requirements.append("published-source-build-prerequisites")
        identifier = hashlib.sha256(f"{path}#{anchor}".encode()).hexdigest()[:12]
        case = {
            "id": "manual-section-" + identifier, "target": "section-specific-guest",
            "channel": "github-alpha", "adapter": "manual", "generated_section": True,
            "source_role": "ordered-occurrence", "review_status": "candidate",
            "review_reason": "mechanically grouped commands; a human must review section fixtures, exit expectations and applicability before execution",
            "title": f"{path}#{anchor}", "minimum_level": "physical-hardware" if any(item in requirements for item in ("ir-camera", "gpu")) else "booted-vm",
            "requirements": list(dict.fromkeys(requirements)),
            "prerequisites": ["restore a pristine disposable guest for this section", "read the enclosing section and earlier prerequisite headings", "bind every user, path, service, device, package and secret placeholder to guest-local fixtures", "record intentionally failing commands with their documented expected outcome"],
            "fixtures": {"binding_policy": "explicit reviewer-supplied guest-local bindings; no host paths or credentials"},
            "restore": "restore the recorded pristine snapshot; retain sanitized evidence outside the guest",
            "timeout_seconds": 1800,
            "sources": [row["source"] for row in rows],
            "steps": [{"id": f"example-{number}", "adapter": "manual", "raw": row["raw"], "classification": row["classification"], "expect_exit": 0, "expect_output": "", "expect_state": ["documented-effect-observed"], "timeout_seconds": 1800, "expectation_review": "confirm the documented outcome and record any expected nonzero exit before execution"} for number, row in enumerate(rows, 1)],
        }
        result.append(case)
    return result


def select_sources(case, occurrences):
    selected = []
    for selector in case["source_selectors"]:
        matches = [row["source"] for row in occurrences if row["source"]["path"] == selector["path"] and selector["contains"] in row["raw"]]
        if not matches:
            raise ValueError(f"source selector no longer matches {selector}; choose and review its replacement")
        selected.extend(matches)
    return selected


def check_sources(cases):
    occurrences = source_occurrences()
    for case in cases:
        if case.get("generated_section"):
            expected = [row for row in occurrences if row["source"]["path"] == case["sources"][0]["path"] and row["source"]["anchor"] == case["sources"][0]["anchor"] and row["classification"] in ("executable", "manual")]
            if case["sources"] != [row["source"] for row in expected] or [step["raw"] for step in case["steps"]] != [row["raw"] for row in expected]:
                raise ValueError(f"stale ordered section scenario {case['id']}; review and refresh")
            continue
        expected = select_sources(case, occurrences)
        if case["sources"] != expected:
            raise ValueError(f"stale source hashes for {case['id']}; review changes and run refresh")


def refresh():
    manifest = load_json(MANIFEST)
    occurrences = source_occurrences()
    pins = {row["id"]: select_sources(row, occurrences) for row in manifest["templates"]}
    manifest["source_pins"] = pins
    MANIFEST.write_text(json.dumps(manifest, indent=2) + "\n")
    manual = manual_sections(load_cases(include_manual=False), occurrences)
    MANUAL_CASES.write_text(json.dumps({"schema_version": 1, "cases": manual}, indent=2) + "\n")
    print("refreshed source pins and candidate manual sections; review the diff and execution prerequisites")


def verify_guest(marker=MARKER):
    if not marker.exists() or marker.is_symlink():
        raise ValueError("disposable guest marker is absent or symlinked")
    info = marker.stat()
    if info.st_uid != 0 or info.st_mode & 0o022 or not stat.S_ISREG(info.st_mode):
        raise ValueError("guest marker must be a root-owned regular file without group/other writes")
    guest = load_json(marker)
    for field in ("guest_id", "os", "image", "init", "snapshot", "level"):
        if not evidence.nonempty(guest.get(field)):
            raise ValueError(f"guest marker omits {field}")
    if guest.get("disposable") is not True or guest["level"] not in ("container", "booted-vm"):
        raise ValueError("runner requires a disposable container or VM; hardware is manual")
    result = subprocess.run(["systemd-detect-virt"], capture_output=True, text=True)
    if result.returncode or result.stdout.strip() in ("", "none"):
        raise ValueError("guest virtualization could not be verified")
    detected = result.stdout.strip()
    containers = ("podman", "docker", "lxc", "systemd-nspawn")
    if (guest["level"] == "container") != (detected in containers):
        raise ValueError("guest evidence level differs from detected virtualization")
    # Refuse shared host paths and hardware. The launcher copies files instead
    # of mounting the checkout, host /run, PAM, devices, or a system bus.
    for line in Path("/proc/self/mountinfo").read_text().splitlines():
        columns = line.split()
        target = columns[4]
        suffix = line.split(" - ", 1)[1].split()
        if suffix[0] in ("9p", "virtiofs", "nfs", "nfs4", "cifs"):
            raise ValueError("shared host filesystem is not allowed in a walkthrough guest")
        protected = ("/etc/pam.d", "/run/dbus", "/var/run/dbus", "/var/lib/facelock", "/etc/facelock")
        if any(target == path or target.startswith(path + "/") for path in protected):
            raise ValueError(f"protected host exposure or unexpected mount: {target}")
    if any(Path("/dev").glob("video*")) or any(Path("/dev").glob("tpm*")):
        raise ValueError("generic runner forbids camera/TPM passthrough; use the manual hardware protocol")
    guest["isolation_verified"] = True
    return guest


def pristine_guest():
    package_absent = True
    for command in (["dpkg-query", "-W", "-f=${db:Status-Abbrev}", "facelock"], ["rpm", "-q", "facelock"], ["pacman", "-Q", "facelock"]):
        if shutil.which(command[0]):
            result = subprocess.run(command, capture_output=True, text=True)
            if result.returncode == 0 and (command[0] != "dpkg-query" or result.stdout.startswith("ii")):
                package_absent = False
    observations = {
        "binary_absent": shutil.which("facelock") is None,
        "config_absent": not Path("/etc/facelock").exists(),
        "state_absent": not Path("/var/lib/facelock").exists() and not Path("/var/log/facelock").exists(),
        "package_absent": package_absent,
    }
    if not all(observations.values()):
        raise ValueError(f"guest is not pristine: {observations}")
    return observations


def adapter_command(step, bindings):
    name = step.get("adapter")
    if name not in ADAPTERS:
        raise ValueError(f"unknown install adapter {name!r}")
    return ["bash", str(HERE / "guest.sh"), name]


def commit():
    snapshot = ROOT / "walkthrough-provenance.json"
    if snapshot.exists():
        return load_json(snapshot)["harness_commit"]
    result = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True)
    if result.returncode == 0:
        return result.stdout.strip()
    raise ValueError("cannot identify harness commit")


def initial_record(case, identity):
    return {"schema_version": 1, "scenario": case["id"], "target": case["target"], "source_role": case.get("source_role", "ordered-occurrence"), "status": "blocked", "level": "syntax-only", "reason": "not executed", "docs_commit": commit(), "harness_commit": commit(), **harness_identity(), "sources": case["sources"], "started_at": now(), "finished_at": now(), "identity": identity, "artifact_commit_status": "asserted; consult installed.source_commit_verification for independently observed provenance", "environment": {}, "steps": [], "installed": {}}


def harness_identity():
    provenance = ROOT / "walkthrough-provenance.json"
    if provenance.exists():
        recorded = load_json(provenance)
        return {key: recorded[key] for key in ("harness_sha256", "harness_tree_dirty")}
    files = sorted(path for path in HERE.iterdir() if path.suffix in (".py", ".sh", ".json"))
    digest = hashlib.sha256()
    for path in files:
        digest.update(path.name.encode() + b"\0" + path.read_bytes() + b"\0")
    dirty = subprocess.run(["git", "-C", str(ROOT), "status", "--porcelain"], capture_output=True, text=True, check=True)
    return {"harness_sha256": digest.hexdigest(), "harness_tree_dirty": bool(dirty.stdout)}


def sanitized(text):
    text = re.sub(r"(?i)(password|token|secret|authorization)(\s*[:=]\s*)\S+", r"\1\2[redacted]", text)
    text = re.sub(r"https://[^/@\s]+:[^/@\s]+@", "https://[redacted]@", text)
    return text


def execute(command, env, timeout):
    process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
        return process.returncode, stdout + stderr
    except subprocess.TimeoutExpired:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            stdout, stderr = process.communicate(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            stdout, stderr = process.communicate()
        return 124, stdout + stderr + f"\nstep timed out after {timeout} seconds\n"


def run_guest(case, identity, output):
    check_sources([case])
    evidence.validate_identity(identity, case)
    guest = verify_guest()
    if guest["os"] != case["target"]:
        raise ValueError("guest OS does not match scenario target")
    guest["pristine_observations"] = pristine_guest()
    guest["pristine"] = True
    if case["adapter"] == "manual":
        raise ValueError("manual scenario requires its documented human/hardware observations")
    output.mkdir(parents=True, exist_ok=False)
    record = initial_record(case, identity)
    record.update(environment=guest, level=guest["level"], status="pass", reason="")
    with tempfile.TemporaryDirectory(prefix="facelock-walkthrough-") as work:
        context = {"case": case, "identity": identity, "work": work}
        context_path = Path(work) / "context.json"
        write_json(context_path, context)
        env = dict(os.environ, FACELOCK_WALKTHROUGH_CONTEXT=str(context_path))
        for step in case["steps"]:
            command = adapter_command(step, context)
            code, text = execute(command, env, step["timeout_seconds"])
            text = sanitized(text)
            log = output / f"{step['id']}.log"
            log.write_text(text)
            status = "pass" if code == step["expect_exit"] and re.search(step["expect_output"], text) else "fail"
            observed = {"id": step["id"], "command": command, "status": status, "executed": True, "exit_code": code, "output": text, "states": {}, "log": {"path": log.name, "sha256": hashlib.sha256(log.read_bytes()).hexdigest(), "sanitized": True}}
            if status == "pass":
                # The guest writes observations only after checking each state.
                observation = load_json(Path(work) / "observations.json")
                observed["states"] = observation["states"]
                observed["command"] = observation["commands"]
                record["installed"] = observation["installed"]
            record["steps"].append(observed)
            if status != "pass":
                record.update(status="fail", reason=f"{step['id']} failed with exit {code}")
                break
    record["finished_at"] = now()
    try:
        evidence.validate(record, case)
    except ValueError as error:
        record.update(status="fail", reason=str(error))
    write_json(output / "evidence.json", record)
    return 0 if record["status"] == "pass" else 1


def container_image(case, image):
    if not case.get("image") or image != case["image"] or "@sha256:" not in image:
        raise ValueError("container must use the exact matrix image for this scenario")
    return image


def launch_container(case, identity, image, output):
    """Rootless, copied inputs, unique resources, no privileged/device mounts."""
    container_image(case, image)
    check_sources([case])
    evidence.validate_identity(identity, case)
    if case["adapter"] in ("manual", "source", "arch"):
        raise ValueError("this route needs a preprovisioned VM (AUR helper/source prerequisites or manual steps)")
    info = subprocess.run(["podman", "info", "--format", "{{.Host.Security.Rootless}}"], capture_output=True, text=True, check=True)
    if info.stdout.strip() != "true":
        raise ValueError("automatic container launch requires rootless Podman")
    output.mkdir(parents=True, exist_ok=False)
    name = "facelock-walkthrough-" + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix=name + "-") as directory:
        stage = Path(directory)
        tree = stage / "walkthrough"
        tree.mkdir()
        for relative in ("test/docs-walkthrough", "test/docs-examples.py", "dist/release-matrix.json", "README.md", "docs", "book", "man", "Cargo.toml"):
            source, destination = ROOT / relative, tree / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            if source.is_dir():
                shutil.copytree(source, destination, ignore=shutil.ignore_patterns("__pycache__", "book", "target"))
            else:
                shutil.copy2(source, destination)
        occurrences = source_occurrences()
        source_files = {source["path"] for row in load_cases() for source in row["sources"]}
        for path in source_files:
            destination = tree / path
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / path, destination)
        write_json(tree / "walkthrough-provenance.json", {"harness_commit": commit(), **harness_identity(), "source_files": {path: hashlib.sha256((ROOT / path).read_bytes()).hexdigest() for path in source_files}})
        write_json(tree / "walkthrough-occurrences.json", occurrences)
        write_json(stage / "identity.json", identity)
        marker = {"disposable": True, "guest_id": name, "os": case["target"], "image": image, "init": "container", "snapshot": f"fresh:{name}", "level": "container", "hardware": []}
        write_json(stage / "marker.json", marker)
        subprocess.run(["podman", "run", "-d", "--name", name, "--network", "pasta", image, "sleep", "infinity"], check=True)
        try:
            for source, destination in ((tree, "/walkthrough"), (stage / "identity.json", "/identity.json"), (stage / "marker.json", str(MARKER))):
                subprocess.run(["podman", "cp", str(source), f"{name}:{destination}"], check=True)
            # Harness-only tools are visible in the transcript. Candidate
            # dependencies are not preinstalled by the launcher.
            bootstrap = "apt-get update && apt-get install -y --no-install-recommends python3 bash systemd ca-certificates curl sudo" if case["adapter"] in ("apt", "deb") else "dnf -y install python3 bash systemd ca-certificates curl sudo"
            boot = subprocess.run(["podman", "exec", name, "sh", "-c", bootstrap], capture_output=True, text=True)
            (output / "bootstrap.log").write_text(sanitized(boot.stdout + boot.stderr))
            if boot.returncode:
                raise ValueError("guest harness bootstrap failed; see bootstrap.log")
            result = subprocess.run(["podman", "exec", name, "python3", "/walkthrough/test/docs-walkthrough/run.py", "run", "--scenario", case["id"], "--identity", "/identity.json", "--output", "/results"], capture_output=True, text=True)
            (output / "runner.log").write_text(sanitized(result.stdout + result.stderr))
            subprocess.run(["podman", "cp", f"{name}:/results/.", str(output)], check=False)
            if not (output / "evidence.json").exists():
                record = initial_record(case, identity)
                record["reason"] = "guest precondition failed: " + sanitized(result.stderr.strip())
                record["environment"] = marker
                write_json(output / "evidence.json", record)
            return result.returncode
        finally:
            subprocess.run(["podman", "rm", "-f", name], check=False, stdout=subprocess.DEVNULL)


def readiness(release, channel, output):
    result = {"schema_version": 1, "checked_at": now(), "release": release, "channel": channel, "status": "blocked", "level": "syntax-only", "checks": {}}
    if not re.fullmatch(r"v\d+\.\d+\.\d+(?:-[A-Za-z0-9.]+)?", release):
        raise ValueError("readiness requires an explicit versioned release tag")
    for tool in ("podman", "qemu-system-x86_64", "virsh", "nix", "gh"):
        result["checks"][tool] = shutil.which(tool)
    if "-" in release and channel in evidence.STABLE_CHANNELS:
        result["reason"] = "release matrix forbids prerelease publication to this stable channel"
    elif shutil.which("gh"):
        remote = subprocess.run(["gh", "release", "view", release, "--repo", "tyvsmith/facelock", "--json", "tagName,isDraft,isPrerelease,assets,publishedAt"], capture_output=True, text=True)
        result["checks"]["release_exit"] = remote.returncode
        if remote.returncode:
            result["reason"] = "published release unavailable: " + sanitized(remote.stderr.strip())
        else:
            result["checks"]["release"] = json.loads(remote.stdout)
            result["status"] = "ready-for-identity-review"
            result["reason"] = "metadata is available; choose exact asset IDs and verify payload hashes before execution"
    else:
        result["reason"] = "gh unavailable; published identity not checked"
    write_json(output, result)
    print(result["reason"])
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    for command in ("list", "check", "refresh", "report"):
        sub.add_parser(command)
    ready = sub.add_parser("readiness")
    ready.add_argument("--release", required=True)
    ready.add_argument("--channel", required=True, choices=("github-alpha", "github-stable", "apt", "aur", "copr-production", "copr-staging", "source", "nix"))
    ready.add_argument("--output", type=Path, required=True)
    for command in ("run", "blocked", "launch-container"):
        child = sub.add_parser(command)
        child.add_argument("--scenario", required=True)
        child.add_argument("--identity", type=Path, required=True)
        child.add_argument("--output", type=Path, required=True)
        if command == "blocked":
            child.add_argument("--reason", required=True)
        if command == "launch-container":
            child.add_argument("--image", required=True)
    args = parser.parse_args()
    try:
        if args.command == "readiness":
            return readiness(args.release, args.channel, args.output)
        if args.command == "refresh":
            refresh()
            return 0
        cases = load_cases()
        if args.command == "list":
            print(json.dumps({"schema_version": 1, "cases": cases}, indent=2))
            return 0
        if args.command == "report":
            check_sources(cases)
            print(json.dumps(coverage(cases, source_occurrences()), indent=2))
            return 0
        if args.command == "check":
            check_sources(cases)
            missing = coverage(cases, source_occurrences())["unmapped"]
            if missing:
                raise ValueError(f"{missing} documented obligations have no scenario; review refresh output")
            print(f"walkthrough cases: {len(cases)} definitions; source hashes match; generated manual sections remain candidates pending human review")
            return 0
        case = next((case for case in cases if case["id"] == args.scenario), None)
        if case is None:
            raise ValueError(f"unknown explicit scenario {args.scenario!r}")
        identity = load_json(args.identity)
        if args.command == "blocked":
            check_sources([case])
            record = initial_record(case, identity)
            record["reason"] = args.reason
            evidence.validate(record, case)
            args.output.mkdir(parents=True, exist_ok=False)
            write_json(args.output / "evidence.json", record)
            return 0
        if args.command == "launch-container":
            return launch_container(case, identity, args.image, args.output)
        return run_guest(case, identity, args.output)
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        print(f"walkthrough: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
