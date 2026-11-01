#!/usr/bin/env python3
"""Fail when a workspace member's edge to the workspace-hack is missing or wrong.

The hack is a path-only dev-dependency, so tests and lints share one build of
every third-party crate while `cargo publish` strips it. `cargo hakari
manage-deps` only writes normal dependencies, so this checks the edges instead:
every member dev-depends on the hack, except those `.config/hakari.toml`
excludes, which must not depend on it at all. Run from `just rs lint`.
"""

import json
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

config = tomllib.loads((ROOT / ".config/hakari.toml").read_text())
hack = config["hakari-package"]
excluded = {hack}
for section in ("final-excludes", "traversal-excludes"):
	excluded |= set(config.get(section, {}).get("workspace-members", []))

metadata = json.loads(
	subprocess.run(
		["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
		cwd=ROOT,
		check=True,
		capture_output=True,
		text=True,
	).stdout
)

errors = []
for package in metadata["packages"]:
	name = package["name"]
	edges = [dep for dep in package["dependencies"] if dep["name"] == hack]
	manifest = Path(package["manifest_path"]).relative_to(ROOT)
	if name in excluded:
		if edges:
			errors.append(f"{manifest}: {name} is excluded in .config/hakari.toml but depends on {hack}")
		continue
	# A version requirement would survive `cargo publish` and point at a crate
	# that was never published.
	if not any(dep["kind"] == "dev" and dep.get("path") and dep["req"] == "*" for dep in edges):
		hack_dir = Path(next(p["manifest_path"] for p in metadata["packages"] if p["name"] == hack)).parent
		rel = Path(*[".."] * len(manifest.parent.parts)) / hack_dir.relative_to(ROOT)
		errors.append(f'{manifest}: add `{hack} = {{ path = "{rel}" }}` under [dev-dependencies]')
	for dep in edges:
		if dep["kind"] != "dev":
			errors.append(f"{manifest}: {hack} must be a dev-dependency only, or publishing {name} fails")

if errors:
	print("\n".join(errors), file=sys.stderr)
	sys.exit(1)
