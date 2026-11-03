# [XS] Browsers expose WebTransport receive credit

## Goal

Condition: WebTransport tells the page how much receive credit a stream it has
stopped reading still holds, in the spec and in shipping Chrome and Firefox.
Then [JS fastest-reader pacing](/quest/m2/js-fastest-reader.md) can pause
groups safely. Delete this quest once the condition holds.

## Plan

Planned 2026-11-02 (maintainer, follow-up planning after the 2026-11-02 /quest-complete run), split out of [moq-dev/moq#7624](https://github.com/moq-dev/moq/pull/7624)'s
fastest-reader decisions (native only; browser parity waits on a real
receive-credit contract). `Reader.closed` watches resets, but nothing says how
much credit a paused stream holds, and guessed window constants would move
memory outside flow control.

Check: the W3C WebTransport draft and the Chrome and Firefox implementations
for a receive-window or credit API on incoming streams (or per-stream
`highWaterMark`-style flow control that maps to QUIC credit).
