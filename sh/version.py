#!/usr/bin/env python3
"""Set the shared release minor, or check it without enforcing before the first cut."""

import argparse
import json
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
SEMVER = r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"


def version(value):
    if not re.fullmatch(SEMVER, value):
        raise ValueError(f"expected a stable MAJOR.MINOR.PATCH version, got {value!r}")
    return tuple(map(int, value.split(".")))


def replace(text, pattern, value):
    """Change exactly one captured version, preserving surrounding formatting."""
    matches = list(re.finditer(pattern, text, re.MULTILINE | re.DOTALL))
    if len(matches) != 1:
        raise ValueError(f"expected one version matching {pattern!r}, found {len(matches)}")
    match = matches[0]
    return text[: match.start(1)] + value + text[match.end(1) :]


@dataclass
class Package:
    ecosystem: str
    name: str
    current: str
    path: Path
    library: bool = True
    dependencies: set[str] = field(default_factory=set)

    @property
    def key(self):
        return self.ecosystem, self.name


# These wrappers have one independently owned version. Generated FFI packages
# take their version from the Rust crate and must not be edited here.
WRAPPERS = {
    "python": ("moq-rs", "py/moq-rs/pyproject.toml", r'(?s:\[project\]\n(?:(?!\n\[).)*?\nversion = ")([^"\n]+)'),
    "swift": ("moq", "swift/VERSION", r"^([^\n]+)$"),
    "kotlin": ("moq", "kt/gradle.properties", r"^moq\.version=([^\n]+)$"),
    "dart": ("moq", "dart/moq/pubspec.yaml", r"^version: ([^\n]+)$"),
    "go": ("moq", "go/wrapper/VERSION", r"^([^\n]+)$"),
    "cpp": ("moq", "cpp/moq/VERSION", r"^([^\n]+)$"),
    "obs": ("moq", "cpp/obs/VERSION", r"^([^\n]+)$"),
}


def run(root, *args):
    return subprocess.run(args, cwd=root, check=True, text=True, capture_output=True).stdout


def cargo_metadata(root):
    return json.loads(run(root, "cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"))


def packages(root, metadata=None):
    metadata = cargo_metadata(root) if metadata is None else metadata
    members = set(metadata["workspace_members"])
    rust = [p for p in metadata["packages"] if p["id"] in members and p["publish"] != []]
    names = {Path(p["manifest_path"]).parent.resolve(): p["name"] for p in rust}
    result = []
    for p in rust:
        result.append(
            Package(
                "rust",
                p["name"],
                p["version"],
                Path(p["manifest_path"]),
                any(set(t["kind"]) - {"bin", "example", "test", "bench", "custom-build"} for t in p["targets"]),
                {
                    names[Path(d["path"]).resolve()]
                    for d in p["dependencies"]
                    if d["kind"] != "dev" and d.get("path") and Path(d["path"]).resolve() in names
                },
            )
        )
    workspace = json.loads((root / "package.json").read_text())["workspaces"]
    for path in sorted({p / "package.json" for pattern in workspace for p in root.glob(pattern)}):
        p = json.loads(path.read_text())
        if p.get("private") or "release" not in p.get("scripts", {}):
            continue
        result.append(
            Package(
                "npm",
                p["name"],
                p["version"],
                path,
                dependencies=set(p.get("dependencies", {}))
                | set(p.get("optionalDependencies", {}))
                | set(p.get("peerDependencies", {})),
            )
        )
    for ecosystem, (name, path, pattern) in WRAPPERS.items():
        path = root / path
        matches = list(re.finditer(pattern, path.read_text(), re.MULTILINE))
        if len(matches) != 1:
            raise ValueError(f"{path}: expected one version")
        value = matches[0][1]
        if ecosystem == "go":
            value += ".0"
        result.append(Package(ecosystem, name, value, path))
    for package in result:
        version(package.current)
    return result


def eligible(packages, exemptions):
    known = {p.key for p in packages}
    exempt = {(ecosystem, name) for ecosystem, names in exemptions.items() for name in names}
    if stale := exempt - known:
        raise ValueError(f"exemptions name missing published packages: {sorted(stale)}")
    selected = {p.key for p in packages if p.key not in exempt and version(p.current)[:2] != (0, 0)}
    # Promotion follows normal/build dependencies, including optional and target
    # dependencies, transitively. Binary-only Rust dependents expose no library API.
    while True:
        promoted = {
            (p.ecosystem, name)
            for p in packages
            if p.key in selected and p.library
            for name in p.dependencies
            if (p.ecosystem, name) in known and (p.ecosystem, name) not in exempt
        }
        following = selected | promoted
        if following == selected:
            return [p for p in packages if p.key in selected]
        selected = following


