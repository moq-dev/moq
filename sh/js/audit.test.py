"""Every Bun lockfile is audited, including when an earlier audit fails."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
# The script finds projects through `git ls-files`, so these only shape the fixture.
PROJECTS = [".", "demo/pub", "infra/apt", "infra/nix"]


class Audit(unittest.TestCase):
    def run_audit(self, failing):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for project in PROJECTS:
                (root / project).mkdir(parents=True, exist_ok=True)
            tools = root / "bin"
            tools.mkdir()
            for name, script in {
                "git": """#!/bin/sh
case "$1" in
rev-parse) printf '%s\\n' "$AUDIT_ROOT" ;;
ls-files) printf '%s\\n' $AUDIT_LOCKS ;;
*) exit 2 ;;
esac
""",
                "bun": """#!/bin/sh
test "$#" = 1 && test "$1" = audit || exit 2
printf '%s\\n' "$PWD" >> "$AUDIT_LOG"
test "$PWD" != "$AUDIT_FAIL"
""",
            }.items():
                path = tools / name
                path.write_text(script)
                path.chmod(0o755)
            result = subprocess.run(
                ["bash", str(ROOT / "sh/js/audit.sh")],
                env={
                    **os.environ,
                    "PATH": f"{tools}:{os.environ['PATH']}",
                    "AUDIT_ROOT": str(root),
                    "AUDIT_LOCKS": " ".join(f"{project}/bun.lock" for project in PROJECTS),
                    "AUDIT_LOG": str(root / "audits"),
                    "AUDIT_FAIL": str(root / failing) if failing else "",
                },
                capture_output=True,
                text=True,
            )
            self.assertEqual(
                (root / "audits").read_text().splitlines(),
                [str((root / project).resolve()) for project in PROJECTS],
                result.stderr,
            )
            self.assertEqual(result.returncode, 1 if failing else 0, result.stderr)

    def test_success(self):
        self.run_audit(None)

    def test_failure_still_audits_every_lockfile(self):
        self.run_audit(".")


if __name__ == "__main__":
    unittest.main()
