# [XS] moq-tokio tests pre-bind their ports

## Goal

moq-tokio's integration tests never fail on a port another process or
parallel test holds. `websocket_forbidden_does_not_end_a_quic_connect`
(`rs/moq-tokio/tests/broadcast.rs`) fails about 1 in 15 local runs today.

## Plan

`test_server()` takes an ephemeral UDP port and then binds TCP on the same
number, which was never reserved on TCP. Bind both sockets up front and hand
them to the server, so nothing is left to collide on. Never retry. No public API or wire change.
