# [XS] WebTransport close delivers its capsule upstream

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

The upstream fix makes the close task own a clone of the whole session:
[moq-dev/noq#23](https://github.com/moq-dev/noq/pull/23) for 2.x (`dev`) and
its backport [moq-dev/noq#24](https://github.com/moq-dev/noq/pull/24) for
1.3.x (`main`). Their `web-transport-moq/tests/close_capsule.rs` drops the
session right after `close()` against a hand-rolled HTTP/3 peer and fails on
1.3.2 and 2.0.0.

Remaining, once the releases carrying them are published:

- Bump the `web-transport-moq` pin on `main` (and on `dev` if 2.0.x lands
  first).
- Delete `CLOSE_LINGER`, its task, and its mock-session tests from
  moq-tokio. They are exactly what #4429 added, so reverting it is enough.

A browser check is left to
[Browser close code](/quest/m1/browser-close-code.md): the playwright harness
has no close-code scenario, so it is not cheap to add here.

Public API: none. Wire: none.

Decided in the 2026-09-30 audit: the UnknownSession log flood quest merged
here, since the same `web-transport-moq` release carries its fix.
`decode_uni` and `decode_bi` mapped a stream reset before its WebTransport
header to `UnknownSession`, flooding relay logs with WARNs; the fork now
keeps the read's cause and logs a reset at debug. After the bump, confirm on
a moq.pro relay that the flood stops.

## Related

- [Browser close code](/quest/m1/browser-close-code.md) - the playwright case that proves Chromium reads the code this quest unblocks
