# [XS] moq-shaper step test on mock time

## Goal

`moq-shaper`'s `a_step_gets_worse_part_way_through` asserts on a
paused or mock clock, not wall-clock latency, so a loaded runner cannot fail it.

## Plan

It failed once under a full-suite `just check --all` during the
[#4720](https://github.com/moq-dev/moq/pull/4720) merge (median latency
61.8ms before the 150ms step, 62.0ms after, against an expected +30ms), and
passed alone 3 times. Drive the shaper's delays from a paused Tokio clock or
the crate's own time source, then assert the step on that clock.

Public API: none. Wire: none.
