# [S] UnknownSession log flood

## Goal

moq.pro relays stop logging
`web_transport_moq::session: failed to decode unidirectional stream err=WebTransportError(UnknownSession)`
for streams that were simply reset or cut off before their WebTransport
header arrived. A stream that really names another session is still reported
as `UnknownSession`, and the error a caller sees says what happened.

## Plan

Seen in production at a rate like the old-group warnings
https://github.com/moq-dev/moq/pull/4208 fixed, on the same hosts.

Likely root cause, to confirm with a test first: `decode_uni` (and
`decode_bi`) in `web-transport-moq`'s `session.rs` (repository
`moq-dev/noq`) map every failure to read the stream type or session ID to
`UnknownSession`. A read fails whenever the peer resets the stream before
those bytes arrive, which moq does routinely: a publisher resets a group's
stream when the group is superseded or expires, and without reliable reset
the header can be discarded with it. `poll_accept_uni` then logs it at WARN,
though its own comment says the stream "was probably reset early".

- Reproduce in `web-transport-moq`'s tests: open a WebTransport uni stream,
  reset it before the header is delivered, and assert the accept loop
  reports a reset rather than `UnknownSession`.
- Fix the mapping at the source: carry the read's real cause (reset, closed,
  truncated), keep `UnknownSession` for a session-ID mismatch, and log the
  expected reset at debug. Not a log filter on the moq side.
- `web-transport-quinn` in `moq-dev/web-transport` carries the same code;
  fix it too if it is still published.
- Release and bump the pin here. Confirm on a moq.pro relay that the rate
  drops, and that any remaining `UnknownSession` lines are real mismatches.

If the reproduction shows something other than early resets (for example a
peer really using another session ID), stop and report before changing the
log level.

## Related

- [Close codes on every transport](/quest/m1/close-codes.md) - another error mapping fix in the same crate
- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - keeps a reset stream's header, which removes most of these
