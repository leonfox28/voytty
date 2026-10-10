#!/usr/bin/env python3
"""Prepare only owned Linux hosts for standalone Android real-host tests."""

import argparse
import datetime as dt
import fcntl
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location("multi_device", Path(__file__).with_name("multi-device.py"))
md = importlib.util.module_from_spec(spec)
spec.loader.exec_module(md)


def prepare(lab, devices):
    for device in dict.fromkeys(devices):
        if device not in md.ACCOUNT:
            raise ValueError("only the owned A/B hosts may be prepared")
        before = lab.cli(device, "status")
        # A local read starts a stopped configured daemon without restarting a
        # live one, creating a Session, or opening a connection from C.
        lab.cli(device, "session", "list", "--target", "local")
        lab.online(device)
        after = lab.cli(device, "status")
        if "Daemon:          Running" not in after or "Network:         Online" not in after:
            raise md.Blocked(f"{device} host lost readiness before Android tests")
        yield {"device": device, "before": before, "after": after,
               "method": "local-session-list-and-status", "online": True}


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--environment", type=Path, default=md.BASE / "environment")
    parser.add_argument("--devices", nargs="+", choices=["A", "B"], default=["A", "B"])
    parser.add_argument("--output", type=Path, required=True, help="new evidence file; never overwritten")
    args = parser.parse_args()
    lab = md.Lab(SimpleNamespace(environment=args.environment))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with (lab.root / "coordinator.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if list((lab.root / "faults").glob("*.json")):
            raise md.Blocked("restore outstanding lab faults before preparing hosts")
        with args.output.open("x") as evidence:
            result = {"started": dt.datetime.now(dt.timezone.utc).isoformat(),
                      "state": "running", "hosts": [],
                      "scope": "owned A/B local readiness only; no Android or peer request"}

            def persist():
                evidence.seek(0)
                json.dump(result, evidence, indent=2)
                evidence.write("\n")
                evidence.truncate()
                evidence.flush()

            persist()
            try:
                for host in prepare(lab, args.devices):
                    result["hosts"].append(host)
                    persist()
                result["state"] = "completed"
            except (Exception, KeyboardInterrupt) as error:
                result.update(state="failed", error=str(error))
            finally:
                result["finished"] = dt.datetime.now(dt.timezone.utc).isoformat()
                persist()
    print(result["state"], args.output)
    return int(result["state"] != "completed")


if __name__ == "__main__":
    raise SystemExit(main())
