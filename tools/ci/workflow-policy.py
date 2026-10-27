#!/usr/bin/env python3
"""Conservative PR path classification and the stable required CI gate."""

import json
import os
from pathlib import PurePosixPath
import re
import subprocess
import sys


HEAVY_JOBS = ("rust", "mobile-core", "relay", "candidates-macos", "candidates-linux")


def documentation_only(paths: list[str]) -> bool:
    # docs/relay.md is an executable contract read by tests/relay/static.sh.
    # Keep build inputs, vendor README files, and unknown paths on the full path.
    return bool(paths) and all(
        path in {"README.md", "AGENTS.md"}
        or (
            path.startswith("docs/")
            and PurePosixPath(path).suffix == ".md"
            and path != "docs/relay.md"
        )
        for path in paths
    )


def full_checks(event: str, base: str, head: str) -> bool:
    if event != "pull_request":
        return True
    try:
        if not all(re.fullmatch(r"[0-9a-f]{40}", sha) for sha in (base, head)):
            raise ValueError("missing or invalid exact PR revisions")
        # Compare the tested merge tree against its base, not just the last
        # commit. --no-renames includes both ends of a source -> docs rename.
        diff = subprocess.run(
            ["git", "diff", "--no-ext-diff", "--no-renames", "--name-only", "-z", base, head, "--"],
            check=True, capture_output=True,
        ).stdout
        paths = [path.decode("utf-8") for path in diff.split(b"\0") if path]
        return not documentation_only(paths)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"::warning::Cannot prove a documentation-only PR; running all checks ({error})")
        return True


def gate_errors(event: str, ref: str, needs: dict) -> list[str]:
    errors = []
    for job in ("changes", "policy", "dependencies"):
        if needs.get(job, {}).get("result") != "success":
            errors.append(f"{job} must succeed")
    mode = needs.get("changes", {}).get("outputs", {}).get("full")
    if mode not in {"true", "false"}:
        errors.append("change classification must explicitly report true or false")
    docs_only = event == "pull_request" and mode == "false"
    if event != "pull_request" and mode != "true":
        errors.append("only pull requests may skip heavy checks")
    expected = "skipped" if docs_only else "success"
    for job in HEAVY_JOBS:
        if needs.get(job, {}).get("result") != expected:
            errors.append(f"{job} must be {expected}")
    candidate = "success" if event == "push" and ref == "refs/heads/main" else "skipped"
    if needs.get("candidate", {}).get("result") != candidate:
        errors.append(f"candidate assembly must be {candidate}")
    return errors


def main() -> None:
    event = os.environ["GITHUB_EVENT_NAME"]
    if sys.argv[1:] == ["classify"]:
        full = full_checks(event, os.environ.get("PR_BASE_SHA", ""), os.environ["GITHUB_SHA"])
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            output.write(f"full={str(full).lower()}\n")
        print("Full CI required" if full else "Documentation-only PR: policy and dependency checks required")
    elif sys.argv[1:] == ["gate"]:
        needs = json.loads(os.environ["CI_NEEDS"])
        errors = gate_errors(event, os.environ["GITHUB_REF"], needs)
        with open(os.environ["GITHUB_STEP_SUMMARY"], "a", encoding="utf-8") as summary:
            summary.write("## CI ownership summary\n\n")
            for job, result in sorted(needs.items()):
                summary.write(f"- {job}: {result['result']}\n")
            for error in errors:
                summary.write(f"- ERROR: {error}\n")
        if errors:
            sys.exit("; ".join(errors))
    else:
        sys.exit("usage: workflow-policy.py classify|gate")


if __name__ == "__main__":
    main()
