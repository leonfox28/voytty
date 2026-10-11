#!/usr/bin/env python3
"""Disposable A/B Linux + C Android acceptance; never uses the operator's identity."""

from __future__ import annotations

import argparse
import contextlib
import copy
from concurrent.futures import ThreadPoolExecutor
import datetime as dt
import codecs
import fcntl
import hashlib
import json
import linecache
import os
from pathlib import Path
import random
import re
import pty
import select
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import struct
import tarfile
import termios
import threading
import time
import traceback
import uuid

ROOT = Path(__file__).resolve().parents[2]
# The environment holds the emulator's disk and the only record of the owned
# VMs, so it must survive `cargo clean`; it also stays outside the source tree
# that `build` archives. Run evidence lives beside it.
BASE = Path(os.environ.get("ZTERM_MULTI_DEVICE_ROOT")
            or Path(os.environ.get("XDG_STATE_HOME") or Path.home() / ".local/state")
            / "zterm-verification/multi-device")
PACKAGE = "io.github.leonfox28.zterm.dev"
RUNNER = PACKAGE + ".test/androidx.test.runner.AndroidJUnitRunner"
ACCOUNT = {"A": "zterm-a", "B": "zterm-b"}
CORE_SCENARIOS = {"local", "fresh-pair", "cold", "first-existing", "detach", "takeover",
                  "recover-short", "recover-long", "host-network", "detach-other", "handoff-early", "handoff-late",
                  "race-connect", "race-takeover", "race-disconnect", "race-reconnect", "path-migrate", "relay-block"}
SDK = Path(os.environ.get("ANDROID_HOME", Path.home() / "Library/Android/sdk"))


def stamp():
    return dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")


def save(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, indent=2, ensure_ascii=False) + "\n")
    temporary.chmod(0o600)
    temporary.replace(path)


def sha256(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


class Lab:
    def __init__(self, args):
        self.args = args
        self.root = args.environment.resolve()
        self.manifest = self.root / "environment.json"
        self.env = json.loads(self.manifest.read_text()) if self.manifest.exists() else {}
        if self.env and (not re.fullmatch(r"zterm-md-\d{8}t\d{6}z", self.env.get("id", "")) or
                         self.env.get("vms") != {d: self.env["id"] + "-" + d.lower() for d in ACCOUNT}):
            raise ValueError("refusing an environment not owned by this disposable runner")
        self.logdir = self.root / "logs"
        self.logdir.mkdir(parents=True, exist_ok=True)
        self.sequence = 0
        self.sequence_lock = threading.Lock()

    def call(self, command, *, timeout=60, check=True, data=None, private=False, log=None):
        with self.sequence_lock:
            self.sequence += 1
            sequence = self.sequence
        start = time.monotonic()
        timed_out = None
        try:
            result = subprocess.run([str(arg) for arg in command], input=data, stdout=subprocess.PIPE,
                                    stderr=subprocess.PIPE, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            timed_out = error
            result = subprocess.CompletedProcess(command, -1, error.stdout or b"", error.stderr or b"")
        if not private:
            destination = log or self.logdir / f"{stamp()}-{os.getpid()}-{sequence:04}.log"
            Path(destination).write_bytes(
                (shlex.join(map(str, command)) + "\n").encode() + result.stdout + result.stderr)
            with self.sequence_lock, (self.logdir / "commands.jsonl").open("a") as stream:
                stream.write(json.dumps({"command": list(map(str, command)), "seconds": time.monotonic()-start,
                                         "exit": result.returncode, "timed_out": timed_out is not None,
                                         "log": str(destination)}) + "\n")
        if timed_out is not None:
            raise TimeoutError(f"command exceeded {timeout}s: {shlex.join(map(str, command))}") from timed_out
        if check and result.returncode:
            details = result.stderr.decode(errors="replace")[-1500:]
            if private:
                # A pairing ticket may be in either output stream. Keep only
                # these public typed codes, never the corresponding detail.
                kind = re.search(r"(?m)^Error: ([a-z_]+):", details)
                allowed = {"deadline_exceeded", "pair_outcome_unknown", "operation_outcome_unknown",
                           "address_unavailable", "daemon_start_timeout", "resource_exhausted",
                           "pair_ticket_invalid", "pair_ticket_expired", "unauthorized",
                           "transport_unavailable", "identity_state_mismatch", "cancelled"}
                details = "private output withheld"
                if kind and kind[1] in allowed:
                    details += "; error_kind=" + kind[1]
            raise RuntimeError(f"command failed ({result.returncode}): {shlex.join(map(str, command))}\n"
                               + details)
        return result.stdout.decode(errors="replace")

    def vm(self, device, *command, **options):
        return self.call(["limactl", "shell", "--workdir=/tmp", self.env["vms"][device], *command], **options)

    def user(self, device, *command, **options):
        return self.vm(device, "sudo", "-Hu", ACCOUNT[device], *command, **options)

    def cli(self, device, *command, **options):
        return self.user(device, "/usr/local/bin/zterm", *command, **options)

    def online(self, device):
        # Local endpoint preparation only; this never contacts a product peer.
        # Cold CLI controllers skip this wait and launch their daemon themselves.
        until(lambda: "Network:         Online" in self.cli(device, "status"), 60,
              f"{device} local endpoint Online")

    def install_linux(self, binary):
        expected = sha256(binary)
        for device in ACCOUNT:
            present = self.vm(device, "sh", "-c", "test ! -x /usr/local/bin/zterm || echo present").strip()
            if present:
                self.cli(device, "daemon", "stop", "-y", check=False)
                until(lambda: "Stopped" in self.cli(device, "status", check=False), 60,
                      f"{device} old daemon stopped before binary installation")
            self.call(["limactl", "copy", binary, self.env["vms"][device] + ":/tmp/zterm-linux-arm64"], timeout=90)
            self.vm(device, "sudo", "install", "-m", "755", "/tmp/zterm-linux-arm64", "/usr/local/bin/zterm.next")
            self.vm(device, "sudo", "mv", "/usr/local/bin/zterm.next", "/usr/local/bin/zterm")
            if self.vm(device, "sha256sum", "/usr/local/bin/zterm").split()[0] != expected:
                raise AssertionError("A/B binary digest mismatch")
            self.cli(device, "setup", "--name", "multi-device-" + device, "--profile", "official-n0")
            self.online(device)
        self.env["binary_sha256"] = expected
        self.persist()

    def adb(self, *command, **options):
        return self.call([SDK / "platform-tools/adb", "-s", self.env["serial"], *command], **options)

    def persist(self):
        save(self.manifest, self.env)

    def create(self):
        if self.env:
            raise RuntimeError("environment already exists; use start/build/run or another --environment")
        identifier = "zterm-md-" + stamp().lower()
        self.env = {"schema": 1, "id": identifier,
                    "vms": {d: identifier + "-" + d.lower() for d in ACCOUNT},
                    "avd": identifier + "-c", "serial": f"emulator-{self.args.port}",
                    "port": self.args.port, "created": stamp(), "profile": "official-n0"}
        self.persist()  # cleanup is possible even if creation fails halfway
        for device in ACCOUNT:
            config = self.root / f"{device}.yaml"
            config.write_text("""base:
  - template:_images/ubuntu-24.04
vmType: vz
arch: aarch64
cpus: 4
memory: 6GiB
disk: 60GiB
mounts: []
networks:
  - vzNAT: true
  - lima: user-v2
containerd:
  system: false
  user: false
""")
            self.call(["limactl", "create", "--tty=false", "--name=" + self.env["vms"][device], config], timeout=900)
        self.call([SDK / "cmdline-tools/latest/bin/avdmanager", "create", "avd", "--name", self.env["avd"],
                   "--package", "system-images;android-36;google_apis;arm64-v8a",
                   "--path", self.root / "avd"], data=b"no\n", timeout=90)
        config = self.root / "avd/config.ini"
        text = config.read_text()
        for key, value in {"hw.cpu.ncore": "2", "hw.ramSize": "3072", "hw.keyboard": "yes",
                           "hw.lcd.width": "720", "hw.lcd.height": "1280", "hw.lcd.density": "240",
                           "disk.dataPartition.size": "6G"}.items():
            text = re.sub(r"(?m)^" + re.escape(key) + r"=.*\n?", "", text)
            text += f"{key}={value}\n"
        config.write_text(text)
        self.start()

    def start(self):
        for device in ACCOUNT:
            print(f"Starting {device}: {self.env['vms'][device]}", flush=True)
            self.call(["limactl", "start", "--tty=false", self.env["vms"][device]], timeout=900)
            uid = 2001 if device == "A" else 2002
            self.vm(device, "sudo", "sh", "-c",
                    f"id {ACCOUNT[device]} >/dev/null 2>&1 || useradd -m -u {uid} -s /bin/bash {ACCOUNT[device]}")
        devices = self.call([SDK / "platform-tools/adb", "devices"])
        if self.env["serial"] in devices:
            actual = self.adb("emu", "avd", "name").splitlines()[0]
            if actual != self.env["avd"]:
                raise RuntimeError("explicit emulator port is occupied by another AVD")
        else:
            with (self.logdir / "emulator.log").open("ab") as output:
                process = subprocess.Popen([str(SDK / "emulator/emulator"), "-avd", self.env["avd"],
                    "-port", str(self.env["port"]), "-no-snapshot", "-no-boot-anim", "-no-audio",
                    "-no-window", "-gpu", "swiftshader"], stdout=output, stderr=subprocess.STDOUT,
                    start_new_session=True)
            self.env["emulator_pid"] = process.pid
            self.persist()
        self.adb("wait-for-device", timeout=180)
        deadline = time.monotonic() + 180
        while self.adb("shell", "getprop", "sys.boot_completed").strip() != "1":
            if time.monotonic() > deadline:
                raise TimeoutError("emulator boot")
            time.sleep(1)
        def rooted():
            try:
                if self.adb("shell", "id", "-u", check=False, timeout=5).strip() == "0":
                    return True
                # adbd itself restarts here and can close this request's
                # socket. Verify its resulting UID instead of treating that
                # management-channel close as an application recovery error.
                self.adb("root", check=False, timeout=5)
            except TimeoutError:
                pass
            return False
        try:
            until(rooted, 45, "owned emulator adbd restarted as root")
        except TimeoutError as error:
            raise Blocked("C management channel did not become root-ready") from error
        print("A/B/C started; adb root verified", flush=True)

    def build(self):
        revision = self.call(["git", "-C", ROOT, "rev-parse", "HEAD"]).strip()
        self.env["revision"] = revision
        source = self.root / ("source-" + uuid.uuid4().hex + ".tar.gz")
        names = self.call(["git", "-C", ROOT, "ls-files", "--cached", "--others", "--exclude-standard", "-z"]).split("\0")
        with tarfile.open(source, "w:gz") as archive:
            for name in names:
                if name and (ROOT / name).is_file():
                    archive.add(ROOT / name, arcname=name, recursive=False)
        self.env["source_sha256"] = sha256(source)
        archived = self.root / ("source-" + self.env["source_sha256"][:16] + ".tar.gz")
        source.replace(archived)
        source = archived
        self.env["source_archive"] = source.name
        changed = self.call(["git", "-C", ROOT, "diff", "--name-only", "HEAD", "-z"]).split("\0")
        changed += self.call(["git", "-C", ROOT, "ls-files", "--others", "--exclude-standard", "-z"]).split("\0")
        self.env["modified_sources"] = {name: sha256(ROOT / name) for name in sorted(set(changed))
                                        if name and (ROOT / name).is_file()}
        self.call(["limactl", "copy", source, self.env["vms"]["A"] + ":/tmp/zterm-source.tar.gz"], timeout=90)
        self.vm("A", "sudo", "sh", "-c",
                "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y build-essential pkg-config "
                "libssl-dev curl ca-certificates iptables python3", timeout=1200)
        self.vm("B", "sudo", "sh", "-c",
                "apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y iptables python3", timeout=1200)
        self.vm("A", "sh", "-c", "mkdir -p ~/zterm-build && tar xzf /tmp/zterm-source.tar.gz -C ~/zterm-build && "
                "(test -x ~/.cargo/bin/rustup || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none) && "
                "cd ~/zterm-build && ~/.cargo/bin/rustup toolchain install 1.98.0 --profile minimal --component clippy,rustfmt && "
                f"umask 077 && ZTERM_SOURCE_COMMIT={revision} CARGO_BUILD_JOBS=1 ~/.cargo/bin/cargo build --locked -p zterm-cli",
                timeout=3600, log=self.logdir / "linux-build.log")
        build_home = self.vm("A", "sh", "-c", "printf '%s' ~").strip()
        binary = self.root / ("zterm-linux-arm64-" + self.env["source_sha256"][:16])
        self.call(["limactl", "copy", self.env["vms"]["A"] + ":" + build_home + "/zterm-build/target/debug/zterm", binary], timeout=90)
        self.install_linux(binary)
        self.call(["sh", ROOT / "tools/android/build.sh", ":app:assembleDebug", ":app:assembleDebugAndroidTest"],
                  timeout=1800, log=self.logdir / "android-build.log")
        for name in ("debug/app-debug.apk", "androidTest/debug/app-debug-androidTest.apk"):
            apk = ROOT / "apps/android/app/build/outputs/apk" / name
            self.env[apk.name + "_sha256"] = sha256(apk)
            artifact = self.root / "artifacts" / self.env[apk.name + "_sha256"] / apk.name
            artifact.parent.mkdir(parents=True, exist_ok=True)
            if not artifact.exists():
                shutil.copyfile(apk, artifact)
            self.adb("install", "-r", apk, timeout=90)
        self.persist()
        self.preflight()

    def preflight(self):
        evidence = {"time": stamp(), "vms": {}}
        addresses = {}
        for device in ACCOUNT:
            interfaces = json.loads(self.vm(device, "ip", "-j", "address", "show", "dev", "lima0"))
            addresses[device] = next(address["local"] for interface in interfaces
                                     for address in interface["addr_info"] if address["family"] == "inet")
            evidence["vms"][device] = {
                "uname": self.vm(device, "uname", "-a"),
                "os": self.vm(device, "cat", "/etc/os-release"),
                "addresses": self.vm(device, "ip", "-j", "address"),
                "identity": self.cli(device, "status"),
                "binary_sha256": self.vm(device, "sha256sum", "/usr/local/bin/zterm").split()[0]}
            self.call(["ping", "-c", "1", "-W", "3000", addresses[device]])
            self.adb("shell", "ping", "-c", "1", "-W", "3", addresses[device])
        for device in ACCOUNT:
            peer = "B" if device == "A" else "A"
            self.vm(device, "ping", "-c", "1", "-W", "3", "lima-" + self.env["vms"][peer] + ".internal")
        evidence["android"] = self.adb("shell", "getprop")
        uid = self.adb("shell", "run-as", PACKAGE, "id", "-u").strip()
        if not uid.isdigit() or int(uid) < 10000:
            raise RuntimeError("invalid isolated Android app UID")
        self.env["android_uid"] = uid
        self.env["addresses"] = addresses
        self.persist()
        evidence["apk_sha256"] = {}
        for package, artifact in ((PACKAGE, "app-debug.apk"), (PACKAGE + ".test", "app-debug-androidTest.apk")):
            installed = self.adb("shell", "pm", "path", package).strip().removeprefix("package:")
            if not installed.startswith("/data/app/") or "\n" in installed:
                raise Blocked("expected one installed APK for " + package)
            evidence["apk_sha256"][artifact] = self.adb("shell", "sha256sum", installed).split()[0]
        evidence["udp"] = self.udp_preflight(addresses)
        save(self.root / "preflight.json", evidence)
        save(self.root / ("preflight-" + evidence["time"] + ".json"), evidence)
        digests = {value["binary_sha256"] for value in evidence["vms"].values()}
        if digests != {self.env.get("binary_sha256")}:
            raise Blocked("A/B binary digests differ from the recorded build")
        if any(digest != self.env.get(name + "_sha256") for name, digest in evidence["apk_sha256"].items()):
            raise Blocked("installed Android APK digests differ from the recorded build")
        if self.adb("shell", "id", "-u").strip() != "0":
            raise Blocked("Android fault injection requires adb root")
        failed = [direction for direction, passed in evidence["udp"].items() if not passed]
        if failed:
            raise Blocked("UDP preflight failed: " + ", ".join(failed))
        print("Preflight passed: A/B digests, Mac→VM→Mac, VM↔VM, C→VM→C, root/UID", flush=True)

    def udp_preflight(self, addresses):
        results = {}
        for device in ACCOUNT:
            port = 42071
            ready = "/tmp/zterm-md-udp-" + uuid.uuid4().hex
            server = ("import socket,pathlib\ns=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)\n"
                      f"s.bind(('0.0.0.0',{port}));s.settimeout(20)\n"
                      f"pathlib.Path({ready!r}).write_text('ready')\n"
                      "try:\n while True:\n  data,peer=s.recvfrom(1024);s.sendto(data,peer)\n"
                      "except TimeoutError: pass\n")
            pid = self.vm(device, "sh", "-c", "nohup python3 -c " + shlex.quote(server) +
                          " >/tmp/zterm-md-udp.log 2>&1 </dev/null & echo $!").strip()
            try:
                until(lambda: self.vm(device, "sh", "-c", "test ! -f " + ready + " || echo ready",
                                      check=False).strip() == "ready", 10, "UDP preflight listener bound")
                with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as client:
                    client.settimeout(3)
                    client.sendto(b"zterm-md-udp", (addresses[device], port))
                    try:
                        results["Mac→" + device] = client.recv(1024) == b"zterm-md-udp"
                    except TimeoutError:
                        results["Mac→" + device] = False
                response = self.adb("shell", "sh", "-c", shlex.quote(
                    f"printf zterm-md-udp | timeout 3 toybox nc -u -w 2 {addresses[device]} {port}"), check=False, timeout=8)
                results["C→" + device] = "zterm-md-udp" in response
                peer = "B" if device == "A" else "A"
                client_script = ("import socket; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.settimeout(3);"
                                 f"s.sendto(b'zterm-md-udp',('lima-{self.env['vms'][device]}.internal',{port}));"
                                 "print(s.recv(1024)==b'zterm-md-udp')")
                results[peer + "→" + device] = self.vm(peer, "python3", "-c", client_script, check=False).strip() == "True"
            finally:
                self.vm(device, "kill", pid, check=False)
                self.vm(device, "rm", "-f", ready, check=False)
        return results

    def stop(self):
        if not self.env:
            return
        cleanup(self)
        for device in ACCOUNT:
            self.call(["limactl", "stop", "--tty=false", self.env["vms"][device]], timeout=120, check=False)
        devices = self.call([SDK / "platform-tools/adb", "devices"])
        if self.env["serial"] in devices:
            actual = self.adb("emu", "avd", "name", check=False).splitlines()[0]
            if actual == self.env["avd"]:
                self.adb("emu", "kill", check=False)
            else:
                raise RuntimeError("refusing to stop an emulator belonging to another AVD")
        states = {device: json.loads(self.call(["limactl", "list", "--json", vm]))["status"]
                  for device, vm in self.env["vms"].items()}
        until(lambda: self.env["serial"] not in self.call([SDK / "platform-tools/adb", "devices"]),
              30, "isolated emulator stopped")
        save(self.root / "stopped.json", {"time": stamp(), "vms": states, "emulator": "stopped",
             "fault_journals": [path.name for path in (self.root / "faults").glob("*.json")]})
        if any(state != "Stopped" for state in states.values()):
            raise RuntimeError("one or more isolated VMs failed to stop: " + str(states))
        print("Stopped isolated instances; data retained at " + str(self.root), flush=True)

    def collect(self):
        destination = BASE / (self.args.run_id or ("collection-" + stamp()))
        destination.mkdir(parents=True, exist_ok=False)
        save(destination / "environment.json", self.env)
        for device in ACCOUNT:
            for name, command in (("status", ("status",)), ("sessions", ("session", "list")),
                                  ("logs", ("logs", "--json", "--include-debug", "-n", "1000"))):
                (destination / f"{device}-{name}.txt").write_text(self.cli(device, *command, check=False))
        for name in ("events.jsonl", "detail.jsonl"):
            (destination / ("android-" + name)).write_text(self.adb(
                "shell", "run-as", PACKAGE, "cat", "no_backup/diagnostics/" + name, check=False, private=True))
        (destination / "logcat.txt").write_text(self.adb("logcat", "-d", "-t", "3000", check=False))
        print(destination, flush=True)


class Blocked(RuntimeError):
    """An environment prerequisite was unavailable; never a passing case."""


def until(predicate, seconds=60, description="condition"):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = predicate()
        if value and time.monotonic() <= deadline:
            return value
        time.sleep(0.15)
    raise TimeoutError(description)


class ScreenInput:
    """Drop unsupported extension queries without turning parameters into cells.

    Keep incomplete CSI sequences between PTY reads. Pyte predates Kitty's
    CSI = flags and DEC's $p queries; their replies aren't needed for input.
    """
    def __init__(self):
        self.pending = ""

    def feed(self, text):
        text = self.pending + text
        self.pending = ""
        tail = re.search(r"\x1b(?:\[[0-?]*[ -/]*)?$", text)
        if tail:
            self.pending, text = text[tail.start():], text[:tail.start()]
        return re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]",
                      lambda match: "" if any(c in match[0][2:-1] for c in "=><$:") else match[0], text)


