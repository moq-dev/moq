# [XS] Safari WebTransport returns when WebKit refills its windows

## Goal

Once a shipping Safari refills MAX_DATA and MAX_STREAMS credit, js/net stops
routing WebKit browsers to the WebSocket and qmux fallback: the gate admits the
fixed version and later, and a Safari watcher plays for hours over WebTransport
as Chrome does. #2388 has been WebKit tracking since PR #3345 and closes with
this.

## Plan

- Name the exact Safari and iOS or iPadOS releases that ship the fix in the
  quest and in the `browser.ts` comment, then gate on them for Safari and for
  the iOS WebKit browsers, keeping older versions on the fallback. As of
  2026-09-30 the bug is still NEW with no fix released.
- Before flipping, rerun the raw WebTransport reproduction from #2388 (about
  7,600 eleven-byte unidirectional streams, and about 16 MiB of data) on the
  fixed Safari, then a watch longer than two minutes in the QA harness on
  Safari for macOS and on Chrome, Firefox and Edge for iOS, with desktop Chrome
  and Firefox as controls.
- Update `doc/lib/js/index.md`.
- The gate needs the Safari version, and the iOS or iPadOS WebKit version
  for the iOS browsers, out of the user-agent check. If
  [JS bundle trims](/quest/m1/js-bundle-trims.md) has replaced bowser in the
  same file by then, its check exposes that version rather than a yes/no
  WebKit flag (decided 2026-10-08).

## Required

- [Safari ships the WebKit 319818 fix](/quest/m3/webkit-319818.md) - the release to gate on

## Closes

- [#2388](https://github.com/moq-dev/moq/issues/2388) - close this issue when the quest finishes

## Related

- [JS bundle trims](/quest/m1/js-bundle-trims.md) - replaces bowser in the same `browser.ts` user-agent check
