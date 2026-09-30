# [S] WebTransport close delivers its capsule upstream

## Goal

Closing a WebTransport session delivers CLOSE_WEBTRANSPORT_SESSION, so a
browser sees the close code and reason, even when the caller drops its last
session handle right after `close()`. The fix lives in `web-transport-moq`,
and moq-tokio's `CLOSE_LINGER` workaround is deleted.

## Plan

[#4429](https://github.com/moq-dev/moq/pull/4429) found the root cause:
`web-transport-moq` 1.3.2's `Session::close` sends the capsule from a spawned
task that holds the connection and the CONNECT stream, but not the H3
`Settings` (control and QPACK streams). Dropping the last `Session` right
after `close()` finishes the control stream in the same flight as the
capsule, and Chromium reports "Connection lost." with no code. #4429 worked
around it in `rs/moq-tokio/src/transport.rs` by keeping a session clone in a
detached task until `closed()` resolves, capped at a 10s `CLOSE_LINGER`.

Decided 2026-09-29: fix it at the source and remove the timeout workaround.

- In moq-dev/noq, the close path keeps whatever the capsule needs alive until
  it is delivered, without the caller's help. Its regression test drops the
  session right after `close()` and asserts the peer reads the capsule
  before the H3 control stream ends; it must fail on 1.3.2. A Rust peer
  alone proves nothing: `session_close_surfaces_a_rejection_code` in
  `rs/moq-tokio/tests/broadcast.rs` passed on 1.3.2 without the linger,
  because only Chromium treats the control stream's end as fatal. Add a
  browser check too if the playwright harness makes it cheap.
- Release `web-transport-moq` 1.3.x and bump the pin on `main` (2.x on `dev`
  if it has moved).
- Delete `CLOSE_LINGER`, its task, and its mock-session tests from
  moq-tokio.

Public API: none. Wire: none.

Decided in the 2026-09-30 audit: the UnknownSession log flood quest merged
here, since the same noq 1.3.3 release (moq-dev/noq#21) carries its fix.
`decode_uni` and `decode_bi` mapped a stream reset before its WebTransport
header to `UnknownSession`, flooding relay logs with WARNs; the fork now
keeps the read's cause and logs a reset at debug. Remaining steps: release
`web-transport-moq` 1.3.3, bump the pin, delete `CLOSE_LINGER`
(`rs/moq-tokio/src/transport.rs:24`), and confirm on a moq.pro relay that the
flood stops.
