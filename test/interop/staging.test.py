#!/usr/bin/env python3
"""Exercise setup and late consumers after collecting only owned build fixtures."""

import os
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def executable(path, body):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + body)
    path.chmod(0o755)


class Staging(unittest.TestCase):
    def setUp(self):
        (ROOT / ".tmp").mkdir(exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=ROOT / ".tmp", prefix="staging-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.target = self.root / "shared target"
        self.profile = self.target / "debug"
        self.profile.mkdir(parents=True)
        self.tools = self.root / "tools"
        self.tools.mkdir()
        self.env = {k: v for k, v in os.environ.items() if not k.startswith(("INTEROP_", "HARNESS_"))}
        self.env.update(
            PATH=f"{self.tools}:{os.environ['PATH']}",
            GITHUB_ACTIONS="true",
            BASH_ENV="",
            CC="cc",
            HARNESS_RUNS=str(self.root / "runs"),
            FIXTURE_TARGET=str(self.target),
        )
        self.env.pop("RELAY_BIN", None)
        self.env.pop("CLI_BIN", None)
        executable(
            self.tools / "cargo",
            """
case "$1" in
    metadata) printf '{"target_directory":"%s"}\n' "$FIXTURE_TARGET" ;;
    install)
        while [[ "$1" != --root ]]; do shift; done
        mkdir -p "$2/bin"
        cp "$FIXTURE_TARGET/debug/moq" "$2/bin/moq"
        cp "$FIXTURE_TARGET/debug/moq-relay" "$2/bin/moq-relay"
        ;;
    build)
        if [[ "$*" == *'--manifest-path'* ]]; then
            while [[ "$1" != --target-dir ]]; do shift; done
            mkdir -p "$2/debug"
            cp "$FIXTURE_TARGET/adapter" "$2/debug/compat-container"
        elif [[ "$*" == *'-p moq-c'* ]]; then
            printf '{"reason":"build-script-executed","package_id":"/moq-c#0.0.0","out_dir":"%s/header"}\n' \
                "$FIXTURE_TARGET"
        fi
        ;;
    *) exit 2 ;;
esac
""",
        )
        for name in ("moq", "moq-relay"):
            executable(self.profile / name, "echo owned-binary\n")

    def run_script(self, script, expected=0):
        path = self.root / "run.sh"
        path.write_text("set -euo pipefail\n" + script)
        result = subprocess.run(["bash", str(path)], env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        return result

    def test_interop_consumers_survive_collection(self):
        source = (ROOT / "test/interop/interop.sh").read_text()
        setup, runtime = source.split("# ── setup", 1)
        setup = re.sub(r"^INTEROP_DIR=.*$", "INTEROP_DIR=" + shlex.quote(str(ROOT / "test/interop")), setup, flags=re.M)
        for name in re.findall(r"\$EXAMPLES/(interop-[\w-]+)", runtime):
            executable(self.profile / "examples" / name, "echo owned-example\n")
        # Exercise dereferencing, including a symlink to a cache outside target.
        cached = self.root / "cache"
        cached.mkdir()
        shutil.move(self.profile / "moq-relay", cached / "relay")
        (self.profile / "moq-relay").symlink_to(cached / "relay")
        clients = self.root / "clients/c"
        clients.mkdir(parents=True)
        (clients / "subscribe.c").write_text('#include "moq.h"\nint main(void) { return fixture() != 42; }\n')
        header = self.target / "header/include"
        header.mkdir(parents=True)
        (header / "moq.h").write_text("int fixture(void);\n")
        native = self.root / "fixture.c"
        native.write_text("int fixture(void) { return 42; }\n")
        subprocess.run(["cc", "-c", str(native), "-o", str(self.root / "fixture.o")], check=True)
        subprocess.run(["ar", "rcs", str(self.profile / "libmoq.a"), str(self.root / "fixture.o")], check=True)
        extension = "dylib" if os.uname().sysname == "Darwin" else "so"
        subprocess.run(
            ["cc", "-shared", "-fPIC", str(native), "-o", str(self.profile / f"libgstmoq.{extension}")], check=True
        )
        # A stray shared moq library must never win over the staged staticlib.
        (self.profile / f"libmoq.{extension}").write_text("invalid shared library")
        executable(self.tools / "gst-launch-1.0", "exit 0\n")
        executable(
            self.tools / "gst-inspect-1.0",
            """
if [[ -n "${GST_PLUGIN_PATH_1_0:-}" ]]; then
    python3 - "$GST_PLUGIN_PATH_1_0" <<'CHECK'
import ctypes, pathlib, sys
paths = list(pathlib.Path(sys.argv[1]).glob('libgstmoq.*'))
assert len(paths) == 1
assert ctypes.CDLL(str(paths[0])).fixture() == 42
CHECK
fi
printf '  moqsink: fixture\n  moqsrc: fixture\n'
""",
        )
        # Header and archive must survive collection before the C compiler reads them.
        executable(self.tools / "cc", 'rm -rf "$FIXTURE_TARGET"\nexec ' + shlex.quote(shutil.which("cc")) + ' "$@"\n')
        # Build C last, since its compiler deliberately collects this fixture target.
        # Preserve the actual consumer expressions, so a leftover target path fails.
        late_examples = "\n".join(
            '"' + path + '"'
            for path in sorted(
                set(re.findall(r"\$(?:EXAMPLES|TARGET_BASE/\$PROFILE/examples)/interop-[\w-]+", runtime))
            )
        )
        self.run_script(
            setup
            + f"""
require_tools
build_relay_cli
prepare_gst
CLIENTS={shlex.quote(str(clients.parent))}
prepare_c
[[ -z "$BROKEN_LANGS" ]]
rm -rf {shlex.quote(str(cached))}
[[ ! -e "$TARGET_BASE" ]]
"$RELAY"
"$MOQ"
"$C_INTEROP"
GST_PLUGIN_PATH_1_0="$GST_PLUGIN_DIR" gst-inspect-1.0 moq
{late_examples}
"""
        )
        self.assertEqual(list((self.root / "runs").iterdir()), [])

    def test_override_binaries_are_owned(self):
        source = (ROOT / "test/interop/interop.sh").read_text().split("# ── setup", 1)[0]
        source = re.sub(
            r"^INTEROP_DIR=.*$", "INTEROP_DIR=" + shlex.quote(str(ROOT / "test/interop")), source, flags=re.M
        )
        self.env.update(
            RELAY_BIN=str(self.profile / "moq-relay"),
            CLI_BIN=str(self.profile / "moq"),
            INTEROP_SUB_MOQ=str(self.profile / "moq"),
        )
        self.run_script(
            source
            + """
require_tools
build_relay_cli
rm -rf "$TARGET_BASE"
"$RELAY"
"$MOQ"
"$INTEROP_SUB_MOQ"
"""
        )

    def test_wire_compat_adapters_and_current_binaries_are_owned(self):
        executable(self.tools / "gh", "exit 1\n")
        executable(
            self.tools / "bun",
            """
if [[ "$*" == *'resolve.ts resolve'* ]]; then
    printf '{"moq-cli":"0.1.0","moq-relay":"0.1.0","hang":"0.1.0"}' >"$3"
fi
""",
        )
        executable(
            self.target / "adapter",
            """
case "$1" in
    encode) echo catalog >"$2"; echo frame >"$3" ;;
    decode) [[ "$(cat "$2")" == catalog && "$(cat "$3")" == frame ]] ;;
    fetch-supported) echo false ;;
    *) exit 2 ;;
esac
""",
        )
        source = (ROOT / "test/interop/compat.sh").read_text()
        setup = source.split("# A separate package root", 1)[0]
        setup = re.sub(r"^DIR=.*$", "DIR=" + shlex.quote(str(ROOT / "test/interop")), setup, flags=re.M)
        adapters = source.split("# Exercise the actual hang library release", 1)[1].split(
            'bun "$DIR/compat/client.ts" encode', 1
        )[0]
        self.run_script(
            setup
            + "# Exercise the actual hang library release"
            + adapters
            + """
rm -rf "$TARGET"
"$CURRENT" --help
"$HARNESS_RUN/current/moq-relay" --help
for source in current released; do
    "$HARNESS_RUN/hang-$source/compat-container" decode "$HARNESS_RUN/current.catalog" "$HARNESS_RUN/current.frame"
    [[ "$("$HARNESS_RUN/hang-$source/compat-container" fetch-supported moq-lite-07)" == false ]]
done
"""
        )

    def test_nested_run_owns_links_after_parent_teardown(self):
        binary = shlex.quote(str(self.profile / "moq"))
        self.run_script(
            f"""
source {shlex.quote(str(ROOT / "test/lib/harness.sh"))}
harness_begin parent
harness_stage bin/moq {binary}
parent="$HARNESS_RUN"
# Model the parent's lifetime ending before a child launches its next client.
HARNESS_RUN="$parent/child"
mkdir -p "$HARNESS_RUN"
harness_stage bin/moq "$parent/bin/moq"
[[ "$HARNESS_RUN/bin/moq" -ef "$parent/bin/moq" ]]
mv "$HARNESS_RUN" "$parent-saved-child"
rm -rf "$parent" "$FIXTURE_TARGET"
HARNESS_RUN="$parent-saved-child"
"$HARNESS_RUN/bin/moq"
"""
        )
        self.assertEqual(list((self.root / "runs").iterdir()), [])

    def test_copy_when_hard_links_are_unavailable(self):
        executable(self.tools / "ln", "exit 1\n")
        self.run_script(
            f"""
source {shlex.quote(str(ROOT / "test/lib/harness.sh"))}
harness_begin copy
harness_stage bin/moq "$FIXTURE_TARGET/debug/moq"
[[ ! "$HARNESS_RUN/bin/moq" -ef "$FIXTURE_TARGET/debug/moq" ]]
rm -rf "$FIXTURE_TARGET"
"$HARNESS_RUN/bin/moq"
"""
        )

    def test_missing_artifact_fails_and_keeps_evidence(self):
        self.run_script(
            f"""
source {shlex.quote(str(ROOT / "test/lib/harness.sh"))}
harness_begin missing
harness_stage bin/missing "$HARNESS_RUN/no-such-file"
echo should-not-run
""",
            expected=1,
        )
        self.assertEqual(len(list((self.root / "runs").iterdir())), 1)


if __name__ == "__main__":
    unittest.main()
