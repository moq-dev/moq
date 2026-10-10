# [S] TS export interleaves each slot so no PID overruns its transport buffer

## Goal

When the multiplex runs faster than a video PID's Rx, `moq export ts` still
keeps that PID's 512-byte transport buffer (ISO 13818-1 2.4.2.3) from
overflowing. The schedule already holds every PID to Rx per slot; the layout
within the slot has to spread the packets as evenly as that budget assumes.

## Plan

Decided (2026-10-10), importing #5142. `Slot::layout`
(`rs/moq-mux/src/container/ts/schedule.rs`) keys each packet by its own PID's
count and only then spreads the nulls uniformly across the media. PIDs with
equal counts therefore land at the same positions, so video, at 86 % of a
slot, runs 12-13 packets back to back and overruns TB on a 25 Mb/s CBR feed
whose video HRD is 18 Mb/s.

- Lay the slot out by smooth weighted round-robin over every position, with
  each PID and the nulls weighted by their count, and each PID keeping its
  order. It replaces both the key sort and the null spread. Tables keep their
  rule of going just ahead of the packet after them. The reporter measured a
  459 B peak on the failing slot, down from 1,067 B. SWRR was chosen over
  staggering the existing keys (more special cases) and over placing packets
  against a simulated TB (it ties the layout to the buffer model).
- When the PCR PID is the video PID, the clock packet counts against that
  PID's per-slot budget (`Buffer::per_slot`). Revisit whether the "less one"
  slack is still needed once the layout is even.
- Tests: the issue's full-slot TB unit test is the regression test. The CI'd
  `just test ts` harness also gains an arm where a CBR multiplex runs above the
  video's HRD rate, which no clip covers today, graded by the strict T-STD
  check.

## Closes

- [#5142](https://github.com/moq-dev/moq/issues/5142) - the slot layout overflows video's transport buffer when the multiplex runs faster than its Rx
