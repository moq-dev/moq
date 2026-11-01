"""The inventory names every opt-in test target no workflow reaches, and only those."""

import importlib.util
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest

spec = importlib.util.spec_from_file_location("test_inventory", Path(__file__).with_name("test-inventory.py"))
module = importlib.util.module_from_spec(spec)
# Dataclasses resolve string annotations through the module's entry here.
sys.modules[spec.name] = module
spec.loader.exec_module(module)

# One crate with ignored tests at every module depth, two benches, and a fuzz crate.
CRATE = {
    "rs/media/Cargo.toml": """
        [package]
        name = "media"

        [[bench]]
        name = "decode"
        harness = false
    """,
    "rs/media/src/lib.rs": """
        mod gpu;

        #[cfg(test)]
        #[path = "probe_test.rs"]
        #[expect(
        	clippy::unwrap_used,
        	reason = "tests"
        )]
        mod probe;
    """,
    "rs/media/src/gpu.rs": """
        #[cfg(test)]
        mod tests {
        	#[test]
        	#[ignore = "needs a GPU"]
        	fn renders() {}

        	#[test]
        	#[ignore = "needs a GPU"]
        	#[expect(
        		clippy::unwrap_used,
        		reason = "a multi-line attribute between the ignore and the fn"
        	)]
        	fn exports() {}
        }
    """,
    "rs/media/src/probe_test.rs": """
        #[test]
        #[ignore = "slow"]
        fn long_sweep() {}
    """,
    "rs/media/tests/device.rs": """
        #[test]
        #[ignore = "needs a speaker"]
        fn plays() {}
    """,
    "rs/media/benches/decode.rs": "",
    "rs/media/benches/encode.rs": "",
    "rs/media/fuzz/Cargo.toml": """
        [package]
        name = "media-fuzz"

        [package.metadata]
        cargo-fuzz = true

        [[bin]]
        name = "frame"
        path = "fuzz_targets/frame.rs"

        [[bin]]
        name = "header"
        path = "fuzz_targets/header.rs"
    """,
    "rs/media/fuzz/fuzz_targets/frame.rs": "",
    "rs/media/fuzz/fuzz_targets/header.rs": "",
    "quest/m2/gpu.md": "# [S] GPU\n",
    "sh/fuzz.sh": 'cargo +nightly fuzz run "$1"\n',
    "sh/a.test.py": "",
    "sh/b.test.py": "",
}

JUSTFILE = """
    mod rs

    gate:
        python3 sh/a.test.py
        just rs test -p media --lib --run-ignored only -E 'test(/^gpu::tests::(renders|exports)$/)'
        cargo bench -p media --bench decode
        # Neither a comment nor a build runs anything: python3 sh/b.test.py
        cargo bench --no-run -p media --bench encode
        cargo +nightly build -p media --bench encode # bash sh/b.test.py
"""

RS = """
    set working-directory := '..'

    [positional-arguments]
    test *args:
        cargo nextest run --tests "$@"

    fuzz target:
        sh/fuzz.sh {{ target }}

    # A selection of its own, so an unreached copy is an orphan too.
    sweep:
        just rs test -p media --lib --run-ignored only -E 'test(long_sweep)'
"""

WORKFLOW = """
    on: push
    jobs:
      gate:
        runs-on: ubuntu-latest
        steps:
          - run: just gate
      fuzz:
        runs-on: ubuntu-latest
        strategy:
          matrix:
            target: [frame]
        steps:
          - run: just rs fuzz ${{ matrix.target }}
"""


class InventoryTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name).resolve()
        files = {**CRATE, "justfile": JUSTFILE, "rs/justfile": RS, ".github/workflows/ci.yml": WORKFLOW}
        for name, text in files.items():
            self.write(name, text)
        subprocess.run(["git", "init", "-q"], cwd=self.root, check=True)

    def write(self, name: str, text: str):
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(textwrap.dedent(text).lstrip("\n"))

    def orphans(self) -> dict[str, str]:
        orphans, errors = module.inventory(self.root)
        self.assertEqual(errors, [])
        return dict(orphans)

    def run_main(self) -> subprocess.CompletedProcess:
        return subprocess.run(["python3", str(Path(module.__file__)), "--root", str(self.root)],
                              capture_output=True, text=True)

    def test_reports_each_orphan_with_where_it_was_found(self):
        self.assertEqual(self.orphans(), {
            "ignore media::probe::long_sweep": "rs/media/src/probe_test.rs:2",
            "ignore media::plays": "rs/media/tests/device.rs:2",
            "bench media::encode": "rs/media/Cargo.toml",
            "fuzz media-fuzz::header": "rs/media/fuzz/Cargo.toml",
            "recipe rs::sweep": "rs/justfile:11",
            "script sh/b.test.py": "sh/b.test.py",
        })

    def test_a_reached_recipe_covers_its_selection(self):
        self.write(".github/workflows/nightly.yml", """
            on: schedule
            jobs:
              sweep:
                runs-on: ubuntu-latest
                defaults:
                  run:
                    working-directory: rs
                steps:
                  - run: just sweep
        """)
        orphans = self.orphans()
        self.assertNotIn("recipe rs::sweep", orphans)
        self.assertNotIn("ignore media::probe::long_sweep", orphans)

    def test_workers_are_tested_by_their_lockfile(self):
        self.write("infra/worker/bun.lock", "")
        self.write("infra/worker/src/policy.test.ts", "")
        self.write("infra/worker/justfile", "test:\n    bun test\n")
        orphans = self.orphans()
        self.assertIn("recipe infra/worker::test", orphans)
        self.assertIn("script infra/worker/src/policy.test.ts", orphans)

        self.write("sh/js/workers.sh", "")
        self.write("justfile", JUSTFILE + "        bash sh/js/workers.sh\n")
        orphans = self.orphans()
        self.assertNotIn("recipe infra/worker::test", orphans)
        self.assertNotIn("script infra/worker/src/policy.test.ts", orphans)

    def test_a_commented_out_caller_covers_nothing(self):
        caller = "just rs test -p media --lib --run-ignored only -E 'test(/^gpu::tests::(renders|exports)$/)'"
        self.write("justfile", JUSTFILE.replace(caller, f"echo disabled # {caller}"))
        orphans = self.orphans()
        self.assertIn("ignore media::gpu::tests::renders", orphans)
        self.assertIn("ignore media::gpu::tests::exports", orphans)

        # A `#` inside a word or quotes is not a comment.
        self.write("justfile", JUSTFILE.replace(caller, f"echo 'a # b' a#b && {caller}"))
        self.assertNotIn("ignore media::gpu::tests::renders", self.orphans())

    def test_a_dispatcher_runs_only_the_branch_its_caller_selects(self):
        self.write("sh/group.sh", """
            #!/usr/bin/env bash
            case "${1:-}" in
                # The sweep needs its own branch.
                sweep)
                    just rs sweep
                    ;;
                quick) echo quick ;;
                *) exit 2 ;;
            esac
        """)
        # A test that runs the script under stubs selects no branch.
        self.write("sh/group.test.py", 'SCRIPT = "sh/group.sh"\n')
        self.write("justfile", JUSTFILE.replace("gate:", "gate: groups") + """
    group name:
        sh/group.sh {{ quote(name) }}

    groups:
        python3 sh/group.test.py
        just group quick
""")
        orphans = self.orphans()
        self.assertIn("recipe rs::sweep", orphans)
        self.assertIn("ignore media::probe::long_sweep", orphans)

        self.write("justfile", (self.root / "justfile").read_text().replace("just group quick", "just group sweep"))
        orphans = self.orphans()
        self.assertNotIn("recipe rs::sweep", orphans)
        self.assertNotIn("ignore media::probe::long_sweep", orphans)

    def test_a_bun_workspace_covers_its_tests_once_its_runner_is_reached(self):
        self.write("package.json", '{"workspaces": ["js/net"]}')
        for package in ("js/net", "js/loose"):
            self.write(f"{package}/package.json", '{"scripts": {"test": "bun test src"}}')
            self.write(f"{package}/src/codec.test.ts", "")
        orphans = self.orphans()
        self.assertIn("script js/net/src/codec.test.ts", orphans)
        self.assertIn("script js/loose/src/codec.test.ts", orphans)

        self.write("sh/js/workspace.py", "")
        # A reached script that only names the runner, or a script that runs it, as the checker
        # does, runs neither.
        self.write("sh/run.sh", "python3 sh/js/workspace.py test --all\n")
        self.write("sh/check.py", 'RUNNERS = ["sh/js/workspace.py", "sh/run.sh"]\n')
        self.write("justfile", JUSTFILE + "        python3 sh/check.py\n")
        self.assertIn("script js/net/src/codec.test.ts", self.orphans())

        self.write("justfile", JUSTFILE + "        python3 sh/js/workspace.py test --all\n")
        orphans = self.orphans()
        self.assertNotIn("script js/net/src/codec.test.ts", orphans)
        self.assertIn("script js/loose/src/codec.test.ts", orphans)

        # The runner runs only the `check`, `test`, and `build` scripts.
        self.write("js/net/package.json", '{"scripts": {"manual-test": "bun test src"}}')
        self.assertIn("script js/net/src/codec.test.ts", self.orphans())

    def test_a_selection_outside_the_ignored_set_covers_nothing(self):
        # Same filter, but without --run-ignored nextest skips the ignored tests.
        self.write("justfile", JUSTFILE.replace("--run-ignored only ", ""))
        orphans = self.orphans()
        self.assertIn("ignore media::gpu::tests::renders", orphans)
        self.assertIn("ignore media::gpu::tests::exports", orphans)

    def test_allowlist(self):
        orphans = self.orphans()
        allow = [f"{key} /quest/m2/gpu.md" for key in orphans]
        self.write("sh/gh/test-inventory.allow", "\n".join(allow))
        self.assertEqual(self.run_main().returncode, 0)

        # An entry with a caller is stale.
        self.write("sh/gh/test-inventory.allow", "\n".join([*allow, "bench media::decode /quest/m2/gpu.md"]))
        result = self.run_main()
        self.assertEqual(result.returncode, 1)
        self.assertIn("bench media::decode is not an orphan", result.stderr)

        # An entry must name a quest in the tree.
        self.write("sh/gh/test-inventory.allow", "\n".join([*allow[1:], allow[0].replace("gpu.md", "gone.md")]))
        result = self.run_main()
        self.assertEqual(result.returncode, 1)
        self.assertIn("/quest/m2/gone.md is not a quest", result.stderr)

        self.write("sh/gh/test-inventory.allow", "\n".join(allow[1:]))
        result = self.run_main()
        self.assertEqual(result.returncode, 1)
        self.assertIn(f"{allow[0].split(' /')[0]} has no CI caller", result.stderr)


class FiltersetTest(unittest.TestCase):
    def test(self):
        test = module.Ignored(module.Target("media", "lib", "media"), "render::export::nv12::tests::round_trip", "")
        for text, expected in (
            ("test(/^render::export::(nv12::)?tests::/)", True),
            ("test(=render::export::nv12::tests::round_trip)", True),
            ("test(round_trip) & not test(/benchmark_/)", True),
            ("test(benchmark_) | package(other)", False),
            ("package(media) - test(nv12)", False),
            ("kind(lib) and binary(media)", True),
            ("(test(a) | test(trip)) & binary_id(media)", True),
        ):
            self.assertEqual(module.Filterset(text).evaluate(test), expected, text)

    def test_unbalanced(self):
        test = module.Ignored(module.Target("media", "lib", "media"), "x", "")
        with self.assertRaises(ValueError):
            module.Filterset("(test(x)").evaluate(test)


if __name__ == "__main__":
    unittest.main()
