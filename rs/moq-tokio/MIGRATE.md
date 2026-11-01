# Migrating moq-tokio

How to update your code for each breaking change, newest first.

## Unreleased

### `tls://` listeners verify client certificates ([#7358](https://github.com/moq-dev/moq/pull/7358))

A `listen.tcp.tls` listener serves the listen policy's client-certificate verifier, so `--listen-tls-root` and pinned `tls.peers` apply to it as they do to QUIC, and a stream-only server with that listener accepts them instead of returning `Error::MtlsUnsupported`.
Its `Request::peer_identity`, `server_name`, and `authority` report the client chain and SNI, which were always `None`.
Under pinned peers, a `tls://` client must present a pinned certificate, as on QUIC.

### `Connection::allocator` ([#6751](https://github.com/moq-dev/moq/pull/6751))

`Connection::bandwidth()` is `Connection::allocator()`, mirroring `moq_net::Session::allocator()`.

### QUIC connections wait for the handshake

Await `quic::driver::Connecting` instead of calling `into_0rtt()`.
`Connection::authenticated()` and `RecvStream::is_0rtt()` are gone because every connection handle already completed its handshake.
Remove `ZeroRttRejected` match arms from `ReadError`, `WriteError`, `ResetError`, and `StoppedError`.
TLS session tickets still enable resumed 1-RTT connections; application data always waits for the handshake.

### QUIC driver APIs follow their consumers

`quic::driver::RecvStream::read_chunk(max)` reads ordered bytes and has no ordering flag or `Chunk::offset`.
Replace `read_chunks` with successive `read_chunk` calls. `ReadError::IllegalOrderedRead` is gone.
Use `Connection::set_limits` with `quic::Limits` instead of `set_max_concurrent_uni_streams`, `set_max_concurrent_bi_streams`, `set_send_window`, and `set_receive_window`.
`Limits::max_streams` sets both directions, so code that called only `set_max_concurrent_uni_streams(n)` now raises the bidirectional limit too.
Each field is a `NonZeroU64`, and `None` leaves that limit unchanged, so a live connection cannot set a stream limit or window to 0; set a per-direction or zero stream limit at construction with `TransportConfig::max_concurrent_uni_streams` and `max_concurrent_bidi_streams`.
The accept, open, and datagram methods return opaque futures; await them without naming the removed `Accept`, `AcceptUni`, `AcceptBi`, `OpenUni`, `OpenBi`, `ReadDatagram`, or `IncomingFuture` types.
Unused connection accessors (`side`, `local_ip`, `min_rtt`, `force_key_update`, `export_keying_material`, `watch_remote_address` with its `RemoteAddressWatcher`, and `handshake_confirmed`), `Connecting::local_ip`, `Endpoint::stats`, `EndpointStats`, `Endpoint::open_connections`, `set_server_config`, `rebind_abstract`, and unused `Incoming` operations (`accept_with`, `retry` with its `RetryError`, `ignore`, `may_retry`, `refuse`, `local_ip`, `remote_address`, `remote_address_validated`, and `orig_dst_cid`) are no longer public.
Read peer addresses through `Connection::remote_address` or `Connecting::remote_address`; configure the endpoint at construction and use `Endpoint::rebind` for a new UDP socket.
To rotate TLS certificates without `set_server_config`, give the `ServerConfig`'s rustls config a `ResolvesServerCert` that returns the current certificate, as `tls::Serve` does.
The `poll_read_buf` helper is private; use `AsyncRead` or `poll_read`.
`quic::driver` no longer re-exports `Side`, `TimeSource`, `StdSystemTime`, `BloomTokenLog`, `TokenMemoryCache`, `TokenLog`, `TokenStore`, `NoneTokenLog`, `NoneTokenStore`, or `TokenReuseError`; name `moq_quic::Side` directly, and use the default token policy.

### The connection receive window is 128 MiB, and has a floor (#5878)

`quic::DEFAULT_RECEIVE_WINDOW` is 128 MiB instead of 64 MiB, since incomplete messages now hold connection credit until complete.
A `quic.receive_window` (`--quic-receive-window`, `[quic] receive_window`) below `moq_net::transport::MIN_RECEIVE_WINDOW`, the 64 MiB announcement cap plus 16 bytes, fails with `Error::ReceiveWindowTooSmall`.
Raise an explicit window to at least that, or unset it.

