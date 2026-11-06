# [S] An audio-only broadcast that can't play hides its play button

## Goal

`js/watch`'s `center-play` hides for an audio-only broadcast that can't play,
as it does when video has an error. The unsupported notice from
[moq-dev/moq#7359](https://github.com/moq-dev/moq/pull/7359) then stands alone.

## Plan

Planned 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-complete run). Use #7359's rule: a broadcast with no video renditions counts as audio.

- `js/watch/src/ui/components/center-play.ts:16` reads only
  `watch.video.source.out.error`; with no video rendition, hide on the audio
  source's error.
- [moq-dev/moq#7359](https://github.com/moq-dev/moq/pull/7359) ran nothing in
  a browser. Add a browser test (in `test/interop/clients/js/`, beside
  `harness.browser.ts`) that a codec the browser refuses reaches the indicator
  through `AudioDecoder.isConfigSupported`, and that the play button hides for
  audio-only. Folded in 2026-11-02 from the duplicate quest
  [moq-dev/moq#7440](https://github.com/moq-dev/moq/pull/7440) planned from the
  same PR (user, 2026-10-31: m2).

Public API: none. Wire: none.
