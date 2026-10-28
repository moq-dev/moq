# [M] The JS mock transport carries only reset codes

## Goal

`js/net`'s mock transport delivers a stream reset to the peer as a code, as
real WebTransport does (0 for a plain `Error`), instead of the local reason
object. Every test that relied on a reason a real wire cannot carry is
fixed, and so is any reset that would go out with the wrong code.

## Plan

Found by [moq-dev/moq#6988](https://github.com/moq-dev/moq/pull/6988). A
cancel hook on the mock's TransformStream works in bun and node but makes
`abort()` reject; wrapping the read side does not. Either breaks 9 tests:

- 4 ietf publisher draft-22 tests expect "unsubscribed" or "range update",
  which suggests those resets go out as INTERNAL_ERROR on a real wire and
  should be CANCELLED. Reproduce against a real peer and fix the code at the
  publisher.
- 2 lite publisher tests and 3 reload/connect setup tests check reasons a
  real wire cannot carry; assert on what the peer can observe.

Public API: none. Wire: any reset found sending the wrong code is corrected.
