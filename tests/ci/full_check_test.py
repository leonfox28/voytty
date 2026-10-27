#!/usr/bin/env python3
"""Exact-source dispatch, reuse, and failure behavior without contacting GitHub."""

import importlib.util
import json
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("full_check", ROOT / "tools/ci/full-check.py")
check = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(check)
SHA = "a" * 40


def record(identifier=1, **values):
    return {"databaseId": identifier, "headSha": SHA, "headBranch": "main", "event": "workflow_dispatch",
            "status": "completed", "conclusion": "success", "url": f"https://example.test/runs/{identifier}", **values}


class FullCheckTests(unittest.TestCase):
    def execute(self, responses, *, reuse=True, remote=None, dirty="", run_error=None):
        responses = iter(responses)
        remotes = iter(remote or [SHA] * 10)
        def output(*args):
            if args[:3] == ("git", "rev-parse", "HEAD"):
                return SHA
            if args[:2] == ("git", "status"):
                return dirty
            if args[:3] == ("gh", "repo", "view"):
                return "owner/repo"
            if args[:2] == ("gh", "api"):
                return next(remotes)
            if args[:3] == ("gh", "run", "list"):
                self.assertIn("workflow_dispatch", args)
                return json.dumps(next(responses))
            self.fail(f"unexpected command: {args}")
        with patch.object(check, "output", side_effect=output), \
             patch.object(check.sys, "argv", ["full-check.py", *(["--reuse"] if reuse else [])]), \
             patch.object(check.subprocess, "run", side_effect=run_error) as run, \
             patch.object(check.time, "sleep"):
            try:
                check.main()
            finally:
                self.commands = [call.args[0] for call in run.call_args_list]

    def test_only_exact_main_dispatches_are_reused(self):
        records = [record(), record(2, event="push"), record(3, event="pull_request"),
                   record(4, headBranch="topic"), record(5, headSha="b" * 40)]
        self.assertEqual(check.matching_runs(records, SHA), [records[0]])
        self.execute([records])
        self.assertEqual(len(self.commands), 1)
        self.assertEqual(self.commands[0][:4], ["gh", "run", "watch", "1"])

    def test_failed_validation_is_not_automatically_retried(self):
        with self.assertRaisesRegex(ValueError, "no automatic retry"):
            self.execute([[record(conclusion="failure")]])
        self.assertEqual(self.commands, [])

    def test_explicit_new_run_excludes_old_run_ids(self):
        self.execute([[record()], [record(2, status="queued", conclusion=""), record()]], reuse=False)
        self.assertIn(f"expected_sha={SHA}", self.commands[0])
        self.assertEqual(self.commands[1][:4], ["gh", "run", "watch", "2"])

    def test_main_movement_and_dirty_sources_prevent_dispatch(self):
        for arguments in ({"remote": ["b" * 40]}, {"dirty": " M source.rs"}):
            with self.subTest(arguments=arguments), self.assertRaises(ValueError):
                self.execute([], **arguments)
            self.assertEqual(self.commands, [])

    def test_main_movement_during_dispatch_discovery_stops_without_second_dispatch(self):
        with self.assertRaisesRegex(ValueError, "main differs"):
            self.execute([[], []], remote=[SHA, "b" * 40])
        self.assertEqual(len(self.commands), 1)
        self.assertEqual(self.commands[0][:3], ["gh", "workflow", "run"])

    def test_watch_failure_propagates_without_rerun(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.execute([[record()]], run_error=subprocess.CalledProcessError(1, "gh"))
        self.assertEqual(len(self.commands), 1)


if __name__ == "__main__":
    unittest.main()
