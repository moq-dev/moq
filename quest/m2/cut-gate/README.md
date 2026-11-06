# [S] Cut readiness

## Goal

Before each cut of `main` into `release`, the checks the cut needs that break
nothing have passed: `main` is no slower than `release`, vendored code has
the upstream fixes it needs, the pre-cut audit's findings are fixed, and the
hand-written `moq-c` is at parity with moq-ffi. This epic's own work, once
every entry under `Required` has merged: confirm the children's results still
hold on the release candidate commit and record that commit in the
[release cut](/quest/m3/release/cut.md)'s Plan.

## Plan

Decided 2026-11-05 (maintainer, milestone re-cut): the cut waits on m2 alone,
so the checks that gate it without breaking anything live under one recurring
gate in m2 instead of in m1, where the cut no longer looks.

Recurs once per release cycle, always in m2. It does not re-create itself:
m2 holds it and the cut requires m2, so a copy made before the cut would
block that cut forever. The release cut's PR re-creates it at this path while
it shifts the milestones, after removing this path from every `Required`. The
new gate keeps this Goal and this paragraph, gets a fresh "main is no slower
than release" child with its Goal and no measurements yet, and takes the
[vendor sync](/quest/m3/vendor-sync/README.md) questline that this
run re-created in m3. The pre-cut audit is a one-time child and does not
recur. Only `/quest-delete` ends the recurrence.

Held by an open claim at the re-cut: [release-perf](/quest/m1/release-perf/README.md)
stays in m1 until its claim merges, so this gate lists it as a blocker; the
[re-cut follow-up](/quest/m2/recut-held.md) moves what remains of it here.

## Required

- [Pre-cut audit findings](/quest/m2/cut-gate/cut-audit/README.md) - fix the crashes, hostile-input costs, and correctness bugs the 2026-10-24 audit of main found
- [Main is no slower than release](/quest/m1/release-perf/README.md) - every benchmark where main is slower than release is fixed or accepted with a reason (held in m1 by an open claim)
