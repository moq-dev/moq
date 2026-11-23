# [M] TS harness late join to one program

## Goal

`just test ts --mpts --program N --join S` splits the harness's two-program
source with `import ts --program all` and starts one `export ts` of program
N's broadcast S seconds in. The export writes its first byte within 3 s of
starting. `holes.py` finds nothing missing from the program's video and audio
from the first random access point to the end. The capture's PAT lists only
that program, with its PMT, PCR PID and elementary streams.

## Plan

Proposed in [#9315](https://github.com/moq-dev/moq/issues/9315), which
measured the first byte within 1.7 s on `b45179d39b` (P2-u). `--mpts` runs
only with `--passthrough` today (`test/ts/run.sh`). This arm adds the
`--program all` split and its own late-starting export, graded with the
existing `holes.py`.

In m3, not m4, because the reporter's broadcast relies on this behaviour and
offered to write the arm.
