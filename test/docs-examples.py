#!/usr/bin/env python3
"""Collect ordered documentation examples without executing shell instructions.

This supports a deliberately bounded shell grammar. Unsupported expressions stay
visible as manual requirements; they never disappear into a successful parse.
"""
from __future__ import annotations

import argparse
from collections import Counter
import hashlib
from html.parser import HTMLParser
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
SHELLS = {"bash", "sh", "shell", "console"}
COMMAND = re.compile(r"^(?:(?:sudo|env)\s+|[A-Z_][A-Z_0-9]*=\S+\s+)*(?:(?:[\w./-]+/)?facelock(?:-bench|-polkit-agent)?|just|cargo|(?:sudo\s+)?(?:apt|apt-get|pacman|yay|paru|dnf)|systemctl|journalctl|busctl|pamtester|curl|git|nix|nixos-rebuild|python3|bash|sh|test/[\w./-]+|scripts/[\w./-]+)(?:\s|$)")
ASSIGN = re.compile(r"^[A-Za-z_][A-Za-z_0-9]*=")
METAVARIABLE = re.compile(r"<[A-Za-z_][\w.|-]*>")


def slug(text):
    return re.sub(r"[^\w\s-]", "", text.lower()).strip().replace(" ", "-")


def roff(text):
    return re.sub(r"\\f(?:\[[^]]*\]|\([^\s]{2}|.)", "", text).replace(r"\-", "-").replace(r"\&", "").replace(r"\e", "\\")


def unwrap(tokens):
    wrapper = []
    while tokens:
        if ASSIGN.match(tokens[0]):
            wrapper.append(tokens.pop(0))
        elif tokens[0] in {"sudo", "env"}:
            tool = tokens.pop(0)
            wrapper.append(tool)
            while tokens and tokens[0].startswith("-"):
                option = tokens.pop(0)
                wrapper.append(option)
                needs_value = {"-u", "-g", "-h", "-p", "-C", "-T", "--user", "--group", "--host", "--prompt", "--close-from", "--command-timeout"} if tool == "sudo" else {"-u", "--unset", "-S", "--split-string"}
                if option in needs_value and tokens:
                    wrapper.append(tokens.pop(0))
        else:
            break
    if tokens and tokens[0] == "cargo" and "run" in tokens and "--bin" in tokens and "--" in tokens:
        bin_at = tokens.index("--bin")
        if bin_at + 1 < len(tokens):
            end = tokens.index("--")
            wrapper.extend(tokens[:end + 1])
            tokens = [tokens[bin_at + 1], *tokens[end + 1:]]
    if tokens and Path(tokens[0]).name in {"facelock", "facelock-bench", "facelock-polkit-agent"}:
        tokens[0] = Path(tokens[0]).name
    return wrapper, tokens


def segments(raw):
    if re.search(r"\$\(|`|<\(|>\(", raw):
        return [], "shell substitution requires a reviewed runtime scenario"
    if re.match(r"\s*(?:if|then|else|elif|fi|for|while|do|done|case|esac|function)\b", raw):
        return [], "shell control flow requires a reviewed runtime scenario"
    # Retain lexical spelling until operators are distinguished from quoted
    # values. shlex alone loses that distinction for an argument such as "|".
    token_pattern = re.compile(r'''\s+|\#[^\n]*|[|&;<>]+|(?:'[^']*'|"(?:\\.|[^"\\])*"|\\.|[^\s|&;<> '"\\])+''')
    tokens = []
    offset = 0
    try:
        while offset < len(raw):
            match = token_pattern.match(raw, offset)
            if not match:
                raise ValueError("unclosed quote or unsupported escape")
            spelling = match[0]
            offset = match.end()
            if spelling.isspace() or spelling.startswith("#"):
                continue
            operator = bool(re.fullmatch(r"[|&;<>]+", spelling))
            tokens.append((spelling if operator else shlex.split(spelling)[0], operator, match.start(), match.end(), spelling))
    except ValueError as error:
        return [], f"shell tokenization: {error}"
    result, current = [], []
    def append(operator):
        if current:
            wrapper, argv = unwrap(current.copy())
            if argv:
                result.append({"wrapper": wrapper, "argv": argv, "operator": operator})
            current.clear()
    i = 0
    while i < len(tokens):
        token, is_operator, start, _, _ = tokens[i]
        if is_operator and token in {"|", "||", "&&", ";", "&"}:
            append(token)
        elif is_operator and token in {">", ">>", "<", "<<", "<<<", ">&", "<&"}:
            if token in {"<<", "<<<"}:
                return [], "here-document/string requires a reviewed runtime scenario"
            if current and i and tokens[i - 1][3] == start and tokens[i - 1][4].isdigit():
                current.pop()
            i += 1  # redirect target is not an argument
        else:
            current.append(token)
        i += 1
    append("")
    return result, None


