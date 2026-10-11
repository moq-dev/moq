"""Exercise the CI installer's ownership boundary without changing Homebrew."""
import runpy
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

SCRIPT = Path(__file__).with_name('gst-formula.py')


class Installer(unittest.TestCase):
    def invoke(self, directory, target='aarch64-apple-darwin'):
        archive = directory / f'moq-gst-v1.2.3-{target}.tar.gz'
        archive.write_bytes(b'archive fixture')
        with patch.object(sys, 'argv', [str(SCRIPT), '1.2.3', target, str(directory)]):
            runpy.run_path(str(SCRIPT), run_name='__main__')

    def test_existing_install_is_not_owned_or_removed(self):
        with tempfile.TemporaryDirectory() as tmp, patch('subprocess.run', return_value=subprocess.CompletedProcess([], 0, 'moq-gst 1.0\n', '')) as run, patch('subprocess.check_output', return_value=tmp):
            (Path(tmp) / 'Formula').mkdir()
            with self.assertRaisesRegex(ValueError, 'already installed'):
                self.invoke(Path(tmp))
            self.assertEqual([call.args[0] for call in run.call_args_list], [['brew', 'list', '--formula', '--versions']])

    def test_other_target_refused_before_brew(self):
        with tempfile.TemporaryDirectory() as tmp, patch('subprocess.run') as run:
            with self.assertRaisesRegex(ValueError, 'macOS'):
                self.invoke(Path(tmp), 'x86_64-unknown-linux-gnu')
            run.assert_not_called()

    def test_failed_test_cleans_only_created_tap_and_install(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'Formula').mkdir()
            def brew(command, **kwargs):
                if command[1] == 'test':
                    raise subprocess.CalledProcessError(1, command)
                return subprocess.CompletedProcess(command, 0, '', '')
            with patch('subprocess.run', side_effect=brew) as run, patch('subprocess.check_output', return_value=str(root)):
                with self.assertRaises(subprocess.CalledProcessError):
                    self.invoke(root)
            commands = [call.args[0] for call in run.call_args_list]
            self.assertEqual(commands[-2:], [['brew', 'uninstall', '--force', 'moq-dev/ci-gst/moq-gst'], ['brew', 'untap', 'moq-dev/ci-gst']])
            formula = (root / 'Formula/moq-gst.rb').read_text()
            self.assertIn((root / 'moq-gst-v1.2.3-aarch64-apple-darwin.tar.gz').as_uri(), formula)
            self.assertNotIn('__VERSION__', formula)


if __name__ == '__main__':
    unittest.main()
