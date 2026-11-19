# [M] A JS resumed reader hands out only groups in its window

## Goal

`js/net/src/resume.ts` hands a reader only groups inside the ranges it resolved, as Rust's `Reader::poll_lowest` does ("the window decides"), so the Nightly `resume-model` property `readers match the logical track` passes. Done when seeds 1–5 and 1218508351 pass at 4096 cases, plus `just test resume-model`.

## Plan

Caused by [moq-dev/moq#9027](https://github.com/moq-dev/moq/pull/9027) (`0c14c87c49`): it moved JS to time anchors to match Rust's [moq-dev/moq#8638](https://github.com/moq-dev/moq/pull/8638) but left out Rust's guard. A copy that doesn't yet hold the group a range starts at resolves the start to an earlier group, and JS hands that group out. The model passes at #9027's parent on four seeds × 4096 cases and fails at #9027 on all of them, including Nightly's counterexample (seed 1218508351, run [44342367698](https://github.com/moq-dev/moq/actions/runs/44342367698)): `reader 0: group 0 outside ranges 1,4 (resolved from 1) and bounds undefined`. Rust is correct: the failing cases, ported to Rust's regression test, pass.

Partial work: branch `fix/js-resume-window-wip` (`0f1b73d365`) adds the guard and a regression case. It passes the Nightly seed and seeds 3 and 5; seeds 1, 2 and 4 still fail with groups missing or leaking. An earlier attempt broke because `#placed` forces copies to resolve early, and the model's own `servingResolved` check calls it too.

Recommended: port Rust's reader window properly:

- a reader-owned window that keeps its first placement, built from each copy's resolution only where a failover cap doesn't empty that range, and filled from each copy's unclamped resolution when ranges are replaced;
- the guard, and Rust's `owes_below` check for ordered readers;
- `#backfillGaps` starting from the window's floor;
- `#placed` no longer forcing copies to resolve early.

Public API: none. Wire: none.