class CommandsHTML(HTMLParser):
    VOID = {"area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr"}
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.anchor = "preamble"
        self.depth = 0
        self.parts = []
        self.rows = []
        self.start = 1

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if tag in self.VOID:
            if self.depth and tag == "br":
                self.parts.append("\n")
            return
        if attrs.get("id") and not self.depth:
            self.anchor = attrs["id"]
        if self.depth:
            self.depth += 1
        elif "command" in attrs.get("class", "").split() or tag in {"pre", "code"}:
            self.depth = 1
            self.parts = []
            self.start = self.getpos()[0]

    def handle_endtag(self, tag):
        if self.depth:
            self.depth -= 1
            if not self.depth:
                self.rows.extend((self.anchor, self.start + i, line, "executable", None) for i, line in enumerate("".join(self.parts).splitlines()))

    def handle_data(self, data):
        if self.depth:
            self.parts.append(data)


def raw_examples(path, text):
    if path.endswith(".html"):
        parser = CommandsHTML()
        parser.feed(text)
        return parser.rows
    if path.endswith('.toml'):
        return [('configuration-comments', number, line.lstrip()[1:].strip(), 'executable', None)
                for number, line in enumerate(text.splitlines(), 1)
                if line.lstrip().startswith('#') and COMMAND.match(line.lstrip()[1:].strip())]
    rows, anchor, fence, language, annotation = [], "preamble", None, "", None
    block, start = [], 1
    for number, line in enumerate(text.splitlines(), 1):
        if path.endswith((".1", ".8")):
            if line.startswith(".SH "):
                anchor = slug(line[4:].strip('"'))
            if line == ".nf":
                fence, language, start, block = ".fi", "sh", number + 1, []
                continue
            if line == ".fi" and fence:
                rows.extend((anchor, start + i, roff(value), "executable", None) for i, value in enumerate(block) if value.strip() and not value.startswith("."))
                fence = None
                continue
            if fence:
                block.append(line)
            continue
        mark = re.match(r"\s*<!-- docs-example: (negative|manual|historical|schematic) (.+?) -->\s*$", line)
        if mark and not fence:
            annotation = (mark[1], mark[2])
            continue
        edge = re.match(r"\s*(`{3,}|~{3,})(\S*)\s*$", line)
        if edge:
            if fence is None:
                fence, language, block, start = edge[1], edge[2], [], number + 1
            elif edge[1][0] == fence[0] and len(edge[1]) >= len(fence):
                for i, value in enumerate(block):
                    candidate = re.sub(r"^\s*\$\s+", "", value.strip())
                    if language in SHELLS or (not language and (COMMAND.match(candidate) or candidate.endswith("\\"))):
                        kind, reason = annotation or ("executable", None)
                        rows.append((anchor, start + i, candidate, kind, reason))
                fence, annotation = None, None
            continue
        if fence:
            block.append(line)
            continue
        heading = re.match(r"^#{1,6}\s+(.+)", line)
        if heading:
            anchor = slug(heading[1])
        for inline in re.findall(r"`([^`\n]+)`", line):
            if COMMAND.match(inline):
                kind, reason = annotation or ("schematic", "inline invocation or syntax reference; runtime evidence is separate")
                rows.append((anchor, number, inline, kind, reason))
        if line.startswith("    ") and COMMAND.match(line.strip()):
            rows.append((anchor, number, line.strip(), "executable", None))
        if line.strip():
            annotation = None
    return rows


def extract_text(path, text):
    counts, records, pending = Counter(), [], None
    for anchor, line, raw, kind, reason in raw_examples(path, text):
        if not raw.strip() or raw.lstrip().startswith("#"):
            continue
        if pending:
            anchor, line, prefix, kind, reason = pending
            raw = prefix + " " + raw.strip()
            pending = None
        if raw.rstrip().endswith("\\"):
            pending = (anchor, line, raw.rstrip()[:-1], kind, reason)
            continue
        if METAVARIABLE.search(raw) and kind == "executable":
            kind, reason = "schematic", "syntax metavariable requires a concrete value before execution"
        parsed, error = segments(raw)
        if error and kind not in {"schematic", "historical"}:
            kind, reason = "manual", error
        if not parsed and not error:
            # An assignment is still an ordered prerequisite even without argv.
            if not ASSIGN.match(raw) and not raw.startswith("export "):
                continue
        counts[anchor] += 1
        source = {"path": path, "anchor": anchor, "ordinal": counts[anchor], "line": line, "sha256": hashlib.sha256(raw.encode()).hexdigest()}
        row = {"source": source, "classification": kind, "shell": "sh", "raw": raw, "segments": parsed, "scenario_ids": []}
        if reason:
            row["reason"] = reason
        if kind == "negative":
            row["expected_error"] = reason
        records.append(row)
    if pending:
        raise ValueError(f"{path}:{pending[1]}: unfinished command continuation")
    return records


