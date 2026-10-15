#!/usr/bin/env python3
"""Exercise artifact selection, independent release gates, and build-free bindings."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "sh/gh/ffi.py"
ATTEMPT_1, ATTEMPT_2 = "2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z"


class Ffi(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.env = dict(os.environ, BASH_ENV="/dev/null", PATH=f"{self.bin}:{os.environ['PATH']}",
                        GITHUB_OUTPUT=str(self.root / "output"), LIB_PREFIX="default",
                        BUILD_TARGETS='["x86_64-unknown-linux-gnu", "aarch64-apple-darwin"]',
                        GITHUB_EVENT_NAME="workflow_run", UPSTREAM_NAME="Release FFI", UPSTREAM_RUN_ID="42",
                        UPSTREAM_RUN_STARTED_AT=ATTEMPT_2,
                        GITHUB_REPOSITORY="moq-dev/moq", TEST_ROOT=str(self.root))

    def tool(self, name, body):
        path = self.bin / name
        path.write_text("#!/usr/bin/env python3\n" + body)
        path.chmod(0o755)

    def run_helper(self, *args, success=True):
        result = subprocess.run([sys.executable, str(HELPER), *args], cwd=self.root,
                                env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode == 0, success, result.stderr)
        return (self.root / "output").read_text() if (self.root / "output").exists() else ""

    def test_select_and_stage_only_requested_targets(self):
        self.assertEqual(self.run_helper("pattern"),
                         "pattern=default-lib-{x86_64-unknown-linux-gnu,aarch64-apple-darwin}\n")
        for target in ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "aarch64-apple-ios"]:
            directory = self.root / f"libs-raw/default-lib-{target}"
            directory.mkdir(parents=True)
            (directory / "library").write_bytes(target.encode())
        self.run_helper("stage")
        self.assertEqual(sorted(p.name for p in (self.root / "libs").iterdir()),
                         ["aarch64-apple-darwin", "x86_64-unknown-linux-gnu"])
        self.assertEqual((self.root / "libs/aarch64-apple-darwin/library").read_bytes(), b"aarch64-apple-darwin")
        self.assertTrue((self.root / "libs-raw/default-lib-aarch64-apple-ios/library").exists())

    def test_missing_target_fails_before_staging(self):
        directory = self.root / "libs-raw/default-lib-x86_64-unknown-linux-gnu"
        directory.mkdir(parents=True)
        (directory / "library").touch()
        self.run_helper("stage", success=False)
        self.assertFalse((self.root / "libs").exists())

    def test_invalid_selection_is_refused(self):
        for targets in ["[]", '{}', '["../outside"]', '["x86-linux","x86-linux"]', '[false]']:
            with self.subTest(targets=targets):
                self.env["BUILD_TARGETS"] = targets
                self.run_helper("pattern", success=False)

    def test_single_target_pattern_has_no_brace_literal(self):
        self.env["BUILD_TARGETS"] = '["x86_64-unknown-linux-gnu"]'
        self.assertEqual(self.run_helper("pattern"), "pattern=default-lib-x86_64-unknown-linux-gnu\n")

    def test_single_artifact_download_is_staged_from_flat_layout(self):
        self.env["BUILD_TARGETS"] = '["x86_64-unknown-linux-gnu"]'
        raw = self.root / "libs-raw"
        raw.mkdir()
        (raw / "libmoq_ffi.a").write_bytes(b"native")
        self.run_helper("stage")
        self.assertEqual((self.root / "libs/x86_64-unknown-linux-gnu/libmoq_ffi.a").read_bytes(), b"native")

    def jobs(self, language, conclusion="success"):
        group, publisher = {
            "go": ("Release Go FFI", "Publish to Go module mirror"),
            "kotlin": ("Release Kotlin FFI", "Package and publish"),
            "swift": ("Release Swift FFI", "Publish to Swift Package mirror"),
        }[language]
        return [{"name": f"{group} / {publisher}", "status": "completed", "conclusion": conclusion}]

    def gh(self, jobs):
        jobs = [dict(dict(run_attempt=2, started_at=ATTEMPT_2), **job) for job in jobs]
        (self.root / "jobs").write_text(json.dumps([{"jobs": jobs[:1]}, {"jobs": jobs[1:]}]))
        self.tool("gh", '''import os, pathlib, sys
assert sys.argv[1:] == ["api", "repos/moq-dev/moq/actions/runs/42/jobs?filter=all", "--paginate", "--slurp"]
root = pathlib.Path(os.environ["TEST_ROOT"])
(root / "queried").touch()
if os.environ.get("GH_FAIL"): sys.exit(1)
print((root / "jobs").read_text())
''')

    def test_unrelated_failure_does_not_suppress_own_release(self):
        for language in ["go", "kotlin", "swift"]:
            with self.subTest(language=language):
                (self.root / "output").unlink(missing_ok=True)
                jobs = self.jobs(language) + [{"name": "Another language / Build", "status": "completed", "conclusion": "failure"},
                                              {"name": self.jobs(language)[0]["name"].split(" / ")[0] + " / Publish dry-run",
                                               "status": "completed", "conclusion": "skipped"}]
                self.gh(jobs)
                self.assertEqual(self.run_helper("completed", language), "ready=true\n")

    def test_own_failure_or_incomplete_job_prevents_release(self):
        for jobs in [self.jobs("go", "failure"), self.jobs("go") + [
            {"name": "Release Go FFI / Package", "status": "in_progress", "conclusion": None}],
            self.jobs("go") + [{"name": "Release Go FFI / Package", "status": "completed", "conclusion": "failure"}]]:
            with self.subTest(jobs=jobs):
                (self.root / "output").unlink(missing_ok=True)
                self.gh(jobs)
                self.assertEqual(self.run_helper("completed", "go"), "ready=false\n")

    def test_partial_reruns_use_the_latest_execution_of_each_own_job(self):
        for old, new, ready in [("failure", "success", "true"), ("success", "failure", "false")]:
            (self.root / "output").unlink(missing_ok=True)
            self.gh([dict(self.jobs("go", old)[0], run_attempt=1, started_at=ATTEMPT_1),
                     dict(self.jobs("go", new)[0], run_attempt=2)])
            self.assertEqual(self.run_helper("completed", "go"), f"ready={ready}\n")

    def test_publisher_copied_from_an_earlier_attempt_does_not_release_again(self):
        # Attempt 1 published Go and failed Kotlin, so Go's wrapper already ran.
        # A newer tag may have published since; rerunning only Kotlin must not
        # re-cut Go's wrapper from this older tag. GitHub copies the successful
        # job into attempt 2 with its original start time.
        go, kotlin = self.jobs("go")[0], self.jobs("kotlin")[0]
        self.gh([dict(go, run_attempt=1, started_at=ATTEMPT_1), dict(go, run_attempt=2, started_at=ATTEMPT_1),
                 dict(kotlin, run_attempt=1, started_at=ATTEMPT_1, conclusion="failure"), kotlin])
        self.assertEqual(self.run_helper("completed", "go"), "ready=false\n")
        (self.root / "output").unlink()
        self.assertEqual(self.run_helper("completed", "kotlin"), "ready=true\n")

    def test_missing_publisher_and_api_error_fail_loudly(self):
        self.gh(self.jobs("swift"))
        self.run_helper("completed", "go", success=False)
        self.env["GH_FAIL"] = "1"
        self.run_helper("completed", "swift", success=False)

    def test_direct_and_legacy_triggers_need_no_api(self):
        for event, name in [("push", ""), ("workflow_run", "Release Go FFI")]:
            (self.root / "output").unlink(missing_ok=True)
            self.env.update(GITHUB_EVENT_NAME=event, UPSTREAM_NAME=name)
            self.assertEqual(self.run_helper("completed", "go"), "ready=true\n")
        self.assertFalse((self.root / "queried").exists())

    def test_bindings_from_existing_library_never_build_moq_ffi(self):
        library = self.root / "libmoq_ffi.so"
        library.write_bytes(b"metadata")
        self.tool("cargo", '''import json, os, pathlib, sys
root = pathlib.Path(os.environ["TEST_ROOT"])
with (root / "calls").open("a") as log: log.write(json.dumps(sys.argv[1:]) + "\\n")
if sys.argv[1] == "metadata": print(json.dumps({"target_directory": str(root / "target")}))
elif sys.argv[1] == "run":
    assert sys.argv[sys.argv.index("--library") + 1] == str(root / "libmoq_ffi.so")
    pathlib.Path(sys.argv[sys.argv.index("--out-dir") + 1]).mkdir(parents=True)
else: raise AssertionError("unexpected native build")
''')
        self.tool("rustc", 'print("host: x86_64-unknown-linux-gnu")\n')
        for tool in ["uniffi-bindgen-go", "uniffi_bindgen_dart", "uniffi-bindgen-cpp"]:
            self.tool(tool, '''import os, pathlib, sys
assert sys.argv[sys.argv.index("--library") + 1] == str(pathlib.Path(os.environ["TEST_ROOT"]) / "libmoq_ffi.so")
pathlib.Path(sys.argv[sys.argv.index("--out-dir") + 1]).mkdir(parents=True)
''')
        args = ["bash", str(ROOT / "rs/moq-ffi/build.sh"), "--bindings-only", "--library", str(library),
                "--version", "1.2.3", "--output", str(self.root / "dist")]
        result = subprocess.run(args, env=self.env, cwd=self.root, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = [json.loads(line) for line in (self.root / "calls").read_text().splitlines()]
        self.assertEqual([call[0] for call in calls], ["metadata", "run", "run", "run"])
        self.assertEqual(sorted(p.name for p in (self.root / "dist/bindings").iterdir()),
                         ["cpp", "dart", "go", "kotlin", "python", "swift"])
        for suffix in [["--library", str(library)], ["--bindings-only", "--library", str(self.root / "missing")]]:
            result = subprocess.run(["bash", str(ROOT / "rs/moq-ffi/build.sh"), *suffix], env=self.env,
                                    cwd=self.root, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("requires --bindings-only and an existing library", result.stderr)


if __name__ == "__main__":
    unittest.main()
