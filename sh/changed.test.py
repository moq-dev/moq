#!/usr/bin/env python3
"""Check base selection against a real local history and a stub GitHub CLI."""

import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


CHANGED = Path(__file__).with_name("changed.sh").resolve()


class ChangedTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.bin = self.root / "bin"
        self.bin.mkdir()
        # Keep the test independent of installed gh and authentication. Only
        # commands used by changed.sh are visible, including a fake timeout.
        for command in ("bash", "git", "sort"):
            (self.bin / command).symlink_to(shutil.which(command))
        self.timeout = self.bin / "timeout"
        self.timeout.write_text('#!/usr/bin/env bash\nprintf "%s\\n" "$1" >"$TIMEOUT_LOG"\nshift\nexec "$@"\n')
        self.timeout.chmod(0o755)
        self.gh = self.bin / "gh"
        self.gh.write_text(
            '#!/usr/bin/env bash\n'
            'printf "%s\\n" "$@" >>"$GH_LOG"\n'
            'if [[ "$GH_STATUS" != 0 ]]; then\n'
            '    echo "offline or unauthenticated" >&2\n'
            '    exit "$GH_STATUS"\n'
            'fi\n'
            '[[ -z "$GH_HEAD" || "$3" == "$GH_HEAD" ]] || exit 1\n'
            'printf "%s\\n" "$GH_BASE"\n'
        )
        self.gh.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.bin), GIT_CONFIG_NOSYSTEM="1",
                        GIT_CONFIG_GLOBAL=os.devnull, GH_BASE="", GH_STATUS="0", GH_HEAD="",
                        GH_LOG=str(self.root / "gh.log"),
                        TIMEOUT_LOG=str(self.root / "timeout.log"))
        self.env.pop("GITHUB_BASE_REF", None)
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.name", "test")
        self.git("config", "user.email", "test@example.com")
        self.git("config", "commit.gpgsign", "false")
        self.git("remote", "add", "origin", "https://github.com/moq-dev/moq.git")
        self.commit("base.txt")
        self.git("update-ref", "refs/remotes/origin/main", "HEAD")
        self.commit("stack.txt")
        self.git("update-ref", "refs/remotes/origin/release", "HEAD")
        self.git("update-ref", "refs/remotes/origin/stack", "HEAD")
        self.git("checkout", "-qb", "local-name")
        self.commit("feature.txt")
        self.git("update-ref", "refs/remotes/origin/quest/m1/pr-name", "HEAD")
        self.git("update-ref", "refs/remotes/origin/local-name", "HEAD")

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env,
                              text=True, capture_output=True, check=True).stdout.strip()

    def commit(self, name):
        (self.repo / name).write_text(name)
        self.git("add", name)
        self.git("commit", "-qm", name)

    def track(self, branch):
        self.git("branch", "--set-upstream-to", branch)

    def expect(self, base, files, *args):
        result = subprocess.run([str(CHANGED), *args], cwd=self.repo, env=self.env,
                                text=True, capture_output=True, check=True)
        self.assertEqual(result.stderr, f"changed: base {base}\n")
        self.assertEqual(result.stdout.splitlines(), sorted(files))

    def test_differently_named_pr_head_is_not_the_base(self):
        self.track("origin/quest/m1/pr-name")
        self.expect("origin/main", ["stack.txt", "feature.txt"])

    def test_pr_uses_tracked_head_and_stacked_base(self):
        self.track("origin/quest/m1/pr-name")
        self.env["GH_BASE"] = "stack"
        self.env["GH_HEAD"] = "quest/m1/pr-name"
        self.expect("origin/stack", ["feature.txt"])
        self.assertEqual((self.root / "gh.log").read_text().splitlines(), [
            "pr", "view", "local-name", "--json", "baseRefName,state", "--jq",
            'select(.state == "OPEN") | .baseRefName',
            "pr", "view", "quest/m1/pr-name", "--json", "baseRefName,state", "--jq",
            'select(.state == "OPEN") | .baseRefName',
        ])
        self.assertEqual((self.root / "timeout.log").read_text(), "3s\n")

    def test_base_upstream_looks_up_local_head(self):
        self.track("origin/main")
        self.env["GH_BASE"] = "stack"
        self.expect("origin/stack", ["feature.txt"])
        self.assertEqual((self.root / "gh.log").read_text().splitlines()[2], "local-name")

    def test_local_pr_wins_over_tracked_stack_pr(self):
        self.track("origin/stack")
        self.env["GH_BASE"] = "stack"
        self.env["GH_HEAD"] = "local-name"
        self.expect("origin/stack", ["feature.txt"])
        self.assertEqual((self.root / "gh.log").read_text().splitlines()[2], "local-name")

    def test_same_named_head_falls_back_to_main(self):
        self.track("origin/local-name")
        self.expect("origin/main", ["stack.txt", "feature.txt"])

    def test_no_upstream(self):
        self.expect("origin/main", ["stack.txt", "feature.txt"])

    def test_release_upstream(self):
        self.track("origin/release")
        self.expect("origin/release", ["feature.txt"])

    def test_offline_unauthenticated_or_timed_out(self):
        self.track("origin/quest/m1/pr-name")
        for status in (1, 4, 124):
            with self.subTest(status=status):
                self.env["GH_STATUS"] = str(status)
                self.expect("origin/main", ["stack.txt", "feature.txt"])

    def test_missing_gh(self):
        self.gh.unlink()
        self.track("origin/release")
        self.expect("origin/release", ["feature.txt"])

    def test_missing_timeout(self):
        self.timeout.unlink()
        self.track("origin/release")
        self.expect("origin/release", ["feature.txt"])
        self.assertFalse((self.root / "gh.log").exists())

    def test_detached_head_without_upstream(self):
        self.git("checkout", "--detach", "-q")
        self.expect("origin/main", ["stack.txt", "feature.txt"])
        self.assertFalse((self.root / "gh.log").exists())

    def test_ci_base_wins_without_lookup(self):
        self.env["GITHUB_BASE_REF"] = "release"
        self.env["GH_BASE"] = "main"
        self.expect("origin/release", ["feature.txt"])
        self.assertFalse((self.root / "gh.log").exists())

    def test_explicit_base_wins_without_lookup(self):
        self.env["GITHUB_BASE_REF"] = "main"
        self.expect("origin/stack", ["feature.txt"], "origin/stack")
        self.assertFalse((self.root / "gh.log").exists())

    def test_untracked_and_working_tree_changes(self):
        (self.repo / "base.txt").write_text("changed")
        (self.repo / "new.txt").write_text("new")
        self.expect("origin/main", ["base.txt", "stack.txt", "feature.txt", "new.txt"])

    def test_invalid_explicit_base_is_an_error(self):
        result = subprocess.run([str(CHANGED), "missing"], cwd=self.repo, env=self.env,
                                text=True, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot resolve merge-base against missing", result.stderr)


if __name__ == "__main__":
    unittest.main()
