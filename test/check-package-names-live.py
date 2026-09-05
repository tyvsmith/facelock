#!/usr/bin/env python3
"""Revalidate documented package names against live repositories (#209, #211).

The offline half of this guard lives in
`crates/facelock-cli/src/conformance/packages.rs`: every package name a
document tells a reader to install must be declared by the packaging manifest
or the narrow, reviewed external-tool allowlist for that distro. That check
needs no network, which is why it can run under `just check` and in CI -- but it proves only that prose and
packaging agree. If the manifest names a package that does not exist, both are
wrong together and the offline check still passes.

This script closes that gap by asking the repositories. It is deliberately
*not* part of `just check`: it needs the network, and a repository outage would
turn an unrelated pull request red.

    just check-package-names-live

Queries, all read-only repository endpoints:

  Arch    https://archlinux.org/packages/search/json/
  AUR     https://aur.archlinux.org/rpc/v5/info
  Debian  https://deb.debian.org/debian/dists/trixie/main/binary-amd64/Packages.xz
  Fedora  https://mdapi.fedoraproject.org

Exit status is 0 when every name resolves, 1 when one does not, and 2 when a
lookup could not be completed (network, rate limit, schema change) -- an
unreachable repository is not evidence that a package is missing.

The Debian lookup checks binary-name presence in Trixie main for amd64 (which
also includes architecture-independent packages), not source-package aliases.
It does not validate package versions, backports selection, or Ubuntu support.
"""

from __future__ import annotations

import http.client
import json
import lzma
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from functools import lru_cache

ROOT = Path(__file__).resolve().parent.parent
TIMEOUT = 20
USER_AGENT = "facelock-package-name-check (+https://github.com/tyvsmith/facelock)"

# Names the project publishes itself. No third-party repository carries them,
# so a "not found" here means the release has not been published yet -- which
# is a release question, not a documentation defect.
SELF_PUBLISHED = {
    "facelock",
    "facelock-bin",
    "facelock-git",
}


class RepositoryUnavailable(Exception):
    """A repository could not answer. Never treated as "package missing"."""


def fetch_json(url: str) -> object:
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
            return json.loads(response.read().decode("utf-8"))
    except (urllib.error.URLError, TimeoutError, json.JSONDecodeError) as exc:
        raise RepositoryUnavailable(f"{url}: {exc}") from exc


def in_arch_repos(name: str) -> bool:
    query = urllib.parse.urlencode({"name": name})
    payload = fetch_json(f"https://archlinux.org/packages/search/json/?{query}")
    if not isinstance(payload, dict):
        raise RepositoryUnavailable("archlinux.org returned an unexpected shape")
    return bool(payload.get("results"))


def in_aur(name: str) -> bool:
    query = urllib.parse.urlencode({"arg[]": name})
    payload = fetch_json(f"https://aur.archlinux.org/rpc/v5/info?{query}")
    if not isinstance(payload, dict):
        raise RepositoryUnavailable("aur.archlinux.org returned an unexpected shape")
    if payload.get("type") == "error":
        raise RepositoryUnavailable(f"AUR RPC error: {payload.get('error')}")
    return bool(payload.get("results"))


@lru_cache(maxsize=1)
def debian_binary_names() -> frozenset[str]:
    # Fetch once per invocation, not once per documented name. A missing or
    # malformed *index* is unavailable, never proof that a package is absent.
    url = "https://deb.debian.org/debian/dists/trixie/main/binary-amd64/Packages.xz"
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
            text = lzma.decompress(response.read()).decode("utf-8")
    except (OSError, EOFError, http.client.HTTPException, lzma.LZMAError, UnicodeError) as exc:
        raise RepositoryUnavailable(f"{url}: {exc}") from exc

    names: set[str] = set()
    # Parse all control stanzas before answering, so even corruption after a
    # matching Package field cannot turn a broken response into a verdict.
    for stanza in re.split(r"\n[ \t]*\n", text.strip()):
        fields: dict[str, str] = {}
        for line in stanza.splitlines():
            if line.startswith((" ", "\t")) and fields:
                continue
            field = re.fullmatch(r"([A-Za-z0-9][A-Za-z0-9-]*):[ \t]*(.*)", line)
            if field is None or field[1].lower() in fields:
                raise RepositoryUnavailable(f"{url}: malformed binary-package stanza")
            fields[field[1].lower()] = field[2]
        name = fields.get("package", "")
        if (
            re.fullmatch(r"[a-z0-9][a-z0-9.+-]+", name) is None
            or not fields.get("version")
            or fields.get("architecture") not in ("amd64", "all")
        ):
            raise RepositoryUnavailable(f"{url}: incomplete or invalid binary-package stanza")
        names.add(name)
    if not names:
        raise RepositoryUnavailable(f"{url}: empty binary-package index")
    return frozenset(names)


