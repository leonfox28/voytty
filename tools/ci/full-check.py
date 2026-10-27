#!/usr/bin/env python3
"""Run or rejoin full validation for one exact main commit; never publish or retry tests."""

import argparse
import json
import re
import subprocess
import sys
import time


def output(*args: str) -> str:
    return subprocess.check_output(args, text=True).strip()


def matching_runs(records: list[dict], commit: str) -> list[dict]:
    return [run for run in records if run.get("headSha") == commit
            and run.get("headBranch") == "main" and run.get("event") == "workflow_dispatch"]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--commit", help="Expected local/main source; defaults to HEAD")
    parser.add_argument("--reuse", action="store_true", help="Rejoin an existing exact full run instead of creating another")
    args = parser.parse_args()
    commit = args.commit or output("git", "rev-parse", "HEAD")
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("expected a complete lowercase source SHA")
    if output("git", "rev-parse", "HEAD") != commit or output("git", "status", "--porcelain", "--untracked-files=all"):
        raise ValueError("full validation requires the clean, exact local source")
    repository = output("gh", "repo", "view", "--json", "nameWithOwner", "--jq", ".nameWithOwner")

    def require_main():
        remote = output("gh", "api", f"repos/{repository}/branches/main", "--jq", ".commit.sha")
        if remote != commit:
            raise ValueError("main differs from the requested source; update/review the checkout before dispatch")

    def runs():
        records = json.loads(output("gh", "run", "list", "--repo", repository, "--workflow", "ci.yml",
                                    "--branch", "main", "--commit", commit, "--event", "workflow_dispatch",
                                    "--limit", "100", "--json", "databaseId,url,headSha,headBranch,event,status,conclusion"))
        return matching_runs(records, commit)

    require_main()
    existing = runs()
    run = existing[0] if args.reuse and existing else None
    if run is None:
        previous = {item["databaseId"] for item in existing}
        subprocess.run(["gh", "workflow", "run", "ci.yml", "--repo", repository, "--ref", "main",
                        "-f", f"expected_sha={commit}"], check=True, stdout=sys.stderr)
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            new = [item for item in runs() if item["databaseId"] not in previous]
            if new:
                run = new[0]
                break
            require_main()
            time.sleep(2)
        if run is None:
            raise ValueError("dispatch result is not yet discoverable; inspect Actions before requesting another run")
    if run["status"] == "completed" and run["conclusion"] != "success":
        raise ValueError(f"full validation failed: {run['url']}; inspect the failure, no automatic retry was started")
    print(f"Exact full validation: {run['url']}", file=sys.stderr, flush=True)
    subprocess.run(["gh", "run", "watch", str(run["databaseId"]), "--repo", repository,
                    "--exit-status", "--compact", "--interval", "15"], check=True, stdout=sys.stderr)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(str(error))