class LinuxTerminal:
    def __init__(self, lab, device, host, output, session=None, takeover=False, create=None):
        import pyte  # requirements-multi-device.txt; environment/build need only stdlib
        class Screen(pyte.Screen):
            # Zterm probes DEC private status/color capabilities. Pyte 0.8.2
            # parses those CSI sequences but its report callbacks lack kwargs.
            def report_device_status(self, mode, **options):
                if not options.get("private"):
                    super().report_device_status(mode)

            def report_device_attributes(self, mode=0, **options):
                if not options.get("private"):
                    super().report_device_attributes(mode)
        self.lab, self.device, self.host = lab, device, host
        # Soak reopens can empty the live-terminal list. A fresh suffix keeps
        # earlier PTY evidence even when a caller reuses its list index.
        self.output = output.with_name(output.stem + "-" + uuid.uuid4().hex[:12] + output.suffix)
        self.screen = Screen(100, 30)
        self.stream = pyte.Stream(self.screen)
        self.lock = threading.Lock()
        self.raw = bytearray()
        self.reader_error = None
        self.closed = False
        self.close_outcome = None
        self.state_event = {}
        self.started_at = dt.datetime.now(dt.timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
        self.last_observation = 0.0
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 30, 100, 0, 0))
        vm = lab.env["vms"][device]
        target = "local" if host == device else "multi-device-" + host
        command = (["/usr/local/bin/zterm", "session", "create", "--target", target, create] if create
                   else ["/usr/local/bin/zterm", "connect", target])
        if session:
            command += ["--session", session]
        if takeover:
            command += ["--takeover"]
        shell = "printf '[ZTERM_MD_PID=%s]\\n' \"$$\"; exec " + shlex.join(command)
        command = ["sudo", "-Hu", ACCOUNT[device], "env", "TERM=xterm-256color", "sh", "-c", shell]
        self.process = subprocess.Popen(["ssh", "-F", str(Path.home() / ".lima" / vm / "ssh.config"),
                                        "-tt", "lima-" + vm, shlex.join(command)],
                                       stdin=slave, stdout=slave, stderr=slave, start_new_session=True)
        os.close(slave)
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def _read(self):
        decoder = codecs.getincrementaldecoder("utf-8")("replace")
        sanitizer = ScreenInput()
        with self.output.open("wb") as log:
            while True:
                try:
                    if not select.select([self.master], [], [], 0.2)[0]:
                        if self.process.poll() is not None:
                            break
                        continue
                    data = os.read(self.master, 65536)
                    if not data:
                        break
                    log.write(data)
                    log.flush()
                    with self.lock:
                        self.raw.extend(data)
                        self.stream.feed(sanitizer.feed(decoder.decode(data)))
                except (OSError, ValueError):
                    break
                except Exception as error:
                    self.reader_error = str(error)
                    break

    def snapshot(self):
        exit_status = self.process.poll()
        if exit_status is not None:
            # ssh can exit just before the reader consumes the final error.
            # Observe process exit and its drained terminal output together.
            self.reader.join(timeout=2)
        if self.reader_error:
            raise RuntimeError("PTY screen decoder failed: " + self.reader_error)
        with self.lock:
            text = "\n".join(self.screen.display)
            raw = self.raw.decode(errors="replace")
            # These fixtures run a main-screen shell, whose cursor remains
            # visible when healthy. The CLI hides it while its input fence is
            # closed, including deferred resize after the driver says Active.
            # Do not infer readiness from the lower-level driver event alone.
            presented_ready = not self.screen.cursor.hidden
        pid = re.search(r"\[ZTERM_MD_PID=(\d+)\]", raw)
        if pid and time.monotonic()-self.last_observation > 0.5 and exit_status is None:
            self.last_observation = time.monotonic()
            events = self.lab.cli(self.device, "logs", "--json", "--component", "terminal", "--since", "5m", "-n", "1000", private=True)
            for line in events.splitlines():
                event = json.loads(line)
                if (event.get("pid") == int(pid[1]) and event.get("event") == "view_state_changed"
                        and event.get("timestamp", "") >= self.started_at):
                    self.state_event = event
        path = re.findall(r"\| (direct|relay|local)\b", text)
        state = "closed" if exit_status is not None else self.state_event.get("fields", {}).get("stage", "preparing")
        return {"text": text, "path": path[-1] if path else "unknown",
                "state": state, "inputReady": state == "active" and presented_ready,
                "stateEvent": self.state_event,
                "rawError": raw[-16000:] if exit_status is not None else "",
                "exit": exit_status}

    def send(self, text):
        if self.process.poll() is not None:
            return False
        os.write(self.master, text.encode())
        return True

    def resize(self, rows, columns):
        if self.closed or self.process.poll() is not None:
            return
        # These scenarios run a main-screen shell. The CLI reserves one
        # physical row for status and one column for its scrollbar gutter;
        # callers specify the child viewport, as Android's measure API does.
        rows, columns = rows + 1, columns + 1
        with self.lock:
            self.screen.resize(rows, columns)
        fcntl.ioctl(self.master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))
        # Popen's new session does not make the inherited slave a controlling
        # terminal. Notify ssh explicitly so it forwards the changed PTY size.
        with contextlib.suppress(ProcessLookupError):
            os.kill(self.process.pid, signal.SIGWINCH)

    def close(self):
        if self.closed:
            return self.close_outcome
        started = time.monotonic()
        forced = False
        if self.process.poll() is None:
            self.send("\x1d.")
            try:
                self.process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                forced = True
                os.killpg(self.process.pid, signal.SIGTERM)
                self.process.wait(timeout=5)
        self.reader.join(timeout=2)
        with contextlib.suppress(OSError):
            os.close(self.master)
        self.closed = True
        self.close_outcome = {"exit": self.process.poll(), "forced": forced,
                              "seconds": time.monotonic()-started}
        return self.close_outcome


