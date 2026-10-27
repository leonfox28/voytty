"""Offline audit regressions; no VM, Android, pyte or network required."""

import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class SoakDurationAuditTests(unittest.TestCase):
    def audit(self, *, seconds=600, required=600, planned=600, damage=None):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "run"
            root.mkdir()
            (root / "runner-source.py").write_text("fixture")
            manifest = {"runner_sha256": hashlib.sha256(b"fixture").hexdigest()}
            if planned is not None:
                manifest["soak_seconds"] = planned
            files = {
                "manifest.json": manifest,
                "matrix.json": [],
                "results.json": {"counts": dict(passed=0, failed=0, blocked=0, skipped=0), "results": []},
            }
            actions = [dict(host=host, old="C", scenario="detach", iteration=index, status="passed")
                       for index, host in enumerate(("A", "B"), 1)]
            soak = dict(seconds=seconds, status="passed", seed=1, sessions=["original-A", "original-B"],
                        actions=actions, connection_failures=[])
            for host in ("A", "B"):
                fixture = root / ("soak-" + host)
                fixture.mkdir()
                (fixture / "baseline.txt").write_text("101|/tmp|variable|102\n")
                (fixture / "execution.log").write_text(
                    "first|101|/tmp|variable|102|1\nsecond|101|/tmp|variable|102|2\n")
                (fixture / (host + "-sessions.txt")).write_text("original-" + host)
            if damage == "continuity":
                (root / "soak-B/execution.log").write_text("first|999|/tmp|variable|102|2\n")
            elif damage == "retained":
                (root / "soak-B/B-sessions.txt").write_text("replacement")
            elif damage == "nonce":
                (root / "soak-B/execution.log").write_text(
                    "same|101|/tmp|variable|102|1\nsame|101|/tmp|variable|102|2\n")
            elif damage == "coverage":
                actions.pop()
            elif damage == "fixture":
                soak["sessions"].pop()
            elif damage == "action":
                actions[0]["status"] = "failed"
            elif damage == "connection":
                soak["connection_failures"].append({"error": "recorded connection failure"})
            files["soak.json"] = soak
            for name, value in files.items():
                (root / name).write_text(json.dumps(value))
            command = [sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), "run"]
            if required is not None:
                command += ["--soak-seconds", str(required)]
            result = subprocess.run(command, env={**os.environ, "VOYTTY_MULTI_DEVICE_ROOT": directory},
                                    capture_output=True, text=True)
            report = root / "acceptance-audit.json"
            return result, json.loads(report.read_text()) if report.exists() else None

    def test_default_still_requires_one_hour(self):
        short, _ = self.audit(required=None)
        self.assertNotEqual(short.returncode, 0)
        hour, report = self.audit(seconds=3600, required=None, planned=None)
        self.assertEqual(hour.returncode, 0, hour.stderr)
        self.assertTrue(report["soak"]["completed_60_minutes"])

    def test_explicit_duration_requires_full_time_and_matching_plan(self):
        result, report = self.audit()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(report["soak"]["required_seconds"], 600)
        self.assertTrue(report["soak"]["completed_required_duration"])
        self.assertFalse(report["soak"]["completed_60_minutes"])
        for change in (dict(seconds=599.99), dict(planned=3600), dict(planned=None),
                       dict(seconds=float("inf")), dict(seconds=float("nan"))):
            with self.subTest(change=change):
                result, report = self.audit(**change)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(report['audit_issues'])

    def test_shorter_scope_preserves_all_continuity_and_failure_checks(self):
        for damage in ("continuity", "retained", "nonce", "coverage", "fixture", "action", "connection"):
            with self.subTest(damage=damage):
                result, report = self.audit(damage=damage)
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(report['audit_issues'])

    def test_nonpositive_target_is_rejected(self):
        for required in (0, -1):
            with self.subTest(required=required):
                result, report = self.audit(required=required)
                self.assertEqual(result.returncode, 2)
                self.assertIsNone(report)


if __name__ == "__main__":
    unittest.main()
