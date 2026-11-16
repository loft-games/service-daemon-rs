#!/usr/bin/env python3
"""Locate the release smoke test through Cargo's public artifact messages."""

import json
from pathlib import Path
import subprocess
import sys


def find_binary(target: str) -> Path:
    command = [
        "cargo", "test", "--release", "-p", "service-daemon",
        "--test", "linkme_smoke", "--no-run", "--message-format=json-render-diagnostics",
    ]
    if target != "host":
        command.extend(["--target", target])
    # Keep compiler errors on stderr while capturing machine-readable artifacts.
    result = subprocess.run(command, check=True, stdout=subprocess.PIPE, encoding="utf-8")
    binaries = set()
    for line in result.stdout.splitlines():
        message = json.loads(line)
        if (
            message.get("reason") == "compiler-artifact"
            and message.get("target", {}).get("name") == "linkme_smoke"
            and "test" in message.get("target", {}).get("kind", [])
            and message.get("profile", {}).get("test")
            and message.get("executable")
        ):
            binaries.add(Path(message["executable"]))
    if len(binaries) != 1:
        raise ValueError(f"Expected one linkme_smoke executable from Cargo; found {len(binaries)}")
    binary = binaries.pop()
    if not binary.is_file():
        raise ValueError(f"Cargo-reported linkme_smoke executable does not exist: {binary}")
    return binary


def main() -> int:
    try:
        print(find_binary(sys.argv[1] if len(sys.argv) > 1 else "host"))
    except subprocess.CalledProcessError as error:
        print(f"Failed to build linkme_smoke: {error}", file=sys.stderr)
        return error.returncode
    except (OSError, ValueError) as error:
        print(f"Could not locate linkme_smoke: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
