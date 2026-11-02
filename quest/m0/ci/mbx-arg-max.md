# [S] Pin mbx after its launch metadata stops causing E2BIG

## Goal

Pin an mbx release whose launch metadata does not turn an OS-valid Cargo
invocation into `E2BIG`. `RecipeTest.test_unexecuted_bench_is_not_compiled`
(`sh/rs/select.test.py`) passes in `just ci`, including with a long inherited
PATH that plain Cargo accepts.

## Plan

Investigated 2026-11-01. The fix belongs in
[jdx/mr-boxington](https://github.com/jdx/mr-boxington), outside this
repository. No upstream post or patch was submitted. An upstream release
with the fix is the condition for completing this quest; then update the
`version` pin in `.github/actions/rust-cache/action.yml` (currently 1.28.0)
and verify the focused test with that release.

The original failure was reported during
[moq-dev/moq#7567](https://github.com/moq-dev/moq/pull/7567).
The focused test passes in a smaller environment. Appending repeated
`:/usr/bin` entries to PATH reproduces its exact mbx error for both its
library and binary fixtures, without changing its Cargo arguments:
`Argument list too long (os error 7)`.

A separate dependency-free Cargo fixture isolates the cause. With only
HOME, PATH, CARGO, RUSTC, MBX_SCHEDULER=0, and MBX_SUMMARY=off in the incoming
environment, direct Cargo succeeds with both PATH lengths. mbx 1.25.0 and
the latest release, [1.29.1](https://github.com/jdx/mr-boxington/releases/tag/v1.29.1),
succeed at 1,080 bytes and fail at 40,077 bytes. The incoming environment
in the failing case is only 40,340 bytes, and the mbx argv is under 200 bytes.
`strace -f -v -s 1000000 -e trace=execve` shows mbx's Cargo exec returning
`E2BIG`, with these environment entry sizes including name, `=`, and NUL:

| Entry | mbx 1.25.0 | mbx 1.29.1 |
| --- | ---: | ---: |
| PATH | 40,083 | 40,083 |
| MBX_SESSION_PATH | 142,525 | 142,525 |
| MBX_SESSION_RESTORE | 145,702 | 145,870 |

This hits Linux's per-string limit (131,072 bytes on this 4 KiB-page host),
not its 2,097,152-byte aggregate `ARG_MAX`.

The source is
[`record_overlay` in 1.29.1](https://github.com/jdx/mr-boxington/blob/v1.29.1/crates/mbx/src/cli/launch.rs#L523).
It JSON-serializes an `OsString` PATH into `MBX_SESSION_PATH`, and includes
PATH again in the serialized restoration overlay, `MBX_SESSION_RESTORE`.
Serde represents Unix `OsString` values as byte arrays, so both entries
expand beyond the per-string limit even though PATH itself is valid.
The same implementation remains in
[upstream main at 42c019c](https://github.com/jdx/mr-boxington/blob/42c019cd656409ab403472efe16804bc6cbfe9b1/crates/mbx/src/cli/launch.rs#L523).
No matching open upstream issue or PR was found during the investigation.

Recommended next action: obtain authorization to submit this reproduction
upstream, recommending file-backed session restoration and PATH metadata,
with lifetime/ownership shared by the build session and its descendants.
Merely using a more compact environment encoding moves the failure to a
longer valid PATH; trimming this repository's environment or bypassing mbx
would hide the upstream bug.

To reproduce in the Nix dev shell, create a dependency-free package under
`.tmp/` and compare real Cargo with mbx. This script inherits no credentials;
run tracing only around this controlled environment if inspecting envp:

```python
import os
import shutil
import subprocess
from pathlib import Path

scratch = Path(".tmp/mbx-arg-max").resolve()
(scratch / "src").mkdir(parents=True, exist_ok=True)
(scratch / "home").mkdir(exist_ok=True)
(scratch / "Cargo.toml").write_text(
    '[package]\nname="arg-max-probe"\nversion="0.0.0"\nedition="2024"\n[workspace]\n'
)
(scratch / "src/lib.rs").write_text("pub fn probe() {}\n")
rustbin = Path(shutil.which("rustc")).parent
mbx = shutil.which("mbx")
env = {
    "HOME": str(scratch / "home"),
    "PATH": f"{rustbin}:/usr/bin:/bin" + ":/usr/bin" * 4444,
    "CARGO": str(rustbin / "cargo"),
    "RUSTC": str(rustbin / "rustc"),
    "MBX_SCHEDULER": "0",
    "MBX_SUMMARY": "off",
}
for program in (str(rustbin / "cargo"), mbx):
    result = subprocess.run(
        [program, "build", "--offline", "--manifest-path", str(scratch / "Cargo.toml")],
        cwd=scratch,
        env=env,
    )
    print(program, result.returncode)
```

Verify both the reproduction and the original focused test when a fix is
available. Keep the benchmark exclusion assertion and production mbx path
intact. The original goal's "however long" is bounded by the OS; this goal
now states the actionable boundary: mbx must not reject caller-valid input
because of its own metadata expansion.
