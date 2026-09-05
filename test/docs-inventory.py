#!/usr/bin/env python3
"""Derive documentation scope and developer entrypoints; never run recipes."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
ROLES = {"current", "canonical", "summary", "internal", "historical", "generated"}


def command_json(argv, root=ROOT):
    return json.loads(subprocess.run(argv, cwd=root, check=True, capture_output=True, text=True).stdout)


def instructional_files(root=ROOT):
    if (root / ".git").exists():
        paths = subprocess.run(["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"], cwd=root, check=True, capture_output=True).stdout.decode().split("\0")
    else:
        # Distribution source archives have no Git metadata. Discover rather
        # than trust the corpus list, so newly added instructions still fail
        # closed. Only known build/worktree outputs are excluded here.
        paths = []
        ignored = {".git", ".worktrees", "target", "__pycache__", "node_modules", ".venv"}
        ignored_paths = {"book/book", "docs/superpowers", ".claude/worktrees"}
        for directory, dirs, files in os.walk(root, followlinks=False):
            relative = Path(directory).relative_to(root)
            dirs[:] = [d for d in dirs if d not in ignored and (relative / d).as_posix() not in ignored_paths and not (Path(directory) / d).is_symlink()]
            paths.extend((relative / name).as_posix() for name in files)
    return sorted({p for p in paths if p and (Path(p).suffix in {".md", ".html", ".1", ".8"} or p in {"config/facelock.toml", "dev/config.toml"})})


def check_corpus(paths, corpus):
    entries = corpus.get("files", {})
    errors = []
    for path in paths:
        if path not in entries:
            errors.append(f"unclassified instructional file: {path}")
        elif entries[path].get("role") not in ROLES or not entries[path].get("reason"):
            errors.append(f"{path}: require a valid role and reason")
    for path in entries.keys() - set(paths):
        errors.append(f"stale corpus entry: {path}")
    return errors


def metadata(root=ROOT):
    cargo = command_json(["cargo", "metadata", "--locked", "--offline", "--no-deps", "--format-version", "1"], root)
    binaries = sorted([{"name": t["name"], "crate": p["name"]} for p in cargo["packages"] for t in p["targets"] if "bin" in t["kind"]], key=lambda b: b["name"])
    just = command_json(["just", "--dump", "--dump-format", "json"], root)
    return binaries, just["recipes"]


def check_just_argv(argv, recipes):
    """Validate a single documented recipe; global inspection flags are harmless."""
    args = argv[1:]
    if not args or args[0] in {"--help", "--list", "--dump", "--version", "--summary"}:
        return None
    while args and "=" in args[0] and not args[0].startswith("-"):
        args = args[1:]
    if not args:
        return None
    name, *values = args
    if name not in recipes:
        return f"unknown just recipe or unsupported global option: {name}"
    parameters = recipes[name].get("parameters", [])
    required = sum(p.get("default") is None and p.get("kind") != "star" for p in parameters)
    variadic = any(p.get("kind") in {"plus", "star"} for p in parameters)
    if len(values) < required or (not variadic and len(values) > len(parameters)):
        return f"just {name}: expected {required}..{'many' if variadic else len(parameters)} arguments, got {len(values)}"
    return None


def check_entrypoint(argv, targets, recipes, schematic=False):
    """Check concrete identities even when an inline synopsis omits values."""
    if not argv:
        return None
    if argv[0] == "just":
        if not schematic:
            return check_just_argv(argv, recipes)
        args = argv[1:]
        while args and "=" in args[0] and not args[0].startswith("-"):
            args = args[1:]
        if args and not args[0].startswith(("-", "<", "[", "$")) and args[0] not in recipes:
            return f"unknown just recipe: {args[0]}"
    if argv[0] == "cargo":
        # This also accepts a cargo-run wrapper terminating at `--`.
        args = argv[1:argv.index("--")] if "--" in argv else argv[1:]
        for index, token in enumerate(args):
            target = token.split("=", 1)[1] if token.startswith("--bin=") else args[index + 1] if token == "--bin" and index + 1 < len(args) else None
            if token == "--bin" and target is None:
                return "Cargo --bin requires a target"
            if target is not None and not target.startswith(("<", "[", "$")) and target not in targets:
                return f"unknown Cargo binary: {target}"
    return None


def render_recipes(recipes, binaries):
    lines = ["# Developer Commands", "", "This index is derived from Cargo targets and the public justfile metadata. Regenerate", "it with `python3 test/docs-inventory.py --write`; `just check-docs` detects drift.", "", "Run recipes from a repository checkout. Recipes can build, download, install, remove,", "or publish state: inspect `just --show RECIPE` and read the linked guide before using", "one. An entry here records an interface, not evidence that a release or hardware test ran.", "", "## Executables", "", "| Executable | Crate | Reference |", "|---|---|---|"]
    for binary in binaries:
        target = "cli.md" if binary["name"] == "facelock" else "auxiliary-commands.md"
        lines.append(f"| `{binary['name']}` | `{binary['crate']}` | [Reference]({target}) |")
    lines += ["", "The PAM module is a shared library, not a command: see [contracts](contracts.md#binaries).", "", "## Prerequisites and effects", "", "- Build/test/lint recipes need the [development dependencies](quickstart.md)", "- Package/container recipes need Podman, their declared images and build tools; [testing safety](testing-safety.md) explains the tiers", "- Camera/TPM/GPU recipes need the named devices/models; skipped hardware is not verification", "- Install/uninstall recipes change system files through sudo; use a disposable guest for testing", "- Release recipes may change versions or publish externally; follow [releasing](releasing.md) and inspect the recipe before invocation", "- Documentation checks inspect examples; [walkthroughs](testing-walkthrough.md) establish actual clean-system results", "", "## Public recipes", "", "Arguments in square brackets are optional; defaults are shown. This is a syntax", "index, so substitute real values for metavariables before running a recipe.", "", "| Invocation | Description |", "|---|---|"]
    for name, recipe in sorted(recipes.items()):
        if recipe.get("private"):
            continue
        params = []
        for p in recipe.get("parameters", []):
            value = f"<{p['name']}>" if p.get("default") is None else f"[{p['name']}={p['default']!s}]"
            params.append(value.replace("|", "\\|"))
        invocation = " ".join(["just", name, *params])
        description = (recipe.get("doc") or "See the recipe body and its prerequisites").replace("\n", " ").replace("|", "\\|")
        lines.append(f"| `{invocation}` | {description} |")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="fail on corpus/index drift")
    parser.add_argument("--write", action="store_true", help="regenerate developer index only")
    args = parser.parse_args()
    corpus = json.loads((ROOT / "test/docs-corpus.json").read_text())
    paths = instructional_files()
    binaries, recipes = metadata()
    errors = check_corpus(paths, corpus)
    for binary in binaries:
        destination = corpus.get("binaries", {}).get(binary["name"])
        if not destination or not (ROOT / destination.split("#")[0]).is_file():
            errors.append(f"undocumented executable: {binary['name']}")
    for name in corpus.get("binaries", {}):
        if name not in {b["name"] for b in binaries}:
            errors.append(f"stale executable: {name}")
    index = ROOT / "docs/developer-commands.md"
    expected = render_recipes(recipes, binaries)
    if args.write:
        index.write_text(expected)
    elif args.check and (not index.exists() or index.read_text() != expected):
        errors.append("developer command index drift: run python3 test/docs-inventory.py --write")
    if args.check:
        for error in errors:
            print(error, file=sys.stderr)
        if not errors:
            print(f"documentation inventory: {len(paths)} files, {len(binaries)} executables, {sum(not r.get('private') for r in recipes.values())} public recipes")
        return bool(errors)
    if not args.write:
        print(json.dumps({"schema_version": 1, "files": paths, "binaries": binaries, "recipes": [{"name": n, "parameters": r.get("parameters", [])} for n, r in sorted(recipes.items()) if not r.get("private")], "errors": errors}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
