# [M] rs/moq-audio: a measured jitter buffer, not just an upper bound

## Goal

Native playback holds a jitter buffer sized by the algorithm from
[Spec](/quest/m0/audio-jitter-target/spec.md), and passes the same conformance
vector as the browser on the same trace. `moq play` on a path with uneven
arrivals plays through a flush without underrunning, where today it has nothing
to absorb one.

Boundaries: no time-stretching and no loss concealment, matching the browser.
Video keeps its own path.

## Plan

`rs/moq-audio` has no jitter buffer at all. `decode::Config` carries only
`latency_max`, an upper bound before skipping a stalled group, and its doc
comment already promises the companion:

> The `_max` suffix is a reminder that we never *add* latency here: the consumer
> skips only when newer data is already this far ahead. A companion
> `latency_min` for jitter-buffer padding will land in a follow-up.

- Measure arrivals at the same point the browser does, on the container
  consumer before the age budget can skip a group, so both languages estimate
  from the same observation.
- Add `latency_min` to `decode::Config`: `None` measures and adapts, `Some`
  pins a floor the way a fixed delay does in the browser. This is additive on a
  `#[non_exhaustive]` struct, so it targets `main`.
- The estimator itself is internal. Only the config field and whatever the
  playback path needs to report its current target become public, and each one
  is argued for rather than exposed by default.
- `rs/moq-cli` `play` gets the matching knob, and `doc/bin/cli.md` follows. Check
  the examples against `--help`.
- The playback sink holds the target the way the browser's rings do: slack above
  it, land on it when skipping, re-stall when dry so the next insert refills to
  the target rather than playing on an empty cushion.
- Tests: the conformance vector from the spec, plus a decode-path test that
  replays the same trimmed arrival trace the browser replays and asserts the
  same target series.

`latency_min` is the natural name only because `latency_max` already exists. If
it reads wrong next to a `Duration` that is a floor rather than a budget,
propose alternatives with a recommendation rather than shipping it.

## Required

- [Spec](/quest/m0/audio-jitter-target/spec.md) - the algorithm this implements