class Android:
    def __init__(self, lab, output):
        self.lab, self.output = lab, output
        self.counter = 0
        self.process = None

    def start(self):
        self.token = uuid.uuid4().hex
        self.directory = "cache/multi-device-" + self.token
        self.lab.adb("shell", "am", "force-stop", PACKAGE)
        self.log = (self.output / ("android-" + self.token + ".log")).open("w")
        self.process = subprocess.Popen([str(SDK / "platform-tools/adb"), "-s", self.lab.env["serial"],
            "shell", "am", "instrument", "-w", "-r", "-e", "multiDevice", self.token,
            "-e", "class", "io.github.leonfox28.zterm.MultiDeviceTest", RUNNER], stdout=self.log, stderr=subprocess.STDOUT)
        def ready():
            if self.process.poll() is not None:
                raise RuntimeError("Android bridge exited; inspect instrumentation log")
            return self.read("ready") == "ready"
        until(ready, 90, "Android bridge ready")

    def read(self, name):
        return self.lab.adb("shell", "run-as", PACKAGE, "cat", self.directory + "/" + name,
                            check=False, private=True).strip()

    def command(self, op, **values):
        self.counter += 1
        payload = json.dumps({"id": self.counter, "op": op, **values}).encode()
        self.lab.adb("shell", "run-as", PACKAGE, "sh", "-c",
                     shlex.quote(f"cat > {self.directory}/command.tmp && mv {self.directory}/command.tmp {self.directory}/command.json"),
                     data=payload, private=True)
        def response():
            if self.process is None or self.process.poll() is not None:
                raise RuntimeError("Android instrumentation exited during " + op + "; inspect its process log")
            text = self.read("response.json")
            if not text:
                return None
            result = json.loads(text)
            return result if result["id"] == self.counter else None
        result = until(response, 65, f"Android {op}")
        with (self.output / "android-commands.jsonl").open("a") as log:
            log.write(json.dumps({"op": op, **values, "result": result}) + "\n")
        if not result["ok"]:
            raise RuntimeError(f"Android {op}: {result}")
        return result

    def snapshot(self, *, require_running=True):
        if require_running and (self.process is None or self.process.poll() is not None):
            raise RuntimeError("Android instrumentation exited during snapshot; inspect its process log")
        value = self.read("status.json")
        return json.loads(value) if value else {}

    def send(self, text):
        return self.command("input", text=text)["accepted"]

    def resize(self, rows, columns):
        self.command("resize", rows=rows, columns=columns)

    def close(self, *, killed=False):
        if self.process and self.process.poll() is None:
            if not killed:
                with contextlib.suppress(Exception):
                    self.command("stop")
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.lab.adb("shell", "am", "force-stop", PACKAGE)
                self.process.wait(timeout=10)
        self.process = None
        if hasattr(self, "log"):
            self.log.close()


class Fault:
    """Isolate the test UID without cutting Lima SSH or adb management.

    Offline additionally drops INPUT to its current socket ports: owner matches
    cannot run in INPUT. New remote flows cannot form while OUTPUT is blocked.
    Journal every rule before mutation; EOF guards also recover after SIGKILL.
    """
    def __init__(self, lab, device, mode):
        self.lab, self.device, self.mode = lab, device, mode
        self.chain = "ZMD_" + uuid.uuid4().hex[:12]
        self.journal = lab.root / "faults" / (self.chain + ".json")
        self.uid = lab.env["android_uid"] if device == "C" else str(2001 if device == "A" else 2002)
        self.guard = None
        self.ingress = {}

    @classmethod
    def restore(cls, lab, path):
        record = json.loads(path.read_text())
        if record.get("device") not in ("A", "B", "C") or record.get("mode") not in ("offline", "egress", "udp", "relay"):
            raise ValueError("invalid scoped fault journal")
        fault = cls(lab, record["device"], record["mode"])
        chain = record.get("chain", "")
        if (not re.fullmatch(r"ZMD_[a-f0-9]{12}", chain) or path.name != chain + ".json"
                or record.get("uid") != fault.uid):
            raise ValueError("invalid fault chain or test UID")
        ingress = record.get("ingress", {})  # Restore older OUTPUT-only journals too.
        if not isinstance(ingress, dict) or set(ingress) - {"iptables", "ip6tables"}:
            raise ValueError("invalid ingress address families")
        for entries in ingress.values():
            if not isinstance(entries, list) or any(
                not isinstance(entry, list) or len(entry) != 2 or entry[0] not in ("tcp", "udp")
                or type(entry[1]) is not int or not 1024 <= entry[1] <= 65535
                or entry[1] in (5037, 5555) for entry in entries
            ):
                raise ValueError("invalid scoped ingress port")
        fault.chain, fault.journal, fault.ingress = chain, path, ingress
        return fault

    def capture_ingress(self):
        ports = set()
        for protocol in ("tcp", "udp"):
            for suffix in ("", "6"):
                output = self.call("cat", "/proc/net/" + protocol + suffix, private=True)
                for line in output.splitlines():
                    fields = line.split()
                    if len(fields) < 8 or fields[7] != self.uid:
                        continue
                    port = int(fields[1].rsplit(":", 1)[1], 16)
                    if port < 1024 or port in (5037, 5555):
                        raise Blocked("test UID unexpectedly owns a reserved management port")
                    ports.add((protocol, port))
        if not ports:
            raise Blocked("offline injection found no owned network sockets")
        # IPv6 wildcard sockets can also receive IPv4-mapped traffic. Cover the
        # union in both families, retaining only owned protocol/port metadata.
        self.ingress = {binary: [list(port) for port in sorted(ports)]
                        for binary in ("iptables", "ip6tables")}

    def start_guard(self):
        # EOF survives SIGKILL and avoids PID-reuse races. Child tools do not
        # inherit the pipe; only this coordinator keeps its write end alive.
        with (self.lab.root / "logs" / (self.chain + "-guard.log")).open("ab") as log:
            self.guard = subprocess.Popen([sys.executable, __file__, "guard", "--environment", str(self.lab.root),
                "--fault-chain", self.chain], stdin=subprocess.PIPE, stdout=log, stderr=log, start_new_session=True)

    def call(self, *args, **kwargs):
        if self.device == "C":
            return self.lab.adb("shell", *args, **kwargs)
        return self.lab.vm(self.device, "sudo", *args, **kwargs)

    def __enter__(self):
        if self.mode == "offline":
            self.capture_ingress()
        save(self.journal, {"device": self.device, "uid": self.uid, "chain": self.chain,
                            "mode": self.mode, "ingress": self.ingress})
        try:
            self.start_guard()
            for binary in ("iptables", "ip6tables"):
                self.call(binary, "-w", "-N", self.chain)
                self.call(binary, "-w", "-A", self.chain, "-o", "lo", "-j", "RETURN")
                if self.mode == "udp":
                    self.call(binary, "-w", "-A", self.chain, "-p", "udp", "!", "--dport", "53", "-j", "DROP")
                elif self.mode == "relay":
                    self.call(binary, "-w", "-A", self.chain, "-p", "tcp", "-j", "DROP")
                elif self.mode in ("offline", "egress"):
                    self.call(binary, "-w", "-A", self.chain, "-j", "DROP")
                else:
                    raise ValueError(self.mode)
                self.call(binary, "-w", "-I", "OUTPUT", "1", "-m", "owner", "--uid-owner", self.uid, "-j", self.chain)
                if self.ingress.get(binary):
                    chain = self.chain + "_I"
                    self.call(binary, "-w", "-N", chain)
                    self.call(binary, "-w", "-A", chain, "-i", "lo", "-j", "RETURN")
                    self.call(binary, "-w", "-A", chain, "-j", "DROP")
                    for protocol, port in self.ingress[binary]:
                        self.call(binary, "-w", "-I", "INPUT", "1", "-p", protocol,
                                  "--dport", str(port), "-j", chain)
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def __exit__(self, *unused):
        counters = {"device": self.device, "uid": self.uid, "chain": self.chain,
                    "mode": self.mode, "observed": stamp(), "families": {},
                    "ingress": self.ingress, "ingress_families": {}}
        errors = []
        for binary in ("iptables", "ip6tables"):
            chains = [(self.chain, "families")]
            if self.ingress.get(binary):
                chains.append((self.chain + "_I", "ingress_families"))
            for chain, key in chains:
                try:
                    counters[key][binary] = self.call(binary, "-w", "-L", chain, "-n", "-v", "-x", check=False)
                except Exception as error:
                    counters[key][binary] = {"collection_error": str(error)}
            commands = [("-D", "OUTPUT", "-m", "owner", "--uid-owner", self.uid, "-j", self.chain)]
            commands += [("-D", "INPUT", "-p", protocol, "--dport", str(port), "-j", self.chain + "_I")
                         for protocol, port in self.ingress.get(binary, [])]
            commands += [(op, chain) for chain, _ in chains for op in ("-F", "-X")]
            # A failure in one family must not skip restoring the other one.
            for command in commands:
                try:
                    self.call(binary, "-w", *command, check=False)
                except Exception as error:
                    errors.append(str(error))
            try:
                if self.chain in self.call(binary, "-w", "-S"):
                    errors.append("fault cleanup incomplete: " + binary + "/" + self.chain)
            except Exception as error:
                errors.append(str(error))
        evidence = self.lab.root / "fault-evidence" / (self.chain + ".json")
        if not evidence.exists():
            save(evidence, counters)
        if errors:
            if self.guard:
                self.guard.stdin.close()  # EOF asks the independent guard to retry.
            raise RuntimeError("fault cleanup failed: " + "; ".join(errors))
        self.journal.unlink(missing_ok=True)
        if self.guard:
            with contextlib.suppress(BrokenPipeError):
                self.guard.stdin.write(b"restored\n")
                self.guard.stdin.close()
            self.guard.wait(timeout=10)
            if self.guard.returncode:
                raise RuntimeError("fault cleanup guard failed: " + self.chain)
            self.guard = None


class RelayFaults:
    """Remove UDP from every participating test UID for an unambiguous Relay run."""
    def __init__(self, lab, devices):
        self.lab, self.devices, self.faults = lab, devices, []

    def __enter__(self):
        try:
            for device in self.devices:
                self.faults.append(Fault(self.lab, device, "udp").__enter__())
            return self
        except BaseException:
            self.__exit__()
            raise

    def reapply(self, device):
        # A guest reboot clears its kernel firewall; the other devices keep
        # their rules and management connections throughout the reboot.
        for index, fault in enumerate(self.faults):
            if fault.device == device:
                fault.__exit__()
                self.faults[index] = Fault(self.lab, device, "udp").__enter__()

    def __exit__(self, *unused):
        errors = []
        for fault in reversed(self.faults):
            try:
                fault.__exit__()
            except BaseException as error:
                errors.append(error)
        if errors:
            raise RuntimeError("Relay fault cleanup failed: " + "; ".join(map(str, errors)))
        self.faults.clear()


def cleanup(lab):
    for path in (lab.root / "faults").glob("*.json"):
        Fault.restore(lab, path).__exit__()
    for path in (lab.root / "identities").glob("*.json"):
        restore_identity(lab, path)


def restore_identity(lab, journal):
    record = json.loads(journal.read_text())
    device, token = record["device"], record["token"]
    if device not in ("A", "B", "C") or not re.fullmatch(r"[a-f0-9]{32}", token):
        raise ValueError("invalid disposable identity journal")
    if device == "C":
        lab.adb("shell", "am", "force-stop", PACKAGE)
        command = (f"if [ -d no_backup/md-original-{token} ]; then "
                   f"if [ -d no_backup/zterm ]; then mv no_backup/zterm no_backup/md-tested-{token}; fi; "
                   f"mv no_backup/md-original-{token} no_backup/zterm; fi")
        lab.adb("shell", "run-as", PACKAGE, "sh", "-c", shlex.quote(command))
    else:
        lab.cli(device, "daemon", "stop", "-y", check=False)
        command = (f"cd /home/{ACCOUNT[device]} && if [ -d .zterm-md-original-{token} ]; then "
                   f"if [ -d .zterm ]; then mv .zterm .zterm-md-tested-{token}; fi; "
                   f"mv .zterm-md-original-{token} .zterm; fi")
        lab.user(device, "sh", "-c", command)
        lab.cli(device, "setup", "--name", "multi-device-" + device, "--profile", "official-n0")
    journal.unlink()


@contextlib.contextmanager
def fresh_identity(lab, device):
    token = uuid.uuid4().hex
    journal = lab.root / "identities" / (token + ".json")
    save(journal, dict(device=device, token=token))
    try:
        if device == "C":
            lab.adb("shell", "am", "force-stop", PACKAGE)
            # Preserve the Keystore key. Both independently generated seeds
            # remain wrapped at rest; no private identity bytes leave C.
            lab.adb("shell", "run-as", PACKAGE, "mv", "no_backup/zterm", "no_backup/md-original-" + token)
        else:
            lab.cli(device, "daemon", "stop", "-y")
            lab.user(device, "mv", f"/home/{ACCOUNT[device]}/.zterm", f"/home/{ACCOUNT[device]}/.zterm-md-original-{token}")
            lab.cli(device, "setup", "--name", "multi-device-" + device, "--profile", "official-n0")
            lab.online(device)
        yield
    finally:
        restore_identity(lab, journal)


