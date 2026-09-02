#!/usr/bin/env python3
"""Static trust-boundary gate for the GitHub Actions workflows.

`.github/workflows/claude.yml` starts a job holding CLAUDE_CODE_OAUTH_TOKEN on
events anyone can raise against a public repository: opening an issue, leaving
a comment, submitting a review. Text alone must never select that job, and no
sibling job may ride the same trigger ungated. In every workflow (`.yml` or
`.yaml`) that subscribes to one of those actor-authored events, every job must
satisfy, for each of the workflow's `on:` events:

  1. a clause `github.event_name == '<event>'` joined by `&&` to
     `contains(fromJSON('[...]'), github.event.<actor>.author_association)`,
     where `<actor>` is the payload field that carries the association for
     that event; a payload without the field fails closed, because
     `contains(array, null)` is false;
  2. the accepted set inside `fromJSON` is exactly OWNER and COLLABORATOR
     (MEMBER never occurs on a user-owned repository and would admit every
     organisation member after a transfer);
  3. the pull-request-shaped events also require the head repository to be
     this repository; `issue_comment` payloads carry no head repository, so
     a job on that event that runs the action must first run a guard step
     (pinned `if:`, placed before the action step) that reads the PR with
     `github.token` and exits non-zero unless the head is this repository;
  4. `issues:` subscribes to exactly `[opened, assigned]`, so relabelling or
     reopening an old `@claude` issue cannot re-fire it;
  5. the job's `permissions:` (the scope of `github.token`) grant nothing but
     `read`, except `id-token: write`, which the action needs to mint its
     GitHub App token.

A workflow on `pull_request` only, where GitHub already withholds secrets from
fork heads, must still guard every secret-bearing job with the same-repo
conjunct and the same permission ceiling.

Every step using `anthropics/claude-code-action` must pin a 40-hex commit and
may not pass `allowed_non_write_users`, `allowed_bots` or `github_token`, the
inputs that bypass the action's own write-access check. A job that names the
action but whose `steps:` cannot be read in the supported shape (items at
six-space `- `, block-form `with:`) fails closed.

No workflow may use `pull_request_target`.

The YAML is read by indentation, not a YAML library: this runs in a bare
archlinux container with the standard library only. A `|` or `>` value pins a
block scalar whose body (deeper-indented lines) is content: never a comment
to strip, never a key to scan. The supported shape is the one this repository
writes: `on:` and `jobs:` as mappings, both at two-space indentation. A workflow whose text names one of the actor events but yields no
events or no jobs under that reading fails closed as unparseable. A
workflow-level `env:` that references `secrets` (dotted, indexed or through
`toJSON(secrets)`, in any letter case) makes every job in the workflow
secret-bearing.
"""

from __future__ import annotations

import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
WORKFLOW_DIR = ROOT / ".github/workflows"
WORKFLOWS = sorted([*WORKFLOW_DIR.glob("*.yml"), *WORKFLOW_DIR.glob("*.yaml")])
PINNED = ".github/workflows/claude.yml"

ACCEPTED = {"OWNER", "COLLABORATOR"}
ASSOCIATION_FIELD = {
    "issue_comment": "github.event.comment.author_association",
    "pull_request_review_comment": "github.event.comment.author_association",
    "pull_request_review": "github.event.review.author_association",
    "issues": "github.event.issue.author_association",
}
PR_SHAPED = {"pull_request_review", "pull_request_review_comment"}
EVENT_TYPES = {"issues": {"opened", "assigned"}}
SAME_REPO = "github.event.pull_request.head.repo.full_name == github.repository"

ACTION = "anthropics/claude-code-action"
BYPASS_INPUTS = ("allowed_non_write_users", "allowed_bots", "github_token")
FORK_GUARD_IF = "github.event_name == 'issue_comment' && github.event.issue.pull_request"
FORK_GUARD_RUN = ("gh api", "/pulls/", ".head.repo.full_name", "exit 1")

