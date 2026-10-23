# Migrating moq-tokio

How to update your code for each breaking change, newest first.

## Unreleased

### External TLS resolvers carry an explicit client-auth policy ([moq-dev/moq#6529](https://github.com/moq-dev/moq/pull/6529))

Replace `listen.tls.resolver = Some(resolver)` with
`tls::Serve::resolver(resolver, &listen.tls)?`, then pass the result in `server::Config.tls`
and clear `server.listen.tls`. The constructor retains `root` and `peers` verification
and refuses `cert`, `key`, `generate`, or `identity`, which would hide a certificate source.
A shared `server.tls` and configured `listen.tls` remain mutually exclusive.

### Key logging takes an explicit path ([moq-dev/moq#6529](https://github.com/moq-dev/moq/pull/6529))

`SSLKEYLOGFILE` alone no longer writes session secrets. Set `quic.keylog = Some(path)`
or `--quic-keylog /private/keys.log` (`MOQ_QUIC_KEYLOG`, `[quic] keylog = "/private/keys.log"`).
The native QUIC backend opens the file during initialization. New Unix files use mode 0600;
an existing mode-0644 log is refused without changing its permissions. Make the file private explicitly.
Iroh currently refuses an explicit path when enabled because its pinned API supports only process-wide environment logging.
It also refuses explicit zero `keep_alive`: its pinned API cannot clear the default heartbeat.
Omit that setting or use a positive interval for Iroh; other backends retain zero-off support.
`tls::KeyLog::open(path)` provides the same explicit logger for custom rustls configurations.

### Accepted sessions are supervised internally ([moq-dev/moq#6529](https://github.com/moq-dev/moq/pull/6529))

`server::Request::ok()` returns a `Session`. Read `session.closed()` for its terminal
cause, including a driver panic; there is no task handle to retain or await.
The protocol driver records a panic before unwinding, so an executor that catches
it cannot turn its later drop into ordinary cancellation.

### Backoff knobs each do one job, and zero turns them off ([moq-dev/moq#6529](https://github.com/moq-dev/moq/pull/6529))

`backoff.initial` is only the first retry delay: zero now retries at once instead of
waiting 50ms. `backoff.max` is only the cap: zero leaves the delay uncapped, and a cap
below `initial` caps the first delay too, where both used to be raised to `initial`.
A multiplier of 0 or 1 keeps the delay constant.
The healthy-session bar is the new `backoff.healthy` (`--backoff-healthy`,
`MOQ_BACKOFF_HEALTHY`, `[connect.backoff] healthy`), defaulting to 1s; it used to follow
`initial`. A session that ends sooner is a failed attempt, so an immediate close still
spends `backoff.timeout`. Zero is refused with `Error::BackoffHealthy`.
If you raised `initial` to slow redials after flapping sessions, set `healthy` to the same value.

### Config durations use one human-readable value ([moq-dev/moq#6529](https://github.com/moq-dev/moq/pull/6529))

Set connect `race`, `resolution_delay`, `timeout`; listen `timeout`; QUIC `idle_timeout`,
`keep_alive`; backoff `initial`, `max`, `timeout`; GOAWAY `handover`; and WebSocket `delay`
with `Some(std_duration.into())`, or parse a `moq_tokio::cli::Duration` from `"500ms"`.
`None` uses the runtime default, including values built with `Default::default()`.
TOML keys stay unchanged and take human-readable strings. A later typed assignment
replaces a parsed CLI value instead of losing to a hidden shadow field.
`websocket::Options.timeout = None` uses the ten-second handshake default;
use `Some(std::time::Duration::ZERO)` to wait indefinitely.
`Backoff::initial()`, `max()`, and `timeout()` report the configured or default
retry values, including the defaults used by foreign settings UIs.

`connect::Config.quic` and `listen::Config.quic` are private legacy parsing fields.
Supply shared transport tuning through the `quic` argument to `Config::init`
or `server::Config.quic`.

### Reconnect policy belongs to each client clone ([moq-dev/moq#6529](https://github.com/moq-dev/moq/pull/6529))

Set `client.with_reconnect(false)` for a one-shot native dial, or `true` to redial.
The duplicate `connect.once` field and `--connect-once` / `MOQ_CONNECT_ONCE` are removed.
Released `--client-reconnect` / `MOQ_CLIENT_RECONNECT` and TOML `reconnect` are refused
with this native API guidance. CLI clients always reconnect and no longer offer
an operator override. Foreign bindings retain their existing `once` setting.

### QUIC-LB settings have one path ([moq-dev/moq#6529](https://github.com/moq-dev/moq/pull/6529))

Move `listen.lb_id` / `listen.lb_nonce` or `listen.load_balancer` to `[listen.lb]`
with `id` and optional `nonce` (default 8). In Rust set `config.lb.id` and `config.lb.nonce`.
The duplicate public `quic::LoadBalancer` runtime record and `Config::load_balancer()` are private;
configure or inspect `listen::LoadBalancer` through `config.lb`.
The `--listen-quic-lb-id` / `--listen-quic-lb-nonce` flags and their environment variables
keep their names and update those same fields. A nonce without an ID is refused.

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

### QUIC limits are `moq_quic::Limits`, set through `Request::quic` (#5871)

`server::Request::set_quic_limits` and `Error::QuicLimits` are gone.
`Request::quic()` returns the `quic::driver::Connection` under a QUIC or WebTransport request, or `None` on other transports, and its `set_limits` takes `quic::Limits`, now `moq_quic::Limits`, where each `None` field keeps that limit.
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
Set the existing `pin` field to keep pinning; `with_pin` is removed.

```rust
let mut workers = moq_tokio::worker::Config::new(8);
workers.pin = true;
```

### Rotating TLS identities are a `tls::Serve` (#5312)

`tls::Listen::server_config_reloading` and `tls::ReloadingServerConfig` are gone; load once with `tls::Listen::load` and read each handshake's config from the returned `tls::Serve` with `config(alpns)`.
`Error::WorkerTlsGenerate` is gone because generated certificates now work on workers too.
A TLS trust-root watcher that fails to register now fails client and server construction.

### "Epoch" means only the publisher epoch (#5146)

The reconnect counter is `connects()`; rename `epoch()` calls.

### Settings no listener reads stop startup (#5006)

A stream-only listener (TCP or Unix, no QUIC bind) refuses the QUIC-only `preferred_v4` / `preferred_v6`, `lb.id`, pinned `tls.peers`, and a TLS cert, key, or `generate` unless `tcp.tls` serves it.
`unix.allow_*` needs `unix.bind`.
Each used to be ignored: drop it, or add the listener it configures.

### A client CA needs a QUIC listener

A stream-only listener with `listen.tls.root` returns `Error::MtlsUnsupported` instead of silently verifying nothing.

### `Transport` names WebTransport

`server::Transport` is `moq_tokio::Transport`, with no re-export.
A WebTransport session reports `Transport::WebTransport` (`"webtransport"` in logs); `Quic` now means raw QUIC only, so match both where you meant either.
`Connection::transport()` reports the live transport.

### Custom sessions declare their request binding

`transport::Session::new` takes the session's `moq_net::transport::Binding`: `Uri` when the transport handshake carries the request URI, `InBand` when SETUP does.
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
