# [M] A Chromium patch delivers a reset that beats the stream's accept

## Goal

A Chromium patch, ready for the maintainer to submit, makes a RESET_STREAM
that reaches an incoming unidirectional stream before Blink took the stream
error the page's stream with its code, instead of leaving it open forever.
`@moq/net` already copes on lite-07; the fix makes that path dead code on
fixed browsers. The workaround in `js/net/src/lite/subscriber.ts` stays: the
draft allows it, and older browsers keep the bug.

## Plan

Found by [moq-dev/moq#8673](https://github.com/moq-dev/moq/pull/8673) with
`just test stream-credit`, whose preempting resets of just-opened streams
(YIELDED then, TOO_FAR_BEHIND since #8965) hit the bug within a few
milliseconds of the stream's data.

Diagnosis on Chromium 156: the network service accepts the stream and replies
to Blink's `AcceptUnidirectionalStream` on the `WebTransport` mojo pipe, while
`OnReceivedResetStream` travels on the `WebTransportClient` pipe, so the reset
can reach `blink::WebTransport::OnReceivedResetStream` before the stream is in
`incoming_stream_map_`, where it is dropped. The FIN path keeps such a close
in `closed_potentially_pending_streams_`; the reset path has no counterpart,
which is the likely shape of the fix. The network service then closes the
data pipe, so the page's stream neither ends nor errors. The relay logged
that Chromium acknowledged every such RESET_STREAM.

A minimal repro is a WebTransport server that opens a unidirectional stream,
writes a few bytes, and resets it with a code about a millisecond later, and a
page that reads it: Chromium's read never settles. `moq-quic` can serve it.
Search crbug.com for an existing report first and attach the patch there; if
none exists, the patch description carries the repro.

Decided 2026-11-13 (maintainer, `/quest-complete`): ✅ keep this follow-up
(rejected: drop it and live with the workaround alone). Decided 2026-11-15
(maintainer, /quest-complete): ✅ approved to file at crbug.com. Decided
2026-11-25 (maintainer, /quest-plan): ✅ write the fix ourselves; the epic's
Plan says what this delivers and how it reaches upstream.

## Related

- [A Firefox patch keeps a reset's code](/quest/m4/browser-fixes/firefox-reset-code.md) - the same race in Firefox
