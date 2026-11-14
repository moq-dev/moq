#!/usr/bin/env python3
"""Exercise paired builds with real Cargo diagnostics and target discovery."""

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


class Builds(unittest.TestCase):
    def test_base_builds_report_errors_and_distinguish_new_targets(self):
        (ROOT / ".tmp").mkdir(exist_ok=True)
        for base in ("broken", "existing", "new", "log-failure", "hangs"):
            with self.subTest(base=base), tempfile.TemporaryDirectory(dir=ROOT / ".tmp") as directory:
                repo = Path(directory) / "repo"
                repo.mkdir()
                shutil.copytree(ROOT / "bench", repo / "bench", ignore=shutil.ignore_patterns("__pycache__"))
                # The fixture measures nothing, so it need not wait behind another
                # session's benchmark for the host lock.
                (repo / "bench/lock.sh").write_text("bench_lock() { :; }\n")
                (repo / ".cargo").mkdir()
                (repo / ".cargo/config.toml").touch()
                (repo / ".gitignore").write_text(".tmp/\ntarget/\n")
                (repo / "Cargo.toml").write_text('[workspace]\nmembers=["rs/moq-relay"]\nresolver="3"\n')
                package = repo / "rs/moq-relay"
                (package / "src").mkdir(parents=True)
                (package / "benches").mkdir()
                (package / "src/lib.rs").touch()
                manifest = '[package]\nname="moq-relay"\nversion="0.0.0"\nedition="2024"\n'
                target = '[[bench]]\nname="catalog"\nharness=false\n'
                (package / "Cargo.toml").write_text(manifest + (target if base != "new" else ""))
                bench = package / "benches/catalog.rs"
                if base != "new":
                    bench.write_text(
                        'compile_error!("base catalog source failed");\n'
                        if base == "broken"
                        else 'fn main() { println!("base catalog ran"); loop { std::thread::park(); } }\n'
                        if base == "hangs"
                        else 'fn main() { println!("base catalog ran"); }\n'
                    )
                env = dict(
                    os.environ,
                    CARGO_TARGET_DIR=str(repo / "target"),
                    BENCH_BASE="HEAD",
                    BENCH_CRITERION_IDLE_SECONDS="2",
                )
                for key in ("BENCH_ARTIFACTS", "BENCH_RUNS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"):
                    env.pop(key, None)

                def run(*args):
                    result = subprocess.run(args, cwd=repo, env=env, text=True, capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                    return result

                run("cargo", "generate-lockfile")
                run("git", "init", "-q", "-b", "main")
                run("git", "add", ".")
                run(
                    "git",
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-qm",
                    "base",
                )
                (package / "Cargo.toml").write_text(manifest + target)
                bench.write_text('fn main() { println!("current catalog ran"); }\n')
                if base == "log-failure":
                    tools = repo / ".tmp/tools"
                    tools.mkdir(parents=True)
                    tee = tools / "tee"
                    tee.write_text(
                        "#!/usr/bin/env bash\n"
                        "if [[ $1 == */base-build.log ]]; then cat; exit 1; fi\n"
                        f'exec {shutil.which("tee")} "$@"\n'
                    )
                    tee.chmod(0o755)
                    env["PATH"] = str(tools) + os.pathsep + env["PATH"]
                result = run("bash", "bench/run.sh", "--criterion", "-p", "moq-relay", "--bench", "catalog")
                (evidence,) = (repo / ".tmp/bench").glob("run.*")
                log = (evidence / "base-build.log").read_text() if base != "log-failure" else ""
                report = json.loads((evidence / "paired.json").read_text())
                self.assertIn("current catalog ran", result.stdout)
                self.assertIn(str(evidence / "base-build.log"), result.stdout)
                if base == "broken":
                    self.assertIn("base catalog source failed", result.stderr)
                    self.assertIn("base catalog source failed", log)
                    self.assertEqual(
                        report["unavailable"], [{"target": "moq-relay/catalog", "reason": "base build failed"}]
                    )
                    self.assertEqual((evidence / "criterion-base.targets").read_text(), "")
                elif base == "hangs":
                    # The silent base is killed and marked unavailable; the current run still reports.
                    self.assertIn("moq-relay/catalog hung: no output for 2s after: base catalog ran", result.stderr)
                    self.assertEqual(
                        report["unavailable"], [{"target": "moq-relay/catalog", "reason": "base run failed"}]
                    )
                else:
                    self.assertEqual(report["unavailable"], [])
                    self.assertEqual("base catalog ran" in result.stdout, base in ("existing", "log-failure"))
                if base == "log-failure":
                    self.assertIn("Failed to write base build log:", result.stderr)
                    self.assertNotEqual((evidence / "criterion-base.targets").read_text(), "")
                self.assertEqual(len(run("git", "worktree", "list", "--porcelain").stdout.split("worktree ")) - 1, 1)


if __name__ == "__main__":
    unittest.main()
