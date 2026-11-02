# Migrating moq-uring

How to update your code for each breaking change, newest first.

## Unreleased

### QUIC early data is disabled

The shared moq-quic core no longer sends or accepts early data, including with caller-supplied rustls server configs.
The uring driver already returns established sessions, so no source migration is needed.
TLS session tickets still enable resumed 1-RTT handshakes.

### QUIC receive chunks are ordered

The underlying `moq_quic::RecvStream::read()` no longer takes an ordering flag, and `Chunk` contains only `bytes`.
The uring driver's receive API keeps its existing ordered behavior.

### `udp::Transmit` is built with `Transmit::new` (#6400)

`udp::Transmit` gains `src_ip`, the local address a send leaves from, and is `#[non_exhaustive]`.
Replace `Transmit { to, segment, ecn }` with `Transmit::new(to, segment)` and set `ecn` (or `src_ip`) on the result.
Pass a received `Packet::dst_ip()` as `src_ip` to answer from the address the peer reached.

### Wrap sessions in `transport::Local` (#6176)

moq-net dropped `connect_lite`, `accept_lite`, and `accept_request_lite`, and every session now needs a `Send` transport.
Wrap the worker's session as `transport::Local::new(transport::Session::new(conn, address))` and call `connect`, `accept`, or `accept_request`.

### Timers run on `kio::clock`

The worker is a `kio::clock` backend, so its deadlines are `kio::clock::Instant`s instead of `std::time::Instant`s.
`Timer::set` takes an `Option<kio::clock::Instant>`, and `quic::handshake::Info::deadline` is one.
Read the worker's time with `kio::clock::now()` inside it.

### Datagrams take `SendOptions` (#5770, #5872)

`quic::Connection::send_datagram` and `quic::web::Session::send_datagram` take a `quic::SendOptions { group, priority }` and never block; pass `SendOptions::default()` for an ungrouped datagram.

### QUIC limits are `moq_quic::Limits` (#5871)

`quic::Limits` is `moq_quic::Limits`, where each `None` field keeps that limit, and an invalid one fails `set_limits` with `Error::Limits` instead of `WindowRange` or `StreamsRange`.
Start from `quic::Limits::default()` and set only the fields you change.

### The `noq` feature is `quic` (#5664)

The QUIC stack is MoQ's own `moq-quic` instead of `moq-noq-proto`.
Rename the `noq` cargo feature to `quic`.
A qlog directory gets one file per endpoint and one per dial instead of one per connection.

### `CID_LEN` is gone (#5688)

`quic::endpoint::CID_LEN` is removed; endpoints still issue 8-byte connection IDs, sharded or not.

### The server config takes a rustls config (#5312)

`quic::server::Config` drops `identity`, `alpn`, and `client_auth` for one `tls: Arc<rustls::ServerConfig>`, and `Config::new` takes it.
Build it from an identity with `identity.server(&alpns, client_auth)?`, or share a resolver with other listeners.

### Sessions are wrapped for moq-net

moq-net no longer takes a `web_transport_trait` session.
Wrap a moq-uring session with `transport::Session::new`, naming how the request target travels.

```rust
let session = moq_uring::transport::Session::new(connection, moq_net::transport::Address::Uri);
```
