# [S] A closed endpoint ends its streams as Closed, never unroutable

## Goal

`py/moq-rs/tests/test_socket.py::test_abort_and_endpoint_close_are_errors`
passes every run.

## Plan

Found 2026-11-15 by `just check` on
[moq-dev/moq#8838](https://github.com/moq-dev/moq/pull/8838), whose change does
not reach a local origin's socket. Run alone, it fails about 1 run in 3 at line
90: after `a.close()`, `server.consumer.read_frame()` raises
`moq.Error.Protocol: unroutable` instead of `moq.Error.Closed`, so closing an
endpoint races its route's withdrawal against its streams' ending. Fix the
endpoint at the cause, as the epic's rules ask; it likely shares one with
[the socket endpoint's ending](/quest/m1/socket-endpoint-end.md).

Public API: none. Wire: none.
