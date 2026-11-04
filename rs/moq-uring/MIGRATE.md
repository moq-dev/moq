# Migrating moq-uring

How to update your code for each breaking change, newest first.

## Unreleased

### QUIC method names and signatures match the core and Tokio driver

Construct an endpoint with `quic::Endpoint::new(config, socket)` instead of `new(socket, config)`.
Rename `quic::Connection::remote_addr()` to `remote_address()`; it now reports the live peer address, following migration instead of caching the handshake address.
`Connection::close(code, reason)` takes a UTF-8 `&str` instead of bytes, matching the core and Tokio driver.
`Connection::set_limits` returns `quic::LimitsError` directly instead of wrapping it in `quic::Error::Limits`.

On `quic::Connection` and `quic::web::Session`, rename `read_datagram()` to `recv_datagram()`.
On both layers' `send::Stream` and `send::Finished`, rename `stopped()` to `closed()`; results and acknowledgement semantics are unchanged.

Both layers' `recv::Stream::poll_lease`, and `quic::web::recv::Stream::poll_reset`, take `&kio::Waiter` instead of `&mut Context`.
Use `kio::wait(|waiter| stream.poll_lease(waiter, want).map_ok(...))` for a lease operation, or retain a `kio::Park` in a `Future` implementation and pass `park.hold(cx)`.
Dropping the caller's waiter cancels its registrations while the stream stays alive.

### Native WebTransport implements moq-net's poll traits ([#7771](https://github.com/moq-dev/moq/pull/7771))

`quic::web` is supported as a WebTransport library without running MoQ.
Pass its `Session` directly to moq-net through `transport::Local::new(session)`.
Remove the generic `transport::Session::new(session, address)` wrapper; a WebTransport session reports `Address::Uri` and `Session::raw(connection)` reports `Address::Setup`.
The generic send/receive adapters, their error wrapper, and the sealed `Grouped`/`GroupedSend` traits are removed, along with every `web_transport_trait` implementation and dependency.
Custom transports implement `moq_net::transport::poll` directly.

Use `quic::web::send::Stream` and `recv::Stream` instead of `SendStream` and `RecvStream`.
Their errors are `send::Error` and `recv::Error`.
`finish(self)` returns a `send::Finished`, which can be awaited for acknowledgements or a peer stop; `reset(self, code)` and `stop(self, code)` consume their stream.
Code using moq-net's poll traits keeps their mutable terminal methods.
Session polls take `&self` and a caller-owned `&kio::Waiter`; opens also take the driver's `send::Options`, preserving full `i64` priorities and real send groups.

`Request` dereferences to `proto::ConnectRequest`; read its `url` and `protocols` fields there.
`Request::connection()` names the underlying QUIC connection.
`Request::respond` takes `impl Into<proto::ConnectResponse>` and requires status OK; `reject` takes an `http::StatusCode` and requires a 4xx or 5xx status.
Replace `web::Response::default()` with `web::proto::ConnectResponse::OK` and `web::Rejected` variants with HTTP status codes.
`send::Stream`, `send::Finished`, and `recv::Stream` expose their stream ID as `id()`.

A receive stream's `closed()` observes an empty FIN without a read. When there is payload, read it before waiting for close. `poll_reset` still watches only failures, so it remains pending after a clean end.
Invalid stream error codes fail as `InvalidReset` or `InvalidStopped`, rather than looking like a clean end.

### QUIC waits have caller-owned poll forms ([#7707](https://github.com/moq-dev/moq/pull/7707))

The QUIC driver connection's stream opens, accepts, datagram receive and close waits,
the endpoint's accept, and both send handles' close waits now expose `poll_*` methods
taking `&self, &kio::Waiter`. Existing async methods keep their behavior and wrap the
polls. `send::Finished` also implements `Future`, with the same result as `stopped()`:
`None` for acknowledged data and FIN or a local reset, `Some(code)` for a peer stop,
and an error if the connection ends first.

### `peer_chain` is `peer_identity` ([#7028](https://github.com/moq-dev/moq/pull/7028))

`quic::driver::Connection::peer_chain()` is now `peer_identity()` and returns `Option<Vec<CertificateDer<'static>>>`, mirroring moq-tokio.
Use `cert.to_vec()` for raw DER.

### Smaller default windows ([#7624](https://github.com/moq-dev/moq/pull/7624))

`quic::transport::Config::default()` takes a 4 MiB connection window, a 1 MiB stream window, and 4 MiB of unacknowledged outgoing data, down from 128 MiB, 4 MiB, and 16 MiB, matching moq-tokio's `[quic]` defaults. Set `limits` explicitly to keep the old windows.
`quic::driver::recv::Stream::received_reset`, and so `quic::web::RecvStream::poll_reset`, fail with `Error::Evicted` for a stream evicted to free a stuck window.

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
Wrap the worker's `quic::web::Session` as `transport::Local::new(session)` and call `connect`, `accept`, or `accept_request`.

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