def guard(lab, stream=sys.stdin.buffer):
    if not re.fullmatch(r"ZMD_[a-f0-9]{12}", lab.args.fault_chain or ""):
        raise ValueError("invalid private fault chain")
    if stream.readline() == b"restored\n":
        return
    path = lab.root / "faults" / (lab.args.fault_chain + ".json")
    if path.exists():
        Fault.restore(lab, path).__exit__()


def matrix(repeat):
    """Explicit rows make omissions and path prerequisites reviewable."""
    rows = []
    for iteration in range(1, repeat + 1):
        for host in ACCOUNT:
            remote = "B" if host == "A" else "A"
            rows.append(dict(scenario="local", host=host, old=host, new=host, path="local", iteration=iteration))
            for path in ("direct", "relay"):
                for client in (remote, "C"):
                    for name in ("fresh-pair", "cold", "first-existing", "detach", "recover-short", "recover-long", "host-network", "client-kill", "client-reboot",
                                 "session-close", "host-daemon-crash", "host-reboot", "revoke", "output-resize", "output-resize-takeover"):
                        new = ("C" if client == remote else remote) if name == "output-resize-takeover" else client
                        rows.append(dict(scenario=name, host=host, old=client, new=new, path=path, iteration=iteration))
                    if client != "C":
                        rows.append(dict(scenario="client-daemon-crash", host=host, old=client, new=client, path=path, iteration=iteration))
                for old, new in ((host, remote), (remote, host), (host, "C"), ("C", host), (remote, "C"), ("C", remote)):
                    rows.append(dict(scenario="takeover", host=host, old=old, new=new, path=path, iteration=iteration))
                for old, new in ((remote, "C"), ("C", remote)):
                    for name in ("detach-other", "handoff-early", "handoff-late", "race-connect", "race-takeover", "race-disconnect", "race-reconnect"):
                        rows.append(dict(scenario=name, host=host, old=old, new=new, path=path, iteration=iteration))
                for name in ("lifecycle-allow", "lifecycle-deny", "wifi-data", "upload-disconnect", "upload-takeover"):
                    rows.append(dict(scenario=name, host=host, old="C", new=remote, path=path, iteration=iteration))
            for client in (remote, "C"):
                for name in ("path-migrate", "relay-block"):
                    rows.append(dict(scenario=name, host=host, old=client, new=client, path="direct", iteration=iteration))
    return rows


def selected_matrix(args):
    rows = matrix(args.repeat) if args.action != "soak" else []
    selected = set(args.scenario.split(",")) if args.scenario else None
    if selected:
        unknown = selected - {r["scenario"] for r in rows}
        if unknown:
            raise ValueError("unknown scenarios: " + ",".join(sorted(unknown)))
        rows = [r for r in rows if r["scenario"] in selected]
    if args.host:
        rows = [r for r in rows if r["host"] == args.host]
    if args.path:
        rows = [r for r in rows if r["path"] == args.path]
    if args.controller:
        rows = [r for r in rows if r["old"] == args.controller]
    if args.group != "all":
        rows = [r for r in rows if (r["scenario"] in CORE_SCENARIOS) == (args.group == "core")]
    return rows


