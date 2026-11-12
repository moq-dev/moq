# [S] A publisher's advertised delay cannot hold a browser viewer without bound

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

Decided 2026-11-12 (coordinator, on the maintainer's behalf while away, from
the PR's recommendations):

- ✅ (a) cap `moq play`'s audio target sum, the held jitter target plus the
  catalog delay, at `LATENCY_MAX` too, so one 10 s ceiling holds everywhere
  (rejected: (b) raise the browser to 20 s; (c) keep the mismatch).
- ✅ (a) clamp catalog `warmup` at `LATENCY_MAX` where the browser reads it,
  and in Rust for parity: `moq_mux::container::Playout`, `moq_video`'s
  decode `Consumer`, and `moq play`'s video refresh, all through one
  `moq_mux::container::WARMUP_MAX` (rejected: (b) reject the rendition; (c)
  leave unbounded).

Findings:

- The constants are pinned together in one place only: the `play` feature's
  test in `rs/moq-cli/src/play/args.rs` pins `DELAY_MAX` to
  `moq_audio::playback::Input::LATENCY_MAX`. The browser's `DELAY_MAX` and
  `moq-mux`'s `WARMUP_MAX` are pinned to it by comment alone, since no test
  builds them together.
- The warmup cap also bounds each decoder's join trim (the browser's audio
  terminal, `moq-audio`'s decoder, `moq-gst`'s Opus clip): the subscription
  reaches back only the ceiling, so trimming further would hold playback
  silent past it.

Public API: `moq_mux::container::WARMUP_MAX` (10 s) is exported so every
Rust reader of catalog `warmup` shares one ceiling; the browser's `DELAY_MAX`
stays internal. Wire: none.
