# [M] rs/moq-audio: a measured jitter buffer, not just an upper bound

## Goal

Native playback holds a jitter buffer sized by the algorithm in
`doc/concept/audio-jitter.md`, and passes the same conformance corpus as the
browser on the same trace. `moq play` on a path with uneven
arrivals plays through a flush without underrunning, where today it has nothing
to absorb one.

Boundaries: no time-stretching and no loss concealment, matching the browser.
Video keeps its own path.

## Plan

`rs/moq-audio` has no jitter buffer at all. `decode::Options`
(`rs/moq-audio/src/decode/consumer.rs`) carries `max_age`, how far
playback may drift from the live edge before skipping a stalled group, applied
to the subscription and clamped to the track's retention, and `start`, where
to begin on a track that already holds groups; `decode::Config` selects the
backend only. Neither adds latency: the consumer skips only when newer data is
already that far ahead.

- Measure arrivals at the same point the browser does, on the container
  consumer before the age budget can skip a group, so both languages estimate
  from the same observation. That is `container::Consumer::read()`, awaited at
  `rs/moq-audio/src/decode/consumer.rs:224`, which is one-to-one with arrivals
  and reads no clock today.
- Add the knob to `decode::Options`, additive on the `#[non_exhaustive]` struct,
  so it targets `main`. The name is settled: `delay`, matching the browser,
  while `max_age` remains the live-edge skip budget. Do not reopen it.
- An explicit `Some(d)` fixes the receiver's jitter target and disables
  adaptation, matching the browser's numeric delay setting. `None` selects
  the shared automatic estimator. Publisher-declared buffering remains a
  separate contribution, floored rather than added, with the same composition in
  both implementations. Refuse an explicit target that is negative or beyond the
  track's retention rather than silently changing it.
- The estimator itself is internal. Only the config field and whatever the
  playback path needs to report its current target become public, and each one
  is argued for rather than exposed by default.
- `rs/moq-cli` `play` gets the matching knob, and `doc/bin/cli.md` follows. Check
  the examples against `--help`.
- The playback sink holds the target the way the browser's rings do: slack above
  it, land on it when skipping, re-stall when dry so the next insert refills to
  the target rather than playing on an empty cushion.
- Tests: the conformance corpus at `doc/concept/audio-jitter/*.json`, read
  directly from Rust, plus a decode-path test that replays the same trimmed
  arrival trace the browser replays and asserts the same target series.
- The frame duration comes from the codec. `opus::packet_samples` already
  parses the TOC byte in `rs/moq-mux/src/codec/opus/mod.rs`, but it is
  `pub(crate)`, so reaching it means widening its visibility or moving it.

