# Migrating moq-tokio

How to update your code for each breaking change, newest first.

## Unreleased

### Auth handles live in `connection::auth` (#5695)

The root `Auth` re-export is gone.
`Connection::auth` returns a `connection::auth::Handle`, and its `add` returns a `connection::auth::Auth` instead of a `Token`.

```rust
// Before
let handle: moq_tokio::Auth = connection.auth();
let token: moq_tokio::connection::auth::Token = handle.add(bytes).await?;
// After
let handle: moq_tokio::connection::auth::Handle = connection.auth();
let auth: moq_tokio::connection::auth::Auth = handle.add(bytes).await?;
```

### Datagrams take a send group and order (#5770)

`quic::driver::Connection::send_datagram` and `quic::web::Session::send_datagram` take the send group (`None` for ungrouped) and an order, and never block.
`send_datagram_wait` and `SendDatagram` are gone.

```rust
// Before
session.send_datagram_wait(data).await?;
// After
session.send_datagram(data, None, 0)?;
```

### The `noq` feature is `quic` (#5664)

The QUIC stack is MoQ's own `moq-quic` instead of the `moq-noq` crates.
Rename the cargo feature, and the error types with it.
A qlog directory gets one file per endpoint instead of one per connection.

```toml
# Before
moq-tokio = { version = "...", features = ["noq"] }
# After
moq-tokio = { version = "...", features = ["quic"] }
```

```rust
// Before
Err(moq_tokio::Error::Noq(err)) => {} // err: moq_tokio::noq::Error
// After
Err(moq_tokio::Error::Quic(err)) => {} // err: moq_tokio::quic::Error
```

### Workers are unpinned by default (#5688)

`worker::Config::new(count)` no longer pins worker *i* to core *i*.
Call `with_pin(true)` to keep pinning.

```rust
let workers = moq_tokio::worker::Config::new(8).with_pin(true);
```

### Accepted sessions return their driver task (#5562)

`server::Request::ok()` returns `(session, driver)`.
Await the driver before classifying the session close, so a driver panic reports `Error::Panic` instead of the cancellation its teardown leaves behind.

```rust
// Before
let session = request.ok().await?;
// After
let (session, driver) = request.ok().await?;
let result = driver.await?;
```

### Config durations are optional

Wrap durations in `Some(...)` for connect `race`, `resolution_delay`, and `timeout`; listen `timeout`; QUIC `idle_timeout` and `keep_alive`; backoff `initial`, `max`, and `timeout`; GOAWAY `handover`; and WebSocket `delay`.
`None` uses the default, and `Some(Duration::ZERO)` keeps each knob's zero behavior.
A value set in code now wins over the parser default unless the flag or environment variable was actually given.
TOML keys are unchanged.

```rust
// Before
config.backoff.timeout = Duration::from_secs(30);
// After
config.backoff.timeout = Some(Duration::from_secs(30));
```

### Rotating TLS identities are a `tls::Serve` (#5312)

`tls::Listen::server_config_reloading` and `tls::ReloadingServerConfig` are gone; load once with `tls::Listen::load` and read each handshake's config from the returned `tls::Serve`.
`Error::WorkerTlsGenerate` is gone because generated certificates now work on workers too.
A TLS trust-root watcher that fails to register now fails client and server construction.

```rust
// Before
let config = listen.server_config_reloading(alpns)?;
// After
let serve = listen.load()?;
let config = serve.config(alpns)?;
```

### "Epoch" means only the publisher epoch (#5146)

The reconnect counter is `connects()`.

```rust
// Before
let n = connection.epoch();
// After
let n = connection.connects();
```

### A QUIC load-balancer nonce needs its server id (#5114)

`listen.lb_nonce` without `lb_id` fails to start.
Setting `lb_id` (or `lb_nonce`) together with `load_balancer` also fails; `lb_id` used to win silently.
Set one of them.

```toml
# Before
[listen]
lb_nonce = 8
# After
[listen]
lb_id = "01"
lb_nonce = 8
```

### Settings no listener reads stop startup (#5006)

A stream-only listener (TCP or Unix, no QUIC bind) refuses the QUIC-only `preferred_v4` / `preferred_v6`, `lb_id`, pinned `tls.peers`, and a TLS cert, key, or `generate` unless `tcp.tls` serves it.
`unix.allow_*` needs `unix.bind`.
Each used to be ignored: drop it, or add the listener it configures.

### A client CA needs a QUIC listener

A stream-only listener with `listen.tls.root` returns `Error::MtlsUnsupported` instead of silently verifying nothing.

### `Transport` names WebTransport

`server::Transport` is `moq_tokio::Transport`, with no re-export.
A WebTransport session reports `Transport::WebTransport` (`"webtransport"` in logs); `Quic` now means raw QUIC only.
`Connection::transport()` reports the live transport.

```rust
// Before
use moq_tokio::server::Transport;
// After
use moq_tokio::Transport;
match transport {
	Transport::Quic | Transport::WebTransport => {}
	_ => {}
}
```

### Custom sessions declare their request binding

`transport::Session::new` takes the session's `moq_net::transport::Binding`: `Uri` when the transport handshake carries the request URI, `InBand` when SETUP does.
`Client` and `Server` are unchanged.

```rust
// Before
let session = moq_tokio::transport::Session::new(session);
// After
let session = moq_tokio::transport::Session::new(session, moq_net::transport::Binding::Uri);
```

### `Redirect::resolve` is removed (#4854)

`Redirect` and `Goaway` stay, but the lenient `resolve(uri, current)` is gone.
Follow a GOAWAY through the connection, which validates the target itself.

```rust
// Before
let next = Redirect::SameHost.resolve(uri, &current);
// After
config.goaway.redirect = Redirect::SameHost; // the connection follows and validates the GOAWAY
```

### The `android-logcat` feature is removed (#4608)

Android builds always log to logcat.
Drop the feature from your dependency.

```toml
# Before
moq-tokio = { version = "...", features = ["android-logcat"] }
# After
moq-tokio = { version = "..." }
```

## moq-native 0.19 -> moq-tokio 0.19

### moq-native is moq-tokio (#2896)

moq-native 0.20.0 is a stub that fails to compile, pointing at the rename.
Names sit under their modules: `connection::Goaway`, `connection::Monitor`, `transport::Session` (#3745).

```rust
// Before
let client = moq_native::ClientConfig::default().init()?;
client.with_publisher(&origin).with_subscriber(origin);
// After
let client = moq_tokio::connect::Config::default().init(quic)?;
client.with_origin(origin);
```

### Origins

`Origin::random().produce()` is `moq_tokio::origin::spawn()`.

```rust
// Before
let origin = Origin::random().produce();
// After
let origin = moq_tokio::origin::spawn();
```
