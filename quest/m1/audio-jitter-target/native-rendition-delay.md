# [S] moq play holds each rendition's catalog delay

## Goal

`moq play` sets its playout from the catalog as the browser does:
`max over subscribed renditions of (delay + target)`
(`doc/concept/audio-jitter.md`), so a rendition whose publisher states a
`delay`, such as a slow video encoder, plays in sync with audio instead of
behind it.

## Plan

Planned 2026-11-08 (maintainer, approved follow-up of
[moq-dev/moq#8272](https://github.com/moq-dev/moq/pull/8272), which gives
moq-mux's playout sync per-track targets and the largest-target delay). Milestone:
m1, an additive fix to shipped playback; start after #8272 merges.

On `main`, nothing under `rs/moq-cli/src/play/` reads a rendition's
`delay`; the browser does (`js/watch/src/video/playhead.ts`,
`js/watch/src/audio/decoder.ts`). With #8272, each track's target is what it
holds through `Track::set_target`: add the rendition's catalog `delay` to it
(video holds `delay` alone, since it has no estimator), recompute when a
rendition is subscribed, dropped, or its catalog entry rises, and test that a
200 ms video `delay` delays audio to match. A fixed `--delay` stays fixed.

Public API: none. Wire: none.
