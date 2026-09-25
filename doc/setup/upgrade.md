---
title: Upgrade
description: Breaking changes between the 2026-09-17 releases and the 2026-09-23 release train, with the replacement for each
---

# Upgrade

This page walks from the 2026-09-17 releases (moq-relay 0.14.18, moq-cli
0.11.2, moq-net 0.2.22, @moq/net 0.3.5, moq-ffi 0.3.19) to the 2026-09-23
release train (moq-relay 0.15.1, moq-cli 0.12.1, moq-net 0.3.0, @moq/net 0.4.0,
moq-ffi 0.4.1). Each crate's `CHANGELOG.md` has the full list; this page is the
subset that breaks a working setup.

A released flag, environment variable, or config key that was renamed is
refused at startup with its replacement named, rather than ignored. Fix what the
error lists and rerun.

## Wire

Older protocol versions still negotiate, so relays and clients can be upgraded
in any order, apart from [re-minting tokens](#relay-and-cli) and two wire
changes:

- The lite 06 ALPN is `moq-lite-06`, not `moq-lite-06-wip`. An explicit
  `moq-lite-06-wip` in a version list is refused (#3941).
- The hang catalog's root `timeline` entry is `archive`, and wall time moved to
  a root `clock: { wall, timescale }` (#3612, #3675). A new `moq export hls`
  finds no timeline in an old publisher's catalog, so upgrade publishers
  before the HLS gateway.

## Relay and CLI

The `moq-relay` and `moq` flags split into `--listen-*` (accepting),
`--connect-*` (dialing), and a shared `--quic-*` section. Environment
variables follow the flag (`MOQ_SERVER_BIND` is `MOQ_LISTEN`).

| Before | After |
| --- | --- |
| `--server-bind` | `--listen` |
| `--server-*`, `--tls-cert`, `--tls-key`, `--tls-generate` | `--listen-*`, `--listen-tls-cert`, `--listen-tls-key`, `--listen-tls-generate` |
| `--client-connect` | `--connect` |
| `--client-*` | `--connect-*` |
| `--client-failover-delay` | `--connect-race` |
| `--client-reconnect=false` | `--connect-once` (inverted) |
| `--tls-disable-verify`, `--client-tls-disable-verify` | `--connect-tls-insecure` |
| `--server-quic-*`, `--client-quic-*` | `--quic-*`, applied to both directions |
| TOML `[server]`, `[client]` | `[listen]`, `[connect]` |
| TOML `[server.quic]`, `[client.quic]` | `[quic]` |
| TOML `listen`, `connect`, `failover_delay`, `reconnect`, `disable_verify` | `bind`, `url`, `race`, `once` (inverted), `insecure` |
| `--cluster-linger` | removed; a broadcast closes when its last publisher is lost |
| `--cluster-connect host:port` | a full URL, `https://host/?jwt=TOKEN` |
| `moq --origin`, `--name`, `--latency-max` | `--hop`, `--broadcast`, `--max-age` |
| `moq publish`, `moq subscribe` | `moq import`, `moq export` |
| `moq token`, the `moq-token` binary | `moq auth` |

Other changes to a deployment:

- **Auth is one contract** (#3688). The relay asks an auth server per session
  (`--auth-url`) or applies a static anonymous grant (`--auth-public`); exactly
  one is required. `--auth-key`, `--auth-key-dir`, `--auth-api`,
  `--auth-api-mode`, `--auth-public-api`, `--auth-domain`, `--auth-mtls-tier`, and `--auth-tls-*`
  are gone: run `moq auth serve --key-dir ...` next to the relay and point
  `--auth-url` at it. The flag-by-flag mapping is in
  [Migrating from the relay flags](/bin/relay/auth#migrating-from-the-relay-flags).
- **Grants are patterns, not prefixes.** `anon` is now exactly the broadcast
  `anon`; write `anon/**` for the subtree. This applies to `--auth-public`,
  TOML `public`, and the `[auth.public]` table, which is now
  `public_subscribe` / `public_publish`.
- **Token grants are patterns.** JWT `publish` and `subscribe` claims are
  patterns, so a token granting `alice` covers only `alice`; sign `alice/**`
  instead. Existing `put`/`get` tokens and key scopes keep working as subtrees,
  and subtree-only grants are still signed in that form, so issuers and
  verifiers can upgrade in either order. Grants only a pattern can express
  need an upgraded verifier.
- **mTLS admits nothing on its own.** A verified client certificate is reported
  to the auth server, which grants it. `moq auth serve --mtls-publish '**' --mtls-subscribe '**'` restores the old full access for every certificate
  the relay's client CA verifies, so keep that CA to cluster peers.
- **`moq --listen` needs auth.** A CLI listener refuses to start without
  `--auth-url` or `--auth-public` instead of accepting everyone.
- **noq is the only QUIC stack** (#3811). The `quinn` and `quiche` cargo
  features and the backend setting are gone.
- **Stats counters** are `*_started` / `*_ended` (`sessions_started`,
  `announces_ended`, ...). This release still writes the old `announced` /
  `*_closed` names beside them, so move consumers now.

## GStreamer

- `moqsink` properties `estimated-send-bitrate` / `estimated-recv-bitrate` are
  `estimated-send-rate` / `estimated-recv-rate`, with no alias; a `gst-launch`
  line naming the old ones fails at runtime.

## Rust

- **moq-native is moq-tokio** (#2896). moq-native 0.20.0 is a stub that fails
  to compile with the rename. `ClientConfig::default().init()?` is
  `moq_tokio::connect::Config::default().init(quic)?`, and
  `with_publisher(&origin).with_subscriber(origin)` is `with_origin(origin)`.
  Names sit under their modules (`connection::Goaway`, `connection::Monitor`,
  `transport::Session`; #3745).
- **moq-token is moq-auth** (#3684). `Claims` holds `Patterns`, and
  `Claims::authorize` returns pattern residuals.
- **Origins** (#3400, #3804). `Origin::random().produce()` is
  `moq_tokio::origin::spawn()`. `create_broadcast(path, route)` is
  `origin.publish(path, route)`, or `create_broadcast(path)` then
  `broadcast.announce(route)`. `with_root` plus `scope(prefixes)` is one
  `scope(root, &patterns)` returning `Result`. `origin::Info` is
  `origin::Config`.
- **Announcements are prefix routes** (#3225, #3770). `announce::Update` is
  `{ prefix, route, kind, captures }`: skip `!update.kind.is_active()` and
  resolve the broadcast with `consumer.request_broadcast(&update.prefix)`.
  Serve a subtree on demand with `origin.dynamic(prefix, route)`.
- **Tracks.** `with_latency_max` / `latency_max` is `with_max_age` / `max_age`.
  `write_datagram(Datagram)` is `insert_datagram(sequence, timestamp, payload)`
  (#3666); `append_datagram` is unchanged. `track::SubscriberControl` is
  `track::Control`, `track::GroupRequest` is `group::Request`, and
  `ConnectionStats` is `session::Stats` with `estimated_send_rate` /
  `estimated_recv_rate`.
- **Oversized groups abort** with `GroupTooLarge` instead of shedding their head
  (#3585).
- **Catalog edits are fallible** (#3644, #3813). moq-mux and moq-json `lock()`
  is `modify()?`, and a guard that fails to publish on drop aborts the
  track. `timeline::Config::wall` is the broadcast `Clock`.
- **moq-json config** (#3718). `compression: bool` is a `Compression` enum;
  track-owning options are `producer::Config` / `consumer::Config`.
- **moq-mux imports are typed.** `import::Init` splits into `AudioInit`,
  `VideoInit`, and `ContainerInit`, with typed formats instead of strings, and
  `Track::new` is `Track::audio` / `Track::video`.
- **moq-relay embedding** (#3638). `Relay` fields are private: clone the
  handles you need, mount routes, then call `Relay::run`.
  `Cluster::with_cache` moved to `cluster::Options`.

## JavaScript

The JavaScript packages have no changelog; this list follows the breaking
PRs, so a minor rename may be missing.

- **@moq/token is @moq/auth.** `sign` / `verify` are `Key.sign` / `Key.verify`,
  and claims are pattern unions (see [Re-mint tokens](#relay-and-cli)).
- **One `Connection`** (#3614, #3636). `Connection.Reload` is
  `new Moq.Connection({ url })`, which pools one connection per relay. `closed`
  settles only on `close()`; the error that stopped retrying is `error`.
- **Origins hold broadcasts** (#2705, #3225). `connection.publish(path,
  broadcast)` is `origin.createBroadcast(path)` then `broadcast.announce()`, and
  `connection.announced(prefix)` is `origin.announced(scope)` yielding
  `{ prefix, kind, route }`. `ignoreSelf` is gone: reflected announces
  are always dropped.
- **Names mirror Rust** (#3710). `latencyMax` is `maxAge` (a `Time.Milli`),
  `writeDatagram` is `insertDatagram`, and `RemoteError` is `Error.Stream` /
  `Error.Session`.
- **@moq/watch** (#3396, #3817). `latency`, `latency-min`, `latency-max`, and
  `jitter` are `delay` and `buffer`, which need a unit (`delay="100ms"`,
  `buffer="30s"`). `reload` is `announced`. `Watch.Broadcast({ connection })`
  is `Watch.Player({ origin: connection.origin, ... })`.
- **@moq/publish** takes `origin: connection.origin` instead of a
  `connection` signal.
- **@moq/json and @moq/binary** take one options object (#3640):
  `new Json.Snapshot.Consumer({ track })` instead of `(track, config)`.
- **@moq/hang** reads the catalog `archive` entry instead of `timeline`.

## Bindings

Python (`moq-rs` 0.5.0), Swift (0.5.0), Kotlin (`dev.moq:moq` 0.5.0), and Go
wrap moq-ffi 0.4. These are the moq-ffi names; each wrapper follows them in its
own casing:

- **Go module path** is `moq.dev/moq` (was `github.com/moq-dev/moq-go/moq`).
- **Publish split by kind.** `publish_media*` is `publish_audio`,
  `publish_video`, or `publish_container`, each taking its own init. The raw
  encoder paths that used to be `publish_audio` / `publish_video` are
  `encode_audio` / `encode_video`.
- **Durations are microseconds** (`max_age_us`, `MoqBackoff.initial_us`), and
  rate estimates are `estimated_send_rate` / `estimated_recv_rate`.
- **Setters are fallible** (#3642). Client and server configuration setters
  return an error (`Busy` during connect or listen) instead of dropping the
  value. `set_tls_disable_verify(bool)` is `set_tls_verify(bool)`.
- **Announcements.** `MoqAnnounced` is `MoqAnnounceConsumer` and
  `MoqAnnouncement` is `MoqAnnounceUpdate`; `MoqBroadcastRequest::abort` is
  `reject`, and `MoqOriginOptions` is `MoqOriginConfig`.
- **`MoqAudioCodec`** is an `opus()` object (#3671).
- **Errors.** `MoqError::Protocol` carries a `MoqProtocolError` (scope, wire
  code, kind) instead of a flattened message.
- **Track and group `finish()`** keeps the handle open so a later `abort()` can
  still run.

C (libmoq 0.6):

- The 41 `moq_client_*` setters are one zero-initializable `moq_client_config`,
  whose durations are `_us`.
- `moq_publish_media` is `moq_publish_audio`, `moq_publish_video`, or
  `moq_publish_container`; the raw encoders are `moq_encode_audio*` /
  `moq_encode_video*`. Formats are enums instead of strings.
- `moq_announced` is `moq_announce_update`, `moq_origin_consume_announced` is
  `moq_origin_announced_broadcast`, and `moq_broadcast_request_abort` is
  `moq_broadcast_request_reject`.
