#!/usr/bin/env python3
"""Select affected PR checks and validate every result in the required CI gate."""

import json
import os
from pathlib import PurePosixPath
import re
import subprocess
import sys

SCOPES = ("rust", "transport", "mobile", "relay", "native", "candidate")
JOBS = {"rust": "rust", "windows": "rust", "mobile-core": "mobile", "relay": "relay",
        "candidates-macos": "native", "candidates-linux": "native", "candidates-windows": "native", "candidate": "candidate"}


def documentation(path: str) -> bool:
    return path in {"README.md", "AGENTS.md"} or (
        path.startswith("docs/") and PurePosixPath(path).suffix == ".md"
        and path != "docs/relay.md")


def full_plan(candidate: bool = False) -> dict[str, bool]:
    return {scope: scope != "candidate" or candidate for scope in SCOPES}


def changed_paths(base: str, head: str) -> list[str]:
    if not all(re.fullmatch(r"[0-9a-f]{40}", sha) for sha in (base, head)):
        raise ValueError("missing or invalid exact revisions")
    # Compare the whole tested merge tree, including deletions and both rename ends.
    diff = subprocess.run(
        ["git", "diff", "--no-ext-diff", "--no-renames", "--name-only", "-z", base, head, "--"],
        check=True, capture_output=True).stdout
    return [path.decode("utf-8") for path in diff.split(b"\0") if path]


def path_plan(paths: list[str]) -> dict[str, bool]:
    plan = dict.fromkeys(SCOPES, False)
    if not paths:
        return full_plan()
    for path in paths:
        if documentation(path):
            continue
        if path.startswith(("deploy/relay/", "tests/relay/")) or path == "docs/relay.md":
            plan["relay"] = True
        elif path.startswith(("tools/release/", "tests/release/", "install/", "release/")):
            plan.update(rust=True, native=True)
        elif path.startswith(("crates/", "proto/")):
            plan["rust"] = True
            if path.startswith(("crates/client/", "crates/core/", "crates/proto/",
                                "crates/diagnostics/", "proto/")):
                plan["mobile"] = True
            if path.endswith("Cargo.toml") or path.endswith("build.rs") or path.startswith("crates/core/src/release/"):
                plan["native"] = True
        else:
            # Dependency/toolchain/CI/vendor edits and unknown inputs are broad.
            return full_plan()
    return plan


def select_plan(event: str, ref: str, base: str, head: str, expected: str = "") -> dict[str, bool]:
    if event == "workflow_dispatch":
        if expected and expected != head:
            raise ValueError("main moved before the requested full check started; refusing a different source")
        return full_plan(candidate=ref == "refs/heads/main")
    if event == "push" and ref == "refs/heads/main":
        # PR checks already gate merges. Publication has a separate exact-SHA full run.
        return dict.fromkeys(SCOPES, False)
    if event != "pull_request":
        raise ValueError(f"unsupported CI event/ref: {event}/{ref}")
    try:
        return path_plan(changed_paths(base, head))
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"::warning::Cannot classify the complete PR; running all checks ({error})", file=sys.stderr)
        return full_plan()


def gate_errors(event: str, ref: str, needs: dict) -> list[str]:
    errors = []
    if set(needs) != {"changes", "policy", "dependencies", *JOBS}:
        errors.append("CI job inventory differs from the reviewed gate")
    for job in ("changes", "policy", "dependencies"):
        if needs.get(job, {}).get("result") != "success":
            errors.append(f"{job} must succeed")
    try:
        plan = json.loads(needs["changes"]["outputs"]["plan"])
        if not isinstance(plan, dict) or set(plan) != set(SCOPES) or any(type(value) is not bool for value in plan.values()):
            raise ValueError("invalid scope flags")
    except (KeyError, TypeError, ValueError):
        return [*errors, "missing or invalid check plan"]
    if event == "push":
        if ref != "refs/heads/main" or any(plan.values()):
            errors.append("ordinary main pushes must use the quick plan")
    elif event == "workflow_dispatch":
        if plan != full_plan(candidate=ref == "refs/heads/main"):
            errors.append("manual validation must run the entire graph")
    elif event != "pull_request" or plan["candidate"]:
        errors.append("only explicit full checks on main may assemble candidates")
    if plan["transport"] and not plan["rust"]:
        errors.append("transport tests require the Rust job")
    for job, scope in JOBS.items():
        expected = "success" if plan[scope] else "skipped"
        if needs.get(job, {}).get("result") != expected:
            errors.append(f"{job} must be {expected}")
    return errors


def local_check(base: str) -> None:
    head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    # Include uncommitted and untracked edits; a local check must not miss them.
    paths = changed_paths(base, head)
    for args in (["git", "diff", "--no-renames", "--name-only", "-z", "HEAD", "--"],
                 ["git", "ls-files", "--others", "--exclude-standard", "-z"]):
        paths += [path.decode("utf-8") for path in subprocess.check_output(args).split(b"\0") if path]
    plan = path_plan(paths)
    print("Local check plan: " + json.dumps(plan), flush=True)
    commands = [["sh", "tests/source-policy.sh"], ["just", "ci-policy"], ["just", "ci-dependencies"]]
    if plan["rust"]:
        commands.append(["just", "ci-windows", str(plan["transport"]).lower()] if os.name == "nt" else
                        ["just", "ci-unix", "false", str(plan["transport"]).lower()])
    if plan["relay"]:
        commands.append(["just", "relay-static"])
    for command in commands:
        subprocess.run(command, check=True)
    print("Hosted CI owns the other OS/CPU, mobile targets, Docker and release packaging checks.")


def main() -> None:
    args = sys.argv[1:]
    if args and args[0] == "local" and len(args) <= 2:
        base = subprocess.check_output(["git", "rev-parse", args[1] if len(args) == 2 else "origin/main"], text=True).strip()
        local_check(base)
        return
    event = os.environ["GITHUB_EVENT_NAME"]
    ref = os.environ["GITHUB_REF"]
    if args == ["classify"]:
        plan = select_plan(event, ref, os.environ.get("PR_BASE_SHA", ""), os.environ["GITHUB_SHA"],
                           os.environ.get("EXPECTED_SHA", ""))
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            output.write("plan=" + json.dumps(plan, separators=(",", ":")) + "\n")
            for scope, enabled in plan.items():
                output.write(f"{scope}={str(enabled).lower()}\n")
        print("Check plan: " + json.dumps(plan))
    elif args == ["gate"]:
        needs = json.loads(os.environ["CI_NEEDS"])
        errors = gate_errors(event, ref, needs)
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
            summary.write("## CI check results\n\n")
            for job, result in sorted(needs.items()):
                summary.write(f"- {job}: {result['result']}\n")
            for error in errors:
                summary.write(f"- ERROR: {error}\n")
        if errors:
            sys.exit("; ".join(errors))
    else:
        sys.exit("usage: workflow-policy.py classify|gate|local [base]")


if __name__ == "__main__":
    main()
