# [S] moq-rs's Python TCP close test settles on a promised error

## Goal

`py/moq-rs/tests/test_socket.py`'s `test_tcp_rst_and_endpoint_close_are_errors`
passes on a loaded host, and its `pytest.mark.skip` is removed. It still
proves that a stream reset reaches the reader as `Protocol` with the app code,
and that an endpoint close never ends a reader with a clean EOF.

## Plan

Planned 2026-11-09 (maintainer's standing rule that flakes join this epic).
It failed once in [moq-dev/moq#8293](https://github.com/moq-dev/moq/pull/8293)'s
loaded `just check`, before a main merge, then passed 30 of 30 runs alone. The
reader after `a.close()` got `Unroutable`, while the test accepts only
`Cancelled` or `Closed`. The test uses an in-process origin with no lite-07
session, so #8293's route changes can't reach it.

Find which path turns the endpoint's retraction into `Unroutable` for a stream
that was already open: the socket module's route lookup, or the origin
retracting the peer's broadcast before the stream's reset lands. Decide whether
`Unroutable` is a cause the socket contract should promise here (then document
it on `moq.socket` and accept it in the test) or a wrong error for an already
routed stream (then fix the cause so `Cancelled` or `Closed` wins). Prefer the
fix if the error leaks to users of a stream they already held. Never match on
error text.

Public API: none, unless the contract's documented errors change. Wire: none.
