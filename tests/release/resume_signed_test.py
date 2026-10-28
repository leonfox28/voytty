"""Fail-closed source binding and write-boundary checks for signed resumption."""

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("resume_signed", ROOT / "tools/release/resume-signed.py")
resume = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(resume)


class ResumeTest(unittest.TestCase):
    def setUp(self):
        self.selection = {"tag": "v1.0.0", "run_id": 12, "artifact_id": 34}
        commit, annotation = "a" * 40, "b" * 40
        self.records = {
            "run": {"id": 12, "head_sha": commit, "head_branch": "v1.0.0", "event": "push",
                    "path": ".github/workflows/release.yml", "status": "completed", "conclusion": "cancelled"},
            "reference": {"ref": "refs/tags/v1.0.0", "object": {"type": "tag", "sha": annotation}},
            "annotated": {"sha": annotation, "object": {"type": "commit", "sha": commit}},
            "artifact": {"id": 34, "name": "signed-release-v1.0.0-1", "expired": False,
                         "digest": "sha256:" + "c" * 64,
                         "workflow_run": {"id": 12, "head_sha": commit, "head_branch": "v1.0.0"}},
            "jobs": [{"name": name, "status": "completed", "conclusion": "success"}
                     for name in resume.draft.REQUIRED_JOBS]
                    + [{"name": name, "status": "completed", "conclusion": "cancelled"}
                       for name in resume.draft.WINDOWS_INSTALLER_JOBS]
                    + [{"name": resume.draft.PUBLISH_JOB, "status": "completed", "conclusion": "skipped"}],
            "annotations": {name: [{"annotation_level": "failure",
                                    "message": "The job has exceeded the maximum execution time of 15m0s"}]
                            for name in resume.draft.WINDOWS_INSTALLER_JOBS},
        }
        self.plan = resume.validate_evidence(self.selection, self.records)
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.signed = Path(self.temporary.name)
        payloads = {
            "voytty-release.json": json.dumps({"tag": "v1.0.0", "source_commit": commit,
                                               "classification": "stable"}).encode(),
            "voytty-x86_64-pc-windows-msvc.tar.gz": b"retained signed archive",
        }
        assets = []
        for index, (name, value) in enumerate(payloads.items()):
            (self.signed / name).write_bytes(value)
            assets.append({"id": index + 1, "name": name, "size": len(value), "state": "uploaded",
                           "digest": "sha256:" + hashlib.sha256(value).hexdigest()})
        self.release = {"id": 56, "tag_name": "v1.0.0", "target_commitish": commit,
                        "draft": True, "immutable": False, "prerelease": False, "assets": assets}

    def test_original_windows_timeout_keeps_the_signed_source(self):
        self.assertEqual(self.plan["source_commit"], "a" * 40)
        self.assertEqual(self.plan["artifact_digest"], "sha256:" + "c" * 64)
        bound = resume.bind_draft(self.plan, self.signed, self.release)
        resume.draft.verify_files(bound, self.signed)

    def test_manual_cancel_is_not_a_fixture_timeout(self):
        self.records["annotations"] = {}
        with self.assertRaisesRegex(ValueError, "manual cancellation"):
            resume.validate_evidence(self.selection, self.records)

    def test_mismatched_run_tag_and_artifact_are_rejected(self):
        mutations = [("run", "id", 99), ("run", "event", "pull_request"),
                     ("run", "path", ".github/workflows/other.yml"), ("run", "head_branch", "main"),
                     ("run", "head_sha", "d" * 40), ("run", "status", "in_progress"),
                     ("run", "conclusion", "success"), ("artifact", "expired", True),
                     ("artifact", "id", 99), ("artifact", "digest", None),
                     ("artifact", "name", "unsigned"), ("annotated", "sha", "d" * 40)]
        for owner, field, value in mutations:
            records = copy.deepcopy(self.records)
            records[owner][field] = value
            with self.subTest(owner=owner, field=field), self.assertRaises(ValueError):
                resume.validate_evidence(self.selection, records)
        for field in ("id", "head_sha", "head_branch"):
            records = copy.deepcopy(self.records)
            records["artifact"]["workflow_run"][field] = "wrong"
            with self.subTest(owner=field), self.assertRaises(ValueError):
                resume.validate_evidence(self.selection, records)

    def test_every_original_success_and_skipped_publication_are_required(self):
        for name in resume.draft.REQUIRED_JOBS | {resume.draft.PUBLISH_JOB}:
            records = copy.deepcopy(self.records)
            next(job for job in records["jobs"] if job["name"] == name)["conclusion"] = "failure"
            with self.subTest(job=name), self.assertRaises(ValueError):
                resume.validate_evidence(self.selection, records)
        for name in resume.draft.WINDOWS_INSTALLER_JOBS:
            records = copy.deepcopy(self.records)
            records["jobs"] = [job for job in records["jobs"] if job["name"] != name]
            with self.subTest(missing=name), self.assertRaises(ValueError):
                resume.validate_evidence(self.selection, records)

    def test_unexpected_and_duplicate_installers_are_rejected(self):
        for job in (self.records["jobs"][0], {"name": "Install other from local HTTPS",
                                            "status": "completed", "conclusion": "success"}):
            records = copy.deepcopy(self.records)
            records["jobs"].append(job)
            with self.assertRaises(ValueError):
                resume.validate_evidence(self.selection, records)

    def test_original_evidence_is_rechecked(self):
        for field in ("source_commit", "tag_object", "artifact_digest"):
            changed = {**self.plan, field: "changed"}
            with patch.object(resume, "inspect", return_value=changed), self.assertRaises(ValueError):
                resume.check_evidence(self.plan)

    def test_drafts_are_discovered_by_list_and_duplicates_fail(self):
        with patch.object(resume, "api", return_value=[[self.release]]) as api:
            self.assertEqual(resume.matching_release(self.plan), self.release)
            api.assert_called_once_with("releases?per_page=100", pages=True)
        with patch.object(resume, "api", return_value=[[self.release, self.release]]), self.assertRaises(ValueError):
            resume.matching_release(self.plan)

    def test_new_draft_upload_never_changes_the_original_tag_or_bytes(self):
        with patch.object(resume, "check_evidence") as check, \
                patch.object(resume, "matching_release", side_effect=[None, self.release]), \
                patch.object(resume.subprocess, "run") as run:
            plan = resume.stage(self.plan, self.signed)
        self.assertEqual(plan["release_id"], 56)
        self.assertEqual(check.call_count, 2)
        commands = [call.args[0] for call in run.call_args_list]
        self.assertEqual([command[2] for command in commands], ["create", "upload"])
        self.assertIn("--draft", commands[0])
        self.assertIn("--verify-tag", commands[0])
        self.assertEqual(commands[0][commands[0].index("--target") + 1], "a" * 40)
        self.assertNotIn("--clobber", commands[1])
        self.assertEqual(set(commands[1][6:]), {str(path) for path in self.signed.iterdir()})

    def test_matching_existing_draft_is_rejoined_without_writes(self):
        with patch.object(resume, "check_evidence"), \
                patch.object(resume, "matching_release", return_value=self.release), \
                patch.object(resume.subprocess, "run") as run:
            resume.stage(self.plan, self.signed)
            run.assert_not_called()

    def test_published_mismatched_or_partial_drafts_are_never_replaced(self):
        mutations = [("draft", False), ("immutable", True), ("target_commitish", "d" * 40),
                     ("prerelease", True), ("tag_name", "v1.0.1"), ("assets", [])]
        for field, value in mutations:
            release = {**self.release, field: value}
            with self.subTest(field=field), patch.object(resume, "check_evidence"), \
                    patch.object(resume, "matching_release", return_value=release), \
                    patch.object(resume.subprocess, "run") as run, self.assertRaises(ValueError):
                resume.stage(self.plan, self.signed)
            run.assert_not_called()
        self.release["assets"][0]["digest"] = "sha256:" + "d" * 64
        with self.assertRaisesRegex(ValueError, "differs"):
            resume.bind_draft(self.plan, self.signed, self.release)

    def test_symlink_or_wrong_manifest_cannot_be_staged(self):
        path = self.signed / "voytty-release.json"
        manifest = json.loads(path.read_text())
        path.write_text(json.dumps({**manifest, "source_commit": "d" * 40}))
        with self.assertRaisesRegex(ValueError, "identity"):
            resume.file_inventory(self.plan, self.signed)
        (self.signed / "extra").symlink_to(path)
        with self.assertRaisesRegex(ValueError, "non-regular"):
            resume.file_inventory(self.plan, self.signed)


if __name__ == "__main__":
    unittest.main()
