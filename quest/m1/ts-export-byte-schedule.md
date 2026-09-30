# [L] moq export ts: PCRs sit on the byte grid the mux rate implies

## Goal

When the catalog carries `mpegts.muxRate`, `moq export ts` emits a
constant-rate stream: the bytes between consecutive PCRs are what the declared
rate implies for that interval, so a receiver recovering its clock from packet
arrival can lock. Today the average rate is right (#3831) but the bytes clump:
188 B to 870 KB between PCRs where 31 KB is needed, and only 3.3 % of intervals
within 1 % of nominal (#3925). Streams without a mux rate stay VBR as today. A
UDP sink is out of scope; delivery stays with an external tool.

## Plan

- Padding and PCR placement become one scheduling decision in
  `rs/moq-mux/src/container/ts/export.rs` (`advance`, `emit`, `stuff`): a PCR's
  value follows its byte position at the mux rate, and a keyframe burst is
  spread over the slots before its DTS instead of landing between two PCRs.
- That makes the mux run ahead of decode by a buffer delay. Decided
  (2026-09-30): the delay is the fixed `--delay` from
  [fixed-delay release](/quest/m1/ts-export-delay.md), not one that grows
  to fit bursts. Say what happens when a burst does not fit the delay (fail
  loud, or drop to VBR with a warning) and record the choice here.
- The CLI `Delivery` pacer releases one slice per PCR interval; confirm it
  still writes on the schedule the PCRs describe.
- `test/ts/pcr-timing.py`'s `pcr-schedule` grades the bytes between
  consecutive PCRs, report-only in the TS recipe. Once the schedule lands, gate
  it there with `--schedule-pct-min`, against a fixture whose keyframes outgrow
  a PCR slot; the generated clip mostly pads and passes already.
- `doc/bin/cli.md`: say that export pads to `mpegts.muxRate` on a constant-rate
  schedule and what latency that adds.

## Required

- [Fixed-delay release](/quest/m1/ts-export-delay.md) - the delay this schedule paces against
