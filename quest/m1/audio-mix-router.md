# [L] moq-audio routes sources into several mixes

## Goal

moq-audio's program `Bus` grows into a router: each source routes to any
subset of several mixes, each mix reads on the audio clock with its own
level meter, a source carries a sync offset, and a mix can be a mix-minus
(every routed source but one). [moq-dev/studio's audio mixes](https://github.com/moq-dev/studio/blob/main/quest/m0/mixes.md)
build on it: a stream mix that drops the music, a recording mix that keeps
it, a headphone monitor, and a mix-minus per guest.

## Plan

Decided 2026-11-02 (maintainer, from moq-dev/studio's mixes): the routing
lives in moq-audio rather than the studio running one `Bus` per mix, so
routing, mix-minus, and offsets have one implementation with its own tests.

- **Cost follows routing:** a mix costs only what is routed into it, never a
  scan of every source. Benchmark source count against mix count, including
  sparse routing and a mix-minus per source, so either axis shows as a slope.
- **Never blocks:** the mixer runs on the audio clock and never waits on a
  slow reader; a reader that falls behind drops its own audio, not the bus's
  or another mix's.
- **Sync offset:** a per-source delay, so a camera that lags its microphone
  lines up. It delays that source in every mix it feeds.
- **Inputs:** a source's channel takes PCM as today, including a consumer of
  `capture::Producer`; gain and
  mute stay on its `Strip`.
- **Breaking:** the single-mix `Bus` API changes shape. Its only consumer
  outside moq-audio is the studio, which updates with it; the migration is
  written in the changelog, and it lands before the next cut.

## Related

- [moq-audio filter chain](/quest/m3/audio-filters.md) - runs before a source reaches the router