EVENT_CLAUSE = re.compile(r"^github\.event_name == '([a-z_]+)'$")
GATE_CLAUSE = re.compile(
    r"^contains\(fromJSON\('(\[[^']*\])'\),(github\.event\.[a-z_.]+\.author_association)\)$"
)
# The runner matches context and function names case-insensitively.
SECRET_REF = re.compile(r"\bsecrets\s*[.:\[]|toJSON\s*\(\s*secrets\b", re.IGNORECASE)
ACTION_USE = re.compile(rf"^\s*(?:- )?uses:\s*{re.escape(ACTION)}@(\S+)")
STEP_KEY = re.compile(r"^\s+([A-Za-z_][A-Za-z0-9_-]*)\s*:")
BLOCK_SCALAR = re.compile(r"^(\s*)(?:- )?[^\s#][^:#]*:\s*[|>][-+]?[0-9]*\s*(?:#.*)?$")

failures: list[str] = []


def fail(context: str, detail: str) -> None:
    failures.append(f"{context}: {detail}")


# ------------------------------------------------------------------ YAML text


def indent_of(line: str) -> int:
    return len(line) - len(line.lstrip(" "))


class ScalarLine(str):
    """A line inside a `|` or `>` block scalar: content, never structure."""


def content_lines(text: str) -> list[str]:
    """Drop blank lines and full-line comments outside block scalars.

    Lines inside a block scalar come back verbatim as `ScalarLine`, so a
    `#` there is not a comment and a `key:` there is not a key."""
    out: list[str] = []
    scalar_indent: int | None = None
    for line in text.splitlines():
        if scalar_indent is not None:
            if not line.strip():
                continue
            if indent_of(line) > scalar_indent:
                out.append(ScalarLine(line))
                continue
            scalar_indent = None
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        out.append(line)
        opened = BLOCK_SCALAR.match(line)
        if opened:
            scalar_indent = len(opened.group(1))
    return out


def structural(lines: list[str]) -> list[str]:
    return [line for line in lines if not isinstance(line, ScalarLine)]


def mapping(lines: list[str], key: str, indent: int) -> tuple[str | None, list[str]]:
    """The inline value and the deeper-indented body of `key:` at `indent`.

    Returns (None, []) when the key is absent."""
    head = re.compile(rf"^{' ' * indent}{re.escape(key)}:(.*)$")
    for i, line in enumerate(lines):
        m = None if isinstance(line, ScalarLine) else head.match(line)
        if not m:
            continue
        body: list[str] = []
        for later in lines[i + 1 :]:
            if indent_of(later) <= indent:
                break
            body.append(later)
        return m.group(1).strip() or None, body
    return None, []


def keys_at(lines: list[str], indent: int) -> list[str]:
    pattern = re.compile(rf"^{' ' * indent}([A-Za-z_][A-Za-z0-9_-]*):")
    return [m.group(1) for line in structural(lines) if (m := pattern.match(line))]


def scalar(value: str) -> str:
    """An inline value with its trailing comment and quotes removed."""
    value = value.strip()
    if value.startswith("#"):
        return ""
    return value.split(" #", 1)[0].strip().strip("'\"")


def inline_list(value: str) -> set[str]:
    return {scalar(item) for item in scalar(value).strip("[]").split(",") if item.strip()}


def events_of(lines: list[str]) -> set[str]:
    """Events under `on:`: a block mapping, an inline `[a, b]` list or one scalar.

    A flow mapping (`on: { ... }`) is not read; the empty set makes the caller
    fail closed."""
    inline, body = mapping(lines, "on", 0)
    inline = scalar(inline or "")
    if inline.startswith("{"):
        return set()
    if inline.startswith("["):
        return inline_list(inline)
    if inline:
        return {inline}
    return set(keys_at(body, 2))


