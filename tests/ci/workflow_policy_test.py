#!/usr/bin/env python3
"""Regression coverage for affected-check selection and the fail-closed CI gate."""

import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("workflow_policy", ROOT / "tools/ci/workflow-policy.py")
policy = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(policy)


class ClassificationTests(unittest.TestCase):
    def enabled(self, paths):
        return {key for key, value in policy.path_plan(paths).items() if value}

    def test_affected_owners(self):
        cases = [
            (["README.md", "AGENTS.md", "docs/development.md"], set()),
            (["crates/cli/src/main.rs"], {"rust"}),
            (["crates/client/src/transport.rs"], {"rust", "mobile"}),
            (["proto/voytty/v2/session.proto"], {"rust", "mobile"}),
            (["crates/core/build.rs"], {"rust", "mobile", "native"}),
            (["crates/platform/Cargo.toml"], {"rust", "native"}),
            (["deploy/relay/Dockerfile", "docs/relay.md"], {"relay"}),
            (["tools/release/build-native.sh"], {"rust", "native"}),
            (["install/versioned.sh.in"], {"rust", "native"}),
            (["crates/cli/src/main.rs", "docs/relay.md"], {"rust", "relay"}),
        ]
        for paths, expected in cases:
            with self.subTest(paths=paths):
                self.assertEqual(self.enabled(paths), expected)

    def test_unknown_inputs_and_dependency_changes_run_all_checks(self):
        for path in ("Cargo.toml", "Cargo.lock", "vendor/iroh/README.md", "docs/input.json",
                     ".github/workflows/ci.yml", "tools/ci/workflow-policy.py", "new-code.rs"):
            with self.subTest(path=path):
                self.assertEqual(policy.path_plan(["docs/development.md", path]), policy.full_plan())
        self.assertEqual(policy.path_plan([]), policy.full_plan())

    def test_main_quick_manual_full_and_exact_dispatch_source(self):
        self.assertFalse(any(policy.select_plan("push", "refs/heads/main", "", "a" * 40).values()))
        self.assertEqual(policy.select_plan("workflow_dispatch", "refs/heads/main", "", "a" * 40), policy.full_plan(True))
        self.assertEqual(policy.select_plan("workflow_dispatch", "refs/heads/topic", "", "a" * 40), policy.full_plan())
        with self.assertRaises(ValueError):
            policy.select_plan("workflow_dispatch", "refs/heads/main", "", "a" * 40, "b" * 40)
        with self.assertRaises(ValueError):
            policy.select_plan("push", "refs/heads/topic", "", "a" * 40)

    def test_missing_history_or_invalid_encoding_runs_full_checks(self):
        def select():
            return policy.select_plan("pull_request", "refs/pull/1/merge", "a" * 40, "b" * 40)
        self.assertEqual(policy.select_plan("pull_request", "", "", ""), policy.full_plan())
        with patch.object(policy.subprocess, "run", side_effect=subprocess.CalledProcessError(128, "git")):
            self.assertEqual(select(), policy.full_plan())
        with patch.object(policy.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"docs/\xff.md\0")):
            self.assertEqual(select(), policy.full_plan())

    def test_git_diff_covers_earlier_commits_deletions_and_renames(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(["git", "-C", directory, *args], text=True).strip()
            git("init", "-q")
            git("config", "user.name", "CI fixture")
            git("config", "user.email", "ci@example.test")
            root = Path(directory)
            (root / "docs").mkdir()
            (root / "source.rs").write_text("source", encoding="utf-8")
            git("add", ".")
            git("commit", "-qm", "base")
            base = git("rev-parse", "HEAD")
            (root / "docs/note.md").write_text("docs", encoding="utf-8")
            git("add", ".")
            git("commit", "-qm", "docs")
            previous = os.getcwd()
            try:
                os.chdir(directory)
                self.assertFalse(any(policy.path_plan(policy.changed_paths(base, git("rev-parse", "HEAD"))).values()))
                git("mv", "source.rs", "docs/renamed.md")
                git("commit", "-qm", "rename source into docs")
                (root / "docs/note.md").write_text("later docs", encoding="utf-8")
                git("commit", "-qam", "last commit is docs only")
                self.assertEqual(policy.path_plan(policy.changed_paths(base, git("rev-parse", "HEAD"))), policy.full_plan())
            finally:
                os.chdir(previous)

    def test_local_plan_includes_uncommitted_and_untracked_sources(self):
        with patch.object(policy, "changed_paths", return_value=["docs/note.md"]), \
             patch.object(policy.subprocess, "check_output", side_effect=["a" * 40, b"crates/cli/src/main.rs\0", b"new-input\0"]), \
             patch.object(policy.subprocess, "run") as run:
            policy.local_check("b" * 40)
        commands = [call.args[0] for call in run.call_args_list]
        self.assertIn(["just", "ci-unix", "false", "true"], commands)
        self.assertIn(["just", "relay-static"], commands)


class GateTests(unittest.TestCase):
    def needs(self, plan):
        needs = {job: {"result": "success" if plan[scope] else "skipped"} for job, scope in policy.JOBS.items()}
        needs.update({job: {"result": "success"} for job in ("changes", "policy", "dependencies")})
        needs["changes"]["outputs"] = {"plan": json.dumps(plan)}
        return needs

    def scenarios(self):
        return [("pull_request", "refs/pull/1/merge", policy.path_plan(paths))
                for paths in (["README.md"], ["crates/cli/src/main.rs"], ["docs/relay.md"], ["Cargo.lock"])] + [
                    ("push", "refs/heads/main", dict.fromkeys(policy.SCOPES, False)),
                    ("workflow_dispatch", "refs/heads/main", policy.full_plan(True)),
                    ("workflow_dispatch", "refs/heads/topic", policy.full_plan())]

    def test_supported_plans(self):
        for event, ref, plan in self.scenarios():
            self.assertEqual(policy.gate_errors(event, ref, self.needs(plan)), [])

    def test_failures_cancellations_missing_jobs_and_unexpected_skips_fail(self):
        for event, ref, plan in self.scenarios():
            needs = self.needs(plan)
            for job in needs:
                missing = copy.deepcopy(needs)
                del missing[job]
                self.assertTrue(policy.gate_errors(event, ref, missing))
                for result in ("failure", "cancelled", "skipped", "success"):
                    if result == needs[job]["result"]:
                        continue
                    with self.subTest(event=event, job=job, result=result):
                        broken = copy.deepcopy(needs)
                        broken[job]["result"] = result
                        self.assertTrue(policy.gate_errors(event, ref, broken))

    def test_malformed_or_incomplete_plan_cannot_pass(self):
        for encoded in (None, "", "{}", "[]", '{"rust":"true"}'):
            broken = self.needs(policy.full_plan())
            broken["changes"]["outputs"]["plan"] = encoded
            self.assertTrue(policy.gate_errors("pull_request", "refs/pull/1/merge", broken))
        self.assertTrue(policy.gate_errors("workflow_dispatch", "refs/heads/main", self.needs(policy.path_plan(["README.md"]))))
        self.assertTrue(policy.gate_errors("pull_request", "refs/pull/1/merge", self.needs(policy.full_plan(True))))
        broken = self.needs(policy.full_plan())
        broken["unreviewed"] = {"result": "success"}
        self.assertTrue(policy.gate_errors("pull_request", "refs/pull/1/merge", broken))


if __name__ == "__main__":
    unittest.main()
