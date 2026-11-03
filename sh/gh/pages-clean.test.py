"""A Pages rerun replaces only its own artifact and refuses API failures."""

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class PagesCleanTest(unittest.TestCase):
    def run_clean(self, artifacts="", fail=""):
        with tempfile.TemporaryDirectory() as directory:
            scratch = Path(directory)
            calls = scratch / "calls"
            stub = scratch / "gh"
            stub.write_text(
                "#!/usr/bin/env bash\n"
                'printf "%s\\n" "$*" >> "$PAGES_CALLS"\n'
                'if [[ "$*" == "$PAGES_FAIL" ]]; then exit 1; fi\n'
                'if [[ "$2" == --paginate ]]; then printf "%s\\n" "$PAGES_ARTIFACTS"; fi\n'
            )
            stub.chmod(0o755)
            env = dict(
                os.environ,
                PATH=f"{scratch}:{os.environ['PATH']}",
                GITHUB_REPOSITORY="owner/repo",
                GITHUB_RUN_ID="123",
                PAGES_CALLS=str(calls),
                PAGES_ARTIFACTS=artifacts,
                PAGES_FAIL=fail,
                BASH_ENV="/dev/null",
            )
            result = subprocess.run(["bash", "sh/gh/pages-clean.sh"], cwd=ROOT, env=env, capture_output=True, text=True)
            return result, calls.read_text().splitlines()

    def test_rerun_deletes_all_prior_attempts_in_this_run(self):
        result, calls = self.run_clean("41\n42")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            calls,
            [
                "api --paginate repos/owner/repo/actions/runs/123/artifacts?name=github-pages --jq .artifacts[].id",
                "api -X DELETE repos/owner/repo/actions/artifacts/41",
                "api -X DELETE repos/owner/repo/actions/artifacts/42",
            ],
        )

    def test_first_attempt_has_nothing_to_delete(self):
        result, calls = self.run_clean()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 1)

    def test_failed_listing_stops_before_any_deletion(self):
        listing = "api --paginate repos/owner/repo/actions/runs/123/artifacts?name=github-pages --jq .artifacts[].id"
        result, calls = self.run_clean("41\n42", fail=listing)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [listing])

    def test_failed_deletion_stops_the_upload(self):
        result, calls = self.run_clean("41\n42", fail="api -X DELETE repos/owner/repo/actions/artifacts/41")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(calls), 2)


if __name__ == "__main__":
    unittest.main()
