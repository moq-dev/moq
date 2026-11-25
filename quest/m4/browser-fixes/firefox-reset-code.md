# [M] A Firefox patch keeps a reset's code when it lands right after the data

## Goal

A Firefox patch (to Gecko or neqo, wherever the code is lost), ready for the
maintainer to submit, makes a RESET_STREAM that reaches an incoming
unidirectional stream right after its data reject the page's read with a
`WebTransportError` carrying its `streamErrorCode`, instead of a bare
`TypeError: Error in input stream`.

## Plan

Found by [moq-dev/moq#8673](https://github.com/moq-dev/moq/pull/8673) with
`just test stream-credit`. On Firefox 157, in one run, 339 of the ~1100
YIELDED resets sent within 5 ms of the stream's data reached the page as
`TypeError: Error in input stream` with no `streamErrorCode`, and none of the
30 sent 50 ms or later did, with RESET_STREAM and RESET_STREAM_AT alike.

Where the code is lost is not yet known: neqo (Rust, a pull request on
GitHub) or Gecko's WebTransport glue (Phabricator). Find it first with the
minimal repro: a WebTransport server (`moq-quic` can serve it) that opens a
unidirectional stream, writes a few bytes, and resets it with a code about a
millisecond later, and a page that reads it. Search bugzilla.mozilla.org for
an existing report first.

Decided 2026-11-15 (maintainer, /quest-complete): ✅ approved to file at
bugzilla.mozilla.org. Decided 2026-11-25 (maintainer, /quest-plan): ✅ write
the fix ourselves (split from the Chromium half of the old
`browser-reset-reports` quest); the epic's Plan says what this delivers and
how it reaches upstream.

## Related

- [A Chromium patch delivers a reset that beats the stream's accept](/quest/m4/browser-fixes/chromium-reset-before-accept.md) - the same race in Chromium
