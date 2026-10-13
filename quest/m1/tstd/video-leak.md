# [S] TS export admits video at its full leak

## Goal

`moq export ts` admits a leak-bound video PID at its full Rbx rate, so a
source that fills its level's leak (no HRD, 1080i25 High@4.0 at Rbx 24 Mb/s)
exports without falling behind and missing a decode deadline.

## Plan

Reported in [#5362](https://github.com/moq-dev/moq/issues/5362) point 2 and
verified against `rs/moq-mux/src/container/ts/`: `Buffer::per_slot`
(`schedule.rs`) floors the rate to whole 188-byte packets per 25 ms slot and
subtracts one packet of slack, and `admit` charges each slot's clock packet to
the PCR PID, which is the video. Together that admits about 2.9 % below what
Rbx carries in payload bytes. The slack's only rationale is a comment about
bunching at slot edges; no commit or quest calls it a deliberate margin.

Decided 2026-10-12:

- **Exact accounting.** Cap a leak-bound PID at the lesser of Rx in packets
  and Rbx in payload bytes, since Rbx governs payload into EB while TB drains
  at Rx. Carry fractional packets from slot to slot and drop the one-packet
  slack. A standalone PCR packet still enters the video's transport buffer,
  so it counts against the Rx packet budget but not the Rbx payload budget.
- Test: a generated source whose video fills Rbx on average exports for the
  whole clip and passes the strict T-STD check that `just test ts --hrd` runs.

## Related

- [Lower TS mux overhead](/quest/m1/tstd/mux-overhead.md) - #5362 point 3, which closes the issue
