# [XS] moq-tokio tests bind their ports independently

## Goal

moq-tokio's integration tests never fail on a port another process or
parallel test holds. `websocket_forbidden_does_not_end_a_quic_connect`
(`rs/moq-tokio/tests/broadcast.rs`) fails about 1 in 15 local runs today.

## Plan

`test_server()` takes an ephemeral UDP port and then binds TCP on the same
number, which was never reserved on TCP. Bind the QUIC and WebSocket
listeners each on `:0` and hand the test client the WebSocket port
explicitly, adding a client config override for the fallback URL's port if
none exists (test-only if possible; otherwise note it as a public API
addition). No retry: this replaces the bounded bind retry
[#4055](https://github.com/moq-dev/moq/pull/4055) added. No wire change.
