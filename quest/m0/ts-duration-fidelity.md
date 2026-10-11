# [S] TS compliance duration-fidelity captures the whole source

## Goal

Interop's TS-compliance `duration-fidelity` check (`test/ts/run.sh`) captures
the whole round-tripped stream, not a fraction of it, fixed at its cause.

## Plan

Moved to m0 on 2026-10-10: it now fails on every Interop run, main
included (`c0c0bb33`, 3.2 s of 20.1 s, and each head of #5099), so it reads
as a regression, not a flake. A red Interop on main hides real regressions
in every PR. Start by finding the last green Interop run on main and the
first red one, and bisect between them. #5217's isolated local run captured
the whole source (20.47 s of 20.06 s), so look at what differs on the CI
runner.

First seen 2026-10-10 on #5158's second Interop run: the export captured 3.3 s of a
20.1 s source, and the check failed; it also failed once on #5140 and passed
on rerun. Find whether the export ends early (an announce `End`, linger, or
a catalog finish arriving before the media), starts late, or the capture is
cut off by the harness. Fix that at its source, with a regression that fails
without it. No longer timeout and no lower duration threshold.

#5147 (export ts follows announcements, merged 2026-10-10) changed how an
export ends, so check it first. Never raise a timeout or add a retry (the
test-flakes-2 rules still apply).

Public API: none expected. Wire: none expected.
