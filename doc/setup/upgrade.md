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

## Unreleased

These land with the next breaking release, not the 2026-09-23 train.

- **moq-binary is moq-flate, and @moq/binary is @moq/flate.** The opaque
  `snapshot` and `stream` tracks moved beside the codec; the wire and the
  catalog's `binary` section are unchanged. In Rust, `moq_binary::X` is
  `moq_flate::X`, and `moq_binary::Error::Flate(e)` is the matching
  `moq_flate::Error` variant (`Decompress`, `TooLarge`). `moq_flate::Error`
  now carries `moq_net::Error`, so it is no longer `PartialEq`. moq-mux's
  `Error::Binary` is `Error::Flate`. In TypeScript, import `Snapshot` and
  `Stream` from `@moq/flate`.
- **moq-ffi flate tracks.** `publish_binary_snapshot` / `publish_binary_stream`
  are `publish_flate_snapshot` / `publish_flate_stream`, taking
  `MoqFlateConfig` and returning `MoqFlateSnapshotProducer` /
  `MoqFlateStreamProducer`. The C `moq_publish_binary_*` calls are unchanged.
- **Demand is read through `demand()`.** In Rust, `track::Producer`'s
  `is_used`, `used`, `unused`, and `poll_unused` are `producer.demand().X`,
  and so are `group::Producer`'s `used` and `unused`.
  `track::Request::poll_unused`, `track::Dynamic::poll_unused`, and
  `group::Request::poll_unused` are `demand().poll_unused`, which returns
  `Poll<Result<()>>` instead of `Poll<()>`: an `Err` means the request closed,
  so treat it as unused too. A `group::Request` no longer needs polling to be
  withdrawn; the last `fetch_group` caller leaving does it, so a handler that
  sees it unused just drops it. A fetch arriving after that queues a fresh
  request instead of joining the abandoned one, so a handler that keeps serving
  without watching `demand()` may see its `accept` return `Error::Duplicate`.
  The moq-json snapshot and moq-flate `is_used()` is `demand().is_used()`.
  In TypeScript, `Track.Producer`'s `used` and `unused()` are
  `producer.demand().used` and `.unused()`, as are `Group.Producer`'s, and
  `Allocator.reserve` takes `producer.demand()`, replacing the
  `Bandwidth.Demand` interface.
- **`request_broadcast` takes an epoch.** In Rust,
  `origin::Consumer::request_broadcast(path)` is
  `request_broadcast(path, None)`; pass an `Epoch` instead to refuse any other
  publisher instance. In TypeScript, `RequestOptions.epoch` does the same. A
  dynamic route updated to another epoch now refuses the requests its handler
  still holds with `Unroutable`, so a handler answering them late sees its
  answer dropped.
- **`moq --hop` is removed; `--epoch` replaces it.** A redundant pair shares an
  epoch, a UUIDv7 such as `uuidgen -7` prints, instead of a Hop ID: pass the
  same `--epoch` (or `MOQ_EPOCH`) to both. Unlike a rename, `--hop` is now an
  unknown flag and `MOQ_HOP` is silently ignored, so remove it from any
  deployment: a pair still keyed on `MOQ_HOP` mints an epoch per process, and
  whenever either member starts or restarts, its new epoch replaces the other's
  broadcast and restarts its viewers. `--cluster-id` no longer falls
  back to `--hop`, so a node that pinned its Hop ID with `--hop` passes
  `--cluster-id`. Relays no longer put a random hop in front of a route that
  names no publisher; it keeps its 0.
