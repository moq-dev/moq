# [S] Browsers deliver a reset that lands right after a stream's data

## Goal

Chromium and Firefox each have an upstream bug report, with a minimal
WebTransport repro, for a RESET_STREAM that reaches an incoming unidirectional
stream before the page took its data: Chromium loses the reset, leaving the
stream open forever, and Firefox errors it with a bare `TypeError` that drops
its code. `@moq/net` already copes on lite-07 (see Plan); the reports let a
browser fix make those paths dead code.

## Plan

Found by [moq-dev/moq#8673](https://github.com/moq-dev/moq/pull/8673) with
`just test stream-credit`, whose YIELDED resets of just-opened streams hit
both bugs within a few milliseconds of the stream's data.

- Chromium 156: the network service accepts the stream and replies to Blink's
  `AcceptUnidirectionalStream` on the `WebTransport` mojo pipe, while
  `OnReceivedResetStream` travels on the `WebTransportClient` pipe, so the reset
  can reach `blink::WebTransport::OnReceivedResetStream` before the stream is in
  `incoming_stream_map_`, where it is dropped. The FIN path keeps such a close in
  `closed_potentially_pending_streams_`; the reset path has no counterpart. The
  network service then closes the data pipe, so the page's stream neither ends
  nor errors. The relay logged that Chromium acknowledged every such
  RESET_STREAM.
- Firefox 157: in one run, 339 of the ~1100 YIELDED resets sent within 5 ms of
  the stream's data reached the page as `TypeError: Error in input stream` with
  no `streamErrorCode`, and none of the 30 sent 50 ms or later did, with
  RESET_STREAM and RESET_STREAM_AT alike. Where Gecko or neqo drops the code is
  not yet known.
- A minimal repro is a WebTransport server that opens a unidirectional stream,
  writes a few bytes, and resets it with a code about a millisecond later, and a
  page that reads it: Chromium's read never settles, Firefox's rejects without
  the code. `moq-quic` can serve it.
- Once a browser fix ships, the matching workaround in `js/net/src/lite/subscriber.ts`
  stays: the draft allows it, and older browsers keep the bug.

## Waiting

- Maintainer: approve filing at crbug.com and bugzilla.mozilla.org, outside the
  moq orgs, which needs your go-ahead; check each tracker's policy on
  LLM-written reports first. Recommendation: file both, with the repro.