def event_types(lines: list[str], event: str) -> set[str]:
    """The `types:` list under `on: <event>:`, inline or block form."""
    _, on_body = mapping(lines, "on", 0)
    _, event_body = mapping(on_body, event, 2)
    inline, body = mapping(event_body, "types", 4)
    if inline:
        return inline_list(inline)
    return {m.group(1) for line in body if (m := re.match(r"^\s+- ([a-z_]+)", line))}


def block_text(inline: str | None, body: list[str]) -> str:
    """Flatten a scalar that may be inline, `|`, `>` or `${{ }}`-wrapped."""
    if inline is not None and inline not in ("|", ">", "|-", ">-"):
        text = inline
    else:
        text = " ".join(line.strip() for line in body)
    text = re.sub(r"^\$\{\{(.*)\}\}$", r"\1", text.strip())
    return text


def steps_of(job: list[str]) -> list[list[str]]:
    """The job's `steps:` items, each as its own list of lines."""
    _, body = mapping(job, "steps", 4)
    steps: list[list[str]] = []
    for line in body:
        if not isinstance(line, ScalarLine) and re.match(r"^      - ", line):
            steps.append([line])
        elif steps:
            steps[-1].append(line)
    return steps


# --------------------------------------------------------------- expressions


def split_top(expr: str, op: str) -> list[str]:
    """Split on `op` at parenthesis depth zero, outside string literals."""
    parts: list[str] = []
    depth = 0
    quote: str | None = None
    start = i = 0
    while i < len(expr):
        c = expr[i]
        if quote:
            if c == quote:
                quote = None
        elif c in "'\"":
            quote = c
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        elif depth == 0 and expr.startswith(op, i):
            parts.append(expr[start:i])
            i += len(op)
            start = i
            continue
        i += 1
    parts.append(expr[start:])
    return [p.strip() for p in parts]


def encloses(expr: str) -> bool:
    """True when the first '(' of `expr` closes at its last character."""
    depth = 0
    quote: str | None = None
    for i, c in enumerate(expr):
        if quote:
            if c == quote:
                quote = None
        elif c in "'\"":
            quote = c
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0:
                return i == len(expr) - 1
    return False


def unwrap(expr: str) -> str:
    expr = expr.strip()
    while expr.startswith("(") and expr.endswith(")") and encloses(expr):
        expr = expr[1:-1].strip()
    return expr


def normalize(expr: str) -> str:
    expr = re.sub(r"\s+", " ", expr.strip())
    return re.sub(r"\s*([(),])\s*", r"\1", expr)


def conjuncts(clause: str) -> list[str]:
    return [unwrap(c) for c in split_top(unwrap(clause), "&&")]


def job_condition(context: str, job: list[str], purpose: str) -> str | None:
    inline, body = mapping(job, "if", 4)
    if inline is None and not body:
        fail(context, f"job has no `if:` {purpose}")
        return None
    return normalize(block_text(inline, body))


# --------------------------------------------------------------------- checks


def check_clause(context: str, clause: str, events: set[str]) -> str | None:
    """Validate one top-level `||` alternative; return the event it gates."""
    parts = conjuncts(clause)
    named = [m.group(1) for part in parts if (m := EVENT_CLAUSE.match(part))]
    if any("github.event_name" in part and not EVENT_CLAUSE.match(part) for part in parts):
        fail(context, f"event_name must be tested as a top-level `== '<event>'` conjunct: {clause}")
        return None
    if len(named) != 1:
        fail(context, f"each `||` alternative must name exactly one event as a top-level conjunct: {clause}")
        return None
    event = named[0]
    if event not in events:
        fail(context, f"alternative gates {event!r}, which the workflow does not subscribe to")
        return None
    if event not in ASSOCIATION_FIELD:
        fail(context, f"no known author_association field for event {event!r}; extend ASSOCIATION_FIELD deliberately")
        return None

    gates = [m for part in parts if (m := GATE_CLAUSE.match(part))]
    for part in parts:
        if "author_association" in part and not GATE_CLAUSE.match(part):
            fail(context, f"author_association may only appear inside the accepted-set gate: {part}")
    matched = False
    for m in gates:
        try:
            accepted = json.loads(m.group(1))
        except json.JSONDecodeError:
            fail(context, f"accepted set is not JSON: {m.group(1)}")
            continue
        if not isinstance(accepted, list) or set(accepted) != ACCEPTED or len(accepted) != len(ACCEPTED):
            fail(context, f"accepted set must be exactly {sorted(ACCEPTED)}, got {accepted}")
            continue
        if m.group(2) == ASSOCIATION_FIELD[event]:
            matched = True
        else:
            fail(context, f"{event}: gate reads {m.group(2)}, expected {ASSOCIATION_FIELD[event]}")
    if not matched:
        fail(context, f"{event}: no top-level `&&` conjunct `contains(fromJSON('[...]'), {ASSOCIATION_FIELD[event]})`")
    if event in PR_SHAPED and normalize(SAME_REPO) not in parts:
        fail(context, f"{event}: missing top-level `&&` conjunct `{SAME_REPO}`")
    return event


