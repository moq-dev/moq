# [S] TS compliance duration-fidelity captures the whole source

## Goal

Interop's TS-compliance `duration-fidelity` check (`test/ts/run.sh`) captures
the whole round-tripped stream, not a fraction of it, fixed at its cause.

## Plan

Seen 2026-10-10 on #5158's second Interop run: the export captured 3.3 s of a
20.1 s source, and the check failed; it also failed once on #5140 and passed
on rerun. Planned under this questline's cause-first rules (decided
2026-10-10): find whether the export ends early (an announce `End`, linger, or
a catalog finish arriving before the media), starts late, or the capture is
cut off by the harness. Fix that at its source, with a regression that fails
without it. No longer timeout and no lower duration threshold.

Check against #5147 (export ts follows announcements), which may change how an export ends.

Public API: none expected. Wire: none expected.
