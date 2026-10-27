#!/usr/bin/env python3
"""Keep shipped project notices identical to their canonical repository texts."""

import argparse
import hashlib
from pathlib import Path
import tomllib

ROOT = Path(__file__).resolve().parents[1]
ASSETS = ROOT / "apps/android/app/src/main/assets/licenses"
COPIES = {
    "LICENSE": "Voytty-PolyForm-Noncommercial-1.0.0.txt",
    "NOTICE": "Voytty-NOTICE.txt",
    "COMMERCIAL-LICENSE.md": "Voytty-Commercial-License.txt",
}
LICENSE_ID = "PolyForm-Noncommercial-1.0.0"
LICENSE_SHA256 = "ffcca38841adb694b6f380647e15f17c446a4d1656fed51a1e2041d064c94cc8"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("check", "sync"))
    args = parser.parse_args()
    if hashlib.sha256((ROOT / "LICENSE").read_bytes()).hexdigest() != LICENSE_SHA256:
        raise SystemExit("LICENSE differs from the official unmodified PolyForm text")
    workspace = tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]
    if workspace["package"].get("license") != LICENSE_ID:
        raise SystemExit("workspace must declare the selected project license")
    for member in workspace["members"]:
        package = tomllib.loads((ROOT / member / "Cargo.toml").read_text())["package"]
        if package.get("license") != {"workspace": True}:
            raise SystemExit(f"{member} must inherit the workspace license")
    probe = tomllib.loads((ROOT / "tests/relay/handshake-probe/Cargo.toml").read_text())
    if probe["package"].get("license") != LICENSE_ID:
        raise SystemExit("the isolated first-party relay probe must declare the project license")
    for source, destination in COPIES.items():
        expected = (ROOT / source).read_bytes()
        target = ASSETS / destination
        if args.command == "sync":
            target.write_bytes(expected)
        elif not target.is_file() or target.read_bytes() != expected:
            raise SystemExit(f"{target.relative_to(ROOT)} is stale; run just licensing-sync")
    print(f"project license metadata and mobile copies {args.command} verified")


if __name__ == "__main__":
    main()
