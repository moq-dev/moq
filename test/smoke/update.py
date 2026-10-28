#!/usr/bin/env python3
"""Update smoke's Nix toolchain and its matching npm Playwright pin together."""

import json
from pathlib import Path
import re
import subprocess


def main():
    smoke = Path(__file__).resolve().parent
    subprocess.run(["nix", "flake", "update"], cwd=smoke, check=True)
    system = subprocess.check_output(
        ["nix", "eval", "--impure", "--raw", "--expr", "builtins.currentSystem"], text=True
    ).strip()
    # Evaluate the hook instead of entering the shell: updating a version must not
    # download every client toolchain or run the refreshed shellHook.
    hook = subprocess.check_output(
        ["nix", "eval", "--raw", f".#devShells.{system}.default.shellHook"], cwd=smoke, text=True
    )
    versions = re.findall(r'^export PLAYWRIGHT_VERSION="([0-9]+\.[0-9]+\.[0-9]+)"$', hook, re.MULTILINE)
    if len(versions) != 1:
        raise ValueError("smoke's shellHook must export one exact stable PLAYWRIGHT_VERSION")
    version = versions[0]
    manifest = smoke / "clients/js/package.json"
    package = json.loads(manifest.read_text())
    package["devDependencies"]["playwright"] = version
    manifest.write_text(json.dumps(package, indent="\t", ensure_ascii=False) + "\n")
    print(f"smoke toolchain updated with playwright {version}")


if __name__ == "__main__":
    main()
