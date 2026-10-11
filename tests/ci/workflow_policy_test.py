#!/usr/bin/env python3
"""Verify that CI optimizations cannot turn missing evidence into a green gate."""

import copy
import importlib.util
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
    def test_only_documentation_allowlist_skips(self):
        self.assertTrue(policy.documentation_only(["README.md", "AGENTS.md", "docs/ci-release-optimization.md"]))
        for path in ("docs/relay.md", "Cargo.lock", "vendor/iroh/README.md", "docs/input.json",
                     ".github/workflows/ci.yml", "tools/ci/workflow-policy.py", "crates/core/src/lib.rs"):
            with self.subTest(path=path):
                self.assertFalse(policy.documentation_only(["docs/development.md", path]))
        self.assertFalse(policy.documentation_only([]))

    def test_non_pr_events_always_run_full_checks(self):
        for event in ("push", "workflow_dispatch", "schedule"):
            self.assertTrue(policy.full_checks(event, "", ""))

    def test_missing_history_or_invalid_encoding_runs_full_checks(self):
        self.assertTrue(policy.full_checks("pull_request", "", ""))
        with patch.object(policy.subprocess, "run", side_effect=subprocess.CalledProcessError(128, "git")):
            self.assertTrue(policy.full_checks("pull_request", "a" * 40, "b" * 40))
        with patch.object(policy.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"docs/\xff.md\0")):
            self.assertTrue(policy.full_checks("pull_request", "a" * 40, "b" * 40))

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
            head = git("rev-parse", "HEAD")
            previous = os.getcwd()
            try:
                os.chdir(directory)
                self.assertFalse(policy.full_checks("pull_request", base, head))
                git("mv", "source.rs", "docs/renamed.md")
                git("commit", "-qm", "rename source into docs")
                renamed = git("rev-parse", "HEAD")
                self.assertTrue(policy.full_checks("pull_request", base, renamed))
                (root / "docs/note.md").write_text("later docs", encoding="utf-8")
                git("commit", "-qam", "last commit is docs only")
                self.assertTrue(policy.full_checks("pull_request", base, git("rev-parse", "HEAD")))
            finally:
                os.chdir(previous)


class GateTests(unittest.TestCase):
    def needs(self, docs=False, main=False):
        needs = {job: {"result": "skipped" if docs else "success"} for job in policy.HEAVY_JOBS}
        needs.update({job: {"result": "success"} for job in ("changes", "policy", "dependencies")})
        needs["changes"]["outputs"] = {"full": "false" if docs else "true"}
        needs["candidate"] = {"result": "success" if main else "skipped"}
        return needs

    def test_supported_paths(self):
        for event, ref, docs, main in (("pull_request", "refs/pull/1/merge", True, False),
                                       ("pull_request", "refs/pull/1/merge", False, False),
                                       ("push", "refs/heads/main", False, True),
                                       ("workflow_dispatch", "refs/heads/main", False, False)):
            self.assertEqual(policy.gate_errors(event, ref, self.needs(docs, main)), [])

    def test_required_failures_cancellations_and_accidental_skips_fail(self):
        for docs in (False, True):
            needs = self.needs(docs)
            for job in needs:
                for result in ("failure", "cancelled", "skipped", "success"):
                    if result == needs[job]["result"]:
                        continue
                    with self.subTest(docs=docs, job=job, result=result):
                        broken = copy.deepcopy(needs)
                        broken[job]["result"] = result
                        self.assertTrue(policy.gate_errors("pull_request", "refs/pull/1/merge", broken))

    def test_missing_classification_or_job_and_non_pr_skips_fail(self):
        for job in self.needs():
            broken = self.needs()
            del broken[job]
            self.assertTrue(policy.gate_errors("pull_request", "refs/pull/1/merge", broken))
        for mode in (None, "", "False", "unknown"):
            broken = self.needs()
            broken["changes"]["outputs"]["full"] = mode
            self.assertTrue(policy.gate_errors("pull_request", "refs/pull/1/merge", broken))
        self.assertTrue(policy.gate_errors("push", "refs/heads/main", self.needs(True, True)))


if __name__ == "__main__":
    unittest.main()
