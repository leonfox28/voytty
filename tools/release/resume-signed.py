#!/usr/bin/env python3
"""Resume a signed desktop inventory after a Windows installer fixture failure."""

import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess


SPEC = importlib.util.spec_from_file_location(
    "draft_recovery", Path(__file__).with_name("recover-draft.py"))
draft = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(draft)
require = draft.require
api = draft.api


def validate_evidence(selection, records):
    tag, run_id, artifact_id = (selection[key] for key in ("tag", "run_id", "artifact_id"))
    run, reference, annotated, artifact, jobs = (
        records[key] for key in ("run", "reference", "annotated", "artifact", "jobs"))
    commit = run["head_sha"]
    require(re.fullmatch(r"[0-9a-f]{40}", commit), "invalid original source")
    require(run["id"] == run_id and run["event"] == "push" and run["head_branch"] == tag
            and run["path"] == ".github/workflows/release.yml"
            and run["status"] == "completed" and run["conclusion"] in ("failure", "timed_out", "cancelled"),
            "selection is not a failed original tag release")
    require(reference["ref"] == f"refs/tags/{tag}" and reference["object"]["type"] == "tag"
            and annotated["sha"] == reference["object"]["sha"]
            and annotated["object"]["type"] == "commit"
            and annotated["object"]["sha"] == commit, "original annotated tag changed")
    for name in draft.REQUIRED_JOBS:
        matching = [job for job in jobs if job["name"] == name]
        require(len(matching) == 1 and matching[0]["status"] == "completed"
                and matching[0]["conclusion"] == "success", f"missing original evidence: {name}")
    failed_windows = False
    for name in draft.WINDOWS_INSTALLER_JOBS:
        matching = [job for job in jobs if job["name"] == name]
        require(len(matching) == 1 and matching[0]["status"] == "completed"
                and matching[0]["conclusion"] in ("success", "failure", "timed_out", "cancelled"),
                f"missing terminal Windows installer evidence: {name}")
        if matching[0]["conclusion"] == "cancelled":
            require(any(annotation["annotation_level"] == "failure" and annotation["message"].startswith(
                "The job has exceeded the maximum execution time of ")
                for annotation in records["annotations"].get(name, [])),
                "manual cancellation is not a Windows fixture timeout")
        failed_windows |= matching[0]["conclusion"] != "success"
    require(failed_windows, "no failed Windows installer to repair")
    installers = {job["name"] for job in jobs if job["name"].startswith("Install ")}
    require(installers == (draft.REQUIRED_JOBS | draft.WINDOWS_INSTALLER_JOBS)
            - {"Validate exact immutable release source", "Approve and sign exact manifest bytes"},
            "unexpected original installer matrix")
    publish = [job for job in jobs if job["name"] == draft.PUBLISH_JOB]
    require(len(publish) == 1 and publish[0]["status"] == "completed"
            and publish[0]["conclusion"] == "skipped", "original publication was already attempted")
    owner = artifact["workflow_run"]
    require(artifact["id"] == artifact_id and artifact["expired"] is False
            and owner["id"] == run_id and owner["head_sha"] == commit
            and owner["head_branch"] == tag
            and re.fullmatch(rf"signed-release-{re.escape(tag)}-[1-9][0-9]*", artifact["name"])
            and re.fullmatch(r"sha256:[0-9a-f]{64}", artifact.get("digest") or ""),
            "signed artifact is expired, unbound or unauthenticated")
    return {**selection, "source_commit": commit, "tag_object": annotated["sha"],
            "artifact_digest": artifact["digest"]}


def inspect(selection):
    tag = selection["tag"]
    require(re.fullmatch(r"v[0-9][A-Za-z0-9.-]*", tag), "invalid tag")
    reference = api(f"git/ref/tags/{tag}")
    require(reference["object"]["type"] == "tag", "tag must be annotated")
    jobs = [job for page in api(
        f"actions/runs/{selection['run_id']}/jobs?filter=latest&per_page=100", pages=True)
        for job in page["jobs"]]
    annotations = {
        job["name"]: [annotation for page in api(
            f"check-runs/{job['id']}/annotations?per_page=100", pages=True) for annotation in page]
        for job in jobs if job["name"] in draft.WINDOWS_INSTALLER_JOBS and job["conclusion"] == "cancelled"
    }
    return validate_evidence(selection, {
        "reference": reference,
        "annotated": api(f"git/tags/{reference['object']['sha']}"),
        "run": api(f"actions/runs/{selection['run_id']}"),
        "artifact": api(f"actions/artifacts/{selection['artifact_id']}"),
        "jobs": jobs,
        "annotations": annotations,
    })