def check_permissions(context: str, job: list[str]) -> None:
    """`permissions:` scopes github.token: read only, `id-token: write` excepted."""
    inline, body = mapping(job, "permissions", 4)
    if inline is None and not body:
        fail(context, "job declares no `permissions:` block")
        return
    if inline is not None:
        if scalar(inline) == "{}":
            return  # no scopes at all: the most restrictive form
        fail(context, f"permissions must be in block form, not {inline!r}")
        return
    for line in structural(body):
        if indent_of(line) != 6 or ":" not in line:
            continue
        scope, _, value = line.strip().partition(":")
        value = scalar(value)
        if scope == "id-token":
            if value != "write":
                fail(context, f"id-token must be `write` (the action mints its app token from the OIDC token), got {value!r}")
        elif value != "read":
            fail(context, f"permission {scope}: {value!r}; a job on these triggers grants only `read` (id-token excepted)")


def check_action_steps(context: str, job: list[str]) -> None:
    """Every claude-code-action step: 40-hex pin, none of the bypass inputs.

    A job that names the action outside a readable step fails closed."""
    matched = 0
    for step in steps_of(job):
        if not uses_action(step):
            continue
        matched += 1
        ref = uses_action(step)
        if not re.fullmatch(r"[0-9a-f]{40}", ref):
            fail(context, f"{ACTION} must be pinned to a 40-hex commit, not {ref!r}")
        for line in structural(step):
            key = STEP_KEY.match(line)
            if not key:
                continue
            if key.group(1) in BYPASS_INPUTS:
                fail(context, f"input {key.group(1)!r} bypasses the action's own write-access check")
            if key.group(1) == "with" and "{" in line:
                fail(context, "`with:` must be in block form; a flow mapping hides its inputs from this check")
    if not matched and ACTION in "\n".join(structural(job)):
        fail(context, f"steps shape not readable: {ACTION} occurs in the job but no step at six-space `- ` uses it")


def uses_action(step: list[str]) -> str | None:
    """The ref this step pins `ACTION` to, or None when it is another step."""
    return next((m.group(1) for line in structural(step) if (m := ACTION_USE.match(line))), None)


def is_fork_guard(step: list[str]) -> bool:
    inline, body = mapping(step, "if", 8)
    if (inline is None and not body) or normalize(block_text(inline, body)) != normalize(FORK_GUARD_IF):
        return False
    run_inline, run_body = mapping(step, "run", 8)
    run = block_text(run_inline, run_body)
    return all(needle in run for needle in FORK_GUARD_RUN)


def check_fork_guard(context: str, job: list[str]) -> None:
    """On `issue_comment` the payload has no head repository: a job that runs the
    action must first refuse fork heads through the API, before the action step."""
    steps = steps_of(job)
    action_at = next((i for i, step in enumerate(steps) if uses_action(step)), None)
    if action_at is None:
        return
    guard_at = next((i for i, step in enumerate(steps) if is_fork_guard(step)), None)
    if guard_at is None:
        fail(context, f"issue_comment job runs {ACTION} without a fork-head guard step (`if: {FORK_GUARD_IF}`, `gh api .../pulls/<n> --jq .head.repo.full_name`, `exit 1` on a fork)")
    elif guard_at > action_at:
        fail(context, "the fork-head guard step must run before the action step")


