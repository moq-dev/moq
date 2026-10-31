# [S] An audio-only broadcast that can't play hides the play button

## Goal

`<moq-watch-ui>`'s center play button hides when an audio-only broadcast's
audio decoder is unsupported, as it already does for a video error, and a
real-browser test proves `AudioDecoder.isConfigSupported` rejects an
unsupported codec and the UI shows the notice.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7359](https://github.com/moq-dev/moq/pull/7359): m2, as
recommended. Start after [moq-dev/moq#7359](https://github.com/moq-dev/moq/pull/7359) (open) lands; it adds the shared support
probe and the indicator.

- `js/watch/src/ui/components/center-play.ts:16` reads only
  `watch.video.source.out.error`. Apply "no video, so audio": with no video
  rendition, hide on the audio source's error.
- [moq-dev/moq#7359](https://github.com/moq-dev/moq/pull/7359) ran nothing in a browser. Add a browser test (in
  `test/interop/clients/js/`, beside `harness.browser.ts`) that a codec the browser refuses reaches the
  indicator through `AudioDecoder.isConfigSupported`, and that the play
  button hides for audio-only.

Public API: none. Wire: none.

## Related

- [Watch support probe](/quest/m2/watch-support-probe.md) - the shared probe