def shell_syntax_errors(path, text):
    """bash -n parses complete shell blocks with startup hooks disabled."""
    errors = []
    env = {k: v for k, v in os.environ.items() if k not in {"BASH_ENV", "ENV", "SHELLOPTS", "BASHOPTS"} and not k.startswith("BASH_FUNC_")}
    pattern = re.compile(r"(?m)^\s*(`{3,}|~{3,})(bash|sh|shell|console)\s*\n(.*?)^\s*\1\s*$", re.S)
    for match in pattern.finditer(text):
        body = re.sub(r"(?m)^\s*\$ ", "", match[3])
        if METAVARIABLE.search(body):
            continue  # These are explicitly inventoried as syntax templates.
        prefix = text[:match.start()].rstrip().splitlines()
        if prefix and re.match(r"<!-- docs-example: (schematic|historical) ", prefix[-1]):
            continue
        check = subprocess.run(["bash", "--noprofile", "--norc", "-n"], input=body, text=True, capture_output=True, env=env, timeout=5)
        if check.returncode:
            line = text[:match.start()].count("\n") + 1
            errors.append(f"{path}:{line}: invalid shell block: {check.stderr.strip()}")
    return errors


def resolve_includes(root, path, stack=()):
    full = (root / path).resolve()
    if not full.is_relative_to(root.resolve()):
        raise ValueError(f"include escapes repository: {path}")
    if full in stack:
        raise ValueError(f"include cycle: {path}")
    text = full.read_text()
    for match in re.finditer(r"\{\{#include\s+([^}]+)\}\}", text):
        target = match[1].strip().split(":")[0]
        resolve_includes(root, str(full.parent / target), (*stack, full))
    return text


def collect(root=ROOT):
    spec = importlib.util.spec_from_file_location("docs_inventory", root / "test/docs-inventory.py")
    inventory = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(inventory)
    corpus = json.loads((root / "test/docs-corpus.json").read_text())
    paths = inventory.instructional_files(root)
    errors = inventory.check_corpus(paths, corpus)
    records = []
    for path in paths:
        try:
            text = resolve_includes(root, path)
            found = extract_text(path, text)
        except (ValueError, OSError) as error:
            errors.append(str(error))
            continue
        role = corpus.get("files", {}).get(path, {}).get("role")
        if role == "historical":
            for row in found:
                row["classification"] = "historical"
                row["reason"] = corpus["files"][path]["reason"]
        else:
            errors.extend(shell_syntax_errors(path, text))
        records.extend(found)
    return {"schema_version": 1, "occurrences": records, "errors": errors}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--root", type=Path, default=ROOT)
    args = parser.parse_args()
    report = collect(args.root)
    if args.check:
        spec = importlib.util.spec_from_file_location("docs_inventory", args.root / "test/docs-inventory.py")
        inventory = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(inventory)
        binaries, recipes = inventory.metadata(args.root)
        targets = {binary["name"] for binary in binaries}
        for row in report["occurrences"]:
            if row["classification"] not in {"executable", "schematic"}:
                continue
            for part in row["segments"]:
                for argv in (part["argv"], part["wrapper"]):
                    error = inventory.check_entrypoint(argv, targets, recipes, schematic=row["classification"] == "schematic")
                    if error:
                        report["errors"].append(f"{row['source']['path']}:{row['source']['line']}: {error}")
    if args.json or not args.check:
        print(json.dumps(report, indent=2))
    else:
        for error in report["errors"]:
            print(error, file=sys.stderr)
        counts = Counter(r["classification"] for r in report["occurrences"])
        print(f"documentation examples: {len(report['occurrences'])} occurrences; {dict(sorted(counts.items()))}")
        print("Parser coverage is not execution evidence; manual/hardware requirements remain in the walkthrough report")
    return bool(report["errors"])


if __name__ == "__main__":
    raise SystemExit(main())
