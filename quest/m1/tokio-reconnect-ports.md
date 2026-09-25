# [S] moq-tokio reconnect and worker tests bind their own ports

## Goal

The moq-tokio integration tests stop picking a free port, releasing it, and
binding it again, a race another process can win. `reconnect.rs`'s
`spawn_server` loses its retry loop, and the worker tests that do not need a
known port bind `:0`.

## Plan

- Add `Server::tcp_local_addr()` and `Listener::tcp_local_addr()`, mirroring
  `websocket_local_addr()`, reporting the bound address of the plain TCP
  (qmux) listener. Today `StreamListeners` keeps only the configured bind and
  moves the bound listener into its accept task. `spawn_server` binds `:0` and
  reads the address back. This is an additive public API.
- In `worker.rs`, the tests that only need some port bind `:0` through the
  group and use `Group::local_addr()`. The tests that rebind the same port
  after a drop, or probe it while the group holds it, keep a known port, which
  is the behavior under test.
- No retries or sleeps.

## Related

- [Test ports](https://github.com/moq-dev/moq/pull/4084) - the same fix for `websocket_forbidden_does_not_end_a_quic_connect`