def check_evidence(plan):
    current = inspect({key: plan[key] for key in ("tag", "run_id", "artifact_id")})
    require(all(plan[key] == value for key, value in current.items()),
            "original release evidence changed")


def file_inventory(plan, directory):
    files = list(directory.iterdir())
    require(files and all(path.is_file() and not path.is_symlink() for path in files),
            "signed inventory contains a non-regular file")
    inventory = []
    for path in files:
        require(re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", path.name), "unsafe signed filename")
        with path.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        inventory.append({"name": path.name, "size": path.stat().st_size, "digest": f"sha256:{digest}"})
    manifest = json.loads((directory / "voytty-release.json").read_text())
    require(manifest["tag"] == plan["tag"] and manifest["source_commit"] == plan["source_commit"]
            and manifest["classification"] in ("stable", "prerelease"), "signed manifest identity mismatch")
    return sorted(inventory, key=lambda asset: asset["name"]), manifest["classification"] == "prerelease"


def matching_release(plan):
    # Draft-by-tag can return 404. Enumerate all releases, including drafts.
    releases = [release for page in api("releases?per_page=100", pages=True) for release in page
                if release["tag_name"] == plan["tag"]]
    require(len(releases) <= 1, "ambiguous release identity")
    return releases[0] if releases else None


def bind_draft(plan, signed, release):
    inventory, prerelease = file_inventory(plan, signed)
    require(release["tag_name"] == plan["tag"] and release["target_commitish"] == plan["source_commit"]
            and release["prerelease"] is prerelease and release["draft"] is True
            and release["immutable"] is False, "existing release is not the exact unpublished draft")
    assets = draft.assets(release)
    require([{key: asset[key] for key in ("name", "size", "digest")} for asset in assets] == inventory,
            "existing draft is incomplete or differs from the retained signed inventory")
    return {**plan, "release_id": release["id"], "prerelease": prerelease, "assets": assets}


def stage(plan, signed):
    check_evidence(plan)
    inventory, prerelease = file_inventory(plan, signed)
    release = matching_release(plan)
    if release is None:
        command = ["gh", "release", "create", plan["tag"], "--repo", draft.REPOSITORY,
                   "--draft", "--verify-tag", "--target", plan["source_commit"],
                   "--title", f"voytty {plan['tag']}", "--generate-notes"]
        if prerelease:
            command.append("--prerelease")
        subprocess.run(command, check=True, timeout=120)
        subprocess.run(["gh", "release", "upload", plan["tag"], "--repo", draft.REPOSITORY,
                        *(str(signed / asset["name"]) for asset in inventory)], check=True, timeout=300)
        release = matching_release(plan)
        require(release is not None, "created draft could not be reconciled")
    result = bind_draft(plan, signed, release)
    check_evidence(plan)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    prepare = commands.add_parser("inspect")
    prepare.add_argument("--tag", required=True)
    for name in ("run-id", "artifact-id"):
        prepare.add_argument(f"--{name}", required=True, type=draft.positive_id)
    prepare.add_argument("--plan", required=True, type=Path)
    for name in ("check", "stage", "publish"):
        command = commands.add_parser(name)
        command.add_argument("--plan", required=True, type=Path)
        if name != "check":
            command.add_argument("--signed", required=True, type=Path)
        if name == "publish":
            command.add_argument("--roundtrip", required=True, type=Path)
    args = parser.parse_args()
    if args.command == "inspect":
        plan = inspect({key: getattr(args, key) for key in ("tag", "run_id", "artifact_id")})
        args.plan.write_text(json.dumps(plan, indent=2) + "\n")
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                output.write(f"source_commit={plan['source_commit']}\n")
        print(f"Bound retained signed {plan['tag']} to {plan['source_commit']}")
    else:
        plan = json.loads(args.plan.read_text())
        if args.command == "check":
            check_evidence(plan)
        elif args.command == "stage":
            plan = stage(plan, args.signed)
            args.plan.write_text(json.dumps(plan, indent=2) + "\n")
            print(f"Bound exact draft {plan['release_id']}")
        else:
            check_evidence(plan)
            url = draft.publish(plan, args.signed, args.roundtrip)
            print(f"Published immutable Release: {url}")
            if os.environ.get("GITHUB_STEP_SUMMARY"):
                with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
                    summary.write(f"Resumed original signed run {plan['run_id']}: {url}\n")


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, OSError, subprocess.SubprocessError) as error:
        raise SystemExit(f"signed inventory resume failed: {error}") from error
