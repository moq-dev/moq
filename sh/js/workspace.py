#!/usr/bin/env python3
"""Run checks, builds and tests for changed Bun workspaces and their dependents."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time


ROOT = Path(__file__).resolve().parents[2]
DEPENDENCIES = ("dependencies", "devDependencies", "peerDependencies", "optionalDependencies")
SOURCES = (".ts", ".tsx", ".mts", ".cts", ".js", ".mjs", ".cjs")
# A string literal climbing out of its directory, like a test reading a shared corpus.
RELATIVE = re.compile(r"""["'`]((?:\.\./)+[^"'`$?\s]*)""")


def inputs(root, directory):
    """Paths outside a workspace that its sources name relative to themselves.

    These include path-algebra test strings that name nothing, which can only
    over-select, and inputs a diff deletes, whose readers must still run.
    """
    found = set()
    for folder, children, files in os.walk(root / directory):
        children[:] = [child for child in children if child not in ("node_modules", "dist")]
        for file in files:
            if not file.endswith(SOURCES):
                continue
            for literal in RELATIVE.findall((Path(folder) / file).read_text(errors="replace")):
                path = os.path.relpath(os.path.normpath(os.path.join(folder, literal)), root)
                # Skip paths in this workspace or its ancestors, outside the repository, and
                # installed packages (the lockfile covers them).
                if (path in (".", "..") or path.startswith("../") or "node_modules" in path.split("/")
                        or path == directory or path.startswith(directory + "/")
                        or directory.startswith(path + "/")):
                    continue
                found.add(path)
    return found


def reads(path, target):
    """Whether a changed path is an input, lies under it, or is an extensionless import's file."""
    return path == target or path.startswith(target + "/") or os.path.splitext(path)[0] == target


def selection(root, paths):
    """Read ownership and dependency edges from the current workspace manifests."""
    manifest = json.loads((root / "package.json").read_text())
    packages = {}
    for directory in manifest["workspaces"]:
        # This workspace uses literal directories, like common/deps.ts. Refuse a
        # new manifest shape rather than silently checking an incomplete graph.
        if re.search(r"[*?\[\]{}!]", directory):
            raise ValueError(f"unsupported workspace pattern: {directory}")
        package = json.loads((root / directory / "package.json").read_text())
        name = package["name"]
        if name in packages:
            raise ValueError(f"duplicate workspace name: {name}")
        packages[name] = {"directory": directory, "manifest": package,
                          "inputs": inputs(root, directory)}

    selected = set()
    # A workspace's `bench/` is no export, so no dependent can import it: only its owner checks it.
    benched = set()
    fallback = paths is None
    for path in paths or []:
        owners = [(name, p["directory"]) for name, p in packages.items()
                  if path == p["directory"] or path.startswith(p["directory"] + "/")]
        if not owners:
            fallback = True
            break
        name, directory = max(owners, key=lambda owner: len(owner[1]))
        (benched if path.startswith(directory + "/bench/") else selected).add(name)
        # Manifests miss a package reading another tree's files, so add those readers too.
        selected |= {name for name, package in packages.items()
                     if any(reads(path, i) for i in package["inputs"])}

    if fallback:
        selected = set(packages)
    else:
        while True:
            dependents = {name for name, package in packages.items()
                          if any(dep in selected for field in DEPENDENCIES
                                 for dep in package["manifest"].get(field, {}))}
            expanded = selected | dependents
            if expanded == selected:
                break
            selected = expanded
        selected |= benched

    return {"packages": {name: packages[name] for name in sorted(selected)},
            "workspaces": [p["directory"] for p in packages.values()],
            "inputs": sorted(set().union(*(p["inputs"] for p in packages.values()))),
            "all": fallback}


def unmapped(root, paths):
    """Existing inputs the dispatcher's static JS impact map misses, so changing one would skip its readers."""
    impact = re.compile(re.search(r"^\s*\[js\]='([^']*)'", (root / "sh/dispatch.sh").read_text(),
                                  re.M).group(1))
    # A directory input matches through the files under it, and an import through its file.
    return [path for path in paths
            if any((root / c).exists() for c in (path, *(path + e for e in SOURCES)))
            and not any(impact.search(c) for c in (path, path + "/", *(path + e for e in SOURCES)))]


def run(action, plan):
    packages = plan["packages"]
    if not packages:
        print("js: no packages affected; skipping.")
        return
    print("js: selecting " + " ".join(packages), flush=True)
    os.chdir(ROOT)

    def command(*args, cwd=ROOT):
        subprocess.run(args, cwd=cwd, check=True)

    def scripts(verb):
        filters = [f"--filter={name}" for name, package in packages.items()
                   if verb in package["manifest"].get("scripts", {})]
        if filters:
            concurrency = [] if os.environ.get("GITHUB_ACTIONS") == "true" else ["--sequential"]
            command("bun", "run", *concurrency, *filters, "--elide-lines=0", verb)

    def reports():
        # Each published package's API reports against its fresh build; `just fix` sets
        # MOQ_API_REPORT=update to rewrite them. No package build runs this, so a consumer
        # building one from source needs none of the report toolchain.
        directories = [package["directory"] for package in packages.values()
                       if "release" in package["manifest"].get("scripts", {})]
        if directories:
            command("bun", "js/common/api.ts", "check", *directories)

    command("bun", "install", "--frozen-lockfile")
    if action == "test":
        command("python3", "sh/js/playwright.test.py")
        command("bun", "test", "js/common/api.test.ts", "js/common/deps.test.ts", "js/common/worklet.test.ts",
                "js/common/release.test.ts")
        scripts("test")
        return
    if action == "build":
        scripts("build")
        reports()
        return

    missing = unmapped(ROOT, plan["inputs"])
    if missing:
        raise RuntimeError("add these cross-tree JS inputs to the js pattern in sh/dispatch.sh: "
                           + " ".join(missing))
    command("python3", "sh/rs/stats-docs.py")
    # These inexpensive shared checks still protect every package's imports.
    result = subprocess.run(["grep", "-rn", "--include=*.ts", "--include=*.tsx",
                             "--exclude-dir=node_modules", "--exclude-dir=dist",
                             r"import\.meta\.env", "js"])
    if result.returncode != 1:
        raise RuntimeError("import.meta.env scan found a reference or failed")
    command("python3", "sh/js/playwright.py")
    command("bun", "js/common/deps.ts")
    scripts("check")
    if plan["all"]:
        command("bunx", "tsc", "--noEmit", "--project", "test/tsconfig.json")
    else:
        # Unowned harnesses have no manifest dependency edges, so keep checking
        # them. Exclude only workspace harnesses outside this selection.
        selected_dirs = {p["directory"] for p in packages.values()}
        excluded = [str(ROOT / directory) for directory in plan["workspaces"]
                    if directory.startswith("test/") and directory not in selected_dirs]
        harness = json.loads((ROOT / "test/tsconfig.json").read_text())
        descriptor, name = tempfile.mkstemp(suffix=".json", dir=ROOT / "test")
        try:
            with os.fdopen(descriptor, "w") as config:
                json.dump({"extends": "./tsconfig.json",
                           "exclude": harness.get("exclude", []) + excluded}, config)
            command("bunx", "tsc", "--noEmit", "--project", name)
        finally:
            Path(name).unlink()
    # The fallback also covers unowned inputs, including demo/pub's Worker.
    # Recommended rules default to warn, which Biome otherwise lets pass.
    command("bun", "biome", "check", "--error-on-warnings", *(["js", "bench/browser", "test", "doc", "demo"] if plan["all"] else
                                       [p["directory"] for p in packages.values()] + ["js/common"]))
    scripts("build")
    reports()
    for name in ("@moq/watch", "@moq/publish"):
        if name in packages:
            path = "./" + packages[name]["directory"] + "/dist/index.js"
            command("bun", "-e", f"await import({json.dumps(path)})")


def main():
    action, source = sys.argv[1:]
    if action not in ("check", "test", "build", "plan"):
        raise ValueError(f"unsupported action: {action}")
    paths = None if source == "--all" else Path(source).read_text().splitlines()
    plan = selection(ROOT, paths)
    if action == "plan":
        print(json.dumps(plan, default=sorted))
    else:
        if plan["packages"] and os.environ.get("GITHUB_ACTIONS") != "true" and os.environ.get("LOCAL_SUPERVISED") != "1":
            os.execvp("bash", ["bash", str(ROOT / "sh/local.sh"),
                               sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]])
        started = time.monotonic()
        run(action, plan)
        print(f"js: {action} finished in {time.monotonic() - started:.2f}s", flush=True)


if __name__ == "__main__":
    main()
