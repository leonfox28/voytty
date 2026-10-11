"""Coordinator regressions that do not launch VMs or contact a relay."""
import importlib.util
import contextlib
import hashlib
import io
import json
from pathlib import Path
import shlex
import tempfile
import threading
import subprocess
import sys
import unittest
from unittest.mock import patch
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location("multi_device", Path(__file__).with_name("multi-device.py"))
md = importlib.util.module_from_spec(spec)
spec.loader.exec_module(md)


class CoordinatorTests(unittest.TestCase):
    def test_android_snapshot_rejects_dead_instrumentation_but_retains_evidence(self):
        for exit_code in (0, 1):
            with self.subTest(exit_code=exit_code):
                android = md.Android.__new__(md.Android)
                android.process = SimpleNamespace(poll=lambda: exit_code)
                retained = dict(state="synchronizing", columns=65, rows=48, elapsedMs=123)
                android.read = lambda name: json.dumps(retained)
                with self.assertRaisesRegex(RuntimeError, "Android instrumentation exited"):
                    android.snapshot()
                self.assertEqual(android.snapshot(require_running=False), retained)
        android.process = SimpleNamespace(poll=lambda: None)
        self.assertEqual(android.snapshot(), retained)

    def test_short_outage_requires_android_surface_before_postfault_input(self):
        for catches_up in (False, True):
            with self.subTest(catches_up=catches_up):
                suite = md.Suite.__new__(md.Suite)
                suite.host, suite.session, suite.metrics = "B", "original", {}
                viewport, now, proof_times = [65, 48], [0.0], []
                suite.original_session = lambda: ("main", "Attached", *viewport, "original")
                suite.read_host = lambda marker: "started"
                suite.offline = lambda device: contextlib.nullcontext()
                suite.proof = lambda terminal, deadline: proof_times.append((now[0], deadline))
                suite.path = lambda *args: None
                def resize(rows, columns):
                    viewport[:] = [columns, rows]
                def snapshot():
                    shown = viewport if catches_up and now[0] >= 14 else [65, 48]
                    return dict(state="active", inputReady=True, columns=shown[0], rows=shown[1])
                terminal = SimpleNamespace(send=lambda text: None, resize=resize, snapshot=snapshot)
                with patch.object(md.time, "monotonic", lambda: now[0]), \
                        patch.object(md.time, "sleep", lambda seconds: now.__setitem__(0, now[0] + seconds)):
                    if catches_up:
                        suite.recover(terminal, dict(scenario="recover-short", old="C", path="relay"), short=True)
                        self.assertEqual(len(proof_times), 1)
                        self.assertGreaterEqual(proof_times[0][0], 14)
                        self.assertEqual(proof_times[0][1], 63, "all recovery checks share the original deadline")
                        self.assertEqual(suite.metrics["short_outage_resize"]["controller_observed"], viewport)
                    else:
                        with self.assertRaisesRegex(TimeoutError, "original controller recovery"):
                            suite.recover(terminal, dict(scenario="recover-short", old="C", path="relay"), short=True)
                        self.assertFalse(proof_times, "stale display must not admit the postfault input assertion")
                        self.assertEqual(suite.metrics["short_outage_resize"]["controller_observed"], [65, 48])

    def test_short_outage_drives_both_directions_without_offline_shell_input(self):
        suite = md.Suite.__new__(md.Suite)
        suite.host, suite.session = "A", "original"
        viewport, pending = [80, 24], []
        offline, writes, changes = False, [], []
        now = [0.0]
        def send(text):
            self.assertFalse(offline, "traffic setup must not replay shell input from the outage")
            self.assertIn("MD_SHORT_TRAFFIC", text)
            writes.append(text)
        def resize(rows, columns):
            self.assertTrue(offline, "exercise controller egress while the fault is active")
            self.assertNotEqual([columns, rows], viewport, "repeated short outages need a distinct resize")
            pending[:] = [columns, rows]
            changes.append(pending.copy())
        @contextlib.contextmanager
        def fault(device):
            nonlocal offline
            self.assertEqual(device, "C")
            offline = True
            try:
                yield
                if not pending:
                    raise md.Blocked("no controller egress during short outage")
            finally:
                offline = False
                if pending:
                    viewport[:] = pending
                    pending.clear()
        suite.original_session = lambda: ("main", "Attached", *viewport, "original")
        suite.read_host = lambda marker: "started"
        suite.offline = fault
        suite.proof = lambda *args: None
        suite.path = lambda *args: None
        terminal = SimpleNamespace(send=send, resize=resize, snapshot=lambda: dict(
            state="active", inputReady=True, columns=viewport[0], rows=viewport[1]))
        with patch.object(md.time, "monotonic", lambda: now[0]), \
                patch.object(md.time, "sleep", lambda seconds: now.__setitem__(0, now[0] + seconds)):
            for _ in range(2):
                suite.metrics = {}
                suite.recover(terminal, dict(scenario="recover-short", old="C", path="relay"), short=True)
                traffic = suite.metrics["short_outage_resize"]
                self.assertEqual(traffic["requested"], viewport)
                self.assertEqual(traffic["observed"], viewport)
                self.assertNotEqual(traffic["before"], traffic["requested"])
        self.assertEqual(len(writes), 2)
        self.assertEqual(len(changes), 2)
        self.assertEqual(now[0], 6.0, "keep the existing three-second short fault")

    def test_unknown_session_cannot_commit_in_the_next_independent_case(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        for query_fails in (False, True):
            with self.subTest(query_fails=query_fails), tempfile.TemporaryDirectory(dir=md.BASE) as directory:
                suite = md.Suite.__new__(md.Suite)
                suite.root, suite.results, suite.terminals = Path(directory), [], []
                suite.report = lambda: None
                calls, sessions = [], []
                stopped = False
                def query(host):
                    calls.append("query")
                    if query_fails:
                        raise RuntimeError("read-only query unavailable")
                    return sessions.copy()
                def cli(device, *command):
                    nonlocal stopped
                    self.assertEqual((device, *command), ("B", "daemon", "stop", "-y"))
                    calls.append("stop")
                    stopped = True
                suite.lab = SimpleNamespace(cli=cli, vm=lambda *args, **kwargs: "[]")
                suite.sessions = query
                suite.collect = lambda: calls.append("collect")
                def scenario(row):
                    calls.append("scenario")
                    suite.phase, suite.host, suite.session = "connection", row["host"], None
                    raise RuntimeError("operation_outcome_unknown: original create result missing")
                suite.scenario = scenario
                with contextlib.redirect_stdout(io.StringIO()):
                    result = suite.execute(dict(scenario="cold", host="B", old="A", new="A", path="relay", iteration=1))
                # The old request can arrive after a point-in-time empty query.
                if not stopped:
                    sessions.append("late previous controller occupies main")
                self.assertEqual(sessions, [], "an unresolved earlier request contaminated the next row")
                self.assertEqual(calls, ["scenario", "query", "collect", "stop"])
                self.assertEqual(result["status"], "failed")
                self.assertIn("original create result missing", result["error"])
                self.assertEqual("reconciliation_error" in result, query_fails)
                barrier = json.loads((suite.case / "session-cleanup-barrier.json").read_text())
                self.assertTrue(barrier["confirmed_stopped"])
                self.assertEqual(barrier["remaining_processes"], [])

    def test_unconfirmed_session_cleanup_retains_failure_and_stops_the_matrix(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=md.BASE) as directory:
            suite = md.Suite.__new__(md.Suite)
            suite.root, suite.results, suite.terminals = Path(directory), [], []
            suite.report = lambda: None
            suite.collect = lambda: None
            suite.sessions = lambda host: []
            suite.lab = SimpleNamespace(cli=lambda *args: None, vm=lambda *args, **kwargs: "[123]")
            def scenario(row):
                suite.phase, suite.host, suite.session = "connection", row["host"], None
                raise RuntimeError("operation_outcome_unknown: original create result missing")
            suite.scenario = scenario
            def expired(predicate, *_):
                self.assertFalse(predicate())
                raise RuntimeError("test host did not exit")
            with patch.object(md, "until", expired), contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(md.Blocked, "Session.*cleanup"):
                    suite.execute(dict(scenario="cold", host="B", old="A", new="A", path="relay", iteration=1))
            self.assertEqual(len(suite.results), 1)
            result = suite.results[0]
            self.assertEqual(result["status"], "failed")
            self.assertIn("original create result missing", result["error"])
            self.assertIn("test host did not exit", result["session_cleanup_error"])
            barrier = json.loads((suite.case / "session-cleanup-barrier.json").read_text())
            self.assertFalse(barrier["confirmed_stopped"])
            self.assertEqual(barrier["remaining_processes"], [123])

    def test_audit_requires_a_stopped_host_after_unknown_session_mutations(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        for scenario in ("cold", "fresh-pair"):
            with self.subTest(scenario=scenario), tempfile.TemporaryDirectory(dir=md.BASE) as directory:
                root = Path(directory)
                case = root / "case"
                case.mkdir()
                row = dict(scenario=scenario, host="B", old="A", new="A", path="relay", iteration=1)
                md.save(root / "matrix.json", [row])
                (root / "runner-source.py").write_text("fixture")
                md.save(root / "manifest.json", {"runner_sha256": hashlib.sha256(b"fixture").hexdigest(),
                    "session_cleanup_barrier_contract": "unknown-session-host-process-exit-v1"})
                md.save(root / "results.json", {"counts": dict(passed=0, failed=1, blocked=0, skipped=0),
                    "results": [{**row, "status": "failed", "evidence": str(case), "error": "operation_outcome_unknown"}]})
                md.save(case / "outcome-reconciliation.json", dict(observed_sessions=[]))
                if scenario == "fresh-pair":
                    md.save(case / "pair-attempt.json", dict(submitted=True, returned=True))
                valid = dict(host="B", method="test-uid-daemon-process-exit", confirmed_stopped=True, remaining_processes=[])
                for value, expected in ((None, 1), (valid, 0), ({**valid, "host": "A"}, 1),
                        ({**valid, "confirmed_stopped": False}, 1), ({**valid, "remaining_processes": [123]}, 1)):
                    path = case / "session-cleanup-barrier.json"
                    if value is None:
                        path.unlink(missing_ok=True)
                    else:
                        md.save(path, value)
                    result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    self.assertEqual(result.returncode, expected, result.stderr.decode())

    def test_concurrent_commands_retain_distinct_complete_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            lab = md.Lab(SimpleNamespace(environment=Path(directory)))
            barrier = threading.Barrier(2)
            def complete(command, **_):
                barrier.wait(timeout=5)
                return subprocess.CompletedProcess(command, 0,
                    stdout=(command[-1] + "\n").encode(), stderr=b"")
            with patch.object(md.subprocess, "run", side_effect=complete), \
                    patch.object(md, "stamp", return_value="same-second"):
                with md.ThreadPoolExecutor(2) as pool:
                    replies = list(pool.map(lab.call, (["fixture", "first"], ["fixture", "second"])))
            self.assertEqual(replies, ["first\n", "second\n"])
            records = [json.loads(line) for line in (lab.logdir / "commands.jsonl").read_text().splitlines()]
            self.assertEqual(len(records), 2)
            self.assertEqual(len({row["log"] for row in records}), 2,
                "simultaneous completions must not overwrite another command's evidence")
            for row in records:
                self.assertEqual(Path(row["log"]).read_text(),
                    " ".join(row["command"]) + "\n" + row["command"][-1] + "\n")

    def test_private_command_failure_retains_only_allowlisted_error_kind(self):
        for kind, expected in (("address_unavailable", True), ("pair_outcome_unknown", True),
                               ("private_bearer_value", False)):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as directory:
                lab = md.Lab(SimpleNamespace(environment=Path(directory)))
                secret = "PRIVATE_TICKET_MUST_NOT_APPEAR"
                completed = subprocess.CompletedProcess(["zterm", "pair", "create"], 1,
                    stdout=secret.encode(), stderr=f"Error: {kind}: {secret}\n".encode())
                with patch.object(md.subprocess, "run", return_value=completed):
                    with self.assertRaises(RuntimeError) as raised:
                        lab.call(completed.args, private=True)
                self.assertNotIn(secret, str(raised.exception))
                self.assertEqual(kind in str(raised.exception), expected)
                self.assertEqual(list(lab.logdir.iterdir()), [])

    def test_android_ticket_write_does_not_require_an_existing_files_directory(self):
        suite = md.Suite.__new__(md.Suite)
        written = []
        def adb(*command, data=None, private=False):
            self.assertTrue(private, "pair tickets must stay out of command logs")
            written.append((command, data))
        suite.lab = SimpleNamespace(cli=lambda *command, **options: "ticket-text\n", adb=adb)
        suite.android = SimpleNamespace(command=lambda op: {"snapshot": {}})
        suite.pair("A", "C")
        (command, data), = written
        self.assertEqual(command[:5], ("shell", "run-as", md.PACKAGE, "sh", "-c"))
        with tempfile.TemporaryDirectory() as app_data:
            # Run the device-side script as run-as would: in the app's data
            # directory of a fresh install, before files/ exists.
            subprocess.run(["sh", "-c", shlex.split(command[5])[0]], input=data, cwd=app_data, check=True)
            self.assertEqual((Path(app_data) / "files/multi-device-ticket.txt").read_bytes(), b"ticket-text")

    def test_failed_pair_cannot_commit_after_its_authorization_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            suite = md.Suite.__new__(md.Suite)
            suite.host, suite.case = "A", Path(directory)
            suite.metrics = {}
            grants, calls = {}, []
            stopped = False
            def commit_late():
                grants["fresh"] = dict(name="fresh", known="No", allowed="Yes")
            def cli(device, *command):
                nonlocal stopped
                calls.append((device, *command))
                if command[:2] == ("daemon", "stop"):
                    stopped = True
                    commit_late()  # An admitted host worker finishes before exit.
                elif command[:2] == ("device", "revoke"):
                    self.assertTrue(stopped, "host must stop admitting this old offer before cleanup")
                    grants[command[2]]["allowed"] = "Revoked"
            @contextlib.contextmanager
            def identity(*_):
                try:
                    yield
                finally:
                    if not stopped:
                        commit_late()  # Old single-query cleanup already returned.
            suite.lab = SimpleNamespace(cli=cli, vm=lambda *args, **kwargs: "[]")
            suite.devices = lambda host: {key: value.copy() for key, value in grants.items()}
            suite.android = SimpleNamespace(close=lambda: None, start=lambda: None)
            suite.collect = lambda *args: None
            def pair(*_):
                raise RuntimeError("pair_outcome_unknown: original result")
            suite.pair = pair
            with patch.object(md, "fresh_identity", identity):
                with self.assertRaisesRegex(RuntimeError, "pair_outcome_unknown: original result"):
                    suite.fresh_pair(dict(old="B", path="direct"))
            self.assertNotEqual(grants.get("fresh", {}).get("allowed"), "Yes")
            barrier = json.loads((suite.case / "pair-cleanup-barrier.json").read_text())
            self.assertTrue(barrier["confirmed_stopped"])
            self.assertEqual(barrier["remaining_processes"], [])
            self.assertEqual(calls.count(("A", "daemon", "stop", "-y")), 1)

    def test_unconfirmed_pair_cleanup_preserves_original_failure_and_stops_matrix(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=md.BASE) as directory:
            suite = md.Suite.__new__(md.Suite)
            suite.root, suite.results, suite.terminals = Path(directory), [], []
            suite.report = lambda: None
            suite.collect = lambda *args: None
            suite.devices = lambda host: {}
            suite.lab = SimpleNamespace(cli=lambda *args: None, vm=lambda *args, **kwargs: "[123]")
            suite.android = SimpleNamespace(close=lambda: None, start=lambda: None)
            attempts = []
            def pair(host, device):
                attempts.append((host, device))
                raise RuntimeError("pair_outcome_unknown: original pair reply missing")
            suite.pair = pair
            def scenario(row):
                suite.phase, suite.host, suite.session = "pairing-and-first-connection", row["host"], None
                suite.fresh_pair(row)
            suite.scenario = scenario
            def expired(predicate, *_):
                self.assertFalse(predicate())
                raise RuntimeError("test host workers still running")
            with patch.object(md, "fresh_identity", lambda *args: contextlib.nullcontext()), \
                    patch.object(md, "until", expired), contextlib.redirect_stdout(io.StringIO()):
                with self.assertRaisesRegex(md.Blocked, "pair.*cleanup"):
                    suite.execute(dict(scenario="fresh-pair", host="B", old="A", new="A", path="relay", iteration=1))
            self.assertEqual(attempts, [("B", "A")], "an unknown pair must not be resubmitted")
            self.assertEqual(len(suite.results), 1)
            result = suite.results[0]
            self.assertEqual(result["status"], "failed")
            self.assertEqual(result["error"], "pair_outcome_unknown: original pair reply missing")
            self.assertIn("workers still running", result["pair_cleanup_error"])
            self.assertNotIn("reconciliation_error", result)
            barrier = json.loads((suite.case / "pair-cleanup-barrier.json").read_text())
            self.assertFalse(barrier["confirmed_stopped"])
            self.assertEqual(barrier["remaining_processes"], [123])

    def test_revocation_targets_the_exact_android_identity_despite_old_test_grants(self):
        suite = md.Suite.__new__(md.Suite)
        current, orphan = "49" * 32, "ba" * 32
        suite.host = "B"
        suite.android = SimpleNamespace(snapshot=lambda: {"deviceId": current})
        suite.devices = lambda host: {
            orphan: dict(name="(unnamed)", known="No", allowed="Yes"),
            current: dict(name="(unnamed)", known="No", allowed="Yes"),
        }
        self.assertEqual(suite.revocation_principal("C"), current)
        suite.android = SimpleNamespace(snapshot=lambda: {})
        with self.assertRaisesRegex(md.Blocked, "public device identity"):
            suite.revocation_principal("C")
        suite.android = SimpleNamespace(snapshot=lambda: {"deviceId": "aa" * 32})
        with self.assertRaisesRegex(AssertionError, "exact controller"):
            suite.revocation_principal("C")

    def test_indeterminate_fresh_pair_saves_authorization_before_cleanup_without_retry(self):
        for collect_fails in (False, True):
            with self.subTest(collect_fails=collect_fails), tempfile.TemporaryDirectory() as directory:
                suite = md.Suite.__new__(md.Suite)
                suite.host, suite.case = "A", Path(directory)
                suite.metrics = {}
                calls = []
                grants = {"existing": dict(name="existing", known="Yes", allowed="Yes")}
                def cli(device, *command):
                    calls.append((device, *command))
                    if command[:2] == ("device", "revoke"):
                        # Evidence must exist before the first cleanup mutation.
                        record = json.loads((suite.case / "outcome-reconciliation.json").read_text())
                        self.assertEqual(record["observed_authorizations"]["fresh"]["allowed"], "Yes")
                        grants[command[2]]["allowed"] = "Revoked"
                suite.lab = SimpleNamespace(cli=cli, vm=lambda *args, **kwargs: "[]")
                suite.devices = lambda host: {key: value.copy() for key, value in grants.items()}
                suite.android = SimpleNamespace(close=lambda: calls.append(("android", "close")),
                    start=lambda: calls.append(("android", "start")))
                def pair(host, device):
                    calls.append(("pair", host, device))
                    grants["fresh"] = dict(name="fresh", known="No", allowed="Yes")
                    raise RuntimeError("operation_outcome_unknown: original pair result")
                suite.pair = pair
                def collect(destination):
                    if collect_fails:
                        raise OSError("diagnostic collection failed")
                suite.collect = collect
                with patch.object(md, "fresh_identity", lambda *args: contextlib.nullcontext()):
                    with self.assertRaisesRegex(RuntimeError, "operation_outcome_unknown: original pair result"):
                        suite.fresh_pair(dict(old="B", path="direct"))
                record = json.loads((suite.case / "outcome-reconciliation.json").read_text())
                self.assertEqual(record["operation"], "pair-accept")
                self.assertEqual(record["new_principals"], ["fresh"])
                self.assertEqual(record["before_authorizations"], {
                    "existing": dict(name="existing", known="Yes", allowed="Yes")})
                self.assertEqual(record["cleanup_revoked"], ["fresh"])
                self.assertEqual(record["post_cleanup_authorizations"]["fresh"]["allowed"], "Revoked")
                self.assertEqual(sum(call[0] == "pair" for call in calls), 1)
                self.assertEqual(grants["existing"]["allowed"], "Yes")
                self.assertEqual(calls[-1], ("android", "start"))
                if collect_fails:
                    self.assertIn("diagnostic collection failed", (suite.case / "fresh-pair-cleanup-errors.json").read_text())

    def test_unknown_pair_evidence_is_not_replaced_with_a_session_query(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="pair-test-", dir=md.BASE) as directory:
            suite = md.Suite.__new__(md.Suite)
            suite.root, suite.results, suite.terminals = Path(directory), [], []
            suite.report = lambda: None
            suite.collect = lambda: None
            suite.sessions = lambda host: self.fail("a Session query cannot reconcile pairing")
            observation = dict(operation="pair-accept", observed_authorizations={"fresh": {"allowed": "Yes"}})
            def scenario(row):
                suite.phase, suite.host = "pairing-and-first-connection", row["host"]
                md.save(suite.case / "outcome-reconciliation.json", observation)
                md.save(suite.case / "pair-attempt.json", dict(submitted=True, returned=False))
                raise RuntimeError("operation_outcome_unknown: pair result missing")
            suite.scenario = scenario
            with contextlib.redirect_stdout(io.StringIO()):
                result = suite.execute(dict(scenario="fresh-pair", host="A", old="B", new="B", path="direct", iteration=1))
            self.assertEqual(result["status"], "failed")
            self.assertNotIn("reconciliation_error", result)
            self.assertEqual(json.loads((suite.case / "outcome-reconciliation.json").read_text()), observation)

    def test_unknown_session_after_successful_pair_queries_sessions_and_retains_authorization(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="pair-test-", dir=md.BASE) as directory:
            suite = md.Suite.__new__(md.Suite)
            suite.root, suite.results, suite.terminals = Path(directory), [], []
            suite.report = lambda: None
            suite.collect = lambda: None
            suite.lab = SimpleNamespace(cli=lambda *args: None, vm=lambda *args, **kwargs: "[]")
            observations = []
            observed = [["main", "Detached", 80, 24, "ab" * 16]]
            def sessions(host):
                observations.append(host)
                return observed
            suite.sessions = sessions
            authorization = dict(operation="pair-accept", observed_authorizations={"fresh": {"allowed": "Yes"}})
            def scenario(row):
                suite.phase, suite.host, suite.session = "pairing-and-first-connection", row["host"], None
                md.save(suite.case / "outcome-reconciliation.json", authorization)
                md.save(suite.case / "pair-attempt.json", dict(submitted=True, returned=True))
                raise RuntimeError("operation_outcome_unknown: first Session create result missing")
            suite.scenario = scenario
            with contextlib.redirect_stdout(io.StringIO()):
                result = suite.execute(dict(scenario="fresh-pair", host="B", old="C", new="C", path="relay", iteration=1))
            self.assertEqual(result["status"], "failed")
            self.assertNotIn("reconciliation_error", result)
            self.assertEqual(observations, ["B"], "authorization does not establish a Session mutation's outcome")
            self.assertEqual(json.loads((suite.case / "outcome-reconciliation.json").read_text()), authorization)
            record = json.loads((suite.case / "session-outcome-reconciliation.json").read_text())
            self.assertEqual(record["observed_sessions"], observed)
            self.assertIsNone(record["expected_session"])
            self.assertEqual(record["method"], "read-only host session list; no mutation retry")

    def test_audit_rejects_authorization_only_for_unknown_session_after_pair(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="audit-test-", dir=md.BASE) as directory:
            root = Path(directory)
            case = root / "case"
            case.mkdir()
            row = dict(scenario="fresh-pair", host="B", old="C", new="C", path="relay", iteration=1)
            md.save(root / "matrix.json", [row])
            (root / "runner-source.py").write_text("fixture")
            md.save(root / "manifest.json", {"runner_sha256": hashlib.sha256(b"fixture").hexdigest(),
                "post_pair_session_contract": "host-session-query-after-accepted-pair-v1"})
            md.save(root / "results.json", {"counts": dict(passed=0, failed=1, blocked=0, skipped=0),
                "results": [{**row, "status": "failed", "evidence": str(case), "error": "operation_outcome_unknown"}]})
            md.save(case / "outcome-reconciliation.json", dict(operation="pair-accept", observed_authorizations={}))
            md.save(case / "pair-attempt.json", dict(submitted=True, returned=True))
            valid = dict(method="read-only host session list; no mutation retry", expected_session=None, observed_sessions=[])
            for value, expected in ((None, 1), ({"observed_authorizations": {}}, 1), (valid, 0)):
                path = case / "session-outcome-reconciliation.json"
                if value is None:
                    path.unlink(missing_ok=True)
                else:
                    md.save(path, value)
                result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, expected, result.stderr.decode())

    def test_pair_audit_requires_authorization_observation_and_cleanup(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="audit-test-", dir=md.BASE) as directory:
            root = Path(directory)
            case = root / "case"
            case.mkdir()
            row = dict(scenario="fresh-pair", host="A", old="B", new="B", path="direct", iteration=1)
            md.save(root / "matrix.json", [row])
            (root / "runner-source.py").write_text("fixture")
            md.save(root / "manifest.json", {"runner_sha256": hashlib.sha256(b"fixture").hexdigest(),
                                             "pair_reconciliation_contract": "host-authorization-before-revoke-v1"})
            md.save(root / "results.json", {"counts": dict(passed=0, failed=1, blocked=0, skipped=0),
                "results": [{**row, "status": "failed", "evidence": str(case), "error": "operation_outcome_unknown"}]})
            observed = {"fresh": dict(known="No", allowed="Yes")}
            valid = dict(operation="pair-accept", host="A", method="read-only host authorization list; no pairing retry",
                         before_authorizations={}, observed_authorizations=observed, new_principals=["fresh"],
                         cleanup_revoked=["fresh"], cleanup_errors=[],
                         post_cleanup_authorizations={"fresh": dict(known="No", allowed="Revoked")})
            for error in ("operation_outcome_unknown", "pair_outcome_unknown"):
                md.save(root / "results.json", {"counts": dict(passed=0, failed=1, blocked=0, skipped=0),
                    "results": [{**row, "status": "failed", "evidence": str(case), "error": error}]})
                for value, expected in (({"observed_sessions": []}, 1),
                        (valid, 0), ({**valid, "post_cleanup_authorizations": observed}, 1),
                        ({**valid, "new_principals": ["existing"]}, 1)):
                    md.save(case / "outcome-reconciliation.json", value)
                    result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    self.assertEqual(result.returncode, expected, result.stderr.decode())

            manifest = json.loads((root / "manifest.json").read_text())
            manifest["pair_cleanup_barrier_contract"] = "failed-accept-host-process-exit-v1"
            md.save(root / "manifest.json", manifest)
            md.save(case / "outcome-reconciliation.json", valid)
            md.save(case / "pair-attempt.json", dict(host="A", client="B", submitted=True, returned=False))
            barrier = dict(host="A", method="test-uid-daemon-process-exit", confirmed_stopped=True, remaining_processes=[])
            for value, expected in ((None, 1), (barrier, 0), ({**barrier, "remaining_processes": [123]}, 1)):
                path = case / "pair-cleanup-barrier.json"
                if value is None:
                    path.unlink(missing_ok=True)
                else:
                    md.save(path, value)
                result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, expected, result.stderr.decode())

    def test_linux_cold_connection_starts_its_daemon_through_the_real_cli(self):
        for scenario in ("cold", "first-existing"):
            with self.subTest(scenario=scenario):
                class Lab:
                    running = True
                    calls = []
                    def cli(self, device, *command):
                        self.calls.append((device, *command))
                        self.running = command[1] != "stop"
                    def online(self, device):
                        self.calls.append((device, "online"))
                        self.running = True
                suite = md.Suite.__new__(md.Suite)
                suite.lab = Lab()
                suite.metrics, suite.unavailable = {}, {}
                suite.android = SimpleNamespace(process=SimpleNamespace(poll=lambda: None),
                                                command=lambda *args: None)
                suite.close_sessions = lambda host: None
                suite.configure_path = lambda row: None
                suite.path = lambda *args: None
                suite.detach = lambda terminal: None
                suite.proof = lambda terminal: None
                suite.establish = lambda terminal: setattr(suite, "session", "original")
                started = []
                def connect(device, host, session=None):
                    if device == "B":
                        self.assertFalse(suite.lab.running,
                                         "waiting for an Online daemon hides CLI cold startup")
                        suite.lab.running = True  # The CLI owns daemon startup.
                        started.append(device)
                        if scenario == "first-existing":
                            self.assertEqual(session, "original")
                    return object()
                suite.connect = connect
                suite.scenario(dict(scenario=scenario, host="A", old="B", new="B", path="direct"))
                self.assertEqual(started, ["B"])
                self.assertIn(("A", "online"), suite.lab.calls)
                self.assertNotIn(("B", "online"), suite.lab.calls)
                self.assertEqual(suite.metrics["cold_start_mode"], "cli-starts-stopped-daemon")

    def test_host_recovery_is_settled_before_an_independent_pairing_row(self):
        for ready in (True, False):
            with self.subTest(ready=ready):
                observed = []
                def online(device):
                    observed.append(("online", device))
                    if not ready:
                        raise TimeoutError("host remains Degraded")
                suite = md.Suite.__new__(md.Suite)
                suite.lab = SimpleNamespace(online=online)
                suite.metrics, suite.unavailable = {}, {}
                suite.android = SimpleNamespace(process=SimpleNamespace(poll=lambda: None),
                                                command=lambda *args: None)
                suite.close_sessions = lambda host: None
                suite.configure_path = lambda row: None
                suite.fresh_pair = lambda row: observed.append(("pair", row["host"], row["old"]))
                row = dict(scenario="fresh-pair", host="A", old="C", new="C", path="direct")
                if ready:
                    suite.scenario(row)
                    self.assertEqual(observed, [("online", "A"), ("pair", "A", "C")])
                    self.assertEqual(suite.metrics["host_readiness"]["device"], "A")
                else:
                    with self.assertRaisesRegex(TimeoutError, "Degraded"):
                        suite.scenario(row)
                    self.assertEqual(observed, [("online", "A")])
                    self.assertEqual(suite.phase, "preparation")

    def test_local_only_scenario_does_not_require_network_readiness(self):
        suite = md.Suite.__new__(md.Suite)
        suite.lab = SimpleNamespace(online=lambda _: self.fail("local terminal must work offline"))
        suite.metrics, suite.unavailable = {}, {}
        suite.android = SimpleNamespace(process=SimpleNamespace(poll=lambda: None),
                                        command=lambda *args: None)
        suite.close_sessions = lambda host: None
        suite.configure_path = lambda row: None
        suite.connect = lambda *args: object()
        suite.path = lambda *args: None
        suite.establish = lambda terminal: None
        suite.scenario(dict(scenario="local", host="A", old="A", new="A", path="local"))
        self.assertNotIn("host_readiness", suite.metrics)

    def test_wait_rejects_a_successful_observation_that_finishes_after_its_deadline(self):
        now = [0.0]
        def slow_read():
            now[0] = 2.0
            return True
        with patch.object(md.time, "monotonic", lambda: now[0]):
            with self.assertRaisesRegex(TimeoutError, "slow observation"):
                md.until(slow_read, 1, "slow observation")

    def test_late_handoff_uses_one_disconnect_deadline_for_client_and_host(self):
        now = [0.0]
        suite = md.Suite.__new__(md.Suite)
        suite.host, suite.session, suite.metrics = "A", "original", {}
        suite.offline = lambda _: contextlib.nullcontext()
        def unexpected_connect(*args, **kwargs):
            self.fail("host released at 130 seconds, beyond the 120 second outage limit")
        suite.connect = unexpected_connect
        def observe(predicate, seconds=60, description="condition"):
            ready_at = {"old detects loss": 80.0, "host releases old controller": 130.0}[description]
            if ready_at > now[0] + seconds:
                now[0] += seconds
                raise TimeoutError(description)
            now[0] = ready_at
            return True
        with patch.object(md.time, "monotonic", lambda: now[0]), patch.object(md, "until", observe):
            with self.assertRaisesRegex(TimeoutError, "host releases old controller"):
                suite.handoff(None, dict(scenario="handoff-late", old="B", new="C"))
        self.assertEqual(now[0], 120.0)
        self.assertEqual(suite.metrics["detection_seconds"], 80.0)

    def test_offline_ports_include_only_the_test_uid_and_both_address_families(self):
        def table(uid, port):
            return f"0: 00000000:{port:04X} 00000000:0000 07 0:0 00:0 0 {uid} 0 123\n"
        with tempfile.TemporaryDirectory() as root:
            sources = {"udp": table(2002, 41000) + table(501, 22),
                       "udp6": table(2002, 42000),
                       "tcp": table(2002, 43000) + table(0, 5555), "tcp6": ""}
            class Lab:
                env = {}
                def __init__(self):
                    self.root = Path(root)
                def vm(self, device, *command, **options):
                    self_test.assertTrue(options["private"])
                    return sources[Path(command[-1]).name]
            self_test = self
            fault = md.Fault(Lab(), "B", "offline")
            fault.capture_ingress()
            expected = [["tcp", 43000], ["udp", 41000], ["udp", 42000]]
            self.assertEqual(fault.ingress, {family: expected for family in ("iptables", "ip6tables")})
            # A dual-stack IPv6 socket can receive IPv4-mapped packets too.
            self.assertIn(["udp", 42000], fault.ingress["iptables"])
            sources = {name: "" for name in sources}
            with self.assertRaisesRegex(md.Blocked, "owned network sockets"):
                fault.capture_ingress()

    def test_fault_restore_rejects_unscoped_or_management_rules(self):
        with tempfile.TemporaryDirectory() as root:
            lab = SimpleNamespace(root=Path(root), env={})
            path = Path(root) / "ZMD_123456789abc.json"
            record = dict(device="B", mode="offline", uid="2002", chain=path.stem,
                          ingress={"iptables": [["udp", 41000]], "ip6tables": [["udp", 41000]]})
            md.save(path, record)
            self.assertEqual(md.Fault.restore(lab, path).ingress, record["ingress"])
            for change in ({"chain": "OUTPUT"}, {"uid": "0"},
                           {"ingress": {"iptables": [["tcp", 22]]}},
                           {"ingress": {"iptables": [["all", 41000]]}},
                           {"ingress": {"iptables": [["udp", 0]]}}):
                md.save(path, record | change)
                with self.assertRaises(ValueError):
                    md.Fault.restore(lab, path)

    def test_normal_detach_cannot_pass_after_forced_cleanup_or_an_error_exit(self):
        for outcome in ("normal", "forced", "error"):
            with self.subTest(outcome=outcome):
                class Process:
                    pid = 123
                    returncode = None
                    waits = 0
                    def poll(self):
                        return self.returncode
                    def wait(self, timeout):
                        self.waits += 1
                        if outcome == "forced" and self.waits == 1:
                            raise subprocess.TimeoutExpired("fixture", timeout)
                        self.returncode = {"normal": 0, "forced": -15, "error": 1}[outcome]
                        return self.returncode
                terminal = md.LinuxTerminal.__new__(md.LinuxTerminal)
                terminal.process = Process()
                terminal.closed = False
                terminal.device = "B"
                terminal.master = -1
                terminal.reader = SimpleNamespace(join=lambda **kwargs: None)
                terminal.snapshot = lambda: {"state": "active"}
                terminal.send = lambda text: True
                suite = md.Suite.__new__(md.Suite)
                suite.android = object()
                suite.metrics = {}
                with patch.object(md.os, "killpg") as kill:
                    if outcome == "normal":
                        suite.detach(terminal)
                    else:
                        with self.assertRaisesRegex(AssertionError, "normal CLI detach"):
                            suite.detach(terminal)
                    self.assertEqual(kill.call_count, int(outcome == "forced"))
                self.assertTrue(terminal.closed, "failed acceptance must still release the PTY")

    def test_race_waits_for_the_second_explicit_request_to_settle(self):
        state_a = dict(state="active", inputReady=True)
        state_b = dict(state="preparing", inputReady=False)
        a = SimpleNamespace(snapshot=lambda: state_a)
        b = SimpleNamespace(snapshot=lambda: state_b)
        self.assertIsNone(md.Suite.settled_race_winner([a, b]))
        state_a.update(state="lease_lost", inputReady=False)
        state_b.update(state="active", inputReady=True)
        self.assertIs(md.Suite.settled_race_winner([a, b]), b)
        state_a.update(state="none", error="session_occupied")
        self.assertIs(md.Suite.settled_race_winner([a, b]), b)
        state_a.update(error="operation_outcome_unknown")
        with self.assertRaisesRegex(AssertionError, "operation_outcome_unknown"):
            md.Suite.settled_race_winner([a, b])
        state_a.clear()
        state_a.update(state="closed", exit=1, rawError="Error: lease_lost: another attachment took over")
        self.assertIs(md.Suite.settled_race_winner([a, b]), b)
        state_a.update(rawError="Error: operation_outcome_unknown: initial attachment result missing")
        with self.assertRaisesRegex(AssertionError, "operation_outcome_unknown"):
            md.Suite.settled_race_winner([a, b])

    def test_explicit_android_takeover_continues_after_known_occupancy(self):
        state = {"hosts": [{"id": "host-a", "name": "multi-device-A"}]}
        commands = []
        def command(op, **args):
            commands.append((op, args))
            state.update(host="host-a", state="none", error="session_occupied")
            if op == "select":
                state.update(state="active", error=None, inputReady=True)
            return {"snapshot": state.copy()}
        suite = md.Suite.__new__(md.Suite)
        suite.android = SimpleNamespace(snapshot=lambda: state, command=command)
        suite.metrics = {}
        suite.connect("C", "A", "original", takeover=True, ordinary=True)
        self.assertEqual(commands, [("connect", {"host": "multi-device-A"}),
                                    ("select", {"session": "original", "takeover": True})])

    def test_android_host_open_does_not_reselect_its_retained_session(self):
        for stage in ("synchronizing", "active", "reconnecting"):
            with self.subTest(stage=stage):
                state = {"hosts": [{"id": "host-b", "name": "multi-device-B"}]}
                commands = []
                def command(op, **args):
                    commands.append((op, args))
                    if op == "connect":
                        state.update(host="host-b", session="original", state=stage, inputReady=False)
                    else:
                        state.update(state="none", error="session_occupied")
                    return {"snapshot": state.copy()}
                suite = md.Suite.__new__(md.Suite)
                suite.android = SimpleNamespace(snapshot=lambda: state.copy(), command=command)
                suite.metrics = {}
                suite.connect("C", "B", "original", ordinary=True)
                self.assertEqual(commands, [("connect", {"host": "multi-device-B"})],
                    "the session-panel UI retains an already attached session during synchronization")
                self.assertNotIn("error", state)
                # Choosing another ID must still perform that explicit selection.
                suite.connect("C", "B", "another", ordinary=True)
                self.assertEqual(commands[-1], ("select", {"session": "another", "takeover": False}))

    def test_soak_audit_requires_a_full_hour_and_both_original_shells(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="audit-test-", dir=md.BASE) as temporary:
            root = Path(temporary)
            md.save(root / "matrix.json", [])
            md.save(root / "results.json", {"counts": dict(passed=0, failed=0, blocked=0, skipped=0), "results": []})
            (root / "runner-source.py").write_text("fixture")
            md.save(root / "manifest.json", {"runner_sha256": hashlib.sha256(b"fixture").hexdigest()})
            for host in ("A", "B"):
                fixture = root / ("soak-" + host)
                fixture.mkdir()
                (fixture / "baseline.txt").write_text("101|/tmp|variable|102\n")
                (fixture / "execution.log").write_text("first|101|/tmp|variable|102|1\nlast|101|/tmp|variable|102|100\n")
                (fixture / (host + "-sessions.txt")).write_text("ID: original-" + host)
            soak = dict(seed=1, seconds=3600, status="passed", sessions=["original-A", "original-B"],
                        actions=[dict(host=host, iteration=i, scenario="detach", status="passed")
                                 for i, host in enumerate(("A", "B"), 1)])
            variants = ((soak, 0), ({**soak, "seconds": 3599}, 1),
                        ({**soak, "sessions": ["replacement-A", "original-B"]}, 1),
                        *(({**soak, "actions": [{**a, "status": status} for a in soak["actions"]]}, 1)
                          for status in ("failed", "blocked", "skipped")))
            for value, expected in variants:
                md.save(root / "soak.json", value)
                result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, expected, result.stderr.decode())

    def test_soak_preserves_blocked_actions_and_original_sessions_without_passing(self):
        with tempfile.TemporaryDirectory() as directory:
            suite = md.Suite.__new__(md.Suite)
            suite.root, suite.terminals = Path(directory), []
            suite.args = SimpleNamespace(soak_seconds=10, seed=1)
            suite.soak_connection_failures = []
            suite.release_path = lambda: None
            suite.close_sessions = lambda host: None
            suite.android = SimpleNamespace(command=lambda *args: None)
            suite.rng = SimpleNamespace(choice=lambda choices: "recover-short")
            suite.named_session = lambda: "original-" + suite.host
            terminal, now, opened = object(), [0.0], []
            def open_original(device, session):
                self.assertEqual(session, "original-" + suite.host)
                suite.session = session
                opened.append((suite.host, session))
                return terminal, device
            suite.open_soak_original = open_original
            def establish(current):
                suite.secret, suite.remote_dir, suite.baseline = "secret", "remote", "baseline"
            suite.establish = establish
            suite.sessions = lambda host: [("named", "Detached", 80, 24, "original-" + host)]
            suite.original_session = lambda: suite.sessions(suite.host)[0]
            suite.collect = lambda *args: None
            suite.path = lambda *args: None
            suite.proof = lambda *args: None
            suite.detach = lambda *args: None
            suite.verify_counter_progress = lambda: None
            def blocked(*args, **kwargs):
                raise md.Blocked("fault injected but no ingress packet loss observed")
            suite.recover = blocked
            with patch.object(md.time, "monotonic", lambda: now[0]), \
                    patch.object(md.time, "sleep", lambda seconds: now.__setitem__(0, now[0] + seconds)), \
                    contextlib.redirect_stdout(io.StringIO()):
                suite.soak()
            report = json.loads((suite.root / "soak.json").read_text())
            self.assertEqual(report["status"], "failed", "blocked samples cannot pass endurance acceptance")
            self.assertEqual([action["status"] for action in report["actions"]], ["blocked", "blocked"])
            self.assertEqual(report["sessions"], ["original-A", "original-B"])
            self.assertEqual(opened, [(host, "original-" + host) for host in ("A", "A", "B", "B", "A", "B")])
            for action in report["actions"]:
                case = suite.root / ("soak-" + action["host"]) / f"failed-action-{action['iteration']:04}"
                self.assertEqual(json.loads((case / "result.json").read_text())["status"], "blocked")
                self.assertTrue((case / "outcome-reconciliation.json").exists())

    def test_audit_rejects_empty_or_duplicate_execution_evidence(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="audit-test-", dir=md.BASE) as temporary:
            root = Path(temporary)
            case = root / "case"
            case.mkdir()
            row = dict(scenario="cold", host="A", old="B", new="B", path="direct", iteration=1)
            md.save(root / "matrix.json", [row])
            md.save(root / "results.json", {
                "counts": dict(passed=1, failed=0, blocked=0, skipped=0),
                "results": [{**row, "status": "passed", "evidence": str(case),
                             "metrics": {"paths": [{"path": "direct", "snapshot": {"path": "direct"}}]}}],
            })
            (root / "runner-source.py").write_text("fixture")
            md.save(root / "manifest.json", {"runner_sha256": hashlib.sha256(b"fixture").hexdigest()})
            (case / "baseline.txt").write_text("101|/tmp|variable|102\n")
            record = "nonce|101|/tmp|variable|102|1\n"
            for proof, expected in ((record, 0), ("", 1), (record * 2, 1)):
                (case / "execution.log").write_text(proof)
                result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, expected, result.stderr.decode())
                audit = json.loads((root / "acceptance-audit.json").read_text())
                self.assertEqual(bool(audit["audit_issues"]), bool(expected))

    def test_audit_enforces_handoff_but_not_short_outage_detection(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="audit-test-", dir=md.BASE) as temporary:
            root = Path(temporary)
            case = root / "case"
            case.mkdir()
            row = dict(scenario="handoff-late", host="A", old="B", new="C", path="direct", iteration=1)
            md.save(root / "matrix.json", [row])
            (root / "runner-source.py").write_text("fixture")
            md.save(root / "manifest.json", {"runner_sha256": hashlib.sha256(b"fixture").hexdigest(),
                                             "timing_contract": "absolute-offline-deadlines-v1"})
            (case / "baseline.txt").write_text("101|/tmp|variable|102\n")
            (case / "execution.log").write_text("nonce|101|/tmp|variable|102|1\n")
            timings = dict(detection_seconds=80, host_release_seconds=110, retirement_seconds=1)
            for scenario, values, expected in (
                    ("handoff-late", timings, 0),
                    ("handoff-late", {**timings, "host_release_seconds": 130}, 1),
                    ("handoff-late", dict(detection_seconds=80, retirement_seconds=1), 1),
                    ("output-resize", dict(recovery_seconds=0.5), 0)):
                row["scenario"] = scenario
                md.save(root / "matrix.json", [row])
                md.save(root / "results.json", {"counts": dict(passed=1, failed=0, blocked=0, skipped=0),
                    "results": [{**row, "status": "passed", "evidence": str(case), "metrics": {
                        "paths": [{"path": "direct", "snapshot": {"path": "direct"}}], **values}}]})
                result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, expected, result.stderr.decode())

    def test_audit_requires_linux_cli_cold_bootstrap_contract(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="audit-test-", dir=md.BASE) as temporary:
            root = Path(temporary)
            case = root / "case"
            case.mkdir()
            (root / "runner-source.py").write_text("fixture")
            md.save(root / "manifest.json", {"runner_sha256": hashlib.sha256(b"fixture").hexdigest(),
                                             "cold_start_contract": "cli-autostart-daemon-v1"})
            (case / "baseline.txt").write_text("101|/tmp|variable|102\n")
            (case / "execution.log").write_text("nonce|101|/tmp|variable|102|1\n")
            for scenario in ("cold", "first-existing"):
                row = dict(scenario=scenario, host="A", old="B", new="B", path="direct", iteration=1)
                md.save(root / "matrix.json", [row])
                for mode, expected in ((None, 1), ("cli-starts-stopped-daemon", 0)):
                    md.save(root / "results.json", {"counts": dict(passed=1, failed=0, blocked=0, skipped=0),
                        "results": [{**row, "status": "passed", "evidence": str(case), "metrics": {
                            "cold_start_mode": mode,
                            "paths": [{"path": "direct", "snapshot": {"path": "direct"}}]}}]})
                    result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                            stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    self.assertEqual(result.returncode, expected, result.stderr.decode())

    def test_audit_requires_local_host_readiness_without_warming_the_controller(self):
        md.BASE.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(prefix="audit-test-", dir=md.BASE) as temporary:
            root = Path(temporary)
            case = root / "case"
            case.mkdir()
            (root / "runner-source.py").write_text("fixture")
            md.save(root / "manifest.json", {"runner_sha256": hashlib.sha256(b"fixture").hexdigest(),
                "host_readiness_contract": "local-host-online-before-scenario-v1"})
            (case / "baseline.txt").write_text("101|/tmp|variable|102\n")
            (case / "execution.log").write_text("nonce|101|/tmp|variable|102|1\n")
            row = dict(scenario="cold", host="A", old="B", new="B", path="direct", iteration=1)
            md.save(root / "matrix.json", [row])
            valid = dict(device="A", method="local-status-only", online=True, seconds=0.2)
            for readiness, expected in (({}, 1), ({**valid, "device": "B"}, 1),
                    ({**valid, "method": "peer-request"}, 1), ({**valid, "online": False}, 1),
                    ({**valid, "seconds": 60.001}, 1), (valid, 0)):
                md.save(root / "results.json", {"counts": dict(passed=1, failed=0, blocked=0, skipped=0),
                    "results": [{**row, "status": "passed", "evidence": str(case), "metrics": {
                        "host_readiness": readiness,
                        "paths": [{"path": "direct", "snapshot": {"path": "direct"}}]}}]})
                result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, expected, result.stderr.decode())

    def test_linux_driver_active_waits_for_the_presented_input_fence(self):
        import pyte
        terminal = md.LinuxTerminal.__new__(md.LinuxTerminal)
        terminal.process = SimpleNamespace(poll=lambda: None)
        terminal.reader_error = None
        terminal.lock = threading.Lock()
        terminal.raw = bytearray()
        terminal.screen = pyte.Screen(100, 30)
        terminal.state_event = {"fields": {"stage": "active"}}
        stream = pyte.Stream(terminal.screen)
        # Driver Active can precede the CLI's deferred resize synchronization.
        # The real shell fixture keeps its cursor visible when input is ready.
        stream.feed("\x1b[?25l")
        self.assertFalse(terminal.snapshot()["inputReady"])
        stream.feed("\x1b[?25h")
        self.assertTrue(terminal.snapshot()["inputReady"])
        terminal.state_event = {"fields": {"stage": "reconnecting"}}
        self.assertFalse(terminal.snapshot()["inputReady"])

    def test_soak_explicit_reopens_keep_the_original_id_and_every_failure(self):
        with tempfile.TemporaryDirectory() as root:
            suite = md.Suite.__new__(md.Suite)
            suite.case = Path(root)
            suite.host = "A"
            suite.terminals = []
            suite.soak_connection_failures = []
            suite.android = SimpleNamespace(command=lambda *args, **kwargs: None)
            session = ("named", "Detached", 80, 25, "original")
            suite.sessions = lambda host: [session]
            suite.original_session = lambda: session
            suite.collect = lambda destination: None
            attempts = []
            terminal = object()
            def connect(device, host, selected):
                attempts.append((device, host, selected))
                if len(attempts) < 3:
                    raise RuntimeError("transport_unavailable")
                return terminal
            suite.connect = connect
            with patch.object(md.time, "sleep", return_value=None):
                self.assertEqual(suite.open_soak_original("B", "original"), (terminal, "B"))
            self.assertEqual(attempts, [(device, "A", "original") for device in ("B", "C", "B")])
            self.assertEqual(len(suite.soak_connection_failures), 2)
            self.assertEqual(len(list(suite.case.glob("connection-failure-*/result.json"))), 2)
            self.assertEqual([failure["controller"] for failure in suite.soak_connection_failures], ["B", "C"])

    def test_soak_continuation_reports_the_controller_that_reopened_the_original(self):
        with tempfile.TemporaryDirectory() as root:
            suite = md.Suite.__new__(md.Suite)
            suite.case, suite.host, suite.terminals = Path(root), "B", []
            suite.soak_connection_failures = []
            suite.android = SimpleNamespace(command=lambda *args, **kwargs: None)
            session = ("named", "Detached", 80, 25, "original")
            suite.sessions = lambda host: [session]
            suite.original_session = lambda: session
            suite.collect = lambda destination: None
            attempts = []
            def connect(device, host, selected):
                attempts.append((device, host, selected))
                if device == "A":
                    raise RuntimeError("initial terminal snapshot deadline")
                return suite.android
            suite.connect = connect
            with patch.object(md.time, "sleep", return_value=None):
                self.assertEqual(suite.open_soak_original("A", "original"), (suite.android, "C"))
            self.assertEqual(attempts, [("A", "B", "original"), ("C", "B", "original")])
            self.assertEqual(len(suite.soak_connection_failures), 1)

    def test_soak_never_reopens_an_unknown_mutation_or_a_missing_session(self):
        for error, present in (("operation_outcome_unknown", True), ("transport_unavailable", False)):
            with self.subTest(error=error), tempfile.TemporaryDirectory() as root:
                suite = md.Suite.__new__(md.Suite)
                suite.case = Path(root)
                suite.host = "A"
                suite.soak_connection_failures = []
                suite.sessions = lambda host: [("named", "Detached", 80, 25, "original")] if present else []
                suite.collect = lambda destination: None
                attempts = []
                def connect(*args):
                    attempts.append(args)
                    raise RuntimeError(error)
                suite.connect = connect
                with self.assertRaisesRegex((RuntimeError, AssertionError), "unknown|replacement is forbidden"):
                    suite.open_soak_original("B", "original")
                self.assertEqual(len(attempts), 1)
                self.assertEqual(len(suite.soak_connection_failures), 1)

    def test_failed_android_host_open_is_not_retried_by_selecting_the_session(self):
        for error in ("deadline_exceeded", "operation_outcome_unknown"):
            with self.subTest(error=error):
                class Android:
                    def __init__(self):
                        self.commands = []
                        self.value = {"hosts": [{"id": "host-a", "name": "multi-device-A"}]}
                    def snapshot(self):
                        return self.value
                    def command(self, op, **arguments):
                        self.commands.append(op)
                        self.value.update(host="host-a", state="none", error=error)
                        if op == "select":
                            # A second attempt could succeed after warming the
                            # transport, but must not erase the first failure.
                            self.value.update(state="active", error=None, inputReady=True)
                        return {"snapshot": self.value.copy()}
                suite = md.Suite.__new__(md.Suite)
                suite.android = Android()
                suite.metrics = {}
                with self.assertRaisesRegex(AssertionError, error):
                    suite.connect("C", "A", "original-session")
                self.assertEqual(suite.android.commands, ["connect"])
                suite.android = Android()
                observed = suite.connect("C", "A", "original-session", ordinary=True)
                self.assertEqual(observed.snapshot()["error"], error)
                self.assertEqual(suite.android.commands, ["connect"])

    def test_split_extension_queries_do_not_corrupt_terminal_cells(self):
        wire = "\x1b[30;1Hlocal\x1b[=0u\x1b[?2031$p\x1b[?996n\x1b[1;1Hprompt"
        expected = "\x1b[30;1Hlocal\x1b[?996n\x1b[1;1Hprompt"
        for cut in range(len(wire) + 1):
            parser = md.ScreenInput()
            self.assertEqual(parser.feed(wire[:cut]) + parser.feed(wire[cut:]), expected)
        parser = md.ScreenInput()
        self.assertEqual("".join(parser.feed(char) for char in wire), expected)

    def test_partial_dual_stack_fault_restores_only_its_own_chain(self):
        with tempfile.TemporaryDirectory() as root:
            calls = []

            class Lab:
                env = {"android_uid": "10213"}

                def __init__(self):
                    self.root = Path(root)

                def vm(self, device, *command, **options):
                    calls.append((device, command))
                    if command[1] == "cat":
                        return "0: 00000000:A028 00000000:0000 07 0:0 00:0 0 2002 0 123\n"
                    # Intent has to be durable before even the first mutation.
                    journals = list((self.root / "faults").glob("*.json"))
                    assert len(journals) == 1
                    record = json.loads(journals[0].read_text())
                    assert record["uid"] == "2002"
                    assert record["ingress"]["iptables"]
                    if command[:4] == ("sudo", "ip6tables", "-w", "-N"):
                        raise RuntimeError("injected IPv6 setup failure")
                    return "-P OUTPUT ACCEPT\n" if command[-1] == "-S" else ""

            fault = md.Fault(Lab(), "B", "offline")
            fault.start_guard = lambda: None
            with self.assertRaisesRegex(RuntimeError, "IPv6"):
                fault.__enter__()
            self.assertFalse(list((Path(root) / "faults").glob("*.json")))
            for family in ("iptables", "ip6tables"):
                self.assertIn(("B", ("sudo", family, "-w", "-D", "OUTPUT", "-m", "owner", "--uid-owner", "2002", "-j", fault.chain)), calls)
                self.assertIn(("B", ("sudo", family, "-w", "-X", fault.chain)), calls)
                self.assertIn(("B", ("sudo", family, "-w", "-D", "INPUT", "-p", "udp", "--dport", "41000", "-j", fault.chain + "_I")), calls)
                self.assertIn(("B", ("sudo", family, "-w", "-X", fault.chain + "_I")), calls)
            self.assertFalse(any(command[-2:] == ("-F", "OUTPUT") for _, command in calls))

    def test_matrix_keeps_each_host_direction_path_and_repetition_distinct(self):
        rows = md.matrix(10)
        keys = [tuple(sorted(row.items())) for row in rows]
        self.assertEqual(len(keys), len(set(keys)))
        for host, remote in (("A", "B"), ("B", "A")):
            for path in ("direct", "relay"):
                for old, new in ((remote, "C"), ("C", remote)):
                    for scenario in ("takeover", "handoff-early", "handoff-late"):
                        subset = [row for row in rows if (row["host"], row["old"], row["new"], row["path"], row["scenario"]) ==
                                  (host, old, new, path, scenario)]
                        self.assertEqual({row["iteration"] for row in subset}, set(range(1, 11)))

    def test_matrix_preview_applies_the_requested_run_filters(self):
        for options, count in ((["--group", "core", "--repeat", "10"], 1460),
                               (["--group", "extended", "--repeat", "1"], 88),
                               (["--scenario", "fresh-pair,revoke,race-takeover", "--repeat", "2"], 48),
                               (["--scenario", "takeover", "--host", "A", "--path", "relay",
                                 "--controller", "C", "--repeat", "3"], 6)):
            with self.subTest(options=options), tempfile.TemporaryDirectory() as directory:
                result = subprocess.run([sys.executable, str(Path(md.__file__)), "matrix",
                    "--environment", directory, *options], text=True, capture_output=True, check=True)
                rows = json.loads(result.stdout)
                self.assertEqual(len(rows), count)
                if "--controller" in options:
                    self.assertEqual({(r["scenario"], r["host"], r["old"], r["path"]) for r in rows},
                                     {("takeover", "A", "C", "relay")})
                    self.assertEqual({r["new"] for r in rows}, {"A", "B"})
                    self.assertEqual({r["iteration"] for r in rows}, {1, 2, 3})
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run([sys.executable, str(Path(md.__file__)), "matrix",
                "--environment", directory, "--scenario", "misspelled-scenario"],
                text=True, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("unknown scenarios: misspelled-scenario", result.stderr)

    def test_guard_restores_journal_on_coordinator_pipe_eof(self):
        with tempfile.TemporaryDirectory() as root:
            calls = []
            class Lab:
                env = {}
                args = SimpleNamespace(fault_chain="ZMD_0123456789ab")
                def __init__(self):
                    self.root = Path(root)
                def vm(self, device, *command, **options):
                    calls.append((device, command))
                    return ""
            lab = Lab()
            journal = lab.root / "faults" / (lab.args.fault_chain + ".json")
            ingress = {family: [["udp", 41000], ["tcp", 42000]] for family in ("iptables", "ip6tables")}
            md.save(journal, dict(device="A", mode="offline", uid="2001", chain=lab.args.fault_chain, ingress=ingress))
            md.guard(lab, io.BytesIO(b""))
            self.assertFalse(journal.exists())
            self.assertEqual(sum(command[-1] == "-S" for _, command in calls), 2)
            self.assertEqual(sum("INPUT" in command and "-D" in command for _, command in calls), 4)
            # Explicit successful cleanup must not issue another firewall mutation.
            calls.clear()
            md.guard(lab, io.BytesIO(b"restored\n"))
            self.assertEqual(calls, [])

    def test_fault_cleanup_attempts_both_families_after_one_rule_errors(self):
        with tempfile.TemporaryDirectory() as root:
            calls = []
            class Lab:
                env = {}
                def __init__(self):
                    self.root = Path(root)
                def vm(self, device, *command, **options):
                    calls.append(command)
                    if command[1] == "iptables" and "-D" in command and "INPUT" in command:
                        raise RuntimeError("injected ingress cleanup error")
                    return ""
            fault = md.Fault(Lab(), "A", "offline")
            fault.ingress = {family: [["udp", 41000]] for family in ("iptables", "ip6tables")}
            md.save(fault.journal, {"preserved": True})
            with self.assertRaisesRegex(RuntimeError, "ingress cleanup error"):
                fault.__exit__()
            self.assertTrue(fault.journal.exists(), "leave durable intent for the cleanup guard")
            self.assertIn(("sudo", "ip6tables", "-w", "-X", fault.chain + "_I"), calls)

    def test_offline_requires_both_direction_counters_and_keeps_original_failure(self):
        with tempfile.TemporaryDirectory() as root:
            suite = md.Suite.__new__(md.Suite)
            suite.lab = SimpleNamespace(root=Path(root))
            suite.case = Path(root) / "case"
            suite.metrics = {}
            class Fault:
                chain = "ZMD_123456789abc"
                def __init__(self, *args):
                    pass
                def __enter__(self):
                    return self
                def __exit__(self, *args):
                    md.save(Path(root) / "fault-evidence" / (self.chain + ".json"), {
                        "families": {"iptables": " 3 100 DROP all"},
                        "ingress_families": {"iptables": f" {ingress_count} 100 DROP all"}})
            with patch.object(md, "Fault", Fault):
                for ingress_count in (0, 2):
                    if ingress_count:
                        with suite.offline("A"):
                            pass
                    else:
                        with self.assertRaises(md.Blocked), suite.offline("A"):
                            pass
                    with self.assertRaisesRegex(TimeoutError, "original failure"), suite.offline("A"):
                        raise TimeoutError("original failure")
            self.assertEqual(suite.metrics["faults"][-1]["dropped_by_direction"], {"egress": 3, "ingress": 2})
            suite.args = SimpleNamespace(offline_scope="egress")
            ingress_count = 0
            with patch.object(md, "Fault", Fault), suite.offline("A"):
                pass
            self.assertEqual(suite.metrics["faults"][-1]["scope"], "test-uid-egress")

    def test_audit_requires_real_bidirectional_counters(self):
        with tempfile.TemporaryDirectory(prefix="audit-test-", dir=md.BASE) as temporary:
            root = Path(temporary)
            case = root / "case"
            case.mkdir()
            row = dict(scenario="recover-long", host="A", old="B", new="B", path="direct", iteration=1)
            md.save(root / "matrix.json", [row])
            (root / "runner-source.py").write_text("fixture")
            md.save(root / "manifest.json", {"runner_sha256": hashlib.sha256(b"fixture").hexdigest(),
                                             "offline_scope": "owned-sockets-bidirectional"})
            (case / "baseline.txt").write_text("101|/tmp|variable|102\n")
            (case / "execution.log").write_text("nonce|101|/tmp|variable|102|1\n")
            evidence = case / "fault.json"
            for ingress, claimed, expected in ((0, 0, 1), (0, 2, 1), (2, 2, 0)):
                md.save(evidence, {"families": {"iptables": " 3 100 DROP all"},
                                   "ingress_families": {"iptables": f" {ingress} 100 DROP all"}})
                md.save(root / "results.json", {"counts": dict(passed=1, failed=0, blocked=0, skipped=0),
                    "results": [{**row, "status": "passed", "evidence": str(case), "metrics": {
                        "paths": [{"path": "direct", "snapshot": {"path": "direct"}}],
                        "faults": [{"scope": "owned-sockets-bidirectional", "evidence": str(evidence),
                                    "dropped_by_direction": {"egress": 3, "ingress": claimed}}]}}]})
                result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, expected, result.stderr.decode())

            row["scenario"] = "recover-short"
            md.save(root / "matrix.json", [row])
            manifest = json.loads((root / "manifest.json").read_text())
            manifest["short_outage_traffic_contract"] = "shell-output-and-controller-resize-v1"
            md.save(root / "manifest.json", manifest)
            record = json.loads((root / "results.json").read_text())
            record["results"][0]["scenario"] = "recover-short"
            valid = dict(before=[80, 24], requested=[101, 31], observed=[101, 31])
            for resize, expected in ((None, 1), (valid, 0),
                    ({**valid, "before": valid["requested"]}, 1),
                    ({**valid, "observed": [80, 24]}, 1)):
                record["results"][0]["metrics"]["short_outage_resize"] = resize
                md.save(root / "results.json", record)
                result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, expected, result.stderr.decode())

            row["old"] = "C"
            md.save(root / "matrix.json", [row])
            record["results"][0]["old"] = "C"
            manifest["short_outage_viewport_contract"] = "host-and-android-surface-v1"
            md.save(root / "manifest.json", manifest)
            visible = {**valid, "controller_observed": [101, 31],
                       "controller_state": "active", "controller_input_ready": True}
            for resize, expected in ((valid, 1), (visible, 0),
                    ({**visible, "controller_observed": [65, 48]}, 1),
                    ({**visible, "controller_state": "synchronizing"}, 1),
                    ({**visible, "controller_input_ready": False}, 1)):
                record["results"][0]["metrics"]["short_outage_resize"] = resize
                md.save(root / "results.json", record)
                result = subprocess.run([sys.executable, str(Path(__file__).with_name("multi-device-audit.py")), root.name],
                                        stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                self.assertEqual(result.returncode, expected, result.stderr.decode())

    def test_relay_reboot_reapplies_one_guest_and_cleanup_attempts_every_guest(self):
        calls = []
        class Fault:
            def __init__(self, lab, device, mode):
                self.device = device
            def __enter__(self):
                calls.append(("enter", self.device))
                return self
            def __exit__(self):
                calls.append(("exit", self.device))
                if self.device == "C":
                    raise RuntimeError("injected C cleanup failure")
        with patch.object(md, "Fault", Fault):
            group = md.RelayFaults(None, ("A", "B", "C")).__enter__()
            calls.clear()
            group.reapply("B")
            self.assertEqual(calls, [("exit", "B"), ("enter", "B")])
            calls.clear()
            with self.assertRaisesRegex(RuntimeError, "C cleanup"):
                group.__exit__()
            self.assertEqual(calls, [("exit", "C"), ("exit", "B"), ("exit", "A")])


if __name__ == "__main__":
    unittest.main()
