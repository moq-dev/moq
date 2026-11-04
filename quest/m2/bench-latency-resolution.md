# [S] moq-bench reports sub-millisecond latency

## Goal

moq-bench's delivery latency over the steady window resolves finer than
1 ms, so relay changes that move latency by microseconds show up in its
percentiles instead of rounding to the same whole millisecond.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03 /quest-afk and /quest-complete runs), milestone:

- m0
- m1
- ✅ m2

`rs/moq-bench/src/stats.rs` records latency with `as_millis()` into
one-millisecond buckets (`LATENCY_BUCKETS`, 60,001 buckets through 60 s).
The relay-per-thread spike
([moq-dev/moq#7534](https://github.com/moq-dev/moq/pull/7534)) found its
glass-to-glass p50 "within the generator's millisecond resolution" while the
in-process hop measured 75 to 175 us.

- Send the keyframe's send time in microseconds: the frame header
  (`rs/moq-bench/src/connection.rs`) carries only `timestamp_ms`, so a finer
  receive clock against a millisecond send time would mostly measure that
  truncation. Add a microsecond field beside `timestamp_ms`, optional in
  `RecvHeader` so older publishers still parse, and pass it to
  `Stats::latency`. It's a bench payload field, not a wire change; don't get
  there by changing the track's timescale.
- Record microseconds (or finer) over the steady window with a bounded
  histogram (for example log-linear buckets), keeping memory flat.
- Keep the report's existing millisecond fields for the nightly series, and
  add finer ones, or migrate the series names deliberately.

Public API: none. Wire: none.