class Suite:
    def __init__(self, lab):
        self.lab = lab
        self.args = lab.args
        self.root = BASE / (self.args.run_id or stamp())
        self.root.mkdir(parents=True, exist_ok=False)
        self.results = []
        self.incidents = []
        self.android = Android(lab, self.root)
        self.terminals = []
        self.case = None
        self.metrics = {}
        self.executions = {}
        self.path_fault = None
        self.path_mode = None
        self.soak_status = None
        self.soak_connection_failures = []
        self.rng = random.Random(self.args.seed)
        source = Path(__file__).read_text()
        (self.root / "runner-source.py").write_text(source)
        # Keep tracebacks tied to this loaded run even during later harness edits.
        linecache.cache[__file__] = (len(source), None, source.splitlines(keepends=True), __file__)
        save(self.root / "manifest.json", {**lab.env, "seed": self.args.seed, "repeat": self.args.repeat,
            "group": self.args.group, "filters": {key: getattr(self.args, key) for key in ("scenario", "host", "path", "controller")},
            "relay_scope": getattr(self.args, "relay_scope", "host"),
            "offline_scope": ("test-uid-egress" if getattr(self.args, "offline_scope", "bidirectional") == "egress"
                              else "owned-sockets-bidirectional"),
            "timing_contract": "absolute-offline-deadlines-v1",
            "cold_start_contract": "cli-autostart-daemon-v1",
            "host_readiness_contract": "local-host-online-before-scenario-v1",
            "android_selection_contract": "retain-current-session-frame-v1",
            "pair_reconciliation_contract": "host-authorization-before-revoke-v1",
            "pair_cleanup_barrier_contract": "failed-accept-host-process-exit-v1",
            "session_cleanup_barrier_contract": "unknown-session-host-process-exit-v1",
            "short_outage_traffic_contract": "shell-output-and-controller-resize-v1",
            "short_outage_viewport_contract": "host-and-android-surface-v1",
            "post_pair_session_contract": "host-session-query-after-accepted-pair-v1",
            "soak_reopen_contract": "alternate-controllers-original-id-v1",
            "soak_action_status_contract": "distinct-failed-and-blocked-v1",
            "soak_seconds": self.args.soak_seconds, "detect_limit": 120, "recover_limit": 60,
            "runner_sha256": sha256(Path(__file__)), "started": stamp()})

    def pair(self, host, client):
        # Pair tickets are never argv, log entries, or persisted evidence.
        ticket = self.lab.cli(host, "pair", "create", private=True).strip().encode()
        if client == "C":
            self.lab.adb("shell", "run-as", PACKAGE, "sh", "-c",
                         shlex.quote("cat > files/multi-device-ticket.txt"), data=ticket, private=True)
            result = self.android.command("pair")
            if result["snapshot"].get("error"):
                raise AssertionError(result)
        else:
            self.lab.cli(client, "pair", "accept", "--stdin", "--alias", "multi-device-" + host,
                         data=ticket)

    def setup(self):
        cleanup(self.lab)
        self.lab.adb("shell", "pm", "grant", PACKAGE, "android.permission.POST_NOTIFICATIONS")
        self.android.start()
        self.android.command("home")
        self.android.command("keepBackground", enabled=True)
        save(self.root / "android-baseline.json", {"keep_background": True, "notification_permission": "granted",
             "note": "lifecycle rows explicitly change both settings; fresh identities retain product defaults"})
        self.unavailable = {}
        pairing = []
        for host, client in (("A", "B"), ("B", "A"), ("A", "C"), ("B", "C")):
            # Do not consume a new mutation when an existing pairing is usable.
            if client == "C":
                known = any(h["name"] == "multi-device-" + host for h in self.android.snapshot().get("hosts", []))
            else:
                known = "multi-device-" + host in self.lab.cli(client, "device", "list")
            if not known:
                started = time.monotonic()
                try:
                    self.pair(host, client)
                    pairing.append(dict(host=host, client=client, status="passed", seconds=time.monotonic()-started))
                except Exception as error:
                    self.unavailable[host, client] = str(error)
                    pairing.append(dict(host=host, client=client, status="failed", error=str(error), seconds=time.monotonic()-started))
                save(self.root / "pairing.json", pairing)
        for device in ACCOUNT:
            (self.root / f"{device}-authorization.txt").write_text(self.lab.cli(device, "device", "list"))

    def sessions(self, host):
        output = self.lab.cli(host, "session", "list")
        return [(m[0], m[1], int(m[2]), int(m[3]), m[4]) for m in
                re.findall(r"(?m)^(\S+)\s+(Attached|Detached)\s+(\d+)x(\d+)\s*\n\s*ID: (\S+)", output)]

    def devices(self, host):
        listing = self.lab.cli(host, "device", "list")
        records = re.findall(r"(?m)^(.+?)\s+(?:Connected|Not connected)\s+(Yes|No)\s+(Yes|No|Revoked)\n\s*ID: (\S+)", listing)
        return {record[3]: dict(name=record[0].strip(), known=record[1], allowed=record[2]) for record in records}

    def original_session(self):
        rows = [row for row in self.sessions(self.host) if row[4] == self.session]
        if len(rows) != 1:
            raise AssertionError("original Session is missing or ambiguous")
        return rows[0]

    def revocation_principal(self, device):
        if device == "C":
            principal = self.android.snapshot().get("deviceId", "")
        else:
            match = re.search(r"(?m)^Device ID:\s*([0-9a-f]{64})\s*$", self.lab.cli(device, "status"))
            principal = match[1] if match else ""
        if not re.fullmatch(r"[0-9a-f]{64}", principal):
            raise Blocked("controller public device identity unavailable; rebuild the instrumentation if needed")
        records = self.devices(self.host)
        if records.get(principal, {}).get("allowed") != "Yes":
            raise AssertionError("exact controller has no active host authorization")
        return principal

    def fresh_pair(self, row):
        device = row["old"]
        self.android.close()
        before = self.devices(self.host)
        try:
            with fresh_identity(self.lab, device):
                attempt = {"host": self.host, "client": device, "submitted": False, "returned": False}
                try:
                    self.android.start()
                    if device == "C" and self.android.snapshot().get("hosts"):
                        raise AssertionError("fresh Android identity retained an address book")
                    attempt["submitted"] = True
                    save(self.case / "pair-attempt.json", attempt)
                    self.pair(self.host, device)
                    attempt["returned"] = True
                    save(self.case / "pair-attempt.json", attempt)
                    after = self.devices(self.host)
                    added = set(after) - set(before)
                    if len(added) != 1:
                        raise AssertionError("pairing did not add one new independent identity")
                    principal = added.pop()
                    if after[principal]["known"] != "No" or after[principal]["allowed"] != "Yes":
                        raise AssertionError("pairing implicitly granted the reverse direction")
                    save(self.case / "pair-direction.json", {"host": after, "fresh_client": principal})
                    if device != "C":
                        records = self.devices(device)
                        if len(records) != 1 or next(iter(records.values()))["allowed"] != "No":
                            raise AssertionError("accepting a ticket implicitly granted inbound access")
                        save(self.case / "client-direction.json", records)
                    terminal = self.connect(device, self.host)
                    self.path(terminal, row["path"])
                    self.establish(terminal)
                    self.collect()
                    self.detach(terminal)
                finally:
                    original_error = sys.exc_info()[1]
                    cleanup_errors = []
                    actions = [lambda: save(self.case / "pair-attempt.json", attempt),
                               lambda: self.collect(self.case / "fresh-identity")]
                    if attempt["submitted"] and not attempt["returned"]:
                        actions.append(self.quiesce_failed_pair_host)
                    actions.extend([lambda: self.reconcile_fresh_pair(before), self.android.close])
                    for action in actions:
                        try:
                            action()
                        except BaseException as error:
                            cleanup_errors.append(error)
                    if cleanup_errors:
                        save(self.case / "fresh-pair-cleanup-errors.json", [repr(error) for error in cleanup_errors])
                        if original_error is None:
                            raise cleanup_errors[0]
        finally:
            self.android.start()

    def quiesce_failed_pair_host(self):
        # The remote accept can commit after the caller's deadline. Stop this
        # disposable host before reconciling so its old in-memory offer and
        # workers cannot grant the temporary principal after a cleanup query.
        # This is failed-case cleanup, outside the measured product operation.
        try:
            self.quiesce_failed_host("pair-cleanup-barrier.json", "failed pair acceptance")
        except BaseException as error:
            # fresh_pair preserves the original pairing error while still
            # attempting the remaining cleanup. The matrix must nevertheless
            # stop if these old workers have not been confirmed gone.
            self.pair_cleanup_error = str(error)
            raise

    def quiesce_failed_host(self, evidence_name, reason):
        """Fence admitted operations before another independent matrix row."""
        start = time.monotonic()
        record = {"host": self.host, "method": "test-uid-daemon-process-exit",
                  "reason": reason, "confirmed_stopped": False}
        uid = 2001 if self.host == "A" else 2002
        probe = ("import json,pathlib\npids=[]\n"
                 "for p in pathlib.Path('/proc').iterdir():\n"
                 " if not p.name.isdigit(): continue\n"
                 " try:\n"
                 f"  if p.stat().st_uid != {uid}: continue\n"
                 "  args=(p/'cmdline').read_bytes().split(b'\\0')\n"
                 "  if args[:2]==[b'/usr/local/bin/zterm',b'--internal-daemon']: pids.append(int(p.name))\n"
                 " except (FileNotFoundError,ProcessLookupError): pass\n"
                 "print(json.dumps(pids))\n")
        try:
            try:
                self.lab.cli(self.host, "daemon", "stop", "-y")
            except Exception as error:
                record["stop_command_error"] = str(error)
            def stopped():
                pids = json.loads(self.lab.vm(self.host, "sudo", "python3", "-c", probe))
                if not isinstance(pids, list) or any(not isinstance(pid, int) or pid <= 0 for pid in pids):
                    raise RuntimeError("invalid host process observation during operation cleanup")
                record["remaining_processes"] = pids
                return not pids
            until(stopped, max(0, 60 - (time.monotonic() - start)),
                  "failed operation host workers exited before the next independent row")
            record["confirmed_stopped"] = True
        finally:
            record["seconds"] = time.monotonic() - start
            save(self.case / evidence_name, record)

    def reconcile_fresh_pair(self, before):
        # A Session list cannot establish whether a pair operation committed.
        # Retain the authorization observation before revoking the test identity.
        evidence = self.case / "outcome-reconciliation.json"
        record = {"operation": "pair-accept", "host": self.host,
                  "method": "read-only host authorization list; no pairing retry",
                  "before_authorizations": before, "cleanup_revoked": []}
        try:
            observed = self.devices(self.host)
        except Exception as error:
            save(evidence, {**record, "query_error": str(error)})
            raise
        principals = sorted(set(observed) - set(before))
        record.update(observed_authorizations=observed, new_principals=principals)
        save(evidence, record)
        errors = []
        for principal in principals:
            try:
                self.lab.cli(self.host, "device", "revoke", principal, "-y")
                record["cleanup_revoked"].append(principal)
            except Exception as error:
                errors.append(str(error))
            record["cleanup_errors"] = errors
            save(evidence, record)
        record["post_cleanup_authorizations"] = self.devices(self.host)
        save(evidence, record)
        if errors or any(record["post_cleanup_authorizations"].get(principal, {}).get("allowed") == "Yes"
                         for principal in principals):
            raise RuntimeError("fresh pairing authorization cleanup incomplete: " + "; ".join(errors))

    def close_sessions(self, host):
        for row in self.sessions(host):
            self.lab.cli(host, "session", "close", row[4], "-y")

    def named_session(self):
        name = "restore-" + uuid.uuid4().hex[:12]
        local = LinuxTerminal(self.lab, self.host, self.host,
                              self.case / f"named-{len(self.terminals)}.ansi", create=name)
        self.terminals.append(local)
        def active():
            snapshot = local.snapshot()
            if snapshot.get("error") or snapshot.get("exit") is not None:
                raise AssertionError("named Session fixture failed: " + str(snapshot))
            return snapshot.get("state") == "active"
        until(active, 60, "named Session created through real CLI")
        rows = [row for row in self.sessions(self.host) if row[0] == name]
        if len(rows) != 1:
            raise AssertionError("named Session fixture is missing or ambiguous")
        self.detach(local)
        return rows[0][4]

    def connect(self, device, host, session=None, takeover=False, ordinary=False):
        start = time.monotonic()
        if device == "C":
            current = self.android.snapshot()
            host_id = next(h["id"] for h in current["hosts"] if h["name"] == "multi-device-" + host)
            if session is None or current.get("host") != host_id:
                opened = self.android.command("connect", host="multi-device-" + host)["snapshot"]
                current = opened
                if opened.get("error") and ordinary and not (takeover and opened["error"] == "session_occupied"):
                    # Expected occupancy/authorization checks inspect this
                    # first response; they must not submit a second request.
                    return self.android
                if opened.get("error") and opened["error"] != "session_occupied":
                    # Selecting afterwards would issue another attempt and
                    # could hide the cold failure or replay an unknown result.
                    raise AssertionError("Android host open failed: " + str(opened))
            # Opening a host may already attach the requested session. Match
            # TerminalScreen's current-row behavior while its first frame is
            # synchronizing, instead of retiring it and issuing another attach.
            retained = (current.get("host") == host_id and current.get("session") == session
                        and current.get("state") in ("active", "synchronizing", "reconnecting")
                        and not current.get("error"))
            if session is not None and not retained:
                self.android.command("select", session=session, takeover=takeover)
            terminal = self.android
        else:
            terminal = LinuxTerminal(self.lab, device, host, self.case / f"{device}-terminal-{len(self.terminals)}.ansi",
                                     session, takeover)
            self.terminals.append(terminal)
        if ordinary:
            return terminal
        def active():
            state = terminal.snapshot()
            if state.get("error") or state.get("exit") is not None:
                raise AssertionError("connection failed: " + str(state))
            return state.get("state") == "active" and state.get("inputReady", True)
        until(active, 60, f"{device}→{host} active")
        self.metrics.setdefault("connect_seconds", []).append(time.monotonic()-start)
        return terminal

    def path(self, terminal, expected):
        if isinstance(terminal, LinuxTerminal) and terminal.device == terminal.host:
            expected = "local"
        start = time.monotonic()
        accepted = {"direct", "relay"} if expected == "auto" else {expected}
        value = until(lambda: snapshot if (snapshot := terminal.snapshot()).get("path") in accepted else None,
                      60, "actual selected path " + expected)
        self.metrics.setdefault("paths", []).append({"path": expected, "seconds": time.monotonic()-start,
                                                     "snapshot": value})

    def read_host(self, name):
        return self.lab.user(self.host, "cat", self.remote_dir + "/" + name, check=False, private=True)

    def establish(self, terminal):
        self.session = self.sessions(self.host)[0][4]
        self.secret = uuid.uuid4().hex
        self.remote_dir = f"/home/{ACCOUNT[self.host]}/md-evidence/{self.root.name}/{self.case.name}"
        self.lab.user(self.host, "mkdir", "-p", self.remote_dir)
        command = (f"stty -echo; cd {shlex.quote(self.remote_dir)}; MD_VALUE={self.secret}; "
                   "(i=0; while :; do i=$((i+1)); printf '%s\\n' \"$i\" > counter.tmp; mv counter.tmp counter; sleep 0.25; done) & MD_JOB=$!; "
                   "printf '%s|%s|%s|%s\\n' \"$$\" \"$PWD\" \"$MD_VALUE\" \"$MD_JOB\" > baseline\r")
        if not terminal.send(command):
            raise AssertionError("initial input rejected")
        self.baseline = until(lambda: self.read_host("baseline").strip(), 60, "initial shell evidence")
        if self.secret not in self.baseline:
            raise AssertionError("shell variable not set")
        self.proof(terminal)

    def proof(self, terminal, deadline=None):
        deadline = deadline or (time.monotonic() + 60)
        until(lambda: (state := terminal.snapshot()).get("state") == "active" and state.get("inputReady", True),
              max(0, deadline-time.monotonic()), "controller input fence open")
        nonce = uuid.uuid4().hex
        self.executions.setdefault(self.remote_dir, []).append(nonce)
        before = until(lambda: int(value) if (value := self.read_host("counter").strip()).isdigit() else None,
                       max(0, deadline-time.monotonic()), "background counter started")
        command = (f"printf '{nonce}|%s|%s|%s|%s|%s\\n' \"$$\" \"$PWD\" \"$MD_VALUE\" \"$MD_JOB\" "
                   f'"$(cat counter)" >> execution.log; printf \'MD_%s\\n\' {nonce[:8]}\r')
        start = time.monotonic()
        if not terminal.send(command):
            raise AssertionError("controller rejected healthy input")
        log = until(lambda: (value if nonce in (value := self.read_host("execution.log")) else None),
                    max(0, deadline-time.monotonic()), "input executed on original shell")
        records = [line for line in log.splitlines() if line.startswith(nonce + "|")]
        if len(records) != 1 or "|".join(records[0].split("|")[1:5]) != self.baseline:
            raise AssertionError("duplicate command or changed PID/cwd/variable/background PID: " + repr(records))
        if int(records[0].split("|")[-1]) < before:
            raise AssertionError("background task counter regressed")
        for previous in self.executions[self.remote_dir]:
            if sum(line.startswith(previous + "|") for line in log.splitlines()) != 1:
                raise AssertionError("a previously submitted command was missing or replayed: " + previous)
        if self.session not in [s[4] for s in self.sessions(self.host)]:
            raise AssertionError("original Session ID no longer exists")
        self.metrics.setdefault("input_seconds", []).append(time.monotonic()-start)
        (self.case / "execution.log").write_text(log)
        (self.case / "baseline.txt").write_text(self.baseline + "\n")

    def rejected(self, terminal):
        nonce = "FORBIDDEN_" + uuid.uuid4().hex
        terminal.send(f"printf '{nonce}\\n' >> {shlex.quote(self.remote_dir)}/execution.log\r")
        return nonce

    def verify_counter_progress(self):
        proof = self.case / "execution.log"
        if not proof.exists():
            return
        counters = [int(line.rsplit("|", 1)[1]) for line in proof.read_text().splitlines()
                    if line.count("|") == 5]
        if len(counters) > 1:
            self.metrics["background_counter"] = {"first": counters[0], "last": counters[-1],
                                                  "samples": len(counters)}
            if counters[-1] <= counters[0]:
                raise AssertionError("background task did not advance between continuity samples")

    def no_replay(self, nonce):
        if nonce in self.read_host("execution.log"):
            raise AssertionError("retired/disconnected input was executed: " + nonce)

    def detach(self, terminal, require_normal_exit=False):
        if terminal is self.android:
            self.android.command("home")
        else:
            expected = require_normal_exit or terminal.snapshot().get("state") == "active"
            outcome = terminal.close()
            self.metrics.setdefault("detaches", []).append({"device": terminal.device,
                "normal_exit_required": expected, **outcome})
            if expected and (outcome["forced"] or outcome["exit"] != 0):
                raise AssertionError("normal CLI detach did not exit cleanly: " + str(outcome))

    def occupied(self, terminal):
        state = until(lambda: (s if (s := terminal.snapshot()).get("error") or s.get("exit") is not None
                              or "occupied" in s.get("text", "").lower() else None), 60, "ordinary connect is occupied")
        if not re.search(r"occupied|controller.busy|already.*control", json.dumps(state), re.I):
            raise AssertionError("ordinary connection did not report occupancy: " + str(state))

    def takeover(self, old, new_device, row):
        contender = self.connect(new_device, self.host, self.session, ordinary=True)
        self.occupied(contender)
        self.detach(contender)  # cancel/no confirmation must not change authority
        self.proof(old)
        start = time.monotonic()
        new = self.connect(new_device, self.host, self.session, takeover=True)
        self.metrics["takeover_seconds"] = time.monotonic()-start
        self.path(new, row["path"])
        self.proof(new)
        forbidden = self.rejected(old)
        new.resize(25, 90)
        until(lambda: self.original_session()[2:4] == (90, 25), 60, "new controller viewport applied")
        size = self.original_session()[2:4]
        old.resize(41, 131)
        self.proof(new)
        self.no_replay(forbidden)
        if self.original_session()[2:4] != size:
            raise AssertionError("retired controller changed viewport")
        return new

    @contextlib.contextmanager
    def offline(self, device):
        egress_only = getattr(getattr(self, "args", None), "offline_scope", "bidirectional") == "egress"
        fault = Fault(self.lab, device, "egress" if egress_only else "offline")
        dropped = {}
        try:
            with fault:
                yield fault
        finally:
            evidence = self.lab.root / "fault-evidence" / (fault.chain + ".json")
            if evidence.exists():
                counters = json.loads(evidence.read_text())
                retained = self.case / "faults" / evidence.name
                save(retained, counters)
                for direction, key in (("egress", "families"), ("ingress", "ingress_families")):
                    dropped[direction] = sum(int(match[1]) for output in counters.get(key, {}).values()
                        if isinstance(output, str)
                        for match in re.finditer(r"(?m)^\s*(\d+)\s+\d+\s+DROP\s", output))
                self.metrics.setdefault("faults", []).append({"chain": fault.chain, "device": device,
                    "dropped_packets": sum(dropped.values()), "dropped_by_direction": dropped,
                    "scope": "test-uid-egress" if egress_only else "owned-sockets-bidirectional",
                    "evidence": str(retained)})
        # Never replace a scenario's original failure with a measurement error.
        directions = ("egress",) if egress_only else ("ingress", "egress")
        if not all(dropped.get(direction, 0) > 0 for direction in directions):
            raise Blocked("offline rule injected, but required packet loss was not observed: " + ",".join(directions))

    def recover(self, terminal, row, device=None, short=False):
        short_viewport = None
        if short and row["scenario"] == "recover-short":
            # A quiet shell can send no packet during a three-second fault.
            # Arm bounded real PTY output before cutting the network so this
            # tests lost traffic, while still permitting a transparent recovery.
            marker = "short-" + uuid.uuid4().hex
            terminal.send(f"(printf started > {marker}; i=0; while [ $i -lt 80 ]; do "
                          "printf 'MD_SHORT_TRAFFIC\\n'; i=$((i+1)); sleep 0.1; done) &\r")
            until(lambda: self.read_host(marker).strip() == "started", 60, "short outage traffic started")
            before_viewport = list(self.original_session()[2:4])
            columns = 101 if before_viewport[0] != 101 else 102
            rows = 31 if before_viewport[1] != 31 else 32
            short_viewport = [columns, rows]
            self.metrics["short_outage_resize"] = {"before": before_viewport, "requested": short_viewport}
        started = time.monotonic()
        with self.offline(device or row["old"]):
            if short:
                if short_viewport is not None:
                    # Once incoming Relay packets are dropped, a quiet
                    # controller need not send transport acknowledgements.
                    # Drive real outbound traffic too, without typing shell
                    # input during the outage or requiring reconnecting.
                    terminal.resize(short_viewport[1], short_viewport[0])
                time.sleep(3)
                forbidden = None
            else:
                until(lambda: terminal.snapshot().get("state") == "reconnecting",
                      max(0, 120-(time.monotonic()-started)), "disconnect detection")
                self.metrics["detection_seconds"] = time.monotonic()-started
                forbidden = self.rejected(terminal)
                terminal.resize(33, 111)
        start = time.monotonic()
        def recovered():
            state = terminal.snapshot()
            if short_viewport is not None and row["old"] == "C":
                observed = [state.get("columns"), state.get("rows")]
                self.metrics["short_outage_resize"].update(
                    controller_observed=observed, controller_state=state.get("state"),
                    controller_input_ready=state.get("inputReady", False))
                if observed != short_viewport:
                    return False
            return state.get("state") == "active" and state.get("inputReady", True)
        until(recovered,
              max(0, 60-(time.monotonic()-start)), "original controller recovery")
        self.proof(terminal, start + 60)
        if short_viewport is not None:
            until(lambda: list(self.original_session()[2:4]) == short_viewport,
                  max(0, 60-(time.monotonic()-start)), "short-outage viewport applied to original Session")
            self.metrics["short_outage_resize"]["observed"] = short_viewport
        self.metrics["recovery_seconds"] = time.monotonic()-start
        if self.metrics["recovery_seconds"] > 60:
            raise AssertionError("recovery exceeded the 60 second usability bound")
        self.path(terminal, row["path"])
        if forbidden:
            self.no_replay(forbidden)

    def handoff(self, terminal, row):
        started = time.monotonic()
        loss_deadline = started + 120
        with self.offline(row["old"]):
            if row["scenario"] == "handoff-late":
                until(lambda: terminal.snapshot().get("state") == "reconnecting",
                      max(0, loss_deadline-time.monotonic()), "old detects loss")
                self.metrics["detection_seconds"] = time.monotonic()-started
                until(lambda: self.original_session()[1] == "Detached",
                      max(0, loss_deadline-time.monotonic()), "host releases old controller")
                self.metrics["host_release_seconds"] = time.monotonic()-started
                new = self.connect(row["new"], self.host, self.session)
            else:
                contender = self.connect(row["new"], self.host, self.session, ordinary=True)
                self.occupied(contender)
                self.detach(contender)
                taking_over = time.monotonic()
                new = self.connect(row["new"], self.host, self.session, takeover=True)
                self.metrics["takeover_seconds"] = time.monotonic()-taking_over
            self.path(new, row["path"])
            self.proof(new)
            if "detection_seconds" not in self.metrics:
                until(lambda: terminal.snapshot().get("state") != "active",
                      max(0, loss_deadline-time.monotonic()), "old authority becomes unusable")
                self.metrics["detection_seconds"] = time.monotonic()-started
            forbidden = self.rejected(terminal)
        # Old endpoint may need its full recovery window before noticing retirement.
        restored = time.monotonic()
        until(lambda: terminal.snapshot().get("state") in ("closed", "lease_lost", "ended"), 60, "old cannot reclaim control")
        self.metrics["retirement_seconds"] = time.monotonic()-restored
        new.resize(25, 90)
        self.proof(new)
        until(lambda: self.original_session()[2:4] == (90, 25), 60, "new controller viewport applied")
        size = self.original_session()[2:4]
        terminal.resize(42, 132)
        self.proof(new)
        self.no_replay(forbidden)
        if self.original_session()[2:4] != size:
            raise AssertionError("recovered old device overwrote dimensions")

    def release_path(self):
        if self.path_fault:
            self.path_fault.__exit__()
            self.path_fault = None
        self.path_mode = None

    def configure_path(self, row):
        mode = (row["host"], row["path"])
        if self.path_mode == mode:
            return
        was_relay = self.path_fault is not None
        self.release_path()
        if row["path"] == "relay":
            # Keep this scoped fault across consecutive Relay rows. This avoids
            # making fresh connections race with unrelated between-case path
            # migration. The dedicated migration row still changes a live path.
            devices = ("A", "B", "C") if getattr(self.args, "relay_scope", "host") == "all" else (row["host"],)
            self.path_fault = RelayFaults(self.lab, devices)
            self.path_fault.__enter__()
        if was_relay or self.path_fault:
            for device in ACCOUNT:
                self.lab.cli(device, "daemon", "restart", "-y")
                self.lab.online(device)
            self.android.close()
            self.android.start()
        self.path_mode = mode

    def scenario(self, row):
        self.phase = "preparation"
        self.host = row["host"]
        self.session = None
        if self.android.process is None or self.android.process.poll() is not None:
            # The preceding case already retained the product crash. Give this
            # independent row a live application instead of cascading stale
            # mailbox failures through the remaining matrix.
            self.metrics["android_restarted_after_previous_case"] = True
            if self.android.process is not None:
                self.incidents.append({"event": "android_coordinator_exited_between_cases", "time": stamp(),
                                       "next_case": self.case.name, "process_log": self.android.log.name})
            self.android.start()
        for client in (row["old"], row["new"]):
            if (self.host, client) in self.unavailable:
                raise Blocked("pairing prerequisite failed: " + self.unavailable[self.host, client])
        self.android.command("home")
        self.close_sessions(self.host)
        self.configure_path(row)
        if row["path"] != "local":
            # Independent rows require a ready host after the preceding row's
            # fault. Only inspect local status: do not warm a peer connection or
            # start the cold controller. Recovery inside a row keeps its limits.
            readiness = self.metrics["host_readiness"] = {
                "device": self.host, "method": "local-status-only", "online": False}
            starting = time.monotonic()
            try:
                self.lab.online(self.host)
                readiness["online"] = True
            finally:
                readiness["seconds"] = time.monotonic() - starting
        name = row["scenario"]
        if name in ("cold", "first-existing"):
            starting = time.monotonic()
            if row["old"] == "C":
                self.android.close()
                self.android.start()
                self.metrics["client_start_seconds"] = time.monotonic()-starting
            else:
                # Let the real connect command launch the stopped daemon.
                # Waiting for Network Online first would omit cold endpoint
                # startup from the measured first connection.
                self.lab.cli(row["old"], "daemon", "stop", "-y")
                self.metrics["client_stop_seconds"] = time.monotonic()-starting
                self.metrics["cold_start_mode"] = "cli-starts-stopped-daemon"
        if name in ("lifecycle-allow", "lifecycle-deny"):
            self.android.close()
            self.lab.adb("shell", "pm", "grant" if name.endswith("allow") else "revoke", PACKAGE, "android.permission.POST_NOTIFICATIONS")
            self.android.start()
        if name == "fresh-pair":
            self.phase = "pairing-and-first-connection"
            self.fresh_pair(row)
            return
        if name == "first-existing":
            local = self.connect(self.host, self.host)
            self.establish(local)
            self.detach(local)
            self.phase = "connection"
            old = self.connect(row["old"], self.host, self.session)
            self.path(old, row["path"])
            self.proof(old)
            return
        saved = self.named_session() if row["old"] == "C" and name in ("client-kill", "client-reboot") else None
        self.phase = "connection"
        old = self.connect(row["old"], self.host, saved)
        self.path(old, row["path"])
        self.establish(old)
        self.phase = "scenario-assertions"
        if name in ("local", "cold"):
            return
        if name in ("detach", "detach-other"):
            self.detach(old, require_normal_exit=True)
            old = self.connect(row["new"], self.host, self.session)
            self.path(old, row["path"])
            self.proof(old)
        elif name == "takeover":
            self.takeover(old, row["new"], row)
        elif name.startswith("recover-") or name == "host-network":
            self.recover(old, row, self.host if name == "host-network" else None, name == "recover-short")
        elif name.startswith("handoff-"):
            self.handoff(old, row)
        elif name == "path-migrate":
            with Fault(self.lab, row["old"], "udp"):
                self.path(old, "relay")
                self.proof(old)
            self.path(old, "direct")
            self.proof(old)
        elif name == "relay-block":
            with Fault(self.lab, row["old"], "relay"):
                self.proof(old)
                time.sleep(5)
                self.path(old, "direct")
                self.proof(old)
        elif name.startswith("lifecycle-"):
            for enabled in (True, False, True):
                self.android.command("keepBackground", enabled=enabled)
                for action in ("background", "lock", "unlock", "foreground", "recreate"):
                    self.android.command(action)
                    self.proof(old)
            if self.android.snapshot().get("notificationVisible") != name.endswith("allow"):
                raise AssertionError("notification permission projection disagrees")
        elif name == "wifi-data":
            before = self.android.snapshot()
            if before.get("network") != "wifi":
                raise Blocked("emulator must begin on a real simulated Wi-Fi default network")
            save(self.case / "network-before.json", before)
            self.lab.adb("shell", "svc", "data", "enable")
            try:
                self.lab.adb("shell", "svc", "wifi", "disable")
                switched = until(lambda: value if (value := old.snapshot()).get("network") == "cellular"
                                 and value.get("networkId") != before.get("networkId") else None,
                                 60, "Android selected simulated cellular network")
                save(self.case / "network-cellular.json", switched)
                until(lambda: old.snapshot().get("state") == "active", 60, "simulated mobile data")
                self.proof(old)
            finally:
                self.lab.adb("shell", "svc", "wifi", "enable")
            until(lambda: old.snapshot().get("network") == "wifi", 60, "Wi-Fi default network restored")
            self.proof(old)
        else:
            self.extended(old, row)

    def extended(self, old, row):
        name, device = row["scenario"], row["old"]
        if name in ("client-kill", "client-daemon-crash", "client-reboot"):
            fault_started = time.monotonic()
            if device == "C":
                # A second default session makes accidentally reopening main
                # observable instead of passing only because one session exists.
                if self.original_session()[0] == "main":
                    raise AssertionError("restart fixture requires an original non-default Session")
                decoy = self.connect(self.host, self.host)
                self.detach(decoy)
                self.restoration_evidence("before-stop")
                fault_started = time.monotonic()
                self.lab.adb("shell", "am", "force-stop", PACKAGE)
                self.android.close(killed=True)
                self.restoration_evidence("after-stop")
                if name == "client-reboot":
                    boot = self.lab.adb("shell", "cat", "/proc/sys/kernel/random/boot_id").strip()
                    self.lab.adb("reboot")
                    def rebooted():
                        current = self.lab.adb("shell", "cat", "/proc/sys/kernel/random/boot_id", check=False, timeout=10).strip()
                        return current and current != boot and self.lab.adb("shell", "getprop", "sys.boot_completed", check=False).strip() == "1"
                    until(rebooted, 180, "Android guest completed a new boot")
                    self.lab.start()
                    if self.path_fault:
                        self.path_fault.reapply("C")
                    self.restoration_evidence("after-boot")
                self.android.start()
                self.restoration_evidence("after-open")
                snapshot = self.android.snapshot()
                if snapshot.get("route") == "Home" and not snapshot.get("error"):
                    # The acceptance contract also permits reopening the saved
                    # host after a process/system restart. Confirm the original
                    # session exists first, then exercise the product's picker.
                    if not any(session[4] == self.session for session in self.sessions(self.host)):
                        raise AssertionError("original Session disappeared before manual reopen")
                    self.metrics["restoration_mode"] = "manual_reopen"
                    self.connect(device, self.host)
                else:
                    self.metrics["restoration_mode"] = "automatic"
                    until(lambda: self.android.snapshot().get("state") == "active", 60, "Android process recovery")
                if self.android.snapshot().get("session") != self.session:
                    raise AssertionError("Android restored a replacement Session")
                new = self.android
            else:
                if name == "client-reboot":
                    self.restart_vm(device)
                else:
                    expression = "^/usr/local/bin/zterm " + ("--internal-daemon" if name == "client-daemon-crash" else "connect") + "( |$)"
                    self.lab.vm(device, "sudo", "pkill", "-KILL", "-u", ACCOUNT[device], "-f", expression)
                    if name == "client-daemon-crash":
                        self.lab.cli(device, "setup", "--name", "multi-device-" + device, "--profile", "official-n0")
                        self.lab.online(device)
                if name == "client-daemon-crash":
                    # The still-running CLI reconnects through the restarted
                    # local daemon. Waiting for Detached misses a fast recovery.
                    until(lambda: old.snapshot().get("state") in ("active", "closed"), 60,
                          "CLI recovers or accurately exits after daemon loss")
                    new = old if old.process.poll() is None else None
                else:
                    new = None
                if new is None:
                    until(lambda: self.original_session()[1] == "Detached", 120, "host releases terminated controller")
                    new = self.connect(device, self.host, self.session)
            self.path(new, row["path"])
            self.proof(new)
            self.metrics["client_fault_to_usable_seconds"] = time.monotonic()-fault_started
        elif name in ("session-close", "host-daemon-crash", "host-reboot"):
            if name == "session-close":
                self.lab.cli(self.host, "session", "close", self.session, "-y")
            elif name == "host-reboot":
                self.restart_vm(self.host)
            else:
                self.lab.vm(self.host, "sudo", "pkill", "-KILL", "-u", ACCOUNT[self.host], "-f", "^/usr/local/bin/zterm --internal-daemon( |$)")
                self.lab.cli(self.host, "setup", "--name", "multi-device-" + self.host, "--profile", "official-n0")
            until(lambda: old.snapshot().get("state") in ("ended", "closed") or old.snapshot().get("error") == "session_not_found",
                  120, "client reports original Session ended")
            if self.sessions(self.host):
                raise AssertionError("host restart/close created a replacement Session")
            if device == "C":
                self.android.command("select", session=self.session)
                if self.android.snapshot().get("error") != "session_not_found":
                    raise AssertionError("exact ended Session retry did not report not found")
            else:
                attempt = self.connect(device, self.host, self.session, ordinary=True)
                until(lambda: attempt.process.poll() is not None, 60, "exact missing Session rejected")
                if not re.search(r"not.found|session_not_found", str(attempt.snapshot()), re.I):
                    raise AssertionError("wrong missing Session error")
            if self.sessions(self.host):
                raise AssertionError("exact Session retry created a substitute")
        elif name == "revoke":
            principal = self.revocation_principal(device)
            save(self.case / "revocation-principal.json", {"controller": device, "host": self.host,
                "principal": principal, "source": "controller-local-public-identity"})
            try:
                self.lab.cli(self.host, "device", "revoke", principal, "-y")
                until(lambda: old.snapshot().get("state") != "active", 120, "revocation terminates attachment")
                forbidden = self.rejected(old)
                self.detach(old)
                attempted = self.connect(device, self.host, self.session, ordinary=True)
                until(lambda: attempted.snapshot().get("error") or attempted.snapshot().get("exit") is not None, 60)
                if not re.search(r"unauthorized|not.authorized|authorization|permission|revoked", str(attempted.snapshot()), re.I):
                    raise AssertionError("revocation error is not authorization-specific")
                self.no_replay(forbidden)
                if self.original_session()[4] != self.session:
                    raise AssertionError("revocation ended/replaced the host Session")
            finally:
                self.pair(self.host, device)
        elif name in ("output-resize", "output-resize-takeover"):
            old.send("(i=0; while [ $i -lt 500 ]; do printf 'STREAM_%06d\\n' $i; i=$((i+1)); sleep 0.02; done) &\r")
            for index in range(8):
                old.resize(25 + index, 85 + index)
            until(lambda: self.original_session()[2:4] == (92, 32), 60, "resize reaches original shell")
            until(lambda: old.snapshot().get("state") == "active" and old.snapshot().get("inputReady", True),
                  60, "input ready after viewport synchronization")
            # Keep a foreground command running across the fault/lease change,
            # as well as the streaming background process and viewport updates.
            command = uuid.uuid4().hex
            old.send(f"printf '{command}\\n' > foreground-start; sleep 10; printf '{command}\\n' >> foreground-end\r")
            until(lambda: command in self.read_host("foreground-start"), 60, "foreground command started")
            if self.read_host("foreground-end").strip():
                raise Blocked("foreground command finished before fault injection")
            if name == "output-resize":
                self.recover(old, row, short=True)
                new = old
            else:
                new = self.connect(row["new"], self.host, self.session, takeover=True)
                self.path(new, row["path"])
                until(lambda: old.snapshot().get("state") in ("closed", "lease_lost"), 60,
                      "old controller retired during output")
                forbidden = self.rejected(old)
                new.resize(25, 90)
                until(lambda: self.original_session()[2:4] == (90, 25), 60, "new viewport applied during output")
                old.resize(41, 131)
                self.proof(new)
                self.no_replay(forbidden)
                if self.original_session()[2:4] != (90, 25):
                    raise AssertionError("retired output controller overwrote viewport")
            self.proof(new)
            completed = self.read_host("foreground-end").splitlines()
            if completed != [command]:
                raise AssertionError("foreground command was lost or executed more than once")
            save(self.case / "foreground.json", {"nonce": command, "executions": len(completed)})
        elif name.startswith("race-"):
            self.race(old, row)
        elif name.startswith("upload-"):
            try:
                if name == "upload-takeover":
                    # Establish the contender and verify occupancy before the
                    # transfer. Cold connection has its own scenarios; here the
                    # lease must actually change while bytes are in flight.
                    contender = self.connect(row["new"], self.host, self.session, ordinary=True)
                    self.occupied(contender)
                    self.detach(contender)
                    self.proof(old)
                self.android.command("uploadPrepare")
                self.android.command("uploadStart")
                def transferring():
                    upload = self.android.snapshot().get("upload")
                    if not upload or upload.get("phase") == "completed":
                        raise Blocked("transfer finished before coordinator could inject its fault")
                    if upload.get("phase") in ("failed", "cancelled"):
                        raise AssertionError("upload failed before fault injection: " + str(upload))
                    return upload if upload.get("bytes", 0) > 0 else None
                active_upload = until(transferring, 60, "upload has accepted bytes before fault injection")
                save(self.case / "upload-before.json", active_upload)
                if name == "upload-disconnect":
                    self.recover(old, row)
                    new = old
                else:
                    completed_before_takeover = False
                    observations = []
                    with ThreadPoolExecutor(1) as pool:
                        connecting = pool.submit(self.connect, row["new"], self.host, self.session, True)
                        while not connecting.done():
                            snapshot = self.android.snapshot()
                            observations.append({key: snapshot.get(key) for key in ("elapsedMs", "state", "upload")})
                            if snapshot.get("state") == "active" and not snapshot.get("upload"):
                                completed_before_takeover = True
                            time.sleep(0.1)
                        new = connecting.result()
                    save(self.case / "upload-takeover-progress.json", observations)
                    if completed_before_takeover:
                        raise Blocked("upload completed while old controller was still active, before takeover")
                    until(lambda: old.snapshot().get("state") == "lease_lost", 60)
                    self.path(new, row["path"])
                until(lambda: not (upload := self.android.snapshot().get("upload")) or upload["phase"] in
                      ("failed", "cancelled"), 60, "retired upload cancelled")
                time.sleep(1)
                self.proof(new)  # a delayed path prefix would corrupt this command
            finally:
                self.android.command("uploadCleanup")
        else:
            raise ValueError("unrecognized scenario " + name)

    def restoration_evidence(self, phase):
        # No identity seed or encrypted identity envelope leaves the device.
        saved = json.loads(self.lab.adb("shell", "run-as", PACKAGE, "cat", "no_backup/zterm/state.json", private=True))
        save(self.case / ("restoration-" + phase + ".json"),
             {"time": stamp(), "activeTerminal": saved.get("activeTerminal"), "recent": saved.get("recent")})

    def restart_vm(self, device):
        started = time.monotonic()
        self.lab.call(["limactl", "stop", "--tty=false", self.lab.env["vms"][device]], timeout=120)
        self.lab.call(["limactl", "start", "--tty=false", self.lab.env["vms"][device]], timeout=180)
        if self.path_fault:
            self.path_fault.reapply(device)
        self.lab.cli(device, "setup", "--name", "multi-device-" + device, "--profile", "official-n0")
        self.lab.online(device)
        self.metrics.setdefault("vm_restart_seconds", {})[device] = time.monotonic()-started

    def race(self, old, row):
        name = row["scenario"]
        if name == "race-disconnect":
            with ThreadPoolExecutor(2) as pool:
                connect = pool.submit(self.connect, row["new"], self.host, self.session, True)
                detach = pool.submit(self.detach, old)
                new = connect.result()
                detach.result()
            self.path(new, row["path"])
            self.proof(new)
            return
        if name == "race-reconnect":
            started = time.monotonic()
            with self.offline(row["old"]) as fault:
                until(lambda: old.snapshot().get("state") == "reconnecting",
                      max(0, started+120-time.monotonic()), "old detects loss")
                self.metrics["detection_seconds"] = time.monotonic()-started
                taking_over = time.monotonic()
                with ThreadPoolExecutor(2) as pool:
                    connect = pool.submit(self.connect, row["new"], self.host, self.session, True)
                    fault.__exit__()
                    restored = time.monotonic()
                    new = connect.result()
                self.metrics["takeover_seconds"] = time.monotonic()-taking_over
            self.path(new, row["path"])
            self.proof(new, restored + 60)
            until(lambda: old.snapshot().get("state") in ("closed", "lease_lost"),
                  max(0, restored+60-time.monotonic()), "old cannot reclaim control")
            self.metrics["retirement_seconds"] = time.monotonic()-restored
            self.no_replay(self.rejected(old))
            return
        self.detach(old)
        barrier = threading.Barrier(2)
        def start(device):
            barrier.wait(timeout=10)
            return self.connect(device, self.host, self.session, takeover=name == "race-takeover", ordinary=True)
        with ThreadPoolExecutor(2) as pool:
            futures = [pool.submit(start, device) for device in (row["old"], row["new"])]
            controllers = [future.result() for future in futures]
        def winner():
            return self.settled_race_winner(controllers)
        current = until(winner, 60, "one race winner")
        time.sleep(1)
        if winner() is not current:
            raise AssertionError("race winner changed without another explicit request")
        self.path(current, row["path"])
        self.proof(current)
        for terminal in controllers:
            if terminal is not current:
                if name == "race-connect" or terminal.snapshot().get("error") == "session_occupied":
                    self.occupied(terminal)
                else:
                    until(lambda: (state := terminal.snapshot()).get("state") in ("closed", "lease_lost")
                          or state.get("error") == "lease_lost",
                          60, "losing takeover retires its controller")
                forbidden = self.rejected(terminal)
                self.proof(current)
                self.no_replay(forbidden)

    @staticmethod
    def settled_race_winner(controllers):
        """A transient sole Active is not a winner while its competitor is opening."""
        active = []
        pending = False
        for terminal in controllers:
            state = terminal.snapshot()
            occupied = state.get("error") == "session_occupied" or (
                state.get("exit") is not None and bool(re.search(
                    r"occupied|controller.busy|already.*control", state.get("rawError", ""), re.I)))
            retired = state.get("state") == "lease_lost" or state.get("error") == "lease_lost" or (
                state.get("exit") is not None and "lease_lost:" in state.get("rawError", ""))
            if ((state.get("error") or state.get("exit") not in (None, 0)) and not (occupied or retired)):
                raise AssertionError("competing connection failed: " + str(state))
            if (state.get("state") == "active" and state.get("inputReady", True)
                    and not state.get("busy") and not (occupied or retired)):
                active.append(terminal)
            elif not (occupied or retired) and state.get("state") != "closed":
                pending = True
        return active[0] if len(active) == 1 and not pending else None

    def collect(self, destination=None):
        destination = destination or self.case
        destination.mkdir(parents=True, exist_ok=True)
        if destination != self.case:
            for terminal in self.terminals:
                with terminal.lock:
                    (destination / terminal.output.name).write_bytes(bytes(terminal.raw))
        for device in ACCOUNT:
            (destination / f"{device}-status.txt").write_text(self.lab.cli(device, "status", check=False))
            (destination / f"{device}-sessions.txt").write_text(self.lab.cli(device, "session", "list", check=False))
            (destination / f"{device}-daemon.log").write_text(self.lab.user(device, "tail", "-c", "1000000",
                                                          f"/home/{ACCOUNT[device]}/.zterm/logs/daemon.log", check=False))
            for name in ("daemon.debug.log", "daemon.debug.log.1"):
                (destination / f"{device}-{name}").write_text(self.lab.user(
                    device, "tail", "-c", "1000000", f"/home/{ACCOUNT[device]}/.zterm/logs/{name}",
                    check=False, private=True))
        # A failed bridge leaves its last mailbox for evidence, but assertions
        # must never use that retained value as a live Android observation.
        save(destination / "android-snapshot.json", self.android.snapshot(require_running=False))
        with (destination / "android.png").open("wb") as image:
            subprocess.run([str(SDK / "platform-tools/adb"), "-s", self.lab.env["serial"], "exec-out", "screencap", "-p"],
                           stdout=image, stderr=subprocess.DEVNULL, timeout=15, check=False)
        (destination / "logcat.txt").write_text(self.lab.adb("logcat", "-d", "-t", "3000", check=False))
        for name in ("events.jsonl", "events.jsonl.1", "detail.jsonl", "detail.jsonl.1"):
            (destination / ("android-" + name)).write_text(self.lab.adb(
                "shell", "run-as", PACKAGE, "cat", "no_backup/diagnostics/" + name, check=False, private=True))

    def execute(self, row):
        label = f"{len(self.results):04}-{row['scenario']}-{row['host']}-{row['old']}-{row['new']}-{row['path']}-{row['iteration']}"
        self.case = self.root / label
        self.case.mkdir()
        self.metrics = {}
        start = time.monotonic()
        result = dict(row, status="passed", started=stamp(), evidence=str(self.case))
        unknown_session = False
        isolation_error = None
        self.pair_cleanup_error = None
        print(label, flush=True)
        try:
            self.scenario(row)
            self.verify_counter_progress()
            if self.android.process is None or self.android.process.poll() is not None:
                raise AssertionError("Android coordinator exited unexpectedly during the case")
        except KeyboardInterrupt:
            result.update(status="skipped", error="coordinator interrupted before completing assertions")
            raise
        except Blocked as error:
            result.update(status="blocked", error=str(error))
        except Exception as error:
            result.update(status="failed", error=str(error))
            unknown_session = ("operation_outcome_unknown" in str(error)
                               and row["scenario"] != "fresh-pair")
            (self.case / "failure.txt").write_text(traceback.format_exc())
            if any(kind in str(error) for kind in ("operation_outcome_unknown", "pair_outcome_unknown")):
                try:
                    if row["scenario"] == "fresh-pair":
                        record = json.loads((self.case / "outcome-reconciliation.json").read_text())
                        if "observed_authorizations" not in record:
                            raise RuntimeError("pairing outcome has no successful authorization query")
                        attempt = json.loads((self.case / "pair-attempt.json").read_text())
                        if not isinstance(attempt.get("returned"), bool):
                            raise RuntimeError("pairing completion was not recorded")
                        if attempt["returned"] and "operation_outcome_unknown" in str(error):
                            unknown_session = True
                            # Pairing succeeded; the later Session mutation is
                            # a separate operation. Preserve both observations.
                            save(self.case / "session-outcome-reconciliation.json", {
                                "method": "read-only host session list; no mutation retry",
                                "expected_session": self.session,
                                "observed_sessions": self.sessions(self.host),
                                "original_error": str(error),
                            })
                    else:
                        save(self.case / "outcome-reconciliation.json", {
                            "method": "read-only host session list; no mutation retry",
                            "expected_session": self.session,
                            "observed_sessions": self.sessions(self.host),
                            "original_error": str(error),
                        })
                except Exception as reconciliation_error:
                    result["reconciliation_error"] = str(reconciliation_error)
        finally:
            try:
                self.collect()
            except Exception as error:
                result["collection_error"] = str(error)
                if result["status"] == "passed":
                    result["status"] = "failed"
            for terminal in self.terminals:
                try:
                    terminal.close()
                except Exception as error:
                    result["terminal_cleanup_error"] = str(error)
                    result["status"] = "failed"
            self.terminals.clear()
            if self.pair_cleanup_error is not None:
                result["pair_cleanup_error"] = self.pair_cleanup_error
            if unknown_session:
                try:
                    # A Session list is a point-in-time observation: an
                    # admitted request can commit after an empty reply. Keep
                    # its original failure and evidence above, then end this
                    # disposable host generation before the next matrix row.
                    # This never retries a mutation or applies to soak recovery.
                    self.quiesce_failed_host("session-cleanup-barrier.json", "unknown Session operation")
                except Exception as error:
                    isolation_error = str(error)
                    result["session_cleanup_error"] = isolation_error
            result.update(seconds=time.monotonic()-start, metrics=self.metrics,
                          phase="completed" if result["status"] == "passed" else self.phase)
            save(self.case / "result.json", result)
            self.results.append(result)
            self.report()
        print(f"  {result['status']}: {result.get('error', '')[:180]}", flush=True)
        if self.pair_cleanup_error is not None:
            raise Blocked("failed pair cleanup left the host generation unconfirmed: " + self.pair_cleanup_error)
        if isolation_error is not None:
            raise Blocked("failed Session cleanup left the host generation unconfirmed: " + isolation_error)
        return result

    def report(self):
        counts = {status: sum(r["status"] == status for r in self.results) for status in ("passed", "failed", "blocked", "skipped")}
        save(self.root / "results.json", {"counts": counts, "results": self.results, "incidents": self.incidents})

    def open_soak_original(self, device, session):
        """Bounded explicit reopens, each failure retained; never replay unknown mutations."""
        self.session = session
        alternate = "C" if device != "C" else ("B" if self.host == "A" else "A")
        for attempt, controller in enumerate((device, alternate, device), 1):
            try:
                return self.connect(controller, self.host, session), controller
            except Exception as error:
                observed = self.sessions(self.host)
                failure = {"host": self.host, "controller": controller, "session": session,
                           "attempt": attempt, "time": stamp(), "error": str(error),
                           "method": "read-only host list before explicit original-ID reopen",
                           "observed_sessions": observed}
                self.soak_connection_failures.append(failure)
                evidence = self.case / f"connection-failure-{len(self.soak_connection_failures):04}"
                failure["evidence"] = str(evidence)
                save(evidence / "result.json", failure)
                save(evidence / "outcome-reconciliation.json", {"method": failure["method"],
                     "expected_session": session, "observed_sessions": observed})
                self.collect(evidence)
                if "operation_outcome_unknown" in str(error):
                    raise
                if not any(row[4] == session for row in observed):
                    raise AssertionError("original soak Session ended; replacement is forbidden") from error
                if attempt == 3:
                    raise
                self.android.command("home")
                for terminal in self.terminals:
                    terminal.close()
                self.terminals.clear()
                until(lambda: self.original_session()[1] == "Detached", 120,
                      "failed explicit reopen releases the original Session")
                time.sleep(1)
        raise AssertionError("bounded reopen exhausted without a result")

    def soak(self):
        """Keep each original shell across random actions; never recreate on error."""
        self.release_path()
        preparation_started = time.monotonic()
        started = None
        preparation_seconds = None
        fixtures = []
        actions = []
        inflight = None
        try:
            for index, host in enumerate(("A", "B")):
                self.host = host
                self.session = None
                self.case = self.root / ("soak-" + host)
                self.case.mkdir()
                self.metrics = {}
                self.android.command("home")
                self.close_sessions(host)
                device = "B" if host == "A" else "A"
                original = self.named_session()
                current, device = self.open_soak_original(device, original)
                self.path(current, "auto")
                self.establish(current)
                if started is None:
                    started = time.monotonic()
                    preparation_seconds = started - preparation_started
                fixture = (self.host, self.session, self.secret, self.remote_dir, self.baseline)
                fixtures.append(fixture)
                stop_at = started + self.args.soak_seconds * (index + 1) / 2
                while time.monotonic() < stop_at:
                    name = self.rng.choice(("recover-short", "recover-long", "takeover", "client-kill", "detach"))
                    other = "C" if device != "C" else ("B" if host == "A" else "A")
                    row = dict(scenario=name, host=host, old=device, new=other, path="auto", iteration=len(actions) + 1)
                    before = time.monotonic()
                    self.metrics = {}
                    inflight = {**row, "started": stamp()}
                    print(f"soak {host} {name} {device}→{other} ({before-started:.0f}s)", flush=True)
                    try:
                        if name.startswith("recover"):
                            self.recover(current, row, short=name.endswith("short"))
                        elif name == "takeover":
                            current = self.takeover(current, other, row)
                            device = other
                        elif name == "client-kill":
                            self.extended(current, row)
                            current = self.android if device == "C" else self.terminals[-1]
                        else:
                            self.detach(current)
                            current = self.connect(device, host, self.session)
                            self.proof(current)
                        self.proof(current)
                        actions.append({**inflight, "seconds": time.monotonic()-before, "status": "passed", "metrics": copy.deepcopy(self.metrics)})
                        inflight = None
                    except Exception as error:
                        failure = {**inflight, "seconds": time.monotonic()-before,
                                   "status": "blocked" if isinstance(error, Blocked) else "failed",
                                   "error": str(error), "metrics": copy.deepcopy(self.metrics)}
                        actions.append(failure)
                        inflight = None
                        evidence = self.case / f"failed-action-{len(actions):04}"
                        evidence.mkdir()
                        (evidence / "failure.txt").write_text(traceback.format_exc())
                        self.collect(evidence)
                        save(evidence / "result.json", failure)
                        print(f"  {failure['status']}: {str(error)[:180]}", flush=True)
                        # Continuing samples cannot turn this failed action or
                        # endurance run into a pass. Reconcile before touching
                        # clients, then explicitly reopen only the original ID.
                        # Unknown mutation outcomes must never be replayed.
                        observed = self.sessions(host)
                        save(evidence / "outcome-reconciliation.json", {
                            "method": "read-only host session list; no mutation retry",
                            "expected_session": self.session, "observed_sessions": observed,
                        })
                        if "operation_outcome_unknown" in str(error):
                            raise
                        if not any(session[4] == self.session for session in observed):
                            raise AssertionError("original soak Session ended; replacement is forbidden") from error
                        # Keep the failed action's path/timing evidence fixed;
                        # an explicit later reopen has its own observations.
                        self.metrics = {}
                        self.android.command("home")
                        for terminal in self.terminals:
                            terminal.close()
                        self.terminals.clear()
                        until(lambda: self.original_session()[1] == "Detached", 120,
                              "failed soak action's controllers release original Session")
                        device = "B" if host == "A" else "A"
                        current, device = self.open_soak_original(device, self.session)
                        self.path(current, "auto")
                        self.proof(current)
                        failure["continuation"] = {"mode": "explicit_reopen_original_id", "session": self.session,
                                                   "controller": device, "time": stamp(), "metrics": copy.deepcopy(self.metrics)}
                        save(evidence / "result.json", failure)
                    for terminal in self.terminals:
                        if terminal is not current:
                            terminal.close()
                    self.terminals = [current] if isinstance(current, LinuxTerminal) else []
                    save(self.root / "soak-progress.json", {"seed": self.args.seed, "seconds": time.monotonic()-started,
                                                           "sessions": [item[1] for item in fixtures], "actions": actions,
                                                           "connection_failures": self.soak_connection_failures})
                    # Keep idle periods as well as transitions in this endurance
                    # run; these are bounded and signal-interruptible.
                    time.sleep(min(5, max(0, stop_at-time.monotonic())))
                self.collect()
                self.detach(current)
            # A's session must survive the entire B phase too.
            for fixture in fixtures:
                self.host, self.session, self.secret, self.remote_dir, self.baseline = fixture
                self.case = self.root / ("soak-" + self.host)
                current, _ = self.open_soak_original("B" if self.host == "A" else "A", self.session)
                self.proof(current)
                self.verify_counter_progress()
                self.collect()
                self.detach(current)
            self.soak_status = "passed" if (all(action["status"] == "passed" for action in actions)
                                             and not self.soak_connection_failures) else "failed"
            save(self.root / "soak.json", {"seed": self.args.seed, "seconds": time.monotonic()-started,
                 "actions": actions, "sessions": [item[1] for item in fixtures], "status": self.soak_status,
                 "connection_failures": self.soak_connection_failures,
                 "preparation_seconds": preparation_seconds})
        except BaseException as error:
            self.soak_status = "failed"
            if "operation_outcome_unknown" in str(error):
                # Also cover an uncertain initial named-session creation,
                # before the normal per-action reconciliation path exists.
                try:
                    save(self.case / "outcome-reconciliation.json", {
                        "method": "read-only host session list; no mutation retry",
                        "expected_session": self.session,
                        "observed_sessions": self.sessions(self.host),
                    })
                except Exception as reconciliation_error:
                    save(self.case / "reconciliation-error.json", {"error": str(reconciliation_error)})
            if inflight is not None:
                actions.append({**inflight, "seconds": time.monotonic()-before, "status": "failed",
                                "error": str(error), "metrics": self.metrics})
            save(self.root / "soak.json", {"seed": self.args.seed,
                 "seconds": time.monotonic()-started if started is not None else 0,
                 "actions": actions, "sessions": [item[1] for item in fixtures], "status": "failed", "error": str(error),
                 "connection_failures": self.soak_connection_failures,
                 "preparation_seconds": preparation_seconds if started is not None else time.monotonic()-preparation_started})
            self.collect()
            raise

    def run(self):
        rows = selected_matrix(self.args)
        save(self.root / "matrix.json", rows)
        try:
            self.setup()
            for row in rows:
                self.execute(row)
            if self.args.soak_seconds:
                self.soak()
        finally:
            errors = []
            # One failed cleanup must not prevent restoring the firewall or
            # writing skipped rows. The independent guard remains a fallback.
            for action in [*(terminal.close for terminal in self.terminals),
                           self.android.close, self.release_path, lambda: cleanup(self.lab)]:
                try:
                    action()
                except BaseException as error:
                    errors.append(repr(error))
            completed = {tuple(r[key] for key in ("scenario", "host", "old", "new", "path", "iteration")) for r in self.results}
            for row in rows:
                if tuple(row[key] for key in ("scenario", "host", "old", "new", "path", "iteration")) not in completed:
                    self.results.append(dict(row, status="skipped", error="coordinator stopped before this row"))
            self.report()
            if errors:
                save(self.root / "cleanup-errors.json", errors)
            if not self.args.keep_running:
                self.lab.stop()
            if errors:
                raise RuntimeError("run cleanup failed: " + "; ".join(errors))
        return 0 if (all(r["status"] == "passed" for r in self.results)
                     and not self.incidents and self.soak_status != "failed"
                     and (self.results or self.args.soak_seconds)) else 1


