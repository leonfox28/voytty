#!/usr/bin/env python3
"""Reproducible iOS bindings, per-SDK Rust library, Xcode build and simulator tests."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import fcntl
from contextlib import ExitStack
from ui_acceptance import HostControl, picker_export_directory, picker_files

ROOT = Path(__file__).resolve().parents[2]
APP = ROOT / "apps/ios"
BUILD_ROOT = (ROOT / os.environ.get("CARGO_TARGET_DIR", "target")).resolve()
# cdylib/rlib package outputs have stable filenames. Keep the binding generator
# and Apple builds separate from workspace/Android builds with other features.
TARGET = BUILD_ROOT / "ios-native"


def run(*args: str, env: dict[str, str] | None = None) -> None:
    subprocess.run(args, cwd=ROOT, env=env, check=True)


def run_ui(args: list[str], simulator: str, fixture: dict, keyboard: bool) -> None:
    """Send a fixed physical-key sequence only when the disposable UI test asks."""
    helper = BUILD_ROOT / "ios/simulator-keyboard"
    helper.parent.mkdir(parents=True, exist_ok=True)
    if keyboard:
        run("xcrun", "clang", "-fobjc-arc", "-fmodules", "-framework", "Foundation",
            str(ROOT / "tools/ios/simulator-keyboard.m"), "-o", str(helper))
    developer = subprocess.check_output(["xcode-select", "-p"], text=True).strip()
    sent = False
    host = HostControl(ROOT, fixture)
    with subprocess.Popen(args, cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                          text=True, bufsize=1) as process:
        try:
            assert process.stdout is not None
            for line in process.stdout:
                print(line, end="", flush=True)
                host.message(line.strip())
                if line.strip() == "ZTERM_UI_KEYBOARD_NAVIGATION":
                    if sent:
                        raise RuntimeError("UI test requested duplicate keyboard injection")
                    sent = True
                    # USB HID Home, End, Delete, PgUp/PgDn, F1/F12, Up, Enter, Backspace.
                    run(str(helper), developer, simulator, "74", "77", "76", "75", "78",
                        "58", "69", "82", "40", "42")
            result = process.wait()
            if result:
                raise subprocess.CalledProcessError(result, args)
            if keyboard and not sent:
                raise RuntimeError("UI test did not request keyboard acceptance")
        finally:
            host.close()
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def prepare(ticket_file: Path | None = None, ui_fixture: Path | None = None) -> None:
    import tomllib
    run("cargo", "+1.98.0", "build", "--locked", "--target-dir", str(TARGET),
        "-p", "zterm-android", "-p", "zterm-uniffi-bindgen")
    generated = APP / "Generated"
    generated.mkdir(parents=True, exist_ok=True)
    run(str(TARGET / "debug/zterm-uniffi-bindgen"), "generate",
        str(TARGET / "debug/libzterm_android.dylib"), "--language", "swift",
        "--config", str(APP / "uniffi.toml"), "--out-dir", str(generated), "--no-format")
    with (ROOT / "Cargo.toml").open("rb") as source:
        version = tomllib.load(source)["workspace"]["package"]["version"]
    (generated / "Version.xcconfig").write_text(
        f"MARKETING_VERSION = {version}\nCURRENT_PROJECT_VERSION = 1\n", encoding="utf-8")
    # Test-only disposable ticket, never included in the application target or Git.
    ticket = generated / "AcceptanceTicket.txt"
    ticket.write_text(ticket_file.read_text().strip() if ticket_file else "", encoding="utf-8")
    ticket.chmod(0o600)
    (generated / "UIAcceptance.json").write_text(ui_fixture.read_text() if ui_fixture else "", encoding="utf-8")
    run("xcodegen", "generate", "--spec", str(APP / "project.yml"))


def native() -> None:
    platform = os.environ["PLATFORM_NAME"]
    architecture = os.environ["CURRENT_ARCH"]
    if architecture == "undefined_arch":
        architecture = os.environ["ARCHS"].strip()
    targets = {("iphoneos", "arm64"): "aarch64-apple-ios",
               ("iphonesimulator", "arm64"): "aarch64-apple-ios-sim",
               ("iphonesimulator", "x86_64"): "x86_64-apple-ios"}
    target = targets.get((platform, architecture))
    if target is None:
        raise SystemExit(f"unsupported iOS platform/architecture: {platform}/{architecture}")
    env = os.environ.copy()
    # Xcode's SDKROOT must not leak into host build scripts/proc macros.
    env.pop("SDKROOT", None)
    env["IPHONEOS_DEPLOYMENT_TARGET"] = "17.0"
    run("cargo", "+1.98.0", "rustc", "--locked", "--release", "-p", "zterm-android",
        "--lib", "--target", target, "--target-dir", str(TARGET), "--crate-type", "staticlib", env=env)
    import shutil
    output = Path(os.environ["BUILT_PRODUCTS_DIR"])
    output.mkdir(parents=True, exist_ok=True)
    shutil.copy2(TARGET / target / "release/libzterm_android.a", output / "libzterm_android.a")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("prepare", "native", "build", "device-build", "check", "ui-check"))
    parser.add_argument("--simulator", help="Explicit simulator UUID for test execution")
    parser.add_argument("--ticket-file", type=Path, help="Disposable host ticket for opt-in remote acceptance")
    parser.add_argument("--ui-fixture", type=Path, help="Explicit already-paired disposable host and restoration Session IDs")
    parser.add_argument("--ui-scope", choices=("all", "services", "files", "photos", "recovery", "reconnect", "connection", "navigation", "deletion", "sessions", "settings"), default="all",
                        help="Target native UI checks without unrelated physical-key injection")
    args = parser.parse_args()
    if args.command == "native":
        native()
        return
    if args.ticket_file and args.command != "check":
        parser.error("--ticket-file is only valid with check")
    if args.ui_fixture and args.command != "ui-check":
        parser.error("--ui-fixture is only valid with ui-check")
    if args.ui_scope != "all" and args.command != "ui-check":
        parser.error("--ui-scope is only valid with ui-check")
    if args.command == "ui-check":
        if not args.ui_fixture:
            parser.error("ui-check requires --ui-fixture for an already-paired disposable host")
        fixture = json.loads(args.ui_fixture.read_text())
        if not isinstance(fixture, dict) or not all(isinstance(fixture.get(key), str) and fixture[key] for key in ("host", "restoreSession", "fixtureRoot")):
            parser.error("UI fixture requires nonempty host, restoreSession and fixtureRoot")
        HostControl(ROOT, fixture)  # Validate before generating test resources.
        if args.ui_scope in ("all", "reconnect") and not HostControl(ROOT, fixture).is_fixture_daemon():
            parser.error("reconnection UI checks require daemonPID identifying this presentation_fixture host")
    prepare(args.ticket_file, args.ui_fixture)
    if args.command == "prepare":
        return
    destination = "generic/platform=iOS Simulator"
    signing = ["CODE_SIGN_IDENTITY=-"]
    if args.command == "device-build":
        destination = "generic/platform=iOS"
        signing = ["CODE_SIGNING_ALLOWED=NO"]
    if args.command in ("check", "ui-check"):
        if not args.simulator:
            parser.error("check requires --simulator UUID")
        if args.simulator == "auto":
            devices = json.loads(subprocess.check_output(["xcrun", "simctl", "list", "devices", "available", "--json"]))
            phones = [device["udid"] for runtime, entries in devices["devices"].items()
                      if ".iOS-" in runtime for device in entries
                      if device["name"].startswith("iPhone") and device["isAvailable"]]
            if not phones:
                parser.error("install an iOS simulator runtime and create an iPhone simulator in Xcode")
            args.simulator = phones[0]
        destination = f"platform=iOS Simulator,id={args.simulator}"
    try:
        testing = ["-parallel-testing-enabled", "NO"] if args.command in ("check", "ui-check") else []
        scopes = {"services": "testNotificationPermissionDeliveryAndNativeDiagnosticExport", "photos": "testNativePhotoUpload",
                  "files": "testNativeFileUploadAndLimit", "recovery": "testOccupiedTakeoverAndEndedRecovery",
                  "reconnect": "testTransportReconnectPreservesSessionAndOutput", "connection": "testConnectionFailureAlertAndRetry",
                  "navigation": "testNativeNavigationBackAndEdgeSwipe", "deletion": "testSessionDeletionUsesNativeAlert",
                  "sessions": "testNativeSessionSheetAndEditorTransitions", "settings": "testNativeSettingsAndHostRemovalCancellation"}
        if args.command == "ui-check" and args.ui_scope in scopes:
            testing += ["-only-testing:ZtermUITests/KeyboardUITests/" + scopes[args.ui_scope]]
        command = ["xcodebuild", "-project", str(APP / "Zterm.xcodeproj"), "-scheme", "ZtermUI" if args.command == "ui-check" else "Zterm",
            "-configuration", "Debug", "-destination", destination,
            "-derivedDataPath", str(BUILD_ROOT / "ios"), *signing, *testing,
            "test" if args.command in ("check", "ui-check") else "build"]
        if args.command == "ui-check":
            with ExitStack() as stack:
                if args.ui_scope in ("all", "services"):
                    fixture.update(stack.enter_context(picker_export_directory(args.simulator)))
                    (APP / "Generated/UIAcceptance.json").write_text(json.dumps(fixture), encoding="utf-8")
                if args.ui_scope in ("all", "files"):
                    fixture.update(stack.enter_context(picker_files(args.simulator)))
                    (APP / "Generated/UIAcceptance.json").write_text(json.dumps(fixture), encoding="utf-8")
                if args.ui_scope in ("all", "photos"):
                    run(*command[:-1], "build-for-testing")
                    runner = BUILD_ROOT / "ios/Build/Products/Debug-iphonesimulator/ZtermUITests-Runner.app"
                    run("xcrun", "simctl", "install", args.simulator, str(runner))
                    run("xcrun", "simctl", "privacy", args.simulator, "grant", "photos", "io.github.leonfox28.zterm.uitests.xctrunner")
                    command[-1] = "test-without-building"
                run_ui(command, args.simulator, fixture, keyboard=args.ui_scope == "all")
        else:
            run(*command)
    finally:
        (APP / "Generated/AcceptanceTicket.txt").write_text("", encoding="utf-8")
        (APP / "Generated/UIAcceptance.json").write_text("", encoding="utf-8")


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "native":
        main()  # Called by the owning Xcode build phase.
    else:
        BUILD_ROOT.mkdir(parents=True, exist_ok=True)
        with (BUILD_ROOT / "ios-build.lock").open("a") as build_lock:
            try:
                fcntl.flock(build_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise SystemExit("another iOS build owns the generated resources; finish it before starting this command")
            main()
