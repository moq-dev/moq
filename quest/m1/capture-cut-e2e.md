# [S] The capture cut throttle is tested end to end

## Goal

A test drives the real capture loop in `moq-video` (`Control::cut()` through
to the published groups) with an encoder that places its own GOP keyframes,
and asserts where the groups open. A cut that a cadence keyframe serves forces
no extra keyframe. A cut right after a cadence keyframe waits out the spacing
window. The test runs in the per-PR `capture-test` pass without depending on
wall-clock timing.

## Plan

- Today `Cuts` and the backends' keyframe flags are unit-tested apart. Nothing
  checks the loop's wiring between them, which is where a regression would
  hide: a missed `keyframe()` call, or a flag read from the wrong unit.
- The existing clock fixtures already feed the loop synthetic frames with
  device timestamps. The throttle decides on those media timestamps, so the
  missing piece is the loop's own timers (stall ticks, read timeouts). Prefer
  a paused tokio clock or an injected clock over sleeps.
- A short-GOP software encoder or a test backend both work, as long as the
  cadence is known. Assert on group boundaries read back from the track, not
  on internal state.

## Related

- [#4295](https://github.com/moq-dev/moq/pull/4295) - the keyframe flag and throttle this covers
