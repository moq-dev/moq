"""Exercise graph selection without installing dependencies."""

import importlib.util
import json
import os
import shutil
from pathlib import Path
import tempfile
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from test_env import dispatch_env

spec = importlib.util.spec_from_file_location("select_js", Path(__file__).with_name("workspace.py"))
selector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(selector)


class SelectionTest(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        self.packages = {
            "js/core": {"name": "@example/core"},
            "js/view": {"name": "@example/view", "dependencies": {"@example/core": "workspace:*"}},
            "doc": {"name": "docs", "devDependencies": {"@example/view": "workspace:*"}},
            "js/bindings": {"name": "@example/bindings"},
            "test/browser": {"name": "browser", "optionalDependencies": {"@example/bindings": "workspace:*"}},
            "demo/nested": {"name": "demo", "peerDependencies": {"browser": "workspace:*"}},
        }
        for package in self.packages.values():
            if package["name"] != "@example/bindings":
                package["scripts"] = {action: "fixture" for action in ("check", "test", "build")}
        self.write()
        (self.root / "test/tsconfig.json").write_text('{"exclude":["**/dist"]}')
        (self.root / "sh").mkdir()
        (self.root / "sh/dispatch.sh").write_text("    [js]='^(js/|doc/|demo/|test/)'\n")

    def write(self):
        (self.root / "package.json").write_text(json.dumps({"workspaces": list(self.packages)}))
        for directory, manifest in self.packages.items():
            path = self.root / directory
            path.mkdir(parents=True, exist_ok=True)
            (path / "package.json").write_text(json.dumps(manifest))

    def plan(self, paths):
        return selector.selection(self.root, paths)

    def test_dispatcher_passes_only_js_impact_paths(self):
        sh = self.root / "sh"
        shutil.copy(selector.ROOT / "sh/dispatch.sh", sh / "dispatch.sh")
        changed = sh / "changed.sh"
        changed.write_text("#!/bin/sh\nprintf '%s\\n' js/auth/src/token.ts demo/pub/worker.ts quest/unrelated.md\n")
        changed.chmod(0o755)
        tools = self.root / "tools"
        tools.mkdir()
        for command in ("bun", "quest", "nixfmt", "taplo"):
            stub = tools / command
            stub.write_text("#!/bin/sh\nexit 0\n")
            stub.chmod(0o755)
        just = tools / "just"
        subprocess.run(["git", "init", "--quiet", str(self.root)], check=True)
        calls = self.root / "calls"
        summary = self.root / "summary"
        with patch.dict(os.environ, CI_STRICT="1", CHECK_MODULES="py", CHECK_JOBS="invalid", GITHUB_ACTIONS="true",
                        GITHUB_BASE_REF="missing", GITHUB_STEP_SUMMARY=str(summary)):
            env = dispatch_env(PATH=f"{tools}:{os.environ['PATH']}", ALLOW_HOST_TOOLCHAIN="1",
                               SELECT_CALLS=str(calls))
        for script in ("markdown.sh", "shell.sh", "large-files.sh"):
            path = sh / script
            path.write_text("#!/bin/sh\nexit 0\n")
            path.chmod(0o755)
        just.write_text('#!/bin/sh\nif [ "$1 $2" = "js test-changed" ]; then\n'
                        'printf "%s\\n" "$1 $2" > "$SELECT_CALLS"\ncat "$3" >> "$SELECT_CALLS"\nfi\n')
        just.chmod(0o755)
        subprocess.run(["bash", "sh/dispatch.sh", "ci", "base"], cwd=self.root, env=env, check=True)
        self.assertEqual(calls.read_text().splitlines(),
                         ["js test-changed", "js/auth/src/token.ts", "demo/pub/worker.ts"])
        self.assertFalse(summary.exists())

    def test_cli_loads_without_shadowing_standard_library_modules(self):
        result = subprocess.run([sys.executable, str(Path(__file__).with_name("workspace.py")),
                                 "plan", "--all"], capture_output=True, text=True, check=True)
        plan = json.loads(result.stdout)
        self.assertTrue(plan["all"])
        self.assertIn("@moq/auth", plan["packages"])

    def test_transitive_dependents_from_manifests(self):
        plan = self.plan(["js/core/src/deleted.ts"])
        self.assertEqual(set(plan["packages"]), {"@example/core", "@example/view", "docs"})
        self.assertFalse(plan["all"])

    def test_a_bench_selects_only_its_workspace(self):
        self.assertEqual(list(self.plan(["js/core/bench/frames.ts"])["packages"]), ["@example/core"])
        self.assertEqual(set(self.plan(["js/core/bench/frames.ts", "js/core/src/index.ts"])["packages"]),
                         {"@example/core", "@example/view", "docs"})

    def test_leaf_does_not_select_its_dependencies(self):
        self.assertEqual(list(self.plan(["doc/index.md"])["packages"]), ["docs"])

    def test_shared_and_deleted_workspaces_fall_back(self):
        for path in ("js/common/deps.ts", "js/justfile", "test/tsconfig.json", "drafts/spec.md",
                     "package.json", "bun.lock", "js/removed/src/index.ts"):
            with self.subTest(path=path):
                plan = self.plan([path])
                self.assertTrue(plan["all"])
                self.assertEqual(len(plan["packages"]), len(self.packages))

    def test_optional_dependency_selects_transitive_consumers(self):
        self.assertEqual(set(self.plan(["js/bindings/README.md"])["packages"]),
                         {"@example/bindings", "browser", "demo"})

    def test_multiple_seeds_and_empty_diff(self):
        self.assertEqual(set(self.plan(["doc/index.md", "test/browser/src/index.ts"])["packages"]),
                         {"docs", "browser", "demo"})
        self.assertEqual(self.plan([])["packages"], {})
        self.assertEqual(len(self.plan(None)["packages"]), len(self.packages))

    def test_manifest_changes_drive_new_dependency_edges(self):
        self.packages["doc"]["devDependencies"] = {"browser": "workspace:*"}
        self.write()
        self.assertEqual(set(self.plan(["test/browser/src/index.ts"])["packages"]),
                         {"browser", "demo", "docs"})

    def reader(self, source):
        self.packages["js/reader"] = {"name": "reader"}
        self.write()
        (self.root / "js/reader/src").mkdir()
        (self.root / "js/reader/src/x.test.ts").write_text(source)

    def test_cross_tree_readers_without_manifest_edges(self):
        (self.root / "doc/corpus").mkdir()
        (self.root / "js/view/src").mkdir()
        (self.root / "js/view/src/index.ts").touch()
        self.reader('const corpus = join(import.meta.dir, "../../../doc/corpus");\n'
                    'import { view } from "../../view/src/index";\n'
                    'expect(resolve("../../..", "../../../missing")).toBe(`../${name}`);\n')
        self.assertEqual(self.plan(None)["inputs"], ["doc/corpus", "js/view/src/index", "missing"])
        self.assertEqual(set(self.plan(["doc/corpus/case.json"])["packages"]), {"docs", "reader"})
        self.assertEqual(set(self.plan(["js/view/src/index.ts"])["packages"]),
                         {"@example/view", "docs", "reader"})
        self.assertEqual(list(self.plan(["doc/index.md"])["packages"]), ["docs"])
        self.assertEqual(selector.unmapped(self.root, ["doc/corpus", "js/view/src/index", "missing"]), [])

    def test_deleted_cross_tree_inputs_keep_their_readers(self):
        # The diff removed the directory and the file, but the reader still names them.
        self.reader('const corpus = join(import.meta.dir, "../../../doc/corpus");\n'
                    'import { view } from "../../view/src/index";\n')
        self.assertEqual(set(self.plan(["doc/corpus/case.json"])["packages"]), {"docs", "reader"})
        self.assertEqual(set(self.plan(["js/view/src/index.ts"])["packages"]),
                         {"@example/view", "docs", "reader"})

    def test_unowned_cross_tree_input_still_falls_back(self):
        (self.root / "rs").mkdir()
        (self.root / "rs/vectors.json").touch()
        self.reader('Bun.file(new URL("../../../rs/vectors.json", import.meta.url));\n')
        self.assertTrue(self.plan(["rs/vectors.json"])["all"])
        with self.assertRaisesRegex(RuntimeError, "rs/vectors.json"):
            self.calls(["js/reader/src/x.test.ts"])

    def test_repository_inputs_reach_their_readers(self):
        plan = selector.selection(selector.ROOT, ["doc/concept/audio-jitter/fixed.json"])
        self.assertIn("@moq/hang", plan["packages"])
        self.assertEqual(selector.unmapped(selector.ROOT, plan["inputs"]), [])

    def test_cycles_terminate(self):
        self.packages["js/core"]["dependencies"] = {"docs": "workspace:*"}
        self.write()
        self.assertEqual(set(self.plan(["js/view/src/index.ts"])["packages"]),
                         {"@example/core", "@example/view", "docs"})

    def calls(self, paths, action="check", github_actions=False):
        calls = []
        self.configurations = []
        def invoke(args, **kwargs):
            calls.append(list(args))
            if args[:2] == ("bunx", "tsc") and args[-1] != "test/tsconfig.json":
                self.configurations.append(json.loads(Path(args[-1]).read_text()))
            return type("Result", (), {"returncode": 1 if args[0] == "grep" else 0})()
        with patch.dict(selector.os.environ, GITHUB_ACTIONS=str(github_actions).lower()), \
                patch.object(selector, "ROOT", self.root), patch.object(selector.os, "chdir"), \
                patch.object(selector.subprocess, "run", side_effect=invoke):
            selector.run(action, self.plan(paths))
        return calls

    def test_executor_runs_only_selected_package_scripts(self):
        calls = self.calls(["doc/index.md"])
        scripts = [c for c in calls if c[:2] == ["bun", "run"]]
        self.assertEqual(scripts, [["bun", "run", "--sequential", "--filter=docs", "--elide-lines=0", action]
                                  for action in ("check", "build")])
        self.assertTrue(any(c[:2] == ["bunx", "tsc"] for c in calls))
        self.assertEqual(self.configurations, [{"extends": "./tsconfig.json", "exclude":
                          ["**/dist", str(self.root / "test/browser")]}])
        self.assertEqual(list((self.root / "test").glob("tmp*.json")), [])

    def test_biome_covers_unowned_js_files_in_fallback(self):
        def biome(paths):
            return next(c for c in self.calls(paths) if c[:3] == ["bun", "biome", "check"])
        self.assertEqual(biome(["js/tsconfig.json"]),
                         ["bun", "biome", "check", "--error-on-warnings", "js", "bench/browser", "test", "doc", "demo"])
        self.assertEqual(biome(["doc/index.md"]), ["bun", "biome", "check", "--error-on-warnings", "doc", "js/common"])
        self.assertEqual(biome(["demo/nested/src/index.ts"]),
                         ["bun", "biome", "check", "--error-on-warnings", "demo/nested", "js/common"])
        self.assertEqual(biome(["demo/pub/src/index.ts"]),
                         ["bun", "biome", "check", "--error-on-warnings", "js", "bench/browser", "test", "doc", "demo"])
        self.assertEqual(biome(["js/core/src/index.ts"]),
                         ["bun", "biome", "check", "--error-on-warnings", "js/core", "js/view", "doc", "js/common"])
        self.assertEqual(biome(["js/bindings/src/index.ts"]),
                         ["bun", "biome", "check", "--error-on-warnings", "js/bindings", "test/browser", "demo/nested",
                          "js/common"])

    def test_tests_do_not_build_unselected_packages(self):
        calls = self.calls(["doc/index.md"], "test")
        self.assertIn(["bun", "run", "--sequential", "--filter=docs", "--elide-lines=0", "test"], calls)
        hosted = self.calls(["doc/index.md"], "test", github_actions=True)
        self.assertIn(["bun", "run", "--filter=docs", "--elide-lines=0", "test"], hosted)
        self.assertFalse(any(c[0] == "just" for c in calls))

    def test_owned_package_without_scripts_keeps_harness_check(self):
        self.packages["doc"].pop("scripts")
        self.write()
        calls = self.calls(["doc/index.md"])
        self.assertFalse(any(c[:2] == ["bun", "run"] for c in calls))
        self.assertTrue(any(c[:2] == ["bunx", "tsc"] for c in calls))
        self.assertFalse(any(c[:2] == ["bun", "run"] for c in self.calls(["doc/index.md"], "test")))
        self.assertFalse(any(c[:2] == ["bun", "run"] for c in self.calls(["doc/index.md"], "build")))

    def test_empty_selection_runs_nothing(self):
        self.assertEqual(self.calls([]), [])

    def test_new_workspace_patterns_fail_loud(self):
        (self.root / "package.json").write_text('{"workspaces":["js/*"]}')
        with self.assertRaisesRegex(ValueError, "unsupported workspace pattern"):
            self.plan(None)

    def test_duplicate_names_fail_loud(self):
        self.packages["doc"]["name"] = "@example/core"
        self.write()
        with self.assertRaisesRegex(ValueError, "duplicate workspace name"):
            self.plan(None)


if __name__ == "__main__":
    unittest.main()
