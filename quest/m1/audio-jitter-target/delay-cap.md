# [XS] A publisher's advertised delay cannot hold a browser viewer without bound

## Goal

A publisher that advertises a huge `jitter` or catalog `delay` cannot hold a
`<moq-watch>` viewer's playback, or the media it buffers, past a fixed
ceiling. The browser caps the resolved `"auto"` delay at 10 s, so a
misbehaving publisher costs its viewers bounded latency and memory, as
AGENTS.md requires.

## Plan

Decided 2026-11-11 (maintainer, /quest-complete, from
[moq-dev/moq#8532](https://github.com/moq-dev/moq/pull/8532)): ✅ plan it
in m1 (rejected: m3 or later; not tracking it).

#8532 capped `moq play`'s clock at `moq_audio::playback::Input::LATENCY_MAX`
(10 s), which also bounds the catalog `delay` and `--delay` there. The
browser had no cap. In `"auto"`, `js/watch` `Sync#runDelay` holds the deepest
part's target, so an hour of advertised delay delayed playback by an hour:
- Audio's target is `max(measured, jitter) + frame + delay`
  (`js/watch/src/audio/latency.ts` `target`).
- Video's target is `spread + delay`, floored by the rendition's `jitter`
  (`js/watch/src/video/decoder.ts`).

The subscription's max delay (`delay + lookahead`) follows the same delay,
so the container consumer keeps that much encoded media.

Recommendations, as implemented:

- The resolved `"auto"` delay is capped once in `Sync#runDelay` at an internal
  constant equal to `LATENCY_MAX`, rather than capping each term, so audio and
  video stay on one clock.
- A fixed delay is left alone: the viewer chose it, not the publisher.
- A catalog advertising an hour of `jitter` and `delay` resolves to the cap.

Findings:

- The premise that `moq play` caps at the same 10 s does not hold. It caps
  each term on its own, the held jitter target and the catalog delay, each at
  `LATENCY_MAX`, so its audio target can reach 20 s
  (`rs/moq-cli/src/play/media.rs`). `moq_mux::sync` caps nothing itself: its
  delay is the largest target its callers set.
- The constants are pinned together in one place only: the `play` feature's
  test in `rs/moq-cli/src/play/args.rs` pins `DELAY_MAX` to
  `moq_audio::playback::Input::LATENCY_MAX`. The browser constant is pinned to
  it by its comment alone, since no test builds both.
- Catalog `warmup` is added to the subscription's max delay in the browser
  with no cap (`js/watch/src/audio/decoder.ts`, `js/watch/src/video/decoder.ts`).
  It is not one of the `jitter` or `delay` fields the Goal names.

Public API: none. The constant stays internal to `@moq/watch`. Wire: none.

## Waiting

- Maintainer: pick the ceiling for `moq play`'s audio target, which reaches 20 s
  while the browser stops at 10 s. Options: (a) cap `moq play`'s target sum at
  `LATENCY_MAX` too, recommended, so one ceiling holds everywhere; (b) raise
  the browser to 20 s to match `moq play`'s worst case; (c) keep the mismatch.
  Check: the answer is recorded here, and a follow-up quest exists for `moq play`.
- Maintainer: pick how a catalog `warmup` above 10 s is bounded. Options: (a)
  clamp it at `LATENCY_MAX` where the browser reads it, and in `rs/hang`'s
  playout for parity, recommended, since only the publisher's own picture
  suffers; (b) reject a rendition whose `warmup` exceeds it, which fails loud
  but drops that rendition; (c) leave it unbounded, which misses this quest's
  Goal for `warmup`. Check: the answer is recorded here, and a follow-up quest
  exists for it.
