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
- That makes the mux run ahead of decode by a buffer delay. The delay grows to
  fit the largest burst seen and is capped by `--max-age`, which already bounds
  the pacer's lead; no new flag. Say what happens when a burst does not fit the
  cap (fail loud, or drop to VBR with a warning) and record the choice here.
- The CLI `Delivery` pacer releases one slice per PCR interval; confirm it
  still writes on the schedule the PCRs describe.
- Grade the distribution of bytes between consecutive PCRs, not only
  adjacency: extend `test/ts/pcr-timing.py` (or add a check beside it) and run
  it in the existing TS test recipe against a CBR fixture.
- `doc/bin/cli.md`: say that export pads to `mpegts.muxRate` on a constant-rate
  schedule and what latency that adds.

## Closes

- [#3925](https://github.com/moq-dev/moq/issues/3925) - close this issue when the quest finishes
