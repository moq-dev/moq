#!/usr/bin/env python3
"""Shared-minor policy and real, nonpublishing release-plz integration."""

import copy
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("shared_version", ROOT / "sh/version.py")
module = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = module
spec.loader.exec_module(module)


class Versions(unittest.TestCase):
    def setUp(self):
        (ROOT / ".tmp").mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=ROOT / ".tmp", prefix="version-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.env = dict(os.environ, CARGO_NET_OFFLINE="true", GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        self.config = {"minor": None, "exemptions": {"rust": ["independent"], "npm": ["@moq/qmux"]}}
        self.write("version.json", json.dumps(self.config))
        self.write("Cargo.toml", '[workspace]\nmembers = ["rs/*"]\nresolver = "2"\n')
        self.crate(
            "shared",
            "0.21.3",
            '[dependencies]\npromoted = { path = "../promoted", version = "0.0.1", optional = true }\n'
            '[dev-dependencies]\ndev = { path = "../dev" }\n'
            "[target.'cfg(windows)'.dependencies]\n"
            'renamed = { package = "target-only", path = "../target-only", version = "0.0.1" }\n',
        )
        self.crate(
            "promoted", "0.0.1", '[build-dependencies]\ntransitive = { path = "../transitive", version = "0.0.1" }\n'
        )
        for name in ("transitive", "archive", "dev", "experiment", "target-only"):
            self.crate(name, "0.0.1")
        self.crate(
            "binary", "0.21.0", '[dependencies]\narchive = { path = "../archive", version = "0.0.1" }\n', binary=True
        )
        self.crate("independent", "0.6.0")
        self.crate("moq-ffi", "0.21.3")
        self.crate(
            "private",
            "0.1.0",
            'publish = false\n[dependencies]\nexperiment = { path = "../experiment", version = "0.0.1" }\n',
        )
        self.call("cargo", "generate-lockfile", "--offline")
        self.write("package.json", json.dumps({"private": True, "workspaces": ["js/*"]}))
        for name, value in (
            ("signals", "0.2.5"),
            ("qmux", "0.7.0"),
            ("experiment", "0.0.1"),
            ("promoted", "0.0.1"),
            ("private", "0.1.0"),
        ):
            p = {"name": f"@moq/{name}", "version": value, "scripts": {"release": "bun publish"}}
            if name == "signals":
                p["dependencies"] = {"@moq/promoted": "workspace:^"}
                p["devDependencies"] = {"@moq/experiment": "workspace:^"}
            if name == "private":
                p["private"] = True
            self.write(f"js/{name}/package.json", json.dumps(p, indent="\t") + "\n")
        self.call("bun", "install", "--lockfile-only", "--ignore-scripts")
        for ecosystem, (_, path, _) in module.WRAPPERS.items():
            value = {
                "python": '[project]\nname = "moq-rs"\nversion = "0.21.3"\ndependencies = ["moq-ffi ~= 0.21.0"]\n',
                "kotlin": "moq.version=0.21.3\n",
                "dart": "name: moq\nversion: 0.21.3\ndependencies:\n  moq_ffi: ^0.21.0\n",
                "go": "0.21\n",
            }.get(ecosystem, "0.21.3\n")
            self.write(path, value)
        self.write("kt/moq/build.gradle.kts", 'val MOQ_FFI_RANGE = "[0.21.0,0.22)"\n')
        self.write(
            "uv.lock",
            'version = 1\n\n[[package]]\nname = "moq-rs"\nversion = "0.21.3"\nsource = { editable = "py/moq-rs" }\n'
            '\n[[package]]\nname = "unrelated"\nversion = "1.2.3"\n',
        )
        self.call("git", "init", "-q")
        self.call("git", "config", "user.name", "Fixture")
        self.call("git", "config", "user.email", "fixture@example.com")
        self.call("git", "config", "commit.gpgsign", "false")
        self.commit("chore: fixture")

    def write(self, path, content):
        path = self.root / path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content)

    def call(self, *args, cwd=None, ok=True):
        result = subprocess.run(args, cwd=cwd or self.root, env=self.env, capture_output=True, text=True)
        if ok:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def commit(self, message):
        self.call("git", "add", ".")
        self.call("git", "commit", "-qm", message)

    def crate(self, name, value, extra="", binary=False):
        self.write(
            f"rs/{name}/Cargo.toml", f'[package]\nname = "{name}"\nversion = "{value}"\nedition = "2024"\n{extra}'
        )
        self.write(
            f"rs/{name}/src/{'main' if binary else 'lib'}.rs", "fn main() {}\n" if binary else "pub fn original() {}\n"
        )

    def check(self, ok=True):
        return self.call(sys.executable, str(ROOT / "sh/version.py"), "--root", str(self.root), ok=ok)

    def cut(self, ok=True):
        return self.call(sys.executable, str(ROOT / "sh/version.py"), "--root", str(self.root), "0.22.0", ok=ok)

    def snapshot(self):
        return {
            str(p.relative_to(self.root)): p.read_bytes()
            for p in self.root.rglob("*")
            if p.is_file() and not set(p.relative_to(self.root).parts) & {".git", ".tmp", "node_modules"}
        }

    def test_disabled_before_cut_and_invalid_marker_fails(self):
        self.assertIn("disabled", self.check().stdout)
        # Disabled enforcement does not require Cargo, the lockfiles, or packages.
        (self.root / "Cargo.toml").unlink()
        self.assertIn("disabled", self.check().stdout)
        for value in ("", "0.22.0", "0.0", "0.022"):
            self.write("version.json", json.dumps({**self.config, "minor": value}))
            self.assertNotEqual(self.check(ok=False).returncode, 0)

    def test_cut_promotes_only_published_library_dependencies_and_preserves_exemptions(self):
        before = self.snapshot()
        self.cut()
        self.check()
        packages = {p.key: p.current for p in module.packages(self.root)}
        for name in ("shared", "promoted", "transitive", "binary", "target-only"):
            self.assertEqual(packages["rust", name], "0.22.0")
        for name in ("archive", "dev", "experiment"):
            self.assertEqual(packages["rust", name], "0.0.1")
        self.assertEqual(packages["rust", "independent"], "0.6.0")
        self.assertNotIn(("rust", "private"), packages)
        for name in ("signals", "promoted"):
            self.assertEqual(packages["npm", f"@moq/{name}"], "0.22.0")
        for name in ("qmux", "experiment", "private"):
            path = f"js/{name}/package.json"
            self.assertEqual((self.root / path).read_bytes(), before[path])
        for ecosystem in module.WRAPPERS:
            self.assertEqual(packages[ecosystem, module.WRAPPERS[ecosystem][0]], "0.22.0")
        self.assertEqual((self.root / "go/wrapper/VERSION").read_text(), "0.22\n")
        self.assertIn('version = "0.22.0", optional = true', (self.root / "rs/shared/Cargo.toml").read_text())
        lock = tomllib.loads((self.root / "Cargo.lock").read_text())
        self.assertEqual(next(p["version"] for p in lock["package"] if p["name"] == "promoted"), "0.22.0")
        # No real changelogs exist in this fixture. set-version must not require
        # them, or create release sections or rewrite published history.
        self.assertFalse(list((self.root / "rs").glob("*/CHANGELOG.md")))
        bun_before = json.loads(
            self.call(
                "bun", "-e", "console.log(JSON.stringify(Bun.JSON5.parse(await Bun.file('bun.lock').text())))"
            ).stdout
        )
        self.call("bun", "install", "--lockfile-only", "--ignore-scripts", "--frozen-lockfile")
        bun_after = json.loads(
            self.call(
                "bun", "-e", "console.log(JSON.stringify(Bun.JSON5.parse(await Bun.file('bun.lock').text())))"
            ).stdout
        )
        self.assertEqual(bun_before, bun_after)
        self.assertIn('name = "unrelated"\nversion = "1.2.3"', (self.root / "uv.lock").read_text())
        self.assertEqual(json.loads((self.root / "version.json").read_text())["minor"], "0.22")

    def test_check_reports_each_ecosystem_mismatch_promotion_and_stale_exemption(self):
        self.cut()
        pristine = self.snapshot()
        for ecosystem, (_, path, pattern) in module.WRAPPERS.items():
            with self.subTest(ecosystem=ecosystem):
                self.write(
                    path, module.replace(pristine[path].decode(), pattern, "0.21" if ecosystem == "go" else "0.21.0")
                )
                self.assertIn("must join 0.22.x", self.check(ok=False).stderr)
                (self.root / path).write_bytes(pristine[path])
        for ecosystem, path in (
            ("rust", "rs/shared/Cargo.toml"),
            ("npm", "js/signals/package.json"),
            ("rust", "rs/promoted/Cargo.toml"),
            ("npm", "js/promoted/package.json"),
        ):
            with self.subTest(ecosystem=ecosystem, path=path):
                self.write(path, pristine[path].decode().replace("0.22.0", "0.0.1" if "promoted" in path else "0.21.1"))
                result = self.check(ok=False)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("must join 0.22.x", result.stderr)
                (self.root / path).write_bytes(pristine[path])
        for ecosystem in ("rust", "npm"):
            config = copy.deepcopy(json.loads((self.root / "version.json").read_text()))
            config["exemptions"][ecosystem].append("removed")
            self.write("version.json", json.dumps(config))
            self.assertIn("exemptions name missing", self.check(ok=False).stderr)
            (self.root / "version.json").write_bytes(pristine["version.json"])

    def test_independent_patches_and_lock_consistency(self):
        self.cut()
        for path, old, new in (
            ("rs/shared/Cargo.toml", 'version = "0.22.0"', 'version = "0.22.3"'),
            ("js/signals/package.json", "0.22.0", "0.22.7"),
        ):
            text = (self.root / path).read_text()
            self.write(path, text.replace(old, new, 1))
        signals = next(p for p in module.packages(self.root) if p.key == ("npm", "@moq/signals"))
        path, pattern = module.lock_pattern(signals, self.root)
        self.write(path, module.replace((self.root / path).read_text(), pattern, "0.22.7"))
        self.call("cargo", "generate-lockfile", "--offline")
        self.check()
        for path in ("bun.lock", "uv.lock"):
            text = (self.root / path).read_text()
            self.write(path, text.replace("0.22.", "0.21.", 1))
            self.assertIn("lockfile version differs", self.check(ok=False).stderr)
            self.write(path, text)

    def test_ffi_constraints_advance_and_reject_stale_ranges(self):
        self.cut()
        for path, pattern, template in module.FFI_REQUIREMENTS:
            with self.subTest(path=path):
                text = (self.root / path).read_text()
                self.assertEqual(module.captured(text, pattern), template.format(version="0.22.0", next_minor=23))
                for floor, upper in (("0.21.0", 22), ("0.23.0", 24)):
                    self.write(path, module.replace(text, pattern, template.format(version=floor, next_minor=upper)))
                    self.assertIn("FFI requirement", self.check(ok=False).stderr)
                self.write(path, text)
        self.write(
            "rs/moq-ffi/Cargo.toml", (self.root / "rs/moq-ffi/Cargo.toml").read_text().replace("0.22.0", "0.22.3")
        )
        for path, pattern, template in module.FFI_REQUIREMENTS:
            self.write(
                path,
                module.replace(
                    (self.root / path).read_text(), pattern, template.format(version="0.22.2", next_minor=23)
                ),
            )
        self.call("cargo", "generate-lockfile", "--offline")
        self.check()

    def test_cut_refuses_bad_targets_and_restores_partial_external_failure(self):
        before = self.snapshot()
        for target in ("0.22", "0.22.1", "1.0.0", "0.0.0", "0.20.0", "0.022.0", "0.22.0-wip"):
            result = self.call(sys.executable, str(ROOT / "sh/version.py"), "--root", str(self.root), target, ok=False)
            self.assertNotEqual(result.returncode, 0, target)
            self.assertEqual(self.snapshot(), before)
        # release-plz can fail after editing some manifests. Keep every caller
        # edit and leave the activation marker disabled on that failure.
        binary = self.root / ".tmp/bin/release-plz"
        self.write(
            ".tmp/bin/release-plz", "#!/bin/sh\nprintf broken > Cargo.toml\nprintf broken > Cargo.lock\nexit 1\n"
        )
        binary.chmod(0o755)
        self.env["PATH"] = f"{binary.parent}:{self.env['PATH']}"
        result = self.cut(ok=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.snapshot(), before)

    def test_release_plz_keeps_manual_cut_despite_breaking_commits(self):
        # update is the version/changelog engine used by release-pr. Local
        # registry manifests exercise it without credentials, publishing, or a forge.
        self.write(".release-plz.toml", "[workspace]\nsemver_check = false\ndependencies_update = false\n")
        for p in module.packages(self.root):
            if p.ecosystem == "rust":
                self.write(
                    p.path.parent / "CHANGELOG.md", f"# Changelog\n\n## [Unreleased]\n\n## [{p.current}] - 2026-01-01\n"
                )
                self.call("git", "tag", f"{p.name}-v{p.current}")
        self.commit("chore: release baseline")
        registry = self.root / ".tmp/registry"
        shutil.copytree(self.root / "rs", registry / "rs")
        shutil.copy(self.root / "Cargo.toml", registry / "Cargo.toml")
        self.write("rs/shared/src/lib.rs", "pub fn replacement() {}\n")
        self.commit("feat!: replace the public API")
        changelogs = {p: p.read_bytes() for p in (self.root / "rs").glob("*/CHANGELOG.md")}
        self.cut()
        for path, original in changelogs.items():
            self.assertEqual(path.read_bytes(), original)
        self.commit("chore: shared minor cut")
        result = self.call(
            "release-plz",
            "update",
            "--registry-manifest-path",
            str(registry / "Cargo.toml"),
            "--repo-url",
            "https://github.com/moq-dev/moq",
        )
        self.assertIn("shared`: 0.21.3 -> 0.22.0", result.stdout)
        self.check()
        self.assertIn("## [0.22.0]", (self.root / "rs/shared/CHANGELOG.md").read_text())
        # Unchanged published members also retain the cut; exempt packages keep
        # independent versioning instead of being coupled by version_group.
        self.assertEqual(
            tomllib.loads((self.root / "rs/binary/Cargo.toml").read_text())["package"]["version"], "0.22.0"
        )


if __name__ == "__main__":
    unittest.main()
