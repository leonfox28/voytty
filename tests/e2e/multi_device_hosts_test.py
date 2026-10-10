"""Host readiness regressions; no VM, Android, or network access."""

import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest

spec = importlib.util.spec_from_file_location("multi_device_hosts", Path(__file__).with_name("multi-device-hosts.py"))
hosts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hosts)


class HostReadinessTests(unittest.TestCase):
    def test_stopped_hosts_start_locally_without_warming_the_android_controller(self):
        calls, running = [], set()

        def cli(device, *args):
            self.assertIn(device, ("A", "B"))
            calls.append((device, *args))
            if args == ("session", "list", "--target", "local"):
                running.add(device)
                return "No sessions"
            self.assertEqual(args, ("status",))
            return ("Daemon:          Running\nNetwork:         Online" if device in running
                    else "Daemon:          Stopped\nNetwork:         Stopped")

        def online(device):
            self.assertIn(device, running)
            calls.append((device, "online"))

        result = list(hosts.prepare(SimpleNamespace(cli=cli, online=online), ["A", "B", "A"]))
        self.assertEqual([v["device"] for v in result], ["A", "B"])
        self.assertTrue(all(v["online"] and "Stopped" in v["before"] for v in result))
        self.assertEqual(calls, [(device, *args) for device in ("A", "B") for args in
                                [("status",), ("session", "list", "--target", "local"), ("online",), ("status",)]])

    def test_host_that_loses_readiness_cannot_be_reported_successful(self):
        lab = SimpleNamespace(cli=lambda *_: "Daemon:          Stopped\nNetwork:         Stopped",
                              online=lambda _: None)
        with self.assertRaises(hosts.md.Blocked):
            list(hosts.prepare(lab, ["A"]))

    def test_android_cannot_be_prepared_as_a_host(self):
        with self.assertRaises(ValueError):
            list(hosts.prepare(SimpleNamespace(), ["C"]))


if __name__ == "__main__":
    unittest.main()