def check_actor_job(context: str, job: list[str], events: set[str]) -> None:
    """A job on an actor-authored trigger: per-event author gate plus the ceiling."""
    expression = job_condition(context, job, "gate on an actor-authored trigger")
    if expression is None:
        return
    gated: set[str] = set()
    for clause in split_top(expression, "||"):
        event = check_clause(context, clause, events)
        if event:
            gated.add(event)
    for event in sorted(events - gated):
        fail(context, f"event {event!r} has no gated `||` alternative")
    check_permissions(context, job)


def check_pull_request_job(context: str, job: list[str]) -> None:
    """A secret-bearing job on `pull_request`: same-repo guard plus the ceiling."""
    expression = job_condition(context, job, "guard while carrying a secret on pull_request")
    if expression is None:
        return
    for clause in split_top(expression, "||"):
        if normalize(SAME_REPO) not in conjuncts(clause):
            fail(context, f"every `||` alternative needs the top-level `&&` conjunct `{SAME_REPO}`: {clause}")
    check_permissions(context, job)


def check_workflow(path: Path) -> int:
    """Return the number of jobs held to a gate in this workflow."""
    relative = path.relative_to(ROOT).as_posix()
    lines = content_lines(path.read_text(encoding="utf-8"))
    text = "\n".join(lines)

    if re.search(r"\bpull_request_target\b", text):
        fail(relative, "`pull_request_target` runs untrusted heads with this repository's secrets")

    events = events_of(lines)
    _, jobs = mapping(lines, "jobs", 0)
    names = keys_at(jobs, 2)
    mentioned = sorted(event for event in ASSOCIATION_FIELD if re.search(rf"\b{event}\b", text))
    malformed = sorted(event for event in events if not re.fullmatch(r"[a-z_]+", event))
    if mentioned and (not events or malformed or not names):
        fail(
            relative,
            f"unparseable shape: names {mentioned} but reads as events={sorted(events)} jobs={names}; "
            "only `on:` and `jobs:` mappings at two-space indentation are supported",
        )
        return 0

    actor = bool(events & ASSOCIATION_FIELD.keys())
    if actor:
        for event, wanted in EVENT_TYPES.items():
            if event in events and event_types(lines, event) != wanted:
                fail(relative, f"`{event}:` must subscribe to exactly types {sorted(wanted)}, got {sorted(event_types(lines, event))}")

    env_inline, env_body = mapping(lines, "env", 0)
    workflow_secret = bool(SECRET_REF.search("\n".join([env_inline or "", *env_body])))
    checked = 0
    for name in names:
        _, job = mapping(jobs, name, 2)
        context = f"{relative} job {name!r}"
        check_action_steps(context, job)
        secret = workflow_secret or bool(SECRET_REF.search("\n".join(job)))
        if actor:
            check_actor_job(context, job, events)
            if "issue_comment" in events:
                check_fork_guard(context, job)
            checked += 1
        elif "pull_request" in events and secret:
            check_pull_request_job(context, job)
            checked += 1
    return checked


def main() -> int:
    checked: dict[str, int] = {}
    for path in WORKFLOWS:
        checked[path.relative_to(ROOT).as_posix()] = check_workflow(path)

    if not checked.get(PINNED):
        fail(PINNED, "expected at least one job to pin; the workflow moved or lost its trigger")

    if failures:
        for line in failures:
            print(f"FAIL  {line}")
        print(f"\nworkflow policy: {len(failures)} failure(s)")
        return 1
    print(
        f"workflow policy: OK ({sum(checked.values())} gated job(s) in "
        f"{sum(1 for n in checked.values() if n)} workflow(s); {len(checked)} scanned)"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
