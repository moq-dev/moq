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

Implementation discovery, 2026-11-03: the clock owner and reader timeline
contract still need a maintainer decision. Today `Bus::read` advances every
source, and clones read that same mix. Studio's
[`Engine::run_audio`](https://github.com/moq-dev/studio/blob/cdc65e0de699964df01ab5bc9f9b5d7225ff13d8/rs/moq-studio/src/engine.rs#L501)
advances it and encodes on the same task, stamping frames from its local
`written` counter. Moving only the samples into independent reader queues
would leave mixing stalled behind encoding, or let a skipped block be
published at the wrong time. The output contract must expose where the
returned samples belong on the shared audio timeline.

Recommended shape, pending that decision: keep the caller's audio clock;
split the cloneable routing/source handle from one exclusive mixer driver
that advances a frame count. Each mix offers independent consumers whose
reads report the starting frame position, including after their own skips.
Studio drives the mixer separately from encoding and maps those positions
to its catalog clock. A library-owned clock is the alternative below.
These are proposed roles, not settled exported names or signatures.

Inspected shared capture at
[`accf99b`](https://github.com/moq-dev/moq/commit/accf99b66e5e346af50a53415cf5f337eeeded7b)
in [moq-dev/moq#7706](https://github.com/moq-dev/moq/pull/7706):
`Bus::capture(capture::Consumer, Strip)` constructs a channel, and its driver
only reads that consumer. Routing can stay downstream of that channel and
propagate capture errors to its caller as before. This needs no decision
about retaining encoded history after terminal source failure; that remains
with [moq-dev/moq#7725](https://github.com/moq-dev/moq/pull/7725).

Studio already has a
[pin quest](https://github.com/moq-dev/studio/blob/cdc65e0de699964df01ab5bc9f9b5d7225ff13d8/quest/m0/pin-audio-mix-router.md).
Its repository rules require the moq change to land first, then a separate
gitlink/API migration PR. Prepare that consumer update against the chosen
API, but keep it a draft until the moq commit is on `main`.

## Waiting

- Maintainer: choose clock ownership and the mix reader contract. (1) Keep
  a caller-driven exclusive mixer and independent readers returning sample
  positions (recommended: preserves studio's sample-exact program clock,
  permits audio-device clocks, and separates output stalls); (2) give
  moq-audio a library-driven clock and return positioned blocks to readers
  (simpler application driver, but adds clock/epoch setup and scheduling to
  the library). Confirm the roles before choosing exported names and
  implementing the router, tests, axis benchmarks, and studio migration.

## Related

- [moq-audio filter chain](/quest/m3/audio-filters.md) - runs before a source reaches the router