def in_debian(name: str) -> bool:
    return name in debian_binary_names()


def in_fedora(name: str) -> bool:
    try:
        payload = fetch_json(
            f"https://mdapi.fedoraproject.org/rawhide/pkg/{urllib.parse.quote(name)}"
        )
    except RepositoryUnavailable as exc:
        # mdapi answers 404 for an unknown package, which urllib raises.
        if "404" in str(exc):
            return False
        raise
    return isinstance(payload, dict) and "basename" in payload


RESOLVERS = {
    # Arch documentation points at both official repositories and the AUR;
    # either can establish name existence (not which repository provides it).
    "Arch": lambda name: in_arch_repos(name) or in_aur(name),
    "Debian": in_debian,
    "Fedora": in_fedora,
}


def documented_names() -> dict[str, set[str]]:
    """Every package name the documents hand a reader, by distro.

    Deliberately a second implementation of the Rust guard's extractor rather
    than a shared one: that guard checks prose against the manifests, this one
    checks names against reality, and the two answer to different owners. If
    the extractors disagree, the difference surfaces as a name checked in one
    place and not the other, which is the safe direction to fail.
    """
    found: dict[str, set[str]] = {distro: set() for distro in RESOLVERS}

    verbs = {
        "pacman -S ": "Arch",
        "pacman -Syu ": "Arch",
        "yay -S ": "Arch",
        "paru -S ": "Arch",
        "apt-get install ": "Debian",
        "apt install ": "Debian",
        "dnf install ": "Fedora",
        "yum install ": "Fedora",
    }

    corpus = [
        ROOT / "README.md",
        ROOT / "AGENTS.md",
        ROOT / "config" / "facelock.toml",
        ROOT / "website" / "index.html",
    ]
    corpus += sorted((ROOT / "docs").rglob("*.md"))
    corpus += sorted((ROOT / "book" / "src").glob("*.md"))

    for path in corpus:
        text = path.read_text(encoding="utf-8")
        if path.suffix == ".html":
            text = re.sub(r"<[^>]*>", "", text).replace("&amp;", "&").replace("&nbsp;", " ")
        for line in text.splitlines():
            for verb, distro in verbs.items():
                start = 0
                while (at := line.find(verb, start)) != -1:
                    start = at + len(verb)
                    tokens = iter(line[start:].split())
                    for token in tokens:
                        if token.startswith("#"):
                            break
                        if token.startswith("-"):
                            if token in ("-t", "--target-release"):
                                next(tokens, None)
                            continue
                        name = token.rstrip("`.,)\"';")
                        if not re.fullmatch(r"[a-z0-9][a-z0-9._+-]*", name):
                            break
                        found[distro].add(name)
                        if name != token:
                            break

    # The GPU tables name the package in a column rather than in a command.
    for path in (ROOT / "README.md", ROOT / "book" / "src" / "gpu.md"):
        for row in path.read_text(encoding="utf-8").splitlines():
            if not row.strip().startswith("|"):
                continue
            for cell in row.strip().strip("|").split("|"):
                cell = cell.strip()
                if len(cell) > 2 and cell[0] == cell[-1] == "`":
                    name = cell[1:-1]
                    if re.fullmatch(r"[a-z0-9][a-z0-9._+-]*", name) and "onnxruntime" in name:
                        found["Arch"].add(name)

    return found


def main() -> int:
    names = documented_names()
    total = sum(len(v) for v in names.values())
    if total == 0:
        print("no package names extracted -- the extractor is broken", file=sys.stderr)
        return 2

    missing: list[str] = []
    unresolved: list[str] = []

    for distro in sorted(names):
        for name in sorted(names[distro]):
            if name in SELF_PUBLISHED:
                print(f"  skip  {distro:7} {name}  (published by this project)")
                continue
            try:
                exists = RESOLVERS[distro](name)
            except RepositoryUnavailable as exc:
                unresolved.append(f"{distro} {name}: {exc}")
                print(f"  ????  {distro:7} {name}")
                continue
            print(f"  {'ok  ' if exists else 'GONE'}  {distro:7} {name}")
            if not exists:
                missing.append(f"{distro} {name}")

    if missing:
        print("", file=sys.stderr)
        print("package names no repository carries:", file=sys.stderr)
        for entry in missing:
            print(f"  {entry}", file=sys.stderr)
        print(
            "\nremove the instruction, or correct it to a name that exists.",
            file=sys.stderr,
        )
        return 1

    if unresolved:
        print("", file=sys.stderr)
        print("lookups that did not complete (not a verdict):", file=sys.stderr)
        for entry in unresolved:
            print(f"  {entry}", file=sys.stderr)
        return 2

    print(f"\n{total} documented package names resolve")
    return 0


if __name__ == "__main__":
    sys.exit(main())
