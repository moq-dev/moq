# [M] Browser publishing paths under test

## Goal

Nightly's media harness publishes from Firefox as well as Chromium and passes,
so the `<video>` capture fallback Chromium never takes runs in CI.

## Plan

Decided 2026-10-22 from the repository improvement audit. The harness takes
`--media-publisher firefox`, its capability probe reports the capture path
production takes, and the fixture must take that path. Firefox 157 in
Playwright 1.64 has no `MediaStreamTrackProcessor`, in a worker or on the
window, and cannot transfer a `MediaStreamTrack`, so it always takes the
`<video>` fallback. Chromium has no worker processor either, so no Playwright
engine on Linux reaches the worker path; its coverage moved to the m3 Safari
quest (decided 2026-10-30, see Related).

What remains before the lane joins Nightly's `media-firefox` job:

- Firefox publishing skews audio about one 200 ms tone step ahead of the
  picture under load, failing `audio/video sync` in the pause, disable,
  republish, and detach cases, and late joins and rejoins land 1.8 s behind live
  against the 1.5 s bound. Chromium forced onto the same `<video>` fallback (its
  window `MediaStreamTrackProcessor` deleted) stays in sync on the same host,
  and on an idle host Firefox's `<video>` presentation lag (about 20 ms) and
  audio worklet stamps (within 10 ms) explain neither. Root-cause it in
  `@moq/publish` or the fixture; Firefox's `requestVideoFrameCallback` reports
  no `captureTime`, so the fallback stamps frames at presentation.
- The capture case needs a fake camera in Firefox. The
  `media.navigator.streams.fake` pref alone still reports `NotFoundError`.
- Then add a Firefox-publisher step to `media-firefox` in
  `.github/workflows/nightly.yml`.

Playwright WebKit and a macOS Safari lane stay with the m3 Safari work;
Playwright's WebKit is not Safari.

## Related

- [Safari WebTransport](/quest/m3/safari-webtransport.md) - the later Safari lane, the only engine with the worker capture path
