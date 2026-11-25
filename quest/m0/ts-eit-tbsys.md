# [M] TS EIT fixtures keep program 1's TBsys in bound

## Goal

The scheduled TS workflow's `EIT fixtures` check passes: `export ts` keeps program 1's system-information transport buffer (TBsys) within its T-STD bound, with no `system-buffers [hard] program 1 TBsys overflow`.

## Plan

Found by the Nightly triage of 2026-11-25 (TS run [45468310138](https://github.com/moq-dev/moq/actions/runs/45468310138)). `EIT fixtures` has failed since the 2026-11-23 scheduled run with `TBsys overflow x22`; every other check in its report passes. The last pass was the 2026-11-22 run at `f2eab6d3c2`. Reproduce with `just test ts-eit` on `main` and bisect from there, looking for SI/PSI pacing changes in moq-mux's TS writer.

`TS private PES` also failed once, in a workflow_dispatch run at `36eda9361c`. Check whether it repeats on a scheduled run first; fold it in here only if it shares this cause.

Decided 2026-11-25 (maintainer, /quest-plan): ✅ keep in m0 (rejected: fold into [ts-si-grid](/quest/m3/ts-si-grid.md); drop), since it is a fresh regression keeping a scheduled run red.

## Related

- [TS congested path](/quest/m0/ts-congested-path.md) - the TS workflow's other failing check
- [TS SI tables repeat on the schedule's grid](/quest/m3/ts-si-grid.md) - SI repetition timing in the same writer
