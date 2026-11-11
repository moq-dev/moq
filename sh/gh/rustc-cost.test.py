#!/usr/bin/env python3
"""The gate cost compiler wrapper records each compilation and stays transparent to Cargo."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

WRAPPER = Path(__file__).with_name("rustc-cost.sh")
HYBRID = (
    "# started on a test\n\n"
    "900,,cpu_atom/instructions/u,1,100.00,,\n"
    "<not counted>,,cpu_core/instructions/u,0,0.00,,\n"
    "100,,cpu_core/instructions/u,1,100.00,,\n"
    "12.75,msec,task-clock,1,100.00,,\n"
)


class WrapperTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        perf = self.root / "perf"
        # Writes the canned counts to perf's -o file, then runs the command after `--`.
        perf.write_text(
            "#!/usr/bin/env bash\n"
            'echo called >>"$PERF_CALLS"\n'
            "while [[ $1 != -- ]]; do [[ $1 != -o ]] || out=$2; shift; done\n"
            'shift\nprintf "%s" "$PERF_STATS" >"$out"\n"$@"\n'
        )
        perf.chmod(0o755)
        self.cost = self.root / "cost.tsv"
        self.env = dict(
            os.environ,
            PATH=f"{self.root}:{os.environ['PATH']}",
            RUSTC_COST=str(self.cost),
            PERF_CALLS=str(self.root / "calls"),
            PERF_STATS=HYBRID,
            CARGO_PKG_NAME="moq-net",
            CARGO_MANIFEST_DIR="/src/rs/moq-net",
        )

    def compile(self, *args, stats=HYBRID):
        return subprocess.run(
            ["bash", str(WRAPPER), "sh", "-c", 'echo "$0 $*"; exit "${STATUS:-0}"', *args],
            env=dict(self.env, PERF_STATS=stats),
            text=True,
            capture_output=True,
        )

    def test_probes_run_untouched(self):
        for args in (["-vV"], ["-", "--crate-name", "___", "--print=file-names"]):
            with self.subTest(args=args):
                result = self.compile(*args)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout.split(), args)
        self.assertFalse(self.cost.exists())
        self.assertFalse((self.root / "calls").exists())

    def test_each_compilation_records_its_kind_cpu_and_summed_instructions(self):
        for args in (
            ["--crate-name", "moq_net", "--crate-type", "lib"],
            ["--crate-name", "integration", "--test"],
            ["--crate-name", "moq", "--crate-type", "bin"],
            ["--crate-name", "build_script_build", "--crate-type", "bin"],
        ):
            result = self.compile(*args)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            [line.split("\t") for line in self.cost.read_text().splitlines()],
            [["moq-net", "/src/rs/moq-net", kind, "12", "1000"] for kind in ("lib", "test", "bin", "build")],
        )

    def test_a_failed_compilation_keeps_its_status(self):
        self.env["STATUS"] = "3"
        result = self.compile("--crate-name", "moq_net")
        self.assertEqual(result.returncode, 3, result.stderr)
        self.assertEqual(len(self.cost.read_text().splitlines()), 1)

    def test_uncounted_instructions_fail(self):
        result = self.compile(
            "--crate-name", "moq_net", stats="<not supported>,,instructions:u,0,0.00,,\n1,msec,task-clock,1,100.00,,\n"
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("perf counted no CPU time or instructions", result.stderr)
        self.assertFalse(self.cost.exists())


if __name__ == "__main__":
    unittest.main()
