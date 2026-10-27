"""Disposable simulator files and a real competing local terminal for UI tests."""

from contextlib import contextmanager
import hashlib
import json
import fcntl
import os
from pathlib import Path
import pty
import re
import shlex
import shutil
import signal
import subprocess
import struct
import termios
import threading
import uuid


def picker_storage(simulator: str) -> Path:
    groups = subprocess.check_output(
        ["xcrun", "simctl", "get_app_container", simulator, "com.apple.DocumentsApp", "groups"], text=True)
    paths = dict(line.split("\t", 1) for line in groups.splitlines())
    storage = Path(paths["group.com.apple.FileProvider.LocalStorage"]) / "File Provider Storage"
    if not storage.is_dir():
        raise RuntimeError("open Files once to initialize On My iPhone storage")
    return storage


@contextmanager
def picker_export_directory(simulator: str):
    directory = picker_storage(simulator) / ("voytty-export-" + uuid.uuid4().hex[:8])
    directory.mkdir(mode=0o700)
    try:
        yield {"exportDirectory": directory.name}
        exports = list(directory.iterdir())
        if len(exports) != 1 or exports[0].name != "voytty-logs.jsonl":
            raise RuntimeError("diagnostic UI acceptance must save exactly one named export")
        lines = exports[0].read_text().splitlines()
        if not lines or json.loads(lines[0]).get("includes_debug") is not True or len(lines) < 2:
            raise RuntimeError("diagnostic export must include saved detail and event history")
        for line in lines[1:]:
            if not isinstance(json.loads(line).get("event"), str):
                raise RuntimeError("diagnostic export contains an invalid event record")
    finally:
        shutil.rmtree(directory)


@contextmanager
def picker_files(simulator: str):
    storage = picker_storage(simulator)
    prefix = "voytty-ui-" + uuid.uuid4().hex[:8]
    payload = bytes(index % 251 for index in range(12203))
    normal, oversized = storage / (prefix + ".txt"), storage / (prefix + "-large.bin")
    try:
        with normal.open("xb") as output:
            output.write(payload)
        with oversized.open("xb") as output:
            output.truncate(50_000_001)
        yield {"uploadFile": normal.name, "uploadSHA256": hashlib.sha256(payload).hexdigest(), "oversizedFile": oversized.name}
    finally:
        normal.unlink(missing_ok=True)
        oversized.unlink(missing_ok=True)


class HostControl:
    def __init__(self, repository: Path, fixture: dict):
        self.root = Path(fixture["fixtureRoot"]).resolve()
        if not (self.root / ".presentation-fixture").is_file():
            raise ValueError("UI host control requires an explicit presentation_fixture root")
        self.cli = [str(repository / "target/debug/examples/presentation_fixture"), "cli", str(self.root)]
        self.sessions: set[str] = set()
        self.controllers: list[tuple[subprocess.Popen, int, threading.Thread]] = []
        self.pid = int(fixture.get("daemonPID", "0"))
        self.paused = False
        self.resume_timer: threading.Timer | None = None

    def message(self, line: str):
        match = re.fullmatch(r"VOYTTY_UI_(SESSION|TAKEOVER|PAUSE|RESUME) (ios-keyboard-ui-[a-fA-F0-9]{8})", line)
        if not match:
            return
        action, name = match.groups()
        if action == "SESSION":
            self.sessions.add(name)
            return
        if name not in self.sessions:
            raise ValueError("takeover can only target the Session created by this UI run")
        if action == "PAUSE":
            if not self.is_fixture_daemon():
                raise ValueError("reconnection acceptance requires the exact disposable daemonPID")
            os.kill(self.pid, signal.SIGSTOP)
            self.paused = True
            self.resume_timer = threading.Timer(60, self.resume)
            self.resume_timer.daemon = True
            self.resume_timer.start()
            return
        if action == "RESUME":
            self.resume()
            return
        master, slave = pty.openpty()
        try:
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
            process = subprocess.Popen(self.cli + ["connect", "local", "--session", name, "--takeover"],
                                       stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
                                       env={**os.environ, "TERM": "xterm-256color"})
        finally:
            os.close(slave)

        def drain():
            try:
                while os.read(master, 65536):
                    pass
            except OSError:
                pass

        thread = threading.Thread(target=drain, daemon=True)
        thread.start()
        self.controllers.append((process, master, thread))

    def is_fixture_daemon(self):
        if self.pid <= 1:
            return False
        result = subprocess.run(["ps", "-p", str(self.pid), "-o", "args="], capture_output=True, text=True, check=False)
        args = shlex.split(result.stdout.strip())
        return len(args) == 3 and Path(args[0]).name == "presentation_fixture" and args[1] == "start" and Path(args[2]).resolve() == self.root

    def resume(self):
        if self.resume_timer:
            self.resume_timer.cancel()
        if self.paused:
            self.paused = False
            if self.is_fixture_daemon():
                os.kill(self.pid, signal.SIGCONT)

    def close(self):
        self.resume()
        for process, master, thread in self.controllers:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill(); process.wait()
            os.close(master)
            thread.join(timeout=1)
        # Names are random, emitted only after creation, and scoped to this
        # explicit disposable host. UI teardown normally already removed them.
        for name in self.sessions:
            subprocess.run(self.cli + ["session", "close", name, "--yes"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False, timeout=10)
