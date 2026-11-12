# [XS] A publisher's advertised delay cannot hold a browser viewer without bound

## Goal

A publisher that advertises a huge `jitter` or catalog `delay` cannot hold a
`<moq-watch>` viewer's playback, or the media it buffers, past a fixed
ceiling. The browser caps the `"auto"` delay at the same 10 s `moq play`
uses, so a misbehaving publisher costs its viewers bounded latency and
memory, as AGENTS.md requires.

## Plan

Decided 2026-11-11 (maintainer, /quest-complete, from
[moq-dev/moq#8532](https://github.com/moq-dev/moq/pull/8532)): ✅ plan it
in m1 (rejected: m3 or later; not tracking it).

#8532 capped `moq play`'s clock at `moq_audio::playback::Input::LATENCY_MAX`
(10 s), which also bounds the catalog `delay` and `--delay` there. The
browser has no cap. In `"auto"`, `js/watch` `Sync#runDelay` holds the deepest
part's target, so an hour of advertised delay delays playback by an hour:
- Audio's target is `max(measured, jitter) + frame + delay`
  (`js/watch/src/audio/latency.ts` `target`).
- Video's target is `spread + delay`, floored by the rendition's `jitter`
  (`js/watch/src/video/decoder.ts`).

The subscription's max delay (`delay + lookahead`) follows the same delay,
so the container consumer keeps that much encoded media.

Recommendations:

- Cap the resolved `"auto"` delay in `Sync#runDelay` at one exported
  constant equal to `LATENCY_MAX`, rather than capping each term, so audio
  and video stay on one clock.
- Leave a fixed delay alone: the viewer chose it, not the publisher.
- Test that a catalog advertising an hour of `jitter` and `delay` resolves
  to the cap.
- Check that `moq_mux::sync` and `moq play` cap the same `"auto"` delay, and
  say where the two constants are pinned together.

Public API: possibly one exported constant in `@moq/watch`. Wire: none.
