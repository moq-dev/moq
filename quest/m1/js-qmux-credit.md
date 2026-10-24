# [S] JS qmux returns stream credit when both halves close

## Goal

`@moq/qmux` returns MAX_STREAMS credit for a peer stream only once both of
its halves are terminal, as QUIC and Rust qmux do, so stream credit bounds
open streams. `@moq/net` then drops its own subscription and FETCH caps to
mirror Rust.

## Plan

`maybeReplenishStreamCredit` in `js/qmux/src/session.ts` fires once a stream
is accepted and its receive half is terminal. A bidirectional subscription
whose receive half ends while its send half stays open frees credit early.
Because of this, [Session limits](/quest/m1/api-audit/session-limits.md)
kept the `@moq/net` caps. Fix the credit, then remove those caps, with a test
that a half-closed stream still holds its credit.

Public API: `@moq/net` loses its subscription and FETCH caps. Wire: none.

## Related

- [Session limits](/quest/m1/api-audit/session-limits.md) - kept the JS caps because of this