- **The `"auto"` delay is measured, not derived from RTT** (#4162). It is sized
  from how late frames arrive (see [audio jitter](/concept/audio-jitter)) in
  `@moq/watch` and `moq play`, which now defaults `--delay` to `auto` instead of
  `100ms`. A numeric `@moq/watch` delay is taken literally instead of having
  the rendition's own delay added on top. `Sync.out.jitter` now always
  equals `Sync.out.delay`, and `"auto"` with no decoder registered resolves to
  0 rather than 100 ms.
- **`moq export ts --delay` replaces `--max-age`** (#4645). Each frame is
  written that long after its decode time, on a clock that follows the
  source's; `--max-age` and `--latency-max` are refused with the new flag
  named. In Rust, moq-mux's `ts::Export::with_max_age` is `with_delay`, and
  `ts::stats::Export` gains the dropped-frame count (late frames, and the
  video frames then dropped waiting for a keyframe), measured drift and
  out-of-tolerance count beside its `streams` rows. It is no longer `Eq`.
- **moq-mux has no clock translators.** `clock::Anchor`, `clock::Lane`, and
  `SourceMap` (#4667) are gone, along with the importers' `live()`. Publish the
  source's own timestamps and let the catalog clock map them to wall time;
  pin that mapping with `Config::with_clock` when the source's zero is known.
- **moq-net owns its transport traits.** `moq_net::web_transport_trait` is
  gone, and `transport::poll::{Session, SendStream, RecvStream}` no longer
  extend `web_transport_trait::poll`. They carry their own `poll_*` methods,
  `transport::Error`, and `transport::Stats`, and `Error::from_transport` takes
  a `transport::Error`. moq-tokio's `Client` and `Server` are unchanged. A
  custom transport handed straight to moq-net implements these traits; a
  `moq-uring` session is wrapped with `moq_uring::transport::Session::new`.
- **`--cluster-mesh` and `--cluster-linger` are unknown flags.** moq-relay
  0.17 refuses them by name; later relays reject them, and TOML `mesh` and
  `linger`, like any unknown setting. `MOQ_CLUSTER_MESH` and
  `MOQ_CLUSTER_LINGER` are no longer read, so drop them from the environment.
  In Rust, `cluster::Config` has no `mesh` or `linger` field.
- **Settings no listener reads stop startup.** A stream-only relay or `moq`
  listener (TCP or Unix, no `--listen`) refuses the QUIC-only
  `--listen-preferred-v4`/`-v6`, `--listen-quic-lb-id`, and pinned `tls.peers`,
  and a `--listen-tls-cert`, `-key`, or `-generate` unless `--listen-tcp-tls`
  serves it. `--listen-unix-allow-*` needs `--listen-unix-bind`, and
  `web.https.cert`, `key`, and `root` need `web.https.listen`. `moq` without a
  listener refuses `--listen-*` and `--auth-*` flags. Each used to be ignored;
  drop it, or add the listener it configures.
- **moq-mux data producers take a broadcast-clock `Timestamp`.** `json` and
  `binary` `Snapshot::update` and `Stream::append` take `Timed<_, Timestamp>`
  instead of `Timed<_, Instant>`, and publish it as given. Convert a capture
  `Instant` with `.at(catalog.clock().capture(instant)?)`, reading
  `catalog.clock()` at write time, since an importer's first frame re-anchors
  it. A timestamp ahead of now is published rather than refused.
- **moq-mux importers publish the catalog at their first frame.** The fMP4,
  MKV, and MPEG-TS importers used to publish it at their init segment (`moov`,
  `Tracks`, or the first PMT) on a provisional clock, then re-anchor it on the
  first frame. They now hold it until that frame, as FLV already did, so the
  first snapshot carries the final root `clock`. A reader waiting for the
  catalog now waits for media of a selected track, not just the init segment:
  a track `with_select` deselects doesn't release it, even if its media
  arrives first. A `moov` or `Tracks` decoded after `finish()` is refused with
  `fmp4::Error::MoovAfterFinish` or `mkv::Error::TracksAfterFinish`, since the
  tracks it declares could never finish.
- **fMP4 export fixes its track set at the init segment.** moq-mux's
  `fmp4::Error` drops `MissingVideoTrack`, `MissingAudioTrack`, and
  `NoCatalogSnapshot`, and adds `TrackAdded`, `TrackChanged`, `TrackRewound`,
  and `TrackUndescribed`. `fmp4::Export` and `moq export fmp4` now end with one
  of these where they used to write a track missing from the moov, and a
  broadcast that ends with media queued behind an undescribed track is an error
  rather than an empty `Ok(None)`. Restart the export to pick up a new
  rendition.
- **Subscriber staleness is max delay.** How far a group may fall behind the
  live edge before a subscriber skips it is now `max_delay`, so it no longer
  shares a name with a publisher's retention, which keeps `max_age`. In Rust,
  `track::Subscription::max_age` and `with_max_age` are `max_delay` and
  `with_max_delay`; moq-mux's `container::Consumer::set_max_age` and the fMP4,
  MKV, FLV, H.264, and H.265 exports' `with_max_age` are `set_max_delay` and
  `with_max_delay`; moq-audio's and moq-video's `decode::Options::max_age` is
  `max_delay`, as is moq-audio's `decode::Consumer::max_age()`; and moq-rtmp's
  `Play::with_max_age`, `Client::with_export_max_age`,
  `listen::Config::export_max_age`, and `DEFAULT_MAX_AGE` are `with_max_delay`,
  `with_export_max_delay`, `export_max_delay`, and `DEFAULT_MAX_DELAY`. In
  TypeScript, `Track.Subscription`'s `maxAge` is `maxDelay`, as are
  `Container.Consumer`'s `maxAge` prop and `@moq/watch`'s `Sync.out.maxAge`;
  JavaScript refuses a `maxAge` key in subscription options or container consumer
  props with a `TypeError` naming `maxDelay`, including when both keys are supplied
  or `maxAge` is `undefined`. Untyped callers must rename it.
  moq-ffi's `MoqSubscription`, `MoqAudioDecoderOutput`, and
  `MoqVideoDecoderOutput` take `max_delay_us` (each binding in its own casing),
  and so do C's `moq_subscription`, `moq_audio_decoder_output`,
  `moq_video_decoder_output`, `moq_consume_video`, and `moq_consume_audio`.
  `moq export fmp4`, `mkv`, `flv`, `h264`, `h265`, and `rtmp` take
  `--max-delay`, and refuse `--max-age`. `track::Info::max_age`,
  `MoqTrackInfo.max_age_us`, and `moq import --max-age` are unchanged, as is the wire.
- **moq-relay auth takes the client-CA answer.** `auth::Config::validate` and
  `init` take `client_ca: bool`, whether any listener verifies client
  certificates, and `validate_client_ca` is gone. `moq --listen` with an
  invalid auth config stops at startup instead of refusing every session.
- **A client CA needs a QUIC listener.** A stream-only relay or `moq` listener
  (TCP or Unix, no `--listen`) refuses to start with `listen.tls.root`, which
  nothing verified; moq-tokio returns `Error::MtlsUnsupported` for it.
- **moq-tokio's `Transport` names WebTransport.** `moq_tokio::server::Transport`
  is `moq_tokio::Transport`, with no re-export. A WebTransport session reports
  `Transport::WebTransport` (`"webtransport"` in logs) instead of `Quic`, which
  now means raw QUIC only, and `Connection::transport()` reports the live
  transport. The bindings' `MoqTransport` gains a `WebTransport` case, so an
  exhaustive `switch` or `when` needs one more arm.
- **moq-net has no `VarInt`.** Varints are plain `u64`s:
  `VarInt::decode_quic(buf)?.into_inner()` is `moq_net::varint::decode_quic(buf)?`,
  and `VarInt::try_from(v)?.encode_quic(buf)` is
  `moq_net::varint::encode_quic(v, buf)`, which fails past
  `varint::MAX_QUIC` (2^62 - 1).
- **Opus mapping family lives only on `mapping`.**
  `moq_mux::codec::opus::Config::mapping_family` is gone. Family 0 is
  `mapping: None`; any other family is the mapping's own (`mapping.family()`).
  Set `mapping` alone when building a surround head. The OpusHead bytes are
  unchanged.
- **moq-mux TS stats live in `ts::stats`.** `ts::Stats` is
  `ts::stats::Snapshot` and `ts::StreamStats` is `ts::stats::Stream`, whose
  `track` is an owned `String`. `ts::Export::stats` returns
  `ts::stats::Export`; feed it to `stats::Log` with `.into()`. `ts::MultipleProgramsError` is
  `#[non_exhaustive]`: recover it by downcast and read `programs`.
- **@moq/publish drops `OpusConfig.usedtx`.** Chromium's DTX output shifts the
  audio timeline, so Opus DTX is always off (the WebCodecs default). Remove the
  field; a plain-JS caller still passing it is ignored.
- **FLV export takes a catalog stream.** `flv::Export::new` is synchronous and
  takes `(source, catalog)`, like `fmp4::Export` and `mkv::Export`.
  `with_catalog_format` and `with_select` are gone. Open the catalog yourself
  (`source.catalog(format)`) and narrow it with `catalog::Stream::select`
  before constructing the export. `moq export flv` still applies the same
  rendition flags.
- **A track is timed or untimed, and nothing fills in a timestamp on
  receive.** An untimed track (no timescale) arrives untimed, except over
  moq-lite 05 and later, which can't encode absence yet and carry a send time
  instead; see [untimed tracks](/concept/moq-lite#subscriptions). A write whose
  timedness doesn't match its track fails with `TimestampMismatch`.
  - moq-net: `track::Info.timescale` is `Option<Timescale>`, `None` for an
    untimed track (`with_timescale` takes `impl Into<Option<_>>`), and
    `group::{Producer,Consumer}::timescale()` return `Option<Timescale>`.
    `Frame.timestamp`, `frame::Info.timestamp`, and `Datagram.timestamp` are
    `Option<Timestamp>`. The `write_frame`, `append_datagram`, and
    `insert_datagram` calls take `impl Into<Option<Timestamp>>`, so passing a
    `Timestamp` still compiles.
  - moq-e2ee: `Frame` and `Datagram` timestamps are optional, and its writers
    take `impl Into<Option<Timestamp>>`.
  - moq-archive: recording an untimed track fails with `Error::Untimed`, so
    `moq export archive` over moq-lite before 05 or moq-transport drafts
    14–16 now fails instead of recording arrival times.
  - moq-ffi: `MoqFrame.timestamp_us` and `MoqDatagram.timestamp_us` are
    optional, null on a frame read from an untimed track, and default to null.
    A raw track you publish is timed, so a raw write without one now fails
    with `TimestampMismatch` instead of going out at 0.
    `MoqMediaProducer::write_frame` without one used to pass a PTS of 0; it
    now passes none, so the importer derives the time itself (elapsed since
    its first frame) or refuses a codec that needs one (avc1, hvc1).
    `MoqTrackInfo.timescale` is null on a received untimed track. In Go,
    `Frame.TimestampUs` and `Datagram.TimestampUs` are `*uint64`; in Kotlin
    and Dart, `Frame.timestamp` and `Datagram.timestamp` are nullable.
  - libmoq: `moq_frame` and `moq_datagram` gain `timestamp_present`, which
    changes their size, so recompile against the new `moq.h`.
  - Receivers: tracks from moq-lite before 05, and moq-transport tracks
    without `TIMESCALE` (every track on drafts 14–16), arrive untimed instead
    of stamped with their arrival time; an object-scope Timescale is ignored.
    On a moq-transport track with `TIMESCALE`, an object without a Timestamp
    is malformed. A new subscriber with no start on an untimed track starts at
    the latest group.

## Wire

Older protocol versions still negotiate, so relays and clients can be upgraded
in any order, apart from [pattern-only token grants](#relay-and-cli) and two wire
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
| `--cluster-mesh`, TOML `mesh` | removed; list every peer with `--cluster-connect` or `--cluster-connect-api` |
| `moq --origin`, `--name`, `--latency-max` | `--hop` (removed for `--epoch` after [Unreleased](#unreleased)), `--broadcast`, `--max-age` |
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
  `public_subscribe` / `public_publish`. A public or mTLS pattern with no
  wildcard refuses to start, naming the subtree to write, rather than pick
  one reading silently. The patterns are rooted at `/`, as in 0.14, so
  `anon/**` admits a client dialed at `/anon` and refuses one dialed outside
  `anon/`. Earlier 0.15 releases rooted them at the dialed path instead.
- **Token grants are patterns.** JWT `publish` and `subscribe` claims are
  patterns, so a token granting `alice` covers only `alice`; sign `alice/**`
  instead. Existing `put`/`get` tokens and key scopes keep working as subtrees,
  and subtree-only grants are still signed in that form, so a `moq-token`
  deployment can upgrade issuers and verifiers in either order. Verifiers on
  the pattern-only `moq-auth` 0.1.0/0.1.1 or `@moq/auth` 0.1.x/0.2.0 refuse
  that form, so upgrade them before their issuers. Grants only a pattern can
  express need an upgraded verifier.
- **Removed auth settings refuse to start.** 0.14's `[auth]` `key`, `key_dir`,
  `auth_api`, `domains`, `mtls_tier`, and `[auth.tls]`, and their flags and
  `MOQ_AUTH_*` variables, stop the relay with the replacement named rather
  than being ignored.
- **Token claims.** `iss`, `sub`, and `jti` are ignored and `nbf` is enforced.
  Any other claim refuses the token with its name, `aud` and `cluster`
  included, where 0.14 ignored all but `aud`: an issuer adding app claims such
  as `user_id` must drop them.
- **Every credential is evaluated or refused.** A relay on `--auth-public`
  refuses a session presenting a token, as 0.14 did, including peers sending
  `cluster.token`, and refuses to start with a client CA. `moq auth serve`
  refuses a session presenting both a JWT and a certificate, so a peer
  presents one or the other.
- **`moq auth serve` never re-checks or expires by default**, as 0.14 never
  did. `--revalidate` needs `--expires`, and `--limit-*` needs `--revalidate`.
- **mTLS admits nothing on its own.** A verified client certificate is reported
  to the auth server, which grants it. `moq auth serve --mtls-publish '**' --mtls-subscribe '**' --mtls-peer` restores the old full access for every certificate
  the relay's client CA verifies, as a cluster peer, so keep that CA to cluster peers.
- **`moq --listen` needs auth.** A CLI listener refuses to start without
  `--auth-url` or `--auth-public` instead of accepting everyone.
- **Gossip discovery is removed.** A relay dials only the peers it lists or
  finds on the LAN, never a URL learned from an announcement, and no longer
  announces `.internal/origins`. The `/nodes` endpoint lists only peers this
  relay dialed. Until every relay that ran `--cluster-mesh` is upgraded, keep
  client grants off `.internal/`: an older relay still dials any URL announced
  there with `cluster.token`, and an upgraded peer still forwards it.
- **Other 0.14 auth differences kept.** Peers identify by certificate or LAN
  path. An auth server's `root` alias may have any depth.
  A path in `--cluster-connect` is not refused, although it shifts the mesh
  frame. `moq auth serve --key` takes a file, not an https or JWKS URL.
  `moq auth sign --root` is the token root and JS `verify --root` the dialed
  path. `/.cluster*` roots are reserved. `--auth-public a,b` splits on commas.
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
  resolve the broadcast with `consumer.request_broadcast(&update.prefix, update.route.epoch)`.
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
  and claims are pattern unions (see [Token grants are patterns](#relay-and-cli)).
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
- **@moq/json and @moq/binary** (now `@moq/flate`) take one options object (#3640):
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
