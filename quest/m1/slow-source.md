# [S] A source the output cannot follow fails the export promptly

## Goal

An export whose source arrives further off real time than the output's
clock may follow ends with an error naming the measured rate, once it has
used half its delay. It no longer drops nearly every frame for hours with
only per-frame "missed its deadline" warnings. A short stall, such as a
reconnect, still drops late frames and recovers.

## Plan

Found on #5435: before #5292, `moq import` fed a 20 s TS clip about 22%
slow on CI's debug build. The TS export measured -222,000 ppm, dropped
1,273 frames as late, and exited clean with 3.2 s captured.

`jitter::Buffer` (`rs/moq-mux/src/jitter.rs`) already bails when the source
is out of tolerance, but only once the steered drift sits at `MAX_DRIFT`.
At `MAX_SLEW` (0.075 Hz/s of 27 MHz) the steered clock needs about 3 hours to
reach 30 ppm, so a grossly slow source never trips it. The rate itself is
known within a few `STEER` steps.

Decided with the maintainer on 2026-10-13:

- Fail loud, not just warn: a source the output can never follow is refused.
- The check lives in the shared jitter buffer, so TS, FLV, and MKV export
  fail the same way. Audio decode is out of scope: `moq-audio` sizes its
  delay with its own estimator and doesn't steer a clock.
- The rule is the existing one minus "the steered clock is at its limit":
  the measured rate is past `MAX_DRIFT` and half the delay is gone. No new
  constant. Rejected: a run of a delay's worth of consecutive drops, a new
  heuristic beside the rate the buffer already measures.
- Prove with mocked time that a stall shorter than half `RATE_WINDOW` (the
  envelope's own bound) doesn't trip it, including an early stall when only
  a few floors exist. If the envelope can't tell the two apart that early,
  fix the measurement rather than adding a minimum span.
- Regressions: a source 22% slow fails within a few seconds of decode time
  with the rate in the error; one 400 ppm off (the existing counted case)
  fails once half the delay is gone; sources within 30 ppm never fail.
- Update any doc or comment that says late frames only drop, inline.

Public API: none; an export that silently degraded now returns an error.
Wire: none.

## Related

- [Live TS clock validation](/quest/m1/ts-live-clock.md) - a local run measured -1382 ppm and exited clean; with this rule it fails loudly, which is evidence for that quest, not a blocker
- [#5435](https://github.com/moq-dev/moq/pull/5435) - the duration-fidelity investigation that found this
