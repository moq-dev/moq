# [M] Browser publishing paths under test

## Goal

The interop media harness publishes from Firefox as well as Chromium, so the
production capture paths Chromium never takes run in CI: the worker
`MediaStreamTrackProcessor` and the video-element fallback.

## Plan

Decided 2026-10-22 from the repository improvement audit. The harness
(`test/interop/clients/js/media.ts`) always launches Chromium for the
publisher, and its capability probe requires a window-level
`MediaStreamTrackProcessor`. `js/publish/src/video/processor.ts` uses that
only on Chromium; elsewhere it spawns a capture worker and falls back to a
video element.

- Add a selectable publisher engine to the harness, and make the capability
  probe check the path production would take (worker support, not the window
  global).
- Cover the worker path end to end in Firefox: frame transfer, source
  replacement, cancellation with a read pending, and that every `VideoFrame`
  is closed.
- Cover the video-element fallback by forcing it (for example, a worker
  refused by CSP), in either engine.
- Firefox only, on Linux. Playwright WebKit and a macOS Safari lane stay with
  the m3 Safari work; Playwright's WebKit is not Safari.
- The test-runner option is the only interface change; document it in
  `test/interop/README.md`.

## Related

- [Safari WebTransport](/quest/m3/safari-webtransport.md) - the later Safari lane