def lock_pattern(package, root):
    if package.ecosystem == "npm":
        path = package.path.parent.relative_to(root).as_posix()
        return "bun.lock", rf'^    "{re.escape(path)}": \{{\n(?:(?!^    \}},).)*?"version": "([^"\n]+)'
    return "uv.lock", r'(?s:\[\[package\]\]\nname = "moq-rs"\nversion = ")([^"\n]+)'


def lock_version(root, package):
    path, pattern = lock_pattern(package, root)
    matches = list(re.finditer(pattern, (root / path).read_text(), re.MULTILINE | re.DOTALL))
    if len(matches) != 1:
        raise ValueError(f"{path}: expected one workspace version for {package.name}")
    return matches[0][1]


def check(root, config):
    minor = config["minor"]
    if minor is None:
        print("shared version: disabled until the release cut runs just version")
        return
    if not re.fullmatch(r"0\.[1-9][0-9]*", minor):
        raise ValueError(f"invalid shared minor: {minor!r}")
    selected = eligible(packages(root), config["exemptions"])
    errors = []
    for p in selected:
        if ".".join(map(str, version(p.current)[:2])) != minor:
            errors.append(f"{p.ecosystem} {p.name}: {p.current} must join {minor}.x")
        if p.ecosystem in ("npm", "python") and lock_version(root, p) != p.current:
            errors.append(f"{p.name}: lockfile version differs from {p.current}")
    if errors:
        raise ValueError("\n".join(errors))
    print(f"shared version: {len(selected)} packages on {minor}.x (patches independent)")


def bump(root, config, target):
    major, minor, patch = version(target)
    if major != 0 or minor == 0 or patch != 0:
        raise ValueError("a shared cut must be 0.MINOR.0 with a nonzero minor")
    metadata = cargo_metadata(root)
    selected = eligible(packages(root, metadata), config["exemptions"])
    if any(version(p.current) > (major, minor, patch) for p in selected):
        raise ValueError("the cut must not downgrade any package; choose a newer minor")
    # Prepare every text edit before release-plz mutates anything. Lockfile edits
    # touch only workspace versions, preserving the dependency graph and audits.
    edits = {}
    for p in selected:
        if p.ecosystem == "rust":
            continue
        text = p.path.read_text()
        if p.ecosystem == "npm":
            pattern = r'^\s*"version": "([^"\n]+)"'
        else:
            pattern = WRAPPERS[p.ecosystem][2]
        edits[p.path] = replace(text, pattern, f"{major}.{minor}" if p.ecosystem == "go" else target)
        if p.ecosystem in ("npm", "python"):
            if lock_version(root, p) != p.current:
                raise ValueError(f"{p.name}: lockfile version differs from {p.current}")
            path, pattern = lock_pattern(p, root)
            path = root / path
            edits[path] = replace(edits.get(path, path.read_text()), pattern, target)
    rust = [p for p in selected if p.ecosystem == "rust"]
    # set-version renames the last release heading, including published history,
    # and refuses crates without a changelog. Give it disposable changelogs;
    # release-pr will generate the real entries from the committed cut later.
    scratch = root / ".tmp"
    scratch.mkdir(exist_ok=True)
    paths = {root / "Cargo.toml", root / "Cargo.lock", root / "version.json", *edits}
    paths.update(Path(p["manifest_path"]) for p in metadata["packages"] if p["id"] in metadata["workspace_members"])
    originals = {path: path.read_bytes() if path.exists() else None for path in paths}
    try:
        with tempfile.TemporaryDirectory(dir=scratch, prefix="version-") as temporary:
            directory = Path(temporary)
            config_path = directory / "release-plz.toml"
            sections = []
            for p in rust:
                changelog = directory / f"{p.name}.md"
                changelog.write_text(f"# Changelog\n\n## [{p.current}]\n")
                sections.append(f'[[package]]\nname = "{p.name}"\nchangelog_path = {json.dumps(str(changelog))}\n')
            config_path.write_text("\n".join(sections))
            if rust:
                run(
                    root,
                    "release-plz",
                    "set-version",
                    "--config",
                    str(config_path),
                    *[f"{p.name}@{target}" for p in rust],
                )
        for path, text in edits.items():
            path.write_text(text)
        config = {**config, "minor": f"{major}.{minor}"}
        check(root, config)
        # Activate enforcement only after every ecosystem passed validation.
        (root / "version.json").write_text(json.dumps(config, indent="\t") + "\n")
    except BaseException:
        for path, original in originals.items():
            if original is None:
                path.unlink(missing_ok=True)
            else:
                path.write_bytes(original)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", nargs="?", help="0.MINOR.0 for a cut; omit to check")
    parser.add_argument("--root", type=Path, default=ROOT, help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        root = args.root.resolve()
        config = json.loads((root / "version.json").read_text())
        if args.target:
            bump(root, config, args.target)
        else:
            check(root, config)
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f"shared version: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError):
            print(error.stderr, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
