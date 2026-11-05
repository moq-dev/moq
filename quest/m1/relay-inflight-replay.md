# [S] Finish request-time relay join snapshots

## Goal

Pending lite-03/04/05 joins skip history before their request-time answer and
deliver every group produced afterward, in Rust and JS.

## Plan

Decided 2026-11-03 (maintainer, /quest-unblock): keep the request-time floor
and land after HLS grid export. The grid prerequisite has landed.

Decided 2026-11-04 (maintainer, /quest-unblock): subscription front with
metadata but no current copy: ✅ defer the wire join until the next copy
supplies its snapshot. Not chosen: joining at the edge the metadata implies.

Decided 2026-11-04 (maintainer, /quest-unblock): lite-01/02 parity:
✅ mirror Rust's arrival-only exception in JS. Not chosen: leaving JS different.

Both decisions are implemented. Pending Rust fronts retain a shared immutable
first-answer cell across splices and copyless intervals. The provisional-install
finding was refuted with a passing visibility regression and accepted by the
local reviewer. Local Claude round 4 reviewed implementation
[8a321d144a](https://github.com/moq-dev/moq/commit/8a321d144aeed36eefca778a831ffca1658f2b45)
with zero findings; local CI (10,814 Rust tests plus JS/repository checks) and
full interop passed.

Subsequent online findings remain:
- Rust captures the snapshot after demand registration wakes producers, allowing
  demand-triggered groups to enter the floor. Capture before publishing demand.
- JS accept uses nullish assignment, which overwrites an empty first snapshot
  on a repeated accept. Preserve null and assign only when undefined.
- Add routed pending JS coverage, so the copy-to-front snapshot handoff is tested.

## Waiting

- Maintainer: the fourth local review round triggers the required design checkpoint before further fixes. Options: continue with the targeted fixes and routed regression above (recommended; the shared immutable snapshot design remains bounded and appropriate), or rethink the approach. Once authorized, implement the remaining findings, rerun checks and local review, and mark the PR ready.
