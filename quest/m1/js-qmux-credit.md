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
Because of this, [moq-dev/moq#6467](https://github.com/moq-dev/moq/pull/6467)
kept the `@moq/net` caps. Fix the credit, then remove those caps, with a test
that a half-closed stream still holds its credit.

Public API: `@moq/net` loses its subscription and FETCH caps. Wire: none.
