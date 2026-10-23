# [M] The receiver owns its decoded audio memory

## Goal

A catalog's advertised `jitter` or `delay`, however large, cannot make a
viewer allocate decoded PCM in proportion to it. The decoded ring is sized
by the receiver, and a deep playout target is reached by holding encoded
frames upstream, so memory follows bytes actually received. A publisher that
genuinely advertises seconds of flush delay still plays smoothly at that delay.

## Plan

Found in a 2026-10-22 media audit at
[main 1f5caa7](https://github.com/moq-dev/moq/commit/1f5caa7026bcfd8a2684da3712b384b5f74a7496)
and confirmed by source at
[main d4cdfe4](https://github.com/moq-dev/moq/commit/d4cdfe4368b17cbdf7d2a0c43d24e364a9626480).
`target()` in `js/watch/src/audio/latency.ts` floors the measured term by the
rendition's advertised `jitter` and adds its catalog `delay`; both are any
u53 in `js/hang/src/catalog/audio.ts`. `ringSamples` turns that target into
`latencySamples`, and the render worklet's `AudioRingBuffer` allocates
`latencySamples * 2` frames per channel of float PCM. The audit computed 512 MiB for a 48 kHz stereo rendition advertising ten minutes
of jitter and 4 GiB for one hour, without allocating them. The estimator's 2 s
ceiling bounds only the measured term.

Decided 2026-10-22 (maintainer): hold the lookahead encoded rather than refuse
a rendition whose hints exceed a ceiling. The existing buffered mode already
keeps the lookahead above the floor as encoded frames behind `Backpressure`;
generalize that so the ring's capacity comes from the receiver's own budget
(a render-quantum floor plus decode-ahead slack), independent of the target,
and the target delay is enforced by when encoded frames are released to the
decoder. Settle while building:

- The subscription's max delay must cover the target, or the transport drops
  the frames the target waits for (`AUTO_MAX_DELAY` is the current floor).
  Encoded retention then sits in the transport's group cache, which is the
  receiver's existing flow-controlled store.
- Whether the non-buffered path survives once both paths gate on timestamps,
  or folds into the buffered one. Prefer one path.

Tests: a rendition advertising an hour of jitter allocates a ring of the
receiver's fixed size and plays at the advertised delay on a stubbed clock;
the replay harness rows keep their budgets. No wire change;
any `js/watch` API change goes in its `MIGRATE.md`.

## Related

- [Buffered backpressure timestamps](/quest/m1/audio-backpressure-timestamps.md) - the 32-bit truncation in the same `Backpressure`, landing first
- [Audio jitter target](/quest/m1/audio-jitter-target/README.md) - the playout target this sizes memory for
