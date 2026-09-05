#!/usr/bin/env python3
"""Thin fixed-command guest adapters. No host package-manager entry point."""
import hashlib
import json
import os
from pathlib import Path
import pwd
import shlex
import shutil
import subprocess
import sys
import tarfile
import urllib.request

import run

COMMANDS = []


def command(argv, cwd=None):
    COMMANDS.append(argv)
    print("$ " + shlex.join(argv), flush=True)
    result = subprocess.run(argv, cwd=cwd, capture_output=True, text=True)
    print(result.stdout, end="", flush=True)
    print(result.stderr, end="", file=sys.stderr, flush=True)
    if result.returncode:
        raise ValueError(f"documented command exited {result.returncode}: {shlex.join(argv)}")
    return result.stdout.strip()


def payload_hash(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def fetch(url, destination, digest, size=None):
    # Input URLs have already been restricted by validate_identity. Redirects
    # are needed for GitHub release storage; bytes are checked before use.
    print(f"fetch {url} -> {destination.name}", flush=True)
    with urllib.request.urlopen(url, timeout=60) as response, destination.open("xb") as output:
        shutil.copyfileobj(response, output)
    if payload_hash(destination) != digest:
        raise ValueError("downloaded artifact SHA256 mismatch")
    if size is not None and destination.stat().st_size != size:
        raise ValueError("downloaded artifact size mismatch")
    return destination


def release_payload(identity, work):
    artifact = identity["artifact"]
    commit_url = "https://api.github.com/repos/tyvsmith/facelock/commits/" + identity["release"]
    with urllib.request.urlopen(commit_url, timeout=30) as response:
        resolved = json.load(response)
    if resolved.get("sha") != identity["artifact_commit"]:
        raise ValueError("published tag commit differs from requested artifact commit")
    (work / "source-commit-verification.json").write_text(json.dumps({"asserted_commit": identity["artifact_commit"], "observed_tag_commit": resolved["sha"], "tag_commit_verified": True, "build_commit_verified": False, "reason": "GitHub tag resolution checked; no binary build attestation validated"}))
    if identity["channel"].startswith("github-"):
        url = "https://api.github.com/repos/tyvsmith/facelock/releases/tags/" + identity["release"]
        with urllib.request.urlopen(url, timeout=30) as response:
            release = json.load(response)
        if release.get("draft") or release.get("tag_name") != identity["release"]:
            raise ValueError("requested release is not published")
        found = next((asset for asset in release.get("assets", []) if asset["id"] == artifact["asset_id"]), None)
        if found is None or found["name"] != artifact["name"] or found["size"] != artifact["size"] or found["browser_download_url"] != artifact["url"]:
            raise ValueError("published release asset ID/name/size/URL mismatch")
        if found.get("digest") and found["digest"] != "sha256:" + artifact["sha256"]:
            raise ValueError("published release API digest mismatch")
    return fetch(artifact["url"], work / Path(artifact["name"]).name, artifact["sha256"], artifact["size"])


def native_version(case):
    if case["adapter"] in ("apt", "deb"):
        return command(["dpkg-query", "-W", "-f=${Version}", "facelock"])
    if case["adapter"] in ("rpm", "copr"):
        return command(["rpm", "-q", "--qf", "%{VERSION}-%{RELEASE}", "facelock"])
    if case["adapter"] == "arch":
        return command(["pacman", "-Q", case["package"]]).split(maxsplit=1)[1]
    return command(["facelock", "--version"]).split()[-1]


def observe(case, identity, work, state, actual_hash):
    version_text = command(["facelock", "--version"])
    version = version_text.split()[-1]
    native = native_version(case)
    installed = {"version": version, "native_version": native, "artifact_sha256": actual_hash}
    installed["payload_binding"] = {"deb": "hash-verified local artifact passed to apt", "rpm": "hash-verified local artifact passed to dnf", "apt": "same-version package downloaded after install; not transaction-byte identity proof", "copr": "matching retained cache payload; same-version replacement not independently excluded", "arch": "matching locally built package version and reviewed recipe; not a reproducible-build assertion", "source": "hash-verified published source archive used for guest build"}[case["adapter"]]
    installed["transaction_payload_verification"] = {"verified": case["adapter"] in ("deb", "rpm", "source"), "method": "local-payload-input" if case["adapter"] in ("deb", "rpm", "source") else "not-established", "sha256": actual_hash}
    provenance = work / "source-commit-verification.json"
    installed["source_commit_verification"] = json.loads(provenance.read_text()) if provenance.exists() else {"asserted_commit": identity["artifact_commit"], "tag_commit_verified": False, "build_commit_verified": False, "reason": "package version and payload verified; source commit was asserted, not established by package-manager metadata"}
    if case["adapter"] == "arch":
        installed["aur_commit"] = (work / "aur-commit").read_text().strip()
    if version != identity["version"] or native != identity["native_version"]:
        raise ValueError(f"installed identity mismatch: expected {identity['version']} / {identity['native_version']}, observed {version} / {native}")
    expected = identity.get("artifact", {}).get("sha256") or identity.get("package_sha256")
    if expected is not None and actual_hash != expected:
        raise ValueError("installed payload does not match expected artifact SHA256")
    output = {"states": {state: True}, "installed": installed, "commands": COMMANDS}
    (work / "observations.json").write_text(json.dumps(output) + "\n")


def apt(case, identity, work):
    repository = identity["repository"]
    key_hash = repository.get("key_sha256", "")
    if not run.evidence.digest(key_hash):
        raise ValueError("APT identity needs the independently reviewed signing-key SHA256")
    command(["sudo", "install", "-d", "-m", "0755", "/etc/apt/keyrings"])
    key = fetch("https://tysmith.me/facelock/apt/tysmith-archive-keyring.gpg", work / "archive-keyring.gpg", key_hash)
    command(["sudo", "install", "-m", "0644", str(key), "/etc/apt/keyrings/tysmith-archive-keyring.gpg"])
    suite = case["suite"]
    entry = f"deb [signed-by=/etc/apt/keyrings/tysmith-archive-keyring.gpg] https://tysmith.me/facelock/apt {suite} facelock\n"
    Path("/etc/apt/sources.list.d/facelock.list").write_text(entry)
    print(f"write /etc/apt/sources.list.d/facelock.list: {entry}", end="")
    command(["sudo", "apt", "update"])
    command(["sudo", "apt", "install", "-y", "facelock"])
    command(["apt-get", "download", "facelock=" + identity["native_version"]], cwd=work)
    files = list(work.glob("*.deb"))
    if len(files) != 1:
        raise ValueError("served package audit did not yield exactly one Debian package")
    return payload_hash(files[0])


def copr(case, identity, work):
    project = "facelock-testing" if case["channel"] == "copr-staging" else "facelock"
    command(["sudo", "dnf", "copr", "enable", "-y", "tyvsmith/" + project])
    # keepcache preserves exactly what this transaction downloaded. It does
    # not change dependency resolution; the transcript records the option.
    command(["sudo", "dnf", "--setopt=keepcache=True", "install", "-y", "facelock"])
    packages = list(Path("/var/cache").glob("**/facelock-*.rpm"))
    matches = [path for path in packages if payload_hash(path) == identity["package_sha256"]]
    if len(matches) != 1:
        raise ValueError("installed COPR transaction did not retain the pinned package payload")
    return payload_hash(matches[0])


def arch(case, identity, work):
    # Helper provisioning is an explicit prerequisite, separate from installing
    # Facelock. makepkg/yay must run as an ordinary user, never as root.
    user = case["fixtures"]["user"]
    command(["id", user])
    command(["runuser", "-u", user, "--", "yay", "-S", "--noconfirm", case["package"]])
    recipe = Path("/home") / user / ".cache/yay" / case["package"]
    recipe_commit = command(["runuser", "-u", user, "--", "git", "-C", str(recipe), "rev-parse", "HEAD"])
    if recipe_commit != identity["repository"]["aur_commit"]:
        raise ValueError("served AUR recipe commit differs from the reviewed identity")
    (work / "aur-commit").write_text(recipe_commit)
    packages = list(recipe.glob("*.pkg.tar.*")) + list(Path("/var/cache/pacman/pkg").glob(case["package"] + "-*.pkg.tar.*"))
    expected_package = case["package"] + " " + identity["native_version"]
    matches = [path for path in packages if not path.name.endswith(".sig") and command(["pacman", "-Qp", str(path)]) == expected_package]
    if not matches:
        raise ValueError("installed AUR build payload was not found after installation")
    hashes = {payload_hash(path) for path in matches}
    if len(hashes) != 1:
        raise ValueError("multiple different AUR payloads claim the installed version")
    return hashes.pop()


def source(case, identity, work):
    archive = release_payload(identity, work)
    source_root = work / "source"
    source_root.mkdir()
    # Python's data filter prevents archive traversal, special files and links
    # outside the staging root. This is never extraction into the host root.
    with tarfile.open(archive) as tar:
        tar.extractall(source_root, filter="data")
    children = list(source_root.iterdir())
    if len(children) != 1 or not (children[0] / "Cargo.toml").is_file():
        raise ValueError("published source archive has an unexpected root")
    user = case["fixtures"]["user"]
    account = pwd.getpwnam(user)
    work.chmod(0o755)
    for path in [source_root, *source_root.rglob("*")]:
        os.chown(path, account.pw_uid, account.pw_gid, follow_symlinks=False)
    command(["runuser", "-u", user, "--", "just", "install"], cwd=children[0])
    return payload_hash(archive)


def main():
    adapter = sys.argv[1]
    run.verify_guest()
    context = json.loads(Path(os.environ["FACELOCK_WALKTHROUGH_CONTEXT"]).read_text())
    case, identity, work = context["case"], context["identity"], Path(context["work"])
    run.evidence.validate_identity(identity, case)
    if adapter == "verify-cli":
        previous = json.loads((work / "observations.json").read_text())
        command(["facelock", "--help"])
        observe(case, identity, work, "identity-matched", previous["installed"]["artifact_sha256"])
        return 0
    if adapter != case["adapter"] or adapter == "manual":
        raise ValueError("adapter differs from the reviewed scenario or requires a manual walkthrough")
    if adapter in ("deb", "rpm"):
        package = release_payload(identity, work)
        if adapter == "deb":
            command(["sudo", "apt", "update"])
            command(["sudo", "apt", "install", "-y", str(package)])
        else:
            command(["sudo", "dnf", "install", "-y", str(package)])
        actual_hash = payload_hash(package)
    else:
        actual_hash = {"apt": apt, "copr": copr, "arch": arch, "source": source}[adapter](case, identity, work)
    observe(case, identity, work, "installed", actual_hash)
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        print(f"guest walkthrough: {error}", file=sys.stderr)
        raise SystemExit(1)
