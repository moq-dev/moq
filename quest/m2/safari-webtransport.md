# [XS] Safari WebTransport returns when WebKit refills its windows

## Goal

Once a shipping Safari refills MAX_DATA and MAX_STREAMS credit, js/net stops
routing WebKit browsers to the WebSocket and qmux fallback: the gate admits the
fixed version and later, and a Safari watcher plays for hours over WebTransport
as Chrome does. #2388 has been WebKit tracking since PR #3345 and closes with
this.

## Plan

- Gate `js/net/src/connection/browser.ts` on the version that ships the fix,
  for Safari and for the iOS WebKit browsers
  [WebKit gate](/quest/m0/webkit-webtransport-gate.md) added, keeping older
  versions on the fallback.
- Before flipping, rerun the raw WebTransport reproduction from #2388 (about
  7,600 eleven-byte unidirectional streams, and about 16 MiB of data) on the
  fixed Safari, then a watch longer than two minutes in the QA harness.
- Update `doc/lib/js/index.md`.

## Required

- WebKit bug 319818 (https://bugs.webkit.org/show_bug.cgi?id=319818) is fixed and shipping in a Safari release
- [WebKit gate](/quest/m0/webkit-webtransport-gate.md) - the engine-wide gate this relaxes

## Closes

- [#2388](https://github.com/moq-dev/moq/issues/2388) - close this issue when the quest finishes
