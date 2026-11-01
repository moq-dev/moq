"""Coverage reports use real package boundaries and cannot outlive a failed run."""

import json
import importlib.util
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]


class CoverageTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        for script in ("sh/js/coverage.py", "sh/rs/coverage.sh", "sh/rs/nextest.sh", "sh/local.sh"):
            target = self.root / script
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / script, target)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        stub = '''#!/usr/bin/env python3
import json, os
from pathlib import Path
import sys
with open(os.environ["COVERAGE_CALLS"], "a") as log:
    log.write(json.dumps([Path(sys.argv[0]).name, str(Path.cwd()), *sys.argv[1:]]) + "\\n")
if os.environ.get("COVERAGE_FAIL") and ("nextest" in sys.argv or "test" in sys.argv):
    sys.exit(17)
if "report" in sys.argv and any(arg in sys.argv for arg in ("--workspace", "--exclude")):
    sys.exit(18)
for arg in sys.argv:
    if arg.startswith("--coverage-dir="):
        (Path(arg.split("=", 1)[1]) / "lcov.info").write_text("fresh")
'''
        for name in ("bun", "cargo"):
            target = self.bin / name
            target.write_text(stub)
            target.chmod(0o755)
        self.calls = self.root / "calls"
        self.env = dict(os.environ, PATH=f"{self.bin}:{os.environ['PATH']}",
                        COVERAGE_CALLS=str(self.calls), BASH_ENV="/dev/null", GITHUB_ACTIONS="", LOCAL_SUPERVISED="1")

    def javascript(self, name="", fail=False):
        directories = ["js/first", "js/second", "js/empty"]
        (self.root / "package.json").write_text(json.dumps({"workspaces": directories}))
        for directory in ["js/common", *directories]:
            source = self.root / directory / "src"
            source.mkdir(parents=True, exist_ok=True)
            if directory != "js/empty":
                (source / "case.test.ts").touch()
        return subprocess.run(["python3", "sh/js/coverage.py", name], cwd=self.root,
                              env=dict(self.env, COVERAGE_FAIL="1" if fail else ""),
                              capture_output=True, text=True)

    def recorded(self):
        return [json.loads(line) for line in self.calls.read_text().splitlines()]

    def test_javascript_runs_packages_separately_without_requiring_a_test_script(self):
        result = self.javascript()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.recorded()
        self.assertEqual(calls[0][2:], ["install", "--frozen-lockfile"])
        tests = calls[1:]
        self.assertEqual([Path(call[1]).name for call in tests], ["common", "first", "second"])
        for call in tests:
            self.assertIn("--coverage", call)
            self.assertEqual((self.root / "target/coverage/js" / Path(call[1]).name / "lcov.info").read_text(), "fresh")

    def test_failed_javascript_run_removes_previous_report(self):
        out = self.root / "target/coverage/js/first"
        out.mkdir(parents=True)
        (out / "lcov.info").write_text("stale")
        result = self.javascript("first", fail=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((out / "lcov.info").exists())
        self.assertEqual(len(self.recorded()), 2)

    def test_unknown_javascript_package_is_refused_before_install(self):
        result = self.javascript("unknown")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.calls.exists())

    def test_javascript_summary_unions_dependency_execution_and_names_unloaded_sources(self):
        self.javascript()
        source = self.root / "js/first/src/source.ts"
        source.touch()
        (source.parent / "unloaded.ts").touch()
        for package, path, hits in (("first", "src/source.ts", 0),
                                    ("second", "../first/src/source.ts", 1)):
            (self.root / f"target/coverage/js/{package}/lcov.info").write_text(
                f"SF:{path}\nDA:1,{hits}\nDA:2,0\nend_of_record\n")
        spec = importlib.util.spec_from_file_location("coverage_report", ROOT / "sh/js/coverage.py")
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        summary = module.report(self.root, [self.root / "js/first", self.root / "js/second"])
        self.assertEqual(summary["first"], {"lines": 2, "covered": 1, "unloaded": ["src/unloaded.ts"]})

    def rust(self, name="workspace", fail=False):
        return subprocess.run(["bash", "sh/rs/coverage.sh", name], cwd=self.root,
                              env=dict(self.env, COVERAGE_FAIL="1" if fail else ""),
                              capture_output=True, text=True)

    def test_rust_measures_native_workspace_without_fuzz_targets(self):
        result = self.rust()
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = self.recorded()
        self.assertEqual(calls[0][2:], ["llvm-cov", "clean", "--workspace"])
        build = calls[1][2:]
        self.assertIn("--workspace", build)
        for excluded in ("moq-net-fuzz", "moq-quic-fuzz"):
            self.assertIn(excluded, build)
        self.assertIn("--tests", build)
        self.assertIn("--test-threads", build)
        self.assertIn("--json", calls[3])

    def test_failed_rust_run_removes_previous_report_and_does_not_report(self):
        out = self.root / "target/coverage/moq-json"
        out.mkdir(parents=True)
        (out / "lcov.info").write_text("stale")
        result = self.rust("moq-json", fail=True)
        self.assertEqual(result.returncode, 17)
        self.assertFalse((out / "lcov.info").exists())
        self.assertEqual(len(self.recorded()), 2)
        self.assertIn("moq-json", self.recorded()[1])


if __name__ == "__main__":
    unittest.main()
