# [M] A TS export never exits on start-up latency

## Goal

`moq export ts` never exits with "the source's clock runs ... ppm off ours"
for a source within the 30 ppm tolerance, however its latency moves while
the export starts (queueing that builds, or a backlog that drains). A source
far off real time (the 220,000 ppm case) still fails within seconds. `just
test ts --full` stops failing this way.

## Plan

Reported in [#5564](https://github.com/moq-dev/moq/issues/5564): 2 of 10 local
`--full` runs on `main` (3abf0161c) and 1 of 51 CI runs exited within 30 s.
The first `TS export release clock` lines read wild drift (-155315, +32045
ppm), and passing runs log none.

From reading the code on 5beead687 (not reproduced): `Buffer::steer`
(`rs/moq-mux/src/jitter.rs`) takes one floor every 2 s, fits an upper convex
hull, and reads both the rate and `gap` (extrapolated to `now`) from it. A
rate needs only two floors, with no minimum span or significance gate. `lost` is measured against `startup`, the gap
at the second floor. The export bails when `|slow| > 30 ppm` and `lost`
passes half the delay (450 ms at `--full`'s 900 ms). So a latency ramp after
the second floor, on a 0 ppm source, can end the export. Before #5490
(41f3087e3) the exit also required the steered drift to reach its limit,
which made start-up noise unable to end it; `--full` (#5516) landed half an
hour after #5490.

Decided 2026-10-15 (maintainer):

- Reproduce first with the issue's loop, and record which floors skew the
  envelope, before choosing a mechanism.
- Fix the measurement; do not add a minimum span or sample count. That keeps
  the decision recorded when #5490 removed `quest/m1/slow-source.md`.
- Never widen the 30 ppm tolerance, `--delay`, or the `lost` threshold.
- Candidate mechanisms, for the implementer to pick from the evidence:
  judge `lost` from observed floors instead of the envelope extrapolated to
  `now`; exit only when the rate's uncertainty excludes 30 ppm; keep
  `startup` floating until the rate is significant; and treat a fast
  source's positive `lost` (a growing buffer) differently from loss.
- Mocked-time regressions in `jitter.rs`, each failing without the fix: a
  0 ppm source whose queueing builds 300 to 500 ms over 5 to 20 s and then
  holds, the same draining, and both again with three tracks shaped like
  `--full` (video sent up to 0.65 s ahead).
  `a_source_far_off_real_time_fails_within_seconds` keeps passing.

Public API: none. Wire: none.

## Closes

- [#5564](https://github.com/moq-dev/moq/issues/5564) - flaky: `just test ts --full` exits on the release clock's first rate estimates

## Related

- [Live TS clock validation](/quest/m1/ts-live-clock.md) - the grader's PCR-rate verdict on `--live`, a different layer
- [TS decoded content](/quest/m1/test-flakes-2/ts-video-content.md) - the other TS CI flake, on content loss
