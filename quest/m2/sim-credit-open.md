# [S] In-memory transports refuse a request without stream credit

## Goal

moq-net's in-memory test transports model the peer's bidirectional stream
credit, and `poll_open_request` returns `Ok(None)` (refused, as
`Error::NoStreamCredit` upstream) when none is left, as native QUIC does.
Integration tests then catch code that spends its own credit before a
prerequisite stream opens.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7332](https://github.com/moq-dev/moq/pull/7332): m2, as
recommended.

[moq-dev/moq#7332](https://github.com/moq-dev/moq/pull/7332)'s review found that pipelined FETCHes could take every bidi stream
before TRACK opened; on native QUIC, TRACK's `poll_open_request` fails at
once (`rs/moq-net/src/coding/stream.rs:49-55`), but the in-memory
transports always open: `rs/moq-net/src/lite/test_transport.rs:532` and
`rs/moq-net/tests/integration/support/mock.rs:558` both map `poll_open_bi`
to `Some`.

- Give both a configurable bidi credit (default unlimited so existing tests
  are unchanged; `with_open_bi_gate` already parks `poll_open_bi`) and make
  `poll_open_request` return `Ok(None)` when it is exhausted, releasing
  credit as streams close.
- Regression: with credit for one stream and fetches queued before TRACK,
  the track still opens (the order [moq-dev/moq#7332](https://github.com/moq-dev/moq/pull/7332) fixed).

Public API: none (test support only). Wire: none.

## Related

- [Pipeline requests](/quest/m2/pipeline-requests/README.md) - [moq-dev/moq#7332](https://github.com/moq-dev/moq/pull/7332) is part of this epic
