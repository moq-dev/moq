#!/usr/bin/env python3
"""Exercise merge-queue platform selection with real diffs and no toolchains."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

SOURCE = Path(__file__).resolve().parent


class PlatformScopeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'sh').mkdir()
        for name in ('dispatch.sh', 'changed.sh'):
            shutil.copy(SOURCE / name, self.root / 'sh' / name)
        self.git('init', '-q')
        self.git('config', 'user.name', 'test')
        self.git('config', 'user.email', 'test@example.com')
        self.git('config', 'commit.gpgsign', 'false')
        self.git('add', '.')
        self.git('commit', '-qm', 'base')

    def git(self, *args):
        return subprocess.run(['git', *args], cwd=self.root, check=True,
                              capture_output=True, text=True).stdout.strip()

    def scope(self, path=None, base='HEAD'):
        if path:
            target = self.root / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text('changed')
        result = subprocess.run(['bash', 'sh/dispatch.sh', 'ci-platform', base],
                                cwd=self.root, env=dict(os.environ, MOQ_ALLOW_HOST='1'),
                                capture_output=True, text=True, check=True)
        return dict(line.split('=') for line in result.stdout.splitlines())

    def test_no_platform_work_for_quest_or_js(self):
        for path in ('quest/m1/example.md', 'js/net/src/example.ts'):
            with self.subTest(path=path):
                self.assertEqual(self.scope(path)['needed'], 'false')

    def test_rust_dependency_rebuilds_external_consumers(self):
        result = self.scope('rs/moq-net/src/example.rs')
        for module in ('rs', 'swift', 'cpp', 'obs_compile', 'needed'):
            self.assertEqual(result[module], 'true')
        self.assertEqual(result['moq_sh'], 'false')

    def test_swift_docs_and_installer_select_only_their_suites(self):
        result = self.scope('doc/lib/swift/index.md')
        self.assertEqual(result['swift'], 'true')
        self.assertEqual(result['rs'], 'false')
        result = self.scope('infra/moq-sh/install.sh')
        self.assertEqual(result['moq_sh'], 'true')
        self.assertEqual(result['rs'], 'false')

    def test_global_configuration_never_skips_platforms(self):
        for path in ('.cargo/config.toml', '.github/workflows/platform.yml'):
            with self.subTest(path=path):
                self.assertTrue(all(value == 'true' for value in self.scope(path).values()))

    def test_all_does_not_need_a_base(self):
        self.assertTrue(all(value == 'true' for value in self.scope(base='--all').values()))

    def test_invalid_base_fails_instead_of_skipping(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.scope(base='missing-base')


if __name__ == '__main__':
    unittest.main()
