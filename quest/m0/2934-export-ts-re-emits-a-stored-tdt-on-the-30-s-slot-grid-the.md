# [M] export ts re-emits a stored TDT on the 30 s slot grid: the clock arrives ~14 s late, and below…

## Goal

Implement and verify the behavior tracked in [#2934](https://github.com/moq-dev/moq/issues/2934)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Pointed the #2914 rig at #2929's head as offered (`dev` `016fc09ae`).

**Carriage is confirmed.** TDT and TOT both reach the egress, and TOT's `local_time_offset` descriptors arrive byte-identical to the source's  -  both regions, CRC recomputed around the re-stamped UTC field. The 0-packet gap is closed, and the descriptor result is the part of your argument for proxying that I could not have taken on trust.

The clock those sections *assert* is a separate matter. Source re-stamped with `tsp -P timeref --start system`, so every TDT transmitted asserts the true UTC of its own transmission and any lateness downstream is the lane's. 300 s per arm; the arms differ only in how the source's cadence sits against `si_interval`'s 30 s for 0x70/0x73.

| arm | source TDT | delivered | median late | max | re-sent a time already asserted |
|---|---|---|---|---|---|
| control  -  same publisher, no MoQ | 15.1 s | 20 × 0x70 | 470 ms | 916 ms |  -  |
| base | 15.1 s | 11 × 0x70 | **14,385 ms** | 15,546 ms | no |
| slow | 45 s | 11 × 0x70 | 23,894 ms | 38,861 ms | **4 of 11** |
| TDT + TOT | 20 s | 11 × 0x70, 11 × 0x73 | 4,332 ms | 5,009 ms | no |

**The defect is the slow arm**  -  a source ticking slower than the exporter emits. Seven distinct source values, eleven emissions:

```text
  0.01 s  asserts 09:54:50      19.21 s  asserts 09:54:50
 49.48 s  asserts 09:55:35      79.23 s  asserts 09:55:35
```

A receiver sets its clock from the first, advances it locally for 19 s, then reads the same time again and steps **backwards** by 19 s. A receiver of the original multiplex never sees that, because every TDT on that wire carries a new time  -  which is the sense in which the staleness here isn't quite the bound a TS receiver already lives with.

**The cost, separately, is the base arm.** `due()` fires when a media timestamp crosses an *absolute* `si_interval` slot, so emission is a 30 s grid rather than a floor on repetition: a section arriving mid-slot waits for the boundary, and with a 15.1 s source half the ticks are dropped and the section sent is the older of the two held. Not drift  -  lateness is flat at 13.4–15.5 s across eight consecutive emissions, against ≤ 917 ms of total instrument slide over the same 300 s. And it is a phase rather than a constant: 4.3 / 14.4 / 23.9 s across the three arms on one build, so it is not an offset anything downstream could be told about and correct for.

**Suggested fix, narrow.** For a latest-value slot, treat the interval as a floor on *repetition* and emit when the value changes, falling back to the grid only to satisfy the DVB maximum. Your reasoning that a source's observed cadence "means nothing downstream" is right for a static table, where any repetition carries the same information whenever it is sent; for a clock the cadence *is* the information. Suppressing a repeat that would assert an already-sent time would remove the backwards step on its own, but TDT is mandatory at ≤ 30 s, so emit-on-change is the option that satisfies both.

Rig is `tdt-moq.sh` + `tdt-staleness.py`, three arms plus the no-MoQ control, and I am happy to re-run it against a fix.

## Closes

- [#2934](https://github.com/moq-dev/moq/issues/2934) - close this issue when the quest finishes
