# [S] The TS export release clock raises no false alarms

## Goal

The TS export's jitter buffer reports a jitter leap or an `out_of_tolerance`
drift only when the source clock actually misbehaves. The recovery arms of
[moq-dev/moq#6139](https://github.com/moq-dev/moq/pull/6139) run clean at a
500 ms export delay and across the long arm, and assert it.

## Plan

Decided 2026-10-21 with the maintainer: m1, under TS reliability. Both seen
in #6139, whose arms stayed at 1 s (`export ts --delay`'s default) to land:

- **Jitter leap at 500 ms.** A declared PCR discontinuity rewinding 475 ms
  after a skipped group reads as a jitter leap. A declared discontinuity is
  a new timeline, not jitter.
- **Drift estimate.** In the long arm the rate estimate read 5,551 ppm once,
  at about 95 s, counting one `out_of_tolerance` against an exact source
  clock. The final estimate was 0.00015 ppm and output was unaffected, but
  anyone monitoring the counter sees a false alarm. Find which sample
  produced it before changing the estimator.

Reproduce each first, then make the arms assert both counters stay zero.

Public API: none. Wire: none.

## Related

- [Arrival stamps](/quest/m1/ts-reliability/arrival-stamps.md) - also changes when the export releases a frame