def main():
    os.umask(0o077)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["create", "start", "build", "preflight", "run", "soak", "matrix", "collect", "cleanup", "stop", "guard"])
    parser.add_argument("--environment", type=Path, default=BASE / "environment")
    parser.add_argument("--port", type=int, default=5586)
    parser.add_argument("--run-id")
    parser.add_argument("--repeat", type=int, default=10)
    parser.add_argument("--scenario", help="comma-separated scenario names")
    parser.add_argument("--host", choices=["A", "B"])
    parser.add_argument("--path", choices=["direct", "relay", "local"])
    parser.add_argument("--controller", choices=["A", "B", "C"], help="filter the original control device")
    parser.add_argument("--relay-scope", choices=["all", "host"], default="all",
                        help="block non-DNS UDP on all three test UIDs; host is an asymmetric diagnostic variant")
    parser.add_argument("--offline-scope", choices=["bidirectional", "egress"], default="bidirectional",
                        help="egress retains the original one-way-loss diagnostic; acceptance uses bidirectional")
    parser.add_argument("--group", choices=["all", "core", "extended"], default="all",
                        help="core = connections, detach/takeover, network recovery, handoff, races and path changes")
    parser.add_argument("--seed", type=int, default=20261003)
    parser.add_argument("--soak-seconds", type=int, default=3600)
    parser.add_argument("--keep-running", action="store_true", help="retain running instances for a follow-up run")
    parser.add_argument("--fault-chain", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.port % 2 or not 5554 <= args.port <= 5682:
        parser.error("emulator port must be even and between 5554 and 5682")
    if args.repeat < 1 or args.soak_seconds < 0:
        parser.error("repeat must be positive and soak-seconds cannot be negative")
    if args.run_id and (not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", args.run_id)):
        parser.error("run-id must be a single filename beginning with a letter or digit")
    lab = Lab(args)
    lock = None
    if args.action not in ("matrix", "collect", "guard"):
        lock = (lab.root / "coordinator.lock").open("a")
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            parser.error("another coordinator owns this disposable environment")
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    try:
        if args.action in ("run", "soak"):
            return Suite(lab).run()
        if args.action == "matrix":
            print(json.dumps(selected_matrix(args), indent=2))
        elif args.action == "cleanup":
            cleanup(lab)
        elif args.action == "guard":
            guard(lab)
        else:
            getattr(lab, args.action)()
    except (Exception, KeyboardInterrupt) as error:
        status = "skipped" if isinstance(error, KeyboardInterrupt) else (
            "blocked" if isinstance(error, (Blocked, FileNotFoundError)) else "failed")
        save(lab.root / ("coordinator-" + stamp() + ".json"),
             {"action": args.action, "status": status, "error": str(error)})
        raise


if __name__ == "__main__":
    sys.exit(main())
