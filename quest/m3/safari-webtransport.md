# [S] Safari WebTransport returns when WebKit refills its windows

## Goal

Once a shipping Safari refills MAX_DATA and MAX_STREAMS credit, js/net stops
routing WebKit browsers to the WebSocket and qmux fallback: the gate admits the
fixed version and later, and a Safari watcher plays for hours over WebTransport
as Chrome does. The media harness also publishes from Safari, covering the
worker capture path no Linux Playwright engine takes. #2388 has been WebKit
tracking since PR #3345 and closes with this.

## Plan

- Name the exact Safari and iOS or iPadOS releases that ship the fix in the
  quest and in the `browser.ts` comment, then gate on them for Safari and for
  the iOS WebKit browsers, keeping older versions on the fallback. As of
  2026-10-22 UTC, [WebKit bug 319818](https://bugs.webkit.org/show_bug.cgi?id=319818)
  is still NEW with no resolution; its 2026-10-18 comment reproduces the
  failure on a newer Safari. No fixed shipping version is established.
- Before flipping, rerun the raw WebTransport reproduction from #2388 (about
  7,600 eleven-byte unidirectional streams, and about 16 MiB of data) on the
  fixed Safari, then a watch longer than two minutes in the QA harness on
  Safari for macOS and on Chrome, Firefox and Edge for iOS, with desktop Chrome
  and Firefox as controls.
- Cover the worker capture path in `js/publish/src/video/processor.ts` from a
  Safari publisher in the media harness (moved here from
  [the Firefox publishing quest](/quest/m2/publish-firefox.md) on 2026-10-30):
  frame transfer, source replacement, cancellation with a read pending, and
  that every `VideoFrame` is closed. Neither Firefox 157 nor Chromium 156 in
  Playwright has a worker `MediaStreamTrackProcessor` or transfers a
  `MediaStreamTrack`; the harness's capability probe already reports the path
  taken, and the fixture must take it.
- Update `doc/lib/js/index.md`.
- `parseUserAgent` in `js/net/src/connection/browser.ts` already returns
  the Safari and iOS versions (bowser left in
  [#5267](https://github.com/moq-dev/moq/pull/5267)); replace the
  unconditional `webkit || ios` refusal with a version check.

## Required

- [Safari ships the WebKit 319818 fix](/quest/m3/webkit-319818.md) - the release to gate on

## Closes

- [#2388](https://github.com/moq-dev/moq/issues/2388) - close this issue when the quest finishes
