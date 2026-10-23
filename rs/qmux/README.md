# qmux

[QMux](https://datatracker.ietf.org/doc/draft-ietf-quic-qmux/) carries QUIC streams,
datagrams, and flow control over reliable transports. This crate uses
`moq-quic`'s stream state machine and speaks draft-02 by default, negotiating
with draft-01 and draft-00 peers.

Use `Session::connect` or `Session::accept` with a transport. The `tcp`, `tls`,
`uds`, and `ws` modules provide TCP, TLS, Unix socket, and WebSocket transports;
`transport::Stream` wraps another Tokio byte stream. Sessions implement
`moq_net::transport::poll` for MoQ's protocol layer.

The default features are `tls` and `wss`. Opt into `tcp`, `uds`, or `ws` for the
corresponding unencrypted transport, or disable defaults when supplying your own.
Unix sockets require a Unix host.

See the [API reference](https://docs.rs/qmux) and the
[client and server examples](https://github.com/moq-dev/moq/tree/main/rs/qmux/examples).
`just rs test --manifest-path rs/qmux/Cargo.toml --features tcp,ws` runs its
protocol tests and compatibility checks with qmux 0.6.3.
