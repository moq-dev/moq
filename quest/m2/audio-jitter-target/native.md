# [M] rs/moq-audio: a measured jitter buffer, not just an upper bound

## Goal

Native playback holds a jitter buffer sized by the algorithm from
[Spec](/quest/m2/audio-jitter-target/spec.md), and passes the same conformance
vector as the browser on the same trace. `moq play` on a path with uneven
arrivals plays through a flush without underrunning, where today it has nothing
to absorb one.

Boundaries: no time-stretching and no loss concealment, matching the browser.
Video keeps its own path.

## Plan

`rs/moq-audio` has no jitter buffer at all. `decode::Config`
(`rs/moq-audio/src/decode/decoder.rs:60-80`) carries `max_age`, how far
playback may drift from the live edge before skipping a stalled group, applied
to the subscription and clamped to the track's retention in
`decode/consumer.rs:113`, and `start`, where to begin on a track that already
holds groups. Neither adds latency: the consumer skips only when newer data is
already that far ahead.

- Measure arrivals at the same point the browser does, on the container
  consumer before the age budget can skip a group, so both languages estimate
  from the same observation.
- Add the knob to `decode::Config`, additive on the `#[non_exhaustive]` struct,
  so it targets `main`. The spec quest settles the final name and shape before
  this implementation starts; `delay` is the recommendation, matching the
  browser while `max_age` remains the live-edge skip budget. Do not reopen or
  independently choose the public spelling in this implementation.
- An explicit `Some(d)` fixes the receiver's jitter target and disables
  adaptation, matching the browser's numeric delay setting. `None` selects
  the shared automatic estimator. Publisher-declared buffering remains a
  separate contribution, with the same composition in both implementations.
  The spec must define precedence against `max_age` and retention; refuse an
  impossible explicit target rather than silently changing it.
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

## Required

- [Spec](/quest/m2/audio-jitter-target/spec.md) - the algorithm this implements
