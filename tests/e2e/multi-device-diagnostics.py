#!/usr/bin/env python3
"""Explicitly renew one finite diagnostic window on the owned A/B/C lab."""

import argparse
import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import uuid

spec = importlib.util.spec_from_file_location("multi_device", Path(__file__).with_name("multi-device.py"))
md = importlib.util.module_from_spec(spec)
spec.loader.exec_module(md)
BASE = md.BASE
PACKAGE = "io.github.leonfox28.voytty.dev"
SDK = Path(os.environ.get("ANDROID_HOME", Path.home() / "Library/Android/sdk"))


def call(command, data=None):
    result = subprocess.run(list(map(str, command)), input=data, text=True,
                            capture_output=True, timeout=30)
    if result.returncode:
        # The fixed command identifies the failure without exporting arbitrary
        # application files or command output into the public error message.
        raise RuntimeError(f"diagnostic command exited {result.returncode}: {shlex.join(list(map(str, command)))}")
    return result.stdout.strip()


def renew(environment, devices):
    adb = [SDK / "platform-tools/adb", "-s", environment["serial"]]
    if set(devices) & {"A", "B"}:
        instances = {item["name"]: item["status"] for line in call(["limactl", "list", "--json"]).splitlines()
                     if (item := json.loads(line))}
        for device in set(devices) & {"A", "B"}:
            name = environment["vms"][device]
            if not name.startswith("voytty-md-") or instances.get(name) != "Running":
                raise RuntimeError(f"owned {device} must already be running; no instance was started")
    if "C" in devices:
        name = call([*adb, "emu", "avd", "name"]).splitlines()[0]
        if name != environment["avd"] or not name.startswith("voytty-md-"):
            raise RuntimeError("recorded serial does not identify the owned emulator")

    for device in devices:
        if device in ("A", "B"):
            output = call(["limactl", "shell", "--workdir=/tmp", environment["vms"][device],
                           "sudo", "-Hu", "voytty-" + device.lower(), "/usr/local/bin/voytty",
                           "logs", "debug", "on"])
            yield {"device": device, "method": "local CLI logs debug on", "output": output}
        else:
            path = "no_backup/diagnostics/control.json"
            before = json.loads(call([*adb, "shell", "run-as", PACKAGE, "cat", path]))
            if (set(before) != {"schema", "started_ms", "deadline_ms"}
                    or before["schema"] != 1 or any(type(value) is not int for value in before.values())):
                raise RuntimeError("Android diagnostic control has an unexpected schema")
            # Use the emulator's clock, as the production storage owner does.
            started = int(call([*adb, "shell", "date", "+%s"])) * 1000
            control = {"schema": 1, "started_ms": started, "deadline_ms": started + 900_000}
            temporary = "no_backup/diagnostics/control.renew-" + uuid.uuid4().hex
            script = f"umask 077; cat > {temporary} && mv {temporary} {path}"
            call([*adb, "shell", shlex.join(["run-as", PACKAGE, "sh", "-c", script])], json.dumps(control))
            after = json.loads(call([*adb, "shell", "run-as", PACKAGE, "cat", path]))
            if after != control:
                raise RuntimeError("Android finite diagnostic marker did not match its write")
            yield {"device": device, "method": "atomic finite marker under application UID",
                   "before": before, "after": after}


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--environment", type=Path, default=BASE / "environment")
    parser.add_argument("--devices", nargs="+", choices=["A", "B", "C"], default=["A", "B", "C"])
    parser.add_argument("--output", type=Path, required=True, help="new evidence JSON; never overwritten")
    args = parser.parse_args()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("x") as evidence:
        result = {"started": dt.datetime.now(dt.timezone.utc).isoformat(), "window_seconds": 900,
                  "state": "running", "devices": [], "scope": "local diagnostic settings only; no restart or peer request"}

        def persist():
            evidence.seek(0)
            json.dump(result, evidence, indent=2)
            evidence.write("\n")
            evidence.truncate()
            evidence.flush()

        persist()
        try:
            environment = json.loads((args.environment / "environment.json").read_text())
            for device in renew(environment, dict.fromkeys(args.devices)):
                result["devices"].append(device)
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
