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

## Related

- [Close codes on every transport](/quest/m1/close-codes.md) - the same symptom over qmux and raw QUIC
- [UnknownSession log flood](/quest/m1/unknown-session-logs.md) - another `web-transport-moq` release and pin bump