### qmux sessions are qmux 0.7 (#6231)

The WebSocket, TCP, TLS, and Unix socket transports run on [qmux 0.7](https://github.com/moq-dev/moq/blob/main/rs/qmux/MIGRATE.md), which implements moq-net's poll traits directly instead of going through `web_transport_trait`.
Code that holds a `qmux::Session` from moq-tokio, such as one from the WebSocket listener's `accept`, follows qmux's migration guide.
A WebSocket client must offer a QMux subprotocol: `websocket::accept` refuses an upgrade without one, and the legacy `webtransport` format is gone.

### MoQ sessions need the `net` feature (#6132)

moq-net is optional, behind the default `net` feature that every transport feature (`quic`, `websocket`, `tcp`, `uds`, `iroh`, `webrtc`) turns on.
A build with `default-features = false` and no transport feature loses `client`, `connect`, `connection`, `listen`, `origin`, `transport`, their root re-exports (`Client`, `Connection`, `ConnectError`, `Transport`, ...), `Error::MoqNet`, `Error::Connect`, `Error::connect_error`, `Error::is_auth`, and the `moq_net` re-export.
Enable `net`, or a transport feature, to keep them.

### `mdns::Secret` is `secret::Secret`, read from a file (#5910)

The 32-byte secret type moved out of `mdns` so `listen.reset.secret` shares it, and it no longer parses inline hex.
Name it `moq_tokio::secret::Secret`.
Replace `Secret::load(value)` with `Secret::read(path)`, which reads a file of 64 hexadecimal characters and returns `secret::Error`; replace `Secret::new(hex)` and `str::parse` with `Secret::from([u8; 32])`, decoding the hex yourself.

### Dropping an unfinished send stream resets it (#5909)

`quic::driver::SendStream` and `quic::web::SendStream` send `RESET_STREAM` with code 0 when dropped before `finish()`, instead of finishing implicitly.
Call `finish()` before dropping a stream whose data should arrive whole.

### `jemalloc::tikv_jemallocator` is 0.7 (#6053)

The re-exported allocator crate moved from tikv-jemallocator 0.6 to 0.7.
Name the allocator through `moq_tokio::jemalloc::tikv_jemallocator`, or bump your own tikv-jemallocator to 0.7.

### Usage types come from usage-rs 7 (#6060)

The `Args` and `Config` derives now implement usage-rs 7 traits, and `cli::usage` is renamed `cli::usage_rs`.
A CLI that flattens these types depends on `usage-rs = "7"` under its own name: the derives emit `::usage_rs` paths and no longer read `Cargo.toml`.
Rename a `usage = { package = "usage-rs" }` alias to `usage-rs`, or add `#[usage(crate = usage)]` to each derive.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Set `quic::Config::keylog` (`--quic-keylog`, `MOQ_QUIC_KEYLOG`, `[quic] keylog`) as well; it now covers the iroh backend too.
Setting it without `SSLKEYLOGFILE` fails with `Error::KeylogUnset`.

### QUIC limits are `moq_quic::Limits`, set through `Request::quic` (#5871)

`server::Request::set_quic_limits` and `Error::QuicLimits` are gone.
`Request::quic()` returns the `quic::Connection` under a QUIC or WebTransport request, or `None` on other transports, and its `set_limits` takes `quic::Limits`, now `moq_quic::Limits`, where each `None` field keeps that limit.
Call `set_limits` on `request.quic()` instead, starting from `quic::Limits::default()` and setting only the fields you change.
`quic::Resolved`'s `receive_window`, `stream_receive_window`, and `send_window` are plain `u64`s with the defaults filled in, and `quic.max_streams = 0` is refused.

### Auth handles live in `connection::auth` (#5695)

The root `Auth` re-export is gone.
`Connection::auth` returns a `connection::auth::Handle`, and its `add` returns a `connection::auth::Auth` instead of a `Token`.

### Streams and datagrams take `SendOptions` (#5770, #5872)

`quic::driver::Connection` and `quic::web::Session` take a `quic::driver::SendOptions { group, priority }` in `open_uni`, `open_bi`, and `send_datagram`, which never blocks.
`send_datagram_wait` and `SendDatagram` are gone; call `send_datagram(data, SendOptions::default())` instead.
`quic::driver::SendDatagramError` carries moq-quic's refusals in `Refused` instead of its own `UnsupportedByPeer`, `Disabled`, and `TooLarge`.

### The `noq` feature is `quic` (#5664)

The QUIC stack is MoQ's own `moq-quic` instead of the `moq-noq` crates.
Rename the `noq` cargo feature to `quic`, and the error types with it: `Error::Noq` is `Error::Quic`, carrying a `moq_tokio::quic::Error`.
A qlog directory gets one file per endpoint instead of one per connection.

### Workers are unpinned by default (#5688)

`worker::Config::new(count)` no longer pins worker *i* to core *i*.
Call `with_pin(true)` to keep pinning.

```rust
let workers = moq_tokio::worker::Config::new(8).with_pin(true);
```

### Accepted sessions return their driver task (#5562)

`server::Request::ok()` returns `(session, driver)`.
Await the driver before classifying the session close, so a driver panic reports `Error::Panic` instead of the cancellation its teardown leaves behind.

### Config durations are optional

Wrap durations in `Some(...)` for connect `race`, `resolution_delay`, and `timeout`; listen `timeout`; QUIC `idle_timeout` and `keep_alive`; backoff `initial`, `max`, and `timeout`; GOAWAY `handover`; and WebSocket `delay`.
`None` uses the default, and `Some(Duration::ZERO)` keeps each knob's zero behavior.
A value set in code now wins over the parser default unless the flag or environment variable was actually given.
TOML keys are unchanged.

### Rotating TLS identities are a `tls::Serve` (#5312)

`tls::Listen::server_config_reloading` and `tls::ReloadingServerConfig` are gone; load once with `tls::Listen::load` and read each handshake's config from the returned `tls::Serve` with `config(alpns)`.
`Error::WorkerTlsGenerate` is gone because generated certificates now work on workers too.
A TLS trust-root watcher that fails to register now fails client and server construction.

### "Epoch" means only the publisher epoch (#5146)

The reconnect counter is `connects()`; rename `epoch()` calls.

### A QUIC load-balancer nonce needs its server id (#5114)

`listen.lb_nonce` without `lb_id` fails to start, so set `lb_id` next to it.
Setting `lb_id` (or `lb_nonce`) together with `load_balancer` also fails; `lb_id` used to win silently.
Set `load_balancer`, or `lb_id` alone (nonce 8), or `lb_id` with `lb_nonce`.

### Settings no listener reads stop startup (#5006)

A stream-only listener (TCP or Unix, no QUIC bind) refuses the QUIC-only `preferred_v4` / `preferred_v6` and `lb_id`, and a TLS cert, key, or `generate` unless `tcp.tls` serves it.
`unix.allow_*` needs `unix.bind`.
Each used to be ignored: drop it, or add the listener it configures.

### A client CA needs a QUIC or `tls://` listener

A stream-only listener with `listen.tls.root` or pinned `tls.peers` returns `Error::MtlsUnsupported` unless `tcp.tls` verifies them, instead of silently verifying nothing.

### `Transport` names WebTransport

`server::Transport` is `moq_tokio::Transport`, with no re-export.
A WebTransport session reports `Transport::WebTransport` (`"webtransport"` in logs); `Quic` now means raw QUIC only, so match both where you meant either.
`Connection::transport()` reports the live transport.

### Custom sessions declare their request address

`transport::Session::new` takes the session's `moq_net::transport::Address`: `Uri` when the transport handshake carries the request URI, `Setup` when SETUP does.
`Client` and `Server` are unchanged.

### `Redirect::resolve` is removed (#4854)

`Redirect` and `Goaway` stay, but the lenient `resolve(uri, current)` is gone.
Follow a GOAWAY through the connection, which validates the target itself: set `config.goaway.redirect`.

### The `android-logcat` feature is removed (#4608)

Android builds always log to logcat.
Drop the feature from your dependency.

## moq-native 0.19 -> moq-tokio 0.19

### moq-native is moq-tokio (#2896)

moq-native 0.20.0 is a stub that fails to compile, pointing at the rename.
Names sit under their modules: `connection::Goaway`, `connection::Monitor`, `transport::Session` (#3745).

```rust
let client = moq_tokio::connect::Config::default().init(quic)?;
client.with_origin(origin);
```

### Origins

`Origin::random().produce()` is `moq_tokio::origin::spawn()`.
